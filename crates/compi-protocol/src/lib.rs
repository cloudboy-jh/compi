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
pub mod program_status;
pub mod prompt;
mod screen;
pub mod system_metrics;
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
// Version 20 adds content files: environment directory listing, file reads and versioned saves.
// Version 21 says why a file is not shown as text, reports writability and checks a held
// version against the file on disk.
// Version 22 adds the review of a repository's uncommitted Git changes.
// Version 23 replaces a content pane's files and view with tabs holding files and diffs.
// Version 24 adds Git writes: stage, unstage, hunk staging, discard and commit.
// Version 25 adds Git branches: upstream and ahead/behind, the branch list, switch and create.
// Version 26 adds program status (OSC 7501): watching every terminal's records and
// acknowledging finished ones.
// Version 27 makes a content pane's content a kind: files (a folder and its tabs) or
// Settings, and opens a tab holding only a content pane.
// Version 28 adds Monitor: a content kind and watching the host's processes.
// Version 29 adds views in place: a terminal pane may carry one content view, shown instead
// of its shell while the shell keeps running underneath at its size.
pub const PROTOCOL_VERSION: u32 = 29;
pub const CONTROL_FRAME: u8 = 1;
pub const SCREEN_FRAME: u8 = 2;
/// A content file of `MAX_CONTENT_FILE_BYTES` stays within this even if JSON escapes every
/// byte into six characters.
pub const MAX_CONTROL_PAYLOAD: usize = 8 * 1024 * 1024;
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
/// Tabs (files and diffs) one content pane may hold.
pub const MAX_CONTENT_FILES: usize = 256;
/// Largest file a content pane reads or saves, in bytes.
pub const MAX_CONTENT_FILE_BYTES: usize = 1024 * 1024;
/// Changes one Git changes listing returns.
pub const MAX_GIT_CHANGES: usize = 5000;
/// Branches one Git branch listing returns.
pub const MAX_GIT_BRANCHES: usize = 2000;
/// Largest image a Git file read returns as an image, in bytes.
pub const MAX_GIT_IMAGE_BYTES: usize = 4 * 1024 * 1024;

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
    /// Immediate children of an absolute folder in a content environment.
    ListContentDirectory {
        environment: ContentEnvironment,
        path: String,
    },
    /// A file's text, or why it cannot be shown as text.
    ReadContentFile {
        environment: ContentEnvironment,
        path: String,
    },
    /// Whether the file still holds `known`; cheap while its size and modification time
    /// are unchanged.
    CheckContentFile {
        environment: ContentEnvironment,
        path: String,
        known: FileVersion,
    },
    /// Replace a file's contents only if they still match `expected`; `None` creates a new
    /// file and fails if one exists. A mismatch fails with `ErrorCode::FileConflict`.
    WriteContentFile {
        environment: ContentEnvironment,
        path: String,
        expected: Option<FileVersion>,
        text: String,
    },
    /// The uncommitted changes of the Git work tree holding `path`, a folder or file.
    ReadGitChanges {
        environment: ContentEnvironment,
        path: String,
    },
    /// One side of a changed file: `path` is relative to the repository top level `repo`.
    ReadGitFile {
        environment: ContentEnvironment,
        repo: String,
        path: String,
        revision: GitRevision,
    },
    /// The local and remote-tracking branches of the repository whose top level is `repo`.
    ReadGitBranches {
        environment: ContentEnvironment,
        repo: String,
    },
    /// A Git write in the repository whose top level is `repo`; writes to one repository
    /// run one at a time. Git's own failure text is the error message.
    GitWrite {
        environment: ContentEnvironment,
        repo: String,
        action: GitAction,
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
        /// GUI-synthesized alternate-screen wheel arrows, not ordinary keyboard input.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        alternate_scroll: bool,
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
    /// Sends each terminal's current program status as `ProgramStatusChanged`, answers
    /// `ProgramStatusWatching`, then sends every later change on this connection.
    WatchProgramStatus,
    /// The user has seen the surface's results: removes its `done` and `error` records.
    AcknowledgeProgramStatus {
        surface_id: SurfaceId,
        expected_lifetime: ProcessLifetimeId,
    },
    /// Answers `SystemMetricsWatching`, then sends `SystemSampled` every few seconds while
    /// this connection stays open. Sampling runs only while some connection watches.
    WatchSystemMetrics,
    /// Answers `SystemTotalsWatching` with the host and the last hour of totals, then sends
    /// each later sample as `SystemTotalsSampled`, in order, while this connection stays open.
    WatchSystemTotals,
    /// The icon of an application the daemon named in an `AppGroup`, answered by `AppIcon`.
    GetAppIcon {
        group_id: String,
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
    ContentDirectoryListed {
        entries: Vec<DirectoryEntry>,
    },
    ContentFileRead {
        content: FileContent,
    },
    ContentFileChecked {
        state: FileState,
    },
    ContentFileWritten {
        version: FileVersion,
    },
    /// `None` when the path is not inside a Git work tree.
    GitChangesRead {
        changes: Option<GitChanges>,
    },
    GitFileRead {
        content: GitFileContent,
    },
    GitBranchesRead {
        listing: GitBranches,
    },
    GitWritten {
        result: GitWriteResult,
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
    /// Unsolicited, after `WatchProgramStatus`: the terminal's complete current records.
    ProgramStatusChanged {
        status: program_status::SurfaceProgramStatus,
    },
    ProgramStatusWatching,
    ProgramStatusAcknowledged,
    SystemMetricsWatching,
    SystemTotalsWatching {
        host: system_metrics::HostInfo,
        history: Vec<system_metrics::TotalsSample>,
    },
    /// Unsolicited, after `WatchSystemTotals`: the next totals sample. Samples arrive in
    /// order; a connection that falls an hour behind skips to what is still kept.
    SystemTotalsSampled {
        sample: system_metrics::TotalsSample,
    },
    /// `None` when the application has no icon the daemon can read.
    AppIcon {
        group_id: String,
        /// A PNG of at most `MAX_APP_ICON_BYTES`, standard padded base64.
        png_base64: Option<String>,
    },
    /// Unsolicited, after `WatchSystemMetrics`: the latest sample of the host. A client
    /// behind on reading is sent the newest one, never a backlog.
    SystemSampled {
        sample: system_metrics::SystemSample,
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
    /// A save found the file changed since `expected`, or already present when creating.
    FileConflict,
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

/// The bytes a content file held when it was last read or saved.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FileVersion {
    pub len: u64,
    /// Lowercase hexadecimal SHA-256 of the file's bytes.
    pub sha256: String,
}

/// What reading a content file found.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileContent {
    /// UTF-8 text of at most `MAX_CONTENT_FILE_BYTES`.
    Text {
        text: String,
        version: FileVersion,
        /// Whether a save can replace the file.
        writable: bool,
    },
    /// Larger than `MAX_CONTENT_FILE_BYTES`; nothing was read past the limit.
    TooLarge { len: u64 },
    /// Not UTF-8 text.
    Binary { len: u64 },
}

/// A content file on disk compared with a version the client holds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileState {
    Unchanged {
        writable: bool,
    },
    Changed {
        version: FileVersion,
        writable: bool,
    },
    /// Deleted, or no longer a regular file.
    Missing,
}

/// Which comparison a change belongs to, as in `git status`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitScope {
    /// The work tree against the index.
    Unstaged,
    /// The index against `HEAD`.
    Staged,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Conflicted,
}

