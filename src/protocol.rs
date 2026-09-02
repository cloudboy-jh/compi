use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 3;
pub const CONTROL_FRAME: u8 = 1;
pub const SCREEN_FRAME: u8 = 2;
pub const MAX_CONTROL_PAYLOAD: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientControl {
    pub request_id: u64,
    #[serde(flatten)]
    pub message: ClientMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello {
        protocol_version: u32,
    },
    ListSessions,
    CreateSession {
        cols: i16,
        rows: i16,
    },
    Attach {
        session_id: String,
        cols: i16,
        rows: i16,
    },
    Detach,
    Input {
        data: Vec<u8>,
    },
    Resize {
        cols: i16,
        rows: i16,
    },
    RequestSnapshot,
    Kill {
        session_id: String,
    },
    ShutdownDaemon,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerControl {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<u64>,
    #[serde(flatten)]
    pub message: ServerMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Hello { protocol_version: u32 },
    Sessions { sessions: Vec<SessionInfo> },
    SessionCreated { session: SessionInfo },
    Attached { session: SessionInfo, sequence: u64 },
    Detached { session_id: String },
    InputAccepted,
    Resized { cols: i16, rows: i16 },
    SnapshotReady { sequence: u64 },
    KillRequested { session_id: String },
    DaemonStopping,
    SessionExited { session_id: String, exit_code: u32 },
    Error { code: ErrorCode, message: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    IncompatibleProtocol,
    InvalidRequest,
    SessionNotFound,
    AlreadyAttached,
    NotAttached,
    SessionExited,
    Internal,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Starting,
    Running,
    Exited,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub status: SessionStatus,
    pub attached: bool,
    pub cols: i16,
    pub rows: i16,
    pub created_at_ms: u64,
    pub exit_code: Option<u32>,
    pub error: Option<String>,
}

pub fn encode_client(message: &ClientControl) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec(message)
}

pub fn decode_client(payload: &[u8]) -> serde_json::Result<ClientControl> {
    serde_json::from_slice(payload)
}

pub fn encode_server(message: &ServerControl) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec(message)
}

pub fn decode_server(payload: &[u8]) -> serde_json::Result<ServerControl> {
    serde_json::from_slice(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_messages_round_trip() {
        let message = ClientControl {
            request_id: 42,
            message: ClientMessage::Attach {
                session_id: "session-1".into(),
                cols: 120,
                rows: 40,
            },
        };

        assert_eq!(
            decode_client(&encode_client(&message).unwrap()).unwrap(),
            message
        );
    }

    #[test]
    fn protocol_version_is_explicit() {
        let message = ServerControl {
            request_id: Some(1),
            message: ServerMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        };
        let json = String::from_utf8(encode_server(&message).unwrap()).unwrap();

        assert!(json.contains("\"protocol_version\":3"));
        assert_eq!(decode_server(json.as_bytes()).unwrap(), message);
    }
}
