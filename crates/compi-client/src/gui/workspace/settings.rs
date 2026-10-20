//! Settings: a content pane (one per window, opened beside the focused pane) and the
//! Quick Appearance popover. Every setting is a row of [`ROWS`]: its section, title,
//! description, search keywords, the controls it holds and how it draws. A pane's
//! section view, its search results and the keyboard order are all derived from
//! the rows shown, so adding a setting is adding a row.
//!
//! The section a pane shows lives in the daemon (`PaneContent::Settings`), so it
//! persists with the pane; the search, scroll and keyboard highlight are this
//! window's.
//!
//! A pane draws a one-line header (title, section icons, search); the selected icon
//! names the section, so a page is just its cards of rows, each under its group's
//! label when the section has more than one group, with the header rows (the scope
//! switch) at the right end of the first label line. The Appearance page and
//! Advanced's terminal colors page end in the theme grid (`catalog`), which fills the
//! pane and previews on the real panes until the Apply / Cancel bar decides.
use super::catalog::{CatalogTarget, GridStep};
use super::*;
use crate::gui::controls::IconName;
use compi_protocol::{PaneContent, SettingsContent, SettingsSection};
use gpui::Entity;
use gpui_component::Sizable as _;
use gpui_component::input::{Input, InputEvent, InputState};

/// Widest a section page grows; a wider pane centres it.
const PAGE_MAX_WIDTH: f32 = 1280.0;
/// The Appearance row of the theme grid.
pub(super) const THEME_ROW: &str = "theme";
/// Narrower than this, a row's control goes below its title.
const STACKED_ROW_WIDTH: f32 = 440.0;
/// Narrower than this, the header drops its title so the section icons fit.
const HEADER_TITLE_MIN_WIDTH: f32 = 380.0;
/// The width of a drawn choice (pane density, focus indicator).
const OPTION_CARD_WIDTH: f32 = 120.0;
/// The row holding the shell prompt group; see `prompt`.
pub(super) const PROMPT_ROW: &str = "shell-prompt";
/// The theme list's controls, as one row of the keyboard order; see `catalog`.
pub(super) const THEME_LIST_ROW: &str = "theme-list";
/// The theme preview's Apply / Cancel bar, as one row of the keyboard order.
pub(super) const THEME_BAR_ROW: &str = "theme-bar";
/// Advanced's group of the configuration file and the terminal server.
const RECOVERY_GROUP: &str = "Configuration and recovery";

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
    /// Global defaults or this window: one switch, Left and Right or Enter flip it.
    Scope,
    /// Opens the theme list for `CatalogTarget`, in the Settings pane.
    AllThemes(CatalogTarget),
    /// The theme list's back control.
    ThemeBack,
    /// The theme list's search field.
    ThemeSearch,
    /// The theme list's filter: Left and Right or Enter move it.
    ThemeFilter,
    /// The theme list's More menu.
    ThemeMore,
    /// The theme list's rows: Up and Down preview the next one, Enter applies.
    ThemeList,
    /// The star of the list's selected theme.
    ThemeFavorite,
    ThemeCancel,
    /// Apply the preview, or in removal remove the selected theme.
    ThemeApply,
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
    /// The theme list's search field; its text is the theme session's query.
    theme_search: Entity<InputState>,
    /// The search, lowercased; empty shows `section`.
    query: String,
    /// The section shown. It is sent to the daemon when it differs from `persisted`,
    /// the section the daemon last reported; `sent` is the one on its way.
    section: SettingsSection,
    persisted: SettingsSection,
    sent: Option<SettingsSection>,
    scroll: SettingsScroll,
    /// The theme grid's own scroll, on the Appearance and terminal colors pages.
    grid: SettingsScroll,
    /// How many columns the theme grid had when last drawn; Up and Down move by it.
    grid_columns: std::cell::Cell<usize>,
    _events: gpui::Subscription,
    _theme_events: gpui::Subscription,
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
    /// The theme list the next Settings pane drawn opens, for the same command.
    pending_themes: Option<CatalogTarget>,
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
    pub(super) title: &'static str,
    pub(super) compact: bool,
    /// Drawn on the page's first label line rather than as a row.
    pub(super) header: bool,
}

