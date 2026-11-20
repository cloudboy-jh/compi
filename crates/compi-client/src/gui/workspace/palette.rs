//! The palette: one drop-down list of open tabs and their panes, threads, commands,
//! visited projects and the focused pane's files. [`choices`] matches and groups them;
//! the window fills it from its workspace, the daemon's threads (listed off the UI
//! thread as the palette opens), command registry, project history and a file search
//! that runs off the UI thread once typing pauses.
//!
//! Open file…, Find file in project, Jump to project and Threads open the palette scoped
//! to Files, Project files, Projects or Threads: a chip beside the query that its × or
//! Backspace in an empty query removes. A thread row's × or Delete (with nothing typed)
//! deletes the thread. Project files searches every file of the selected tab's project
//! in the daemon and, with nothing typed, lists the project's files opened last.

use super::*;
use crate::chat::{age_label, thread_status, thread_title};
use crate::project_history::ProjectHistory;
use compi_protocol::chat::ThreadInfo;
use compi_protocol::{ContentEnvironment, Project};

/// Open tabs listed before anything is typed.
const RECENT_TABS: usize = 8;
const MATCHED_TABS: usize = 16;
/// Visited projects listed before anything is typed.
const RECENT_PROJECTS: usize = 5;
const MATCHED_PROJECTS: usize = 8;
const SCOPED_PROJECTS: usize = 50;
const MATCHED_FILES: usize = 20;
const SCOPED_FILES: usize = 50;
const MATCHED_THREADS: usize = 8;
const SCOPED_THREADS: usize = 100;
/// How long to wait before asking again while the daemon still reads a project's files.
const INDEXING_RETRY: Duration = Duration::from_millis(700);
/// How long typing must pause before the folder is searched.
const FILE_SEARCH_DELAY: Duration = Duration::from_millis(120);

static NEXT_FILE_SEARCH: AtomicU64 = AtomicU64::new(1);
static NEXT_THREAD_LISTING: AtomicU64 = AtomicU64::new(1);
static NEXT_PROJECT_SEARCH: AtomicU64 = AtomicU64::new(1);

/// The daemon's threads, listed over a connection of their own. It blocks, so it runs
/// off the UI thread.
pub(super) fn fetch_threads(target: &ConnectionTarget) -> Result<Vec<ThreadInfo>, String> {
    (|| -> crate::Result<_> { target.connect()?.list_threads() })()
        .map_err(|error| error.to_string())
}

/// What the palette lists: everything, or only one kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    Projects,
    Files,
    /// Every file of the selected tab's project.
    ProjectFiles,
    Threads,
}