/// Which version of a repository file to read.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitRevision {
    Head,
    Index,
    WorkTree,
}

/// One changed path in one scope. A path changed both in the index and in the work tree
/// appears once in each scope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitChange {
    pub scope: GitScope,
    pub kind: GitChangeKind,
    /// Repository-relative, `/`-separated.
    pub path: String,
    /// The source path of a rename or copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    /// Lines added and removed (git numstat); `None` for binary, untracked and conflicted
    /// files.
    pub added: Option<u32>,
    pub removed: Option<u32>,
    /// Opaque; differs whenever either side of this change may hold different bytes
    /// (index/HEAD blob ids, plus the work-tree file's length and modification time for
    /// Unstaged).
    pub stamp: String,
}

/// A repository's uncommitted changes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitChanges {
    /// Absolute repository top level in the request's environment.
    pub repo: String,
    /// Current branch, `None` when detached or unborn.
    pub branch: Option<String>,
    /// Abbreviated HEAD commit, `None` before the first commit.
    pub head: Option<String>,
    /// The current branch's upstream, such as `origin/main`; `None` when it has none.
    pub upstream: Option<String>,
    /// Commits on the current branch and not on its upstream, and the reverse; 0 without an
    /// upstream or when the upstream branch is gone.
    pub ahead: u32,
    pub behind: u32,
    /// Unstaged entries first, then staged; each group sorted by path.
    pub changes: Vec<GitChange>,
    /// More than `MAX_GIT_CHANGES` entries; the list was cut.
    pub truncated: bool,
}

