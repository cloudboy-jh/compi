use crate::Result;
use crate::conpty::ConptySession;
use crate::frame;
use crate::pipe;
use crate::protocol::{
    CONTROL_FRAME, ErrorCode, SCREEN_FRAME, ServerControl, ServerMessage, SessionInfo,
    SessionStatus, WorkingDirectory, encode_server,
};
use crate::session_store::SessionStore;
use crate::terminal::{ScreenMessage, TerminalState, encode_screen};
use crate::wsl::{self, WslLaunch};
use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::IO::CancelSynchronousIo;
use windows::Win32::System::Threading::{GetCurrentThreadId, OpenThread, THREAD_TERMINATE};

const TRANSPORT_CHUNK: usize = 32 * 1024;
const CLIENT_QUEUE_FRAMES: usize = 256;
const CLIENT_SCREEN_QUEUE_FRAMES: usize = 32;

pub struct SessionManager {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    store: SessionStore,
    next_id: AtomicU64,
}

pub struct Session {
    id: String,
    created_at_ms: u64,
    state: Mutex<SessionRuntime>,
    input: Mutex<Option<File>>,
    commands: SyncSender<SessionCommand>,
    worker: Mutex<Option<JoinHandle<()>>>,
    store: SessionStore,
    working_directory: Option<WorkingDirectory>,
}

struct SessionRuntime {
    status: SessionStatus,
    cols: i16,
    rows: i16,
    exit_code: Option<u32>,
    error: Option<String>,
    terminal: TerminalState,
    client: Option<ConnectionSink>,
}

enum SessionCommand {
    Resize {
        cols: i16,
        rows: i16,
        acknowledgement: SyncSender<std::result::Result<(), String>>,
    },
    Kill,
}

#[derive(Clone)]
pub struct ConnectionSink {
    id: u64,
    connection: Arc<File>,
    sender: SyncSender<Outgoing>,
    alive: Arc<AtomicBool>,
    queued_frames: Arc<AtomicUsize>,
    writer_thread: Arc<OwnedHandle>,
}

struct Outgoing {
    kind: u8,
    payload: Vec<u8>,
    acknowledgement: Option<SyncSender<std::result::Result<(), String>>>,
}

#[derive(Debug)]
pub enum SessionError {
    AlreadyAttached,
    NotAttached,
    Exited,
    Internal(String),
}

