use crate::workspace_store::{PendingRemoval, RemovalTarget, StoredWorkspace, WorkspaceStore};
use compi_protocol::{
    ErrorCode, LaunchRequest, LayoutNode, MAX_MUTATION_RECEIPTS, MAX_TAB_PANES, MergedTab,
    MutationId, MutationReceipt, MutationRequest, PaneId, PlannedLayoutNode, ProcessLifetimeId,
    ServerGeneration, SessionId, SplitAxis, SurfaceId, SurfaceInfo, SurfaceStatus, TabId, TabMerge,
    WorkspaceMutation, WorkspaceSession, WorkspaceSnapshot, WorkspaceTab,
};
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ACTOR_QUEUE: usize = 64;
const EFFECT_QUEUE: usize = 64;
const SUBSCRIBER_QUEUE: usize = 64;

#[derive(Clone)]
pub struct WorkspaceActor {
    sender: SyncSender<ActorCommand>,
    generation: ServerGeneration,
}

#[derive(Debug, Clone)]
pub enum WorkspaceEffect {
    Launch(SurfaceInfo, Option<Box<compi_protocol::LaunchContext>>),
    /// One transaction's effects occupy one bounded queue slot. The worker
    /// processes them in order while the actor remains available for feedback.
    Batch(Vec<WorkspaceEffect>),
    End {
        surface_id: SurfaceId,
        process_lifetime_id: ProcessLifetimeId,
    },
}

#[derive(Debug, Clone)]
pub enum RuntimeObservation {
    Running {
        surface_id: SurfaceId,
        process_lifetime_id: ProcessLifetimeId,
        working_directory: Option<compi_protocol::WorkingDirectory>,
    },
    Resized {
        surface_id: SurfaceId,
        process_lifetime_id: ProcessLifetimeId,
        cols: i16,
        rows: i16,
    },
    Exited {
        surface_id: SurfaceId,
        process_lifetime_id: ProcessLifetimeId,
        exit_code: u32,
    },
    Failed {
        surface_id: SurfaceId,
        process_lifetime_id: ProcessLifetimeId,
        error: String,
    },
    EndFailed {
        surface_id: SurfaceId,
        process_lifetime_id: ProcessLifetimeId,
        error: String,
    },
    /// The shell reported a new current directory (OSC 7).
    Directory {
        surface_id: SurfaceId,
        process_lifetime_id: ProcessLifetimeId,
        directory: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceEvent {
    Revision(u64),
}

#[derive(Debug, Clone)]
pub struct ActorError {
    pub code: ErrorCode,
    pub message: String,
    pub current_revision: Option<u64>,
}

impl ActorError {
    fn new(code: ErrorCode, message: impl Into<String>, revision: Option<u64>) -> Self {
        Self {
            code,
            message: message.into(),
            current_revision: revision,
        }
    }
}

impl fmt::Display for ActorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ActorError {}

enum ActorCommand {
    Snapshot(SyncSender<WorkspaceSnapshot>),
    ConditionalStop(
        compi_protocol::LifecycleConsent,
        Arc<AtomicBool>,
        SyncSender<std::result::Result<(), ActorError>>,
    ),
    Mutate(
        MutationRequest,
        SyncSender<std::result::Result<MutationReceipt, ActorError>>,
    ),
    Outcome(
        MutationId,
        SyncSender<std::result::Result<Option<MutationReceipt>, ActorError>>,
    ),
    Observe(RuntimeObservation),
    Subscribe(SyncSender<WorkspaceEvent>),
}

struct PersistJob {
    candidate: StoredWorkspace,
}

struct PersistResult {
    result: crate::Result<()>,
}

struct PendingCommit {
    candidate: StoredWorkspace,
    receipt: Option<MutationReceipt>,
    effects: Vec<WorkspaceEffect>,
    prelaunched: Vec<(SurfaceId, ProcessLifetimeId)>,
    reply: Option<SyncSender<std::result::Result<MutationReceipt, ActorError>>>,
}

struct ActorState {
    workspace: StoredWorkspace,
    generation: ServerGeneration,
    effect_sender: SyncSender<WorkspaceEffect>,
    subscribers: Vec<SyncSender<WorkspaceEvent>>,
    queued: VecDeque<ActorCommand>,
    pending: Option<PendingCommit>,
    deferred_effect: Option<WorkspaceEffect>,
    read_only_error: Option<String>,
    next_ordinal: u64,
    stopping: bool,
}

impl WorkspaceActor {
    pub fn memory() -> (Self, Receiver<WorkspaceEffect>) {
        let (store, workspace) = WorkspaceStore::memory();
        Self::start(store, workspace)
    }

    pub fn persistent(instance: Option<&str>) -> crate::Result<(Self, Receiver<WorkspaceEffect>)> {
        let (store, workspace) = WorkspaceStore::open(instance)?;
        Ok(Self::start(store, workspace))
    }

    fn start(
        store: WorkspaceStore,
        workspace: StoredWorkspace,
    ) -> (Self, Receiver<WorkspaceEffect>) {
        let generation = ServerGeneration::new(new_id("generation", 0));
        let (sender, receiver) = mpsc::sync_channel(ACTOR_QUEUE);
        let (effect_sender, effect_receiver) = mpsc::sync_channel(EFFECT_QUEUE);
        let actor = Self {
            sender,
            generation: generation.clone(),
        };
        thread::spawn(move || run_actor(receiver, store, workspace, generation, effect_sender));
        (actor, effect_receiver)
    }

    pub fn generation(&self) -> &ServerGeneration {
        &self.generation
    }

    pub fn snapshot(&self) -> std::result::Result<WorkspaceSnapshot, ActorError> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.sender
            .try_send(ActorCommand::Snapshot(reply))
            .map_err(queue_error)?;
        receive.recv().map_err(disconnected)
    }

    pub fn mutate(
        &self,
        mutation: MutationRequest,
    ) -> std::result::Result<MutationReceipt, ActorError> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.sender
            .try_send(ActorCommand::Mutate(mutation, reply))
            .map_err(queue_error)?;
        receive.recv().map_err(disconnected)?
    }

    pub fn conditional_stop(
        &self,
        consent: compi_protocol::LifecycleConsent,
        stopping: Arc<AtomicBool>,
    ) -> std::result::Result<(), ActorError> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.sender
            .try_send(ActorCommand::ConditionalStop(consent, stopping, reply))
            .map_err(queue_error)?;
        receive.recv().map_err(disconnected)?
    }

    pub fn outcome(
        &self,
        mutation_id: MutationId,
    ) -> std::result::Result<Option<MutationReceipt>, ActorError> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.sender
            .try_send(ActorCommand::Outcome(mutation_id, reply))
            .map_err(queue_error)?;
        receive.recv().map_err(disconnected)?
    }

    pub fn observe(&self, observation: RuntimeObservation) {
        let _ = self.sender.send(ActorCommand::Observe(observation));
    }

    pub fn subscribe(&self) -> Receiver<WorkspaceEvent> {
        let (sender, receiver) = mpsc::sync_channel(SUBSCRIBER_QUEUE);
        let _ = self.sender.send(ActorCommand::Subscribe(sender));
        receiver
    }
}

