use crate::workspace_store::{PendingRemoval, RemovalTarget, StoredWorkspace, WorkspaceStore};
use compi_protocol::{
    ErrorCode, LaunchRequest, LayoutNode, MAX_MUTATION_RECEIPTS, MutationId, MutationReceipt,
    MutationRequest, PaneId, ProcessLifetimeId, ServerGeneration, SessionId, SplitAxis, SurfaceId,
    SurfaceInfo, SurfaceStatus, TabId, WorkspaceMutation, WorkspaceSession, WorkspaceSnapshot,
    WorkspaceTab,
};
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
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
    Launch(SurfaceInfo),
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
    reply: Option<SyncSender<std::result::Result<MutationReceipt, ActorError>>>,
}

struct ActorState {
    workspace: StoredWorkspace,
    generation: ServerGeneration,
    effect_sender: SyncSender<WorkspaceEffect>,
    subscribers: Vec<SyncSender<WorkspaceEvent>>,
    queued: VecDeque<ActorCommand>,
    pending: Option<PendingCommit>,
    read_only_error: Option<String>,
    next_ordinal: u64,
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
        read_only_error: None,
        next_ordinal: 1,
    };

    loop {
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
    matches!(command, ActorCommand::Mutate(..) | ActorCommand::Observe(_))
}

fn reject_busy(command: ActorCommand, revision: u64) {
    if let ActorCommand::Mutate(_, reply) = command {
        let _ = reply.send(Err(ActorError::new(
            ErrorCode::Busy,
            "workspace mutation queue is full",
            Some(revision),
        )));
    }
}