/// A repository's branches: the current one first, then local branches by most recent
/// commit, then remote-tracking branches by most recent commit.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitBranches {
    pub branches: Vec<GitBranch>,
    /// More than `MAX_GIT_BRANCHES` branches; the list was cut.
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitBranch {
    /// `main` for a local branch, `origin/main` for a remote-tracking one.
    pub name: String,
    /// Checked out in this work tree.
    pub current: bool,
    /// A local branch's upstream, such as `origin/main`.
    pub upstream: Option<String>,
    /// A remote-tracking branch; switching to it creates a local branch tracking it.
    pub remote: bool,
}

/// One side of a changed file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GitFileContent {
    /// UTF-8 text of at most `MAX_CONTENT_FILE_BYTES`.
    Text { text: String },
    /// An image of at most `MAX_GIT_IMAGE_BYTES`, recognised by extension and signature,
    /// as standard padded base64.
    Image {
        format: GitImageFormat,
        base64: String,
        len: u64,
    },
    /// Not UTF-8 text, and not a recognised image.
    Binary { len: u64 },
    /// Larger than `MAX_CONTENT_FILE_BYTES`, or `MAX_GIT_IMAGE_BYTES` for an image.
    TooLarge { len: u64 },
    /// No such file at that revision (added, deleted or untracked side).
    Missing,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitImageFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
    Ico,
    Svg,
}

/// A write to a repository. Paths are relative to its top level, as `ReadGitFile` checks them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GitAction {
    /// `git add -A -- <paths>`: stages modifications, additions and deletions.
    Stage { paths: Vec<String> },
    /// `git restore --staged -- <paths>`; before the first commit `git rm --cached`.
    Unstage { paths: Vec<String> },
    /// Replace lines `lines` (0-based, half-open) of the path's index text with
    /// `replacement` (exact text including line endings, as `ReadGitFile` shows the index
    /// side), only if that text still has the version `expected`. Staging a hunk replaces
    /// its index lines with its work-tree lines; unstaging one restores its `HEAD` lines.
    /// The index changed since then: `GitWriteResult::Stale`.
    SetIndexLines {
        path: String,
        expected: FileVersion,
        lines: std::ops::Range<u32>,
        replacement: String,
    },
    /// `git restore --worktree -- <paths>` for tracked paths, `git clean -f -q -- <untracked>`.
    Discard {
        paths: Vec<String>,
        untracked: Vec<String>,
    },
    /// `git commit -F -` with `message` on standard input, `--amend` when asked.
    Commit { message: String, amend: bool },
    /// `HEAD`'s full message, to edit for an amend; writes nothing.
    LastMessage,
    /// `git switch <name>` to the local branch `name`; when there is none but a
    /// remote-tracking branch `name` (`origin/x`), `git switch --track <name>`, which creates
    /// the local branch `x` tracking it (git refuses if `x` exists). Never forced: local
    /// changes the switch would overwrite make git refuse, and its refusal is the error.
    SwitchBranch { name: String },
    /// `git switch -c <name>`: a new branch at `HEAD`, checked out. The name must be a valid
    /// branch name (`git check-ref-format --branch`) not starting with `-`.
    CreateBranch { name: String },
}

/// What a Git write did.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GitWriteResult {
    Done,
    /// The new `HEAD`, abbreviated, and its summary line.
    Committed {
        head: String,
        summary: String,
    },
    /// `HEAD`'s message (`LastMessage`).
    Message {
        text: String,
    },
    /// The index text changed since the client read it; nothing was written.
    Stale,
}

/// Whether `path` names a file relative to a repository top level: `/`-separated, not
/// absolute, without NUL or `..` components.
pub fn is_repo_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_DIRECTORY_PATH_BYTES
        && !path.starts_with('/')
        && !path.contains('\0')
        && !path.split('/').any(|part| part == "..")
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
    /// The WSL2 distribution the shell runs in, on a Windows daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution: Option<String>,
}

impl SurfaceInfo {
    /// The WSL2 distribution this terminal's shell runs in: the one the daemon launched
    /// it in, else the one its requested directory or profile names. `None` for a shell
    /// on the daemon host itself.
    pub fn wsl_distribution(&self) -> Option<&str> {
        self.distribution
            .as_deref()
            .or_else(|| {
                self.working_directory
                    .as_ref()
                    .map(|directory| directory.distribution.as_str())
            })
            .or_else(|| {
                self.launch
                    .profile
                    .as_ref()
                    .and_then(|profile| profile.distribution.as_deref())
            })
            .filter(|distribution| !distribution.is_empty())
    }
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
    /// A terminal pane, backed by one surface and its process. It may carry a content
    /// view shown in its place.
    Pane {
        pane_id: PaneId,
        surface_id: SurfaceId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<PaneView>,
    },
    /// A pane that shows files or Settings instead of a terminal. It has no surface or
    /// process.
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
    /// A terminal leaf with no view.
    pub fn terminal(pane_id: PaneId, surface_id: SurfaceId) -> Self {
        Self::Pane {
            pane_id,
            surface_id,
            view: None,
        }
    }

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

