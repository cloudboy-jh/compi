//! Content panes: layout leaves that show files instead of a terminal. They tile,
//! float, zoom and move like terminal panes but have no surface, so they never
//! attach, resize a PTY, or receive terminal input. Without an active file a pane
//! browses its folder; with one it shows its open files as tabs above a GPUI Kit code
//! editor, with the folder as an optional sidebar. Folders are listed and files read
//! and saved through the daemon, so host and WSL folders behave the same.
use super::tree::{Entry, FileTree};
use super::tree_ui::tree_entry_icon;
use super::*;
use compi_protocol::{ContentEnvironment, DirectoryEntry, ErrorCode, FileVersion, PaneContent};
use gpui::Entity;
use gpui_component::highlighter::HighlightTheme;
use gpui_component::input::{Editor, EditorState, InputEvent, Position, Rope};
use gpui_component::theme::{Theme, ThemeMode, ThemeTokens};

/// Width of the folder sidebar beside an open file.
const SIDEBAR_WIDTH: f32 = 220.0;
const FILE_TAB_HEIGHT: f32 = 30.0;
const TREE_ROW_HEIGHT: f32 = 22.0;
#[cfg(target_os = "macos")]
const SAVE_SHORTCUT: &str = "⌘S";
#[cfg(windows)]
const SAVE_SHORTCUT: &str = "Ctrl+S";

/// Client-local state of content panes. What a pane shows (its folder, open files and
/// active file) lives in the daemon's `PaneContent`; this keeps what only this window
/// needs: keyboard focus, folder trees, and an editor per open file.
#[derive(Default)]
pub(in crate::gui) struct ContentUi {
    panes: HashMap<PaneId, PaneUi>,
    /// The pane whose editor the user left with Escape or by clicking its folder;
    /// the pane keeps the keyboard until the editor is entered again.
    released: Option<PaneId>,
    /// A line to reveal in a file whose pane `OpenContent` is still creating.
    pending_reveal: Option<PendingReveal>,
    /// Some file was read and needs an editor, which only a window can build.
    unbuilt: bool,
}

struct PendingReveal {
    environment: ContentEnvironment,
    path: String,
    line: u32,
}

struct PaneUi {
    /// Keyboard focus of the pane itself; its editor and search field sit inside it.
    focus: FocusHandle,
    environment: ContentEnvironment,
    tree: FileTree,
    tree_scroll: ScrollHandle,
    /// The folder shows beside an open file.
    sidebar: bool,
    /// The latest save failure or close warning, and the file it is about.
    notice: Option<(String, String)>,
    files: HashMap<String, OpenFile>,
}

struct OpenFile {
    body: FileBody,
    /// 1-based line to put the cursor on once the editor shows.
    reveal: Option<u32>,
}

enum FileBody {
    Loading,
    Failed(String),
    /// Read, waiting for the next frame to build its editor.
    Read {
        text: String,
        version: FileVersion,
    },
    Ready(Box<FileEditor>),
}

struct FileEditor {
    state: Entity<EditorState>,
    _changes: Subscription,
    /// What the file on disk was when last read or saved; a save must still match it.
    version: FileVersion,
    /// The text last read or saved. The file is dirty while the editor differs.
    saved: Rope,
    dirty: bool,
    /// The text being saved, until the daemon answers.
    saving: Option<Rope>,
    /// The tab's close button was pressed once with unsaved changes.
    discard_armed: bool,
}

/// The GPUI Kit colors this window last installed; see `sync_editor_theme`.
#[derive(PartialEq)]
struct AppliedEditorTheme {
    colors: ThemeColors,
    dark: bool,
    ui_font: &'static str,
}

impl gpui::Global for AppliedEditorTheme {}

impl PaneUi {
    fn new(pane_id: &PaneId, environment: ContentEnvironment, root: String, cx: &mut App) -> Self {
        Self {
            focus: cx.focus_handle(),
            environment,
            tree: FileTree::new(pane_id.clone(), root),
            tree_scroll: ScrollHandle::new(),
            sidebar: false,
            notice: None,
            files: HashMap::new(),
        }
    }