fn process_command(
    state: &mut ActorState,
    command: ActorCommand,
    persist_sender: &SyncSender<PersistJob>,
) -> bool {
    match command {
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
            if let Some(error) = state.read_only_error.as_ref() {
                let _ = reply.send(Err(ActorError::new(
                    ErrorCode::PersistenceUnavailable,
                    error.clone(),
                    Some(state.workspace.revision),
                )));
                return false;
            }
            match prepare_mutation(state, &request) {
                Ok(Prepared::Duplicate(receipt)) => {
                    let _ = reply.send(Ok(receipt));
                }
                Ok(Prepared::Commit(mut pending)) => {
                    pending.reply = Some(reply);
                    if persist_sender
                        .send(PersistJob {
                            candidate: pending.candidate.clone(),
                        })
                        .is_err()
                    {
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
            for effect in pending.effects {
                if state.effect_sender.send(effect).is_err() {
                    state.read_only_error = Some("workspace runtime effect worker stopped".into());
                    break;
                }
            }
            if let (Some(reply), Some(receipt)) = (pending.reply, pending.receipt) {
                let _ = reply.send(Ok(receipt));
            }
        }
        Err(error) => {
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

enum Prepared {
    Duplicate(MutationReceipt),
    Commit(PendingCommit),
}

fn prepare_mutation(
    state: &mut ActorState,
    request: &MutationRequest,
) -> std::result::Result<Prepared, ActorError> {
    let fingerprint = fingerprint(request).map_err(|error| {
        ActorError::new(
            ErrorCode::InvalidRequest,
            format!("could not fingerprint mutation: {error}"),
            Some(state.workspace.revision),
        )
    })?;
    if let Some(receipt) = state
        .workspace
        .receipts
        .iter()
        .find(|receipt| receipt.mutation_id == request.mutation_id)
    {
        return if receipt.fingerprint == fingerprint {
            Ok(Prepared::Duplicate(receipt.clone()))
        } else {
            Err(ActorError::new(
                ErrorCode::MutationIdReused,
                "mutation ID was already used for different content",
                Some(state.workspace.revision),
            ))
        };
    }
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
    let operation_state = apply_mutation(
        &mut candidate,
        &request.operation,
        &mut state.next_ordinal,
        &mut affected_sessions,
        &mut affected_tabs,
        &mut affected_panes,
        &mut affected_surfaces,
        &mut effects,
    )?;
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
    Ok(Prepared::Commit(PendingCommit {
        candidate,
        receipt: Some(receipt),
        effects,
        reply: None,
    }))
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
                    label: "Shell".into(),
                    layout: LayoutNode::Pane {
                        pane_id: pane_id.clone(),
                        surface_id: surface.id.clone(),
                    },
                }],
            });
            workspace.initialized = true;
            affected_sessions.push(session_id);
            affected_tabs.push(tab_id);
            affected_panes.push(pane_id);
            affected_surfaces.push(surface.id.clone());
            effects.push(WorkspaceEffect::Launch(surface.clone()));
            workspace.surfaces.push(surface);
            Ok("starting".into())
        }
        WorkspaceMutation::CreateSession { label } => {
            validate_label(label).map_err(invalid)?;
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
            validate_label(label).map_err(invalid)?;
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
            validate_label(label).map_err(invalid)?;
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
            });
            affected_sessions.push(session_id.clone());
            affected_tabs.push(tab_id);
            affected_panes.push(pane_id);
            affected_surfaces.push(surface.id.clone());
            effects.push(WorkspaceEffect::Launch(surface.clone()));
            workspace.surfaces.push(surface);
            Ok("starting".into())
        }
        WorkspaceMutation::RenameTab { tab_id, label } => {
            validate_label(label).map_err(invalid)?;
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
        } => {
            dimensions(*cols, *rows).map_err(invalid)?;
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
            effects.push(WorkspaceEffect::Launch(surface.clone()));
            workspace.surfaces.push(surface);
            Ok("starting".into())
        }
        WorkspaceMutation::SetSplitRatio {
            tab_id,
            pane_id,
            ratio,
        } => {
            if !ratio.is_finite() || *ratio <= 0.0 || *ratio >= 1.0 {
                return Err(invalid(
                    "split ratio must be finite and strictly between zero and one".into(),
                ));
            }
            let tab = find_tab_mut(&mut workspace.sessions, tab_id)
                .ok_or_else(|| invalid(format!("tab {tab_id} was not found")))?;
            if !set_parent_ratio(&mut tab.layout, pane_id, *ratio) {
                return Err(invalid(format!(
                    "pane {pane_id} has no containing split in tab {tab_id}"
                )));
            }
            affected_tabs.push(tab_id.clone());
            affected_panes.push(pane_id.clone());
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
                SurfaceStatus::Starting | SurfaceStatus::Running
            ) {
                return Err(ActorError::new(
                    ErrorCode::SurfaceUnavailable,
                    format!("surface {surface_id} is not running"),
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
            surface.process_lifetime_id = ProcessLifetimeId::new(allocate("lifetime", ordinal));
            surface.status = SurfaceStatus::Starting;
            surface.attached = false;
            surface.cols = *cols;
            surface.rows = *rows;
            surface.exit_code = None;
            surface.error = None;
            effects.push(WorkspaceEffect::Launch(surface.clone()));
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
        launch: LaunchRequest { working_directory },
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

fn set_parent_ratio(node: &mut LayoutNode, target: &PaneId, ratio: f32) -> bool {
    match node {
        LayoutNode::Pane { .. } => false,
        LayoutNode::Split {
            ratio: current,
            first,
            second,
            ..
        } => {
            if set_parent_ratio(first, target, ratio) || set_parent_ratio(second, target, ratio) {
                true
            } else if contains_pane(first, target) || contains_pane(second, target) {
                *current = ratio;
                true
            } else {
                false
            }
        }
    }
}

fn contains_pane(node: &LayoutNode, target: &PaneId) -> bool {
    match node {
        LayoutNode::Pane { pane_id, .. } => pane_id == target,
        LayoutNode::Split { first, second, .. } => {
            contains_pane(first, target) || contains_pane(second, target)
        }
    }
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

fn validate_label(label: &str) -> std::result::Result<(), String> {
    if label.trim().is_empty() {
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
                },
            ))
            .unwrap();
        assert!(matches!(
            actor.snapshot().unwrap().sessions[0].tabs[0].layout,
            LayoutNode::Split { .. }
        ));
        assert!(matches!(
            effects.recv().unwrap(),
            WorkspaceEffect::Launch(_)
        ));
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
        let WorkspaceEffect::Launch(surface) = effects.recv().unwrap() else {
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
        let WorkspaceEffect::Launch(surface) = effects.recv().unwrap() else {
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
}
