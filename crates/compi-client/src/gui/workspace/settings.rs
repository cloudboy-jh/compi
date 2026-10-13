//! Settings: a content pane (one per window, opened beside the focused pane) and the
//! Quick Appearance popover. Every setting is a row of [`ROWS`]: its section, title,
//! description, search keywords, the controls it holds and how it draws. A pane's
//! section view, its search results and the keyboard order are all derived from
//! the rows shown, so adding a setting is adding a row.
//!
//! The section a pane shows lives in the daemon (`PaneContent::Settings`), so it
//! persists with the pane; the search, scroll and keyboard highlight are this
//! window's.
use super::catalog::CatalogTarget;
use super::*;
use compi_protocol::{PaneContent, SettingsContent, SettingsSection};
use gpui::Entity;
use gpui_component::input::{Input, InputEvent, InputState};

/// Narrower than this, the section list wraps above the rows instead of beside them.
const SECTION_LIST_MIN_WIDTH: f32 = 620.0;
const SECTION_LIST_WIDTH: f32 = 184.0;
/// Narrower than this, a row's control goes below its title.
const STACKED_ROW_WIDTH: f32 = 480.0;
/// The row holding the shell prompt group; see `prompt`.
pub(super) const PROMPT_ROW: &str = "shell-prompt";

/// Focusable controls of Settings → Updates. Which ones exist depends on the update
/// state; see `update_stops`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UpdateControl {
    Primary(crate::updates::UpdateButton),
    CheckForUpdates,
    DownloadAutomatically,
    CandidateMore,
    CandidateNotes,
    WhatsNewMore,
    ReleaseNotes,
    Advanced,
    Repair,
    OpenLog,
}

/// What a Settings control does when pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SettingsAction {
    Scope(SettingsScope),
    BrowseThemes(CatalogTarget),
    ToggleTransparency,
    ToggleBlur,
    FollowTheme,
    /// Opens a font select's list.
    FontSelect(Popup),
    /// The opacity slider; Enter raises it a step, Left and Right move it.
    Opacity,
    ResetWindowAppearance,
    ResetSidebar,
    ResetLayout,
    Zoom(Command),
    OpenConfiguration,
    OpenPalette,
    OpenSettings,
    ToggleFps,
    RebuildRenderer,
    CopyPerformance,
    Reconnect,
    OpenDiagnostics,
    RestartDaemon,
    Update(UpdateControl),
    Prompt(super::prompt::PromptAction),
    Metadata(usize),
    Density(crate::theme::WorkspaceDensity),
    ToggleConfirmClose,
    FocusIndicator(crate::theme::FocusIndicator),
    ProgramStatus(usize),
}

/// Where Settings controls are drawn: a Settings pane, or Quick Appearance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SettingsHost {
    Quick,
    Pane(PaneId),
}

/// One stop of the keyboard order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stop {
    /// The pane's search field.
    Search,
    /// An entry of the pane's section list.
    Section(SettingsSection),
    /// A control of the row `row`.
    Control {
        row: &'static str,
        action: SettingsAction,
    },
}

/// The control with the keyboard highlight, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SettingsFocus {
    host: SettingsHost,
    stop: Stop,
}

#[derive(Clone, Copy)]
enum SettingsButtonTone {
    Secondary,
    Primary,
    Destructive,
}

/// A scrolled settings area: its handle, where it was last drawn, and whether the
/// focused control should be scrolled into it.
pub(super) struct SettingsScroll {
    handle: ScrollHandle,
    bounds: Option<Bounds<Pixels>>,
    to_focus: bool,
}

impl Default for SettingsScroll {
    fn default() -> Self {
        Self {
            handle: ScrollHandle::new(),
            bounds: None,
            to_focus: false,
        }
    }
}

/// One Settings pane's client state.
struct SettingsPane {
    /// Keyboard focus of the pane itself; its search field sits inside it.
    focus: FocusHandle,
    search: Entity<InputState>,
    /// The search, lowercased; empty shows `section`.
    query: String,
    /// The section shown. It is sent to the daemon when it differs from `persisted`,
    /// the section the daemon last reported; `sent` is the one on its way.
    section: SettingsSection,
    persisted: SettingsSection,
    sent: Option<SettingsSection>,
    scroll: SettingsScroll,
    _events: gpui::Subscription,
}

/// Settings in this window: each Settings pane's state, Quick Appearance's scroll
/// and the keyboard highlight, which follows whichever of them has the keyboard.
#[derive(Default)]
pub(in crate::gui) struct SettingsUi {
    focus: Option<SettingsFocus>,
    panes: HashMap<PaneId, SettingsPane>,
    quick: SettingsScroll,
    /// The stop the next Settings pane drawn starts on: set when a command opens one.
    pending: Option<Stop>,
    /// The terminal Settings was last opened from; the shell prompt group configures
    /// the prompt of its environment.
    pub(super) origin: Option<PaneId>,
    /// The shell prompt group was shown in the last frame.
    prompt_shown: bool,
}

/// What a row's renderer needs: where it is drawn and how.
#[derive(Clone)]
pub(super) struct RowCx {
    pub(super) host: SettingsHost,
    pub(super) row: &'static str,
    title: &'static str,
    pub(super) compact: bool,
}

/// One setting: its place, the words search matches, the controls it holds in
/// keyboard order, and how it draws.
pub(super) struct Row {
    id: &'static str,
    section: SettingsSection,
    /// The subheading it sits under within its section.
    group: Option<&'static str>,
    title: &'static str,
    description: &'static str,
    keywords: &'static str,
    /// Whether it shows now; some rows depend on other settings.
    shown: fn(&CompiApp) -> bool,
    stops: fn(&CompiApp) -> Vec<SettingsAction>,
    render: fn(&CompiApp, &RowCx, &Context<CompiApp>) -> AnyElement,
}

impl Row {
    /// Whether every term of `query` is in the title, description, keywords or
    /// group, ignoring case.
    fn matches(&self, query: &str) -> bool {
        let fields = [
            self.title,
            self.description,
            self.keywords,
            self.group.unwrap_or(""),
        ];
        query
            .split_whitespace()
            .all(|term| fields.iter().any(|field| contains_folded(field, term)))
    }
}

/// Whether `haystack` contains `needle`, ignoring case, without allocating.
fn contains_folded(haystack: &str, needle: &str) -> bool {
    haystack.char_indices().any(|(start, _)| {
        let mut rest = haystack[start..].chars().flat_map(char::to_lowercase);
        needle
            .chars()
            .flat_map(char::to_lowercase)
            .all(|wanted| rest.next() == Some(wanted))
    })
}

const fn row(
    id: &'static str,
    section: SettingsSection,
    title: &'static str,
    description: &'static str,
    keywords: &'static str,
    stops: fn(&CompiApp) -> Vec<SettingsAction>,
    render: fn(&CompiApp, &RowCx, &Context<CompiApp>) -> AnyElement,
) -> Row {
    Row {
        id,
        section,
        group: None,
        title,
        description,
        keywords,
        shown: |_| true,
        stops,
        render,
    }
}

const fn grouped(group: &'static str, row: Row) -> Row {
    Row {
        group: Some(group),
        ..row
    }
}

const fn shown_when(shown: fn(&CompiApp) -> bool, row: Row) -> Row {
    Row { shown, ..row }
}

/// A Tab metadata toggle; `stops` and `render` name its index.
const fn metadata_row(
    id: &'static str,
    title: &'static str,
    description: &'static str,
    stops: fn(&CompiApp) -> Vec<SettingsAction>,
    render: fn(&CompiApp, &RowCx, &Context<CompiApp>) -> AnyElement,
) -> Row {
    grouped(
        "Tab metadata",
        row(
            id,
            SettingsSection::Interface,
            title,
            description,
            "tab caption",
            stops,
            render,
        ),
    )
}

/// A Program status toggle; `stops` and `render` name its index.
const fn status_row(
    id: &'static str,
    title: &'static str,
    description: &'static str,
    stops: fn(&CompiApp) -> Vec<SettingsAction>,
    render: fn(&CompiApp, &RowCx, &Context<CompiApp>) -> AnyElement,
) -> Row {
    grouped(
        "Program status",
        row(
            id,
            SettingsSection::Interface,
            title,
            description,
            "osc 7501 blocked failed working done idle",
            stops,
            render,
        ),
    )
}

