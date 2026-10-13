//! A content pane's repository and its diff tabs. While a pane is visible its folder's
//! Git changes are read again every `CHECK_INTERVAL` (`repo_changes`); its sidebar can list
//! them in two sections as `git status` does (Unstaged: the work tree against the index,
//! untracked files included; Staged: the index against `HEAD`), and a change opens as a
//! diff tab beside the file tabs (`open_diff`). An open diff is read again when its change's
//! stamp moves and replaced in place, keeping scroll, context and find. Reading never
//! writes: the daemon reads with Git's optional locks off.
//!
//! Writes are explicit actions only (`git_write`): stage, unstage and discard on list rows,
//! section headers and diff tab headers, a hunk's button in a diff tab, and the commit box
//! above the list. Discarding asks first, naming the files. A write's controls show a
//! spinner until it is answered (`InFlight`). After every write the folder is read again
//! at once, so the list, open diffs and gutters follow.
//!
//! The list has a keyboard selection (`changes_list_key`) kept by what a row shows, so it
//! survives a new listing; it follows the diff that opens.
use super::content::{
    CHECK_INTERVAL, FILE_TAB_HEIGHT, PaneUi, TREE_INDENT, TREE_INSET, file_name, find_content,
    reusable_content, same_folder,
};
use super::diff_view::{DiffSettings, DiffStyle, DiffView, HunkButton, HunkClicked, LayoutMode};
use super::files::FileScope;
use super::tree::join_path;
use super::tree_ui::tree_entry_icon;
use super::*;
use crate::content_path::{repo_relative, work_tree_path};
use crate::diff::{HunkWrite, Side, Whitespace, set_index_lines};
use crate::gui::controls::{self, ChipTone, Control, IconName, icon_button, text_button};
use compi_protocol::{
    ContentEnvironment, ContentTab, GitAction, GitChange, GitChangeKind, GitChanges,
    GitFileContent, GitRevision, GitScope, GitWriteResult, PaneContent,
};
use gpui::{
    AnyElement, ClickEvent, Entity, ScrollStrategy, SharedString, UniformListScrollHandle,
    uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use std::borrow::Cow;
use std::cell::Cell;
use std::collections::BTreeMap;

/// A diff that takes longer than this to read and build shows a loading line.
const LOADING_DELAY: Duration = Duration::from_millis(150);
/// A repository read that takes longer than this shows the refreshing mark.
const REFRESHING_DELAY: Duration = Duration::from_millis(300);
/// A folder outside any work tree is asked again this rarely, so `git init` is noticed
/// without running Git every few seconds for every folder.
const NOT_REPO_INTERVAL: Duration = Duration::from_secs(30);
/// Rough glyph advance of the UI font at micro size, for eliding folders.
const MICRO_GLYPH_WIDTH: f32 = UI_MICRO_TEXT_SIZE * 0.56;
const SMALL_GLYPH_WIDTH: f32 = UI_SMALL_TEXT_SIZE * 0.56;
/// How long a commit's or a refused hunk's line stays under the commit box.
const NOTICE_DURATION: Duration = Duration::from_secs(8);

/// How every Changes list and diff tab of a window shows changes. Not saved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ChangesSettings {
    pub diff: DiffSettings,
    /// The list groups changes by folder.
    pub tree: bool,
}

/// What a content pane's folder sidebar, or its folder view, lists. Client-local.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SidebarMode {
    #[default]
    Files,
    Changes,
}

/// The Git changes of a content pane's folder and the Changes list built from them.
pub(super) struct RepoStatus {
    /// The folder read: the pane's root.
    root: String,
    listing: Listing,
    /// Files and lines across the listing: distinct paths, added, removed.
    totals: (usize, u64, u64),
    /// A listing request is out, since then.
    reading: Option<Instant>,
    /// The last listing request finished then.
    read_at: Option<Instant>,
    /// The refreshing mark shows for the request out.
    marked: bool,
    /// The latest read failed; the listing shown is the one before.
    refresh_error: Option<String>,
    folds: Folds,
    /// The list as drawn: sections, folders and files not folded away.
    rows: Vec<ListRow>,
    /// Every change in list order, folded or not, for stepping through files.
    order: Vec<usize>,
    scroll: UniformListScrollHandle,
    /// The list's width at the last draw, for eliding folder names.
    list_width: Cell<f32>,
    /// A diff asked for before the folder was read: path, scope and line for `open_diff`.
    pub(super) pending_open: Option<(String, Option<GitScope>, Option<u32>)>,
    /// The folder tree's marks for the latest listing, by tree path; see `tree_marks`.
    marks: HashMap<String, TreeMark>,
    /// The commit box above the list; built by the first frame that draws the list.
    commit: Option<CommitBox>,
    /// Stage, unstage and discard writes sent and not answered, for their spinners.
    in_flight: Vec<InFlight>,
    /// Counts the writes sent, to tell their answers apart.
    writes: u64,
    /// A write finished while a listing read was out: read again once it lands.
    reread: bool,
    /// What the last write left under the commit box.
    outcome: Option<Outcome>,
    /// The keyboard selection: what the row shows, and where it was last.
    selected: Option<(RowKey, usize)>,
}

/// The commit message being written, and the Amend toggle.
struct CommitBox {
    summary: Entity<InputState>,
    body: Entity<TextareaState>,
    /// The summary holds more than whitespace.
    has_summary: bool,
    amend: bool,
    /// What the box held before Amend loaded `HEAD`'s message; back when Amend goes off.
    draft: Option<(String, String)>,
    /// A commit is on its way.
    committing: bool,
    _events: Subscription,
}

/// What the last write left under the commit box.
enum Outcome {
    /// A quiet line that goes after `NOTICE_DURATION`: a commit made, the index moved.
    Notice(String, Instant),
    /// Git's message for a failed write, kept until the next write.
    Error(String),
}

/// What the answer to a write means for the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriteKind {
    Index,
    Commit,
    LastMessage,
}

/// What sent a write, for what shows it running.
enum WriteOrigin {
    /// A list row, a diff header, the commit box or a confirmation.
    Control,
    /// A section's Stage all or Unstage all.
    Section,
    /// A hunk's button in this diff tab.
    Hunk(ContentTab),
}

/// A stage, unstage or discard sent and not answered: the controls that sent it, and the
/// rows of its paths, show a spinner meanwhile.
struct InFlight {
    id: u64,
    discard: bool,
    /// The section its paths leave.
    scope: GitScope,
    paths: Vec<String>,
    /// It moves every change of its section.
    section: bool,
}

impl InFlight {
    fn of(id: u64, action: &GitAction, section: bool) -> Option<Self> {
        let (discard, scope, paths) = match action {
            GitAction::Stage { paths } => (false, GitScope::Unstaged, paths.clone()),
            GitAction::Unstage { paths } => (false, GitScope::Staged, paths.clone()),
            GitAction::Discard { paths, untracked } => (
                true,
                GitScope::Unstaged,
                paths.iter().chain(untracked).cloned().collect(),
            ),
            _ => return None,
        };
        Some(Self {
            id,
            discard,
            scope,
            paths,
            section,
        })
    }

    fn names(&self, scope: GitScope, path: &str) -> bool {
        self.scope == scope && self.paths.iter().any(|named| named == path)
    }
}

impl CommitBox {
    /// Puts `summary` and `body` in the fields.
    fn fill(&self, summary: &str, body: &str, window: &mut Window, cx: &mut App) {
        self.summary
            .update(cx, |input, cx| input.set_value(summary, window, cx));
        self.body
            .update(cx, |input, cx| input.set_value(body, window, cx));
    }

    /// Amend off: the draft set aside for `HEAD`'s message comes back.
    fn restore_draft(&mut self, window: &mut Window, cx: &mut App) {
        self.amend = false;
        let (summary, body) = self.draft.take().unwrap_or_default();
        self.has_summary = !summary.trim().is_empty();
        self.fill(&summary, &body, window, cx);
    }
}

/// What the folder tree shows beside a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TreeMark {
    /// A changed file: its status, and its repository path for opening its diff.
    File { kind: GitChangeKind, path: String },
    /// A folder holding changes.
    Folder,
}

enum Listing {
    Reading,
    NotRepo,
    Failed(String),
    Ready(GitChanges),
}

/// The diff a diff tab shows.
#[derive(Default)]
pub(super) struct DiffTab {
    /// The diff on screen, at its change's stamp.
    shown: Option<Shown>,
    /// The diff being read or built at a newer stamp.
    pending: Option<Pending>,
    /// Reading the sides at this stamp failed.
    failed: Option<(String, String)>,
    /// Counts loads, so a superseded one is dropped when it finishes.
    loads: u64,
}

struct Shown {
    stamp: String,
    view: Entity<DiffView>,
    /// Hunk buttons of the view.
    _events: Subscription,
}

struct Pending {
    stamp: String,
    load: u64,
    started: Instant,
    /// Built and waiting until it can draw; the first subscription promotes it then, the
    /// second hears its hunk buttons.
    view: Option<(Entity<DiffView>, Subscription, Subscription)>,
}

#[derive(Default)]
struct Folds {
    sections: HashSet<GitScope>,
    /// Folded folders by section and folder path.
    folders: HashSet<(GitScope, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ListRow {
    Section {
        scope: GitScope,
        count: usize,
        collapsed: bool,
    },
    /// A folder in tree mode; `label` joins a chain of folders that hold only the next.
    Folder {
        scope: GitScope,
        path: String,
        label: String,
        depth: usize,
        collapsed: bool,
    },
    /// A change, by index into the listing.
    File { change: usize, depth: usize },
}

/// A list row by what it shows rather than where, so a new listing or a fold keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RowKey {
    Section(GitScope),
    Folder(GitScope, String),
    File(GitScope, String),
}

/// What Left or Right does on a selected row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sideways {
    /// Fold or unfold the selected section or folder.
    Toggle,
    /// Select this row: the one above (Left) or the first inside (Right).
    Select(usize),
    Stay,
}

impl RepoStatus {
    pub(super) fn new(root: String) -> Self {
        Self {
            root,
            listing: Listing::Reading,
            totals: (0, 0, 0),
            reading: None,
            read_at: None,
            marked: false,
            refresh_error: None,
            folds: Folds::default(),
            rows: Vec::new(),
            order: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            list_width: Cell::new(0.0),
            pending_open: None,
            marks: HashMap::new(),
            commit: None,
            in_flight: Vec::new(),
            writes: 0,
            reread: false,
            outcome: None,
            selected: None,
        }
    }

    /// The latest listing; `None` outside a work tree or before the first read.
    pub(super) fn listing(&self) -> Option<&GitChanges> {
        match &self.listing {
            Listing::Ready(listing) => Some(listing),
            _ => None,
        }
    }

    fn changes(&self) -> &[GitChange] {
        self.listing()
            .map_or(&[], |listing| listing.changes.as_slice())
    }

    /// The change the tab of absolute `file` shows in Diff mode against `scope`, if the
    /// listing has it.
    pub(super) fn change(&self, file: &str, scope: GitScope) -> Option<&GitChange> {
        let listing = self.listing()?;
        let path = repo_relative(&listing.repo, file)?;
        listing
            .changes
            .iter()
            .find(|change| change.scope == scope && change.path == path)
    }

    /// Why absolute `file`'s tab cannot switch to Diff mode: no uncommitted change of it in
    /// this folder's repository, or the repository is not known.
    fn diff_unavailable(&self, file: &str) -> Option<&'static str> {
        match &self.listing {
            Listing::Reading => Some("Still reading this folder's Git changes"),
            Listing::NotRepo => Some("This folder is not in a Git repository"),
            Listing::Failed(_) => Some("Cannot read this folder's Git changes"),
            Listing::Ready(listing) => match repo_relative(&listing.repo, file) {
                None => Some("The file is outside this folder's repository"),
                Some(path) => (!listing.changes.iter().any(|change| change.path == path))
                    .then_some("The file has no uncommitted changes"),
            },
        }
    }

    /// Lists the rows again; the selection keeps its row, or the fold hiding it, or the
    /// place it had.
    fn rebuild(&mut self, tree: bool) {
        let (rows, order) = list_rows(self.changes(), tree, &self.folds);
        self.rows = rows;
        self.order = order;
        let selected = self.selected.take();
        self.selected = selected.and_then(|(key, was)| {
            let at = reselect(&self.rows, self.changes(), &key, was)?;
            Some((row_key(&self.rows[at], self.changes()), at))
        });
    }

    /// The selected row's index.
    fn selected_row(&self) -> Option<usize> {
        self.selected.as_ref().map(|(_, at)| *at)
    }

    /// Selects row `at` and scrolls the list to it.
    fn select_row(&mut self, at: usize) {
        if let Some(row) = self.rows.get(at) {
            self.selected = Some((row_key(row, self.changes()), at));
            self.scroll.scroll_to_item(at, ScrollStrategy::Nearest);
        }
    }

    /// A stage or unstage of `path` out of `scope` is on its way.
    fn moving(&self, scope: GitScope, path: &str) -> bool {
        self.in_flight
            .iter()
            .any(|write| !write.discard && write.names(scope, path))
    }

    /// A discard of the unstaged `path` is on its way.
    fn discarding(&self, path: &str) -> bool {
        self.in_flight
            .iter()
            .any(|write| write.discard && write.names(GitScope::Unstaged, path))
    }

    /// A section's Stage all or Unstage all is on its way.
    fn moving_section(&self, scope: GitScope) -> bool {
        self.in_flight
            .iter()
            .any(|write| write.section && write.scope == scope)
    }

    /// Takes a fresh listing; whether anything shown changed.
    fn apply_listing(&mut self, listing: GitChanges, tree: bool) -> bool {
        let cleared = self.refresh_error.take().is_some();
        if let Listing::Ready(current) = &self.listing
            && *current == listing
        {
            return cleared;
        }
        let mut paths: Vec<&str> = listing
            .changes
            .iter()
            .map(|change| change.path.as_str())
            .collect();
        paths.sort_unstable();
        paths.dedup();
        self.totals =
            listing
                .changes
                .iter()
                .fold((paths.len(), 0, 0), |(files, added, removed), change| {
                    (
                        files,
                        added + u64::from(change.added.unwrap_or(0)),
                        removed + u64::from(change.removed.unwrap_or(0)),
                    )
                });
        self.marks = tree_marks(&self.root, &listing);
        self.listing = Listing::Ready(listing);
        self.rebuild(tree);
        true
    }

    /// A listing read for this folder elsewhere (the Changes command), as if read here.
    pub(super) fn seed(&mut self, listing: GitChanges, tree: bool) {
        self.apply_listing(listing, tree);
        self.read_at = Some(Instant::now());
    }

    /// The folder tree's mark for the tree path `path`.
    pub(super) fn tree_mark(&self, path: &str) -> Option<&TreeMark> {
        self.marks.get(path)
    }

    /// Unfolds what hides the change of `path` in `scope`, selects it and scrolls the list
    /// to it.
    fn reveal(&mut self, path: &str, scope: GitScope, tree: bool) {
        let mut unfolded = self.folds.sections.remove(&scope);
        self.folds.folders.retain(|(folded, folder)| {
            let hides = *folded == scope
                && path
                    .strip_prefix(folder.as_str())
                    .is_some_and(|rest| rest.starts_with('/'));
            unfolded |= hides;
            !hides
        });
        if unfolded {
            self.rebuild(tree);
        }
        let key = RowKey::File(scope, path.to_owned());
        if let Some(row) = self
            .rows
            .iter()
            .position(|row| row_is(row, self.changes(), &key))
        {
            self.select_row(row);
        }
    }
}

impl DiffTab {
    /// The diff on screen.
    fn view(&self) -> Option<&Entity<DiffView>> {
        self.shown.as_ref().map(|shown| &shown.view)
    }