impl SessionError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::AlreadyAttached => ErrorCode::AlreadyAttached,
            Self::NotAttached => ErrorCode::NotAttached,
            Self::Exited => ErrorCode::SessionExited,
            Self::Internal(_) => ErrorCode::Internal,
        }
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyAttached => write!(formatter, "session already has a controlling client"),
            Self::NotAttached => write!(formatter, "connection is not attached to this session"),
            Self::Exited => write!(formatter, "session is not running"),
            Self::Internal(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for SessionError {}

impl SessionManager {
    pub fn new() -> Self {
        Self::with_store(SessionStore::memory())
    }

    pub fn persistent(instance: Option<&str>) -> Result<Self> {
        Ok(Self::with_store(SessionStore::open(instance)?))
    }

    fn with_store(store: SessionStore) -> Self {
        let next_id = store.next_ordinal();
        Self {
            sessions: Mutex::new(HashMap::new()),
            store,
            next_id: AtomicU64::new(next_id),
        }
    }

    pub fn create(
        &self,
        cols: i16,
        rows: i16,
        working_directory: Option<String>,
    ) -> Result<Arc<Session>> {
        validate_dimensions(cols, rows)?;
        let launch = wsl::resolve_launch(working_directory.as_deref())?;
        let created_at_ms = now_ms();
        let ordinal = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = format!("s-{created_at_ms:x}-{ordinal:x}");
        self.store.record(&SessionInfo {
            id: id.clone(),
            status: SessionStatus::Starting,
            attached: false,
            cols,
            rows,
            created_at_ms,
            exit_code: None,
            error: None,
            working_directory: launch.metadata.clone(),
        })?;
        let session = match Session::spawn(
            id.clone(),
            created_at_ms,
            cols,
            rows,
            launch.clone(),
            self.store.clone(),
        ) {
            Ok(session) => session,
            Err(error) => {
                let _ = self.store.record(&SessionInfo {
                    id,
                    status: SessionStatus::Failed,
                    attached: false,
                    cols,
                    rows,
                    created_at_ms,
                    exit_code: None,
                    error: Some(error.to_string()),
                    working_directory: launch.metadata,
                });
                return Err(error);
            }
        };
        let mut sessions = match self.sessions.lock() {
            Ok(sessions) => sessions,
            Err(_) => {
                let _ = session.kill();
                session.join();
                return Err("session registry lock was poisoned".into());
            }
        };
        sessions.insert(id, session.clone());
        Ok(session)
    }

    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.lock().ok()?.get(id).cloned()
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        let mut sessions: HashMap<_, _> = self
            .store
            .list()
            .into_iter()
            .map(|session| (session.id.clone(), session))
            .collect();
        if let Ok(live) = self.sessions.lock() {
            for session in live.values().map(|session| session.info()) {
                sessions.insert(session.id.clone(), session);
            }
        }
        let mut sessions: Vec<_> = sessions.into_values().collect();
        sessions.sort_by_key(|session| (session.created_at_ms, session.id.clone()));
        sessions
    }

    pub fn get_info(&self, id: &str) -> Option<SessionInfo> {
        self.get(id)
            .map(|session| session.info())
            .or_else(|| self.store.get(id))
    }

    pub fn session_count(&self) -> usize {
        self.sessions
            .lock()
            .map(|sessions| sessions.len())
            .unwrap_or_default()
    }

    pub fn shutdown_all(&self, reason: &str) {
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .map(|sessions| sessions.values().cloned().collect())
            .unwrap_or_default();
        let active_ids: Vec<_> = sessions
            .iter()
            .filter(|session| {
                matches!(
                    session.info().status,
                    SessionStatus::Starting | SessionStatus::Running
                )
            })
            .map(|session| session.id.clone())
            .collect();
        for session in &sessions {
            let _ = session.kill();
        }
        for session in sessions {
            session.join();
        }
        if let Err(error) = self.store.mark_dead(&active_ids, reason) {
            eprintln!("compi-daemon: could not persist daemon shutdown state: {error}");
        }
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    fn spawn(
        id: String,
        created_at_ms: u64,
        cols: i16,
        rows: i16,
        launch: WslLaunch,
        store: SessionStore,
    ) -> Result<Arc<Self>> {
        let mut conpty = ConptySession::spawn(
            cols,
            rows,
            launch.distribution.as_deref(),
            &launch.directory,
        )?;
        let (input, mut output) = conpty.take_io()?;
        let (command_sender, command_receiver) = sync_channel(64);
        let session = Arc::new(Self {
            id,
            created_at_ms,
            state: Mutex::new(SessionRuntime {
                status: SessionStatus::Starting,
                cols,
                rows,
                exit_code: None,
                error: None,
                terminal: TerminalState::new(cols as u16, rows as u16),
                client: None,
            }),
            input: Mutex::new(Some(input)),
            commands: command_sender,
            worker: Mutex::new(None),
            store,
            working_directory: launch.metadata,
        });

        let output_session = session.clone();
        let output_thread = thread::spawn(move || {
            let mut buffer = [0_u8; TRANSPORT_CHUNK];
            loop {
                let read = match output.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(_) => break,
                };
                let (sink, delta, replies) = {
                    let Ok(mut state) = output_session.state.lock() else {
                        break;
                    };
                    let (delta, replies) = state.terminal.advance(&buffer[..read]);
                    (state.client.clone(), delta, replies)
                };
                if !replies.is_empty() {
                    let Ok(mut input) = output_session.input.lock() else {
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
                    && sink.send_screen(&ScreenMessage::Delta { delta }).is_err()
                {
                    output_session.detach_connection(sink.id);
                }
            }
        });

        let (worker_start, worker_ready) = sync_channel(0);
        let worker_session = session.clone();
        let worker = thread::spawn(move || {
            if worker_ready.recv().is_ok() {
                worker_session.run_worker(conpty, command_receiver, output_thread);
            }
            worker_session.release_worker_handle();
        });
        *session
            .worker
            .lock()
            .map_err(|_| "session worker lock was poisoned")? = Some(worker);
        worker_start
            .send(())
            .map_err(|_| "session worker stopped before startup")?;
        session
            .state
            .lock()
            .map_err(|_| "session state lock was poisoned")?
            .status = SessionStatus::Running;
        if let Err(error) = session.persist() {
            let _ = session.kill();
            session.join();
            return Err(error);
        }
        Ok(session)
    }

    fn run_worker(
        &self,
        mut conpty: ConptySession,
        commands: Receiver<SessionCommand>,
        output_thread: JoinHandle<()>,
    ) {
        let mut failure = None;
        let exit_code = 'running: loop {
            loop {
                match commands.try_recv() {
                    Ok(SessionCommand::Resize {
                        cols,
                        rows,
                        acknowledgement,
                    }) => match conpty.resize_owned(cols, rows) {
                        Ok(()) => {
                            let update = self.state.lock().ok().map(|mut state| {
                                state.cols = cols;
                                state.rows = rows;
                                let delta = state.terminal.resize(cols as u16, rows as u16);
                                (state.client.clone(), delta)
                            });
                            if let Some((Some(sink), Some(delta))) = update
                                && sink.send_screen(&ScreenMessage::Delta { delta }).is_err()
                            {
                                self.detach_connection(sink.id);
                            }
                            let _ = acknowledgement.send(Ok(()));
                        }
                        Err(error) => {
                            let message = format!("ConPTY resize failed: {error}");
                            let _ = acknowledgement.send(Err(message.clone()));
                            failure = Some(message);
                            let _ = conpty.terminate(1);
                        }
                    },
                    Ok(SessionCommand::Kill) => {
                        if let Err(error) = conpty.terminate(137) {
                            failure = Some(format!("session termination failed: {error}"));
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        let _ = conpty.terminate(1);
                        break;
                    }
                }
            }

            match conpty.wait(50) {
                Ok(Some(code)) => break 'running code,
                Ok(None) => {}
                Err(error) => {
                    failure = Some(format!("process wait failed: {error}"));
                    let _ = conpty.terminate(1);
                    break 'running 1;
                }
            }
        };

        conpty.close_pseudoconsole();
        let _ = output_thread.join();
        if let Ok(mut input) = self.input.lock() {
            input.take();
        }

        let sink = if let Ok(mut state) = self.state.lock() {
            state.exit_code = Some(exit_code);
            state.status = if failure.is_some() {
                SessionStatus::Failed
            } else {
                SessionStatus::Exited
            };
            state.error = failure;
            state.client.take()
        } else {
            None
        };
        if let Err(error) = self.persist() {
            eprintln!(
                "compi-daemon: could not persist terminal state for {}: {error}",
                self.id
            );
        }
        if let Some(sink) = sink {
            let _ = sink.send_control(&ServerControl {
                request_id: None,
                message: ServerMessage::SessionExited {
                    session_id: self.id.clone(),
                    exit_code,
                },
            });
        }
    }

    pub fn info(&self) -> SessionInfo {
        self.state
            .lock()
            .map(|state| SessionInfo {
                id: self.id.clone(),
                status: state.status,
                attached: state.client.as_ref().is_some_and(ConnectionSink::is_alive),
                cols: state.cols,
                rows: state.rows,
                created_at_ms: self.created_at_ms,
                exit_code: state.exit_code,
                error: state.error.clone(),
                working_directory: self.working_directory.clone(),
            })
            .unwrap_or_else(|_| SessionInfo {
                id: self.id.clone(),
                status: SessionStatus::Failed,
                attached: false,
                cols: 0,
                rows: 0,
                created_at_ms: self.created_at_ms,
                exit_code: None,
                error: Some("session state lock was poisoned".into()),
                working_directory: self.working_directory.clone(),
            })
    }

    pub fn attach(
        &self,
        sink: ConnectionSink,
        request_id: u64,
        cols: i16,
        rows: i16,
    ) -> std::result::Result<(), SessionError> {
        validate_dimensions(cols, rows)
            .map_err(|error| SessionError::Internal(error.to_string()))?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| SessionError::Internal("session state lock was poisoned".into()))?;
        if state.status != SessionStatus::Running {
            return Err(SessionError::Exited);
        }
        if state.client.as_ref().is_some_and(ConnectionSink::is_alive) {
            return Err(SessionError::AlreadyAttached);
        }

        state.client = Some(sink.clone());
        let mut info = self.info_from_state(&state);
        info.attached = true;
        let snapshot = state.terminal.snapshot();
        if let Err(error) = sink.send_control(&ServerControl {
            request_id: Some(request_id),
            message: ServerMessage::Attached {
                session: info,
                sequence: snapshot.sequence,
            },
        }) {
            state.client = None;
            return Err(SessionError::Internal(error.to_string()));
        }
        if let Err(error) = sink.send_screen_recovery(&ScreenMessage::Snapshot { snapshot }) {
            state.client = None;
            return Err(SessionError::Internal(error.to_string()));
        }
        drop(state);
        self.resize(sink.id, cols, rows)
    }

    pub fn detach(
        &self,
        connection_id: u64,
        request_id: u64,
    ) -> std::result::Result<(), SessionError> {
        let sink = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| SessionError::Internal("session state lock was poisoned".into()))?;
            let Some(sink) = state.client.as_ref() else {
                return Err(SessionError::NotAttached);
            };
            if sink.id != connection_id {
                return Err(SessionError::NotAttached);
            }
            let sink = sink.clone();
            state.client = None;
            sink
        };
        sink.send_control(&ServerControl {
            request_id: Some(request_id),
            message: ServerMessage::Detached {
                session_id: self.id.clone(),
            },
        })
        .map_err(|error| SessionError::Internal(error.to_string()))
    }

    pub fn request_snapshot(
        &self,
        connection_id: u64,
        request_id: u64,
    ) -> std::result::Result<(), SessionError> {
        let state = self
            .state
            .lock()
            .map_err(|_| SessionError::Internal("session state lock was poisoned".into()))?;
        let Some(sink) = state
            .client
            .as_ref()
            .filter(|sink| sink.id == connection_id && sink.is_alive())
        else {
            return Err(SessionError::NotAttached);
        };
        let snapshot = state.terminal.snapshot();
        sink.send_control(&ServerControl {
            request_id: Some(request_id),
            message: ServerMessage::SnapshotReady {
                sequence: snapshot.sequence,
            },
        })
        .and_then(|_| sink.send_screen_recovery(&ScreenMessage::Snapshot { snapshot }))
        .map_err(|error| SessionError::Internal(error.to_string()))
    }

    pub fn detach_connection(&self, connection_id: u64) {
        if let Ok(mut state) = self.state.lock()
            && state
                .client
                .as_ref()
                .is_some_and(|sink| sink.id == connection_id)
        {
            state.client = None;
        }
    }

    pub fn write_input(
        &self,
        connection_id: u64,
        bytes: &[u8],
    ) -> std::result::Result<(), SessionError> {
        self.require_attached(connection_id)?;
        let mut input = self
            .input
            .lock()
            .map_err(|_| SessionError::Internal("ConPTY input lock was poisoned".into()))?;
        let input = input.as_mut().ok_or(SessionError::Exited)?;
        input
            .write_all(bytes)
            .and_then(|_| input.flush())
            .map_err(|error| SessionError::Internal(error.to_string()))
    }

    pub fn resize(
        &self,
        connection_id: u64,
        cols: i16,
        rows: i16,
    ) -> std::result::Result<(), SessionError> {
        validate_dimensions(cols, rows)
            .map_err(|error| SessionError::Internal(error.to_string()))?;
        self.require_attached(connection_id)?;
        let (acknowledgement, result) = sync_channel(0);
        self.commands
            .try_send(SessionCommand::Resize {
                cols,
                rows,
                acknowledgement,
            })
            .map_err(|error| SessionError::Internal(format!("resize queue failed: {error}")))?;
        result
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| SessionError::Internal("timed out resizing ConPTY".into()))?
            .map_err(SessionError::Internal)?;
        self.persist()
            .map_err(|error| SessionError::Internal(error.to_string()))
    }

    pub fn kill(&self) -> std::result::Result<(), SessionError> {
        if self.info().status != SessionStatus::Running {
            return Err(SessionError::Exited);
        }
        self.commands
            .try_send(SessionCommand::Kill)
            .map_err(|error| SessionError::Internal(format!("kill queue failed: {error}")))
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

    fn require_attached(&self, connection_id: u64) -> std::result::Result<(), SessionError> {
        let state = self
            .state
            .lock()
            .map_err(|_| SessionError::Internal("session state lock was poisoned".into()))?;
        if state.status != SessionStatus::Running {
            return Err(SessionError::Exited);
        }
        if state
            .client
            .as_ref()
            .is_some_and(|sink| sink.id == connection_id && sink.is_alive())
        {
            Ok(())
        } else {
            Err(SessionError::NotAttached)
        }
    }

    fn persist(&self) -> Result<()> {
        self.store.record(&self.info())
    }

    fn info_from_state(&self, state: &SessionRuntime) -> SessionInfo {
        SessionInfo {
            id: self.id.clone(),
            status: state.status,
            attached: state.client.as_ref().is_some_and(ConnectionSink::is_alive),
            cols: state.cols,
            rows: state.rows,
            created_at_ms: self.created_at_ms,
            exit_code: state.exit_code,
            error: state.error.clone(),
            working_directory: self.working_directory.clone(),
        }
    }
}

