//! One source of command metadata, enablement, and terminal-safe shortcut routing.
use compi_protocol::SurfaceStatus;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Mac,
    Windows,
    Linux,
}

impl Platform {
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CommandSpec {
    pub command: Command,
    pub id: &'static str,
    pub label: &'static str,
    pub mac_shortcut: Option<&'static str>,
    pub windows_shortcut: Option<&'static str>,
}

macro_rules! registry {
    ($( $command:ident, $id:literal, $label:literal, $mac:expr, $windows:expr; )*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Command { $( $command, )* }
        pub static REGISTRY: &[CommandSpec] = &[
            $(CommandSpec { command: Command::$command, id: $id, label: $label, mac_shortcut: $mac, windows_shortcut: $windows },)*
        ];
    };
}

registry! {
    OpenPalette, "open_palette", "Open command palette", Some("cmd-shift-p"), Some("ctrl-shift-p");
    CreateWorkspace, "create_workspace", "Create workspace", Some("cmd-alt-n"), Some("ctrl-shift-alt-n");
    SwitchWorkspace, "switch_workspace", "Switch workspace", None, None;
    RenameWorkspace, "rename_workspace", "Rename workspace", None, None;
    RemoveWorkspace, "remove_workspace", "Remove workspace…", None, None;
    NewTab, "new_tab", "New terminal tab", Some("cmd-t"), Some("ctrl-shift-t");
    SwitchTab, "switch_tab", "Switch terminal tab", None, None;
    PreviousTab, "previous_tab", "Previous terminal tab", Some("cmd-shift-left"), Some("ctrl-shift-pageup");
    NextTab, "next_tab", "Next terminal tab", Some("cmd-shift-right"), Some("ctrl-shift-pagedown");
    RenameTab, "rename_tab", "Rename terminal tab", None, None;
    MoveTabLeft, "move_tab_left", "Move terminal tab left", Some("cmd-alt-shift-left"), Some("ctrl-shift-alt-left");
    MoveTabRight, "move_tab_right", "Move terminal tab right", Some("cmd-alt-shift-right"), Some("ctrl-shift-alt-right");
    RemoveTab, "remove_tab", "Remove terminal tab…", None, None;
    DetachTab, "detach_tab", "Hide terminal tab (keep processes running)", Some("cmd-w"), Some("ctrl-shift-w");
    RestoreHiddenTab, "restore_hidden_tab", "Restore hidden terminal tab", Some("cmd-shift-t"), None;
    NewWindow, "new_window", "New window", Some("cmd-n"), Some("ctrl-shift-n");
    MoveTabToNewWindow, "move_tab_to_new_window", "Move terminal tab to new window", None, None;
    MoveTabToWindow, "move_tab_to_window", "Move terminal tab to another window", None, None;
    SplitRight, "split_right", "Split right", Some("cmd-d"), Some("ctrl-shift-d");
    SplitDown, "split_down", "Split down", Some("cmd-shift-d"), Some("ctrl-shift-e");
    FocusLeft, "focus_left", "Focus pane left", Some("cmd-alt-left"), Some("ctrl-shift-left");
    FocusRight, "focus_right", "Focus pane right", Some("cmd-alt-right"), Some("ctrl-shift-right");
    FocusUp, "focus_up", "Focus pane above", Some("cmd-alt-up"), Some("ctrl-shift-up");
    FocusDown, "focus_down", "Focus pane below", Some("cmd-alt-down"), Some("ctrl-shift-down");
    ResizeSplitDecrease, "resize_split_decrease", "Move divider toward first pane", Some("cmd-alt-minus"), Some("ctrl-shift-alt-minus");
    ResizeSplitIncrease, "resize_split_increase", "Move divider toward second pane", Some("cmd-alt-equal"), Some("ctrl-shift-alt-equal");
    ResetSplitRatio, "reset_split_ratio", "Equalize focused split", None, None;
    RemovePane, "remove_pane", "Remove pane…", None, None;
    EndSurface, "end_surface", "End surface process…", None, None;
    RestartSurface, "restart_surface", "Restart exited, failed, or lost surface", None, None;
    ToggleSidebar, "toggle_sidebar", "Show/hide workspace sidebar", Some("cmd-b"), Some("ctrl-shift-b");
    ResetSidebarWidth, "reset_sidebar_width", "Reset sidebar width", None, None;
    Copy, "copy", "Copy selection", Some("cmd-c"), Some("ctrl-shift-c");
    Paste, "paste", "Paste", Some("cmd-v"), Some("ctrl-shift-v");
    SelectAll, "select_all", "Select all terminal text", Some("cmd-a"), Some("ctrl-shift-a");
    ClearScrollback, "clear_scrollback", "Clear scrollback", Some("cmd-k"), Some("ctrl-shift-k");
    ZoomIn, "zoom_in", "Increase font size", Some("cmd-equal"), Some("ctrl-shift-equal");
    ZoomOut, "zoom_out", "Decrease font size", Some("cmd-minus"), Some("ctrl-shift-minus");
    ZoomReset, "zoom_reset", "Reset font size", Some("cmd-0"), Some("ctrl-shift-0");
    ChangeTheme, "change_theme", "Change theme…", None, None;
    OpenConfiguration, "open_configuration", "Open configuration", Some("cmd-,"), Some("ctrl-shift-,");
    ResetClientLayout, "reset_client_layout", "Reset client layout", None, None;
    Reconnect, "reconnect", "Reconnect to server", None, None;
    OpenDiagnostics, "open_diagnostics", "Open diagnostics", None, None;
    Quit, "quit", "Quit client (keep server running)", Some("cmd-q"), Some("ctrl-shift-q");
}

impl CommandSpec {
    pub fn shortcut(&self, platform: Platform) -> Option<&'static str> {
        match platform {
            Platform::Mac => self.mac_shortcut,
            Platform::Windows | Platform::Linux => self.windows_shortcut,
        }
    }
    /// Every query word must occur in the human label or stable command ID.
    pub fn matches_query(&self, query: &str) -> bool {
        query.split_whitespace().all(|word| {
            contains_ignore_case(self.label, word) || contains_ignore_case(self.id, word)
        })
    }
    pub fn disabled_reason(&self, context: &CommandContext) -> Option<&'static str> {
        self.command.disabled_reason(context)
    }
}

