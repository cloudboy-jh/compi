//! Content panes: layout leaves that show files instead of a terminal. They tile,
//! float, zoom and move like terminal panes but have no surface, so they never
//! attach, resize a PTY, or receive terminal input. Without an active tab a pane
//! browses its folder; with one it shows its open tabs (files, and diffs of changed
//! files) above a GPUI Kit code editor or a diff, with the folder (or, in a Git work
//! tree, its changes) as an optional sidebar. Folders are listed and files read
//! and saved through the daemon, so host and WSL folders behave the same. Shown files
//! are compared with the disk every few seconds, so outside edits reload or warn, and
//! a save never replaces bytes the editor has not seen. Dirty editors are backed up
//! (see `unsaved`) and never dropped without asking.
use super::browser::MediaSource;
use super::changes::{ChangesSettings, DiffTab, RepoStatus, SidebarMode};
use super::git_gutter::GitGutter;
use super::tree::{Entry, FileTree, filter_rows};
use super::tree_ui::tree_entry_icon;
use super::unsaved::{self, Backup, Backups, SaveTicket};
use super::*;
use crate::gui::controls::{self, ChipTone, IconName, icon_button, text_button};
use compi_protocol::{
    ContentEnvironment, ContentTab, DirectoryEntry, ErrorCode, FileContent, FileKind, FileState,
    FileVersion, FolderContent, GitChanges, GitScope, MAX_CONTENT_FILE_BYTES, PaneContent,
    PaneSide, PaneView, ServerGeneration, ServerId,
};
use gpui::Entity;
use gpui_component::highlighter::HighlightTheme;
use gpui_component::input::{
    Editor, EditorState, Input, InputEvent, InputState, Position, Rope, RopeExt,
};
use gpui_component::theme::{Theme, ThemeMode, ThemeTokens};

mod image_file;
mod large_text;
pub(in crate::gui) mod notes;
use image_file::{ImageFile, SvgView};
pub(in crate::gui) use image_file::{LoadedImage, is_svg};
pub(in crate::gui) use large_text::Part as LargeTextPart;
use large_text::{LargeText, Step};

/// Width of the folder sidebar beside an open tab.
pub(super) const SIDEBAR_WIDTH: f32 = 220.0;
pub(super) const FILE_TAB_HEIGHT: f32 = 30.0;
/// Folder rows: the left inset and the indent per level.
pub(super) const TREE_INSET: f32 = 8.0;
pub(super) const TREE_INDENT: f32 = 14.0;
/// How often a shown file is compared with the disk, and a shown pane's folder read
/// again for its Git changes.
pub(super) const CHECK_INTERVAL: Duration = Duration::from_secs(2);
const WATCH_TICK: Duration = Duration::from_millis(250);
/// Larger files open as plain text: a 900 KB Rust file took about twice as long per
/// keystroke highlighted as plain, while 334 KB showed no measurable cost.
pub(super) const HIGHLIGHT_LIMIT: usize = 512 * 1024;

/// Why a save failed.
pub(in crate::gui) enum SaveFailure {
    /// The file is not what the save expected: it changed or was deleted on disk, or a
    /// new file's path already exists.
    Conflict,
    Other(String),
}

/// What a save must find on disk.
enum Expect {
    /// These bytes, or with `None` no file: Save as and re-creating a deleted file.
    Exactly(Option<FileVersion>),
    /// Overwrite: whatever is there when the save starts, checked against this version.
    Whatever(FileVersion),
}

/// Client-local state of content panes. What a pane shows (its folder, open tabs and
/// active tab) lives in the daemon's `PaneContent`; this keeps what only this window
/// needs: keyboard focus, folder trees, an editor per open file, the folder's Git changes
/// and the diffs of diff tabs.
#[derive(Default)]
pub(in crate::gui) struct ContentUi {
    pub(super) panes: HashMap<PaneId, PaneUi>,
    /// The pane whose editor the user left with Escape or by clicking its folder;
    /// the pane keeps the keyboard until the editor is entered again.
    released: Option<PaneId>,
    /// A line to reveal in a file whose pane `OpenContent` is still creating.
    pending_reveal: Option<PendingReveal>,
    /// Some file was read and needs an editor, or an editor needs the text read again,
    /// which only a window can do.
    unbuilt: bool,
    /// Where this instance keeps unsaved edits; `None` without a data directory.
    backups: Option<Backups>,
    /// The daemon run the panes were last pruned against. Panes gone within one run
    /// were removed, so their backups go too; after a restart they are kept.
    server: Option<(ServerId, ServerGeneration)>,
    /// How every Changes list and diff in this window shows changes.
    pub(super) changes: ChangesSettings,
    /// A listing the Changes command just read, for the pane `OpenContent` is creating.
    pub(super) changes_seed: Option<(ContentEnvironment, GitChanges)>,
    /// A diff asked for in a pane this window has not drawn yet: `open_diff`'s arguments.
    pub(super) pending_diff: Option<(PaneId, String, Option<GitScope>, Option<u32>)>,
}

impl ContentUi {
    /// `server` is the daemon run the window's first workspace came from.
    pub(in crate::gui) fn new(
        backups: Option<Backups>,
        server: (ServerId, ServerGeneration),
    ) -> Self {
        Self {
            backups,
            server: Some(server),
            ..Self::default()
        }
    }

    pub(super) fn backups(&self) -> Option<&Backups> {
        self.backups.as_ref()
    }
}

struct PendingReveal {
    environment: ContentEnvironment,
    path: String,
    line: u32,
}

pub(super) struct PaneUi {
    /// Keyboard focus of the pane itself; its editor and search field sit inside it.
    pub(super) focus: FocusHandle,
    pub(super) environment: ContentEnvironment,
    tree: FileTree,
    tree_scroll: ScrollHandle,
    /// Narrows the folder to names containing this, ignoring case. Only folders already
    /// listed are searched; collapsed ones are not opened for it.
    filter: String,
    /// The sidebar shows beside an open tab.
    pub(super) sidebar: bool,
    /// What the sidebar, or the folder view, lists.
    pub(super) mode: SidebarMode,
    /// The latest save failure or close warning, and the file it is about.
    notice: Option<(String, String)>,
    files: HashMap<String, OpenFile>,
    /// The folder's Git changes.
    pub(super) repo: RepoStatus,
    /// The diff of each diff tab drawn so far.
    pub(super) diffs: HashMap<ContentTab, DiffTab>,
    /// A new-side line to reveal in a diff tab once its diff exists.
    pub(super) diff_reveal: Option<(ContentTab, u32)>,
    pub(super) notes: notes::NotesUi,
    /// SVG files shown in View mode, drawn from their editors' text.
    svg_views: HashMap<String, SvgView>,
}

struct OpenFile {
    body: FileBody,
    /// 1-based line to put the cursor on once the editor shows.
    reveal: Option<u32>,
}

enum FileBody {
    Loading,
    Failed(String),
    /// Read, with the backup of its unsaved edits if it has one, waiting for the next
    /// frame to build its editor.
    Read(FreshText, Option<Box<Backup>>),
    Ready(Box<FileEditor>),
    /// Shown as a placeholder instead of an editor.
    Unopenable {
        reason: Unopenable,
        len: u64,
        kind: FileKind,
    },
    /// An image the editor cannot open, shown as one.
    Image(Box<ImageFile>),
    /// Text too large for the editor, shown read-only a part at a time.
    LargeText(Box<LargeText>),
    /// A PDF, audio or video file, shown by a system web view page.
    Media(FileKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unopenable {
    TooLarge,
    Binary,
}

fn image_format_name(format: compi_protocol::ImageFormat) -> &'static str {
    use compi_protocol::ImageFormat;
    match format {
        ImageFormat::Png => "PNG",
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Gif => "GIF",
        ImageFormat::Webp => "WebP",
        ImageFormat::Bmp => "BMP",
        ImageFormat::Ico => "ICO",
        ImageFormat::Svg => "SVG",
    }
}

/// What a file the editor cannot open is, in words. Images come here only when larger
/// than the image limit.
fn describe_unopenable(reason: Unopenable, kind: FileKind) -> String {
    match kind {
        FileKind::Image { format } => format!(
            "{} image larger than the {} MiB image limit",
            image_format_name(format),
            crate::image_input::MAX_ENCODED_BYTES / (1024 * 1024)
        ),
        FileKind::Pdf => "PDF document".into(),
        FileKind::Audio => "Audio file".into(),
        FileKind::Video => "Video file".into(),
        FileKind::Text if reason == Unopenable::TooLarge => format!(
            "Text larger than the {} MiB editor limit",
            MAX_CONTENT_FILE_BYTES / (1024 * 1024)
        ),
        FileKind::Text | FileKind::Other => "Not a text file".into(),
    }
}

pub(super) struct FileEditor {
    pub(super) state: Entity<EditorState>,
    _changes: Subscription,
    /// What the file on disk was when last read or saved; a save must still match it.
    version: FileVersion,
    /// The text last read or saved. The file is dirty while the editor differs.
    saved: Rope,
    pub(super) dirty: bool,
    /// The text being saved, until the daemon answers.
    saving: Option<Rope>,
    /// How the file ended its lines when read; saves keep it.
    endings: LineEndings,
    /// Don't save was chosen: the edits are about to go with the editor, unasked and
    /// without a backup. Typing again takes it back.
    discarded: bool,
    /// When the edits are next written to their backup.
    backup_due: Option<Instant>,
    /// Prompts waiting for the save under way.
    save_tickets: Vec<SaveTicket>,
    /// The edits came from a backup written at this Unix time.
    restored: Option<u64>,
    /// Whether a save can replace the file; a read-only file opens a read-only editor.
    writable: bool,
    /// What the last check found on disk.
    disk: OnDisk,
    banner: Option<Banner>,
    /// Reload was pressed once with unsaved changes.
    reload_armed: bool,
    /// A read of the file is under way to replace the editor's text.
    reload: Option<Reload>,
    /// The text that read found, until a window puts it in the editor.
    fresh: Option<FreshText>,
    /// A check of the file against the disk is under way.
    checking: bool,
    last_check: Instant,
    /// The editor was visible, in an active window, at the last watch tick.
    shown: bool,
    save_as: Option<SaveAs>,
    /// Saved as this file; the editor moves to it once the pane lists it.
    moving_to: Option<String>,
    /// Marks where the text differs from the Git index.
    pub(super) gutter: GitGutter,
}

/// A file on disk compared with the version its editor holds.
#[derive(Clone, Debug, PartialEq, Eq)]
enum OnDisk {
    Current,
    /// Changed to this version, which the user chose not to load.
    Changed(FileVersion),
    Missing,
}

/// A file notice with actions, shown above its editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Banner {
    /// A save found the file changed on disk: reload, overwrite, or save as.
    Conflict,
    /// Changed on disk while the editor has unsaved changes: reload or keep mine.
    Changed,
    Deleted,
    /// Saving a read-only file: save as instead.
    ReadOnly,
}

impl Banner {
    /// Banners that a matching disk resolves; `ReadOnly` lasts while the file is.
    fn about_disk(self) -> bool {
        self != Self::ReadOnly
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reload {
    /// The file changed while the editor was clean; edits made meanwhile win.
    Quiet,
    /// The user discards their edits.
    Discard,
}

struct FreshText {
    text: String,
    version: FileVersion,
    writable: bool,
}

/// The inline path field of Save as.
struct SaveAs {
    input: Entity<InputState>,
    _events: Subscription,
    error: Option<String>,
}

/// What a check of the disk asks of an editor.
#[derive(Debug, PartialEq, Eq)]
enum CheckAction {
    Nothing,
    /// The disk matches again: drop disk banners.
    Settled,
    /// Read the file again into the clean editor.
    Reload,
    Show(Banner),
}

/// How the last known disk state `previous` and a fresh check combine, for an editor
/// that is `clean` (no unsaved changes, no save under way).
fn after_check(previous: &OnDisk, state: &FileState, clean: bool) -> (OnDisk, CheckAction) {
    match state {
        FileState::Unchanged { .. } if *previous == OnDisk::Current => {
            (OnDisk::Current, CheckAction::Nothing)
        }
        FileState::Unchanged { .. } => (OnDisk::Current, CheckAction::Settled),
        FileState::Changed { .. } if clean => (previous.clone(), CheckAction::Reload),
        FileState::Changed { version, .. } => {
            let changed = OnDisk::Changed(version.clone());
            if *previous == changed {
                (changed, CheckAction::Nothing)
            } else {
                (changed, CheckAction::Show(Banner::Changed))
            }
        }
        FileState::Missing if *previous == OnDisk::Missing => {
            (OnDisk::Missing, CheckAction::Nothing)
        }
        FileState::Missing => (OnDisk::Missing, CheckAction::Show(Banner::Deleted)),
    }
}

/// The version a plain save must find: a deleted file is created again.
fn save_expected(disk: &OnDisk, version: &FileVersion) -> Option<FileVersion> {
    (*disk != OnDisk::Missing).then(|| version.clone())
}

/// The byte offset of `position` in `text`, clamped to its last line and that line's end.
fn clamped_offset(text: &Rope, position: Position) -> usize {
    let line = position.line.min(text.lines_len().saturating_sub(1) as u32);
    text.position_to_offset(&Position::new(line, position.character))
}

/// A byte count as people read file sizes: `812 B`, `24 KB`, `2.4 MB`.
pub(super) fn human_size(len: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if len < 1024 {
        return format!("{len} B");
    }
    let mut size = len as f64 / 1024.0;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    if size < 10.0 {
        format!("{size:.1} {}", UNITS[unit])
    } else {
        format!("{size:.0} {}", UNITS[unit])
    }
}

/// Where content lives, for captions: `host` or `WSL: Ubuntu` on this machine; through
/// SSH, its host name first.
fn environment_label(ssh_host: Option<&str>, environment: &ContentEnvironment) -> String {
    match (ssh_host, environment) {
        (None, environment) => environment.to_string(),
        (Some(host), ContentEnvironment::Host) => format!("SSH: {host}"),
        (Some(host), environment) => format!("SSH: {host}, {environment}"),
    }
}

/// This machine's home folder, which a local host folder shows as `~`.
static CLIENT_HOME: LazyLock<Option<String>> = LazyLock::new(|| {
    std::env::home_dir().and_then(|home| home.into_os_string().into_string().ok())
});

/// The Unix home folder holding `path`, `/home/<user>` or `/root`. WSL and remote homes
/// are not known here, so a folder under one shows it as `~`.
fn unix_home(path: &str) -> Option<&str> {
    if let Some(rest) = path.strip_prefix("/home/") {
        let user = rest.split('/').next().filter(|user| !user.is_empty())?;
        return Some(&path[.."/home/".len() + user.len()]);
    }
    (path == "/root" || path.starts_with("/root/")).then_some("/root")
}

/// What follows `home` in `path` when `path` is `home` or inside it. Drive paths ignore
/// case, as Windows does.
fn strip_home<'a>(path: &'a str, home: &str) -> Option<&'a str> {
    let head = path.get(..home.len())?;
    let same = if home.contains(':') {
        head.eq_ignore_ascii_case(home)
    } else {
        head == home
    };
    let rest = &path[home.len()..];
    (same && (rest.is_empty() || rest.starts_with(['/', '\\']))).then_some(rest)
}

/// `path` for a caption: `home` as `~`, and at most `max` characters, eliding middle
/// folders so the start and the innermost folders stay readable.
fn caption_path(path: &str, home: Option<&str>, max: usize) -> String {
    let separator = if path.contains('\\') && !path.contains('/') {
        '\\'
    } else {
        '/'
    };
    // A trailing separator goes, unless it is the root itself: `/` or `C:\`.
    let path = match path.trim_end_matches(['/', '\\']) {
        trimmed if trimmed.is_empty() || trimmed.ends_with(':') => path,
        trimmed => trimmed,
    };
    let home = home
        .map(|home| home.trim_end_matches(['/', '\\']))
        .filter(|home| !home.is_empty());
    let path = match home.and_then(|home| strip_home(path, home)) {
        Some(rest) => format!("~{rest}"),
        None => path.to_owned(),
    };
    let count = path.chars().count();
    if count <= max {
        return path;
    }
    let first = path.split(separator).next().unwrap_or_default();
    // The first component, a separator and the ellipsis.
    let kept = first.chars().count() + 2;
    let mut tail = path.len();
    for (index, _) in path.rmatch_indices(separator) {
        if index <= first.len() || kept + path[index..].chars().count() > max {
            break;
        }
        tail = index;
    }
    if tail < path.len() {
        return format!("{first}{separator}…{}", &path[tail..]);
    }
    // Not even the innermost name fits: elide characters instead.
    let head = max.saturating_sub(1) / 2;
    let tail = max.saturating_sub(1) - head;
    let at = |chars: usize| {
        path.char_indices()
            .nth(chars)
            .map_or(path.len(), |(at, _)| at)
    };
    format!("{}…{}", &path[..at(head)], &path[at(count - tail)..])
}

