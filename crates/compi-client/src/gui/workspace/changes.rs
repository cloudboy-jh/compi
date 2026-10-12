//! Changes panes: a repository's uncommitted Git changes, reviewed beside the terminal
//! that made them. The list shows two sections as `git status` does (Unstaged: the work
//! tree against the index, untracked files included; Staged: the index against `HEAD`)
//! and the selected change shows in a `DiffView` beside it. While a pane is visible its
//! repository is read again every `CHECK_INTERVAL`; the list updates in place and only
//! the selected change is read again, when its stamp changed. Reviewing never writes:
//! the daemon reads with Git's optional locks off, so neither files, the index nor any
//! other Git state change.
use super::content::{
    CHECK_INTERVAL, FILE_TAB_HEIGHT, PaneUi, SIDEBAR_WIDTH, TREE_INDENT, TREE_INSET,
    content_leaves, find_content, same_folder,
};
use super::diff_view::{DiffSettings, DiffStyle, DiffView, LayoutMode};
use super::files::FileScope;
use super::tree_ui::tree_entry_icon;
use super::*;
use crate::diff::Whitespace;
use compi_protocol::{
    ContentEnvironment, ContentView, GitChange, GitChangeKind, GitChanges, GitFileContent,
    GitFileRef, GitRevision, GitScope, PaneContent,
};
use gpui::{AnyElement, Entity, ScrollStrategy, UniformListScrollHandle, uniform_list};
use std::borrow::Cow;
use std::cell::Cell;
use std::collections::BTreeMap;

/// A diff of another file that takes longer than this to read and build replaces the
/// previous one with a loading line; quicker ones swap without it.
const LOADING_DELAY: Duration = Duration::from_millis(150);
/// A repository read that takes longer than this shows the refreshing mark.
const REFRESHING_DELAY: Duration = Duration::from_millis(300);
/// A selection rests this long before it is saved with the pane, so stepping through
/// files does not queue a workspace change per file.
const SELECTION_SAVE_DELAY: Duration = Duration::from_secs(1);
/// Rough glyph advance of the UI font at micro size, for eliding folders.
const MICRO_GLYPH_WIDTH: f32 = UI_MICRO_TEXT_SIZE * 0.56;
const SMALL_GLYPH_WIDTH: f32 = UI_SMALL_TEXT_SIZE * 0.56;

/// How every Changes pane of a window shows changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ChangesSettings {
    pub diff: DiffSettings,
    /// The list groups changes by folder.
    pub tree: bool,
}

/// A Changes pane's client state: the latest listing, the list built from it, and the
/// diff of the selected change.
pub(super) struct ChangesUi {
    /// The repository top level, as the pane's content root names it.
    repo: String,
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
    selected: Option<GitFileRef>,
    /// The user picked `selected` then; the pane has not saved it yet.
    picked_at: Option<Instant>,
    /// The diff on screen.
    shown: Option<Shown>,
    /// The diff being read or built to replace it.
    pending: Option<Pending>,
    /// Reading the sides of this change failed.
    failed: Option<(GitFileRef, String)>,
    /// Counts loads, so a superseded one is dropped when it finishes.
    loads: u64,
}

enum Listing {
    Reading,
    NotRepo,
    Failed(String),
    Ready(GitChanges),
}

struct Shown {
    file: GitFileRef,
    stamp: String,
    view: Entity<DiffView>,
}

struct Pending {
    file: GitFileRef,
    stamp: String,
    load: u64,
    started: Instant,
    /// Built and waiting until it can draw; the subscription promotes it then.
    view: Option<(Entity<DiffView>, Subscription)>,
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

impl ChangesUi {
    fn new(repo: String, selected: Option<GitFileRef>) -> Self {
        Self {
            repo,
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
            list_width: Cell::new(SIDEBAR_WIDTH),
            selected,
            picked_at: None,
            shown: None,
            pending: None,
            failed: None,
            loads: 0,
        }
    }

    pub(super) fn repo(&self) -> &str {
        &self.repo
    }

    fn changes(&self) -> &[GitChange] {
        match &self.listing {
            Listing::Ready(listing) => &listing.changes,
            _ => &[],
        }
    }

    fn selected_change(&self) -> Option<&GitChange> {
        let selected = self.selected.as_ref()?;
        self.changes().iter().find(|change| is(change, selected))
    }

