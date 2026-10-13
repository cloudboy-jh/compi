//! Program status records reported by programs through OSC 7501.
//!
//! Every text field is untrusted program output: the daemon has checked it against the
//! protocol's limits and refused control characters, but nothing else.

use crate::{ProcessLifetimeId, SurfaceId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProgramState {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
}

/// What a `blocked` record waits for.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum BlockedKind {
    Permission,
    Question,
    Auth,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProgramStatusRecord {
    /// Slash-separated record path; `None` is the root record.
    pub id: Option<String>,
    pub state: ProgramState,
    /// Only on `blocked` records.
    pub kind: Option<BlockedKind>,
    /// 0 to 100, only on `working` and `blocked` records; `None` is indeterminate.
    pub progress: Option<u8>,
    /// The record's own `app`; display code inherits it from the nearest ancestor.
    pub app: Option<String>,
    pub title: Option<String>,
    pub message: Option<String>,
}

/// Every record one terminal holds; no records means the terminal reports nothing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SurfaceProgramStatus {
    pub surface_id: SurfaceId,
    pub process_lifetime_id: ProcessLifetimeId,
    pub records: Vec<ProgramStatusRecord>,
}