fn run_actor(
    receiver: Receiver<ActorCommand>,
    store: WorkspaceStore,
    workspace: StoredWorkspace,
    generation: ServerGeneration,
    effect_sender: SyncSender<WorkspaceEffect>,
) {
    let (persist_sender, persist_receiver) = mpsc::sync_channel::<PersistJob>(1);
    let (completion_sender, completion_receiver) = mpsc::sync_channel::<PersistResult>(1);
    let persistence = store.clone();
    thread::spawn(move || {
        while let Ok(job) = persist_receiver.recv() {
            let result = persistence.commit(&job.candidate);
            if completion_sender.send(PersistResult { result }).is_err() {
                return;
            }
        }
    });

    let mut state = ActorState {
        workspace,
        generation,
        effect_sender,
        subscribers: Vec::new(),
        queued: VecDeque::new(),
        pending: None,
        deferred_effect: None,
        read_only_error: None,
        next_ordinal: 1,
        stopping: false,
    };

    loop {
        flush_deferred_effect(&mut state);
        match completion_receiver.try_recv() {
            Ok(completion) => finish_commit(&mut state, completion),
            Err(TryRecvError::Disconnected) => {
                state.read_only_error = Some("workspace persistence worker stopped".into())
            }
            Err(TryRecvError::Empty) => {}
        }
        if state.pending.is_none() {
            while let Some(command) = state.queued.pop_front() {
                if process_command(&mut state, command, &persist_sender) {
                    return;
                }
                if state.pending.is_some() {
                    break;
                }
            }
        }

        match receiver.recv_timeout(Duration::from_millis(5)) {
            Ok(command) if state.pending.is_some() && is_commit_command(&command) => {
                if state.queued.len() >= ACTOR_QUEUE {
                    reject_busy(command, state.workspace.revision);
                } else {
                    state.queued.push_back(command);
                }
            }
            Ok(command) => {
                if process_command(&mut state, command, &persist_sender) {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn is_commit_command(command: &ActorCommand) -> bool {
    matches!(
        command,
        ActorCommand::Mutate(..) | ActorCommand::Observe(_) | ActorCommand::ConditionalStop(..)
    )
}

fn reject_busy(command: ActorCommand, revision: u64) {
    let error = ActorError::new(
        ErrorCode::Busy,
        "workspace mutation queue is full",
        Some(revision),
    );
    match command {
        ActorCommand::Mutate(_, reply) => {
            let _ = reply.send(Err(error));
        }
        ActorCommand::ConditionalStop(_, _, reply) => {
            let _ = reply.send(Err(error));
        }
        _ => {}
    }
}

fn process_command(
    state: &mut ActorState,
    command: ActorCommand,
    persist_sender: &SyncSender<PersistJob>,
) -> bool {
    match command {
        ActorCommand::ConditionalStop(consent, stopping, reply) => {
            let snapshot = state.workspace.snapshot(state.generation.clone());
            let result = if consent.server_id != snapshot.server_id
                || consent.server_generation != snapshot.server_generation
            {
                Err(ActorError::new(
                    ErrorCode::StaleGeneration,
                    "daemon generation changed; review live work again",
                    Some(snapshot.revision),
                ))
            } else if consent.workspace_revision != snapshot.revision
                || consent.live_surfaces != compi_protocol::live_surface_inventory(&snapshot)
            {
                Err(ActorError::new(
                    ErrorCode::RevisionConflict,
                    "live work changed; review shutdown consent again",
                    Some(snapshot.revision),
                ))
            } else if state.deferred_effect.is_some() {
                Err(ActorError::new(
                    ErrorCode::Busy,
                    "workspace runtime effects are awaiting dispatch",
                    Some(snapshot.revision),
                ))
            } else if state.stopping {
                Err(ActorError::new(
                    ErrorCode::Busy,
                    "daemon is already stopping",
                    Some(snapshot.revision),
                ))
            } else {
                // Serialized with all mutations and pending commits. No later command
                // can create work after the inventory has been accepted.
                state.stopping = true;
                stopping.store(true, Ordering::Release);
                Ok(())
            };
            let _ = reply.send(result);
        }
        ActorCommand::Snapshot(reply) => {
            let _ = reply.send(state.workspace.snapshot(state.generation.clone()));
        }
        ActorCommand::Outcome(mutation_id, reply) => {
            let receipt = state
                .workspace
                .receipts
                .iter()
                .find(|receipt| receipt.mutation_id == mutation_id)
                .cloned();
            let _ = reply.send(Ok(receipt));
        }
        ActorCommand::Subscribe(subscriber) => state.subscribers.push(subscriber),
        ActorCommand::Mutate(request, reply) => {
            if state.stopping {
                let _ = reply.send(Err(ActorError::new(
                    ErrorCode::Busy,
                    "daemon is stopping",
                    Some(state.workspace.revision),
                )));
                return false;
            }
            if let Some(error) = state.read_only_error.as_ref() {
                let _ = reply.send(Err(ActorError::new(
                    ErrorCode::PersistenceUnavailable,
                    error.clone(),
                    Some(state.workspace.revision),
                )));
                return false;
            }
            let fingerprint = match fingerprint(&request) {
                Ok(fingerprint) => fingerprint,
                Err(error) => {
                    let _ = reply.send(Err(ActorError::new(
                        ErrorCode::InvalidRequest,
                        format!("could not fingerprint mutation: {error}"),
                        Some(state.workspace.revision),
                    )));
                    return false;
                }
            };
            if let Some(receipt) = state
                .workspace
                .receipts
                .iter()
                .find(|receipt| receipt.mutation_id == request.mutation_id)
            {
                let result = if receipt.fingerprint == fingerprint {
                    Ok(receipt.clone())
                } else {
                    Err(ActorError::new(
                        ErrorCode::MutationIdReused,
                        "mutation ID was already used for different content",
                        Some(state.workspace.revision),
                    ))
                };
                let _ = reply.send(result);
                return false;
            }
            if state.deferred_effect.is_some() {
                let _ = reply.send(Err(ActorError::new(
                    ErrorCode::Busy,
                    "workspace runtime effects are awaiting dispatch",
                    Some(state.workspace.revision),
                )));
                return false;
            }
            let next_ordinal = state.next_ordinal;
            match prepare_mutation(state, &request, fingerprint) {
                Ok(mut pending) => {
                    pending.reply = Some(reply);
                    let mut launches = Vec::new();
                    for effect in std::mem::take(&mut pending.effects) {
                        let WorkspaceEffect::Launch(info, _) = &effect else {
                            pending.effects.push(effect);
                            continue;
                        };
                        pending
                            .prelaunched
                            .push((info.id.clone(), info.process_lifetime_id.clone()));
                        launches.push(effect);
                    }
                    if let Some(effect) = effect_batch(launches)
                        && let Err(error) = state.effect_sender.try_send(effect)
                    {
                        // An atomic queue handoff failed; none of these launches can run.
                        state.next_ordinal = next_ordinal;
                        pending.prelaunched.clear();
                        let (code, message) = match error {
                            TrySendError::Full(_) => {
                                (ErrorCode::Busy, "workspace runtime effect queue is full")
                            }
                            TrySendError::Disconnected(_) => {
                                state.read_only_error =
                                    Some("workspace runtime effect worker stopped".into());
                                (
                                    ErrorCode::PersistenceUnavailable,
                                    "workspace runtime effect worker stopped",
                                )
                            }
                        };
                        let _ = pending.reply.take().unwrap().send(Err(ActorError::new(
                            code,
                            message,
                            Some(state.workspace.revision),
                        )));
                    } else if persist_sender
                        .send(PersistJob {
                            candidate: pending.candidate.clone(),
                        })
                        .is_err()
                    {
                        let cleanup = pending
                            .prelaunched
                            .into_iter()
                            .map(|(surface_id, process_lifetime_id)| WorkspaceEffect::End {
                                surface_id,
                                process_lifetime_id,
                            })
                            .collect();
                        defer_effects(state, cleanup);
                        let _ = pending.reply.take().unwrap().send(Err(ActorError::new(
                            ErrorCode::PersistenceUnavailable,
                            "workspace persistence worker stopped",
                            Some(state.workspace.revision),
                        )));
                        state.read_only_error = Some("workspace persistence worker stopped".into());
                    } else {
                        state.pending = Some(pending);
                    }
                }
                Err(error) => {
                    let _ = reply.send(Err(error));
                }
            }
        }
        ActorCommand::Observe(observation) => {
            if state.read_only_error.is_some() {
                return false;
            }
            if let Some(candidate) = apply_observation(&state.workspace, observation) {
                let pending = PendingCommit {
                    candidate,
                    receipt: None,
                    effects: Vec::new(),
                    prelaunched: Vec::new(),
                    reply: None,
                };
                if persist_sender
                    .send(PersistJob {
                        candidate: pending.candidate.clone(),
                    })
                    .is_ok()
                {
                    state.pending = Some(pending);
                } else {
                    state.read_only_error = Some("workspace persistence worker stopped".into());
                }
            }
        }
    }
    false
}

fn finish_commit(state: &mut ActorState, completion: PersistResult) {
    let Some(pending) = state.pending.take() else {
        return;
    };
    match completion.result {
        Ok(()) => {
            state.workspace = pending.candidate;
            let revision = state.workspace.revision;
            state.subscribers.retain(|subscriber| {
                subscriber
                    .try_send(WorkspaceEvent::Revision(revision))
                    .is_ok()
            });
            defer_effects(state, pending.effects);
            if let (Some(reply), Some(receipt)) = (pending.reply, pending.receipt) {
                let _ = reply.send(Ok(receipt));
            }
        }
        Err(error) => {
            let cleanup = pending
                .prelaunched
                .into_iter()
                .map(|(surface_id, process_lifetime_id)| WorkspaceEffect::End {
                    surface_id,
                    process_lifetime_id,
                })
                .collect();
            defer_effects(state, cleanup);
            let message = format!("workspace persistence is unavailable: {error}");
            state.read_only_error = Some(message.clone());
            if let Some(reply) = pending.reply {
                let _ = reply.send(Err(ActorError::new(
                    ErrorCode::PersistenceUnavailable,
                    message,
                    Some(state.workspace.revision),
                )));
            }
        }
    }
}

fn effect_batch(mut effects: Vec<WorkspaceEffect>) -> Option<WorkspaceEffect> {
    match effects.len() {
        0 => None,
        1 => effects.pop(),
        _ => Some(WorkspaceEffect::Batch(effects)),
    }
}

fn defer_effects(state: &mut ActorState, effects: Vec<WorkspaceEffect>) {
    let Some(effect) = effect_batch(effects) else {
        return;
    };
    match state.effect_sender.try_send(effect) {
        Ok(()) => {}
        Err(TrySendError::Full(effect)) => {
            debug_assert!(state.deferred_effect.is_none());
            state.deferred_effect = Some(effect);
        }
        Err(TrySendError::Disconnected(_)) => {
            state.read_only_error = Some("workspace runtime effect worker stopped".into());
        }
    }
}

fn flush_deferred_effect(state: &mut ActorState) {
    let Some(effect) = state.deferred_effect.take() else {
        return;
    };
    match state.effect_sender.try_send(effect) {
        Ok(()) => {}
        Err(TrySendError::Full(effect)) => state.deferred_effect = Some(effect),
        Err(TrySendError::Disconnected(_)) => {
            state.read_only_error = Some("workspace runtime effect worker stopped".into());
        }
    }
}

fn prepare_mutation(
    state: &mut ActorState,
    request: &MutationRequest,
    fingerprint: String,
) -> std::result::Result<PendingCommit, ActorError> {
    if request.server_id != state.workspace.server_id {
        return Err(ActorError::new(
            ErrorCode::InvalidRequest,
            "mutation targets a different server workspace",
            Some(state.workspace.revision),
        ));
    }
    if request.expected_generation != state.generation {
        return Err(ActorError::new(
            ErrorCode::StaleGeneration,
            "mutation targets an obsolete server generation",
            Some(state.workspace.revision),
        ));
    }
    if request.expected_revision != state.workspace.revision {
        return Err(ActorError::new(
            ErrorCode::RevisionConflict,
            format!(
                "workspace revision {} is stale; current revision is {}",
                request.expected_revision, state.workspace.revision
            ),
            Some(state.workspace.revision),
        ));
    }
    if !state.workspace.pending_removals.is_empty()
        && !matches!(request.operation, WorkspaceMutation::EndSurface { .. })
    {
        return Err(ActorError::new(
            ErrorCode::Busy,
            "a workspace removal is awaiting process cleanup",
            Some(state.workspace.revision),
        ));
    }

    let mut candidate = state.workspace.clone();
    let mut affected_sessions = Vec::new();
    let mut affected_tabs = Vec::new();
    let mut affected_panes = Vec::new();
    let mut affected_surfaces = Vec::new();
    let mut effects = Vec::new();
    let mut next_ordinal = state.next_ordinal;
    if let Some(context) = &request.launch {
        validate_launch_context(context).map_err(|message| {
            ActorError::new(
                ErrorCode::InvalidRequest,
                message,
                Some(state.workspace.revision),
            )
        })?;
        if !matches!(
            request.operation,
            WorkspaceMutation::Initialize { .. }
                | WorkspaceMutation::CreateTab { .. }
                | WorkspaceMutation::SplitPane { .. }
                | WorkspaceMutation::GrowTab { .. }
                | WorkspaceMutation::RestartSurface { .. }
        ) {
            return Err(ActorError::new(
                ErrorCode::InvalidRequest,
                "launch context requires a launch operation",
                Some(state.workspace.revision),
            ));
        }
    }
    let operation_state = apply_mutation(
        &mut candidate,
        &request.operation,
        &mut next_ordinal,
        &mut affected_sessions,
        &mut affected_tabs,
        &mut affected_panes,
        &mut affected_surfaces,
        &mut effects,
    )?;
    for effect in &mut effects {
        if let WorkspaceEffect::Launch(info, context) = effect {
            *context = request.launch.clone();
            if let Some(context) = context {
                info.launch.profile = Some(Box::new(context.profile.clone()));
            }
            if let Some(surface) = candidate
                .surfaces
                .iter_mut()
                .find(|surface| surface.id == info.id)
            {
                surface.launch = info.launch.clone();
            }
        }
    }
    candidate.revision = candidate.revision.saturating_add(1);
    let receipt = MutationReceipt {
        mutation_id: request.mutation_id.clone(),
        fingerprint,
        revision: candidate.revision,
        affected_sessions,
        affected_tabs,
        affected_panes,
        affected_surfaces,
        operation_state,
    };
    candidate.receipts.push(receipt.clone());
    if candidate.receipts.len() > MAX_MUTATION_RECEIPTS {
        let remove = candidate.receipts.len() - MAX_MUTATION_RECEIPTS;
        candidate.receipts.drain(..remove);
    }
    crate::workspace_store::validate(&candidate).map_err(|error| {
        ActorError::new(
            ErrorCode::InvalidRequest,
            format!("invalid workspace mutation: {error}"),
            Some(state.workspace.revision),
        )
    })?;
    state.next_ordinal = next_ordinal;
    Ok(PendingCommit {
        candidate,
        receipt: Some(receipt),
        effects,
        prelaunched: Vec::new(),
        reply: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn apply_mutation(
    workspace: &mut StoredWorkspace,
    operation: &WorkspaceMutation,
    ordinal: &mut u64,
    affected_sessions: &mut Vec<SessionId>,
    affected_tabs: &mut Vec<TabId>,
    affected_panes: &mut Vec<PaneId>,
    affected_surfaces: &mut Vec<SurfaceId>,
    effects: &mut Vec<WorkspaceEffect>,
) -> std::result::Result<String, ActorError> {
    let invalid = |message: String| {
        ActorError::new(ErrorCode::InvalidRequest, message, Some(workspace.revision))
    };
    match operation {
        WorkspaceMutation::Initialize {
            cols,
            rows,
            working_directory,
        } => {
            if workspace.initialized {
                return Err(invalid("workspace is already initialized".into()));
            }
            dimensions(*cols, *rows).map_err(invalid)?;
            let session_id = SessionId::new(allocate("session", ordinal));
            let tab_id = TabId::new(allocate("tab", ordinal));
            let pane_id = PaneId::new(allocate("pane", ordinal));
            let surface = new_surface(*cols, *rows, working_directory.clone(), ordinal);
            workspace.sessions.push(WorkspaceSession {
                id: session_id.clone(),
                label: "Default".into(),
                tabs: vec![WorkspaceTab {
                    id: tab_id.clone(),
                    label: String::new(),
                    layout: LayoutNode::Pane {
                        pane_id: pane_id.clone(),
                        surface_id: surface.id.clone(),
                    },
                    previous_layout: None,
                    merge: None,
                }],
            });
            workspace.initialized = true;
            affected_sessions.push(session_id);
            affected_tabs.push(tab_id);
            affected_panes.push(pane_id);
            affected_surfaces.push(surface.id.clone());
            effects.push(WorkspaceEffect::Launch(surface.clone(), None));
            workspace.surfaces.push(surface);
            Ok("starting".into())
        }
        WorkspaceMutation::CreateSession { label } => {
            validate_label(label, true).map_err(invalid)?;
            let id = SessionId::new(allocate("session", ordinal));
            workspace.sessions.push(WorkspaceSession {
                id: id.clone(),
                label: label.clone(),
                tabs: Vec::new(),
            });
            workspace.initialized = true;
            affected_sessions.push(id);
            Ok("committed".into())
        }
        WorkspaceMutation::RenameSession { session_id, label } => {
            validate_label(label, true).map_err(invalid)?;
            let session = workspace
                .sessions
                .iter_mut()
                .find(|session| &session.id == session_id)
                .ok_or_else(|| invalid(format!("session {session_id} was not found")))?;
            session.label = label.clone();
            affected_sessions.push(session_id.clone());
            Ok("committed".into())
        }
        WorkspaceMutation::CreateTab {
            session_id,
            label,
            cols,
            rows,
            working_directory,
        } => {
            validate_label(label, false).map_err(invalid)?;
            dimensions(*cols, *rows).map_err(invalid)?;
            let tab_id = TabId::new(allocate("tab", ordinal));
            let pane_id = PaneId::new(allocate("pane", ordinal));
            let surface = new_surface(*cols, *rows, working_directory.clone(), ordinal);
            let session = workspace
                .sessions
                .iter_mut()
                .find(|session| &session.id == session_id)
                .ok_or_else(|| invalid(format!("session {session_id} was not found")))?;
            session.tabs.push(WorkspaceTab {
                id: tab_id.clone(),
                label: label.clone(),
                layout: LayoutNode::Pane {
                    pane_id: pane_id.clone(),
                    surface_id: surface.id.clone(),
                },
                previous_layout: None,
                merge: None,
            });
            affected_sessions.push(session_id.clone());
            affected_tabs.push(tab_id);
            affected_panes.push(pane_id);
            affected_surfaces.push(surface.id.clone());
            effects.push(WorkspaceEffect::Launch(surface.clone(), None));
            workspace.surfaces.push(surface);
            Ok("starting".into())
        }
        WorkspaceMutation::RenameTab { tab_id, label } => {
            validate_label(label, false).map_err(invalid)?;
            let tab = find_tab_mut(&mut workspace.sessions, tab_id)
                .ok_or_else(|| invalid(format!("tab {tab_id} was not found")))?;
            tab.label = label.clone();
            affected_tabs.push(tab_id.clone());
            Ok("committed".into())
        }
        WorkspaceMutation::MoveTab {
            session_id,
            tab_id,
            index,
        } => {
            let session = workspace
                .sessions
                .iter_mut()
                .find(|session| &session.id == session_id)
                .ok_or_else(|| invalid(format!("session {session_id} was not found")))?;
            let old = session
                .tabs
                .iter()
                .position(|tab| &tab.id == tab_id)
                .ok_or_else(|| invalid(format!("tab {tab_id} was not found in session")))?;
            let tab = session.tabs.remove(old);
            let index = (*index).min(session.tabs.len());
            session.tabs.insert(index, tab);
            affected_sessions.push(session_id.clone());
            affected_tabs.push(tab_id.clone());
            Ok("committed".into())
        }
        WorkspaceMutation::SplitPane {
            pane_id,
            axis,
            cols,
            rows,
            working_directory,
            geometry,
        } => {
            dimensions(*cols, *rows).map_err(invalid)?;
            validate_split_geometry(geometry, *axis).map_err(invalid)?;
            let new_pane = PaneId::new(allocate("pane", ordinal));
            let surface = new_surface(*cols, *rows, working_directory.clone(), ordinal);
            let mut replaced = false;
            for session in &mut workspace.sessions {
                for tab in &mut session.tabs {
                    if split_pane(
                        &mut tab.layout,
                        pane_id,
                        *axis,
                        new_pane.clone(),
                        surface.id.clone(),
                    ) {
                        affected_sessions.push(session.id.clone());
                        affected_tabs.push(tab.id.clone());
                        replaced = true;
                        break;
                    }
                }
                if replaced {
                    break;
                }
            }
            if !replaced {
                return Err(invalid(format!("pane {pane_id} was not found")));
            }
            affected_panes.extend([pane_id.clone(), new_pane]);
            affected_surfaces.push(surface.id.clone());
            effects.push(WorkspaceEffect::Launch(surface.clone(), None));
            workspace.surfaces.push(surface);
            Ok("starting".into())
        }
        WorkspaceMutation::GrowTab {
            tab_id,
            layout,
            working_directory,
        } => {
            let (session_index, tab_index) = workspace
                .sessions
                .iter()
                .enumerate()
                .find_map(|(session_index, session)| {
                    session
                        .tabs
                        .iter()
                        .position(|tab| &tab.id == tab_id)
                        .map(|tab_index| (session_index, tab_index))
                })
                .ok_or_else(|| invalid(format!("tab {tab_id} was not found")))?;
            let tab = &workspace.sessions[session_index].tabs[tab_index];
            let mut leaves = Vec::new();
            collect_leaves(&tab.layout, &mut leaves);
            let existing: std::collections::HashMap<_, _> = leaves.into_iter().collect();
            let extent = tab_cell_extent(&tab.layout, &workspace.surfaces).map_err(invalid)?;
            let mut seen = HashSet::new();
            let mut count = 0;
            validate_growth_plan(layout, extent, &existing, &mut seen, &mut count, 0)
                .map_err(invalid)?;
            if seen.len() != existing.len() {
                return Err(invalid(
                    "split never deletes existing panes; include every pane exactly once".into(),
                ));
            }
            let mut created = Vec::with_capacity(count - existing.len());
            let proposed = materialize_growth_plan(
                layout,
                extent,
                &existing,
                working_directory,
                ordinal,
                &mut created,
            );
            affected_sessions.push(workspace.sessions[session_index].id.clone());
            affected_tabs.push(tab_id.clone());
            affected_panes.extend(pane_ids(&proposed));
            collect_surfaces(&proposed, affected_surfaces);
            let tab = &mut workspace.sessions[session_index].tabs[tab_index];
            if tab.layout != proposed {
                tab.previous_layout = Some(Box::new(std::mem::replace(&mut tab.layout, proposed)));
            }
            let starting = !created.is_empty();
            for surface in created {
                effects.push(WorkspaceEffect::Launch(surface.clone(), None));
                workspace.surfaces.push(surface);
            }
            Ok(if starting { "starting" } else { "committed" }.into())
        }
        WorkspaceMutation::DetachPane { pane_id } => {
            for session in &mut workspace.sessions {
                let Some(index) = session
                    .tabs
                    .iter()
                    .position(|tab| contains_pane(&tab.layout, pane_id))
                else {
                    continue;
                };
                let tab = &mut session.tabs[index];
                if matches!(tab.layout, LayoutNode::Pane { .. }) {
                    return Err(invalid(format!(
                        "pane {pane_id} is the only pane in its tab"
                    )));
                }
                let detached =
                    extract_pane(&mut tab.layout, pane_id).expect("pane was found in a split tab");
                forget_pane_history(tab, pane_id);
                let LayoutNode::Pane { surface_id, .. } = &detached else {
                    unreachable!("only a leaf can be detached");
                };
                let surface_id = surface_id.clone();
                let old_tab_id = tab.id.clone();
                let new_tab_id = TabId::new(allocate("tab", ordinal));
                session.tabs.insert(
                    index + 1,
                    WorkspaceTab {
                        id: new_tab_id.clone(),
                        label: String::new(),
                        layout: detached,
                        previous_layout: None,
                        merge: None,
                    },
                );
                affected_sessions.push(session.id.clone());
                affected_tabs.extend([new_tab_id, old_tab_id]);
                affected_panes.push(pane_id.clone());
                affected_surfaces.push(surface_id);
                return Ok("committed".into());
            }
            Err(invalid(format!("pane {pane_id} was not found")))
        }
        WorkspaceMutation::SetSplitRatio {
            tab_id,
            path,
            ratio,
        } => {
            if !ratio.is_finite() || *ratio <= 0.0 || *ratio >= 1.0 {
                return Err(invalid(
                    "split ratio must be finite and strictly between zero and one".into(),
                ));
            }
            let tab = find_tab_mut(&mut workspace.sessions, tab_id)
                .ok_or_else(|| invalid(format!("tab {tab_id} was not found")))?;
            if !set_split_ratio(&mut tab.layout, path, *ratio) {
                return Err(invalid(format!(
                    "split path does not identify a divider in tab {tab_id}"
                )));
            }
            affected_tabs.push(tab_id.clone());
            // Paths are revision-scoped structural addresses, not pane identities.
            Ok("committed".into())
        }
        WorkspaceMutation::ArrangeTab { tab_id, layout } => {
            let tab = find_tab_mut(&mut workspace.sessions, tab_id)
                .ok_or_else(|| invalid(format!("tab {tab_id} was not found")))?;
            if !same_leaves(&[&tab.layout], layout) {
                return Err(invalid(format!(
                    "arrangement must contain exactly the panes of tab {tab_id}"
                )));
            }
            if &tab.layout == layout {
                affected_tabs.push(tab_id.clone());
                return Ok("unchanged".into());
            }
            // Ratios are validated with the whole candidate before commit.
            let previous = std::mem::replace(&mut tab.layout, layout.clone());
            tab.previous_layout = Some(Box::new(previous));
            affected_tabs.push(tab_id.clone());
            affected_panes.extend(pane_ids(layout));
            Ok("committed".into())
        }
        WorkspaceMutation::MergeTabs {
            tab_id,
            sources,
            layout,
        } => {
            let session = workspace
                .sessions
                .iter_mut()
                .find(|session| session.tabs.iter().any(|tab| &tab.id == tab_id))
                .ok_or_else(|| invalid(format!("tab {tab_id} was not found")))?;
            let mut unique = std::collections::HashSet::new();
            if sources.is_empty()
                || sources
                    .iter()
                    .any(|source| source == tab_id || !unique.insert(source))
            {
                return Err(invalid(
                    "merge needs one or more distinct tabs other than the receiving tab".into(),
                ));
            }
            let mut source_indices = Vec::with_capacity(sources.len());
            for source in sources {
                source_indices.push(
                    session
                        .tabs
                        .iter()
                        .position(|tab| &tab.id == source)
                        .ok_or_else(|| {
                            invalid(format!(
                                "tab {source} is not in the receiving tab's workspace"
                            ))
                        })?,
                );
            }
            source_indices.sort_unstable();
            let target_index = session
                .tabs
                .iter()
                .position(|tab| &tab.id == tab_id)
                .expect("the session contains the receiving tab");
            let expected: Vec<_> = std::iter::once(target_index)
                .chain(source_indices.iter().copied())
                .map(|index| &session.tabs[index].layout)
                .collect();
            if !same_leaves(&expected, layout) {
                return Err(invalid(
                    "merged arrangement must contain exactly the panes of the merged tabs".into(),
                ));
            }
            let target = &session.tabs[target_index];
            let mut merge = target
                .merge
                .as_deref()
                .cloned()
                .unwrap_or_else(|| TabMerge {
                    own_layout: Some(target.layout.clone()),
                    tabs: Vec::new(),
                });
            // A source that was itself a merge contributes its own original
            // tree and every tab merged into it, so one split restores them all.
            for &index in &source_indices {
                let source = &session.tabs[index];
                let own = match source.merge.as_deref() {
                    Some(nested) => nested.own_layout.clone(),
                    None => Some(source.layout.clone()),
                };
                if let Some(own) = own {
                    merge.tabs.push(MergedTab {
                        id: source.id.clone(),
                        label: source.label.clone(),
                        layout: own,
                        index,
                    });
                }
                if let Some(nested) = source.merge.as_deref() {
                    merge.tabs.extend(nested.tabs.iter().cloned());
                }
            }
            let source_ids: Vec<TabId> = source_indices
                .iter()
                .map(|&index| session.tabs[index].id.clone())
                .collect();
            let target = &mut session.tabs[target_index];
            target.layout = layout.clone();
            target.previous_layout = None;
            target.merge = Some(Box::new(merge));
            session.tabs.retain(|tab| !source_ids.contains(&tab.id));
            affected_sessions.push(session.id.clone());
            affected_tabs.push(tab_id.clone());
            affected_tabs.extend(source_ids);
            affected_panes.extend(pane_ids(layout));
            Ok("committed".into())
        }
        WorkspaceMutation::SplitMergedTabs { tab_id } => {
            let session = workspace
                .sessions
                .iter_mut()
                .find(|session| session.tabs.iter().any(|tab| &tab.id == tab_id))
                .ok_or_else(|| invalid(format!("tab {tab_id} was not found")))?;
            let target_index = session
                .tabs
                .iter()
                .position(|tab| &tab.id == tab_id)
                .expect("the session contains the receiving tab");
            let target = &mut session.tabs[target_index];
            let Some(merge) = target.merge.take() else {
                return Err(invalid(format!("tab {tab_id} has no merged tabs to split")));
            };
            let TabMerge {
                own_layout,
                mut tabs,
            } = *merge;
            // Records hold only panes still in this tab, so every recorded pane moves.
            let mut remaining = Some(target.layout.clone());
            for record in &tabs {
                for pane in pane_ids(&record.layout) {
                    affected_panes.push(pane.clone());
                    remaining = remaining.and_then(|tree| remove_pane(tree, &pane));
                }
            }
            match remaining {
                Some(remaining) => {
                    // Without panes added since the merge, return to the original tree.
                    target.layout = match own_layout {
                        Some(own) if same_leaves(&[&own], &remaining) => own,
                        _ => remaining,
                    };
                    target.previous_layout = None;
                }
                None => {
                    session.tabs.remove(target_index);
                }
            }
            tabs.sort_by_key(|record| record.index);
            affected_sessions.push(session.id.clone());
            affected_tabs.push(tab_id.clone());
            for record in tabs {
                let index = record.index.min(session.tabs.len());
                affected_tabs.push(record.id.clone());
                session.tabs.insert(
                    index,
                    WorkspaceTab {
                        id: record.id,
                        label: record.label,
                        layout: record.layout,
                        previous_layout: None,
                        merge: None,
                    },
                );
            }
            Ok("committed".into())
        }
        WorkspaceMutation::EndSurface {
            surface_id,
            expected_lifetime,
        } => {
            let surface = workspace
                .surfaces
                .iter_mut()
                .find(|surface| &surface.id == surface_id)
                .ok_or_else(|| invalid(format!("surface {surface_id} was not found")))?;
            verify_lifetime(surface, expected_lifetime, workspace.revision)?;
            if !matches!(
                surface.status,
                SurfaceStatus::Starting | SurfaceStatus::Running | SurfaceStatus::Ending
            ) {
                return Err(ActorError::new(
                    ErrorCode::SurfaceUnavailable,
                    format!("surface {surface_id} is not running or awaiting cleanup"),
                    Some(workspace.revision),
                ));
            }
            surface.status = SurfaceStatus::Ending;
            surface.error = None;
            effects.push(WorkspaceEffect::End {
                surface_id: surface_id.clone(),
                process_lifetime_id: expected_lifetime.clone(),
            });
            affected_surfaces.push(surface_id.clone());
            Ok("ending".into())
        }
        WorkspaceMutation::RestartSurface {
            surface_id,
            expected_lifetime,
            cols,
            rows,
        } => {
            dimensions(*cols, *rows).map_err(invalid)?;
            let surface = workspace
                .surfaces
                .iter_mut()
                .find(|surface| &surface.id == surface_id)
                .ok_or_else(|| invalid(format!("surface {surface_id} was not found")))?;
            verify_lifetime(surface, expected_lifetime, workspace.revision)?;
            if !matches!(
                surface.status,
                SurfaceStatus::Exited | SurfaceStatus::Failed | SurfaceStatus::Lost
            ) {
                return Err(ActorError::new(
                    ErrorCode::SurfaceUnavailable,
                    format!(
                        "surface {surface_id} cannot restart from {:?}",
                        surface.status
                    ),
                    Some(workspace.revision),
                ));
            }
            // A shell lost to a daemon restart resumes where the user last was, if that
            // directory still exists; ended or failed shells restart from their recorded
            // launch.
            let resume = (surface.status == SurfaceStatus::Lost)
                .then(|| workspace.last_directories.get(surface_id).cloned())
                .flatten()
                .filter(|directory| resume_directory_exists(surface, directory));
            surface.process_lifetime_id = ProcessLifetimeId::new(allocate("lifetime", ordinal));
            surface.status = SurfaceStatus::Starting;
            surface.attached = false;
            surface.cols = *cols;
            surface.rows = *rows;
            surface.exit_code = None;
            surface.error = None;
            let mut launch = surface.clone();
            if resume.is_some() {
                launch.launch.working_directory = resume;
            }
            effects.push(WorkspaceEffect::Launch(launch, None));
            affected_surfaces.push(surface_id.clone());
            Ok("starting".into())
        }
        WorkspaceMutation::RemovePane { pane_id } => prepare_removal(
            workspace,
            RemovalTarget::Pane {
                pane_id: pane_id.clone(),
            },
            affected_surfaces,
            effects,
        ),
        WorkspaceMutation::RemoveTab { tab_id } => prepare_removal(
            workspace,
            RemovalTarget::Tab {
                tab_id: tab_id.clone(),
            },
            affected_surfaces,
            effects,
        ),
        WorkspaceMutation::RemoveSession { session_id } => prepare_removal(
            workspace,
            RemovalTarget::Session {
                session_id: session_id.clone(),
            },
            affected_surfaces,
            effects,
        ),
    }
}

/// Whether a shell's last reported directory still exists where it ran. A WSL shell's
/// path is checked through its distribution's share; without a recorded distribution it
/// cannot be checked, and the launch itself reports a missing directory.
fn resume_directory_exists(surface: &SurfaceInfo, directory: &str) -> bool {
    #[cfg(windows)]
    {
        let Some(distribution) = surface
            .working_directory
            .as_ref()
            .map(|directory| directory.distribution.as_str())
        else {
            return true;
        };
        let share = format!(
            r"\\wsl.localhost\{distribution}{}",
            directory.replace('/', "\\")
        );
        std::fs::metadata(share).is_ok_and(|metadata| metadata.is_dir())
    }
    #[cfg(unix)]
    {
        let _ = surface;
        std::path::Path::new(directory).is_dir()
    }
}

fn prepare_removal(
    workspace: &mut StoredWorkspace,
    target: RemovalTarget,
    affected_surfaces: &mut Vec<SurfaceId>,
    effects: &mut Vec<WorkspaceEffect>,
) -> std::result::Result<String, ActorError> {
    if let RemovalTarget::Pane { pane_id } = &target {
        let removable = workspace
            .sessions
            .iter()
            .flat_map(|session| &session.tabs)
            .any(|tab| {
                contains_pane(&tab.layout, pane_id)
                    && matches!(tab.layout, LayoutNode::Split { .. })
            });
        if !removable {
            return Err(ActorError::new(
                ErrorCode::InvalidRequest,
                format!("pane {pane_id} is not removable from its tab"),
                Some(workspace.revision),
            ));
        }
    }
    let ids = surfaces_for_target(workspace, &target).ok_or_else(|| {
        ActorError::new(
            ErrorCode::InvalidRequest,
            "removal target was not found",
            Some(workspace.revision),
        )
    })?;
    let mut pending = Vec::new();
    for id in &ids {
        let Some(surface) = workspace
            .surfaces
            .iter_mut()
            .find(|surface| &surface.id == id)
        else {
            continue;
        };
        affected_surfaces.push(id.clone());
        if matches!(
            surface.status,
            SurfaceStatus::Starting | SurfaceStatus::Running | SurfaceStatus::Ending
        ) {
            if surface.status != SurfaceStatus::Ending {
                surface.status = SurfaceStatus::Ending;
                effects.push(WorkspaceEffect::End {
                    surface_id: id.clone(),
                    process_lifetime_id: surface.process_lifetime_id.clone(),
                });
            }
            pending.push((id.clone(), surface.process_lifetime_id.clone()));
        }
    }
    if pending.is_empty() {
        apply_removal(workspace, &target);
        Ok("removed".into())
    } else {
        workspace.pending_removals.push(PendingRemoval {
            target,
            surfaces: pending,
        });
        Ok("ending".into())
    }
}

fn apply_observation(
    workspace: &StoredWorkspace,
    observation: RuntimeObservation,
) -> Option<StoredWorkspace> {
    let mut candidate = workspace.clone();
    let (surface_id, lifetime) = match &observation {
        RuntimeObservation::Running {
            surface_id,
            process_lifetime_id,
            ..
        }
        | RuntimeObservation::Resized {
            surface_id,
            process_lifetime_id,
            ..
        }
        | RuntimeObservation::Exited {
            surface_id,
            process_lifetime_id,
            ..
        }
        | RuntimeObservation::Failed {
            surface_id,
            process_lifetime_id,
            ..
        }
        | RuntimeObservation::EndFailed {
            surface_id,
            process_lifetime_id,
            ..
        }
        | RuntimeObservation::Directory {
            surface_id,
            process_lifetime_id,
            ..
        } => (surface_id, process_lifetime_id),
    };
    let surface = candidate
        .surfaces
        .iter_mut()
        .find(|surface| &surface.id == surface_id)?;
    if &surface.process_lifetime_id != lifetime {
        return None;
    }
    match observation {
        RuntimeObservation::Running {
            working_directory, ..
        } => {
            if surface.status != SurfaceStatus::Starting {
                return None;
            }
            surface.status = SurfaceStatus::Running;
            surface.working_directory = working_directory;
            surface.error = None;
        }
        RuntimeObservation::Resized { cols, rows, .. } => {
            surface.cols = cols;
            surface.rows = rows;
        }
        RuntimeObservation::Exited { exit_code, .. } => {
            surface.status = SurfaceStatus::Exited;
            surface.attached = false;
            surface.exit_code = Some(exit_code);
            surface.error = None;
        }
        RuntimeObservation::Failed { error, .. } => {
            surface.status = SurfaceStatus::Failed;
            surface.attached = false;
            surface.exit_code = None;
            surface.error = Some(error);
        }
        RuntimeObservation::EndFailed { error, .. } => {
            surface.status = SurfaceStatus::Ending;
            surface.error = Some(error);
        }
        RuntimeObservation::Directory { directory, .. } => {
            // Clients never see this, so no revision bump and no refresh for them.
            let id = surface.id.clone();
            if candidate.last_directories.get(&id) == Some(&directory) {
                return None;
            }
            candidate.last_directories.insert(id, directory);
            return Some(candidate);
        }
    }
    let completed: Vec<_> = candidate
        .pending_removals
        .iter()
        .filter(|removal| {
            removal.surfaces.iter().all(|(id, lifetime)| {
                candidate.surfaces.iter().any(|surface| {
                    &surface.id == id
                        && &surface.process_lifetime_id == lifetime
                        && matches!(
                            surface.status,
                            SurfaceStatus::Exited | SurfaceStatus::Failed | SurfaceStatus::Lost
                        )
                })
            })
        })
        .map(|removal| removal.target.clone())
        .collect();
    if !completed.is_empty() {
        candidate
            .pending_removals
            .retain(|removal| !completed.contains(&removal.target));
        for target in completed {
            apply_removal(&mut candidate, &target);
        }
    }
    candidate.revision = candidate.revision.saturating_add(1);
    Some(candidate)
}

fn new_surface(
    cols: i16,
    rows: i16,
    working_directory: Option<String>,
    ordinal: &mut u64,
) -> SurfaceInfo {
    SurfaceInfo {
        id: SurfaceId::new(allocate("surface", ordinal)),
        process_lifetime_id: ProcessLifetimeId::new(allocate("lifetime", ordinal)),
        status: SurfaceStatus::Starting,
        attached: false,
        cols,
        rows,
        created_at_ms: now_ms(),
        exit_code: None,
        error: None,
        launch: LaunchRequest {
            working_directory,
            profile: None,
        },
        working_directory: None,
    }
}

fn find_tab_mut<'a>(
    sessions: &'a mut [WorkspaceSession],
    tab_id: &TabId,
) -> Option<&'a mut WorkspaceTab> {
    sessions
        .iter_mut()
        .flat_map(|session| &mut session.tabs)
        .find(|tab| &tab.id == tab_id)
}

fn split_pane(
    node: &mut LayoutNode,
    target: &PaneId,
    axis: SplitAxis,
    new_pane: PaneId,
    new_surface: SurfaceId,
) -> bool {
    match node {
        LayoutNode::Pane { pane_id, .. } if pane_id == target => {
            let original = node.clone();
            *node = LayoutNode::Split {
                axis,
                ratio: 0.5,
                first: Box::new(original),
                second: Box::new(LayoutNode::Pane {
                    pane_id: new_pane,
                    surface_id: new_surface,
                }),
            };
            true
        }
        LayoutNode::Pane { .. } => false,
        LayoutNode::Split { first, second, .. } => {
            split_pane(first, target, axis, new_pane.clone(), new_surface.clone())
                || split_pane(second, target, axis, new_pane, new_surface)
        }
    }
}

fn set_split_ratio(mut node: &mut LayoutNode, path: &[bool], ratio: f32) -> bool {
    for second_child in path {
        let LayoutNode::Split { first, second, .. } = node else {
            return false;
        };
        node = if *second_child { second } else { first };
    }
    if let LayoutNode::Split { ratio: current, .. } = node {
        *current = ratio;
        true
    } else {
        false
    }
}

/// Infer a conservative tab rectangle from current measured terminal cell sizes.
/// Cap split-axis sums by both children's ratio-implied extents: a child awaiting
/// resize after an earlier split must not overstate the tab's available space.
fn tab_cell_extent(node: &LayoutNode, surfaces: &[SurfaceInfo]) -> Result<(i32, i32), String> {
    match node {
        LayoutNode::Pane { surface_id, .. } => {
            let surface = surfaces
                .iter()
                .find(|surface| &surface.id == surface_id)
                .ok_or_else(|| format!("pane surface {surface_id} was not found"))?;
            Ok((i32::from(surface.cols), i32::from(surface.rows)))
        }
        LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let (a_cols, a_rows) = tab_cell_extent(first, surfaces)?;
            let (b_cols, b_rows) = tab_cell_extent(second, surfaces)?;
            // For N usable cells, a=floor(N*r) and b=ceil(N*(1-r)).
            // Their inverse upper bounds must include the fractional cell lost
            // by the first child; otherwise valid layouts shrink on every reuse.
            let combined = |a: i32, b: i32| {
                (a + b + 1)
                    .min((f64::from(a + 1) / f64::from(*ratio)).ceil() as i32)
                    .min(
                        ((f64::from(b) / (1.0 - f64::from(*ratio))).floor() as i32)
                            .saturating_add(1),
                    )
            };
            Ok(match axis {
                SplitAxis::Horizontal => (combined(a_cols, b_cols), a_rows.min(b_rows)),
                SplitAxis::Vertical => (a_cols.min(b_cols), combined(a_rows, b_rows)),
            })
        }
    }
}

fn split_cell_extent(extent: (i32, i32), axis: SplitAxis, ratio: f32) -> ((i32, i32), (i32, i32)) {
    let (cols, rows) = extent;
    match axis {
        SplitAxis::Horizontal => {
            let first = ((cols - 1) as f64 * f64::from(ratio)).floor() as i32;
            ((first, rows), (cols - 1 - first, rows))
        }
        SplitAxis::Vertical => {
            let first = ((rows - 1) as f64 * f64::from(ratio)).floor() as i32;
            ((cols, first), (cols, rows - 1 - first))
        }
    }
}

fn validate_growth_plan<'a>(
    plan: &'a PlannedLayoutNode,
    extent: (i32, i32),
    existing: &std::collections::HashMap<&PaneId, &SurfaceId>,
    seen: &mut HashSet<&'a PaneId>,
    count: &mut usize,
    depth: usize,
) -> Result<(), String> {
    if depth >= MAX_TAB_PANES {
        return Err("tab layout exceeds the pane count/depth limit".into());
    }
    match plan {
        PlannedLayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            if !ratio.is_finite() || *ratio <= 0.0 || *ratio >= 1.0 {
                return Err("split ratios must be finite and strictly between zero and one".into());
            }
            let (a, b) = split_cell_extent(extent, *axis, *ratio);
            validate_growth_plan(first, a, existing, seen, count, depth + 1)?;
            validate_growth_plan(second, b, existing, seen, count, depth + 1)
        }
        PlannedLayoutNode::ExistingPane { .. } | PlannedLayoutNode::NewPane => {
            *count += 1;
            if *count > MAX_TAB_PANES {
                return Err(format!("a tab can hold at most {MAX_TAB_PANES} panes"));
            }
            if let PlannedLayoutNode::ExistingPane { pane_id } = plan
                && (!existing.contains_key(pane_id) || !seen.insert(pane_id))
            {
                return Err("growth layout must include each existing pane exactly once".into());
            }
            if !(20..=1_000).contains(&extent.0) || !(4..=1_000).contains(&extent.1) {
                return Err(
                    "layout cannot fit minimum 20-column by 4-row panes; enlarge the tab first"
                        .into(),
                );
            }
            Ok(())
        }
    }
}

fn materialize_growth_plan(
    plan: &PlannedLayoutNode,
    extent: (i32, i32),
    existing: &std::collections::HashMap<&PaneId, &SurfaceId>,
    working_directory: &Option<String>,
    ordinal: &mut u64,
    created: &mut Vec<SurfaceInfo>,
) -> LayoutNode {
    match plan {
        PlannedLayoutNode::ExistingPane { pane_id } => LayoutNode::Pane {
            pane_id: pane_id.clone(),
            surface_id: (*existing[pane_id]).clone(),
        },
        PlannedLayoutNode::NewPane => {
            let pane_id = PaneId::new(allocate("pane", ordinal));
            let surface = new_surface(
                extent.0 as i16,
                extent.1 as i16,
                working_directory.clone(),
                ordinal,
            );
            let surface_id = surface.id.clone();
            created.push(surface);
            LayoutNode::Pane {
                pane_id,
                surface_id,
            }
        }
        PlannedLayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let (a, b) = split_cell_extent(extent, *axis, *ratio);
            LayoutNode::Split {
                axis: *axis,
                ratio: *ratio,
                first: Box::new(materialize_growth_plan(
                    first,
                    a,
                    existing,
                    working_directory,
                    ordinal,
                    created,
                )),
                second: Box::new(materialize_growth_plan(
                    second,
                    b,
                    existing,
                    working_directory,
                    ordinal,
                    created,
                )),
            }
        }
    }
}

fn validate_split_geometry(
    geometry: &compi_protocol::SplitGeometry,
    axis: SplitAxis,
) -> Result<(), String> {
    let values = [
        geometry.width,
        geometry.height,
        geometry.min_width,
        geometry.min_height,
        geometry.divider,
    ];
    if values
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0 || *value > 1_000_000.0)
    {
        return Err("split geometry must contain finite positive dimensions".into());
    }
    let (width, height) = match axis {
        SplitAxis::Horizontal => (
            2.0 * geometry.min_width + geometry.divider,
            geometry.min_height,
        ),
        SplitAxis::Vertical => (
            geometry.min_width,
            2.0 * geometry.min_height + geometry.divider,
        ),
    };
    if geometry.width < width || geometry.height < height {
        return Err("focused pane cannot contain two minimum-sized children".into());
    }
    Ok(())
}

fn validate_launch_context(context: &compi_protocol::LaunchContext) -> Result<(), String> {
    if context.scrollback_lines > 100_000
        || context.graphics_bytes > compi_protocol::MAX_GRAPHICS_BYTES
    {
        return Err("launch resource limits exceed supported bounds".into());
    }
    let profile = &context.profile;
    if profile
        .executable
        .as_ref()
        .is_some_and(|value| value.trim().is_empty() || value.contains('\0'))
        || profile
            .working_directory
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.contains('\0'))
        || profile
            .distribution
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.contains('\0'))
        || profile.args.len() > 256
        || profile.args.iter().any(|value| value.contains('\0'))
        || context.env.len() > 256
        || context
            .env
            .iter()
            .any(|(key, value)| key.is_empty() || key.contains(['=', '\0']) || value.contains('\0'))
    {
        return Err("invalid launch profile or environment entry".into());
    }
    Ok(())
}

