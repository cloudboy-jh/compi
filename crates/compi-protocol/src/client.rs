use crate::Result;
use crate::frame;
use crate::{
    CONTROL_FRAME, ClientControl, ClientMessage, DirectoryEntry, ErrorCode,
    IMAGE_UPLOAD_CHUNK_BYTES, MAX_IMAGE_UPLOAD_BYTES, MutationId, MutationReceipt, MutationRequest,
    PROTOCOL_VERSION, RuntimeMetrics, SCREEN_FRAME, SearchEntry, ServerMessage, SurfaceId,
    SurfaceInfo, SurfaceStatus, TerminalTarget, WorkspaceMutation, WorkspaceSnapshot,
    decode_server, decode_terminal_frame, encode_client,
};
use crate::{identity, pipe};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fs::File;
use std::io::Read;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static NEXT_MUTATION: AtomicU64 = AtomicU64::new(1);

#[cfg(windows)]
struct ProcessExit(std::os::windows::io::OwnedHandle);
#[cfg(unix)]
struct ProcessExit(u32);

impl ProcessExit {
    fn open(pid: u32) -> Result<Self> {
        if pid == 0 || pid == std::process::id() {
            return Err(
                "cannot wait for an invalid or current process as an external daemon".into(),
            );
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::FromRawHandle;
            use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
            let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid)? };
            Ok(Self(unsafe {
                std::os::windows::io::OwnedHandle::from_raw_handle(handle.0)
            }))
        }
        #[cfg(unix)]
        {
            Ok(Self(pid))
        }
    }

    fn wait(&self, deadline: Instant) -> Result<()> {
        loop {
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawHandle;
                use windows::Win32::{
                    Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
                    System::Threading::WaitForSingleObject,
                };
                match unsafe { WaitForSingleObject(HANDLE(self.0.as_raw_handle()), 0) } {
                    WAIT_OBJECT_0 => return Ok(()),
                    WAIT_TIMEOUT => {}
                    _ => return Err(std::io::Error::last_os_error().into()),
                }
            }
            #[cfg(unix)]
            {
                if unsafe { libc::kill(self.0 as i32, 0) } == -1 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() == Some(libc::ESRCH) {
                        return Ok(());
                    }
                    return Err(error.into());
                }
            }
            if Instant::now() >= deadline {
                return Err("daemon or supervisor did not exit after acknowledged shutdown".into());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionFailureKind {
    Absent,
    Incompatible,
    Unauthorized,
    Transport,
}

#[derive(Debug)]
pub struct ConnectionFailure {
    pub kind: ConnectionFailureKind,
    pub message: String,
}

impl std::fmt::Display for ConnectionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ConnectionFailure {}

impl ConnectionFailure {
    pub fn kind(error: &(dyn std::error::Error + Send + Sync + 'static)) -> ConnectionFailureKind {
        error
            .downcast_ref::<Self>()
            .map_or(ConnectionFailureKind::Transport, |error| error.kind)
    }

    fn transport(error: crate::Error) -> crate::Error {
        let mut kind = ConnectionFailureKind::Transport;
        if let Some(io) = error.downcast_ref::<std::io::Error>() {
            kind = match io.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                    ConnectionFailureKind::Absent
                }
                std::io::ErrorKind::PermissionDenied => ConnectionFailureKind::Unauthorized,
                _ => ConnectionFailureKind::Transport,
            };
        }
        #[cfg(windows)]
        if let Some(win) = error.downcast_ref::<windows::core::Error>() {
            kind = match win.code().0 as u32 & 0xffff {
                2 | 3 => ConnectionFailureKind::Absent,
                5 => ConnectionFailureKind::Unauthorized,
                _ => ConnectionFailureKind::Transport,
            };
        }
        Box::new(Self {
            kind,
            message: error.to_string(),
        })
    }
}

#[derive(Debug)]
pub struct DaemonError {
    pub code: ErrorCode,
    pub message: String,
    pub current_revision: Option<u64>,
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "daemon error ({:?}): {}",
            self.code, self.message
        )
    }
}

impl std::error::Error for DaemonError {}

pub enum ServerEvent {
    Control {
        request_id: Option<u64>,
        message: ServerMessage,
    },
    Screen(crate::ScreenMessage),
}

pub struct ClientIo {
    reader: File,
    writer: Option<File>,
    child: Option<Child>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_thread: Option<JoinHandle<()>>,
}

impl ClientIo {
    fn local(connection: File) -> Self {
        Self {
            reader: connection,
            writer: None,
            child: None,
            stderr: Arc::new(Mutex::new(Vec::new())),
            stderr_thread: None,
        }
    }