impl ConnectionSink {
    pub fn new(id: u64, connection: Arc<File>) -> Result<Self> {
        let (sender, receiver) = sync_channel::<Outgoing>(CLIENT_QUEUE_FRAMES);
        let (thread_sender, thread_receiver) = sync_channel(0);
        let alive = Arc::new(AtomicBool::new(true));
        let writer_connection = connection.clone();
        let writer_alive = alive.clone();
        let queued_frames = Arc::new(AtomicUsize::new(0));
        let writer_queued_frames = queued_frames.clone();
        thread::spawn(move || {
            let thread_handle = unsafe {
                OpenThread(THREAD_TERMINATE, false, GetCurrentThreadId())
                    .map(|handle| OwnedHandle::from_raw_handle(handle.0))
            };
            if thread_sender.send(thread_handle).is_err() {
                writer_alive.store(false, Ordering::Release);
                return;
            }

            let result = (|| -> Result<()> {
                while let Ok(message) = receiver.recv() {
                    writer_queued_frames.fetch_sub(1, Ordering::AcqRel);
                    let mut writer = &*writer_connection;
                    let write_result = frame::write(&mut writer, message.kind, &message.payload);
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

        let writer_thread = thread_receiver
            .recv()
            .map_err(|_| "connection writer failed to start")??;
        Ok(Self {
            id,
            connection,
            sender,
            alive,
            queued_frames,
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

    pub fn send_screen(&self, message: &ScreenMessage) -> Result<bool> {
        if self.queued_frames.load(Ordering::Acquire) >= CLIENT_SCREEN_QUEUE_FRAMES {
            return Ok(false);
        }
        let payload = encode_screen(message)?;
        self.send(SCREEN_FRAME, &payload).map(|_| true)
    }

    fn send_screen_recovery(&self, message: &ScreenMessage) -> Result<()> {
        let payload = encode_screen(message)?;
        self.send(SCREEN_FRAME, &payload)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub fn disconnect(&self) {
        self.alive.store(false, Ordering::Release);
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
    if cols <= 0 || rows <= 0 {
        return Err("terminal dimensions must be positive".into());
    }
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}