fn contains_pane(node: &LayoutNode, target: &PaneId) -> bool {
    match node {
        LayoutNode::Pane { pane_id, .. } => pane_id == target,
        LayoutNode::Split { first, second, .. } => {
            contains_pane(first, target) || contains_pane(second, target)
        }
    }
}

// Remove a leaf by promoting its sibling without rebuilding ancestor splits.
fn extract_pane(node: &mut LayoutNode, target: &PaneId) -> Option<LayoutNode> {
    let LayoutNode::Split { first, second, .. } = node else {
        return None;
    };
    let child = match (&**first, &**second) {
        (
            LayoutNode::Pane {
                pane_id,
                surface_id,
            },
            _,
        ) if pane_id == target => Some((true, pane_id, surface_id)),
        (
            _,
            LayoutNode::Pane {
                pane_id,
                surface_id,
            },
        ) if pane_id == target => Some((false, pane_id, surface_id)),
        _ => None,
    };
    if let Some((take_first, pane_id, surface_id)) = child {
        let placeholder = LayoutNode::Pane {
            pane_id: pane_id.clone(),
            surface_id: surface_id.clone(),
        };
        let LayoutNode::Split { first, second, .. } = std::mem::replace(node, placeholder) else {
            unreachable!("the node was a split");
        };
        return if take_first {
            *node = *second;
            Some(*first)
        } else {
            *node = *first;
            Some(*second)
        };
    }
    extract_pane(first, target).or_else(|| extract_pane(second, target))
}