fn contains_ignore_case(text: &str, needle: &str) -> bool {
    text.as_bytes()
        .windows(needle.len())
        .any(|part| part.eq_ignore_ascii_case(needle.as_bytes()))
}

pub fn by_id(id: &str) -> Option<&'static CommandSpec> {
    REGISTRY.iter().find(|spec| spec.id == id)
}

pub fn search(query: &str) -> impl Iterator<Item = &'static CommandSpec> + '_ {
    REGISTRY
        .iter()
        .filter(move |spec| spec.matches_query(query))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CommandContext {
    pub connected: bool,
    pub workspace_count: usize,
    pub tab_count: usize,
    pub hidden_tab_count: usize,
    pub pane_count: usize,
    pub has_workspace: bool,
    pub has_tab: bool,
    pub has_pane: bool,
    pub tab_index: usize,
    pub has_selection: bool,
    pub terminal_available: bool,
    pub can_paste: bool,
    pub surface_status: Option<SurfaceStatus>,
    pub mutation_pending: bool,
    pub transfer_in_progress: bool,
    pub other_window_available: bool,
    /// Revision at which the current command targets/context were captured.
    pub revision: u64,
    pub current_revision: u64,
    pub split_right_reason: Option<&'static str>,
    pub split_down_reason: Option<&'static str>,
    pub resize_reason: Option<&'static str>,
    pub focus_left: bool,
    pub focus_right: bool,
    pub focus_up: bool,
    pub focus_down: bool,
}

impl Command {
    pub fn spec(self) -> &'static CommandSpec {
        REGISTRY
            .iter()
            .find(|spec| spec.command == self)
            .expect("all commands belong to the registry")
    }
    pub fn disabled_reason(self, c: &CommandContext) -> Option<&'static str> {
        use Command::*;
        let local = matches!(
            self,
            OpenPalette
                | NewWindow
                | ToggleSidebar
                | ResetSidebarWidth
                | ZoomIn
                | ZoomOut
                | ZoomReset
                | ChangeTheme
                | OpenConfiguration
                | ResetClientLayout
                | Reconnect
                | OpenDiagnostics
                | Quit
        );
        if local {
            return None;
        }
        if c.revision != c.current_revision {
            return Some("Workspace changed; select the command again");
        }
        let mutation = matches!(
            self,
            CreateWorkspace
                | RenameWorkspace
                | RemoveWorkspace
                | NewTab
                | RenameTab
                | MoveTabLeft
                | MoveTabRight
                | RemoveTab
                | SplitRight
                | SplitDown
                | ResizeSplitDecrease
                | ResizeSplitIncrease
                | ResetSplitRatio
                | RemovePane
                | EndSurface
                | RestartSurface
        );
        if !c.connected
            && (mutation
                || matches!(
                    self,
                    Paste | ClearScrollback | MoveTabToNewWindow | MoveTabToWindow
                ))
        {
            return Some("Reconnect to the server first");
        }
        if mutation && c.mutation_pending {
            return Some("Wait for the current workspace change to finish");
        }
        if c.transfer_in_progress && !matches!(self, Copy | SelectAll) {
            return Some("Wait for the terminal tab transfer to finish");
        }
        match self {
            CreateWorkspace => None,
            SwitchWorkspace => (c.workspace_count == 0).then_some("No workspaces are available"),
            RenameWorkspace | RemoveWorkspace | NewTab => {
                (!c.has_workspace).then_some("Select a workspace first")
            }
            RestoreHiddenTab => {
                (c.hidden_tab_count == 0).then_some("No hidden terminal tabs are available")
            }
            SwitchTab => {
                (!c.has_workspace || c.tab_count == 0).then_some("No terminal tabs are available")
            }
            PreviousTab | NextTab => {
                (!c.has_tab || c.tab_count < 2).then_some("There is no other terminal tab")
            }
            RenameTab | RemoveTab | DetachTab | MoveTabToNewWindow => {
                (!c.has_tab).then_some("Select a terminal tab first")
            }
            MoveTabToWindow => {
                if !c.has_tab {
                    Some("Select a terminal tab first")
                } else {
                    (!c.other_window_available)
                        .then_some("No other window for this server is available")
                }
            }
            MoveTabLeft => {
                if !c.has_tab {
                    Some("Select a terminal tab first")
                } else {
                    (c.tab_index == 0 || c.tab_index >= c.tab_count)
                        .then_some("This is the first terminal tab")
                }
            }
            MoveTabRight => {
                if !c.has_tab {
                    Some("Select a terminal tab first")
                } else {
                    (c.tab_index.saturating_add(1) >= c.tab_count)
                        .then_some("This is the last terminal tab")
                }
            }
            SplitRight | SplitDown => {
                if !c.has_pane {
                    Some("Select a pane first")
                } else if self == SplitRight {
                    c.split_right_reason
                } else {
                    c.split_down_reason
                }
            }
            FocusLeft | FocusRight | FocusUp | FocusDown => {
                let available = match self {
                    FocusLeft => c.focus_left,
                    FocusRight => c.focus_right,
                    FocusUp => c.focus_up,
                    _ => c.focus_down,
                };
                (!c.has_pane || !available).then_some("No pane in that direction")
            }
            ResizeSplitDecrease | ResizeSplitIncrease | ResetSplitRatio => {
                if !c.has_pane || c.pane_count < 2 {
                    Some("The focused pane has no split")
                } else {
                    c.resize_reason
                }
            }
            RemovePane => (!c.has_pane).then_some("Select a pane first"),
            EndSurface => {
                if !c.has_pane {
                    Some("Select a pane first")
                } else if matches!(
                    c.surface_status,
                    Some(SurfaceStatus::Starting | SurfaceStatus::Running | SurfaceStatus::Ending)
                ) {
                    None
                } else {
                    Some("The surface has no live process")
                }
            }
            RestartSurface => {
                if !c.has_pane {
                    Some("Select a pane first")
                } else if matches!(
                    c.surface_status,
                    Some(SurfaceStatus::Exited | SurfaceStatus::Failed | SurfaceStatus::Lost)
                ) {
                    None
                } else {
                    Some("Only exited, failed, or lost surfaces can be restarted")
                }
            }
            Copy => (!c.has_selection).then_some("Select terminal text to copy"),
            Paste => {
                if !c.has_pane
                    || c.surface_status != Some(SurfaceStatus::Running)
                    || !c.terminal_available
                {
                    Some("The focused terminal is not ready for input")
                } else {
                    (!c.can_paste).then_some("Clipboard text is unavailable or paste is disabled")
                }
            }
            SelectAll | ClearScrollback => {
                (!c.has_pane || !c.terminal_available).then_some("No terminal text is available")
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ShortcutKey<'a> {
    pub key: &'a str,
    pub control: bool,
    pub shift: bool,
    pub alt: bool,
    pub command: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputOwner {
    Terminal,
    Palette,
    Menu,
    ThemePicker,
    TextField,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRoute {
    /// Forward to the existing terminal encoder/native composed-text path.
    Terminal,
    /// The focused palette, menu, picker, or text field owns this event.
    Owned,
    Command(Command),
    /// Consume a recognized disabled shortcut; never leak it to the shell.
    Disabled(Command, &'static str),
}

fn route_command(command: Command, context: &CommandContext) -> KeyRoute {
    match command.disabled_reason(context) {
        Some(reason) => KeyRoute::Disabled(command, reason),
        None => KeyRoute::Command(command),
    }
}

/// Explicit user bindings take precedence over defaults and selection-aware Ctrl+C.
/// Empty overrides unbind commands. Resolve collisions in stable registry order,
/// never HashMap iteration order. Focused overlays own all their keyboard input.
pub fn resolve_key(
    platform: Platform,
    key: ShortcutKey<'_>,
    owner: InputOwner,
    context: &CommandContext,
    overrides: &HashMap<String, String>,
) -> KeyRoute {
    if owner != InputOwner::Terminal {
        return KeyRoute::Owned;
    }
    for spec in REGISTRY {
        if let Some(binding) = overrides.get(spec.id)
            && shortcut_matches(binding, key)
        {
            return route_command(spec.command, context);
        }
    }
    if platform == Platform::Windows
        && key.control
        && !key.shift
        && !key.alt
        && !key.command
        && key.key.eq_ignore_ascii_case("c")
    {
        return if context.has_selection && !overrides.contains_key("copy") {
            route_command(Command::Copy, context)
        } else {
            KeyRoute::Terminal
        };
    }
    for spec in REGISTRY {
        if overrides.contains_key(spec.id) {
            continue;
        }
        if spec
            .shortcut(platform)
            .is_some_and(|binding| shortcut_matches(binding, key))
        {
            return route_command(spec.command, context);
        }
    }
    KeyRoute::Terminal
}

/// Accepts `cmd-shift-p` and `Ctrl+Shift+P`. Use `minus`, `equal`, or `plus`
/// for punctuation that is otherwise a separator. An empty binding disables it.
pub fn validate_shortcut(binding: &str) -> Result<(), &'static str> {
    if binding.trim().is_empty() {
        return Ok(());
    }
    parse_shortcut(binding).map(|_| ())
}

pub fn shortcut_matches(binding: &str, key: ShortcutKey<'_>) -> bool {
    let Ok(parsed) = parse_shortcut(binding) else {
        return false;
    };
    parsed.control == key.control
        && parsed.shift == key.shift
        && parsed.alt == key.alt
        && parsed.command == key.command
        && normalized_key(parsed.key).eq_ignore_ascii_case(normalized_key(key.key))
}

fn normalized_key(key: &str) -> &str {
    if key.eq_ignore_ascii_case("return") {
        return "enter";
    }
    if key.eq_ignore_ascii_case("esc") {
        return "escape";
    }
    match key {
        "-" => "minus",
        "=" => "equal",
        "+" => "plus",
        " " => "space",
        _ => key,
    }
}

fn parse_shortcut(binding: &str) -> Result<ShortcutKey<'_>, &'static str> {
    let mut parsed = ShortcutKey::default();
    let mut parts = binding.trim().split(['-', '+']).peekable();
    while let Some(part) = parts.next() {
        let part = part.trim();
        if part.is_empty() {
            return Err("Shortcut contains an empty key; use minus or plus for those keys");
        }
        if parts.peek().is_none() {
            if is_modifier(part) {
                return Err("Shortcut must end with a key, not a modifier");
            }
            if part.chars().count() != 1
                && ![
                    "enter",
                    "return",
                    "tab",
                    "space",
                    "escape",
                    "esc",
                    "backspace",
                    "delete",
                    "insert",
                    "home",
                    "end",
                    "pageup",
                    "pagedown",
                    "up",
                    "down",
                    "left",
                    "right",
                    "minus",
                    "equal",
                    "plus",
                    "f1",
                    "f2",
                    "f3",
                    "f4",
                    "f5",
                    "f6",
                    "f7",
                    "f8",
                    "f9",
                    "f10",
                    "f11",
                    "f12",
                    "f13",
                    "f14",
                    "f15",
                    "f16",
                    "f17",
                    "f18",
                    "f19",
                    "f20",
                    "f21",
                    "f22",
                    "f23",
                    "f24",
                ]
                .iter()
                .any(|name| part.eq_ignore_ascii_case(name))
            {
                return Err("Unknown shortcut key");
            }
            parsed.key = part;
            return Ok(parsed);
        }
        let field = if part.eq_ignore_ascii_case("ctrl") || part.eq_ignore_ascii_case("control") {
            &mut parsed.control
        } else if part.eq_ignore_ascii_case("cmd")
            || part.eq_ignore_ascii_case("command")
            || part.eq_ignore_ascii_case("super")
        {
            &mut parsed.command
        } else if part.eq_ignore_ascii_case("alt") || part.eq_ignore_ascii_case("option") {
            &mut parsed.alt
        } else if part.eq_ignore_ascii_case("shift") {
            &mut parsed.shift
        } else {
            return Err("Unknown shortcut modifier");
        };
        if *field {
            return Err("Shortcut repeats a modifier");
        }
        *field = true;
    }
    Err("Shortcut has no key")
}

fn is_modifier(part: &str) -> bool {
    [
        "ctrl", "control", "cmd", "command", "super", "alt", "option", "shift",
    ]
    .iter()
    .any(|name| part.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> CommandContext {
        CommandContext {
            connected: true,
            has_workspace: true,
            has_tab: true,
            has_pane: true,
            workspace_count: 1,
            tab_count: 2,
            pane_count: 2,
            terminal_available: true,
            can_paste: true,
            surface_status: Some(SurfaceStatus::Running),
            revision: 3,
            current_revision: 3,
            ..CommandContext::default()
        }
    }
    fn ctrl(key: &str) -> ShortcutKey<'_> {
        ShortcutKey {
            key,
            control: true,
            ..ShortcutKey::default()
        }
    }

    #[test]
    fn control_stays_terminal_input_except_windows_selection_copy() {
        let mut context = ready();
        let bindings = HashMap::new();
        for platform in [Platform::Windows, Platform::Mac] {
            for key in ["t", "w", "v", "a", "c", "d"] {
                assert_eq!(
                    resolve_key(
                        platform,
                        ctrl(key),
                        InputOwner::Terminal,
                        &context,
                        &bindings
                    ),
                    KeyRoute::Terminal
                );
            }
        }
        context.has_selection = true;
        assert_eq!(
            resolve_key(
                Platform::Windows,
                ctrl("c"),
                InputOwner::Terminal,
                &context,
                &bindings
            ),
            KeyRoute::Command(Command::Copy)
        );
        assert_eq!(
            resolve_key(
                Platform::Mac,
                ctrl("c"),
                InputOwner::Terminal,
                &context,
                &bindings
            ),
            KeyRoute::Terminal
        );
        assert_eq!(
            resolve_key(
                Platform::Mac,
                ShortcutKey {
                    key: "c",
                    command: true,
                    ..ShortcutKey::default()
                },
                InputOwner::Terminal,
                &context,
                &bindings
            ),
            KeyRoute::Command(Command::Copy)
        );
        context.has_selection = false;
        assert!(matches!(
            resolve_key(
                Platform::Windows,
                ShortcutKey {
                    shift: true,
                    ..ctrl("c")
                },
                InputOwner::Terminal,
                &context,
                &bindings
            ),
            KeyRoute::Disabled(Command::Copy, _)
        ));
    }

    #[test]
    fn overlay_and_explicit_override_precedence_is_deterministic() {
        let context = ready();
        let overrides = HashMap::from([
            ("new_tab".to_owned(), "ctrl-c".to_owned()),
            ("copy".to_owned(), "ctrl-shift-t".to_owned()),
        ]);
        assert_eq!(
            resolve_key(
                Platform::Windows,
                ctrl("c"),
                InputOwner::Terminal,
                &context,
                &overrides
            ),
            KeyRoute::Command(Command::NewTab)
        );
        assert!(matches!(
            resolve_key(
                Platform::Windows,
                ShortcutKey {
                    shift: true,
                    ..ctrl("t")
                },
                InputOwner::Terminal,
                &context,
                &overrides
            ),
            KeyRoute::Disabled(Command::Copy, _)
        ));
        for owner in [
            InputOwner::Palette,
            InputOwner::Menu,
            InputOwner::ThemePicker,
            InputOwner::TextField,
        ] {
            assert_eq!(
                resolve_key(Platform::Windows, ctrl("c"), owner, &context, &overrides),
                KeyRoute::Owned
            );
        }
        let overrides = HashMap::from([("new_tab".to_owned(), String::new())]);
        assert_eq!(
            resolve_key(
                Platform::Windows,
                ShortcutKey {
                    shift: true,
                    ..ctrl("t")
                },
                InputOwner::Terminal,
                &context,
                &overrides
            ),
            KeyRoute::Terminal
        );
    }

    #[test]
    fn stale_targets_and_lifecycle_prevent_unsafe_actions() {
        let mut context = ready();
        assert!(Command::EndSurface.disabled_reason(&context).is_none());
        assert!(Command::RestartSurface.disabled_reason(&context).is_some());
        context.surface_status = Some(SurfaceStatus::Lost);
        assert!(Command::EndSurface.disabled_reason(&context).is_some());
        assert!(Command::RestartSurface.disabled_reason(&context).is_none());
        context.current_revision += 1;
        assert!(Command::RestartSurface.disabled_reason(&context).is_some());
        assert!(Command::DetachTab.disabled_reason(&context).is_some());
        assert!(Command::OpenDiagnostics.disabled_reason(&context).is_none());
        context.revision = context.current_revision;
        context.split_right_reason = Some("Workspace overflow");
        assert!(Command::SplitRight.disabled_reason(&context).is_some());
        assert!(Command::SplitDown.disabled_reason(&context).is_none());
        context.has_pane = false;
        assert!(Command::SplitDown.disabled_reason(&context).is_some());
    }

    #[test]
    fn query_words_and_shortcut_parser_accept_useful_inputs_not_typos() {
        assert!(search("terminal hidden").any(|spec| spec.command == Command::RestoreHiddenTab));
        assert!(!Command::NewTab.spec().matches_query("workspace remove"));
        assert!(shortcut_matches(
            "Ctrl+Shift+P",
            ShortcutKey {
                shift: true,
                ..ctrl("p")
            }
        ));
        assert!(validate_shortcut("ctrl-ctrl-p").is_err());
        assert!(validate_shortcut("control-shfit-p").is_err());
        assert!(validate_shortcut("cmd-unknownkey").is_err());
        assert!(validate_shortcut("cmd-shift").is_err());
    }
}
