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
//! A pane draws a one-line header (title, section icons in groups split by dividers,
//! search). A section's page opens with its heading: its name and a line on what it
//! covers, with the header rows (the scope switch, Monitor's Open as pane) at its right
//! end. Then come its cards of rows, each under its group's label. The theme grid
//! (`catalog`) leads the Appearance page in a scroll of its own and fills Advanced's
//! terminal colors page; it previews on the real panes until the Apply / Cancel bar
//! decides. Monitor's page is Overview's cards, drawn on the page itself.
use super::catalog::{CatalogTarget, GridStep};
use super::chat::SECONDARY_KEY;
use super::project_settings::{PROJECTS_ROW, ProjectControl, ProjectFields};
use super::*;
use crate::config::{ChatEnter, ChatOpen, DevServer, NewTab};
use crate::gui::controls::IconName;
use compi_protocol::chat::{AgentInstall, AgentInstalls, AgentProgram, ChatApproval, ChatHarness};
use compi_protocol::{PaneContent, Project, SettingsContent, SettingsSection, WindowsShell};
use gpui::Entity;
use gpui_component::Sizable as _;
use gpui_component::input::{Input, InputEvent, InputState};
use std::rc::Rc;

/// Widest a section page grows, so controls stay near their text; a wider pane centres
/// it.
const PAGE_MAX_WIDTH: f32 = 720.0;
/// The Appearance row of the theme grid.
pub(super) const THEME_ROW: &str = "theme";
/// Narrower than this, a pane stacks its rows: a row's control goes below its title.
const STACKED_PANE_WIDTH: f32 = 520.0;
/// Narrower than this, the header drops its title so the section icons fit.
const HEADER_TITLE_MIN_WIDTH: f32 = 510.0;
/// The room a divider between groups of section icons takes, its gap included.
const STRIP_DIVIDER_WIDTH: f32 = 9.0;
/// The height of the header line, and of each line of section icons when they wrap.
const HEADER_HEIGHT: f32 = 44.0;
/// The width of a picture tile (pane density, new tabs, an agent).
const TILE_WIDTH: f32 = 120.0;
/// The height of a picture tile's drawing.
const TILE_PREVIEW_HEIGHT: f32 = 58.0;
/// The size of an agent's mark on its card and its tile.
const AGENT_MARK_SIZE: f32 = 24.0;
/// The side of the square an item card's mark sits in.
pub(super) const ITEM_MARK_TILE: f32 = 40.0;
/// The width of Quick Appearance's content.
const QUICK_APPEARANCE_WIDTH: f32 = 380.0;
/// The row holding the shell prompt group; see `prompt`.
pub(super) const PROMPT_ROW: &str = "shell-prompt";
/// The theme list's controls, as one row of the keyboard order; see `catalog`.
pub(super) const THEME_LIST_ROW: &str = "theme-list";
/// The theme preview's Apply / Cancel bar, as one row of the keyboard order.
pub(super) const THEME_BAR_ROW: &str = "theme-bar";
/// Advanced's group of the configuration file and the terminal server.
const RECOVERY_GROUP: &str = "Configuration and recovery";
/// Advanced's group of Compi's own performance tools.
const PERFORMANCE_GROUP: &str = "Performance";
/// The Monitor page's Overview; while shown, Monitor's watchers run (see `monitor`).
pub(super) const MONITOR_ROW: &str = "monitor";
/// Compi's own resource use; while shown, the window samples it.
pub(super) const RESOURCES_ROW: &str = "compi-resources";
/// Copy diagnostics, which copies those samples; while shown, the window takes them.
pub(super) const DIAGNOSTICS_ROW: &str = "performance-diagnostics";
/// Rebuild renderer.
pub(super) const REBUILD_ROW: &str = "rebuild-renderer";
/// Settings → Keyboard: every command and its shortcut.
const COMMANDS_ROW: &str = "commands";
/// Settings → Agents: every agent's card. While shown, the window looks up where each
/// agent is installed.
const AGENTS_ROW: &str = "agents";

/// Focusable controls of Settings → Updates. Which ones exist depends on the update
/// state; see the Updates rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UpdateControl {
    Primary(crate::updates::UpdateButton),
    CheckForUpdates,
    DownloadAutomatically,
    /// More or Less of the notes.
    NotesMore,
    /// The notes' release page.
    ReleaseNotes,
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
    /// Opens a select's list (a font, notification delivery).
    Select(Popup),
    /// The opacity slider; Enter raises it a step, Left and Right move it.
    Opacity,
    ResetWindowAppearance,
    ResetSidebar,
    ResetLayout,
    FontSize(Command),
    /// Zoom in, out or reset of the file text size.
    FileTextSize(Command),
    /// Zoom in, out or reset of the chat text size.
    ChatTextSize(Command),
    // Choices: one stop each, drawn as a switch or as picture tiles. Left and Right
    // pick the one before or after (stopping at the ends), Enter the next (going round);
    // see `CompiApp::step_choice`.
    /// Enter while the agent works: Steer or Queue.
    ChatEnter,
    /// Opening a chat: in place or beside.
    ChatOpen,
    /// Default agent, passing agents not installed where new chats open.
    ChatAgent,
    /// New tabs: Chat first or Terminal first.
    NewTab,
    /// Shell (Windows): Automatic, PowerShell or WSL.
    Shell,
    /// When a dev server starts.
    DevServer,
    /// Pane density: Comfy or Compact.
    Density,
    /// Focus indicator.
    FocusIndicator,
    ToggleChatAgentsBar,
    /// Copies how to install an agent installed nowhere.
    CopyInstallCommand(ChatHarness),
    OpenConfiguration,
    /// A command's shortcut, by its index in `commands::REGISTRY`: pressing it records
    /// the next keys pressed.
    Shortcut(usize),
    /// Gives a command its platform default shortcut again.
    ResetShortcut(usize),
    /// Starting folder: pick a folder for new tabs.
    ChooseStartingFolder,
    /// Starting folder: back to the shell's home.
    StartAtHome,
    OpenSettings,
    OpenMonitor,
    ToggleFps,
    RebuildRenderer,
    CopyPerformance,
    Reconnect,
    OpenDiagnostics,
    RestartDaemon,
    Update(UpdateControl),
    Prompt(super::prompt::PromptAction),
    Metadata(usize),
    ToggleConfirmClose,
    ProgramStatus(usize),
    NotificationAttention,
    OtherShellCommands,
    NotificationFinished,
    PlayNotificationSound,
    OpenNotificationSystemSettings,
    TestNotification,
    /// A control of a project's row, or of its detail, on the Projects page.
    Project(ProjectControl),
    /// The Projects page's search field, once it lists enough projects.
    ProjectSearch,
    /// The Projects page's Archived heading: folds or unfolds the archived projects.
    ArchivedProjects,
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
    /// The Projects page's search and name fields.
    projects: ProjectFields,
}

/// Settings in this window: each Settings pane's state, Quick Appearance's scroll
/// and the keyboard highlight, which follows whichever of them has the keyboard.
#[derive(Default)]
pub(in crate::gui) struct SettingsUi {
    focus: Option<SettingsFocus>,
    /// The section icons draw the highlight: the keyboard took it there. Opened or
    /// clicked, only the current section's icon is lit.
    section_ring: bool,
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
    /// The command (index in `commands::REGISTRY`) whose shortcut is being recorded,
    /// and the Settings pane recording it. It records only while its shortcut keeps
    /// the highlight; see `CompiApp::recording_shortcut`.
    recording: Option<(PaneId, usize)>,
    /// What the last shortcut change did, under that command: a key that was taken
    /// from another command, or why a key was refused.
    shortcut_notice: Option<(usize, String)>,
    /// The agents installed where new chats open, for Default agent: asked when
    /// Settings opens; `None` while asking, when asking failed or where is unknown, and
    /// Default agent then offers every agent.
    chat_agents: Option<Vec<ChatHarness>>,
    /// The last such question asked; a reply to an earlier one is dropped.
    chat_agents_asked: u64,
    /// Where every agent is installed, for Settings → Agents: asked each time its cards
    /// show anew; `None` while asking, else the daemon's answer or why there is none.
    agent_installs: Option<Result<AgentInstalls, String>>,
    /// The last such question asked; a reply to an earlier one is dropped.
    agent_installs_asked: u64,
    /// The agents' cards were shown in the last frame.
    agents_shown: bool,
    /// The agent whose install command was copied last, until its card is looked up
    /// again.
    install_copied: Option<ChatHarness>,
    /// The Projects detail page, or `None` for the saved-project list.
    pub(super) project_selected: Option<Project>,
    /// The Projects page lists its archived projects.
    pub(super) archived_open: bool,
}

/// What a row's renderer needs: where it is drawn and how.
#[derive(Clone)]
pub(super) struct RowCx {
    pub(super) host: SettingsHost,
    pub(super) row: &'static str,
    pub(super) title: &'static str,
    pub(super) compact: bool,
    /// Drawn on the page's heading line rather than as a row.
    pub(super) header: bool,
}

/// One picture tile of a choice; see `CompiApp::render_tiles`.
pub(super) struct Tile {
    pub(super) label: SharedString,
    /// The drawing, which the tile sizes and frames.
    pub(super) preview: Div,
    pub(super) picked: bool,
    /// Why it can't be picked, under its label; the tile shows dimmed.
    pub(super) unavailable: Option<&'static str>,
}

/// What picking the tile of a given index does.
pub(super) type PickTile = Rc<dyn Fn(&mut CompiApp, usize, &mut Window, &mut Context<CompiApp>)>;

/// How a row draws.
#[derive(Clone, Copy)]
enum Draw {
    /// Inside its group's card.
    Card(fn(&CompiApp, &RowCx, &Context<CompiApp>) -> AnyElement),
    /// On the page in place of a card, given the page's width: a row of cards of its own.
    Page(fn(&CompiApp, &RowCx, f32, &Context<CompiApp>) -> AnyElement),
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
    /// Drawn at the right end of the page's heading line rather than in a card; search
    /// results show it as a row.
    header: bool,
    /// Whether it shows now; some rows depend on other settings.
    shown: fn(&CompiApp) -> bool,
    stops: fn(&CompiApp) -> Vec<SettingsAction>,
    draw: Draw,
}

impl Row {
    /// Whether every term of `query` is in the title, description, keywords or
    /// group, ignoring case; an Agents row's, and Default agent's, also name every
    /// agent.
    fn matches(&self, query: &str) -> bool {
        static AGENT_WORDS: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            ChatHarness::ALL
                .map(|agent| format!("{} {}", agent.program(), agent.name()))
                .join(" ")
        });
        let agents = if self.section == SettingsSection::Agents || self.id == "chat-agent" {
            AGENT_WORDS.as_str()
        } else {
            ""
        };
        let fields = [
            self.title,
            self.description,
            self.keywords,
            self.group.unwrap_or(""),
            agents,
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
        draw: Draw::Card(render),
    }
}