fn collect_surfaces(node: &LayoutNode, output: &mut Vec<SurfaceId>) {
    match node {
        LayoutNode::Pane { surface_id, .. } => output.push(surface_id.clone()),
        LayoutNode::Split { first, second, .. } => {
            collect_surfaces(first, output);
            collect_surfaces(second, output);
        }
    }
}

fn collect_leaves<'a>(node: &'a LayoutNode, output: &mut Vec<(&'a PaneId, &'a SurfaceId)>) {
    match node {
        LayoutNode::Pane {
            pane_id,
            surface_id,
        } => output.push((pane_id, surface_id)),
        LayoutNode::Split { first, second, .. } => {
            collect_leaves(first, output);
            collect_leaves(second, output);
        }
    }
}

fn pane_ids(node: &LayoutNode) -> Vec<PaneId> {
    let mut leaves = Vec::new();
    collect_leaves(node, &mut leaves);
    leaves.into_iter().map(|(pane, _)| pane.clone()).collect()
}

/// Whether `proposed` holds each pane/surface leaf of `expected` exactly once
/// and nothing else, so committing it can only move existing panes.
fn same_leaves(expected: &[&LayoutNode], proposed: &LayoutNode) -> bool {
    let mut current = Vec::new();
    for tree in expected {
        collect_leaves(tree, &mut current);
    }
    let mut candidate = Vec::new();
    collect_leaves(proposed, &mut candidate);
    let expected: std::collections::HashSet<_> = current.iter().copied().collect();
    let mut seen = std::collections::HashSet::new();
    candidate.len() == current.len()
        && candidate
            .iter()
            .all(|leaf| expected.contains(leaf) && seen.insert(*leaf))
}

/// Keep restore and split-back records limited to panes that remain in the tab.
/// A restorable tree without any split no longer describes an arrangement.
fn forget_pane_history(tab: &mut WorkspaceTab, pane_id: &PaneId) {
    tab.previous_layout = tab
        .previous_layout
        .take()
        .and_then(|previous| remove_pane(*previous, pane_id))
        .filter(|previous| matches!(previous, LayoutNode::Split { .. }))
        .map(Box::new);
    tab.merge = tab.merge.take().and_then(|merge| {
        let TabMerge { own_layout, tabs } = *merge;
        let own_layout = own_layout.and_then(|own| remove_pane(own, pane_id));
        let tabs: Vec<_> = tabs
            .into_iter()
            .filter_map(|record| {
                Some(MergedTab {
                    layout: remove_pane(record.layout, pane_id)?,
                    ..record
                })
            })
            .collect();
        (!tabs.is_empty()).then(|| Box::new(TabMerge { own_layout, tabs }))
    });
}

fn surfaces_for_target(
    workspace: &StoredWorkspace,
    target: &RemovalTarget,
) -> Option<Vec<SurfaceId>> {
    let mut surfaces = Vec::new();
    match target {
        RemovalTarget::Pane { pane_id } => {
            let tab = workspace
                .sessions
                .iter()
                .flat_map(|session| &session.tabs)
                .find(|tab| contains_pane(&tab.layout, pane_id))?;
            collect_surface_for_pane(&tab.layout, pane_id, &mut surfaces);
        }
        RemovalTarget::Tab { tab_id } => {
            let tab = workspace
                .sessions
                .iter()
                .flat_map(|session| &session.tabs)
                .find(|tab| &tab.id == tab_id)?;
            collect_surfaces(&tab.layout, &mut surfaces);
        }
        RemovalTarget::Session { session_id } => {
            let session = workspace
                .sessions
                .iter()
                .find(|session| &session.id == session_id)?;
            for tab in &session.tabs {
                collect_surfaces(&tab.layout, &mut surfaces);
            }
        }
    }
    Some(surfaces)
}

fn collect_surface_for_pane(node: &LayoutNode, pane_id: &PaneId, output: &mut Vec<SurfaceId>) {
    match node {
        LayoutNode::Pane {
            pane_id: current,
            surface_id,
        } if current == pane_id => output.push(surface_id.clone()),
        LayoutNode::Pane { .. } => {}
        LayoutNode::Split { first, second, .. } => {
            collect_surface_for_pane(first, pane_id, output);
            collect_surface_for_pane(second, pane_id, output);
        }
    }
}

fn apply_removal(workspace: &mut StoredWorkspace, target: &RemovalTarget) {
    match target {
        RemovalTarget::Pane { pane_id } => {
            for tab in workspace
                .sessions
                .iter_mut()
                .flat_map(|session| &mut session.tabs)
            {
                if contains_pane(&tab.layout, pane_id)
                    && let Some(replacement) = remove_pane(tab.layout.clone(), pane_id)
                {
                    tab.layout = replacement;
                    forget_pane_history(tab, pane_id);
                    break;
                }
            }
        }
        RemovalTarget::Tab { tab_id } => {
            for session in &mut workspace.sessions {
                session.tabs.retain(|tab| &tab.id != tab_id);
            }
        }
        RemovalTarget::Session { session_id } => {
            workspace
                .sessions
                .retain(|session| &session.id != session_id);
        }
    }
    prune_unreferenced_surfaces(workspace);
}