    fn root(&self) -> &str {
        &self.tree.rows[0].path
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

    fn editor(&self, path: &str) -> Option<&FileEditor> {
        match &self.files.get(path)?.body {
            FileBody::Ready(editor) => Some(editor),
            _ => None,
        }
    }

    fn editor_mut(&mut self, path: &str) -> Option<&mut FileEditor> {
        match &mut self.files.get_mut(path)?.body {
            FileBody::Ready(editor) => Some(editor),
            _ => None,
        }
    }
}

/// The content `pane` shows, if it is a content pane.
pub(in crate::gui) fn find_content<'a>(
    workspace: &'a WorkspaceSnapshot,
    pane: &PaneId,
) -> Option<&'a PaneContent> {
    workspace
        .sessions
        .iter()
        .flat_map(|session| &session.tabs)
        .find_map(|tab| match leaf_node(&tab.layout, pane)? {
            LayoutNode::Content { content, .. } => Some(content),
            _ => None,
        })
}

fn content_leaves<'a>(node: &'a LayoutNode, visit: &mut impl FnMut(&'a PaneId, &'a PaneContent)) {
    match node {
        LayoutNode::Content { pane_id, content } => visit(pane_id, content),
        LayoutNode::Pane { .. } => {}
        LayoutNode::Split { first, second, .. } => {
            content_leaves(first, visit);
            content_leaves(second, visit);
        }
    }
}