    /// A terminal leaf's view, shown or not.
    pub fn view(&self) -> Option<&PaneView> {
        match self {
            Self::Pane { view, .. } => view.as_ref(),
            Self::Content { .. } | Self::Split { .. } => None,
        }
    }

    /// What this leaf holds besides a shell: a content leaf's content or a terminal
    /// leaf's view, shown or not.
    pub fn content(&self) -> Option<&PaneContent> {
        match self {
            Self::Content { content, .. } => Some(content),
            Self::Pane { view, .. } => view.as_ref().map(|view| &view.content),
            Self::Split { .. } => None,
        }
    }

    /// The content drawn in this leaf right now: a content leaf's, or a terminal leaf's
    /// view while it shows its view side.
    pub fn shown_content(&self) -> Option<&PaneContent> {
        match self {
            Self::Content { content, .. } => Some(content),
            Self::Pane { view, .. } => view
                .as_ref()
                .filter(|view| view.side == PaneSide::View)
                .map(|view| &view.content),
            Self::Split { .. } => None,
        }
    }

    /// Whether any leaf in this tree is a content pane. A terminal leaf with a view is
    /// not: it is still sized in cells.
    pub fn has_content(&self) -> bool {
        match self {
            Self::Pane { .. } => false,
            Self::Content { .. } => true,
            Self::Split { first, second, .. } => first.has_content() || second.has_content(),
        }
    }
}

/// Which side a terminal pane with a view shows.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PaneSide {
    /// The shell.
    Terminal,
    /// The view.
    #[default]
    View,
}

/// Content carried by a terminal pane and shown in place of its shell. The shell keeps
/// running underneath at its size; the view shares the pane's ID.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaneView {
    pub content: PaneContent,
    #[serde(default)]
    pub side: PaneSide,
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
            .wsl_distribution()
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

/// What a content pane shows. Every kind tiles, floats, moves and closes alike; only
/// what is drawn inside differs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaneContent {
    /// A folder in one environment and the files open from it.
    Files(FolderContent),
    /// The client's Settings. Only the section shown is kept; what the settings hold
    /// lives in each client's configuration.
    Settings(SettingsContent),
    /// Monitor: the daemon host's processes.
    Monitor(MonitorContent),
}

impl PaneContent {
    /// Shape checks every host applies. Whether a path is absolute on the daemon's host
    /// is checked by the daemon, which knows its own path rules.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Files(folder) => folder.validate(),
            Self::Settings(_) | Self::Monitor(_) => Ok(()),
        }
    }

    /// The folder and files shown, when this shows files.
    pub fn folder(&self) -> Option<&FolderContent> {
        match self {
            Self::Files(folder) => Some(folder),
            Self::Settings(_) | Self::Monitor(_) => None,
        }
    }

    /// The Settings shown, when this shows Settings.
    pub fn settings(&self) -> Option<&SettingsContent> {
        match self {
            Self::Settings(settings) => Some(settings),
            Self::Files(_) | Self::Monitor(_) => None,
        }
    }

    /// Whether this shows Monitor.
    pub fn is_monitor(&self) -> bool {
        matches!(self, Self::Monitor(_))
    }

    /// Whether `other` is the same kind of content, which an update must keep.
    pub fn same_kind(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

/// A content pane showing files: a folder in one environment and the tabs open from it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FolderContent {
    pub environment: ContentEnvironment,
    /// Absolute folder path in `environment`.
    pub root: String,
    /// Open tabs in display order, each once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<ContentTab>,
    /// One of `tabs`, or `None` when the folder is shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<ContentTab>,
}

/// A content pane showing Settings, open at `section`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingsContent {
    pub section: SettingsSection,
}

/// A content pane showing Monitor, open at `page`. What it shows is the daemon's host; how
/// it is sorted, filtered and which range it shows is each window's own.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MonitorContent {
    #[serde(default)]
    pub page: MonitorPage,
}

/// Monitor's pages, in the order they are listed.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MonitorPage {
    #[default]
    Overview,
    Processes,
    Cpu,
    Memory,
    Disk,
    Network,
}