fn remove_pane(node: LayoutNode, target: &PaneId) -> Option<LayoutNode> {
    match node {
        LayoutNode::Pane { pane_id, .. } if &pane_id == target => None,
        pane @ LayoutNode::Pane { .. } => Some(pane),
        LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => match (remove_pane(*first, target), remove_pane(*second, target)) {
            (Some(first), Some(second)) => Some(LayoutNode::Split {
                axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (Some(survivor), None) | (None, Some(survivor)) => Some(survivor),
            (None, None) => None,
        },
    }
}

fn prune_unreferenced_surfaces(workspace: &mut StoredWorkspace) {
    fn collect(node: &LayoutNode, output: &mut HashSet<SurfaceId>) {
        match node {
            LayoutNode::Pane { surface_id, .. } => {
                output.insert(surface_id.clone());
            }
            LayoutNode::Split { first, second, .. } => {
                collect(first, output);
                collect(second, output);
            }
        }
    }

    let mut referenced = HashSet::new();
    for tab in workspace.sessions.iter().flat_map(|session| &session.tabs) {
        collect(&tab.layout, &mut referenced);
    }
    workspace
        .surfaces
        .retain(|surface| referenced.contains(&surface.id));
    workspace
        .last_directories
        .retain(|surface, _| referenced.contains(surface));
}

fn verify_lifetime(
    surface: &SurfaceInfo,
    expected: &ProcessLifetimeId,
    revision: u64,
) -> std::result::Result<(), ActorError> {
    if &surface.process_lifetime_id == expected {
        Ok(())
    } else {
        Err(ActorError::new(
            ErrorCode::StaleLifetime,
            format!("surface {} has a different process lifetime", surface.id),
            Some(revision),
        ))
    }
}

fn validate_label(label: &str, required: bool) -> std::result::Result<(), String> {
    if required && label.trim().is_empty() {
        Err("workspace labels must not be empty".into())
    } else if label.len() > 256 {
        Err("workspace labels must not exceed 256 bytes".into())
    } else {
        Ok(())
    }
}

fn dimensions(cols: i16, rows: i16) -> std::result::Result<(), String> {
    if !(1..=1_000).contains(&cols) || !(1..=1_000).contains(&rows) {
        Err("terminal dimensions must be between 1 and 1000".into())
    } else {
        Ok(())
    }
}

fn fingerprint(request: &MutationRequest) -> serde_json::Result<String> {
    let bytes = serde_json::to_vec(request)?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn allocate(prefix: &str, ordinal: &mut u64) -> String {
    let current = *ordinal;
    *ordinal = ordinal.saturating_add(1);
    new_id(prefix, current)
}

fn new_id(prefix: &str, ordinal: u64) -> String {
    format!(
        "{prefix}-{:x}-{:x}-{ordinal:x}",
        now_ms(),
        std::process::id()
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn queue_error<T>(error: TrySendError<T>) -> ActorError {
    match error {
        TrySendError::Full(_) => ActorError::new(ErrorCode::Busy, "workspace actor is busy", None),
        TrySendError::Disconnected(_) => disconnected(mpsc::RecvError),
    }
}

fn disconnected(_: mpsc::RecvError) -> ActorError {
    ActorError::new(ErrorCode::Internal, "workspace actor stopped", None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn lifecycle_consent(actor: &WorkspaceActor) -> compi_protocol::LifecycleConsent {
        let snapshot = actor.snapshot().unwrap();
        compi_protocol::LifecycleConsent {
            live_surfaces: compi_protocol::live_surface_inventory(&snapshot),
            server_id: snapshot.server_id,
            server_generation: snapshot.server_generation,
            workspace_revision: snapshot.revision,
            connected_clients: Vec::new(),
        }
    }

    #[test]
    fn shutdown_consent_rejects_new_work_and_freezes_mutations_only_after_acceptance() {
        let (actor, _effects) = WorkspaceActor::memory();
        let stopping = Arc::new(AtomicBool::new(false));
        let stale = lifecycle_consent(&actor);
        actor
            .mutate(request(
                &actor,
                "initialize",
                0,
                WorkspaceMutation::Initialize {
                    cols: 80,
                    rows: 24,
                    working_directory: None,
                },
            ))
            .unwrap();
        assert_eq!(
            actor
                .conditional_stop(stale, stopping.clone())
                .unwrap_err()
                .code,
            ErrorCode::RevisionConflict
        );
        assert!(!stopping.load(Ordering::Acquire));
        let current = lifecycle_consent(&actor);
        let mut omitted_work = current.clone();
        omitted_work.live_surfaces.clear();
        assert_eq!(
            actor
                .conditional_stop(omitted_work, stopping.clone())
                .unwrap_err()
                .code,
            ErrorCode::RevisionConflict
        );
        let mut old_generation = current.clone();
        old_generation.server_generation = ServerGeneration::from("old-generation");
        assert_eq!(
            actor
                .conditional_stop(old_generation, stopping.clone())
                .unwrap_err()
                .code,
            ErrorCode::StaleGeneration
        );
        assert!(!stopping.load(Ordering::Acquire));
        actor.conditional_stop(current, stopping.clone()).unwrap();
        assert!(stopping.load(Ordering::Acquire));
        let revision = actor.snapshot().unwrap().revision;
        let error = actor
            .mutate(request(
                &actor,
                "after-stop",
                revision,
                WorkspaceMutation::CreateSession {
                    label: "Must not be created".into(),
                },
            ))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Busy);
        assert_eq!(actor.snapshot().unwrap().revision, revision);
    }

    fn request(
        actor: &WorkspaceActor,
        id: &str,
        revision: u64,
        operation: WorkspaceMutation,
    ) -> MutationRequest {
        let snapshot = actor.snapshot().unwrap();
        MutationRequest {
            server_id: snapshot.server_id,
            expected_generation: snapshot.server_generation,
            mutation_id: MutationId::new(id),
            expected_revision: revision,
            operation,
            launch: None,
        }
    }

    fn wait_revision(actor: &WorkspaceActor, expected: u64) -> WorkspaceSnapshot {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let workspace = actor.snapshot().unwrap();
            if workspace.revision >= expected {
                return workspace;
            }
            assert!(
                Instant::now() < deadline,
                "workspace did not reach revision {expected}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn exact_retry_returns_receipt_but_changed_reuse_fails() {
        let (actor, _effects) = WorkspaceActor::memory();
        let mutation = request(
            &actor,
            "mutation-1",
            0,
            WorkspaceMutation::CreateSession {
                label: "Work".into(),
            },
        );
        let first = actor.mutate(mutation.clone()).unwrap();
        assert_eq!(actor.mutate(mutation).unwrap(), first);
        let changed = request(
            &actor,
            "mutation-1",
            1,
            WorkspaceMutation::CreateSession {
                label: "Other".into(),
            },
        );
        assert_eq!(
            actor.mutate(changed).unwrap_err().code,
            ErrorCode::MutationIdReused
        );
    }

    #[test]
    fn stale_revision_changes_nothing() {
        let (actor, _effects) = WorkspaceActor::memory();
        actor
            .mutate(request(
                &actor,
                "mutation-1",
                0,
                WorkspaceMutation::CreateSession {
                    label: "Work".into(),
                },
            ))
            .unwrap();
        let stale = request(
            &actor,
            "mutation-2",
            0,
            WorkspaceMutation::CreateSession {
                label: "Stale".into(),
            },
        );
        assert_eq!(
            actor.mutate(stale).unwrap_err().code,
            ErrorCode::RevisionConflict
        );
        assert_eq!(actor.snapshot().unwrap().sessions.len(), 1);
    }

    fn growth_fixture(
        cols: i16,
        rows: i16,
    ) -> (WorkspaceActor, Receiver<WorkspaceEffect>, WorkspaceSnapshot) {
        let (actor, effects) = WorkspaceActor::memory();
        actor
            .mutate(request(
                &actor,
                "growth-init",
                0,
                WorkspaceMutation::Initialize {
                    cols,
                    rows,
                    working_directory: None,
                },
            ))
            .unwrap();
        assert!(matches!(
            effects.recv().unwrap(),
            WorkspaceEffect::Launch(_, _)
        ));
        let snapshot = actor.snapshot().unwrap();
        (actor, effects, snapshot)
    }

    fn receive_launches(effects: &Receiver<WorkspaceEffect>, expected: usize) -> Vec<SurfaceInfo> {
        let mut pending = VecDeque::new();
        let mut launched = Vec::with_capacity(expected);
        while launched.len() < expected {
            match pending
                .pop_front()
                .unwrap_or_else(|| effects.recv().unwrap())
            {
                WorkspaceEffect::Batch(batch) => pending.extend(batch),
                WorkspaceEffect::Launch(surface, _) => launched.push(surface),
                WorkspaceEffect::End { .. } => panic!("growth must only launch missing panes"),
            }
        }
        assert!(
            pending.is_empty(),
            "growth launched more shells than requested"
        );
        launched
    }

    fn balanced_growth_plan(
        count: usize,
        axis: SplitAxis,
        existing: &mut impl Iterator<Item = PaneId>,
    ) -> PlannedLayoutNode {
        if count == 1 {
            return existing
                .next()
                .map_or(PlannedLayoutNode::NewPane, |pane_id| {
                    PlannedLayoutNode::ExistingPane { pane_id }
                });
        }
        let first_count = count / 2;
        let next_axis = match axis {
            SplitAxis::Horizontal => SplitAxis::Vertical,
            SplitAxis::Vertical => SplitAxis::Horizontal,
        };
        PlannedLayoutNode::Split {
            axis,
            ratio: first_count as f32 / count as f32,
            first: Box::new(balanced_growth_plan(first_count, next_axis, existing)),
            second: Box::new(balanced_growth_plan(
                count - first_count,
                next_axis,
                existing,
            )),
        }
    }

    fn growth_operation(snapshot: &WorkspaceSnapshot, count: usize) -> WorkspaceMutation {
        let tab = &snapshot.sessions[0].tabs[0];
        WorkspaceMutation::GrowTab {
            tab_id: tab.id.clone(),
            layout: balanced_growth_plan(
                count,
                SplitAxis::Vertical,
                &mut pane_ids(&tab.layout).into_iter(),
            ),
            working_directory: Some("/preserved-launch-directory".into()),
        }
    }

    fn columns(count: usize, panes: &mut impl Iterator<Item = PaneId>) -> PlannedLayoutNode {
        let first = panes.next().map_or(PlannedLayoutNode::NewPane, |pane_id| {
            PlannedLayoutNode::ExistingPane { pane_id }
        });
        if count == 1 {
            return first;
        }
        PlannedLayoutNode::Split {
            axis: SplitAxis::Horizontal,
            ratio: 1.0 / count as f32,
            first: Box::new(first),
            second: Box::new(columns(count - 1, panes)),
        }
    }

    #[test]
    fn atomic_growth_full_effect_queue_returns_busy_without_partial_workspace_or_launches() {
        let (actor, effects, initial) = growth_fixture(1000, 1000);
        for index in 0..EFFECT_QUEUE {
            let snapshot = actor.snapshot().unwrap();
            actor
                .mutate(request(
                    &actor,
                    &format!("fill-effect-queue-{index}"),
                    snapshot.revision,
                    WorkspaceMutation::CreateTab {
                        session_id: initial.sessions[0].id.clone(),
                        label: String::new(),
                        cols: 1000,
                        rows: 1000,
                        working_directory: None,
                    },
                ))
                .unwrap();
        }
        let before = actor.snapshot().unwrap();
        let mutation = request(
            &actor,
            "grow-queue-full",
            before.revision,
            growth_operation(&before, 100),
        );
        assert_eq!(
            actor.mutate(mutation.clone()).unwrap_err().code,
            ErrorCode::Busy
        );
        assert_eq!(actor.snapshot().unwrap(), before);
        assert!(
            actor
                .outcome(mutation.mutation_id.clone())
                .unwrap()
                .is_none()
        );
        let queued = receive_launches(&effects, EFFECT_QUEUE);
        assert!(
            queued
                .iter()
                .all(|surface| before.surface(&surface.id) == Some(surface))
        );
        assert!(matches!(effects.try_recv(), Err(TryRecvError::Empty)));
        let receipt = actor.mutate(mutation).unwrap();
        let created = receive_launches(&effects, 99);
        assert_eq!(receipt.revision, before.revision + 1);
        assert_eq!(receipt.affected_panes.len(), 100);
        assert!(created.iter().all(|surface| {
            before.surface(&surface.id).is_none() && receipt.affected_surfaces.contains(&surface.id)
        }));
    }

    #[test]
    fn atomic_growth_above_effect_queue_capacity_keeps_runtime_feedback_live() {
        let (actor, effects, initial) = growth_fixture(1000, 1000);
        let worker_actor = actor.clone();
        let worker = thread::spawn(move || {
            let mut pending = VecDeque::new();
            let mut launched = Vec::new();
            for _ in 0..99 {
                let surface = loop {
                    let effect = pending.pop_front().unwrap_or_else(|| {
                        effects
                            .recv_timeout(Duration::from_secs(5))
                            .expect("growth dispatch stalled")
                    });
                    match effect {
                        WorkspaceEffect::Batch(batch) => pending.extend(batch),
                        WorkspaceEffect::Launch(surface, _) => break surface,
                        WorkspaceEffect::End { .. } => panic!("unexpected cleanup"),
                    }
                };
                // Surface::spawn asks for a snapshot and reports Running. Both
                // callbacks must work while the transaction dispatches its batch.
                worker_actor.snapshot().unwrap();
                worker_actor.observe(RuntimeObservation::Running {
                    surface_id: surface.id.clone(),
                    process_lifetime_id: surface.process_lifetime_id.clone(),
                    working_directory: None,
                });
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    let snapshot = worker_actor.snapshot().unwrap();
                    if snapshot
                        .surface(&surface.id)
                        .is_some_and(|info| info.status == SurfaceStatus::Running)
                    {
                        break;
                    }
                    assert!(Instant::now() < deadline, "runtime observation stalled");
                    thread::sleep(Duration::from_millis(1));
                }
                launched.push(surface.id);
            }
            launched
        });
        let mutation = request(
            &actor,
            "grow-hundred",
            initial.revision,
            growth_operation(&initial, 100),
        );
        let mutation_actor = actor.clone();
        let (reply, receive) = mpsc::sync_channel(1);
        let mutation_worker = thread::spawn(move || {
            reply.send(mutation_actor.mutate(mutation)).unwrap();
        });
        let receipt = receive
            .recv_timeout(Duration::from_secs(5))
            .expect("growth blocked the actor on runtime feedback")
            .unwrap();
        mutation_worker.join().unwrap();
        let launched = worker.join().unwrap();
        let final_snapshot = actor.snapshot().unwrap();
        assert_eq!(receipt.revision, initial.revision + 1);
        assert_eq!(receipt.affected_panes.len(), 100);
        assert_eq!(receipt.affected_surfaces.len(), 100);
        assert_eq!(final_snapshot.surfaces.len(), 100);
        assert!(launched.into_iter().all(|id| {
            receipt.affected_surfaces.contains(&id)
                && final_snapshot
                    .surface(&id)
                    .is_some_and(|surface| surface.status == SurfaceStatus::Running)
        }));
        assert_eq!(
            final_snapshot.surface(&initial.surfaces[0].id),
            Some(&initial.surfaces[0])
        );
    }

    #[test]
    fn atomic_growth_reports_all_ids_in_one_revision_and_preserves_existing_processes() {
        let (actor, effects, initial) = growth_fixture(80, 24);
        let receipt = actor
            .mutate(request(
                &actor,
                "grow-four",
                initial.revision,
                growth_operation(&initial, 4),
            ))
            .unwrap();
        let four = actor.snapshot().unwrap();
        assert_eq!(receipt.revision, initial.revision + 1);
        assert_eq!(four.revision, receipt.revision);
        assert_eq!(
            receipt.affected_sessions,
            vec![initial.sessions[0].id.clone()]
        );
        assert_eq!(
            receipt.affected_tabs,
            vec![initial.sessions[0].tabs[0].id.clone()]
        );
        assert_eq!(
            receipt.affected_panes,
            pane_ids(&four.sessions[0].tabs[0].layout)
        );
        assert_eq!(receipt.affected_panes.len(), 4);
        assert_eq!(four.surfaces.len(), 4);
        assert_eq!(
            four.surface(&initial.surfaces[0].id),
            Some(&initial.surfaces[0])
        );
        let mut launched = HashSet::new();
        for surface in receive_launches(&effects, 3) {
            assert_eq!(
                surface.launch.working_directory.as_deref(),
                Some("/preserved-launch-directory")
            );
            assert!(receipt.affected_surfaces.contains(&surface.id));
            assert_eq!(four.surface(&surface.id), Some(&surface));
            assert!(launched.insert(surface.id));
        }
        assert_eq!(
            receipt.affected_surfaces.iter().collect::<HashSet<_>>(),
            four.surfaces.iter().map(|surface| &surface.id).collect()
        );
        assert!(matches!(effects.try_recv(), Err(TryRecvError::Empty)));

        let next = actor
            .mutate(request(
                &actor,
                "grow-six",
                four.revision,
                growth_operation(&four, 6),
            ))
            .unwrap();
        let six = actor.snapshot().unwrap();
        assert_eq!(six.revision, four.revision + 1);
        assert_eq!(next.affected_panes.len(), 6);
        for surface in &four.surfaces {
            assert_eq!(six.surface(&surface.id), Some(surface));
        }
        let mut old_leaves = Vec::new();
        let mut new_leaves = Vec::new();
        collect_leaves(&four.sessions[0].tabs[0].layout, &mut old_leaves);
        collect_leaves(&six.sessions[0].tabs[0].layout, &mut new_leaves);
        assert!(
            old_leaves
                .into_iter()
                .all(|leaf| new_leaves.contains(&leaf))
        );
        for surface in receive_launches(&effects, 2) {
            assert!(four.surface(&surface.id).is_none());
        }
        assert!(matches!(effects.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn atomic_growth_preflights_late_layout_and_geometry_errors_without_launching() {
        let (actor, effects, initial) = growth_fixture(80, 24);
        let mut invalid = growth_operation(&initial, 4);
        let WorkspaceMutation::GrowTab {
            layout: PlannedLayoutNode::Split { second, .. },
            ..
        } = &mut invalid
        else {
            unreachable!();
        };
        let PlannedLayoutNode::Split { ratio, .. } = &mut **second else {
            unreachable!();
        };
        *ratio = 0.0;
        assert_eq!(
            actor
                .mutate(request(&actor, "grow-bad-ratio", initial.revision, invalid))
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
        assert_eq!(actor.snapshot().unwrap(), initial);
        assert!(matches!(effects.try_recv(), Err(TryRecvError::Empty)));
        assert!(
            actor
                .outcome(MutationId::from("grow-bad-ratio"))
                .unwrap()
                .is_none()
        );

        let too_small = WorkspaceMutation::GrowTab {
            tab_id: initial.sessions[0].tabs[0].id.clone(),
            layout: PlannedLayoutNode::Split {
                axis: SplitAxis::Horizontal,
                ratio: 0.1,
                first: Box::new(PlannedLayoutNode::NewPane),
                second: Box::new(PlannedLayoutNode::ExistingPane {
                    pane_id: pane_ids(&initial.sessions[0].tabs[0].layout)[0].clone(),
                }),
            },
            working_directory: None,
        };
        assert_eq!(
            actor
                .mutate(request(
                    &actor,
                    "grow-too-small",
                    initial.revision,
                    too_small
                ))
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
        assert_eq!(actor.snapshot().unwrap(), initial);
        assert!(matches!(effects.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn atomic_growth_refuses_excess_and_never_removes_existing_panes() {
        let (actor, effects, initial) = growth_fixture(1000, 1000);
        let error = actor
            .mutate(request(
                &actor,
                "grow-excess",
                initial.revision,
                growth_operation(&initial, MAX_TAB_PANES * 2),
            ))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert!(error.message.contains("at most"));
        assert_eq!(actor.snapshot().unwrap(), initial);
        assert!(matches!(effects.try_recv(), Err(TryRecvError::Empty)));
        let remove_existing = WorkspaceMutation::GrowTab {
            tab_id: initial.sessions[0].tabs[0].id.clone(),
            layout: PlannedLayoutNode::NewPane,
            working_directory: None,
        };
        assert_eq!(
            actor
                .mutate(request(
                    &actor,
                    "grow-omits-existing",
                    initial.revision,
                    remove_existing
                ))
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
        assert_eq!(actor.snapshot().unwrap(), initial);
        assert!(matches!(effects.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn atomic_growth_does_not_treat_unresized_original_pane_as_extra_tab_space() {
        let (actor, effects, initial) = growth_fixture(80, 24);
        let operation = |snapshot: &WorkspaceSnapshot, count| WorkspaceMutation::GrowTab {
            tab_id: snapshot.sessions[0].tabs[0].id.clone(),
            layout: columns(
                count,
                &mut pane_ids(&snapshot.sessions[0].tabs[0].layout).into_iter(),
            ),
            working_directory: None,
        };
        actor
            .mutate(request(
                &actor,
                "grow-two-unresized",
                initial.revision,
                operation(&initial, 2),
            ))
            .unwrap();
        assert!(matches!(
            effects.recv().unwrap(),
            WorkspaceEffect::Launch(_, _)
        ));
        let before = actor.snapshot().unwrap();
        // No resize observation has arrived for the original 80-column pane.
        assert_eq!(before.surface(&initial.surfaces[0].id).unwrap().cols, 80);
        assert_eq!(
            actor
                .mutate(request(
                    &actor,
                    "grow-four-unresized",
                    before.revision,
                    operation(&before, 4),
                ))
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
        assert_eq!(actor.snapshot().unwrap(), before);
        assert!(matches!(effects.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn atomic_growth_retains_valid_rounded_three_column_layout_after_measured_resizes() {
        let (actor, effects, initial) = growth_fixture(63, 24);
        let operation = |snapshot: &WorkspaceSnapshot| WorkspaceMutation::GrowTab {
            tab_id: snapshot.sessions[0].tabs[0].id.clone(),
            layout: columns(
                3,
                &mut pane_ids(&snapshot.sessions[0].tabs[0].layout).into_iter(),
            ),
            working_directory: None,
        };
        actor
            .mutate(request(
                &actor,
                "rounded-columns",
                initial.revision,
                operation(&initial),
            ))
            .unwrap();
        let launched = receive_launches(&effects, 2);
        assert_eq!(
            launched
                .iter()
                .map(|surface| surface.cols)
                .collect::<Vec<_>>(),
            vec![20, 21]
        );
        let created = actor.snapshot().unwrap();
        actor.observe(RuntimeObservation::Resized {
            surface_id: initial.surfaces[0].id.clone(),
            process_lifetime_id: initial.surfaces[0].process_lifetime_id.clone(),
            cols: 20,
            rows: 24,
        });
        let measured = wait_revision(&actor, created.revision + 1);
        let receipt = actor
            .mutate(request(
                &actor,
                "rounded-columns-again",
                measured.revision,
                operation(&measured),
            ))
            .unwrap();
        let repeated = actor.snapshot().unwrap();
        assert_eq!(receipt.revision, measured.revision + 1);
        assert_eq!(repeated.sessions, measured.sessions);
        assert_eq!(repeated.surfaces, measured.surfaces);
        assert_eq!(
            receipt.affected_panes,
            pane_ids(&measured.sessions[0].tabs[0].layout)
        );
        assert!(matches!(effects.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn atomic_growth_stale_revision_has_no_shell_or_workspace_effects() {
        let (actor, effects, initial) = growth_fixture(80, 24);
        assert_eq!(
            actor
                .mutate(request(
                    &actor,
                    "grow-stale",
                    initial.revision - 1,
                    growth_operation(&initial, 4),
                ))
                .unwrap_err()
                .code,
            ErrorCode::RevisionConflict
        );
        assert_eq!(actor.snapshot().unwrap(), initial);
        assert!(matches!(effects.try_recv(), Err(TryRecvError::Empty)));
        assert!(
            actor
                .outcome(MutationId::from("grow-stale"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn split_creates_complete_tree_before_launch_effect() {
        let (actor, effects) = WorkspaceActor::memory();
        actor
            .mutate(request(
                &actor,
                "initialize",
                0,
                WorkspaceMutation::Initialize {
                    cols: 80,
                    rows: 24,
                    working_directory: None,
                },
            ))
            .unwrap();
        let _ = effects.recv().unwrap();
        let workspace = actor.snapshot().unwrap();
        let pane_id = match &workspace.sessions[0].tabs[0].layout {
            LayoutNode::Pane { pane_id, .. } => pane_id.clone(),
            _ => unreachable!(),
        };
        actor
            .mutate(request(
                &actor,
                "split",
                workspace.revision,
                WorkspaceMutation::SplitPane {
                    pane_id,
                    axis: SplitAxis::Horizontal,
                    cols: 80,
                    rows: 24,
                    working_directory: None,
                    geometry: compi_protocol::SplitGeometry {
                        width: 800.0,
                        height: 480.0,
                        min_width: 160.0,
                        min_height: 80.0,
                        divider: 4.0,
                    },
                },
            ))
            .unwrap();
        assert!(matches!(
            actor.snapshot().unwrap().sessions[0].tabs[0].layout,
            LayoutNode::Split { .. }
        ));
        assert!(matches!(
            effects.recv().unwrap(),
            WorkspaceEffect::Launch(_, _)
        ));
    }

    #[test]
    fn detach_preserves_nested_survivors_and_running_surface_in_durable_shape() {
        let (actor, effects) = WorkspaceActor::memory();
        actor
            .mutate(request(
                &actor,
                "detach-init",
                0,
                WorkspaceMutation::Initialize {
                    cols: 80,
                    rows: 24,
                    working_directory: None,
                },
            ))
            .unwrap();
        let initial = actor.snapshot().unwrap();
        let LayoutNode::Pane { pane_id: a, .. } = &initial.sessions[0].tabs[0].layout else {
            panic!("expected initial leaf");
        };
        let a = a.clone();
        let split = |id: &str, pane_id: PaneId| {
            actor
                .mutate(request(
                    &actor,
                    id,
                    actor.snapshot().unwrap().revision,
                    WorkspaceMutation::SplitPane {
                        pane_id,
                        axis: SplitAxis::Horizontal,
                        cols: 80,
                        rows: 24,
                        working_directory: None,
                        geometry: compi_protocol::SplitGeometry {
                            width: 800.0,
                            height: 480.0,
                            min_width: 160.0,
                            min_height: 80.0,
                            divider: 4.0,
                        },
                    },
                ))
                .unwrap()
                .affected_panes[1]
                .clone()
        };
        let b = split("detach-root", a.clone());
        let c = split("detach-left", a.clone());
        let d = split("detach-right", b.clone());
        for (id, path, ratio) in [
            ("detach-ratio-root", vec![], 0.63),
            ("detach-ratio-left", vec![false], 0.27),
            ("detach-ratio-right", vec![true], 0.71),
        ] {
            actor
                .mutate(request(
                    &actor,
                    id,
                    actor.snapshot().unwrap().revision,
                    WorkspaceMutation::SetSplitRatio {
                        tab_id: initial.sessions[0].tabs[0].id.clone(),
                        path,
                        ratio,
                    },
                ))
                .unwrap();
        }
        for _ in 0..3 {
            assert!(matches!(
                effects.recv().unwrap(),
                WorkspaceEffect::Launch(_, _)
            ));
        }
        let WorkspaceEffect::Launch(detached_surface, _) = effects.recv().unwrap() else {
            panic!("expected detached pane launch");
        };
        let revision = actor.snapshot().unwrap().revision;
        actor.observe(RuntimeObservation::Running {
            surface_id: detached_surface.id.clone(),
            process_lifetime_id: detached_surface.process_lifetime_id.clone(),
            working_directory: None,
        });
        let before = wait_revision(&actor, revision + 1);
        let receipt = actor
            .mutate(request(
                &actor,
                "detach-leaf",
                before.revision,
                WorkspaceMutation::DetachPane { pane_id: d.clone() },
            ))
            .unwrap();
        let after = actor.snapshot().unwrap();
        let session = &after.sessions[0];
        assert_eq!(after.revision, before.revision + 1);
        assert_eq!(session.tabs.len(), 2);
        assert_eq!(receipt.revision, after.revision);
        assert_eq!(receipt.operation_state, "committed");
        assert_eq!(receipt.affected_sessions, vec![session.id.clone()]);
        assert_eq!(
            receipt.affected_tabs,
            vec![session.tabs[1].id.clone(), session.tabs[0].id.clone()]
        );
        assert_eq!(receipt.affected_panes, vec![d.clone()]);
        assert_eq!(receipt.affected_surfaces, vec![detached_surface.id.clone()]);
        assert_eq!(session.tabs[0].id, before.sessions[0].tabs[0].id);
        assert!(session.tabs[1].label.is_empty());
        assert!(matches!(
            &session.tabs[1].layout,
            LayoutNode::Pane { pane_id, surface_id }
                if pane_id == &d && surface_id == &detached_surface.id
        ));
        let LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            ratio: root_ratio,
            first,
            second,
        } = &session.tabs[0].layout
        else {
            panic!("expected surviving root split");
        };
        assert_eq!(*root_ratio, 0.63);
        assert!(matches!(&**second, LayoutNode::Pane { pane_id, .. } if pane_id == &b));
        let LayoutNode::Split {
            ratio: left_ratio,
            first: left_first,
            second: left_second,
            ..
        } = &**first
        else {
            panic!("expected surviving nested split");
        };
        assert_eq!(*left_ratio, 0.27);
        assert!(matches!(&**left_first, LayoutNode::Pane { pane_id, .. } if pane_id == &a));
        assert!(matches!(&**left_second, LayoutNode::Pane { pane_id, .. } if pane_id == &c));
        assert_eq!(after.surfaces, before.surfaces);
        assert_eq!(
            after.surface(&detached_surface.id).unwrap().status,
            SurfaceStatus::Running
        );
        assert!(matches!(effects.try_recv(), Err(TryRecvError::Empty)));

        let stored = StoredWorkspace {
            server_id: after.server_id.clone(),
            revision: after.revision,
            initialized: after.initialized,
            sessions: after.sessions.clone(),
            surfaces: after.surfaces.clone(),
            receipts: vec![receipt],
            pending_removals: vec![],
            recovery_message: after.recovery_message.clone(),
            last_directories: Default::default(),
        };
        let path = std::env::temp_dir().join(format!(
            "compi-detach-{}-{}.json",
            std::process::id(),
            now_ms()
        ));
        let (store, _) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        store.commit(&stored).unwrap();
        let (_, reopened) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        assert_eq!(reopened.sessions, stored.sessions);
        assert_eq!(reopened.receipts, stored.receipts);
        assert_eq!(reopened.server_id, stored.server_id);
        assert_eq!(reopened.surfaces.len(), stored.surfaces.len());
        for (restored, original) in reopened.surfaces.iter().zip(&stored.surfaces) {
            assert_eq!(restored.id, original.id);
            assert_eq!(restored.process_lifetime_id, original.process_lifetime_id);
            assert_eq!(restored.status, SurfaceStatus::Lost);
        }
        std::fs::remove_file(path).unwrap();

        let survivor = actor
            .mutate(request(
                &actor,
                "detach-survivor",
                after.revision,
                WorkspaceMutation::DetachPane { pane_id: b.clone() },
            ))
            .unwrap();
        let collapsed = actor.snapshot().unwrap();
        assert_eq!(survivor.revision, collapsed.revision);
        assert_eq!(collapsed.sessions[0].tabs.len(), 3);
        assert!(matches!(
            &collapsed.sessions[0].tabs[0].layout,
            LayoutNode::Split { ratio, .. } if *ratio == 0.27
        ));
        assert!(matches!(effects.try_recv(), Err(TryRecvError::Empty)));
        let refused = actor
            .mutate(request(
                &actor,
                "detach-single",
                collapsed.revision,
                WorkspaceMutation::DetachPane { pane_id: d },
            ))
            .unwrap_err();
        assert_eq!(refused.code, ErrorCode::InvalidRequest);
        assert_eq!(actor.snapshot().unwrap(), collapsed);
    }

    #[test]
    fn arrange_only_permutes_existing_panes_and_keeps_a_restorable_tree() {
        let (actor, effects) = WorkspaceActor::memory();
        actor
            .mutate(request(
                &actor,
                "arrange-init",
                0,
                WorkspaceMutation::Initialize {
                    cols: 80,
                    rows: 24,
                    working_directory: None,
                },
            ))
            .unwrap();
        let initial = actor.snapshot().unwrap();
        let tab_id = initial.sessions[0].tabs[0].id.clone();
        let LayoutNode::Pane { pane_id: a, .. } = &initial.sessions[0].tabs[0].layout else {
            panic!("expected initial leaf");
        };
        let split = |id: &str, pane_id: PaneId| {
            actor
                .mutate(request(
                    &actor,
                    id,
                    actor.snapshot().unwrap().revision,
                    WorkspaceMutation::SplitPane {
                        pane_id,
                        axis: SplitAxis::Horizontal,
                        cols: 80,
                        rows: 24,
                        working_directory: None,
                        geometry: compi_protocol::SplitGeometry {
                            width: 800.0,
                            height: 480.0,
                            min_width: 160.0,
                            min_height: 80.0,
                            divider: 4.0,
                        },
                    },
                ))
                .unwrap()
                .affected_panes[1]
                .clone()
        };
        let b = split("arrange-split-b", a.clone());
        let c = split("arrange-split-c", b.clone());
        let mut launched = Vec::new();
        for _ in 0..3 {
            let WorkspaceEffect::Launch(surface, _) = effects.recv().unwrap() else {
                panic!("expected launch");
            };
            launched.push(surface);
        }
        let before = actor.snapshot().unwrap();
        let tree = before.sessions[0].tabs[0].layout.clone();
        let leaf = |pane: &PaneId| {
            let mut found = Vec::new();
            collect_leaves(&tree, &mut found);
            let (pane_id, surface_id) = found.into_iter().find(|(id, _)| *id == pane).unwrap();
            LayoutNode::Pane {
                pane_id: pane_id.clone(),
                surface_id: surface_id.clone(),
            }
        };
        let node = |axis, ratio, first: LayoutNode, second: LayoutNode| LayoutNode::Split {
            axis,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        };
        let arrange = |id: &str, revision: u64, layout: LayoutNode| {
            actor.mutate(request(
                &actor,
                id,
                revision,
                WorkspaceMutation::ArrangeTab {
                    tab_id: tab_id.clone(),
                    layout,
                },
            ))
        };

        // Missing, duplicated, foreign, and mismatched leaves are all refused.
        let foreign_surface = LayoutNode::Pane {
            pane_id: c.clone(),
            surface_id: launched[0].id.clone(),
        };
        let unknown = LayoutNode::Pane {
            pane_id: PaneId::new("pane-unknown"),
            surface_id: launched[2].id.clone(),
        };
        for (id, layout) in [
            (
                "arrange-missing",
                node(SplitAxis::Vertical, 0.5, leaf(a), leaf(&b)),
            ),
            (
                "arrange-duplicate",
                node(
                    SplitAxis::Vertical,
                    0.5,
                    leaf(a),
                    node(SplitAxis::Vertical, 0.5, leaf(&b), leaf(&b)),
                ),
            ),
            (
                "arrange-foreign",
                node(
                    SplitAxis::Vertical,
                    0.5,
                    leaf(a),
                    node(SplitAxis::Vertical, 0.5, leaf(&b), foreign_surface),
                ),
            ),
            (
                "arrange-unknown",
                node(
                    SplitAxis::Vertical,
                    0.5,
                    leaf(a),
                    node(SplitAxis::Vertical, 0.5, leaf(&b), unknown),
                ),
            ),
            (
                "arrange-ratio",
                node(
                    SplitAxis::Vertical,
                    1.0,
                    leaf(&c),
                    node(SplitAxis::Horizontal, 0.5, leaf(a), leaf(&b)),
                ),
            ),
        ] {
            let error = arrange(id, before.revision, layout).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidRequest, "{id}");
            assert_eq!(actor.snapshot().unwrap(), before, "{id}");
        }

        let arranged = node(
            SplitAxis::Vertical,
            0.4,
            leaf(&c),
            node(SplitAxis::Horizontal, 0.5, leaf(a), leaf(&b)),
        );
        let receipt = arrange("arrange-apply", before.revision, arranged.clone()).unwrap();
        assert_eq!(receipt.operation_state, "committed");
        assert_eq!(receipt.affected_tabs, vec![tab_id.clone()]);
        assert_eq!(
            receipt.affected_panes,
            vec![c.clone(), a.clone(), b.clone()]
        );
        let after = actor.snapshot().unwrap();
        assert_eq!(after.sessions[0].tabs[0].layout, arranged);
        assert_eq!(
            after.sessions[0].tabs[0].previous_layout.as_deref(),
            Some(&tree)
        );
        // Same surfaces and lifetimes; no process was launched or ended.
        assert_eq!(after.surfaces, before.surfaces);
        assert!(matches!(effects.try_recv(), Err(TryRecvError::Empty)));

        // Re-applying the current tree keeps the restorable one.
        let unchanged = arrange("arrange-same", after.revision, arranged.clone()).unwrap();
        assert_eq!(unchanged.operation_state, "unchanged");
        assert_eq!(
            actor.snapshot().unwrap().sessions[0].tabs[0]
                .previous_layout
                .as_deref(),
            Some(&tree)
        );

        // Detaching a pane drops it from the restorable tree too.
        actor
            .mutate(request(
                &actor,
                "arrange-detach",
                actor.snapshot().unwrap().revision,
                WorkspaceMutation::DetachPane { pane_id: b.clone() },
            ))
            .unwrap();
        let detached = actor.snapshot().unwrap();
        let previous = detached.sessions[0].tabs[0]
            .previous_layout
            .as_deref()
            .unwrap();
        let mut remaining = Vec::new();
        collect_leaves(previous, &mut remaining);
        assert_eq!(
            remaining
                .iter()
                .map(|(id, _)| (*id).clone())
                .collect::<Vec<_>>(),
            vec![a.clone(), c.clone()]
        );
        assert!(detached.sessions[0].tabs[1].previous_layout.is_none());
        crate::workspace_store::validate(&StoredWorkspace {
            server_id: detached.server_id.clone(),
            revision: detached.revision,
            initialized: detached.initialized,
            sessions: detached.sessions.clone(),
            surfaces: detached.surfaces.clone(),
            receipts: vec![],
            pending_removals: vec![],
            recovery_message: None,
            last_directories: Default::default(),
        })
        .unwrap();

        // Removing a live pane holds every mutation, arrangements included,
        // until cleanup, then forgets a restorable tree that no longer splits.
        let a_surface = &launched[0];
        actor
            .mutate(request(
                &actor,
                "arrange-remove",
                detached.revision,
                WorkspaceMutation::RemovePane { pane_id: a.clone() },
            ))
            .unwrap();
        assert!(matches!(
            effects.recv().unwrap(),
            WorkspaceEffect::End { .. }
        ));
        let ending = actor.snapshot().unwrap();
        let busy = arrange(
            "arrange-busy",
            ending.revision,
            ending.sessions[0].tabs[0].layout.clone(),
        )
        .unwrap_err();
        assert_eq!(busy.code, ErrorCode::Busy);
        actor.observe(RuntimeObservation::Exited {
            surface_id: a_surface.id.clone(),
            process_lifetime_id: a_surface.process_lifetime_id.clone(),
            exit_code: 0,
        });
        let removed = wait_revision(&actor, ending.revision + 1);
        assert!(matches!(
            &removed.sessions[0].tabs[0].layout,
            LayoutNode::Pane { pane_id, .. } if pane_id == &c
        ));
        assert!(removed.sessions[0].tabs[0].previous_layout.is_none());
    }

    #[test]
    fn merged_tabs_keep_their_shells_and_split_back_into_the_original_tabs() {
        let (actor, effects) = WorkspaceActor::memory();
        let mutate = |id: &str, operation: WorkspaceMutation| {
            actor.mutate(request(
                &actor,
                id,
                actor.snapshot().unwrap().revision,
                operation,
            ))
        };
        mutate(
            "merge-init",
            WorkspaceMutation::Initialize {
                cols: 80,
                rows: 24,
                working_directory: None,
            },
        )
        .unwrap();
        let session_id = actor.snapshot().unwrap().sessions[0].id.clone();
        let create = |id: &str, label: &str| {
            mutate(
                id,
                WorkspaceMutation::CreateTab {
                    session_id: session_id.clone(),
                    label: label.into(),
                    cols: 80,
                    rows: 24,
                    working_directory: None,
                },
            )
            .unwrap()
        };
        let b_receipt = create("merge-tab-b", "build");
        let b_split = mutate(
            "merge-split-b",
            WorkspaceMutation::SplitPane {
                pane_id: b_receipt.affected_panes[0].clone(),
                axis: SplitAxis::Vertical,
                cols: 80,
                rows: 24,
                working_directory: None,
                geometry: compi_protocol::SplitGeometry {
                    width: 800.0,
                    height: 480.0,
                    min_width: 160.0,
                    min_height: 80.0,
                    divider: 4.0,
                },
            },
        )
        .unwrap();
        let c_receipt = create("merge-tab-c", "logs");
        for _ in 0..4 {
            assert!(matches!(
                effects.recv().unwrap(),
                WorkspaceEffect::Launch(_, _)
            ));
        }
        let before = actor.snapshot().unwrap();
        let tabs = before.sessions[0].tabs.clone();
        let (a_tab, b_tab, c_tab) = (&tabs[0], &tabs[1], &tabs[2]);
        let (b, b2, c) = (
            b_receipt.affected_panes[0].clone(),
            b_split.affected_panes[1].clone(),
            c_receipt.affected_panes[0].clone(),
        );
        let leaf = |pane: &PaneId| {
            let tree = tabs
                .iter()
                .map(|tab| &tab.layout)
                .find(|tree| contains_pane(tree, pane))
                .unwrap();
            let mut found = Vec::new();
            collect_leaves(tree, &mut found);
            let (pane_id, surface_id) = found.into_iter().find(|(id, _)| *id == pane).unwrap();
            LayoutNode::Pane {
                pane_id: pane_id.clone(),
                surface_id: surface_id.clone(),
            }
        };
        let node = |axis, first: LayoutNode, second: LayoutNode| LayoutNode::Split {
            axis,
            ratio: 0.5,
            first: Box::new(first),
            second: Box::new(second),
        };
        let a = pane_ids(&a_tab.layout)[0].clone();
        let merged = node(
            SplitAxis::Horizontal,
            leaf(&a),
            node(
                SplitAxis::Vertical,
                leaf(&c),
                node(SplitAxis::Horizontal, leaf(&b), leaf(&b2)),
            ),
        );
        let merge = |id: &str, sources: Vec<TabId>, layout: LayoutNode| {
            mutate(
                id,
                WorkspaceMutation::MergeTabs {
                    tab_id: a_tab.id.clone(),
                    sources,
                    layout,
                },
            )
        };

        // Empty, self, duplicate, cross-workspace, and incomplete merges change nothing.
        mutate(
            "merge-other-session",
            WorkspaceMutation::CreateSession {
                label: "Other".into(),
            },
        )
        .unwrap();
        let other_session = actor.snapshot().unwrap().sessions[1].id.clone();
        let foreign = mutate(
            "merge-foreign-tab",
            WorkspaceMutation::CreateTab {
                session_id: other_session,
                label: "elsewhere".into(),
                cols: 80,
                rows: 24,
                working_directory: None,
            },
        )
        .unwrap()
        .affected_tabs[0]
            .clone();
        assert!(matches!(
            effects.recv().unwrap(),
            WorkspaceEffect::Launch(_, _)
        ));
        let ready = actor.snapshot().unwrap();
        for (id, sources, layout) in [
            ("merge-none", vec![], merged.clone()),
            ("merge-self", vec![a_tab.id.clone()], merged.clone()),
            (
                "merge-duplicate",
                vec![b_tab.id.clone(), b_tab.id.clone(), c_tab.id.clone()],
                merged.clone(),
            ),
            (
                "merge-foreign",
                vec![b_tab.id.clone(), c_tab.id.clone(), foreign.clone()],
                merged.clone(),
            ),
            (
                "merge-incomplete",
                vec![b_tab.id.clone(), c_tab.id.clone()],
                node(SplitAxis::Horizontal, leaf(&a), leaf(&c)),
            ),
        ] {
            let error = merge(id, sources, layout).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidRequest, "{id}");
            assert_eq!(actor.snapshot().unwrap(), ready, "{id}");
        }

        let receipt = merge(
            "merge-apply",
            vec![c_tab.id.clone(), b_tab.id.clone()],
            merged.clone(),
        )
        .unwrap();
        assert_eq!(receipt.operation_state, "committed");
        let after = actor.snapshot().unwrap();
        let tab = &after.sessions[0].tabs;
        assert_eq!(tab.len(), 1);
        assert_eq!(tab[0].id, a_tab.id);
        assert_eq!(tab[0].layout, merged);
        assert!(tab[0].previous_layout.is_none());
        let record = tab[0].merge.as_deref().unwrap();
        assert_eq!(record.own_layout.as_ref(), Some(&a_tab.layout));
        assert_eq!(
            record
                .tabs
                .iter()
                .map(|tab| (tab.id.clone(), tab.label.as_str(), tab.index))
                .collect::<Vec<_>>(),
            vec![
                (b_tab.id.clone(), "build", 1),
                (c_tab.id.clone(), "logs", 2)
            ]
        );
        assert_eq!(after.surfaces, ready.surfaces);
        assert!(matches!(effects.try_recv(), Err(TryRecvError::Empty)));

        // Arranging the merged tab keeps the split-back record; detaching a
        // pane removes it from the recorded original tab.
        mutate(
            "merge-arrange",
            WorkspaceMutation::ArrangeTab {
                tab_id: a_tab.id.clone(),
                layout: node(
                    SplitAxis::Vertical,
                    leaf(&b),
                    node(
                        SplitAxis::Vertical,
                        leaf(&b2),
                        node(SplitAxis::Vertical, leaf(&c), leaf(&a)),
                    ),
                ),
            },
        )
        .unwrap();
        let detached_tab = mutate(
            "merge-detach",
            WorkspaceMutation::DetachPane {
                pane_id: b2.clone(),
            },
        )
        .unwrap()
        .affected_tabs[0]
            .clone();
        let detached = actor.snapshot().unwrap();
        let record = detached.sessions[0].tabs[0].merge.as_deref().unwrap();
        assert_eq!(record.tabs[0].layout, leaf(&b));

        mutate(
            "merge-split",
            WorkspaceMutation::SplitMergedTabs {
                tab_id: a_tab.id.clone(),
            },
        )
        .unwrap();
        let split = actor.snapshot().unwrap();
        let restored: Vec<_> = split.sessions[0]
            .tabs
            .iter()
            .map(|tab| (tab.id.clone(), tab.label.clone(), tab.layout.clone()))
            .collect();
        assert_eq!(
            restored,
            vec![
                (a_tab.id.clone(), a_tab.label.clone(), a_tab.layout.clone()),
                (b_tab.id.clone(), "build".into(), leaf(&b)),
                (c_tab.id.clone(), "logs".into(), c_tab.layout.clone()),
                (detached_tab, String::new(), leaf(&b2)),
            ]
        );
        assert!(split.sessions[0].tabs.iter().all(|tab| tab.merge.is_none()));
        assert_eq!(split.surfaces, ready.surfaces);
        assert!(matches!(effects.try_recv(), Err(TryRecvError::Empty)));
        let refused = mutate(
            "merge-split-again",
            WorkspaceMutation::SplitMergedTabs {
                tab_id: a_tab.id.clone(),
            },
        )
        .unwrap_err();
        assert_eq!(refused.code, ErrorCode::InvalidRequest);

        // Merging a merged tab flattens both records, so one split restores all.
        mutate(
            "merge-nested-inner",
            WorkspaceMutation::MergeTabs {
                tab_id: c_tab.id.clone(),
                sources: vec![b_tab.id.clone()],
                layout: node(SplitAxis::Horizontal, leaf(&c), leaf(&b)),
            },
        )
        .unwrap();
        merge(
            "merge-nested-outer",
            vec![c_tab.id.clone()],
            node(
                SplitAxis::Vertical,
                leaf(&a),
                node(SplitAxis::Horizontal, leaf(&c), leaf(&b)),
            ),
        )
        .unwrap();
        mutate(
            "merge-nested-split",
            WorkspaceMutation::SplitMergedTabs {
                tab_id: a_tab.id.clone(),
            },
        )
        .unwrap();
        let nested = actor.snapshot().unwrap();
        let ids: Vec<_> = nested.sessions[0]
            .tabs
            .iter()
            .map(|tab| tab.id.clone())
            .collect();
        assert!(ids.contains(&b_tab.id) && ids.contains(&c_tab.id));
        let stored = StoredWorkspace {
            server_id: nested.server_id.clone(),
            revision: nested.revision,
            initialized: nested.initialized,
            sessions: nested.sessions.clone(),
            surfaces: nested.surfaces.clone(),
            receipts: vec![],
            pending_removals: vec![],
            recovery_message: None,
            last_directories: Default::default(),
        };
        crate::workspace_store::validate(&stored).unwrap();
    }

    #[test]
    fn old_lifetime_observation_is_ignored_after_restart() {
        let (actor, effects) = WorkspaceActor::memory();
        actor
            .mutate(request(
                &actor,
                "initialize",
                0,
                WorkspaceMutation::Initialize {
                    cols: 80,
                    rows: 24,
                    working_directory: None,
                },
            ))
            .unwrap();
        let WorkspaceEffect::Launch(surface, _) = effects.recv().unwrap() else {
            unreachable!()
        };
        actor.observe(RuntimeObservation::Failed {
            surface_id: surface.id.clone(),
            process_lifetime_id: surface.process_lifetime_id.clone(),
            error: "launch failed".into(),
        });
        thread::sleep(Duration::from_millis(30));
        let failed = actor.snapshot().unwrap();
        actor
            .mutate(request(
                &actor,
                "restart",
                failed.revision,
                WorkspaceMutation::RestartSurface {
                    surface_id: surface.id.clone(),
                    expected_lifetime: surface.process_lifetime_id.clone(),
                    cols: 80,
                    rows: 24,
                },
            ))
            .unwrap();
        let restarted = actor
            .snapshot()
            .unwrap()
            .surface(&surface.id)
            .unwrap()
            .clone();
        actor.observe(RuntimeObservation::Exited {
            surface_id: surface.id,
            process_lifetime_id: surface.process_lifetime_id,
            exit_code: 0,
        });
        thread::sleep(Duration::from_millis(30));
        assert_eq!(
            actor
                .snapshot()
                .unwrap()
                .surface(&restarted.id)
                .unwrap()
                .status,
            SurfaceStatus::Starting
        );
    }

    #[test]
    fn lost_shell_restarts_in_its_last_reported_directory() {
        let path = std::env::temp_dir().join(format!(
            "compi-last-directory-{}-{}.json",
            std::process::id(),
            now_ms()
        ));
        let (store, workspace) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        let (actor, effects) = WorkspaceActor::start(store, workspace);
        actor
            .mutate(request(
                &actor,
                "initialize",
                0,
                WorkspaceMutation::Initialize {
                    cols: 80,
                    rows: 24,
                    working_directory: None,
                },
            ))
            .unwrap();
        let WorkspaceEffect::Launch(surface, _) = effects.recv().unwrap() else {
            unreachable!()
        };
        let observe_directory = |directory: &str| {
            actor.observe(RuntimeObservation::Directory {
                surface_id: surface.id.clone(),
                process_lifetime_id: surface.process_lifetime_id.clone(),
                directory: directory.into(),
            });
        };
        // An existing directory, so the resume check passes on every platform.
        let project = std::env::temp_dir().to_string_lossy().into_owned();
        let revision = actor.snapshot().unwrap().revision;
        observe_directory("/home/me/first");
        observe_directory(&project);
        thread::sleep(Duration::from_millis(50));
        // Directory changes are saved without a client-visible revision.
        assert_eq!(actor.snapshot().unwrap().revision, revision);
        drop(actor);
        drop(effects);
        thread::sleep(Duration::from_millis(50));

        // Restarting the daemon loses the shell; restarting it resumes in /home/me/project.
        let (store, reopened) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        assert_eq!(reopened.surfaces[0].status, SurfaceStatus::Lost);
        let (actor, effects) = WorkspaceActor::start(store, reopened);
        let lost = actor.snapshot().unwrap();
        actor
            .mutate(request(
                &actor,
                "restart-lost",
                lost.revision,
                WorkspaceMutation::RestartSurface {
                    surface_id: surface.id.clone(),
                    expected_lifetime: surface.process_lifetime_id.clone(),
                    cols: 80,
                    rows: 24,
                },
            ))
            .unwrap();
        let launched = loop {
            match effects.recv().unwrap() {
                WorkspaceEffect::Launch(launched, _) => break launched,
                WorkspaceEffect::Batch(batch) => {
                    if let Some(WorkspaceEffect::Launch(launched, _)) = batch.into_iter().next() {
                        break launched;
                    }
                }
                WorkspaceEffect::End { .. } => panic!("restart must not end anything"),
            }
        };
        assert_eq!(
            launched.launch.working_directory.as_deref(),
            Some(project.as_str())
        );
        // The recorded launch is where this shell now started.
        assert_eq!(
            actor
                .snapshot()
                .unwrap()
                .surface(&surface.id)
                .unwrap()
                .launch
                .working_directory
                .as_deref(),
            Some(project.as_str())
        );
        drop(actor);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_missing_last_directory_is_not_resumed() {
        let mut surface = SurfaceInfo {
            id: SurfaceId::new("surface-1"),
            process_lifetime_id: ProcessLifetimeId::new("lifetime-1"),
            status: SurfaceStatus::Lost,
            attached: false,
            cols: 80,
            rows: 24,
            created_at_ms: 1,
            exit_code: None,
            error: None,
            launch: LaunchRequest {
                working_directory: None,
                profile: None,
            },
            working_directory: None,
        };
        let missing = format!("/compi-missing-{}-{}", std::process::id(), now_ms());
        if cfg!(windows) {
            // Windows checks a WSL shell's path through its distribution's share.
            surface.working_directory = Some(compi_protocol::WorkingDirectory {
                requested: "/".into(),
                resolved_wsl_path: "/".into(),
                distribution: format!("compi-no-such-distribution-{}", std::process::id()),
                warning: None,
            });
        }
        assert!(!resume_directory_exists(&surface, &missing));
    }

    #[test]
    fn structural_removal_waits_for_cleanup_and_retains_failures() {
        let (actor, effects) = WorkspaceActor::memory();
        actor
            .mutate(request(
                &actor,
                "initialize",
                0,
                WorkspaceMutation::Initialize {
                    cols: 80,
                    rows: 24,
                    working_directory: None,
                },
            ))
            .unwrap();
        let WorkspaceEffect::Launch(surface, _) = effects.recv().unwrap() else {
            unreachable!()
        };
        actor.observe(RuntimeObservation::Running {
            surface_id: surface.id.clone(),
            process_lifetime_id: surface.process_lifetime_id.clone(),
            working_directory: None,
        });
        let running = wait_revision(&actor, 2);
        let tab_id = running.sessions[0].tabs[0].id.clone();

        actor
            .mutate(request(
                &actor,
                "remove-tab",
                running.revision,
                WorkspaceMutation::RemoveTab {
                    tab_id: tab_id.clone(),
                },
            ))
            .unwrap();
        assert!(matches!(
            effects.recv().unwrap(),
            WorkspaceEffect::End { .. }
        ));
        let ending = actor.snapshot().unwrap();
        assert_eq!(
            ending.surface(&surface.id).unwrap().status,
            SurfaceStatus::Ending
        );
        assert_eq!(ending.sessions[0].tabs[0].id, tab_id);

        actor.observe(RuntimeObservation::EndFailed {
            surface_id: surface.id.clone(),
            process_lifetime_id: surface.process_lifetime_id.clone(),
            error: "descendant cleanup failed".into(),
        });
        let retained = wait_revision(&actor, ending.revision + 1);
        let retained_surface = retained.surface(&surface.id).unwrap();
        assert_eq!(retained_surface.status, SurfaceStatus::Ending);
        assert_eq!(
            retained_surface.error.as_deref(),
            Some("descendant cleanup failed")
        );
        assert_eq!(retained.sessions[0].tabs[0].id, tab_id);

        actor.observe(RuntimeObservation::Exited {
            surface_id: surface.id.clone(),
            process_lifetime_id: surface.process_lifetime_id,
            exit_code: 137,
        });
        let removed = wait_revision(&actor, retained.revision + 1);
        assert!(removed.sessions[0].tabs.is_empty());
        assert!(removed.surface(&surface.id).is_none());
    }
    #[test]
    fn nested_divider_paths_and_rejected_geometry_preserve_other_work() {
        let (actor, _effects) = WorkspaceActor::memory();
        actor
            .mutate(request(
                &actor,
                "init-paths",
                0,
                WorkspaceMutation::Initialize {
                    cols: 80,
                    rows: 24,
                    working_directory: None,
                },
            ))
            .unwrap();
        let geometry = compi_protocol::SplitGeometry {
            width: 800.0,
            height: 480.0,
            min_width: 160.0,
            min_height: 80.0,
            divider: 4.0,
        };
        for index in 0..3 {
            let before = actor.snapshot().unwrap();
            let tab = &before.sessions[0].tabs[0];
            fn leaves(node: &LayoutNode, output: &mut Vec<PaneId>) {
                match node {
                    LayoutNode::Pane { pane_id, .. } => output.push(pane_id.clone()),
                    LayoutNode::Split { first, second, .. } => {
                        leaves(first, output);
                        leaves(second, output);
                    }
                }
            }
            let mut panes = Vec::new();
            leaves(&tab.layout, &mut panes);
            let pane_id = if index == 2 {
                panes.last().unwrap().clone()
            } else {
                panes[0].clone()
            };
            let operation = WorkspaceMutation::SplitPane {
                pane_id,
                axis: if index == 0 {
                    SplitAxis::Horizontal
                } else {
                    SplitAxis::Vertical
                },
                cols: 40,
                rows: 12,
                working_directory: None,
                geometry,
            };
            actor
                .mutate(request(
                    &actor,
                    &format!("nested-{index}"),
                    before.revision,
                    operation,
                ))
                .unwrap();
        }
        let before = actor.snapshot().unwrap();
        let tab_id = before.sessions[0].tabs[0].id.clone();
        let operation = WorkspaceMutation::SetSplitRatio {
            tab_id,
            path: vec![],
            ratio: 0.7,
        };
        actor
            .mutate(request(
                &actor,
                "outer-path",
                before.revision,
                operation.clone(),
            ))
            .unwrap();
        let committed = actor.snapshot().unwrap();
        assert!(matches!(&committed.sessions[0].tabs[0].layout,
            LayoutNode::Split { ratio, first, second, .. } if *ratio == 0.7
                && matches!(**first, LayoutNode::Split { ratio: 0.5, .. })
                && matches!(**second, LayoutNode::Split { ratio: 0.5, .. })));
        assert_eq!(committed.surfaces, before.surfaces);
        let stale = actor
            .mutate(request(&actor, "stale-path", before.revision, operation))
            .unwrap_err();
        assert_eq!(stale.code, ErrorCode::RevisionConflict);
        let pane = match &committed.sessions[0].tabs[0].layout {
            LayoutNode::Split { first, .. } => match &**first {
                LayoutNode::Split { first, .. } => match &**first {
                    LayoutNode::Pane { pane_id, .. } => pane_id.clone(),
                    _ => unreachable!(),
                },
                _ => unreachable!(),
            },
            _ => unreachable!(),
        };
        let rejected = actor.mutate(request(
            &actor,
            "too-small",
            committed.revision,
            WorkspaceMutation::SplitPane {
                pane_id: pane,
                axis: SplitAxis::Horizontal,
                cols: 20,
                rows: 4,
                working_directory: None,
                geometry: compi_protocol::SplitGeometry {
                    width: 160.0,
                    ..geometry
                },
            },
        ));
        assert_eq!(rejected.unwrap_err().code, ErrorCode::InvalidRequest);
        assert_eq!(actor.snapshot().unwrap(), committed);
    }

    #[test]
    fn terminal_tabs_allow_automatic_titles_but_workspaces_require_names() {
        let (actor, _effects) = WorkspaceActor::memory();
        let receipt = actor
            .mutate(request(
                &actor,
                "named-workspace",
                0,
                WorkspaceMutation::CreateSession {
                    label: "Project".into(),
                },
            ))
            .unwrap();
        let session_id = receipt.affected_sessions[0].clone();
        let before = actor.snapshot().unwrap();
        let created = actor
            .mutate(request(
                &actor,
                "automatic-title",
                before.revision,
                WorkspaceMutation::CreateTab {
                    session_id,
                    label: String::new(),
                    cols: 80,
                    rows: 24,
                    working_directory: None,
                },
            ))
            .unwrap();
        let snapshot = actor.snapshot().unwrap();
        assert!(snapshot.sessions[0].tabs[0].label.is_empty());
        let surface_id = created.affected_surfaces[0].clone();
        actor
            .mutate(request(
                &actor,
                "explicit-title",
                snapshot.revision,
                WorkspaceMutation::RenameTab {
                    tab_id: created.affected_tabs[0].clone(),
                    label: "Editor".into(),
                },
            ))
            .unwrap();
        let renamed = actor.snapshot().unwrap();
        actor
            .mutate(request(
                &actor,
                "restore-automatic-title",
                renamed.revision,
                WorkspaceMutation::RenameTab {
                    tab_id: created.affected_tabs[0].clone(),
                    label: String::new(),
                },
            ))
            .unwrap();
        let restored = actor.snapshot().unwrap();
        assert!(restored.sessions[0].tabs[0].label.is_empty());
        assert_eq!(restored.surface(&surface_id), snapshot.surface(&surface_id));
        let error = actor
            .mutate(request(
                &actor,
                "blank-workspace",
                restored.revision,
                WorkspaceMutation::CreateSession {
                    label: String::new(),
                },
            ))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert_eq!(actor.snapshot().unwrap(), restored);
    }

    #[test]
    fn launch_environment_is_ephemeral_and_ending_can_retry() {
        let (actor, effects) = WorkspaceActor::memory();
        let mut context = compi_protocol::LaunchContext::default();
        context.profile.executable = Some("/bin/sh".into());
        context
            .env
            .insert("SECRET_TOKEN".into(), "not-persisted".into());
        let mut mutation = request(
            &actor,
            "ephemeral",
            0,
            WorkspaceMutation::Initialize {
                cols: 80,
                rows: 24,
                working_directory: None,
            },
        );
        mutation.launch = Some(Box::new(context));
        actor.mutate(mutation).unwrap();
        let WorkspaceEffect::Launch(surface, Some(context)) = effects.recv().unwrap() else {
            unreachable!()
        };
        assert_eq!(context.env["SECRET_TOKEN"], "not-persisted");
        let snapshot = actor.snapshot().unwrap();
        assert_eq!(
            snapshot.surfaces[0]
                .launch
                .profile
                .as_ref()
                .unwrap()
                .executable
                .as_deref(),
            Some("/bin/sh")
        );
        let published = serde_json::to_string(&snapshot).unwrap();
        assert!(!published.contains("SECRET_TOKEN") && !published.contains("not-persisted"));
        actor.observe(RuntimeObservation::EndFailed {
            surface_id: surface.id.clone(),
            process_lifetime_id: surface.process_lifetime_id.clone(),
            error: "cleanup failed".into(),
        });
        let failed = wait_revision(&actor, snapshot.revision + 1);
        actor
            .mutate(request(
                &actor,
                "retry-end",
                failed.revision,
                WorkspaceMutation::EndSurface {
                    surface_id: surface.id,
                    expected_lifetime: surface.process_lifetime_id,
                },
            ))
            .unwrap();
        assert!(matches!(
            effects.recv().unwrap(),
            WorkspaceEffect::End { .. }
        ));
    }
}
