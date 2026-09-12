use crate::Result;
use crate::launch::{self, LaunchDescription};
use crate::pty::{PtySession, PtyWriter};
use crate::terminal::TerminalState;
use crate::terminal::trace::TerminalTraceRecorder;
use crate::workspace::{
    ActorError, RuntimeObservation, WorkspaceActor, WorkspaceEffect, WorkspaceEvent,
};
use compi_protocol::ScreenMessage;
use compi_protocol::frame;
use compi_protocol::pipe;
use compi_protocol::{
    AttachmentId, CONTROL_FRAME, ErrorCode, MutationId, MutationReceipt, MutationRequest,
    ProcessLifetimeId, SCREEN_FRAME, ServerControl, ServerMessage, SurfaceId, SurfaceInfo,
    SurfaceStatus, TerminalFrame, TerminalIdentity, TerminalTarget, WorkingDirectory,
    WorkspaceSnapshot, encode_server, encode_terminal_frame,
};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
#[cfg(windows)]
use windows::Win32::Foundation::HANDLE;
#[cfg(windows)]
use windows::Win32::System::IO::CancelSynchronousIo;
#[cfg(windows)]
use windows::Win32::System::Threading::{GetCurrentThreadId, OpenThread, THREAD_TERMINATE};

const TRANSPORT_CHUNK: usize = 32 * 1024;
const CLIENT_QUEUE_FRAMES: usize = 256;
const CLIENT_SCREEN_QUEUE_FRAMES: usize = 32;
const MAX_PENDING_LATENCY_IDS: usize = 4_096;
static NEXT_ATTACHMENT: AtomicU64 = AtomicU64::new(1);

pub struct SurfaceManager {
    surfaces: Arc<Mutex<HashMap<SurfaceId, Arc<Surface>>>>,
    actor: WorkspaceActor,
}

pub struct Surface {
    id: SurfaceId,
    process_lifetime_id: ProcessLifetimeId,
    identity: TerminalIdentity,
    created_revision: u64,
    retired: AtomicBool,
    created_at_ms: u64,
    launch_request: compi_protocol::LaunchRequest,
    state: Mutex<SurfaceRuntime>,
    input: Mutex<Option<PtyWriter>>,
    trace: Mutex<Option<TerminalTraceRecorder>>,
    pending_latency: Mutex<VecDeque<PendingLatency>>,
    commands: SyncSender<SurfaceCommand>,
    worker: Mutex<Option<JoinHandle<()>>>,
    worker_finished: AtomicBool,
    pending_cleanup: Mutex<Option<PtySession>>,
    actor: WorkspaceActor,
    working_directory: Option<WorkingDirectory>,
}

struct SurfaceRuntime {
    status: SurfaceStatus,
    cols: i16,
    rows: i16,
    exit_code: Option<u32>,
    error: Option<String>,
    terminal: TerminalState,
    client: Option<(ConnectionSink, AttachmentId)>,
    client_ready: bool,
}

struct PendingLatency {
    id: u64,
    output_received: bool,
}

enum SurfaceCommand {
    Resize {
        cols: i16,
        rows: i16,
        acknowledgement: SyncSender<std::result::Result<(), String>>,
    },
    Kill {
        for_removal: bool,
    },
}

#[derive(Clone)]
pub struct ConnectionSink {
    id: u64,
    connection: Arc<File>,
    sender: SyncSender<Outgoing>,
    alive: Arc<AtomicBool>,
    queued_frames: Arc<AtomicUsize>,
    #[cfg(windows)]
    writer_thread: Arc<OwnedHandle>,
}

struct Outgoing {
    kind: u8,
    payload: Vec<u8>,
    acknowledgement: Option<SyncSender<std::result::Result<(), String>>>,
}

#[derive(Debug)]
pub enum SurfaceError {
    AlreadyAttached,
    NotAttached,
    Unavailable,
    StaleLifetime,
    Internal(String),
}