    fn command(mut child: Child) -> Result<Self> {
        let stdin = child.stdin.take().ok_or("transport stdin was not piped")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("transport stdout was not piped")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("transport stderr was not piped")?;
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let stderr_thread = Some(capture_stderr(stderr, diagnostics.clone()));
        Ok(Self {
            reader: child_stdout_file(stdout),
            writer: Some(child_stdin_file(stdin)),
            child: Some(child),
            stderr: diagnostics,
            stderr_thread,
        })
    }

    pub fn reader(&self) -> &File {
        &self.reader
    }

    pub fn writer(&self) -> &File {
        self.writer.as_ref().unwrap_or(&self.reader)
    }

    fn finish_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(thread) = self.stderr_thread.take() {
            let _ = thread.join();
        }
    }

    fn diagnostics(&mut self) -> String {
        self.finish_child();
        self.stderr
            .lock()
            .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned())
            .unwrap_or_default()
    }
}

impl Drop for ClientIo {
    fn drop(&mut self) {
        self.finish_child();
    }
}

fn capture_stderr(mut stderr: ChildStderr, diagnostics: Arc<Mutex<Vec<u8>>>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut chunk = [0_u8; 4096];
        while let Ok(count) = stderr.read(&mut chunk) {
            if count == 0 {
                break;
            }
            if let Ok(mut stored) = diagnostics.lock() {
                let remaining = (64 * 1024_usize).saturating_sub(stored.len());
                stored.extend_from_slice(&chunk[..count.min(remaining)]);
            }
        }
    })
}

#[cfg(unix)]
fn child_stdout_file(stdout: ChildStdout) -> File {
    use std::os::fd::{FromRawFd, IntoRawFd};
    unsafe { File::from_raw_fd(stdout.into_raw_fd()) }
}

#[cfg(unix)]
fn child_stdin_file(stdin: ChildStdin) -> File {
    use std::os::fd::{FromRawFd, IntoRawFd};
    unsafe { File::from_raw_fd(stdin.into_raw_fd()) }
}

#[cfg(windows)]
fn child_stdout_file(stdout: ChildStdout) -> File {
    use std::os::windows::io::{FromRawHandle, IntoRawHandle};
    unsafe { File::from_raw_handle(stdout.into_raw_handle()) }
}

#[cfg(windows)]
fn child_stdin_file(stdin: ChildStdin) -> File {
    use std::os::windows::io::{FromRawHandle, IntoRawHandle};
    unsafe { File::from_raw_handle(stdin.into_raw_handle()) }
}

pub struct DaemonClient {
    connection: ClientIo,
    next_request_id: u64,
    pending_screen: VecDeque<crate::ScreenMessage>,
    pending_screen_sizes: VecDeque<usize>,
    pending_screen_bytes: usize,
    last_screen_bytes: usize,
    poll_reader: pipe::PipeReader,
    target: Option<TerminalTarget>,
    workspace: Option<WorkspaceSnapshot>,
}

impl DaemonClient {
    pub fn connect(instance: Option<&str>, timeout: Duration) -> Result<Self> {
        let names = identity::instance_names(instance)?;
        Self::connect_to(&names.pipe, timeout)
    }

    pub fn connect_to(pipe_name: &str, timeout: Duration) -> Result<Self> {
        let connection = pipe::connect(pipe_name, timeout).map_err(ConnectionFailure::transport)?;
        Self::handshake(Self::from_parts(connection, 1))
    }

    pub fn connect_command(command: &mut Command) -> Result<Self> {
        let child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let connection = ClientIo::command(child)?;
        Self::handshake(Self::from_io(connection, 1))
    }

    fn handshake(mut client: Self) -> Result<Self> {
        let response = client.request_bounded(
            ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
            Duration::from_secs(15),
        );
        if matches!(&response, Ok(ServerMessage::Hello { protocol_version }) if *protocol_version == PROTOCOL_VERSION)
        {
            return Ok(client);
        }
        let mut kind = match &response {
            Ok(ServerMessage::Hello { .. })
            | Ok(ServerMessage::Error {
                code: ErrorCode::IncompatibleProtocol,
                ..
            }) => ConnectionFailureKind::Incompatible,
            Err(error)
                if error
                    .downcast_ref::<DaemonError>()
                    .is_some_and(|error| error.code == ErrorCode::IncompatibleProtocol) =>
            {
                ConnectionFailureKind::Incompatible
            }
            _ => ConnectionFailureKind::Transport,
        };
        let mut message = match response {
            Ok(message) => format!("daemon rejected hello: {message:?}"),
            Err(error) => error.to_string(),
        };
        let diagnostics = client.connection.diagnostics();
        if diagnostics.contains("Permission denied")
            || diagnostics.contains("Authentication failed")
        {
            kind = ConnectionFailureKind::Unauthorized;
        }
        if !diagnostics.is_empty() {
            message.push_str(": ");
            message.push_str(&diagnostics);
        }
        Err(Box::new(ConnectionFailure { kind, message }))
    }