    /// Every change in list order.
    fn refs(&self) -> Vec<GitFileRef> {
        let changes = self.changes();
        self.order
            .iter()
            .map(|&index| file_ref(&changes[index]))
            .collect()
    }

    fn rebuild(&mut self, tree: bool) {
        let (rows, order) = list_rows(self.changes(), tree, &self.folds);
        self.rows = rows;
        self.order = order;
    }

    /// Takes a fresh listing; whether anything shown changed. The selection stays on
    /// its change, or moves to the change now at its place in the list.
    fn apply_listing(&mut self, listing: GitChanges, tree: bool) -> bool {
        let cleared = self.refresh_error.take().is_some();
        if let Listing::Ready(current) = &self.listing
            && *current == listing
        {
            return cleared;
        }
        let previous = self.refs();
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
        let first = !matches!(self.listing, Listing::Ready(_));
        self.listing = Listing::Ready(listing);
        self.rebuild(tree);
        let current = self.refs();
        self.selected = match &self.selected {
            Some(selected) => reselect(&previous, selected, &current),
            None => current.first().cloned(),
        };
        if first {
            // A restored pane's selection may be far down the list.
            self.scroll_to_selected();
        }
        true
    }

    /// Unfolds what hides `file` and scrolls the list to it.
    fn reveal(&mut self, file: &GitFileRef, tree: bool) {
        let mut unfolded = self.folds.sections.remove(&file.scope);
        self.folds.folders.retain(|(scope, folder)| {
            let hides = *scope == file.scope
                && file
                    .path
                    .strip_prefix(folder.as_str())
                    .is_some_and(|rest| rest.starts_with('/'));
            unfolded |= hides;
            !hides
        });
        if unfolded {
            self.rebuild(tree);
        }
        self.scroll_to_selected();
    }