/// The editor inserts `\n` for Enter, so a file whose every line ended in `\r\n` would
/// gain bare `\n` lines on save. Mixed files are saved as typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum LineEndings {
    Lf,
    Crlf,
}

impl LineEndings {
    fn of(text: &str) -> Self {
        let bytes = text.as_bytes();
        let mut newlines = bytes
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte == b'\n')
            .peekable();
        if newlines.peek().is_some()
            && newlines.all(|(index, _)| index > 0 && bytes[index - 1] == b'\r')
        {
            Self::Crlf
        } else {
            Self::Lf
        }
    }

    /// `text` as it should be written: every bare `\n` becomes `\r\n` in a CRLF file.
    fn apply(self, text: String) -> String {
        if self == Self::Lf || !text.contains('\n') {
            return text;
        }
        let mut out = String::with_capacity(text.len() + text.len() / 32);
        let mut previous = '\0';
        for c in text.chars() {
            if c == '\n' && previous != '\r' {
                out.push('\r');
            }
            out.push(c);
            previous = c;
        }
        out
    }
}

/// The GPUI Kit colors this window last installed; see `sync_editor_theme`.
#[derive(PartialEq)]
struct AppliedEditorTheme {
    colors: ThemeColors,
    dark: bool,
    /// The pane material is solid, so the editor gutter can be too.
    opaque: bool,
    ui_font: &'static str,
}

impl gpui::Global for AppliedEditorTheme {}

impl PaneUi {
    pub(super) fn new(
        pane_id: &PaneId,
        environment: ContentEnvironment,
        root: String,
        cx: &mut App,
    ) -> Self {
        Self {
            focus: cx.focus_handle(),
            environment,
            tree: FileTree::new(pane_id.clone(), root.clone()),
            tree_scroll: ScrollHandle::new(),
            filter: String::new(),
            sidebar: false,
            mode: SidebarMode::Files,
            notice: None,
            files: HashMap::new(),
            repo: RepoStatus::new(root),
            diffs: HashMap::new(),
            diff_reveal: None,
            notes: notes::NotesUi::default(),
            svg_views: HashMap::new(),
        }
    }

    fn root(&self) -> &str {
        &self.tree.rows[0].path
    }

    /// Indices of the tree rows the folder shows: every row below the root, which the
    /// header names, or with a filter the matching names and the folders above them.
    fn visible_rows(&self) -> Vec<usize> {
        let mut rows = filter_rows(&self.tree.rows, &self.filter);
        rows.retain(|&index| index > 0);
        rows
    }

    /// The folder selected in the tree, when its row shows.
    fn selected_folder(&self) -> Option<String> {
        let row = self.tree.rows.get(self.tree.selected)?;
        (row.directory && self.visible_rows().contains(&self.tree.selected))
            .then(|| row.path.clone())
    }

    fn scroll_to_selected(&self) {
        if let Some(position) = self
            .visible_rows()
            .iter()
            .position(|&index| index == self.tree.selected)
        {
            self.tree_scroll.scroll_to_item(position);
        }
    }

    /// Whether `focused` is this pane, something inside it, or one of its editors,
    /// shown or not.
    fn owns(&self, focused: &FocusHandle, window: &Window, cx: &App) -> bool {
        *focused == self.focus
            || self.focus.contains_focused(window, cx)
            || self.files.values().any(|file| match &file.body {
                FileBody::Ready(editor) => editor.state.focus_handle(cx) == *focused,
                _ => false,
            })
    }

    pub(super) fn editor(&self, path: &str) -> Option<&FileEditor> {
        match &self.files.get(path)?.body {
            FileBody::Ready(editor) => Some(editor),
            _ => None,
        }
    }

    pub(super) fn editor_mut(&mut self, path: &str) -> Option<&mut FileEditor> {
        match &mut self.files.get_mut(path)?.body {
            FileBody::Ready(editor) => Some(editor),
            _ => None,
        }
    }

    /// Whether `path` shows as a preview (an image, media, a large text file a part at a
    /// time, or a placeholder) rather than in the editor.
    pub(super) fn previews(&self, path: &str) -> bool {
        matches!(
            self.files.get(path).map(|file| &file.body),
            Some(
                FileBody::Image(_)
                    | FileBody::LargeText(_)
                    | FileBody::Media(_)
                    | FileBody::Unopenable { .. }
            )
        )
    }
}

/// The terminal or content leaf of `pane`, in whichever tab holds it.
pub(in crate::gui) fn find_leaf<'a>(
    workspace: &'a WorkspaceSnapshot,
    pane: &PaneId,
) -> Option<&'a LayoutNode> {
    workspace
        .sessions
        .iter()
        .flat_map(|session| &session.tabs)
        .find_map(|tab| leaf_node(&tab.layout, pane))
}

/// What `pane` holds besides a shell: a content pane's content, or the view a terminal
/// pane carries, shown or not. Client state of content stays keyed by the pane either
/// way, so it survives flipping a view to its shell and back.
pub(in crate::gui) fn find_pane_content<'a>(
    workspace: &'a WorkspaceSnapshot,
    pane: &PaneId,
) -> Option<&'a PaneContent> {
    find_leaf(workspace, pane)?.content()
}

/// What `pane` draws instead of a terminal right now: a content pane's content, or a
/// terminal pane's view while it shows its view side.
pub(in crate::gui) fn find_shown_content<'a>(
    workspace: &'a WorkspaceSnapshot,
    pane: &PaneId,
) -> Option<&'a PaneContent> {
    find_leaf(workspace, pane)?.shown_content()
}

/// The folder and files `pane` holds, if it is a content pane or terminal view showing
/// files, shown or not.
pub(in crate::gui) fn find_content<'a>(
    workspace: &'a WorkspaceSnapshot,
    pane: &PaneId,
) -> Option<&'a FolderContent> {
    find_pane_content(workspace, pane)?.folder()
}

/// The panes of `node` holding files: content panes, and terminal panes whose view
/// shows files on either side.
pub(super) fn content_leaves<'a>(
    node: &'a LayoutNode,
    visit: &mut impl FnMut(&'a PaneId, &'a FolderContent),
) {
    match node {
        LayoutNode::Split { first, second, .. } => {
            content_leaves(first, visit);
            content_leaves(second, visit);
        }
        leaf => {
            if let (Some(pane_id), Some(PaneContent::Files(folder))) =
                (leaf.leaf_pane(), leaf.content())
            {
                visit(pane_id, folder);
            }
        }
    }
}

/// The content pane in `layout` to show `file` (or, without one, the folder `root`) in:
/// one in `environment` whose root holds the file or that has it open already,
/// preferring `beside` itself, then one with the file open, then the deepest root. A
/// terminal's view stays with its shell.
pub(super) fn reusable_content<'a>(
    layout: &'a LayoutNode,
    beside: &PaneId,
    environment: &ContentEnvironment,
    root: Option<&str>,
    file: Option<&str>,
) -> Option<(&'a PaneId, &'a FolderContent)> {
    let mut best: Option<((bool, bool, usize), &PaneId, &FolderContent)> = None;
    content_leaves(layout, &mut |pane_id, content| {
        if &content.environment != environment
            || leaf_node(layout, pane_id).is_some_and(|leaf| leaf.surface_id().is_some())
        {
            return;
        }
        let rank = match (file, root) {
            (Some(file), _) => {
                let open = content.files().any(|open| open == file);
                (open || root_contains(&content.root, file)).then_some((open, content.root.len()))
            }
            (None, Some(root)) => same_folder(&content.root, root).then_some((true, 0)),
            (None, None) => None,
        };
        let Some((open, depth)) = rank else {
            return;
        };
        let rank = (pane_id == beside, open, depth);
        if best.as_ref().is_none_or(|(current, ..)| rank > *current) {
            best = Some((rank, pane_id, content));
        }
    });
    best.map(|(_, pane_id, content)| (pane_id, content))
}

/// Whether `path` lies below the folder `root`.
pub(super) fn root_contains(root: &str, path: &str) -> bool {
    path.strip_prefix(root.trim_end_matches(['/', '\\']))
        .is_some_and(|rest| rest.len() > 1 && rest.starts_with(['/', '\\']))
}

pub(super) fn same_folder(left: &str, right: &str) -> bool {
    left.trim_end_matches(['/', '\\']) == right.trim_end_matches(['/', '\\'])
}

/// The folder holding `path`: `/` and `C:\` keep their separator.
pub(super) fn parent_folder(path: &str) -> String {
    match path.rfind(['/', '\\']) {
        Some(0) => path[..1].to_owned(),
        Some(index) if path[..index].ends_with(':') => path[..=index].to_owned(),
        Some(index) => path[..index].to_owned(),
        None => path.to_owned(),
    }
}

pub(super) fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// The highlighter language for `path`, by file name and then extension.
pub(super) fn language_for(path: &str) -> &'static str {
    const NAMES: &[(&str, &str)] = &[
        ("Cargo.lock", "toml"),
        (".bashrc", "bash"),
        (".bash_profile", "bash"),
        (".profile", "bash"),
        (".zshrc", "bash"),
    ];
    const EXTENSIONS: &[(&str, &str)] = &[
        ("rs", "rust"),
        ("toml", "toml"),
        ("json", "json"),
        ("jsonc", "json"),
        ("md", "markdown"),
        ("markdown", "markdown"),
        ("py", "python"),
        ("pyi", "python"),
        ("js", "javascript"),
        ("mjs", "javascript"),
        ("cjs", "javascript"),
        ("jsx", "javascript"),
        ("ts", "typescript"),
        ("mts", "typescript"),
        ("cts", "typescript"),
        ("tsx", "tsx"),
        ("go", "go"),
        ("c", "c"),
        ("h", "c"),
        ("cc", "cpp"),
        ("cpp", "cpp"),
        ("cxx", "cpp"),
        ("hh", "cpp"),
        ("hpp", "cpp"),
        ("hxx", "cpp"),
        ("css", "css"),
        ("scss", "css"),
        ("html", "html"),
        ("htm", "html"),
        ("yaml", "yaml"),
        ("yml", "yaml"),
        ("sh", "bash"),
        ("bash", "bash"),
        ("zsh", "bash"),
    ];
    let name = file_name(path);
    NAMES
        .iter()
        .find(|(known, _)| *known == name)
        .or_else(|| {
            let (_, extension) = name.rsplit_once('.')?;
            EXTENSIONS
                .iter()
                .find(|(known, _)| known.eq_ignore_ascii_case(extension))
        })
        .map_or("text", |(_, language)| language)
}

fn is_save_key(key: &Keystroke) -> bool {
    let modifiers = &key.modifiers;
    let primary = if cfg!(target_os = "macos") {
        modifiers.platform && !modifiers.control
    } else {
        modifiers.control && !modifiers.platform
    };
    primary && !modifiers.shift && !modifiers.alt && key.key.eq_ignore_ascii_case("s")
}

/// Puts the cursor on 1-based `line` and scrolls it to the middle of the laid-out view,
/// as editors do when jumping to a location.
pub(super) fn reveal_in_editor(
    state: &mut EditorState,
    line: u32,
    window: &mut Window,
    cx: &mut Context<EditorState>,
) {
    let row = line.saturating_sub(1);
    state.set_cursor_position(Position::new(row, 0), window, cx);
    if let (Some(height), Some(visible)) = (state.line_height(), state.visible_row_range()) {
        let top = (row as usize).saturating_sub(visible.len() / 2);
        let offset = state.scroll_offset();
        state.set_scroll_offset(point(offset.x, -(height * top as f32)), cx);
    }
}

/// The highlighter language for a file of `len` bytes at `path`.
fn editor_language(path: &str, len: usize) -> &'static str {
    if len > HIGHLIGHT_LIMIT {
        "text"
    } else {
        language_for(path)
    }
}

/// Marks the editor of `pane_id` dirty, or clean again, as its text changes.
fn watch_editor(
    pane_id: &PaneId,
    state: &Entity<EditorState>,
    cx: &mut Context<CompiApp>,
) -> Subscription {
    let pane_id = pane_id.clone();
    cx.subscribe(state, move |this, editor, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change) {
            this.editor_changed(&pane_id, &editor, cx);
        }
    })
}

/// Enter in the Save as field of `path` in `pane_id` saves there.
fn save_as_events(
    pane_id: &PaneId,
    path: &str,
    input: &Entity<InputState>,
    cx: &mut Context<CompiApp>,
) -> Subscription {
    let (pane, file) = (pane_id.clone(), path.to_owned());
    cx.subscribe(input, move |this, _, event: &InputEvent, cx| {
        if matches!(event, InputEvent::PressEnter { .. }) {
            this.submit_save_as(&pane, &file, cx);
            cx.notify();
        }
    })
}

/// The editor of a file just read, holding the edits of `restore` if it has a backup.
fn file_editor(
    pane_id: &PaneId,
    path: &str,
    read: FreshText,
    restore: Option<Box<Backup>>,
    window: &mut Window,
    cx: &mut Context<CompiApp>,
) -> FileEditor {
    let FreshText {
        text,
        version,
        writable,
    } = read;
    let endings = LineEndings::of(&text);
    let state = cx.new(|cx| {
        EditorState::new(window, cx)
            .language(editor_language(path, text.len()))
            .soft_wrap(false)
            .default_value(text)
    });
    let saved = state.read(cx).text().clone();
    let changes = watch_editor(pane_id, &state, cx);
    let mut editor = FileEditor {
        state,
        _changes: changes,
        version,
        saved,
        dirty: false,
        saving: None,
        endings,
        discarded: false,
        backup_due: None,
        save_tickets: Vec::new(),
        restored: None,
        writable,
        disk: OnDisk::Current,
        banner: None,
        reload_armed: false,
        reload: None,
        fresh: None,
        checking: false,
        last_check: Instant::now(),
        // Editors are built for shown files, which were just read.
        shown: true,
        save_as: None,
        moving_to: None,
        gutter: GitGutter::default(),
    };
    if let Some(backup) = restore.map(|backup| *backup) {
        editor
            .state
            .update(cx, |state, cx| state.set_value(backup.text, window, cx));
        editor.dirty = editor.state.read(cx).text() != &editor.saved;
        if editor.dirty {
            editor.endings = backup.endings;
            editor.restored = Some(backup.saved_at);
            if unsaved::restore_against(&backup.base, &editor.version) == unsaved::Restore::Changed
            {
                // As if the file changed while editing: saves must still match the
                // version the edits started from, and the banner offers the disk's.
                editor.disk = OnDisk::Changed(std::mem::replace(&mut editor.version, backup.base));
                editor.banner = Some(Banner::Changed);
            }
        }
    }
    editor
}

/// Puts text read again into `editor`, keeping the cursor's line and column (clamped)
/// and the scroll position.
fn apply_fresh(
    editor: &mut FileEditor,
    path: &str,
    fresh: FreshText,
    window: &mut Window,
    cx: &mut App,
) {
    let language = editor_language(path, fresh.text.len());
    editor.endings = LineEndings::of(&fresh.text);
    editor.state.update(cx, |state, cx| {
        let cursor = state.cursor_position();
        let scroll = state.scroll_offset();
        if state.language_name() != language {
            state.set_highlighter(language, cx);
        }
        state.set_value(fresh.text, window, cx);
        let offset = clamped_offset(state.text(), cursor);
        state.set_selected_range(offset..offset, cx);
        state.set_scroll_offset(scroll, cx);
    });
    editor.saved = editor.state.read(cx).text().clone();
    editor.version = fresh.version;
    editor.writable = fresh.writable;
    editor.dirty = false;
    editor.discarded = false;
    editor.backup_due = None;
    editor.restored = None;
    editor.reload_armed = false;
    editor.disk = OnDisk::Current;
    editor.banner = editor
        .banner
        .filter(|banner| !banner.about_disk() && !editor.writable);
    // `set_value` sends no change event.
    editor.gutter.text_replaced();
}