    pub fn endpoint_available(instance: Option<&str>, timeout: Duration) -> Result<bool> {
        let names = identity::instance_names(instance)?;
        match pipe::connect(&names.pipe, timeout).map_err(ConnectionFailure::transport) {
            Ok(_) => Ok(true),
            Err(error)
                if ConnectionFailure::kind(error.as_ref()) == ConnectionFailureKind::Absent =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Inspect an existing authenticated endpoint without Hello or daemon startup.
    pub fn lifecycle_status(instance: Option<&str>) -> Result<crate::LifecycleStatus> {
        let names = identity::instance_names(instance)?;
        let connection = pipe::connect(&names.pipe, Duration::from_secs(2))
            .map_err(ConnectionFailure::transport)?;
        Self::from_parts(connection, 1).lifecycle()
    }

    #[cfg(windows)]
    fn reject_supervisor_backoff() -> Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
            System::Threading::WaitForSingleObject,
        };
        let marker = crate::paths::data_dir()?.join("supervisor.pid");
        let pid = match std::fs::read_to_string(marker) {
            Ok(marker) => {
                let marker: serde_json::Value = serde_json::from_str(&marker)?;
                u32::try_from(
                    marker
                        .get("pid")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or("invalid supervisor presence record; inspect or repair Compi")?,
                )?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let process = match ProcessExit::open(pid) {
            Ok(process) => process,
            Err(error)
                if error
                    .downcast_ref::<windows::core::Error>()
                    .is_some_and(|error| error.code().0 as u32 & 0xffff == 87) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        match unsafe { WaitForSingleObject(HANDLE(process.0.as_raw_handle()), 0) } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => Err("Compi supervisor is starting or in recovery backoff; wait for its daemon to become inspectable before updating".into()),
            _ => Err(std::io::Error::last_os_error().into()),
        }
    }

    pub fn local_lifecycle_statuses() -> Result<Vec<crate::LifecycleStatus>> {
        let mut instances = std::collections::BTreeSet::new();
        instances.insert(None);
        let directory = crate::paths::data_dir()?;
        match std::fs::read_dir(directory) {
            Ok(entries) => {
                for entry in entries {
                    let name = entry?.file_name().to_string_lossy().into_owned();
                    if let Some(instance) = name
                        .strip_prefix("workspace-")
                        .and_then(|name| name.strip_suffix("-v1.json"))
                    {
                        identity::instance_names(Some(instance))?;
                        instances.insert(Some(instance.to_owned()));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut statuses = Vec::new();
        for instance in instances {
            match Self::lifecycle_status(instance.as_deref()) {
                Ok(status) => {
                    if !statuses.iter().any(|existing: &crate::LifecycleStatus| {
                        existing.server_generation == status.server_generation
                    }) {
                        statuses.push(status);
                    }
                }
                Err(error)
                    if ConnectionFailure::kind(error.as_ref()) == ConnectionFailureKind::Absent => {
                }
                Err(error) => return Err(error),
            }
        }
        #[cfg(windows)]
        if !statuses.iter().any(|status| status.instance.is_none()) {
            Self::reject_supervisor_backoff()?;
        }
        Ok(statuses)
    }

    pub fn local_lifecycle_statuses_for_install(
        root: &std::path::Path,
    ) -> Result<Vec<crate::LifecycleStatus>> {
        crate::lifecycle_inventory::for_install(root)
    }

    pub fn lifecycle_command(command: &mut Command) -> Result<crate::LifecycleStatus> {
        let child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        Self::from_io(ClientIo::command(child)?, 1).lifecycle()
    }

    pub fn lifecycle(&mut self) -> Result<crate::LifecycleStatus> {
        match self.request_bounded(ClientMessage::GetLifecycleStatus { lifecycle_version: crate::LIFECYCLE_VERSION }, Duration::from_secs(5))? {
            ServerMessage::LifecycleStatus { status } if status.lifecycle_version == crate::LIFECYCLE_VERSION => Ok(status),
            message => Err(format!("daemon lifecycle inspection unavailable; deliberately stop the old application before migration: {message:?}").into()),
        }
    }

    /// Consent must be the snapshot presented to the user, not a new snapshot.
    pub fn conditional_stop(
        instance: Option<&str>,
        consent: &crate::LifecycleConsent,
        timeout: Duration,
    ) -> Result<()> {
        let names = identity::instance_names(instance)?;
        let connection = pipe::connect(&names.pipe, Duration::from_secs(2))
            .map_err(ConnectionFailure::transport)?;
        let mut client = Self::from_parts(connection, 1);
        let status = client.lifecycle()?;
        if status.server_id != consent.server_id
            || status.server_generation != consent.server_generation
        {
            return Err("daemon generation changed; review shutdown consent again".into());
        }
        let daemon_exit = ProcessExit::open(status.daemon_pid)?;
        let supervisor_exit = status.supervisor_pid.map(ProcessExit::open).transpose()?;
        match client.request_bounded(
            ClientMessage::ConditionalStop {
                lifecycle_version: crate::LIFECYCLE_VERSION,
                consent: consent.clone(),
            },
            timeout,
        )? {
            ServerMessage::DaemonStopping => {}
            message => return Err(unexpected_response(message)),
        }
        let exit_deadline = Instant::now() + timeout;
        daemon_exit.wait(exit_deadline)?;
        if let Some(supervisor_exit) = supervisor_exit {
            supervisor_exit.wait(exit_deadline)?;
        }
        drop(client);
        let deadline = Instant::now() + timeout;
        loop {
            match pipe::connect(&names.pipe, Duration::from_millis(25)) {
                Ok(connection) => drop(connection),
                Err(error) => {
                    let error = ConnectionFailure::transport(error);
                    if ConnectionFailure::kind(error.as_ref()) == ConnectionFailureKind::Absent {
                        return Ok(());
                    }
                    return Err(error);
                }
            }
            if Instant::now() >= deadline {
                return Err("daemon acknowledged stop but its endpoint did not disappear".into());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn workspace(&mut self) -> Result<WorkspaceSnapshot> {
        match self.request(ClientMessage::GetWorkspace)? {
            ServerMessage::Workspace { workspace } => Ok(workspace),
            message => Err(unexpected_response(message)),
        }
    }
    pub fn runtime_metrics(&mut self) -> Result<RuntimeMetrics> {
        match self.request(ClientMessage::GetRuntimeMetrics)? {
            ServerMessage::RuntimeMetrics { metrics } => Ok(metrics),
            message => Err(unexpected_response(message)),
        }
    }

    /// List immediate children in the filesystem namespace of a workspace surface.
    /// `path` must be an absolute Unix/WSL path, even when the client runs on Windows.
    pub fn list_directory(
        &mut self,
        surface_id: SurfaceId,
        path: String,
    ) -> Result<Vec<DirectoryEntry>> {
        match self.request(ClientMessage::ListDirectory { surface_id, path })? {
            ServerMessage::DirectoryListed { entries } => Ok(entries),
            message => Err(unexpected_response(message)),
        }
    }

    /// Search descendant names beneath an absolute Unix/WSL root for one workspace surface.
    pub fn search_directory(
        &mut self,
        surface_id: SurfaceId,
        root: String,
        query: String,
    ) -> Result<Vec<SearchEntry>> {
        match self.request(ClientMessage::SearchDirectory {
            surface_id,
            root,
            query,
        })? {
            ServerMessage::DirectorySearched { entries } => Ok(entries),
            message => Err(unexpected_response(message)),
        }
    }

    pub fn upload_image(&mut self, name: &str, bytes: &[u8]) -> Result<String> {
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_UPLOAD_BYTES {
            return Err(format!(
                "image upload must contain 1 through {MAX_IMAGE_UPLOAD_BYTES} bytes"
            )
            .into());
        }
        let byte_len = u64::try_from(bytes.len())?;
        let sha256: [u8; 32] = Sha256::digest(bytes).into();
        let upload_id = match self.request(ClientMessage::BeginImageUpload {
            name: name.to_owned(),
            byte_len,
            sha256,
        })? {
            ServerMessage::ImageUploadStarted { upload_id } => upload_id,
            message => return Err(unexpected_response(message)),
        };
        let mut next_offset = 0_u64;
        for chunk in bytes.chunks(IMAGE_UPLOAD_CHUNK_BYTES) {
            let expected = next_offset + u64::try_from(chunk.len())?;
            match self.request(ClientMessage::UploadImageChunk {
                upload_id: upload_id.clone(),
                offset: next_offset,
                data: chunk.to_vec(),
            })? {
                ServerMessage::ImageUploadProgress {
                    upload_id: acknowledged,
                    next_offset: actual,
                } if acknowledged == upload_id && actual == expected => {
                    next_offset = actual;
                }
                message => return Err(unexpected_response(message)),
            }
        }
        match self.request(ClientMessage::FinishImageUpload { upload_id })? {
            ServerMessage::ImageUploaded { path } => Ok(path),
            message => Err(unexpected_response(message)),
        }
    }

    pub fn list_surfaces(&mut self) -> Result<Vec<SurfaceInfo>> {
        Ok(self.workspace()?.surfaces)
    }

    pub fn submit_mutation(&mut self, mutation: MutationRequest) -> Result<MutationReceipt> {
        match self.request(ClientMessage::Mutate { mutation })? {
            ServerMessage::MutationCommitted { receipt } => {
                self.workspace = None;
                Ok(receipt)
            }
            message => Err(unexpected_response(message)),
        }
    }

    pub fn mutate(&mut self, operation: WorkspaceMutation) -> Result<MutationReceipt> {
        for _ in 0..8 {
            let workspace = self.workspace()?;
            let request_id = self.send(ClientMessage::Mutate {
                mutation: MutationRequest {
                    server_id: workspace.server_id,
                    expected_generation: workspace.server_generation,
                    mutation_id: MutationId::new(next_mutation_id()),
                    expected_revision: workspace.revision,
                    operation: operation.clone(),
                    launch: None,
                },
            })?;
            loop {
                match self.read_event()? {
                    Some(ServerEvent::Control {
                        request_id: Some(response_id),
                        message,
                    }) if response_id == request_id => match message {
                        ServerMessage::MutationCommitted { receipt } => {
                            self.workspace = None;
                            return Ok(receipt);
                        }
                        ServerMessage::Error {
                            code: ErrorCode::RevisionConflict,
                            ..
                        } => {
                            self.workspace = None;
                            break;
                        }
                        ServerMessage::Error { code, message, .. } => {
                            return Err(format!("daemon error ({code:?}): {message}").into());
                        }
                        message => return Err(unexpected_response(message)),
                    },
                    Some(ServerEvent::Control { .. }) => {}
                    Some(ServerEvent::Screen(message)) => self.queue_screen(message),
                    None => return Err("daemon disconnected before responding".into()),
                }
            }
        }
        Err("workspace kept changing while applying the mutation".into())
    }

    pub fn mutation_outcome(&mut self, mutation_id: MutationId) -> Result<MutationReceipt> {
        match self.request(ClientMessage::MutationOutcome { mutation_id })? {
            ServerMessage::MutationOutcome {
                receipt: Some(receipt),
            } => Ok(receipt),
            ServerMessage::MutationOutcome { receipt: None } => {
                Err("mutation outcome is unknown".into())
            }
            message => Err(unexpected_response(message)),
        }
    }

    pub fn create_surface(
        &mut self,
        cols: i16,
        rows: i16,
        working_directory: Option<String>,
    ) -> Result<SurfaceInfo> {
        let workspace = self.workspace()?;
        let receipt = if !workspace.initialized {
            self.mutate(WorkspaceMutation::Initialize {
                cols,
                rows,
                working_directory,
            })?
        } else {
            let session_id = if let Some(session) = workspace.sessions.first() {
                session.id.clone()
            } else {
                let created = self.mutate(WorkspaceMutation::CreateSession {
                    label: "Default".into(),
                })?;
                let refreshed = self.workspace()?;
                refreshed
                    .sessions
                    .iter()
                    .find(|session| created.affected_sessions.contains(&session.id))
                    .ok_or("created session missing from workspace")?
                    .id
                    .clone()
            };
            self.mutate(WorkspaceMutation::CreateTab {
                session_id,
                label: "Shell".into(),
                cols,
                rows,
                working_directory,
            })?
        };
        let surface_id = receipt
            .affected_surfaces
            .first()
            .cloned()
            .ok_or("workspace mutation did not create a surface")?;
        self.wait_for_surface(&surface_id, Duration::from_secs(30))
    }

    pub fn end_surface(&mut self, surface: &SurfaceInfo) -> Result<MutationReceipt> {
        self.mutate(WorkspaceMutation::EndSurface {
            surface_id: surface.id.clone(),
            expected_lifetime: surface.process_lifetime_id.clone(),
        })
    }

    pub fn wait_for_surface(
        &mut self,
        surface_id: &SurfaceId,
        timeout: Duration,
    ) -> Result<SurfaceInfo> {
        let deadline = Instant::now() + timeout;
        loop {
            let workspace = self.workspace()?;
            let surface = workspace
                .surface(surface_id)
                .cloned()
                .ok_or("surface disappeared from workspace")?;
            match surface.status {
                SurfaceStatus::Running
                | SurfaceStatus::Exited
                | SurfaceStatus::Failed
                | SurfaceStatus::Lost => return Ok(surface),
                SurfaceStatus::Starting | SurfaceStatus::Ending => {}
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out after {timeout:?} waiting for surface {surface_id}; last state: {surface:?}"
                ).into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn attach_surface(&mut self, surface: &SurfaceInfo, cols: i16, rows: i16) -> Result<()> {
        match self.request(ClientMessage::Attach {
            surface_id: surface.id.clone(),
            expected_lifetime: surface.process_lifetime_id.clone(),
            cols,
            rows,
        })? {
            ServerMessage::Attached { .. } => Ok(()),
            message => Err(unexpected_response(message)),
        }
    }

    /// Explicit destructive CLI intent only. Installers and GUI consent flows
    /// must use conditional_stop instead.
    pub fn shutdown_daemon(&mut self) -> Result<()> {
        match self.request(ClientMessage::ShutdownDaemon)? {
            ServerMessage::DaemonStopping => Ok(()),
            message => Err(unexpected_response(message)),
        }
    }

    pub fn request_snapshot(&mut self) -> Result<u64> {
        match self.request(ClientMessage::RequestSnapshot)? {
            ServerMessage::SnapshotReady { sequence } => Ok(sequence),
            message => Err(unexpected_response(message)),
        }
    }

    pub fn send(&mut self, message: ClientMessage) -> Result<u64> {
        let request_id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or("protocol request ID overflow")?;
        let target = terminal_operation(&message)
            .then(|| self.target.clone().ok_or("terminal is not attached"))
            .transpose()?;
        let payload = encode_client(&ClientControl {
            request_id,
            target,
            message,
        })?;
        frame::write(&mut self.connection.writer(), CONTROL_FRAME, &payload)?;
        Ok(request_id)
    }

    fn request_bounded(
        &mut self,
        message: ClientMessage,
        timeout: Duration,
    ) -> Result<ServerMessage> {
        let request_id = self.send(message)?;
        let deadline = Instant::now() + timeout;
        loop {
            match self.poll_event()? {
                Some(ServerEvent::Control {
                    request_id: Some(response_id),
                    message,
                }) if response_id == request_id => {
                    if let ServerMessage::Error {
                        code,
                        message,
                        current_revision,
                    } = message
                    {
                        return Err(Box::new(DaemonError {
                            code,
                            message,
                            current_revision,
                        }));
                    }
                    return Ok(message);
                }
                Some(ServerEvent::Screen(message)) => self.queue_screen(message),
                Some(_) => {}
                None => {
                    if Instant::now() >= deadline {
                        return Err(Box::new(ConnectionFailure {
                            kind: ConnectionFailureKind::Transport,
                            message: "daemon control response timed out".into(),
                        }));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    pub fn request(&mut self, message: ClientMessage) -> Result<ServerMessage> {
        let request_id = self.send(message)?;
        loop {
            match self.read_event()? {
                Some(ServerEvent::Control {
                    request_id: Some(response_id),
                    message,
                }) if response_id == request_id => {
                    if let ServerMessage::Error {
                        code,
                        message,
                        current_revision,
                    } = &message
                    {
                        return Err(Box::new(DaemonError {
                            code: *code,
                            message: message.clone(),
                            current_revision: *current_revision,
                        }));
                    }
                    return Ok(message);
                }
                Some(ServerEvent::Control { .. }) => {}
                Some(ServerEvent::Screen(message)) => self.queue_screen(message),
                None => return Err("daemon disconnected before responding".into()),
            }
        }
    }

    pub fn read_event(&mut self) -> Result<Option<ServerEvent>> {
        loop {
            let Some(frame) = self.poll_reader.read(self.connection.reader())? else {
                return Ok(None);
            };
            if let Some(event) = self.decode_event(frame)? {
                if let ServerEvent::Control { message, .. } = &event {
                    self.observe_control(message)?;
                }
                return Ok(Some(event));
            }
        }
    }

    pub fn poll_event(&mut self) -> Result<Option<ServerEvent>> {
        loop {
            let Some(frame) = self.poll_reader.poll(self.connection.reader())? else {
                return Ok(None);
            };
            if let Some(event) = self.decode_event(frame)? {
                if let ServerEvent::Control { message, .. } = &event {
                    self.observe_control(message)?;
                }
                return Ok(Some(event));
            }
        }
    }

    pub fn take_pending_screen(&mut self) -> Option<crate::ScreenMessage> {
        if let Some(bytes) = self.pending_screen_sizes.pop_front() {
            self.pending_screen_bytes -= bytes;
        }
        self.pending_screen.pop_front()
    }

    fn queue_screen(&mut self, message: crate::ScreenMessage) {
        if self.pending_screen.len() >= 32
            || self
                .pending_screen_bytes
                .saturating_add(self.last_screen_bytes)
                > frame::MAX_SCREEN_PAYLOAD
        {
            // Keep the newest event. A discarded baseline/delta is observable as
            // a sequence gap, recovered through the existing snapshot protocol.
            self.pending_screen.clear();
            self.pending_screen_sizes.clear();
            self.pending_screen_bytes = 0;
        }
        self.pending_screen.push_back(message);
        self.pending_screen_sizes.push_back(self.last_screen_bytes);
        self.pending_screen_bytes += self.last_screen_bytes;
    }

    pub fn into_parts(
        self,
    ) -> (
        ClientIo,
        u64,
        VecDeque<crate::ScreenMessage>,
        Option<TerminalTarget>,
        Option<WorkspaceSnapshot>,
    ) {
        (
            self.connection,
            self.next_request_id,
            self.pending_screen,
            self.target,
            self.workspace,
        )
    }

    pub fn from_parts(connection: File, next_request_id: u64) -> Self {
        Self::from_io(ClientIo::local(connection), next_request_id)
    }

    fn from_io(connection: ClientIo, next_request_id: u64) -> Self {
        Self {
            connection,
            next_request_id,
            pending_screen: VecDeque::new(),
            pending_screen_sizes: VecDeque::new(),
            pending_screen_bytes: 0,
            last_screen_bytes: 0,
            poll_reader: pipe::PipeReader::default(),
            target: None,
            workspace: None,
        }
    }

    pub fn from_attached_parts(
        connection: ClientIo,
        next_request_id: u64,
        target: TerminalTarget,
        workspace: Option<WorkspaceSnapshot>,
    ) -> Self {
        Self {
            connection,
            next_request_id,
            pending_screen: VecDeque::new(),
            pending_screen_sizes: VecDeque::new(),
            pending_screen_bytes: 0,
            last_screen_bytes: 0,
            poll_reader: pipe::PipeReader::default(),
            target: Some(target),
            workspace,
        }
    }

    fn decode_event(&mut self, message: frame::Frame) -> Result<Option<ServerEvent>> {
        match message.kind {
            CONTROL_FRAME => {
                let control = decode_server(&message.payload)?;
                Ok(Some(ServerEvent::Control {
                    request_id: control.request_id,
                    message: control.message,
                }))
            }
            SCREEN_FRAME => {
                self.last_screen_bytes = message.payload.len();
                let terminal = decode_terminal_frame(&message.payload)?;
                if self
                    .target
                    .as_ref()
                    .is_some_and(|target| target.identity != terminal.identity)
                {
                    return Ok(None);
                }
                Ok(Some(ServerEvent::Screen(terminal.message)))
            }
            kind => Err(format!("unknown server frame type {kind}").into()),
        }
    }

    fn observe_control(&mut self, message: &ServerMessage) -> Result<()> {
        match message {
            ServerMessage::Workspace { workspace } => self.workspace = Some(workspace.clone()),
            ServerMessage::WorkspaceChanged { revision } => {
                if self
                    .workspace
                    .as_ref()
                    .is_some_and(|workspace| workspace.revision != *revision)
                {
                    self.workspace = None;
                }
            }
            ServerMessage::Attached {
                identity,
                surface,
                attachment_id,
                ..
            } => {
                if identity.surface_id != surface.id
                    || identity.process_lifetime_id != surface.process_lifetime_id
                {
                    return Err("daemon returned inconsistent terminal identity".into());
                }
                self.target = Some(TerminalTarget {
                    attachment_id: attachment_id.clone(),
                    identity: identity.clone(),
                });
            }
            ServerMessage::Detached { .. } => self.target = None,
            ServerMessage::SurfaceExited { identity, .. }
                if self
                    .target
                    .as_ref()
                    .is_some_and(|target| &target.identity == identity) =>
            {
                self.target = None;
            }
            _ => {}
        }
        Ok(())
    }
}

fn terminal_operation(message: &ClientMessage) -> bool {
    matches!(
        message,
        ClientMessage::Detach
            | ClientMessage::Input { .. }
            | ClientMessage::Resize { .. }
            | ClientMessage::RequestSnapshot
            | ClientMessage::ClearScrollback
    )
}

fn next_mutation_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let ordinal = NEXT_MUTATION.fetch_add(1, Ordering::Relaxed);
    format!("mutation-{}-{now:x}-{ordinal:x}", std::process::id())
}

fn unexpected_response(message: ServerMessage) -> crate::Error {
    match message {
        ServerMessage::Error {
            code,
            message,
            current_revision,
        } => Box::new(DaemonError {
            code,
            message,
            current_revision,
        }),
        message => format!("unexpected daemon response: {message:?}").into(),
    }
}

#[cfg(test)]
mod connection_classification_tests {
    use super::*;

    #[test]
    fn only_endpoint_absence_permits_startup() {
        for (kind, expected) in [
            (std::io::ErrorKind::NotFound, ConnectionFailureKind::Absent),
            (
                std::io::ErrorKind::ConnectionRefused,
                ConnectionFailureKind::Absent,
            ),
            (
                std::io::ErrorKind::PermissionDenied,
                ConnectionFailureKind::Unauthorized,
            ),
            (
                std::io::ErrorKind::TimedOut,
                ConnectionFailureKind::Transport,
            ),
            (
                std::io::ErrorKind::WouldBlock,
                ConnectionFailureKind::Transport,
            ),
            (
                std::io::ErrorKind::BrokenPipe,
                ConnectionFailureKind::Transport,
            ),
        ] {
            let error = ConnectionFailure::transport(std::io::Error::from(kind).into());
            assert_eq!(ConnectionFailure::kind(error.as_ref()), expected);
        }
    }

    #[cfg(windows)]
    #[test]
    fn busy_named_pipe_and_access_denied_never_mean_absent() {
        for (code, expected) in [
            (231, ConnectionFailureKind::Transport),
            (5, ConnectionFailureKind::Unauthorized),
            (2, ConnectionFailureKind::Absent),
        ] {
            let error =
                windows::core::Error::from_hresult(windows::core::HRESULT::from_win32(code));
            assert_eq!(
                ConnectionFailure::kind(ConnectionFailure::transport(error.into()).as_ref()),
                expected
            );
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn incompatible_hello_is_not_connection_absence() {
        let (receiver, mut sender) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            frame::read(&mut sender).unwrap().unwrap();
            let payload = crate::encode_server(&crate::ServerControl {
                request_id: Some(1),
                message: ServerMessage::Error {
                    code: ErrorCode::IncompatibleProtocol,
                    message: "different full-workspace protocol".into(),
                    current_revision: None,
                },
            })
            .unwrap();
            frame::write(&mut sender, CONTROL_FRAME, &payload).unwrap();
        });
        let client = DaemonClient::from_parts(File::from(OwnedFd::from(receiver)), 1);
        let error = DaemonClient::handshake(client).err().unwrap();
        assert_eq!(
            ConnectionFailure::kind(error.as_ref()),
            ConnectionFailureKind::Incompatible
        );
        server.join().unwrap();
    }

    #[test]
    fn broken_hello_connection_is_transport_failure_not_absence() {
        let (receiver, mut sender) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            frame::read(&mut sender).unwrap().unwrap();
        });
        let client = DaemonClient::from_parts(File::from(OwnedFd::from(receiver)), 1);
        let error = DaemonClient::handshake(client).err().unwrap();
        assert_eq!(
            ConnectionFailure::kind(error.as_ref()),
            ConnectionFailureKind::Transport
        );
        server.join().unwrap();
    }

    #[test]
    fn blocking_read_preserves_frames_buffered_by_polling() {
        let (receiver, mut sender) = UnixStream::pair().unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut bytes = Vec::new();
        for version in [7, 9] {
            let payload = crate::encode_server(&crate::ServerControl {
                request_id: None,
                message: ServerMessage::Hello {
                    protocol_version: version,
                },
            })
            .unwrap();
            frame::write(&mut bytes, CONTROL_FRAME, &payload).unwrap();
        }
        sender.write_all(&bytes).unwrap();
        drop(sender);
        let mut client = DaemonClient::from_parts(File::from(OwnedFd::from(receiver)), 1);
        assert!(matches!(
            client.poll_event().unwrap(),
            Some(ServerEvent::Control {
                message: ServerMessage::Hello {
                    protocol_version: 7
                },
                ..
            })
        ));
        assert!(matches!(
            client.read_event().unwrap(),
            Some(ServerEvent::Control {
                message: ServerMessage::Hello {
                    protocol_version: 9
                },
                ..
            })
        ));
        assert!(client.read_event().unwrap().is_none());
    }
}
