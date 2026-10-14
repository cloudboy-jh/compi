//! The palette: one drop-down list of open tabs and their panes, commands, visited
//! projects and the focused pane's files. [`choices`] matches and groups them; the
//! window fills it from its workspace, command registry, project history and a file
//! search that runs off the UI thread once typing pauses.
//!
//! Open file… and Jump to project open the palette scoped to Files or Projects: a chip
//! beside the query that its × or Backspace in an empty query removes.

use super::*;
use crate::project_history::ProjectHistory;
use compi_protocol::ContentEnvironment;

/// Open tabs listed before anything is typed.
const RECENT_TABS: usize = 8;
const MATCHED_TABS: usize = 16;
/// Visited projects listed before anything is typed.
const RECENT_PROJECTS: usize = 5;
const MATCHED_PROJECTS: usize = 8;
const SCOPED_PROJECTS: usize = 50;
const MATCHED_FILES: usize = 20;
const SCOPED_FILES: usize = 50;
/// How long typing must pause before the folder is searched.
const FILE_SEARCH_DELAY: Duration = Duration::from_millis(120);

static NEXT_FILE_SEARCH: AtomicU64 = AtomicU64::new(1);

/// What the palette lists: everything, or only one kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    Projects,
    Files,
}

impl Scope {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Projects => "Projects",
            Self::Files => "Files",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GroupKind {
    Tabs,
    Commands,
    Projects,
    Files,
}

/// What a row does when chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Target {
    Tab(TabId),
    /// A pane of a tab: the tab is selected and the pane focused.
    Pane(TabId, PaneId),
    Command(Command),
    /// A visited directory, opened in a new tab.
    Project(String),
    /// An absolute path in the searched folder's environment, opened in the editor.
    File(String),
}

/// An open tab, or one pane of a tab with several.
pub(super) struct TabEntry {
    pub tab_id: TabId,
    pub pane_id: Option<PaneId>,
    pub title: String,
    pub detail: String,
    /// The words a query is matched against.
    pub haystack: String,
    pub hidden: bool,
}

