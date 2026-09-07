use crate::Result;
use crate::identity;
use crate::pipe;
use compi_protocol::frame;
use compi_protocol::{
    CONTROL_FRAME, ClientControl, ClientMessage, ErrorCode, MutationId, MutationReceipt,
    MutationRequest, PROTOCOL_VERSION, SCREEN_FRAME, ServerMessage, SurfaceId, SurfaceInfo,
    SurfaceStatus, TerminalTarget, WorkspaceMutation, WorkspaceSnapshot, decode_server,
    decode_terminal_frame, encode_client,
};
use std::collections::VecDeque;
use std::fs::File;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static NEXT_MUTATION: AtomicU64 = AtomicU64::new(1);

pub enum ServerEvent {
    Control {
        request_id: Option<u64>,
        message: ServerMessage,
    },
    Screen(compi_protocol::ScreenMessage),
}

pub struct DaemonClient {
    connection: File,
    next_request_id: u64,
    pending_screen: VecDeque<compi_protocol::ScreenMessage>,
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
        let connection = pipe::connect(pipe_name, timeout)?;
        let mut client = Self::from_parts(connection, 1);
        match client.request(ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        })? {
            ServerMessage::Hello { protocol_version } if protocol_version == PROTOCOL_VERSION => {
                Ok(client)
            }
            ServerMessage::Error { code, message, .. } => {
                Err(format!("daemon rejected protocol ({code:?}): {message}").into())
            }
            message => Err(format!("unexpected daemon hello response: {message:?}").into()),
        }
    }

    pub fn workspace(&mut self) -> Result<WorkspaceSnapshot> {
        match self.request(ClientMessage::GetWorkspace)? {
            ServerMessage::Workspace { workspace } => Ok(workspace),
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
                    Some(ServerEvent::Screen(message)) => self.pending_screen.push_back(message),
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
        self.wait_for_surface(&surface_id, Duration::from_secs(5))
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
                return Err(format!("timed out waiting for surface {surface_id}").into());
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
        frame::write(&mut self.connection, CONTROL_FRAME, &payload)?;
        Ok(request_id)
    }

    pub fn request(&mut self, message: ClientMessage) -> Result<ServerMessage> {
        let request_id = self.send(message)?;
        loop {
            match self.read_event()? {
                Some(ServerEvent::Control {
                    request_id: Some(response_id),
                    message,
                }) if response_id == request_id => {
                    if let ServerMessage::Error { code, message, .. } = &message {
                        return Err(format!("daemon error ({code:?}): {message}").into());
                    }
                    return Ok(message);
                }
                Some(ServerEvent::Control { .. }) => {}
                Some(ServerEvent::Screen(message)) => self.pending_screen.push_back(message),
                None => return Err("daemon disconnected before responding".into()),
            }
        }
    }

    pub fn read_event(&mut self) -> Result<Option<ServerEvent>> {
        loop {
            let Some(frame) = frame::read(&mut self.connection)? else {
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
            let Some(frame) = self.poll_reader.poll(&self.connection)? else {
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

    pub fn take_pending_screen(&mut self) -> Option<compi_protocol::ScreenMessage> {
        self.pending_screen.pop_front()
    }

    pub fn into_parts(
        self,
    ) -> (
        File,
        u64,
        VecDeque<compi_protocol::ScreenMessage>,
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
        Self {
            connection,
            next_request_id,
            pending_screen: VecDeque::new(),
            poll_reader: pipe::PipeReader::default(),
            target: None,
            workspace: None,
        }
    }

    pub fn from_attached_parts(
        connection: File,
        next_request_id: u64,
        target: TerminalTarget,
        workspace: Option<WorkspaceSnapshot>,
    ) -> Self {
        Self {
            connection,
            next_request_id,
            pending_screen: VecDeque::new(),
            poll_reader: pipe::PipeReader::default(),
            target: Some(target),
            workspace,
        }
    }

    fn decode_event(&self, message: frame::Frame) -> Result<Option<ServerEvent>> {
        match message.kind {
            CONTROL_FRAME => {
                let control = decode_server(&message.payload)?;
                Ok(Some(ServerEvent::Control {
                    request_id: control.request_id,
                    message: control.message,
                }))
            }
            SCREEN_FRAME => {
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
        ServerMessage::Error { code, message, .. } => {
            format!("daemon error ({code:?}): {message}").into()
        }
        message => format!("unexpected daemon response: {message:?}").into(),
    }
}