impl Scope {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Projects => "Projects",
            Self::Files => "Files",
            Self::ProjectFiles => "Project files",
            Self::Threads => "Threads",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GroupKind {
    Tabs,
    Threads,
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
    /// A thread, by its chat's id: opened in a pane, or its pane focused.
    Thread(String),
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

/// Where the daemon's threads list stands.
#[derive(Clone, Copy)]
pub(super) enum Threads<'a> {
    Listing,
    Listed(&'a [ThreadInfo]),
    Failed(&'a str),
}

/// Where the project's file search for the current query stands.
#[derive(Clone, Copy)]
pub(super) enum ProjectFiles<'a> {
    /// The selected tab has no project.
    NoProject,
    /// Nothing typed: the project's files opened last, most recent first.
    Recent(&'a [FileHit]),
    Searching,
    /// `indexing`: the daemon is still reading the project's files, so more may match.
    Found {
        hits: &'a [FileHit],
        indexing: bool,
    },
    Failed(&'a str),
}

pub(super) struct Sources<'a> {
    pub tabs: &'a [TabEntry],
    /// Every command the palette offers, in registry order.
    pub commands: &'a [CommandEntry],
    pub projects: &'a ProjectHistory,
    /// Why a new tab cannot open now, which disables project rows.
    pub project_reason: Option<&'a str>,
    /// What Settings → Projects set for a visited folder taken as a project.
    pub project_look: &'a dyn Fn(&str) -> ProjectLook,
    pub files: Files<'a>,
    /// The Project files scope's rows.
    pub project_files: ProjectFiles<'a>,
    /// Newest first, as the daemon lists them.
    pub threads: Threads<'a>,
    /// Now, in milliseconds since the epoch, for the threads' ages.
    pub now_ms: u64,
}

/// What Settings → Projects set for a project the palette lists: the name it goes by,
/// else its folder's, and whether it is pinned, which lists it first.
#[derive(Default)]
pub(super) struct ProjectLook {
    pub name: Option<String>,
    pub pinned: bool,
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

/// The palette's groups for `query`, in order Tabs, Threads, Commands, Projects, Files;
/// only non-empty ones. Threads and Files wait for a query unless scoped. A scope lists
/// its one kind, with a note when it has no rows.
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
    }
    if scope.is_none_or(|scope| scope == Scope::Threads) && (scope.is_some() || !query.is_empty()) {
        let (rows, note) = thread_rows(query, scope.is_some(), sources);
        push_group(&mut groups, GroupKind::Threads, "Threads", rows, note);
    }
    if scope.is_none() {
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
        // An empty query ranks by frecency alone; only typed text is fuzzy-matched, to the
        // folder or the name its project goes by. Pinned projects come first, before the
        // limit.
        let mut found: Vec<(&str, ProjectLook)> = sources
            .projects
            .search_named(query, |path| (sources.project_look)(path).name)
            .into_iter()
            .map(|path| (path, (sources.project_look)(path)))
            .collect();
        found.sort_by_key(|(_, look)| !look.pinned);
        let rows: Vec<Row> = found
            .into_iter()
            .take(limit)
            .map(|(path, look)| Row {
                title: look.name.unwrap_or_else(|| concise_path_title(path)),
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
    if scope == Some(Scope::ProjectFiles) {
        let (rows, note) = project_file_rows(query, sources.project_files);
        push_group(&mut groups, GroupKind::Files, "Project files", rows, note);
    }
    groups
}

/// The Project files group's rows: the file's name, then its folder within the project;
/// and the note in place of rows still to come or none matching, or while the daemon
/// still reads the project's files.
fn project_file_rows(query: &str, files: ProjectFiles) -> (Vec<Row>, Option<String>) {
    let rows = |hits: &[FileHit]| -> Vec<Row> {
        hits.iter()
            .take(SCOPED_FILES)
            .map(|hit| Row {
                title: content::file_name(&hit.path).to_owned(),
                detail: hit
                    .relative
                    .rsplit_once(['/', '\\'])
                    .map_or("", |(folder, _)| folder)
                    .to_owned(),
                reason: None,
                target: Target::File(hit.path.clone()),
            })
            .collect()
    };
    match files {
        ProjectFiles::NoProject => (Vec::new(), Some("This tab has no project yet".to_owned())),
        ProjectFiles::Recent(hits) => {
            let rows = rows(hits);
            let note = rows
                .is_empty()
                .then(|| "No files opened in this project yet; type to search it".to_owned());
            (rows, note)
        }
        ProjectFiles::Searching => (Vec::new(), Some("Searching files…".to_owned())),
        ProjectFiles::Failed(error) => (Vec::new(), Some(format!("Cannot search files: {error}"))),
        ProjectFiles::Found { hits, indexing } => {
            let rows = rows(hits);
            let note = if indexing {
                Some("Still reading this project's files; more may match".to_owned())
            } else {
                rows.is_empty()
                    .then(|| format!("No file matches “{query}”"))
            };
            (rows, note)
        }
    }
}

/// The Threads group's rows for `query` and, when `scoped`, the note in place of rows
/// still to come or none matching.
fn thread_rows(query: &str, scoped: bool, sources: &Sources) -> (Vec<Row>, Option<String>) {
    let threads = match sources.threads {
        Threads::Listing => return (Vec::new(), scoped.then(|| "Listing threads…".to_owned())),
        Threads::Failed(error) => {
            return (
                Vec::new(),
                scoped.then(|| format!("Cannot list threads: {error}")),
            );
        }
        Threads::Listed(threads) => threads,
    };
    let rows: Vec<Row> = threads
        .iter()
        .filter(|thread| {
            query.is_empty()
                || words_match(
                    &format!("{} {}", thread_title(thread), thread.chat.folder),
                    query,
                )
        })
        .take(if scoped {
            SCOPED_THREADS
        } else {
            MATCHED_THREADS
        })
        .map(|thread| Row {
            title: thread_title(thread).to_owned(),
            detail: format!(
                "{} · {} · {}",
                super::chat::folder_label(&thread.chat.folder),
                age_label(thread.updated_ms, sources.now_ms),
                thread_status(thread)
            ),
            reason: None,
            target: Target::Thread(thread.chat.id.clone()),
        })
        .collect();
    let note = (scoped && rows.is_empty()).then(|| {
        if query.is_empty() {
            "No threads yet; New chat starts one".to_owned()
        } else {
            format!("No thread matches “{query}”")
        }
    });
    (rows, note)
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

/// `path` relative to `folder` when inside it, with `/` or Windows `\` separators.
fn relative_path(path: &str, folder: &str) -> String {
    const SEPARATORS: [char; 2] = ['/', '\\'];
    path.strip_prefix(folder.trim_end_matches(SEPARATORS))
        .filter(|rest| rest.starts_with(SEPARATORS))
        .map_or(path, |rest| rest.trim_start_matches(SEPARATORS))
        .to_owned()
}

/// The palette's state beyond its query: the scope chip, the file searches and the
/// threads.
#[derive(Default)]
pub(in crate::gui) struct PaletteState {
    scope: Option<Scope>,
    /// The focused pane's folder when the palette opened, if it can be searched.
    files: Option<FileSearch>,
    /// The selected tab's project when the palette opened, if it has one.
    project: Option<ProjectSearch>,
    threads: ThreadList,
    /// The listing a reply must answer to be shown; earlier ones are stale.
    threads_request: u64,
}

#[derive(Default)]
enum ThreadList {
    #[default]
    Listing,
    Listed(Vec<ThreadInfo>),
    Failed(String),
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

struct ProjectSearch {
    project: Project,
    /// The pane files open beside: the selected tab's focused pane.
    pane_id: Option<PaneId>,
    /// The query `state` answers.
    query: String,
    /// The search a reply must carry to be shown; earlier ones are stale.
    request: Arc<AtomicU64>,
    state: ProjectFileState,
}

enum ProjectFileState {
    Idle,
    Searching,
    Found { hits: Vec<FileHit>, indexing: bool },
    Failed(String),
}

impl CompiApp {
    /// Opens the palette, scoped to one kind or listing everything, and lists the
    /// daemon's threads.
    pub(super) fn open_palette(&mut self, scope: Option<Scope>, cx: &mut Context<Self>) {
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
        let project = self
            .selected_tab()
            .and_then(|tab| tab.project.clone())
            .map(|project| ProjectSearch {
                project,
                pane_id: self.focused_pane().cloned(),
                query: String::new(),
                request: Arc::new(AtomicU64::new(0)),
                state: ProjectFileState::Idle,
            });
        self.palette = PaletteState {
            scope,
            files,
            project,
            ..PaletteState::default()
        };
        self.list_palette_threads(cx);
    }

    /// Lists the daemon's threads off the UI thread; what is listed stays shown until
    /// the reply replaces it, and a reply to an older listing is dropped. The sidebar
    /// takes the listing too.
    fn list_palette_threads(&mut self, cx: &mut Context<Self>) {
        let request = NEXT_THREAD_LISTING.fetch_add(1, Ordering::Relaxed);
        self.palette.threads_request = request;
        let target = self.target.clone();
        cx.spawn(async move |this, cx| {
            let listed = cx
                .background_executor()
                .spawn(async move { fetch_threads(&target) })
                .await;
            this.update(cx, |this, cx| {
                if let Ok(threads) = &listed {
                    this.sidebar_threads_listed(threads.clone());
                }
                if !matches!(this.overlay, Some(Overlay::Palette))
                    || this.palette.threads_request != request
                {
                    cx.notify();
                    return;
                }
                this.palette.threads = match listed {
                    Ok(threads) => ThreadList::Listed(threads),
                    Err(error) => ThreadList::Failed(error),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The listed thread whose chat is `chat`.
    pub(super) fn palette_thread(&self, chat: &str) -> Option<&ThreadInfo> {
        match &self.palette.threads {
            ThreadList::Listed(threads) => threads.iter().find(|thread| thread.chat.id == chat),
            _ => None,
        }
    }

    /// The thread of the palette's selected row, with nothing typed: what Delete deletes.
    pub(super) fn selected_palette_thread(&self, cx: &App) -> Option<String> {
        if !matches!(self.overlay, Some(Overlay::Palette)) || !self.ime_text.is_empty() {
            return None;
        }
        match self
            .palette_choices(cx)
            .0
            .into_iter()
            .nth(self.overlay_index)?
            .action
        {
            ChoiceAction::Thread(chat) => Some(chat),
            _ => None,
        }
    }

    /// Deletes thread `chat` off the UI thread, then lists the threads again. The daemon
    /// refuses while a pane or view holds the thread; its reason shows.
    pub(super) fn delete_thread(&mut self, chat: String, cx: &mut Context<Self>) {
        let target = self.target.clone();
        cx.spawn(async move |this, cx| {
            let asked = chat.clone();
            let deleted = cx
                .background_executor()
                .spawn(async move {
                    (|| -> crate::Result<()> { target.connect()?.delete_thread(&asked) })()
                        .map_err(|error| error.to_string())
                })
                .await;
            this.update(cx, |this, cx| {
                match deleted {
                    Ok(()) => {
                        if let ThreadList::Listed(threads) = &mut this.palette.threads {
                            threads.retain(|thread| thread.chat.id != chat);
                        }
                        if matches!(this.overlay, Some(Overlay::Palette)) {
                            let count = this.palette_choices(cx).0.len();
                            this.overlay_index = this.overlay_index.min(count.saturating_sub(1));
                            this.list_palette_threads(cx);
                        }
                    }
                    Err(error) => {
                        this.global_error = Some(format!("Cannot delete the thread: {error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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

    /// Searches the folder, or in the Project files scope the project, for the new query
    /// once typing pauses; a reply to an older query is dropped.
    pub(super) fn palette_query_changed(&mut self) {
        self.search_palette_project();
        let wants_files = self.palette.scope.is_none_or(|scope| scope == Scope::Files);
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

    /// The Project files scope's search: once typing pauses, then again every
    /// [`INDEXING_RETRY`] while the daemon still reads the project's files, until the
    /// query changes or the palette closes.
    fn search_palette_project(&mut self) {
        let wanted = self.palette.scope == Some(Scope::ProjectFiles);
        let query = self.ime_text.trim().to_owned();
        let Some(search) = self.palette.project.as_mut() else {
            return;
        };
        if query.is_empty() || !wanted {
            search.request.store(0, Ordering::Relaxed);
            search.query.clear();
            search.state = ProjectFileState::Idle;
            return;
        }
        if search.query == query && !matches!(search.state, ProjectFileState::Idle) {
            return;
        }
        let request = NEXT_PROJECT_SEARCH.fetch_add(1, Ordering::Relaxed);
        search.request.store(request, Ordering::Relaxed);
        search.query = query.clone();
        search.state = ProjectFileState::Searching;
        let latest = search.request.clone();
        let project = search.project.clone();
        let target = self.target.clone();
        let sender = self.event_tx.clone();
        thread::spawn(move || {
            thread::sleep(FILE_SEARCH_DELAY);
            // Only the palette still holding this search wants its answers.
            while latest.load(Ordering::Relaxed) == request && Arc::strong_count(&latest) > 1 {
                let result = (|| -> crate::Result<_> {
                    target
                        .connect()?
                        .search_project_files(project.clone(), query.clone())
                })()
                .map_err(|error| error.to_string());
                let indexing = matches!(result, Ok((_, true)));
                if !sender.send(UiEvent::PaletteProjectFilesSearched { request, result })
                    || !indexing
                {
                    return;
                }
                thread::sleep(INDEXING_RETRY);
            }
        });
    }

    pub(super) fn palette_project_files_searched(
        &mut self,
        request: u64,
        result: Result<(Vec<String>, bool), String>,
    ) {
        let open = matches!(self.overlay, Some(Overlay::Palette));
        let Some(search) = self
            .palette
            .project
            .as_mut()
            .filter(|search| search.request.load(Ordering::Relaxed) == request)
        else {
            return;
        };
        if !open {
            // Stops asking again while the daemon reads the project.
            search.request.store(0, Ordering::Relaxed);
            return;
        }
        search.state = match result {
            Ok((paths, indexing)) => ProjectFileState::Found {
                hits: paths
                    .into_iter()
                    .map(|path| FileHit {
                        relative: relative_path(&path, &search.project.root),
                        path,
                    })
                    .collect(),
                indexing,
            },
            Err(error) => ProjectFileState::Failed(error),
        };
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
                        Target::Thread(chat) => ChoiceAction::Thread(chat),
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
        let threads = match &self.palette.threads {
            ThreadList::Listing => Threads::Listing,
            ThreadList::Listed(threads) => Threads::Listed(threads),
            ThreadList::Failed(error) => Threads::Failed(error),
        };
        let recent: Vec<FileHit> = match &self.palette.project {
            Some(search) if matches!(search.state, ProjectFileState::Idle) => self
                .state
                .recent_project_files(&search.project)
                .iter()
                .map(|path| FileHit {
                    relative: relative_path(path, &search.project.root),
                    path: path.clone(),
                })
                .collect(),
            _ => Vec::new(),
        };
        let project_files = match self.palette.project.as_ref().map(|search| &search.state) {
            None => ProjectFiles::NoProject,
            Some(ProjectFileState::Idle) => ProjectFiles::Recent(&recent),
            Some(ProjectFileState::Searching) => ProjectFiles::Searching,
            Some(ProjectFileState::Found { hits, indexing }) => ProjectFiles::Found {
                hits,
                indexing: *indexing,
            },
            Some(ProjectFileState::Failed(error)) => ProjectFiles::Failed(error),
        };
        choices(
            &self.ime_text,
            self.palette.scope,
            &Sources {
                tabs: &tabs,
                commands: &commands,
                projects: &self.state.project_history,
                project_reason: Command::NewTab.disabled_reason(&context),
                project_look: &|path| {
                    let project = self.visited_project(path);
                    ProjectLook {
                        name: self
                            .project_settings(&project)
                            .and_then(|settings| settings.name.clone()),
                        pinned: self.project_pinned(&project),
                    }
                },
                files,
                project_files,
                threads,
                now_ms: crate::chat::now_ms(),
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
                // The workspace, and the tab's group, give the tab its context.
                let context = match tab.group.as_ref().and_then(|id| session.group(id)) {
                    Some(group) => {
                        format!(
                            "{} · {}",
                            session.label,
                            super::tab_groups::group_title(group)
                        )
                    }
                    None => session.label.clone(),
                };
                let mut haystack = format!("{context} {label}");
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
                        context
                    } else {
                        format!("{context} · {status}")
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

    /// Opens a found file in the editor beside the pane the palette searched from; a
    /// project's file beside the selected tab's focused pane, rooted at the project.
    pub(super) fn open_palette_file(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette.scope == Some(Scope::ProjectFiles) {
            let Some(search) = self.palette.project.as_ref() else {
                return;
            };
            let Some(pane_id) = search.pane_id.clone() else {
                self.global_error = Some("Select a pane first".into());
                return;
            };
            let project = search.project.clone();
            self.open_content_file(
                &pane_id,
                project.environment,
                Some(project.root),
                Some(path),
                None,
                false,
                window,
                cx,
            );
            return;
        }
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

    /// The project whose icon leads a palette row: a tab's (and its panes'), a thread's,
    /// or a visited folder taken as a project's root. `None` for rows without one.
    pub(super) fn palette_row_project(&self, action: &ChoiceAction) -> Option<Option<Project>> {
        match action {
            ChoiceAction::Tab(tab_id) | ChoiceAction::Pane(tab_id, _) => Some(
                self.workspace
                    .as_ref()?
                    .sessions
                    .iter()
                    .flat_map(|session| &session.tabs)
                    .find(|tab| &tab.id == tab_id)?
                    .project
                    .clone(),
            ),
            ChoiceAction::Thread(chat) => Some(self.palette_thread(chat)?.project.clone()),
            ChoiceAction::Project(path) => Some(Some(self.visited_project(path))),
            _ => None,
        }
    }

    /// A visited folder as a project: the tabs' project rooted there, else one in the
    /// environment the folder's syntax names. A Linux folder on a local Windows host is
    /// in WSL, in the distribution of the window's WSL terminals.
    fn visited_project(&self, path: &str) -> Project {
        let workspace = self.workspace.as_ref();
        if let Some(project) = workspace
            .into_iter()
            .flat_map(|workspace| &workspace.sessions)
            .flat_map(|session| &session.tabs)
            .filter_map(|tab| tab.project.as_ref())
            .find(|project| project.root == path)
        {
            return project.clone();
        }
        let distribution = (path.starts_with('/') && self.windows_paths(&ContentEnvironment::Host))
            .then(|| {
                workspace?
                    .surfaces
                    .iter()
                    .find_map(|surface| surface.wsl_distribution())
            })
            .flatten();
        Project {
            environment: distribution.map_or(ContentEnvironment::Host, |distribution| {
                ContentEnvironment::Wsl {
                    distribution: distribution.to_owned(),
                }
            }),
            root: path.to_owned(),
        }
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
            project_look: &|_| ProjectLook::default(),
            files: Files::Found(&hits),
            threads: Threads::Listing,
            project_files: ProjectFiles::NoProject,
            now_ms: 0,
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
                project_look: &|_| ProjectLook::default(),
                files: Files::Searching,
                threads: Threads::Listing,
                project_files: ProjectFiles::NoProject,
                now_ms: 0,
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
    fn pinned_projects_lead_the_list_under_the_name_they_go_by() {
        let projects = history(&["/a", "/b", "/c", "/d", "/e", "/f", "/g"]);
        let look = |path: &str| ProjectLook {
            name: (path == "/a").then(|| "Alpha".to_owned()),
            pinned: path == "/a" || path == "/c",
        };
        let list = |query: &str| -> Vec<(String, String)> {
            let groups = choices(
                query,
                Some(Scope::Projects),
                &Sources {
                    tabs: &[],
                    commands: &[],
                    projects: &projects,
                    project_reason: None,
                    project_look: &look,
                    files: Files::Unavailable,
                    threads: Threads::Listing,
                    project_files: ProjectFiles::NoProject,
                    now_ms: 0,
                },
            );
            groups[0]
                .rows
                .iter()
                .map(|row| (row.title.clone(), row.detail.clone()))
                .take(3)
                .collect()
        };
        let row = |title: &str, path: &str| (title.to_owned(), path.to_owned());
        // The pinned first, the least recent one too, in the search's order.
        assert_eq!(
            list(""),
            [row("c", "/c"), row("Alpha", "/a"), row("g", "/g")]
        );
        // A renamed project matches by its name as well as its folder.
        assert_eq!(list("alph"), [row("Alpha", "/a")]);
        assert_eq!(list("/a"), [row("Alpha", "/a")]);
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
                project_look: &|_| ProjectLook::default(),
                files: Files::Unavailable,
                threads: Threads::Listing,
                project_files: ProjectFiles::NoProject,
                now_ms: 0,
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
                project_look: &|_| ProjectLook::default(),
                files: Files::Unavailable,
                threads: Threads::Listing,
                project_files: ProjectFiles::NoProject,
                now_ms: 0,
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
            project_look: &|_| ProjectLook::default(),
            files,
            threads: Threads::Listing,
            project_files: ProjectFiles::NoProject,
            now_ms: 0,
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
            project_look: &|_| ProjectLook::default(),
            files,
            threads: Threads::Listing,
            project_files: ProjectFiles::NoProject,
            now_ms: 0,
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
                        project_look: &|_| ProjectLook::default(),
                        files: Files::Unavailable,
                        threads: Threads::Listing,
                        project_files: ProjectFiles::NoProject,
                        now_ms: 0,
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

    fn thread(
        id: &str,
        title: Option<&str>,
        folder: &str,
        updated_ms: u64,
        activity: compi_protocol::chat::ThreadActivity,
        open: bool,
    ) -> ThreadInfo {
        let mut chat = crate::chat::new_chat(
            compi_protocol::chat::ChatHarness::Omp,
            ContentEnvironment::Host,
            folder.into(),
        );
        chat.id = id.into();
        ThreadInfo {
            chat,
            title: title.map(str::to_owned),
            created_ms: 0,
            updated_ms,
            activity,
            project: None,
            open,
        }
    }

    #[test]
    fn threads_match_a_query_or_fill_their_scope() {
        use compi_protocol::chat::ThreadActivity;
        let now = 100 * 3_600_000;
        let threads = [
            thread(
                "c1",
                Some("Fix the build"),
                "/home/me/proj/compi",
                now - 5 * 60_000,
                ThreadActivity::Working,
                true,
            ),
            thread(
                "c2",
                None,
                "/work/api",
                now - 2 * 3_600_000,
                ThreadActivity::Idle,
                false,
            ),
        ];
        let tabs = [tab("t1", "api server", false)];
        let projects = ProjectHistory::default();
        let sources = |threads| Sources {
            tabs: &tabs,
            commands: &[],
            projects: &projects,
            project_reason: None,
            project_look: &|_| ProjectLook::default(),
            files: Files::Unavailable,
            threads,
            project_files: ProjectFiles::NoProject,
            now_ms: now,
        };
        let listed = sources(Threads::Listed(&threads));
        // Unscoped, threads wait for a query and follow the tabs.
        assert!(!kinds(&choices("", None, &listed)).contains(&GroupKind::Threads));
        let groups = choices("build", None, &listed);
        assert_eq!(kinds(&groups), [GroupKind::Threads]);
        assert_eq!(groups[0].rows[0].target, Target::Thread("c1".into()));
        assert_eq!(groups[0].rows[0].title, "Fix the build");
        assert_eq!(groups[0].rows[0].detail, "…/proj/compi · 5m · Working");
        let groups = choices("api", None, &listed);
        assert_eq!(kinds(&groups), [GroupKind::Tabs, GroupKind::Threads]);
        assert!(groups[1].note.is_none());
        // Still listing, an unscoped query shows no threads and no note.
        assert_eq!(
            kinds(&choices("api", None, &sources(Threads::Listing))),
            [GroupKind::Tabs]
        );

        // The scope lists every thread, newest first, with nothing typed.
        let groups = choices("", Some(Scope::Threads), &listed);
        assert_eq!(kinds(&groups), [GroupKind::Threads]);
        assert_eq!(
            groups[0]
                .rows
                .iter()
                .map(|row| (row.title.as_str(), row.detail.as_str()))
                .collect::<Vec<_>>(),
            [
                ("Fix the build", "…/proj/compi · 5m · Working"),
                ("New thread", "/work/api · 2h · Ended"),
            ]
        );
        assert!(groups[0].note.is_none());
        let note = |query, threads| {
            let groups = choices(query, Some(Scope::Threads), &sources(threads));
            assert_eq!(kinds(&groups), [GroupKind::Threads]);
            assert!(groups[0].rows.is_empty());
            groups[0].note.clone().unwrap()
        };
        assert_eq!(
            note("zzz", Threads::Listed(&threads)),
            "No thread matches “zzz”"
        );
        assert_eq!(
            note("", Threads::Listed(&[])),
            "No threads yet; New chat starts one"
        );
        assert_eq!(note("", Threads::Listing), "Listing threads…");
        assert_eq!(
            note("", Threads::Failed("daemon gone")),
            "Cannot list threads: daemon gone"
        );
    }

    #[test]
    fn project_files_list_recent_files_then_matches_with_notes() {
        let tabs = [tab("t1", "compi build", false)];
        let commands = commands();
        let projects = history(&["/work/compi"]);
        let sources = |project_files| Sources {
            tabs: &tabs,
            commands: &commands,
            projects: &projects,
            project_reason: None,
            project_look: &|_| ProjectLook::default(),
            files: Files::Unavailable,
            project_files,
            threads: Threads::Listing,
            now_ms: 0,
        };
        let scoped = |query, project_files| {
            let groups = choices(query, Some(Scope::ProjectFiles), &sources(project_files));
            assert_eq!(kinds(&groups), [GroupKind::Files]);
            let rows = groups[0]
                .rows
                .iter()
                .map(|row| (row.title.clone(), row.detail.clone()))
                .collect::<Vec<_>>();
            (rows, groups[0].note.clone())
        };
        let recent = [
            FileHit {
                path: "/work/compi/crates/client/src/gui.rs".into(),
                relative: "crates/client/src/gui.rs".into(),
            },
            FileHit {
                path: "/work/compi/README.md".into(),
                relative: "README.md".into(),
            },
        ];
        // Nothing typed: the project's files opened last, by name then folder.
        assert_eq!(
            scoped("", ProjectFiles::Recent(&recent)),
            (
                vec![
                    ("gui.rs".to_owned(), "crates/client/src".to_owned()),
                    ("README.md".to_owned(), String::new()),
                ],
                None
            )
        );
        assert_eq!(
            scoped("", ProjectFiles::Recent(&[])).1.as_deref(),
            Some("No files opened in this project yet; type to search it")
        );
        assert_eq!(
            scoped("", ProjectFiles::NoProject),
            (Vec::new(), Some("This tab has no project yet".to_owned()))
        );
        assert_eq!(
            scoped("gui", ProjectFiles::Searching).1.as_deref(),
            Some("Searching files…")
        );
        // While the daemon still reads the project, matches so far show with a note.
        let (rows, note) = scoped(
            "gui",
            ProjectFiles::Found {
                hits: &recent[..1],
                indexing: true,
            },
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            note.as_deref(),
            Some("Still reading this project's files; more may match")
        );
        assert_eq!(
            scoped(
                "zzz",
                ProjectFiles::Found {
                    hits: &[],
                    indexing: false
                }
            )
            .1
            .as_deref(),
            Some("No file matches “zzz”")
        );
        assert_eq!(
            scoped("gui", ProjectFiles::Failed("gone")).1.as_deref(),
            Some("Cannot search files: gone")
        );
        // A file hit opens the file; other scopes and the full list leave project files out.
        let groups = choices(
            "gui",
            Some(Scope::ProjectFiles),
            &sources(ProjectFiles::Recent(&recent)),
        );
        assert_eq!(
            groups[0].rows[0].target,
            Target::File("/work/compi/crates/client/src/gui.rs".into())
        );
        let groups = choices(
            "gui",
            None,
            &sources(ProjectFiles::Found {
                hits: &recent,
                indexing: false,
            }),
        );
        assert!(groups.iter().all(|group| group.label != "Project files"));
    }
}