/// The sections of Settings, in the order they are listed.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SettingsSection {
    #[default]
    Appearance,
    Interface,
    Terminal,
    Keyboard,
    /// Monitor's Overview. Saved as "performance", the section's name before it
    /// became Monitor, so older workspace files still load.
    #[serde(rename = "performance")]
    Monitor,
    Updates,
    Advanced,
}

impl SettingsSection {
    pub const ALL: [Self; 7] = [
        Self::Appearance,
        Self::Interface,
        Self::Terminal,
        Self::Keyboard,
        Self::Monitor,
        Self::Updates,
        Self::Advanced,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Interface => "Interface",
            Self::Terminal => "Terminal",
            Self::Keyboard => "Keyboard",
            Self::Monitor => "Monitor",
            Self::Updates => "Updates",
            Self::Advanced => "Advanced",
        }
    }
}

/// One tab of a files content pane: one per file. Only `FolderContent` holds tabs, so
/// every tab is a file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentTab {
    /// An open file: absolute path in the pane's environment. `diff` set shows the file's
    /// change against that baseline (Diff mode) instead of the editor (Edit mode); a deleted
    /// file is a tab in Diff mode whose path no longer exists.
    File {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<GitScope>,
    },
}

impl ContentTab {
    /// The file in Edit mode.
    pub fn edit(path: impl Into<String>) -> Self {
        Self::File {
            path: path.into(),
            diff: None,
        }
    }

    /// The file in Diff mode against `scope`'s baseline.
    pub fn diff(path: impl Into<String>, scope: GitScope) -> Self {
        Self::File {
            path: path.into(),
            diff: Some(scope),
        }
    }

    /// The file's absolute path.
    pub fn path(&self) -> &str {
        let Self::File { path, .. } = self;
        path
    }

    /// The comparison shown in Diff mode, `None` in Edit mode.
    pub fn diff_scope(&self) -> Option<GitScope> {
        let Self::File { diff, .. } = self;
        *diff
    }

    /// The same file in another mode.
    pub fn with_diff(&self, diff: Option<GitScope>) -> Self {
        Self::File {
            path: self.path().to_owned(),
            diff,
        }
    }
}