    fn scroll_to_selected(&self) {
        let Some(selected) = &self.selected else {
            return;
        };
        let changes = self.changes();
        if let Some(row) = self.rows.iter().position(|row| match row {
            ListRow::File { change, .. } => is(&changes[*change], selected),
            _ => false,
        }) {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
    }

    /// The diffs this pane holds: the one shown and the one replacing it.
    fn views(&self) -> impl Iterator<Item = &Entity<DiffView>> {
        self.shown
            .as_ref()
            .map(|shown| &shown.view)
            .into_iter()
            .chain(
                self.pending
                    .as_ref()
                    .and_then(|pending| pending.view.as_ref())
                    .map(|(view, _)| view),
            )
    }

    /// A new diff is slow to come, so the previous one gives way to a loading line.
    fn loading(&self) -> bool {
        self.pending.as_ref().is_some_and(|pending| {
            pending.started.elapsed() >= LOADING_DELAY
                && self
                    .shown
                    .as_ref()
                    .is_none_or(|shown| shown.file != pending.file)
        })
    }
}

fn is(change: &GitChange, file: &GitFileRef) -> bool {
    change.scope == file.scope && change.path == file.path
}

fn file_ref(change: &GitChange) -> GitFileRef {
    GitFileRef {
        path: change.path.clone(),
        scope: change.scope,
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

/// The change to select after the list went from `previous` to `current`: the same one,
/// else the one now at its place, else none when the list is empty.
fn reselect(
    previous: &[GitFileRef],
    selected: &GitFileRef,
    current: &[GitFileRef],
) -> Option<GitFileRef> {
    if current.contains(selected) {
        return Some(selected.clone());
    }
    let at = previous
        .iter()
        .position(|file| file == selected)
        .unwrap_or(0);
    current.get(at.min(current.len().checked_sub(1)?)).cloned()
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

fn status_letter(kind: GitChangeKind) -> &'static str {
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

fn kind_color(kind: GitChangeKind, colors: &ThemeColors) -> u32 {
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

impl CompiApp {
    /// The Changes command: the repository holding the focused pane's folder, in a
    /// Changes pane beside it, or the one this tab already has for it.
    pub(super) fn show_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let FileScope {
            pane_id,
            environment,
            folder,
            ..
        } = match self.file_scope() {
            Ok(scope) => scope,
            Err(reason) => {
                self.global_error = Some(reason.into());
                return;
            }
        };
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
        let mut existing = None;
        content_leaves(&tab.layout, &mut |pane_id, content| {
            if content.environment == environment
                && !content.view.is_files()
                && same_folder(&content.root, &listing.repo)
                && existing.is_none_or(|found| found != beside)
            {
                existing = Some(pane_id);
            }
        });
        let tree = self.content.changes.tree;
        if let Some(pane_id) = existing.cloned() {
            self.focus_pane(pane_id.clone());
            if let Some(changes) = self.changes_mut(&pane_id)
                && changes.apply_listing(listing, tree)
            {
                changes.read_at = Some(Instant::now());
                self.load_selected(&pane_id, window, cx);
            }
            cx.notify();
            return;
        }
        let (_, order) = list_rows(&listing.changes, tree, &Folds::default());
        let content = PaneContent {
            environment: environment.clone(),
            root: listing.repo.clone(),
            files: Vec::new(),
            active_file: None,
            view: ContentView::Changes {
                selected: order
                    .first()
                    .map(|&index| file_ref(&listing.changes[index])),
            },
        };
        if self.open_content_beside(beside, content) {
            self.content.changes_seed = Some((environment, listing));
        }
        cx.notify();
    }

    fn changes_mut(&mut self, pane_id: &PaneId) -> Option<&mut ChangesUi> {
        self.content.panes.get_mut(pane_id)?.changes.as_deref_mut()
    }

    /// State for a Changes pane this window has none for: just opened, restored after a
    /// restart, or moved here. The Changes command's listing is used when it is for
    /// this pane; otherwise the repository is read.
    pub(super) fn prepare_changes_pane(
        &mut self,
        pane_id: PaneId,
        content: PaneContent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ContentView::Changes { selected } = content.view else {
            return;
        };
        let seed = self
            .content
            .changes_seed
            .take_if(|(environment, listing)| {
                *environment == content.environment && same_folder(&listing.repo, &content.root)
            })
            .map(|(_, listing)| listing);
        let mut changes = ChangesUi::new(content.root.clone(), selected);
        let seeded = seed.is_some();
        if let Some(listing) = seed {
            changes.apply_listing(listing, self.content.changes.tree);
            changes.read_at = Some(Instant::now());
        }
        let ui = self
            .content
            .panes
            .entry(pane_id.clone())
            .or_insert_with(|| {
                PaneUi::new(
                    &pane_id,
                    content.environment.clone(),
                    content.root.clone(),
                    cx,
                )
            });
        if ui.environment != content.environment {
            *ui = PaneUi::new(
                &pane_id,
                content.environment.clone(),
                content.root.clone(),
                cx,
            );
        }
        ui.changes = Some(Box::new(changes));
        if seeded {
            self.load_selected(&pane_id, window, cx);
        } else {
            self.request_changes_listing(&pane_id, window, cx);
        }
    }

    /// Gives every diff the window's colors and terminal font; unchanged, a no-op.
    pub(super) fn sync_changes_views(&mut self, cx: &mut Context<Self>) {
        let style = DiffStyle {
            colors: *self.colors(),
            typography: self.typography.clone(),
        };
        for ui in self.content.panes.values() {
            let Some(changes) = ui.changes.as_deref() else {
                continue;
            };
            for view in changes.views() {
                view.update(cx, |view, cx| view.set_style(style.clone(), cx));
            }
        }
    }

    /// On the watch tick: reads each shown Changes pane's repository again once
    /// `CHECK_INTERVAL` passed since the last read finished, one read per pane at a time,
    /// marks reads that take a while, and saves selections that came to rest.
    pub(super) fn refresh_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.update_quiesced.load(Ordering::Acquire) {
            return;
        }
        let shown: Vec<PaneId> = self
            .shown_content_panes()
            .filter(|pane_id| {
                self.content
                    .panes
                    .get(*pane_id)
                    .is_some_and(|ui| ui.changes.is_some())
            })
            .cloned()
            .collect();
        let mut redraw = false;
        for pane_id in &shown {
            let Some(changes) = self.changes_mut(pane_id) else {
                continue;
            };
            match changes.reading {
                Some(since) => {
                    if !changes.marked && since.elapsed() >= REFRESHING_DELAY {
                        changes.marked = true;
                        redraw = true;
                    }
                }
                None => {
                    if changes
                        .read_at
                        .is_none_or(|at| at.elapsed() >= CHECK_INTERVAL)
                    {
                        self.request_changes_listing(pane_id, window, cx);
                    }
                }
            }
        }
        self.save_changes_selection();
        if redraw {
            cx.notify();
        }
    }

    fn request_changes_listing(
        &mut self,
        pane_id: &PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        let environment = ui.environment.clone();
        let Some(changes) = ui.changes.as_deref_mut() else {
            return;
        };
        if changes.reading.is_some() {
            return;
        }
        changes.reading = Some(Instant::now());
        let repo = changes.repo.clone();
        let target = self.target.clone();
        let pane_id = pane_id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    (|| -> crate::Result<_> {
                        target.connect()?.read_git_changes(environment, repo)
                    })()
                    .map_err(|error| error.to_string())
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.changes_listed(&pane_id, result, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn changes_listed(
        &mut self,
        pane_id: &PaneId,
        result: Result<Option<GitChanges>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tree = self.content.changes.tree;
        let Some(changes) = self.changes_mut(pane_id) else {
            return;
        };
        let marked = std::mem::take(&mut changes.marked);
        changes.reading = None;
        changes.read_at = Some(Instant::now());
        let changed = match result {
            Ok(Some(listing)) => changes.apply_listing(listing, tree),
            Ok(None) => {
                let changed = !matches!(changes.listing, Listing::NotRepo);
                changes.listing = Listing::NotRepo;
                changes.rebuild(tree);
                changed
            }
            Err(error) if matches!(changes.listing, Listing::Ready(_)) => {
                let changed = changes.refresh_error.as_ref() != Some(&error);
                changes.refresh_error = Some(error);
                changed
            }
            Err(error) => {
                changes.listing = Listing::Failed(error);
                true
            }
        };
        if changed {
            self.load_selected(pane_id, window, cx);
        }
        if changed || marked {
            cx.notify();
        }
    }

    /// Reads both sides of the selected change, unless they are on screen or on their
    /// way at its current stamp. A diff of the change on screen is replaced in place;
    /// another change's diff replaces the one shown once it can draw.
    fn load_selected(&mut self, pane_id: &PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ui) = self.content.panes.get_mut(pane_id) else {
            return;
        };
        let environment = ui.environment.clone();
        let Some(changes) = ui.changes.as_deref_mut() else {
            return;
        };
        let Some(change) = changes.selected_change().cloned() else {
            changes.shown = None;
            changes.pending = None;
            changes.failed = None;
            return;
        };
        let file = file_ref(&change);
        let current = |at: &GitFileRef, stamp: &str| *at == file && stamp == change.stamp;
        if changes
            .shown
            .as_ref()
            .is_some_and(|shown| current(&shown.file, &shown.stamp))
        {
            changes.pending = None;
            changes.failed = None;
            return;
        }
        if changes
            .pending
            .as_ref()
            .is_some_and(|pending| current(&pending.file, &pending.stamp))
        {
            return;
        }
        changes.loads += 1;
        let load = changes.loads;
        changes.failed = None;
        changes.pending = Some(Pending {
            file,
            stamp: change.stamp.clone(),
            load,
            started: Instant::now(),
            view: None,
        });
        let [old, new] =
            sides(&change).map(|side| side.map(|(path, revision)| (path.to_owned(), revision)));
        let executor = cx.background_executor().clone();
        let read = |side: Option<(String, GitRevision)>| {
            let (target, environment, repo) = (
                self.target.clone(),
                environment.clone(),
                changes.repo.clone(),
            );
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
        let pane = pane_id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (old, new) = (old.await, new.await);
            this.update_in(cx, |this, window, cx| {
                this.change_read(&pane, load, old, new, window, cx);
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

    fn change_read(
        &mut self,
        pane_id: &PaneId,
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
        let Some(changes) = self.changes_mut(pane_id) else {
            return;
        };
        let Some(pending) = changes.pending.take_if(|pending| pending.load == load) else {
            return;
        };
        let (old, new) = match (old, new) {
            (Ok(old), Ok(new)) => (old, new),
            (Err(error), _) | (_, Err(error)) => {
                changes.failed = Some((pending.file, error));
                cx.notify();
                return;
            }
        };
        if let Some(shown) = changes
            .shown
            .as_mut()
            .filter(|shown| shown.file == pending.file)
        {
            shown.view.update(cx, |view, cx| view.replace(old, new, cx));
            shown.stamp = pending.stamp;
            cx.notify();
            return;
        }
        let path = pending.file.path.clone();
        let view = cx.new(|cx| {
            let mut view = DiffView::new(path, old, new, settings, window, cx);
            view.set_style(style, cx);
            view
        });
        if view.read(cx).is_ready() {
            changes.shown = Some(Shown {
                file: pending.file,
                stamp: pending.stamp,
                view,
            });
        } else {
            let pane = pane_id.clone();
            let subscription = cx.observe_in(&view, window, move |this, view, _, cx| {
                this.diff_view_changed(&pane, &view, cx);
            });
            changes.pending = Some(Pending {
                view: Some((view, subscription)),
                ..pending
            });
        }
        cx.notify();
    }

    /// A diff waiting to replace the one shown takes its place once it can draw.
    fn diff_view_changed(
        &mut self,
        pane_id: &PaneId,
        view: &Entity<DiffView>,
        cx: &mut Context<Self>,
    ) {
        if !view.read(cx).is_ready() {
            return;
        }
        let Some(changes) = self.changes_mut(pane_id) else {
            return;
        };
        let Some(pending) = changes.pending.take_if(|pending| {
            pending
                .view
                .as_ref()
                .is_some_and(|(waiting, _)| waiting == view)
        }) else {
            return;
        };
        changes.shown = Some(Shown {
            file: pending.file,
            stamp: pending.stamp,
            view: view.clone(),
        });
        cx.notify();
    }

    /// The user picked `file`: it shows, its list row comes into view, and the pane
    /// saves it once the selection rests.
    fn select_change(
        &mut self,
        pane_id: &PaneId,
        file: GitFileRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tree = self.content.changes.tree;
        let Some(changes) = self.changes_mut(pane_id) else {
            return;
        };
        if changes.selected.as_ref() != Some(&file) {
            changes.selected = Some(file.clone());
            changes.picked_at = Some(Instant::now());
        }
        changes.reveal(&file, tree);
        self.load_selected(pane_id, window, cx);
        cx.notify();
    }

    /// `]` and `[`: the next or previous change in list order, across sections.
    fn step_change(
        &mut self,
        pane_id: &PaneId,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(changes) = self.changes_mut(pane_id) else {
            return;
        };
        let refs = changes.refs();
        let Some(last) = refs.len().checked_sub(1) else {
            return;
        };
        let at = changes
            .selected
            .as_ref()
            .and_then(|selected| refs.iter().position(|file| file == selected));
        let next = match at {
            Some(at) if forward => (at + 1).min(last),
            Some(at) => at.saturating_sub(1),
            None => 0,
        };
        self.select_change(pane_id, refs[next].clone(), window, cx);
    }

    /// Saves one rested user selection with its pane per call; a workspace change under
    /// way waits for the next tick.
    fn save_changes_selection(&mut self) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let mut save = None;
        for (pane_id, ui) in &mut self.content.panes {
            let Some(changes) = ui.changes.as_deref_mut() else {
                continue;
            };
            if changes
                .picked_at
                .is_none_or(|at| at.elapsed() < SELECTION_SAVE_DELAY)
            {
                continue;
            }
            let Some(mut content) = find_content(workspace, pane_id).cloned() else {
                changes.picked_at = None;
                continue;
            };
            let ContentView::Changes { selected } = &mut content.view else {
                changes.picked_at = None;
                continue;
            };
            if *selected == changes.selected {
                changes.picked_at = None;
            } else if save.is_none() && !self.mutation_pending {
                selected.clone_from(&changes.selected);
                changes.picked_at = None;
                save = Some((pane_id.clone(), content));
            }
        }
        if let Some((pane_id, content)) = save {
            self.update_content(&pane_id, content);
        }
    }

    fn set_changes_settings(&mut self, settings: ChangesSettings, cx: &mut Context<Self>) {
        let previous = std::mem::replace(&mut self.content.changes, settings);
        for ui in self.content.panes.values_mut() {
            let Some(changes) = ui.changes.as_deref_mut() else {
                continue;
            };
            if previous.tree != settings.tree {
                changes.rebuild(settings.tree);
                changes.scroll_to_selected();
            }
            if previous.diff != settings.diff {
                for view in changes.views() {
                    view.update(cx, |view, cx| view.set_settings(settings.diff, cx));
                }
            }
        }
        cx.notify();
    }

    fn toggle_changes_section(&mut self, pane_id: &PaneId, scope: GitScope) {
        let tree = self.content.changes.tree;
        if let Some(changes) = self.changes_mut(pane_id) {
            if !changes.folds.sections.remove(&scope) {
                changes.folds.sections.insert(scope);
            }
            changes.rebuild(tree);
        }
    }

    fn toggle_changes_folder(&mut self, pane_id: &PaneId, scope: GitScope, path: String) {
        let tree = self.content.changes.tree;
        if let Some(changes) = self.changes_mut(pane_id) {
            let key = (scope, path);
            if !changes.folds.folders.remove(&key) {
                changes.folds.folders.insert(key);
            }
            changes.rebuild(tree);
        }
    }

    fn retry_changes(&mut self, pane_id: &PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(changes) = self.changes_mut(pane_id) {
            changes.failed = None;
        }
        self.request_changes_listing(pane_id, window, cx);
        self.load_selected(pane_id, window, cx);
    }

    /// Keys while the Changes pane itself has the keyboard: `[`/`]` step through the
    /// changes, Ctrl/Cmd+F finds in the diff, and the diff takes the rest it knows.
    pub(super) fn changes_key_down(
        &mut self,
        pane_id: &PaneId,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = &event.keystroke;
        let modifiers = &key.modifiers;
        let primary = if cfg!(target_os = "macos") {
            modifiers.platform && !modifiers.control
        } else {
            modifiers.control && !modifiers.platform
        };
        let view = self
            .content
            .panes
            .get(pane_id)
            .and_then(|ui| ui.changes.as_deref())
            .and_then(|changes| changes.shown.as_ref())
            .map(|shown| shown.view.clone());
        if primary && !modifiers.alt && !modifiers.shift && key.key == "f" {
            if let Some(view) = view {
                view.update(cx, |view, cx| view.focus_find(window, cx));
            }
            return true;
        }
        if !modifiers.control && !modifiers.platform && !modifiers.alt {
            let typed = key.key_char.as_deref().unwrap_or(key.key.as_str());
            if matches!(typed, "[" | "]") {
                self.step_change(pane_id, typed == "]", window, cx);
                return true;
            }
        }
        view.is_some_and(|view| view.update(cx, |view, cx| view.key_down(event, window, cx)))
    }

    /// A Changes pane: the bar, then the list beside the selected change's diff, or a
    /// message while there is nothing to list.
    pub(super) fn render_changes(
        &self,
        pane_id: &PaneId,
        content: &PaneContent,
        changes: &ChangesUi,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let body = match &changes.listing {
            Listing::Reading => self.changes_message("Reading changes…", None, None, cx),
            Listing::NotRepo => self.changes_message(
                "Not a Git repository",
                Some((content.root.clone(), colors.muted)),
                Some(pane_id),
                cx,
            ),
            Listing::Failed(error) => self.changes_message(
                "Cannot read the changes",
                Some((error.clone(), colors.error)),
                Some(pane_id),
                cx,
            ),
            Listing::Ready(listing) if listing.changes.is_empty() => self.changes_message(
                "No changes",
                Some((
                    format!(
                        "{} · the working tree and index match HEAD",
                        branch_label(listing)
                    ),
                    colors.muted,
                )),
                None,
                cx,
            ),
            Listing::Ready(listing) => {
                let list_width = SIDEBAR_WIDTH.min(width / 2.0);
                changes.list_width.set(list_width);
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_changes_list(pane_id, changes, listing, list_width, cx))
                    .child(self.render_change_detail(pane_id, changes, cx))
                    .into_any_element()
            }
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_changes_bar(content, changes, cx))
            .child(body)
            .into_any_element()
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
                                this.retry_changes(&pane_id, window, cx);
                                cx.notify();
                            }),
                        ),
                )
            })
            .into_any_element()
    }