    /// The diffs this tab holds: the one shown and one waiting to replace it.
    fn views(&self) -> impl Iterator<Item = &Entity<DiffView>> {
        self.view().into_iter().chain(
            self.pending
                .as_ref()
                .and_then(|pending| pending.view.as_ref())
                .map(|(view, _, _)| view),
        )
    }

    /// Nothing to show yet, and the first diff is slow to come.
    fn loading(&self) -> bool {
        self.shown.is_none()
            && self
                .pending
                .as_ref()
                .is_some_and(|pending| pending.started.elapsed() >= LOADING_DELAY)
    }
}

/// The folder tree's marks for `listing`, keyed by tree path below the pane's folder
/// `root` (built as the tree joins names): every changed file still in the work tree with
/// its status (an unstaged change wins over a staged one, as the listing puts unstaged
/// changes first), and every folder holding a change, deletions included.
fn tree_marks(root: &str, listing: &GitChanges) -> HashMap<String, TreeMark> {
    let mut marks = HashMap::new();
    let top = listing.repo.trim_end_matches(['/', '\\']);
    let here = root.trim_end_matches(['/', '\\']);
    let prefix = if top == here || (listing.repo.contains('\\') && top.eq_ignore_ascii_case(here)) {
        String::new()
    } else {
        match repo_relative(&listing.repo, root) {
            Some(relative) => format!("{relative}/"),
            None => return marks,
        }
    };
    for change in &listing.changes {
        let Some(rest) = change.path.strip_prefix(prefix.as_str()) else {
            continue;
        };
        let mut path = root.to_owned();
        let mut parts = rest.split('/').peekable();
        while let Some(part) = parts.next() {
            path = join_path(&path, part);
            if parts.peek().is_some() {
                marks.entry(path.clone()).or_insert(TreeMark::Folder);
            } else if change.kind != GitChangeKind::Deleted {
                marks.entry(path).or_insert_with(|| TreeMark::File {
                    kind: change.kind,
                    path: change.path.clone(),
                });
                break;
            }
        }
    }
    marks
}

/// What a diff tab compares, in words, when its change is not listed.
fn scope_baseline(scope: GitScope) -> &'static str {
    match scope {
        GitScope::Unstaged => "Working tree vs index",
        GitScope::Staged => "Index vs HEAD",
    }
}

/// A folder of the tree being built, borrowing the listing's paths.
#[derive(Default)]
struct TreeFolder<'a> {
    folders: BTreeMap<&'a str, TreeFolder<'a>>,
    files: Vec<(&'a str, usize)>,
}

impl<'a> TreeFolder<'a> {
    fn insert(&mut self, path: &'a str, change: usize) {
        match path.split_once('/') {
            Some((folder, rest)) => self.folders.entry(folder).or_default().insert(rest, change),
            None => self.files.push((path, change)),
        }
    }

    /// Folders first, then files, each by name. Every file goes to `order`; rows only
    /// for what no fold hides.
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &self,
        scope: GitScope,
        prefix: &str,
        depth: usize,
        hidden: bool,
        folds: &Folds,
        rows: &mut Vec<ListRow>,
        order: &mut Vec<usize>,
    ) {
        for (name, folder) in &self.folders {
            let mut label = (*name).to_owned();
            let mut folder = folder;
            while folder.files.is_empty()
                && folder.folders.len() == 1
                && let Some((child, inner)) = folder.folders.iter().next()
            {
                label.push('/');
                label.push_str(child);
                folder = inner;
            }
            let path = if prefix.is_empty() {
                label.clone()
            } else {
                format!("{prefix}/{label}")
            };
            let collapsed = folds.folders.contains(&(scope, path.clone()));
            if !hidden {
                rows.push(ListRow::Folder {
                    scope,
                    path: path.clone(),
                    label,
                    depth,
                    collapsed,
                });
            }
            folder.emit(
                scope,
                &path,
                depth + 1,
                hidden || collapsed,
                folds,
                rows,
                order,
            );
        }
        let mut files = self.files.clone();
        files.sort_unstable();
        for (_, change) in files {
            order.push(change);
            if !hidden {
                rows.push(ListRow::File { change, depth });
            }
        }
    }
}

/// The list of `changes`: a section per scope that has changes, Unstaged first, holding
/// its changes in path order or, as a tree, in folders. Also every change in that order,
/// folded or not.
fn list_rows(changes: &[GitChange], tree: bool, folds: &Folds) -> (Vec<ListRow>, Vec<usize>) {
    let mut rows = Vec::new();
    let mut order = Vec::with_capacity(changes.len());
    for scope in [GitScope::Unstaged, GitScope::Staged] {
        let count = changes
            .iter()
            .filter(|change| change.scope == scope)
            .count();
        if count == 0 {
            continue;
        }
        let collapsed = folds.sections.contains(&scope);
        rows.push(ListRow::Section {
            scope,
            count,
            collapsed,
        });
        let members = changes
            .iter()
            .enumerate()
            .filter(|(_, change)| change.scope == scope);
        if tree {
            let mut root = TreeFolder::default();
            for (index, change) in members {
                root.insert(&change.path, index);
            }
            root.emit(scope, "", 0, collapsed, folds, &mut rows, &mut order);
        } else {
            for (index, _) in members {
                order.push(index);
                if !collapsed {
                    rows.push(ListRow::File {
                        change: index,
                        depth: 0,
                    });
                }
            }
        }
    }
    (rows, order)
}

fn row_key(row: &ListRow, changes: &[GitChange]) -> RowKey {
    match row {
        ListRow::Section { scope, .. } => RowKey::Section(*scope),
        ListRow::Folder { scope, path, .. } => RowKey::Folder(*scope, path.clone()),
        ListRow::File { change, .. } => {
            let change = &changes[*change];
            RowKey::File(change.scope, change.path.clone())
        }
    }
}

/// Whether `row` shows what `key` names.
fn row_is(row: &ListRow, changes: &[GitChange], key: &RowKey) -> bool {
    match (row, key) {
        (ListRow::Section { scope, .. }, RowKey::Section(wanted)) => scope == wanted,
        (ListRow::Folder { scope, path, .. }, RowKey::Folder(wanted, folder)) => {
            scope == wanted && path == folder
        }
        (ListRow::File { change, .. }, RowKey::File(wanted, path)) => {
            changes[*change].scope == *wanted && changes[*change].path == *path
        }
        _ => false,
    }
}

/// Where the selection of `key`, last at row `was`, goes in `rows`: its row; else the
/// folded section or folder hiding it; else the row now at its place (the next one, when
/// its change left), or the last.
fn reselect(rows: &[ListRow], changes: &[GitChange], key: &RowKey, was: usize) -> Option<usize> {
    if let Some(at) = rows.iter().position(|row| row_is(row, changes, key)) {
        return Some(at);
    }
    if let RowKey::Folder(scope, path) | RowKey::File(scope, path) = key
        && let Some(at) = rows.iter().rposition(|row| match row {
            ListRow::Section {
                scope: folded,
                collapsed: true,
                ..
            } => folded == scope,
            ListRow::Folder {
                scope: folded,
                path: folder,
                collapsed: true,
                ..
            } => {
                folded == scope
                    && path
                        .strip_prefix(folder.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            }
            _ => false,
        })
    {
        return Some(at);
    }
    let last = rows.len().checked_sub(1)?;
    Some(was.min(last))
}

/// The row Down (`forward`) or Up selects from `at` among `len` rows: the first or the
/// last with nothing selected; it stops at either end.
fn step_row(len: usize, at: Option<usize>, forward: bool) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(match at {
        Some(at) if forward => (at + 1).min(last),
        Some(at) => at.saturating_sub(1),
        None if forward => 0,
        None => last,
    })
}

/// How deep `row` sits: sections at 0, what they hold below.
fn row_level(row: &ListRow) -> usize {
    match row {
        ListRow::Section { .. } => 0,
        ListRow::Folder { depth, .. } | ListRow::File { depth, .. } => depth + 1,
    }
}

/// Left (`open` false) or Right on row `at`: a section or folder folds or unfolds; one
/// already so, or a file, moves to the row holding it (Left) or the first it holds (Right).
fn sideways(rows: &[ListRow], at: usize, open: bool) -> Sideways {
    let Some(row) = rows.get(at) else {
        return Sideways::Stay;
    };
    let folded = match row {
        ListRow::Section { collapsed, .. } | ListRow::Folder { collapsed, .. } => Some(*collapsed),
        ListRow::File { .. } => None,
    };
    if folded == Some(open) {
        return Sideways::Toggle;
    }
    let level = row_level(row);
    let target = if open {
        rows.get(at + 1)
            .is_some_and(|next| row_level(next) > level)
            .then_some(at + 1)
    } else {
        rows[..at]
            .iter()
            .rposition(|above| row_level(above) < level)
    };
    target.map_or(Sideways::Stay, Sideways::Select)
}

/// The paths staging or unstaging the changes of `scope` names, only those inside
/// `folder` when given; sorted, once each.
fn scope_paths(changes: &[GitChange], scope: GitScope, folder: Option<&str>) -> Vec<String> {
    let mut paths: Vec<String> = changes
        .iter()
        .filter(|change| {
            change.scope == scope
                && folder.is_none_or(|folder| {
                    change
                        .path
                        .strip_prefix(folder)
                        .is_some_and(|rest| rest.starts_with('/'))
                })
        })
        .flat_map(change_paths)
        .collect();
    paths.sort_unstable();
    paths.dedup();
    paths
}

/// What `s` (`Unstaged`) or `u` (`Staged`) moves on `row`: its file, or every file of its
/// folder or section, when the row is in that section.
fn row_paths(row: &ListRow, changes: &[GitChange], scope: GitScope) -> Vec<String> {
    match row {
        ListRow::Section { scope: at, .. } if *at == scope => scope_paths(changes, scope, None),
        ListRow::Folder {
            scope: at, path, ..
        } if *at == scope => scope_paths(changes, scope, Some(path.as_str())),
        ListRow::File { change, .. } if changes[*change].scope == scope => {
            change_paths(&changes[*change]).collect()
        }
        _ => Vec::new(),
    }
}

/// The two sides a change compares, old then new: a path and the revision to read it
/// at, or `None` for a side without the file.
fn sides(change: &GitChange) -> [Option<(&str, GitRevision)>; 2] {
    use GitChangeKind::*;
    use GitRevision::*;
    let path = change.path.as_str();
    let old_path = change.old_path.as_deref().unwrap_or(path);
    match (change.scope, change.kind) {
        (_, Conflicted) => [Some((path, Head)), Some((path, WorkTree))],
        (_, Untracked) | (GitScope::Unstaged, Added) => [None, Some((path, WorkTree))],
        (GitScope::Unstaged, Deleted) => [Some((path, Index)), None],
        (GitScope::Unstaged, _) => [Some((old_path, Index)), Some((path, WorkTree))],
        (GitScope::Staged, Added) => [None, Some((path, Index))],
        (GitScope::Staged, Deleted) => [Some((path, Head)), None],
        (GitScope::Staged, _) => [Some((old_path, Head)), Some((path, Index))],
    }
}

/// Why the tab of `change`'s file cannot switch to Edit: the change deletes the file, so
/// there is no new side to edit.
fn edit_disabled(change: &GitChange) -> Option<&'static str> {
    sides(change)[1]
        .is_none()
        .then_some("This change deletes the file, so there is nothing to edit")
}

/// How hunks of `change`'s diff move, and the index path they change: staged from an
/// unstaged diff, unstaged from a staged one. `None` when the diff does not compare two
/// versions of one file in the index (added, deleted, untracked or conflicted files).
fn hunk_target(change: &GitChange) -> Option<(HunkWrite, String)> {
    if matches!(
        change.kind,
        GitChangeKind::Conflicted | GitChangeKind::Untracked
    ) {
        return None;
    }
    let [Some((old, _)), Some((new, _))] = sides(change) else {
        return None;
    };
    Some(match change.scope {
        GitScope::Unstaged => (HunkWrite::Stage, old.to_owned()),
        GitScope::Staged => (HunkWrite::Unstage, new.to_owned()),
    })
}

/// The paths staging or unstaging `change` names: a rename's old path too, so both of its
/// sides move together.
fn change_paths(change: &GitChange) -> impl Iterator<Item = String> + '_ {
    std::iter::once(change.path.clone()).chain(change.old_path.clone())
}

/// `summary` and `body` as one commit message: the body after a blank line, when there
/// is one.
fn commit_message(summary: &str, body: &str) -> String {
    let summary = summary.trim();
    let body = body.trim_end().trim_start_matches(['\r', '\n']);
    if body.trim().is_empty() {
        summary.to_owned()
    } else {
        format!("{summary}\n\n{body}")
    }
}

/// A commit message as the box shows it: its first line as the summary, the rest without
/// the blank lines after the summary as the body.
fn split_message(message: &str) -> (String, String) {
    let message = message.trim_end();
    let (summary, body) = message.split_once('\n').unwrap_or((message, ""));
    (
        summary.trim_end_matches('\r').to_owned(),
        body.trim_start_matches(['\r', '\n']).to_owned(),
    )
}

/// Why Commit (or Amend, `amend`) cannot run yet: nothing `staged` to commit, or no
/// summary written.
fn commit_refusal(staged: bool, amend: bool, has_summary: bool) -> Option<&'static str> {
    if !staged && !amend {
        Some("Stage changes to commit them")
    } else if !has_summary {
        Some("Write a summary first")
    } else {
        None
    }
}

/// What a change compares, in words.
fn baseline(change: &GitChange) -> String {
    let comparison = match (change.kind, change.scope) {
        (GitChangeKind::Untracked, _) => return "New file (untracked)".into(),
        (GitChangeKind::Conflicted, _) => return "Conflicted · working tree vs HEAD".into(),
        (_, GitScope::Unstaged) => "Working tree vs index",
        (_, GitScope::Staged) => "Index vs HEAD",
    };
    match &change.old_path {
        Some(old) => format!("{old} → {} · {comparison}", change.path),
        None => comparison.into(),
    }
}

fn kind_label(kind: GitChangeKind) -> &'static str {
    match kind {
        GitChangeKind::Added => "Added",
        GitChangeKind::Modified => "Modified",
        GitChangeKind::Deleted => "Deleted",
        GitChangeKind::Renamed => "Renamed",
        GitChangeKind::Copied => "Copied",
        GitChangeKind::TypeChanged => "Type changed",
        GitChangeKind::Untracked => "Untracked",
        GitChangeKind::Conflicted => "Conflicted",
    }
}

pub(super) fn status_letter(kind: GitChangeKind) -> &'static str {
    match kind {
        GitChangeKind::Added => "A",
        GitChangeKind::Modified => "M",
        GitChangeKind::Deleted => "D",
        GitChangeKind::Renamed => "R",
        GitChangeKind::Copied => "C",
        GitChangeKind::TypeChanged => "T",
        GitChangeKind::Untracked => "?",
        GitChangeKind::Conflicted => "!",
    }
}

pub(super) fn kind_color(kind: GitChangeKind, colors: &ThemeColors) -> u32 {
    match kind {
        GitChangeKind::Added | GitChangeKind::Untracked => colors.added,
        GitChangeKind::Deleted => colors.removed,
        GitChangeKind::Modified | GitChangeKind::TypeChanged => colors.modified,
        GitChangeKind::Renamed | GitChangeKind::Copied => colors.accent,
        GitChangeKind::Conflicted => colors.error,
    }
}

fn scope_label(scope: GitScope) -> &'static str {
    match scope {
        GitScope::Unstaged => "Unstaged",
        GitScope::Staged => "Staged",
    }
}

