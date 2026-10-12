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
//! above the list. Discarding asks first, naming the files. After every write the folder
//! is read again at once, so the list, open diffs and gutters follow.
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
use compi_protocol::{
    ContentEnvironment, ContentTab, GitAction, GitChange, GitChangeKind, GitChanges,
    GitFileContent, GitRevision, GitScope, GitWriteResult, PaneContent,
};
use gpui::{AnyElement, Entity, ScrollStrategy, UniformListScrollHandle, uniform_list};
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
    /// Git writes sent for this repository and not answered yet.
    writing: usize,
    /// A write finished while a listing read was out: read again once it lands.
    reread: bool,
    /// What the last write left under the commit box.
    outcome: Option<Outcome>,
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
            writing: 0,
            reread: false,
            outcome: None,
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

    /// The change a diff tab of `repo` shows, if the listing has it.
    fn change(&self, repo: &str, path: &str, scope: GitScope) -> Option<&GitChange> {
        let listing = self.listing()?;
        if !same_folder(&listing.repo, repo) {
            return None;
        }
        listing
            .changes
            .iter()
            .find(|change| change.scope == scope && change.path == path)
    }

    fn rebuild(&mut self, tree: bool) {
        let (rows, order) = list_rows(self.changes(), tree, &self.folds);
        self.rows = rows;
        self.order = order;
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

    /// Unfolds what hides the change of `path` in `scope` and scrolls the list to it.
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
        let changes = self.changes();
        if let Some(row) = self.rows.iter().position(|row| match row {
            ListRow::File { change, .. } => {
                changes[*change].scope == scope && changes[*change].path == path
            }
            _ => false,
        }) {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
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
fn truncate_middle(text: &str, max: usize) -> Cow<'_, str> {
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

fn branch_label(listing: &GitChanges) -> String {
    match (&listing.branch, &listing.head) {
        (Some(branch), _) => branch.clone(),
        (None, Some(head)) => format!("detached at {head}"),
        (None, None) => "no commits yet".into(),
    }
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

fn list_icon(tree: bool, tint: Hsla) -> impl IntoElement {
    canvas(
        move |_, _, _| (),
        move |bounds, _, window, _| {
            let x = |value: f32| bounds.left() + px(value);
            let y = |value: f32| bounds.top() + px(value);
            let mut path = PathBuilder::stroke(px(1.25));
            if tree {
                path.move_to(point(x(2.5), y(4.0)));
                path.line_to(point(x(13.5), y(4.0)));
                path.move_to(point(x(4.5), y(4.0)));
                path.line_to(point(x(4.5), y(12.0)));
                path.line_to(point(x(13.5), y(12.0)));
                path.move_to(point(x(4.5), y(8.0)));
                path.line_to(point(x(13.5), y(8.0)));
            } else {
                for row in [4.0, 8.0, 12.0] {
                    path.move_to(point(x(2.5), y(row)));
                    path.line_to(point(x(13.5), y(row)));
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, tint);
            }
        },
    )
    .size(px(16.0))
}

/// A plus over a minus: open a file's diff.
fn diff_icon(tint: Hsla) -> impl IntoElement {
    canvas(
        move |_, _, _| (),
        move |bounds, _, window, _| {
            let x = |value: f32| bounds.left() + px(value);
            let y = |value: f32| bounds.top() + px(value);
            let mut path = PathBuilder::stroke(px(1.25));
            path.move_to(point(x(4.5), y(5.5)));
            path.line_to(point(x(11.5), y(5.5)));
            path.move_to(point(x(8.0), y(2.0)));
            path.line_to(point(x(8.0), y(9.0)));
            path.move_to(point(x(4.5), y(12.5)));
            path.line_to(point(x(11.5), y(12.5)));
            if let Ok(path) = path.build() {
                window.paint_path(path, tint);
            }
        },
    )
    .size(px(16.0))
}

/// Chevrons apart (show unchanged lines) or together (fold them again).
fn context_icon(expanded: bool, tint: Hsla) -> impl IntoElement {
    canvas(
        move |_, _, _| (),
        move |bounds, _, window, _| {
            let x = |value: f32| bounds.left() + px(value);
            let y = |value: f32| bounds.top() + px(value);
            let mut path = PathBuilder::stroke(px(1.25));
            let (top, bottom) = if expanded {
                ([2.5, 6.0, 2.5], [13.5, 10.0, 13.5])
            } else {
                ([6.0, 2.5, 6.0], [10.0, 13.5, 10.0])
            };
            for points in [top, bottom] {
                path.move_to(point(x(4.5), y(points[0])));
                path.line_to(point(x(8.0), y(points[1])));
                path.line_to(point(x(11.5), y(points[2])));
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, tint);
            }
        },
    )
    .size(px(16.0))
}

/// A list row's compact action: `glyph` in a small square, `title` as its tooltip.
fn row_button(
    id: &'static str,
    index: usize,
    glyph: &'static str,
    title: &'static str,
    colors: ThemeColors,
    on_press: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id((id, index))
        .flex_none()
        .size(px(18.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .text_size(px(UI_SMALL_TEXT_SIZE))
        .text_color(color(colors.foreground))
        .hover(move |style| style.bg(color(colors.selection)))
        .child(glyph)
        .tooltip(move |_, cx| {
            cx.new(|_| HeaderTooltip {
                title: title.into(),
                reason: None,
                colors,
            })
            .into()
        })
        .on_mouse_down(MouseButton::Left, on_press)
}

/// A row's actions at its right end while it is hovered, over its line counts.
fn row_actions(colors: ThemeColors, buttons: Vec<gpui::Stateful<gpui::Div>>) -> gpui::Div {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .right(px(6.0))
        .flex()
        .items_center()
        .gap(px(2.0))
        .px(px(2.0))
        .invisible()
        .group_hover("changes-row", |style| style.visible())
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(2.0))
                .rounded_sm()
                .bg(color(colors.surface_hover))
                .children(buttons),
        )
}

impl CompiApp {
    /// The Git changes of `pane_id`'s folder: `None` outside a work tree or until the
    /// first read lands. Kept fresh while the pane is visible.
    pub(super) fn repo_changes(&self, pane_id: &PaneId) -> Option<&GitChanges> {
        self.content.panes.get(pane_id)?.repo.listing()
    }

    /// Shows the diff of repository path `path` as a tab of the content pane `pane_id`,
    /// opening it or activating the one open. Without `scope`, the unstaged change wins
    /// over the staged one; a file with neither leaves a message. `line` (1-based, new
    /// side) is scrolled into view, now or once the diff is built. Asked before the folder
    /// was read, it happens when the read lands.
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
        let repo = listing.repo.clone();
        ui.repo.reveal(&path, scope, tree);
        let tab = ContentTab::Diff { repo, path, scope };
        if let Some(line) = line {
            match ui.diffs.get(&tab).and_then(DiffTab::view) {
                Some(view) => view.update(cx, |view, cx| view.reveal_line(Side::New, line, cx)),
                None => ui.diff_reveal = Some((tab.clone(), line)),
            }
        }
        let listed = content.tabs.contains(&tab);
        if !listed && content.tabs.len() >= compi_protocol::MAX_CONTENT_FILES {
            self.global_error =
                Some("Close a tab first: this pane has as many as it can hold".into());
            return;
        }
        if !listed || content.active.as_ref() != Some(&tab) {
            if !listed {
                content.tabs.push(tab.clone());
            }
            content.active = Some(tab);
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
        let content = PaneContent {
            environment: environment.clone(),
            root: listing.repo.clone(),
            tabs: Vec::new(),
            active: None,
        };
        if self.open_content_beside(beside, content) {
            self.content.changes_seed = Some((environment, listing));
        }
        cx.notify();
    }

    /// The end of a folder tree row with a mark: a changed file's status letter, with an
    /// **Open diff** button before it while the row is hovered, or a folder's quiet dot.
    pub(super) fn render_tree_mark(
        &self,
        pane_id: &PaneId,
        index: usize,
        mark: &TreeMark,
        row_height: f32,
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
                    .id(("content-tree-diff", index))
                    .flex_none()
                    .size(px(16.0))
                    .rounded_sm()
                    .cursor_pointer()
                    .invisible()
                    .group_hover("content-tree-row", |style| style.visible())
                    .hover(move |style| style.bg(color(colors.surface_hover)))
                    .child(diff_icon(color(colors.muted)))
                    .tooltip(move |_, cx| {
                        cx.new(|_| HeaderTooltip {
                            title: "Open diff".into(),
                            reason: None,
                            colors,
                        })
                        .into()
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.open_diff(&pane, path.clone(), None, None, window, cx);
                            cx.stop_propagation();
                            cx.notify();
                        }),
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

    /// Diff this file: the focused pane's open file against the index, else its staged
    /// change, at the editor's cursor line.
    pub(in crate::gui) fn diff_this_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((pane_id, path)) = self.focused_pane().cloned().and_then(|pane_id| {
            let path = self.focused_content()?.active_file()?.to_owned();
            Some((pane_id, path))
        }) else {
            self.global_error = Some("Open a file in the editor first".into());
            return;
        };
        let name = file_name(&path).to_owned();
        let Some(ui) = self.content.panes.get(&pane_id) else {
            return;
        };
        // The diff's new side is the saved file (unstaged) or the index (staged), so the
        // cursor's line is a close guess when the buffer has unsaved edits.
        let line = ui
            .editor(&path)
            .map(|editor| editor.state.read(cx).cursor_position().line + 1);
        let relative = match &ui.repo.listing {
            Listing::Ready(listing) => repo_relative(&listing.repo, &path),
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
        self.open_diff(&pane_id, relative, None, line, window, cx);
    }

    /// Edit file: the work-tree file of a diff tab, at the line the diff shows a third of
    /// the way down.
    fn edit_from_diff(
        &mut self,
        pane_id: &PaneId,
        tab: &ContentTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ContentTab::Diff { repo, path, .. } = tab else {
            return;
        };
        let Some(ui) = self.content.panes.get(pane_id) else {
            return;
        };
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
            Some(work_tree_path(repo, path)),
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
                    matches!(active, ContentTab::Diff { .. })
                        .then(|| (pane_id.clone(), active.clone()))
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
                let button = match tab {
                    ContentTab::Diff { repo, path, scope } => ui
                        .repo
                        .change(repo, path, *scope)
                        .and_then(hunk_target)
                        .map(|(write, _)| match write {
                            HunkWrite::Stage => HunkButton::Stage,
                            HunkWrite::Unstage => HunkButton::Unstage,
                        }),
                    ContentTab::File { .. } => None,
                };
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
        let ContentTab::Diff { repo, path, scope } = tab else {
            return;
        };
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        let Some(change) = ui.repo.change(repo, path, *scope).cloned() else {
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
        let ContentTab::Diff { path, .. } = tab else {
            return;
        };
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
        let path = path.clone();
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
        let kind = match action {
            GitAction::Commit { .. } => WriteKind::Commit,
            GitAction::LastMessage => WriteKind::LastMessage,
            _ => WriteKind::Index,
        };
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        ui.repo.writing += 1;
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
                this.git_written(&pane, &environment, &repo, kind, result, window, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    fn git_written(
        &mut self,
        pane_id: &PaneId,
        environment: &ContentEnvironment,
        repo: &str,
        kind: WriteKind,
        result: Result<GitWriteResult, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ui) = self.content.panes.get_mut(pane_id) {
            let status = &mut ui.repo;
            status.writing = status.writing.saturating_sub(1);
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
        cx.notify();
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
    /// repository `pane_id` lists.
    fn move_changes(
        &mut self,
        pane_id: &PaneId,
        scope: GitScope,
        paths: Vec<String>,
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
        self.git_write(pane_id, repo, action, window, cx);
    }

    /// The paths of every listed change in `scope`, for a section's Stage / Unstage.
    fn section_paths(&self, pane_id: &PaneId, scope: GitScope) -> Vec<String> {
        let mut paths: Vec<String> = self
            .repo_changes(pane_id)
            .map(|listing| {
                listing
                    .changes
                    .iter()
                    .filter(|change| change.scope == scope)
                    .flat_map(change_paths)
                    .collect()
            })
            .unwrap_or_default();
        paths.sort_unstable();
        paths.dedup();
        paths
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
        let ContentTab::Diff { repo, path, scope } = tab else {
            return;
        };
        let Some((write, index_path)) = self
            .content
            .panes
            .get(pane_id)
            .and_then(|ui| ui.repo.change(repo, path, *scope))
            .and_then(hunk_target)
        else {
            return;
        };
        let action = set_index_lines(&event.diff, &event.hunk, write, index_path);
        self.git_write(pane_id, repo.clone(), action, window, cx);
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
        let refusal = if !commit.has_summary {
            Some("Write a summary first")
        } else if !staged && !commit.amend {
            Some("Nothing is staged to commit")
        } else {
            None
        };
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

    /// Ctrl/Cmd+Enter in a commit box's field commits. Whether the key was taken.
    pub(super) fn commit_box_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = &key.modifiers;
        let primary = if cfg!(target_os = "macos") {
            modifiers.platform && !modifiers.control
        } else {
            modifiers.control && !modifiers.platform
        };
        if key.key != "enter" || !primary || modifiers.alt || modifiers.shift {
            return false;
        }
        let Some(pane_id) = self.focused_pane().cloned() else {
            return false;
        };
        let focused = self
            .content
            .panes
            .get(&pane_id)
            .and_then(|ui| ui.repo.commit.as_ref())
            .is_some_and(|commit| {
                commit.summary.focus_handle(cx).is_focused(window)
                    || commit.body.focus_handle(cx).is_focused(window)
            });
        if focused {
            self.commit(&pane_id, window, cx);
        }
        focused
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

    /// `[` and `]` on a diff tab: the previous or next change in list order, across
    /// sections, in the same tab (or the tab already showing it).
    fn step_diff(&mut self, pane_id: &PaneId, forward: bool) {
        let Some(mut content) = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_content(workspace, pane_id))
            .cloned()
        else {
            return;
        };
        let Some(current) = content.active.clone() else {
            return;
        };
        let ContentTab::Diff { repo, path, scope } = &current else {
            return;
        };
        let tree = self.content.changes.tree;
        let Some(status) = self.content.panes.get_mut(pane_id).map(|ui| &mut ui.repo) else {
            return;
        };
        let changes = status.changes();
        let order: Vec<(&str, GitScope)> = status
            .order
            .iter()
            .map(|&index| (changes[index].path.as_str(), changes[index].scope))
            .collect();
        let Some(last) = order.len().checked_sub(1) else {
            return;
        };
        let next = match order
            .iter()
            .position(|&(listed, at)| listed == path && at == *scope)
        {
            Some(at) if forward => (at + 1).min(last),
            Some(at) => at.saturating_sub(1),
            None => 0,
        };
        let next = ContentTab::Diff {
            repo: repo.clone(),
            path: order[next].0.to_owned(),
            scope: order[next].1,
        };
        if next == current {
            return;
        }
        if let ContentTab::Diff { path, scope, .. } = &next {
            status.reveal(path, *scope, tree);
        }
        if !content.tabs.contains(&next)
            && let Some(slot) = content.tabs.iter_mut().find(|tab| **tab == current)
        {
            *slot = next.clone();
        }
        content.active = Some(next);
        self.update_content(pane_id, content);
    }

    /// Keys while a diff tab is active and the pane itself has the keyboard: `[`/`]` step
    /// through the changes, `s` stages the file of an unstaged diff and `u` unstages the
    /// file of a staged one (the row the list selects), Ctrl/Cmd+F finds in the diff, and
    /// the diff takes the rest it knows.
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
            if let ContentTab::Diff { scope, .. } = tab
                && wanted == Some(*scope)
            {
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
        let ContentTab::Diff { repo, path, scope } = tab else {
            return;
        };
        let Some(paths) = self
            .content
            .panes
            .get(pane_id)
            .and_then(|ui| ui.repo.change(repo, path, *scope))
            .map(|change| change_paths(change).collect())
        else {
            return;
        };
        self.move_changes(pane_id, *scope, paths, window, cx);
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
        let colors = *self.colors();
        let count = ui.repo.listing().map(|listing| listing.changes.len());
        let segment = |mode: SidebarMode, label: &'static str, index: usize| {
            let active = ui.mode == mode;
            let pane = pane_id.clone();
            div()
                .id(("content-sidebar-mode", index))
                .flex_none()
                .h(px(20.0))
                .px_2()
                .flex()
                .items_center()
                .justify_center()
                .gap_1()
                .rounded_sm()
                .cursor_pointer()
                .text_color(color(if active {
                    colors.foreground
                } else {
                    colors.muted
                }))
                .when(active, |segment| segment.bg(color(colors.selection)))
                .when(!active, |segment| {
                    segment.hover(move |style| style.bg(color(colors.surface_hover)))
                })
                .child(label)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        this.set_sidebar_mode(&pane, mode);
                        this.release_editor(&pane, window, cx);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
        };
        let switch = div()
            .flex_none()
            .p(px(1.0))
            .flex()
            .gap(px(1.0))
            .rounded_sm()
            .border_1()
            .border_color(color(colors.border))
            .child(segment(SidebarMode::Files, "Files", 0))
            .child(segment(SidebarMode::Changes, "Changes", 1).when_some(
                count.filter(|count| *count > 0),
                |segment, count| {
                    segment.child(
                        div()
                            .text_color(color(colors.muted))
                            .child(count.to_string()),
                    )
                },
            ));
        Some(
            div()
                .flex_none()
                .px_2()
                .pt_1()
                .pb_1()
                .flex()
                .text_size(px(UI_MICRO_TEXT_SIZE))
                .child(switch)
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
            Listing::Reading => return self.changes_message("Reading changes…", None, None, cx),
            Listing::NotRepo => {
                return self.changes_message(
                    "Not a Git repository",
                    Some((repo.root.clone(), colors.muted)),
                    None,
                    cx,
                );
            }
            Listing::Failed(error) => {
                return self.changes_message(
                    "Cannot read the changes",
                    Some((error.clone(), colors.error)),
                    Some(pane_id),
                    cx,
                );
            }
            Listing::Ready(listing) => listing,
        };
        repo.list_width.set(width);
        let tree = self.content.changes.tree;
        let (files, added, removed) = repo.totals;
        let tree_button = div()
            .id("changes-tree")
            .flex_none()
            .w(px(24.0))
            .h(px(22.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .cursor_pointer()
            .when(tree, |button| button.bg(color(colors.selection)))
            .hover(move |style| style.bg(color(colors.surface_hover)))
            .child(list_icon(
                tree,
                color(if tree { colors.accent } else { colors.muted }),
            ))
            .tooltip(move |_, cx| {
                cx.new(|_| HeaderTooltip {
                    title: if tree { "Show as list" } else { "Show as tree" }.into(),
                    reason: None,
                    colors,
                })
                .into()
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    let mut settings = this.content.changes;
                    settings.tree = !settings.tree;
                    this.set_changes_settings(settings, cx);
                    cx.stop_propagation();
                }),
            );
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
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(branch_label(listing)),
            )
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
                        .tooltip(move |_, cx| {
                            cx.new(|_| HeaderTooltip {
                                title: "The repository could not be read again".into(),
                                reason: Some(error.clone()),
                                colors,
                            })
                            .into()
                        }),
                )
            })
            .child(div().flex_1())
            .child(tree_button);
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
                cx.processor(move |this, range: Range<usize>, _, cx| {
                    this.render_change_rows(&pane, range, cx)
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
        let refusal = if commit.committing {
            Some("Committing…")
        } else if staged == 0 && !commit.amend {
            Some("Stage changes to commit them")
        } else if !commit.has_summary {
            Some("Write a summary first")
        } else {
            None
        };
        let ready = refusal.is_none();
        let shortcut = if cfg!(target_os = "macos") {
            "⌘Enter"
        } else {
            "Ctrl+Enter"
        };
        let pane = pane_id.clone();
        let amend = div()
            .id("commit-amend")
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .h(px(22.0))
            .rounded_sm()
            .text_color(color(if commit.amend {
                colors.foreground
            } else {
                colors.muted
            }))
            .child(
                div()
                    .size(px(12.0))
                    .rounded_sm()
                    .border_1()
                    .border_color(color(if commit.amend {
                        colors.accent
                    } else {
                        colors.border
                    }))
                    .when(commit.amend, |check| check.bg(color(colors.accent))),
            )
            .child("Amend")
            .when(born, |toggle| {
                toggle
                    .cursor_pointer()
                    .hover(move |style| style.bg(color(colors.surface_hover)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.toggle_amend(&pane, window, cx);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
            })
            .when(!born, |toggle| toggle.opacity(0.5))
            .tooltip(move |_, cx| {
                cx.new(|_| HeaderTooltip {
                    title: "Amend the last commit".into(),
                    reason: Some(
                        if born {
                            "Loads its message; staged changes join it"
                        } else {
                            "There is no commit yet"
                        }
                        .into(),
                    ),
                    colors,
                })
                .into()
            });
        let pane = pane_id.clone();
        let label = match (commit.committing, commit.amend) {
            (true, _) => "Committing…",
            (false, true) => "Amend",
            (false, false) => "Commit",
        };
        let button = div()
            .id("commit-button")
            .flex_none()
            .h(px(22.0))
            .px_3()
            .flex()
            .items_center()
            .rounded_sm()
            .bg(color(colors.accent))
            .text_color(color(ui_text_color(colors.background, colors.accent)))
            .font_weight(FontWeight::MEDIUM)
            .child(label)
            .when(ready, |button| {
                button
                    .cursor_pointer()
                    .hover(|style| style.opacity(0.9))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.commit(&pane, window, cx);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
            })
            .when(!ready, |button| button.opacity(0.45))
            .tooltip(move |_, cx| {
                cx.new(|_| HeaderTooltip {
                    title: refusal.unwrap_or(shortcut).into(),
                    reason: None,
                    colors,
                })
                .into()
            });
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
                        .child(div().flex_1())
                        .child(
                            div()
                                .flex_none()
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
                    div()
                        .id("changes-retry")
                        .mt_1()
                        .cursor_pointer()
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .text_color(color(colors.accent))
                        .child("Try again")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                this.retry_repo(&pane_id, window, cx);
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        ),
                )
            })
            .into_any_element()
    }

    /// The list rows in `range`, drawn as the list scrolls to them.
    fn render_change_rows(
        &self,
        pane_id: &PaneId,
        range: Range<usize>,
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
            .and_then(|content| match &content.active {
                Some(ContentTab::Diff { repo, path, scope })
                    if same_folder(repo, &listing.repo) =>
                {
                    Some((path.as_str(), *scope))
                }
                _ => None,
            });
        let colors = *self.colors();
        let tree = self.content.changes.tree;
        let row_height = self.typography.cell_height.max(18.0);
        let list_width = repo.list_width.get();
        let mono = self.typography.font.family.clone();
        rows.iter()
            .zip(range)
            .map(|(row, index)| {
                let pane = pane_id.clone();
                let base = div()
                    .id(("changes-row", index))
                    .group("changes-row")
                    .relative()
                    .h(px(row_height))
                    .flex_none()
                    .pr_2()
                    .flex()
                    .items_center()
                    .cursor_pointer();
                match row {
                    ListRow::Section {
                        scope,
                        count,
                        collapsed,
                    } => {
                        let scope = *scope;
                        let mover = pane.clone();
                        let (glyph, title) = match scope {
                            GitScope::Unstaged => ("+", "Stage all"),
                            GitScope::Staged => ("−", "Unstage all"),
                        };
                        let move_all = row_button(
                            "changes-section-move",
                            index,
                            glyph,
                            title,
                            colors,
                            cx.listener(move |this, _, window, cx| {
                                let paths = this.section_paths(&mover, scope);
                                this.move_changes(&mover, scope, paths, window, cx);
                                cx.stop_propagation();
                            }),
                        );
                        base.pl(px(TREE_INSET))
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(color(colors.muted))
                            .hover(move |style| style.text_color(color(colors.foreground)))
                            .child(
                                div()
                                    .px_1()
                                    .flex()
                                    .items_center()
                                    .child(div().w(px(14.0)).flex_none().child(if *collapsed {
                                        "▸"
                                    } else {
                                        "▾"
                                    }))
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(scope_label(scope)),
                                    )
                                    .child(div().ml_2().child(count.to_string())),
                            )
                            .child(row_actions(colors, vec![move_all]))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    this.release_editor(&pane, window, cx);
                                    this.toggle_changes_section(&pane, scope);
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            )
                            .into_any_element()
                    }
                    ListRow::Folder {
                        scope,
                        path,
                        label,
                        depth,
                        collapsed,
                    } => {
                        let (scope, folder) = (*scope, path.clone());
                        base.pl(px(TREE_INSET + *depth as f32 * TREE_INDENT))
                            .children(indent_guides(*depth, colors.border))
                            .child(
                                div()
                                    .h(px(row_height - 2.0))
                                    .min_w_0()
                                    .px_1()
                                    .flex()
                                    .items_center()
                                    .rounded_sm()
                                    .whitespace_nowrap()
                                    .group_hover("changes-row", move |style| {
                                        style.bg(color(colors.surface_hover))
                                    })
                                    .child(
                                        div()
                                            .w(px(14.0))
                                            .flex_none()
                                            .text_color(color(colors.muted))
                                            .child(if *collapsed { "▸" } else { "▾" }),
                                    )
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
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    this.release_editor(&pane, window, cx);
                                    this.toggle_changes_folder(&pane, scope, folder.clone());
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            )
                            .into_any_element()
                    }
                    ListRow::File { change, depth } => {
                        let change = &listing.changes[*change];
                        let selected = active == Some((change.path.as_str(), change.scope));
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
                        let (path, scope) = (change.path.clone(), change.scope);
                        let deleted = change.kind == GitChangeKind::Deleted;
                        let kind = change.kind;
                        let mut actions = Vec::new();
                        if scope == GitScope::Unstaged && kind != GitChangeKind::Conflicted {
                            let (discarder, discarded) = (pane.clone(), change.clone());
                            actions.push(row_button(
                                "changes-row-discard",
                                index,
                                "↺",
                                if kind == GitChangeKind::Untracked {
                                    "Delete file…"
                                } else {
                                    "Discard changes…"
                                },
                                colors,
                                cx.listener(move |this, _, _, cx| {
                                    this.confirm_discard(
                                        &discarder,
                                        std::slice::from_ref(&discarded),
                                    );
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            ));
                        }
                        let (mover, paths) =
                            (pane.clone(), change_paths(change).collect::<Vec<_>>());
                        actions.push(row_button(
                            "changes-row-move",
                            index,
                            if scope == GitScope::Unstaged {
                                "+"
                            } else {
                                "−"
                            },
                            match (scope, kind) {
                                (GitScope::Staged, _) => "Unstage",
                                (_, GitChangeKind::Conflicted) => "Stage (marks it resolved)",
                                _ => "Stage",
                            },
                            colors,
                            cx.listener(move |this, _, window, cx| {
                                this.move_changes(&mover, scope, paths.clone(), window, cx);
                                cx.stop_propagation();
                            }),
                        ));
                        base.pl(px(inset))
                            .when(tree, |row| {
                                row.children(indent_guides(level, colors.border))
                            })
                            .child(
                                div()
                                    .h(px(row_height - 2.0))
                                    .flex_1()
                                    .min_w_0()
                                    .px_1()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .rounded_sm()
                                    .whitespace_nowrap()
                                    .when(selected, |item| item.bg(color(colors.selection)))
                                    .when(!selected, |item| {
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
                                            .text_color(color(if deleted && !selected {
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
                                                .child(counts)
                                        })
                                    })),
                            )
                            .child(row_actions(colors, actions))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    if let Some(ui) = this.content.panes.get_mut(&pane) {
                                        ui.sidebar = true;
                                    }
                                    this.open_diff(
                                        &pane,
                                        path.clone(),
                                        Some(scope),
                                        None,
                                        window,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            )
                            .into_any_element()
                    }
                }
            })
            .collect()
    }

    /// A diff tab's caption: the file name, a scope hint for staged changes, and what is
    /// compared for the tooltip.
    pub(super) fn diff_tab_caption(
        ui: &PaneUi,
        tab: &ContentTab,
    ) -> Option<(String, Option<&'static str>, String)> {
        let ContentTab::Diff { repo, path, scope } = tab else {
            return None;
        };
        let compared = ui
            .repo
            .change(repo, path, *scope)
            .map_or_else(|| scope_baseline(*scope).to_owned(), baseline);
        Some((
            split_path(path).1.to_owned(),
            (*scope == GitScope::Staged).then_some("staged"),
            format!("{path} · {}", lowercase_first(&compared)),
        ))
    }

    /// A diff tab's body: the change's header with the diff controls, then its diff or
    /// what keeps it from showing.
    pub(super) fn render_diff_tab(
        &self,
        pane_id: &PaneId,
        ui: &PaneUi,
        tab: &ContentTab,
        cx: &Context<Self>,
    ) -> AnyElement {
        let ContentTab::Diff { repo, path, scope } = tab else {
            return div().into_any_element();
        };
        let colors = *self.colors();
        let change = ui.repo.change(repo, path, *scope);
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
            (Listing::Ready(listing), _) if !same_folder(&listing.repo, repo) => {
                muted_line(format!("Not in this folder's repository: {repo}"))
            }
            (Listing::Ready(_), None) => self.changes_message(
                "No changes",
                Some((
                    format!(
                        "{} · {}",
                        split_path(path).1,
                        scope_baseline(*scope).to_lowercase()
                    ),
                    colors.muted,
                )),
                None,
                cx,
            ),
            (Listing::Ready(_), Some(_)) => match state {
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
                        .child(
                            div()
                                .id("diff-retry")
                                .mt_1()
                                .cursor_pointer()
                                .text_color(color(colors.accent))
                                .child("Try again")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, window, cx| {
                                        this.retry_repo(&retry, window, cx);
                                        cx.notify();
                                    }),
                                ),
                        )
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
            },
        };
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(self.render_diff_header(pane_id, tab, path, *scope, change, state, cx))
            .child(body)
            .into_any_element()
    }

    /// Path, kind, line counts and what the diff compares; how diffs show; Edit file.
    #[allow(clippy::too_many_arguments)]
    fn render_diff_header(
        &self,
        pane_id: &PaneId,
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
        let (title, reason) = (path.to_owned(), compared.clone());
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
        let layout = div()
            .flex_none()
            .flex()
            .p(px(1.0))
            .gap(px(1.0))
            .rounded_sm()
            .border_1()
            .border_color(color(colors.border))
            .children(
                layouts
                    .into_iter()
                    .enumerate()
                    .map(|(index, (mode, label, reason))| {
                        let active = settings.diff.layout == mode;
                        div()
                            .id(("diff-layout", index))
                            .h(px(18.0))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded_sm()
                            .cursor_pointer()
                            .text_color(color(if active {
                                colors.foreground
                            } else {
                                colors.muted
                            }))
                            .when(active, |segment| segment.bg(color(colors.selection)))
                            .when(!active, |segment| {
                                segment.hover(move |style| style.bg(color(colors.surface_hover)))
                            })
                            .child(label)
                            .tooltip(move |_, cx| {
                                cx.new(|_| HeaderTooltip {
                                    title: format!("{label} layout"),
                                    reason: Some(reason.into()),
                                    colors,
                                })
                                .into()
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    let mut settings = this.content.changes;
                                    settings.diff.layout = mode;
                                    this.set_changes_settings(settings, cx);
                                    cx.stop_propagation();
                                }),
                            )
                    }),
            );
        let whitespace = settings.diff.whitespace;
        let (whitespace_label, next_whitespace) = match whitespace {
            Whitespace::Exact => ("Exact whitespace", Whitespace::IgnoreSpace),
            Whitespace::IgnoreSpace => ("Ignore space", Whitespace::IgnoreFormatting),
            Whitespace::IgnoreFormatting => ("Ignore formatting", Whitespace::Exact),
        };
        let button = |id: &'static str| {
            div()
                .id(id)
                .flex_none()
                .h(px(22.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
        };
        let whitespace_button = button("diff-whitespace")
            .px_2()
            .border_1()
            .border_color(color(colors.border))
            .cursor_pointer()
            .text_color(color(if whitespace == Whitespace::Exact {
                colors.muted
            } else {
                colors.accent
            }))
            .hover(move |style| style.bg(color(colors.surface_hover)))
            .child(whitespace_label)
            .tooltip(move |_, cx| {
                cx.new(|_| HeaderTooltip {
                    title: "Whitespace".into(),
                    reason: Some(
                        "Click to switch: exact, ignore changes in spacing, ignore formatting-only changes"
                            .into(),
                    ),
                    colors,
                })
                .into()
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    let mut settings = this.content.changes;
                    settings.diff.whitespace = next_whitespace;
                    this.set_changes_settings(settings, cx);
                    cx.stop_propagation();
                }),
            );
        let context_button = button("diff-context")
            .w(px(24.0))
            .when(expanded, |button| button.bg(color(colors.selection)))
            .when(shown.is_some(), |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(color(colors.surface_hover)))
            })
            .when(shown.is_none(), |button| button.opacity(0.4))
            .child(context_icon(
                expanded,
                color(if expanded {
                    colors.accent
                } else {
                    colors.muted
                }),
            ))
            .tooltip(move |_, cx| {
                cx.new(|_| HeaderTooltip {
                    title: if expanded {
                        "Fold unchanged lines"
                    } else {
                        "Show all unchanged lines"
                    }
                    .into(),
                    reason: Some("E".into()),
                    colors,
                })
                .into()
            })
            .when_some(shown, |button, view| {
                button.on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    view.update(cx, |view, cx| view.set_context_expanded(!expanded, cx));
                    cx.stop_propagation();
                })
            });
        // A change without a work-tree file (deleted) has nothing to edit.
        let editable = change.is_some_and(|change| sides(change)[1].is_some());
        let (edit_pane, edit_tab) = (pane_id.clone(), tab.clone());
        let edit_button = button("diff-edit-file")
            .px_2()
            .border_1()
            .border_color(color(colors.border))
            .text_color(color(colors.foreground))
            .child("Edit file")
            .when(editable, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(color(colors.surface_hover)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.edit_from_diff(&edit_pane, &edit_tab, window, cx);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
            })
            .when(!editable, |button| button.opacity(0.4))
            .tooltip(move |_, cx| {
                cx.new(|_| HeaderTooltip {
                    title: "Edit file".into(),
                    reason: Some(
                        if editable {
                            "Opens the working-tree file at the line in view"
                        } else {
                            "The file is not in the working tree"
                        }
                        .into(),
                    ),
                    colors,
                })
                .into()
            });
        let (move_label, move_reason) = match scope {
            GitScope::Unstaged => ("Stage file", "Adds the whole file to the index · S"),
            GitScope::Staged => ("Unstage file", "Takes the whole file out of the index · U"),
        };
        let (move_pane, move_tab) = (pane_id.clone(), tab.clone());
        let move_button = change.map(|_| {
            button("diff-move-file")
                .px_2()
                .border_1()
                .border_color(color(colors.border))
                .text_color(color(colors.foreground))
                .cursor_pointer()
                .hover(move |style| style.bg(color(colors.surface_hover)))
                .child(move_label)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        this.move_tab_file(&move_pane, &move_tab, window, cx);
                        cx.stop_propagation();
                    }),
                )
                .tooltip(move |_, cx| {
                    cx.new(|_| HeaderTooltip {
                        title: move_label.into(),
                        reason: Some(move_reason.into()),
                        colors,
                    })
                    .into()
                })
        });
        let discard_pane = pane_id.clone();
        let discard_button = change
            .filter(|change| {
                change.scope == GitScope::Unstaged && change.kind != GitChangeKind::Conflicted
            })
            .cloned()
            .map(|change| {
                let untracked = change.kind == GitChangeKind::Untracked;
                button("diff-discard")
                    .px_2()
                    .border_1()
                    .border_color(color(colors.border))
                    .text_color(color(colors.error))
                    .cursor_pointer()
                    .hover(move |style| style.bg(color(colors.surface_hover)))
                    .child("Discard")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.confirm_discard(&discard_pane, std::slice::from_ref(&change));
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .tooltip(move |_, cx| {
                        cx.new(|_| HeaderTooltip {
                            title: if untracked {
                                "Delete file…"
                            } else {
                                "Discard changes…"
                            }
                            .into(),
                            reason: Some("Asks first".into()),
                            colors,
                        })
                        .into()
                    })
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
                        header.child(
                            div()
                                .flex_none()
                                .px_1()
                                .rounded_sm()
                                .bg(color(colors.surface_hover))
                                .text_size(px(UI_MICRO_TEXT_SIZE))
                                .text_color(color(kind_color(change.kind, &colors)))
                                .child(kind_label(change.kind)),
                        )
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
                    .tooltip(move |_, cx| {
                        cx.new(|_| HeaderTooltip {
                            title: title.clone(),
                            reason: Some(reason.clone()),
                            colors,
                        })
                        .into()
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .child(layout)
                    .child(whitespace_button)
                    .child(context_button)
                    .child(edit_button)
                    .children(move_button)
                    .children(discard_button),
            )
    }
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
}
