//! Compi's shared process contract, local transport, and runtime support.
mod client;
pub mod frame;
#[cfg_attr(unix, path = "identity_unix.rs")]
pub mod identity;
mod lifecycle_inventory;
pub mod metadata;
pub mod paths;
pub mod perf;
pub mod pipe;
pub mod prompt;
mod screen;
#[cfg(windows)]
pub mod wsl;

pub use screen::{
    Cell, Color, CursorShape, CursorState, KittyImage, KittyPlacement, MouseMode, Row, RowUpdate,
    ScreenDelta, ScreenMessage, ScreenSnapshot, ShellAction, TerminalFrame, TerminalModes,
    TextAttributes, decode_screen, decode_terminal_frame, encode_screen, encode_terminal_frame,
};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T, E = Error> = std::result::Result<T, E>;

pub use client::{
    ClientIo, ConnectionFailure, ConnectionFailureKind, DaemonClient, DaemonError, LocalDaemon,
    ServerEvent,
};
pub use lifecycle_inventory::{LocalDaemonProcess, local_daemon_processes};

use serde::{Deserialize, Serialize};
use std::fmt;

// Version 19 adds content panes: layout leaves without a terminal surface.
pub const PROTOCOL_VERSION: u32 = 19;
pub const CONTROL_FRAME: u8 = 1;
pub const SCREEN_FRAME: u8 = 2;
pub const MAX_CONTROL_PAYLOAD: usize = 1024 * 1024;
pub const MAX_MUTATION_RECEIPTS: usize = 1_024;
pub const MAX_TAB_PANES: usize = 256;

/// Authoritative graphics storage, measured as retained base64 bytes.
pub const DEFAULT_GRAPHICS_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_GRAPHICS_BYTES: usize = 64 * 1024 * 1024;
/// Maximum RGBA allocation for one decoded image.
pub const MAX_DECODED_IMAGE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_IMAGE_UPLOAD_BYTES: usize = 64 * 1024 * 1024;
pub const IMAGE_UPLOAD_CHUNK_BYTES: usize = 256 * 1024;
pub const MAX_DIRECTORY_PATH_BYTES: usize = 4096;
pub const MAX_DIRECTORY_QUERY_BYTES: usize = 256;
/// Open files one content pane may list.
pub const MAX_CONTENT_FILES: usize = 256;

macro_rules! opaque_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