/// One setting: its place, the words search matches, the controls it holds in
/// keyboard order, and how it draws.
pub(super) struct Row {
    id: &'static str,
    section: SettingsSection,
    /// The group it sits in within its section; its label heads the group's card.
    group: Option<&'static str>,
    title: &'static str,
    description: &'static str,
    keywords: &'static str,
    /// Drawn at the right end of the page's first label line rather than in a card;
    /// search results show it as a row.
    header: bool,
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
        header: false,
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

const fn in_header(row: Row) -> Row {
    Row {
        header: true,
        ..row
    }
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
    in_header(row(
        "scope",
        SettingsSection::Appearance,
        "Scope",
        "Change the global defaults or only this window.",
        "global window override",
        |_| vec![SettingsAction::Scope],
        |app, row, cx| app.render_scope_row(row, cx),
    )),
    grouped(
        "Window",
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
    ),
    // Opacity and blur only act on a transparent background; otherwise they show
    // greyed and the keyboard passes them by.
    grouped(
        "Window",
        row(
            "opacity",
            SettingsSection::Appearance,
            "Opacity",
            "How much of the desktop shows through.",
            "transparency translucent",
            |app| {
                if app.scoped_appearance().transparent_background {
                    vec![SettingsAction::Opacity]
                } else {
                    Vec::new()
                }
            },
            |app, row, cx| app.render_opacity_row(row, cx),
        ),
    ),
    grouped(
        "Window",
        row(
            "blur",
            SettingsSection::Appearance,
            "Blur background",
            "Soften what's behind the window.",
            "transparency translucent glass acrylic",
            |app| {
                if app.scoped_appearance().transparent_background {
                    vec![SettingsAction::ToggleBlur]
                } else {
                    Vec::new()
                }
            },
            |app, row, cx| {
                let appearance = app.scoped_appearance();
                let blurred = appearance.background_effect == BackgroundEffect::Blurred;
                if appearance.transparent_background {
                    app.render_toggle_row(row, blurred, SettingsAction::ToggleBlur, "", cx)
                } else {
                    app.settings_row(
                        row.title,
                        "Needs a transparent background.".into(),
                        div()
                            .opacity(0.45)
                            .child(controls::switch((row.row, 0_usize), blurred).label(row.title))
                            .into_any_element(),
                        false,
                    )
                }
            },
        ),
    ),
    shown_when(
        |app| app.settings_scope == SettingsScope::Window,
        grouped(
            "Window",
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
    ),
    // The theme grid; on the Appearance page it fills the rest of the pane under its
    // own header line.
    grouped(
        "Theme",
        row(
            THEME_ROW,
            SettingsSection::Appearance,
            "Theme",
            "The color theme of the interface and terminals.",
            "colors color scheme dark light catalog browse favorites",
            |app| {
                if app
                    .theme_catalog
                    .as_ref()
                    .is_some_and(|catalog| catalog.target == CatalogTarget::Both)
                {
                    theme_grid_stops()
                } else {
                    vec![SettingsAction::AllThemes(CatalogTarget::Both)]
                }
            },
            |app, row, cx| app.render_theme_row(row, cx),
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
    in_header(row(
        "advanced-scope",
        SettingsSection::Advanced,
        "Scope",
        "Change the global defaults or only this window.",
        "global window override",
        |_| vec![SettingsAction::Scope],
        |app, row, cx| app.render_scope_row(row, cx),
    )),
    grouped(
        "Colors",
        row(
            "terminal-colors",
            SettingsSection::Advanced,
            "Terminal colors",
            "Override the theme's terminal colors; colors set by programs stay unchanged.",
            "theme palette ansi override follow",
            |app| {
                let mut stops = Vec::with_capacity(2);
                if app.scoped_appearance().terminal_theme_override {
                    stops.push(SettingsAction::FollowTheme);
                }
                stops.push(SettingsAction::AllThemes(CatalogTarget::Terminal));
                stops
            },
            |app, row, cx| app.render_terminal_colors_row(row, cx),
        ),
    ),
    grouped(
        RECOVERY_GROUP,
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
    ),
    grouped(
        RECOVERY_GROUP,
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
    ),
    grouped(
        RECOVERY_GROUP,
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
    ),
    grouped(
        RECOVERY_GROUP,
        row(
            "restart-daemon",
            SettingsSection::Advanced,
            "Restart daemon",
            "Restart the terminal server; running terminals stop.",
            "daemon server restart stop terminals",
            |_| vec![SettingsAction::RestartDaemon],
            |app, row, cx| app.render_restart_daemon_row(row, cx),
        ),
    ),
];

/// The registered description of the row `id`, for rows that show it as is.
fn row_description(id: &str) -> &'static str {
    ROWS.iter()
        .find(|row| row.id == id)
        .map_or("", |row| row.description)
}

/// The theme grid's keyboard order: its search, filter, More, the tiles, the star.
fn theme_grid_stops() -> Vec<SettingsAction> {
    vec![
        SettingsAction::ThemeSearch,
        SettingsAction::ThemeFilter,
        SettingsAction::ThemeMore,
        SettingsAction::ThemeList,
        SettingsAction::ThemeFavorite,
    ]
}

/// The terminal colors page's keyboard order, before the preview bar's.
fn theme_list_stops() -> Vec<SettingsAction> {
    let mut stops = vec![SettingsAction::ThemeBack];
    stops.extend(theme_grid_stops());
    stops
}

/// Whether `stop` sits in the theme grid, which scrolls on its own.
fn in_theme_grid(stop: Stop) -> bool {
    matches!(
        stop,
        Stop::Control {
            action: SettingsAction::ThemeList | SettingsAction::ThemeFavorite,
            ..
        }
    )
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

/// Whether `section`'s rows (header rows aside) fall into more than one group, so its
/// cards show their group's label; one group shows just the card.
fn section_grouped(section: SettingsSection) -> bool {
    let mut groups = ROWS
        .iter()
        .filter(|row| row.section == section && !row.header)
        .map(|row| row.group);
    groups
        .next()
        .is_some_and(|first| groups.any(|group| group != first))
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

/// The Settings panes of `node`: content panes, and terminal views on either side.
fn settings_leaves<'a>(node: &'a LayoutNode, output: &mut Vec<&'a PaneId>) {
    match node {
        LayoutNode::Split { first, second, .. } => {
            settings_leaves(first, output);
            settings_leaves(second, output);
        }
        leaf => {
            if let (Some(pane_id), Some(PaneContent::Settings(_))) =
                (leaf.leaf_pane(), leaf.content())
            {
                output.push(pane_id);
            }
        }
    }
}

/// What typing in, entering and leaving the search field of Settings pane `pane_id` do.
fn settings_search_events(
    pane_id: &PaneId,
    search: &Entity<InputState>,
    cx: &mut Context<CompiApp>,
) -> gpui::Subscription {
    let changed = pane_id.clone();
    cx.subscribe(search, move |this, search, event: &InputEvent, cx| {
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
            // Leaving an empty field folds it back behind its icon.
            InputEvent::Blur => {
                let Some(pane) = this.settings.panes.get(&changed) else {
                    return;
                };
                let host = SettingsHost::Pane(changed.clone());
                if !pane.query.trim().is_empty()
                    || this.settings.focus.as_ref()
                        != Some(&SettingsFocus {
                            host: host.clone(),
                            stop: Stop::Search,
                        })
                {
                    return;
                }
                let section = pane.section;
                this.settings.focus = Some(SettingsFocus {
                    host,
                    stop: Stop::Section(section),
                });
            }
            InputEvent::PressEnter { .. } => return,
        }
        cx.notify();
    })
}

/// What typing in and entering the theme search field of Settings pane `pane_id` do.
fn theme_search_events(
    pane_id: &PaneId,
    theme_search: &Entity<InputState>,
    cx: &mut Context<CompiApp>,
) -> gpui::Subscription {
    let changed = pane_id.clone();
    cx.subscribe(theme_search, move |this, search, event: &InputEvent, cx| {
        match event {
            InputEvent::Change => {
                let query = search.read(cx).value().to_lowercase();
                if let Some(catalog) = &mut this.theme_catalog
                    && catalog.pane == changed
                {
                    catalog.query = query;
                }
            }
            InputEvent::Focus => {
                this.settings.focus = Some(SettingsFocus {
                    host: SettingsHost::Pane(changed.clone()),
                    stop: Stop::Control {
                        row: this.theme_stop_row(&changed),
                        action: SettingsAction::ThemeSearch,
                    },
                });
            }
            InputEvent::PressEnter { .. } | InputEvent::Blur => return,
        }
        cx.notify();
    })
}

impl CompiApp {
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
        let pane_id = match host {
            SettingsHost::Quick => {
                // Quick Appearance draws the theme as one button on to Settings.
                let rows = self.quick_rows();
                let mut order = focus_order(
                    false,
                    rows.iter().map(|row| {
                        if row.id == THEME_ROW {
                            (row.id, vec![SettingsAction::AllThemes(CatalogTarget::Both)])
                        } else {
                            (row.id, (row.stops)(self))
                        }
                    }),
                );
                order.push(Stop::Control {
                    row: "all-settings",
                    action: SettingsAction::OpenSettings,
                });
                return order;
            }
            SettingsHost::Pane(pane_id) => pane_id,
        };
        let Some(pane) = self.settings.panes.get(pane_id) else {
            return Vec::new();
        };
        let mut order = if self.theme_list_shown(pane_id) {
            focus_order(true, [(THEME_LIST_ROW, theme_list_stops())])
        } else {
            let rows = self.pane_rows(pane);
            focus_order(true, rows.iter().map(|row| (row.id, (row.stops)(self))))
        };
        if self.theme_bar_shown(pane_id) {
            order.extend(
                [SettingsAction::ThemeCancel, SettingsAction::ThemeApply].map(|action| {
                    Stop::Control {
                        row: THEME_BAR_ROW,
                        action,
                    }
                }),
            );
        }
        order
    }

    /// Whether the Settings pane `pane_id` shows the terminal colors page (a theme grid
    /// for the terminal colors override) in place of Advanced.
    fn theme_list_shown(&self, pane_id: &PaneId) -> bool {
        self.theme_catalog.as_ref().is_some_and(|catalog| {
            &catalog.pane == pane_id && catalog.list && catalog.target == CatalogTarget::Terminal
        }) && self
            .settings
            .panes
            .get(pane_id)
            .is_some_and(|pane| pane.query.trim().is_empty())
    }

    /// The row the theme grid's controls belong to in `pane_id`: the terminal colors
    /// page's, or Appearance's theme row.
    fn theme_stop_row(&self, pane_id: &PaneId) -> &'static str {
        if self.theme_list_shown(pane_id) {
            THEME_LIST_ROW
        } else {
            THEME_ROW
        }
    }

    /// Whether the Settings pane `pane_id` shows the theme preview's Apply / Cancel bar.
    fn theme_bar_shown(&self, pane_id: &PaneId) -> bool {
        self.theme_catalog
            .as_ref()
            .is_some_and(|catalog| &catalog.pane == pane_id && catalog.awaiting_decision())
    }

    /// The Settings pane whose control has the keyboard highlight.
    fn highlighted_pane(&self) -> Option<PaneId> {
        match &self.settings.focus.as_ref()?.host {
            SettingsHost::Pane(pane) => Some(pane.clone()),
            SettingsHost::Quick => None,
        }
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

    /// The scroll `host` keeps; with `grid`, a pane's theme grid scroll.
    fn settings_scroll_mut(
        &mut self,
        host: &SettingsHost,
        grid: bool,
    ) -> Option<&mut SettingsScroll> {
        match host {
            SettingsHost::Quick => Some(&mut self.settings.quick),
            SettingsHost::Pane(pane) => self.settings.panes.get_mut(pane).map(|pane| {
                if grid {
                    &mut pane.grid
                } else {
                    &mut pane.scroll
                }
            }),
        }
    }

    /// Moves the keyboard highlight to `stop` where `host` draws, scrolling it into view.
    fn set_settings_focus(&mut self, host: SettingsHost, stop: Stop) {
        if let Some(scroll) = self.settings_scroll_mut(&host, in_theme_grid(stop)) {
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
            header: false,
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
    pub(super) fn settings_click(
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
    /// showing it where a terminal carries it; else shows it in place of the focused
    /// terminal, else opens one beside the focused pane. The keyboard starts on `stop`,
    /// else on the section; the search field stays behind its icon until wanted.
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
            self.reveal_view(&pane_id);
            self.focus_pane(pane_id.clone());
            let mut current = section.unwrap_or_default();
            if let Some(pane) = self.settings.panes.get_mut(&pane_id) {
                if let Some(section) = section {
                    pane.section = section;
                    pane.query.clear();
                    pane.search
                        .update(cx, |search, cx| search.set_value("", window, cx));
                }
                current = pane.section;
                pane.scroll.handle.set_offset(point(px(0.0), px(0.0)));
            }
            self.set_settings_focus(
                SettingsHost::Pane(pane_id),
                stop.unwrap_or(Stop::Section(current)),
            );
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
        self.settings.pending = Some(stop.unwrap_or(Stop::Section(section.unwrap_or_default())));
        if let Some(terminal) = self.focused_terminal() {
            self.show_view(&terminal, content, window, cx);
            return;
        }
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

    /// Opens Settings on the theme grid for `target`: Appearance's, or Advanced's
    /// terminal colors page. Quick Appearance's theme and Browse themes come here.
    pub(super) fn open_theme_settings(
        &mut self,
        target: CatalogTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (section, row) = match target {
            CatalogTarget::Both => (SettingsSection::Appearance, THEME_ROW),
            CatalogTarget::Terminal => (SettingsSection::Advanced, THEME_LIST_ROW),
        };
        let stop = Stop::Control {
            row,
            action: SettingsAction::ThemeList,
        };
        self.settings.pending = None;
        self.open_settings(Some(section), Some(stop), window, cx);
        if self.settings.pending.is_some() {
            // The pane opens with the next workspace update.
            self.settings.pending_themes = Some(target);
        } else if let Some((_, _, pane)) = self.window_settings_pane() {
            self.start_theme_session(pane, target, true);
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

    /// Settings moved from `from` to pane `to` with its view: its search, section,
    /// scroll and keyboard highlight go along.
    pub(super) fn move_settings_pane(
        &mut self,
        from: &PaneId,
        to: &PaneId,
        cx: &mut Context<Self>,
    ) {
        if let Some(mut pane) = self.settings.panes.remove(from) {
            pane._events = settings_search_events(to, &pane.search, cx);
            pane._theme_events = theme_search_events(to, &pane.theme_search, cx);
            self.settings.panes.insert(to.clone(), pane);
        }
        if let Some(focus) = &mut self.settings.focus
            && focus.host == SettingsHost::Pane(from.clone())
        {
            focus.host = SettingsHost::Pane(to.clone());
        }
        if let Some(catalog) = &mut self.theme_catalog
            && catalog.pane == *from
        {
            catalog.pane = to.clone();
        }
    }

    /// Before drawing: state for each Settings pane shown, its section sent to the
    /// daemon when it changed, and the shell prompt detected when its group shows.
    pub(super) fn prepare_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let moving = self.views.moving_from();
        self.settings.panes.retain(|pane, _| {
            moving == Some(pane)
                || content::find_pane_content(workspace, pane)
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
                    let events = settings_search_events(&pane_id, &search, cx);
                    let theme_search =
                        cx.new(|cx| InputState::new(window, cx).placeholder("Search themes"));
                    let theme_events = theme_search_events(&pane_id, &theme_search, cx);
                    self.settings.panes.insert(
                        pane_id.clone(),
                        SettingsPane {
                            focus: cx.focus_handle(),
                            search,
                            theme_search,
                            query: String::new(),
                            section: daemon,
                            persisted: daemon,
                            sent: None,
                            scroll: SettingsScroll::default(),
                            grid: SettingsScroll::default(),
                            grid_columns: std::cell::Cell::new(1),
                            _events: events,
                            _theme_events: theme_events,
                        },
                    );
                    if let Some(stop) = self.settings.pending.take() {
                        self.set_settings_focus(SettingsHost::Pane(pane_id.clone()), stop);
                    }
                    if let Some(target) = self.settings.pending_themes.take() {
                        self.start_theme_session(pane_id.clone(), target, true);
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
        // The theme session lasts while its pane shows its section and, once the pane
        // had the keyboard, while it keeps it.
        if let Some((pane_id, section)) = self
            .theme_catalog
            .as_ref()
            .map(|catalog| (catalog.pane.clone(), catalog.section()))
        {
            let focused = self.focused_pane() == Some(&pane_id);
            let shows = self
                .settings
                .panes
                .get(&pane_id)
                .is_some_and(|pane| pane.section == section && pane.query.trim().is_empty());
            let held = self.theme_catalog.as_mut().is_some_and(|catalog| {
                catalog.held |= focused;
                catalog.held
            });
            if !shows || (held && !focused) {
                self.cancel_catalog_preview();
                if self.popup == Some(Popup::CatalogMenu) {
                    self.popup = None;
                }
            }
        }
        // The Appearance page always shows the theme grid, so it always has a session.
        let appearance = self
            .shown_content_panes()
            .find(|pane| {
                self.settings.panes.get(*pane).is_some_and(|ui| {
                    ui.section == SettingsSection::Appearance && ui.query.trim().is_empty()
                })
            })
            .cloned();
        if self.theme_catalog.is_none()
            && let Some(pane_id) = appearance
        {
            self.start_theme_session(pane_id.clone(), CatalogTarget::Both, true);
            let query = self
                .settings
                .panes
                .get(&pane_id)
                .map(|ui| ui.theme_search.read(cx).value().to_lowercase());
            if let (Some(catalog), Some(query)) = (&mut self.theme_catalog, query) {
                catalog.query = query;
            }
        }
        // A control clicked while a search field had the keyboard takes it.
        if let Some(pane_id) = self.focused_pane().cloned()
            && let Some(pane) = self.settings.panes.get(&pane_id)
            && (pane.search.focus_handle(cx).is_focused(window)
                || pane.theme_search.focus_handle(cx).is_focused(window))
            && let Some(keyboard) = self.settings_keyboard(&pane_id, cx)
            && !keyboard.is_focused(window)
        {
            window.focus(&keyboard, cx);
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
                || pane.theme_search.focus_handle(cx) == *focused
        })
    }

    /// Where the keyboard goes in the focused Settings pane `pane`: its search field
    /// unless a control has the highlight there, the theme list's search field when
    /// that has it.
    pub(super) fn settings_keyboard(&self, pane: &PaneId, cx: &App) -> Option<FocusHandle> {
        let ui = self.settings.panes.get(pane)?;
        let stop = self
            .settings
            .focus
            .as_ref()
            .filter(|focus| focus.host == SettingsHost::Pane(pane.clone()))
            .map(|focus| focus.stop);
        Some(match stop {
            None | Some(Stop::Search) => ui.search.focus_handle(cx),
            Some(Stop::Control {
                action: SettingsAction::ThemeSearch,
                ..
            }) => ui.theme_search.focus_handle(cx),
            Some(_) => ui.focus.clone(),
        })
    }

    /// The focused Settings pane's search field, or its theme list's, has the keyboard.
    pub(super) fn settings_search_has_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.focused_pane()
            .and_then(|pane| self.settings.panes.get(pane))
            .is_some_and(|pane| {
                pane.search.focus_handle(cx).is_focused(window)
                    || pane.theme_search.focus_handle(cx).is_focused(window)
            })
    }

    /// Keys in the focused Settings pane's search fields, before the field's own: Tab
    /// and Down move on to the controls, Escape clears the search or leaves (Settings,
    /// or the theme list).
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
        let themes = self
            .settings
            .panes
            .get(&pane_id)
            .is_some_and(|pane| pane.theme_search.focus_handle(cx).is_focused(window));
        let host = SettingsHost::Pane(pane_id.clone());
        let field = if themes {
            Stop::Control {
                row: self.theme_stop_row(&pane_id),
                action: SettingsAction::ThemeSearch,
            }
        } else {
            Stop::Search
        };
        match key.key.as_str() {
            "tab" | "down" | "enter" => {
                let order = self.settings_order(&host);
                let forward = !(key.key == "tab" && modifiers.shift);
                let next = if key.key == "tab" {
                    step(&order, Some(field), forward)
                } else if themes {
                    // Down and Enter go to the theme rows.
                    Some(Stop::Control {
                        row: self.theme_stop_row(&pane_id),
                        action: SettingsAction::ThemeList,
                    })
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
            "escape" if themes => {
                let Some(pane) = self.settings.panes.get(&pane_id) else {
                    return false;
                };
                if pane.theme_search.read(cx).value().is_empty() {
                    self.settings_escape(&pane_id);
                } else {
                    pane.theme_search
                        .update(cx, |search, cx| search.set_value("", window, cx));
                    if let Some(catalog) = &mut self.theme_catalog {
                        catalog.query.clear();
                    }
                }
                self.apply_settings_focus(&pane_id, window, cx);
                true
            }
            "escape" => {
                let Some(pane) = self.settings.panes.get_mut(&pane_id) else {
                    return false;
                };
                if pane.query.is_empty() {
                    // An empty field folds back into the search icon.
                    self.collapse_settings_search(&pane_id, window, cx);
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

    /// Escape in a Settings pane's controls: a waiting theme preview is cancelled, else
    /// the theme list closes, else the highlight goes up to the section shown, and from
    /// there the keyboard goes back to the terminal.
    fn settings_escape(&mut self, pane_id: &PaneId) {
        let host = SettingsHost::Pane(pane_id.clone());
        if self.theme_bar_shown(pane_id) {
            self.cancel_theme_preview();
            self.settle_theme_focus(pane_id);
        } else if self.theme_list_shown(pane_id) {
            self.close_theme_list();
            self.focus_theme_origin(pane_id);
        } else if self
            .settings
            .focus
            .as_ref()
            .is_some_and(|focus| focus.host == host && matches!(focus.stop, Stop::Section(_)))
        {
            self.leave_settings(pane_id);
        } else if let Some(section) = self.settings.panes.get(pane_id).map(|pane| pane.section) {
            self.set_settings_focus(host, Stop::Section(section));
        }
    }

    /// After a theme decision: a highlight on a control that went away (the bar's)
    /// goes back to the control that opened the theme list.
    fn settle_theme_focus(&mut self, pane_id: &PaneId) {
        let host = SettingsHost::Pane(pane_id.clone());
        let order = self.settings_order(&host);
        if self
            .settings
            .focus
            .as_ref()
            .is_some_and(|focus| focus.host == host && !order.contains(&focus.stop))
        {
            self.focus_theme_origin(pane_id);
        }
    }

    /// The highlight back on the control that opened the terminal colors page, or on
    /// the theme grid.
    fn focus_theme_origin(&mut self, pane_id: &PaneId) {
        let section = self.settings.panes.get(pane_id).map(|pane| pane.section);
        let stop = match section {
            Some(SettingsSection::Advanced) => Stop::Control {
                row: "terminal-colors",
                action: SettingsAction::AllThemes(CatalogTarget::Terminal),
            },
            _ => Stop::Control {
                row: THEME_ROW,
                action: SettingsAction::ThemeList,
            },
        };
        self.set_settings_focus(SettingsHost::Pane(pane_id.clone()), stop);
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
        if pane.search.focus_handle(cx).is_focused(window)
            || pane.theme_search.focus_handle(cx).is_focused(window)
        {
            // Text belongs to the field; Tab, Down and Escape were taken before it.
            return;
        }
        let key = &event.keystroke;
        let host = SettingsHost::Pane(pane_id.clone());
        let plain = !key.modifiers.control && !key.modifiers.alt && !key.modifiers.platform;
        let handled = if key.key == "escape" && plain {
            if self.popup.take().is_none() && !self.close_prompt_dropdown() {
                self.settings_escape(pane_id);
            }
            true
        } else if key.key == "f"
            && (key.modifiers.control || key.modifiers.platform)
            && !key.modifiers.alt
            && !key.modifiers.shift
        {
            // Ctrl/Cmd+F opens the search field behind its icon.
            self.set_settings_focus(host, Stop::Search);
            true
        } else if self.settings_navigation_key(key, &host, window, cx) {
            true
        } else if let Some(text) = key
            .key_char
            .as_deref()
            .filter(|text| plain && !text.is_empty() && !text.chars().any(char::is_control))
        {
            // Typing searches: the theme list's themes while it shows, else settings.
            let text = text.to_owned();
            let themes = self.theme_list_shown(pane_id);
            if let Some(pane) = self.settings.panes.get(pane_id) {
                let field = if themes {
                    &pane.theme_search
                } else {
                    &pane.search
                };
                let query = format!("{}{text}", field.read(cx).value());
                field.update(cx, |search, cx| search.set_value(query, window, cx));
                let lowered = field.read(cx).value().to_lowercase();
                if themes {
                    if let Some(catalog) = &mut self.theme_catalog {
                        catalog.query = lowered;
                    }
                } else if let Some(pane) = self.settings.panes.get_mut(pane_id) {
                    pane.query = lowered;
                }
            }
            self.set_settings_focus(
                host,
                if themes {
                    Stop::Control {
                        row: self.theme_stop_row(pane_id),
                        action: SettingsAction::ThemeSearch,
                    }
                } else {
                    Stop::Search
                },
            );
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
            (
                "up" | "down" | "left" | "right",
                Some(Stop::Control {
                    action: SettingsAction::ThemeList,
                    ..
                }),
            ) => {
                // Arrows move the preview across the grid: a tile, or a row of them.
                let columns = pane
                    .as_ref()
                    .and_then(|pane| self.settings.panes.get(pane))
                    .map_or(1, |ui| ui.grid_columns.get());
                self.move_catalog_selection(
                    match key.key.as_str() {
                        "up" => GridStep::Up,
                        "down" => GridStep::Down,
                        "left" => GridStep::Previous,
                        _ => GridStep::Next,
                    },
                    columns,
                );
                current
            }
            (
                "left" | "right",
                Some(Stop::Control {
                    action: SettingsAction::ThemeFilter,
                    ..
                }),
            ) => {
                self.step_catalog_filter(key.key == "right");
                current
            }
            (
                "left" | "right",
                Some(Stop::Control {
                    action: SettingsAction::Scope,
                    ..
                }),
            ) => {
                self.set_settings_scope(if key.key == "left" {
                    SettingsScope::Global
                } else {
                    SettingsScope::Window
                });
                current
            }
            ("tab", _) => step(&order, current, !modifiers.shift),
            ("left" | "right", Some(Stop::Section(section))) => {
                let index = SettingsSection::ALL
                    .iter()
                    .position(|candidate| *candidate == section)
                    .unwrap_or(0);
                let count = SettingsSection::ALL.len();
                let next = SettingsSection::ALL[if key.key == "left" {
                    (index + count - 1) % count
                } else {
                    (index + 1) % count
                }];
                if let Some(pane) = &pane {
                    self.select_settings_section(pane, next, window, cx);
                }
                Some(Stop::Section(next))
            }
            ("down", Some(Stop::Section(_))) => controls.first().copied().or(current),
            ("up", Some(Stop::Section(_))) => Some(Stop::Search),
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
        if self.theme_list_shown(pane_id) {
            // The current section again: back from the theme list.
            self.close_theme_list();
        }
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
            SettingsAction::Scope => self.set_settings_scope(match self.settings_scope {
                SettingsScope::Global => SettingsScope::Window,
                SettingsScope::Window => SettingsScope::Global,
            }),
            SettingsAction::AllThemes(target) => match self.highlighted_pane() {
                // From search results: the Appearance page and its grid.
                Some(pane) if target == CatalogTarget::Both => {
                    self.select_settings_section(&pane, SettingsSection::Appearance, window, cx);
                    self.set_settings_focus(
                        SettingsHost::Pane(pane),
                        Stop::Control {
                            row: THEME_ROW,
                            action: SettingsAction::ThemeList,
                        },
                    );
                }
                Some(pane) => {
                    self.start_theme_session(pane.clone(), target, true);
                    self.set_settings_focus(
                        SettingsHost::Pane(pane),
                        Stop::Control {
                            row: THEME_LIST_ROW,
                            action: SettingsAction::ThemeList,
                        },
                    );
                }
                // Quick Appearance goes on to the Settings pane.
                None => self.open_theme_settings(target, window, cx),
            },
            SettingsAction::ThemeBack => {
                self.close_theme_list();
                if let Some(pane) = self.highlighted_pane() {
                    self.focus_theme_origin(&pane);
                }
            }
            SettingsAction::ThemeSearch => {
                if let Some(pane) = self.highlighted_pane() {
                    self.apply_settings_focus(&pane, window, cx);
                }
            }
            SettingsAction::ThemeFilter => self.step_catalog_filter(true),
            SettingsAction::ThemeMore => self.popup = Some(Popup::CatalogMenu),
            SettingsAction::ThemeList => {
                if self
                    .highlighted_pane()
                    .is_some_and(|pane| self.theme_bar_shown(&pane))
                {
                    self.decide_theme(window, cx);
                }
            }
            SettingsAction::ThemeFavorite => self.toggle_selected_favorite(window, cx),
            SettingsAction::ThemeCancel | SettingsAction::ThemeApply => {
                if action == SettingsAction::ThemeCancel {
                    self.cancel_theme_preview();
                } else {
                    self.decide_theme(window, cx);
                }
                if let Some(pane) = self.highlighted_pane() {
                    self.settle_theme_focus(&pane);
                }
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

    /// Global defaults or this window; a theme preview of the other scope ends.
    fn set_settings_scope(&mut self, scope: SettingsScope) {
        if self.settings_scope != scope {
            self.cancel_catalog_preview();
            self.settings_scope = scope;
        }
    }

    /// The theme list's next (or previous) filter.
    fn step_catalog_filter(&mut self, forward: bool) {
        let Some(catalog) = &self.theme_catalog else {
            return;
        };
        let filters = super::catalog::ThemeFilter::ALL;
        let index = filters
            .iter()
            .position(|filter| *filter == catalog.filter)
            .unwrap_or(0);
        let count = filters.len();
        self.set_catalog_filter(
            filters[if forward {
                (index + 1) % count
            } else {
                (index + count - 1) % count
            }],
        );
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

    /// `control` as `action` of `row`: a click takes the highlight and acts, and the
    /// highlight draws a ring around it.
    pub(super) fn settings_control(
        &self,
        row: &RowCx,
        action: SettingsAction,
        control: controls::Control,
        cx: &Context<Self>,
    ) -> AnyElement {
        let focused = self.settings_focused(row, action);
        controls::highlight(
            focused,
            control.on_click(self.settings_click(row, action, cx)),
            cx,
        )
        .child(self.settings_focus_anchor(focused, cx))
        .into_any_element()
    }

    /// An outline text button for `action` of `row`, its `index`-th control.
    pub(super) fn settings_button(
        &self,
        row: &RowCx,
        index: usize,
        label: impl Into<SharedString>,
        action: SettingsAction,
        cx: &Context<Self>,
    ) -> AnyElement {
        self.settings_control(
            row,
            action,
            controls::text_button((row.row, index), label).header(),
            cx,
        )
    }

    /// The on/off switch of `action` in `row`, on when `enabled`.
    pub(super) fn settings_switch(
        &self,
        row: &RowCx,
        enabled: bool,
        action: SettingsAction,
        cx: &Context<Self>,
    ) -> AnyElement {
        let focused = self.settings_focused(row, action);
        let toggled = row.clone();
        controls::highlight(
            focused,
            controls::switch((row.row, 0_usize), enabled)
                .label(row.title)
                .on_toggle(cx.listener(move |this, _: &bool, window, cx| {
                    this.focus_settings_control(&toggled, action);
                    this.activate_settings_action(action, window, cx);
                    cx.notify();
                })),
            cx,
        )
        .child(self.settings_focus_anchor(focused, cx))
        .into_any_element()
    }

    /// Whether the Settings pane `pane_id` shows its search field: while it has the
    /// highlight or holds a query. Otherwise the header shows the search icon.
    fn settings_search_open(&self, pane_id: &PaneId, pane: &SettingsPane) -> bool {
        !pane.query.is_empty()
            || self.settings.focus.as_ref()
                == Some(&SettingsFocus {
                    host: SettingsHost::Pane(pane_id.clone()),
                    stop: Stop::Search,
                })
    }

    /// Closes the search field of the Settings pane `pane_id`: the query goes and the
    /// highlight returns to the section shown.
    fn collapse_settings_search(&mut self, pane_id: &PaneId, window: &mut Window, cx: &mut App) {
        let Some(pane) = self.settings.panes.get_mut(pane_id) else {
            return;
        };
        if !pane.query.is_empty() {
            pane.query.clear();
            pane.search
                .update(cx, |search, cx| search.set_value("", window, cx));
        }
        let section = pane.section;
        self.set_settings_focus(SettingsHost::Pane(pane_id.clone()), Stop::Section(section));
        self.apply_settings_focus(pane_id, window, cx);
    }

    /// The header line of the Settings pane `pane_id`: the title, the section icons
    /// (the current one filled and underlined in the accent) and the search icon; or
    /// while searching, the title and the search field in their place.
    fn render_settings_header(
        &self,
        pane_id: &PaneId,
        pane: &SettingsPane,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        // Narrow panes give the title's room to the section icons.
        let titled = width >= HEADER_TITLE_MIN_WIDTH;
        let header = div()
            .flex_none()
            .h(px(44.0))
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(color(colors.border))
            .when(titled, |header| {
                header.child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Settings"),
                )
            });
        if self.settings_search_open(pane_id, pane) {
            let close = pane_id.clone();
            return header
                .child(
                    div().flex_1().min_w_0().max_w(px(420.0)).child(
                        Input::new(&pane.search)
                            .appearance(true)
                            .with_size(gpui_component::Size::Small),
                    ),
                )
                .child(
                    controls::icon_button("settings-search-close", IconName::X, "Close search")
                        .header()
                        .key("escape")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.collapse_settings_search(&close, window, cx);
                            cx.notify();
                        })),
                )
                .into_any_element();
        }
        let host = SettingsHost::Pane(pane_id.clone());
        let entries = SettingsSection::ALL
            .into_iter()
            .enumerate()
            .map(|(index, section)| {
                let active = pane.section == section;
                let focused = self.settings.focus.as_ref()
                    == Some(&SettingsFocus {
                        host: host.clone(),
                        stop: Stop::Section(section),
                    });
                let pane_id = pane_id.clone();
                let entry = controls::icon_button(
                    ("settings-section", index),
                    section_icon(section),
                    section.label(),
                )
                .header()
                .selected(active)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_settings_section(&pane_id, section, window, cx);
                    this.set_settings_focus(
                        SettingsHost::Pane(pane_id.clone()),
                        Stop::Section(section),
                    );
                    this.apply_settings_focus(&pane_id, window, cx);
                    cx.notify();
                }));
                // The entry fills the header's height: the icon centred, its ring clear of
                // the edges, and the underline on the header's bottom edge.
                div()
                    .relative()
                    .flex_none()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(
                        controls::highlight(focused, entry, cx)
                            .child(self.settings_focus_anchor(focused, cx)),
                    )
                    .when(active, |entry| {
                        entry.child(
                            div()
                                .absolute()
                                .left(px(2.0))
                                .right(px(2.0))
                                .bottom_0()
                                .h(px(2.0))
                                .rounded_full()
                                .bg(color(colors.accent)),
                        )
                    })
            });
        let open = pane_id.clone();
        header
            .child(
                div()
                    .id("settings-sections")
                    .h_full()
                    .min_w_0()
                    // The strip scrolls, so it clips: room for the end icons' rings.
                    .px(px(4.0))
                    .flex()
                    .gap_1()
                    .overflow_x_scroll()
                    .children(entries),
            )
            .child(div().flex_1())
            .child(
                controls::icon_button("settings-search", IconName::Search, "Search settings")
                    .header()
                    .key("secondary-f")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_settings_focus(SettingsHost::Pane(open.clone()), Stop::Search);
                        this.apply_settings_focus(&open, window, cx);
                        cx.notify();
                    })),
            )
            .into_any_element()
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
                    let Some((host, stop)) = this
                        .settings
                        .focus
                        .as_ref()
                        .map(|focus| (focus.host.clone(), focus.stop))
                    else {
                        return;
                    };
                    let Some(scroll) = this.settings_scroll_mut(&host, in_theme_grid(stop)) else {
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

    /// Records where `host`'s settings area (with `grid`, its theme grid) is, so
    /// keyboard moves can scroll into it.
    fn settings_scroll_viewport(
        &self,
        host: SettingsHost,
        grid: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        canvas(
            |_, _, _| (),
            move |bounds, _, _, cx| {
                entity.update(cx, |this, _| {
                    if let Some(scroll) = this.settings_scroll_mut(&host, grid) {
                        scroll.bounds = Some(bounds);
                    }
                });
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }

    /// One setting inside its card: title and description on the left, the control on
    /// the right (below them when `compact`).
    pub(super) fn settings_row(
        &self,
        label: &'static str,
        detail: String,
        control: AnyElement,
        compact: bool,
    ) -> AnyElement {
        let muted = self.colors().muted;
        self.tinted_settings_row(label.into(), detail, muted, control, compact)
    }

    /// A settings row whose description is in `tint` (a theme color, made legible).
    fn tinted_settings_row(
        &self,
        label: SharedString,
        detail: String,
        tint: u32,
        control: AnyElement,
        compact: bool,
    ) -> AnyElement {
        let colors = *self.colors();
        div()
            .min_w_0()
            .min_h(px(52.0))
            .py_3()
            .flex()
            .gap_3()
            .when(compact, |row| row.flex_col().items_start())
            .when(!compact, |row| row.items_center().justify_between())
            .child(
                div()
                    .min_w_0()
                    // Basis auto, not `flex_1`'s zero basis: with a zero basis Taffy sizes
                    // the row as if every character wrapped, which leaves blank scroll space
                    // below rows nested deeper in a column (the shell prompt group).
                    .when(!compact, |label| label.flex_auto().min_w(px(72.0)))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(div().font_weight(FontWeight::MEDIUM).child(label))
                    .when(!detail.is_empty(), |label| {
                        label.child(
                            div()
                                .text_size(px(UI_SMALL_TEXT_SIZE))
                                .line_height(px(UI_SMALL_LINE_HEIGHT))
                                .text_color(color(modal_text_color(tint, &colors)))
                                .child(detail),
                        )
                    }),
            )
            .child(
                div()
                    .min_w_0()
                    .when(compact, |control| control.w_full())
                    .when(!compact, |control| control.flex_none())
                    .flex()
                    .items_center()
                    .child(control),
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
            self.settings_switch(row, enabled, action, cx),
            false,
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
        let button = controls::text_button((row.row, 0_usize), label).header();
        self.settings_row(
            row.title,
            detail,
            self.settings_control(
                row,
                action,
                if destructive { button.danger() } else { button },
                cx,
            ),
            row.compact,
        )
    }

    /// Global defaults or This window: a small switch, at the right end of the page's
    /// first label line or, in search results and Quick Appearance, as a row.
    fn render_scope_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let focused = self.settings_focused(row, SettingsAction::Scope);
        let options = [
            (
                SettingsScope::Global,
                "Global",
                "Defaults for windows without an override.",
            ),
            (
                SettingsScope::Window,
                "This window",
                "Only this window; global defaults stay unchanged.",
            ),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (scope, label, detail))| {
            let row = row.clone();
            controls::text_button((row.row, index), label)
                .selected(self.settings_scope == scope)
                .detail(detail)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.focus_settings_control(&row, SettingsAction::Scope);
                    this.set_settings_scope(scope);
                    cx.notify();
                }))
        });
        let switch = controls::highlight(
            focused,
            controls::segmented((row.row, 9_usize), options),
            cx,
        )
        .child(self.settings_focus_anchor(focused, cx))
        .into_any_element();
        if row.header {
            return switch;
        }
        self.settings_row(
            row.title,
            match self.settings_scope {
                SettingsScope::Global => "Defaults for windows without an override.",
                SettingsScope::Window => "Only this window; global defaults stay unchanged.",
            }
            .into(),
            switch,
            false,
        )
    }

    /// Theme outside the Appearance page (search results, Quick Appearance): the
    /// current theme and the way on to the grid in Settings. The page draws the grid
    /// itself; see `render_theme_section`.
    fn render_theme_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let theme = self.resolve_theme(&self.scoped_appearance().theme);
        self.settings_row(
            row.title,
            theme.label().to_owned(),
            self.settings_button(
                row,
                0,
                "Browse themes…",
                SettingsAction::AllThemes(CatalogTarget::Both),
                cx,
            ),
            false,
        )
    }

    fn render_opacity_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let mut appearance = self.scoped_appearance();
        if self.opacity_drag_origin.is_some() {
            appearance.terminal_opacity = self.terminal_opacity;
        }
        let enabled = appearance.transparent_background;
        let focused = enabled && self.settings_focused(row, SettingsAction::Opacity);
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
        let slider = div()
            .id((row.row, 0_usize))
            .relative()
            .when(row.compact, |slider| slider.w_full())
            .when(!row.compact, |slider| slider.w(px(180.0)))
            .h(px(24.0))
            .when(enabled, |slider| {
                slider.cursor(gpui::CursorStyle::ResizeLeftRight)
            })
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(10.0))
                    .h(px(4.0))
                    .rounded_full()
                    .bg(color(colors.border)),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(10.0))
                    .w(gpui::relative(opacity_progress))
                    .h(px(4.0))
                    .rounded_full()
                    .bg(color(colors.accent)),
            )
            .child(
                div()
                    .absolute()
                    .left(gpui::relative(opacity_progress))
                    .top(px(5.0))
                    .ml(px(-7.0))
                    .size(px(14.0))
                    .rounded_full()
                    .border_1()
                    .border_color(color(colors.foreground))
                    .bg(color(colors.accent)),
            )
            .when(enabled, |slider| slider.child(opacity_slider_input));
        let control = div()
            .flex()
            .items_center()
            .gap_3()
            .when(row.compact, |control| control.w_full())
            .when(!enabled, |control| control.opacity(0.45))
            .child(
                controls::highlight(focused, slider, cx)
                    .when(row.compact, |slider| slider.flex_1().min_w_0())
                    .child(self.settings_focus_anchor(focused, cx)),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(40.0))
                    .text_right()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .child(format!("{:.0}%", appearance.terminal_opacity * 100.0)),
            )
            .into_any_element();
        self.settings_row(
            row.title,
            if enabled {
                String::new()
            } else {
                "Needs a transparent background.".into()
            },
            control,
            row.compact,
        )
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

    /// One drawn choice of a setting: `preview` above its `label`, an accent edge when
    /// `selected`.
    fn option_card(
        &self,
        row: &RowCx,
        (index, action): (usize, SettingsAction),
        selected: bool,
        label: &'static str,
        preview: Div,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let focused = self.settings_focused(row, action);
        let card = div()
            .id((row.row, index))
            .w(px(OPTION_CARD_WIDTH))
            .flex()
            .flex_col()
            .gap_1()
            .cursor_pointer()
            .on_click(self.settings_click(row, action, cx))
            .child(
                preview
                    .h(px(58.0))
                    .w_full()
                    .rounded_md()
                    .overflow_hidden()
                    .bg(material_color(colors.background, 1.0))
                    .when(selected, |preview| {
                        preview.border_2().border_color(color(colors.accent))
                    })
                    .when(!selected, |preview| {
                        preview.border_1().border_color(color(colors.border))
                    }),
            )
            .child(
                div()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .line_height(px(UI_SMALL_LINE_HEIGHT))
                    .text_color(color(modal_text_color(
                        if selected {
                            colors.foreground
                        } else {
                            colors.muted
                        },
                        &colors,
                    )))
                    .when(selected, |label| label.font_weight(FontWeight::MEDIUM))
                    .child(label),
            );
        controls::highlight(focused, card, cx)
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element()
    }

    /// A pane of an option drawing: the surface with two lines of text.
    fn sketch_pane(&self) -> Div {
        let colors = *self.colors();
        let line = |width: f32| {
            div()
                .h(px(3.0))
                .w(px(width))
                .rounded_full()
                .bg(color(colors.muted).opacity(0.5))
        };
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .p(px(6.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .bg(color(colors.surface))
            .child(line(22.0))
            .child(line(14.0))
    }

    fn render_density_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        use crate::theme::WorkspaceDensity;
        let colors = *self.colors();
        let cards = WorkspaceDensity::ALL
            .into_iter()
            .enumerate()
            .map(|(index, density)| {
                let comfy = density == WorkspaceDensity::Comfy;
                let pane = |sketch: Div| {
                    if comfy {
                        sketch
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(color(colors.border))
                    } else {
                        sketch
                    }
                };
                let seam = color(colors.border);
                let preview = div()
                    .flex()
                    .when(comfy, |preview| preview.p(px(5.0)).gap(px(4.0)))
                    .child(pane(self.sketch_pane()).flex_none().w(px(28.0)))
                    .when(!comfy, |preview| preview.child(div().w(px(1.0)).bg(seam)))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .when(comfy, |column| column.gap(px(4.0)))
                            .child(pane(self.sketch_pane()))
                            .when(!comfy, |column| column.child(div().h(px(1.0)).bg(seam)))
                            .child(pane(self.sketch_pane())),
                    );
                self.option_card(
                    row,
                    (index, SettingsAction::Density(density)),
                    self.config.density == density,
                    density.label(),
                    preview,
                    cx,
                )
            });
        self.settings_row(
            row.title,
            match self.config.density {
                WorkspaceDensity::Comfy => "Rounded panes with gaps between them, in every window.",
                WorkspaceDensity::Compact => {
                    "Edge-to-edge panes for the most terminal space, in every window."
                }
            }
            .into(),
            div()
                .flex()
                .flex_wrap()
                .gap_3()
                .children(cards)
                .into_any_element(),
            true,
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
        use crate::theme::FocusIndicator;
        let colors = *self.colors();
        let cards = FocusIndicator::ALL
            .into_iter()
            .enumerate()
            .map(|(index, indicator)| {
                let island = |sketch: Div| {
                    sketch
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(color(colors.border))
                };
                let focused = match indicator {
                    FocusIndicator::Marker => island(self.sketch_pane()).child(
                        div()
                            .absolute()
                            .top(px(-1.0))
                            .left(px(6.0))
                            .w(px(14.0))
                            .h(px(2.0))
                            .rounded_full()
                            .bg(color(colors.accent)),
                    ),
                    FocusIndicator::Outline => {
                        island(self.sketch_pane()).border_color(color(colors.accent))
                    }
                    FocusIndicator::Dim | FocusIndicator::None => island(self.sketch_pane()),
                }
                // Every indicator keeps the focused pane's cursor.
                .child(div().w(px(5.0)).h(px(7.0)).bg(color(colors.foreground)));
                let other = island(self.sketch_pane())
                    .when(indicator == FocusIndicator::Dim, |pane| pane.opacity(0.4));
                let preview = div()
                    .flex()
                    .p(px(5.0))
                    .gap(px(4.0))
                    .child(focused)
                    .child(other);
                self.option_card(
                    row,
                    (index, SettingsAction::FocusIndicator(indicator)),
                    self.config.focus_indicator == indicator,
                    indicator.label(),
                    preview,
                    cx,
                )
            });
        self.settings_row(
            row.title,
            match self.config.focus_indicator {
                FocusIndicator::Marker => {
                    "A short accent bar on the focused pane; an outline flashes when focus moves."
                }
                FocusIndicator::Outline => "An accent outline around the focused pane.",
                FocusIndicator::Dim => "Other panes are dimmed.",
                FocusIndicator::None => "Only the cursor, which unfocused panes do not draw.",
            }
            .into(),
            div()
                .flex()
                .flex_wrap()
                .gap_3()
                .children(cards)
                .into_any_element(),
            true,
        )
    }

    fn render_zoom_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let zoom = div()
            .flex()
            .items_center()
            .gap_1()
            .child(self.settings_control(
                row,
                SettingsAction::Zoom(Command::ZoomOut),
                controls::icon_button((row.row, 0_usize), IconName::Minus, "Zoom out").header(),
                cx,
            ))
            .child(self.settings_button(
                row,
                1,
                "Reset",
                SettingsAction::Zoom(Command::ZoomReset),
                cx,
            ))
            .child(self.settings_control(
                row,
                SettingsAction::Zoom(Command::ZoomIn),
                controls::icon_button((row.row, 2_usize), IconName::Plus, "Zoom in").header(),
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
        let current = if appearance.terminal_theme_override {
            format!(
                "Override: {}.",
                self.resolve_theme(&appearance.terminal_theme).label()
            )
        } else {
            "Following the theme.".into()
        };
        let buttons = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .when(appearance.terminal_theme_override, |buttons| {
                buttons.child(self.settings_button(
                    row,
                    0,
                    "Follow theme",
                    SettingsAction::FollowTheme,
                    cx,
                ))
            })
            .child(self.settings_button(
                row,
                1,
                "Choose override ›",
                SettingsAction::AllThemes(CatalogTarget::Terminal),
                cx,
            ))
            .into_any_element();
        self.settings_row(
            row.title,
            if theme_locked {
                "Colors are fixed by --theme.".into()
            } else {
                format!("{} {current}", row_description(row.row))
            },
            buttons,
            row.compact,
        )
    }

    fn render_restart_daemon_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let live_processes = self.command_context(cx).live_surface_count;
        let button = controls::text_button(
            (row.row, 0_usize),
            if live_processes == 0 {
                "Restart"
            } else {
                "Review restart"
            },
        )
        .header();
        self.settings_row(
            row.title,
            format!(
                "{} running terminal{}. Restarting stops all running terminals.",
                live_processes,
                if live_processes == 1 { "" } else { "s" }
            ),
            self.settings_control(
                row,
                SettingsAction::RestartDaemon,
                if live_processes > 0 {
                    button.danger()
                } else {
                    button
                },
                cx,
            ),
            row.compact,
        )
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

    /// The installed version as a row: the updater's status under it and its one
    /// primary button on the right, the download's progress below while it runs.
    fn render_update_status(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        use crate::updates::UpdateButton;
        let colors = *self.colors();
        let view = self.update_view();
        let primary = view.button.map_or_else(
            || div().into_any_element(),
            |button| {
                let action = SettingsAction::Update(UpdateControl::Primary(button));
                let control = controls::text_button((row.row, 0_usize), button.label()).header();
                self.settings_control(
                    row,
                    action,
                    match button {
                        UpdateButton::RestartEndShells => control.danger(),
                        UpdateButton::Cancel => control,
                        _ => control.primary(),
                    },
                    cx,
                )
            },
        );
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .child(self.tinted_settings_row(
                format!("Compi {}", env!("CARGO_PKG_VERSION")).into(),
                view.status,
                if view.failed {
                    colors.error
                } else {
                    colors.muted
                },
                primary,
                row.compact,
            ))
            .when_some(view.progress, |summary, fraction| {
                summary.child(
                    div().pb_3().child(
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
                    ),
                )
            })
            .into_any_element()
    }

    /// Release notes with More/Less and a link, titled with their version.
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
            self.settings_button(
                row,
                0,
                if expanded { "Less" } else { "More" },
                more_action,
                cx,
            )
        };
        let link = self.settings_button(row, 1, "Release notes", SettingsAction::Update(link), cx);
        div()
            .min_w_0()
            .py_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("What's new in {}", notes.version)),
            )
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
        let open = self.settings_update_advanced;
        let disclosure = self.settings_control(
            row,
            SettingsAction::Update(UpdateControl::Advanced),
            controls::text_button((row.row, 0_usize), "Advanced")
                .icon(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .header()
                .ghost(),
            cx,
        );
        div()
            .min_w_0()
            .py_3()
            .flex()
            .flex_col()
            .items_start()
            .gap_2()
            .child(disclosure)
            .when(open, |content| {
                content.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .when(cfg!(windows), |buttons| {
                            buttons.child(self.settings_button(
                                row,
                                1,
                                "Repair Compi",
                                SettingsAction::Update(UpdateControl::Repair),
                                cx,
                            ))
                        })
                        .child(self.settings_button(
                            row,
                            2,
                            "Open update log",
                            SettingsAction::Update(UpdateControl::OpenLog),
                            cx,
                        )),
                )
            })
            .into_any_element()
    }

    /// `rows` in cards: consecutive rows of one section and group share a card, with
    /// hairlines between its rows. A label above a card names its group when the
    /// section has more than one; in search results (`search`) every card has one,
    /// "Section · Group" or just the section. `header` rows (the scope switch) sit at
    /// the right end of the first label line.
    fn render_settings_cards(
        &self,
        host: &SettingsHost,
        rows: &[&'static Row],
        header: &[&'static Row],
        search: bool,
        compact: bool,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let colors = *self.colors();
        let mut header = (!header.is_empty()).then(|| {
            header
                .iter()
                .map(|row| {
                    let row_cx = RowCx {
                        host: host.clone(),
                        row: row.id,
                        title: row.title,
                        compact: false,
                        header: true,
                    };
                    (row.render)(self, &row_cx, cx)
                })
                .collect::<Vec<_>>()
        });
        let mut elements = Vec::new();
        for run in rows.chunk_by(|a, b| a.section == b.section && a.group == b.group) {
            let first = run[0];
            let group = first.group.filter(|_| section_grouped(first.section));
            let label: Option<SharedString> = match (search, group) {
                (true, Some(group)) => Some(format!("{} · {group}", first.section.label()).into()),
                (true, None) => Some(first.section.label().into()),
                (false, group) => group.map(SharedString::from),
            };
            let card =
                settings_card(&colors).children(run.iter().enumerate().map(|(index, row)| {
                    let row_cx = RowCx {
                        host: host.clone(),
                        row: row.id,
                        title: row.title,
                        compact,
                        header: false,
                    };
                    div()
                        .min_w_0()
                        .px_4()
                        .when(index > 0, |line| {
                            line.border_t_1().border_color(color(colors.border))
                        })
                        .child((row.render)(self, &row_cx, cx))
                }));
            let trailing = header.take();
            elements.push(if label.is_some() || trailing.is_some() {
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(settings_label_line(label, trailing))
                    .child(card)
                    .into_any_element()
            } else {
                card.into_any_element()
            });
        }
        if let Some(trailing) = header {
            elements.push(settings_label_line(None, Some(trailing)).into_any_element());
        }
        elements
    }

    /// A Settings pane: the header line, then the section's page, the search results
    /// or the terminal colors page, and the theme preview's bar at the foot. Pages with
    /// the theme grid (Appearance, terminal colors) fill the pane, the grid taking the
    /// height left in its own scroll; the others scroll as a column of cards.
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
        let page_width = (width - 48.0).clamp(1.0, PAGE_MAX_WIDTH);
        let stacked = page_width < STACKED_ROW_WIDTH;
        let searching = !pane.query.trim().is_empty();
        let rows = self.pane_rows(pane);
        let terminal_page = self.theme_list_shown(pane_id);
        let appearance_page = !searching
            && !terminal_page
            && pane.section == SettingsSection::Appearance
            && self.theme_catalog.as_ref().is_some_and(|catalog| {
                &catalog.pane == pane_id && catalog.target == CatalogTarget::Both
            });
        let page = if terminal_page || appearance_page {
            // The grid's own padding (room for a tile's ring) comes off the page width.
            let columns = super::catalog::theme_grid_columns(page_width - 8.0);
            pane.grid_columns.set(columns);
            let grid_row = RowCx {
                host: host.clone(),
                row: if terminal_page {
                    THEME_LIST_ROW
                } else {
                    THEME_ROW
                },
                title: "Theme",
                compact: stacked,
                header: false,
            };
            let grid = self.render_theme_section(
                &grid_row,
                &pane.theme_search,
                (
                    &pane.grid.handle,
                    self.settings_scroll_viewport(host.clone(), true, cx),
                ),
                columns,
                terminal_page,
                cx,
            );
            // No `min_h_0`: the page is at least its cards plus the grid's minimum, so a
            // short pane scrolls the page instead of drawing past its bottom.
            let mut page = div().flex_1().min_w_0().flex().flex_col().gap_4();
            if !terminal_page {
                let (header, rows): (Vec<_>, Vec<_>) = rows
                    .into_iter()
                    .filter(|row| row.id != THEME_ROW)
                    .partition(|row| row.header);
                page = page.children(
                    self.render_settings_cards(&host, &rows, &header, false, stacked, cx)
                        .into_iter()
                        .map(|card| div().flex_none().child(card)),
                );
            }
            // Short panes scroll the whole page; the grid keeps a usable height.
            div()
                .id("settings-theme-page")
                .relative()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .px_6()
                .py_5()
                .child(
                    page.w_full()
                        .max_w(px(PAGE_MAX_WIDTH))
                        .mx_auto()
                        .child(grid),
                )
                .into_any_element()
        } else {
            let body = if searching && rows.is_empty() {
                vec![
                    div()
                        .text_color(color(modal_text_color(colors.muted, &colors)))
                        .child(format!(
                            "No settings match “{}”.",
                            pane.search.read(cx).value().trim()
                        ))
                        .into_any_element(),
                ]
            } else if searching {
                self.render_settings_cards(&host, &rows, &[], true, stacked, cx)
            } else {
                let (header, rows): (Vec<_>, Vec<_>) = rows.into_iter().partition(|row| row.header);
                self.render_settings_cards(&host, &rows, &header, false, stacked, cx)
            };
            div()
                .relative()
                .min_w_0()
                .flex()
                .min_h_0()
                .flex_1()
                .child(
                    div()
                        .id("settings-scroll")
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&pane.scroll.handle)
                        .px_6()
                        .py_5()
                        .child(
                            div()
                                .w_full()
                                .max_w(px(PAGE_MAX_WIDTH))
                                .mx_auto()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_5()
                                .children(body),
                        ),
                )
                .child(self.settings_scroll_viewport(host.clone(), false, cx))
                .into_any_element()
        };
        let key_pane = pane_id.clone();
        let click_pane = pane_id.clone();
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
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
            .child(self.render_settings_header(pane_id, pane, width, cx))
            .child(page)
            .children(self.render_theme_bar(pane_id, cx))
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
            header: false,
        };
        let rows = self.quick_rows();
        popover
            .content(
                div()
                    .w(px(380.0))
                    .flex()
                    .flex_col()
                    .gap_3()
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
                                        self.render_settings_cards(
                                            &host,
                                            &rows,
                                            &[],
                                            false,
                                            false,
                                            cx,
                                        ),
                                    )),
                            )
                            .child(self.settings_scroll_viewport(host, false, cx)),
                    )
                    .child(
                        div().flex().justify_end().child(
                            self.settings_control(
                                &all,
                                SettingsAction::OpenSettings,
                                controls::text_button("open-full-settings", all.title)
                                    .header()
                                    .primary(),
                                cx,
                            ),
                        ),
                    ),
            )
            .into_any_element()
    }
}