/// Every setting, in section order and, within a section, in display order.
static ROWS: &[Row] = &[
    // Appearance; Quick Appearance shows these too.
    row(
        "scope",
        SettingsSection::Appearance,
        "Scope",
        "Change the global defaults or only this window.",
        "global window override",
        |_| scope_stops(),
        |app, row, cx| app.render_scope_row(row, cx),
    ),
    row(
        "theme",
        SettingsSection::Appearance,
        "Theme",
        "The color theme of the interface and terminals.",
        "colors color scheme dark light catalog browse",
        |_| vec![SettingsAction::BrowseThemes(CatalogTarget::Both)],
        |app, row, cx| app.render_theme_row(row, cx),
    ),
    row(
        "transparency",
        SettingsSection::Appearance,
        "Transparent background",
        "Let the desktop show through the window.",
        "opacity translucent glass",
        |_| vec![SettingsAction::ToggleTransparency],
        |app, row, cx| {
            app.render_toggle_row(
                row,
                app.scoped_appearance().transparent_background,
                SettingsAction::ToggleTransparency,
                "",
                cx,
            )
        },
    ),
    shown_when(
        |app| app.scoped_appearance().transparent_background,
        row(
            "opacity",
            SettingsSection::Appearance,
            "Opacity",
            "How much of the desktop shows through.",
            "transparency translucent",
            |_| vec![SettingsAction::Opacity],
            |app, row, cx| app.render_opacity_row(row, cx),
        ),
    ),
    shown_when(
        |app| app.scoped_appearance().transparent_background,
        row(
            "blur",
            SettingsSection::Appearance,
            "Blur background",
            "Soften what's behind the window.",
            "transparency translucent glass acrylic",
            |_| vec![SettingsAction::ToggleBlur],
            |app, row, cx| {
                app.render_toggle_row(
                    row,
                    app.scoped_appearance().background_effect == BackgroundEffect::Blurred,
                    SettingsAction::ToggleBlur,
                    "Soften what's behind the window.",
                    cx,
                )
            },
        ),
    ),
    shown_when(
        |app| app.settings_scope == SettingsScope::Window,
        row(
            "window-overrides",
            SettingsSection::Appearance,
            "Window overrides",
            "Reset this window's appearance to the global defaults.",
            "reset scope",
            |_| vec![SettingsAction::ResetWindowAppearance],
            |app, row, cx| {
                app.render_button_row(
                    row,
                    "Reset this window's appearance to the global defaults.".into(),
                    "Use global defaults",
                    SettingsAction::ResetWindowAppearance,
                    false,
                    cx,
                )
            },
        ),
    ),
    // Interface.
    row(
        "ui-font",
        SettingsSection::Interface,
        "Interface font",
        "The font of menus, tabs and Settings.",
        "typeface family ui",
        |_| vec![SettingsAction::FontSelect(Popup::UiFont)],
        |app, row, cx| {
            app.settings_row(
                row.title,
                String::new(),
                app.render_font_select(row, Popup::UiFont, cx),
                row.compact,
            )
        },
    ),
    row(
        "density",
        SettingsSection::Interface,
        "Pane density",
        "Rounded panes with gaps between them, or edge to edge.",
        "comfy compact gaps spacing islands",
        |_| {
            crate::theme::WorkspaceDensity::ALL
                .map(SettingsAction::Density)
                .to_vec()
        },
        |app, row, cx| app.render_density_row(row, cx),
    ),
    row(
        "sidebar-width",
        SettingsSection::Interface,
        "Sidebar width",
        "Reset the sidebar to its default width.",
        "reset",
        |_| vec![SettingsAction::ResetSidebar],
        |app, row, cx| {
            app.render_button_row(
                row,
                format!("{:.0}px", app.sidebar_width),
                "Reset width",
                SettingsAction::ResetSidebar,
                false,
                cx,
            )
        },
    ),
    row(
        "window-layout",
        SettingsSection::Interface,
        "Window layout",
        "Restore the saved layout defaults.",
        "reset",
        |_| vec![SettingsAction::ResetLayout],
        |app, row, cx| {
            app.render_button_row(
                row,
                "Restore the saved layout defaults.".into(),
                "Reset layout",
                SettingsAction::ResetLayout,
                false,
                cx,
            )
        },
    ),
    metadata_row(
        "metadata-directory",
        "Directory",
        "Show the current directory reported by the shell.",
        |_| vec![SettingsAction::Metadata(0)],
        |app, row, cx| app.render_metadata_row(row, 0, cx),
    ),
    metadata_row(
        "metadata-process",
        "Active process",
        "Show the terminal foreground process, not the launcher.",
        |_| vec![SettingsAction::Metadata(1)],
        |app, row, cx| app.render_metadata_row(row, 1, cx),
    ),
    metadata_row(
        "metadata-git",
        "Git branch and changes",
        "Show branch and change state in the terminal's environment.",
        |_| vec![SettingsAction::Metadata(2)],
        |app, row, cx| app.render_metadata_row(row, 2, cx),
    ),
    metadata_row(
        "metadata-dimensions",
        "Dimensions",
        "Show terminal columns and rows.",
        |_| vec![SettingsAction::Metadata(3)],
        |app, row, cx| app.render_metadata_row(row, 3, cx),
    ),
    grouped(
        "Closing",
        row(
            "confirm-close",
            SettingsSection::Interface,
            "Confirm before closing",
            "Ask before closing a tab or pane ends its processes. Off: one click closes.",
            "close end process prompt",
            |_| vec![SettingsAction::ToggleConfirmClose],
            |app, row, cx| {
                app.render_toggle_row(
                    row,
                    app.config.confirm_close,
                    SettingsAction::ToggleConfirmClose,
                    row_description("confirm-close"),
                    cx,
                )
            },
        ),
    ),
    grouped(
        "Focus",
        row(
            "focus-indicator",
            SettingsSection::Interface,
            "Focus indicator",
            "How the focused pane is marked: marker, outline, dim or none.",
            "marker outline dim focused pane",
            |_| {
                crate::theme::FocusIndicator::ALL
                    .map(SettingsAction::FocusIndicator)
                    .to_vec()
            },
            |app, row, cx| app.render_focus_indicator_row(row, cx),
        ),
    ),
    status_row(
        "status-tabs",
        "Tabs",
        "Show what programs report on each tab: blocked, failed, working, done or idle.",
        |_| vec![SettingsAction::ProgramStatus(0)],
        |app, row, cx| app.render_status_row(row, 0, cx),
    ),
    status_row(
        "status-panes",
        "Panes",
        "Show it on each pane's top edge and floating title strip.",
        |_| vec![SettingsAction::ProgramStatus(1)],
        |app, row, cx| app.render_status_row(row, 1, cx),
    ),
    status_row(
        "status-sidebar",
        "Sidebar and palette",
        "Start each tab's status line with it.",
        |_| vec![SettingsAction::ProgramStatus(2)],
        |app, row, cx| app.render_status_row(row, 2, cx),
    ),
    // Terminal.
    row(
        "terminal-font",
        SettingsSection::Terminal,
        "Terminal font",
        "The font terminals draw text in.",
        "typeface family monospace",
        |_| vec![SettingsAction::FontSelect(Popup::TerminalFont)],
        |app, row, cx| {
            app.settings_row(
                row.title,
                String::new(),
                app.render_font_select(row, Popup::TerminalFont, cx),
                row.compact,
            )
        },
    ),
    row(
        "zoom",
        SettingsSection::Terminal,
        "Zoom",
        "Make terminal text larger or smaller.",
        "size scale bigger smaller line height",
        |_| {
            vec![
                SettingsAction::Zoom(Command::ZoomOut),
                SettingsAction::Zoom(Command::ZoomReset),
                SettingsAction::Zoom(Command::ZoomIn),
            ]
        },
        |app, row, cx| app.render_zoom_row(row, cx),
    ),
    row(
        "custom-typography",
        SettingsSection::Terminal,
        "Custom typography",
        "Set a custom font, size, or line height in the configuration file.",
        "toml config",
        |_| vec![SettingsAction::OpenConfiguration],
        |app, row, cx| {
            app.render_button_row(
                row,
                row_description("custom-typography").into(),
                "Edit configuration",
                SettingsAction::OpenConfiguration,
                false,
                cx,
            )
        },
    ),
    row(
        PROMPT_ROW,
        SettingsSection::Terminal,
        "Shell prompt",
        "Choose an Oh My Posh or Starship prompt for Compi's shells.",
        "oh my posh starship ps1 theme bash zsh wsl distribution history restore",
        |app| {
            app.prompt_actions()
                .into_iter()
                .map(SettingsAction::Prompt)
                .collect()
        },
        |app, row, cx| app.render_prompt_group(row, cx),
    ),
    // Keyboard.
    row(
        "commands",
        SettingsSection::Keyboard,
        "Commands",
        "Search every command and its shortcut.",
        "palette shortcuts keys",
        |_| vec![SettingsAction::OpenPalette],
        |app, row, cx| {
            app.render_button_row(
                row,
                format!(
                    "Search {} commands and their shortcuts.",
                    commands::REGISTRY.len()
                ),
                "Open palette",
                SettingsAction::OpenPalette,
                false,
                cx,
            )
        },
    ),
    row(
        "keybindings",
        SettingsSection::Keyboard,
        "Keybindings",
        "Custom shortcuts replace the platform defaults.",
        "shortcuts keys bindings toml config",
        |_| vec![SettingsAction::OpenConfiguration],
        |app, row, cx| {
            app.render_button_row(
                row,
                row_description("keybindings").into(),
                "Edit keybindings",
                SettingsAction::OpenConfiguration,
                false,
                cx,
            )
        },
    ),
    // Performance.
    row(
        "performance",
        SettingsSection::Performance,
        "Performance",
        "Frame rate, resource use and renderer caches.",
        "fps frames cpu memory renderer cache diagnostics rebuild",
        |_| {
            vec![
                SettingsAction::ToggleFps,
                SettingsAction::RebuildRenderer,
                SettingsAction::CopyPerformance,
            ]
        },
        |app, row, cx| app.render_performance_section(row, cx),
    ),
    // Updates.
    row(
        "update-status",
        SettingsSection::Updates,
        "Compi version",
        "The installed version and what the updater is doing.",
        "update upgrade release version download install restart",
        |app| {
            app.update_view()
                .button
                .map(|button| SettingsAction::Update(UpdateControl::Primary(button)))
                .into_iter()
                .collect()
        },
        |app, row, cx| app.render_update_status(row, cx),
    ),
    row(
        "check-for-updates",
        SettingsSection::Updates,
        "Check for updates",
        "Look for new Compi releases.",
        "update upgrade release automatic",
        |_| vec![SettingsAction::Update(UpdateControl::CheckForUpdates)],
        |app, row, cx| {
            app.render_toggle_row(
                row,
                app.updates.snapshot().preferences.check_for_updates,
                SettingsAction::Update(UpdateControl::CheckForUpdates),
                "",
                cx,
            )
        },
    ),
    row(
        "download-automatically",
        SettingsSection::Updates,
        "Download updates automatically",
        "Download a new release as soon as one is found.",
        "update upgrade release automatic",
        |_| vec![SettingsAction::Update(UpdateControl::DownloadAutomatically)],
        |app, row, cx| {
            app.render_toggle_row(
                row,
                app.updates
                    .snapshot()
                    .preferences
                    .download_updates_automatically,
                SettingsAction::Update(UpdateControl::DownloadAutomatically),
                "",
                cx,
            )
        },
    ),
    shown_when(
        |app| app.updates.snapshot().candidate().is_some(),
        row(
            "update-notes",
            SettingsSection::Updates,
            "What's new in the update",
            "Release notes of the version available.",
            "release notes changelog update",
            |app| app.candidate_stops(),
            |app, row, cx| app.render_candidate_notes(row, cx),
        ),
    ),
    shown_when(
        |_| crate::release_notes::current().is_some(),
        row(
            "whats-new",
            SettingsSection::Updates,
            "What's new",
            "Release notes of the installed version.",
            "release notes changelog",
            |_| {
                vec![
                    SettingsAction::Update(UpdateControl::WhatsNewMore),
                    SettingsAction::Update(UpdateControl::ReleaseNotes),
                ]
            },
            |app, row, cx| app.render_whats_new_notes(row, cx),
        ),
    ),
    row(
        "update-advanced",
        SettingsSection::Updates,
        "Repair and update log",
        "Repair the installation or read the update log.",
        "advanced repair log troubleshoot",
        |app| app.update_advanced_stops(),
        |app, row, cx| app.render_update_advanced(row, cx),
    ),
    // Advanced.
    row(
        "advanced-scope",
        SettingsSection::Advanced,
        "Scope",
        "Change the global defaults or only this window.",
        "global window override",
        |_| scope_stops(),
        |app, row, cx| app.render_scope_row(row, cx),
    ),
    row(
        "terminal-colors",
        SettingsSection::Advanced,
        "Terminal colors",
        "Override the theme's terminal colors; colors set by programs stay unchanged.",
        "theme palette ansi override follow",
        |_| {
            vec![
                SettingsAction::FollowTheme,
                SettingsAction::BrowseThemes(CatalogTarget::Terminal),
            ]
        },
        |app, row, cx| app.render_terminal_colors_row(row, cx),
    ),
    row(
        "configuration",
        SettingsSection::Advanced,
        "Configuration",
        "Open the configuration file.",
        "toml config file edit",
        |_| vec![SettingsAction::OpenConfiguration],
        |app, row, cx| {
            app.render_button_row(
                row,
                String::new(),
                "Open file",
                SettingsAction::OpenConfiguration,
                false,
                cx,
            )
        },
    ),
    row(
        "connection",
        SettingsSection::Advanced,
        "Connection",
        "Reconnect without stopping running processes.",
        "reconnect daemon server",
        |_| vec![SettingsAction::Reconnect],
        |app, row, cx| {
            app.render_button_row(
                row,
                row_description("connection").into(),
                "Reconnect",
                SettingsAction::Reconnect,
                false,
                cx,
            )
        },
    ),
    row(
        "diagnostics",
        SettingsSection::Advanced,
        "Diagnostics",
        "Inspect connection and rendering details.",
        "debug connection rendering",
        |_| vec![SettingsAction::OpenDiagnostics],
        |app, row, cx| {
            app.render_button_row(
                row,
                row_description("diagnostics").into(),
                "Open diagnostics",
                SettingsAction::OpenDiagnostics,
                false,
                cx,
            )
        },
    ),
    row(
        "restart-daemon",
        SettingsSection::Advanced,
        "Restart daemon",
        "Restart the terminal server; running terminals stop.",
        "daemon server restart stop terminals",
        |_| vec![SettingsAction::RestartDaemon],
        |app, row, cx| app.render_restart_daemon_row(row, cx),
    ),
];

fn scope_stops() -> Vec<SettingsAction> {
    vec![
        SettingsAction::Scope(SettingsScope::Global),
        SettingsAction::Scope(SettingsScope::Window),
    ]
}

/// The registered description of the row `id`, for rows that show it as is.
fn row_description(id: &str) -> &'static str {
    ROWS.iter()
        .find(|row| row.id == id)
        .map_or("", |row| row.description)
}

/// The rows a pane shows for `section` and the lowercased `query`, before `shown`:
/// with a query, every row it matches in any section; without, `section`'s rows.
fn rows_for(section: SettingsSection, query: &str) -> impl Iterator<Item = &'static Row> {
    let searching = !query.trim().is_empty();
    ROWS.iter().filter(move |row| {
        if searching {
            row.matches(query)
        } else {
            row.section == section
        }
    })
}

/// Keyboard order: the search field and section list when `pane`, then each row's
/// controls in row order.
fn focus_order(
    pane: bool,
    rows: impl IntoIterator<Item = (&'static str, Vec<SettingsAction>)>,
) -> Vec<Stop> {
    let mut order = Vec::new();
    if pane {
        order.push(Stop::Search);
        order.extend(SettingsSection::ALL.map(Stop::Section));
    }
    for (row, actions) in rows {
        order.extend(
            actions
                .into_iter()
                .map(|action| Stop::Control { row, action }),
        );
    }
    order
}

/// The stop after (or before) `current` in `order`, wrapping; the first (or last)
/// when `current` is not in it.
fn step(order: &[Stop], current: Option<Stop>, forward: bool) -> Option<Stop> {
    let last = order.len().checked_sub(1)?;
    let at = current.and_then(|current| order.iter().position(|stop| *stop == current));
    Some(
        order[match (at, forward) {
            (Some(at), true) => (at + 1) % order.len(),
            (Some(0), false) => last,
            (Some(at), false) => at - 1,
            (None, true) => 0,
            (None, false) => last,
        }],
    )
}

/// The Settings leaves of `node`.
fn settings_leaves<'a>(node: &'a LayoutNode, output: &mut Vec<&'a PaneId>) {
    match node {
        LayoutNode::Content {
            pane_id,
            content: PaneContent::Settings(_),
        } => output.push(pane_id),
        LayoutNode::Content { .. } | LayoutNode::Pane { .. } => {}
        LayoutNode::Split { first, second, .. } => {
            settings_leaves(first, output);
            settings_leaves(second, output);
        }
    }
}