/// A row drawn on the page rather than in a card; `render` gets the page's width.
const fn page_row(
    id: &'static str,
    section: SettingsSection,
    title: &'static str,
    description: &'static str,
    keywords: &'static str,
    stops: fn(&CompiApp) -> Vec<SettingsAction>,
    render: fn(&CompiApp, &RowCx, f32, &Context<CompiApp>) -> AnyElement,
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
        draw: Draw::Page(render),
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

/// A Tab details toggle; `stops` and `render` name its index.
const fn metadata_row(
    id: &'static str,
    title: &'static str,
    description: &'static str,
    stops: fn(&CompiApp) -> Vec<SettingsAction>,
    render: fn(&CompiApp, &RowCx, &Context<CompiApp>) -> AnyElement,
) -> Row {
    grouped(
        "Tab details",
        row(
            id,
            SettingsSection::Appearance,
            title,
            description,
            "tab caption metadata",
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
            SettingsSection::Notifications,
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
    // General: how you work. Its rows are the page, with no group label.
    row(
        "new-tab",
        SettingsSection::General,
        "New tabs",
        "What the tab strip's + and Ctrl+T open: a new thread or a shell.",
        "chat first terminal first new tab plus ctrl+t thread shell default",
        |_| vec![SettingsAction::NewTab],
        |app, row, cx| app.render_new_tab_row(row, cx),
    ),
    row(
        "chat-agent",
        SettingsSection::General,
        "Default agent",
        "Every new thread starts with it, unless its project picks another.",
        "default agent harness new thread chat",
        |_| vec![SettingsAction::ChatAgent],
        |app, row, cx| app.render_chat_agent_row(row, cx),
    ),
    shown_when(
        |_| cfg!(windows),
        row(
            "shell",
            SettingsSection::General,
            "Shell",
            "What new tabs and splits start: picked for their folder, Windows PowerShell or WSL. Compi runs commands meant for the other shell there either way.",
            "powershell wsl linux bash windows default distribution automatic",
            |_| vec![SettingsAction::Shell],
            |app, row, cx| app.render_shell_row(row, cx),
        ),
    ),
    row(
        "confirm-close",
        SettingsSection::General,
        "Confirm before closing",
        "Ask before closing a tab or pane ends its processes. Off: one click closes.",
        "close closing end process prompt",
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
    // Terminal.
    grouped(
        "Shell",
        row(
            "starting-folder",
            SettingsSection::Terminal,
            "Starting folder",
            "Where a new tab starts when it doesn't open from another pane's folder.",
            "working directory cwd home project default folder",
            |app| {
                let mut stops = vec![SettingsAction::ChooseStartingFolder];
                if app.starting_folder().is_some() {
                    stops.push(SettingsAction::StartAtHome);
                }
                stops
            },
            |app, row, cx| app.render_starting_folder_row(row, cx),
        ),
    ),
    shown_when(
        |_| cfg!(windows),
        grouped(
            "Shell",
            row(
                "other-shell-commands",
                SettingsSection::Terminal,
                "Commands from the other shell",
                "Runs clearly PowerShell or Bash commands in the right shell on Enter. Applies to new shells.",
                "powershell bash zsh wsl commands typed paste",
                |_| vec![SettingsAction::OtherShellCommands],
                |app, row, cx| {
                    app.render_toggle_row(
                        row,
                        app.config.other_shell_commands(),
                        SettingsAction::OtherShellCommands,
                        row_description("other-shell-commands"),
                        cx,
                    )
                },
            ),
        ),
    ),
    grouped(
        "Font",
        row(
            "terminal-font",
            SettingsSection::Terminal,
            "Terminal font",
            "Terminal and editor text.",
            "typeface family monospace",
            |_| vec![SettingsAction::Select(Popup::TerminalFont)],
            |app, row, cx| {
                app.settings_row(
                    row.title,
                    row_description(row.row).into(),
                    app.render_font_select(row, Popup::TerminalFont, cx),
                    row.compact,
                )
            },
        ),
    ),
    grouped(
        "Font",
        row(
            "font-size",
            SettingsSection::Terminal,
            "Font size",
            "Zooms terminal text in every window. Files follow it unless File text size is set.",
            "zoom scale bigger smaller larger text line height",
            |_| {
                vec![
                    SettingsAction::FontSize(Command::ZoomOut),
                    SettingsAction::FontSize(Command::ZoomIn),
                    SettingsAction::FontSize(Command::ZoomReset),
                ]
            },
            |app, row, cx| {
                app.render_size_row(row, app.font_settings.size, SettingsAction::FontSize, cx)
            },
        ),
    ),
    grouped(
        "Font",
        row(
            "file-text-size",
            SettingsSection::Terminal,
            "File text size",
            "Editors, diffs, Markdown and folder trees in every window. Reset follows the font size.",
            "zoom scale bigger smaller larger font editor diff markdown files changes",
            |_| {
                vec![
                    SettingsAction::FileTextSize(Command::ZoomOut),
                    SettingsAction::FileTextSize(Command::ZoomIn),
                    SettingsAction::FileTextSize(Command::ZoomReset),
                ]
            },
            |app, row, cx| {
                app.render_size_row(row, app.file_text_size(), SettingsAction::FileTextSize, cx)
            },
        ),
    ),
    grouped(
        "Font",
        row(
            "custom-typography",
            SettingsSection::Terminal,
            "Custom typography",
            "Set a custom font, line height or fallback fonts in the configuration file.",
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
    ),
    grouped(
        "Dev servers",
        row(
            "dev-server",
            SettingsSection::Terminal,
            "When a dev server starts",
            "What a terminal does once a local address it printed, such as localhost:5173, answers.",
            "localhost browser web page preview open card automatically vite npm port http",
            |_| vec![SettingsAction::DevServer],
            |app, row, cx| app.render_dev_server_row(row, cx),
        ),
    ),
    grouped(
        "Prompt",
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
    ),
    // Chat.
    grouped(
        "Messages",
        row(
            "chat-text-size",
            SettingsSection::Chat,
            "Text size",
            "Agent chats, with their cards and composer, in every window.",
            "zoom scale bigger smaller larger font agent",
            |_| {
                vec![
                    SettingsAction::ChatTextSize(Command::ZoomOut),
                    SettingsAction::ChatTextSize(Command::ZoomIn),
                    SettingsAction::ChatTextSize(Command::ZoomReset),
                ]
            },
            |app, row, cx| {
                app.render_size_row(
                    row,
                    app.config.chat_text_size,
                    SettingsAction::ChatTextSize,
                    cx,
                )
            },
        ),
    ),
    grouped(
        "Messages",
        row(
            "chat-enter",
            SettingsSection::Chat,
            "Enter while the agent works",
            "Whether Enter steers the running turn or queues the message for after it.",
            "steer queue send key ctrl cmd turn message composer agent",
            |_| vec![SettingsAction::ChatEnter],
            |app, row, cx| app.render_chat_enter_row(row, cx),
        ),
    ),
    grouped(
        "Opening",
        row(
            "chat-open",
            SettingsSection::Chat,
            "Opening a chat",
            "Where New chat and compi chat open a chat from a terminal: in place of its shell or beside it.",
            "split pane beside in place shell view palette agent",
            |_| vec![SettingsAction::ChatOpen],
            |app, row, cx| app.render_chat_open_row(row, cx),
        ),
    ),
    // Agents: every agent and where it is installed, then how they ask, then each one's
    // own.
    grouped(
        "Programs",
        page_row(
            AGENTS_ROW,
            SettingsSection::Agents,
            "Agents",
            "Every agent Compi chats with: where it is installed, its version, and how to install it.",
            "installed install version program windows wsl ubuntu linux distribution default not installed npm bun",
            |app| app.agent_stops(),
            |app, row, _, cx| app.render_agents_page(row, cx),
        ),
    ),
    grouped(
        "Permissions",
        row(
            "chat-approval",
            SettingsSection::Agents,
            "Approvals",
            "How the agent asks before it runs tools. Applies to new chats and chats that start again.",
            "approval permission ask allow edits everything yolo tools agent",
            |_| vec![SettingsAction::Select(Popup::ChatApproval)],
            |app, row, cx| app.render_chat_approval_row(row, cx),
        ),
    ),
    grouped(
        "OMP",
        row(
            "chat-agents-bar",
            SettingsSection::Agents,
            "Agents bar",
            "While OMP's sub-agents run, list them above the message box.",
            "sub-agents subagents task tasks running agent omp",
            |_| vec![SettingsAction::ToggleChatAgentsBar],
            |app, row, cx| {
                app.render_toggle_row(
                    row,
                    app.config.chat_agents_bar,
                    SettingsAction::ToggleChatAgentsBar,
                    row_description("chat-agents-bar"),
                    cx,
                )
            },
        ),
    ),
    // Projects.
    page_row(
        PROJECTS_ROW,
        SettingsSection::Projects,
        "Projects",
        "Add projects and manage their names, icons, pins, thread defaults, and archives.",
        "project projects add clone folder repository rename name icon emoji monogram logo favicon pin pinned agent model new thread archive archived restore remove delete forget",
        |app| app.project_stops(),
        |app, row, _, cx| app.render_projects_page(row, cx),
    ),
    // Appearance: how Compi looks. Quick Appearance shows the scope, Window and Theme
    // rows; see `quick_rows`.
    in_header(row(
        "scope",
        SettingsSection::Appearance,
        "Scope",
        "Change the global defaults or only this window.",
        "global window override",
        |_| vec![SettingsAction::Scope],
        |app, row, cx| app.render_scope_row(row, cx),
    )),
    // The theme grid; on the Appearance page it heads the page under its own header
    // line, in a scroll of its own.
    grouped(
        "Theme",
        row(
            THEME_ROW,
            SettingsSection::Appearance,
            "Current theme",
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
                    row_description("transparency"),
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
                    app.render_toggle_row(
                        row,
                        blurred,
                        SettingsAction::ToggleBlur,
                        row_description("blur"),
                        cx,
                    )
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
    grouped(
        "Text",
        row(
            "ui-font",
            SettingsSection::Appearance,
            "Interface font",
            "Sidebar, tabs, menus and Settings.",
            "typeface family ui",
            |_| vec![SettingsAction::Select(Popup::UiFont)],
            |app, row, cx| {
                app.settings_row(
                    row.title,
                    row_description(row.row).into(),
                    app.render_font_select(row, Popup::UiFont, cx),
                    row.compact,
                )
            },
        ),
    ),
    grouped(
        "Layout",
        row(
            "density",
            SettingsSection::Appearance,
            "Pane density",
            "Rounded panes with gaps between them, or edge to edge.",
            "comfy compact gaps spacing islands",
            |_| vec![SettingsAction::Density],
            |app, row, cx| app.render_density_row(row, cx),
        ),
    ),
    grouped(
        "Layout",
        row(
            "sidebar-width",
            SettingsSection::Appearance,
            "Sidebar width",
            "Drag the sidebar's edge to resize it, or reset it to its default width.",
            "reset",
            |_| vec![SettingsAction::ResetSidebar],
            |app, row, cx| {
                app.settings_row(
                    row.title,
                    row_description(row.row).into(),
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(app.settings_value(format!("{:.0} px", app.sidebar_width)))
                        .child(app.settings_button(
                            row,
                            0,
                            "Reset width",
                            SettingsAction::ResetSidebar,
                            cx,
                        ))
                        .into_any_element(),
                    row.compact,
                )
            },
        ),
    ),
    grouped(
        "Layout",
        row(
            "window-layout",
            SettingsSection::Appearance,
            "Window layout",
            "Resets this window's layout without changing your tabs.",
            "reset",
            |_| vec![SettingsAction::ResetLayout],
            |app, row, cx| {
                app.render_button_row(
                    row,
                    row_description(row.row).into(),
                    "Reset layout",
                    SettingsAction::ResetLayout,
                    false,
                    cx,
                )
            },
        ),
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
        "Focus",
        row(
            "focus-indicator",
            SettingsSection::Appearance,
            "Focus indicator",
            "How the focused pane is marked: marker, outline, dim or none.",
            "marker outline dim focused pane",
            |_| vec![SettingsAction::FocusIndicator],
            |app, row, cx| app.render_focus_indicator_row(row, cx),
        ),
    ),
    // Keyboard.
    page_row(
        COMMANDS_ROW,
        SettingsSection::Keyboard,
        "Commands",
        "Every command and its shortcut. Click a shortcut to change it.",
        "palette shortcuts keys keybindings bindings record reset",
        // The commands the pane's search leaves; see `CompiApp::command_stops`.
        |_| Vec::new(),
        |app, row, _, cx| app.render_commands_page(row, cx),
    ),
    // Notifications: when and where Compi tells you, then where program status shows.
    grouped(
        "Delivery",
        row(
            "notification-delivery",
            SettingsSection::Notifications,
            "Show notifications",
            "Where notifications appear: in Compi, from the system, or both.",
            "notify alert toast banner system in-app automatic both",
            |_| vec![SettingsAction::Select(Popup::NotificationDelivery)],
            |app, row, cx| app.render_notification_delivery_row(row, cx),
        ),
    ),
    grouped(
        "Delivery",
        row(
            "notification-system",
            SettingsSection::Notifications,
            "System notifications",
            "Whether the system shows Compi's notifications.",
            "notify permission focus assist do not disturb windows macos",
            |_| {
                if crate::gui::os_notifications::SETTINGS_URL.is_some() {
                    vec![SettingsAction::OpenNotificationSystemSettings]
                } else {
                    Vec::new()
                }
            },
            |app, row, cx| app.render_notification_system_row(row, cx),
        ),
    ),
    grouped(
        "Delivery",
        row(
            "notification-test",
            SettingsSection::Notifications,
            "Test notification",
            "Sends one now with these settings.",
            "notify try check preview",
            |_| vec![SettingsAction::TestNotification],
            |app, row, cx| {
                app.render_button_row(
                    row,
                    app.notifications
                        .test_result()
                        .unwrap_or(row_description("notification-test"))
                        .to_owned(),
                    "Send test",
                    SettingsAction::TestNotification,
                    false,
                    cx,
                )
            },
        ),
    ),
    grouped(
        "Events",
        row(
            "notification-attention",
            SettingsSection::Notifications,
            "Needs attention",
            "A program asks for permission, a question's answer or a sign-in.",
            "notify blocked permission question sign in osc 7501",
            |_| vec![SettingsAction::NotificationAttention],
            |app, row, cx| {
                app.render_toggle_row(
                    row,
                    app.config.notifications.attention,
                    SettingsAction::NotificationAttention,
                    row_description("notification-attention"),
                    cx,
                )
            },
        ),
    ),
    grouped(
        "Events",
        row(
            "notification-finished",
            SettingsSection::Notifications,
            "Finished",
            "A program reports its work done.",
            "notify done complete osc 7501",
            |_| vec![SettingsAction::NotificationFinished],
            |app, row, cx| {
                app.render_toggle_row(
                    row,
                    app.config.notifications.finished,
                    SettingsAction::NotificationFinished,
                    row_description("notification-finished"),
                    cx,
                )
            },
        ),
    ),
    grouped(
        "Events",
        row(
            "notification-focused-pane",
            SettingsSection::Notifications,
            "Focused pane",
            "When the pane you're in notifies too: after you stop typing, clicking or scrolling for a while.",
            "notify idle away active focused current pane typing inactive",
            |_| vec![SettingsAction::Select(Popup::NotificationFocusedPane)],
            |app, row, cx| app.render_notification_focused_pane_row(row, cx),
        ),
    ),
    grouped(
        "Sound",
        row(
            "notification-sound",
            SettingsSection::Notifications,
            "Notification sound",
            "The sound a notification plays, when the system's sound and quiet settings allow it.",
            "notify audio chime bell play preview",
            |app| {
                let mut stops = vec![SettingsAction::Select(Popup::NotificationSound)];
                if app.config.notifications.sound != crate::config::NotificationSound::Off {
                    stops.push(SettingsAction::PlayNotificationSound);
                }
                stops
            },
            |app, row, cx| app.render_notification_sound_row(row, cx),
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
        "Show it as a dot on each sidebar card, and first in each palette tab's status line.",
        |_| vec![SettingsAction::ProgramStatus(2)],
        |app, row, cx| app.render_status_row(row, 2, cx),
    ),
    // Monitor.
    in_header(row(
        "monitor-open",
        SettingsSection::Monitor,
        "Open as pane",
        "Open Monitor in a pane, with its processes and each resource's history.",
        "monitor pane open processes task manager",
        |_| vec![SettingsAction::OpenMonitor],
        |app, row, cx| app.render_open_monitor_row(row, cx),
    )),
    page_row(
        MONITOR_ROW,
        SettingsSection::Monitor,
        "Monitor",
        "The host's CPU, memory, GPU, disk and network, and the apps using its memory.",
        "monitor overview cpu memory gpu graphics disk network processes apps usage activity",
        |_| Vec::new(),
        |app, _, width, cx| app.render_settings_monitor(width, cx),
    ),
    // Updates: Compi's own card, the notes, the switches, then Advanced.
    grouped(
        "Version",
        page_row(
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
            |app, row, _, cx| app.render_update_status(row, cx),
        ),
    ),
    shown_when(
        |app| app.updates.snapshot().service_lag.is_some(),
        grouped(
            "Version",
            row(
                "update-service",
                SettingsSection::Updates,
                "Background service",
                "The service that runs your shells, while it still runs an older version.",
                "daemon service server restart version update",
                |app| app.service_lag_action().into_iter().collect(),
                |app, row, cx| app.render_service_lag_row(row, cx),
            ),
        ),
    ),
    shown_when(
        |app| app.update_notes().is_some(),
        page_row(
            "update-notes",
            SettingsSection::Updates,
            "What's new",
            "Release notes of the update, or of the installed version.",
            "release notes changelog update",
            |app| app.update_notes_stops(),
            |app, row, _, cx| app.render_update_notes(row, cx),
        ),
    ),
    grouped(
        "Automatic",
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
                    "When Compi starts and every 6 hours while it runs.",
                    cx,
                )
            },
        ),
    ),
    grouped(
        "Automatic",
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
                    "Installing still waits for you.",
                    cx,
                )
            },
        ),
    ),
    grouped(
        "Advanced",
        row(
            "update-advanced",
            SettingsSection::Updates,
            "Repair and update log",
            "When updating goes wrong: repair the installation, or read what the updater did.",
            "advanced repair log troubleshoot",
            |_| {
                let mut stops = Vec::with_capacity(2);
                if cfg!(windows) {
                    stops.push(SettingsAction::Update(UpdateControl::Repair));
                }
                stops.push(SettingsAction::Update(UpdateControl::OpenLog));
                stops
            },
            |app, row, cx| app.render_update_advanced(row, cx),
        ),
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
        PERFORMANCE_GROUP,
        row(
            "show-fps",
            SettingsSection::Advanced,
            "Show FPS",
            "Show the frame rate on screen.",
            "fps frames frame rate overlay",
            |_| vec![SettingsAction::ToggleFps],
            |app, row, cx| {
                app.render_toggle_row(
                    row,
                    app.state.show_fps,
                    SettingsAction::ToggleFps,
                    row_description("show-fps"),
                    cx,
                )
            },
        ),
    ),
    grouped(
        PERFORMANCE_GROUP,
        row(
            RESOURCES_ROW,
            SettingsSection::Advanced,
            "Compi's resource use",
            "Frame rate, CPU and memory of Compi and its terminal server, and renderer caches.",
            "fps frames cpu memory renderer cache resources usage",
            |_| Vec::new(),
            |app, _, _| app.render_compi_resources(),
        ),
    ),
    grouped(
        PERFORMANCE_GROUP,
        row(
            REBUILD_ROW,
            SettingsSection::Advanced,
            "Rebuild renderer",
            "Clears rendering caches without stopping your terminals.",
            "renderer cache rebuild reset",
            |_| vec![SettingsAction::RebuildRenderer],
            |app, row, cx| {
                app.render_performance_button(
                    row,
                    "Rebuild renderer",
                    SettingsAction::RebuildRenderer,
                    cx,
                )
            },
        ),
    ),
    grouped(
        PERFORMANCE_GROUP,
        row(
            DIAGNOSTICS_ROW,
            SettingsSection::Advanced,
            "Performance diagnostics",
            "Copy Compi's frame rate, resource use and renderer caches as text.",
            "diagnostics copy clipboard fps cpu memory renderer",
            |_| vec![SettingsAction::CopyPerformance],
            |app, row, cx| {
                app.render_performance_button(
                    row,
                    "Copy diagnostics",
                    SettingsAction::CopyPerformance,
                    cx,
                )
            },
        ),
    ),
    grouped(
        RECOVERY_GROUP,
        row(
            "configuration",
            SettingsSection::Advanced,
            "Configuration",
            "Settings as a TOML file, including some found only there.",
            "toml config file edit",
            |_| vec![SettingsAction::OpenConfiguration],
            |app, row, cx| {
                app.render_button_row(
                    row,
                    row_description("configuration").into(),
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
pub(super) fn row_description(id: &str) -> &'static str {
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
/// with a query, every row it matches in any section, and the command list when it
/// matches a command; without, `section`'s rows.
fn rows_for(section: SettingsSection, query: &str) -> impl Iterator<Item = &'static Row> {
    let searching = !query.trim().is_empty();
    ROWS.iter().filter(move |row| {
        if searching {
            row.matches(query)
                || (row.id == COMMANDS_ROW && commands::search(query).next().is_some())
        } else {
            row.section == section
        }
    })
}

/// Whether Quick Appearance shows `row`: Appearance's scope, Window and Theme rows.
fn quick(row: &Row) -> bool {
    row.section == SettingsSection::Appearance
        && (row.header || matches!(row.group, Some("Window" | "Theme")))
}

/// The commands (indices in `commands::REGISTRY`) the command list shows for the
/// lowercased `query`: those it matches, or all of them when it matches none (the
/// list itself matched) or there is none.
fn shown_commands(query: &str) -> Vec<usize> {
    let all = || (0..commands::REGISTRY.len()).collect();
    if query.trim().is_empty() {
        return all();
    }
    let matched: Vec<usize> = commands::REGISTRY
        .iter()
        .enumerate()
        .filter(|(_, spec)| spec.matches_query(query))
        .map(|(index, _)| index)
        .collect();
    if matched.is_empty() { all() } else { matched }
}

/// `shown_commands(query)` by category, in the palette's category order.
fn command_groups(query: &str) -> Vec<(commands::CommandCategory, Vec<usize>)> {
    let shown = shown_commands(query);
    commands::CommandCategory::ALL
        .into_iter()
        .filter_map(|category| {
            let group: Vec<usize> = shown
                .iter()
                .copied()
                .filter(|&index| commands::REGISTRY[index].category() == category)
                .collect();
            (!group.is_empty()).then_some((category, group))
        })
        .collect()
}

/// The note under a command given `keys` that terminals read themselves.
fn terminal_key_notice(keys: &str) -> Option<String> {
    commands::terminal_reads(keys).then(|| {
        format!(
            "Terminals no longer receive {} while it runs this command.",
            controls::key_label(keys)
        )
    })
}

/// Where `install` has its agent, as its Settings → Agents card says: each side with its
/// version when it reports one, the Linux side first (`Ubuntu 19.0.0`, `Windows 2.1.3`);
/// empty when it has none. The Linux side is named for the WSL `distribution` it was
/// looked for in, else for this computer.
fn agent_places(distribution: Option<&str>, install: Option<&AgentInstall>) -> Vec<String> {
    let Some(install) = install else {
        return Vec::new();
    };
    let linux = distribution.unwrap_or(if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    });
    [(linux, &install.linux), ("Windows", &install.windows)]
        .into_iter()
        .filter_map(|(side, program)| {
            let program: &AgentProgram = program.as_ref()?;
            Some(match &program.version {
                Some(version) => format!("{side} {version}"),
                None => side.to_owned(),
            })
        })
        .collect()
}

/// The line under `section`'s heading: what its page covers.
fn section_summary(section: SettingsSection) -> &'static str {
    match section {
        SettingsSection::General => {
            "How you work: what new tabs open, which agent new threads use, the shell, and \
             closing."
        }
        SettingsSection::Terminal => "Starting folder, font, dev servers, prompt and shell.",
        SettingsSection::Chat => "How agent chats look and behave.",
        SettingsSection::Agents => {
            "Every agent: where it is installed and its version, and how agents ask before \
             they run tools."
        }
        SettingsSection::Projects => "Saved folders and their settings.",
        SettingsSection::Appearance => {
            "Theme, window, interface font, pane layout, tab details and focus."
        }
        SettingsSection::Keyboard => {
            "Click a shortcut, then press the new keys. Escape cancels; Backspace leaves the \
             command without one."
        }
        SettingsSection::Notifications => {
            "When and where Compi tells you a program needs you, and where program status \
             shows."
        }
        SettingsSection::Monitor => "This computer's processes and what they use.",
        SettingsSection::Updates => "Compi's version, what's new and how it updates.",
        SettingsSection::Advanced => "Terminal colors and performance details.",
    }
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

/// The choice after (`forward`) or before `index` of `count`: at the ends it stays, or
/// with `wrap` goes round.
pub(super) fn choice_index(index: usize, count: usize, forward: bool, wrap: bool) -> usize {
    match (forward, wrap) {
        (true, true) => (index + 1) % count,
        (true, false) => (index + 1).min(count - 1),
        (false, true) => (index + count - 1) % count,
        (false, false) => index.saturating_sub(1),
    }
}

/// Shell's choices, in the order its tiles show them.
const WINDOWS_SHELLS: [WindowsShell; 3] = [
    WindowsShell::Automatic,
    WindowsShell::PowerShell,
    WindowsShell::Wsl,
];

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

    /// The Appearance rows Quick Appearance shows now: the scope and Window, then the
    /// theme's way on to Settings.
    fn quick_rows(&self) -> Vec<&'static Row> {
        let mut rows: Vec<_> = rows_for(SettingsSection::Appearance, "")
            .filter(|row| quick(row) && (row.shown)(self))
            .collect();
        rows.sort_by_key(|row| row.id == THEME_ROW);
        rows
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
            focus_order(
                true,
                rows.iter().map(|row| {
                    if row.id == COMMANDS_ROW {
                        (row.id, self.command_stops(&pane.query))
                    } else {
                        (row.id, (row.stops)(self))
                    }
                }),
            )
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
    /// On a section icon it shows only once a key moves it (see `settings_pane_key_down`).
    pub(super) fn set_settings_focus(&mut self, host: SettingsHost, stop: Stop) {
        if let Some(scroll) = self.settings_scroll_mut(&host, in_theme_grid(stop)) {
            scroll.to_focus = true;
        }
        self.settings.focus = Some(SettingsFocus { host, stop });
        self.settings.section_ring = false;
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
        self.reset_projects_page();
        self.popup = None;
        // Asked while the pane Settings opens from still has the keyboard.
        self.read_chat_agents(cx);
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
            pane.projects.follow_pane(to, cx);
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
    /// daemon when it changed, the shell prompt detected when its group shows, and where
    /// the agents are installed looked up when their cards show.
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
                    let projects = ProjectFields::new(&pane_id, window, cx);
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
                            projects,
                        },
                    );
                    if let Some(stop) = self.settings.pending.take() {
                        self.set_settings_focus(SettingsHost::Pane(pane_id.clone()), stop);
                    }
                    if let Some(target) = self.settings.pending_themes.take() {
                        self.start_theme_session(pane_id.clone(), target, true);
                    }
                    // Settings opened another way (`compi settings`, a restored pane).
                    if self.settings.chat_agents_asked == 0 {
                        self.read_chat_agents(cx);
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
        // A highlight on the section icons stays on the section shown, however it
        // changed (`compi settings`, a restored pane).
        if let Some(focus) = &mut self.settings.focus
            && let (SettingsHost::Pane(pane), Stop::Section(section)) =
                (&focus.host, &mut focus.stop)
            && let Some(ui) = self.settings.panes.get(pane)
        {
            *section = ui.section;
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
                || pane.theme_search.focus_handle(cx).is_focused(window)
                || pane.projects.has_keyboard(window, cx))
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
        let agents = self.settings_row_shown(AGENTS_ROW);
        if agents && !self.settings.agents_shown {
            self.read_agent_installs(cx);
        }
        self.settings.agents_shown = agents;
        self.prepare_projects_page(window, cx);
    }

    /// Where the keyboard highlight is: a Settings pane, or Quick Appearance.
    pub(super) fn settings_focus_host(&self) -> Option<SettingsHost> {
        Some(self.settings.focus.as_ref()?.host.clone())
    }

    /// Whether Settings pane `pane` shows `section` (and no search).
    pub(super) fn settings_pane_shows(&self, pane: &PaneId, section: SettingsSection) -> bool {
        self.settings
            .panes
            .get(pane)
            .is_some_and(|ui| ui.section == section && ui.query.trim().is_empty())
    }

    /// The Projects page's fields of Settings pane `pane`; without one, of the pane
    /// with the highlight, else of any.
    pub(super) fn page_project_fields(&self, pane: Option<&PaneId>) -> Option<&ProjectFields> {
        let pane = pane.or_else(|| match &self.settings.focus.as_ref()?.host {
            SettingsHost::Pane(pane) => Some(pane),
            SettingsHost::Quick => None,
        });
        match pane {
            Some(pane) => self.settings.panes.get(pane),
            None => self.settings.panes.values().next(),
        }
        .map(|ui| &ui.projects)
    }

    pub(super) fn project_fields_mut(&mut self, pane: &PaneId) -> Option<&mut ProjectFields> {
        self.settings.panes.get_mut(pane).map(|ui| &mut ui.projects)
    }

    /// The Projects page's fields of every Settings pane.
    pub(super) fn project_fields_each(&mut self) -> impl Iterator<Item = &mut ProjectFields> {
        self.settings.panes.values_mut().map(|ui| &mut ui.projects)
    }

    /// Moves the highlight of Settings pane `pane_id` to the next stop (or, not
    /// `forward`, the one before), and the keyboard with it.
    pub(super) fn step_settings_focus(
        &mut self,
        pane_id: &PaneId,
        forward: bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        let host = SettingsHost::Pane(pane_id.clone());
        let order = self.settings_order(&host);
        let current = self
            .settings
            .focus
            .as_ref()
            .filter(|focus| focus.host == host)
            .map(|focus| focus.stop);
        if let Some(next) = step(&order, current, forward) {
            self.set_settings_focus(host, next);
            self.apply_settings_focus(pane_id, window, cx);
        }
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
                || pane.projects.owns(focused, cx)
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
            Some(Stop::Control {
                action: SettingsAction::ProjectSearch,
                ..
            }) => ui.projects.search_focus(cx),
            Some(Stop::Control {
                action: SettingsAction::Project(ProjectControl::Name),
                ..
            }) => ui.projects.name_focus(cx),
            Some(_) => ui.focus.clone(),
        })
    }

    /// The focused Settings pane's search field, its theme list's, or a field of the
    /// Projects page has the keyboard.
    pub(super) fn settings_search_has_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.focused_pane()
            .and_then(|pane| self.settings.panes.get(pane))
            .is_some_and(|pane| {
                pane.search.focus_handle(cx).is_focused(window)
                    || pane.theme_search.focus_handle(cx).is_focused(window)
                    || pane.projects.has_keyboard(window, cx)
            })
    }

    /// Keys in the focused Settings pane's search fields, before the field's own: Tab
    /// and Down move on to the controls, Escape clears the search or leaves (Settings,
    /// or the theme list). The Projects page's fields have their own
    /// (`projects_field_key`).
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
        if self
            .settings
            .panes
            .get(&pane_id)
            .is_some_and(|pane| pane.projects.has_keyboard(window, cx))
        {
            return self.projects_field_key(&pane_id, key, window, cx);
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

    /// Escape cancels a theme preview or leaves a detail page first. Otherwise it
    /// moves the highlight to the section strip, then returns to the terminal.
    pub(super) fn settings_escape(&mut self, pane_id: &PaneId) {
        let host = SettingsHost::Pane(pane_id.clone());
        if self.theme_bar_shown(pane_id) {
            self.cancel_theme_preview();
            self.settle_theme_focus(pane_id);
        } else if self.theme_list_shown(pane_id) {
            self.close_theme_list();
            self.focus_theme_origin(pane_id);
        } else if self.settings.project_selected.is_some()
            && self.settings_pane_shows(pane_id, SettingsSection::Projects)
        {
            self.back_to_projects(host);
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
    pub(super) fn apply_settings_focus(&self, pane_id: &PaneId, window: &mut Window, cx: &mut App) {
        if let Some(handle) = self.settings_keyboard(pane_id, cx)
            && !handle.is_focused(window)
        {
            window.focus(&handle, cx);
        }
    }

    /// Returns the keyboard to the shell: a terminal showing Settings as its view shows
    /// its shell again; else the terminal Settings was opened from, or another terminal
    /// of the tab, takes it.
    fn leave_settings(&mut self, pane_id: &PaneId) {
        if self.escape_to_shell(pane_id) {
            return;
        }
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
            || pane.projects.has_keyboard(window, cx)
            || self
                .project_model_search()
                .is_some_and(|search| search.focus_handle(cx).is_focused(window))
        {
            // Text belongs to the field; Tab, Down and Escape were taken before it.
            return;
        }
        let key = &event.keystroke;
        if let Some(command) = self.recording_shortcut(pane_id) {
            // Every key goes to the recording, Compi's own shortcuts included.
            self.record_shortcut(command, key, window, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
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
            if self.type_in_project_search(pane_id, text, window, cx) {
                self.apply_settings_focus(pane_id, window, cx);
                cx.stop_propagation();
                cx.notify();
                return;
            }
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
            // A key that leaves the highlight on the section icons shows it there.
            self.settings.section_ring = self
                .settings
                .focus
                .as_ref()
                .is_some_and(|focus| matches!(focus.stop, Stop::Section(_)));
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
        // A choice's Left and Right pick the one before or after.
        if let ("left" | "right", Some(Stop::Control { action, .. })) = (key.key.as_str(), current)
            && self.step_choice(action, key.key == "right", false, window, cx)
        {
            return true;
        }
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
            (
                "left" | "right",
                Some(Stop::Control {
                    action: SettingsAction::Project(ProjectControl::Agent),
                    ..
                }),
            ) => {
                self.step_project_agent(key.key == "right", false, cx);
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
                if matches!(action, SettingsAction::Project(_))
                    && self.settings.focus.as_ref().map(|focus| focus.stop) != current
                {
                    // The project control moved the highlight to a different stop.
                    return true;
                }
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
        self.reset_projects_page();
        let Some(pane) = self.settings.panes.get_mut(pane_id) else {
            return;
        };
        pane.section = section;
        self.settings.recording = None;
        self.settings.shortcut_notice = None;
        pane.scroll.handle.set_offset(point(px(0.0), px(0.0)));
        if !pane.query.is_empty() {
            pane.query.clear();
            pane.search
                .update(cx, |search, cx| search.set_value("", window, cx));
        }
    }

    fn reset_projects_page(&mut self) {
        self.settings.project_selected = None;
        self.settings.archived_open = false;
        self.close_project_model_picker();
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
            SettingsAction::ChatEnter
            | SettingsAction::ChatOpen
            | SettingsAction::ChatAgent
            | SettingsAction::NewTab
            | SettingsAction::Shell
            | SettingsAction::DevServer
            | SettingsAction::Density
            | SettingsAction::FocusIndicator => {
                self.step_choice(action, true, true, window, cx);
            }
            SettingsAction::CopyInstallCommand(agent) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                    agent.entry().install.to_owned(),
                ));
                self.settings.install_copied = Some(agent);
            }
            SettingsAction::ToggleChatAgentsBar => {
                self.set_chat_agents_bar(!self.config.chat_agents_bar, cx);
            }
            SettingsAction::OtherShellCommands => {
                let enabled = !self.config.other_shell_commands();
                if let Err(error) = self.config.save_other_shell_commands(enabled) {
                    self.global_error = Some(error);
                } else {
                    share_launch_change(cx, move |launch| {
                        launch.env.insert(
                            "COMPI_OTHER_SHELL_COMMANDS".into(),
                            if enabled { "1" } else { "0" }.into(),
                        );
                    });
                }
            }
            SettingsAction::NotificationAttention => {
                let mut settings = self.config.notifications;
                settings.attention = !settings.attention;
                self.set_notification_settings(settings, cx);
            }
            SettingsAction::NotificationFinished => {
                let mut settings = self.config.notifications;
                settings.finished = !settings.finished;
                self.set_notification_settings(settings, cx);
            }
            SettingsAction::PlayNotificationSound => self.play_notification_sound(cx),
            SettingsAction::OpenNotificationSystemSettings => {
                if let Some(url) = crate::gui::os_notifications::SETTINGS_URL {
                    cx.open_url(url);
                }
            }
            SettingsAction::TestNotification => self.test_notification(window, cx),
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
            SettingsAction::Select(popup) => self.popup = Some(popup),
            SettingsAction::Opacity => self.adjust_opacity(0.05, window, cx),
            SettingsAction::ResetWindowAppearance => self.reset_window_appearance(window),
            SettingsAction::ResetSidebar => self.execute(Command::ResetSidebarWidth, window, cx),
            SettingsAction::ResetLayout => self.execute(Command::ResetClientLayout, window, cx),
            SettingsAction::FontSize(command) => self.execute(command, window, cx),
            SettingsAction::ChatTextSize(command) => self.step_chat_text_size(command, window, cx),
            SettingsAction::FileTextSize(command) => self.step_file_text_size(command, window, cx),
            SettingsAction::OpenConfiguration => {
                self.execute(Command::OpenConfiguration, window, cx)
            }
            SettingsAction::Shortcut(command) => self.toggle_shortcut_recording(command),
            SettingsAction::ResetShortcut(command) => {
                self.change_shortcut(command, None, window, cx)
            }
            SettingsAction::OpenMonitor => self.execute(Command::OpenMonitor, window, cx),
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
            SettingsAction::ChooseStartingFolder => self.choose_starting_folder(cx),
            SettingsAction::StartAtHome => self.set_starting_folder(None, cx),
            SettingsAction::Project(control) => self.activate_project_control(control, window, cx),
            // The field takes the keyboard (`settings_keyboard`).
            SettingsAction::ProjectSearch => {}
            SettingsAction::ArchivedProjects => {
                self.settings.archived_open = !self.settings.archived_open;
            }
            SettingsAction::OpenSettings => {
                self.open_settings(Some(SettingsSection::Appearance), None, window, cx)
            }
        }
    }

    /// Picks the choice of `action` after the current one (`forward`) or before it:
    /// at the ends it stops there, or with `wrap` goes round. False when `action` is
    /// not a choice.
    fn step_choice(
        &mut self,
        action: SettingsAction,
        forward: bool,
        wrap: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::theme::{FocusIndicator, WorkspaceDensity};
        fn next<T: Copy + PartialEq>(all: &[T], current: T, forward: bool, wrap: bool) -> T {
            let index = all.iter().position(|choice| *choice == current);
            all[choice_index(index.unwrap_or(0), all.len(), forward, wrap)]
        }
        match action {
            SettingsAction::ChatEnter => {
                let enter = next(&ChatEnter::ALL, self.config.chat_enter, forward, wrap);
                self.set_chat_enter(enter, cx);
            }
            SettingsAction::ChatOpen => {
                let open = next(&ChatOpen::ALL, self.config.chat_open, forward, wrap);
                self.set_chat_open(open, cx);
            }
            SettingsAction::ChatAgent => self.step_chat_agent(forward, wrap, cx),
            SettingsAction::NewTab => {
                let current = self.config.new_tab.unwrap_or_default();
                self.set_new_tab(next(&NewTab::ALL, current, forward, wrap), cx);
            }
            SettingsAction::Shell => {
                if let Some(current) = self.windows_shell() {
                    self.set_windows_shell(next(&WINDOWS_SHELLS, current, forward, wrap), cx);
                }
            }
            SettingsAction::DevServer => {
                let action = next(&DevServer::ALL, self.config.dev_server, forward, wrap);
                self.set_dev_server(action, cx);
            }
            SettingsAction::Density => {
                let density = next(&WorkspaceDensity::ALL, self.config.density, forward, wrap);
                self.apply_density(density, window, cx);
            }
            SettingsAction::FocusIndicator => {
                let indicator = next(
                    &FocusIndicator::ALL,
                    self.config.focus_indicator,
                    forward,
                    wrap,
                );
                if let Err(error) = self.config.save_focus_indicator(indicator) {
                    self.global_error = Some(error);
                }
            }
            _ => return false,
        }
        true
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

    /// Save the terminal font size (`None` restores the default) for every window.
    pub(super) fn apply_font_size(
        &mut self,
        size: Option<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let unchanged = match size {
            Some(size) => size == self.config.font.size,
            None => self.config.provenance.font_size == crate::config::ValueSource::Default,
        };
        if unchanged {
            return;
        }
        if let Err(error) = self.config.save_font_size(size) {
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
        self.performance_notice = Some((
            REBUILD_ROW,
            "Renderer rebuilt. Visual caches will repopulate as needed.",
        ));
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
    /// (the current one lit: drawn in the accent, filled and underlined) with a thin
    /// divider between their groups, and the search icon; or while searching, the title
    /// and the search field in their place. Too narrow for every icon on one line, the
    /// icons wrap onto lines of their own, without dividers.
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
        // The icons' room: the pane less the padding, the search icon and the gaps.
        let strip_room = width - 32.0 - 24.0 - 12.0;
        let dividers = SettingsSection::ALL
            .into_iter()
            .filter(|section| opens_strip_group(*section))
            .count();
        let strip_width =
            SettingsSection::ALL.len() as f32 * 28.0 + 4.0 + dividers as f32 * STRIP_DIVIDER_WIDTH;
        let wrapped = !titled && strip_room < strip_width;
        let line_height = if wrapped { 34.0 } else { HEADER_HEIGHT };
        let header = div()
            .flex_none()
            .min_h(px(HEADER_HEIGHT))
            .px_4()
            .when(wrapped, |header| header.py_1())
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
            .flat_map(|(index, section)| {
                let divider = (!wrapped && opens_strip_group(section)).then(|| {
                    div()
                        .flex_none()
                        .h(px(line_height))
                        .mx(px(2.0))
                        .flex()
                        .items_center()
                        .child(div().w(px(1.0)).h(px(16.0)).bg(color(colors.border)))
                        .into_any_element()
                });
                let active = pane.section == section;
                let focused = self.settings.section_ring
                    && self.settings.focus.as_ref()
                        == Some(&SettingsFocus {
                            host: host.clone(),
                            stop: Stop::Section(section),
                        });
                let pane_id = pane_id.clone();
                let entry = controls::icon_button(
                    ("settings-section", index),
                    if active {
                        controls::glyph(section_icon(section), color(colors.accent))
                    } else {
                        section_icon(section).into()
                    },
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
                // The entry fills its line's height: the icon centred, its ring clear of
                // the edges, and the underline on the line's bottom edge.
                let entry = div()
                    .relative()
                    .flex_none()
                    .h(px(line_height))
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
                    .into_any_element();
                divider.into_iter().chain([entry])
            });
        let open = pane_id.clone();
        header
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    // Room for the end icons' rings.
                    .px(px(4.0))
                    .flex()
                    .flex_wrap()
                    .gap_x_1()
                    .children(entries),
            )
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
    /// the right, every row's control ending on the card's one right edge; below them
    /// when `compact` (a narrow pane, picture tiles).
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
            .min_h(px(48.0))
            .py(px(10.0))
            .flex()
            .when(compact, |row| row.flex_col().items_start().gap(px(10.0)))
            .when(!compact, |row| row.items_center().justify_between().gap_4())
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

    /// A setting's current value beside its control, muted: "237 px", "80%".
    fn settings_value(&self, value: impl Into<SharedString>) -> Div {
        let colors = *self.colors();
        div()
            .flex_none()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .line_height(px(UI_SMALL_LINE_HEIGHT))
            .text_color(color(modal_text_color(colors.muted, &colors)))
            .child(value.into())
    }

    /// The head of an item card (an agent, a project, Compi itself): its `mark` in a
    /// square, its `title` line (the name, then any chips), what it says of itself
    /// (`details`) and its `action`, which goes under the text when `stacked`. Each
    /// item is a card of its own, unlike the setting rows a group's card holds.
    pub(super) fn item_card_head(
        &self,
        mark: impl IntoElement,
        title: impl IntoIterator<Item = AnyElement>,
        details: impl IntoIterator<Item = AnyElement>,
        action: Option<AnyElement>,
        stacked: bool,
    ) -> Div {
        let colors = *self.colors();
        let mark = div()
            .flex_none()
            .size(px(ITEM_MARK_TILE))
            .flex()
            .items_center()
            .justify_center()
            .rounded_lg()
            .border_1()
            .border_color(color(colors.border))
            .bg(material_color(colors.background, 1.0))
            .child(mark);
        let text = div()
            .min_w_0()
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x_1p5()
                    .font_weight(FontWeight::MEDIUM)
                    .children(title),
            )
            .children(details);
        let head = div()
            .min_w_0()
            .w_full()
            .px_4()
            .py_3()
            .flex()
            .items_center()
            .gap_3()
            .child(mark)
            .child(text);
        match action {
            Some(action) if stacked => div()
                .min_w_0()
                .w_full()
                .flex()
                .flex_col()
                .child(head)
                .child(
                    div()
                        .min_w_0()
                        .pl(px(16.0 + ITEM_MARK_TILE + 12.0))
                        .pr_4()
                        .pb_3()
                        .flex()
                        .child(action),
                ),
            Some(action) => head.child(div().flex_none().flex().items_center().child(action)),
            None => head,
        }
    }

    /// A line under an item card's name, in `tint` (a theme color, made legible).
    pub(super) fn item_detail(&self, text: impl Into<SharedString>, tint: u32) -> AnyElement {
        let colors = *self.colors();
        div()
            .min_w_0()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .line_height(px(UI_SMALL_LINE_HEIGHT))
            .text_color(color(modal_text_color(tint, &colors)))
            .child(text.into())
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

    pub(super) fn render_button_row(
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

    /// Monitor's Open as pane: a button at the right end of the page's heading line
    /// or, in search results, a row.
    fn render_open_monitor_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        if row.header {
            return self.settings_button(row, 0, row.title, SettingsAction::OpenMonitor, cx);
        }
        self.render_button_row(
            row,
            row_description(row.row).into(),
            "Open as pane",
            SettingsAction::OpenMonitor,
            false,
            cx,
        )
    }

    /// Global defaults or This window: a small switch, at the right end of the page's
    /// heading line or, in search results and Quick Appearance, as a row.
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
                self.settings_value(format!("{:.0}%", appearance.terminal_opacity * 100.0))
                    .w(px(40.0))
                    .text_right(),
            )
            .into_any_element();
        self.settings_row(
            row.title,
            if enabled {
                row_description(row.row).into()
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
        let action = SettingsAction::Select(popup);
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

    /// Shell (Windows): what new tabs and splits start, picked for their folder, Windows
    /// PowerShell or WSL, as tiles of each one's console. Saved to the configuration and
    /// given to every window; running shells keep theirs.
    fn render_shell_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let current = self.windows_shell();
        let tiles = WINDOWS_SHELLS.map(|shell| Tile {
            label: match shell {
                WindowsShell::Automatic => "Automatic",
                WindowsShell::PowerShell => "PowerShell",
                WindowsShell::Wsl => "WSL",
            }
            .into(),
            preview: match shell {
                // Half each: whichever the folder calls for.
                WindowsShell::Automatic => div()
                    .flex()
                    .child(self.sketch_powershell(true))
                    .child(self.sketch_ubuntu(true)),
                WindowsShell::PowerShell => div().flex().child(self.sketch_powershell(false)),
                WindowsShell::Wsl => div().flex().child(self.sketch_ubuntu(false)),
            },
            picked: current == Some(shell),
            unavailable: None,
        });
        let detail = match current {
            Some(WindowsShell::Automatic) => {
                "New tabs and splits in a Linux folder start WSL; elsewhere Windows PowerShell, which starts without waiting for WSL. Open tabs keep their shell."
            }
            Some(WindowsShell::PowerShell) => {
                "New tabs and splits start Windows PowerShell. Open tabs keep their shell."
            }
            Some(WindowsShell::Wsl) => {
                "New tabs and splits start your default WSL distribution. Open tabs keep their shell."
            }
            None => "Fix the configuration file to choose a shell.",
        };
        self.render_tile_row(
            row,
            SettingsAction::Shell,
            detail.into(),
            tiles,
            Rc::new(|this, index, _, cx| this.set_windows_shell(WINDOWS_SHELLS[index], cx)),
            cx,
        )
    }

    /// The shell new tabs start, as configured; `None` while the configuration is broken.
    fn windows_shell(&self) -> Option<WindowsShell> {
        let launch = self.config.launch.as_ref().ok()?;
        Some(launch.profile.windows_shell)
    }

    /// Saves the shell and gives it to every window.
    fn set_windows_shell(&mut self, shell: WindowsShell, cx: &mut Context<Self>) {
        if let Err(error) = self.config.save_windows_shell(shell) {
            self.global_error = Some(error);
            return;
        }
        share_launch_change(cx, move |launch| launch.profile.windows_shell = shell);
    }

    /// Approvals: a dropdown of how a chat's agent asks before it runs tools.
    fn render_chat_approval_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let popup = Popup::ChatApproval;
        let action = SettingsAction::Select(popup);
        let focused = self.settings_focused(row, action);
        let current = self.config.chat_approval;
        let options = crate::config::CHAT_APPROVALS.into_iter().map(|approval| {
            controls::select_option(chat_approval_label(approval))
                .detail(chat_approval_detail(approval))
        });
        let open_row = row.clone();
        let select = controls::select(
            "chat-approval-select",
            options,
            crate::config::CHAT_APPROVALS
                .iter()
                .position(|approval| *approval == current),
            self.popup == Some(popup),
        )
        .placeholder(chat_approval_label(current))
        .focused(focused)
        .anchor(Anchor::TopRight)
        .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
            this.focus_settings_control(&open_row, action);
            this.set_popup(popup, *open);
            cx.notify();
        }))
        .on_pick(cx.listener(|this, index: &usize, _, cx| {
            if let Some(approval) = crate::config::CHAT_APPROVALS.get(*index) {
                this.set_chat_approval(*approval, cx);
            }
            cx.notify();
        }));
        let control = div()
            .relative()
            .flex_none()
            .child(select)
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element();
        self.settings_row(
            row.title,
            row_description("chat-approval").into(),
            control,
            row.compact,
        )
    }

    /// Saves the chat approval and gives it to every window, whose chat watchers pass
    /// it on when they next connect.
    fn set_chat_approval(&mut self, approval: ChatApproval, cx: &mut Context<Self>) {
        if let Err(error) = self.config.save_chat_approval(approval) {
            self.global_error = Some(error);
            return;
        }
        share_config_change(cx, move |config| config.chat_approval = approval);
    }

    /// Enter while the agent works: Steer or Queue, as a switch.
    fn render_chat_enter_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let current = self.config.chat_enter;
        let detail = match current {
            ChatEnter::Steer => {
                format!("Enter steers the running turn; {SECONDARY_KEY} queues for after it.")
            }
            ChatEnter::Queue => {
                format!("Enter queues for after the running turn; {SECONDARY_KEY} steers it.")
            }
        };
        self.render_choice_row(
            row,
            SettingsAction::ChatEnter,
            ChatEnter::ALL.map(|enter| {
                (
                    enter.label(),
                    match enter {
                        ChatEnter::Steer => "Into the running turn",
                        ChatEnter::Queue => "After the running turn",
                    },
                    enter == current,
                )
            }),
            |app, index, cx| app.set_chat_enter(ChatEnter::ALL[index], cx),
            detail,
            cx,
        )
    }

    /// New tabs: Chat first or Terminal first, as tiles of a new thread and a new shell.
    /// Unset reads as Terminal first, what + opens until it is chosen.
    fn render_new_tab_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let current = self.config.new_tab.unwrap_or_default();
        let detail = match current {
            NewTab::Chat => {
                "The tab strip's + and Ctrl+T open a new thread; right-click + for a terminal."
            }
            NewTab::Terminal => {
                "The tab strip's + and Ctrl+T open a shell; right-click + for a new thread."
            }
        };
        let tiles = NewTab::ALL.map(|new_tab| Tile {
            label: new_tab.label().into(),
            preview: div().flex().child(match new_tab {
                NewTab::Chat => self.sketch_chat(),
                NewTab::Terminal => self.sketch_shell(),
            }),
            picked: new_tab == current,
            unavailable: None,
        });
        self.render_tile_row(
            row,
            SettingsAction::NewTab,
            detail.into(),
            tiles,
            Rc::new(|this, index, _, cx| this.set_new_tab(NewTab::ALL[index], cx)),
            cx,
        )
    }

    /// Opening a chat: in place of the shell or beside it, as tiles of the pane.
    fn render_chat_open_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let current = self.config.chat_open;
        let detail = match current {
            ChatOpen::InPlace => {
                "New chat and compi chat show the chat over the terminal's shell, which keeps running."
            }
            ChatOpen::Beside => {
                "New chat and compi chat open the chat in a new pane beside the terminal."
            }
        };
        let seam = color(self.colors().border);
        let tiles = ChatOpen::ALL.map(|open| Tile {
            label: open.label().into(),
            preview: match open {
                ChatOpen::InPlace => div().flex().child(self.sketch_chat()),
                ChatOpen::Beside => div()
                    .flex()
                    .child(self.sketch_shell())
                    .child(div().flex_none().w(px(1.0)).bg(seam))
                    .child(self.sketch_chat()),
            },
            picked: open == current,
            unavailable: None,
        });
        self.render_tile_row(
            row,
            SettingsAction::ChatOpen,
            detail.into(),
            tiles,
            Rc::new(|this, index, _, cx| this.set_chat_open(ChatOpen::ALL[index], cx)),
            cx,
        )
    }

    /// Default agent: a tile per agent, its own mark and name; one not installed where
    /// new chats open is dimmed, says so, and can't be picked.
    fn render_chat_agent_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let current = self.config.chat_agent;
        let tiles = ChatHarness::ALL.map(|agent| Tile {
            label: agent.name().into(),
            preview: self.sketch_agent(agent),
            picked: agent == current,
            unavailable: (!self.chat_agent_installed(agent)).then_some("Not installed"),
        });
        self.render_tile_row(
            row,
            SettingsAction::ChatAgent,
            row_description(row.row).into(),
            tiles,
            Rc::new(|this, index, _, cx| this.set_chat_agent(ChatHarness::ALL[index], cx)),
            cx,
        )
    }

    /// Whether `agent` is installed where new chats open, as far as Settings knows: an
    /// agent counts as installed until the daemon says otherwise.
    fn chat_agent_installed(&self, agent: ChatHarness) -> bool {
        self.settings
            .chat_agents
            .as_ref()
            .is_none_or(|installed| installed.contains(&agent))
    }

    /// Asks the daemon, off the UI thread, which agents are installed where new chats
    /// open (the focused pane's environment), for Default agent. Nowhere known, every
    /// agent stays on offer.
    pub(super) fn read_chat_agents(&mut self, cx: &mut Context<Self>) {
        self.settings.chat_agents = None;
        self.settings.chat_agents_asked += 1;
        let Ok((environment, _)) = self.chat_scope() else {
            return;
        };
        let asked = self.settings.chat_agents_asked;
        let target = self.target.clone();
        cx.spawn(async move |this, cx| {
            let listed = cx
                .background_executor()
                .spawn(async move { target.connect()?.chat_agents(&environment) })
                .await;
            this.update(cx, |this, cx| {
                if this.settings.chat_agents_asked == asked {
                    this.settings.chat_agents = listed.ok();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Asks the daemon, off the UI thread, where every agent is installed on this PC,
    /// with its versions, for Settings → Agents.
    fn read_agent_installs(&mut self, cx: &mut Context<Self>) {
        self.settings.agent_installs = None;
        self.settings.agent_installs_asked += 1;
        self.settings.install_copied = None;
        let asked = self.settings.agent_installs_asked;
        let target = self.target.clone();
        cx.spawn(async move |this, cx| {
            let installs = cx
                .background_executor()
                .spawn(async move { target.connect()?.agent_installs() })
                .await;
            this.update(cx, |this, cx| {
                if this.settings.agent_installs_asked == asked {
                    this.settings.agent_installs =
                        Some(installs.map_err(|error| error.to_string()));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Where `agent` is installed, as far as Settings → Agents knows: each side it has,
    /// with its version (`agent_places`); `None` while asking or when asking failed.
    fn agent_places(&self, agent: ChatHarness) -> Option<Vec<String>> {
        let Some(Ok(installs)) = &self.settings.agent_installs else {
            return None;
        };
        Some(agent_places(
            installs.distribution.as_deref(),
            installs
                .agents
                .iter()
                .find(|install| install.harness == agent),
        ))
    }

    /// The Agents page's controls: Copy command on each agent installed nowhere.
    fn agent_stops(&self) -> Vec<SettingsAction> {
        ChatHarness::ALL
            .into_iter()
            .filter(|agent| {
                self.agent_places(*agent)
                    .is_some_and(|places| places.is_empty())
            })
            .map(SettingsAction::CopyInstallCommand)
            .collect()
    }

    /// Settings → Agents: a card per agent.
    fn render_agents_page(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .children(
                ChatHarness::ALL
                    .into_iter()
                    .map(|agent| self.render_agent_card(row, agent, cx)),
            )
            .into_any_element()
    }

    /// One agent's item card: its mark and name, Default when new threads start with it,
    /// and where it is installed with each side's version, or how to install it, with
    /// Copy command.
    fn render_agent_card(&self, row: &RowCx, agent: ChatHarness, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let places = self.agent_places(agent);
        let missing = places.as_ref().is_some_and(Vec::is_empty);
        let status = match (&self.settings.agent_installs, places) {
            (None, _) => "Looking for it…".to_owned(),
            (Some(Err(error)), _) => format!("Couldn't look for it: {error}"),
            (Some(Ok(_)), Some(places)) if !places.is_empty() => places.join(" · "),
            (Some(Ok(_)), _) => {
                format!("Not installed. Install it with {}", agent.entry().install)
            }
        };
        let copy = missing.then(|| {
            let label = if self.settings.install_copied == Some(agent) {
                "Copied"
            } else {
                "Copy command"
            };
            self.settings_control(
                row,
                SettingsAction::CopyInstallCommand(agent),
                controls::text_button(("agent-install", agent as usize), label)
                    .header()
                    .detail(agent.entry().install),
                cx,
            )
        });
        let default = (self.config.chat_agent == agent).then(|| {
            controls::chip("Default", controls::ChipTone::Tint(colors.accent)).into_any_element()
        });
        settings_card(&colors)
            .child(
                self.item_card_head(
                    gpui::img(super::chat::harness_mark(agent))
                        .size(px(AGENT_MARK_SIZE))
                        .when(missing, |mark| mark.opacity(0.5)),
                    std::iter::once(div().child(agent.name()).into_any_element()).chain(default),
                    [self.item_detail(status, colors.muted)],
                    copy,
                    row.compact,
                ),
            )
            .into_any_element()
    }

    /// Default agent from the keyboard: the next installed agent after the current one,
    /// `forward` or back; Enter (`wrap`) goes round from the last to the first, Left and
    /// Right stop at the ends.
    fn step_chat_agent(&mut self, forward: bool, wrap: bool, cx: &mut Context<Self>) {
        let agents = ChatHarness::ALL;
        let start = agents
            .iter()
            .position(|agent| *agent == self.config.chat_agent)
            .unwrap_or(0);
        let order: Vec<usize> = if forward {
            (start + 1..agents.len())
                .chain((0..start).filter(|_| wrap))
                .collect()
        } else {
            (0..start)
                .rev()
                .chain((start + 1..agents.len()).rev().filter(|_| wrap))
                .collect()
        };
        if let Some(agent) = order
            .into_iter()
            .map(|index| agents[index])
            .find(|agent| self.chat_agent_installed(*agent))
        {
            self.set_chat_agent(agent, cx);
        }
    }

    /// When a dev server starts: tiles of the terminal with the card, the page in its
    /// place, and the terminal as it is.
    fn render_dev_server_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let current = self.config.dev_server;
        let detail = match current {
            DevServer::Offer => {
                "A card at the terminal's bottom right offers the page once the address answers."
            }
            DevServer::Open => {
                "The page shows in place of the shell once the address answers, one time for each start of the server."
            }
            DevServer::Nothing => "The terminal stays as it is.",
        };
        let card = || {
            div()
                .absolute()
                .right(px(5.0))
                .bottom(px(5.0))
                .w(px(46.0))
                .h(px(16.0))
                .px(px(4.0))
                .flex()
                .items_center()
                .gap(px(3.0))
                .rounded(px(3.0))
                .border_1()
                .border_color(color(colors.accent))
                .bg(material_color(colors.background, 1.0))
                .child(
                    div()
                        .flex_none()
                        .size(px(5.0))
                        .rounded_full()
                        .bg(color(colors.accent)),
                )
                .child(self.sketch_line(24.0))
        };
        let page = || {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .bg(color(colors.surface))
                .child(
                    div()
                        .flex_none()
                        .h(px(12.0))
                        .px(px(4.0))
                        .flex()
                        .items_center()
                        .gap(px(2.0))
                        .border_b_1()
                        .border_color(color(colors.border))
                        .children((0..3).map(|_| {
                            div()
                                .flex_none()
                                .size(px(3.0))
                                .rounded_full()
                                .bg(color(colors.muted).opacity(0.6))
                        }))
                        .child(
                            div()
                                .ml(px(3.0))
                                .h(px(6.0))
                                .w(px(52.0))
                                .rounded_full()
                                .border_1()
                                .border_color(color(colors.border)),
                        ),
                )
                .child(
                    div()
                        .p(px(5.0))
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(
                            div()
                                .h(px(6.0))
                                .w(px(34.0))
                                .rounded(px(2.0))
                                .bg(color(colors.accent).opacity(0.8)),
                        )
                        .child(self.sketch_line(52.0))
                        .child(self.sketch_line(40.0)),
                )
        };
        let tiles = DevServer::ALL.map(|action| Tile {
            label: action.label().into(),
            preview: match action {
                DevServer::Offer => div()
                    .relative()
                    .flex()
                    .child(self.sketch_shell())
                    .child(card()),
                DevServer::Open => div().flex().child(page()),
                DevServer::Nothing => div().flex().child(self.sketch_shell()),
            },
            picked: action == current,
            unavailable: None,
        });
        self.render_tile_row(
            row,
            SettingsAction::DevServer,
            detail.into(),
            tiles,
            Rc::new(|this, index, _, cx| this.set_dev_server(DevServer::ALL[index], cx)),
            cx,
        )
    }

    /// A choice among `options` (label, tooltip detail, picked) as a segmented switch, one
    /// stop for `action`: a click or Left and Right pick, Enter moves to the next.
    fn render_choice_row<const N: usize>(
        &self,
        row: &RowCx,
        action: SettingsAction,
        options: [(&'static str, &'static str, bool); N],
        pick: fn(&mut Self, usize, &mut Context<Self>),
        detail: String,
        cx: &Context<Self>,
    ) -> AnyElement {
        let focused = self.settings_focused(row, action);
        let buttons = options
            .into_iter()
            .enumerate()
            .map(|(index, (label, tip, picked))| {
                let row = row.clone();
                controls::text_button((row.row, index), label)
                    .selected(picked)
                    .detail(tip)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.focus_settings_control(&row, action);
                        pick(this, index, cx);
                        cx.notify();
                    }))
            });
        let switch = controls::highlight(
            focused,
            controls::segmented((row.row, 9_usize), buttons),
            cx,
        )
        .child(self.settings_focus_anchor(focused, cx))
        .into_any_element();
        self.settings_row(row.title, detail, switch, row.compact)
    }

    /// Saves what Enter does while the agent works and gives it to every window.
    fn set_chat_enter(&mut self, enter: ChatEnter, cx: &mut Context<Self>) {
        if let Err(error) = self.config.save_chat_enter(enter) {
            self.global_error = Some(error);
            return;
        }
        share_config_change(cx, move |config| config.chat_enter = enter);
    }

    /// Saves where a new chat opens and gives it to every window.
    fn set_chat_open(&mut self, open: ChatOpen, cx: &mut Context<Self>) {
        if let Err(error) = self.config.save_chat_open(open) {
            self.global_error = Some(error);
            return;
        }
        share_config_change(cx, move |config| config.chat_open = open);
    }

    /// Saves the agent new threads start with and gives it to every window.
    fn set_chat_agent(&mut self, agent: ChatHarness, cx: &mut Context<Self>) {
        if let Err(error) = self.config.save_chat_agent(agent) {
            self.global_error = Some(error);
            return;
        }
        share_config_change(cx, move |config| config.chat_agent = agent);
    }

    /// Saves what a new tab opens with and gives it to every window.
    pub(super) fn set_new_tab(&mut self, new_tab: NewTab, cx: &mut Context<Self>) {
        if let Err(error) = self.config.save_new_tab(new_tab) {
            self.global_error = Some(error);
            return;
        }
        share_config_change(cx, move |config| config.new_tab = Some(new_tab));
    }

    /// Saves what a terminal does when a dev server starts and gives it to every
    /// window, whose terminals follow it from their next check.
    fn set_dev_server(&mut self, action: DevServer, cx: &mut Context<Self>) {
        if let Err(error) = self.config.save_dev_server(action) {
            self.global_error = Some(error);
            return;
        }
        share_config_change(cx, move |config| config.dev_server = action);
    }

    /// Saves whether chats show the agents bar and gives it to every window.
    fn set_chat_agents_bar(&mut self, shown: bool, cx: &mut Context<Self>) {
        if let Err(error) = self.config.save_chat_agents_bar(shown) {
            self.global_error = Some(error);
            return;
        }
        share_config_change(cx, move |config| config.chat_agents_bar = shown);
    }

    /// The folder new tabs start in when they open from no other pane; `None` is home.
    fn starting_folder(&self) -> Option<&str> {
        self.config
            .launch
            .as_ref()
            .ok()?
            .profile
            .working_directory
            .as_deref()
    }

    /// Starting folder: the folder, muted, with Choose… and, once one is set, Home.
    fn render_starting_folder_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let folder = self.starting_folder();
        let value = match folder {
            Some(folder) => folder,
            None if self.config.launch.is_err() => "Fix the configuration file",
            None => "Home folder",
        };
        let control = div()
            .min_w_0()
            .flex()
            .items_center()
            .gap_1()
            .child(
                self.settings_value(value.to_owned())
                    .flex_initial()
                    .min_w_0()
                    .max_w(px(240.0))
                    .mr_2()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis(),
            )
            .child(self.settings_button(
                row,
                0,
                "Choose…",
                SettingsAction::ChooseStartingFolder,
                cx,
            ))
            .when(folder.is_some(), |buttons| {
                buttons.child(self.settings_button(row, 1, "Home", SettingsAction::StartAtHome, cx))
            })
            .into_any_element();
        self.settings_row(
            row.title,
            row_description(row.row).into(),
            control,
            row.compact,
        )
    }

    /// Asks for a folder on this computer. A WSL shell starts in a Windows folder's WSL path.
    fn choose_starting_folder(&mut self, cx: &mut Context<Self>) {
        let picker = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(folders))) = picker.await else {
                return;
            };
            let Some(folder) = folders.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.set_starting_folder(Some(folder.to_string_lossy().into_owned()), cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Saves the starting folder and gives it to every window.
    fn set_starting_folder(&mut self, folder: Option<String>, cx: &mut Context<Self>) {
        if let Err(error) = self.config.save_working_directory(folder.clone()) {
            self.global_error = Some(error);
            return;
        }
        share_launch_change(cx, move |launch| {
            launch.profile.working_directory = folder.clone();
        });
    }

    /// A choice drawn as picture tiles, one stop for `action` (`id` tells apart rows
    /// that share a row id): a click picks a tile (`pick` with its index), Left and
    /// Right pick the one before or after, Enter the next. While the stop has the
    /// highlight, the picked tile has the ring. The tiles wrap to the width they get.
    pub(super) fn render_tiles(
        &self,
        row: &RowCx,
        (id, action): (usize, SettingsAction),
        tiles: impl IntoIterator<Item = Tile>,
        pick: PickTile,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let focused = self.settings_focused(row, action);
        let tiles: Vec<Tile> = tiles.into_iter().collect();
        // With none picked, the first tile takes the ring.
        let ringed = tiles.iter().position(|tile| tile.picked).unwrap_or(0);
        let tiles = tiles.into_iter().enumerate().map(|(index, tile)| {
            let picked = tile.picked;
            let preview = tile
                .preview
                .h(px(TILE_PREVIEW_HEIGHT))
                .w_full()
                .rounded_md()
                .overflow_hidden()
                .bg(material_color(colors.background, 1.0))
                .when(picked, |preview| {
                    preview.border_2().border_color(color(colors.accent))
                })
                .when(!picked, |preview| {
                    preview.border_1().border_color(color(colors.border))
                });
            let label = div()
                .min_w_0()
                .flex()
                .flex_col()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .line_height(px(UI_SMALL_LINE_HEIGHT))
                .child(
                    div()
                        .text_color(if picked {
                            color(modal_text_color(colors.foreground, &colors))
                        } else {
                            muted
                        })
                        .when(picked, |label| label.font_weight(FontWeight::MEDIUM))
                        .child(tile.label),
                )
                .children(
                    tile.unavailable
                        .map(|why| div().text_color(muted).child(why)),
                );
            let (tile_row, pick) = (row.clone(), pick.clone());
            let card = div()
                .id((row.row, id * 16 + index))
                .w(px(TILE_WIDTH))
                .flex()
                .flex_col()
                .gap_1()
                .child(preview)
                .child(label)
                .when_some(tile.unavailable, |card, _| card.opacity(0.55))
                .when(tile.unavailable.is_none(), |card| {
                    card.cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.focus_settings_control(&tile_row, action);
                            pick(this, index, window, cx);
                            cx.stop_propagation();
                            cx.notify();
                        }))
                });
            let ring = focused && index == ringed;
            controls::highlight(ring, card, cx).child(self.settings_focus_anchor(ring, cx))
        });
        div()
            .min_w_0()
            .flex()
            .flex_wrap()
            .gap_3()
            .children(tiles)
            .into_any_element()
    }

    /// A setting whose choice is picture tiles, under its title and `detail`.
    fn render_tile_row(
        &self,
        row: &RowCx,
        action: SettingsAction,
        detail: String,
        tiles: impl IntoIterator<Item = Tile>,
        pick: PickTile,
        cx: &Context<Self>,
    ) -> AnyElement {
        let tiles = self.render_tiles(row, (0, action), tiles, pick, cx);
        self.settings_row(row.title, detail, tiles, true)
    }

    /// A line of text in a tile's drawing, `width` long.
    fn sketch_line(&self, width: f32) -> Div {
        div()
            .flex_none()
            .h(px(3.0))
            .w(px(width))
            .rounded_full()
            .bg(color(self.colors().muted).opacity(0.5))
    }

    /// Terminal text in a tile's drawing.
    fn sketch_text(&self, text: &'static str, tint: impl Into<gpui::Hsla>) -> Div {
        div()
            .flex_none()
            .whitespace_nowrap()
            .font(self.typography.font.clone())
            .text_size(px(9.0))
            .line_height(px(11.0))
            .text_color(tint.into())
            .child(text)
    }

    /// A terminal's cursor in a tile's drawing.
    fn sketch_cursor(tint: impl Into<gpui::Hsla>) -> Div {
        div().flex_none().w(px(5.0)).h(px(9.0)).bg(tint.into())
    }

    /// A pane of a tile's drawing: the surface with two lines of text.
    fn sketch_pane(&self) -> Div {
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .p(px(6.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .bg(color(self.colors().surface))
            .child(self.sketch_line(22.0))
            .child(self.sketch_line(14.0))
    }

    /// A chat in a tile's drawing: its title, a reply, and the message box at the foot.
    fn sketch_chat(&self) -> Div {
        let colors = *self.colors();
        div()
            .flex_1()
            .min_w_0()
            .p(px(5.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .bg(color(colors.surface))
            .child(
                self.sketch_line(30.0)
                    .h(px(4.0))
                    .bg(color(colors.foreground).opacity(0.7)),
            )
            .child(self.sketch_line(44.0))
            .child(self.sketch_line(32.0))
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .h(px(12.0))
                    .w_full()
                    .px(px(2.0))
                    .flex()
                    .items_center()
                    .justify_end()
                    .rounded(px(3.0))
                    .border_1()
                    .border_color(color(colors.border))
                    .bg(material_color(colors.background, 1.0))
                    .child(div().size(px(6.0)).rounded_full().bg(color(colors.accent))),
            )
    }

    /// A shell in a tile's drawing: a command, its output, and the prompt with the
    /// cursor.
    fn sketch_shell(&self) -> Div {
        let colors = *self.colors();
        div()
            .flex_1()
            .min_w_0()
            .p(px(5.0))
            .flex()
            .flex_col()
            .gap(px(3.0))
            .bg(color(colors.surface))
            .child(self.sketch_text("$ ls", color(colors.muted)))
            .child(self.sketch_line(40.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(3.0))
                    .child(self.sketch_text("$", color(colors.accent)))
                    .child(Self::sketch_cursor(color(colors.foreground))),
            )
    }

    /// A shell of `background` in a tile's drawing, its prompt `parts` (text and
    /// color) and the cursor under a line of earlier output.
    fn sketch_prompt(&self, background: u32, parts: &[(&'static str, u32)]) -> Div {
        let text = parts.last().map_or(0xffffff, |(_, tint)| *tint);
        div()
            .flex_1()
            .min_w_0()
            .p(px(5.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .bg(gpui::rgb(background))
            .child(
                div()
                    .flex_none()
                    .h(px(3.0))
                    .w(px(34.0))
                    .rounded_full()
                    .bg(gpui::Hsla::from(gpui::rgb(text)).opacity(0.35)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(1.0))
                    .children(
                        parts
                            .iter()
                            .map(|(part, tint)| self.sketch_text(part, gpui::rgb(*tint))),
                    )
                    .child(Self::sketch_cursor(gpui::rgb(text))),
            )
    }

    /// Windows PowerShell's console in a tile's drawing; `short` for half a tile.
    fn sketch_powershell(&self, short: bool) -> Div {
        let prompt = if short { "PS>" } else { "PS C:\\>" };
        self.sketch_prompt(0x012456, &[(prompt, 0xeeedf0)])
    }

    /// Ubuntu's shell under WSL in a tile's drawing; `short` for half a tile.
    fn sketch_ubuntu(&self, short: bool) -> Div {
        if short {
            self.sketch_prompt(0x300a24, &[("~$", 0xeeeeec)])
        } else {
            self.sketch_prompt(0x300a24, &[("ubuntu", 0x8ae234), (":~$", 0xeeeeec)])
        }
    }

    /// An agent's tile drawing: its own mark.
    pub(super) fn sketch_agent(&self, agent: ChatHarness) -> Div {
        div().flex().items_center().justify_center().child(
            gpui::img(super::chat::harness_mark(agent))
                .flex_none()
                .size(px(AGENT_MARK_SIZE + 4.0)),
        )
    }

    fn render_density_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        use crate::theme::WorkspaceDensity;
        let colors = *self.colors();
        let tiles = WorkspaceDensity::ALL.map(|density| {
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
            Tile {
                label: density.label().into(),
                preview,
                picked: self.config.density == density,
                unavailable: None,
            }
        });
        self.render_tile_row(
            row,
            SettingsAction::Density,
            match self.config.density {
                WorkspaceDensity::Comfy => "Rounded panes with gaps between them, in every window.",
                WorkspaceDensity::Compact => {
                    "Edge-to-edge panes for the most terminal space, in every window."
                }
            }
            .into(),
            tiles,
            Rc::new(|this, index, window, cx| {
                this.apply_density(WorkspaceDensity::ALL[index], window, cx);
            }),
            cx,
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
        let tiles = FocusIndicator::ALL.map(|indicator| {
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
            Tile {
                label: indicator.label().into(),
                preview,
                picked: self.config.focus_indicator == indicator,
                unavailable: None,
            }
        });
        self.render_tile_row(
            row,
            SettingsAction::FocusIndicator,
            match self.config.focus_indicator {
                FocusIndicator::Marker => {
                    "A short accent bar on the focused pane; an outline flashes when focus moves."
                }
                FocusIndicator::Outline => "An accent outline around the focused pane.",
                FocusIndicator::Dim => "Other panes are dimmed.",
                FocusIndicator::None => "Only the cursor, which unfocused panes do not draw.",
            }
            .into(),
            tiles,
            Rc::new(|this, index, _, _| {
                if let Err(error) = this.config.save_focus_indicator(FocusIndicator::ALL[index]) {
                    this.global_error = Some(error);
                }
            }),
            cx,
        )
    }

    /// A size with −, + and Reset, which send `action` with Zoom out, in and reset.
    fn render_size_row(
        &self,
        row: &RowCx,
        size: f32,
        action: fn(Command) -> SettingsAction,
        cx: &Context<Self>,
    ) -> AnyElement {
        let shown = if size.fract() == 0.0 {
            format!("{size:.0}")
        } else {
            format!("{size}")
        };
        let control = div()
            .flex()
            .items_center()
            .gap_1()
            .child(self.settings_control(
                row,
                action(Command::ZoomOut),
                controls::icon_button((row.row, 0_usize), IconName::Minus, "Smaller").header(),
                cx,
            ))
            .child(div().min_w(px(32.0)).text_center().child(shown))
            .child(self.settings_control(
                row,
                action(Command::ZoomIn),
                controls::icon_button((row.row, 2_usize), IconName::Plus, "Larger").header(),
                cx,
            ))
            .child(self.settings_button(row, 1, "Reset", action(Command::ZoomReset), cx))
            .into_any_element();
        self.settings_row(
            row.title,
            row_description(row.row).into(),
            control,
            row.compact,
        )
    }

    fn render_terminal_colors_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let appearance = self.scoped_appearance();
        let theme_locked = self.config.provenance.theme == crate::config::ValueSource::CommandLine;
        let current: SharedString = if appearance.terminal_theme_override {
            self.resolve_theme(&appearance.terminal_theme)
                .label()
                .to_owned()
                .into()
        } else {
            "Following the theme".into()
        };
        let buttons = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(self.settings_value(current).mr_1())
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
                row_description(row.row).into()
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

    /// Restart when the service's task already starts the installed version; otherwise
    /// Repair, which moves the task first.
    fn service_lag_action(&self) -> Option<SettingsAction> {
        let lag = self.updates.snapshot().service_lag?;
        Some(if lag.task_behind {
            SettingsAction::Update(UpdateControl::Repair)
        } else {
            SettingsAction::RestartDaemon
        })
    }

    fn render_service_lag_row(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let (Some(lag), Some(action)) = (
            self.updates.snapshot().service_lag,
            self.service_lag_action(),
        ) else {
            return div().into_any_element();
        };
        let live_processes = self.command_context(cx).live_surface_count;
        let button = controls::text_button(
            (row.row, 0_usize),
            if lag.task_behind {
                "Repair Compi"
            } else {
                "Restart service"
            },
        )
        .header();
        self.settings_row(
            row.title,
            format!(
                "It still runs Compi {}. Restarting it switches to {} and stops {} running terminal{}.",
                lag.running,
                lag.selected,
                live_processes,
                if live_processes == 1 { "" } else { "s" }
            ),
            self.settings_control(
                row,
                action,
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

    /// Every 200 ms: keeps a shown Updates section, the sidebar card and the dot current.
    pub(super) fn poll_updates(&mut self, cx: &mut Context<Self>) {
        let page = self.settings_section_shown(SettingsSection::Updates);
        if page && !self.updates_page_open {
            self.updates.refresh_review();
        }
        self.updates_page_open = page;
        let snapshot = self.updates.snapshot();
        let later = crate::release_notes::seen().update_later;
        let dot = snapshot.shows_dot(later.as_deref());
        let card = snapshot.card(unix_now(), later.as_deref(), |surface| {
            self.shell_label(surface)
        });
        if page || dot != self.update_dot || card != self.update_card {
            self.update_dot = dot;
            self.update_card = card;
            cx.notify();
        }
    }

    /// What the Updates section shows for the current update state.
    fn update_view(&self) -> crate::updates::UpdateView {
        self.updates
            .snapshot()
            .view(unix_now(), |surface| self.shell_label(surface))
    }

    /// The notes the Updates page shows: the update's, else the installed version's.
    fn update_notes(&self) -> Option<crate::release_notes::ReleaseNotes> {
        match self.updates.snapshot().candidate() {
            Some(release) => Some(crate::release_notes::of_release(&release.manifest)),
            None => crate::release_notes::current().cloned(),
        }
    }

    /// More when the notes have details, then Release notes.
    fn update_notes_stops(&self) -> Vec<SettingsAction> {
        let Some(notes) = self.update_notes() else {
            return Vec::new();
        };
        let mut stops = Vec::new();
        if !notes.details.is_empty() {
            stops.push(SettingsAction::Update(UpdateControl::NotesMore));
        }
        stops.push(SettingsAction::Update(UpdateControl::ReleaseNotes));
        stops
    }

    /// An update action from Settings or the sidebar card; restarting asks about unsaved
    /// edits first and consents to end exactly the shells shown.
    pub(super) fn press_update_button(
        &mut self,
        button: crate::updates::UpdateButton,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::updates::UpdateButton;
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
                super::unsaved::AfterUnsaved::Install(self.updates.snapshot().ending_shells()),
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

    fn activate_update_control(
        &mut self,
        control: UpdateControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::updates::Preference;
        match control {
            UpdateControl::Primary(button) => {
                // The state moved on since this button was drawn; the next frame shows it.
                if self.update_view().button == Some(button) {
                    self.press_update_button(button, window, cx);
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
            UpdateControl::NotesMore => {
                self.settings_update_notes_expanded = !self.settings_update_notes_expanded
            }
            UpdateControl::ReleaseNotes => {
                if let Some(notes) = self.update_notes() {
                    self.open_release_notes(&notes);
                }
            }
            UpdateControl::Repair => self.updates.repair(),
            UpdateControl::OpenLog => {
                if let Err(error) = open_update_log() {
                    self.global_error = Some(error);
                }
            }
        }
    }

    /// Compi's own item card: its mark and version, the updater's status, facts and what
    /// updating ends under it, its one primary button, and the download's progress at
    /// the foot while it runs.
    fn render_update_status(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        use crate::updates::UpdateButton;
        let colors = *self.colors();
        let view = self.update_view();
        let primary = match (view.button, view.button_label()) {
            (Some(button), Some(label)) => {
                let action = SettingsAction::Update(UpdateControl::Primary(button));
                let control = controls::text_button((row.row, 0_usize), label).header();
                Some(self.settings_control(
                    row,
                    action,
                    match button {
                        UpdateButton::RestartEndShells => control.danger(),
                        UpdateButton::Cancel => control,
                        _ => control.primary(),
                    },
                    cx,
                ))
            }
            _ => None,
        };
        let accent = color(colors.accent);
        let mark = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| brand::paint(bounds, accent, window),
        )
        .size(px(AGENT_MARK_SIZE));
        let status_tint = if view.failed {
            colors.error
        } else {
            colors.foreground
        };
        let details = std::iter::once(self.item_detail(view.status, status_tint)).chain(
            [view.facts, view.consequence]
                .into_iter()
                .flatten()
                .map(|line| self.item_detail(line, colors.muted)),
        );
        settings_card(&colors)
            .child(
                self.item_card_head(
                    mark,
                    [div()
                        .child(format!("Compi {}", env!("CARGO_PKG_VERSION")))
                        .into_any_element()],
                    details,
                    primary,
                    row.compact,
                ),
            )
            .when_some(view.progress, |card, fraction| {
                card.child(
                    div().px_4().pb_3().child(
                        div()
                            .h(px(4.0))
                            .rounded_full()
                            .bg(color(colors.border))
                            .child(
                                div()
                                    .h(px(4.0))
                                    .rounded_full()
                                    .bg(accent)
                                    .w(gpui::relative(fraction)),
                            ),
                    ),
                )
            })
            .into_any_element()
    }

    /// The update's notes, else the installed version's, under "What's new in <version>",
    /// with More/Less and the release page.
    fn render_update_notes(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let Some(notes) = self.update_notes() else {
            return div().into_any_element();
        };
        let colors = *self.colors();
        let expanded = self.settings_update_notes_expanded;
        let more = if notes.details.is_empty() {
            div().into_any_element()
        } else {
            self.settings_button(
                row,
                0,
                if expanded { "Less" } else { "More" },
                SettingsAction::Update(UpdateControl::NotesMore),
                cx,
            )
        };
        let link = self.settings_button(
            row,
            1,
            "Release notes",
            SettingsAction::Update(UpdateControl::ReleaseNotes),
            cx,
        );
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(settings_group_label(
                format!("What's new in {}", notes.version),
                &colors,
            ))
            .child(
                settings_card(&colors).child(
                    div()
                        .min_w_0()
                        .px_4()
                        .py_3()
                        .child(self.render_release_notes_view(&notes, expanded, more, link)),
                ),
            )
            .into_any_element()
    }

    /// Repair (Windows) and Open update log.
    fn render_update_advanced(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let buttons = div()
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
            ))
            .into_any_element();
        self.settings_row(
            row.title,
            row_description(row.row).into(),
            buttons,
            row.compact,
        )
    }

    /// The command whose shortcut the Settings pane `pane_id` is recording, while that
    /// shortcut keeps the highlight there.
    fn recording_shortcut(&self, pane_id: &PaneId) -> Option<usize> {
        let (pane, command) = self.settings.recording.as_ref()?;
        let focus = self.settings.focus.as_ref()?;
        (pane == pane_id
            && focus.host == SettingsHost::Pane(pane_id.clone())
            && focus.stop
                == Stop::Control {
                    row: COMMANDS_ROW,
                    action: SettingsAction::Shortcut(*command),
                })
        .then_some(*command)
    }

    /// The command list's controls for `query`, in the order it draws them: each
    /// command's shortcut, then its Reset when it has a binding of its own.
    fn command_stops(&self, query: &str) -> Vec<SettingsAction> {
        command_groups(query)
            .into_iter()
            .flat_map(|(_, group)| group)
            .flat_map(|index| {
                let custom = self
                    .config
                    .keybindings
                    .contains_key(commands::REGISTRY[index].id);
                [
                    Some(SettingsAction::Shortcut(index)),
                    custom.then_some(SettingsAction::ResetShortcut(index)),
                ]
            })
            .flatten()
            .collect()
    }

    /// Starts recording `command`'s shortcut in the highlighted Settings pane, or stops
    /// when it already records it.
    fn toggle_shortcut_recording(&mut self, command: usize) {
        let Some(pane) = self.highlighted_pane() else {
            return;
        };
        self.settings.shortcut_notice = None;
        self.settings.recording = if self.settings.recording == Some((pane.clone(), command)) {
            None
        } else {
            Some((pane, command))
        };
    }

    /// A key pressed while `command`'s shortcut records: Escape stops, Backspace or
    /// Delete removes the shortcut, and a key with Ctrl, Alt or Cmd (or a function
    /// key) becomes it. A key that cannot be a shortcut says why and keeps recording.
    fn record_shortcut(
        &mut self,
        command: usize,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = &key.modifiers;
        let plain = !modifiers.control && !modifiers.alt && !modifiers.platform && !modifiers.shift;
        if matches!(
            key.key.as_str(),
            "shift" | "control" | "alt" | "platform" | "function" | "capslock"
        ) {
            return;
        }
        match key.key.as_str() {
            "escape" if plain => {
                self.settings.recording = None;
                return;
            }
            "backspace" | "delete" if plain => {
                self.settings.recording = None;
                self.change_shortcut(command, Some(String::new()), window, cx);
                return;
            }
            _ => {}
        }
        match commands::recorded_binding(commands::ShortcutKey {
            key: &key.key,
            control: modifiers.control,
            shift: modifiers.shift,
            alt: modifiers.alt,
            command: modifiers.platform,
        }) {
            Ok(binding) => {
                self.settings.recording = None;
                self.change_shortcut(command, Some(binding), window, cx);
            }
            Err(reason) => {
                self.settings.shortcut_notice = Some((command, format!("{reason}.")));
            }
        }
    }

    /// Gives `command` the shortcut `binding` (`""` for none), or with `None` its
    /// platform default. Another command holding the same keys loses them, and the
    /// notice under `command` says so; Reset gives that command its own back.
    fn change_shortcut(
        &mut self,
        command: usize,
        binding: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let platform = commands::Platform::current();
        let spec = &commands::REGISTRY[command];
        let reset = binding.is_none();
        // A binding equal to the default is the default: no entry of its own. So is
        // no shortcut for a command without a default.
        let binding = binding.filter(|binding| match spec.shortcut(platform) {
            Some(default) => !commands::same_shortcut(default, binding),
            None => !binding.is_empty(),
        });
        let keys = match &binding {
            Some(binding) if binding.is_empty() => None,
            Some(binding) => Some(binding.clone()),
            None => spec.shortcut(platform).map(str::to_owned),
        };
        let taken_from = keys.as_deref().and_then(|keys| {
            commands::REGISTRY.iter().find(|other| {
                other.id != spec.id
                    && other
                        .configured_shortcut(platform, &self.config.keybindings)
                        .is_some_and(|theirs| commands::same_shortcut(theirs, keys))
            })
        });
        let mut changes = vec![(spec.id, binding)];
        let notice = match (taken_from, &keys) {
            // Reset: the default stays with the command that took it, which wins.
            (Some(other), Some(keys)) if reset => Some(format!(
                "{} is also {}'s shortcut, which takes it.",
                controls::key_label(keys),
                other.label
            )),
            (Some(other), Some(keys)) => {
                // The other command goes back to its own default, or, when the keys
                // are its default, to none.
                let default = other.shortcut(platform);
                let own = default.filter(|default| !commands::same_shortcut(default, keys));
                changes.push((
                    other.id,
                    (default.is_some() && own.is_none()).then(String::new),
                ));
                Some(match own {
                    Some(own) => format!(
                        "{} was {}'s shortcut; that command is back to {}.",
                        controls::key_label(keys),
                        other.label,
                        controls::key_label(own)
                    ),
                    None => format!(
                        "{} was {}'s shortcut; that command has none now.",
                        controls::key_label(keys),
                        other.label
                    ),
                })
            }
            _ => keys.as_deref().and_then(terminal_key_notice),
        };
        if let Err(error) = self.config.save_keybindings(&changes) {
            self.global_error = Some(error);
            return;
        }
        self.settings.shortcut_notice = notice.map(|notice| (command, notice));
        self.share_keybindings(window, cx);
    }

    /// Gives this window's keybindings to Compi's other windows, which read the
    /// configuration only when they open.
    fn share_keybindings(&self, window: &Window, cx: &mut Context<Self>) {
        let this = window.window_handle().window_id();
        for handle in cx.windows() {
            if handle.window_id() == this {
                continue;
            }
            if let Some(app) = handle.downcast::<CompiApp>() {
                let keybindings = self.config.keybindings.clone();
                let _ = app.update(cx, move |app, _, cx| {
                    app.config.keybindings = keybindings;
                    cx.notify();
                });
            }
        }
    }

    /// Settings → Keyboard: the commands the search leaves, in a card per category,
    /// each with its shortcut as a button that records new keys.
    fn render_commands_page(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let SettingsHost::Pane(pane_id) = &row.host else {
            return div().into_any_element();
        };
        let query = self
            .settings
            .panes
            .get(pane_id)
            .map_or("", |pane| pane.query.as_str());
        let recording = self.recording_shortcut(pane_id);
        let platform = commands::Platform::current();
        let groups = command_groups(query).into_iter().map(|(category, group)| {
            let lines = group.into_iter().enumerate().map(|(position, index)| {
                let spec = &commands::REGISTRY[index];
                let custom = self.config.keybindings.contains_key(spec.id);
                let shortcut = spec.configured_shortcut(platform, &self.config.keybindings);
                let label = if recording == Some(index) {
                    "Press keys…".to_owned()
                } else {
                    shortcut.map_or_else(|| "Add shortcut".to_owned(), controls::key_label)
                };
                let mut button = controls::text_button(("shortcut", index), label)
                    .header()
                    .selected(recording == Some(index))
                    .tab_stop(false);
                if shortcut.is_none() && recording != Some(index) {
                    button = button.ghost();
                }
                let reset = custom.then(|| {
                    self.settings_control(
                        row,
                        SettingsAction::ResetShortcut(index),
                        controls::text_button(("shortcut-reset", index), "Reset")
                            .header()
                            .ghost()
                            .tab_stop(false)
                            .detail(match spec.shortcut(platform) {
                                Some(default) => {
                                    format!("Back to {}", controls::key_label(default))
                                }
                                None => "Back to no shortcut".to_owned(),
                            }),
                        cx,
                    )
                });
                let control = div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(reset)
                    .child(self.settings_control(row, SettingsAction::Shortcut(index), button, cx))
                    .into_any_element();
                let (detail, tint) = match &self.settings.shortcut_notice {
                    Some((noticed, notice)) if *noticed == index => (notice.clone(), colors.accent),
                    _ => (
                        spec.command.description().unwrap_or_default().to_owned(),
                        colors.muted,
                    ),
                };
                div()
                    .min_w_0()
                    .px_4()
                    .when(position > 0, |line| {
                        line.border_t_1().border_color(color(colors.border))
                    })
                    .child(self.tinted_settings_row(
                        spec.label.into(),
                        detail,
                        tint,
                        control,
                        row.compact,
                    ))
            });
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_2()
                .child(settings_group_label(category.label(), &colors))
                .child(settings_card(&colors).children(lines))
        });
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_6()
            .children(groups)
            .into_any_element()
    }

    /// `row` drawn for `host`: on its page's heading line when `header`, else as a row,
    /// stacked when `compact`; a page row gets the page's `width`.
    fn draw_settings_row(
        &self,
        host: &SettingsHost,
        row: &Row,
        header: bool,
        compact: bool,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let row_cx = RowCx {
            host: host.clone(),
            row: row.id,
            title: row.title,
            compact: compact && !header,
            header,
        };
        match row.draw {
            Draw::Card(render) => render(self, &row_cx, cx),
            Draw::Page(render) => render(self, &row_cx, width, cx),
        }
    }

    /// The top of `section`'s page: its name and what it covers, with its `header` rows
    /// (the scope switch, Open as pane) at the right end, wrapping under them when the
    /// page is narrow.
    fn render_settings_heading(
        &self,
        host: &SettingsHost,
        section: SettingsSection,
        header: &[&'static Row],
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        div()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_3()
            .gap_y_2()
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(18.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(section.label()),
                    )
                    .child(
                        div()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .line_height(px(UI_SMALL_LINE_HEIGHT))
                            .text_color(color(modal_text_color(colors.muted, &colors)))
                            .child(section_summary(section)),
                    ),
            )
            .when(!header.is_empty(), |heading| {
                heading.child(
                    div().ml_auto().flex().items_center().gap_2().children(
                        header
                            .iter()
                            .map(|row| self.draw_settings_row(host, row, true, false, 0.0, cx)),
                    ),
                )
            })
            .into_any_element()
    }

    /// `rows` in cards: consecutive rows of one section and group share a card, with
    /// hairlines between its rows; a page row draws in place of a card, `width` wide.
    /// A label above a card names its group; in search results (`search`) every card
    /// has one, "Section · Group" or just the section.
    fn render_settings_cards(
        &self,
        host: &SettingsHost,
        rows: &[&'static Row],
        search: bool,
        compact: bool,
        width: f32,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let colors = *self.colors();
        let draw = |row: &Row| self.draw_settings_row(host, row, false, compact, width, cx);
        let mut elements = Vec::new();
        let in_card = |row: &Row| matches!(row.draw, Draw::Card(_));
        // A page row splits its group into runs; only the first is labelled.
        let mut previous: Option<(SettingsSection, Option<&'static str>)> = None;
        for run in rows.chunk_by(|a, b| {
            a.section == b.section && a.group == b.group && in_card(a) && in_card(b)
        }) {
            let first = run[0];
            let continued = previous.replace((first.section, first.group))
                == Some((first.section, first.group));
            let label: Option<SharedString> = match (search, first.group) {
                _ if continued || first.id == PROJECTS_ROW => None,
                (true, Some(group)) => Some(format!("{} · {group}", first.section.label()).into()),
                (true, None) => Some(first.section.label().into()),
                (false, group) => group.map(SharedString::from),
            };
            let card = if in_card(first) {
                settings_card(&colors)
                    .children(run.iter().enumerate().map(|(index, row)| {
                        div()
                            .min_w_0()
                            .px_4()
                            .when(index > 0, |line| {
                                line.border_t_1().border_color(color(colors.border))
                            })
                            .child(draw(row))
                    }))
                    .into_any_element()
            } else {
                draw(first)
            };
            elements.push(match label {
                Some(label) => div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(settings_group_label(label, &colors))
                    .child(card)
                    .into_any_element(),
                None => card,
            });
        }
        elements
    }

    /// A Settings pane: the header line, then the section's page, the search results
    /// or the terminal colors page, and the theme preview's bar at the foot. The terminal
    /// colors page fills the pane, the grid taking the height left in its own scroll;
    /// Appearance leads with the grid, its own scroll a fixed height, and the others are
    /// a column of cards. Each page scrolls as a whole.
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
        // Narrow panes stack their rows and give the margins' room to the page.
        let stacked = width < STACKED_PANE_WIDTH;
        let margin = if stacked { 16.0 } else { 24.0 };
        let page_width = (width - 2.0 * margin).clamp(1.0, PAGE_MAX_WIDTH);
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
            // The grid's frame, padding and scrollbar come off the page width.
            let columns =
                super::catalog::theme_grid_columns(page_width - super::catalog::THEME_GRID_INSET);
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
            let page = div().flex_1().min_w_0().flex().flex_col().gap_6();
            let page = if terminal_page {
                // No `min_h_0`: the page is at least the grid's minimum, so a short pane
                // scrolls the page instead of drawing past its bottom.
                page.child(grid)
            } else {
                // Appearance: the heading, the theme grid, then the other groups' cards.
                let (header, rows): (Vec<_>, Vec<_>) = rows
                    .into_iter()
                    .filter(|row| row.id != THEME_ROW)
                    .partition(|row| row.header);
                page.child(div().flex_none().child(self.render_settings_heading(
                    &host,
                    pane.section,
                    &header,
                    cx,
                )))
                .child(grid)
                .children(
                    self.render_settings_cards(&host, &rows, false, stacked, page_width, cx)
                        .into_iter()
                        .map(|card| div().flex_none().child(card)),
                )
            };
            // Appearance's page scrolls on the pane's scroll, so the keyboard brings its
            // cards into view; the terminal colors page keeps the page it came from's.
            div()
                .relative()
                .min_w_0()
                .flex()
                .min_h_0()
                .flex_1()
                .child(
                    div()
                        .id("settings-theme-page")
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .when(!terminal_page, |scroll| {
                            scroll.track_scroll(&pane.scroll.handle)
                        })
                        .px(px(margin))
                        .py_5()
                        .child(page.w_full().max_w(px(PAGE_MAX_WIDTH)).mx_auto()),
                )
                .when(!terminal_page, |page| {
                    page.child(self.settings_scroll_viewport(host.clone(), false, cx))
                })
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
                self.render_settings_cards(&host, &rows, true, stacked, page_width, cx)
            } else {
                let (header, rows): (Vec<_>, Vec<_>) = rows.into_iter().partition(|row| row.header);
                let mut body = Vec::new();
                if pane.section != SettingsSection::Projects {
                    body.push(self.render_settings_heading(&host, pane.section, &header, cx));
                }
                body.extend(
                    self.render_settings_cards(&host, &rows, false, stacked, page_width, cx),
                );
                body
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
                        .px(px(margin))
                        .py_5()
                        .child(
                            div()
                                .w_full()
                                .max_w(px(PAGE_MAX_WIDTH))
                                .mx_auto()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_6()
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
    /// to all settings; a `full_width` trigger fills its slot. It holds the keyboard
    /// while open; see `quick_appearance_key`.
    pub(super) fn render_quick_appearance(
        &self,
        id: &'static str,
        trigger: AnyElement,
        anchor: Anchor,
        full_width: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let open = self.popup == Some(Popup::QuickAppearance);
        let mut popover = controls::popover(id, trigger, open)
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
        if full_width {
            popover = popover.full_width();
        }
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
                    .w(px(QUICK_APPEARANCE_WIDTH))
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
                                            false,
                                            false,
                                            QUICK_APPEARANCE_WIDTH,
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

/// What Approvals calls `approval`.
pub(super) fn chat_approval_label(approval: ChatApproval) -> &'static str {
    match approval {
        ChatApproval::Harness => "Same as the agent",
        ChatApproval::AlwaysAsk => "Always ask",
        ChatApproval::Write => "Allow edits",
        ChatApproval::Yolo => "Allow everything",
    }
}

/// What `approval` lets the agent do, as Approvals and a chat's picker describe it.
pub(super) fn chat_approval_detail(approval: ChatApproval) -> &'static str {
    match approval {
        ChatApproval::Harness => "The agent's own approval setting",
        ChatApproval::AlwaysAsk => "Ask before anything but reading",
        ChatApproval::Write => "Read and edit; ask before commands",
        ChatApproval::Yolo => "Run every tool without asking",
    }
}

/// The section bar icon of `section`.
fn section_icon(section: SettingsSection) -> IconName {
    match section {
        SettingsSection::General => IconName::Settings,
        SettingsSection::Terminal => IconName::SquareTerminal,
        SettingsSection::Chat => IconName::MessageSquare,
        SettingsSection::Agents => IconName::Bot,
        SettingsSection::Projects => IconName::FolderCog,
        SettingsSection::Appearance => IconName::Palette,
        SettingsSection::Keyboard => IconName::Keyboard,
        SettingsSection::Notifications => IconName::Bell,
        SettingsSection::Monitor => IconName::Gauge,
        SettingsSection::Updates => IconName::CircleArrowDown,
        SettingsSection::Advanced => IconName::SlidersHorizontal,
    }
}

/// Whether a divider leads `section` in the section strip, which runs General |
/// Terminal, Chat, Agents, Projects | Appearance | Keyboard, Notifications | Monitor,
/// Updates, Advanced.
fn opens_strip_group(section: SettingsSection) -> bool {
    matches!(
        section,
        SettingsSection::Terminal
            | SettingsSection::Appearance
            | SettingsSection::Keyboard
            | SettingsSection::Monitor
    )
}

/// A group's label: the line above its card, or a heading inside one. Quiet, so a page
/// reads as its cards.
pub(super) fn settings_group_label(label: impl Into<SharedString>, colors: &ThemeColors) -> Div {
    div()
        .min_w_0()
        .text_size(px(UI_SMALL_TEXT_SIZE))
        .line_height(px(UI_SMALL_LINE_HEIGHT))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color(modal_text_color(colors.muted, colors)))
        .child(label.into())
}

/// Seconds since the Unix epoch, for the update views' "Checked 5 min ago".
pub(super) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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

/// Gives every window a launch setting this one saved, since each launches from its own
/// copy of the configuration.
fn share_launch_change(
    cx: &mut Context<CompiApp>,
    change: impl Fn(&mut compi_protocol::LaunchContext) + 'static,
) {
    cx.defer(move |cx| {
        for handle in cx.windows() {
            if let Some(typed) = handle.downcast::<CompiApp>() {
                let _ = typed.update(cx, |app, _, cx| {
                    if let Ok(launch) = &mut app.config.launch {
                        change(launch);
                    }
                    cx.notify();
                });
            }
        }
    });
}

/// Gives every window a setting this one saved, since each keeps its own copy of the
/// configuration.
pub(super) fn share_config_change(
    cx: &mut Context<CompiApp>,
    change: impl Fn(&mut crate::config::LoadedConfig) + 'static,
) {
    cx.defer(move |cx| {
        for handle in cx.windows() {
            if let Some(typed) = handle.downcast::<CompiApp>() {
                let _ = typed.update(cx, |app, _, cx| {
                    change(&mut app.config);
                    cx.notify();
                });
            }
        }
    });
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
                THEME_ROW,
                "transparency",
                "opacity",
                "blur",
                "window-overrides",
                "ui-font",
                "density",
                "sidebar-width",
                "window-layout",
                "metadata-directory",
                "metadata-process",
                "metadata-git",
                "metadata-dimensions",
                "focus-indicator"
            ]
        );
        assert_eq!(ids("   "), ids(""));
        // Quick Appearance shows the scope, Window and Theme rows of the page.
        let shown: Vec<_> = rows_for(SettingsSection::Appearance, "")
            .filter(|row| quick(row))
            .map(|row| row.id)
            .collect();
        assert_eq!(
            shown,
            [
                "scope",
                THEME_ROW,
                "transparency",
                "opacity",
                "blur",
                "window-overrides"
            ]
        );
        let keyboard: Vec<_> = rows_for(SettingsSection::Keyboard, "")
            .map(|row| row.id)
            .collect();
        assert_eq!(keyboard, [COMMANDS_ROW]);
    }

    #[test]
    fn the_section_strip_runs_in_groups() {
        let strip: Vec<_> = SettingsSection::ALL
            .into_iter()
            .flat_map(|section| {
                opens_strip_group(section)
                    .then_some("|")
                    .into_iter()
                    .chain([section.label()])
            })
            .collect();
        assert_eq!(
            strip.join(" "),
            "General | Terminal Chat Agents Projects | Appearance | Keyboard Notifications \
             | Monitor Updates Advanced"
        );
    }

    #[test]
    fn search_matches_titles_descriptions_and_keywords_across_sections() {
        // Titles, in any section, ignoring case; command names find the command list.
        assert_eq!(
            ids("font"),
            [
                "terminal-font",
                "font-size",
                "file-text-size",
                "custom-typography",
                "chat-text-size",
                "ui-font",
                COMMANDS_ROW
            ]
        );
        assert_eq!(ids("FONT"), ids("font"));
        // The settings themselves, whether or not a command matches too.
        let settings = |query: &str| -> Vec<&'static str> {
            ids(query)
                .into_iter()
                .filter(|id| *id != COMMANDS_ROW)
                .collect()
        };
        // Keywords: the prompt group is one row however many controls it holds.
        assert_eq!(settings("starship"), [PROMPT_ROW]);
        assert_eq!(settings("opacity"), ["transparency", "opacity"]);
        // Descriptions.
        assert_eq!(settings("foreground process"), ["metadata-process"]);
        // Compi's performance tools are Advanced's; Monitor's words find its page.
        assert_eq!(settings("rebuild"), [REBUILD_ROW]);
        assert_eq!(
            settings("fps"),
            ["show-fps", RESOURCES_ROW, DIAGNOSTICS_ROW]
        );
        assert!(ids("network").contains(&MONITOR_ROW));
        // Every agent's program and name find Default agent and the Agents page.
        for agent in ChatHarness::ALL {
            for word in [agent.program(), agent.name()] {
                assert!(settings(word).contains(&"chat-agent"), "{word}");
                assert!(settings(word).contains(&AGENTS_ROW), "{word}");
            }
        }
        // Approvals and OMP's Agents bar are found on the Agents page.
        let section = |id: &str| ROWS.iter().find(|row| row.id == id).map(|row| row.section);
        assert!(settings("approval").contains(&"chat-approval"));
        assert_eq!(settings("sub-agents"), ["chat-agents-bar"]);
        for id in [AGENTS_ROW, "chat-approval", "chat-agents-bar"] {
            assert_eq!(section(id), Some(SettingsSection::Agents), "{id}");
        }
        // A group name finds its rows; so does the group's old name.
        let tab_details = [
            "metadata-directory",
            "metadata-process",
            "metadata-git",
            "metadata-dimensions",
        ];
        assert_eq!(settings("tab details"), tab_details);
        assert_eq!(settings("tab metadata"), tab_details);
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
    fn the_command_list_shows_the_commands_a_search_names() {
        let names = |query: &str| -> Vec<&str> {
            shown_commands(query)
                .into_iter()
                .map(|index| commands::REGISTRY[index].id)
                .collect()
        };
        assert_eq!(names("font size"), ["zoom_in", "zoom_out", "zoom_reset"]);
        // Words of the list itself, matching no command, show every command.
        assert_eq!(names("keybindings").len(), commands::REGISTRY.len());
        assert_eq!(names("").len(), commands::REGISTRY.len());
        assert!(ids("keybindings").contains(&COMMANDS_ROW));
        assert!(!ids("zebra").contains(&COMMANDS_ROW));
        // Grouped in the palette's category order, every shown command once.
        let groups = command_groups("");
        let mut seen: Vec<usize> = groups.iter().flat_map(|(_, group)| group.clone()).collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..commands::REGISTRY.len()).collect::<Vec<_>>());
        let order: Vec<_> = groups.iter().map(|(category, _)| *category).collect();
        assert!(order.windows(2).all(|pair| {
            let at = |category| {
                commands::CommandCategory::ALL
                    .iter()
                    .position(|c| *c == category)
            };
            at(pair[0]) < at(pair[1])
        }));
    }

    #[test]
    fn keyboard_order_is_search_sections_then_each_rows_controls() {
        let toggle = SettingsAction::ToggleConfirmClose;
        let zoom = [
            SettingsAction::FontSize(Command::ZoomOut),
            SettingsAction::FontSize(Command::ZoomReset),
            SettingsAction::FontSize(Command::ZoomIn),
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
        // The scope switch sits on the page's heading line, so it comes first in its
        // sections, ahead of their first group: Theme, and Colors in Advanced.
        for (section, group) in [
            (SettingsSection::Appearance, "Theme"),
            (SettingsSection::Advanced, "Colors"),
        ] {
            let mut rows = rows_for(section, "");
            assert!(rows.next().unwrap().header);
            assert_eq!(rows.next().unwrap().group, Some(group));
        }
        // Monitor's Open as pane leads its page the same way, above Overview.
        let mut monitor = rows_for(SettingsSection::Monitor, "");
        assert!(monitor.next().unwrap().header);
        assert_eq!(monitor.next().unwrap().id, MONITOR_ROW);
        assert_eq!(ROWS.iter().filter(|row| row.header).count(), 3);
    }

    #[test]
    fn rows_sit_under_group_labels_in_a_fixed_order() {
        // No group: General's rows, which are its whole page under its heading, and the
        // page rows that draw their own labels: the project list, the command list,
        // Overview and the release notes.
        let ungrouped: Vec<_> = ROWS
            .iter()
            .filter(|row| !row.header && row.group.is_none())
            .map(|row| row.id)
            .collect();
        assert_eq!(
            ungrouped,
            [
                "new-tab",
                "chat-agent",
                "shell",
                "confirm-close",
                PROJECTS_ROW,
                COMMANDS_ROW,
                MONITOR_ROW,
                "update-notes"
            ]
        );
        assert!(
            ROWS.iter()
                .filter(|row| ungrouped.contains(&row.id))
                .all(|row| row.section == SettingsSection::General
                    || matches!(row.draw, Draw::Page(_)))
        );
        // Each section's labels top to bottom; a page row stands for its own.
        let groups = |section| {
            let mut groups: Vec<&str> = Vec::new();
            for row in rows_for(section, "").filter(|row| !row.header) {
                let group = row.group.unwrap_or(row.title);
                if groups.last() != Some(&group) {
                    groups.push(group);
                }
            }
            groups
        };
        assert_eq!(
            groups(SettingsSection::Appearance),
            ["Theme", "Window", "Text", "Layout", "Tab details", "Focus"]
        );
        assert_eq!(
            groups(SettingsSection::Notifications),
            ["Delivery", "Events", "Sound", "Program status"]
        );
        // General draws no group label; its shell choice is Windows only.
        assert!(rows_for(SettingsSection::General, "").all(|row| row.group.is_none()));
        assert_eq!(
            groups(SettingsSection::Terminal),
            ["Shell", "Font", "Dev servers", "Prompt"]
        );
        assert_eq!(
            groups(SettingsSection::Agents),
            ["Programs", "Permissions", "OMP"]
        );
        assert_eq!(groups(SettingsSection::Chat), ["Messages", "Opening"]);
        assert_eq!(groups(SettingsSection::Projects), ["Projects"]);
        assert_eq!(groups(SettingsSection::Keyboard), ["Commands"]);
        assert_eq!(groups(SettingsSection::Monitor), ["Monitor"]);
        assert_eq!(
            groups(SettingsSection::Updates),
            ["Version", "What's new", "Automatic", "Advanced"]
        );
        assert_eq!(
            groups(SettingsSection::Advanced),
            ["Colors", PERFORMANCE_GROUP, RECOVERY_GROUP]
        );
        // Every page says what it covers, and every row what it does, without
        // repeating its group's label.
        assert!(
            SettingsSection::ALL
                .iter()
                .all(|section| !section_summary(*section).is_empty())
        );
        for row in ROWS {
            assert!(!row.description.is_empty(), "{}", row.id);
            assert_ne!(row.group, Some(row.title), "{}", row.id);
        }
    }

    #[test]
    fn an_agent_card_names_each_side_it_is_installed_on_with_its_version() {
        let install = |linux: Option<&str>, windows: Option<&str>| AgentInstall {
            harness: ChatHarness::Omp,
            linux: linux.map(|version| AgentProgram {
                version: (!version.is_empty()).then(|| version.to_owned()),
            }),
            windows: windows.map(|version| AgentProgram {
                version: (!version.is_empty()).then(|| version.to_owned()),
            }),
        };
        assert_eq!(
            agent_places(
                Some("Ubuntu"),
                Some(&install(Some("19.0.0"), Some("19.0.1")))
            ),
            ["Ubuntu 19.0.0", "Windows 19.0.1"]
        );
        // A program that reports no version is named alone.
        assert_eq!(
            agent_places(Some("Ubuntu"), Some(&install(None, Some("")))),
            ["Windows"]
        );
        assert_eq!(
            agent_places(Some("Debian"), Some(&install(Some(""), None))),
            ["Debian"]
        );
        assert!(agent_places(Some("Ubuntu"), Some(&install(None, None))).is_empty());
        assert!(agent_places(Some("Ubuntu"), None).is_empty());
    }

    #[test]
    fn left_and_right_stop_at_a_choices_ends_and_enter_goes_round() {
        // Left and Right (no wrap) stay at the ends.
        assert_eq!(choice_index(0, 3, false, false), 0);
        assert_eq!(choice_index(1, 3, false, false), 0);
        assert_eq!(choice_index(1, 3, true, false), 2);
        assert_eq!(choice_index(2, 3, true, false), 2);
        // Enter (forward, wrapping) goes from the last to the first.
        assert_eq!(choice_index(2, 3, true, true), 0);
        assert_eq!(choice_index(0, 3, false, true), 2);
        assert_eq!(choice_index(0, 1, true, true), 0);
    }
}