/// A card of settings: one rounded, bordered surface per topic.
pub(super) fn settings_card(colors: &ThemeColors) -> Div {
    div()
        .min_w_0()
        .w_full()
        .flex()
        .flex_col()
        .rounded_md()
        .border_1()
        .border_color(color(colors.border))
        .bg(color(colors.surface))
}

/// The section bar icon of `section`.
fn section_icon(section: SettingsSection) -> IconName {
    match section {
        SettingsSection::Appearance => IconName::Palette,
        SettingsSection::Interface => IconName::LayoutPanelLeft,
        SettingsSection::Terminal => IconName::SquareTerminal,
        SettingsSection::Keyboard => IconName::Keyboard,
        SettingsSection::Performance => IconName::Gauge,
        SettingsSection::Updates => IconName::CircleArrowDown,
        SettingsSection::Advanced => IconName::SlidersHorizontal,
    }
}

/// The line above a card: its `label` on the left and `trailing` controls (the scope
/// switch) at the right end, wrapping under the label when the page is narrow.
fn settings_label_line(label: Option<SharedString>, trailing: Option<Vec<AnyElement>>) -> Div {
    div()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x_3()
        .gap_y_2()
        .children(label.map(|label| {
            div()
                .min_w_0()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .font_weight(FontWeight::SEMIBOLD)
                .child(label)
        }))
        .when_some(trailing, |line, trailing| {
            line.child(
                div()
                    .ml_auto()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(trailing),
            )
        })
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
                "transparency",
                "opacity",
                "blur",
                "window-overrides",
                THEME_ROW
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

    #[test]
    fn the_theme_list_reaches_every_control_once_and_header_rows_lead_their_section() {
        let order = focus_order(true, [(THEME_LIST_ROW, theme_list_stops())]);
        let controls = &order[1 + SettingsSection::ALL.len()..];
        assert_eq!(controls.len(), theme_list_stops().len());
        assert!(
            controls
                .iter()
                .enumerate()
                .all(|(index, stop)| !controls[..index].contains(stop))
        );
        // Back first, the rows after the search field and the filter, the star last.
        assert_eq!(
            controls.first(),
            Some(&Stop::Control {
                row: THEME_LIST_ROW,
                action: SettingsAction::ThemeBack
            })
        );
        assert_eq!(
            controls.last(),
            Some(&Stop::Control {
                row: THEME_LIST_ROW,
                action: SettingsAction::ThemeFavorite
            })
        );
        // The scope switch sits on the first label line, so it comes first in its
        // sections; that line is its first group's: Window, and Colors in Advanced.
        for (section, group) in [
            (SettingsSection::Appearance, "Window"),
            (SettingsSection::Advanced, "Colors"),
        ] {
            let mut rows = rows_for(section, "");
            assert!(rows.next().unwrap().header);
            assert_eq!(rows.next().unwrap().group, Some(group));
            assert!(section_grouped(section));
        }
        assert_eq!(ROWS.iter().filter(|row| row.header).count(), 2);
    }

    #[test]
    fn cards_are_labelled_only_in_sections_of_several_groups() {
        assert!(section_grouped(SettingsSection::Interface));
        for section in [
            SettingsSection::Terminal,
            SettingsSection::Keyboard,
            SettingsSection::Performance,
            SettingsSection::Updates,
        ] {
            assert!(!section_grouped(section), "{section:?}");
        }
        // Updates' one card opens on the installed version.
        assert_eq!(
            rows_for(SettingsSection::Updates, "")
                .next()
                .map(|row| row.id),
            Some("update-status")
        );
    }
}