impl SurfaceError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::AlreadyAttached => ErrorCode::AlreadyAttached,
            Self::NotAttached => ErrorCode::NotAttached,
            Self::Unavailable => ErrorCode::SurfaceUnavailable,
            Self::StaleLifetime => ErrorCode::StaleLifetime,
            Self::Internal(_) => ErrorCode::Internal,
        }
    }
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyAttached => write!(formatter, "surface already has a controlling client"),
            Self::NotAttached => write!(formatter, "connection is not attached to this surface"),
            Self::Unavailable => write!(formatter, "surface is not running"),
            Self::StaleLifetime => write!(formatter, "surface process lifetime changed"),
            Self::Internal(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for SurfaceError {}

impl SurfaceManager {
    pub fn new() -> Self {
        let (actor, effects) = WorkspaceActor::memory();
        Self::with_actor(actor, effects)
    }

    pub fn persistent(instance: Option<&str>) -> Result<Self> {
        let (actor, effects) = WorkspaceActor::persistent(instance)?;
        Ok(Self::with_actor(actor, effects))
    }

    fn with_actor(actor: WorkspaceActor, effects: Receiver<WorkspaceEffect>) -> Self {
        let surfaces = Arc::new(Mutex::new(HashMap::<SurfaceId, Arc<Surface>>::new()));
        let worker_surfaces = surfaces.clone();
        let worker_actor = actor.clone();
        thread::spawn(move || {
            while let Ok(effect) = effects.recv() {
                match effect {
                    WorkspaceEffect::Launch(info, context) => {
                        if let Ok(registry) = worker_surfaces.lock()
                            && let Some(previous) = registry.get(&info.id)
                        {
                            previous.retire();
                        }
                        let launch = match launch::resolve_profile(&info.launch, context.as_deref())
                        {
                            Ok(launch) => launch,
                            Err(error) => {
                                worker_actor.observe(RuntimeObservation::Failed {
                                    surface_id: info.id,
                                    process_lifetime_id: info.process_lifetime_id,
                                    error: error.to_string(),
                                });
                                continue;
                            }
                        };
                        let surface_id = info.id.clone();
                        let lifetime = info.process_lifetime_id.clone();
                        match Surface::spawn(info, launch, context.as_deref(), worker_actor.clone())
                        {
                            Ok(surface) => {
                                if let Ok(mut registry) = worker_surfaces.lock()
                                    && let Some(previous) =
                                        registry.insert(surface_id.clone(), surface)
                                {
                                    previous.join();
                                }
                            }
                            Err(error) => worker_actor.observe(RuntimeObservation::Failed {
                                surface_id,
                                process_lifetime_id: lifetime,
                                error: error.to_string(),
                            }),
                        }
                    }
                    WorkspaceEffect::End {
                        surface_id,
                        process_lifetime_id,
                    } => {
                        let surface = worker_surfaces
                            .lock()
                            .ok()
                            .and_then(|registry| registry.get(&surface_id).cloned());
                        match surface {
                            Some(surface) if surface.process_lifetime_id == process_lifetime_id => {
                                if let Err(error) = surface.kill(true) {
                                    worker_actor.observe(RuntimeObservation::EndFailed {
                                        surface_id,
                                        process_lifetime_id,
                                        error: error.to_string(),
                                    });
                                }
                            }
                            Some(_) => {}
                            None => worker_actor.observe(RuntimeObservation::EndFailed {
                                surface_id,
                                process_lifetime_id,
                                error: "surface runtime was not found".into(),
                            }),
                        }
                    }
                }
            }
        });
        Self { surfaces, actor }
    }

    pub fn actor(&self) -> &WorkspaceActor {
        &self.actor
    }

    pub fn snapshot(&self) -> std::result::Result<WorkspaceSnapshot, ActorError> {
        let mut snapshot = self.actor.snapshot()?;
        if let Ok(mut surfaces) = self.surfaces.lock() {
            let lifetimes: HashMap<_, _> = snapshot
                .surfaces
                .iter()
                .map(|surface| (&surface.id, &surface.process_lifetime_id))
                .collect();
            surfaces.retain(|id, surface| {
                // A runtime may have been published after this snapshot was captured.
                let current = surface.created_revision > snapshot.revision
                    || lifetimes
                        .get(id)
                        .is_some_and(|lifetime| **lifetime == surface.process_lifetime_id);
                if !current {
                    surface.retire();
                }
                current
            });
            drop(lifetimes);
            for persisted in &mut snapshot.surfaces {
                if let Some(runtime) = surfaces.get(&persisted.id)
                    && runtime.process_lifetime_id == persisted.process_lifetime_id
                {
                    persisted.attached = runtime.info().attached;
                }
            }
        }
        Ok(snapshot)
    }

    pub fn mutate(
        &self,
        request: MutationRequest,
    ) -> std::result::Result<MutationReceipt, ActorError> {
        self.actor.mutate(request)
    }

    pub fn outcome(
        &self,
        mutation_id: MutationId,
    ) -> std::result::Result<Option<MutationReceipt>, ActorError> {
        self.actor.outcome(mutation_id)
    }

    pub fn subscribe(&self) -> Receiver<WorkspaceEvent> {
        self.actor.subscribe()
    }

    pub fn get(&self, id: &SurfaceId) -> Option<Arc<Surface>> {
        self.surfaces.lock().ok()?.get(id).cloned()
    }

    pub fn get_info(&self, id: &SurfaceId) -> Option<SurfaceInfo> {
        self.get(id)
            .map(|surface| surface.info())
            .or_else(|| self.snapshot().ok()?.surface(id).cloned())
    }

    pub fn surface_count(&self) -> usize {
        self.surfaces
            .lock()
            .map(|surfaces| surfaces.len())
            .unwrap_or_default()
    }

    pub fn shutdown_all(&self, _reason: &str) {
        let surfaces: Vec<_> = self
            .surfaces
            .lock()
            .map(|surfaces| surfaces.values().cloned().collect())
            .unwrap_or_default();
        for surface in &surfaces {
            let _ = surface.kill(false);
        }
        for surface in surfaces {
            surface.join();
        }
    }
}

impl Default for SurfaceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Surface {
    fn spawn(
        info: SurfaceInfo,
        launch: LaunchDescription,
        context: Option<&compi_protocol::LaunchContext>,
        actor: WorkspaceActor,
    ) -> Result<Arc<Self>> {
        let mut pty = PtySession::spawn(&launch, info.cols, info.rows)?;
        let (input, mut output) = pty.take_io()?;
        let (command_sender, command_receiver) = sync_channel(64);
        let trace = match TerminalTraceRecorder::from_env(
            info.id.as_str(),
            info.cols as u16,
            info.rows as u16,
        ) {
            Ok(trace) => trace,
            Err(error) => {
                eprintln!(
                    "compi-daemon: could not enable terminal trace for {}: {error}",
                    info.id
                );
                None
            }
        };
        let trace_label = trace
            .as_ref()
            .and_then(|trace| trace.path().file_stem())
            .and_then(|stem| stem.to_str())
            .map(str::to_owned);
        let mut terminal = TerminalState::new(info.cols as u16, info.rows as u16);
        if let Some(context) = context {
            terminal.set_resource_limits(context.scrollback_lines, context.graphics_bytes);
        }
        terminal.set_diagnostic_context(info.id.as_str(), trace_label.as_deref());
        let workspace = actor
            .snapshot()
            .map_err(|error| format!("could not read workspace identity: {error}"))?;
        let identity = TerminalIdentity {
            server_id: workspace.server_id,
            server_generation: workspace.server_generation,
            surface_id: info.id.clone(),
            process_lifetime_id: info.process_lifetime_id.clone(),
        };
        let surface = Arc::new(Self {
            id: info.id,
            process_lifetime_id: info.process_lifetime_id,
            identity,
            created_revision: workspace.revision,
            retired: AtomicBool::new(false),
            created_at_ms: info.created_at_ms,
            launch_request: info.launch,
            state: Mutex::new(SurfaceRuntime {
                status: SurfaceStatus::Starting,
                cols: info.cols,
                rows: info.rows,
                exit_code: None,
                error: None,
                terminal,
                client: None,
                client_ready: false,
            }),
            input: Mutex::new(Some(input)),
            trace: Mutex::new(trace),
            pending_latency: Mutex::new(VecDeque::new()),
            commands: command_sender,
            worker: Mutex::new(None),
            worker_finished: AtomicBool::new(false),
            pending_cleanup: Mutex::new(None),
            actor,
            working_directory: launch.metadata,
        });

        let output_surface = surface.clone();
        let output_thread = thread::spawn(move || {
            let mut buffer = [0_u8; TRANSPORT_CHUNK];
            loop {
                let read = match output.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                if compi_protocol::perf::enabled()
                    && let Ok(mut pending) = output_surface.pending_latency.lock()
                {
                    for latency in pending
                        .iter_mut()
                        .filter(|latency| !latency.output_received)
                    {
                        latency.output_received = true;
                        compi_protocol::perf::log_input_latency_stage(
                            latency.id,
                            "pty_output",
                            None,
                        );
                    }
                }
                output_surface.record_trace_output(&buffer[..read]);
                let (sink, mut delta, replies) = {
                    let Ok(mut state) = output_surface.state.lock() else {
                        break;
                    };
                    let (delta, replies) = state.terminal.advance(&buffer[..read]);
                    (
                        state
                            .client
                            .as_ref()
                            .filter(|_| state.client_ready)
                            .map(|(sink, _)| sink.clone()),
                        delta,
                        replies,
                    )
                };
                if let Some(delta) = delta.as_mut()
                    && let Ok(mut pending) = output_surface.pending_latency.lock()
                {
                    while pending
                        .front()
                        .is_some_and(|latency| latency.output_received)
                    {
                        let latency = pending.pop_front().expect("checked pending latency");
                        compi_protocol::perf::log_input_latency_stage(
                            latency.id,
                            "terminal_state",
                            Some(delta.sequence),
                        );
                        delta.latency_ids.push(latency.id);
                    }
                }
                if !replies.is_empty() {
                    let Ok(mut input) = output_surface.input.lock() else {
                        break;
                    };
                    let Some(input) = input.as_mut() else {
                        break;
                    };
                    for reply in replies {
                        if input.write_all(&reply).is_err() {
                            break;
                        }
                    }
                    let _ = input.flush();
                }
                if let (Some(sink), Some(delta)) = (sink, delta)
                    && sink
                        .send_screen(&TerminalFrame {
                            identity: output_surface.identity.clone(),
                            message: ScreenMessage::Delta {
                                delta: crate::screen::delta(delta),
                            },
                        })
                        .is_err()
                {
                    output_surface.detach_connection(sink.id);
                }
            }
        });

        let (worker_start, worker_ready) = sync_channel(0);
        let worker_surface = surface.clone();
        let worker = thread::spawn(move || {
            if worker_ready.recv().is_ok() {
                worker_surface.run_worker(pty, command_receiver, output_thread);
            }
            worker_surface
                .worker_finished
                .store(true, Ordering::Release);
            worker_surface.release_worker_handle();
        });
        *surface
            .worker
            .lock()
            .map_err(|_| "surface worker lock was poisoned")? = Some(worker);
        surface
            .state
            .lock()
            .map_err(|_| "surface state lock was poisoned")?
            .status = SurfaceStatus::Running;
        worker_start
            .send(())
            .map_err(|_| "surface worker stopped before startup")?;
        surface.actor.observe(RuntimeObservation::Running {
            surface_id: surface.id.clone(),
            process_lifetime_id: surface.process_lifetime_id.clone(),
            working_directory: surface.working_directory.clone(),
        });
        Ok(surface)
    }

    fn run_worker(
        &self,
        mut pty: PtySession,
        commands: Receiver<SurfaceCommand>,
        output_thread: JoinHandle<()>,
    ) {
        let mut failure = None;
        let mut termination_deadline = None;
        let mut removal_requested = false;
        let exit_code = 'running: loop {
            loop {
                match commands.try_recv() {
                    Ok(SurfaceCommand::Resize {
                        cols,
                        rows,
                        acknowledgement,
                    }) => match pty.resize(cols, rows) {
                        Ok(()) => {
                            let update = self.state.lock().ok().map(|mut state| {
                                state.cols = cols;
                                state.rows = rows;
                                let delta = state.terminal.resize(cols as u16, rows as u16);
                                (
                                    state
                                        .client
                                        .as_ref()
                                        .filter(|_| state.client_ready)
                                        .map(|(sink, _)| sink.clone()),
                                    delta,
                                )
                            });
                            self.record_trace_resize(cols as u16, rows as u16);
                            if let Some((Some(sink), Some(delta))) = update
                                && sink
                                    .send_screen(&TerminalFrame {
                                        identity: self.identity.clone(),
                                        message: ScreenMessage::Delta {
                                            delta: crate::screen::delta(delta),
                                        },
                                    })
                                    .is_err()
                            {
                                self.detach_connection(sink.id);
                            }
                            let _ = acknowledgement.send(Ok(()));
                        }
                        Err(error) => {
                            let message = format!("PTY resize failed: {error}");
                            let _ = acknowledgement.send(Err(message.clone()));
                            failure = Some(message);
                            let _ = pty.terminate(1);
                            termination_deadline.get_or_insert_with(|| {
                                std::time::Instant::now() + Duration::from_secs(2)
                            });
                        }
                    },
                    Ok(SurfaceCommand::Kill { for_removal }) => {
                        removal_requested |= for_removal;
                        if let Err(error) = pty.terminate(137) {
                            failure = Some(format!("surface termination failed: {error}"));
                        }
                        termination_deadline.get_or_insert_with(|| {
                            std::time::Instant::now() + Duration::from_secs(2)
                        });
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        let _ = pty.terminate(1);
                        termination_deadline.get_or_insert_with(|| {
                            std::time::Instant::now() + Duration::from_secs(2)
                        });
                        break;
                    }
                }
            }

            match pty.wait(50) {
                Ok(Some(code)) => break 'running code,
                Ok(None) => {}
                Err(error) => {
                    failure = Some(format!("process wait failed: {error}"));
                    let _ = pty.terminate(1);
                    break 'running 1;
                }
            }
            if termination_deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                failure = Some("process did not exit after termination".into());
                break 'running 1;
            }
        };

        let cleanup_failed = match pty.close() {
            Ok(()) => false,
            Err(error) => {
                failure = Some(format!("PTY descendant cleanup failed: {error}"));
                if let Ok(mut pending) = self.pending_cleanup.lock() {
                    *pending = Some(pty);
                }
                true
            }
        };
        if removal_requested && !cleanup_failed {
            // Final owned-process cleanup is authoritative even if the initial
            // termination request raced a natural exit or exceeded its grace period.
            failure = None;
        }
        let _ = output_thread.join();
        if let Ok(mut input) = self.input.lock() {
            input.take();
        }

        let client = if let Ok(mut state) = self.state.lock() {
            state.exit_code = Some(exit_code);
            state.status = if cleanup_failed {
                SurfaceStatus::Ending
            } else if failure.is_some() {
                SurfaceStatus::Failed
            } else {
                SurfaceStatus::Exited
            };
            state.error = failure.clone();
            state.client_ready = false;
            state.client.take()
        } else {
            None
        };
        if let Some(error) = failure {
            let observation = if removal_requested || cleanup_failed {
                RuntimeObservation::EndFailed {
                    surface_id: self.id.clone(),
                    process_lifetime_id: self.process_lifetime_id.clone(),
                    error,
                }
            } else {
                RuntimeObservation::Failed {
                    surface_id: self.id.clone(),
                    process_lifetime_id: self.process_lifetime_id.clone(),
                    error,
                }
            };
            self.actor.observe(observation);
        } else {
            self.actor.observe(RuntimeObservation::Exited {
                surface_id: self.id.clone(),
                process_lifetime_id: self.process_lifetime_id.clone(),
                exit_code,
            });
        }
        if let Some((sink, _)) = client {
            let _ = sink.send_control(&ServerControl {
                request_id: None,
                message: ServerMessage::SurfaceExited {
                    identity: self.identity.clone(),
                    exit_code,
                },
            });
        }
    }

    pub fn info(&self) -> SurfaceInfo {
        self.state
            .lock()
            .map(|state| self.info_from_state(&state))
            .unwrap_or_else(|_| SurfaceInfo {
                id: self.id.clone(),
                process_lifetime_id: self.process_lifetime_id.clone(),
                status: SurfaceStatus::Failed,
                attached: false,
                cols: 0,
                rows: 0,
                created_at_ms: self.created_at_ms,
                exit_code: None,
                error: Some("surface state lock was poisoned".into()),
                launch: self.launch_request.clone(),
                working_directory: self.working_directory.clone(),
            })
    }

    pub fn attach(
        &self,
        sink: ConnectionSink,
        request_id: u64,
        expected_lifetime: &ProcessLifetimeId,
        cols: i16,
        rows: i16,
    ) -> std::result::Result<(), SurfaceError> {
        validate_dimensions(cols, rows)
            .map_err(|error| SurfaceError::Internal(error.to_string()))?;
        let attachment_id = AttachmentId::new(format!(
            "attachment-{:x}-{:x}",
            sink.id,
            NEXT_ATTACHMENT.fetch_add(1, Ordering::Relaxed)
        ));
        let target = TerminalTarget {
            attachment_id: attachment_id.clone(),
            identity: self.identity.clone(),
        };
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| SurfaceError::Internal("surface state lock was poisoned".into()))?;
            if self.retired.load(Ordering::Acquire)
                || expected_lifetime != &self.process_lifetime_id
            {
                return Err(SurfaceError::StaleLifetime);
            }
            if !matches!(
                state.status,
                SurfaceStatus::Running | SurfaceStatus::Exited | SurfaceStatus::Failed
            ) {
                return Err(SurfaceError::Unavailable);
            }
            if state
                .client
                .as_ref()
                .is_some_and(|(sink, _)| sink.is_alive())
            {
                return Err(SurfaceError::AlreadyAttached);
            }
            state.client = Some((sink.clone(), attachment_id.clone()));
            state.client_ready = false;
        }
        let result = (|| -> std::result::Result<(), SurfaceError> {
            // Reserve control without publishing deltas, size the PTY/grid, then
            // publish the authoritative baseline under the same lock as retirement.
            self.resize(&target, cols, rows)?;
            let mut state = self
                .state
                .lock()
                .map_err(|_| SurfaceError::Internal("surface state lock was poisoned".into()))?;
            self.validate_target(&target)?;
            if !state
                .client
                .as_ref()
                .is_some_and(|(current, id)| id == &attachment_id && current.is_alive())
            {
                return Err(SurfaceError::NotAttached);
            }
            let mut info = self.info_from_state(&state);
            info.attached = true;
            let snapshot = crate::screen::snapshot(state.terminal.snapshot());
            sink.send_control(&ServerControl {
                request_id: Some(request_id),
                message: ServerMessage::Attached {
                    identity: self.identity.clone(),
                    surface: info,
                    attachment_id: attachment_id.clone(),
                    sequence: snapshot.sequence,
                },
            })
            .map_err(|error| SurfaceError::Internal(error.to_string()))?;
            sink.send_screen_recovery(&TerminalFrame {
                identity: self.identity.clone(),
                message: ScreenMessage::Snapshot { snapshot },
            })
            .map_err(|error| SurfaceError::Internal(error.to_string()))?;
            state.client_ready = true;
            Ok(())
        })();
        if result.is_err()
            && let Ok(mut state) = self.state.lock()
            && state
                .client
                .as_ref()
                .is_some_and(|(_, id)| id == &attachment_id)
        {
            state.client = None;
            state.client_ready = false;
        }
        result
    }

    pub fn detach(
        &self,
        target: &TerminalTarget,
        request_id: u64,
    ) -> std::result::Result<(), SurfaceError> {
        let sink = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| SurfaceError::Internal("surface state lock was poisoned".into()))?;
            self.validate_target(target)?;
            let Some((sink, attachment_id)) = state.client.as_ref() else {
                return Err(SurfaceError::NotAttached);
            };
            if attachment_id != &target.attachment_id || !sink.is_alive() {
                return Err(SurfaceError::NotAttached);
            }
            let sink = sink.clone();
            state.client = None;
            state.client_ready = false;
            sink
        };
        sink.send_control(&ServerControl {
            request_id: Some(request_id),
            message: ServerMessage::Detached {
                surface_id: self.id.clone(),
            },
        })
        .map_err(|error| SurfaceError::Internal(error.to_string()))
    }

    pub fn request_snapshot(
        &self,
        target: &TerminalTarget,
        request_id: u64,
    ) -> std::result::Result<(), SurfaceError> {
        self.validate_target(target)?;
        let state = self
            .state
            .lock()
            .map_err(|_| SurfaceError::Internal("surface state lock was poisoned".into()))?;
        let Some((sink, _attachment_id)) = state.client.as_ref().filter(|(sink, attachment_id)| {
            attachment_id == &target.attachment_id && sink.is_alive()
        }) else {
            return Err(SurfaceError::NotAttached);
        };
        let snapshot = crate::screen::snapshot(state.terminal.snapshot());
        sink.send_control(&ServerControl {
            request_id: Some(request_id),
            message: ServerMessage::SnapshotReady {
                sequence: snapshot.sequence,
            },
        })
        .and_then(|_| {
            sink.send_screen_recovery(&TerminalFrame {
                identity: self.identity.clone(),
                message: ScreenMessage::Snapshot { snapshot },
            })
        })
        .map_err(|error| SurfaceError::Internal(error.to_string()))
    }

    pub fn clear_scrollback(
        &self,
        target: &TerminalTarget,
        request_id: u64,
    ) -> std::result::Result<(), SurfaceError> {
        self.validate_target(target)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| SurfaceError::Internal("surface state lock was poisoned".into()))?;
        let sink = state
            .client
            .as_ref()
            .filter(|(sink, attachment_id)| {
                attachment_id == &target.attachment_id && sink.is_alive()
            })
            .map(|(sink, _)| sink.clone())
            .ok_or(SurfaceError::NotAttached)?;
        state.terminal.clear_scrollback();
        let snapshot = crate::screen::snapshot(state.terminal.snapshot());
        sink.send_control(&ServerControl {
            request_id: Some(request_id),
            message: ServerMessage::SnapshotReady {
                sequence: snapshot.sequence,
            },
        })
        .and_then(|_| {
            sink.send_screen_recovery(&TerminalFrame {
                identity: self.identity.clone(),
                message: ScreenMessage::Snapshot { snapshot },
            })
        })
        .map_err(|error| SurfaceError::Internal(error.to_string()))
    }

    pub fn detach_connection(&self, connection_id: u64) {
        if let Ok(mut state) = self.state.lock()
            && state
                .client
                .as_ref()
                .is_some_and(|(sink, _)| sink.id == connection_id)
        {
            state.client = None;
            state.client_ready = false;
        }
    }

    pub fn write_input(
        &self,
        target: &TerminalTarget,
        bytes: &[u8],
        latency_id: Option<u64>,
    ) -> std::result::Result<(), SurfaceError> {
        self.require_attached(target)?;
        let mut input_guard = self
            .input
            .lock()
            .map_err(|_| SurfaceError::Internal("PTY input lock was poisoned".into()))?;
        let input = input_guard.as_mut().ok_or(SurfaceError::Unavailable)?;
        let mut pending_latency = latency_id
            .filter(|_| compi_protocol::perf::enabled())
            .map(|id| {
                compi_protocol::perf::log_input_latency_stage(id, "daemon_input", None);
                self.pending_latency
                    .lock()
                    .map(|mut pending| {
                        if pending.len() >= MAX_PENDING_LATENCY_IDS {
                            pending.pop_front();
                        }
                        pending.push_back(PendingLatency {
                            id,
                            output_received: false,
                        });
                    })
                    .map_err(|_| SurfaceError::Internal("latency queue lock was poisoned".into()))
            })
            .transpose()?;
        if let Err(error) = input.write_all(bytes).and_then(|_| input.flush()) {
            if pending_latency.is_some()
                && let Ok(mut pending) = self.pending_latency.lock()
            {
                pending.pop_back();
            }
            return Err(SurfaceError::Internal(error.to_string()));
        }
        pending_latency.take();
        drop(input_guard);
        self.record_trace_input(bytes);
        Ok(())
    }

    pub fn resize(
        &self,
        target: &TerminalTarget,
        cols: i16,
        rows: i16,
    ) -> std::result::Result<(), SurfaceError> {
        validate_dimensions(cols, rows)
            .map_err(|error| SurfaceError::Internal(error.to_string()))?;
        self.validate_target(target)?;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| SurfaceError::Internal("surface state lock was poisoned".into()))?;
            if state.cols == cols && state.rows == rows {
                return if state
                    .client
                    .as_ref()
                    .is_some_and(|(sink, id)| id == &target.attachment_id && sink.is_alive())
                {
                    Ok(())
                } else {
                    Err(SurfaceError::NotAttached)
                };
            }
            if matches!(state.status, SurfaceStatus::Exited | SurfaceStatus::Failed) {
                let sink = state
                    .client
                    .as_ref()
                    .filter(|(sink, id)| id == &target.attachment_id && sink.is_alive())
                    .map(|(sink, _)| sink.clone())
                    .ok_or(SurfaceError::NotAttached)?;
                state.terminal.resize(cols as u16, rows as u16);
                state.cols = cols;
                state.rows = rows;
                if state.client_ready {
                    let snapshot = crate::screen::snapshot(state.terminal.snapshot());
                    sink.send_screen_recovery(&TerminalFrame {
                        identity: self.identity.clone(),
                        message: ScreenMessage::Snapshot { snapshot },
                    })
                    .map_err(|error| SurfaceError::Internal(error.to_string()))?;
                }
                drop(state);
                self.actor.observe(RuntimeObservation::Resized {
                    surface_id: self.id.clone(),
                    process_lifetime_id: self.process_lifetime_id.clone(),
                    cols,
                    rows,
                });
                return Ok(());
            }
        }
        self.require_attached(target)?;
        let (acknowledgement, result) = sync_channel(0);
        self.commands
            .try_send(SurfaceCommand::Resize {
                cols,
                rows,
                acknowledgement,
            })
            .map_err(|error| SurfaceError::Internal(format!("resize queue failed: {error}")))?;
        result
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| SurfaceError::Internal("timed out resizing PTY".into()))?
            .map_err(SurfaceError::Internal)?;
        self.actor.observe(RuntimeObservation::Resized {
            surface_id: self.id.clone(),
            process_lifetime_id: self.process_lifetime_id.clone(),
            cols,
            rows,
        });
        Ok(())
    }

    fn record_trace_input(&self, bytes: &[u8]) {
        self.record_trace(|trace| trace.record_input(bytes));
    }

    fn record_trace_output(&self, bytes: &[u8]) {
        self.record_trace(|trace| trace.record_output(bytes));
    }

    fn record_trace_resize(&self, cols: u16, rows: u16) {
        self.record_trace(|trace| trace.record_resize(cols, rows));
    }

    fn record_trace(&self, record: impl FnOnce(&mut TerminalTraceRecorder) -> std::io::Result<()>) {
        let Ok(mut trace) = self.trace.lock() else {
            return;
        };
        let Some(recorder) = trace.as_mut() else {
            return;
        };
        if let Err(error) = record(recorder) {
            eprintln!(
                "compi-daemon: terminal trace disabled after write failure for {}: {error}",
                self.id
            );
            trace.take();
        }
    }

    fn kill(&self, for_removal: bool) -> std::result::Result<(), SurfaceError> {
        let mut pending = self
            .pending_cleanup
            .lock()
            .map_err(|_| SurfaceError::Internal("cleanup ownership lock was poisoned".into()))?;
        if let Some(pty) = pending.as_mut() {
            if !self.worker_finished.load(Ordering::Acquire) {
                return Err(SurfaceError::Internal(
                    "Cleanup is still finishing; retry termination shortly".into(),
                ));
            }
            pty.close().map_err(|error| {
                SurfaceError::Internal(format!("PTY descendant cleanup retry failed: {error}"))
            })?;
            pending.take();
            drop(pending);
            let exit_code = {
                let mut state = self.state.lock().map_err(|_| {
                    SurfaceError::Internal("surface state lock was poisoned".into())
                })?;
                state.status = SurfaceStatus::Exited;
                state.error = None;
                state.exit_code.unwrap_or(137)
            };
            self.actor.observe(RuntimeObservation::Exited {
                surface_id: self.id.clone(),
                process_lifetime_id: self.process_lifetime_id.clone(),
                exit_code,
            });
            return Ok(());
        }
        drop(pending);
        if !matches!(
            self.info().status,
            SurfaceStatus::Starting | SurfaceStatus::Running | SurfaceStatus::Ending
        ) {
            return Err(SurfaceError::Unavailable);
        }
        self.commands
            .try_send(SurfaceCommand::Kill { for_removal })
            .map_err(|error| SurfaceError::Internal(format!("kill queue failed: {error}")))
    }

    pub fn join(&self) {
        let worker = self.worker.lock().ok().and_then(|mut worker| worker.take());
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }

    fn release_worker_handle(&self) {
        if let Ok(mut worker) = self.worker.lock() {
            worker.take();
        }
    }

    fn retire(&self) {
        let _state = self.state.lock();
        self.retired.store(true, Ordering::Release);
    }

    fn validate_target(&self, target: &TerminalTarget) -> std::result::Result<(), SurfaceError> {
        if self.retired.load(Ordering::Acquire) {
            return Err(SurfaceError::StaleLifetime);
        }
        if target.identity == self.identity {
            Ok(())
        } else if target.identity.surface_id == self.id {
            Err(SurfaceError::StaleLifetime)
        } else {
            Err(SurfaceError::NotAttached)
        }
    }

    fn require_attached(&self, target: &TerminalTarget) -> std::result::Result<(), SurfaceError> {
        self.validate_target(target)?;
        let state = self
            .state
            .lock()
            .map_err(|_| SurfaceError::Internal("surface state lock was poisoned".into()))?;
        if state.status != SurfaceStatus::Running {
            return Err(SurfaceError::Unavailable);
        }
        if state.client.as_ref().is_some_and(|(sink, attachment_id)| {
            attachment_id == &target.attachment_id && sink.is_alive()
        }) {
            Ok(())
        } else {
            Err(SurfaceError::NotAttached)
        }
    }

    fn info_from_state(&self, state: &SurfaceRuntime) -> SurfaceInfo {
        SurfaceInfo {
            id: self.id.clone(),
            process_lifetime_id: self.process_lifetime_id.clone(),
            status: state.status,
            attached: state.client_ready
                && state
                    .client
                    .as_ref()
                    .is_some_and(|(sink, _)| sink.is_alive()),
            cols: state.cols,
            rows: state.rows,
            created_at_ms: self.created_at_ms,
            exit_code: state.exit_code,
            error: state.error.clone(),
            launch: self.launch_request.clone(),
            working_directory: self.working_directory.clone(),
        }
    }
}