impl FolderContent {
    /// Shape checks every host applies; see `PaneContent::validate`.
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
        if self.tabs.len() > MAX_CONTENT_FILES {
            return Err(format!(
                "a content pane can list at most {MAX_CONTENT_FILES} tabs"
            ));
        }
        let mut seen = std::collections::HashSet::with_capacity(self.tabs.len());
        for tab in &self.tabs {
            path(tab.path(), "content file")?;
            if !seen.insert(tab.path()) {
                return Err(format!("content file {} is listed twice", tab.path()));
            }
        }
        if let Some(active) = &self.active
            && !self.tabs.contains(active)
        {
            return Err("the active tab must be one of the open tabs".into());
        }
        Ok(())
    }

    /// Open files' paths in display order, in either mode.
    pub fn files(&self) -> impl Iterator<Item = &str> {
        self.tabs.iter().map(ContentTab::path)
    }

    /// The open tab of `path`, in whichever mode it is.
    pub fn tab(&self, path: &str) -> Option<&ContentTab> {
        self.tabs.iter().find(|tab| tab.path() == path)
    }

    /// The active tab's path when it shows the editor (Edit mode).
    pub fn active_file(&self) -> Option<&str> {
        self.active
            .as_ref()
            .filter(|tab| tab.diff_scope().is_none())
            .map(ContentTab::path)
    }

    /// The active tab's path and comparison when it shows a diff (Diff mode).
    pub fn active_diff(&self) -> Option<(&str, GitScope)> {
        let tab = self.active.as_ref()?;
        Some((tab.path(), tab.diff_scope()?))
    }

    /// Opens `tab` (its file's tab moves to that mode in place, else it is appended) and makes
    /// it active.
    pub fn show(&mut self, tab: ContentTab) {
        match self.tabs.iter_mut().find(|open| open.path() == tab.path()) {
            Some(open) => *open = tab.clone(),
            None => self.tabs.push(tab.clone()),
        }
        self.active = Some(tab);
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
    /// A new tab holding one content pane and no shell.
    CreateContentTab {
        session_id: SessionId,
        label: String,
        content: PaneContent,
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
    /// Replace what the content pane `pane_id` shows, or the view a terminal pane
    /// carries; the kind must stay the same and a view keeps its side.
    UpdateContent {
        pane_id: PaneId,
        content: PaneContent,
    },
    /// Give the terminal pane `pane_id` the view `content`, replacing any view it
    /// carries, and show it. No process starts or ends; the shell keeps its size.
    ShowView {
        pane_id: PaneId,
        content: PaneContent,
    },
    /// Show the terminal pane `pane_id`'s shell or its view.
    SetPaneSide {
        pane_id: PaneId,
        side: PaneSide,
    },
    /// Drop the view the terminal pane `pane_id` carries; its shell is shown.
    CloseView {
        pane_id: PaneId,
    },
    /// Take the terminal pane `pane_id`'s view out into a new content pane placed
    /// second beside it, like `OpenContent`; the terminal shows its shell.
    MoveViewToPane {
        pane_id: PaneId,
        axis: SplitAxis,
        geometry: SplitGeometry,
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
    fn input_json_remains_compatible_without_alternate_scroll_marker() {
        let legacy = br#"{"request_id":7,"type":"input","data":[27,91,65],"latency_id":null}"#;
        let request = decode_client(legacy).unwrap();
        assert_eq!(
            request.message,
            ClientMessage::Input {
                data: b"\x1b[A".to_vec(),
                latency_id: None,
                alternate_scroll: false,
            }
        );
        let encoded: serde_json::Value =
            serde_json::from_slice(&encode_client(&request).unwrap()).unwrap();
        assert!(encoded.get("alternate_scroll").is_none());
        assert_eq!(
            encoded,
            serde_json::from_slice::<serde_json::Value>(legacy).unwrap()
        );
    }

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
    fn git_writes_roundtrip_with_tagged_actions() {
        let request = ClientControl {
            request_id: 3,
            target: None,
            message: ClientMessage::GitWrite {
                environment: ContentEnvironment::Host,
                repo: "/home/user/project".into(),
                action: GitAction::SetIndexLines {
                    path: "src/main.rs".into(),
                    expected: FileVersion {
                        len: 4,
                        sha256: "ab".into(),
                    },
                    lines: 1..3,
                    replacement: "one\r\ntwo\n".into(),
                },
            },
        };
        let encoded = serde_json::to_value(&request.message).unwrap();
        assert_eq!(encoded["action"]["type"], "set_index_lines");
        assert_eq!(encoded["action"]["lines"]["start"], 1);
        assert_eq!(
            decode_client(&encode_client(&request).unwrap()).unwrap(),
            request
        );
        for result in [
            GitWriteResult::Done,
            GitWriteResult::Stale,
            GitWriteResult::Committed {
                head: "a1b2c3d".into(),
                summary: "Fix".into(),
            },
            GitWriteResult::Message {
                text: "Fix\n".into(),
            },
        ] {
            let response = ServerControl {
                request_id: Some(3),
                message: ServerMessage::GitWritten { result },
            };
            assert_eq!(
                decode_server(&encode_server(&response).unwrap()).unwrap(),
                response
            );
        }
    }

    #[test]
    fn git_branches_roundtrip() {
        let requests = [
            ClientMessage::ReadGitBranches {
                environment: ContentEnvironment::Host,
                repo: "/home/user/project".into(),
            },
            ClientMessage::GitWrite {
                environment: ContentEnvironment::Host,
                repo: "/home/user/project".into(),
                action: GitAction::SwitchBranch {
                    name: "origin/feature".into(),
                },
            },
            ClientMessage::GitWrite {
                environment: ContentEnvironment::Host,
                repo: "/home/user/project".into(),
                action: GitAction::CreateBranch {
                    name: "topic".into(),
                },
            },
        ];
        for message in requests {
            let request = ClientControl {
                request_id: 4,
                target: None,
                message,
            };
            assert_eq!(
                decode_client(&encode_client(&request).unwrap()).unwrap(),
                request
            );
        }
        let encoded = serde_json::to_value(GitAction::SwitchBranch {
            name: "main".into(),
        })
        .unwrap();
        assert_eq!(encoded["type"], "switch_branch");

        let replies = [
            ServerMessage::GitBranchesRead {
                listing: GitBranches {
                    branches: vec![
                        GitBranch {
                            name: "main".into(),
                            current: true,
                            upstream: Some("origin/main".into()),
                            remote: false,
                        },
                        GitBranch {
                            name: "origin/main".into(),
                            current: false,
                            upstream: None,
                            remote: true,
                        },
                    ],
                    truncated: false,
                },
            },
            ServerMessage::GitChangesRead {
                changes: Some(GitChanges {
                    repo: "/home/user/project".into(),
                    branch: Some("main".into()),
                    head: Some("a1b2c3d".into()),
                    upstream: Some("origin/main".into()),
                    ahead: 2,
                    behind: 1,
                    changes: Vec::new(),
                    truncated: false,
                }),
            },
        ];
        for message in replies {
            let response = ServerControl {
                request_id: Some(4),
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
            distribution: None,
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
                    layout: LayoutNode::terminal(PaneId::new("pane-1"), surface.id.clone()),
                    previous_layout: None,
                    merge: None,
                }],
            }],
            surfaces: vec![surface.clone()],
            recovery_message: None,
        };
        assert_eq!(workspace.first_surface(), Some(&surface));
    }

    #[test]
    fn surface_environment_comes_from_its_distribution() {
        let mut surface = SurfaceInfo {
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
            distribution: None,
        };
        assert_eq!(
            ContentEnvironment::of_surface(&surface),
            ContentEnvironment::Host
        );
        // A shell in the default distribution names it nowhere but in its launch.
        surface.distribution = Some("Ubuntu".into());
        assert_eq!(
            ContentEnvironment::of_surface(&surface),
            ContentEnvironment::Wsl {
                distribution: "Ubuntu".into()
            }
        );
        let json = serde_json::to_value(&surface).unwrap();
        assert_eq!(json["distribution"], "Ubuntu");
        // Workspaces persisted before the field existed still load.
        let mut old = json;
        old.as_object_mut().unwrap().remove("distribution");
        let old: SurfaceInfo = serde_json::from_value(old).unwrap();
        assert_eq!(old.distribution, None);
        assert_eq!(
            ContentEnvironment::of_surface(&old),
            ContentEnvironment::Host
        );
    }
}