pub(super) struct CommandEntry {
    pub command: Command,
    pub title: String,
    pub shortcut: String,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileHit {
    /// Absolute, in the folder's environment.
    pub path: String,
    /// Relative to the searched folder.
    pub relative: String,
}

/// Where the file search for the current query stands.
#[derive(Clone, Copy)]
pub(super) enum Files<'a> {
    /// The focused pane has no searchable folder.
    Unavailable,
    /// Nothing to search for yet.
    Idle,
    Searching,
    Found(&'a [FileHit]),
    Failed(&'a str),
}

pub(super) struct Sources<'a> {
    pub tabs: &'a [TabEntry],
    /// Every command the palette offers, in registry order.
    pub commands: &'a [CommandEntry],
    pub projects: &'a ProjectHistory,
    /// Why a new tab cannot open now, which disables project rows.
    pub project_reason: Option<&'a str>,
    pub files: Files<'a>,
}

#[derive(Debug)]
pub(super) struct Row {
    pub title: String,
    pub detail: String,
    pub reason: Option<String>,
    pub target: Target,
}

pub(super) struct Group {
    pub kind: GroupKind,
    pub label: &'static str,
    pub rows: Vec<Row>,
    /// A line in place of rows still to come, or why there are none.
    pub note: Option<String>,
}

/// The palette's groups for `query`, in order Tabs, Commands, Projects, Files; only
/// non-empty ones. A scope lists its one kind, with a note when it has no rows.
pub(super) fn choices(query: &str, scope: Option<Scope>, sources: &Sources) -> Vec<Group> {
    let query = query.trim();
    let mut groups = Vec::new();
    if scope.is_none() {
        let tabs: Vec<Row> = sources
            .tabs
            .iter()
            .filter(|entry| {
                if query.is_empty() {
                    entry.pane_id.is_none() && !entry.hidden
                } else {
                    words_match(&entry.haystack, query)
                }
            })
            .take(if query.is_empty() {
                RECENT_TABS
            } else {
                MATCHED_TABS
            })
            .map(|entry| Row {
                title: entry.title.clone(),
                detail: entry.detail.clone(),
                reason: None,
                target: match &entry.pane_id {
                    Some(pane_id) => Target::Pane(entry.tab_id.clone(), pane_id.clone()),
                    None => Target::Tab(entry.tab_id.clone()),
                },
            })
            .collect();
        push_group(&mut groups, GroupKind::Tabs, "Tabs", tabs, None);
        for category in commands::CommandCategory::ALL {
            let rows: Vec<Row> = sources
                .commands
                .iter()
                .filter(|entry| {
                    entry.command.category() == category
                        && entry.command.spec().matches_query(query)
                })
                .map(|entry| Row {
                    title: entry.title.clone(),
                    detail: entry.shortcut.clone(),
                    reason: entry.reason.clone(),
                    target: Target::Command(entry.command),
                })
                .collect();
            if query.is_empty() {
                push_group(
                    &mut groups,
                    GroupKind::Commands,
                    category_label(category),
                    rows,
                    None,
                );
            } else if !rows.is_empty() {
                match groups.last_mut() {
                    Some(group) if group.kind == GroupKind::Commands => group.rows.extend(rows),
                    _ => push_group(&mut groups, GroupKind::Commands, "Commands", rows, None),
                }
            }
        }
    }
    if scope.is_none_or(|scope| scope == Scope::Projects) {
        let limit = match (scope, query.is_empty()) {
            (Some(_), _) => SCOPED_PROJECTS,
            (None, true) => RECENT_PROJECTS,
            (None, false) => MATCHED_PROJECTS,
        };
        // An empty query ranks by frecency alone; only typed text is fuzzy-matched.
        let rows: Vec<Row> = sources
            .projects
            .search(query)
            .into_iter()
            .take(limit)
            .map(|path| Row {
                title: concise_path_title(path),
                detail: path.to_owned(),
                reason: sources.project_reason.map(str::to_owned),
                target: Target::Project(path.to_owned()),
            })
            .collect();
        let note = (scope.is_some() && rows.is_empty()).then(|| {
            if query.is_empty() {
                "No visited folders yet; Compi lists the folders your shells visit".to_owned()
            } else {
                format!("No visited folder matches “{query}”")
            }
        });
        push_group(&mut groups, GroupKind::Projects, "Projects", rows, note);
    }
    if scope.is_none_or(|scope| scope == Scope::Files) && (scope.is_some() || !query.is_empty()) {
        let limit = if scope.is_some() {
            SCOPED_FILES
        } else {
            MATCHED_FILES
        };
        let (rows, note) = match sources.files {
            Files::Unavailable => (Vec::new(), None),
            Files::Idle => (
                Vec::new(),
                scope
                    .is_some()
                    .then(|| "Type to search this folder's files".to_owned()),
            ),
            Files::Searching => (Vec::new(), Some("Searching files…".to_owned())),
            Files::Failed(error) => (Vec::new(), Some(format!("Cannot search files: {error}"))),
            Files::Found(hits) => {
                let rows: Vec<Row> = hits
                    .iter()
                    .take(limit)
                    .map(|hit| Row {
                        title: content::file_name(&hit.path).to_owned(),
                        detail: hit.relative.clone(),
                        reason: None,
                        target: Target::File(hit.path.clone()),
                    })
                    .collect();
                let note = (scope.is_some() && rows.is_empty())
                    .then(|| format!("No file matches “{query}”"));
                (rows, note)
            }
        };
        push_group(&mut groups, GroupKind::Files, "Files", rows, note);
    }
    groups
}

fn push_group(
    groups: &mut Vec<Group>,
    kind: GroupKind,
    label: &'static str,
    rows: Vec<Row>,
    note: Option<String>,
) {
    if !rows.is_empty() || note.is_some() {
        groups.push(Group {
            kind,
            label,
            rows,
            note,
        });
    }
}

/// A command category's heading while every command is listed; distinct from the
/// palette's own Tabs and Files groups.
fn category_label(category: commands::CommandCategory) -> &'static str {
    use commands::CommandCategory::*;
    match category {
        Tabs => "Tab commands",
        Panes => "Pane commands",
        Files => "File commands",
        Workspaces => "Workspace commands",
        Terminal => "Terminal commands",
        Window => "Window commands",
        Appearance => "Appearance commands",
        System => "System commands",
    }
}