impl ConnectionSink {
    pub fn new(id: u64, connection: Arc<File>) -> Result<Self> {
        let (sender, receiver) = sync_channel::<Outgoing>(CLIENT_QUEUE_FRAMES);
        #[cfg(windows)]
        let (thread_sender, thread_receiver) = sync_channel(0);
        let alive = Arc::new(AtomicBool::new(true));
        let writer_connection = connection.clone();
        let writer_alive = alive.clone();
        let queued_frames = Arc::new(AtomicUsize::new(0));
        let writer_queued_frames = queued_frames.clone();
        thread::spawn(move || {
            #[cfg(windows)]
            {
                let thread_handle = unsafe {
                    OpenThread(THREAD_TERMINATE, false, GetCurrentThreadId())
                        .map(|handle| OwnedHandle::from_raw_handle(handle.0))
                };
                if thread_sender.send(thread_handle).is_err() {
                    writer_alive.store(false, Ordering::Release);
                    return;
                }
            }

            let result = (|| -> Result<()> {
                while let Ok(message) = receiver.recv() {
                    writer_queued_frames.fetch_sub(1, Ordering::AcqRel);
                    let mut writer = &*writer_connection;
                    let write_result = frame::write(&mut writer, message.kind, &message.payload)
                        .map_err(Into::into)
                        .and_then(|()| {
                            // DisconnectNamedPipe discards unread bytes. A synchronous
                            // response must be consumed before its handler tears down
                            // the connection; the caller's timeout cancels stalled IO.
                            if message.acknowledgement.is_some() {
                                pipe::flush(&writer_connection)
                            } else {
                                Ok(())
                            }
                        });
                    if let Some(acknowledgement) = message.acknowledgement {
                        let _ = acknowledgement.send(
                            write_result
                                .as_ref()
                                .map(|_| ())
                                .map_err(ToString::to_string),
                        );
                    }
                    write_result?;
                }
                Ok(())
            })();
            writer_alive.store(false, Ordering::Release);
            if result.is_err() {
                pipe::disconnect(&writer_connection);
            }
        });

        #[cfg(windows)]
        let writer_thread = thread_receiver
            .recv()
            .map_err(|_| "connection writer failed to start")??;
        Ok(Self {
            id,
            connection,
            sender,
            alive,
            queued_frames,
            #[cfg(windows)]
            writer_thread: Arc::new(writer_thread),
        })
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn send_control(&self, message: &ServerControl) -> Result<()> {
        let payload = encode_server(message)?;
        self.send(CONTROL_FRAME, &payload)
    }

    pub fn send_control_sync(&self, message: &ServerControl) -> Result<()> {
        let payload = encode_server(message)?;
        let (sender, receiver) = sync_channel(0);
        self.enqueue(Outgoing {
            kind: CONTROL_FRAME,
            payload,
            acknowledgement: Some(sender),
        })?;
        receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "timed out writing control response")?
            .map_err(Into::into)
    }