/// The content pane in `layout` to show `file` (or, without one, the folder `root`) in:
/// one in `environment` whose root holds the file, preferring `beside` itself, then one
/// with the file open, then the deepest root.
fn reusable_content<'a>(
    layout: &'a LayoutNode,
    beside: &PaneId,
    environment: &ContentEnvironment,
    root: Option<&str>,
    file: Option<&str>,
) -> Option<(&'a PaneId, &'a PaneContent)> {
    let mut best: Option<((bool, bool, usize), &PaneId, &PaneContent)> = None;
    content_leaves(layout, &mut |pane_id, content| {
        if &content.environment != environment {
            return;
        }
        let rank = match (file, root) {
            (Some(file), _) => root_contains(&content.root, file).then(|| {
                (
                    content.files.iter().any(|open| open == file),
                    content.root.len(),
                )
            }),
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

fn same_folder(left: &str, right: &str) -> bool {
    left.trim_end_matches(['/', '\\']) == right.trim_end_matches(['/', '\\'])
}

/// The folder holding `path`: `/` and `C:\` keep their separator.
fn parent_folder(path: &str) -> String {
    match path.rfind(['/', '\\']) {
        Some(0) => path[..1].to_owned(),
        Some(index) if path[..index].ends_with(':') => path[..=index].to_owned(),
        Some(index) => path[..index].to_owned(),
        None => path.to_owned(),
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// The highlighter language for `path`, by file name and then extension.
fn language_for(path: &str) -> &'static str {
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
fn reveal_in_editor(
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

fn file_editor(
    pane_id: &PaneId,
    path: &str,
    text: String,
    version: FileVersion,
    window: &mut Window,
    cx: &mut Context<CompiApp>,
) -> FileEditor {
    let state = cx.new(|cx| {
        EditorState::new(window, cx)
            .language(language_for(path))
            .soft_wrap(false)
            .default_value(text)
    });
    let saved = state.read(cx).text().clone();
    let (pane_id, path) = (pane_id.clone(), path.to_owned());
    let changes = cx.subscribe(&state, move |this, editor, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change) {
            this.editor_changed(&pane_id, &path, &editor, cx);
        }
    });
    FileEditor {
        state,
        _changes: changes,
        version,
        saved,
        dirty: false,
        saving: None,
        discard_armed: false,
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
        theme.input = color(colors.border);
        theme.ring = color(colors.accent);
        theme.selection = color(colors.selection);
        theme.caret = color(colors.accent);
        theme.accent = color(colors.surface_hover);
        theme.accent_foreground = color(colors.foreground);
        theme.muted = color(colors.surface);
        theme.muted_foreground = color(colors.muted);
        theme.popover = color(colors.surface);
        theme.popover_foreground = color(colors.foreground);
        theme.list_hover = color(colors.surface_hover);
        theme.primary = color(colors.accent);
        theme.primary_hover = color(colors.accent);
        theme.primary_foreground = color(colors.background);
        theme.secondary = color(colors.surface);
        theme.secondary_hover = color(colors.surface_hover);
        theme.secondary_foreground = color(colors.foreground);
        theme.danger = color(colors.error);
        theme.scrollbar = gpui::transparent_black();
        theme.scrollbar_thumb = color(colors.muted).opacity(0.4);
        theme.scrollbar_thumb_hover = color(colors.muted);
        theme.tokens = ThemeTokens::from(&theme.colors);
        let mut highlight = if applied.dark {
            HighlightTheme::default_dark()
        } else {
            HighlightTheme::default_light()
        }
        .as_ref()
        .clone();
        // The pane's material shows through; the gutter and current line stay subtle.
        highlight.style.editor_background = Some(gpui::transparent_black());
        highlight.style.editor_gutter_background = None;
        highlight.style.editor_foreground = Some(color(colors.foreground));
        highlight.style.editor_line_number = Some(color(colors.muted));
        highlight.style.editor_active_line_number = Some(color(colors.foreground));
        highlight.style.editor_active_line = Some(color(colors.surface).opacity(0.6));
        theme.highlight_theme = Arc::new(highlight);
        Theme::sync_base(cx);
        cx.set_global(applied);
    }

    /// Before drawing: state for every visible content pane, a read for each shown file
    /// not yet loaded, editors for files just read, and pending line reveals.
    pub(super) fn prepare_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut work = Vec::new();
        if let Some(workspace) = &self.workspace {
            let visible = self
                .visible_layout()
                .into_iter()
                .flat_map(|layout| &layout.panes)
                .chain(self.float_layouts.iter().map(|float| &float.pane))
                .filter(|pane| pane.surface_id.is_none());
            for pane in visible {
                let Some(content) = find_content(workspace, &pane.pane_id) else {
                    continue;
                };
                let ui = self.content.panes.get(&pane.pane_id);
                let current = ui.is_some_and(|ui| {
                    ui.environment == content.environment && ui.root() == content.root
                });
                let shown = content
                    .active_file
                    .as_ref()
                    .is_none_or(|active| ui.is_some_and(|ui| ui.files.contains_key(active)));
                if !current || !shown {
                    work.push((pane.pane_id.clone(), content.clone()));
                }
            }
        }
        for (pane_id, content) in work {
            self.prepare_pane(pane_id, content, cx);
        }
        if std::mem::take(&mut self.content.unbuilt) {
            for (pane_id, ui) in &mut self.content.panes {
                for (path, file) in &mut ui.files {
                    file.body = match std::mem::replace(&mut file.body, FileBody::Loading) {
                        FileBody::Read { text, version } => FileBody::Ready(Box::new(file_editor(
                            pane_id, path, text, version, window, cx,
                        ))),
                        body => body,
                    };
                }
            }
        }
        self.apply_reveals(window, cx);
    }

    fn prepare_pane(&mut self, pane_id: PaneId, content: PaneContent, cx: &mut Context<Self>) {
        let relist = match self.content.panes.get_mut(&pane_id) {
            Some(ui) if ui.environment != content.environment => {
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
        if relist {
            self.request_content_listing(
                pane_id.clone(),
                content.environment.clone(),
                content.root.clone(),
                content.root.clone(),
            );
        }
        let Some(active) = content.active_file else {
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
        self.request_content_read(pane_id, content.environment, active);
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
                find_content(workspace, pane_id).and_then(|content| content.active_file.as_ref())
                && ui
                    .files
                    .get(active)
                    .is_some_and(|file| file.reveal.is_some())
            {
                pending.push((pane_id.clone(), active.clone()));
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

    /// Drops state of panes and files the workspace no longer has.
    pub(super) fn prune_content(&mut self) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        self.content
            .panes
            .retain(|pane_id, ui| match find_content(workspace, pane_id) {
                Some(content) => {
                    ui.files.retain(|path, _| content.files.contains(path));
                    true
                }
                None => false,
            });
    }

    /// Open files in `pane_id` with unsaved changes.
    pub(super) fn unsaved_files(&self, pane_id: &PaneId) -> usize {
        self.content.panes.get(pane_id).map_or(0, |ui| {
            ui.files
                .values()
                .filter(|file| matches!(&file.body, FileBody::Ready(editor) if editor.dirty))
                .count()
        })
    }

    /// Keyboard focus follows the focused pane: a content pane keeps it on its editor
    /// (or on the pane while it has no editor, or the user left it), while terminals,
    /// overlays and the file tree use `focus_handle`, which carries their text input.
    /// Focus held by anything else is left alone.
    pub(super) fn reconcile_keyboard_focus(&mut self, window: &mut Window, cx: &mut App) {
        let ours = window.focused(cx).is_none_or(|focused| {
            focused == self.focus_handle
                || self
                    .content
                    .panes
                    .values()
                    .any(|ui| ui.owns(&focused, window, cx))
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
            Some(pane_id) => {
                let Some(ui) = self.content.panes.get(pane_id) else {
                    return;
                };
                if ui.focus.contains_focused(window, cx) && !ui.focus.is_focused(window) {
                    // The editor or its search field already has the keyboard.
                    return;
                }
                self.focused_content()
                    .and_then(|content| content.active_file.as_deref())
                    .filter(|_| self.content.released.as_ref() != Some(pane_id))
                    .and_then(|active| ui.editor(active))
                    .map_or_else(|| ui.focus.clone(), |editor| editor.state.focus_handle(cx))
            }
            None => self.focus_handle.clone(),
        };
        if !wanted.is_focused(window) {
            window.focus(&wanted, cx);
        }
    }

    /// The focused pane when it is a content pane that should have the keyboard: no
    /// overlay or terminal file tree is open.
    fn keyboard_content_pane(&self) -> Option<&PaneId> {
        self.focused_pane().filter(|_| {
            self.overlay.is_none() && self.file_tree.is_none() && self.focused_content().is_some()
        })
    }

    /// The focused content pane's editor or its search field has the keyboard.
    pub(super) fn editor_has_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.overlay.is_none()
            && self
                .focused_pane()
                .and_then(|pane| self.content.panes.get(pane))
                .is_some_and(|ui| {
                    !ui.focus.is_focused(window) && ui.focus.contains_focused(window, cx)
                })
    }

    /// Runs before the editor's own key bindings, so Compi shortcuts win over editor
    /// bindings on the same keys; Ctrl/Cmd+S saves. Everything else goes to the editor.
    pub(super) fn intercept_editor_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.editor_has_keyboard(window, cx) {
            return;
        }
        if is_save_key(key) {
            self.save_focused_file(cx);
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

    /// Opens `file` (or, without one, the folder `root`) in a content pane of `beside`'s
    /// tab: one in `environment` whose root holds the file, unless `new_pane`; else a new
    /// pane split beside `beside`, rooted at `root` or the file's folder. The file becomes
    /// the active one, with the cursor on the 1-based `line`.
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
                let listed = content.files.contains(file);
                if !listed || content.active_file.as_ref() != Some(file) {
                    if !listed {
                        content.files.push(file.clone());
                    }
                    content.active_file = Some(file.clone());
                    self.mutate(
                        WorkspaceMutation::UpdateContent {
                            pane_id: pane_id.clone(),
                            content,
                        },
                        false,
                    );
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
        let Some(layout) = &self.layout else {
            return;
        };
        let kind = layout::PaneKind::Content;
        let (rect, minimum) = match layout
            .split_feasibility(beside, SplitAxis::Horizontal, kind)
            .and_then(|rect| {
                layout
                    .split_minimum(beside, kind)
                    .map(|minimum| (rect, minimum))
                    .ok_or("The pane to open files beside no longer exists")
            }) {
            Ok(fit) => fit,
            Err(reason) => {
                self.global_error = Some(reason.into());
                return;
            }
        };
        if let (Some(path), Some(line)) = (&file, line) {
            self.content.pending_reveal = Some(PendingReveal {
                environment: environment.clone(),
                path: path.clone(),
                line,
            });
        }
        let divider = self.metrics().divider_thickness;
        self.mutate(
            WorkspaceMutation::OpenContent {
                pane_id: beside.clone(),
                axis: SplitAxis::Horizontal,
                geometry: compi_protocol::SplitGeometry {
                    width: rect.width,
                    height: rect.height,
                    min_width: minimum.width,
                    min_height: minimum.height,
                    divider,
                },
                content: PaneContent {
                    environment,
                    root,
                    files: file.iter().cloned().collect(),
                    active_file: file,
                },
            },
            true,
        );
        cx.notify();
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
            .and_then(|content| content.active_file.as_deref())
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

    fn update_content(&mut self, pane_id: &PaneId, content: PaneContent) {
        self.mutate(
            WorkspaceMutation::UpdateContent {
                pane_id: pane_id.clone(),
                content,
            },
            false,
        );
    }

    fn activate_content_file(&mut self, pane_id: &PaneId, path: &str) {
        let Some(mut content) = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane_id))
            .filter(|content| content.active_file.as_deref() != Some(path))
            .cloned()
        else {
            return;
        };
        content.active_file = Some(path.to_owned());
        self.content.released = None;
        self.update_content(pane_id, content);
    }

    /// Closes the tab of `path`. Unsaved changes take a second click to discard.
    fn close_content_file(&mut self, pane_id: &PaneId, path: &str) {
        let Some(mut content) = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane_id))
            .cloned()
        else {
            return;
        };
        let Some(index) = content.files.iter().position(|file| file == path) else {
            return;
        };
        if let Some(ui) = self.content.panes.get_mut(pane_id)
            && let Some(editor) = ui.editor_mut(path)
            && editor.dirty
            && !editor.discard_armed
        {
            editor.discard_armed = true;
            ui.notice = Some((
                path.to_owned(),
                format!(
                    "{} has unsaved changes. Save with {SAVE_SHORTCUT}, or close its tab again to discard them.",
                    file_name(path)
                ),
            ));
            return;
        }
        content.files.remove(index);
        if content.active_file.as_deref() == Some(path) {
            content.active_file = content
                .files
                .get(index)
                .or_else(|| content.files.get(index.wrapping_sub(1)))
                .cloned();
        }
        self.update_content(pane_id, content);
    }

    fn save_focused_file(&mut self, cx: &mut Context<Self>) {
        let Some((pane_id, environment, path)) = self.focused_pane().and_then(|pane| {
            let content = self.focused_content()?;
            Some((
                pane.clone(),
                content.environment.clone(),
                content.active_file.clone()?,
            ))
        }) else {
            return;
        };
        let Some(editor) = self
            .content
            .panes
            .get_mut(&pane_id)
            .and_then(|ui| ui.editor_mut(&path))
            .filter(|editor| editor.dirty && editor.saving.is_none())
        else {
            return;
        };
        let text = editor.state.read(cx).text().clone();
        editor.saving = Some(text.clone());
        let expected = editor.version.clone();
        let target = self.target.clone();
        let sender = self.event_tx.clone();
        thread::spawn(move || {
            let result = (|| -> crate::Result<_> {
                target.connect()?.write_content_file(
                    environment,
                    path.clone(),
                    Some(expected),
                    text.to_string(),
                )
            })()
            .map_err(|error| {
                match error.downcast_ref::<compi_protocol::DaemonError>() {
                    Some(error) if error.code == ErrorCode::FileConflict => {
                        "The file changed on disk after it was opened, so Compi did not overwrite it. Your edits are still here.".to_owned()
                    }
                    _ => error.to_string(),
                }
            });
            sender.send(UiEvent::ContentFileSaved {
                pane_id,
                path,
                result,
            });
        });
    }

    fn editor_changed(
        &mut self,
        pane_id: &PaneId,
        path: &str,
        state: &Entity<EditorState>,
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
        let dirty = state.read(cx).text() != &editor.saved;
        if dirty != editor.dirty {
            editor.dirty = dirty;
            editor.discard_armed &= dirty;
            cx.notify();
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

    fn request_content_read(&self, pane_id: PaneId, environment: ContentEnvironment, path: String) {
        let target = self.target.clone();
        let sender = self.event_tx.clone();
        thread::spawn(move || {
            let result = (|| -> crate::Result<_> {
                target
                    .connect()?
                    .read_content_file(environment, path.clone())
            })()
            .map_err(|error| error.to_string());
            sender.send(UiEvent::ContentFileRead {
                pane_id,
                path,
                result,
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
        result: Result<(String, FileVersion), String>,
    ) {
        let Some(file) = self
            .content
            .panes
            .get_mut(&pane_id)
            .and_then(|ui| ui.files.get_mut(&path))
            .filter(|file| matches!(file.body, FileBody::Loading))
        else {
            return;
        };
        file.body = match result {
            Ok((text, version)) => {
                self.content.unbuilt = true;
                FileBody::Read { text, version }
            }
            Err(error) => FileBody::Failed(error),
        };
    }

    pub(super) fn content_file_saved(
        &mut self,
        pane_id: PaneId,
        path: String,
        result: Result<FileVersion, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(ui) = self.content.panes.get_mut(&pane_id) else {
            return;
        };
        let Some(editor) = ui.editor_mut(&path) else {
            return;
        };
        let Some(saved) = editor.saving.take() else {
            return;
        };
        match result {
            Ok(version) => {
                editor.version = version;
                editor.dirty = editor.state.read(cx).text() != &saved;
                editor.discard_armed = false;
                editor.saved = saved;
                if ui.notice.as_ref().is_some_and(|(file, _)| *file == path) {
                    ui.notice = None;
                }
            }
            Err(error) => {
                let message = format!("{} was not saved: {error}", file_name(&path));
                ui.notice = Some((path, message));
            }
        }
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

    /// Files in a content pane: its folder, beside an open file as the sidebar, with the
    /// keyboard on the folder.
    pub(super) fn show_content_folder(
        &mut self,
        pane_id: &PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let file_open = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane_id))
            .is_some_and(|content| content.active_file.is_some());
        if let Some(ui) = self.content.panes.get_mut(pane_id) {
            ui.sidebar |= file_open;
        }
        self.release_editor(pane_id, window, cx);
    }

    /// The pane takes the keyboard from its editor until the editor is entered again.
    fn release_editor(&mut self, pane_id: &PaneId, window: &mut Window, cx: &mut App) {
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
        ui.tree_scroll.scroll_to_item(index);
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

    /// Folder navigation while the pane itself has the keyboard: arrows move and
    /// expand, Enter opens. With a file open and the folder hidden, Enter returns to
    /// the editor.
    fn content_tree_key(
        &mut self,
        pane_id: &PaneId,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = &key.modifiers;
        if modifiers.control || modifiers.platform || modifiers.alt || modifiers.shift {
            return false;
        }
        let file_open = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane_id))
            .is_some_and(|content| content.active_file.is_some());
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return false;
        };
        if file_open && !ui.sidebar {
            if key.key == "enter" {
                self.content.released = None;
                return true;
            }
            return false;
        }
        let tree = &mut ui.tree;
        match key.key.as_str() {
            "up" => tree.move_selection(-1),
            "down" => tree.move_selection(1),
            "right"
                if tree
                    .selected()
                    .is_some_and(|row| row.directory && !row.expanded) =>
            {
                let index = tree.selected;
                self.activate_content_row(pane_id, index, window, cx);
                return true;
            }
            "left" => {
                let selected = tree.selected;
                if tree.rows[selected].expanded {
                    tree.toggle(selected);
                } else if tree.rows[selected].depth > 0 {
                    tree.selected = (0..selected)
                        .rev()
                        .find(|index| tree.rows[*index].depth < tree.rows[selected].depth)
                        .unwrap_or(0);
                }
            }
            "enter" => {
                let index = tree.selected;
                self.activate_content_row(pane_id, index, window, cx);
                return true;
            }
            _ => return false,
        }
        ui.tree_scroll.scroll_to_item(ui.tree.selected);
        true
    }

    fn content_key_down(
        &mut self,
        pane_id: &PaneId,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ui) = self.content.panes.get(pane_id) else {
            return;
        };
        if !ui.focus.is_focused(window) {
            // A key the editor or its search field left alone. Text input still needs
            // it, so only Escape stops here: it hands the keyboard to the pane.
            if event.keystroke.key == "escape" {
                if !self.handle_app_key(event, window, cx) {
                    self.release_editor(pane_id, window, cx);
                }
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        // Compi shortcuts still run; every other key stops here, so nothing typed in
        // a content pane reaches a shell.
        if !self.content_tree_key(pane_id, &event.keystroke, window, cx) {
            self.handle_app_key(event, window, cx);
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
                cx.listener(move |this, _, _, cx| {
                    this.focus_pane(menu_id.clone());
                    this.open_overlay(Overlay::Palette, "");
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
                            .map(|body| match &content.active_file {
                                Some(active) => body
                                    .child(self.render_file_tabs(&pane_id, content, ui, cx))
                                    .when_some(ui.notice.as_ref(), |body, (_, notice)| {
                                        body.child(self.render_content_notice(&pane_id, notice, cx))
                                    })
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_h_0()
                                            .flex()
                                            .when(ui.sidebar, |row| {
                                                row.child(
                                                    div()
                                                        .w(px(SIDEBAR_WIDTH.min(rect.width / 2.0)))
                                                        .flex_none()
                                                        .h_full()
                                                        .flex()
                                                        .flex_col()
                                                        .border_r_1()
                                                        .border_color(color(colors.border))
                                                        .child(self.render_content_tree(
                                                            &pane_id,
                                                            ui,
                                                            Some(active),
                                                            cx,
                                                        )),
                                                )
                                            })
                                            .child(self.render_file_body(&pane_id, active, ui, cx)),
                                    ),
                                None => {
                                    body.child(self.render_folder_view(&pane_id, content, ui, cx))
                                }
                            }),
                    )
                },
            )
    }

    fn render_folder_view(
        &self,
        pane_id: &PaneId,
        content: &PaneContent,
        ui: &PaneUi,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .border_b_1()
                    .border_color(color(colors.border))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
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
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(UI_MICRO_TEXT_SIZE))
                                    .text_color(color(colors.muted))
                                    .child(content.environment.to_string()),
                            ),
                    )
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(color(colors.muted))
                            .child(content.root.clone()),
                    ),
            )
            .child(self.render_content_tree(pane_id, ui, None, cx))
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .text_color(color(colors.muted))
                    .child("Click a file to open it · ↑↓ select · ←→ fold · Enter open"),
            )
    }

    fn render_content_tree(
        &self,
        pane_id: &PaneId,
        ui: &PaneUi,
        active: Option<&str>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        let rows = ui.tree.rows.iter().enumerate().map(|(index, row)| {
            let selected = index == ui.tree.selected;
            let open = active == Some(row.path.as_str());
            let folder = row.directory;
            let row_pane = pane_id.clone();
            let tint = if selected || open {
                colors.foreground
            } else if folder {
                colors.accent
            } else {
                colors.muted
            };
            div()
                .id(("content-tree-row", index))
                .h(px(TREE_ROW_HEIGHT))
                .flex_none()
                .pl(px(6.0 + row.depth.min(31) as f32 * 14.0))
                .pr_2()
                .flex()
                .items_center()
                .whitespace_nowrap()
                .cursor_pointer()
                .when(selected, |entry| entry.bg(color(colors.selection)))
                .when(!selected, |entry| {
                    entry.hover(move |style| style.bg(color(colors.surface_hover)))
                })
                .when(open, |entry| entry.text_color(color(colors.accent)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        if folder {
                            this.release_editor(&row_pane, window, cx);
                        }
                        this.activate_content_row(&row_pane, index, window, cx);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .w(px(14.0))
                        .flex_none()
                        .text_color(color(colors.muted))
                        .child(match (folder, row.expanded) {
                            (false, _) => "",
                            (true, true) => "▾",
                            (true, false) => "▸",
                        }),
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
        });
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .text_size(px(UI_SMALL_TEXT_SIZE))
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
                    .py_1()
                    .overflow_y_scroll()
                    .track_scroll(&ui.tree_scroll)
                    .children(rows),
            )
    }

    fn render_file_tabs(
        &self,
        pane_id: &PaneId,
        content: &PaneContent,
        ui: &PaneUi,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        let toggle_pane = pane_id.clone();
        let tabs = content.files.iter().enumerate().map(|(index, path)| {
            let active = content.active_file.as_ref() == Some(path);
            let dirty = ui.editor(path).is_some_and(|editor| editor.dirty);
            let (open_pane, open_path) = (pane_id.clone(), path.clone());
            let (close_pane, close_path) = (pane_id.clone(), path.clone());
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
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.activate_content_file(&open_pane, &open_path);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(file_name(path).to_owned()),
                )
                .when(dirty, |tab| {
                    tab.child(
                        div()
                            .flex_none()
                            .text_color(color(colors.accent))
                            .child("●"),
                    )
                })
                .child(
                    div()
                        .id(("content-file-close", index))
                        .flex_none()
                        .w(px(18.0))
                        .h(px(18.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .text_color(color(colors.muted))
                        .hover(move |style| {
                            style
                                .bg(color(colors.surface_hover))
                                .text_color(color(colors.foreground))
                        })
                        .child("×")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.close_content_file(&close_pane, &close_path);
                                cx.stop_propagation();
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
                    .id("content-sidebar-toggle")
                    .flex_none()
                    .w(px(FILE_TAB_HEIGHT))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_r_1()
                    .border_color(color(colors.border))
                    .cursor_pointer()
                    .hover(move |style| style.bg(color(colors.surface_hover)))
                    .child(tree_entry_icon(
                        true,
                        ui.sidebar,
                        color(if ui.sidebar {
                            colors.accent
                        } else {
                            colors.muted
                        }),
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.toggle_content_sidebar(&toggle_pane);
                            cx.stop_propagation();
                            cx.notify();
                        }),
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
            .text_color(color(colors.error))
            .child(div().flex_1().min_w_0().child(notice.to_owned()))
            .child(
                div()
                    .id("content-notice-dismiss")
                    .flex_none()
                    .cursor_pointer()
                    .child("×")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            if let Some(ui) = this.content.panes.get_mut(&dismiss_pane) {
                                ui.notice = None;
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    ),
            )
    }

    fn render_file_body(
        &self,
        pane_id: &PaneId,
        path: &str,
        ui: &PaneUi,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        let area = div().flex_1().min_w_0().h_full();
        match ui.files.get(path).map(|file| &file.body) {
            Some(FileBody::Ready(editor)) => area
                // The editor's own context menu, not the palette, answers a right click.
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .child(
                    Editor::new(&editor.state)
                        .appearance(false)
                        .bordered(false)
                        .font_family(self.typography.font.family.clone())
                        .text_size(px(self.typography.font_size))
                        .size_full(),
                )
                .into_any_element(),
            Some(FileBody::Failed(error)) => {
                let (retry_pane, retry_path) = (pane_id.clone(), path.to_owned());
                area.p_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .child(format!("Cannot open {}", file_name(path)))
                    .child(div().text_color(color(colors.error)).child(error.clone()))
                    .child(
                        div()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(color(colors.muted))
                            .child(path.to_owned()),
                    )
                    .child(
                        div()
                            .id("content-file-retry")
                            .mt_1()
                            .cursor_pointer()
                            .text_color(color(colors.accent))
                            .child("Try again")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.retry_content_file(&retry_pane, &retry_path);
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            ),
                    )
                    .into_any_element()
            }
            Some(FileBody::Loading | FileBody::Read { .. }) | None => area
                .p_3()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .text_color(color(colors.muted))
                .child(format!("Opening {}…", file_name(path)))
                .into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            content: PaneContent {
                environment: wsl.clone(),
                root: root.into(),
                files: files.iter().map(|file| (*file).into()).collect(),
                active_file: None,
            },
        };
        let split = |first, second| LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            ratio: 0.5,
            first: Box::new(first),
            second: Box::new(second),
        };
        let layout = split(
            pane("outer", "/work", &[]),
            split(
                pane("inner", "/work/src", &[]),
                pane("open", "/work", &["/work/src/a.rs"]),
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
    }
}