    /// Repository, branch and totals on the left; how diffs show on the right.
    fn render_changes_bar(
        &self,
        content: &PaneContent,
        changes: &ChangesUi,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        let settings = self.content.changes;
        let listing = match &changes.listing {
            Listing::Ready(listing) => Some(listing),
            _ => None,
        };
        let (files, added, removed) = changes.totals;
        let shown = changes.shown.as_ref().map(|shown| shown.view.clone());
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
                            .id(("changes-layout", index))
                            .h(px(20.0))
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
        let whitespace_button = div()
            .id("changes-whitespace")
            .flex_none()
            .h(px(22.0))
            .px_2()
            .flex()
            .items_center()
            .rounded_sm()
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
                }),
            );
        let icon_button = |id: &'static str, active: bool, enabled: bool| {
            div()
                .id(id)
                .flex_none()
                .w(px(24.0))
                .h(px(22.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .when(active, |button| button.bg(color(colors.selection)))
                .when(enabled, |button| {
                    button
                        .cursor_pointer()
                        .hover(move |style| style.bg(color(colors.surface_hover)))
                })
                .when(!enabled, |button| button.opacity(0.4))
        };
        let tree = settings.tree;
        let tree_button = icon_button("changes-tree", tree, true)
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
                }),
            );
        let context_button = icon_button("changes-context", expanded, shown.is_some())
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
                    .flex_1()
                    .min_w(px(200.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .overflow_hidden()
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(format!("Changes · {}", content.folder_name())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .px_1()
                            .rounded_sm()
                            .bg(color(colors.surface_hover))
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(color(colors.muted))
                            .child(self.environment_label(&content.environment)),
                    )
                    .when_some(listing, |bar, listing| {
                        bar.child(
                            div()
                                .flex_none()
                                .text_color(color(colors.muted))
                                .child(branch_label(listing)),
                        )
                        .when(!listing.changes.is_empty(), |bar| {
                            bar.child(div().flex_none().text_color(color(colors.muted)).child(
                                if files == 1 {
                                    "1 file".to_owned()
                                } else {
                                    format!("{files} files")
                                },
                            ))
                            .children(
                                line_counts(added, removed, &colors).map(|counts| {
                                    div()
                                        .flex_none()
                                        .font_family(self.typography.font.family.clone())
                                        .text_size(px(UI_MICRO_TEXT_SIZE))
                                        .child(counts)
                                }),
                            )
                        })
                    })
                    .when(changes.marked, |bar| {
                        bar.child(
                            div()
                                .flex_none()
                                .text_size(px(UI_MICRO_TEXT_SIZE))
                                .text_color(color(colors.muted))
                                .child("Refreshing…"),
                        )
                    })
                    .when_some(changes.refresh_error.clone(), |bar, error| {
                        bar.child(
                            div()
                                .id("changes-refresh-error")
                                .flex_none()
                                .text_size(px(UI_MICRO_TEXT_SIZE))
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
                    .child(tree_button)
                    .child(context_button),
            )
    }

    fn render_changes_list(
        &self,
        pane_id: &PaneId,
        changes: &ChangesUi,
        listing: &GitChanges,
        width: f32,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        let pane = pane_id.clone();
        div()
            .w(px(width))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(color(colors.border))
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .child(
                uniform_list(
                    "changes-list",
                    changes.rows.len(),
                    cx.processor(move |this, range: Range<usize>, _, cx| {
                        this.render_change_rows(&pane, range, cx)
                    }),
                )
                .track_scroll(&changes.scroll)
                .flex_1()
                .min_h_0()
                .py_1(),
            )
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
    }

    /// The list rows in `range`, drawn as the list scrolls to them.
    fn render_change_rows(
        &self,
        pane_id: &PaneId,
        range: Range<usize>,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(changes) = self
            .content
            .panes
            .get(pane_id)
            .and_then(|ui| ui.changes.as_deref())
        else {
            return Vec::new();
        };
        let Listing::Ready(listing) = &changes.listing else {
            return Vec::new();
        };
        let Some(rows) = changes.rows.get(range.clone()) else {
            return Vec::new();
        };
        let colors = *self.colors();
        let tree = self.content.changes.tree;
        let row_height = self.typography.cell_height.max(18.0);
        let list_width = changes.list_width.get();
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
                        let selected = changes
                            .selected
                            .as_ref()
                            .is_some_and(|selected| is(change, selected));
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
                        let file = file_ref(change);
                        let deleted = change.kind == GitChangeKind::Deleted;
                        let kind = change.kind;
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
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    this.release_editor(&pane, window, cx);
                                    this.select_change(&pane, file.clone(), window, cx);
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

    /// The selected change's header and diff.
    fn render_change_detail(
        &self,
        pane_id: &PaneId,
        changes: &ChangesUi,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        let change = changes.selected_change();
        let body = div().flex_1().min_h_0().relative().flex().flex_col();
        let body = match (change, &changes.failed, &changes.shown) {
            (None, ..) => body
                .items_center()
                .justify_center()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .text_color(color(colors.muted))
                .child("Select a change")
                .into_any_element(),
            (Some(change), Some((file, error)), _) if is(change, file) => {
                let retry = pane_id.clone();
                body.p_3()
                    .gap_1()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .child(format!("Cannot read {}", split_path(&change.path).1))
                    .child(div().text_color(color(colors.error)).child(error.clone()))
                    .child(
                        div()
                            .id("change-retry")
                            .mt_1()
                            .cursor_pointer()
                            .text_color(color(colors.accent))
                            .child("Try again")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    this.retry_changes(&retry, window, cx);
                                    cx.notify();
                                }),
                            ),
                    )
                    .into_any_element()
            }
            (Some(change), ..) if changes.loading() => body
                .p_3()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .text_color(color(colors.muted))
                .child(format!("Loading {}…", split_path(&change.path).1))
                .into_any_element(),
            (Some(_), _, Some(shown)) => body.child(shown.view.clone()).into_any_element(),
            (Some(_), _, None) => body.into_any_element(),
        };
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .when_some(change, |detail, change| {
                detail.child(self.render_change_header(change, changes, cx))
            })
            .child(body)
    }

    /// Path, kind, line counts and what the diff compares.
    fn render_change_header(
        &self,
        change: &GitChange,
        changes: &ChangesUi,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        let (folder, name) = split_path(&change.path);
        let counts = changes
            .shown
            .as_ref()
            .filter(|shown| is(change, &shown.file))
            .and_then(|shown| shown.view.read(cx).stats())
            .or(change.added.zip(change.removed))
            .map(|(added, removed)| (u64::from(added), u64::from(removed)));
        let title = change.path.clone();
        let reason = baseline(change);
        div()
            .id("change-header")
            .flex_none()
            .h(px(FILE_TAB_HEIGHT))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(color(colors.border))
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .whitespace_nowrap()
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
            .child(
                div()
                    .flex_none()
                    .px_1()
                    .rounded_sm()
                    .bg(color(colors.surface_hover))
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .text_color(color(kind_color(change.kind, &colors)))
                    .child(kind_label(change.kind)),
            )
            .children(counts.and_then(|(added, removed)| {
                line_counts(added, removed, &colors).map(|counts| {
                    div()
                        .flex_none()
                        .font_family(self.typography.font.family.clone())
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .child(counts)
                })
            }))
            .child(div().flex_1())
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .text_color(color(colors.muted))
                    .child(baseline(change)),
            )
            .tooltip(move |_, cx| {
                cx.new(|_| HeaderTooltip {
                    title: title.clone(),
                    reason: Some(reason.clone()),
                    colors,
                })
                .into()
            })
    }
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

    fn file(scope: GitScope, path: &str) -> GitFileRef {
        GitFileRef {
            path: path.into(),
            scope,
        }
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
    fn a_vanished_selection_moves_to_the_change_at_its_place() {
        let a = file(GitScope::Unstaged, "a");
        let b = file(GitScope::Unstaged, "b");
        let c = file(GitScope::Staged, "c");
        let previous = [a.clone(), b.clone(), c.clone()];
        assert_eq!(reselect(&previous, &b, &previous), Some(b.clone()));
        assert_eq!(
            reselect(&previous, &b, &[a.clone(), c.clone()]),
            Some(c.clone())
        );
        assert_eq!(reselect(&previous, &c, &[a.clone(), b.clone()]), Some(b));
        assert_eq!(reselect(&previous, &a, &[]), None);
        // A selection the list never had (a restored pane) goes to the first change.
        assert_eq!(
            reselect(
                &[],
                &file(GitScope::Staged, "gone"),
                std::slice::from_ref(&c)
            ),
            Some(c)
        );
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