/// A repository path's folder (empty at the top) and name.
fn split_path(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// `text` in at most `max` characters, eliding its middle: `src/…/workspace`.
pub(super) fn truncate_middle(text: &str, max: usize) -> Cow<'_, str> {
    let count = text.chars().count();
    if count <= max {
        return Cow::Borrowed(text);
    }
    let Some(keep) = max.checked_sub(1) else {
        return Cow::Borrowed("");
    };
    let head = keep / 2;
    let mut elided: String = text.chars().take(head).collect();
    elided.push('…');
    elided.extend(text.chars().skip(count - (keep - head)));
    Cow::Owned(elided)
}

/// `+a −r` as two runs in the diff colors; nothing when both are zero.
fn line_counts(added: u64, removed: u64, colors: &ThemeColors) -> Option<impl IntoElement> {
    (added > 0 || removed > 0).then(|| {
        div()
            .flex_none()
            .flex()
            .gap_1()
            .when(added > 0, |counts| {
                counts.child(
                    div()
                        .text_color(color(colors.added))
                        .child(format!("+{added}")),
                )
            })
            .when(removed > 0, |counts| {
                counts.child(
                    div()
                        .text_color(color(colors.removed))
                        .child(format!("−{removed}")),
                )
            })
    })
}

/// A row's actions at its right end, on a background so they read over the row's text
/// (a file row hides its line counts meanwhile): shown while the row is hovered, and
/// always when `shown` (the selected row, or one whose write runs).
fn row_controls(
    shown: bool,
    colors: &ThemeColors,
    controls: impl IntoIterator<Item = Control>,
) -> gpui::Div {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .right(px(6.0))
        .flex()
        .items_center()
        .when(!shown, |strip| {
            strip
                .invisible()
                .group_hover("changes-row", |style| style.visible())
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(2.0))
                .rounded_sm()
                .bg(color(colors.surface_hover))
                .children(controls),
        )
}

impl CompiApp {
    /// The Git changes of `pane_id`'s folder: `None` outside a work tree or until the
    /// first read lands. Kept fresh while the pane is visible.
    pub(super) fn repo_changes(&self, pane_id: &PaneId) -> Option<&GitChanges> {
        self.content.panes.get(pane_id)?.repo.listing()
    }