#[cfg(test)]
mod content_tests {
    use super::*;

    fn file(path: &str) -> ContentTab {
        ContentTab::edit(path)
    }

    fn content(files: &[&str], active: Option<&str>) -> FolderContent {
        FolderContent {
            environment: ContentEnvironment::Wsl {
                distribution: "Ubuntu".into(),
            },
            root: "/home/user/project/".into(),
            tabs: files.iter().map(|path| file(path)).collect(),
            active: active.map(file),
        }
    }

    #[test]
    fn content_leaf_roundtrips_and_has_no_surface() {
        let leaf = LayoutNode::Content {
            pane_id: PaneId::new("pane-2"),
            content: PaneContent::Files(content(
                &["/home/user/project/a.rs"],
                Some("/home/user/project/a.rs"),
            )),
        };
        let json = serde_json::to_value(&leaf).unwrap();
        assert_eq!(json["content"]["kind"], "files");
        assert_eq!(json["content"]["root"], "/home/user/project/");
        assert_eq!(serde_json::from_value::<LayoutNode>(json).unwrap(), leaf);
        assert_eq!(leaf.leaf_pane(), Some(&PaneId::new("pane-2")));
        assert_eq!(leaf.surface_id(), None);
        assert!(leaf.has_content());
        if let LayoutNode::Content { content, .. } = &leaf {
            let folder = content.folder().unwrap();
            assert_eq!(folder.folder_name(), "project");
            assert_eq!(folder.environment.to_string(), "WSL: Ubuntu");
            assert!(content.settings().is_none());
        }
    }

    #[test]
    fn a_settings_leaf_carries_only_its_section() {
        let settings = PaneContent::Settings(SettingsContent {
            section: SettingsSection::Terminal,
        });
        let leaf = LayoutNode::Content {
            pane_id: PaneId::new("pane-3"),
            content: settings.clone(),
        };
        let json = serde_json::to_value(&leaf).unwrap();
        assert_eq!(
            json["content"],
            serde_json::json!({"kind": "settings", "section": "terminal"})
        );
        assert_eq!(serde_json::from_value::<LayoutNode>(json).unwrap(), leaf);
        assert!(leaf.has_content() && leaf.surface_id().is_none());
        assert!(settings.validate().is_ok());
        assert!(settings.folder().is_none());
        assert_eq!(settings.settings().unwrap().section.label(), "Terminal");
        // Content without a kind, or of a kind this version does not know, is refused.
        let untagged = serde_json::json!({"environment": {"type": "host"}, "root": "/work"});
        assert!(serde_json::from_value::<PaneContent>(untagged).is_err());
        let unknown = serde_json::json!({"kind": "notes"});
        assert!(serde_json::from_value::<PaneContent>(unknown).is_err());
        let section = serde_json::json!({"kind": "settings", "section": "fonts"});
        assert!(serde_json::from_value::<PaneContent>(section).is_err());
        // An update keeps the kind.
        let files = PaneContent::Files(content(&[], None));
        assert!(!settings.same_kind(&files));
        assert!(settings.same_kind(&PaneContent::Settings(SettingsContent::default())));
    }