/// Writes the backups of `ui`'s editors whose edits are due by `due_by`, or with `None`
/// of every editor whose backup is behind.
fn write_pane_backups(backups: &Backups, ui: &mut PaneUi, due_by: Option<Instant>, cx: &App) {
    for (path, file) in &mut ui.files {
        let FileBody::Ready(editor) = &mut file.body else {
            continue;
        };
        // A file being saved as another is backed up under its new path once it moves.
        if editor.moving_to.is_some()
            || !editor
                .backup_due
                .is_some_and(|due| due_by.is_none_or(|by| due <= by))
        {
            continue;
        }
        editor.backup_due = None;
        if !editor.dirty || editor.discarded {
            continue;
        }
        let text = editor.state.read(cx).text().clone();
        // Larger text cannot be saved, nor restored into an editor.
        if text.len() <= MAX_CONTENT_FILE_BYTES {
            backups.write(
                &ui.environment,
                path,
                editor.version.clone(),
                editor.endings,
                text,
            );
        }
    }
}

impl CompiApp {
    /// GPUI Kit keeps one theme for the whole app while every Compi window has its own,
    /// so each window installs its colors before it draws. Other windows are not
    /// refreshed; they install theirs when they next draw.
    pub(super) fn sync_editor_theme(&self, cx: &mut App) {
        let applied = AppliedEditorTheme {
            colors: *self.colors(),
            dark: self.theme.is_dark(),
            opaque: self.effective_background_opacity() >= 1.0,
            ui_font: self.ui_font.family(),
        };
        if cx.try_global::<AppliedEditorTheme>() == Some(&applied) {
            return;
        }
        let colors = applied.colors;
        let theme = Theme::global_mut(cx);
        theme.mode = if applied.dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        };
        theme.font_family = applied.ui_font.into();
        theme.font_size = px(UI_SMALL_TEXT_SIZE);
        theme.background = color(colors.background);
        theme.foreground = color(colors.foreground);
        theme.border = color(colors.border);
        theme.selection = color(colors.selection);
        theme.caret = color(colors.accent);
        theme.accent = color(colors.surface_hover);
        theme.accent_foreground = color(colors.foreground);
        theme.muted = color(colors.surface);
        // Line numbers paint in this; reduced, they recede behind the code.
        theme.muted_foreground = color(colors.muted).opacity(0.7);
        theme.list_hover = color(colors.surface_hover);
        theme.secondary = color(colors.surface);
        theme.secondary_hover = color(colors.surface_hover);
        theme.secondary_foreground = color(colors.foreground);
        theme.scrollbar = gpui::transparent_black();
        theme.scrollbar_thumb = color(colors.muted).opacity(0.4);
        theme.scrollbar_thumb_hover = color(colors.muted);
        controls::sync_theme(theme, &colors);
        theme.link = color(colors.accent);
        theme.table_head = color(colors.surface_hover);
        theme.table_head_foreground = color(colors.foreground);
        theme.tokens = ThemeTokens::from(&theme.colors);
        let mut highlight = if applied.dark {
            HighlightTheme::default_dark()
        } else {
            HighlightTheme::default_light()
        }
        .as_ref()
        .clone();
        // The code area is transparent, so the pane material shows through. The gutter
        // is painted over horizontally scrolled text, and without its own color GPUI Kit
        // reads the transparent editor background as unset and fills the gutter with its
        // surface color, a lighter strip. Over a solid pane the gutter takes the pane's
        // color, matching the code and still covering scrolled text; over a translucent
        // pane it stays clear.
        highlight.style.editor_background = Some(gpui::transparent_black());
        highlight.style.editor_gutter_background = Some(if applied.opaque {
            material_color(colors.background, 1.0)
        } else {
            gpui::transparent_black()
        });
        highlight.style.editor_foreground = Some(color(colors.foreground));
        highlight.style.editor_line_number = Some(theme.muted_foreground);
        highlight.style.editor_active_line_number = Some(color(colors.foreground));
        // A faint wash, not a block: the current line's number already stands out.
        highlight.style.editor_active_line = Some(color(colors.foreground).opacity(0.05));
        theme.highlight_theme = Arc::new(highlight);
        Theme::sync_base(cx);
        controls::install_colors(colors, cx);
        cx.set_global(applied);
    }

    /// Panes this window draws content in: tiled in the visible tab, or floating;
    /// content panes, and terminal panes showing their view.
    pub(super) fn shown_content_panes(&self) -> impl Iterator<Item = &PaneId> {
        let workspace = self.workspace.as_ref();
        self.visible_layout()
            .into_iter()
            .flat_map(|layout| &layout.panes)
            .chain(self.float_layouts.iter().map(|float| &float.pane))
            .filter(move |pane| {
                pane.surface_id.is_none()
                    || workspace.is_some_and(|workspace| {
                        find_shown_content(workspace, &pane.pane_id).is_some()
                    })
            })
            .map(|pane| &pane.pane_id)
    }

    /// Every PDF, audio and video file open in a content pane, and whether its tab is
    /// the active one of a shown pane.
    pub(super) fn media_files(&self) -> Vec<((PaneId, String), MediaSource, bool)> {
        let shown: Vec<(&PaneId, Option<&str>)> = self
            .shown_content_panes()
            .map(|pane_id| {
                let content = self
                    .workspace
                    .as_ref()
                    .and_then(|workspace| find_content(workspace, pane_id));
                (pane_id, content.and_then(|content| content.active_file()))
            })
            .collect();
        let mut files = Vec::new();
        for (pane_id, ui) in &self.content.panes {
            let active = shown
                .iter()
                .find(|(shown, _)| *shown == pane_id)
                .and_then(|(_, active)| *active);
            for (path, file) in &ui.files {
                if let FileBody::Media(kind) = file.body {
                    let source = MediaSource {
                        environment: ui.environment.clone(),
                        path: path.clone(),
                        kind,
                    };
                    let key = (pane_id.clone(), path.clone());
                    files.push((key, source, active == Some(path.as_str())));
                }
            }
        }
        files
    }

    /// Before drawing: state for every visible content pane, a read for each shown file
    /// not yet loaded, editors for files just read, text read again into its editor,
    /// pending line reveals, and active diff tabs read and given the window's look.
    pub(super) fn prepare_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut work = Vec::new();
        if let Some(workspace) = &self.workspace {
            for pane_id in self.shown_content_panes() {
                let Some(content) = find_content(workspace, pane_id) else {
                    continue;
                };
                let ui = self.content.panes.get(pane_id);
                let current = ui.is_some_and(|ui| {
                    ui.environment == content.environment && ui.root() == content.root
                });
                let shown = content
                    .active_file()
                    .is_none_or(|active| ui.is_some_and(|ui| ui.files.contains_key(active)));
                if !current || !shown {
                    work.push((pane_id.clone(), content.clone()));
                }
            }
        }
        for (pane_id, content) in work {
            self.prepare_pane(pane_id, content, cx);
        }
        self.prepare_diff_tabs(window, cx);
        if std::mem::take(&mut self.content.unbuilt) {
            let backups = self.content.backups.as_ref();
            for (pane_id, ui) in &mut self.content.panes {
                for (path, file) in &mut ui.files {
                    file.body = match std::mem::replace(&mut file.body, FileBody::Loading) {
                        FileBody::Read(fresh, restore) => {
                            let restoring = restore.is_some();
                            let editor = file_editor(pane_id, path, fresh, restore, window, cx);
                            if restoring && !editor.dirty {
                                // The file already holds the backed-up text.
                                if let Some(backups) = backups {
                                    backups.delete(&ui.environment, path);
                                }
                            }
                            FileBody::Ready(Box::new(editor))
                        }
                        FileBody::Ready(mut editor) => {
                            if let Some(fresh) = editor.fresh.take() {
                                let discarding = editor.dirty;
                                apply_fresh(&mut editor, path, fresh, window, cx);
                                if discarding && let Some(backups) = backups {
                                    backups.delete(&ui.environment, path);
                                }
                            }
                            FileBody::Ready(editor)
                        }
                        FileBody::LargeText(mut large) => {
                            large.build(window, cx);
                            FileBody::LargeText(large)
                        }
                        body => body,
                    };
                }
            }
        }
        self.apply_reveals(window, cx);
        self.prepare_note_views(window, cx);
        self.prepare_svg_views(cx);
    }

    fn prepare_pane(&mut self, pane_id: PaneId, content: FolderContent, cx: &mut Context<Self>) {
        let created = match self.content.panes.get_mut(&pane_id) {
            Some(ui) if ui.environment != content.environment => {
                // Its editors go with the old environment; their edits stay in backups.
                if let Some(backups) = &self.content.backups {
                    write_pane_backups(backups, ui, None, cx);
                }
                *ui = PaneUi::new(
                    &pane_id,
                    content.environment.clone(),
                    content.root.clone(),
                    cx,
                );
                true
            }
            Some(ui) if ui.root() != content.root => {
                ui.tree = FileTree::new(pane_id.clone(), content.root.clone());
                ui.filter.clear();
                ui.repo = RepoStatus::new(content.root.clone());
                true
            }
            Some(_) => false,
            None => {
                self.content.panes.insert(
                    pane_id.clone(),
                    PaneUi::new(
                        &pane_id,
                        content.environment.clone(),
                        content.root.clone(),
                        cx,
                    ),
                );
                true
            }
        };
        if created {
            self.request_content_listing(
                pane_id.clone(),
                content.environment.clone(),
                content.root.clone(),
                content.root.clone(),
            );
            let seed = self
                .content
                .changes_seed
                .take_if(|(environment, listing)| {
                    *environment == content.environment && same_folder(&listing.repo, &content.root)
                })
                .map(|(_, listing)| listing);
            let pending = self
                .content
                .pending_diff
                .take_if(|(pending, ..)| *pending == pane_id)
                .map(|(_, path, scope, line)| (path, scope, line));
            let tree = self.content.changes.tree;
            if let Some(ui) = self.content.panes.get_mut(&pane_id) {
                if let Some(listing) = seed {
                    // The Changes command opened this pane to list them.
                    ui.mode = SidebarMode::Changes;
                    ui.repo.seed(listing, tree);
                }
                ui.repo.pending_open = pending;
            }
        }
        let Some(active) = content.active_file().map(str::to_owned) else {
            return;
        };
        let Some(ui) = self.content.panes.get_mut(&pane_id) else {
            return;
        };
        if ui.files.contains_key(&active) {
            return;
        }
        let reveal = self
            .content
            .pending_reveal
            .take_if(|pending| pending.environment == content.environment && pending.path == active)
            .map(|pending| pending.line);
        ui.files.insert(
            active.clone(),
            OpenFile {
                body: FileBody::Loading,
                reveal,
            },
        );
        self.request_content_read(pane_id, content.environment, active, true);
    }

    /// Puts the cursor on pending lines of shown editors that have laid out; until then
    /// the window draws again so the line is revealed without further input.
    fn apply_reveals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let mut pending = Vec::new();
        for (pane_id, ui) in &self.content.panes {
            if let Some(active) =
                find_content(workspace, pane_id).and_then(|content| content.active_file())
                && ui
                    .files
                    .get(active)
                    .is_some_and(|file| file.reveal.is_some())
            {
                pending.push((pane_id.clone(), active.to_owned()));
            }
        }
        let mut waiting = false;
        for (pane_id, path) in pending {
            let visible = self
                .visible_layout()
                .is_some_and(|layout| layout.pane(&pane_id).is_some())
                || self
                    .float_layouts
                    .iter()
                    .any(|float| float.pane.pane_id == pane_id);
            let Some(file) = self
                .content
                .panes
                .get_mut(&pane_id)
                .and_then(|ui| ui.files.get_mut(&path))
            else {
                continue;
            };
            let (Some(line), FileBody::Ready(editor)) = (file.reveal, &file.body) else {
                continue;
            };
            if editor.state.read(cx).line_height().is_none() {
                waiting |= visible;
                continue;
            }
            file.reveal = None;
            editor
                .state
                .update(cx, |state, cx| reveal_in_editor(state, line, window, cx));
        }
        if waiting {
            let view = cx.entity_id();
            window.on_next_frame(move |_, cx| cx.notify(view));
        }
    }

    /// Drops state of panes and files the workspace no longer has, after moving an
    /// editor saved as another file to that file once its pane lists it. Unsaved edits
    /// of a pane or file someone removed (the CLI asked first; another window asked
    /// about its own) are discarded with their backups; after a daemon restart the
    /// backups are kept for Recover unsaved edits.
    pub(super) fn prune_content(&mut self) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let settled = !self.mutation_pending;
        let server = (
            workspace.server_id.clone(),
            workspace.server_generation.clone(),
        );
        let backups = self
            .content
            .backups
            .as_ref()
            .filter(|_| self.content.server.as_ref() == Some(&server));
        let forget = |environment: &ContentEnvironment, path: &str, file: &OpenFile| {
            if let (Some(backups), FileBody::Ready(editor)) = (backups, &file.body)
                && editor.dirty
            {
                backups.delete(environment, path);
            }
        };
        // A view on its way to its own pane keeps its state until the receipt names
        // the pane it goes to.
        let moving = self.views.moving_from().cloned();
        self.content
            .panes
            .retain(|pane_id, ui| match find_content(workspace, pane_id) {
                None if moving.as_ref() == Some(pane_id) => true,
                Some(content) => {
                    let moves: Vec<(String, String)> = ui
                        .files
                        .iter()
                        .filter_map(|(path, file)| match &file.body {
                            FileBody::Ready(editor) => editor
                                .moving_to
                                .as_ref()
                                .filter(|target| content.files().any(|open| open == *target))
                                .map(|target| (path.clone(), target.clone())),
                            _ => None,
                        })
                        .collect();
                    for (from, to) in moves {
                        if let Some(mut file) = ui.files.remove(&from) {
                            if let FileBody::Ready(editor) = &mut file.body {
                                editor.moving_to = None;
                            }
                            ui.files.insert(to, file);
                        }
                    }
                    if settled {
                        // The change that would have moved them is over.
                        for file in ui.files.values_mut() {
                            if let FileBody::Ready(editor) = &mut file.body {
                                editor.moving_to = None;
                            }
                        }
                    }
                    ui.files.retain(|path, file| {
                        let open = content.files().any(|open| open == path);
                        if !open {
                            forget(&ui.environment, path, file);
                        }
                        open
                    });
                    // A diff stays while its file is open, so Edit and Diff switch back fast.
                    ui.diffs.retain(|tab, _| content.tab(tab.path()).is_some());
                    ui.repo.retain_commits(content);
                    true
                }
                None => {
                    for (path, file) in &ui.files {
                        forget(&ui.environment, path, file);
                    }
                    false
                }
            });
        self.content.server = Some(server);
    }

    /// Content panes with client state, shown or not.
    pub(super) fn content_panes(&self) -> impl Iterator<Item = &PaneId> {
        self.content.panes.keys()
    }

    /// The folder tree of `pane_id` shows and has the keyboard: beside an open file the
    /// sidebar opens on Files; without one the folder view lists it anyway.
    pub(super) fn show_folder_tree(&mut self, pane_id: &PaneId) {
        if let Some(ui) = self.content.panes.get_mut(pane_id) {
            ui.sidebar = true;
            ui.mode = SidebarMode::Files;
        }
        self.content.released = Some(pane_id.clone());
    }

    /// Why the files `pane_id` holds cannot move to another pane now: a save or a Git
    /// write under way answers the pane it started in.
    pub(super) fn content_busy(&self, pane_id: &PaneId) -> Option<&'static str> {
        let ui = self.content.panes.get(pane_id)?;
        if ui
            .files
            .values()
            .any(|file| matches!(&file.body, FileBody::Ready(editor) if editor.saving.is_some()))
        {
            Some("Wait for the save to finish")
        } else if ui.repo.busy() {
            Some("Wait for the Git change to finish")
        } else {
            None
        }
    }

    /// The files of `from` now show in pane `to`: its editors, unsaved edits, folder
    /// and changes go along. Reads still on their way answer `from`, so they go again.
    pub(super) fn move_content_pane(&mut self, from: &PaneId, to: &PaneId, cx: &mut Context<Self>) {
        if self.content.released.as_ref() == Some(from) {
            self.content.released = Some(to.clone());
        }
        if let Some((pending, ..)) = &mut self.content.pending_diff
            && pending == from
        {
            *pending = to.clone();
        }
        let Some(mut ui) = self.content.panes.remove(from) else {
            return;
        };
        ui.tree.pane_id = to.clone();
        ui.files
            .retain(|_, file| !matches!(file.body, FileBody::Loading));
        for (path, file) in &mut ui.files {
            if let FileBody::Ready(editor) = &mut file.body {
                editor._changes = watch_editor(to, &editor.state, cx);
                if let Some(save_as) = &mut editor.save_as {
                    save_as._events = save_as_events(to, path, &save_as.input, cx);
                }
            }
        }
        ui.diffs.clear();
        ui.repo.moved();
        let relist = ui.tree.rows[0].loading;
        for row in ui.tree.rows.iter_mut().skip(1).filter(|row| row.loading) {
            row.loading = false;
            row.expanded = false;
        }
        let (environment, root) = (ui.environment.clone(), ui.root().to_owned());
        self.content.panes.insert(to.clone(), ui);
        if relist {
            self.request_content_listing(to.clone(), environment, root.clone(), root);
        }
    }

    /// Open files in `pane_id` with unsaved changes, in tab order.
    pub(super) fn unsaved_files(&self, pane_id: &PaneId) -> Vec<String> {
        let (Some(ui), Some(content)) = (
            self.content.panes.get(pane_id),
            self.workspace
                .as_ref()
                .and_then(|workspace| find_content(workspace, pane_id)),
        ) else {
            return Vec::new();
        };
        content
            .files()
            .filter(|path| {
                ui.editor(path)
                    .is_some_and(|editor| editor.dirty && !editor.discarded)
            })
            .map(str::to_owned)
            .collect()
    }

    /// This window's editors with edits a close would lose, by path.
    pub(super) fn unsaved_editors(&self) -> Vec<(PaneId, String, ContentEnvironment)> {
        let mut editors: Vec<_> = self
            .content
            .panes
            .iter()
            .flat_map(|(pane_id, ui)| {
                ui.files
                    .iter()
                    .filter(|(_, file)| {
                        matches!(&file.body, FileBody::Ready(editor)
                            if editor.dirty && !editor.discarded)
                    })
                    .map(move |(path, _)| (pane_id.clone(), path.clone(), ui.environment.clone()))
            })
            .collect();
        editors.sort_by(|left, right| left.1.cmp(&right.1));
        editors
    }

    /// Don't save: the edits go with their editor, without a backup or asking again.
    pub(super) fn discard_edits(&mut self, pane_id: &PaneId, path: &str) {
        let ContentUi { panes, backups, .. } = &mut self.content;
        let Some(ui) = panes.get_mut(pane_id) else {
            return;
        };
        if let Some(editor) = ui.editor_mut(path) {
            editor.discarded = true;
            editor.backup_due = None;
        }
        if let Some(backups) = backups {
            backups.delete(&ui.environment, path);
        }
    }

    /// The prompt's Save for one file; `ticket` tells the prompt whether it was saved.
    /// A read-only file is not written and shows why.
    pub(super) fn save_for_prompt(
        &mut self,
        pane_id: &PaneId,
        path: &str,
        ticket: SaveTicket,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self
            .content
            .panes
            .get_mut(pane_id)
            .and_then(|ui| ui.editor_mut(path))
        else {
            return;
        };
        if !editor.dirty {
            ticket.saved();
            return;
        }
        if !editor.writable {
            editor.banner = Some(Banner::ReadOnly);
            return;
        }
        if editor.moving_to.is_some() {
            return;
        }
        editor.save_tickets.push(ticket);
        // A save already under way answers for this one.
        if editor.saving.is_none() {
            let expected = save_expected(&editor.disk, &editor.version);
            self.write_content(
                pane_id,
                path,
                path.to_owned(),
                Expect::Exactly(expected),
                cx,
            );
        }
    }

    /// Writes the backups of edits that waited `BACKUP_DELAY`, or with `all` of every
    /// edit not yet written.
    pub(super) fn write_backups(&mut self, all: bool, cx: &App) {
        let ContentUi {
            panes,
            backups: Some(backups),
            ..
        } = &mut self.content
        else {
            return;
        };
        let due_by = (!all).then(Instant::now);
        for ui in panes.values_mut() {
            write_pane_backups(backups, ui, due_by, cx);
        }
    }

    /// Drops this window's editors of `panes`, which another window now shows; edits
    /// not saved or discarded travel through their backups.
    pub(super) fn release_content_panes(&mut self, panes: &[PaneId], cx: &App) {
        for pane_id in panes {
            if let Some(mut ui) = self.content.panes.remove(pane_id)
                && let Some(backups) = &self.content.backups
            {
                write_pane_backups(backups, &mut ui, None, cx);
            }
        }
    }

    /// Keyboard focus follows the focused pane: a content pane keeps it on its editor
    /// (or on the pane while it has no editor, or the user left it), a Settings pane on
    /// its search field or highlighted control, a Monitor pane on its table or
    /// search field, while terminals, overlays and the file tree use `focus_handle`,
    /// which carries their text input. Focus held by anything else is left alone.
    pub(super) fn reconcile_keyboard_focus(&mut self, window: &mut Window, cx: &mut App) {
        let ours = window.focused(cx).is_none_or(|focused| {
            focused == self.focus_handle
                || self
                    .content
                    .panes
                    .values()
                    .any(|ui| ui.owns(&focused, window, cx))
                || self.settings_owns(&focused, window, cx)
                || self.monitor_owns(&focused, window, cx)
                || self.browser_owns(&focused, cx)
        });
        if !ours {
            return;
        }
        if self
            .content
            .released
            .as_ref()
            .is_some_and(|released| self.keyboard_content_pane() != Some(released))
        {
            self.content.released = None;
        }
        let wanted = match self.keyboard_content_pane() {
            Some(pane_id) if self.settings_pane_focus(pane_id).is_some() => {
                if self
                    .settings_pane_focus(pane_id)
                    .is_some_and(|focus| focus.contains_focused(window, cx))
                {
                    // Its search field or the pane already has the keyboard.
                    return;
                }
                let Some(wanted) = self.settings_keyboard(pane_id, cx) else {
                    return;
                };
                wanted
            }
            Some(pane_id) if self.monitor_pane_focus(pane_id).is_some() => {
                let Some(wanted) = self.monitor_keyboard(pane_id, window, cx) else {
                    // Its search field or the pane already has the keyboard.
                    return;
                };
                wanted
            }
            Some(pane_id) if self.browser_pane_focus(pane_id).is_some() => {
                let Some(wanted) = self.browser_keyboard(pane_id, window, cx) else {
                    // Its address field or the pane already has the keyboard.
                    return;
                };
                wanted
            }
            Some(pane_id) => {
                let Some(ui) = self.content.panes.get(pane_id) else {
                    return;
                };
                if ui.focus.contains_focused(window, cx) && !ui.focus.is_focused(window) {
                    // The editor or its search field already has the keyboard.
                    return;
                }
                self.focused_content()
                    .and_then(|content| content.active_file())
                    .filter(|_| self.content.released.as_ref() != Some(pane_id))
                    .filter(|active| !ui.notes.is_rendered(active))
                    .and_then(|active| ui.editor(active))
                    .map_or_else(|| ui.focus.clone(), |editor| editor.state.focus_handle(cx))
            }
            None => self.focus_handle.clone(),
        };
        if !wanted.is_focused(window) {
            window.focus(&wanted, cx);
        }
    }

    /// The focused pane when it is a content pane (of any kind) that should have the
    /// keyboard: no overlay or terminal file tree is open.
    fn keyboard_content_pane(&self) -> Option<&PaneId> {
        self.focused_pane().filter(|_| {
            self.overlay.is_none()
                && self.file_tree.is_none()
                && self.focused_pane_content().is_some()
        })
    }

    /// The focused content pane's editor or a search field in it has the keyboard.
    pub(super) fn editor_has_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.overlay.is_none()
            && (self.settings_search_has_keyboard(window, cx)
                || self.monitor_search_has_keyboard(window, cx)
                || self
                    .focused_pane()
                    .and_then(|pane| self.content.panes.get(pane))
                    .is_some_and(|ui| {
                        !ui.focus.is_focused(window) && ui.focus.contains_focused(window, cx)
                    }))
    }

    /// Runs before the editor's own key bindings, so Compi shortcuts win over editor
    /// bindings on the same keys; Ctrl/Cmd+S saves, and Ctrl/Cmd+Enter in a commit box
    /// commits. Everything else goes to the editor.
    pub(super) fn intercept_editor_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.editor_has_keyboard(window, cx) {
            return;
        }
        if self.settings_search_key(key, window, cx) || self.monitor_search_key(key, window, cx) {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if is_save_key(key) {
            self.save_focused_file(cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.commit_box_key(key, window, cx) {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let route = commands::resolve_key(
            commands::Platform::current(),
            commands::ShortcutKey {
                key: &key.key,
                control: key.modifiers.control,
                shift: key.modifiers.shift,
                alt: key.modifiers.alt,
                command: key.modifiers.platform,
            },
            commands::InputOwner::Editor,
            &self.command_context(cx),
            &self.config.keybindings,
        );
        match route {
            commands::KeyRoute::Command(command) => {
                self.execute(command, window, cx);
                cx.stop_propagation();
            }
            commands::KeyRoute::Disabled(_, reason) => {
                self.global_error = Some(reason.into());
                cx.stop_propagation();
                cx.notify();
            }
            commands::KeyRoute::Terminal
            | commands::KeyRoute::Editor
            | commands::KeyRoute::Owned => {}
        }
    }

    /// Opens `file` (or, without one, the folder `root`) for `beside`. A terminal pane
    /// shows it in its view, in place of its shell; with `new_pane` only when it
    /// carries no view yet. Otherwise a content pane of `beside`'s tab in `environment`
    /// whose root holds the file shows it, unless `new_pane`; else a new pane split
    /// beside `beside`, rooted at `root` or the file's folder. The file becomes the
    /// active one, with the cursor on the 1-based `line`.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::gui) fn open_content_file(
        &mut self,
        beside: &PaneId,
        environment: ContentEnvironment,
        root: Option<String>,
        file: Option<String>,
        line: Option<u32>,
        new_pane: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.workspace.as_ref().and_then(|workspace| {
            workspace
                .sessions
                .iter()
                .flat_map(|session| &session.tabs)
                .find(|tab| layout_contains_pane(&tab.layout, beside))
        }) else {
            self.global_error = Some("The pane to open files beside no longer exists".into());
            return;
        };
        if let Some(view) = leaf_node(&tab.layout, beside)
            .filter(|leaf| leaf.surface_id().is_some() && (!new_pane || leaf.view().is_none()))
            .map(|leaf| leaf.view().cloned())
        {
            self.content.released = None;
            self.open_file_in_view(beside, view, environment, root, file, line, window, cx);
            return;
        }
        let reuse = (!new_pane)
            .then(|| {
                reusable_content(
                    &tab.layout,
                    beside,
                    &environment,
                    root.as_deref(),
                    file.as_deref(),
                )
            })
            .flatten()
            .map(|(pane_id, content)| (pane_id.clone(), content.clone()));
        self.content.released = None;
        if let Some((pane_id, mut content)) = reuse {
            if let Some(file) = &file {
                // The file's tab, in whichever mode, returns to editing.
                let tab = ContentTab::edit(file.clone());
                if content.active.as_ref() != Some(&tab) {
                    content.show(tab);
                    self.update_content(&pane_id, content);
                }
                if let Some(line) = line {
                    self.reveal_line(&pane_id, &environment, file, line, window, cx);
                }
            }
            self.focus_pane(pane_id);
            cx.notify();
            return;
        }
        let Some(root) = root.or_else(|| file.as_deref().map(parent_folder)) else {
            return;
        };
        let content = PaneContent::Files(FolderContent {
            environment: environment.clone(),
            root,
            tabs: file.iter().map(ContentTab::edit).collect(),
            active: file.clone().map(ContentTab::edit),
        });
        if self.open_content_beside(beside, content)
            && let (Some(path), Some(line)) = (file, line)
        {
            self.content.pending_reveal = Some(PendingReveal {
                environment,
                path,
                line,
            });
        }
        cx.notify();
    }

    /// `open_content_file` in terminal pane `pane_id`, which carries `view`: a Files view
    /// in `environment` keeps its tabs and takes the file, moving its root to `root` (or
    /// the file's folder) when the file lies outside it; anything else gives way to a
    /// new Files view, asking first about unsaved edits it would drop.
    #[allow(clippy::too_many_arguments)]
    fn open_file_in_view(
        &mut self,
        pane_id: &PaneId,
        view: Option<PaneView>,
        environment: ContentEnvironment,
        root: Option<String>,
        file: Option<String>,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = view
            .as_ref()
            .and_then(|view| view.content.folder())
            .filter(|folder| folder.environment == environment);
        let content = match current {
            Some(current) => {
                let mut merged = current.clone();
                match (&file, &root) {
                    (Some(file), root) => {
                        if !root_contains(&merged.root, file) {
                            merged.root = root.clone().unwrap_or_else(|| parent_folder(file));
                        }
                        merged.show(ContentTab::edit(file.clone()));
                    }
                    (None, Some(root)) if !same_folder(&merged.root, root) => {
                        merged.root = root.clone();
                    }
                    (None, _) => {}
                }
                merged
            }
            None => {
                let Some(root) = root.or_else(|| file.as_deref().map(parent_folder)) else {
                    return;
                };
                FolderContent {
                    environment: environment.clone(),
                    root,
                    tabs: file.iter().map(ContentTab::edit).collect(),
                    active: file.clone().map(ContentTab::edit),
                }
            }
        };
        let content = PaneContent::Files(content);
        let shown = view.is_some_and(|view| view.side == PaneSide::View && view.content == content);
        if !shown {
            self.show_view(pane_id, content, window, cx);
        }
        if let (Some(file), Some(line)) = (file, line) {
            self.reveal_line(pane_id, &environment, &file, line, window, cx);
        }
        self.focus_pane(pane_id.clone());
        cx.notify();
    }

    /// Splits `beside` for a new content pane showing `content`; `false` with a global
    /// error when there is no room.
    pub(super) fn open_content_beside(&mut self, beside: &PaneId, content: PaneContent) -> bool {
        if self.layout.is_none() {
            return false;
        }
        let geometry = match self.content_split(beside) {
            Ok(geometry) => geometry,
            Err(reason) => {
                self.global_error = Some(reason.into());
                return false;
            }
        };
        self.mutate(
            WorkspaceMutation::OpenContent {
                pane_id: beside.clone(),
                axis: SplitAxis::Horizontal,
                geometry,
                content,
            },
            true,
        );
        true
    }

    /// Where a content pane split side by side with `beside` goes, or why it does not fit.
    pub(super) fn content_split(
        &self,
        beside: &PaneId,
    ) -> Result<compi_protocol::SplitGeometry, &'static str> {
        let layout = self
            .layout
            .as_ref()
            .ok_or("The pane to open files beside no longer exists")?;
        let kind = layout::PaneKind::Content;
        let rect = layout.split_feasibility(beside, SplitAxis::Horizontal, kind)?;
        let minimum = layout
            .split_minimum(beside, kind)
            .ok_or("The pane to open files beside no longer exists")?;
        Ok(compi_protocol::SplitGeometry {
            width: rect.width,
            height: rect.height,
            min_width: minimum.width,
            min_height: minimum.height,
            divider: self.metrics().divider_thickness,
        })
    }

    /// Moves the cursor to `line` of `path` in `pane_id` now if its editor shows, or
    /// once it does.
    fn reveal_line(
        &mut self,
        pane_id: &PaneId,
        environment: &ContentEnvironment,
        path: &str,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane_id))
            .and_then(|content| content.active_file())
            == Some(path);
        let Some(file) = self
            .content
            .panes
            .get_mut(pane_id)
            .and_then(|ui| ui.files.get_mut(path))
        else {
            self.content.pending_reveal = Some(PendingReveal {
                environment: environment.clone(),
                path: path.to_owned(),
                line,
            });
            return;
        };
        match &file.body {
            FileBody::Ready(editor) if active && editor.state.read(cx).line_height().is_some() => {
                file.reveal = None;
                editor
                    .state
                    .update(cx, |state, cx| reveal_in_editor(state, line, window, cx));
            }
            _ => file.reveal = Some(line),
        }
    }

    /// Shows `content` in the files pane `pane_id`.
    pub(super) fn update_content(&mut self, pane_id: &PaneId, content: FolderContent) {
        self.mutate(
            WorkspaceMutation::UpdateContent {
                pane_id: pane_id.clone(),
                content: PaneContent::Files(content),
            },
            false,
        );
    }

    fn activate_content_tab(&mut self, pane_id: &PaneId, tab: &ContentTab) {
        let Some(mut content) = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane_id))
            .filter(|content| content.active.as_ref() != Some(tab))
            .cloned()
        else {
            return;
        };
        content.active = Some(tab.clone());
        self.content.released = None;
        self.update_content(pane_id, content);
    }

    /// The tab's close button: unsaved changes ask first.
    fn close_content_file(
        &mut self,
        pane_id: &PaneId,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.guard_unsaved(
            unsaved::CloseScope::File {
                pane_id: pane_id.clone(),
                path: path.to_owned(),
            },
            unsaved::AfterUnsaved::CloseFile {
                pane_id: pane_id.clone(),
                path: path.to_owned(),
            },
            window,
            cx,
        );
    }

    /// Closes `path`'s tab, its editor and diff going once the pane no longer lists it. The
    /// tab after it becomes active, else the one before.
    pub(super) fn drop_content_tab(&mut self, pane_id: &PaneId, path: &str) {
        let Some(mut content) = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane_id))
            .cloned()
        else {
            return;
        };
        let Some(index) = content.tabs.iter().position(|open| open.path() == path) else {
            return;
        };
        let tab = content.tabs.remove(index);
        if content.active.as_ref() == Some(&tab) {
            content.active = content
                .tabs
                .get(index)
                .or_else(|| content.tabs.get(index.wrapping_sub(1)))
                .cloned();
        }
        self.update_content(pane_id, content);
    }

    /// Ctrl/Cmd+S: saves the focused file if it has changes, or was deleted on disk.
    /// A read-only file is not written; its banner offers Save as instead.
    fn save_focused_file(&mut self, cx: &mut Context<Self>) {
        let Some((pane_id, path)) = self.focused_pane().and_then(|pane| {
            Some((
                pane.clone(),
                self.focused_content()?.active_file()?.to_owned(),
            ))
        }) else {
            return;
        };
        let Some(editor) = self
            .content
            .panes
            .get_mut(&pane_id)
            .and_then(|ui| ui.editor_mut(&path))
        else {
            return;
        };
        if !editor.writable {
            editor.banner = Some(Banner::ReadOnly);
            return;
        }
        if (editor.dirty || editor.disk == OnDisk::Missing) && editor.saving.is_none() {
            let expected = save_expected(&editor.disk, &editor.version);
            self.write_content(&pane_id, &path, path.clone(), Expect::Exactly(expected), cx);
        }
    }

    /// Writes the editor of `path` to `target` (itself, or a Save as file) in the pane's
    /// environment; `ContentFileSaved` reports the outcome.
    fn write_content(
        &mut self,
        pane_id: &PaneId,
        path: &str,
        target: String,
        expect: Expect,
        cx: &mut Context<Self>,
    ) {
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        let environment = ui.environment.clone();
        let Some(editor) = ui
            .editor_mut(path)
            .filter(|editor| editor.saving.is_none() && editor.moving_to.is_none())
        else {
            return;
        };
        let text = editor.state.read(cx).text().clone();
        editor.saving = Some(text.clone());
        let endings = editor.endings;
        let connection = self.target.clone();
        let sender = self.event_tx.clone();
        let (pane_id, path) = (pane_id.clone(), path.to_owned());
        thread::spawn(move || {
            let result = (|| -> crate::Result<_> {
                let mut client = connection.connect()?;
                let expected = match expect {
                    Expect::Exactly(expected) => expected,
                    Expect::Whatever(known) => match client.check_content_file(
                        environment.clone(),
                        target.clone(),
                        known.clone(),
                    )? {
                        FileState::Unchanged { .. } => Some(known),
                        FileState::Changed { version, .. } => Some(version),
                        FileState::Missing => None,
                    },
                };
                client.write_content_file(
                    environment,
                    target.clone(),
                    expected,
                    endings.apply(text.to_string()),
                )
            })()
            .map_err(|error| {
                match error.downcast_ref::<compi_protocol::DaemonError>() {
                    Some(error) if error.code == ErrorCode::FileConflict => SaveFailure::Conflict,
                    _ => SaveFailure::Other(error.to_string()),
                }
            });
            sender.send(UiEvent::ContentFileSaved {
                pane_id,
                path,
                target,
                result,
            });
        });
    }

    /// Overwrite: saves over whatever the disk holds now. If it changes again between
    /// that check and the write, the conflict shows again.
    fn overwrite_content_file(&mut self, pane_id: &PaneId, path: &str, cx: &mut Context<Self>) {
        let Some(editor) = self
            .content
            .panes
            .get_mut(pane_id)
            .and_then(|ui| ui.editor_mut(path))
        else {
            return;
        };
        if !editor.writable {
            editor.banner = Some(Banner::ReadOnly);
            return;
        }
        let known = editor.version.clone();
        self.write_content(pane_id, path, path.to_owned(), Expect::Whatever(known), cx);
    }

    /// Reload: reads the file again, discarding the editor's text. With unsaved
    /// changes it takes a second click.
    fn reload_content_file(&mut self, pane_id: &PaneId, path: &str) {
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        let environment = ui.environment.clone();
        let Some(editor) = ui
            .editor_mut(path)
            .filter(|editor| editor.saving.is_none() && editor.moving_to.is_none())
        else {
            return;
        };
        if editor.dirty && !editor.reload_armed {
            editor.reload_armed = true;
            return;
        }
        editor.reload_armed = false;
        editor.reload = Some(Reload::Discard);
        self.request_content_read(pane_id.clone(), environment, path.to_owned(), false);
    }

    fn dismiss_banner(&mut self, pane_id: &PaneId, path: &str) {
        if let Some(editor) = self
            .content
            .panes
            .get_mut(pane_id)
            .and_then(|ui| ui.editor_mut(path))
        {
            editor.banner = None;
            editor.reload_armed = false;
        }
    }

    /// Save as: an inline path field, filled with the file's path, in the pane's
    /// environment.
    fn open_save_as(
        &mut self,
        pane_id: &PaneId,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self
            .content
            .panes
            .get_mut(pane_id)
            .and_then(|ui| ui.editor_mut(path))
        else {
            return;
        };
        let input = match &editor.save_as {
            Some(save_as) => save_as.input.clone(),
            None => {
                let input = cx.new(|cx| InputState::new(window, cx).default_value(path.to_owned()));
                let events = save_as_events(pane_id, path, &input, cx);
                editor.save_as = Some(SaveAs {
                    input: input.clone(),
                    _events: events,
                    error: None,
                });
                input
            }
        };
        input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn submit_save_as(&mut self, pane_id: &PaneId, path: &str, cx: &mut Context<Self>) {
        let Some(save_as) = self
            .content
            .panes
            .get_mut(pane_id)
            .and_then(|ui| ui.editor_mut(path))
            .and_then(|editor| editor.save_as.as_mut())
        else {
            return;
        };
        let target = save_as.input.read(cx).value().trim().to_owned();
        if target.is_empty() || target == path {
            save_as.error = Some("Choose a new path".into());
            return;
        }
        save_as.error = None;
        self.write_content(pane_id, path, target, Expect::Exactly(None), cx);
    }

    /// Closes the Save as field and returns the keyboard to the editor.
    fn close_save_as(
        &mut self,
        pane_id: &PaneId,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self
            .content
            .panes
            .get_mut(pane_id)
            .and_then(|ui| ui.editor_mut(path))
            && editor.save_as.take().is_some()
        {
            editor.state.update(cx, |state, cx| state.focus(window, cx));
        }
    }

    fn editor_changed(
        &mut self,
        pane_id: &PaneId,
        state: &Entity<EditorState>,
        cx: &mut Context<Self>,
    ) {
        let ContentUi { panes, backups, .. } = &mut self.content;
        let Some(ui) = panes.get_mut(pane_id) else {
            return;
        };
        let Some((path, editor)) =
            ui.files
                .iter_mut()
                .find_map(|(path, file)| match &mut file.body {
                    FileBody::Ready(editor) if editor.state == *state => Some((path, editor)),
                    _ => None,
                })
        else {
            return;
        };
        let dirty = state.read(cx).text() != &editor.saved;
        if dirty {
            editor.discarded = false;
            // Within `BACKUP_DELAY` of the first edit since the last write, however
            // fast the typing.
            editor
                .backup_due
                .get_or_insert_with(|| Instant::now() + unsaved::BACKUP_DELAY);
        } else if editor.dirty {
            // Undone back to the file: nothing left to keep.
            editor.backup_due = None;
            editor.restored = None;
            if let Some(backups) = backups {
                backups.delete(&ui.environment, path);
            }
        }
        if dirty != editor.dirty {
            editor.dirty = dirty;
            editor.reload_armed &= dirty;
            cx.notify();
        }
        let path = path.clone();
        self.gutter_edited(pane_id, &path, cx);
    }

    /// Compares shown files with the disk on a timer while the window is active, reads
    /// shown panes' folders again for their Git changes, brings shown editors' Git gutters up to
    /// date, and writes the backups of edits as they come due.
    pub(super) fn watch_content_files(&self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(WATCH_TICK).await;
                if this
                    .update_in(cx, |this, window, cx| {
                        this.check_shown_files(window);
                        this.refresh_repos(window, cx);
                        this.write_backups(false, cx);
                        this.refresh_gutters(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Checks each file shown in an active window when it comes into view and every
    /// `CHECK_INTERVAL` after, one check per file at a time, on background threads.
    fn check_shown_files(&mut self, window: &Window) {
        if self.update_quiesced.load(Ordering::Acquire) {
            return;
        }
        let shown: Vec<(PaneId, String)> = match &self.workspace {
            Some(workspace) if window.is_window_active() => self
                .shown_content_panes()
                .filter_map(|pane_id| {
                    let active = find_content(workspace, pane_id)?.active_file()?.to_owned();
                    Some((pane_id.clone(), active))
                })
                .collect(),
            _ => Vec::new(),
        };
        let mut checks = Vec::new();
        for (pane_id, ui) in &mut self.content.panes {
            for (path, file) in &mut ui.files {
                let FileBody::Ready(editor) = &mut file.body else {
                    continue;
                };
                if !shown
                    .iter()
                    .any(|(shown_pane, shown_path)| shown_pane == pane_id && shown_path == path)
                {
                    editor.shown = false;
                    continue;
                }
                let due = !editor.shown || editor.last_check.elapsed() >= CHECK_INTERVAL;
                editor.shown = true;
                let busy = editor.checking
                    || editor.saving.is_some()
                    || editor.reload.is_some()
                    || editor.fresh.is_some()
                    || editor.moving_to.is_some();
                if due && !busy {
                    editor.checking = true;
                    editor.last_check = Instant::now();
                    checks.push((
                        pane_id.clone(),
                        ui.environment.clone(),
                        path.clone(),
                        editor.version.clone(),
                    ));
                }
            }
        }
        for (pane_id, environment, path, known) in checks {
            let connection = self.target.clone();
            let sender = self.event_tx.clone();
            thread::spawn(move || {
                let result = (|| -> crate::Result<_> {
                    connection.connect()?.check_content_file(
                        environment,
                        path.clone(),
                        known.clone(),
                    )
                })()
                .map_err(|error| error.to_string());
                sender.send(UiEvent::ContentFileChecked {
                    pane_id,
                    path,
                    known,
                    result,
                });
            });
        }
    }

    pub(super) fn content_file_checked(
        &mut self,
        pane_id: &PaneId,
        path: &str,
        known: &FileVersion,
        result: Result<FileState, String>,
    ) {
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        let environment = ui.environment.clone();
        let Some(editor) = ui.editor_mut(path) else {
            return;
        };
        editor.checking = false;
        // A save or reload since the check began makes its answer stale; a failed check
        // is tried again on the next tick.
        let Ok(state) = result else {
            return;
        };
        if editor.version != *known
            || editor.saving.is_some()
            || editor.reload.is_some()
            || editor.fresh.is_some()
            || editor.moving_to.is_some()
        {
            return;
        }
        if let FileState::Unchanged { writable } | FileState::Changed { writable, .. } = &state {
            editor.writable = *writable;
            if *writable && editor.banner == Some(Banner::ReadOnly) {
                editor.banner = None;
            }
        }
        let (disk, action) = after_check(&editor.disk, &state, !editor.dirty);
        editor.disk = disk;
        match action {
            CheckAction::Nothing => {}
            CheckAction::Settled => {
                editor.banner = editor.banner.filter(|banner| !banner.about_disk());
                editor.reload_armed = false;
            }
            CheckAction::Show(banner) => {
                editor.banner = Some(banner);
                editor.reload_armed = false;
            }
            CheckAction::Reload => {
                editor.reload = Some(Reload::Quiet);
                self.request_content_read(pane_id.clone(), environment, path.to_owned(), false);
            }
        }
    }

    fn request_content_listing(
        &self,
        pane_id: PaneId,
        environment: ContentEnvironment,
        root: String,
        parent: String,
    ) {
        let target = self.target.clone();
        let sender = self.event_tx.clone();
        thread::spawn(move || {
            let result = (|| -> crate::Result<_> {
                target
                    .connect()?
                    .list_content_directory(environment, parent.clone())
            })()
            .map_err(|error| error.to_string());
            sender.send(UiEvent::ContentListed {
                pane_id,
                root,
                parent,
                result,
            });
        });
    }

    /// Reads `path` for its pane; with `restore`, a first read that also finds the
    /// file's backup, if it has one.
    fn request_content_read(
        &self,
        pane_id: PaneId,
        environment: ContentEnvironment,
        path: String,
        restore: bool,
    ) {
        let target = self.target.clone();
        let sender = self.event_tx.clone();
        let backups = self.content.backups.clone().filter(|_| restore);
        thread::spawn(move || {
            let result = (|| -> crate::Result<_> {
                target
                    .connect()?
                    .read_content_file(environment.clone(), path.clone())
            })()
            .map_err(|error| error.to_string());
            let backup = match (&result, backups) {
                (Ok(FileContent::Text { .. }), Some(backups)) => backups.load(&environment, &path),
                _ => None,
            };
            sender.send(UiEvent::ContentFileRead {
                pane_id,
                path,
                result,
                backup,
            });
        });
    }

    pub(super) fn content_listed(
        &mut self,
        pane_id: &PaneId,
        root: &str,
        parent: &str,
        result: Result<Vec<DirectoryEntry>, String>,
    ) {
        let Some(ui) = self
            .content
            .panes
            .get_mut(pane_id)
            .filter(|ui| ui.root() == root)
        else {
            return;
        };
        match result {
            Ok(mut entries) => {
                fn folded(name: &str) -> impl Iterator<Item = u8> + '_ {
                    name.bytes().map(|byte| byte.to_ascii_lowercase())
                }
                entries.sort_by(|left, right| {
                    right
                        .is_directory
                        .cmp(&left.is_directory)
                        .then_with(|| folded(&left.name).cmp(folded(&right.name)))
                });
                ui.tree.error = None;
                ui.tree.replace_children(
                    parent,
                    entries
                        .into_iter()
                        .map(|entry| Entry {
                            name: entry.name,
                            directory: entry.is_directory,
                        })
                        .collect(),
                );
            }
            Err(error) => {
                if let Some(row) = ui.tree.rows.iter_mut().find(|row| row.path == parent) {
                    row.loading = false;
                    row.expanded = row.depth == 0;
                }
                ui.tree.error = Some(format!("Could not list {parent}: {error}"));
            }
        }
    }

    pub(super) fn content_file_read(
        &mut self,
        pane_id: PaneId,
        path: String,
        result: Result<FileContent, String>,
        backup: Option<Backup>,
        cx: &mut Context<Self>,
    ) {
        let Some(ui) = self.content.panes.get_mut(&pane_id) else {
            return;
        };
        let Some(file) = ui.files.get_mut(&path) else {
            return;
        };
        // Text for an editor, or the placeholder that replaces one.
        let result = result.map(|content| match content {
            FileContent::Text {
                text,
                version,
                writable,
            } => Ok(FreshText {
                text,
                version,
                writable,
            }),
            FileContent::TooLarge {
                len,
                kind: FileKind::Text,
            } => Err(FileBody::LargeText(Box::new(LargeText::new(len)))),
            FileContent::TooLarge {
                kind: kind @ (FileKind::Pdf | FileKind::Audio | FileKind::Video),
                ..
            }
            | FileContent::Binary {
                kind: kind @ (FileKind::Pdf | FileKind::Audio | FileKind::Video),
                ..
            } => Err(FileBody::Media(kind)),
            FileContent::TooLarge { len, kind } => Err(image_file::image_tab(kind, len).map_or(
                FileBody::Unopenable {
                    reason: Unopenable::TooLarge,
                    len,
                    kind,
                },
                |image| FileBody::Image(Box::new(image)),
            )),
            FileContent::Binary { len, kind } => Err(image_file::image_tab(kind, len).map_or(
                FileBody::Unopenable {
                    reason: Unopenable::Binary,
                    len,
                    kind,
                },
                |image| FileBody::Image(Box::new(image)),
            )),
        });
        let replacement = match &mut file.body {
            FileBody::Loading => Some(match result {
                Ok(Ok(fresh)) => {
                    self.content.unbuilt = true;
                    FileBody::Read(fresh, backup.map(Box::new))
                }
                Ok(Err(placeholder)) => placeholder,
                Err(error) => FileBody::Failed(error),
            }),
            FileBody::Ready(editor) => {
                let Some(reload) = editor.reload.take() else {
                    return;
                };
                match result {
                    Ok(read)
                        if reload == Reload::Quiet && (editor.dirty || editor.saving.is_some()) =>
                    {
                        // Edits made while it was read win; the banner offers the disk's.
                        if editor.saving.is_none() {
                            if let Ok(fresh) = read {
                                editor.disk = OnDisk::Changed(fresh.version);
                            }
                            editor.banner = Some(Banner::Changed);
                        }
                        None
                    }
                    Ok(Ok(fresh)) => {
                        editor.fresh = Some(fresh);
                        self.content.unbuilt = true;
                        None
                    }
                    Ok(Err(placeholder)) => Some(placeholder),
                    Err(error) => {
                        if reload == Reload::Discard {
                            ui.notice = Some((
                                path.clone(),
                                format!("{} was not reloaded: {error}", file_name(&path)),
                            ));
                        }
                        None
                    }
                }
            }
            _ => None,
        };
        let starts_image = matches!(replacement, Some(FileBody::Image(_)));
        let starts_text = matches!(replacement, Some(FileBody::LargeText(_)));
        if let Some(body) = replacement {
            file.body = body;
        }
        if starts_image && let FileBody::Image(image) = &file.body {
            let load = image.load_key();
            let environment = ui.environment.clone();
            self.request_image_load(pane_id, environment, path, load, cx);
        } else if starts_text {
            self.step_large_text(&pane_id, &path, Step::Start);
        }
    }

    pub(super) fn content_file_saved(
        &mut self,
        pane_id: PaneId,
        path: String,
        target: String,
        result: Result<FileVersion, SaveFailure>,
        cx: &mut Context<Self>,
    ) {
        let ContentUi { panes, backups, .. } = &mut self.content;
        let Some(ui) = panes.get_mut(&pane_id) else {
            return;
        };
        let Some(FileBody::Ready(editor)) = ui.files.get_mut(&path).map(|file| &mut file.body)
        else {
            return;
        };
        let Some(saved) = editor.saving.take() else {
            return;
        };
        // Dropped unresolved on failure: prompts waiting for this save cancel.
        let tickets = std::mem::take(&mut editor.save_tickets);
        let save_as = target != path;
        match result {
            Ok(version) => {
                editor.version = version;
                editor.dirty = editor.state.read(cx).text() != &saved;
                editor.reload_armed = false;
                editor.saved = saved;
                editor.disk = OnDisk::Current;
                editor.banner = editor.banner.filter(|banner| !banner.about_disk());
                editor.restored = None;
                if let Some(backups) = backups {
                    backups.delete(&ui.environment, &path);
                }
                // Edits typed during the save are backed up against the new version.
                editor.backup_due = editor.dirty.then(Instant::now);
                for ticket in tickets {
                    ticket.saved();
                }
                if save_as {
                    editor.writable = true;
                    editor.banner = None;
                    editor.save_as = None;
                    editor.moving_to = Some(target.clone());
                    let language = editor_language(&target, editor.saved.len());
                    editor.state.update(cx, |state, cx| {
                        if state.language_name() != language {
                            state.set_highlighter(language, cx);
                        }
                    });
                }
                if ui.notice.as_ref().is_some_and(|(file, _)| *file == path) {
                    ui.notice = None;
                }
            }
            Err(failure) => {
                match (editor.save_as.as_mut().filter(|_| save_as), failure) {
                    (Some(field), SaveFailure::Conflict) => {
                        field.error = Some(format!("{} already exists", file_name(&target)));
                    }
                    (Some(field), SaveFailure::Other(error)) => field.error = Some(error),
                    (None, SaveFailure::Conflict) => {
                        editor.banner = Some(Banner::Conflict);
                        editor.reload_armed = false;
                    }
                    (None, SaveFailure::Other(error)) => {
                        let message = format!("{} was not saved: {error}", file_name(&path));
                        ui.notice = Some((path, message));
                    }
                }
                return;
            }
        }
        if !save_as {
            return;
        }
        // The tab now shows the new file; the original is left as it was.
        let Some(mut content) = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, &pane_id))
            .cloned()
        else {
            return;
        };
        // The new file is edited, so its tab is in Edit mode.
        let target = ContentTab::edit(target);
        content.tabs.retain(|tab| tab.path() != target.path());
        match content.tabs.iter().position(|tab| tab.path() == path) {
            Some(index) => content.tabs[index] = target.clone(),
            None => content.tabs.push(target.clone()),
        }
        content.active = Some(target);
        self.update_content(&pane_id, content);
    }

    fn retry_content_file(&mut self, pane_id: &PaneId, path: &str) {
        if let Some(ui) = self.content.panes.get_mut(pane_id) {
            ui.files.remove(path);
        }
    }

    fn toggle_content_sidebar(&mut self, pane_id: &PaneId) {
        if let Some(ui) = self.content.panes.get_mut(pane_id) {
            ui.sidebar = !ui.sidebar;
        }
    }

    /// Files in a content pane: its folder, beside an open tab as the sidebar, with the
    /// keyboard on the folder.
    pub(super) fn show_content_folder(
        &mut self,
        pane_id: &PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(content) = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane_id))
        else {
            return;
        };
        let tab_open = content.active.is_some();
        if let Some(ui) = self.content.panes.get_mut(pane_id) {
            ui.sidebar |= tab_open;
            ui.mode = SidebarMode::Files;
        }
        self.release_editor(pane_id, window, cx);
    }

    /// The pane takes the keyboard from its editor until the editor is entered again.
    pub(super) fn release_editor(&mut self, pane_id: &PaneId, window: &mut Window, cx: &mut App) {
        if let Some(ui) = self.content.panes.get(pane_id) {
            self.content.released = Some(pane_id.clone());
            window.focus(&ui.focus, cx);
        }
    }

    /// Expands or collapses a folder row, or opens a file row.
    fn activate_content_row(
        &mut self,
        pane_id: &PaneId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        let Some(row) = ui.tree.rows.get(index) else {
            return;
        };
        let file = (!row.directory).then(|| row.path.clone());
        ui.tree.selected = index;
        ui.scroll_to_selected();
        let environment = ui.environment.clone();
        match file {
            Some(path) => self.open_content_file(
                pane_id,
                environment,
                None,
                Some(path),
                None,
                false,
                window,
                cx,
            ),
            None => {
                if let Some(path) = ui.tree.toggle(index) {
                    let root = ui.root().to_owned();
                    self.request_content_listing(pane_id.clone(), environment, root, path);
                }
            }
        }
    }

    /// Right-click on a Files row: open it (a folder expands or collapses), open its
    /// diff when it changed, copy its path or its path within the folder.
    fn open_files_row_menu(
        &mut self,
        pane_id: &PaneId,
        index: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        let Some(row) = ui.tree.rows.get(index) else {
            return;
        };
        let (path, folder, expanded) = (row.path.clone(), row.directory, row.expanded);
        ui.tree.selected = index;
        let root = ui.root();
        let relative = path
            .strip_prefix(root)
            .map(|rest| rest.trim_start_matches(['/', '\\']).to_owned())
            .unwrap_or_else(|| path.clone());
        let diff = match ui.repo.tree_mark(&path) {
            Some(changes::TreeMark::File { path, .. }) => Some(path.clone()),
            _ => None,
        };
        let (open_pane, diff_pane) = (pane_id.clone(), pane_id.clone());
        let mut entries = vec![
            controls::menu_item(
                match (folder, expanded) {
                    (false, _) => "Open",
                    (true, false) => "Expand",
                    (true, true) => "Collapse",
                },
                cx.listener(move |this, _, window, cx| {
                    this.activate_content_row(&open_pane, index, window, cx);
                    cx.notify();
                }),
            )
            .into(),
        ];
        if let Some(diff) = diff {
            entries.push(
                controls::menu_item(
                    "Open diff",
                    cx.listener(move |this, _, window, cx| {
                        this.open_diff(&diff_pane, diff.clone(), None, None, window, cx);
                        cx.notify();
                    }),
                )
                .into(),
            );
        }
        entries.push(controls::MenuEntry::Separator);
        entries.push(
            controls::menu_item("Copy path", move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
            })
            .into(),
        );
        entries.push(
            controls::menu_item("Copy relative path", move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(relative.clone()));
            })
            .into(),
        );
        entries.push(controls::MenuEntry::Separator);
        entries.push(self.pane_submenu(pane_id, cx));
        self.open_menu(position, Anchor::TopLeft, entries, window, cx);
    }

    /// Folder navigation while the pane itself has the keyboard: arrows move and
    /// expand, Enter opens, typing filters the names and Escape clears the filter.
    /// With a file open and the folder hidden, Enter returns to the editor.
    fn content_tree_key(
        &mut self,
        pane_id: &PaneId,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = &key.modifiers;
        if modifiers.control || modifiers.platform || modifiers.alt {
            return false;
        }
        let tab_open = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane_id))
            .is_some_and(|content| content.active.is_some());
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return false;
        };
        if tab_open && !ui.sidebar {
            if key.key == "enter" && !modifiers.shift {
                self.content.released = None;
                return true;
            }
            return false;
        }
        if ui.mode == SidebarMode::Changes {
            return false;
        }
        let visible = ui.visible_rows();
        let at = visible.iter().position(|&index| index == ui.tree.selected);
        let selected = at.map(|_| ui.tree.selected);
        match key.key.as_str() {
            "escape" if !ui.filter.is_empty() => ui.filter.clear(),
            "backspace" if !ui.filter.is_empty() => {
                ui.filter.pop();
            }
            "up" | "down" if !modifiers.shift => {
                let Some(last) = visible.len().checked_sub(1) else {
                    return true;
                };
                let up = key.key == "up";
                ui.tree.selected = visible[match at {
                    Some(at) if up => at.saturating_sub(1),
                    Some(at) => (at + 1).min(last),
                    None if up => last,
                    None => 0,
                }];
            }
            "right"
                if !modifiers.shift
                    && selected.is_some_and(|index| {
                        let row = &ui.tree.rows[index];
                        row.directory && !row.expanded
                    }) =>
            {
                let index = ui.tree.selected;
                self.activate_content_row(pane_id, index, window, cx);
                return true;
            }
            "left" if !modifiers.shift => {
                let Some(selected) = selected else {
                    return true;
                };
                let tree = &mut ui.tree;
                let depth = tree.rows[selected].depth;
                if tree.rows[selected].expanded {
                    tree.toggle(selected);
                } else if depth > 1 {
                    // The folder above; it shows, since a shown row's folders do.
                    tree.selected = (0..selected)
                        .rev()
                        .find(|index| tree.rows[*index].depth < depth)
                        .unwrap_or(selected);
                }
            }
            "enter" if !modifiers.shift => {
                if let Some(index) = selected {
                    self.activate_content_row(pane_id, index, window, cx);
                }
                return true;
            }
            _ => {
                let Some(text) = key
                    .key_char
                    .as_deref()
                    .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
                else {
                    return false;
                };
                ui.filter.push_str(text);
            }
        }
        // A selection the filter hid moves to the first row still shown.
        let visible = ui.visible_rows();
        if !visible.contains(&ui.tree.selected)
            && let Some(&first) = visible.first()
        {
            ui.tree.selected = first;
        }
        ui.scroll_to_selected();
        true
    }

    fn content_key_down(
        &mut self,
        pane_id: &PaneId,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.note_key(pane_id, &event.keystroke, window, cx) {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let Some(ui) = self.content.panes.get(pane_id) else {
            return;
        };
        let key = &event.keystroke;
        let plain = !key.modifiers.control && !key.modifiers.alt && !key.modifiers.platform;
        if key.key == "tab" && plain {
            // Reaches here only when nothing focused took it (the editor indents):
            // the pane or one of its controls moves through the pane's controls.
            let focus = ui.focus.clone();
            controls::cycle_focus(&focus, !key.modifiers.shift, window, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if !ui.focus.is_focused(window) {
            // A key the editor, a field or a control left alone. Text input still
            // needs it, so only Escape stops here: it closes Save as; from the editor
            // it hands the keyboard to the pane, from a control or field it goes back
            // to the editor, or to the pane when there is none.
            if key.key == "escape" {
                let saving_as = ui.files.iter().find_map(|(path, file)| match &file.body {
                    FileBody::Ready(editor) => editor
                        .save_as
                        .as_ref()
                        .filter(|save_as| save_as.input.focus_handle(cx).is_focused(window))
                        .map(|_| path.clone()),
                    _ => None,
                });
                let editor = self
                    .workspace
                    .as_ref()
                    .and_then(|workspace| find_content(workspace, pane_id))
                    .and_then(|content| content.active_file())
                    .filter(|active| !ui.notes.is_rendered(active))
                    .and_then(|active| ui.editor(active))
                    .map(|editor| editor.state.focus_handle(cx));
                if let Some(path) = saving_as {
                    self.close_save_as(pane_id, &path, window, cx);
                } else if !self.handle_app_key(event, window, cx) {
                    match editor {
                        Some(editor) if !editor.contains_focused(window, cx) => {
                            self.content.released = None;
                            window.focus(&editor, cx);
                        }
                        _ => self.release_editor(pane_id, window, cx),
                    }
                }
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        // Compi shortcuts still run; every other key stops here, so nothing typed in
        // a content pane reaches a shell. The Changes list takes its keys first, then
        // a tab in Diff mode the keys it knows.
        let diff = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane_id))
            .and_then(|content| content.active.clone())
            .filter(|tab| tab.diff_scope().is_some());
        // With a shell waiting in `compi tree`, Ctrl/Cmd+Enter enters the selected
        // folder.
        let awaiting = self.views.awaits_folder(pane_id);
        if awaiting
            && key.key == "enter"
            && (key.modifiers.control || key.modifiers.platform)
            && !key.modifiers.shift
            && !key.modifiers.alt
        {
            if let Some(folder) = self
                .content
                .panes
                .get(pane_id)
                .and_then(PaneUi::selected_folder)
            {
                self.enter_view_folder(pane_id, folder);
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let handled = self.changes_list_key(pane_id, key, window, cx)
            || diff.is_some_and(|tab| self.diff_tab_key_down(pane_id, &tab, event, window, cx))
            || self.content_tree_key(pane_id, key, window, cx);
        // Esc with nothing else to dismiss shows a terminal's shell again (and leaves a
        // waiting `compi tree` where it was).
        if !handled
            && !self.handle_app_key(event, window, cx)
            && key.key == "escape"
            && !key.modifiers.control
            && !key.modifiers.alt
            && !key.modifiers.platform
        {
            self.escape_to_shell(pane_id);
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// One content pane at `rect`, sharing `render_pane`'s placement so tiled and
    /// floating presentations look alike.
    pub(super) fn render_content_pane(
        &self,
        id: impl Into<gpui::ElementId>,
        geometry: &layout::PaneLayout,
        rect: layout::Rect,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let colors = *self.colors();
        let pane_id = geometry.pane_id.clone();
        let content = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, &pane_id));
        let ui = self.content.panes.get(&pane_id);
        let pane_content = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_pane_content(workspace, &pane_id));
        let settings = pane_content.is_some_and(|content| content.settings().is_some());
        let monitor = pane_content.is_some_and(PaneContent::is_monitor);
        let menu_id = pane_id.clone();
        let focus_id = pane_id.clone();
        let click_id = pane_id.clone();
        let key_id = pane_id.clone();
        div()
            .id(id)
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.width))
            .h(px(rect.height))
            .bg(material_color(
                colors.background,
                self.effective_background_opacity(),
            ))
            .flex()
            .flex_col()
            .capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
                if this.focused_pane() != Some(&focus_id) {
                    this.focus_pane(focus_id.clone());
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.focus_pane(menu_id.clone());
                    this.open_pane_menu(&menu_id, event.position, Anchor::TopLeft, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .when(
                self.file_tree
                    .as_ref()
                    .is_some_and(|tree| tree.pane_id == pane_id),
                |pane| pane.child(self.render_file_tree(cx)),
            )
            .when(settings, |pane| {
                pane.child(self.render_settings_pane(&pane_id, rect.width, cx))
            })
            .when(monitor, |pane| {
                pane.child(self.render_monitor_pane(&pane_id, rect.width, cx))
            })
            .when(
                pane_content.is_some_and(|content| content.browser().is_some()),
                |pane| pane.child(self.render_browser_body(&pane_id, cx)),
            )
            .when_some(
                content.zip(ui).filter(|_| {
                    self.file_tree
                        .as_ref()
                        .is_none_or(|tree| tree.pane_id != pane_id)
                }),
                |pane, (content, ui)| {
                    pane.child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .relative()
                            .overflow_hidden()
                            .track_focus(&ui.focus)
                            .on_key_down(cx.listener(
                                move |this, event: &KeyDownEvent, window, cx| {
                                    this.content_key_down(&key_id, event, window, cx);
                                },
                            ))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    // The editor takes focus itself; elsewhere the pane does.
                                    if !window.default_prevented() {
                                        this.release_editor(&click_id, window, cx);
                                    }
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            )
                            .flex()
                            .flex_col()
                            .map(|body| match &content.active {
                                Some(tab) => body
                                    .child(self.render_content_tabs(&pane_id, content, ui, cx))
                                    .when_some(ui.notice.as_ref(), |body, (_, notice)| {
                                        body.child(self.render_content_notice(&pane_id, notice, cx))
                                    })
                                    .when_some(
                                        content
                                            .active_file()
                                            .and_then(|active| Some((active, ui.editor(active)?))),
                                        |body, (active, editor)| {
                                            body.child(
                                                self.render_file_banner(
                                                    &pane_id, active, editor, cx,
                                                ),
                                            )
                                        },
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_h_0()
                                            .flex()
                                            .when(ui.sidebar, |row| {
                                                row.child(self.render_content_sidebar(
                                                    &pane_id,
                                                    content,
                                                    ui,
                                                    SIDEBAR_WIDTH.min(rect.width / 2.0),
                                                    cx,
                                                ))
                                            })
                                            .child(match tab.diff_scope() {
                                                None => self
                                                    .render_file_body(&pane_id, tab.path(), ui, cx)
                                                    .into_any_element(),
                                                Some(_) => {
                                                    self.render_diff_tab(&pane_id, ui, tab, cx)
                                                }
                                            }),
                                    ),
                                None => body.child(
                                    self.render_folder_view(&pane_id, content, ui, rect.width, cx),
                                ),
                            }),
                    )
                },
            )
    }

    /// The sidebar beside an open tab: the folder's name, the Files | Changes switch in a
    /// work tree, and the folder or its changes.
    fn render_content_sidebar(
        &self,
        pane_id: &PaneId,
        content: &FolderContent,
        ui: &PaneUi,
        width: f32,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        div()
            .w(px(width))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(color(colors.border))
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .pt_2()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .text_color(color(colors.muted))
                    .child(content.folder_name().to_owned()),
            )
            .children(self.render_sidebar_switch(pane_id, ui, cx))
            .child(match ui.mode {
                SidebarMode::Files => div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .pt_1()
                    .child(self.render_content_tree(pane_id, ui, content.active_file(), cx))
                    .into_any_element(),
                SidebarMode::Changes => self.render_changes_list(pane_id, ui, width, cx),
            })
    }

    fn render_folder_view(
        &self,
        pane_id: &PaneId,
        content: &FolderContent,
        ui: &PaneUi,
        width: f32,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        let home = match (&content.environment, self.target.ssh_host()) {
            (ContentEnvironment::Host, None) => CLIENT_HOME.as_deref(),
            _ => unix_home(&content.root),
        };
        // Roughly how many caption characters fit; the middle goes before the pane clips.
        let fits = ((width - 24.0) / (UI_MICRO_TEXT_SIZE * 0.6)).max(12.0) as usize;
        let root = content.root.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(UI_BODY_TEXT_SIZE))
                                    .child(content.folder_name().to_owned()),
                            )
                            .child(controls::chip(
                                self.environment_label(&content.environment),
                                ChipTone::Neutral,
                            )),
                    )
                    .child(
                        div()
                            .id("content-folder-path")
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(color(colors.muted))
                            .child(caption_path(&content.root, home, fits))
                            .tooltip(controls::tooltip(root, None, None)),
                    ),
            )
            .children(self.render_sidebar_switch(pane_id, ui, cx))
            .child(match ui.mode {
                SidebarMode::Files => self
                    .render_content_tree(pane_id, ui, None, cx)
                    .into_any_element(),
                SidebarMode::Changes => self.render_changes_list(pane_id, ui, width, cx),
            })
    }

    /// The folder's filter field and the rows it shows, below the root the header names.
    fn render_content_tree(
        &self,
        pane_id: &PaneId,
        ui: &PaneUi,
        active: Option<&str>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        // The density of the terminal's file tree.
        let row_height = self.typography.cell_height.max(18.0);
        // Keys reach the folder rather than an editor.
        let keyboard = self.keyboard_content_pane() == Some(pane_id)
            && (active.is_none() || self.content.released.as_ref() == Some(pane_id));
        let visible = ui.visible_rows();
        let empty = if !visible.is_empty() {
            None
        } else if !ui.filter.is_empty() {
            Some("No listed name matches; folders not expanded are not searched.")
        } else if ui.tree.rows[0].loading {
            Some("Loading…")
        } else if ui.tree.error.is_none() {
            Some("Empty folder")
        } else {
            None
        };
        let rows = visible.into_iter().map(|index| {
            let row = &ui.tree.rows[index];
            let selected = index == ui.tree.selected;
            let open = active == Some(row.path.as_str());
            let folder = row.directory;
            let level = row.depth.saturating_sub(1).min(31);
            let row_pane = pane_id.clone();
            let menu_pane = pane_id.clone();
            let mark = ui.repo.tree_mark(&row.path);
            let tint = if selected || open {
                colors.foreground
            } else if folder {
                colors.accent
            } else {
                colors.muted
            };
            div()
                .id(("content-tree-row", index))
                .group("content-tree-row")
                .relative()
                .h(px(row_height))
                .flex_none()
                .pl(px(TREE_INSET + level as f32 * TREE_INDENT))
                .pr_2()
                .flex()
                .items_center()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    if folder {
                        this.release_editor(&row_pane, window, cx);
                    }
                    this.activate_content_row(&row_pane, index, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        this.open_files_row_menu(&menu_pane, index, event.position, window, cx);
                        cx.stop_propagation();
                    }),
                )
                // One guide per folder above, through the middle of its arrow.
                .children((0..level).map(|guide| {
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(TREE_INSET + guide as f32 * TREE_INDENT + 11.0))
                        .w(px(1.0))
                        .bg(color(colors.border))
                }))
                .child(
                    div()
                        .h(px(row_height - 2.0))
                        .flex_1()
                        .min_w_0()
                        .px_1()
                        .flex()
                        .items_center()
                        .rounded_sm()
                        .whitespace_nowrap()
                        .when(selected, |item| item.bg(color(colors.selection)))
                        .when(!selected, |item| {
                            item.group_hover("content-tree-row", move |style| {
                                style.bg(color(colors.surface_hover))
                            })
                        })
                        .when(open, |item| item.text_color(color(colors.accent)))
                        // Dotfiles and dot-folders recede.
                        .when(row.name.starts_with('.') && !selected, |item| {
                            item.opacity(0.55)
                        })
                        .child(
                            div()
                                .w(px(14.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .children(folder.then(|| {
                                    controls::glyph(
                                        if row.expanded {
                                            IconName::ChevronDown
                                        } else {
                                            IconName::ChevronRight
                                        },
                                        color(colors.muted),
                                    )
                                })),
                        )
                        .child(tree_entry_icon(folder, row.expanded, color(tint)))
                        .child(
                            div()
                                .ml_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(row.name.clone()),
                        )
                        .child(div().flex_1())
                        .when_some(mark, |item, mark| {
                            let mark = self
                                .render_tree_mark(pane_id, index, mark, row_height, selected, cx);
                            item.child(mark)
                        }),
                )
        });
        let focus_pane = pane_id.clone();
        let clear_pane = pane_id.clone();
        let caret = keyboard.then(|| {
            div()
                .flex_none()
                .w(px(1.0))
                .h(px(UI_SMALL_LINE_HEIGHT - 4.0))
                .bg(color(colors.accent))
        });
        let filter = div()
            .id("content-tree-filter")
            .flex_none()
            .mx_2()
            .mb_1()
            .h(px(row_height + 4.0))
            .px_2()
            .flex()
            .items_center()
            .rounded_sm()
            .border_1()
            .border_color(color(if keyboard {
                colors.accent
            } else {
                colors.border
            }))
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.release_editor(&focus_pane, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .tooltip(controls::tooltip(
                "Type to filter the listed names",
                Some("↑↓ select · ←→ fold · Enter open · Esc clear".into()),
                None,
            ))
            .map(|field| {
                if ui.filter.is_empty() {
                    field
                        .children(caret)
                        .child(div().text_color(color(colors.muted)).child("Filter"))
                } else {
                    field
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(ui.filter.clone()),
                        )
                        .children(caret)
                        .child(div().flex_1())
                        .child(
                            icon_button("content-tree-filter-clear", IconName::X, "Clear filter")
                                .key("escape")
                                .tab_stop(false)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if let Some(ui) = this.content.panes.get_mut(&clear_pane) {
                                        ui.filter.clear();
                                        ui.scroll_to_selected();
                                    }
                                    this.release_editor(&clear_pane, window, cx);
                                    cx.notify();
                                })),
                        )
                }
            });
        // A shell waiting in `compi tree` changes to the folder selected here.
        let enter = self.views.awaits_folder(pane_id).then(|| {
            let folder = ui.selected_folder();
            let (pane, path) = (pane_id.clone(), folder.clone());
            div().flex_none().mx_2().mb_1().flex().child(
                text_button("content-enter-folder", "Enter folder")
                    .detail("The shell that ran compi tree changes to the selected folder")
                    .key("secondary-enter")
                    .disabled_because(folder.is_none().then_some("Select a folder to enter"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(path) = &path {
                            this.enter_view_folder(&pane, path.clone());
                        }
                        cx.notify();
                    })),
            )
        });
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .child(filter)
            .children(enter)
            .when_some(ui.tree.error.as_deref(), |tree, error| {
                tree.child(
                    div()
                        .flex_none()
                        .px_3()
                        .py_1()
                        .text_color(color(colors.error))
                        .child(error.to_owned()),
                )
            })
            .child(
                div()
                    .id("content-tree")
                    .flex_1()
                    .min_h_0()
                    .pb_1()
                    .overflow_y_scroll()
                    .track_scroll(&ui.tree_scroll)
                    .children(rows)
                    .when_some(empty, |list, text| {
                        list.child(
                            div()
                                .px_3()
                                .py_1()
                                .text_color(color(colors.muted))
                                .child(text),
                        )
                    }),
            )
    }

    /// The tab strip: the sidebar toggle, the file tabs in order, the Edit / Diff switch of
    /// a file in Edit mode, and where the files live.
    fn render_content_tabs(
        &self,
        pane_id: &PaneId,
        content: &FolderContent,
        ui: &PaneUi,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        let toggle_pane = pane_id.clone();
        let environment = self.environment_label(&ui.environment);
        let tabs = content.tabs.iter().enumerate().map(|(index, tab)| {
            let active = content.active.as_ref() == Some(tab);
            let name = file_name(tab.path()).to_owned();
            // In Diff mode the tooltip names what the diff compares.
            let glyph = tab.diff_scope().is_some();
            let title = Self::diff_tab_baseline(ui, tab).unwrap_or_else(|| tab.path().to_owned());
            let editor = ui.editor(tab.path());
            let dirty = editor.is_some_and(|editor| editor.dirty);
            let read_only = editor.is_some_and(|editor| !editor.writable);
            let (open_pane, open_tab) = (pane_id.clone(), tab.clone());
            let (close_pane, close_tab) = (pane_id.clone(), tab.clone());
            let reason: SharedString = if dirty {
                format!("{environment} · unsaved changes").into()
            } else {
                environment.clone().into()
            };
            div()
                .id(("content-file-tab", index))
                .flex_none()
                .h_full()
                .max_w(px(220.0))
                .pl_3()
                .pr_1()
                .flex()
                .items_center()
                .gap_1()
                .border_r_1()
                .border_color(color(colors.border))
                .cursor_pointer()
                .text_color(color(if active {
                    colors.foreground
                } else {
                    colors.muted
                }))
                .when(active, |tab| {
                    tab.bg(color(colors.surface))
                        .border_b_2()
                        .border_color(color(colors.accent))
                })
                .when(!active, |tab| {
                    tab.hover(move |style| style.bg(color(colors.surface_hover)))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.activate_content_tab(&open_pane, &open_tab);
                    cx.stop_propagation();
                    cx.notify();
                }))
                .tooltip(controls::tooltip(title, Some(reason), None))
                .when(glyph, |tab| {
                    tab.child(controls::glyph(IconName::FileDiff, color(colors.muted)))
                })
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(name),
                )
                .when(read_only, |tab| {
                    tab.child(controls::chip("Read-only", ChipTone::Neutral))
                })
                .when(dirty, |tab| {
                    tab.child(
                        div()
                            .flex_none()
                            .text_color(color(colors.accent))
                            .child("●"),
                    )
                })
                .child(
                    icon_button(("content-file-close", index), IconName::X, "Close").on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.close_content_file(&close_pane, close_tab.path(), window, cx);
                            cx.notify();
                        }),
                    ),
                )
        });
        div()
            .flex_none()
            .h(px(FILE_TAB_HEIGHT))
            .flex()
            .border_b_1()
            .border_color(color(colors.border))
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .child(
                div()
                    .flex_none()
                    .w(px(FILE_TAB_HEIGHT))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_r_1()
                    .border_color(color(colors.border))
                    .child(
                        icon_button(
                            "content-sidebar-toggle",
                            if ui.sidebar {
                                IconName::PanelLeftClose
                            } else {
                                IconName::PanelLeftOpen
                            },
                            if ui.sidebar {
                                "Hide sidebar"
                            } else {
                                "Show sidebar"
                            },
                        )
                        .header()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_content_sidebar(&toggle_pane);
                            cx.notify();
                        })),
                    ),
            )
            .child(
                div()
                    .id("content-file-tabs")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .overflow_x_scroll()
                    .children(tabs),
            )
            .children(
                content
                    .active
                    .as_ref()
                    .filter(|tab| tab.diff_scope().is_none())
                    .and_then(|tab| self.render_mode_switch(pane_id, ui, tab, cx))
                    .map(|switch| div().flex_none().pl_2().flex().items_center().child(switch)),
            )
            .children(
                content
                    .active_file()
                    .and_then(|path| self.render_baseline(pane_id, path, cx)),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(160.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .text_color(color(colors.muted))
                    .child(environment),
            )
    }

    /// Where content of `environment` lives, as captions name it.
    pub(super) fn environment_label(&self, environment: &ContentEnvironment) -> String {
        environment_label(self.target.ssh_host(), environment)
    }

    /// The active file's banner and its Save as field.
    fn render_file_banner(
        &self,
        pane_id: &PaneId,
        path: &str,
        editor: &FileEditor,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        let name = file_name(path);
        // Banner and Save as actions run on this file of this pane.
        let bind =
            |control: controls::Control,
             run: fn(&mut Self, &PaneId, &str, &mut Window, &mut Context<Self>)| {
                let (pane_id, path) = (pane_id.clone(), path.to_owned());
                control
                    .header()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        run(this, &pane_id, &path, window, cx);
                        cx.notify();
                    }))
            };
        let reload = || {
            let button = text_button("content-banner-reload", "Reload");
            bind(
                if editor.reload_armed {
                    button.danger().detail("Your edits are discarded")
                } else {
                    button
                },
                |this, pane, path, _, _| this.reload_content_file(pane, path),
            )
        };
        let save_as = || {
            bind(
                text_button("content-banner-save-as", "Save as…"),
                |this, pane, path, window, cx| this.open_save_as(pane, path, window, cx),
            )
        };
        let dismiss = || {
            bind(
                icon_button("content-banner-dismiss", IconName::X, "Dismiss"),
                |this, pane, path, _, _| this.dismiss_banner(pane, path),
            )
        };
        let restored = editor.restored.map(|saved_at| {
            div()
                .flex_none()
                .px_3()
                .py_1()
                .flex()
                .items_center()
                .gap_1()
                .border_b_1()
                .border_color(color(colors.border))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(color(colors.muted))
                        .child(unsaved::restored_note(saved_at)),
                )
                .child(bind(
                    icon_button("content-restored-dismiss", IconName::X, "Dismiss"),
                    |this, pane, path, _, _| {
                        if let Some(editor) = this
                            .content
                            .panes
                            .get_mut(pane)
                            .and_then(|ui| ui.editor_mut(path))
                        {
                            editor.restored = None;
                        }
                    },
                ))
        });
        let armed = "Reload again to discard your edits.".to_owned();
        let banner = editor.banner.map(|banner| {
            let (tone, message, actions): (u32, String, Vec<_>) = match banner {
                Banner::Conflict => (
                    colors.error,
                    if editor.reload_armed {
                        armed.clone()
                    } else {
                        format!("{name} changed on disk since you opened it.")
                    },
                    vec![
                        reload(),
                        bind(
                            text_button("content-banner-overwrite", "Overwrite")
                                .danger()
                                .detail("Replaces the file on disk with your version"),
                            |this, pane, path, _, cx| this.overwrite_content_file(pane, path, cx),
                        ),
                        save_as(),
                        dismiss(),
                    ],
                ),
                Banner::Changed => (
                    colors.accent,
                    if editor.reload_armed {
                        armed.clone()
                    } else {
                        format!("{name} changed on disk.")
                    },
                    vec![
                        reload(),
                        bind(
                            text_button("content-banner-keep", "Keep mine"),
                            |this, pane, path, _, _| this.dismiss_banner(pane, path),
                        ),
                    ],
                ),
                Banner::Deleted => (
                    colors.error,
                    format!("{name} was deleted on disk. Saving creates it again."),
                    vec![dismiss()],
                ),
                Banner::ReadOnly => (
                    colors.accent,
                    format!("{name} is read-only."),
                    vec![save_as(), dismiss()],
                ),
            };
            div()
                .flex_none()
                .px_3()
                .py_1()
                .flex()
                .items_center()
                .gap_1()
                .border_b_1()
                .border_color(color(tone))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(color(ui_text_color(tone, colors.background)))
                        .child(message),
                )
                .children(actions)
        });
        let field = editor.save_as.as_ref().map(|field| {
            div()
                .flex_none()
                .px_3()
                .py_1()
                .flex()
                .flex_col()
                .gap_1()
                .border_b_1()
                .border_color(color(colors.border))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_none().child("Save as"))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&field.input).appearance(true)),
                        )
                        .child(
                            bind(
                                text_button("content-save-as-submit", "Save").primary(),
                                |this, pane, path, _, cx| this.submit_save_as(pane, path, cx),
                            )
                            .key("enter")
                            .loading(editor.saving.is_some()),
                        )
                        .child(
                            bind(
                                text_button("content-save-as-cancel", "Cancel"),
                                |this, pane, path, window, cx| {
                                    this.close_save_as(pane, path, window, cx)
                                },
                            )
                            .key("escape"),
                        ),
                )
                .when_some(field.error.clone(), |row, error| {
                    row.child(div().text_color(color(colors.error)).child(error))
                })
        });
        div()
            .flex_none()
            .flex()
            .flex_col()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .children(restored)
            .children(banner)
            .children(field)
    }

    fn render_content_notice(
        &self,
        pane_id: &PaneId,
        notice: &str,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        let dismiss_pane = pane_id.clone();
        div()
            .flex_none()
            .px_3()
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(color(colors.error))
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .text_color(color(ui_text_color(colors.error, colors.background)))
            .child(div().flex_1().min_w_0().child(notice.to_owned()))
            .child(
                icon_button("content-notice-dismiss", IconName::X, "Dismiss")
                    .header()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(ui) = this.content.panes.get_mut(&dismiss_pane) {
                            ui.notice = None;
                        }
                        cx.notify();
                    })),
            )
    }

    fn render_file_body(
        &self,
        pane_id: &PaneId,
        path: &str,
        ui: &PaneUi,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        if ui.notes.is_rendered(path) && ui.editor(path).is_some() {
            if is_svg(path) {
                return self.render_image_body(
                    pane_id,
                    path,
                    ui.svg_views.get(path).map(|view| view.file()),
                    cx,
                );
            }
            return self.render_note_body(pane_id, path, ui, div().into_any_element(), cx);
        }
        let colors = *self.colors();
        let area = div().flex_1().min_w_0().h_full();
        let body = match ui.files.get(path).map(|file| &file.body) {
            Some(FileBody::Ready(editor)) => area
                .relative()
                // The editor's own context menu, not the palette, answers a right click.
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .child(
                    Editor::new(&editor.state)
                        .appearance(false)
                        .bordered(false)
                        .readonly(!editor.writable)
                        .font_family(self.typography.font.family.clone())
                        .text_size(px(self.typography.font_size))
                        .size_full(),
                )
                .children(self.render_gutter(pane_id, path, cx))
                .into_any_element(),
            Some(FileBody::Image(image)) => self.render_image_body(pane_id, path, Some(image), cx),
            Some(FileBody::LargeText(large)) => self.render_large_text(pane_id, path, large, cx),
            Some(FileBody::Media(_)) => self.render_media_page(pane_id, path),
            Some(FileBody::Unopenable { reason, len, kind }) => {
                let copied = path.to_owned();
                area.p_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .child(
                        div()
                            .text_size(px(UI_BODY_TEXT_SIZE))
                            .child(file_name(path).to_owned()),
                    )
                    .child(div().text_color(color(colors.muted)).child(format!(
                        "{} · {}",
                        human_size(*len),
                        describe_unopenable(*reason, *kind)
                    )))
                    .child(
                        div()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(color(colors.muted))
                            .child(path.to_owned()),
                    )
                    .child(
                        div().mt_1().flex().child(
                            text_button("content-file-copy-path", "Copy path")
                                .icon(IconName::Copy)
                                .header()
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        copied.clone(),
                                    ));
                                }),
                        ),
                    )
                    .into_any_element()
            }
            Some(FileBody::Failed(error)) => {
                let (retry_pane, retry_path) = (pane_id.clone(), path.to_owned());
                area.p_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .child(format!("Cannot open {}", file_name(path)))
                    .child(
                        div()
                            .text_color(color(ui_text_color(colors.error, colors.background)))
                            .child(error.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(color(colors.muted))
                            .child(path.to_owned()),
                    )
                    .child(
                        div().mt_1().flex().child(
                            text_button("content-file-retry", "Try again")
                                .header()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.retry_content_file(&retry_pane, &retry_path);
                                    cx.notify();
                                })),
                        ),
                    )
                    .into_any_element()
            }
            Some(FileBody::Loading | FileBody::Read(..)) | None => area
                .p_3()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .text_color(color(colors.muted))
                .child(format!("Opening {}…", file_name(path)))
                .into_any_element(),
        };
        if crate::notes::is_markdown(path) && ui.editor(path).is_some() {
            self.render_note_body(pane_id, path, ui, body, cx)
        } else {
            body
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_keep_crlf_files_crlf_and_leave_other_files_as_typed() {
        let crlf = LineEndings::of("\u{feff}one\r\ntwo\r\n");
        assert_eq!(crlf, LineEndings::Crlf);
        // Enter in the editor inserts a bare `\n`.
        assert_eq!(
            crlf.apply("\u{feff}one\r\nnew\ntwo\r\n".into()),
            "\u{feff}one\r\nnew\r\ntwo\r\n"
        );
        assert_eq!(LineEndings::of("one\ntwo\r\n"), LineEndings::Lf);
        assert_eq!(LineEndings::of("no newline"), LineEndings::Lf);
        assert_eq!(
            LineEndings::Lf.apply("mixed\r\nnew\n".into()),
            "mixed\r\nnew\n"
        );
    }

    #[test]
    fn languages_follow_file_names_then_extensions() {
        assert_eq!(language_for("/work/src/main.rs"), "rust");
        assert_eq!(language_for(r"C:\work\Cargo.lock"), "toml");
        assert_eq!(language_for("/work/README.MD"), "markdown");
        assert_eq!(language_for("/home/me/.bashrc"), "bash");
        assert_eq!(language_for("/work/web/app.tsx"), "tsx");
        assert_eq!(language_for("/work/Makefile"), "text");
        assert_eq!(language_for("/work/notes"), "text");
    }

    #[test]
    fn files_open_in_the_folder_that_holds_them() {
        assert!(root_contains("/work", "/work/src/main.rs"));
        assert!(root_contains("/work/", "/work/main.rs"));
        assert!(root_contains("/", "/main.rs"));
        assert!(root_contains(r"C:\work", r"C:\work\main.rs"));
        assert!(!root_contains("/work", "/workshop/main.rs"));
        assert!(!root_contains("/work", "/work"));
        assert_eq!(parent_folder("/work/src/main.rs"), "/work/src");
        assert_eq!(parent_folder("/main.rs"), "/");
        assert_eq!(parent_folder(r"C:\main.rs"), r"C:\");
        assert_eq!(parent_folder(r"C:\work\main.rs"), r"C:\work");
    }

    #[test]
    fn a_file_reuses_the_closest_pane_of_its_environment() {
        let wsl = ContentEnvironment::Wsl {
            distribution: "Ubuntu".into(),
        };
        let pane = |id: &str, root: &str, files: &[&str]| LayoutNode::Content {
            pane_id: PaneId::new(id),
            content: PaneContent::Files(FolderContent {
                environment: wsl.clone(),
                root: root.into(),
                tabs: files.iter().map(|file| ContentTab::edit(*file)).collect(),
                active: None,
            }),
        };
        let split = |first, second| LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            ratio: 0.5,
            first: Box::new(first),
            second: Box::new(second),
        };
        let settings = LayoutNode::Content {
            pane_id: PaneId::new("settings"),
            content: PaneContent::Settings(compi_protocol::SettingsContent::default()),
        };
        let layout = split(
            split(pane("outer", "/work", &[]), settings),
            split(
                pane("inner", "/work/src", &[]),
                pane("open", "/work", &["/work/src/a.rs", "/elsewhere/c.rs"]),
            ),
        );
        let target = |beside: &str, file: &str, environment: &ContentEnvironment| {
            reusable_content(&layout, &PaneId::new(beside), environment, None, Some(file))
                .map(|(pane_id, _)| pane_id.as_str().to_owned())
        };
        assert_eq!(
            target("terminal", "/work/src/a.rs", &wsl).as_deref(),
            Some("open")
        );
        assert_eq!(
            target("terminal", "/work/src/b.rs", &wsl).as_deref(),
            Some("inner")
        );
        assert_eq!(
            target("outer", "/work/src/b.rs", &wsl).as_deref(),
            Some("outer")
        );
        assert_eq!(target("terminal", "/elsewhere/b.rs", &wsl), None);
        assert_eq!(
            target("terminal", "/work/src/b.rs", &ContentEnvironment::Host),
            None
        );
        // A pane that has the file open shows it, though its folder does not hold it
        // (`compi edit FILE --pane` keeps the pane's folder).
        assert_eq!(
            target("open", "/elsewhere/c.rs", &wsl).as_deref(),
            Some("open")
        );
        assert_eq!(
            target("terminal", "/elsewhere/c.rs", &wsl).as_deref(),
            Some("open")
        );
        // A Settings pane never shows files, not even beside itself.
        assert_eq!(
            target("settings", "/work/src/b.rs", &wsl).as_deref(),
            Some("inner")
        );
        let mut leaves = Vec::new();
        content_leaves(&layout, &mut |pane_id, _| leaves.push(pane_id.as_str()));
        assert_eq!(leaves, ["outer", "inner", "open"]);
    }

    #[test]
    fn sizes_read_like_file_sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(812), "812 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(24 * 1024 + 300), "24 KB");
        assert_eq!(human_size(2_516_582), "2.4 MB");
        assert_eq!(human_size(1024 * 1024 + 1), "1.0 MB");
        assert_eq!(human_size(3 << 30), "3.0 GB");
    }

    #[test]
    fn a_reload_keeps_the_cursor_within_the_new_text() {
        let text = Rope::from("one\ntwo\n");
        assert_eq!(clamped_offset(&text, Position::new(1, 1)), 5);
        assert_eq!(clamped_offset(&text, Position::new(1, 99)), 7);
        assert_eq!(clamped_offset(&text, Position::new(9, 3)), 8);
        assert_eq!(clamped_offset(&Rope::from("é\nx"), Position::new(0, 1)), 2);
        assert_eq!(clamped_offset(&Rope::from(""), Position::new(4, 4)), 0);
    }

    #[test]
    fn checks_reload_clean_editors_and_warn_once_about_unsaved_ones() {
        let version = |sha: &str| FileVersion {
            len: 1,
            sha256: sha.into(),
        };
        let changed = |sha: &str| FileState::Changed {
            version: version(sha),
            writable: true,
        };
        let unchanged = FileState::Unchanged { writable: true };
        let current = OnDisk::Current;
        assert_eq!(
            after_check(&current, &unchanged, false),
            (OnDisk::Current, CheckAction::Nothing)
        );
        assert_eq!(
            after_check(&current, &changed("b"), true),
            (OnDisk::Current, CheckAction::Reload)
        );
        assert_eq!(
            after_check(&current, &changed("b"), false),
            (
                OnDisk::Changed(version("b")),
                CheckAction::Show(Banner::Changed)
            )
        );
        // Keep mine: the same change does not warn again; a newer one does.
        let kept = OnDisk::Changed(version("b"));
        assert_eq!(
            after_check(&kept, &changed("b"), false),
            (kept.clone(), CheckAction::Nothing)
        );
        assert_eq!(
            after_check(&kept, &changed("c"), false),
            (
                OnDisk::Changed(version("c")),
                CheckAction::Show(Banner::Changed)
            )
        );
        assert_eq!(
            after_check(&kept, &unchanged, false),
            (OnDisk::Current, CheckAction::Settled)
        );
        assert_eq!(
            after_check(&current, &FileState::Missing, true),
            (OnDisk::Missing, CheckAction::Show(Banner::Deleted))
        );
        assert_eq!(
            after_check(&OnDisk::Missing, &FileState::Missing, false),
            (OnDisk::Missing, CheckAction::Nothing)
        );
        assert_eq!(
            after_check(&OnDisk::Missing, &unchanged, true),
            (OnDisk::Current, CheckAction::Settled)
        );
        // A deleted file is created again; any other save must find its version.
        assert_eq!(save_expected(&OnDisk::Missing, &version("a")), None);
        assert_eq!(save_expected(&kept, &version("a")), Some(version("a")));
    }

    #[test]
    fn remote_content_names_its_ssh_host() {
        let wsl = ContentEnvironment::Wsl {
            distribution: "Ubuntu".into(),
        };
        assert_eq!(environment_label(None, &wsl), wsl.to_string());
        assert_eq!(
            environment_label(None, &ContentEnvironment::Host),
            ContentEnvironment::Host.to_string()
        );
        assert!(
            environment_label(Some("build.example"), &ContentEnvironment::Host)
                .contains("build.example")
        );
        let remote_wsl = environment_label(Some("build.example"), &wsl);
        assert!(remote_wsl.contains("build.example") && remote_wsl.contains("Ubuntu"));
    }

    #[test]
    fn folder_paths_show_home_as_tilde_and_elide_their_middle() {
        let home = Some("/home/john");
        assert_eq!(
            caption_path("/home/john/src/compi/", home, 80),
            "~/src/compi"
        );
        assert_eq!(caption_path("/home/john", home, 80), "~");
        assert_eq!(caption_path("/home/johnny/x", home, 80), "/home/johnny/x");
        assert_eq!(
            caption_path(r"C:\Users\John\proj\", Some(r"c:\users\john\"), 80),
            r"~\proj"
        );
        assert_eq!(caption_path("/", None, 80), "/");
        assert_eq!(caption_path(r"C:\", None, 80), r"C:\");
        assert_eq!(
            caption_path("/home/john/a/b/c/deeply/nested/project", home, 24),
            "~/…/nested/project"
        );
        assert_eq!(
            caption_path(r"D:\work\a\b\client", None, 16),
            r"D:\…\a\b\client"
        );
        assert_eq!(
            caption_path("/averyveryverylongfoldername", None, 10),
            "/ave…rname"
        );
        assert_eq!(unix_home("/home/john/src"), Some("/home/john"));
        assert_eq!(unix_home("/root"), Some("/root"));
        assert_eq!(unix_home("/home/"), None);
        assert_eq!(unix_home("/srv/john"), None);
    }
}