    pub fn send_screen(&self, message: &TerminalFrame) -> Result<bool> {
        if self.queued_frames.load(Ordering::Acquire) >= CLIENT_SCREEN_QUEUE_FRAMES {
            return Ok(false);
        }
        let payload = encode_terminal_frame(message)?;
        self.send(SCREEN_FRAME, &payload).map(|_| true)
    }

    fn send_screen_recovery(&self, message: &TerminalFrame) -> Result<()> {
        let payload = encode_terminal_frame(message)?;
        self.send(SCREEN_FRAME, &payload)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub fn disconnect(&self) {
        self.alive.store(false, Ordering::Release);
        #[cfg(windows)]
        let _ = unsafe { CancelSynchronousIo(HANDLE(self.writer_thread.as_raw_handle())) };
        pipe::disconnect(&self.connection);
    }

    fn send(&self, kind: u8, payload: &[u8]) -> Result<()> {
        self.enqueue(Outgoing {
            kind,
            payload: payload.to_vec(),
            acknowledgement: None,
        })
    }

    fn enqueue(&self, outgoing: Outgoing) -> Result<()> {
        if !self.is_alive() {
            return Err("client connection is closed".into());
        }
        self.queued_frames.fetch_add(1, Ordering::AcqRel);
        match self.sender.try_send(outgoing) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.queued_frames.fetch_sub(1, Ordering::AcqRel);
                self.disconnect();
                Err("client output queue is full".into())
            }
            Err(TrySendError::Disconnected(_)) => {
                self.queued_frames.fetch_sub(1, Ordering::AcqRel);
                self.alive.store(false, Ordering::Release);
                Err("client writer stopped".into())
            }
        }
    }
}

fn validate_dimensions(cols: i16, rows: i16) -> Result<()> {
    if cols <= 0 || rows <= 0 || cols > 1_000 || rows > 1_000 {
        return Err("terminal dimensions must be between 1 and 1000".into());
    }
    Ok(())
}
