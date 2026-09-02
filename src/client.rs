use crate::Result;
use crate::frame;
use crate::identity;
use crate::pipe;
use crate::protocol::{
    CONTROL_FRAME, ClientControl, ClientMessage, PROTOCOL_VERSION, SCREEN_FRAME, ServerMessage,
    SessionInfo, decode_server, encode_client,
};
use crate::terminal::{ScreenMessage, decode_screen};
use std::collections::VecDeque;
use std::fs::File;
use std::time::Duration;

pub enum ServerEvent {
    Control {
        request_id: Option<u64>,
        message: ServerMessage,
    },
    Screen(ScreenMessage),
}

pub struct DaemonClient {
    connection: File,
    next_request_id: u64,
    pending_screen: VecDeque<ScreenMessage>,
    poll_reader: pipe::PipeReader,
}

impl DaemonClient {
    pub fn connect(instance: Option<&str>, timeout: Duration) -> Result<Self> {
        let names = identity::instance_names(instance)?;
        Self::connect_to(&names.pipe, timeout)
    }

    pub fn connect_to(pipe_name: &str, timeout: Duration) -> Result<Self> {
        let connection = pipe::connect(pipe_name, timeout)?;
        let mut client = Self {
            connection,
            next_request_id: 1,
            pending_screen: VecDeque::new(),
            poll_reader: pipe::PipeReader::default(),
        };
        match client.request(ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        })? {
            ServerMessage::Hello { protocol_version } if protocol_version == PROTOCOL_VERSION => {
                Ok(client)
            }
            ServerMessage::Error { code, message } => {
                Err(format!("daemon rejected protocol ({code:?}): {message}").into())
            }
            message => Err(format!("unexpected daemon hello response: {message:?}").into()),
        }
    }

    pub fn list_sessions(&mut self) -> Result<Vec<SessionInfo>> {
        match self.request(ClientMessage::ListSessions)? {
            ServerMessage::Sessions { sessions } => Ok(sessions),
            message => Err(unexpected_response(message)),
        }
    }

    pub fn create_session(&mut self, cols: i16, rows: i16) -> Result<SessionInfo> {
        match self.request(ClientMessage::CreateSession { cols, rows })? {
            ServerMessage::SessionCreated { session } => Ok(session),
            message => Err(unexpected_response(message)),
        }
    }

    pub fn kill_session(&mut self, session_id: String) -> Result<()> {
        match self.request(ClientMessage::Kill { session_id })? {
            ServerMessage::KillRequested { .. } => Ok(()),
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
        let payload = encode_client(&ClientControl {
            request_id,
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
                    if let ServerMessage::Error { code, message } = &message {
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
        frame::read(&mut self.connection)?
            .map(decode_event)
            .transpose()
    }

    pub fn poll_event(&mut self) -> Result<Option<ServerEvent>> {
        self.poll_reader
            .poll(&self.connection)?
            .map(decode_event)
            .transpose()
    }

    pub fn take_pending_screen(&mut self) -> Option<ScreenMessage> {
        self.pending_screen.pop_front()
    }

    pub fn into_parts(self) -> (File, u64, VecDeque<ScreenMessage>) {
        (self.connection, self.next_request_id, self.pending_screen)
    }

    pub fn from_parts(connection: File, next_request_id: u64) -> Self {
        Self {
            connection,
            next_request_id,
            pending_screen: VecDeque::new(),
            poll_reader: pipe::PipeReader::default(),
        }
    }
}

fn decode_event(message: frame::Frame) -> Result<ServerEvent> {
    match message.kind {
        CONTROL_FRAME => {
            let control = decode_server(&message.payload)?;
            Ok(ServerEvent::Control {
                request_id: control.request_id,
                message: control.message,
            })
        }
        SCREEN_FRAME => Ok(ServerEvent::Screen(decode_screen(&message.payload)?)),
        kind => Err(format!("unknown server frame type {kind}").into()),
    }
}

fn unexpected_response(message: ServerMessage) -> crate::Error {
    match message {
        ServerMessage::Error { code, message } => {
            format!("daemon error ({code:?}): {message}").into()
        }
        message => format!("unexpected daemon response: {message:?}").into(),
    }
}