    #[test]
    fn the_monitor_section_keeps_the_performance_name() {
        let monitor = PaneContent::Settings(SettingsContent {
            section: SettingsSection::Monitor,
        });
        let saved = serde_json::json!({"kind": "settings", "section": "performance"});
        assert_eq!(serde_json::to_value(&monitor).unwrap(), saved);
        assert_eq!(
            serde_json::from_value::<PaneContent>(saved).unwrap(),
            monitor
        );
        assert_eq!(SettingsSection::Monitor.label(), "Monitor");
    }

    #[test]
    fn a_terminal_view_is_omitted_when_absent_and_shown_only_on_its_side() {
        let bare = LayoutNode::terminal(PaneId::new("pane-1"), SurfaceId::new("surface-1"));
        let json = serde_json::to_value(&bare).unwrap();
        // A terminal without a view is written as protocol 28 wrote it.
        assert_eq!(
            json,
            serde_json::json!({"type": "pane", "pane_id": "pane-1", "surface_id": "surface-1"})
        );
        assert_eq!(serde_json::from_value::<LayoutNode>(json).unwrap(), bare);
        assert!(bare.content().is_none() && bare.shown_content().is_none());

        let files = PaneContent::Files(content(&[], None));
        let mut leaf = LayoutNode::Pane {
            pane_id: PaneId::new("pane-1"),
            surface_id: SurfaceId::new("surface-1"),
            view: Some(PaneView {
                content: files.clone(),
                side: PaneSide::View,
            }),
        };
        let json = serde_json::to_value(&leaf).unwrap();
        assert_eq!(json["view"]["side"], "view");
        assert_eq!(json["view"]["content"]["kind"], "files");
        assert_eq!(serde_json::from_value::<LayoutNode>(json).unwrap(), leaf);
        assert_eq!(leaf.shown_content(), Some(&files));
        // Still a terminal, sized in cells.
        assert!(!leaf.has_content());
        assert_eq!(leaf.surface_id(), Some(&SurfaceId::new("surface-1")));
        if let LayoutNode::Pane {
            view: Some(view), ..
        } = &mut leaf
        {
            view.side = PaneSide::Terminal;
        }
        assert!(leaf.shown_content().is_none());
        assert_eq!(leaf.content(), Some(&files));
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

    #[test]
    fn a_file_tab_in_diff_mode_is_still_the_files_one_tab() {
        let diff = |path: &str| ContentTab::diff(path, GitScope::Staged);
        let mut shown = FolderContent {
            tabs: vec![file("/a"), diff("/home/user/project/src/main.rs")],
            active: Some(diff("/home/user/project/src/main.rs")),
            ..content(&[], None)
        };
        assert!(shown.validate().is_ok());
        assert_eq!(
            shown.files().collect::<Vec<_>>(),
            ["/a", "/home/user/project/src/main.rs"]
        );
        assert_eq!(shown.active_file(), None);
        assert_eq!(
            shown.active_diff(),
            Some(("/home/user/project/src/main.rs", GitScope::Staged))
        );
        let json = serde_json::to_value(&shown).unwrap();
        assert_eq!(
            json["tabs"][0],
            serde_json::json!({"type": "file", "path": "/a"})
        );
        assert_eq!(json["tabs"][1]["type"], "file");
        assert_eq!(json["active"]["diff"], "staged");
        assert_eq!(
            serde_json::from_value::<FolderContent>(json).unwrap(),
            shown
        );
        // The same file in both modes is one file listed twice.
        let twice = FolderContent {
            tabs: vec![file("/a"), diff("/a")],
            ..content(&[], None)
        };
        assert!(twice.validate().is_err());
        // The active tab names the mode its file's tab is in.
        let stray = FolderContent {
            active: Some(file("/home/user/project/src/main.rs")),
            ..shown.clone()
        };
        assert!(stray.validate().is_err());
        // Showing a mode switches the file's tab in place.
        shown.show(file("/home/user/project/src/main.rs"));
        assert_eq!(shown.tabs.len(), 2);
        assert_eq!(shown.active_file(), Some("/home/user/project/src/main.rs"));
        shown.show(diff("/b"));
        assert_eq!(shown.files().collect::<Vec<_>>().len(), 3);
        assert!(shown.validate().is_ok());
        // A folder with nothing open omits both fields.
        let folder = serde_json::to_value(content(&[], None)).unwrap();
        assert!(folder.get("tabs").is_none() && folder.get("active").is_none());
    }
}