impl CompiApp {
    /// The Settings content `pane` shows, if it is a Settings pane.
    pub(super) fn settings_content(&self, pane: &PaneId) -> Option<SettingsContent> {
        content::find_pane_content(self.workspace.as_ref()?, pane)?
            .settings()
            .copied()
    }

    /// The rows the Settings pane `pane` shows now.
    fn pane_rows(&self, pane: &SettingsPane) -> Vec<&'static Row> {
        rows_for(pane.section, &pane.query)
            .filter(|row| (row.shown)(self))
            .collect()
    }

    /// The Appearance rows Quick Appearance shows now.
    fn quick_rows(&self) -> Vec<&'static Row> {
        rows_for(SettingsSection::Appearance, "")
            .filter(|row| (row.shown)(self))
            .collect()
    }

    /// The keyboard order where `host` draws.
    fn settings_order(&self, host: &SettingsHost) -> Vec<Stop> {
        let (pane, rows) = match host {
            SettingsHost::Quick => (false, self.quick_rows()),
            SettingsHost::Pane(pane_id) => match self.settings.panes.get(pane_id) {
                Some(pane) => (true, self.pane_rows(pane)),
                None => return Vec::new(),
            },
        };
        let mut order = focus_order(pane, rows.iter().map(|row| (row.id, (row.stops)(self))));
        if *host == SettingsHost::Quick {
            order.push(Stop::Control {
                row: "all-settings",
                action: SettingsAction::OpenSettings,
            });
        }
        order
    }

    /// Rows shown in any Settings pane this window draws.
    fn shown_settings_rows(&self) -> Vec<&'static Row> {
        let mut rows: Vec<&'static Row> = Vec::new();
        for pane in self.shown_content_panes() {
            if let Some(pane) = self.settings.panes.get(pane) {
                for row in self.pane_rows(pane) {
                    if !rows.iter().any(|shown| shown.id == row.id) {
                        rows.push(row);
                    }
                }
            }
        }
        rows
    }

    /// Whether a Settings pane this window draws shows the row `id`.
    pub(super) fn settings_row_shown(&self, id: &str) -> bool {
        self.shown_settings_rows().iter().any(|row| row.id == id)
    }

    /// Whether a Settings pane this window draws shows rows of `section`.
    fn settings_section_shown(&self, section: SettingsSection) -> bool {
        self.shown_settings_rows()
            .iter()
            .any(|row| row.section == section)
    }

    fn settings_scroll_mut(&mut self, host: &SettingsHost) -> Option<&mut SettingsScroll> {
        match host {
            SettingsHost::Quick => Some(&mut self.settings.quick),
            SettingsHost::Pane(pane) => self
                .settings
                .panes
                .get_mut(pane)
                .map(|pane| &mut pane.scroll),
        }
    }

    /// Moves the keyboard highlight to `stop` where `host` draws, scrolling it into view.
    fn set_settings_focus(&mut self, host: SettingsHost, stop: Stop) {
        if let Some(scroll) = self.settings_scroll_mut(&host) {
            scroll.to_focus = true;
        }
        self.settings.focus = Some(SettingsFocus { host, stop });
    }

    /// Whether `action` of `row` has the keyboard highlight.
    pub(super) fn settings_focused(&self, row: &RowCx, action: SettingsAction) -> bool {
        self.settings.focus.as_ref().is_some_and(|focus| {
            focus.host == row.host
                && focus.stop
                    == Stop::Control {
                        row: row.row,
                        action,
                    }
        })
    }

    /// The Shell prompt row in the Settings pane that has the keyboard highlight.
    pub(super) fn prompt_row_cx(&self) -> Option<RowCx> {
        let host = self
            .settings
            .focus
            .as_ref()
            .map(|focus| focus.host.clone())
            .filter(|host| matches!(host, SettingsHost::Pane(_)))?;
        Some(RowCx {
            host,
            row: PROMPT_ROW,
            title: "Shell prompt",
            compact: false,
        })
    }

    /// Highlights `action` of `row`, as a click on it does.
    pub(super) fn focus_settings_control(&mut self, row: &RowCx, action: SettingsAction) {
        self.set_settings_focus(
            row.host.clone(),
            Stop::Control {
                row: row.row,
                action,
            },
        );
    }

    /// A click on a control: it takes the highlight and does what it does.
    fn settings_click(
        &self,
        row: &RowCx,
        action: SettingsAction,
        cx: &Context<Self>,
    ) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
        let row = row.clone();
        cx.listener(move |this, _, window, cx| {
            this.focus_settings_control(&row, action);
            this.activate_settings_action(action, window, cx);
            cx.stop_propagation();
            cx.notify();
        })
    }

    /// Opens Settings: focuses this window's Settings pane, at `section` when given,
    /// or opens one beside the focused pane. The keyboard starts on `stop`, else on
    /// the search field.
    pub(super) fn open_settings(
        &mut self,
        section: Option<SettingsSection>,
        stop: Option<Stop>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlay.is_some() {
            self.dismiss_overlay();
        }
        self.popup = None;
        if let Some(pane) = self.focused_pane()
            && self.focused_view().is_some()
        {
            self.settings.origin = Some(pane.clone());
        }
        if let Some((session, tab, pane_id)) = self.window_settings_pane() {
            if self.selected_tab().map(|tab| &tab.id) != Some(&tab) {
                if let Some(workspace) = &self.workspace {
                    self.state.select_session(workspace, &session);
                }
                self.select_terminal(tab, false);
            }
            self.focus_pane(pane_id.clone());
            if let Some(pane) = self.settings.panes.get_mut(&pane_id) {
                if let Some(section) = section {
                    pane.section = section;
                    pane.query.clear();
                    pane.search
                        .update(cx, |search, cx| search.set_value("", window, cx));
                }
                pane.scroll.handle.set_offset(point(px(0.0), px(0.0)));
            }
            self.set_settings_focus(SettingsHost::Pane(pane_id), stop.unwrap_or(Stop::Search));
            return;
        }
        let content = PaneContent::Settings(SettingsContent {
            section: section.unwrap_or_default(),
        });
        let beside = self.focused_pane().cloned().or_else(|| {
            let mut leaves = Vec::new();
            collect_leaves(&self.selected_tab()?.layout, &mut leaves);
            leaves.into_iter().next().map(|(pane, _)| pane)
        });
        self.settings_scope = SettingsScope::Global;
        self.settings.pending = Some(stop.unwrap_or(Stop::Search));
        match beside {
            Some(beside) => {
                self.open_content_beside(&beside, content);
            }
            // No tab to open beside: Settings gets a tab of its own.
            None => {
                let Some(session_id) = self
                    .state
                    .selected_session
                    .clone()
                    .or_else(|| Some(self.workspace.as_ref()?.sessions.first()?.id.clone()))
                else {
                    self.settings.pending = None;
                    self.global_error = Some("Open a workspace first".into());
                    return;
                };
                self.mutate(
                    WorkspaceMutation::CreateContentTab {
                        session_id,
                        label: String::new(),
                        content,
                    },
                    true,
                );
            }
        }
    }

    /// The Settings pane of this window: in the selected tab if it has one, else in
    /// another tab the window shows.
    fn window_settings_pane(&self) -> Option<(SessionId, TabId, PaneId)> {
        let workspace = self.workspace.as_ref()?;
        let selected = self.selected_tab().map(|tab| &tab.id);
        let mut found = None;
        for session in &workspace.sessions {
            for tab in self.state.visible_tabs(session) {
                let mut panes = Vec::new();
                settings_leaves(&tab.layout, &mut panes);
                let Some(pane) = panes.first() else {
                    continue;
                };
                let hit = (session.id.clone(), tab.id.clone(), (*pane).clone());
                if selected == Some(&tab.id) {
                    return Some(hit);
                }
                found.get_or_insert(hit);
            }
        }
        found
    }

    /// Before drawing: state for each Settings pane shown, its section sent to the
    /// daemon when it changed, and the shell prompt detected when its group shows.
    pub(super) fn prepare_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        self.settings.panes.retain(|pane, _| {
            content::find_pane_content(workspace, pane)
                .is_some_and(|content| content.settings().is_some())
        });
        let shown: Vec<(PaneId, SettingsSection)> = self
            .shown_content_panes()
            .filter_map(|pane| {
                let settings = content::find_pane_content(workspace, pane)?.settings()?;
                Some((pane.clone(), settings.section))
            })
            .collect();
        for (pane_id, daemon) in shown {
            match self.settings.panes.get_mut(&pane_id) {
                None => {
                    let search =
                        cx.new(|cx| InputState::new(window, cx).placeholder("Search settings"));
                    let changed = pane_id.clone();
                    let events =
                        cx.subscribe(&search, move |this, search, event: &InputEvent, cx| {
                            match event {
                                InputEvent::Change => {
                                    let query = search.read(cx).value().to_lowercase();
                                    if let Some(pane) = this.settings.panes.get_mut(&changed) {
                                        pane.query = query;
                                        pane.scroll.handle.set_offset(point(px(0.0), px(0.0)));
                                    }
                                }
                                // A click in the field moves the highlight there.
                                InputEvent::Focus => {
                                    this.settings.focus = Some(SettingsFocus {
                                        host: SettingsHost::Pane(changed.clone()),
                                        stop: Stop::Search,
                                    });
                                }
                                InputEvent::PressEnter { .. } | InputEvent::Blur => return,
                            }
                            cx.notify();
                        });
                    self.settings.panes.insert(
                        pane_id.clone(),
                        SettingsPane {
                            focus: cx.focus_handle(),
                            search,
                            query: String::new(),
                            section: daemon,
                            persisted: daemon,
                            sent: None,
                            scroll: SettingsScroll::default(),
                            _events: events,
                        },
                    );
                    if let Some(stop) = self.settings.pending.take() {
                        self.set_settings_focus(SettingsHost::Pane(pane_id.clone()), stop);
                    }
                }
                Some(pane) => {
                    if pane.sent == Some(daemon) {
                        pane.sent = None;
                        pane.persisted = daemon;
                    } else if pane.sent.is_some() && !self.mutation_pending {
                        // The change did not apply; show what the daemon has.
                        pane.sent = None;
                        pane.persisted = daemon;
                        pane.section = daemon;
                    } else if pane.sent.is_none() && daemon != pane.persisted {
                        pane.persisted = daemon;
                        pane.section = daemon;
                    }
                }
            }
            let Some(pane) = self.settings.panes.get_mut(&pane_id) else {
                continue;
            };
            if pane.section != pane.persisted && pane.sent.is_none() && !self.mutation_pending {
                pane.sent = Some(pane.section);
                let content = PaneContent::Settings(SettingsContent {
                    section: pane.section,
                });
                self.mutate(
                    WorkspaceMutation::UpdateContent {
                        pane_id: pane_id.clone(),
                        content,
                    },
                    false,
                );
            }
        }
        if self
            .settings
            .focus
            .as_ref()
            .is_some_and(|focus| match &focus.host {
                SettingsHost::Pane(pane) => !self.settings.panes.contains_key(pane),
                SettingsHost::Quick => false,
            })
        {
            self.settings.focus = None;
        }
        // A control clicked while the search field had the keyboard takes it.
        if let Some(pane_id) = self.focused_pane().cloned()
            && self.settings.focus.as_ref().is_some_and(|focus| {
                focus.host == SettingsHost::Pane(pane_id.clone()) && focus.stop != Stop::Search
            })
            && let Some(pane) = self.settings.panes.get(&pane_id)
            && pane.search.focus_handle(cx).is_focused(window)
        {
            window.focus(&pane.focus, cx);
        }
        let prompt = self.settings_row_shown(PROMPT_ROW);
        if prompt && !self.settings.prompt_shown {
            self.enter_prompt_settings();
        }
        self.settings.prompt_shown = prompt;
    }

    /// Whether `pane` is a Settings pane with client state, and its focus handle.
    pub(super) fn settings_pane_focus(&self, pane: &PaneId) -> Option<&FocusHandle> {
        self.settings.panes.get(pane).map(|pane| &pane.focus)
    }

    /// Whether `focused` is inside one of this window's Settings panes.
    pub(super) fn settings_owns(&self, focused: &FocusHandle, window: &Window, cx: &App) -> bool {
        self.settings.panes.values().any(|pane| {
            *focused == pane.focus
                || pane.focus.contains_focused(window, cx)
                || pane.search.focus_handle(cx) == *focused
        })
    }

    /// Where the keyboard goes in the focused Settings pane `pane`: its search field
    /// unless a control has the highlight there.
    pub(super) fn settings_keyboard(&self, pane: &PaneId, cx: &App) -> Option<FocusHandle> {
        let ui = self.settings.panes.get(pane)?;
        let control = self.settings.focus.as_ref().is_some_and(|focus| {
            focus.host == SettingsHost::Pane(pane.clone()) && focus.stop != Stop::Search
        });
        Some(if control {
            ui.focus.clone()
        } else {
            ui.search.focus_handle(cx)
        })
    }

    /// The focused Settings pane's search field has the keyboard.
    pub(super) fn settings_search_has_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.focused_pane()
            .and_then(|pane| self.settings.panes.get(pane))
            .is_some_and(|pane| pane.search.focus_handle(cx).is_focused(window))
    }

    /// Keys in the focused Settings pane's search field, before the field's own: Tab
    /// and Down move on to the controls, Escape clears the search or leaves Settings.
    pub(super) fn settings_search_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.settings_search_has_keyboard(window, cx) {
            return false;
        }
        let Some(pane_id) = self.focused_pane().cloned() else {
            return false;
        };
        let modifiers = &key.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform {
            return false;
        }
        let host = SettingsHost::Pane(pane_id.clone());
        match key.key.as_str() {
            "tab" | "down" | "enter" => {
                let order = self.settings_order(&host);
                let forward = !(key.key == "tab" && modifiers.shift);
                let next = if key.key == "tab" {
                    step(&order, Some(Stop::Search), forward)
                } else {
                    // Down and Enter go to the first result.
                    order
                        .iter()
                        .find(|stop| matches!(stop, Stop::Control { .. }))
                        .copied()
                };
                if let Some(next) = next {
                    self.set_settings_focus(host, next);
                    self.apply_settings_focus(&pane_id, window, cx);
                }
                true
            }
            "escape" => {
                let Some(pane) = self.settings.panes.get_mut(&pane_id) else {
                    return false;
                };
                if pane.query.is_empty() {
                    self.leave_settings(&pane_id);
                } else {
                    pane.query.clear();
                    pane.search
                        .update(cx, |search, cx| search.set_value("", window, cx));
                }
                true
            }
            _ => false,
        }
    }

    /// Gives the keyboard to the search field or the pane, following the highlight.
    fn apply_settings_focus(&self, pane_id: &PaneId, window: &mut Window, cx: &mut App) {
        if let Some(handle) = self.settings_keyboard(pane_id, cx)
            && !handle.is_focused(window)
        {
            window.focus(&handle, cx);
        }
    }

    /// Returns the keyboard to the terminal Settings was opened from, or another
    /// terminal of the tab.
    fn leave_settings(&mut self, pane_id: &PaneId) {
        let terminals: Vec<PaneId> = self
            .selected_tab()
            .map(|tab| {
                let mut leaves = Vec::new();
                collect_leaves(&tab.layout, &mut leaves);
                leaves
                    .into_iter()
                    .filter_map(|(pane, surface)| surface.map(|_| pane))
                    .collect()
            })
            .unwrap_or_default();
        let target = self
            .settings
            .origin
            .clone()
            .filter(|origin| terminals.contains(origin))
            .or_else(|| terminals.into_iter().next());
        if let Some(target) = target
            && target != *pane_id
        {
            self.focus_pane(target);
        }
    }

    /// Keys in a Settings pane that its search field and controls left alone.
    fn settings_pane_key_down(
        &mut self,
        pane_id: &PaneId,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.settings.panes.get(pane_id) else {
            return;
        };
        if pane.search.focus_handle(cx).is_focused(window) {
            // Text belongs to the field; Tab, Down and Escape were taken before it.
            return;
        }
        let key = &event.keystroke;
        let host = SettingsHost::Pane(pane_id.clone());
        let plain = !key.modifiers.control && !key.modifiers.alt && !key.modifiers.platform;
        let handled = if key.key == "escape" && plain {
            if self.popup.take().is_none() && !self.close_prompt_dropdown() {
                self.set_settings_focus(host, Stop::Search);
            }
            true
        } else if self.settings_navigation_key(key, &host, window, cx) {
            true
        } else if let Some(text) = key
            .key_char
            .as_deref()
            .filter(|text| plain && !text.is_empty() && !text.chars().any(char::is_control))
        {
            // Typing searches.
            let text = text.to_owned();
            if let Some(pane) = self.settings.panes.get(pane_id) {
                let query = format!("{}{text}", pane.search.read(cx).value());
                pane.search
                    .update(cx, |search, cx| search.set_value(query, window, cx));
                let lowered = pane.search.read(cx).value().to_lowercase();
                if let Some(pane) = self.settings.panes.get_mut(pane_id) {
                    pane.query = lowered;
                }
            }
            self.set_settings_focus(host, Stop::Search);
            true
        } else {
            self.handle_app_key(event, window, cx)
        };
        if handled {
            self.apply_settings_focus(pane_id, window, cx);
        }
        // Nothing typed in Settings reaches a shell.
        cx.stop_propagation();
        cx.notify();
    }

    /// Keys in the Quick Appearance popover; Escape closes it before reaching here.
    pub(super) fn quick_appearance_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_navigation_key(key, &SettingsHost::Quick, window, cx) {
            cx.notify();
        }
    }

    /// Moves the highlight through `host`'s stops and presses the highlighted one.
    fn settings_navigation_key(
        &mut self,
        key: &Keystroke,
        host: &SettingsHost,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = &key.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform {
            return false;
        }
        let order = self.settings_order(host);
        let current = self
            .settings
            .focus
            .as_ref()
            .filter(|focus| focus.host == *host)
            .map(|focus| focus.stop)
            .filter(|stop| order.contains(stop));
        let controls: Vec<Stop> = order
            .iter()
            .filter(|stop| matches!(stop, Stop::Control { .. }))
            .copied()
            .collect();
        let pane = match host {
            SettingsHost::Pane(pane) => Some(pane.clone()),
            SettingsHost::Quick => None,
        };
        let next = match (key.key.as_str(), current) {
            (
                "up" | "down" | "home" | "end",
                Some(Stop::Control {
                    action: SettingsAction::Prompt(super::prompt::PromptAction::Styles),
                    ..
                }),
            ) => {
                self.move_prompt_style(key.key.as_str());
                current
            }
            ("tab", _) => step(&order, current, !modifiers.shift),
            ("up" | "down", Some(Stop::Section(section))) => {
                let index = SettingsSection::ALL
                    .iter()
                    .position(|candidate| *candidate == section)
                    .unwrap_or(0);
                let count = SettingsSection::ALL.len();
                let next = SettingsSection::ALL[if key.key == "up" {
                    (index + count - 1) % count
                } else {
                    (index + 1) % count
                }];
                if let Some(pane) = &pane {
                    self.select_settings_section(pane, next, window, cx);
                }
                Some(Stop::Section(next))
            }
            ("up" | "down", _) => step(&controls, current, key.key == "down"),
            (
                "left" | "right",
                Some(Stop::Control {
                    action: SettingsAction::Opacity,
                    ..
                }),
            ) => {
                self.adjust_opacity(if key.key == "left" { -0.05 } else { 0.05 }, window, cx);
                current
            }
            ("right", Some(Stop::Section(_))) => controls.first().copied().or(current),
            ("left", Some(Stop::Control { .. })) if pane.is_some() => pane
                .as_ref()
                .and_then(|pane| self.settings.panes.get(pane))
                .map(|pane| Stop::Section(pane.section)),
            ("enter" | "space", Some(Stop::Section(section))) => {
                if let Some(pane) = &pane {
                    self.select_settings_section(pane, section, window, cx);
                }
                current
            }
            ("enter" | "space", Some(Stop::Control { action, .. })) => {
                self.activate_settings_action(action, window, cx);
                current
            }
            _ => return false,
        };
        if let Some(next) = next {
            self.set_settings_focus(host.clone(), next);
        }
        true
    }

    /// Shows `section` in the Settings pane `pane_id`, clearing its search.
    fn select_settings_section(
        &mut self,
        pane_id: &PaneId,
        section: SettingsSection,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(pane) = self.settings.panes.get_mut(pane_id) else {
            return;
        };
        pane.section = section;
        pane.scroll.handle.set_offset(point(px(0.0), px(0.0)));
        if !pane.query.is_empty() {
            pane.query.clear();
            pane.search
                .update(cx, |search, cx| search.set_value("", window, cx));
        }
    }

    pub(super) fn activate_settings_action(
        &mut self,
        action: SettingsAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            SettingsAction::Update(control) => self.activate_update_control(control, window, cx),
            SettingsAction::Metadata(index) => {
                let mut settings = self.config.metadata.clone();
                match index {
                    0 => settings.directory = !settings.directory,
                    1 => settings.process = !settings.process,
                    2 => settings.git = !settings.git,
                    _ => settings.dimensions = !settings.dimensions,
                }
                if let Err(error) = self.config.save_metadata_settings(settings) {
                    self.global_error = Some(error);
                }
            }
            SettingsAction::ToggleConfirmClose => {
                if let Err(error) = self.config.save_confirm_close(!self.config.confirm_close) {
                    self.global_error = Some(error);
                }
            }
            SettingsAction::FocusIndicator(indicator) => {
                if let Err(error) = self.config.save_focus_indicator(indicator) {
                    self.global_error = Some(error);
                }
            }
            SettingsAction::ProgramStatus(index) => {
                let mut settings = self.config.program_status.clone();
                match index {
                    0 => settings.tabs = !settings.tabs,
                    1 => settings.panes = !settings.panes,
                    _ => settings.sidebar = !settings.sidebar,
                }
                if let Err(error) = self.config.save_program_status_settings(settings) {
                    self.global_error = Some(error);
                }
            }
            SettingsAction::Scope(scope) => self.settings_scope = scope,
            SettingsAction::BrowseThemes(target) => {
                self.open_theme_catalog_target(self.settings_scope, target)
            }
            SettingsAction::ToggleTransparency => {
                self.set_transparent_background(
                    !self.scoped_appearance().transparent_background,
                    window,
                    cx,
                );
            }
            SettingsAction::ToggleBlur => {
                self.set_blur_background(
                    self.scoped_appearance().background_effect != BackgroundEffect::Blurred,
                    window,
                    cx,
                );
            }
            SettingsAction::FollowTheme => self.follow_theme_terminal_colors(window, cx),
            SettingsAction::FontSelect(popup) => self.popup = Some(popup),
            SettingsAction::Opacity => self.adjust_opacity(0.05, window, cx),
            SettingsAction::ResetWindowAppearance => self.reset_window_appearance(window),
            SettingsAction::ResetSidebar => self.execute(Command::ResetSidebarWidth, window, cx),
            SettingsAction::ResetLayout => self.execute(Command::ResetClientLayout, window, cx),
            SettingsAction::Zoom(command) => self.execute(command, window, cx),
            SettingsAction::OpenConfiguration => {
                self.execute(Command::OpenConfiguration, window, cx)
            }
            SettingsAction::OpenPalette => self.execute(Command::OpenPalette, window, cx),
            SettingsAction::ToggleFps => {
                self.state.show_fps = !self.state.show_fps;
                self.save_state();
            }
            SettingsAction::RebuildRenderer => self.rebuild_renderer(window),
            SettingsAction::CopyPerformance => self.copy_performance_diagnostics(cx),
            SettingsAction::Reconnect => self.execute(Command::Reconnect, window, cx),
            SettingsAction::OpenDiagnostics => self.execute(Command::OpenDiagnostics, window, cx),
            SettingsAction::RestartDaemon => self.execute(Command::RestartDaemon, window, cx),
            SettingsAction::Prompt(action) => self.activate_prompt_action(action),
            SettingsAction::Density(density) => self.apply_density(density, window, cx),
            SettingsAction::OpenSettings => {
                self.open_settings(Some(SettingsSection::Appearance), None, window, cx)
            }
        }
    }

    fn adjust_opacity(&mut self, delta: f32, window: &mut Window, cx: &mut Context<Self>) {
        let mut appearance = self.scoped_appearance();
        appearance.terminal_opacity = (appearance.terminal_opacity + delta).clamp(
            crate::config::MIN_TERMINAL_OPACITY,
            crate::config::MAX_TERMINAL_OPACITY,
        );
        self.apply_scoped_appearance(appearance, window, cx);
    }

    fn apply_ui_font(&mut self, preset: UiFontPreset, window: &Window, cx: &mut Context<Self>) {
        if self.config.ui_font == preset {
            return;
        }
        if let Err(error) = self.config.save_ui_font(preset) {
            self.global_error = Some(error);
            return;
        }
        self.ui_font = crate::font_catalog::resolve_ui_font(preset, window.text_system());
        self.broadcast_global_appearance(window, cx);
        cx.notify();
    }

    fn apply_density(
        &mut self,
        density: crate::theme::WorkspaceDensity,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.config.density == density {
            return;
        }
        if let Err(error) = self.config.save_density(density) {
            self.global_error = Some(error);
            return;
        }
        // Geometry changes go through the ordinary resize path; shells keep running.
        self.rebuild_layout(window, true);
        self.broadcast_global_appearance(window, cx);
        cx.notify();
    }

    fn apply_terminal_font(
        &mut self,
        preset: TerminalFontPreset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .config
            .configured_font
            .family
            .eq_ignore_ascii_case(preset.family())
        {
            return;
        }
        if let Err(error) = self.config.save_terminal_font(preset) {
            self.global_error = Some(error);
            return;
        }
        self.font_settings = self.config.font.clone();
        self.typography_scale = 0.0;
        if self.refresh_typography(window) {
            self.rebuild_layout(window, true);
        }
        self.broadcast_global_appearance(window, cx);
        cx.notify();
    }

    pub(super) fn rebuild_renderer(&mut self, window: &mut Window) {
        for view in &mut self.surface_views {
            if let Ok(mut cache) = view.row_render_cache.lock() {
                cache.clear();
            }
            view.image_cache.clear();
            view.image_pending.clear();
            view.image_rejected.clear();
            view.image_capacity_rejected.clear();
            view.image_cache_bytes = 0;
            view.image_viewport = None;
            view.images_dirty = true;
            view.image_retry = true;
            view.image_error = None;
        }
        for (_, image) in self.rendered_images.drain() {
            let _ = window.drop_image(image);
        }
        self.rendered_image_ids.clear();
        self.typography_scale = 0.0;
        self.refresh_typography(window);
        self.rebuild_layout(window, true);
        self.performance_notice =
            Some("Renderer rebuilt. Visual caches will repopulate as needed.".into());
    }

    pub(super) fn settings_heading(&self, title: &'static str) -> AnyElement {
        div()
            .text_size(px(18.0))
            .font_weight(FontWeight::SEMIBOLD)
            .child(title)
            .into_any_element()
    }

    pub(super) fn settings_subheading(&self, label: &str) -> AnyElement {
        let colors = *self.colors();
        div()
            .pb_1()
            .text_size(px(UI_MICRO_TEXT_SIZE))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(color(modal_text_color(colors.muted, &colors)))
            .child(label.to_uppercase())
            .into_any_element()
    }

    fn settings_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        focused: bool,
        tone: SettingsButtonTone,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        self.settings_button_face(id, label, focused, tone)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(on_click)
            .into_any_element()
    }

    /// A settings button that opens a popup: the popup's trigger takes its clicks.
    pub(super) fn settings_popup_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        focused: bool,
        destructive: bool,
    ) -> AnyElement {
        self.settings_button_face(
            id,
            label,
            focused,
            if destructive {
                SettingsButtonTone::Destructive
            } else {
                SettingsButtonTone::Secondary
            },
        )
        .into_any_element()
    }

    fn settings_button_face(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        focused: bool,
        tone: SettingsButtonTone,
    ) -> Stateful<Div> {
        let colors = *self.colors();
        let (background, hover, foreground) = match tone {
            SettingsButtonTone::Secondary => (
                blend_rgb(colors.surface, colors.foreground, 0.04),
                blend_rgb(colors.surface, colors.foreground, 0.09),
                colors.foreground,
            ),
            SettingsButtonTone::Primary => (
                blend_rgb(colors.surface, colors.accent, 0.12),
                blend_rgb(colors.surface, colors.accent, 0.20),
                colors.foreground,
            ),
            SettingsButtonTone::Destructive => (
                blend_rgb(colors.surface, colors.error, 0.06),
                blend_rgb(colors.surface, colors.error, 0.12),
                colors.error,
            ),
        };
        let border = if focused {
            if matches!(tone, SettingsButtonTone::Primary) {
                colors.foreground
            } else {
                colors.accent
            }
        } else if matches!(tone, SettingsButtonTone::Primary) {
            colors.accent
        } else {
            background
        };
        div()
            .id(id)
            .h(px(32.0))
            .min_w(px(72.0))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(UI_BODY_TEXT_SIZE))
            .line_height(px(UI_BODY_LINE_HEIGHT))
            .text_center()
            .whitespace_nowrap()
            .rounded_sm()
            .border_1()
            .border_color(color(border))
            .bg(color(background))
            .font_weight(if matches!(tone, SettingsButtonTone::Secondary) {
                FontWeight::MEDIUM
            } else {
                FontWeight::SEMIBOLD
            })
            .text_color(color(ui_text_color(foreground, background)))
            .hover(move |style| style.bg(color(hover)).cursor_pointer())
            .child(Into::<SharedString>::into(label))
    }

    pub(super) fn settings_action_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        focused: bool,
        destructive: bool,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        self.settings_button(
            id,
            label,
            focused,
            if destructive {
                SettingsButtonTone::Destructive
            } else {
                SettingsButtonTone::Secondary
            },
            on_click,
        )
    }

    pub(super) fn settings_primary_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        focused: bool,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        self.settings_button(id, label, focused, SettingsButtonTone::Primary, on_click)
    }

    pub(super) fn settings_segment_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        active: bool,
        focused: bool,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let colors = *self.colors();
        let group = colors.surface;
        let selected = blend_rgb(colors.surface, colors.accent, 0.08);
        let background = if active { selected } else { group };
        div()
            .id(id)
            .h(px(32.0))
            .min_w(px(72.0))
            .flex_none()
            .px_3()
            .rounded_sm()
            .border_1()
            .border_color(color(if focused || active {
                colors.accent
            } else {
                colors.border
            }))
            .bg(color(background))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(UI_BODY_TEXT_SIZE))
            .line_height(px(UI_BODY_LINE_HEIGHT))
            .text_center()
            .whitespace_nowrap()
            .font_weight(if active {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::MEDIUM
            })
            .text_color(color(ui_text_color(
                if active {
                    colors.foreground
                } else {
                    colors.muted
                },
                background,
            )))
            .hover(move |style| {
                style
                    .bg(color(if active {
                        selected
                    } else {
                        colors.surface_hover
                    }))
                    .cursor_pointer()
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(on_click)
            .child(Into::<SharedString>::into(label))
            .into_any_element()
    }

    pub(super) fn overlay_close_button(
        &self,
        id: impl Into<gpui::ElementId>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let background = blend_rgb(colors.surface, colors.foreground, 0.07);
        let hover = blend_rgb(colors.surface, colors.foreground, 0.13);
        div()
            .id(id)
            .size(px(36.0))
            .rounded_sm()
            .flex()
            .items_center()
            .justify_center()
            .hover(move |style| style.bg(color(hover)).cursor_pointer())
            .tooltip(controls::tooltip("Close", None, Some("escape")))
            .on_click(cx.listener(|this, _, _, cx| {
                this.dismiss_overlay();
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                div()
                    .size(px(28.0))
                    .rounded_sm()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(color(background))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(16.0))
                    .text_color(color(ui_text_color(colors.foreground, background)))
                    .child("×"),
            )
            .into_any_element()
    }

    /// The section list of the Settings pane `pane_id`: a column beside the rows, or
    /// when `compact` a wrapped row above them. No entry is current while searching.
    fn render_settings_navigation(
        &self,
        pane_id: &PaneId,
        pane: &SettingsPane,
        compact: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let searching = !pane.query.trim().is_empty();
        let host = SettingsHost::Pane(pane_id.clone());
        let items = SettingsSection::ALL
            .into_iter()
            .enumerate()
            .map(|(index, section)| {
                let active = !searching && pane.section == section;
                let focused = self.settings.focus.as_ref()
                    == Some(&SettingsFocus {
                        host: host.clone(),
                        stop: Stop::Section(section),
                    });
                // Only the current section has a fill; the others sit on the pane.
                let background = blend_rgb(colors.surface, colors.foreground, 0.06);
                let pane_id = pane_id.clone();
                div()
                    .id(("settings-nav", index))
                    .relative()
                    .min_h(px(36.0))
                    .flex_none()
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(if focused {
                        color(colors.accent)
                    } else if active {
                        color(background)
                    } else {
                        gpui::transparent_black()
                    })
                    .when(active, |item| item.bg(color(background)))
                    .text_color(color(modal_text_color(
                        if active {
                            colors.foreground
                        } else {
                            colors.muted
                        },
                        &colors,
                    )))
                    .font_weight(if active {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    })
                    .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_settings_section(&pane_id, section, window, cx);
                        this.set_settings_focus(
                            SettingsHost::Pane(pane_id.clone()),
                            Stop::Section(section),
                        );
                        this.apply_settings_focus(&pane_id, window, cx);
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .child(section.label())
                    .child(self.settings_focus_anchor(focused, cx))
            });
        if compact {
            div()
                .flex_none()
                .px_4()
                .py_2()
                .flex()
                .flex_wrap()
                .gap_1()
                .border_b_1()
                .border_color(color(colors.border))
                .children(items)
                .into_any_element()
        } else {
            div()
                .w(px(SECTION_LIST_WIDTH))
                .flex_none()
                .p_3()
                .flex()
                .flex_col()
                .gap_1()
                .border_r_1()
                .border_color(color(colors.border))
                .children(items)
                .into_any_element()
        }
    }

    /// Scrolls the highlighted control into its settings area once, after a keyboard
    /// move. `focused` says whether this control has the highlight.
    pub(super) fn settings_focus_anchor(&self, focused: bool, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity();
        canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                if !focused {
                    return;
                }
                entity.update(cx, |this, _| {
                    let Some(host) = this.settings.focus.as_ref().map(|focus| focus.host.clone())
                    else {
                        return;
                    };
                    let Some(scroll) = this.settings_scroll_mut(&host) else {
                        return;
                    };
                    if !scroll.to_focus {
                        return;
                    }
                    let Some(viewport) = scroll.bounds else {
                        return;
                    };
                    scroll.to_focus = false;
                    let top = viewport.top() + px(8.0);
                    let bottom = viewport.bottom() - px(8.0);
                    let delta = if bounds.top() < top {
                        top - bounds.top()
                    } else if bounds.bottom() > bottom {
                        bottom - bounds.bottom()
                    } else {
                        px(0.0)
                    };
                    if delta != px(0.0) {
                        let offset = scroll.handle.offset();
                        scroll.handle.set_offset(point(offset.x, offset.y + delta));
                        window.refresh();
                    }
                });
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }

    /// Records where `host`'s settings area is, so keyboard moves can scroll into it.
    fn settings_scroll_viewport(&self, host: SettingsHost, cx: &Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        canvas(
            |_, _, _| (),
            move |bounds, _, _, cx| {
                entity.update(cx, |this, _| {
                    if let Some(scroll) = this.settings_scroll_mut(&host) {
                        scroll.bounds = Some(bounds);
                    }
                });
            },
        )
        .absolute()
        .inset_0()
    }

    pub(super) fn render_settings_toggle(
        &self,
        row: &RowCx,
        enabled: bool,
        action: SettingsAction,
        cx: &Context<Self>,
    ) -> AnyElement {
        let focused = self.settings_focused(row, action);
        let colors = *self.colors();
        div()
            .id((row.row, 0_usize))
            .relative()
            .flex_none()
            .min_w(px(92.0))
            .min_h(px(36.0))
            .px_2()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .rounded_sm()
            .border_1()
            .border_color(color(if focused {
                colors.accent
            } else {
                colors.border
            }))
            .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(self.settings_click(row, action, cx))
            .child(if enabled { "On" } else { "Off" })
            .child(
                div()
                    .w(px(32.0))
                    .h(px(18.0))
                    .p(px(2.0))
                    .flex()
                    .items_center()
                    .rounded_full()
                    .bg(color(if enabled {
                        colors.accent
                    } else {
                        colors.border
                    }))
                    .when(enabled, |switch| switch.justify_end())
                    .child(
                        div()
                            .size(px(14.0))
                            .rounded_full()
                            .bg(color(colors.foreground)),
                    ),
            )
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element()
    }

    pub(super) fn settings_row(
        &self,
        label: &'static str,
        detail: String,
        control: AnyElement,
        compact: bool,
    ) -> AnyElement {
        let colors = *self.colors();
        div()
            .min_w_0()
            .flex()
            .gap_3()
            .py_3()
            .border_b_1()
            .border_color(color(colors.border))
            .when(compact, |row| row.flex_col().items_start())
            .when(!compact, |row| row.items_center().justify_between())
            .child(
                div()
                    .min_w_0()
                    // Basis auto, not `flex_1`'s zero basis: with a zero basis Taffy sizes
                    // the row as if every character wrapped, which leaves blank scroll space
                    // below rows nested deeper in a column (the shell prompt group).
                    .when(!compact, |label| label.flex_auto())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().font_weight(FontWeight::MEDIUM).child(label))
                    .when(!detail.is_empty(), |label| {
                        label.child(
                            div()
                                .text_size(px(UI_SMALL_TEXT_SIZE))
                                .text_color(color(modal_text_color(colors.muted, &colors)))
                                .child(detail),
                        )
                    }),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_none()
                    .when(compact, |control| control.w_full())
                    .child(control),
            )
            .into_any_element()
    }

    /// One button for `action` of `row`.
    fn settings_control_button(
        &self,
        row: &RowCx,
        index: usize,
        label: &'static str,
        action: SettingsAction,
        destructive: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let focused = self.settings_focused(row, action);
        div()
            .relative()
            .flex_none()
            .child(self.settings_action_button(
                (row.row, index),
                label,
                focused,
                destructive,
                self.settings_click(row, action, cx),
            ))
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element()
    }

    /// Segment buttons for `choices`: label, whether it is the current one, and what
    /// it does.
    fn settings_segments(
        &self,
        row: &RowCx,
        choices: impl IntoIterator<Item = (&'static str, bool, SettingsAction)>,
        cx: &Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .when(row.compact, |controls| controls.flex_col().items_start())
            .gap_2()
            .children(
                choices
                    .into_iter()
                    .enumerate()
                    .map(|(index, (label, active, action))| {
                        let focused = self.settings_focused(row, action);
                        div()
                            .relative()
                            .flex_none()
                            .child(self.settings_segment_button(
                                (row.row, index),
                                label,
                                active,
                                focused,
                                self.settings_click(row, action, cx),
                            ))
                            .child(self.settings_focus_anchor(focused, cx))
                    }),
            )
            .into_any_element()
    }

    fn render_toggle_row(
        &self,
        row: &RowCx,
        enabled: bool,
        action: SettingsAction,
        detail: &'static str,
        cx: &Context<Self>,
    ) -> AnyElement {
        self.settings_row(
            row.title,
            detail.into(),
            self.render_settings_toggle(row, enabled, action, cx),
            row.compact,
        )
    }

    fn render_button_row(
        &self,
        row: &RowCx,
        detail: String,
        label: &'static str,
        action: SettingsAction,
        destructive: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        self.settings_row(
            row.title,
            detail,
            self.settings_control_button(row, 0, label, action, destructive, cx),
            row.compact,
        )
    }

    fn render_scope_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let choices = [
            (SettingsScope::Global, "Global defaults"),
            (SettingsScope::Window, "This window"),
        ]
        .map(|(scope, label)| {
            (
                label,
                self.settings_scope == scope,
                SettingsAction::Scope(scope),
            )
        });
        self.settings_row(
            row.title,
            match self.settings_scope {
                SettingsScope::Global => "Defaults for windows without an override.",
                SettingsScope::Window => "Only this window; global defaults stay unchanged.",
            }
            .into(),
            self.settings_segments(row, choices, cx),
            row.compact,
        )
    }

    fn render_theme_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let appearance = self.scoped_appearance();
        let theme_locked = self.config.provenance.theme == crate::config::ValueSource::CommandLine;
        let action = SettingsAction::BrowseThemes(CatalogTarget::Both);
        let focused = self.settings_focused(row, action);
        let theme_control = div()
            .relative()
            .min_w_0()
            .child(self.render_theme_catalog_entry_target(
                self.resolve_theme(&appearance.theme),
                CatalogTarget::Both,
                focused,
                cx,
            ))
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element();
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.settings_row(
                row.title,
                if theme_locked {
                    "Colors are fixed by --theme.".into()
                } else {
                    String::new()
                },
                theme_control,
                true,
            ))
            .when(appearance.terminal_theme_override, |section| {
                section.child(
                    div()
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .text_color(color(modal_text_color(colors.muted, &colors)))
                        .child("Terminal colors overridden. Change or reset in Advanced."),
                )
            })
            .into_any_element()
    }

    fn render_opacity_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let mut appearance = self.scoped_appearance();
        if self.opacity_drag_origin.is_some() {
            appearance.terminal_opacity = self.terminal_opacity;
        }
        let focused = self.settings_focused(row, SettingsAction::Opacity);
        let opacity_progress = ((appearance.terminal_opacity
            - crate::config::MIN_TERMINAL_OPACITY)
            / (crate::config::MAX_TERMINAL_OPACITY - crate::config::MIN_TERMINAL_OPACITY))
            .clamp(0.0, 1.0);
        let opacity_input = cx.entity();
        let slider_row = row.clone();
        let opacity_slider_input = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                window.on_mouse_event({
                    let input = opacity_input.clone();
                    let row = slider_row.clone();
                    move |event: &MouseDownEvent, _, window, cx| {
                        if event.button != MouseButton::Left || !bounds.contains(&event.position) {
                            return;
                        }
                        let opacity = opacity_at_slider_position(event.position.x, bounds);
                        input.update(cx, |this, cx| {
                            this.focus_settings_control(&row, SettingsAction::Opacity);
                            this.preview_terminal_opacity(opacity, window);
                            cx.stop_propagation();
                            cx.notify();
                        });
                    }
                });
                window.on_mouse_event({
                    let input = opacity_input.clone();
                    move |event: &MouseMoveEvent, _, window, cx| {
                        if !event.dragging() {
                            return;
                        }
                        let opacity = opacity_at_slider_position(event.position.x, bounds);
                        input.update(cx, |this, cx| {
                            if this.opacity_drag_origin.is_some() {
                                this.preview_terminal_opacity(opacity, window);
                                cx.stop_propagation();
                                cx.notify();
                            }
                        });
                    }
                });
            },
        )
        .absolute()
        .inset_0();
        div()
            .relative()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap_3()
                    .child(row.title)
                    .child(format!("{:.0}%", appearance.terminal_opacity * 100.0)),
            )
            .child(
                div()
                    .id((row.row, 0_usize))
                    .relative()
                    .mx_2()
                    .h(px(44.0))
                    .rounded_sm()
                    .border_1()
                    .border_color(color(if focused {
                        colors.accent
                    } else {
                        colors.surface
                    }))
                    .cursor(gpui::CursorStyle::ResizeLeftRight)
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .top(px(20.0))
                            .h(px(4.0))
                            .rounded_full()
                            .bg(color(colors.border)),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top(px(20.0))
                            .w(gpui::relative(opacity_progress))
                            .h(px(4.0))
                            .rounded_full()
                            .bg(color(colors.accent)),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(gpui::relative(opacity_progress))
                            .top(px(14.0))
                            .ml(px(-7.0))
                            .size(px(14.0))
                            .rounded_full()
                            .border_1()
                            .border_color(color(colors.foreground))
                            .bg(color(colors.accent)),
                    )
                    .child(opacity_slider_input),
            )
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element()
    }

    /// Interface font or Terminal font: a select of the bundled presets, each shown in
    /// its own font. A custom terminal family from the configuration file shows as the
    /// value with no preset checked.
    fn render_font_select(&self, row: &RowCx, popup: Popup, cx: &Context<Self>) -> AnyElement {
        let action = SettingsAction::FontSelect(popup);
        let focused = self.settings_focused(row, action);
        let (options, selected, value): (Vec<_>, _, SharedString) = match popup {
            Popup::UiFont => (
                UiFontPreset::ALL
                    .iter()
                    .map(|preset| {
                        controls::select_option(preset.label())
                            .detail("Aa Bb 0123")
                            .font(preset.family())
                    })
                    .collect(),
                UiFontPreset::ALL
                    .iter()
                    .position(|preset| *preset == self.config.ui_font),
                self.config.ui_font.label().into(),
            ),
            _ => (
                TerminalFontPreset::ALL
                    .iter()
                    .map(|preset| {
                        controls::select_option(preset.label())
                            .detail("0O1l {} [] $ cargo")
                            .font(preset.family())
                    })
                    .collect(),
                TerminalFontPreset::ALL.iter().position(|preset| {
                    self.config
                        .configured_font
                        .family
                        .eq_ignore_ascii_case(preset.family())
                }),
                self.config.configured_font.family.clone().into(),
            ),
        };
        let open_row = row.clone();
        let select = controls::select(
            match popup {
                Popup::UiFont => "ui-font-select",
                _ => "terminal-font-select",
            },
            options,
            selected,
            self.popup == Some(popup),
        )
        .placeholder(value)
        .focused(focused)
        .anchor(Anchor::TopRight)
        .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
            this.focus_settings_control(&open_row, action);
            this.set_popup(popup, *open);
            cx.notify();
        }))
        .on_pick(cx.listener(move |this, index: &usize, window, cx| {
            if popup == Popup::UiFont {
                if let Some(preset) = UiFontPreset::ALL.get(*index) {
                    this.apply_ui_font(*preset, window, cx);
                }
            } else if let Some(preset) = TerminalFontPreset::ALL.get(*index) {
                this.apply_terminal_font(*preset, window, cx);
            }
            cx.notify();
        }));
        div()
            .relative()
            .flex_none()
            .child(select)
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element()
    }

    fn render_density_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let choices = crate::theme::WorkspaceDensity::ALL.map(|density| {
            (
                density.label(),
                self.config.density == density,
                SettingsAction::Density(density),
            )
        });
        self.settings_row(
            row.title,
            match self.config.density {
                crate::theme::WorkspaceDensity::Comfy => {
                    "Rounded panes with gaps between them, in every window."
                }
                crate::theme::WorkspaceDensity::Compact => {
                    "Edge-to-edge panes for the most terminal space, in every window."
                }
            }
            .into(),
            self.settings_segments(row, choices, cx),
            row.compact,
        )
    }

    fn render_metadata_row(&self, row: &RowCx, index: usize, cx: &Context<Self>) -> AnyElement {
        let metadata = &self.config.metadata;
        let enabled = [
            metadata.directory,
            metadata.process,
            metadata.git,
            metadata.dimensions,
        ][index.min(3)];
        self.render_toggle_row(
            row,
            enabled,
            SettingsAction::Metadata(index),
            row_description(row.row),
            cx,
        )
    }

    fn render_status_row(&self, row: &RowCx, index: usize, cx: &Context<Self>) -> AnyElement {
        let status = &self.config.program_status;
        let enabled = [status.tabs, status.panes, status.sidebar][index.min(2)];
        self.render_toggle_row(
            row,
            enabled,
            SettingsAction::ProgramStatus(index),
            row_description(row.row),
            cx,
        )
    }

    fn render_focus_indicator_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let choices = crate::theme::FocusIndicator::ALL.map(|indicator| {
            (
                indicator.label(),
                self.config.focus_indicator == indicator,
                SettingsAction::FocusIndicator(indicator),
            )
        });
        self.settings_row(
            row.title,
            match self.config.focus_indicator {
                crate::theme::FocusIndicator::Marker => {
                    "A short accent bar on the focused pane; an outline flashes when focus moves."
                }
                crate::theme::FocusIndicator::Outline => {
                    "An accent outline around the focused pane."
                }
                crate::theme::FocusIndicator::Dim => "Other panes are dimmed.",
                crate::theme::FocusIndicator::None => {
                    "Only the cursor, which unfocused panes do not draw."
                }
            }
            .into(),
            self.settings_segments(row, choices, cx),
            row.compact,
        )
    }

    fn render_zoom_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let zoom = div()
            .flex()
            .gap_2()
            .child(self.settings_control_button(
                row,
                0,
                "−",
                SettingsAction::Zoom(Command::ZoomOut),
                false,
                cx,
            ))
            .child(self.settings_control_button(
                row,
                1,
                "Reset",
                SettingsAction::Zoom(Command::ZoomReset),
                false,
                cx,
            ))
            .child(self.settings_control_button(
                row,
                2,
                "+",
                SettingsAction::Zoom(Command::ZoomIn),
                false,
                cx,
            ))
            .into_any_element();
        self.settings_row(
            row.title,
            format!(
                "{:.0}% · {:.1}px · {:.2} line height",
                self.zoom * 100.0,
                self.font_settings.size,
                self.font_settings.line_height
            ),
            zoom,
            row.compact,
        )
    }

    fn render_terminal_colors_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let appearance = self.scoped_appearance();
        let theme_locked = self.config.provenance.theme == crate::config::ValueSource::CommandLine;
        let terminal_colors = div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().font_weight(FontWeight::MEDIUM).child(
                if appearance.terminal_theme_override {
                    self.resolve_theme(&appearance.terminal_theme)
                        .label()
                        .to_string()
                } else {
                    "Follow theme".into()
                },
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(self.settings_control_button(
                        row,
                        0,
                        "Follow theme",
                        SettingsAction::FollowTheme,
                        false,
                        cx,
                    ))
                    .child(self.settings_control_button(
                        row,
                        1,
                        "Choose override",
                        SettingsAction::BrowseThemes(CatalogTarget::Terminal),
                        false,
                        cx,
                    )),
            )
            .into_any_element();
        self.settings_row(
            row.title,
            if theme_locked {
                "Colors are fixed by --theme.".into()
            } else {
                row_description(row.row).into()
            },
            terminal_colors,
            true,
        )
    }

    fn render_restart_daemon_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let live_processes = self.command_context(cx).live_surface_count;
        div()
            .pt_3()
            .border_t_1()
            .border_color(color(colors.error).opacity(0.55))
            .child(self.settings_row(
                row.title,
                format!(
                    "{} running terminal{}. Restarting stops all running terminals.",
                    live_processes,
                    if live_processes == 1 { "" } else { "s" }
                ),
                self.settings_control_button(
                    row,
                    0,
                    if live_processes == 0 {
                        "Restart"
                    } else {
                        "Review restart"
                    },
                    SettingsAction::RestartDaemon,
                    live_processes > 0,
                    cx,
                ),
                row.compact,
            ))
            .into_any_element()
    }

    /// Pane or tab label of a shell this window shows.
    fn shell_label(&self, surface: &SurfaceId) -> Option<String> {
        let workspace = self.workspace.as_ref()?;
        for tab in workspace.sessions.iter().flat_map(|session| &session.tabs) {
            let mut leaves = Vec::new();
            collect_leaves(&tab.layout, &mut leaves);
            let Some(index) = leaves
                .iter()
                .position(|(_, id)| id.as_ref() == Some(surface))
            else {
                continue;
            };
            let custom = tab.label.trim();
            if leaves.len() == 1 && !custom.is_empty() {
                return Some(custom.to_owned());
            }
            return self
                .tab_panes(tab)
                .into_iter()
                .nth(index)
                .map(|pane| pane.title);
        }
        None
    }

    /// Every 200 ms: keeps a shown Updates section live and the sidebar dot current.
    pub(super) fn poll_updates(&mut self, cx: &mut Context<Self>) {
        let page = self.settings_section_shown(SettingsSection::Updates);
        if page {
            if !self.updates_page_open {
                self.updates.refresh_review();
            }
            self.updates.acknowledge_ready();
        }
        self.updates_page_open = page;
        let dot = self.updates.shows_dot();
        if page || dot != self.update_dot {
            self.update_dot = dot;
            cx.notify();
        }
    }

    /// What the Updates section shows for the current update state.
    fn update_view(&self) -> crate::updates::UpdateView {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.updates
            .snapshot()
            .view(now, |surface| self.shell_label(surface))
    }

    /// The available release's controls: More when its notes have details, then
    /// Release notes.
    fn candidate_stops(&self) -> Vec<SettingsAction> {
        let snapshot = self.updates.snapshot();
        let Some(release) = snapshot.candidate() else {
            return Vec::new();
        };
        let mut stops = Vec::new();
        if !crate::release_notes::of_release(&release.manifest)
            .details
            .is_empty()
        {
            stops.push(SettingsAction::Update(UpdateControl::CandidateMore));
        }
        stops.push(SettingsAction::Update(UpdateControl::CandidateNotes));
        stops
    }

    /// Advanced, then while it is open Repair (Windows) and Open update log.
    fn update_advanced_stops(&self) -> Vec<SettingsAction> {
        let mut stops = vec![SettingsAction::Update(UpdateControl::Advanced)];
        if self.settings_update_advanced {
            if cfg!(windows) {
                stops.push(SettingsAction::Update(UpdateControl::Repair));
            }
            stops.push(SettingsAction::Update(UpdateControl::OpenLog));
        }
        stops
    }

    fn activate_update_control(
        &mut self,
        control: UpdateControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::updates::{Preference, UpdateButton};
        match control {
            UpdateControl::Primary(button) => {
                let view = self.update_view();
                // The state moved on since this button was drawn; the next frame shows it.
                if view.button != Some(button) {
                    return;
                }
                match button {
                    UpdateButton::CheckNow => self.updates.check(),
                    UpdateButton::Download => self.updates.download(),
                    UpdateButton::Cancel => self.updates.cancel(),
                    // Installing restarts Compi: unsaved edits in any window ask first.
                    UpdateButton::Restart => self.guard_unsaved(
                        super::unsaved::CloseScope::App,
                        super::unsaved::AfterUnsaved::Install(Vec::new()),
                        window,
                        cx,
                    ),
                    // The listed shells are exactly what this click consents to end.
                    UpdateButton::RestartEndShells => self.guard_unsaved(
                        super::unsaved::CloseScope::App,
                        super::unsaved::AfterUnsaved::Install(
                            self.updates.snapshot().ending_shells(),
                        ),
                        window,
                        cx,
                    ),
                    UpdateButton::TryAgain => self.updates.retry(),
                    UpdateButton::DownloadSetup => {
                        if let Err(error) = open_web_url(crate::updates::SETUP_DOWNLOAD_URL) {
                            self.global_error = Some(error);
                        }
                    }
                }
            }
            UpdateControl::CheckForUpdates => {
                let enabled = self.updates.snapshot().preferences.check_for_updates;
                self.updates
                    .set_preference(self.config.clone(), Preference::CheckForUpdates(!enabled));
            }
            UpdateControl::DownloadAutomatically => {
                let enabled = self
                    .updates
                    .snapshot()
                    .preferences
                    .download_updates_automatically;
                self.updates.set_preference(
                    self.config.clone(),
                    Preference::DownloadAutomatically(!enabled),
                );
            }
            UpdateControl::CandidateMore => {
                self.settings_update_notes_expanded = !self.settings_update_notes_expanded
            }
            UpdateControl::CandidateNotes => {
                if let Some(release) = self.updates.snapshot().candidate() {
                    self.open_release_notes(&crate::release_notes::of_release(&release.manifest));
                }
            }
            UpdateControl::WhatsNewMore => {
                self.settings_whats_new_expanded = !self.settings_whats_new_expanded
            }
            UpdateControl::ReleaseNotes => {
                if let Some(notes) = crate::release_notes::current() {
                    self.open_release_notes(notes);
                }
            }
            UpdateControl::Advanced => {
                self.settings_update_advanced = !self.settings_update_advanced
            }
            UpdateControl::Repair => self.updates.repair(),
            UpdateControl::OpenLog => {
                if let Err(error) = open_update_log() {
                    self.global_error = Some(error);
                }
            }
        }
    }

    /// The installed version, the updater's status and its one primary button.
    fn render_update_status(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        use crate::updates::UpdateButton;
        let colors = *self.colors();
        let view = self.update_view();
        let primary = view.button.map(|button| {
            let action = SettingsAction::Update(UpdateControl::Primary(button));
            let focused = self.settings_focused(row, action);
            let tone = match button {
                UpdateButton::RestartEndShells => SettingsButtonTone::Destructive,
                UpdateButton::Cancel => SettingsButtonTone::Secondary,
                _ => SettingsButtonTone::Primary,
            };
            div()
                .relative()
                .flex_none()
                .child(self.settings_button(
                    (row.row, 0_usize),
                    button.label(),
                    focused,
                    tone,
                    self.settings_click(row, action, cx),
                ))
                .child(self.settings_focus_anchor(focused, cx))
        });
        let header = div()
            .min_w_0()
            .min_h(px(32.0))
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("Compi {}", env!("CARGO_PKG_VERSION"))),
            )
            .children(primary);
        let status = div()
            .min_w_0()
            .text_color(color(modal_text_color(
                if view.failed {
                    colors.error
                } else {
                    colors.muted
                },
                &colors,
            )))
            .child(view.status);
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .pb_3()
            .border_b_1()
            .border_color(color(colors.border))
            .child(header)
            .child(status)
            .when_some(view.progress, |summary, fraction| {
                summary.child(
                    div()
                        .h(px(4.0))
                        .rounded_full()
                        .bg(color(colors.border))
                        .child(
                            div()
                                .h(px(4.0))
                                .rounded_full()
                                .bg(color(colors.accent))
                                .w(gpui::relative(fraction)),
                        ),
                )
            })
            .into_any_element()
    }

    /// Release notes with More/Less and a link, under their version.
    fn render_notes_row(
        &self,
        row: &RowCx,
        notes: &crate::release_notes::ReleaseNotes,
        expanded: bool,
        more: UpdateControl,
        link: UpdateControl,
        cx: &Context<Self>,
    ) -> AnyElement {
        let more_action = SettingsAction::Update(more);
        let more = if notes.details.is_empty() && more == UpdateControl::CandidateMore {
            div().into_any_element()
        } else {
            self.settings_control_button(
                row,
                0,
                if expanded { "Less" } else { "More" },
                more_action,
                false,
                cx,
            )
        };
        let link = self.settings_control_button(
            row,
            1,
            "Release notes",
            SettingsAction::Update(link),
            false,
            cx,
        );
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.settings_subheading(&format!("What's new in {}", notes.version)))
            .child(self.render_release_notes_view(notes, expanded, more, link))
            .into_any_element()
    }

    fn render_candidate_notes(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let snapshot = self.updates.snapshot();
        let Some(release) = snapshot.candidate() else {
            return div().into_any_element();
        };
        let notes = crate::release_notes::of_release(&release.manifest);
        self.render_notes_row(
            row,
            &notes,
            self.settings_update_notes_expanded,
            UpdateControl::CandidateMore,
            UpdateControl::CandidateNotes,
            cx,
        )
    }

    fn render_whats_new_notes(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let Some(notes) = crate::release_notes::current() else {
            return div().into_any_element();
        };
        self.render_notes_row(
            row,
            notes,
            self.settings_whats_new_expanded,
            UpdateControl::WhatsNewMore,
            UpdateControl::ReleaseNotes,
            cx,
        )
    }

    fn render_update_advanced(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let action = SettingsAction::Update(UpdateControl::Advanced);
        let focused = self.settings_focused(row, action);
        let disclosure = div()
            .id((row.row, 0_usize))
            .relative()
            .min_h(px(32.0))
            .px_2()
            .flex()
            .items_center()
            .rounded_sm()
            .border_1()
            .border_color(color(if focused {
                colors.accent
            } else {
                colors.surface
            }))
            .font_weight(FontWeight::MEDIUM)
            .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(self.settings_click(row, action, cx))
            .child(if self.settings_update_advanced {
                "Advanced ▾"
            } else {
                "Advanced ▸"
            })
            .child(self.settings_focus_anchor(focused, cx));
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().flex().child(disclosure))
            .when(self.settings_update_advanced, |content| {
                content.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .when(cfg!(windows), |buttons| {
                            buttons.child(self.settings_control_button(
                                row,
                                1,
                                "Repair Compi",
                                SettingsAction::Update(UpdateControl::Repair),
                                false,
                                cx,
                            ))
                        })
                        .child(self.settings_control_button(
                            row,
                            2,
                            "Open update log",
                            SettingsAction::Update(UpdateControl::OpenLog),
                            false,
                            cx,
                        )),
                )
            })
            .into_any_element()
    }

    /// `rows` as a section shows them (`searching` false): a heading, then each row
    /// under its group's subheading; or as search results, each under its section.
    fn render_settings_rows(
        &self,
        host: &SettingsHost,
        rows: &[&'static Row],
        heading: Option<SettingsSection>,
        searching: bool,
        compact: bool,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let mut elements = Vec::with_capacity(rows.len() * 2 + 1);
        if let Some(section) = heading {
            elements.push(self.settings_heading(section.label()));
        }
        let mut section = None;
        let mut group = None;
        for row in rows {
            if searching {
                if section != Some(row.section) {
                    section = Some(row.section);
                    elements.push(
                        div()
                            .pt_2()
                            .child(self.settings_heading(row.section.label()))
                            .into_any_element(),
                    );
                }
            } else if row.group != group {
                group = row.group;
                if let Some(group) = group {
                    elements.push(self.settings_subheading(group));
                }
            }
            let row_cx = RowCx {
                host: host.clone(),
                row: row.id,
                title: row.title,
                compact,
            };
            elements.push((row.render)(self, &row_cx, cx));
        }
        elements
    }

    /// A Settings pane: a header with the search field, the section list beside the
    /// rows (above them when narrow), and the section's rows or the search results.
    pub(super) fn render_settings_pane(
        &self,
        pane_id: &PaneId,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(pane) = self.settings.panes.get(pane_id) else {
            return div().into_any_element();
        };
        let colors = *self.colors();
        let host = SettingsHost::Pane(pane_id.clone());
        let wide = width >= SECTION_LIST_MIN_WIDTH;
        let content_width = width - if wide { SECTION_LIST_WIDTH } else { 0.0 } - 32.0;
        let stacked = content_width < STACKED_ROW_WIDTH;
        let searching = !pane.query.trim().is_empty();
        let rows = self.pane_rows(pane);
        let body = if searching && rows.is_empty() {
            vec![
                div()
                    .pt_4()
                    .text_color(color(modal_text_color(colors.muted, &colors)))
                    .child(format!(
                        "No settings match “{}”.",
                        pane.search.read(cx).value().trim()
                    ))
                    .into_any_element(),
            ]
        } else {
            self.render_settings_rows(
                &host,
                &rows,
                (!searching).then_some(pane.section),
                searching,
                stacked,
                cx,
            )
        };
        let key_pane = pane_id.clone();
        let click_pane = pane_id.clone();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .track_focus(&pane.focus)
            .text_color(color(modal_text_color(colors.foreground, &colors)))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                this.settings_pane_key_down(&key_pane, event, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    // The search field takes focus itself; elsewhere the pane does.
                    if !window.default_prevented()
                        && let Some(pane) = this.settings.panes.get(&click_pane)
                    {
                        window.focus(&pane.focus, cx);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(color(colors.border))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(UI_BODY_TEXT_SIZE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Settings"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .max_w(px(360.0))
                            .child(Input::new(&pane.search).appearance(true).cleanable(true)),
                    ),
            )
            .when(!wide, |column| {
                column.child(self.render_settings_navigation(pane_id, pane, true, cx))
            })
            .child(
                div()
                    .relative()
                    .min_w_0()
                    .flex()
                    .min_h_0()
                    .flex_1()
                    .when(wide, |body| {
                        body.child(self.render_settings_navigation(pane_id, pane, false, cx))
                    })
                    .child(
                        div()
                            .id("settings-scroll")
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&pane.scroll.handle)
                            .p_4()
                            .child(div().min_w_0().flex().flex_col().gap_3().children(body)),
                    )
                    .child(self.settings_scroll_viewport(host, cx)),
            )
            .into_any_element()
    }

    /// Opens Quick Appearance on its button. The palette it may be run from is
    /// already closing.
    pub(super) fn open_quick_appearance(&mut self) {
        if self.overlay.is_some() {
            self.dismiss_overlay();
        }
        self.settings_scope = SettingsScope::Global;
        self.settings.focus = None;
        self.settings
            .quick
            .handle
            .set_offset(point(px(0.0), px(0.0)));
        self.popup = Some(Popup::QuickAppearance);
    }

    /// Back in Quick Appearance on the Theme row, after the theme catalog it opened.
    pub(super) fn return_to_quick_appearance(&mut self) {
        self.popup = Some(Popup::QuickAppearance);
        self.settings
            .quick
            .handle
            .set_offset(point(px(0.0), px(0.0)));
        self.set_settings_focus(
            SettingsHost::Quick,
            Stop::Control {
                row: "theme",
                action: SettingsAction::BrowseThemes(CatalogTarget::Both),
            },
        );
    }

    /// Quick Appearance: the Appearance rows in a popover on `trigger`, with a way on
    /// to all settings. It holds the keyboard while open; see `quick_appearance_key`.
    pub(super) fn render_quick_appearance(
        &self,
        id: &'static str,
        trigger: AnyElement,
        anchor: Anchor,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let open = self.popup == Some(Popup::QuickAppearance);
        let popover = controls::popover(id, trigger, open)
            .anchor(anchor)
            .on_open_change(cx.listener(|this, open: &bool, _, cx| {
                if *open {
                    this.open_quick_appearance();
                } else {
                    this.set_popup(Popup::QuickAppearance, false);
                }
                cx.notify();
            }))
            .on_key(cx.listener(|this, key: &Keystroke, window, cx| {
                this.quick_appearance_key(key, window, cx);
            }));
        if !open {
            return popover.into_any_element();
        }
        let (_, viewport_height) = overlay_viewport_size(window);
        let host = SettingsHost::Quick;
        let all = RowCx {
            host: host.clone(),
            row: "all-settings",
            title: "All settings…",
            compact: false,
        };
        let rows = self.quick_rows();
        popover
            .content(
                div()
                    .w(px(360.0))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Quick Appearance"),
                    )
                    .child(
                        div()
                            .relative()
                            .min_h_0()
                            .child(
                                div()
                                    .id("quick-appearance-scroll")
                                    .max_h(px((viewport_height - 160.0).max(160.0)))
                                    .overflow_y_scroll()
                                    .track_scroll(&self.settings.quick.handle)
                                    .child(div().min_w_0().flex().flex_col().gap_3().children(
                                        self.render_settings_rows(
                                            &host, &rows, None, false, false, cx,
                                        ),
                                    )),
                            )
                            .child(self.settings_scroll_viewport(host, cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .child(self.settings_primary_button(
                                "open-full-settings",
                                all.title,
                                self.settings_focused(&all, SettingsAction::OpenSettings),
                                self.settings_click(&all, SettingsAction::OpenSettings, cx),
                            )),
                    ),
            )
            .into_any_element()
    }
}

fn open_update_log() -> Result<(), String> {
    let path = crate::updates::update_log_path()
        .filter(|path| path.is_file())
        .ok_or("No update log yet.")?;
    #[cfg(windows)]
    let result = std::process::Command::new("notepad.exe").arg(&path).spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open")
        .arg("-e")
        .arg(&path)
        .spawn();
    result.map(|_| ()).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(query: &str) -> Vec<&'static str> {
        rows_for(SettingsSection::Appearance, query)
            .map(|row| row.id)
            .collect()
    }

    #[test]
    fn row_ids_are_unique_and_rows_are_in_section_order() {
        let mut seen = std::collections::HashSet::new();
        assert!(ROWS.iter().all(|row| seen.insert(row.id)));
        assert!(ROWS.windows(2).all(|pair| {
            let order = |section| SettingsSection::ALL.iter().position(|s| *s == section);
            order(pair[0].section) <= order(pair[1].section)
        }));
        assert!(!seen.contains("all-settings"));
        // Every section has rows to show.
        assert!(
            SettingsSection::ALL
                .iter()
                .all(|section| ROWS.iter().any(|row| row.section == *section))
        );
    }

    #[test]
    fn without_a_query_a_section_shows_its_own_rows() {
        assert_eq!(
            ids(""),
            [
                "scope",
                "theme",
                "transparency",
                "opacity",
                "blur",
                "window-overrides"
            ]
        );
        assert_eq!(ids("   "), ids(""));
        let keyboard: Vec<_> = rows_for(SettingsSection::Keyboard, "")
            .map(|row| row.id)
            .collect();
        assert_eq!(keyboard, ["commands", "keybindings"]);
    }

    #[test]
    fn search_matches_titles_descriptions_and_keywords_across_sections() {
        // Titles, in any section, ignoring case.
        assert_eq!(
            ids("font"),
            ["ui-font", "terminal-font", "custom-typography"]
        );
        assert_eq!(ids("FONT"), ids("font"));
        // Keywords: the prompt group is one row however many controls it holds.
        assert_eq!(ids("starship"), [PROMPT_ROW]);
        assert_eq!(ids("opacity"), ["transparency", "opacity"]);
        // Descriptions.
        assert_eq!(ids("foreground process"), ["metadata-process"]);
        // A group name finds its rows.
        assert_eq!(
            ids("tab metadata"),
            [
                "metadata-directory",
                "metadata-process",
                "metadata-git",
                "metadata-dimensions"
            ]
        );
        // Every term must match.
        assert!(ids("font zebra").is_empty());
        assert!(ids("zebra").is_empty());
        // Results keep section order, so each falls under its section's heading.
        let sections: Vec<_> = rows_for(SettingsSection::Appearance, "reset")
            .map(|row| row.section)
            .collect();
        assert!(sections.windows(2).all(|pair| {
            let order = |section| SettingsSection::ALL.iter().position(|s| *s == section);
            order(pair[0]) <= order(pair[1])
        }));
        assert!(contains_folded("Größe", "größe"));
        assert!(!contains_folded("ab", "abc"));
    }

    #[test]
    fn keyboard_order_is_search_sections_then_each_rows_controls() {
        let toggle = SettingsAction::ToggleConfirmClose;
        let zoom = [
            SettingsAction::Zoom(Command::ZoomOut),
            SettingsAction::Zoom(Command::ZoomReset),
            SettingsAction::Zoom(Command::ZoomIn),
        ];
        let order = focus_order(
            true,
            [
                ("confirm-close", vec![toggle]),
                ("zoom", zoom.to_vec()),
                ("empty", Vec::new()),
            ],
        );
        assert_eq!(order[0], Stop::Search);
        assert_eq!(
            order[1..=SettingsSection::ALL.len()],
            SettingsSection::ALL.map(Stop::Section)
        );
        let controls = &order[1 + SettingsSection::ALL.len()..];
        assert_eq!(
            controls,
            [
                Stop::Control {
                    row: "confirm-close",
                    action: toggle
                },
                Stop::Control {
                    row: "zoom",
                    action: zoom[0]
                },
                Stop::Control {
                    row: "zoom",
                    action: zoom[1]
                },
                Stop::Control {
                    row: "zoom",
                    action: zoom[2]
                },
            ]
        );
        // Tab and Shift+Tab wrap; a stop no longer shown starts over.
        let last = *order.last().unwrap();
        assert_eq!(step(&order, Some(last), true), Some(Stop::Search));
        assert_eq!(step(&order, Some(Stop::Search), false), Some(last));
        assert_eq!(step(&order, Some(order[3]), true), Some(order[4]));
        let gone = Stop::Control {
            row: "gone",
            action: toggle,
        };
        assert_eq!(step(&order, Some(gone), true), Some(Stop::Search));
        assert_eq!(step(&order, None, false), Some(last));
        assert_eq!(step(&[], None, true), None);
        // Quick Appearance has no search field or section list.
        assert_eq!(
            focus_order(false, [("confirm-close", vec![toggle])]),
            [Stop::Control {
                row: "confirm-close",
                action: toggle
            }]
        );
    }
}