    /// Shows the diff of repository path `path` in its file's tab of the content pane
    /// `pane_id` (Diff mode), opening the tab or switching the one open. Without `scope`, the
    /// unstaged change wins over the staged one; a file with neither leaves a message. `line`
    /// (1-based, new side) is scrolled into view, now or once the diff is built. Asked before
    /// the folder was read, it happens when the read lands.
    pub(in crate::gui) fn open_diff(
        &mut self,
        pane_id: &PaneId,
        path: String,
        scope: Option<GitScope>,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut content) = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane_id))
            .cloned()
        else {
            self.global_error = Some("The pane to show the diff in no longer exists".into());
            return;
        };
        let tree = self.content.changes.tree;
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            // Not drawn in this window yet; it reads its folder when it is.
            self.content.pending_diff = Some((pane_id.clone(), path, scope, line));
            self.focus_pane(pane_id.clone());
            cx.notify();
            return;
        };
        let listing = match &ui.repo.listing {
            Listing::Ready(listing) => listing,
            Listing::Reading => {
                ui.repo.pending_open = Some((path, scope, line));
                self.request_repo_listing(pane_id, window, cx);
                return;
            }
            Listing::NotRepo => {
                self.global_error = Some(format!("Not a Git repository: {}", content.root));
                return;
            }
            Listing::Failed(error) => {
                self.global_error = Some(format!("Cannot read the Git changes: {error}"));
                return;
            }
        };
        let listed = |scope: GitScope| {
            listing
                .changes
                .iter()
                .any(|change| change.scope == scope && change.path == path)
        };
        let scope = match scope {
            Some(scope) => scope,
            None => match [GitScope::Unstaged, GitScope::Staged]
                .into_iter()
                .find(|scope| listed(*scope))
            {
                Some(scope) => scope,
                None => {
                    self.global_error =
                        Some(format!("{} has no uncommitted changes", file_name(&path)));
                    cx.notify();
                    return;
                }
            },
        };
        // The file's tab, whatever path spelling it was opened with, else the work-tree path.
        let file = content
            .files()
            .find(|open| repo_relative(&listing.repo, open).as_deref() == Some(path.as_str()))
            .map_or_else(|| work_tree_path(&listing.repo, &path), str::to_owned);
        ui.repo.reveal(&path, scope, tree);
        let tab = ContentTab::diff(file.clone(), scope);
        if let Some(line) = line {
            match ui.diffs.get(&tab).and_then(DiffTab::view) {
                Some(view) => view.update(cx, |view, cx| view.reveal_line(Side::New, line, cx)),
                None => ui.diff_reveal = Some((tab.clone(), line)),
            }
        }
        if content.tab(&file).is_none() && content.tabs.len() >= compi_protocol::MAX_CONTENT_FILES {
            self.global_error =
                Some("Close a tab first: this pane has as many as it can hold".into());
            return;
        }
        if content.active.as_ref() != Some(&tab) {
            content.show(tab);
            self.update_content(pane_id, content);
        }
        self.focus_pane(pane_id.clone());
        self.release_editor(pane_id, window, cx);
        cx.notify();
    }

    /// The Changes command: in a content pane, its sidebar lists the changes; from a
    /// terminal, the content pane of its repository does, opened beside it if the tab
    /// has none.
    pub(in crate::gui) fn show_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let FileScope {
            pane_id,
            environment,
            folder,
            content,
            ..
        } = match self.file_scope() {
            Ok(scope) => scope,
            Err(reason) => {
                self.global_error = Some(reason.into());
                return;
            }
        };
        if content {
            self.show_changes_list(&pane_id, window, cx);
            return;
        }
        let target = self.target.clone();
        cx.spawn_in(window, async move |this, cx| {
            let read = {
                let (environment, folder) = (environment.clone(), folder.clone());
                cx.background_executor()
                    .spawn(async move {
                        (|| -> crate::Result<_> {
                            target.connect()?.read_git_changes(environment, folder)
                        })()
                        .map_err(|error| error.to_string())
                    })
                    .await
            };
            this.update_in(cx, |this, window, cx| {
                match read {
                    Ok(Some(listing)) => {
                        this.open_changes(&pane_id, environment, listing, window, cx)
                    }
                    Ok(None) => this.global_error = Some(format!("Not a Git repository: {folder}")),
                    Err(error) => {
                        this.global_error =
                            Some(format!("Cannot read the Git changes of {folder}: {error}"))
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The content pane of `listing`'s repository in `beside`'s tab shows its changes,
    /// or a new one beside it does.
    fn open_changes(
        &mut self,
        beside: &PaneId,
        environment: ContentEnvironment,
        listing: GitChanges,
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
            self.global_error = Some("The pane to show changes beside no longer exists".into());
            return;
        };
        let existing =
            reusable_content(&tab.layout, beside, &environment, Some(&listing.repo), None)
                .map(|(pane_id, _)| pane_id.clone());
        let tree = self.content.changes.tree;
        if let Some(pane_id) = existing {
            if let Some(ui) = self.content.panes.get_mut(&pane_id)
                && ui.repo.reading.is_none()
                && same_folder(&ui.repo.root, &listing.repo)
                && ui.repo.apply_listing(listing, tree)
            {
                ui.repo.read_at = Some(Instant::now());
            }
            self.focus_pane(pane_id.clone());
            self.show_changes_list(&pane_id, window, cx);
            return;
        }
        let content = PaneContent::Files(compi_protocol::FolderContent {
            environment: environment.clone(),
            root: listing.repo.clone(),
            tabs: Vec::new(),
            active: None,
        });
        if self.open_content_beside(beside, content) {
            self.content.changes_seed = Some((environment, listing));
        }
        cx.notify();
    }

    /// The end of a folder tree row with a mark: a changed file's status letter, with a
    /// **Diff** button before it while the row is hovered or `selected`, or a folder's
    /// quiet dot.
    pub(super) fn render_tree_mark(
        &self,
        pane_id: &PaneId,
        index: usize,
        mark: &TreeMark,
        row_height: f32,
        selected: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let slot = div()
            .flex_none()
            .w(px(12.0))
            .h(px(row_height))
            .flex()
            .items_center()
            .justify_center();
        let TreeMark::File { kind, path } = mark else {
            return slot
                .child(
                    div()
                        .size(px(5.0))
                        .rounded_full()
                        .bg(color(colors.modified))
                        .opacity(0.6),
                )
                .into_any_element();
        };
        let (pane, path) = (pane_id.clone(), path.clone());
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .when(!selected, |slot| {
                        slot.invisible()
                            .group_hover("content-tree-row", |style| style.visible())
                    })
                    .child(
                        icon_button(("content-tree-diff", index), IconName::FileDiff, "Diff")
                            .tab_stop(false)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.open_diff(&pane, path.clone(), None, None, window, cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                slot.text_size(px(UI_MICRO_TEXT_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color(kind_color(*kind, &colors)))
                    .child(status_letter(*kind)),
            )
            .into_any_element()
    }

    /// Switches `pane_id`'s sidebar to the Changes list and shows it beside an open tab.
    pub(super) fn show_changes_list(
        &mut self,
        pane_id: &PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab_open = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane_id))
            .is_some_and(|content| content.active.is_some());
        if let Some(ui) = self.content.panes.get_mut(pane_id) {
            ui.mode = SidebarMode::Changes;
            ui.sidebar |= tab_open;
        }
        self.release_editor(pane_id, window, cx);
        cx.notify();
    }

    /// Diff this file (Ctrl+Shift+D): the focused pane's open file switches to Diff mode
    /// against the index, else its staged change, at the editor's cursor line; in Diff mode
    /// it returns to editing at the line in view.
    pub(in crate::gui) fn diff_this_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((pane_id, path, diff)) = self.focused_pane().cloned().and_then(|pane_id| {
            let tab = self.focused_content()?.active.as_ref()?;
            Some((pane_id, tab.path().to_owned(), tab.diff_scope().is_some()))
        }) else {
            self.global_error = Some("Open a file in the editor first".into());
            return;
        };
        if diff {
            self.edit_from_diff(&pane_id, &path, window, cx);
        } else {
            self.diff_from_edit(&pane_id, &path, window, cx);
        }
    }

    /// Diff: the file's tab switches to Diff mode against the index, else its staged
    /// change, at the editor's cursor line.
    fn diff_from_edit(
        &mut self,
        pane_id: &PaneId,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = file_name(path).to_owned();
        let Some(ui) = self.content.panes.get(pane_id) else {
            return;
        };
        // The diff's new side is the saved file (unstaged) or the index (staged), so the
        // cursor's line is a close guess when the buffer has unsaved edits.
        let line = ui
            .editor(path)
            .map(|editor| editor.state.read(cx).cursor_position().line + 1);
        let relative = match &ui.repo.listing {
            Listing::Ready(listing) => repo_relative(&listing.repo, path),
            Listing::Reading => {
                self.global_error = Some("Still reading this folder's Git changes".into());
                return;
            }
            Listing::NotRepo => {
                self.global_error = Some(format!("{name} is not in a Git repository"));
                return;
            }
            Listing::Failed(error) => {
                self.global_error = Some(format!("Cannot read the Git changes: {error}"));
                return;
            }
        };
        let Some(relative) = relative else {
            self.global_error = Some(format!("{name} is outside this folder's repository"));
            return;
        };
        self.open_diff(pane_id, relative, None, line, window, cx);
    }

    /// Edit: the file's tab returns to Edit mode at the line the diff shows a third of the
    /// way down. A file the change deleted stays in Diff mode.
    pub(super) fn edit_from_diff(
        &mut self,
        pane_id: &PaneId,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ui) = self.content.panes.get(pane_id) else {
            return;
        };
        let Some(tab) = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane_id))
            .and_then(|content| content.tab(path))
        else {
            return;
        };
        if let Some(reason) = tab
            .diff_scope()
            .and_then(|scope| ui.repo.change(path, scope))
            .and_then(edit_disabled)
        {
            self.global_error = Some(reason.into());
            return;
        }
        let line = ui
            .diffs
            .get(tab)
            .and_then(DiffTab::view)
            .and_then(|view| view.read(cx).line_in_view())
            .and_then(|(side, line)| (side == Side::New).then_some(line));
        let environment = ui.environment.clone();
        self.open_content_file(
            pane_id,
            environment,
            None,
            Some(path.to_owned()),
            line,
            false,
            window,
            cx,
        );
    }

    /// Before drawing: the active diff tab of each shown pane is read when its change is
    /// new or moved, every diff takes the window's colors and font and the hunk button its
    /// change allows, and each shown Changes list gets its commit box.
    pub(super) fn prepare_diff_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active: Vec<(PaneId, ContentTab)> = match &self.workspace {
            Some(workspace) => self
                .shown_content_panes()
                .filter_map(|pane_id| {
                    let active = find_content(workspace, pane_id)?.active.as_ref()?;
                    active
                        .diff_scope()
                        .map(|_| (pane_id.clone(), active.clone()))
                })
                .collect(),
            None => Vec::new(),
        };
        for (pane_id, tab) in active {
            self.load_diff(&pane_id, &tab, window, cx);
        }
        let style = DiffStyle {
            colors: *self.colors(),
            typography: self.typography.clone(),
        };
        for ui in self.content.panes.values() {
            for (tab, state) in &ui.diffs {
                let button = tab
                    .diff_scope()
                    .and_then(|scope| ui.repo.change(tab.path(), scope))
                    .and_then(hunk_target)
                    .map(|(write, _)| match write {
                        HunkWrite::Stage => HunkButton::Stage,
                        HunkWrite::Unstage => HunkButton::Unstage,
                    });
                for view in state.views() {
                    view.update(cx, |view, cx| {
                        view.set_style(style.clone(), cx);
                        view.set_hunk_button(button, cx);
                    });
                }
            }
        }
        let boxless: Vec<PaneId> = self
            .shown_content_panes()
            .filter(|pane_id| {
                self.content.panes.get(*pane_id).is_some_and(|ui| {
                    ui.mode == SidebarMode::Changes
                        && ui.repo.commit.is_none()
                        && ui.repo.listing().is_some()
                })
            })
            .cloned()
            .collect();
        for pane_id in boxless {
            let commit = self.new_commit_box(&pane_id, window, cx);
            if let Some(ui) = self.content.panes.get_mut(&pane_id) {
                ui.repo.commit = Some(commit);
            }
        }
    }

    /// The summary field, the body field and what their events do.
    fn new_commit_box(
        &mut self,
        pane_id: &PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> CommitBox {
        let summary = cx.new(|cx| InputState::new(window, cx).placeholder("Summary"));
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Description")
                .auto_grow(2, 8)
        });
        let (pane, next) = (pane_id.clone(), body.clone());
        let events = cx.subscribe_in(
            &summary,
            window,
            move |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let filled = !input.read(cx).value().trim().is_empty();
                    if let Some(commit) = this
                        .content
                        .panes
                        .get_mut(&pane)
                        .and_then(|ui| ui.repo.commit.as_mut())
                        && commit.has_summary != filled
                    {
                        commit.has_summary = filled;
                        cx.notify();
                    }
                }
                // Enter in the summary goes on to the description.
                InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                } => next.update(cx, |body, cx| body.focus(window, cx)),
                _ => {}
            },
        );
        CommitBox {
            summary,
            body,
            has_summary: false,
            amend: false,
            draft: None,
            committing: false,
            _events: events,
        }
    }

    /// Reads both sides of a diff tab's change unless they are on screen or on their way
    /// at its current stamp. A diff on screen is replaced in place.
    fn load_diff(
        &mut self,
        pane_id: &PaneId,
        tab: &ContentTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(scope) = tab.diff_scope() else {
            return;
        };
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        let Some((change, repo)) = ui
            .repo
            .change(tab.path(), scope)
            .cloned()
            .zip(ui.repo.listing().map(|listing| listing.repo.clone()))
        else {
            return;
        };
        let environment = ui.environment.clone();
        let state = ui.diffs.entry(tab.clone()).or_default();
        let current = |stamp: &str| stamp == change.stamp;
        if state
            .shown
            .as_ref()
            .is_some_and(|shown| current(&shown.stamp))
        {
            state.pending = None;
            return;
        }
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| current(&pending.stamp))
            || state
                .failed
                .as_ref()
                .is_some_and(|(stamp, _)| current(stamp))
        {
            return;
        }
        state.loads += 1;
        let load = state.loads;
        state.failed = None;
        state.pending = Some(Pending {
            stamp: change.stamp.clone(),
            load,
            started: Instant::now(),
            view: None,
        });
        let [old, new] =
            sides(&change).map(|side| side.map(|(path, revision)| (path.to_owned(), revision)));
        let executor = cx.background_executor().clone();
        let read = |side: Option<(String, GitRevision)>| {
            let (target, environment, repo) =
                (self.target.clone(), environment.clone(), repo.clone());
            executor.spawn(async move {
                let Some((path, revision)) = side else {
                    return Ok(GitFileContent::Missing);
                };
                (|| -> crate::Result<_> {
                    target
                        .connect()?
                        .read_git_file(environment, repo, path, revision)
                })()
                .map_err(|error| error.to_string())
            })
        };
        // Both sides read at once, on background threads.
        let (old, new) = (read(old), read(new));
        let (pane, tab) = (pane_id.clone(), tab.clone());
        cx.spawn_in(window, async move |this, cx| {
            let (old, new) = (old.await, new.await);
            this.update_in(cx, |this, window, cx| {
                this.diff_read(&pane, &tab, load, old, new, window, cx);
            })
            .ok();
        })
        .detach();
        // The loading line, should the diff still be on its way.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LOADING_DELAY).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn diff_read(
        &mut self,
        pane_id: &PaneId,
        tab: &ContentTab,
        load: u64,
        old: Result<GitFileContent, String>,
        new: Result<GitFileContent, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let style = DiffStyle {
            colors: *self.colors(),
            typography: self.typography.clone(),
        };
        let settings = self.content.changes.diff;
        let Some(PaneUi {
            diffs, diff_reveal, ..
        }) = self.content.panes.get_mut(pane_id)
        else {
            return;
        };
        let Some(state) = diffs.get_mut(tab) else {
            return;
        };
        let Some(pending) = state.pending.take_if(|pending| pending.load == load) else {
            return;
        };
        let (old, new) = match (old, new) {
            (Ok(old), Ok(new)) => (old, new),
            (Err(error), _) | (_, Err(error)) => {
                state.failed = Some((pending.stamp, error));
                cx.notify();
                return;
            }
        };
        if let Some(shown) = &mut state.shown {
            shown.view.update(cx, |view, cx| view.replace(old, new, cx));
            shown.stamp = pending.stamp;
            cx.notify();
            return;
        }
        let reveal = diff_reveal
            .take_if(|(waiting, _)| waiting == tab)
            .map(|(_, line)| line);
        // The tab's path picks the grammar.
        let path = tab.path().to_owned();
        let view = cx.new(|cx| {
            let mut view = DiffView::new(path, old, new, settings, window, cx);
            view.set_style(style, cx);
            if let Some(line) = reveal {
                view.reveal_line(Side::New, line, cx);
            }
            view
        });
        let (pane, waiting) = (pane_id.clone(), tab.clone());
        let subscription = cx.observe_in(&view, window, move |this, view, _, cx| {
            this.diff_view_changed(&pane, &waiting, &view, cx);
        });
        let (pane, clicked) = (pane_id.clone(), tab.clone());
        let events = cx.subscribe_in(
            &view,
            window,
            move |this, _, event: &HunkClicked, window, cx| {
                this.hunk_clicked(&pane, &clicked, event, window, cx);
            },
        );
        state.pending = Some(Pending {
            view: Some((view, subscription, events)),
            ..pending
        });
        cx.notify();
    }

    /// A new diff takes its tab once it can draw.
    fn diff_view_changed(
        &mut self,
        pane_id: &PaneId,
        tab: &ContentTab,
        view: &Entity<DiffView>,
        cx: &mut Context<Self>,
    ) {
        if !view.read(cx).is_ready() {
            return;
        }
        let Some(state) = self
            .content
            .panes
            .get_mut(pane_id)
            .and_then(|ui| ui.diffs.get_mut(tab))
        else {
            return;
        };
        let Some(pending) = state.pending.take_if(|pending| {
            pending
                .view
                .as_ref()
                .is_some_and(|(waiting, _, _)| waiting == view)
        }) else {
            return;
        };
        let Some((view, _, events)) = pending.view else {
            return;
        };
        state.shown = Some(Shown {
            stamp: pending.stamp,
            view,
            _events: events,
        });
        cx.notify();
    }

    /// On the watch tick: reads each shown content pane's folder again once its interval
    /// passed since the last read finished, one read per pane at a time, and marks reads
    /// that take a while.
    pub(super) fn refresh_repos(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.update_quiesced.load(Ordering::Acquire) {
            return;
        }
        let shown: Vec<PaneId> = self.shown_content_panes().cloned().collect();
        let mut redraw = false;
        for pane_id in &shown {
            let Some(repo) = self.content.panes.get_mut(pane_id).map(|ui| &mut ui.repo) else {
                continue;
            };
            match repo.reading {
                Some(since) => {
                    if !repo.marked && since.elapsed() >= REFRESHING_DELAY {
                        repo.marked = true;
                        redraw = true;
                    }
                }
                None => {
                    let interval = match repo.listing {
                        Listing::NotRepo => NOT_REPO_INTERVAL,
                        _ => CHECK_INTERVAL,
                    };
                    if repo.read_at.is_none_or(|at| at.elapsed() >= interval) {
                        self.request_repo_listing(pane_id, window, cx);
                    }
                }
            }
        }
        if redraw {
            cx.notify();
        }
    }

    fn request_repo_listing(
        &mut self,
        pane_id: &PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        if ui.repo.reading.is_some() {
            return;
        }
        ui.repo.reading = Some(Instant::now());
        let (environment, root) = (ui.environment.clone(), ui.repo.root.clone());
        let target = self.target.clone();
        let pane_id = pane_id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = {
                let root = root.clone();
                cx.background_executor()
                    .spawn(async move {
                        (|| -> crate::Result<_> {
                            target.connect()?.read_git_changes(environment, root)
                        })()
                        .map_err(|error| error.to_string())
                    })
                    .await
            };
            this.update_in(cx, |this, window, cx| {
                this.repo_listed(&pane_id, &root, result, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn repo_listed(
        &mut self,
        pane_id: &PaneId,
        root: &str,
        result: Result<Option<GitChanges>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tree = self.content.changes.tree;
        let Some(repo) = self
            .content
            .panes
            .get_mut(pane_id)
            .map(|ui| &mut ui.repo)
            .filter(|repo| repo.root == root)
        else {
            return;
        };
        let marked = std::mem::take(&mut repo.marked);
        repo.reading = None;
        repo.read_at = Some(Instant::now());
        let changed = match result {
            Ok(Some(listing)) => repo.apply_listing(listing, tree),
            Ok(None) => {
                let changed = !matches!(repo.listing, Listing::NotRepo);
                repo.listing = Listing::NotRepo;
                repo.rebuild(tree);
                repo.marks.clear();
                changed
            }
            Err(error) if matches!(repo.listing, Listing::Ready(_)) => {
                let changed = repo.refresh_error.as_ref() != Some(&error);
                repo.refresh_error = Some(error);
                changed
            }
            Err(error) => {
                repo.listing = Listing::Failed(error);
                true
            }
        };
        let reread = std::mem::take(&mut repo.reread);
        if let Some((path, scope, line)) = repo.pending_open.take() {
            self.open_diff(pane_id, path, scope, line, window, cx);
        }
        if reread {
            self.request_repo_listing(pane_id, window, cx);
        }
        if changed {
            // Gutters key their index reads by the listing; a write moves them at once.
            self.refresh_gutters(cx);
        }
        if changed || marked {
            cx.notify();
        }
    }

    /// Try again after a failed read of the folder or of the active diff.
    fn retry_repo(&mut self, pane_id: &PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ui) = self.content.panes.get_mut(pane_id) {
            for state in ui.diffs.values_mut() {
                state.failed = None;
            }
        }
        self.request_repo_listing(pane_id, window, cx);
    }

    /// Sends `action` to the repository whose top level is `repo`, listed in `pane_id`.
    /// When it is answered, every pane listing that repository reads it again at once.
    pub(super) fn git_write(
        &mut self,
        pane_id: &PaneId,
        repo: String,
        action: GitAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.send_write(pane_id, repo, action, WriteOrigin::Control, window, cx);
    }

    /// `git_write` from `origin`: a stage, unstage or discard is in flight until answered,
    /// and a hunk's answer ends its button's spinner.
    fn send_write(
        &mut self,
        pane_id: &PaneId,
        repo: String,
        action: GitAction,
        origin: WriteOrigin,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let kind = match action {
            GitAction::Commit { .. } => WriteKind::Commit,
            GitAction::LastMessage => WriteKind::LastMessage,
            _ => WriteKind::Index,
        };
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        ui.repo.writes += 1;
        let id = ui.repo.writes;
        let (section, hunk) = match origin {
            WriteOrigin::Control => (false, None),
            WriteOrigin::Section => (true, None),
            WriteOrigin::Hunk(tab) => (false, Some(tab)),
        };
        ui.repo.in_flight.extend(InFlight::of(id, &action, section));
        if kind != WriteKind::LastMessage {
            ui.repo.outcome = None;
        }
        let environment = ui.environment.clone();
        let target = self.target.clone();
        let pane = pane_id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = {
                let (environment, repo) = (environment.clone(), repo.clone());
                cx.background_executor()
                    .spawn(async move {
                        (|| -> crate::Result<_> {
                            target.connect()?.git_write(environment, repo, action)
                        })()
                        .map_err(|error| error.to_string())
                    })
                    .await
            };
            this.update_in(cx, |this, window, cx| {
                this.git_written(
                    &pane,
                    &environment,
                    &repo,
                    (kind, id, hunk.as_ref()),
                    result,
                    window,
                    cx,
                );
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// The answer to the write `sent` (its kind, its id, the diff tab of its hunk).
    #[allow(clippy::too_many_arguments)]
    fn git_written(
        &mut self,
        pane_id: &PaneId,
        environment: &ContentEnvironment,
        repo: &str,
        sent: (WriteKind, u64, Option<&ContentTab>),
        result: Result<GitWriteResult, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (kind, id, hunk) = sent;
        if let Some(ui) = self.content.panes.get_mut(pane_id) {
            for view in hunk
                .and_then(|tab| ui.diffs.get(tab))
                .into_iter()
                .flat_map(DiffTab::views)
            {
                view.update(cx, |view, cx| view.finish_hunk_write(cx));
            }
            let status = &mut ui.repo;
            status.in_flight.retain(|write| write.id != id);
            if kind == WriteKind::Commit
                && let Some(commit) = &mut status.commit
            {
                commit.committing = false;
            }
            let mut notice = None;
            match result {
                Ok(GitWriteResult::Done) => {}
                Ok(GitWriteResult::Committed { head, summary }) => {
                    if let Some(commit) = &mut status.commit {
                        commit.amend = false;
                        commit.draft = None;
                        commit.has_summary = false;
                        commit.fill("", "", window, cx);
                    }
                    notice = Some(format!("Committed {head} {summary}"));
                }
                Ok(GitWriteResult::Message { text }) => {
                    // Amend may have been turned off while the message was on its way.
                    if let Some(commit) = status.commit.as_mut().filter(|commit| commit.amend) {
                        let (summary, body) = split_message(&text);
                        commit.has_summary = !summary.trim().is_empty();
                        commit.fill(&summary, &body, window, cx);
                    }
                }
                Ok(GitWriteResult::Stale) => {
                    notice = Some("The index changed; refreshed".to_owned());
                    // Read the diffs again even if their stamps look current.
                    for state in ui.diffs.values_mut() {
                        if let Some(shown) = &mut state.shown {
                            shown.stamp.clear();
                        }
                    }
                }
                Err(error) => {
                    if kind == WriteKind::LastMessage
                        && let Some(commit) = status.commit.as_mut().filter(|commit| commit.amend)
                    {
                        commit.restore_draft(window, cx);
                    }
                    status.outcome = Some(Outcome::Error(error));
                }
            }
            if let Some(text) = notice {
                status.outcome = Some(Outcome::Notice(text, Instant::now()));
                let pane = pane_id.clone();
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(NOTICE_DURATION).await;
                    this.update(cx, |this, cx| {
                        if let Some(status) = this.content.panes.get_mut(&pane).map(|ui| &mut ui.repo)
                            && matches!(&status.outcome, Some(Outcome::Notice(_, at)) if at.elapsed() >= NOTICE_DURATION)
                        {
                            status.outcome = None;
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
            }
        }
        if kind != WriteKind::LastMessage {
            self.refresh_repo_listers(environment, repo, window, cx);
        }
        cx.notify();
    }

    /// Reads again at once every pane listing the repository `repo` of `environment`, so
    /// lists, open diffs and gutters follow a write.
    pub(super) fn refresh_repo_listers(
        &mut self,
        environment: &ContentEnvironment,
        repo: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let listing: Vec<PaneId> = self
            .content
            .panes
            .iter()
            .filter(|(_, ui)| {
                ui.environment == *environment
                    && ui
                        .repo
                        .listing()
                        .is_some_and(|listing| same_folder(&listing.repo, repo))
            })
            .map(|(pane_id, _)| pane_id.clone())
            .collect();
        for pane_id in listing {
            self.refresh_repo_now(&pane_id, window, cx);
        }
    }

    /// Reads `pane_id`'s folder now; a read already out is answered, then read again.
    fn refresh_repo_now(&mut self, pane_id: &PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        if ui.repo.reading.is_some() {
            ui.repo.reread = true;
        } else {
            self.request_repo_listing(pane_id, window, cx);
        }
    }

    /// The top level of the repository `pane_id` lists, once read.
    fn listed_repo(&self, pane_id: &PaneId) -> Option<String> {
        self.repo_changes(pane_id)
            .map(|listing| listing.repo.clone())
    }

    /// Stages (`Unstaged` changes) or unstages (`Staged` changes) `changes` of the
    /// repository `pane_id` lists; `section` when they are all of the section's.
    fn move_changes(
        &mut self,
        pane_id: &PaneId,
        scope: GitScope,
        paths: Vec<String>,
        section: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.listed_repo(pane_id) else {
            return;
        };
        if paths.is_empty() {
            return;
        }
        let action = match scope {
            GitScope::Unstaged => GitAction::Stage { paths },
            GitScope::Staged => GitAction::Unstage { paths },
        };
        let origin = if section {
            WriteOrigin::Section
        } else {
            WriteOrigin::Control
        };
        self.send_write(pane_id, repo, action, origin, window, cx);
    }

    /// The paths of every listed change in `scope`, for a section's Stage all / Unstage all.
    fn section_paths(&self, pane_id: &PaneId, scope: GitScope) -> Vec<String> {
        self.repo_changes(pane_id)
            .map(|listing| scope_paths(&listing.changes, scope, None))
            .unwrap_or_default()
    }

    /// Asks before discarding the work-tree changes of `changes` (unstaged ones), naming
    /// them; untracked files are deleted.
    fn confirm_discard(&mut self, pane_id: &PaneId, changes: &[GitChange]) {
        let Some(repo) = self.listed_repo(pane_id) else {
            return;
        };
        let (mut paths, mut untracked) = (Vec::new(), Vec::new());
        for change in changes {
            match change.kind {
                GitChangeKind::Untracked => untracked.push(change.path.clone()),
                GitChangeKind::Conflicted => {}
                _ => paths.push(change.path.clone()),
            }
        }
        if paths.is_empty() && untracked.is_empty() {
            return;
        }
        self.open_overlay(
            Overlay::ConfirmDiscard {
                pane_id: pane_id.clone(),
                repo,
                paths,
                untracked,
            },
            "",
        );
    }

    /// Right-click on a Changes row. A file: open its diff or the file, stage or
    /// unstage it, discard it (asking first). A section or folder row: the pane's
    /// actions.
    fn open_change_row_menu(
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
        let change = match (&ui.repo.listing, ui.repo.rows.get(index)) {
            (Listing::Ready(listing), Some(ListRow::File { change, .. })) => listing
                .changes
                .get(*change)
                .map(|change| (listing.repo.clone(), change.clone())),
            _ => None,
        };
        let Some((repo, change)) = change else {
            self.open_pane_menu(pane_id, position, Anchor::TopLeft, window, cx);
            return;
        };
        ui.repo.select_row(index);
        let environment = ui.environment.clone();
        let (scope, kind) = (change.scope, change.kind);
        let mut entries: Vec<controls::MenuEntry> = Vec::new();
        let (pane, path) = (pane_id.clone(), change.path.clone());
        entries.push(
            controls::menu_item(
                "Open diff",
                cx.listener(move |this, _, window, cx| {
                    this.open_diff(&pane, path.clone(), Some(scope), None, window, cx);
                    cx.notify();
                }),
            )
            .into(),
        );
        if kind != GitChangeKind::Deleted {
            let (pane, file) = (pane_id.clone(), work_tree_path(&repo, &change.path));
            entries.push(
                controls::menu_item(
                    "Open file",
                    cx.listener(move |this, _, window, cx| {
                        this.open_content_file(
                            &pane,
                            environment.clone(),
                            None,
                            Some(file.clone()),
                            None,
                            false,
                            window,
                            cx,
                        );
                        cx.notify();
                    }),
                )
                .into(),
            );
        }
        entries.push(controls::MenuEntry::Separator);
        let (pane, paths) = (pane_id.clone(), change_paths(&change).collect::<Vec<_>>());
        entries.push(
            controls::menu_item(
                match (scope, kind) {
                    (GitScope::Staged, _) => "Unstage",
                    (_, GitChangeKind::Conflicted) => "Stage (marks it resolved)",
                    _ => "Stage",
                },
                cx.listener(move |this, _, window, cx| {
                    this.move_changes(&pane, scope, paths.clone(), false, window, cx);
                }),
            )
            .into(),
        );
        if scope == GitScope::Unstaged && kind != GitChangeKind::Conflicted {
            let pane = pane_id.clone();
            entries.push(
                controls::menu_item(
                    if kind == GitChangeKind::Untracked {
                        "Delete file…"
                    } else {
                        "Discard changes…"
                    },
                    cx.listener(move |this, _, _, cx| {
                        this.confirm_discard(&pane, std::slice::from_ref(&change));
                        cx.notify();
                    }),
                )
                .into(),
            );
        }
        entries.push(controls::MenuEntry::Separator);
        entries.push(self.pane_submenu(pane_id, cx));
        self.open_menu(position, Anchor::TopLeft, entries, window, cx);
    }

    /// A diff tab's hunk button: the hunk moves into or out of the index, if the index
    /// text still is the one the diff shows.
    fn hunk_clicked(
        &mut self,
        pane_id: &PaneId,
        tab: &ContentTab,
        event: &HunkClicked,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((write, index_path, repo)) = tab.diff_scope().and_then(|scope| {
            let status = &self.content.panes.get(pane_id)?.repo;
            let (write, index_path) = hunk_target(status.change(tab.path(), scope)?)?;
            Some((write, index_path, status.listing()?.repo.clone()))
        }) else {
            // Nothing to write: the hunk's button stops waiting.
            if let Some(state) = self
                .content
                .panes
                .get(pane_id)
                .and_then(|ui| ui.diffs.get(tab))
            {
                for view in state.views() {
                    view.update(cx, |view, cx| view.finish_hunk_write(cx));
                }
            }
            return;
        };
        let action = set_index_lines(&event.diff, &event.hunk, write, index_path);
        self.send_write(
            pane_id,
            repo,
            action,
            WriteOrigin::Hunk(tab.clone()),
            window,
            cx,
        );
    }

    /// Commits what is staged with the box's message, or amends `HEAD` with it.
    fn commit(&mut self, pane_id: &PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        let Listing::Ready(listing) = &ui.repo.listing else {
            return;
        };
        let repo = listing.repo.clone();
        let staged = listing
            .changes
            .iter()
            .any(|change| change.scope == GitScope::Staged);
        let Some(commit) = ui.repo.commit.as_mut() else {
            return;
        };
        if commit.committing {
            return;
        }
        let refusal = commit_refusal(staged, commit.amend, commit.has_summary);
        if let Some(refusal) = refusal {
            ui.repo.outcome = Some(Outcome::Notice(refusal.into(), Instant::now()));
            cx.notify();
            return;
        }
        let message = commit_message(
            &commit.summary.read(cx).value(),
            &commit.body.read(cx).value(),
        );
        let amend = commit.amend;
        commit.committing = true;
        self.git_write(
            pane_id,
            repo,
            GitAction::Commit { message, amend },
            window,
            cx,
        );
    }

    /// Amend on: `HEAD`'s message replaces the box's draft, which is kept. Off: the draft
    /// comes back.
    fn toggle_amend(&mut self, pane_id: &PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        let Listing::Ready(listing) = &ui.repo.listing else {
            return;
        };
        let (repo, born) = (listing.repo.clone(), listing.head.is_some());
        let Some(commit) = ui.repo.commit.as_mut() else {
            return;
        };
        if commit.amend {
            commit.restore_draft(window, cx);
            cx.notify();
            return;
        }
        if !born {
            return;
        }
        commit.amend = true;
        commit.draft = Some((
            commit.summary.read(cx).value().to_string(),
            commit.body.read(cx).value().to_string(),
        ));
        self.git_write(pane_id, repo, GitAction::LastMessage, window, cx);
    }

    /// Keys in a commit box's field: Tab and Shift+Tab move on through the box's controls
    /// (the description would take Tab as text), Ctrl/Cmd+Enter commits. Whether the key
    /// was taken.
    pub(super) fn commit_box_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = &key.modifiers;
        let Some(pane_id) = self.focused_pane().cloned() else {
            return false;
        };
        let Some(ui) = self.content.panes.get(&pane_id) else {
            return false;
        };
        let focused = ui.repo.commit.as_ref().is_some_and(|commit| {
            commit.summary.focus_handle(cx).is_focused(window)
                || commit.body.focus_handle(cx).is_focused(window)
        });
        if !focused {
            return false;
        }
        if key.key == "tab" && !modifiers.control && !modifiers.platform && !modifiers.alt {
            let focus = ui.focus.clone();
            controls::cycle_focus(&focus, !modifiers.shift, window, cx);
            return true;
        }
        let primary = if cfg!(target_os = "macos") {
            modifiers.platform && !modifiers.control
        } else {
            modifiers.control && !modifiers.platform
        };
        if key.key != "enter" || !primary || modifiers.alt || modifiers.shift {
            return false;
        }
        self.commit(&pane_id, window, cx);
        true
    }

    /// Keys for the Changes list while `pane_id` itself has the keyboard and shows the
    /// list: Up and Down select a row; Enter opens the selected file's diff or folds the
    /// selected section or folder; Left and Right fold and unfold it, or move out of or
    /// into it; `s` stages and `u` unstages the selected file, folder or section; Delete
    /// discards the selected unstaged file, asking first. Without a selection, only Up
    /// and Down are taken, so a diff tab keeps the rest. Whether the key was taken.
    pub(super) fn changes_list_key(
        &mut self,
        pane_id: &PaneId,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = &key.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.shift {
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
        if ui.mode != SidebarMode::Changes || (tab_open && !ui.sidebar) {
            return false;
        }
        let status = &mut ui.repo;
        if status.rows.is_empty() {
            return false;
        }
        let at = status.selected_row();
        match key.key.as_str() {
            "up" | "down" => {
                if let Some(next) = step_row(status.rows.len(), at, key.key == "down") {
                    status.select_row(next);
                }
                true
            }
            "enter" => {
                let Some(at) = at else {
                    return false;
                };
                self.activate_change_row(pane_id, at, window, cx);
                true
            }
            "left" | "right" => {
                let Some(at) = at else {
                    return false;
                };
                match sideways(&status.rows, at, key.key == "right") {
                    Sideways::Toggle => self.activate_change_row(pane_id, at, window, cx),
                    Sideways::Select(row) => status.select_row(row),
                    Sideways::Stay => {}
                }
                true
            }
            "s" | "u" => {
                let Some(at) = at else {
                    return false;
                };
                let scope = if key.key == "s" {
                    GitScope::Unstaged
                } else {
                    GitScope::Staged
                };
                let row = &status.rows[at];
                let section = matches!(row, ListRow::Section { .. });
                let paths = row_paths(row, status.changes(), scope);
                self.move_changes(pane_id, scope, paths, section, window, cx);
                true
            }
            "delete" => {
                let Some(at) = at else {
                    return false;
                };
                if let ListRow::File { change, .. } = status.rows[at] {
                    let change = status.changes()[change].clone();
                    if change.scope == GitScope::Unstaged
                        && change.kind != GitChangeKind::Conflicted
                    {
                        self.confirm_discard(pane_id, std::slice::from_ref(&change));
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// What clicking row `at` of the Changes list does: a file opens its diff beside the
    /// list, a section or folder folds or unfolds.
    fn activate_change_row(
        &mut self,
        pane_id: &PaneId,
        at: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        let Some(key) = ui
            .repo
            .rows
            .get(at)
            .map(|row| row_key(row, ui.repo.changes()))
        else {
            return;
        };
        match key {
            RowKey::Section(scope) => self.toggle_changes_section(pane_id, scope),
            RowKey::Folder(scope, path) => self.toggle_changes_folder(pane_id, scope, path),
            RowKey::File(scope, path) => {
                ui.sidebar = true;
                self.open_diff(pane_id, path, Some(scope), None, window, cx);
            }
        }
    }

    fn set_changes_settings(&mut self, settings: ChangesSettings, cx: &mut Context<Self>) {
        let previous = std::mem::replace(&mut self.content.changes, settings);
        for ui in self.content.panes.values_mut() {
            if previous.tree != settings.tree {
                ui.repo.rebuild(settings.tree);
            }
            if previous.diff != settings.diff {
                for view in ui.diffs.values().flat_map(DiffTab::views) {
                    view.update(cx, |view, cx| view.set_settings(settings.diff, cx));
                }
            }
        }
        cx.notify();
    }

    fn set_sidebar_mode(&mut self, pane_id: &PaneId, mode: SidebarMode) {
        if let Some(ui) = self.content.panes.get_mut(pane_id) {
            ui.mode = mode;
        }
    }

    fn toggle_changes_section(&mut self, pane_id: &PaneId, scope: GitScope) {
        let tree = self.content.changes.tree;
        if let Some(ui) = self.content.panes.get_mut(pane_id) {
            let folds = &mut ui.repo.folds;
            if !folds.sections.remove(&scope) {
                folds.sections.insert(scope);
            }
            ui.repo.rebuild(tree);
        }
    }

    fn toggle_changes_folder(&mut self, pane_id: &PaneId, scope: GitScope, path: String) {
        let tree = self.content.changes.tree;
        if let Some(ui) = self.content.panes.get_mut(pane_id) {
            let key = (scope, path);
            let folds = &mut ui.repo.folds;
            if !folds.folders.remove(&key) {
                folds.folders.insert(key);
            }
            ui.repo.rebuild(tree);
        }
    }

    /// `[` and `]` in Diff mode: the previous or next change in list order, across
    /// sections, in its file's tab. A file not open takes the place of the current tab when
    /// that tab was only reviewed (its file never edited), else opens beside it.
    fn step_diff(&mut self, pane_id: &PaneId, forward: bool) {
        let Some(mut content) = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane_id))
            .cloned()
        else {
            return;
        };
        let Some((path, scope)) = content
            .active_diff()
            .map(|(path, scope)| (path.to_owned(), scope))
        else {
            return;
        };
        let tree = self.content.changes.tree;
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        let editing = ui.editor(&path).is_some();
        let status = &mut ui.repo;
        let Some(listing) = status.listing() else {
            return;
        };
        let repo = listing.repo.clone();
        let relative = repo_relative(&repo, &path);
        let changes = status.changes();
        let order: Vec<(&str, GitScope)> = status
            .order
            .iter()
            .map(|&index| (changes[index].path.as_str(), changes[index].scope))
            .collect();
        let Some(last) = order.len().checked_sub(1) else {
            return;
        };
        let at = order
            .iter()
            .position(|&(listed, at)| Some(listed) == relative.as_deref() && at == scope);
        let next = match at {
            Some(at) if forward => (at + 1).min(last),
            Some(at) => at.saturating_sub(1),
            None => 0,
        };
        if Some(next) == at {
            return;
        }
        let (next_path, next_scope) = (order[next].0.to_owned(), order[next].1);
        status.reveal(&next_path, next_scope, tree);
        let file = content
            .files()
            .find(|open| repo_relative(&repo, open).as_deref() == Some(next_path.as_str()))
            .map_or_else(|| work_tree_path(&repo, &next_path), str::to_owned);
        let next = ContentTab::diff(file.clone(), next_scope);
        if content.tab(&file).is_none()
            && let Some(index) = content.tabs.iter().position(|tab| tab.path() == path)
        {
            if !editing {
                content.tabs[index] = next.clone();
            } else if content.tabs.len() < compi_protocol::MAX_CONTENT_FILES {
                content.tabs.insert(index + 1, next.clone());
            } else {
                self.global_error =
                    Some("Close a tab first: this pane has as many as it can hold".into());
                return;
            }
        }
        content.show(next);
        self.update_content(pane_id, content);
    }

    /// Keys while a tab in Diff mode is active and the pane itself has the keyboard:
    /// `[`/`]` step through the changes, `s` stages the file of an unstaged diff and `u`
    /// unstages the file of a staged one, Ctrl/Cmd+F finds in the diff, and the diff takes
    /// the rest it knows (`n`/`p` move through hunks, Enter on the current hunk is its
    /// Stage hunk or Unstage hunk button).
    pub(super) fn diff_tab_key_down(
        &mut self,
        pane_id: &PaneId,
        tab: &ContentTab,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = &event.keystroke;
        let modifiers = &key.modifiers;
        if !modifiers.control && !modifiers.platform && !modifiers.alt {
            let typed = key.key_char.as_deref().unwrap_or(key.key.as_str());
            if matches!(typed, "[" | "]") {
                self.step_diff(pane_id, typed == "]");
                return true;
            }
            let wanted = match typed {
                "s" => Some(GitScope::Unstaged),
                "u" => Some(GitScope::Staged),
                _ => None,
            };
            if wanted.is_some() && wanted == tab.diff_scope() {
                self.move_tab_file(pane_id, tab, window, cx);
                return true;
            }
        }
        let view = self
            .content
            .panes
            .get(pane_id)
            .and_then(|ui| ui.diffs.get(tab))
            .and_then(DiffTab::view)
            .cloned();
        view.is_some_and(|view| view.update(cx, |view, cx| view.key_down(event, window, cx)))
    }

    /// Stages the file of an unstaged diff tab, unstages the file of a staged one.
    fn move_tab_file(
        &mut self,
        pane_id: &PaneId,
        tab: &ContentTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(scope) = tab.diff_scope() else {
            return;
        };
        let Some(paths) = self
            .content
            .panes
            .get(pane_id)
            .and_then(|ui| ui.repo.change(tab.path(), scope))
            .map(|change| change_paths(change).collect())
        else {
            return;
        };
        self.move_changes(pane_id, scope, paths, false, window, cx);
    }

    /// The sidebar's Files | Changes switch, shown once the folder is known to be in a
    /// work tree (or while Changes shows why it is not).
    pub(super) fn render_sidebar_switch(
        &self,
        pane_id: &PaneId,
        ui: &PaneUi,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let repository = matches!(ui.repo.listing, Listing::Ready(_));
        if !repository && ui.mode == SidebarMode::Files {
            return None;
        }
        let count = ui.repo.listing().map_or(0, |listing| listing.changes.len());
        let option = |mode: SidebarMode, id: &'static str, label: SharedString| {
            let pane = pane_id.clone();
            text_button(id, label)
                .selected(ui.mode == mode)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.set_sidebar_mode(&pane, mode);
                    this.release_editor(&pane, window, cx);
                    cx.notify();
                }))
        };
        let changes = if count > 0 {
            SharedString::from(format!("Changes {count}"))
        } else {
            SharedString::new_static("Changes")
        };
        Some(
            div()
                .flex_none()
                .px_2()
                .pt_1()
                .pb_1()
                .flex()
                .child(controls::segmented(
                    "content-sidebar-mode",
                    [
                        option(SidebarMode::Files, "sidebar-files", "Files".into()),
                        option(SidebarMode::Changes, "sidebar-changes", changes),
                    ],
                ))
                .into_any_element(),
        )
    }

    /// The Changes list of a pane's sidebar or folder view: branch and totals above the
    /// sections, or what keeps them from showing.
    pub(super) fn render_changes_list(
        &self,
        pane_id: &PaneId,
        ui: &PaneUi,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let repo = &ui.repo;
        let listing = match &repo.listing {
            Listing::Reading => {
                let message = self.changes_message("Reading changes…", None, None, cx);
                return self.without_listing(pane_id, "Reading the repository…", message, cx);
            }
            Listing::NotRepo => {
                let message = self.changes_message(
                    "Not a Git repository",
                    Some((repo.root.clone(), colors.muted)),
                    None,
                    cx,
                );
                return self.without_listing(
                    pane_id,
                    "This folder is not in a Git repository",
                    message,
                    cx,
                );
            }
            Listing::Failed(error) => {
                let message = self.changes_message(
                    "Cannot read the changes",
                    Some((error.clone(), colors.error)),
                    Some(pane_id),
                    cx,
                );
                return self.without_listing(pane_id, "The repository cannot be read", message, cx);
            }
            Listing::Ready(listing) => listing,
        };
        repo.list_width.set(width);
        let tree = self.content.changes.tree;
        let (files, added, removed) = repo.totals;
        let tree_button = icon_button(
            "changes-tree",
            if tree {
                IconName::List
            } else {
                IconName::ListTree
            },
            if tree { "Show as list" } else { "Show as tree" },
        )
        .header()
        .selected(tree)
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            let mut settings = this.content.changes;
            settings.tree = !settings.tree;
            this.set_changes_settings(settings, cx);
        }));
        let header = div()
            .flex_none()
            .pl_3()
            .pr_1()
            .py(px(2.0))
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_2()
            .text_size(px(UI_MICRO_TEXT_SIZE))
            .text_color(color(colors.muted))
            .whitespace_nowrap()
            .child(self.render_branch_status(pane_id, listing, cx))
            .when(!listing.changes.is_empty(), |header| {
                header
                    .child(div().flex_none().child(if files == 1 {
                        "1 file".to_owned()
                    } else {
                        format!("{files} files")
                    }))
                    .children(line_counts(added, removed, &colors).map(|counts| {
                        div()
                            .flex_none()
                            .font_family(self.typography.font.family.clone())
                            .child(counts)
                    }))
            })
            .when(repo.marked, |header| {
                header.child(div().flex_none().child("Refreshing…"))
            })
            .when_some(repo.refresh_error.clone(), |header, error| {
                header.child(
                    div()
                        .id("changes-refresh-error")
                        .flex_none()
                        .text_color(color(colors.error))
                        .child("Refresh failed")
                        .tooltip(controls::tooltip(
                            "The repository could not be read again",
                            Some(error.into()),
                            None,
                        )),
                )
            })
            .child(
                // One group, so the actions wrap together and stay at the right.
                div()
                    .flex_none()
                    .ml_auto()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .children(self.remote_controls(pane_id, Ok(listing), cx))
                    .child(tree_button),
            );
        let body = if listing.changes.is_empty() {
            self.changes_message(
                "No changes",
                Some(("The working tree and index match HEAD".into(), colors.muted)),
                None,
                cx,
            )
        } else {
            let pane = pane_id.clone();
            uniform_list(
                "changes-list",
                repo.rows.len(),
                cx.processor(move |this, range: Range<usize>, window, cx| {
                    // The selection shows while the keyboard is the list's.
                    let keyboard = this
                        .content
                        .panes
                        .get(&pane)
                        .is_some_and(|ui| ui.focus.is_focused(window));
                    this.render_change_rows(&pane, range, keyboard, cx)
                }),
            )
            .track_scroll(&repo.scroll)
            .flex_1()
            .min_h_0()
            .py_1()
            .into_any_element()
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .child(header)
            .children(self.render_commit_box(pane_id, repo, listing, cx))
            .child(body)
            .when(listing.truncated, |list| {
                list.child(
                    div()
                        .flex_none()
                        .px_3()
                        .py_1()
                        .border_t_1()
                        .border_color(color(colors.border))
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .text_color(color(colors.muted))
                        .child(format!(
                            "Showing the first {} changes",
                            compi_protocol::MAX_GIT_CHANGES
                        )),
                )
            })
            .into_any_element()
    }

    /// The commit box: summary, description, Amend, the staged count and Commit, then the
    /// last write's line. Nothing until a frame has built its fields.
    fn render_commit_box(
        &self,
        pane_id: &PaneId,
        repo: &RepoStatus,
        listing: &GitChanges,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let commit = repo.commit.as_ref()?;
        let colors = *self.colors();
        let staged = listing
            .changes
            .iter()
            .filter(|change| change.scope == GitScope::Staged)
            .count();
        let born = listing.head.is_some();
        let refusal = commit_refusal(staged > 0, commit.amend, commit.has_summary);
        let pane = pane_id.clone();
        let app = cx.entity().downgrade();
        let amend = controls::checkbox("commit-amend", "Amend", commit.amend)
            .header()
            .tooltip("Amend the last commit")
            .detail("Loads its message; staged changes join it")
            .disabled_because((!born).then_some("There is no commit yet"))
            .on_toggle(move |_, window, cx| {
                app.update(cx, |this, cx| {
                    this.toggle_amend(&pane, window, cx);
                    cx.notify();
                })
                .ok();
            });
        let pane = pane_id.clone();
        let button = text_button(
            "commit-button",
            if commit.amend { "Amend" } else { "Commit" },
        )
        .primary()
        .header()
        .key("secondary-enter")
        .loading(commit.committing)
        .disabled_because(refusal)
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.commit(&pane, window, cx);
            cx.notify();
        }));
        let outcome = repo.outcome.as_ref().map(|outcome| {
            let (text, tint, mono) = match outcome {
                Outcome::Notice(text, _) => (text.clone(), colors.muted, false),
                Outcome::Error(text) => (text.clone(), colors.error, true),
            };
            div()
                .id("commit-outcome")
                .max_h(px(96.0))
                .overflow_y_scroll()
                .text_size(px(UI_MICRO_TEXT_SIZE))
                .text_color(color(tint))
                .when(mono, |line| {
                    line.font_family(self.typography.font.family.clone())
                })
                .child(text)
        });
        Some(
            div()
                .flex_none()
                .px_2()
                .pt_1()
                .pb_2()
                .flex()
                .flex_col()
                .gap_1()
                .border_b_1()
                .border_color(color(colors.border))
                .child(Input::new(&commit.summary).appearance(true))
                .child(Textarea::new(&commit.body).appearance(true))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .child(amend)
                        .child(
                            // Gives way (elided) when a narrow list's button grows its spinner.
                            div()
                                .ml_auto()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_color(color(colors.muted))
                                .child(format!("{staged} staged")),
                        )
                        .child(button),
                )
                .children(outcome)
                .into_any_element(),
        )
    }

    fn changes_message(
        &self,
        title: &'static str,
        detail: Option<(String, u32)>,
        retry: Option<&PaneId>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let retry = retry.cloned();
        div()
            .flex_1()
            .min_h_0()
            .p_4()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .text_color(color(colors.muted))
            .child(div().text_size(px(UI_BODY_TEXT_SIZE)).child(title))
            .when_some(detail, |message, (detail, tint)| {
                message.child(
                    div()
                        .max_w(px(480.0))
                        .text_center()
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .text_color(color(tint))
                        .child(detail),
                )
            })
            .when_some(retry, |message, pane_id| {
                message.child(
                    div().mt_1().child(
                        text_button("changes-retry", "Try again")
                            .header()
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.retry_repo(&pane_id, window, cx);
                                cx.notify();
                            })),
                    ),
                )
            })
            .into_any_element()
    }

    /// The list rows in `range`, drawn as the list scrolls to them. While `keyboard` (the
    /// pane itself has the keyboard) the selected row shows an outline and its actions.
    fn render_change_rows(
        &self,
        pane_id: &PaneId,
        range: Range<usize>,
        keyboard: bool,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(repo) = self.content.panes.get(pane_id).map(|ui| &ui.repo) else {
            return Vec::new();
        };
        let Listing::Ready(listing) = &repo.listing else {
            return Vec::new();
        };
        let Some(rows) = repo.rows.get(range.clone()) else {
            return Vec::new();
        };
        let active = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane_id))
            .and_then(|content| content.active_diff())
            .and_then(|(file, scope)| Some((repo_relative(&listing.repo, file)?, scope)));
        let colors = *self.colors();
        let tree = self.content.changes.tree;
        let row_height = self
            .typography
            .cell_height
            .max(controls::ControlSize::Row.height());
        let list_width = repo.list_width.get();
        let mono = self.typography.font.family.clone();
        let chosen_row = repo.selected_row().filter(|_| keyboard);
        rows.iter()
            .zip(range)
            .map(|(row, index)| {
                let chosen = chosen_row == Some(index);
                // The keyboard selection is an outline, apart from the open diff's fill.
                let outline = if chosen {
                    color(colors.accent)
                } else {
                    gpui::transparent_black()
                };
                let pane = pane_id.clone();
                let menu_pane = pane_id.clone();
                let base = div()
                    .id(("changes-row", index))
                    .group("changes-row")
                    .relative()
                    .h(px(row_height))
                    .w_full()
                    .flex_none()
                    .pr_2()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if let Some(ui) = this.content.panes.get_mut(&pane) {
                            ui.repo.select_row(index);
                        }
                        this.release_editor(&pane, window, cx);
                        this.activate_change_row(&pane, index, window, cx);
                        cx.notify();
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.open_change_row_menu(
                                &menu_pane,
                                index,
                                event.position,
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                        }),
                    );
                let item = div()
                    .h(px(row_height - 2.0))
                    .flex_1()
                    .min_w_0()
                    .px_1()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(outline)
                    .whitespace_nowrap();
                match row {
                    ListRow::Section {
                        scope,
                        count,
                        collapsed,
                    } => {
                        let scope = *scope;
                        let (icon, title, key) = match scope {
                            GitScope::Unstaged => (IconName::Plus, "Stage all", "s"),
                            GitScope::Staged => (IconName::Minus, "Unstage all", "u"),
                        };
                        let moving = repo.moving_section(scope);
                        let mover = pane_id.clone();
                        let move_all = icon_button(("changes-section-move", index), icon, title)
                            .key(key)
                            .tab_stop(false)
                            .loading(moving)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                let paths = this.section_paths(&mover, scope);
                                this.move_changes(&mover, scope, paths, true, window, cx);
                            }));
                        base.pl(px(TREE_INSET))
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(color(colors.muted))
                            .hover(move |style| style.text_color(color(colors.foreground)))
                            .child(
                                item.child(fold_chevron(*collapsed, color(colors.muted)))
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(scope_label(scope)),
                                    )
                                    .child(div().ml_2().child(count.to_string())),
                            )
                            .child(row_controls(chosen || moving, &colors, [move_all]))
                            .into_any_element()
                    }
                    ListRow::Folder {
                        label,
                        depth,
                        collapsed,
                        ..
                    } => base
                        .pl(px(TREE_INSET + *depth as f32 * TREE_INDENT))
                        .children(indent_guides(*depth, colors.border))
                        .child(
                            item.group_hover("changes-row", move |style| {
                                style.bg(color(colors.surface_hover))
                            })
                            .child(fold_chevron(*collapsed, color(colors.muted)))
                            .child(tree_entry_icon(true, !collapsed, color(colors.accent)))
                            .child(
                                div()
                                    .ml_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_color(color(colors.muted))
                                    .child(label.clone()),
                            ),
                        )
                        .into_any_element(),
                    ListRow::File { change, depth } => {
                        let change = &listing.changes[*change];
                        let open = active.as_ref().is_some_and(|(path, scope)| {
                            *path == change.path && *scope == change.scope
                        });
                        let (folder, name) = split_path(&change.path);
                        let level = if tree { *depth } else { 0 };
                        let inset = TREE_INSET + level as f32 * TREE_INDENT;
                        let counts = change
                            .added
                            .zip(change.removed)
                            .map(|(added, removed)| (u64::from(added), u64::from(removed)));
                        let counts_chars = counts.map_or(0, |(added, removed)| {
                            let digits = |value: u64| value.to_string().len();
                            (if added > 0 { digits(added) + 2 } else { 0 })
                                + if removed > 0 { digits(removed) + 2 } else { 0 }
                        });
                        // What the folder may take once the letter, name and counts have
                        // theirs.
                        let room = list_width
                            - inset
                            - 8.0 * 3.0
                            - 14.0
                            - name.chars().count() as f32 * SMALL_GLYPH_WIDTH
                            - counts_chars as f32 * MICRO_GLYPH_WIDTH;
                        let folder = (!tree && !folder.is_empty())
                            .then(|| truncate_middle(folder, (room / MICRO_GLYPH_WIDTH) as usize))
                            .filter(|folder| folder.chars().count() >= 4);
                        let scope = change.scope;
                        let deleted = change.kind == GitChangeKind::Deleted;
                        let kind = change.kind;
                        let moving = repo.moving(scope, &change.path);
                        let discarding = repo.discarding(&change.path);
                        let mut actions = Vec::new();
                        if scope == GitScope::Unstaged && kind != GitChangeKind::Conflicted {
                            let (discarder, discarded) = (pane_id.clone(), change.clone());
                            actions.push(
                                icon_button(
                                    ("changes-row-discard", index),
                                    IconName::RotateCcw,
                                    if kind == GitChangeKind::Untracked {
                                        "Delete file…"
                                    } else {
                                        "Discard changes…"
                                    },
                                )
                                .key("delete")
                                .tab_stop(false)
                                .loading(discarding)
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _, cx| {
                                        this.confirm_discard(
                                            &discarder,
                                            std::slice::from_ref(&discarded),
                                        );
                                        cx.notify();
                                    },
                                )),
                            );
                        }
                        let (mover, paths) =
                            (pane_id.clone(), change_paths(change).collect::<Vec<_>>());
                        let (icon, key) = match scope {
                            GitScope::Unstaged => (IconName::Plus, "s"),
                            GitScope::Staged => (IconName::Minus, "u"),
                        };
                        actions.push(
                            icon_button(
                                ("changes-row-move", index),
                                icon,
                                match (scope, kind) {
                                    (GitScope::Staged, _) => "Unstage",
                                    (_, GitChangeKind::Conflicted) => "Stage (marks it resolved)",
                                    _ => "Stage",
                                },
                            )
                            .key(key)
                            .tab_stop(false)
                            .loading(moving)
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, window, cx| {
                                    this.move_changes(
                                        &mover,
                                        scope,
                                        paths.clone(),
                                        false,
                                        window,
                                        cx,
                                    );
                                },
                            )),
                        );
                        let actions_shown = chosen || moving || discarding;
                        base.pl(px(inset))
                            .when(tree, |row| {
                                row.children(indent_guides(level, colors.border))
                            })
                            .child(
                                item.gap_1()
                                    .when(open, |item| item.bg(color(colors.selection)))
                                    .when(!open, |item| {
                                        item.group_hover("changes-row", move |style| {
                                            style.bg(color(colors.surface_hover))
                                        })
                                    })
                                    .child(
                                        div()
                                            .w(px(14.0))
                                            .flex_none()
                                            .font_family(mono.clone())
                                            .text_size(px(UI_MICRO_TEXT_SIZE))
                                            .text_color(color(kind_color(kind, &colors)))
                                            .child(status_letter(kind)),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .text_color(color(if deleted && !open {
                                                colors.muted
                                            } else {
                                                colors.foreground
                                            }))
                                            .when(deleted, |name| name.line_through())
                                            .child(name.to_owned()),
                                    )
                                    .when_some(folder, |item, folder| {
                                        item.child(
                                            div()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .text_size(px(UI_MICRO_TEXT_SIZE))
                                                .text_color(color(colors.muted))
                                                .child(folder.into_owned()),
                                        )
                                    })
                                    .child(div().flex_1())
                                    .children(counts.and_then(|(added, removed)| {
                                        line_counts(added, removed, &colors).map(|counts| {
                                            div()
                                                .flex_none()
                                                .font_family(mono.clone())
                                                .text_size(px(UI_MICRO_TEXT_SIZE))
                                                .map(|counts| {
                                                    if actions_shown {
                                                        counts.invisible()
                                                    } else {
                                                        counts.group_hover("changes-row", |style| {
                                                            style.invisible()
                                                        })
                                                    }
                                                })
                                                .child(counts)
                                        })
                                    })),
                            )
                            .child(row_controls(actions_shown, &colors, actions))
                            .into_any_element()
                    }
                }
            })
            .collect()
    }

    /// What a tab in Diff mode compares, for its tooltip: `src/a.rs · working tree vs index`.
    pub(super) fn diff_tab_baseline(ui: &PaneUi, tab: &ContentTab) -> Option<String> {
        let scope = tab.diff_scope()?;
        let file = tab.path();
        let path = ui
            .repo
            .listing()
            .and_then(|listing| repo_relative(&listing.repo, file))
            .unwrap_or_else(|| file.to_owned());
        let compared = ui
            .repo
            .change(file, scope)
            .map_or_else(|| scope_baseline(scope).to_owned(), baseline);
        Some(format!("{path} · {}", lowercase_first(&compared)))
    }

    /// A tab's body in Diff mode: the change's header with the Edit / Diff switch and the
    /// diff controls, then its diff or what keeps it from showing.
    pub(super) fn render_diff_tab(
        &self,
        pane_id: &PaneId,
        ui: &PaneUi,
        tab: &ContentTab,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(scope) = tab.diff_scope() else {
            return div().into_any_element();
        };
        let file = tab.path();
        // The repository path once the listing places the file, else the path as opened.
        let relative = ui
            .repo
            .listing()
            .and_then(|listing| repo_relative(&listing.repo, file));
        let path = relative.as_deref().unwrap_or(file);
        let colors = *self.colors();
        let change = ui.repo.change(file, scope);
        let state = ui.diffs.get(tab);
        let body = div().flex_1().min_h_0().relative().flex().flex_col();
        let muted_line = |text: String| {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .text_color(color(colors.muted))
                .child(text)
                .into_any_element()
        };
        let body = match (&ui.repo.listing, change) {
            (Listing::Reading, _) => muted_line("Reading changes…".into()),
            (Listing::NotRepo, _) => muted_line(format!("Not a Git repository: {}", ui.repo.root)),
            (Listing::Failed(_), _) => {
                self.changes_message("Cannot read the changes", None, Some(pane_id), cx)
            }
            (Listing::Ready(_), _) if relative.is_none() => {
                muted_line(format!("Not in this folder's repository: {file}"))
            }
            (Listing::Ready(_), None) => self.changes_message(
                "No changes",
                Some((
                    format!(
                        "{} · {}",
                        split_path(path).1,
                        scope_baseline(scope).to_lowercase()
                    ),
                    colors.muted,
                )),
                None,
                cx,
            ),
            (Listing::Ready(_), Some(_)) => {
                match state {
                    Some(DiffTab {
                        failed: Some((_, error)),
                        ..
                    }) => {
                        let retry = pane_id.clone();
                        body.p_3()
                            .gap_1()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .child(format!("Cannot read {}", split_path(path).1))
                            .child(div().text_color(color(colors.error)).child(error.clone()))
                            .child(div().mt_1().flex().child(
                                text_button("diff-retry", "Try again").header().on_click(
                                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                                        this.retry_repo(&retry, window, cx);
                                        cx.notify();
                                    }),
                                ),
                            ))
                            .into_any_element()
                    }
                    Some(state) if state.loading() => body
                        .p_3()
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .text_color(color(colors.muted))
                        .child(format!("Loading {}…", split_path(path).1))
                        .into_any_element(),
                    Some(DiffTab {
                        shown: Some(shown), ..
                    }) => body.child(shown.view.clone()).into_any_element(),
                    _ => body.into_any_element(),
                }
            }
        };
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(self.render_diff_header(pane_id, ui, tab, path, scope, change, state, cx))
            .child(body)
            .into_any_element()
    }

    /// The Edit / Diff switch of a file's tab, both options keyed to Diff this file. Diff
    /// needs an uncommitted change of the file in this folder's repository; Edit needs the
    /// file in the working tree.
    pub(super) fn render_mode_switch(
        &self,
        pane_id: &PaneId,
        ui: &PaneUi,
        tab: &ContentTab,
        cx: &Context<Self>,
    ) -> AnyElement {
        let file = tab.path();
        let diff = tab.diff_scope().is_some();
        let edit_reason = tab
            .diff_scope()
            .and_then(|scope| ui.repo.change(file, scope))
            .and_then(edit_disabled);
        let diff_reason = (!diff).then(|| ui.repo.diff_unavailable(file)).flatten();
        let (pane, path) = (pane_id.clone(), file.to_owned());
        let mut edit = text_button("file-mode-edit", "Edit")
            .selected(!diff)
            .detail("Edits the file at the line in view")
            .disabled_because(edit_reason)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                if diff {
                    this.edit_from_diff(&pane, &path, window, cx);
                    cx.notify();
                }
            }));
        let (pane, path) = (pane_id.clone(), file.to_owned());
        let mut show_diff = text_button("file-mode-diff", "Diff")
            .selected(diff)
            .detail("Shows the file's uncommitted changes at the cursor's line")
            .disabled_because(diff_reason)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                if !diff {
                    this.diff_from_edit(&pane, &path, window, cx);
                    cx.notify();
                }
            }));
        if let Some(key) = crate::commands::Command::DiffThisFile
            .spec()
            .shortcut(crate::commands::Platform::current())
        {
            edit = edit.key(key);
            show_diff = show_diff.key(key);
        }
        controls::segmented("file-mode", [edit, show_diff]).into_any_element()
    }

    /// Path, kind, scope, line counts and what the diff compares; the Edit / Diff switch,
    /// how diffs show; Stage or Unstage, and Discard.
    #[allow(clippy::too_many_arguments)]
    fn render_diff_header(
        &self,
        pane_id: &PaneId,
        ui: &PaneUi,
        tab: &ContentTab,
        path: &str,
        scope: GitScope,
        change: Option<&GitChange>,
        state: Option<&DiffTab>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        let settings = self.content.changes;
        let (folder, name) = split_path(path);
        // A diff whose change left the listing is not on screen; neither are its counts.
        let shown = state
            .and_then(DiffTab::view)
            .filter(|_| change.is_some())
            .cloned();
        let counts = shown
            .as_ref()
            .and_then(|view| view.read(cx).stats())
            .or(change.and_then(|change| change.added.zip(change.removed)))
            .map(|(added, removed)| (u64::from(added), u64::from(removed)));
        let compared = change.map_or_else(|| scope_baseline(scope).to_owned(), baseline);
        let (title, detail) = (path.to_owned(), SharedString::from(compared.clone()));
        let expanded = shown
            .as_ref()
            .is_some_and(|view| view.read(cx).context_expanded());
        let layouts = [
            (
                LayoutMode::Auto,
                "Auto",
                "Split when the pane is wide enough",
            ),
            (
                LayoutMode::Unified,
                "Unified",
                "Old and new lines in one column",
            ),
            (LayoutMode::Split, "Split", "Old and new side by side"),
        ];
        let layout = controls::segmented(
            "diff-layout",
            layouts
                .into_iter()
                .enumerate()
                .map(|(index, (mode, label, detail))| {
                    text_button(("diff-layout-mode", index), label)
                        .selected(settings.diff.layout == mode)
                        .tooltip(format!("{label} layout"))
                        .detail(detail)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let mut settings = this.content.changes;
                            settings.diff.layout = mode;
                            this.set_changes_settings(settings, cx);
                        }))
                }),
        );
        let whitespace = settings.diff.whitespace;
        let (whitespace_label, next_whitespace) = match whitespace {
            Whitespace::Exact => ("Exact whitespace", Whitespace::IgnoreSpace),
            Whitespace::IgnoreSpace => ("Ignore space", Whitespace::IgnoreFormatting),
            Whitespace::IgnoreFormatting => ("Ignore formatting", Whitespace::Exact),
        };
        let whitespace_button = text_button("diff-whitespace", whitespace_label)
            .header()
            .selected(whitespace != Whitespace::Exact)
            .tooltip("Whitespace")
            .detail("Switches between exact, ignoring changes in spacing and ignoring formatting-only changes")
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                let mut settings = this.content.changes;
                settings.diff.whitespace = next_whitespace;
                this.set_changes_settings(settings, cx);
            }));
        let mut context_button = icon_button(
            "diff-context",
            if expanded {
                IconName::ChevronsDownUp
            } else {
                IconName::ChevronsUpDown
            },
            if expanded {
                "Fold unchanged lines"
            } else {
                "Show all unchanged lines"
            },
        )
        .header()
        .key("e")
        .selected(expanded)
        .disabled_because(shown.is_none().then_some("The diff is not shown yet"));
        if let Some(view) = shown {
            context_button = context_button.on_click(move |_, _, cx| {
                view.update(cx, |view, cx| view.set_context_expanded(!expanded, cx));
            });
        }
        let mode_switch = self.render_mode_switch(pane_id, ui, tab, cx);
        // The diff reads the saved file (or the index), never the editor's buffer.
        let unsaved = ui.editor(tab.path()).is_some_and(|editor| editor.dirty);
        let status = self.content.panes.get(pane_id).map(|ui| &ui.repo);
        let (move_label, move_key, move_detail) = match scope {
            GitScope::Unstaged => ("Stage", "s", "Adds the whole file to the index"),
            GitScope::Staged => ("Unstage", "u", "Takes the whole file out of the index"),
        };
        let (move_pane, move_tab) = (pane_id.clone(), tab.clone());
        let move_button = change.map(|_| {
            text_button("diff-move-file", move_label)
                .header()
                .key(move_key)
                .detail(move_detail)
                .loading(status.is_some_and(|status| status.moving(scope, path)))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.move_tab_file(&move_pane, &move_tab, window, cx);
                }))
        });
        let discard_pane = pane_id.clone();
        let discard_button = change
            .filter(|change| {
                change.scope == GitScope::Unstaged && change.kind != GitChangeKind::Conflicted
            })
            .cloned()
            .map(|change| {
                let label = if change.kind == GitChangeKind::Untracked {
                    "Delete file…"
                } else {
                    "Discard changes…"
                };
                text_button("diff-discard", label)
                    .danger()
                    .header()
                    .detail("Asks first")
                    .loading(status.is_some_and(|status| status.discarding(&change.path)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.confirm_discard(&discard_pane, std::slice::from_ref(&change));
                        cx.notify();
                    }))
            });
        div()
            .flex_none()
            .min_h(px(FILE_TAB_HEIGHT))
            .py(px(3.0))
            .pl_3()
            .pr_1()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_2()
            .gap_y_1()
            .border_b_1()
            .border_color(color(colors.border))
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .whitespace_nowrap()
            .child(
                div()
                    .id("diff-header")
                    .flex_1()
                    .min_w(px(160.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .overflow_hidden()
                    .child(
                        div()
                            // The name stays readable; the folder and the baseline give way.
                            .flex_shrink_0()
                            .max_w(gpui::relative(0.7))
                            .min_w_0()
                            .flex()
                            .overflow_hidden()
                            .when(!folder.is_empty(), |path| {
                                path.child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .text_color(color(colors.muted))
                                        .child(format!("{folder}/")),
                                )
                            })
                            .child(div().flex_none().child(name.to_owned())),
                    )
                    .when_some(change, |header, change| {
                        header.child(controls::chip(
                            kind_label(change.kind),
                            ChipTone::Tint(kind_color(change.kind, &colors)),
                        ))
                    })
                    .when(scope == GitScope::Staged, |header| {
                        header.child(controls::chip("staged", ChipTone::Neutral))
                    })
                    .when(unsaved, |header| {
                        header.child(controls::chip("Unsaved edits not shown", ChipTone::Neutral))
                    })
                    .children(counts.and_then(|(added, removed)| {
                        line_counts(added, removed, &colors).map(|counts| {
                            div()
                                .flex_none()
                                .font_family(self.typography.font.family.clone())
                                .text_size(px(UI_MICRO_TEXT_SIZE))
                                .child(counts)
                        })
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(color(colors.muted))
                            .child(compared),
                    )
                    .tooltip(controls::tooltip(title, Some(detail), None)),
            )
            .child(
                // Shrinks to the pane and wraps there, so a narrow pane clips no control.
                div()
                    .min_w_0()
                    .min_h(px(controls::ControlSize::Header.height()))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .child(mode_switch)
                    .child(layout)
                    .child(whitespace_button)
                    .child(context_button)
                    .children(move_button)
                    .children(discard_button),
            )
    }
}

/// A section's or folder's fold mark: pointing right while folded, down while open.
fn fold_chevron(collapsed: bool, tint: Hsla) -> gpui::Div {
    div()
        .w(px(14.0))
        .flex_none()
        .flex()
        .items_center()
        .child(controls::glyph(
            if collapsed {
                IconName::ChevronRight
            } else {
                IconName::ChevronDown
            },
            tint,
        ))
}

/// `text` with its first letter in lower case, for captions joined mid-sentence.
fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

/// One guide per folder above a tree row, through the middle of its arrow.
fn indent_guides(levels: usize, border: u32) -> impl Iterator<Item = gpui::Div> {
    (0..levels.min(31)).map(move |guide| {
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(TREE_INSET + guide as f32 * TREE_INDENT + 11.0))
            .w(px(1.0))
            .bg(color(border))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(scope: GitScope, kind: GitChangeKind, path: &str) -> GitChange {
        GitChange {
            scope,
            kind,
            path: path.into(),
            old_path: None,
            added: Some(1),
            removed: Some(0),
            stamp: "s".into(),
        }
    }

    #[test]
    fn tree_marks_key_tree_paths_below_the_pane_folder() {
        use GitChangeKind::{Added, Deleted, Modified};
        use GitScope::{Staged, Unstaged};
        let listing = |repo: &str, changes: Vec<GitChange>| GitChanges {
            repo: repo.into(),
            branch: None,
            head: None,
            changes,
            truncated: false,
            ..Default::default()
        };
        let file = |kind, path: &str| {
            Some(TreeMark::File {
                kind,
                path: path.into(),
            })
        };
        let marks = tree_marks(
            "/home/me/proj/src",
            &listing(
                "/home/me/proj",
                vec![
                    change(Unstaged, Modified, "src/gui/a.rs"),
                    change(Unstaged, Deleted, "src/old/gone.rs"),
                    change(Unstaged, Modified, "README.md"),
                    change(Staged, Added, "src/gui/a.rs"),
                    change(Staged, Added, "src/new.rs"),
                ],
            ),
        );
        let mark = |path: &str| marks.get(path).cloned();
        assert_eq!(
            mark("/home/me/proj/src/gui/a.rs"),
            file(Modified, "src/gui/a.rs")
        );
        assert_eq!(mark("/home/me/proj/src/new.rs"), file(Added, "src/new.rs"));
        assert_eq!(mark("/home/me/proj/src/gui"), Some(TreeMark::Folder));
        assert_eq!(mark("/home/me/proj/src/old"), Some(TreeMark::Folder));
        assert_eq!(mark("/home/me/proj/src/old/gone.rs"), None);
        assert_eq!(marks.len(), 4);
        // A Windows folder in other letter case than Git's top level.
        let marks = tree_marks(
            r"c:\users\me\proj\",
            &listing(
                r"C:\Users\Me\Proj",
                vec![change(Unstaged, Modified, "src/a.rs")],
            ),
        );
        assert_eq!(
            marks.get(r"c:\users\me\proj\src\a.rs").cloned(),
            file(Modified, "src/a.rs")
        );
        assert_eq!(marks.get(r"c:\users\me\proj\src"), Some(&TreeMark::Folder));
    }

    #[test]
    fn trees_join_single_folder_chains_and_list_folders_before_files() {
        use GitChangeKind::Modified;
        use GitScope::Unstaged;
        let changes = [
            change(Unstaged, Modified, "README.md"),
            change(Unstaged, Modified, "crates/client/src/gui/a.rs"),
            change(Unstaged, Modified, "crates/client/src/gui/b.rs"),
            change(Unstaged, Modified, "crates/client/src/lib.rs"),
            change(Unstaged, Modified, "docs/guide/intro.md"),
        ];
        let (rows, order) = list_rows(&changes, true, &Folds::default());
        let labels: Vec<String> = rows
            .iter()
            .map(|row| match row {
                ListRow::Section { count, .. } => format!("section {count}"),
                ListRow::Folder { label, depth, .. } => format!("{depth} {label}/"),
                ListRow::File { change, depth } => format!("{depth} {}", changes[*change].path),
            })
            .collect();
        assert_eq!(
            labels,
            [
                "section 5",
                "0 crates/client/src/",
                "1 gui/",
                "2 crates/client/src/gui/a.rs",
                "2 crates/client/src/gui/b.rs",
                "1 crates/client/src/lib.rs",
                "0 docs/guide/",
                "1 docs/guide/intro.md",
                "0 README.md",
            ]
        );
        assert_eq!(order, [1, 2, 3, 4, 0]);
        // A folded folder hides its rows; stepping still reaches every change.
        let mut folds = Folds::default();
        folds
            .folders
            .insert((Unstaged, "crates/client/src".to_owned()));
        let (folded, folded_order) = list_rows(&changes, true, &folds);
        assert_eq!(folded.len(), 5);
        assert!(matches!(
            &folded[1],
            ListRow::Folder {
                collapsed: true,
                ..
            }
        ));
        assert_eq!(folded_order, order);
    }

    #[test]
    fn sections_follow_git_status_and_skip_empty_scopes() {
        use GitChangeKind::*;
        let changes = [
            change(GitScope::Unstaged, Modified, "b.rs"),
            change(GitScope::Unstaged, Untracked, "new.rs"),
            change(GitScope::Staged, Modified, "b.rs"),
        ];
        let (rows, order) = list_rows(&changes, false, &Folds::default());
        assert_eq!(
            rows,
            [
                ListRow::Section {
                    scope: GitScope::Unstaged,
                    count: 2,
                    collapsed: false
                },
                ListRow::File {
                    change: 0,
                    depth: 0
                },
                ListRow::File {
                    change: 1,
                    depth: 0
                },
                ListRow::Section {
                    scope: GitScope::Staged,
                    count: 1,
                    collapsed: false
                },
                ListRow::File {
                    change: 2,
                    depth: 0
                },
            ]
        );
        assert_eq!(order, [0, 1, 2]);
        let mut folds = Folds::default();
        folds.sections.insert(GitScope::Unstaged);
        let (rows, order) = list_rows(&changes, false, &folds);
        assert_eq!(rows.len(), 3);
        assert_eq!(order, [0, 1, 2]);
        let (rows, _) = list_rows(&changes[2..], false, &Folds::default());
        assert!(matches!(
            rows[0],
            ListRow::Section {
                scope: GitScope::Staged,
                ..
            }
        ));
    }

    #[test]
    fn sides_compare_the_scope_baselines() {
        use GitChangeKind::*;
        use GitRevision::*;
        let mut renamed = change(GitScope::Staged, Renamed, "new.rs");
        renamed.old_path = Some("old.rs".into());
        assert_eq!(
            sides(&renamed),
            [Some(("old.rs", Head)), Some(("new.rs", Index))]
        );
        assert_eq!(
            sides(&change(GitScope::Unstaged, Modified, "a")),
            [Some(("a", Index)), Some(("a", WorkTree))]
        );
        assert_eq!(
            sides(&change(GitScope::Unstaged, Untracked, "a")),
            [None, Some(("a", WorkTree))]
        );
        assert_eq!(
            sides(&change(GitScope::Unstaged, Deleted, "a")),
            [Some(("a", Index)), None]
        );
        assert_eq!(
            sides(&change(GitScope::Staged, Added, "a")),
            [None, Some(("a", Index))]
        );
        assert_eq!(
            sides(&change(GitScope::Staged, Deleted, "a")),
            [Some(("a", Head)), None]
        );
        assert_eq!(
            sides(&change(GitScope::Unstaged, Conflicted, "a")),
            [Some(("a", Head)), Some(("a", WorkTree))]
        );
    }

    #[test]
    fn commit_messages_join_and_split_at_the_blank_line() {
        assert_eq!(commit_message("  Fix it  ", ""), "Fix it");
        assert_eq!(commit_message("Fix it", " \n\n"), "Fix it");
        assert_eq!(
            commit_message("Fix it", "\nWhy:\n- because\n\n"),
            "Fix it\n\nWhy:\n- because"
        );
        assert_eq!(
            split_message("Fix it\n\nWhy:\n- because\n"),
            ("Fix it".to_owned(), "Why:\n- because".to_owned())
        );
        assert_eq!(
            split_message("Only a summary\n"),
            ("Only a summary".to_owned(), String::new())
        );
        assert_eq!(
            split_message("Windows\r\n\r\nbody\r\n"),
            ("Windows".to_owned(), "body".to_owned())
        );
        let (summary, body) = split_message("Fix it\n\nWhy:\n- because\n");
        assert_eq!(commit_message(&summary, &body), "Fix it\n\nWhy:\n- because");
    }

    #[test]
    fn hunks_move_only_between_two_index_versions() {
        use GitChangeKind::*;
        use GitScope::{Staged, Unstaged};
        assert_eq!(
            hunk_target(&change(Unstaged, Modified, "a.rs")),
            Some((HunkWrite::Stage, "a.rs".into()))
        );
        assert_eq!(
            hunk_target(&change(Staged, Modified, "a.rs")),
            Some((HunkWrite::Unstage, "a.rs".into()))
        );
        let renamed = GitChange {
            old_path: Some("old.rs".into()),
            ..change(Staged, Renamed, "new.rs")
        };
        assert_eq!(
            hunk_target(&renamed),
            Some((HunkWrite::Unstage, "new.rs".into()))
        );
        assert_eq!(
            change_paths(&renamed).collect::<Vec<_>>(),
            ["new.rs", "old.rs"]
        );
        for (scope, kind) in [
            (Unstaged, Untracked),
            (Unstaged, Conflicted),
            (Unstaged, Deleted),
            (Staged, Added),
            (Staged, Deleted),
        ] {
            assert_eq!(hunk_target(&change(scope, kind, "a.rs")), None, "{kind:?}");
        }
    }

    #[test]
    fn baselines_name_what_is_compared() {
        use GitChangeKind::*;
        assert_eq!(
            baseline(&change(GitScope::Unstaged, Modified, "a")),
            "Working tree vs index"
        );
        assert_eq!(
            baseline(&change(GitScope::Staged, Added, "a")),
            "Index vs HEAD"
        );
        assert_eq!(
            baseline(&change(GitScope::Unstaged, Untracked, "a")),
            "New file (untracked)"
        );
        assert_eq!(
            baseline(&change(GitScope::Unstaged, Conflicted, "a")),
            "Conflicted · working tree vs HEAD"
        );
        let mut renamed = change(GitScope::Staged, Renamed, "src/new.rs");
        renamed.old_path = Some("src/old.rs".into());
        assert_eq!(
            baseline(&renamed),
            "src/old.rs → src/new.rs · Index vs HEAD"
        );
    }

    #[test]
    fn folders_elide_their_middle() {
        assert_eq!(truncate_middle("src/gui", 7), "src/gui");
        assert_eq!(
            truncate_middle("crates/compi-client/src/gui", 11),
            "crate…c/gui"
        );
        assert_eq!(truncate_middle("añb/ü", 3), "a…ü");
        assert_eq!(truncate_middle("abc", 1), "…");
        assert_eq!(truncate_middle("abc", 0), "");
    }

    /// Unstaged `src/a.rs`, `src/b.rs` and untracked `README.md`; staged `src/a.rs`.
    fn selection_changes() -> Vec<GitChange> {
        use GitChangeKind::{Modified, Untracked};
        use GitScope::{Staged, Unstaged};
        vec![
            change(Unstaged, Modified, "src/a.rs"),
            change(Unstaged, Modified, "src/b.rs"),
            change(Unstaged, Untracked, "README.md"),
            change(Staged, Modified, "src/a.rs"),
        ]
    }

    #[test]
    fn up_and_down_stop_at_the_ends() {
        assert_eq!(step_row(8, None, true), Some(0));
        assert_eq!(step_row(8, None, false), Some(7));
        assert_eq!(step_row(8, Some(3), true), Some(4));
        assert_eq!(step_row(8, Some(3), false), Some(2));
        assert_eq!(step_row(8, Some(7), true), Some(7));
        assert_eq!(step_row(8, Some(0), false), Some(0));
        assert_eq!(step_row(0, None, true), None);
    }

    #[test]
    fn left_and_right_fold_or_move_out_and_in() {
        let changes = selection_changes();
        // 0 Unstaged, 1 src/, 2 a.rs, 3 b.rs, 4 README.md, 5 Staged, 6 src/, 7 a.rs.
        let (rows, _) = list_rows(&changes, true, &Folds::default());
        assert_eq!(rows.len(), 8);
        assert_eq!(sideways(&rows, 0, false), Sideways::Toggle);
        assert_eq!(sideways(&rows, 0, true), Sideways::Select(1));
        assert_eq!(sideways(&rows, 1, false), Sideways::Toggle);
        assert_eq!(sideways(&rows, 1, true), Sideways::Select(2));
        assert_eq!(sideways(&rows, 3, false), Sideways::Select(1));
        assert_eq!(sideways(&rows, 3, true), Sideways::Stay);
        assert_eq!(sideways(&rows, 4, false), Sideways::Select(0));
        let mut folds = Folds::default();
        folds.sections.insert(GitScope::Staged);
        let (folded, _) = list_rows(&changes, true, &folds);
        assert_eq!(sideways(&folded, 5, true), Sideways::Toggle);
        assert_eq!(sideways(&folded, 5, false), Sideways::Stay);
    }

    #[test]
    fn the_selection_keeps_its_row_across_listings_and_folds() {
        use GitScope::{Staged, Unstaged};
        let changes = selection_changes();
        let (rows, _) = list_rows(&changes, true, &Folds::default());
        let b = RowKey::File(Unstaged, "src/b.rs".into());
        assert_eq!(reselect(&rows, &changes, &b, 0), Some(3));
        assert_eq!(row_key(&rows[3], &changes), b);
        assert_eq!(
            reselect(&rows, &changes, &RowKey::Folder(Staged, "src".into()), 0),
            Some(6)
        );
        // A fold hiding the row takes the selection.
        let mut folds = Folds::default();
        folds.folders.insert((Unstaged, "src".into()));
        let (folded, _) = list_rows(&changes, true, &folds);
        assert_eq!(reselect(&folded, &changes, &b, 3), Some(1));
        folds.sections.insert(Unstaged);
        let (folded, _) = list_rows(&changes, true, &folds);
        assert_eq!(reselect(&folded, &changes, &b, 3), Some(0));
        // Staged: the row now at its place.
        let mut staged = changes.clone();
        staged[1].scope = Staged;
        let (rows, _) = list_rows(&staged, true, &Folds::default());
        assert_eq!(reselect(&rows, &staged, &b, 3), Some(3));
        assert_eq!(
            row_key(&rows[3], &staged),
            RowKey::File(Unstaged, "README.md".into())
        );
        assert_eq!(reselect(&rows, &staged, &b, 40), Some(rows.len() - 1));
        assert_eq!(reselect(&[], &staged, &b, 3), None);
    }

    #[test]
    fn s_and_u_move_the_selected_rows_changes() {
        use GitScope::{Staged, Unstaged};
        let changes = selection_changes();
        let (rows, _) = list_rows(&changes, true, &Folds::default());
        assert_eq!(
            row_paths(&rows[0], &changes, Unstaged),
            ["README.md", "src/a.rs", "src/b.rs"]
        );
        assert!(row_paths(&rows[0], &changes, Staged).is_empty());
        assert_eq!(
            row_paths(&rows[1], &changes, Unstaged),
            ["src/a.rs", "src/b.rs"]
        );
        assert_eq!(row_paths(&rows[7], &changes, Staged), ["src/a.rs"]);
        assert!(row_paths(&rows[7], &changes, Unstaged).is_empty());
    }

    #[test]
    fn writes_in_flight_name_the_rows_they_move() {
        let discard = InFlight::of(
            1,
            &GitAction::Discard {
                paths: vec!["a.rs".into()],
                untracked: vec!["new.rs".into()],
            },
            false,
        )
        .expect("a discard is in flight");
        assert!(discard.discard);
        assert!(discard.names(GitScope::Unstaged, "new.rs"));
        assert!(!discard.names(GitScope::Staged, "a.rs"));
        let unstage = InFlight::of(
            2,
            &GitAction::Unstage {
                paths: vec!["a.rs".into()],
            },
            true,
        )
        .expect("an unstage is in flight");
        assert!(!unstage.discard && unstage.section);
        assert!(unstage.names(GitScope::Staged, "a.rs"));
        assert!(InFlight::of(3, &GitAction::LastMessage, false).is_none());
    }
}