opaque_id!(ServerId);
opaque_id!(ServerGeneration);
opaque_id!(SessionId);
opaque_id!(TabId);
opaque_id!(PaneId);
opaque_id!(SurfaceId);
opaque_id!(ProcessLifetimeId);
opaque_id!(MutationId);
opaque_id!(AttachmentId);
opaque_id!(UploadId);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TerminalIdentity {
    pub server_id: ServerId,
    pub server_generation: ServerGeneration,
    pub surface_id: SurfaceId,
    pub process_lifetime_id: ProcessLifetimeId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TerminalTarget {
    pub attachment_id: AttachmentId,
    pub identity: TerminalIdentity,
}

/// Versioned separately from the exact full-workspace protocol.
pub const LIFECYCLE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveSurface {
    pub surface_id: SurfaceId,
    pub process_lifetime_id: ProcessLifetimeId,
    pub status: SurfaceStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LifecycleStatus {
    pub lifecycle_version: u32,
    pub product_version: String,
    pub protocol_version: u32,
    pub daemon_pid: u32,
    pub supervisor_pid: Option<u32>,
    pub daemon_executable: String,
    pub server_id: ServerId,
    pub server_generation: ServerGeneration,
    pub instance: Option<String>,
    pub workspace_revision: u64,
    pub connected_clients: Vec<u64>,
    pub live_surfaces: Vec<LiveSurface>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LifecycleConsent {
    pub server_id: ServerId,
    pub server_generation: ServerGeneration,
    pub workspace_revision: u64,
    pub connected_clients: Vec<u64>,
    pub live_surfaces: Vec<LiveSurface>,
}

impl LifecycleStatus {
    pub fn consent(&self) -> LifecycleConsent {
        LifecycleConsent {
            server_id: self.server_id.clone(),
            server_generation: self.server_generation.clone(),
            workspace_revision: self.workspace_revision,
            connected_clients: self.connected_clients.clone(),
            live_surfaces: self.live_surfaces.clone(),
        }
    }
}

pub fn live_surface_inventory(workspace: &WorkspaceSnapshot) -> Vec<LiveSurface> {
    let mut live: Vec<_> = workspace
        .surfaces
        .iter()
        .filter(|surface| {
            matches!(
                surface.status,
                SurfaceStatus::Starting | SurfaceStatus::Running | SurfaceStatus::Ending
            )
        })
        .map(|surface| LiveSurface {
            surface_id: surface.id.clone(),
            process_lifetime_id: surface.process_lifetime_id.clone(),
            status: surface.status,
        })
        .collect();
    live.sort_by(|a, b| a.surface_id.cmp(&b.surface_id));
    live
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClientControl {
    pub request_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<TerminalTarget>,
    #[serde(flatten)]
    pub message: ClientMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello {
        protocol_version: u32,
    },
    GetLifecycleStatus {
        lifecycle_version: u32,
    },
    ConditionalStop {
        lifecycle_version: u32,
        consent: LifecycleConsent,
    },
    GetRuntimeMetrics,
    GetWorkspace,
    ObserveSurface {
        surface_id: SurfaceId,
        expected_lifetime: ProcessLifetimeId,
        scrollback: bool,
    },
    SendSurfaceInput {
        surface_id: SurfaceId,
        expected_lifetime: ProcessLifetimeId,
        data: Vec<u8>,
    },
    GetSurfaceMetadata {
        surface_id: SurfaceId,
        expected_lifetime: ProcessLifetimeId,
    },
    AttachConsole {
        surface_id: SurfaceId,
        expected_lifetime: ProcessLifetimeId,
        cols: i16,
        rows: i16,
    },
    DetachSurface {
        surface_id: SurfaceId,
        expected_lifetime: ProcessLifetimeId,
    },
    ListDirectory {
        surface_id: SurfaceId,
        path: String,
    },
    SearchDirectory {
        surface_id: SurfaceId,
        root: String,
        query: String,
    },
    Mutate {
        mutation: MutationRequest,
    },
    MutationOutcome {
        mutation_id: MutationId,
    },
    Attach {
        surface_id: SurfaceId,
        expected_lifetime: ProcessLifetimeId,
        cols: i16,
        rows: i16,
    },
    Detach,
    Input {
        data: Vec<u8>,
        latency_id: Option<u64>,
    },
    Resize {
        cols: i16,
        rows: i16,
    },
    RequestSnapshot,
    ClearScrollback,
    BeginImageUpload {
        name: String,
        byte_len: u64,
        sha256: [u8; 32],
    },
    UploadImageChunk {
        upload_id: UploadId,
        offset: u64,
        data: Vec<u8>,
    },
    FinishImageUpload {
        upload_id: UploadId,
    },
    /// Prompt settings in the target environment. `distribution` selects a WSL2
    /// distribution on Windows and must be absent elsewhere.
    Prompt {
        distribution: Option<String>,
        request: Box<prompt::PromptRequest>,
    },
    ShutdownDaemon,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerControl {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<u64>,
    #[serde(flatten)]
    pub message: ServerMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Hello {
        protocol_version: u32,
    },
    LifecycleStatus {
        status: LifecycleStatus,
    },
    RuntimeMetrics {
        metrics: RuntimeMetrics,
    },
    Workspace {
        workspace: WorkspaceSnapshot,
    },
    SurfaceObserved {
        identity: TerminalIdentity,
        sequence: u64,
    },
    SurfaceMetadata {
        metadata: Box<metadata::PaneMetadata>,
    },
    DirectoryListed {
        entries: Vec<DirectoryEntry>,
    },
    DirectorySearched {
        entries: Vec<SearchEntry>,
    },
    WorkspaceChanged {
        revision: u64,
    },
    MutationCommitted {
        receipt: MutationReceipt,
    },
    MutationOutcome {
        receipt: Option<MutationReceipt>,
    },
    Attached {
        identity: TerminalIdentity,
        surface: SurfaceInfo,
        attachment_id: AttachmentId,
        sequence: u64,
    },
    Detached {
        surface_id: SurfaceId,
    },
    InputAccepted,
    Resized {
        cols: i16,
        rows: i16,
    },
    SnapshotReady {
        sequence: u64,
    },
    ImageUploadStarted {
        upload_id: UploadId,
    },
    ImageUploadProgress {
        upload_id: UploadId,
        next_offset: u64,
    },
    ImageUploaded {
        path: String,
    },
    DaemonStopping,
    Prompt {
        response: Box<prompt::PromptResponse>,
    },
    SurfaceExited {
        identity: TerminalIdentity,
        exit_code: u32,
    },
    Error {
        code: ErrorCode,
        message: String,
        current_revision: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    IncompatibleProtocol,
    InvalidRequest,
    SurfaceNotFound,
    AlreadyAttached,
    NotAttached,
    SurfaceUnavailable,
    RevisionConflict,
    StaleGeneration,
    StaleLifetime,
    MutationIdReused,
    OutcomeUnknown,
    Busy,
    PersistenceUnavailable,
    Internal,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceStatus {
    Starting,
    Running,
    Ending,
    Exited,
    Failed,
    Lost,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub is_directory: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchEntry {
    /// Absolute path in the terminal's filesystem namespace.
    pub path: String,
    pub is_directory: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkingDirectory {
    pub requested: String,
    pub resolved_wsl_path: String,
    pub distribution: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}
/// Cross-platform process counters. Unsupported counters remain absent rather
/// than being reported as a misleading zero.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProcessMetrics {
    pub cpu_time_ns: Option<u64>,
    pub private_bytes: Option<u64>,
    pub resident_bytes: Option<u64>,
    pub virtual_bytes: Option<u64>,
    pub working_set_bytes: Option<u64>,
    pub handles: Option<u64>,
    pub file_descriptors: Option<u64>,
    pub threads: Option<u64>,
}

/// One inexpensive daemon snapshot used by the in-app Performance surface.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RuntimeMetrics {
    pub process: ProcessMetrics,
    pub surfaces: u32,
    pub live_surfaces: u32,
    pub attached_surfaces: u32,
}

/// Persistable launch choices. Environment overrides travel separately and are never stored.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchProfile {
    pub executable: Option<String>,
    pub args: Vec<String>,
    pub login: Option<bool>,
    pub working_directory: Option<String>,
    pub distribution: Option<String>,
}

/// Invocation-local launch context, retained only until the accepted launch effect runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchContext {
    pub profile: LaunchProfile,
    pub env: std::collections::BTreeMap<String, String>,
    pub scrollback_lines: usize,
    pub graphics_bytes: usize,
}

impl Default for LaunchContext {
    fn default() -> Self {
        Self {
            profile: LaunchProfile::default(),
            env: std::collections::BTreeMap::new(),
            scrollback_lines: 10_000,
            graphics_bytes: DEFAULT_GRAPHICS_BYTES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Box<LaunchProfile>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SurfaceInfo {
    pub id: SurfaceId,
    pub process_lifetime_id: ProcessLifetimeId,
    pub status: SurfaceStatus,
    pub attached: bool,
    pub cols: i16,
    pub rows: i16,
    pub created_at_ms: u64,
    pub exit_code: Option<u32>,
    pub error: Option<String>,
    pub launch: LaunchRequest,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<WorkingDirectory>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutNode {
    /// A terminal pane, backed by one surface and its process.
    Pane {
        pane_id: PaneId,
        surface_id: SurfaceId,
    },
    /// A pane that shows files instead of a terminal. It has no surface or process.
    Content {
        pane_id: PaneId,
        content: PaneContent,
    },
    Split {
        axis: SplitAxis,
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

impl LayoutNode {
    /// The pane of a terminal or content leaf; `None` for a split.
    pub fn leaf_pane(&self) -> Option<&PaneId> {
        match self {
            Self::Pane { pane_id, .. } | Self::Content { pane_id, .. } => Some(pane_id),
            Self::Split { .. } => None,
        }
    }

    /// The surface of a terminal leaf; `None` for content leaves and splits.
    pub fn surface_id(&self) -> Option<&SurfaceId> {
        match self {
            Self::Pane { surface_id, .. } => Some(surface_id),
            Self::Content { .. } | Self::Split { .. } => None,
        }
    }

    /// Whether any leaf in this tree is a content pane.
    pub fn has_content(&self) -> bool {
        match self {
            Self::Pane { .. } => false,
            Self::Content { .. } => true,
            Self::Split { first, second, .. } => first.has_content() || second.has_content(),
        }
    }
}

/// Where a content pane's paths live.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentEnvironment {
    /// The daemon host's own filesystem.
    Host,
    /// A WSL2 distribution of a Windows daemon host; paths are Linux paths inside it.
    Wsl { distribution: String },
}

impl ContentEnvironment {
    /// The environment a terminal runs in, so content opened beside it shows the same files.
    pub fn of_surface(surface: &SurfaceInfo) -> Self {
        surface
            .working_directory
            .as_ref()
            .map(|directory| directory.distribution.as_str())
            .or_else(|| {
                surface
                    .launch
                    .profile
                    .as_ref()
                    .and_then(|profile| profile.distribution.as_deref())
            })
            .filter(|distribution| !distribution.is_empty())
            .map_or(Self::Host, |distribution| Self::Wsl {
                distribution: distribution.to_owned(),
            })
    }
}

impl fmt::Display for ContentEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host => formatter.write_str("host"),
            Self::Wsl { distribution } => write!(formatter, "WSL: {distribution}"),
        }
    }
}

/// What a content pane shows: a folder in one environment and the files open from it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaneContent {
    pub environment: ContentEnvironment,
    /// Absolute folder path in `environment`.
    pub root: String,
    /// Absolute file paths in `environment`, in display order, each once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// One of `files`, or `None` when no file is shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_file: Option<String>,
}

impl PaneContent {
    /// Shape checks every host applies. Whether a path is absolute on the daemon's host
    /// is checked by the daemon, which knows its own path rules.
    pub fn validate(&self) -> Result<(), String> {
        fn path(value: &str, what: &str) -> Result<(), String> {
            if value.is_empty() || value.len() > MAX_DIRECTORY_PATH_BYTES || value.contains('\0') {
                return Err(format!(
                    "{what} must be a non-empty path of at most {MAX_DIRECTORY_PATH_BYTES} bytes"
                ));
            }
            Ok(())
        }
        if let ContentEnvironment::Wsl { distribution } = &self.environment
            && (distribution.is_empty()
                || distribution.len() > 256
                || distribution.contains(['\0', '/', '\\']))
        {
            return Err("content environment names an invalid WSL distribution".into());
        }
        path(&self.root, "content root")?;
        if self.files.len() > MAX_CONTENT_FILES {
            return Err(format!(
                "a content pane can list at most {MAX_CONTENT_FILES} files"
            ));
        }
        let mut seen = std::collections::HashSet::with_capacity(self.files.len());
        for file in &self.files {
            path(file, "content file")?;
            if !seen.insert(file.as_str()) {
                return Err(format!("content file {file} is listed twice"));
            }
        }
        if let Some(active) = &self.active_file
            && !seen.contains(active.as_str())
        {
            return Err("the active file must be one of the open files".into());
        }
        Ok(())
    }

    /// The root folder's last component, for captions.
    pub fn folder_name(&self) -> &str {
        let trimmed = self.root.trim_end_matches(['/', '\\']);
        trimmed
            .rsplit(['/', '\\'])
            .find(|part| !part.is_empty())
            .unwrap_or(if trimmed.is_empty() {
                &self.root
            } else {
                trimmed
            })
    }
}

/// A complete tab layout proposed for atomic growth. Existing panes retain their
/// surfaces and processes; the actor assigns identities to each new pane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlannedLayoutNode {
    ExistingPane {
        pane_id: PaneId,
    },
    NewPane,
    Split {
        axis: SplitAxis,
        ratio: f32,
        first: Box<PlannedLayoutNode>,
        second: Box<PlannedLayoutNode>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceTab {
    pub id: TabId,
    pub label: String,
    pub layout: LayoutNode,
    /// The tree replaced by the latest `ArrangeTab`, holding only panes that are
    /// still in this tab. Restoring it is itself an arrangement. A merge clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_layout: Option<Box<LayoutNode>>,
    /// How to split this tab back into the tabs merged into it, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge: Option<Box<TabMerge>>,
}

/// The pre-merge shape of a merged tab, holding only panes still in that tab.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TabMerge {
    /// The receiving tab's own tree before its first merge; `None` once none of
    /// its original panes remain.
    pub own_layout: Option<LayoutNode>,
    /// Tabs merged in, in the order they are recreated.
    pub tabs: Vec<MergedTab>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MergedTab {
    pub id: TabId,
    pub label: String,
    pub layout: LayoutNode,
    /// Session position before the merge; splitting reinserts at this index.
    pub index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceSession {
    pub id: SessionId,
    pub label: String,
    pub tabs: Vec<WorkspaceTab>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceSnapshot {
    pub server_id: ServerId,
    pub server_generation: ServerGeneration,
    pub revision: u64,
    pub initialized: bool,
    pub sessions: Vec<WorkspaceSession>,
    pub surfaces: Vec<SurfaceInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_message: Option<String>,
}

impl WorkspaceSnapshot {
    pub fn surface(&self, id: &SurfaceId) -> Option<&SurfaceInfo> {
        self.surfaces.iter().find(|surface| &surface.id == id)
    }

    /// The first terminal in workspace order, skipping content panes.
    pub fn first_surface(&self) -> Option<&SurfaceInfo> {
        fn first(node: &LayoutNode) -> Option<&SurfaceId> {
            match node {
                LayoutNode::Pane { surface_id, .. } => Some(surface_id),
                LayoutNode::Content { .. } => None,
                LayoutNode::Split {
                    first: a, second, ..
                } => first(a).or_else(|| first(second)),
            }
        }
        self.sessions
            .iter()
            .flat_map(|session| &session.tabs)
            .find_map(|tab| self.surface(first(&tab.layout)?))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MutationRequest {
    pub server_id: ServerId,
    pub expected_generation: ServerGeneration,
    pub mutation_id: MutationId,
    pub expected_revision: u64,
    pub operation: WorkspaceMutation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<Box<LaunchContext>>,
}

/// Measured focused-pane rectangle and minimum child allocations in logical pixels.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct SplitGeometry {
    pub width: f32,
    pub height: f32,
    pub min_width: f32,
    pub min_height: f32,
    pub divider: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkspaceMutation {
    Initialize {
        cols: i16,
        rows: i16,
        working_directory: Option<String>,
    },
    CreateSession {
        label: String,
    },
    RenameSession {
        session_id: SessionId,
        label: String,
    },
    RemoveSession {
        session_id: SessionId,
    },
    CreateTab {
        session_id: SessionId,
        label: String,
        cols: i16,
        rows: i16,
        working_directory: Option<String>,
    },
    RenameTab {
        tab_id: TabId,
        label: String,
    },
    MoveTab {
        session_id: SessionId,
        tab_id: TabId,
        index: usize,
    },
    RemoveTab {
        tab_id: TabId,
    },
    SplitPane {
        pane_id: PaneId,
        axis: SplitAxis,
        cols: i16,
        rows: i16,
        working_directory: Option<String>,
        geometry: SplitGeometry,
    },
    DetachPane {
        pane_id: PaneId,
    },
    SetSplitRatio {
        tab_id: TabId,
        path: Vec<bool>,
        ratio: f32,
    },
    /// Replace a tab's tree with one holding exactly the same pane/surface leaves.
    /// No process is launched, restarted, or ended; panes only move and resize.
    ArrangeTab {
        tab_id: TabId,
        layout: LayoutNode,
    },
    /// Move every pane of `sources` (same session) into `tab_id`, laid out by
    /// `layout`, which must hold exactly all of their leaves; the emptied tabs
    /// are removed. No process is launched, restarted, or ended.
    MergeTabs {
        tab_id: TabId,
        sources: Vec<TabId>,
        layout: LayoutNode,
    },
    /// Recreate the tabs recorded by `MergeTabs` with their IDs, labels, order,
    /// and inner splits, moving their surviving panes back out of `tab_id`.
    SplitMergedTabs {
        tab_id: TabId,
    },
    RemovePane {
        pane_id: PaneId,
    },
    EndSurface {
        surface_id: SurfaceId,
        expected_lifetime: ProcessLifetimeId,
    },
    RestartSurface {
        surface_id: SurfaceId,
        expected_lifetime: ProcessLifetimeId,
        cols: i16,
        rows: i16,
    },
    /// Grow and arrange a tab in one transaction, preserving every existing pane.
    /// The final layout must fit minimum 20-column by 4-row pane allocations, so
    /// tabs holding content panes, which are not sized in cells, are rejected.
    GrowTab {
        tab_id: TabId,
        layout: PlannedLayoutNode,
        working_directory: Option<String>,
    },
    /// Split `pane_id` and put a new content pane second, without starting a shell.
    /// `geometry` is checked like `SplitPane`'s.
    OpenContent {
        pane_id: PaneId,
        axis: SplitAxis,
        geometry: SplitGeometry,
        content: PaneContent,
    },
    /// Replace what the content pane `pane_id` shows.
    UpdateContent {
        pane_id: PaneId,
        content: PaneContent,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutationReceipt {
    pub mutation_id: MutationId,
    pub fingerprint: String,
    pub revision: u64,
    pub affected_sessions: Vec<SessionId>,
    pub affected_tabs: Vec<TabId>,
    pub affected_panes: Vec<PaneId>,
    pub affected_surfaces: Vec<SurfaceId>,
    pub operation_state: String,
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
    fn ids_are_distinct_and_serialize_as_strings() {
        let surface = SurfaceId::new("surface-1");
        assert_eq!(serde_json::to_string(&surface).unwrap(), "\"surface-1\"");
        assert_eq!(surface.as_str(), "surface-1");
    }

    #[test]
    fn filesystem_requests_and_replies_roundtrip() {
        let surface_id = SurfaceId::new("surface-from-workspace");
        let requests = [
            ClientMessage::ListDirectory {
                surface_id: surface_id.clone(),
                path: "/home/user".into(),
            },
            ClientMessage::SearchDirectory {
                surface_id,
                root: "/home/user".into(),
                query: "main.rs".into(),
            },
        ];
        for message in requests {
            let request = ClientControl {
                request_id: 7,
                target: None,
                message,
            };
            assert_eq!(
                decode_client(&encode_client(&request).unwrap()).unwrap(),
                request
            );
        }

        let replies = [
            ServerMessage::DirectoryListed {
                entries: vec![DirectoryEntry {
                    name: "src".into(),
                    is_directory: true,
                }],
            },
            ServerMessage::DirectorySearched {
                entries: vec![SearchEntry {
                    path: "/home/user/src/main.rs".into(),
                    is_directory: false,
                }],
            },
        ];
        for message in replies {
            let response = ServerControl {
                request_id: Some(7),
                message,
            };
            assert_eq!(
                decode_server(&encode_server(&response).unwrap()).unwrap(),
                response
            );
        }
    }

    #[test]
    fn workspace_finds_first_surface_in_nested_layout() {
        let surface = SurfaceInfo {
            id: SurfaceId::new("surface-1"),
            process_lifetime_id: ProcessLifetimeId::new("lifetime-1"),
            status: SurfaceStatus::Running,
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
        let workspace = WorkspaceSnapshot {
            server_id: ServerId::new("server-1"),
            server_generation: ServerGeneration::new("generation-1"),
            revision: 1,
            initialized: true,
            sessions: vec![WorkspaceSession {
                id: SessionId::new("session-1"),
                label: "Default".into(),
                tabs: vec![WorkspaceTab {
                    id: TabId::new("tab-1"),
                    label: "Shell".into(),
                    layout: LayoutNode::Pane {
                        pane_id: PaneId::new("pane-1"),
                        surface_id: surface.id.clone(),
                    },
                    previous_layout: None,
                    merge: None,
                }],
            }],
            surfaces: vec![surface.clone()],
            recovery_message: None,
        };
        assert_eq!(workspace.first_surface(), Some(&surface));
    }
}

#[cfg(test)]
mod content_tests {
    use super::*;

    fn content(files: &[&str], active: Option<&str>) -> PaneContent {
        PaneContent {
            environment: ContentEnvironment::Wsl {
                distribution: "Ubuntu".into(),
            },
            root: "/home/user/project/".into(),
            files: files.iter().map(|file| (*file).to_owned()).collect(),
            active_file: active.map(str::to_owned),
        }
    }

    #[test]
    fn content_leaf_roundtrips_and_has_no_surface() {
        let leaf = LayoutNode::Content {
            pane_id: PaneId::new("pane-2"),
            content: content(
                &["/home/user/project/a.rs"],
                Some("/home/user/project/a.rs"),
            ),
        };
        let json = serde_json::to_string(&leaf).unwrap();
        assert_eq!(serde_json::from_str::<LayoutNode>(&json).unwrap(), leaf);
        assert_eq!(leaf.leaf_pane(), Some(&PaneId::new("pane-2")));
        assert_eq!(leaf.surface_id(), None);
        assert!(leaf.has_content());
        if let LayoutNode::Content { content, .. } = &leaf {
            assert_eq!(content.folder_name(), "project");
            assert_eq!(content.environment.to_string(), "WSL: Ubuntu");
        }
    }

    #[test]
    fn content_validation_rejects_duplicates_and_stray_active_files() {
        assert!(content(&["/a", "/b"], Some("/b")).validate().is_ok());
        assert!(content(&["/a", "/a"], None).validate().is_err());
        assert!(content(&["/a"], Some("/b")).validate().is_err());
        assert!(content(&[""], None).validate().is_err());
        let mut root = content(&[], None);
        root.root.clear();
        assert!(root.validate().is_err());
    }
}
