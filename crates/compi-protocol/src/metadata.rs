//! Observational metadata collected in the terminal's daemon-side environment.
use crate::{ProcessLifetimeId, SurfaceId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataState {
    Available,
    Stale,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataField<T> {
    pub state: MetadataState,
    pub value: Option<T>,
    pub reason: Option<String>,
}

impl<T> MetadataField<T> {
    pub fn available(value: T) -> Self {
        Self {
            state: MetadataState::Available,
            value: Some(value),
            reason: None,
        }
    }
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            state: MetadataState::Unavailable,
            value: None,
            reason: Some(reason.into()),
        }
    }
    pub fn mark_stale(&mut self, reason: &str) {
        if self.state != MetadataState::Unavailable {
            self.state = MetadataState::Stale;
        }
        self.reason = Some(reason.to_owned());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentKind {
    Unix,
    Windows,
    Wsl,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentIdentity {
    pub kind: EnvironmentKind,
    /// The host reported by the environment doing the collection, not the GUI host.
    pub hostname: MetadataField<String>,
    pub distribution: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveProcess {
    pub pid: u32,
    pub process_group: u32,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitMetadata {
    /// None represents detached HEAD, not a failed query.
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneDimensions {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneMetadata {
    pub surface_id: SurfaceId,
    pub process_lifetime_id: ProcessLifetimeId,
    /// Overall observation freshness; individual fields may still be unavailable.
    pub state: MetadataState,
    pub environment: EnvironmentIdentity,
    pub collected_at_ms: Option<u64>,
    pub title: String,
    /// Resolved launch shell, distinct from the current foreground program.
    pub shell_executable: Option<String>,
    pub directory: MetadataField<String>,
    pub process: MetadataField<ActiveProcess>,
    /// Available with no value means this directory is not a Git worktree.
    pub git: MetadataField<GitMetadata>,
    pub dimensions: PaneDimensions,
}

impl PaneMetadata {
    pub fn mark_stale(&mut self, reason: &str) {
        self.state = if self.collected_at_ms.is_some() {
            MetadataState::Stale
        } else {
            MetadataState::Unavailable
        };
        self.directory.mark_stale(reason);
        self.process.mark_stale(reason);
        self.git.mark_stale(reason);
        self.environment.hostname.mark_stale(reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_worktree_remains_available_after_transport_and_can_become_stale() {
        let field: MetadataField<GitMetadata> = MetadataField {
            state: MetadataState::Available,
            value: None,
            reason: None,
        };
        let mut observed: MetadataField<GitMetadata> =
            serde_json::from_slice(&serde_json::to_vec(&field).unwrap()).unwrap();
        assert_eq!(observed.state, MetadataState::Available);
        assert!(observed.value.is_none());
        observed.mark_stale("terminal disconnected");
        assert_eq!(observed.state, MetadataState::Stale);
        assert!(observed.value.is_none());

        let mut unavailable: MetadataField<GitMetadata> = MetadataField::unavailable("git missing");
        unavailable.mark_stale("terminal disconnected");
        assert_eq!(unavailable.state, MetadataState::Unavailable);
    }
}