/// Every query word occurs in `haystack`, ignoring case.
fn words_match(haystack: &str, query: &str) -> bool {
    let haystack = haystack.to_lowercase();
    query
        .split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
}

/// `path` relative to `folder` when inside it.
fn relative_path(path: &str, folder: &str) -> String {
    path.strip_prefix(folder.trim_end_matches('/'))
        .filter(|rest| rest.starts_with('/'))
        .map_or(path, |rest| rest.trim_start_matches('/'))
        .to_owned()
}

/// The palette's state beyond its query: the scope chip and the file search.
#[derive(Default)]
pub(in crate::gui) struct PaletteState {
    scope: Option<Scope>,
    /// The focused pane's folder when the palette opened, if it can be searched.
    files: Option<FileSearch>,
}

struct FileSearch {
    pane_id: PaneId,
    environment: ContentEnvironment,
    folder: String,
    surface_id: SurfaceId,
    /// The query `state` answers.
    query: String,
    /// The search a reply must carry to be shown; earlier ones are stale.
    request: Arc<AtomicU64>,
    state: FileState,
}

enum FileState {
    Idle,
    Searching,
    Found(Vec<FileHit>),
    Failed(String),
}

impl CompiApp {
    /// Opens the palette, scoped to one kind or listing everything.
    pub(super) fn open_palette(&mut self, scope: Option<Scope>) {
        self.open_overlay(Overlay::Palette, "");
        let files = self.file_scope().ok().and_then(|scope| {
            Some(FileSearch {
                surface_id: scope.search.ok()?,
                pane_id: scope.pane_id,
                environment: scope.environment,
                folder: scope.folder,
                query: String::new(),
                request: Arc::new(AtomicU64::new(0)),
                state: FileState::Idle,
            })
        });
        self.palette = PaletteState { scope, files };
    }

    pub(super) fn palette_scope(&self) -> Option<Scope> {
        self.palette.scope
    }

    /// Removes the scope chip: the palette lists everything again.
    pub(super) fn clear_palette_scope(&mut self) {
        if self.palette.scope.take().is_some() {
            self.overlay_index = 0;
            self.overlay_scroll.set_offset(point(px(0.0), px(0.0)));
            self.palette_query_changed();
        }
    }

    /// Searches the folder for the new query once typing pauses; a reply to an older
    /// query is dropped.
    pub(super) fn palette_query_changed(&mut self) {
        let wants_files = self.palette.scope != Some(Scope::Projects);
        let query = self.ime_text.trim().to_owned();
        let Some(search) = self.palette.files.as_mut() else {
            return;
        };
        if query.is_empty() || !wants_files {
            search.request.store(0, Ordering::Relaxed);
            search.query.clear();
            search.state = FileState::Idle;
            return;
        }
        if search.query == query && !matches!(search.state, FileState::Idle) {
            return;
        }
        let request = NEXT_FILE_SEARCH.fetch_add(1, Ordering::Relaxed);
        search.request.store(request, Ordering::Relaxed);
        search.query = query.clone();
        search.state = FileState::Searching;
        let latest = search.request.clone();
        let (surface_id, folder) = (search.surface_id.clone(), search.folder.clone());
        let target = self.target.clone();
        let sender = self.event_tx.clone();
        thread::spawn(move || {
            thread::sleep(FILE_SEARCH_DELAY);
            if latest.load(Ordering::Relaxed) != request {
                return;
            }
            let result = (|| -> crate::Result<_> {
                target
                    .connect()?
                    .search_directory(surface_id, folder, query)
            })()
            .map_err(|error| error.to_string());
            sender.send(UiEvent::PaletteFilesSearched { request, result });
        });
    }

    pub(super) fn palette_files_searched(
        &mut self,
        request: u64,
        result: Result<Vec<compi_protocol::SearchEntry>, String>,
    ) {
        if !matches!(self.overlay, Some(Overlay::Palette)) {
            return;
        }
        let Some(search) = self
            .palette
            .files
            .as_mut()
            .filter(|search| search.request.load(Ordering::Relaxed) == request)
        else {
            return;
        };
        search.state = match result {
            Ok(entries) => FileState::Found(
                entries
                    .into_iter()
                    .filter(|entry| !entry.is_directory)
                    .map(|entry| FileHit {
                        relative: relative_path(&entry.path, &search.folder),
                        path: entry.path,
                    })
                    .collect(),
            ),
            Err(error) => FileState::Failed(error),
        };
    }

    /// The palette's rows as overlay choices, and the note under the last group: only
    /// the last group has one (a scope's only group, or Files).
    pub(super) fn palette_choices(
        &self,
        cx: &App,
    ) -> (Vec<Choice>, Option<(&'static str, String)>) {
        let mut groups = self.palette_groups(cx);
        let note = groups
            .last_mut()
            .and_then(|group| Some((group.label, group.note.take()?)));
        let choices = groups
            .into_iter()
            .flat_map(|group| {
                let label = group.label;
                group.rows.into_iter().map(move |row| Choice {
                    title: row.title,
                    detail: row.detail,
                    group: Some(label),
                    reason: row.reason,
                    action: match row.target {
                        Target::Tab(tab_id) => ChoiceAction::Tab(tab_id),
                        Target::Pane(tab_id, pane_id) => ChoiceAction::Pane(tab_id, pane_id),
                        Target::Command(command) => ChoiceAction::Command(command),
                        Target::Project(path) => ChoiceAction::Project(path),
                        Target::File(path) => ChoiceAction::File(path),
                    },
                })
            })
            .collect();
        (choices, note)
    }

    /// The palette's groups for the current query.
    fn palette_groups(&self, cx: &App) -> Vec<Group> {
        let context = self.command_context(cx);
        let commands: Vec<CommandEntry> = commands::REGISTRY
            .iter()
            .filter(|spec| spec.command != Command::OpenPalette)
            .map(|spec| CommandEntry {
                command: spec.command,
                title: self.command_label(spec.command).into(),
                shortcut: self.configured_shortcut(spec.command).unwrap_or("").into(),
                reason: spec.command.disabled_reason(&context).map(str::to_owned),
            })
            .collect();
        let tabs = self.palette_tab_entries();
        let files = match self.palette.files.as_ref().map(|search| &search.state) {
            None => Files::Unavailable,
            Some(FileState::Idle) => Files::Idle,
            Some(FileState::Searching) => Files::Searching,
            Some(FileState::Found(hits)) => Files::Found(hits),
            Some(FileState::Failed(error)) => Files::Failed(error),
        };
        choices(
            &self.ime_text,
            self.palette.scope,
            &Sources {
                tabs: &tabs,
                commands: &commands,
                projects: &self.state.project_history,
                project_reason: Command::NewTab.disabled_reason(&context),
                files,
            },
        )
    }

    /// Every tab, the selected workspace's first, each followed by its panes when it
    /// has several.
    fn palette_tab_entries(&self) -> Vec<TabEntry> {
        let Some(workspace) = &self.workspace else {
            return Vec::new();
        };
        let selected = self
            .state
            .selected_session(workspace)
            .map(|session| &session.id);
        let sessions = workspace
            .sessions
            .iter()
            .filter(|session| Some(&session.id) == selected)
            .chain(
                workspace
                    .sessions
                    .iter()
                    .filter(|session| Some(&session.id) != selected),
            );
        let mut entries = Vec::new();
        for session in sessions {
            for tab in &session.tabs {
                let panes = self.tab_panes(tab);
                let label = self.tab_label(tab);
                let hidden = self.state.hidden_tabs.contains(&tab.id);
                let mut haystack = format!("{} {label}", session.label);
                for pane in &panes {
                    haystack.push(' ');
                    haystack.push_str(&pane.title);
                    if let Some(directory) = &pane.directory {
                        haystack.push(' ');
                        haystack.push_str(directory);
                    }
                }
                let status = self.tab_status(tab);
                entries.push(TabEntry {
                    tab_id: tab.id.clone(),
                    pane_id: None,
                    title: label.clone(),
                    detail: if status.is_empty() {
                        session.label.clone()
                    } else {
                        format!("{} · {status}", session.label)
                    },
                    haystack,
                    hidden,
                });
                if panes.len() < 2 {
                    continue;
                }
                for pane in panes {
                    let haystack = match &pane.directory {
                        Some(directory) => format!("{} {directory}", pane.title),
                        None => pane.title.clone(),
                    };
                    entries.push(TabEntry {
                        tab_id: tab.id.clone(),
                        detail: match &pane.directory {
                            Some(directory) => format!("{label} · {directory}"),
                            None => label.clone(),
                        },
                        pane_id: Some(pane.pane_id),
                        title: pane.title,
                        haystack,
                        hidden,
                    });
                }
            }
        }
        entries
    }

    /// Selects a tab and focuses one of its panes.
    pub(super) fn open_palette_pane(
        &mut self,
        tab_id: TabId,
        pane_id: PaneId,
        window: &mut Window,
    ) {
        if self.selected_tab().is_none_or(|tab| tab.id != tab_id) {
            self.select_terminal(tab_id, true);
            self.rebuild_layout(window, true);
        }
        self.focus_pane(pane_id);
    }

    /// Opens a found file in the editor beside the pane the palette searched from.
    pub(super) fn open_palette_file(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(search) = self.palette.files.as_ref() else {
            return;
        };
        let (pane_id, environment, folder) = (
            search.pane_id.clone(),
            search.environment.clone(),
            search.folder.clone(),
        );
        self.open_content_file(
            &pane_id,
            environment,
            Some(folder),
            Some(path),
            None,
            false,
            window,
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: &str, title: &str, hidden: bool) -> TabEntry {
        TabEntry {
            tab_id: TabId::new(id),
            pane_id: None,
            title: title.into(),
            detail: String::new(),
            haystack: format!("Work {title}"),
            hidden,
        }
    }

    fn pane(tab_id: &str, id: &str, title: &str, directory: &str) -> TabEntry {
        TabEntry {
            tab_id: TabId::new(tab_id),
            pane_id: Some(PaneId::new(id)),
            title: title.into(),
            detail: String::new(),
            haystack: format!("{title} {directory}"),
            hidden: false,
        }
    }

    fn commands() -> Vec<CommandEntry> {
        commands::REGISTRY
            .iter()
            .filter(|spec| spec.command != Command::OpenPalette)
            .map(|spec| CommandEntry {
                command: spec.command,
                title: spec.label.into(),
                shortcut: String::new(),
                reason: None,
            })
            .collect()
    }

    fn history(paths: &[&str]) -> ProjectHistory {
        let mut history = ProjectHistory::default();
        for path in paths {
            history.record(path);
        }
        history
    }

    fn kinds(groups: &[Group]) -> Vec<GroupKind> {
        let mut kinds: Vec<_> = groups.iter().map(|group| group.kind).collect();
        kinds.dedup();
        kinds
    }

    fn hits() -> Vec<FileHit> {
        vec![FileHit {
            path: "/work/compi/src/main.rs".into(),
            relative: "src/main.rs".into(),
        }]
    }

    #[test]
    fn groups_come_in_order_tabs_commands_projects_files() {
        let tabs = [tab("t1", "compi build", false)];
        let commands = commands();
        let projects = history(&["/work/compi", "/work/other"]);
        let hits = hits();
        let sources = Sources {
            tabs: &tabs,
            commands: &commands,
            projects: &projects,
            project_reason: None,
            files: Files::Found(&hits),
        };
        // "compi" names a tab, Quit Compi, a project and (in this search) a file.
        let groups = choices("compi", None, &sources);
        assert_eq!(
            kinds(&groups),
            [
                GroupKind::Tabs,
                GroupKind::Commands,
                GroupKind::Projects,
                GroupKind::Files
            ]
        );
        assert_eq!(groups[1].rows[0].target, Target::Command(Command::Quit));
        let groups = choices("tab", None, &sources);
        assert_eq!(kinds(&groups), [GroupKind::Commands, GroupKind::Files]);
        // With a query, commands are one group.
        assert_eq!(
            groups
                .iter()
                .filter(|group| group.kind == GroupKind::Commands)
                .count(),
            1
        );
        assert_eq!(groups[0].label, "Commands");
        let groups = choices("build", None, &sources);
        assert_eq!(kinds(&groups), [GroupKind::Tabs, GroupKind::Files]);
        assert!(groups.iter().all(|group| group.note.is_none()));
        assert_eq!(
            groups.last().unwrap().rows[0].target,
            Target::File("/work/compi/src/main.rs".into())
        );
        assert_eq!(groups.last().unwrap().rows[0].title, "main.rs");
        assert_eq!(groups.last().unwrap().rows[0].detail, "src/main.rs");
    }

    #[test]
    fn empty_query_lists_open_tabs_every_command_and_recent_projects() {
        let mut tabs: Vec<_> = (0..12)
            .map(|index| tab(&format!("t{index}"), &format!("tab {index}"), index == 0))
            .collect();
        tabs.push(pane("t1", "p1", "server", "/work/api"));
        let commands = commands();
        let projects = history(&["/a", "/b", "/c", "/d", "/e", "/f", "/g"]);
        let groups = choices(
            "  ",
            None,
            &Sources {
                tabs: &tabs,
                commands: &commands,
                projects: &projects,
                project_reason: Some("Select a workspace first"),
                files: Files::Searching,
            },
        );
        assert_eq!(
            kinds(&groups),
            [GroupKind::Tabs, GroupKind::Commands, GroupKind::Projects]
        );
        // Hidden tabs and pane rows wait for a query; at most eight tabs.
        let tab_rows = &groups[0].rows;
        assert_eq!(tab_rows.len(), RECENT_TABS);
        assert_eq!(tab_rows[0].target, Target::Tab(TabId::new("t1")));
        assert!(
            tab_rows
                .iter()
                .all(|row| matches!(row.target, Target::Tab(_)))
        );
        // Every command, under its category.
        let command_groups: Vec<_> = groups
            .iter()
            .filter(|group| group.kind == GroupKind::Commands)
            .collect();
        assert!(command_groups.len() > 1);
        assert_eq!(command_groups[0].label, "Tab commands");
        assert_eq!(
            command_groups
                .iter()
                .map(|group| group.rows.len())
                .sum::<usize>(),
            commands.len()
        );
        // The most recent projects; the reason disables them.
        let projects = groups.last().unwrap();
        assert_eq!(projects.rows.len(), RECENT_PROJECTS);
        assert_eq!(projects.rows[0].target, Target::Project("/g".into()));
        assert_eq!(projects.rows[0].title, "g");
        assert_eq!(
            projects.rows[0].reason.as_deref(),
            Some("Select a workspace first")
        );
    }

    #[test]
    fn a_query_matches_panes_and_hidden_tabs() {
        let tabs = [
            tab("t1", "editor", true),
            pane("t1", "p1", "server", "/work/api"),
            pane("t1", "p2", "shell", "/work/web"),
        ];
        let projects = ProjectHistory::default();
        let groups = choices(
            "api",
            None,
            &Sources {
                tabs: &tabs,
                commands: &[],
                projects: &projects,
                project_reason: None,
                files: Files::Unavailable,
            },
        );
        assert_eq!(kinds(&groups), [GroupKind::Tabs]);
        assert_eq!(
            groups[0]
                .rows
                .iter()
                .map(|row| row.target.clone())
                .collect::<Vec<_>>(),
            [Target::Pane(TabId::new("t1"), PaneId::new("p1"))]
        );
        let groups = choices(
            "editor",
            None,
            &Sources {
                tabs: &tabs,
                commands: &[],
                projects: &projects,
                project_reason: None,
                files: Files::Unavailable,
            },
        );
        assert_eq!(groups[0].rows[0].target, Target::Tab(TabId::new("t1")));
    }

    #[test]
    fn scope_chips_list_one_kind() {
        let tabs = [tab("t1", "compi", false)];
        let commands = commands();
        let projects = history(&["/work/compi", "/work/other"]);
        let hits = hits();
        let sources = |files| Sources {
            tabs: &tabs,
            commands: &commands,
            projects: &projects,
            project_reason: None,
            files,
        };
        let groups = choices("", Some(Scope::Projects), &sources(Files::Found(&hits)));
        assert_eq!(kinds(&groups), [GroupKind::Projects]);
        assert_eq!(groups[0].rows.len(), 2);
        let groups = choices("zzz", Some(Scope::Projects), &sources(Files::Idle));
        assert_eq!(kinds(&groups), [GroupKind::Projects]);
        assert!(groups[0].rows.is_empty());
        assert!(groups[0].note.is_some());

        let groups = choices("", Some(Scope::Files), &sources(Files::Idle));
        assert_eq!(kinds(&groups), [GroupKind::Files]);
        assert_eq!(
            groups[0].note.as_deref(),
            Some("Type to search this folder's files")
        );
        let groups = choices("main", Some(Scope::Files), &sources(Files::Searching));
        assert_eq!(groups[0].note.as_deref(), Some("Searching files…"));
        let groups = choices("main", Some(Scope::Files), &sources(Files::Found(&hits)));
        assert_eq!(kinds(&groups), [GroupKind::Files]);
        assert_eq!(groups[0].rows.len(), 1);
        let groups = choices("main", Some(Scope::Files), &sources(Files::Found(&[])));
        assert!(groups[0].note.is_some());
    }

    #[test]
    fn files_need_a_query_and_a_searchable_folder() {
        let commands = commands();
        let projects = ProjectHistory::default();
        let sources = |files| Sources {
            tabs: &[],
            commands: &commands,
            projects: &projects,
            project_reason: None,
            files,
        };
        // Nothing typed: no file group, even with a pending or finished search.
        for files in [Files::Idle, Files::Searching, Files::Found(&[])] {
            let groups = choices("", None, &sources(files));
            assert!(!kinds(&groups).contains(&GroupKind::Files));
        }
        let groups = choices("qqq", None, &sources(Files::Searching));
        assert_eq!(kinds(&groups), [GroupKind::Files]);
        assert_eq!(groups[0].note.as_deref(), Some("Searching files…"));
        assert!(choices("qqq", None, &sources(Files::Unavailable)).is_empty());
        assert!(choices("qqq", None, &sources(Files::Found(&[]))).is_empty());
    }

    #[test]
    fn empty_and_blank_queries_never_fuzzy_match_projects() {
        // `fuzzy_score` panics on an empty query; reaching these rows proves it was
        // never called with one.
        let projects = history(&["/work/compi", "/work/other"]);
        for query in ["", " ", "\t  "] {
            for scope in [None, Some(Scope::Projects)] {
                let groups = choices(
                    query,
                    scope,
                    &Sources {
                        tabs: &[],
                        commands: &[],
                        projects: &projects,
                        project_reason: None,
                        files: Files::Unavailable,
                    },
                );
                assert_eq!(groups[0].rows.len(), 2, "{query:?} {scope:?}");
            }
        }
    }

    #[test]
    fn found_paths_are_shown_relative_to_the_folder() {
        assert_eq!(
            relative_path("/work/compi/src/a.rs", "/work/compi"),
            "src/a.rs"
        );
        assert_eq!(
            relative_path("/work/compi/src/a.rs", "/work/compi/"),
            "src/a.rs"
        );
        assert_eq!(
            relative_path("/work/compiler/a.rs", "/work/compi"),
            "/work/compiler/a.rs"
        );
        assert_eq!(relative_path("/a.rs", "/"), "a.rs");
    }
}
