//! The window's sidebar, with T3 Code's rounded cards and search bar in Compi's own
//! look. At the top, the search with New workspace, New tab and New thread beside it.
//! Under it, each workspace as a plain label that folds, over its cards: one per open
//! tab. At the bottom, a row of equal icon buttons across the sidebar, with the update
//! button last while a release is on offer.
//!
//! Cards keep their workspace's tab order, pinned tabs first in the order they were
//! pinned; nothing reorders them on its own. When a workspace's cards belong to two or
//! more projects (cards without a project count as one more), they gather under a
//! one-line header per project, in the order each project's first card comes. A header
//! folds its project's cards and, folded, shows one dot for the most urgent of them.
//! Headers and cards sit at one indent: this is a list of cards, never a file tree.
//! The window's state keeps what is folded.
//!
//! A card shows the project's icon and folder with how long ago its status changed,
//! the chat's or the tab's title, whether a chat or a terminal is in it, and one dot
//! for what its programs most need the user for. A chat card's title is the chat's
//! while this window watches it, else the one the daemon's thread list gives, asked
//! for off the UI thread when the workspace changes.
//!
//! New workspace makes "Workspace N" at once and renames its label in place: Enter
//! keeps the name typed, Escape the default. Dragging a card onto another workspace's
//! label or cards moves the tab to the end of that workspace. A card, a project header
//! and a workspace label each have a menu on right-click, as T3 Code's thread, folder
//! and project rows do.
//!
//! Typing in the search shows only the cards whose title, folder or project matches,
//! without headers; with the search holding the keyboard, Up and Down move through
//! them, Enter opens one, Escape clears the search and F2 renames the tab. Ctrl+1–9
//! (Cmd+1–9 on macOS) open the first nine cards shown, skipping folded workspaces and
//! projects.
use super::*;
use crate::chat::{age_label, now_ms, thread_title};
use crate::gui::controls::{IconName, MenuEntry, menu_item};
use compi_protocol::chat::{ChatContent, ThreadInfo};
use compi_protocol::program_status::ProgramState;
use compi_protocol::{MutationReceipt, PaneContent, Project, TabPlacement, WorkspaceSession};
use gpui::{DragMoveEvent, Entity, Render, img};
use gpui_component::Sizable as _;
use gpui_component::input::{Input, InputEvent, InputState};

/// The cards Go to sidebar card 1–9 reach.
const SHORTCUT_CARDS: usize = 9;
/// How tall the bottom row's buttons are.
const FOOTER_BUTTON_HEIGHT: f32 = 32.0;

/// The sidebar's own state: its search, the threads last listed, a label being
/// renamed in place and a card being dragged. What is folded lives in the window's
/// state.
pub(in crate::gui) struct SidebarUi {
    search: Entity<InputState>,
    _search_events: Subscription,
    /// The search, trimmed and lowercased.
    query: String,
    /// The shown card Up and Down moved to while the search has the keyboard.
    highlight: Option<usize>,
    scroll: ScrollHandle,
    /// The daemon's threads as last listed: the titles of chats this window does not
    /// watch.
    threads: Vec<ThreadInfo>,
    /// A listing is under way.
    listing: bool,
    /// The workspace changed since the last listing began.
    listing_due: bool,
    /// The workspace label being renamed in place.
    rename: Option<LabelRename>,
    /// The workspace New workspace asked for, until the daemon answers.
    creating: Option<NewWorkspace>,
    /// The other workspace a dragged card is over, with the list row it is over.
    drop_target: Option<(SessionId, usize)>,
    /// Where the list shows, as last seen while a card is dragged.
    list_bounds: Option<Bounds<Pixels>>,
}

impl SidebarUi {
    pub(in crate::gui) fn new(window: &mut Window, cx: &mut Context<CompiApp>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let events = cx.subscribe(&search, |this, search, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                let query = search.read(cx).value().trim().to_lowercase();
                this.sidebar.highlight = (!query.is_empty()).then_some(0);
                this.sidebar.query = query;
                this.sidebar.scroll.set_offset(point(px(0.0), px(0.0)));
                cx.notify();
            }
        });
        Self {
            search,
            _search_events: events,
            query: String::new(),
            highlight: None,
            scroll: ScrollHandle::new(),
            threads: Vec::new(),
            listing: false,
            listing_due: true,
            rename: None,
            creating: None,
            drop_target: None,
            list_bounds: None,
        }
    }

    /// The workspace changed: the threads are listed again.
    pub(in crate::gui) fn workspace_changed(&mut self) {
        self.listing_due = true;
    }
}

/// A workspace label being renamed in place.
struct LabelRename {
    /// The workspace; `None` while New workspace waits for it.
    session: Option<SessionId>,
    input: Entity<InputState>,
    _events: Subscription,
    /// The label before; an unchanged or empty name renames nothing.
    original: String,
    /// The name was confirmed; it is sent once the workspace is there and no other
    /// change is in flight.
    committed: bool,
}

/// A workspace New workspace asked the daemon for.
struct NewWorkspace {
    /// The tab Move to workspace › New workspace… moves into it.
    moving: Option<TabId>,
}

/// A tab's card dragged toward another workspace; it draws itself under the pointer.
#[derive(Clone)]
struct CardDrag {
    tab: TabId,
    session: SessionId,
    title: String,
    background: u32,
    foreground: u32,
    border: u32,
}

impl Render for CardDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(240.0))
            .px_2p5()
            .py_1p5()
            .rounded(px(8.0))
            .border_1()
            .border_color(color(self.border))
            .bg(color(self.background))
            .shadow_md()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .text_color(color(self.foreground))
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .child(self.title.clone())
    }
}

/// One card as the sidebar draws it: one tab.
struct SidebarCard {
    tab: TabId,
    /// The workspace whose group shows it.
    session: SessionId,
    title: String,
    /// The project's name, else the folder's.
    folder: Option<String>,
    project: Option<Project>,
    /// A chat is in it, rather than a terminal.
    chat: bool,
    /// Its chat is a draft: nothing sent yet, so no thread is kept.
    draft: bool,
    /// What its programs most need the user for; nothing while idle.
    state: Option<ProgramState>,
    /// When its program status last changed, else its thread's transcript.
    changed_ms: Option<u64>,
    hidden: bool,
    pinned: bool,
    selected: bool,
}

impl SidebarCard {
    fn matches(&self, query: &str) -> bool {
        let project = self.project.as_ref().map_or("", Project::name);
        matches_query(
            query,
            &[&self.title, self.folder.as_deref().unwrap_or(""), project],
        )
    }
}

/// The quiet note at a card's top right: "Draft" for a thread not sent yet, else how
/// long before `now` it last changed.
fn card_note(card: &SidebarCard, now: u64) -> Option<String> {
    if card.draft {
        return Some("Draft".to_owned());
    }
    card.changed_ms.map(|then| age_label(then, now))
}

/// One project's cards in a workspace.
struct ProjectSection {
    /// `None` for the cards without a project.
    project: Option<Project>,
    folded: bool,
    cards: Vec<SidebarCard>,
}

impl ProjectSection {
    /// What the section's programs most need the user for, shown on its folded header.
    fn state(&self) -> Option<ProgramState> {
        self.cards
            .iter()
            .filter_map(|card| card.state)
            .max_by_key(|state| crate::program_status::urgency(*state))
    }
}

/// One workspace's label and the cards under it that the search matches.
struct SidebarGroup {
    session: SessionId,
    label: String,
    selected: bool,
    folded: bool,
    /// Its cards by project; one section while searching or when they share a project.
    sections: Vec<ProjectSection>,
}

impl SidebarGroup {
    /// Its sections show project headers: its cards span two or more projects.
    fn headed(&self) -> bool {
        self.sections.len() > 1
    }

    fn card_count(&self) -> usize {
        self.sections
            .iter()
            .map(|section| section.cards.len())
            .sum()
    }

    /// What its programs most need the user for, shown on its label while folded.
    fn state(&self) -> Option<ProgramState> {
        self.sections
            .iter()
            .filter_map(ProjectSection::state)
            .max_by_key(|state| crate::program_status::urgency(*state))
    }
}

/// A workspace's cards in order: pinned tabs in the order pinned, then the other tabs
/// in the workspace's order.
fn card_order(tabs: &[TabId], pinned: &[TabId]) -> Vec<TabId> {
    pinned
        .iter()
        .filter(|id| tabs.contains(id))
        .chain(tabs.iter().filter(|id| !pinned.contains(id)))
        .cloned()
        .collect()
}

/// `cards` by project, in the order each project's first card comes, each keeping the
/// order the cards came in; the cards without a project make one more.
fn project_sections<T>(
    cards: impl IntoIterator<Item = T>,
    project: impl Fn(&T) -> Option<&Project>,
) -> Vec<(Option<Project>, Vec<T>)> {
    let mut sections: Vec<(Option<Project>, Vec<T>)> = Vec::new();
    for card in cards {
        let key = project(&card);
        match sections.iter_mut().find(|(known, _)| known.as_ref() == key) {
            Some((_, section)) => section.push(card),
            None => {
                let key = key.cloned();
                sections.push((key, vec![card]));
            }
        }
    }
    sections
}

/// How many cards a workspace shows of those whose `projects` are given, one per
/// card: all of them, or under project headers, those of the projects not `folded`.
fn shown_count(projects: &[Option<&Project>], folded: impl Fn(Option<&Project>) -> bool) -> usize {
    let sections = project_sections(projects.iter().copied(), |project| *project);
    if sections.len() < 2 {
        return projects.len();
    }
    sections
        .iter()
        .filter(|(project, _)| !folded(project.as_ref()))
        .map(|(_, cards)| cards.len())
        .sum()
}

/// Whether every word of `query` (lowercase) is in one of `fields`.
fn matches_query(query: &str, fields: &[&str]) -> bool {
    let fields: Vec<String> = fields.iter().map(|field| field.to_lowercase()).collect();
    query
        .split_whitespace()
        .all(|word| fields.iter().any(|field| field.contains(word)))
}

/// Whether `groups` are listed for a search: every group shows its matching cards,
/// folded or not, without project headers.
fn searching(query: &str) -> bool {
    !query.is_empty()
}

/// The cards shown, top to bottom: while searching every match, otherwise the cards of
/// the workspaces and projects that are not folded.
fn shown_cards(groups: &[SidebarGroup], searching: bool) -> impl Iterator<Item = &SidebarCard> {
    groups
        .iter()
        .filter(move |group| searching || !group.folded)
        .flat_map(|group| {
            let headed = group.headed();
            group
                .sections
                .iter()
                .filter(move |section| !(headed && section.folded))
                .flat_map(|section| &section.cards)
        })
}

/// The row of the sidebar's list that shows card `shown` (an index into
/// [`shown_cards`]): each group is its label, then its cards under their project
/// headers, or a placeholder when it shows none.
fn card_row(groups: &[SidebarGroup], searching: bool, shown: usize) -> Option<usize> {
    let mut row = 0;
    let mut card = 0;
    for group in groups {
        row += 1;
        if group.folded && !searching {
            continue;
        }
        if group.sections.is_empty() {
            row += 1;
            continue;
        }
        let headed = group.headed();
        for section in &group.sections {
            if headed {
                row += 1;
                if section.folded {
                    continue;
                }
            }
            if shown < card + section.cards.len() {
                return Some(row + shown - card);
            }
            row += section.cards.len();
            card += section.cards.len();
        }
    }
    None
}

/// The name New workspace gives: "Workspace N", N one past the workspaces there are,
/// counting on while another workspace has that name.
fn new_workspace_label<'a>(labels: impl IntoIterator<Item = &'a str>) -> String {
    let labels: Vec<&str> = labels.into_iter().collect();
    (labels.len() + 1..)
        .map(|number| format!("Workspace {number}"))
        .find(|label| !labels.contains(&label.as_str()))
        .expect("some number is free")
}

/// Moving tab `tab` to the end of workspace `target`, out of any group; nothing when
/// it is already there or either is gone.
fn move_to_workspace(
    sessions: &[WorkspaceSession],
    tab: &TabId,
    target: &SessionId,
) -> Option<WorkspaceMutation> {
    let source = sessions
        .iter()
        .find(|session| session.tabs.iter().any(|known| &known.id == tab))?;
    if &source.id == target {
        return None;
    }
    let target = sessions.iter().find(|session| &session.id == target)?;
    Some(WorkspaceMutation::MoveTab {
        session_id: target.id.clone(),
        tab_id: tab.clone(),
        index: target.tabs.len(),
        group: TabPlacement::Leave,
    })
}

/// The last name of `path`.
fn folder_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    trimmed
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

/// Every terminal and content leaf of `node`, in reading order.
fn leaves<'a>(node: &'a LayoutNode, output: &mut Vec<&'a LayoutNode>) {
    match node {
        LayoutNode::Split { first, second, .. } => {
            leaves(first, output);
            leaves(second, output);
        }
        leaf => output.push(leaf),
    }
}

/// An entry putting `text` on the clipboard; unavailable, saying `reason`, without it.
fn copy_entry(
    label: &'static str,
    text: Option<String>,
    reason: Option<&'static str>,
) -> MenuEntry {
    menu_item(label, move |_, _, cx: &mut App| {
        if let Some(text) = &text {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
        }
    })
    .disabled_because(reason)
    .into()
}

/// What a row of a card's menu does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CardAction {
    Pin,
    Unpin,
    Rename,
    CopyPath,
    CopyThreadId,
    MoveToWorkspace,
    AddToTabGroup,
    RemoveFromTabGroup,
    MoveToNewWindow,
    Close,
}

impl CardAction {
    fn label(self) -> &'static str {
        match self {
            Self::Pin => "Pin",
            Self::Unpin => "Unpin",
            Self::Rename => "Rename…",
            Self::CopyPath => "Copy path",
            Self::CopyThreadId => "Copy thread ID",
            Self::MoveToWorkspace => "Move to workspace",
            Self::AddToTabGroup => "Add to tab group",
            Self::RemoveFromTabGroup => "Remove from tab group",
            Self::MoveToNewWindow => "Move to new window",
            Self::Close => "Close",
        }
    }
}

/// What a row of a project header's menu does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectAction {
    NewTerminal,
    NewThread,
    CopyPath,
}

impl ProjectAction {
    fn label(self) -> &'static str {
        match self {
            Self::NewTerminal => "New terminal here",
            Self::NewThread => "New thread here",
            Self::CopyPath => "Copy path",
        }
    }
}

/// What a row of a workspace label's menu does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkspaceAction {
    Rename,
    NewTerminal,
    NewThread,
    Fold,
    Unfold,
    Close,
}

impl WorkspaceAction {
    fn label(self) -> &'static str {
        match self {
            Self::Rename => "Rename…",
            Self::NewTerminal => "New terminal",
            Self::NewThread => "New thread",
            Self::Fold => "Fold",
            Self::Unfold => "Unfold",
            Self::Close => "Close workspace…",
        }
    }
}

/// A sidebar menu's rows, top to bottom: an action with why it is unavailable, or
/// `None` for a separator.
type MenuRows<A> = Vec<Option<(A, Option<&'static str>)>>;

/// A card's menu, as T3 Code's thread row has it without settling, snoozing and
/// archiving: Pin or Unpin, Rename…, Copy path, Copy thread ID (a chat's card only),
/// Move to workspace ›, Add to tab group › (and Remove from tab group for a grouped
/// tab), Move to new window and Close.
fn card_menu_rows(
    pinned: bool,
    path_known: bool,
    chat: bool,
    grouped: bool,
) -> MenuRows<CardAction> {
    let pin = if pinned {
        CardAction::Unpin
    } else {
        CardAction::Pin
    };
    let mut rows = vec![
        Some((pin, None)),
        Some((CardAction::Rename, None)),
        None,
        Some((
            CardAction::CopyPath,
            (!path_known).then_some("Its folder is not known yet"),
        )),
    ];
    if chat {
        rows.push(Some((CardAction::CopyThreadId, None)));
    }
    rows.extend([
        None,
        Some((CardAction::MoveToWorkspace, None)),
        Some((CardAction::AddToTabGroup, None)),
    ]);
    if grouped {
        rows.push(Some((CardAction::RemoveFromTabGroup, None)));
    }
    rows.extend([
        Some((CardAction::MoveToNewWindow, None)),
        None,
        Some((CardAction::Close, None)),
    ]);
    rows
}

/// A project header's menu, as T3 Code's folder header has it: New terminal here, New
/// thread here and Copy path, all at the project's root; unavailable for the cards
/// without a project.
fn project_menu_rows(project: Option<&Project>) -> MenuRows<ProjectAction> {
    let reason = project
        .is_none()
        .then_some("These tabs belong to no project");
    vec![
        Some((ProjectAction::NewTerminal, reason)),
        Some((ProjectAction::NewThread, reason)),
        None,
        Some((ProjectAction::CopyPath, reason)),
    ]
}

/// A workspace label's menu: Rename…, New terminal, New thread, Fold or Unfold, and
/// Close workspace…. A thread starts from a pane, so an `empty` workspace starts none.
fn workspace_menu_rows(folded: bool, empty: bool) -> MenuRows<WorkspaceAction> {
    let fold = if folded {
        WorkspaceAction::Unfold
    } else {
        WorkspaceAction::Fold
    };
    vec![
        Some((WorkspaceAction::Rename, None)),
        None,
        Some((WorkspaceAction::NewTerminal, None)),
        Some((
            WorkspaceAction::NewThread,
            empty.then_some("A thread starts from a tab; open one here first"),
        )),
        None,
        Some((fold, None)),
        None,
        Some((WorkspaceAction::Close, None)),
    ]
}

/// `rows` as menu entries, each action's from `entry`.
fn menu_entries<A>(
    rows: MenuRows<A>,
    mut entry: impl FnMut(A, Option<&'static str>) -> MenuEntry,
) -> Vec<MenuEntry> {
    rows.into_iter()
        .map(|row| match row {
            Some((action, reason)) => entry(action, reason),
            None => MenuEntry::Separator,
        })
        .collect()
}

impl CompiApp {
    /// The sidebar's groups, each with the cards the search matches, by project; while
    /// searching, only groups with a match, their cards in one section.
    fn sidebar_groups(&self) -> Vec<SidebarGroup> {
        let Some(workspace) = &self.workspace else {
            return Vec::new();
        };
        let query = &self.sidebar.query;
        let searching = searching(query);
        let selected_session = self
            .state
            .selected_session(workspace)
            .map(|session| session.id.clone());
        let selected_tab = self.selected_tab().map(|tab| tab.id.clone());
        workspace
            .sessions
            .iter()
            .filter_map(|session| {
                let tabs: Vec<TabId> = session.tabs.iter().map(|tab| tab.id.clone()).collect();
                let cards: Vec<SidebarCard> = card_order(&tabs, &self.state.pinned_tabs)
                    .into_iter()
                    .filter_map(|id| {
                        let tab = session.tabs.iter().find(|tab| tab.id == id)?;
                        Some(self.tab_card(&session.id, tab, selected_tab.as_ref()))
                    })
                    .filter(|card| card.matches(query))
                    .collect();
                let sections = if searching {
                    if cards.is_empty() {
                        return None;
                    }
                    vec![ProjectSection {
                        project: None,
                        folded: false,
                        cards,
                    }]
                } else {
                    project_sections(cards, |card| card.project.as_ref())
                        .into_iter()
                        .map(|(project, cards)| ProjectSection {
                            folded: self.state.project_folded(&session.id, project.as_ref()),
                            project,
                            cards,
                        })
                        .collect()
                };
                Some(SidebarGroup {
                    session: session.id.clone(),
                    label: session.label.clone(),
                    selected: selected_session.as_ref() == Some(&session.id),
                    folded: self.state.workspace_folded(&session.id),
                    sections,
                })
            })
            .collect()
    }

    fn tab_card(
        &self,
        session: &SessionId,
        tab: &WorkspaceTab,
        selected_tab: Option<&TabId>,
    ) -> SidebarCard {
        let chat = self.tab_chat_content(tab);
        let listed = chat.and_then(|chat| {
            self.sidebar
                .threads
                .iter()
                .find(|thread| thread.chat.id == chat.id)
        });
        // A chat this window has not drawn is a draft when the daemon lists no thread of
        // it.
        let draft = chat.is_some_and(|chat| self.chat_draft(chat).unwrap_or(listed.is_none()));
        let title = match chat {
            Some(_) if draft => "New thread".to_owned(),
            Some(chat) => self
                .watched_chat_title(&chat.id)
                .or_else(|| listed.map(|thread| thread_title(thread).to_owned()))
                .unwrap_or_else(|| "New thread".to_owned()),
            None => self.tab_label(tab),
        };
        let folder = match &tab.project {
            Some(project) => Some(project.name().to_owned()),
            None => self
                .tab_folder(tab)
                .map(|folder| folder_name(&folder).to_owned()),
        };
        let state = self
            .tab_program_headline(tab)
            .filter(|_| self.config.program_status.sidebar)
            .map(|headline| headline.state)
            .filter(|state| *state != ProgramState::Idle);
        SidebarCard {
            tab: tab.id.clone(),
            session: session.clone(),
            title,
            folder,
            project: tab.project.clone(),
            chat: chat.is_some(),
            draft,
            state,
            changed_ms: self
                .tab_status_changed_ms(tab)
                .or_else(|| listed.map(|thread| thread.updated_ms)),
            hidden: self.state.hidden_tabs.contains(&tab.id),
            pinned: self.state.pinned_tabs.contains(&tab.id),
            selected: selected_tab == Some(&tab.id),
        }
    }

    /// The chat the tab shows: a chat pane's, or a terminal's showing its chat view.
    fn tab_chat_content<'a>(&self, tab: &'a WorkspaceTab) -> Option<&'a ChatContent> {
        let mut found = Vec::new();
        leaves(&tab.layout, &mut found);
        found
            .into_iter()
            .find_map(|leaf| leaf.shown_content().and_then(PaneContent::chat))
    }

    /// The folder the tab's first pane is in: its files' or chat's folder, or where its
    /// terminal started.
    fn tab_folder(&self, tab: &WorkspaceTab) -> Option<String> {
        let mut found = Vec::new();
        leaves(&tab.layout, &mut found);
        let leaf = found.first()?;
        if let Some(content) = leaf.shown_content() {
            return match content {
                PaneContent::Files(folder) => Some(folder.root.clone()),
                PaneContent::Chat(chat) => Some(chat.folder.clone()),
                _ => None,
            };
        }
        let surface = self.workspace.as_ref()?.surface(leaf.surface_id()?)?;
        surface
            .working_directory
            .as_ref()
            .map(|folder| folder.resolved_wsl_path.clone())
            .or_else(|| surface.launch.working_directory.clone())
    }

    /// The folder a tab card's Copy path copies: a terminal's current folder, as the
    /// pane menu's Copy terminal path has it, else the folder of the tab's first pane.
    fn tab_path(&self, tab: &WorkspaceTab) -> Option<String> {
        let mut found = Vec::new();
        collect_leaves(&tab.layout, &mut found);
        found
            .iter()
            .filter(|(_, surface)| surface.is_some())
            .find_map(|(pane, _)| self.shell_folder(pane).ok())
            .map(|(_, folder)| folder)
            .or_else(|| self.tab_folder(tab))
    }

    /// How many cards Go to sidebar card 1–9 reach.
    pub(super) fn sidebar_card_count(&self) -> usize {
        let query = &self.sidebar.query;
        if searching(query) {
            let groups = self.sidebar_groups();
            return shown_cards(&groups, true).take(SHORTCUT_CARDS).count();
        }
        // Without a search, counting the cards needs none of their titles.
        let Some(workspace) = &self.workspace else {
            return 0;
        };
        workspace
            .sessions
            .iter()
            .filter(|session| !self.state.workspace_folded(&session.id))
            .map(|session| {
                let projects: Vec<Option<&Project>> = session
                    .tabs
                    .iter()
                    .map(|tab| tab.project.as_ref())
                    .collect();
                shown_count(&projects, |project| {
                    self.state.project_folded(&session.id, project)
                })
            })
            .sum::<usize>()
            .min(SHORTCUT_CARDS)
    }

    /// Go to sidebar card `number` (1-based), counted among the cards shown.
    pub(super) fn open_sidebar_card(
        &mut self,
        number: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let groups = self.sidebar_groups();
        let tab = shown_cards(&groups, searching(&self.sidebar.query))
            .nth(number.saturating_sub(1))
            .map(|card| card.tab.clone());
        if let Some(tab) = tab {
            self.open_card(tab, window, cx);
        }
    }

    /// Opens a card: selects its tab, then gives the panes the keyboard.
    fn open_card(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.select_terminal(tab, true);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Asks to rename tab `id`, as Rename terminal tab does for the selected one.
    fn rename_tab(&mut self, id: &TabId) {
        let Some(label) = self.tab_by_id(id).map(|tab| tab.label.clone()) else {
            return;
        };
        self.open_overlay(
            Overlay::Text {
                purpose: TextPurpose::RenameTab(id.clone()),
                title: "Rename terminal tab",
            },
            &label,
        );
    }

    /// Shows workspace `session`, as picking it in Switch workspace does.
    fn select_workspace(&mut self, session: &SessionId, window: &Window) {
        if let Some(workspace) = &self.workspace {
            self.state.select_session(workspace, session);
        }
        self.sync_visible_views();
        self.save_state();
        self.rebuild_layout(window, true);
    }

    /// New workspace: makes "Workspace N" at once and renames its label in place; with
    /// `moving`, moves that tab into it once it is there.
    fn new_workspace(
        &mut self,
        moving: Option<TabId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let label = new_workspace_label(
            workspace
                .sessions
                .iter()
                .map(|session| session.label.as_str()),
        );
        let operation = WorkspaceMutation::CreateSession {
            label: label.clone(),
        };
        if self.mutate_in(operation, true, None).is_none() {
            cx.notify();
            return;
        }
        self.sidebar.creating = Some(NewWorkspace { moving });
        // A search would hide the new, empty workspace.
        self.clear_sidebar_search(window, cx);
        self.start_label_rename(None, label, window, cx);
        self.sidebar.scroll.scroll_to_bottom();
    }

    /// Moves tab `tab` to the end of workspace `target`.
    fn move_tab_to_workspace(&mut self, tab: &TabId, target: &SessionId) {
        let operation = self
            .workspace
            .as_ref()
            .and_then(|workspace| move_to_workspace(&workspace.sessions, tab, target));
        if let Some(operation) = operation {
            self.mutate(operation, false);
        }
    }

    /// Renames workspace `session`'s label in place, starting from `label`; `None` is
    /// the workspace New workspace waits for. A rename already open is confirmed.
    fn start_label_rename(
        &mut self,
        session: Option<SessionId>,
        label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_label_rename(cx);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(label.clone()));
        // Clicking away keeps the name typed, as Enter does.
        let events = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            if let InputEvent::Blur = event
                && this
                    .sidebar
                    .rename
                    .as_ref()
                    .is_some_and(|rename| rename.input == input)
            {
                this.commit_label_rename(cx);
                cx.notify();
            }
        });
        input.update(cx, |input, cx| input.select_all(window, cx));
        // After the click that asked for it, whose menu gives the keyboard back.
        let focus = input.clone();
        cx.defer_in(window, move |_, window, cx| {
            focus.update(cx, |input, cx| input.focus(window, cx));
        });
        self.sidebar.rename = Some(LabelRename {
            session,
            input,
            _events: events,
            original: label,
            committed: false,
        });
        cx.notify();
    }

    /// Confirms the label being renamed.
    fn commit_label_rename(&mut self, cx: &App) {
        if let Some(rename) = &mut self.sidebar.rename {
            rename.committed = true;
        }
        self.apply_label_rename(cx);
    }

    /// Sends a confirmed rename once its workspace is there and no other change is in
    /// flight.
    fn apply_label_rename(&mut self, cx: &App) {
        let ready = self
            .sidebar
            .rename
            .as_ref()
            .is_some_and(|rename| rename.committed && rename.session.is_some());
        if !ready || self.mutation_pending {
            return;
        }
        let Some(LabelRename {
            session: Some(session_id),
            input,
            original,
            ..
        }) = self.sidebar.rename.take()
        else {
            return;
        };
        let label = input.read(cx).value().trim().to_owned();
        if !label.is_empty() && label != original {
            self.mutate(
                WorkspaceMutation::RenameSession { session_id, label },
                false,
            );
        }
    }

    /// A workspace change finished (`receipt`, or `None` when its outcome is unknown):
    /// the workspace New workspace asked for gets its label and the tab moving into it,
    /// and a confirmed rename waiting for a change in flight is sent.
    pub(super) fn sidebar_mutation_finished(
        &mut self,
        receipt: Option<&MutationReceipt>,
        cx: &App,
    ) {
        if let Some(creating) = self.sidebar.creating.take() {
            let waiting = self
                .sidebar
                .rename
                .as_ref()
                .is_some_and(|rename| rename.session.is_none());
            let Some(session) = receipt.and_then(|receipt| receipt.affected_sessions.first())
            else {
                if waiting {
                    self.sidebar.rename = None;
                }
                return;
            };
            if waiting && let Some(rename) = &mut self.sidebar.rename {
                rename.session = Some(session.clone());
            }
            if let Some(tab) = creating.moving {
                let session = session.clone();
                // The rename follows once the move is done.
                self.move_tab_to_workspace(&tab, &session);
                return;
            }
        }
        self.apply_label_rename(cx);
    }

    /// Takes the daemon's thread list; a chat's title may now match the search.
    pub(super) fn sidebar_threads_listed(&mut self, threads: Vec<ThreadInfo>) {
        self.sidebar.threads = threads;
        self.clamp_sidebar_highlight();
    }

    fn clamp_sidebar_highlight(&mut self) {
        if let Some(highlight) = self.sidebar.highlight {
            let groups = self.sidebar_groups();
            let count = shown_cards(&groups, searching(&self.sidebar.query)).count();
            self.sidebar.highlight = (count > 0).then(|| highlight.min(count - 1));
        }
    }

    /// Lists the daemon's threads off the UI thread once the workspace changed; one
    /// listing at a time.
    pub(super) fn list_sidebar_threads(&mut self, cx: &mut Context<Self>) {
        let sidebar = &self.sidebar;
        if sidebar.listing
            || !sidebar.listing_due
            || self.workspace.is_none()
            || self.connection_error.is_some()
        {
            return;
        }
        self.sidebar.listing = true;
        self.sidebar.listing_due = false;
        let target = self.target.clone();
        cx.spawn(async move |this, cx| {
            let listed = cx
                .background_executor()
                .spawn(async move { palette::fetch_threads(&target) })
                .await;
            this.update(cx, |this, cx| {
                this.sidebar.listing = false;
                // A failed listing keeps the threads shown; the next one tries again.
                if let Ok(threads) = listed {
                    this.sidebar_threads_listed(threads);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The sidebar's search, or a workspace label renamed in place, has the keyboard.
    pub(super) fn sidebar_has_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.sidebar_open
            && self.overlay.is_none()
            && (self.sidebar.search.focus_handle(cx).is_focused(window)
                || self.renaming_label(window, cx))
    }

    fn renaming_label(&self, window: &Window, cx: &App) -> bool {
        self.sidebar
            .rename
            .as_ref()
            .is_some_and(|rename| rename.input.focus_handle(cx).is_focused(window))
    }

    /// Keys in the sidebar's fields, before the field's own. Renaming a label, Enter
    /// keeps the name and Escape the one before. In the search, Up and Down move
    /// through the cards shown, Enter opens the highlighted one (else the first match),
    /// Escape clears the search or, when it is empty, gives the panes the keyboard, and
    /// F2 renames the highlighted tab, else the selected one.
    pub(super) fn sidebar_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.sidebar_has_keyboard(window, cx) {
            return false;
        }
        let modifiers = &key.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.shift {
            return false;
        }
        if self.renaming_label(window, cx) {
            return match key.key.as_str() {
                "enter" => {
                    self.commit_label_rename(cx);
                    window.focus(&self.focus_handle, cx);
                    true
                }
                "escape" => {
                    self.sidebar.rename = None;
                    window.focus(&self.focus_handle, cx);
                    true
                }
                _ => false,
            };
        }
        let searching = searching(&self.sidebar.query);
        match key.key.as_str() {
            "up" | "down" => {
                let groups = self.sidebar_groups();
                let count = shown_cards(&groups, searching).count();
                if count == 0 {
                    return true;
                }
                let next = match (self.sidebar.highlight, key.key.as_str()) {
                    (None, "down") => 0,
                    (None, _) => count - 1,
                    (Some(at), "down") => (at + 1).min(count - 1),
                    (Some(at), _) => at.saturating_sub(1),
                };
                self.sidebar.highlight = Some(next);
                if let Some(row) = card_row(&groups, searching, next) {
                    self.sidebar.scroll.scroll_to_item(row);
                }
                true
            }
            "enter" => {
                let groups = self.sidebar_groups();
                let tab = self
                    .sidebar
                    .highlight
                    .or(searching.then_some(0))
                    .and_then(|at| shown_cards(&groups, searching).nth(at))
                    .map(|card| card.tab.clone());
                if let Some(tab) = tab {
                    self.clear_sidebar_search(window, cx);
                    self.open_card(tab, window, cx);
                }
                true
            }
            "escape" => {
                if !searching && self.sidebar.highlight.is_none() {
                    window.focus(&self.focus_handle, cx);
                } else {
                    self.clear_sidebar_search(window, cx);
                }
                true
            }
            "f2" => {
                let groups = self.sidebar_groups();
                let tab = self
                    .sidebar
                    .highlight
                    .and_then(|at| shown_cards(&groups, searching).nth(at))
                    .map(|card| card.tab.clone())
                    .or_else(|| self.selected_tab().map(|tab| tab.id.clone()));
                if let Some(id) = tab {
                    self.rename_tab(&id);
                }
                true
            }
            _ => false,
        }
    }

    fn clear_sidebar_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar.query.clear();
        self.sidebar.highlight = None;
        self.sidebar
            .search
            .update(cx, |search, cx| search.set_value("", window, cx));
    }

    fn toggle_sidebar_fold(&mut self, session: &SessionId) {
        self.state.toggle_workspace_folded(session);
        self.save_state();
        self.sidebar.highlight = None;
    }

    fn toggle_project_fold(&mut self, session: &SessionId, project: Option<&Project>) {
        self.state.toggle_project_folded(session, project);
        self.save_state();
        self.sidebar.highlight = None;
    }

    /// Right-click on tab `id`'s card in workspace `session` (`card_menu_rows`).
    fn open_card_menu(
        &mut self,
        id: TabId,
        session: SessionId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.tab_by_id(&id);
        let path = tab.and_then(|tab| self.tab_path(tab));
        let chat = tab
            .and_then(|tab| self.tab_chat_content(tab))
            .map(|chat| chat.id.clone());
        let rows = card_menu_rows(
            self.state.pinned_tabs.contains(&id),
            path.is_some(),
            chat.is_some(),
            self.tab_group(&id).is_some(),
        );
        let entries = menu_entries(rows, |action, reason| match action {
            CardAction::CopyPath => copy_entry(action.label(), path.clone(), reason),
            CardAction::CopyThreadId => copy_entry(action.label(), chat.clone(), reason),
            CardAction::MoveToWorkspace => controls::submenu(
                action.label(),
                self.move_to_workspace_entries(&id, &session, cx),
            ),
            CardAction::AddToTabGroup => {
                controls::submenu(action.label(), self.tab_group_entries(&id, cx))
            }
            _ => {
                let id = id.clone();
                menu_item(
                    action.label(),
                    cx.listener(move |this, _, window, cx| {
                        match action {
                            CardAction::Pin | CardAction::Unpin => {
                                this.state.set_pinned(&id, action == CardAction::Pin);
                                this.save_state();
                            }
                            CardAction::Rename => this.rename_tab(&id),
                            CardAction::RemoveFromTabGroup => this.join_group(id.clone(), None),
                            CardAction::MoveToNewWindow => {
                                this.transfer_tabs(vec![id.clone()], None, window, cx);
                            }
                            CardAction::Close => this.close_work(
                                WorkspaceMutation::RemoveTab { tab_id: id.clone() },
                                "Close this tab and end every process in its panes?",
                                "Close tab",
                                window,
                                cx,
                            ),
                            CardAction::CopyPath
                            | CardAction::CopyThreadId
                            | CardAction::MoveToWorkspace
                            | CardAction::AddToTabGroup => {}
                        }
                        cx.notify();
                    }),
                )
                .disabled_because(reason)
                .into()
            }
        });
        self.open_menu(position, Anchor::TopLeft, entries, window, cx);
    }

    /// Move to workspace's rows: every other workspace, then New workspace….
    fn move_to_workspace_entries(
        &self,
        id: &TabId,
        session: &SessionId,
        cx: &Context<Self>,
    ) -> Vec<MenuEntry> {
        let mut entries: Vec<MenuEntry> = self
            .workspace
            .iter()
            .flat_map(|workspace| &workspace.sessions)
            .filter(|other| &other.id != session)
            .map(|other| {
                let (tab, target) = (id.clone(), other.id.clone());
                menu_item(
                    other.label.clone(),
                    cx.listener(move |this, _, _, cx| {
                        this.move_tab_to_workspace(&tab, &target);
                        cx.notify();
                    }),
                )
                .into()
            })
            .collect();
        if !entries.is_empty() {
            entries.push(MenuEntry::Separator);
        }
        let tab = id.clone();
        entries.push(
            menu_item(
                "New workspace…",
                cx.listener(move |this, _, window, cx| {
                    this.new_workspace(Some(tab.clone()), window, cx);
                }),
            )
            .into(),
        );
        entries
    }

    /// Right-click on a workspace's label (`workspace_menu_rows`).
    fn open_workspace_menu(
        &mut self,
        session: SessionId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((label, empty)) = self.workspace.as_ref().and_then(|workspace| {
            let found = workspace
                .sessions
                .iter()
                .find(|known| known.id == session)?;
            Some((found.label.clone(), found.tabs.is_empty()))
        }) else {
            return;
        };
        let rows = workspace_menu_rows(self.state.workspace_folded(&session), empty);
        let entries = menu_entries(rows, |action, reason| {
            let session = session.clone();
            let label = label.clone();
            menu_item(
                action.label(),
                cx.listener(move |this, _, window, cx| {
                    match action {
                        WorkspaceAction::Rename => {
                            this.start_label_rename(
                                Some(session.clone()),
                                label.clone(),
                                window,
                                cx,
                            );
                        }
                        WorkspaceAction::NewTerminal => {
                            this.select_workspace(&session, window);
                            this.execute(Command::NewTerminalTab, window, cx);
                        }
                        WorkspaceAction::NewThread => {
                            this.select_workspace(&session, window);
                            this.execute(Command::OpenChat, window, cx);
                        }
                        WorkspaceAction::Fold | WorkspaceAction::Unfold => {
                            this.toggle_sidebar_fold(&session);
                        }
                        WorkspaceAction::Close => this.remove_workspace(session.clone()),
                    }
                    cx.notify();
                }),
            )
            .disabled_because(reason)
            .into()
        });
        self.open_menu(position, Anchor::TopLeft, entries, window, cx);
    }

    /// Right-click on a project header of workspace `session` (`project_menu_rows`).
    fn open_project_menu(
        &mut self,
        session: SessionId,
        project: Option<Project>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = project_menu_rows(project.as_ref());
        let entries = menu_entries(rows, |action, reason| {
            if action == ProjectAction::CopyPath {
                let root = project.as_ref().map(|project| project.root.clone());
                return copy_entry(action.label(), root, reason);
            }
            let (session, project) = (session.clone(), project.clone());
            menu_item(
                action.label(),
                cx.listener(move |this, _, _, cx| {
                    if let Some(project) = &project {
                        match action {
                            ProjectAction::NewTerminal => {
                                this.new_project_terminal(session.clone(), project);
                            }
                            ProjectAction::NewThread => {
                                this.new_project_thread(session.clone(), project);
                            }
                            ProjectAction::CopyPath => {}
                        }
                    }
                    cx.notify();
                }),
            )
            .disabled_because(reason)
            .into()
        });
        self.open_menu(position, Anchor::TopLeft, entries, window, cx);
    }

    /// New terminal here: a new tab of workspace `session` whose shell starts at the
    /// project's root, in the project's WSL distribution for a WSL project.
    fn new_project_terminal(&mut self, session: SessionId, project: &Project) {
        let distribution = match &project.environment {
            compi_protocol::ContentEnvironment::Wsl { distribution } => Some(distribution.clone()),
            compi_protocol::ContentEnvironment::Host => None,
        };
        self.mutate_in(
            WorkspaceMutation::CreateTab {
                session_id: session,
                label: String::new(),
                cols: self.terminal_cols,
                rows: self.terminal_rows,
                working_directory: Some(project.root.clone()),
            },
            true,
            distribution,
        );
    }

    /// New thread here: an OMP chat at the project's root, in a new tab of workspace
    /// `session`.
    fn new_project_thread(&mut self, session: SessionId, project: &Project) {
        let chat = crate::chat::new_chat(project.environment.clone(), project.root.clone());
        self.mutate(
            WorkspaceMutation::CreateContentTab {
                session_id: session,
                label: String::new(),
                content: PaneContent::Chat(chat),
            },
            true,
        );
    }

    pub(super) fn render_sidebar(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let comfy = self.comfy();
        let context = self.command_context(cx);
        div()
            .w(px(self.sidebar_extent()))
            .h_full()
            .flex_none()
            .flex()
            // Comfy: an island inset by the gutter on the canvas behind the panes.
            .when(comfy, |sidebar| {
                sidebar
                    .relative()
                    .pl(px(PANE_GUTTER))
                    .py(px(PANE_GUTTER))
                    .child(self.comfy_canvas(|bounds| {
                        let gutter = px(PANE_GUTTER);
                        vec![Bounds::new(
                            point(bounds.left() + gutter, bounds.top() + gutter),
                            size(
                                bounds.size.width - gutter,
                                bounds.size.height - gutter * 2.0,
                            ),
                        )]
                    }))
            })
            .child(
                div()
                    .w(px(self.sidebar_extent()
                        - if comfy {
                            PANE_GUTTER
                        } else {
                            self.seam_width()
                        }))
                    .h_full()
                    .flex()
                    .flex_col()
                    .when(comfy, |island| island.rounded(px(PANE_RADIUS)))
                    .bg(color(colors.surface).opacity(if self.glass { 0.9 } else { 1.0 }))
                    .child(self.render_sidebar_search(&context, cx))
                    .child(
                        div()
                            .id("workspace-sidebar-list")
                            .flex_1()
                            .min_h_0()
                            .pb_2()
                            .overflow_y_scroll()
                            .track_scroll(&self.sidebar.scroll)
                            // Where the list shows, so a card dragged over the search
                            // or the bottom row lights no row scrolled under them.
                            .on_drag_move(cx.listener(
                                |this, event: &DragMoveEvent<CardDrag>, _, cx| {
                                    this.sidebar.list_bounds = Some(event.bounds);
                                    if !event.bounds.contains(&event.event.position)
                                        && this.sidebar.drop_target.take().is_some()
                                    {
                                        cx.notify();
                                    }
                                },
                            ))
                            .children(self.render_sidebar_rows(cx)),
                    )
                    .children(self.render_whats_new_card(cx))
                    .child(self.render_sidebar_footer(&context, window, cx)),
            )
            .when(!comfy, |sidebar| {
                sidebar.child(
                    // One device pixel of seam; the transparent grab zone extends into the
                    // sidebar's padding so the seam stays easy to drag.
                    div()
                        .w(px(self.seam_width()))
                        .h_full()
                        .relative()
                        .bg(color(self.seam_color()))
                        .child(
                            div()
                                .id("sidebar-divider")
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .right_0()
                                .w(px(SEAM_GRAB))
                                .group("sidebar-seam")
                                .cursor(gpui::CursorStyle::ResizeLeftRight)
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .bottom_0()
                                        .right_0()
                                        .w(px(self.seam_width()))
                                        .when(self.sidebar_drag, |line| {
                                            line.bg(color(colors.accent))
                                        })
                                        .group_hover("sidebar-seam", move |style| {
                                            style.bg(color(colors.accent))
                                        }),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(Self::on_sidebar_divider_down),
                                ),
                        ),
                )
            })
            .into_any_element()
    }

    /// The search, then New workspace, New tab and New thread.
    fn render_sidebar_search(
        &self,
        context: &commands::CommandContext,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let new_workspace_reason = Command::CreateWorkspace.disabled_reason(context);
        div()
            .flex_none()
            .px_2()
            .pt_2()
            .pb_1()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    // Typing here never reaches a shell.
                    .on_key_down(|_, _, cx| cx.stop_propagation())
                    .child(
                        Input::new(&self.sidebar.search)
                            .appearance(true)
                            .cleanable(true)
                            .with_size(gpui_component::Size::Small)
                            .prefix(controls::glyph(IconName::Search, muted)),
                    ),
            )
            .child(
                // One click: the workspace is there at once, its name ready to type.
                self.sidebar_button_face(
                    "sidebar-new-workspace",
                    IconName::FolderPlus,
                    self.command_title("New workspace", Command::CreateWorkspace),
                    new_workspace_reason,
                    false,
                    false,
                )
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .when(new_workspace_reason.is_none(), |button| {
                    button.on_click(cx.listener(|this, _, window, cx| {
                        this.new_workspace(None, window, cx);
                        cx.stop_propagation();
                    }))
                }),
            )
            .child(self.sidebar_command(
                "sidebar-new-tab",
                IconName::SquareTerminal,
                "New terminal tab",
                Command::NewTerminalTab,
                false,
                context,
                cx,
            ))
            .child(self.sidebar_command(
                "sidebar-new-thread",
                IconName::MessageSquarePlus,
                "New thread",
                Command::OpenChat,
                false,
                context,
                cx,
            ))
    }

    /// Settings, Commands, Appearance, Changes and Monitor, then the update button, as
    /// equal cells across the sidebar.
    fn render_sidebar_footer(
        &self,
        context: &commands::CommandContext,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        let appearance_open = self.popup == Some(Popup::QuickAppearance);
        div()
            .flex_none()
            .px_1()
            .py_1()
            .flex()
            .items_center()
            .gap_0p5()
            .border_t_1()
            .border_color(color(colors.border))
            .child(self.sidebar_command(
                "settings-sidebar",
                IconName::Settings,
                "Settings",
                Command::OpenSettings,
                true,
                context,
                cx,
            ))
            .child(self.sidebar_command(
                "workspace-actions",
                IconName::Command,
                "Commands",
                Command::OpenPalette,
                true,
                context,
                cx,
            ))
            .child(
                div().flex_1().min_w_0().flex().flex_col().child(
                    self.render_quick_appearance(
                        "quick-appearance",
                        self.sidebar_button_face(
                            "appearance-sidebar",
                            IconName::Palette,
                            self.command_title("Appearance", Command::OpenQuickAppearance),
                            None,
                            appearance_open,
                            true,
                        )
                        .into_any_element(),
                        Anchor::BottomLeft,
                        true,
                        window,
                        cx,
                    ),
                ),
            )
            .child(self.sidebar_command(
                "changes-sidebar",
                IconName::FileDiff,
                "Changes",
                Command::ShowChanges,
                true,
                context,
                cx,
            ))
            .child(self.sidebar_command(
                "monitor-sidebar",
                IconName::Gauge,
                "Monitor",
                Command::OpenMonitor,
                true,
                context,
                cx,
            ))
            .children(self.render_update_button(cx).map(|button| {
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(FOOTER_BUTTON_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(button)
            }))
    }

    /// `label`, with `command`'s shortcut when it has one.
    fn command_title(&self, label: &str, command: Command) -> String {
        match self.configured_shortcut(command) {
            Some(shortcut) => format!("{label} · {}", controls::key_label(shortcut)),
            None => label.to_owned(),
        }
    }

    /// An icon button of the sidebar, without its click: also a popover's trigger. A
    /// `cell` is one of the bottom row's equal buttons, with a larger icon.
    fn sidebar_button_face(
        &self,
        id: &'static str,
        icon: IconName,
        title: String,
        reason: Option<&'static str>,
        active: bool,
        cell: bool,
    ) -> Stateful<Div> {
        let colors = *self.colors();
        let disabled = reason.is_some();
        let tint = if active {
            colors.accent
        } else {
            modal_text_color(colors.muted, &colors)
        };
        let glyph = controls::glyph(icon, color(tint)).with_size(if cell {
            gpui_component::Size::Medium
        } else {
            gpui_component::Size::Small
        });
        div()
            .id(id)
            .when(cell, |button| {
                button
                    .flex_1()
                    .min_w_0()
                    .w_full()
                    .h(px(FOOTER_BUTTON_HEIGHT))
            })
            .when(!cell, |button| button.flex_none().size(px(26.0)))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .when(active, |button| button.bg(color(colors.surface_hover)))
            .when(disabled, |button| button.opacity(0.5))
            .when(!disabled, |button| {
                button.hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
            })
            .tooltip(controls::tooltip(title, reason.map(Into::into), None))
            .child(glyph)
    }

    /// An icon button running `command`, named `label` in its tooltip with its shortcut,
    /// or why it cannot run; a `cell` of the bottom row or a button of the top one.
    #[allow(clippy::too_many_arguments)]
    fn sidebar_command(
        &self,
        id: &'static str,
        icon: IconName,
        label: &str,
        command: Command,
        cell: bool,
        context: &commands::CommandContext,
        cx: &Context<Self>,
    ) -> AnyElement {
        let reason = command.disabled_reason(context);
        self.sidebar_button_face(
            id,
            icon,
            self.command_title(label, command),
            reason,
            false,
            cell,
        )
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .when(reason.is_none(), |button| {
            button.on_click(cx.listener(move |this, _, window, cx| {
                this.execute(command, window, cx);
                cx.stop_propagation();
            }))
        })
        .into_any_element()
    }

    /// Makes list row `row` of workspace `session` a place to drop a dragged card: a
    /// card of another workspace lights `session`'s label while over it, and moves its
    /// tab there when dropped.
    fn drop_zone(
        &self,
        element: Stateful<Div>,
        session: &SessionId,
        row: usize,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let (over, drop) = (session.clone(), session.clone());
        element
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<CardDrag>, _, cx| {
                    let position = event.event.position;
                    let inside = event.bounds.contains(&position)
                        && this
                            .sidebar
                            .list_bounds
                            .is_some_and(|list| list.contains(&position))
                        && event.drag(cx).session != over;
                    let current = this.sidebar.drop_target.as_ref().map(|(_, at)| *at);
                    if inside && current != Some(row) {
                        this.sidebar.drop_target = Some((over.clone(), row));
                        cx.notify();
                    } else if !inside && current == Some(row) {
                        this.sidebar.drop_target = None;
                        cx.notify();
                    }
                }),
            )
            .on_drop(cx.listener(move |this, drag: &CardDrag, _, cx| {
                this.sidebar.drop_target = None;
                this.move_tab_to_workspace(&drag.tab, &drop);
                cx.notify();
            }))
    }

    /// The list's rows: each workspace's label, then its cards under their project
    /// headers, or a line saying it has none; last, the label of a workspace New
    /// workspace is making.
    fn render_sidebar_rows(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let colors = *self.colors();
        let muted = modal_text_color(colors.muted, &colors);
        let searching = searching(&self.sidebar.query);
        let groups = self.sidebar_groups();
        let now = now_ms();
        let drop_target = self
            .sidebar
            .drop_target
            .as_ref()
            .filter(|_| cx.has_active_drag())
            .map(|(session, _)| session);
        let mut rows = Vec::new();
        let mut shown = 0;
        for group in groups {
            let open = searching || !group.folded;
            let target = drop_target == Some(&group.session);
            rows.push(self.render_workspace_label(rows.len(), &group, open, target, cx));
            if !open {
                continue;
            }
            if group.sections.is_empty() {
                let session = group.session.clone();
                let row = rows.len();
                let placeholder = div()
                    .id(("sidebar-empty", row))
                    .mx_2()
                    .px_2p5()
                    .py_1()
                    .rounded(px(8.0))
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .text_color(color(muted))
                    .cursor_pointer()
                    .hover(move |style| {
                        style.bg(color(blend_rgb(colors.surface, colors.foreground, 0.05)))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_workspace(&session, window);
                        cx.notify();
                    }))
                    .child("No tabs");
                rows.push(
                    self.drop_zone(placeholder, &group.session, row, cx)
                        .into_any_element(),
                );
                continue;
            }
            let headed = group.headed();
            for section in group.sections {
                if headed {
                    let row = rows.len();
                    let header = self.render_project_header(row, &group.session, &section, cx);
                    rows.push(
                        self.drop_zone(header, &group.session, row, cx)
                            .into_any_element(),
                    );
                    if section.folded {
                        continue;
                    }
                }
                for card in section.cards {
                    let highlighted = self.sidebar.highlight == Some(shown);
                    let row = rows.len();
                    rows.push(self.render_card(shown, row, card, highlighted, now, cx));
                    shown += 1;
                }
            }
        }
        if let Some(rename) = self
            .sidebar
            .rename
            .as_ref()
            .filter(|rename| rename.session.is_none())
        {
            rows.push(
                div()
                    .id("sidebar-new-workspace-label")
                    .mx_2()
                    .mt_2()
                    .px_1()
                    .py_1()
                    .flex()
                    .items_center()
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color(colors.foreground))
                    .child(self.render_label_input(rename, cx))
                    .into_any_element(),
            );
        }
        rows
    }

    /// A workspace's label: it folds the workspace, has the workspace's menu on
    /// right-click and lights while a card of another workspace is dragged over it or
    /// its rows (`target`). Folded, it shows how many cards are under it.
    fn render_workspace_label(
        &self,
        row: usize,
        group: &SidebarGroup,
        open: bool,
        target: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = modal_text_color(colors.muted, &colors);
        let count = group.card_count();
        let renaming = self
            .sidebar
            .rename
            .as_ref()
            .filter(|rename| rename.session.as_ref() == Some(&group.session));
        let (fold, menu) = (group.session.clone(), group.session.clone());
        let label = div()
            .id(("sidebar-group", row))
            .mx_2()
            .mt_2()
            .px_1()
            .py_1()
            .rounded(px(6.0))
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(UI_MICRO_TEXT_SIZE))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(color(if target {
                colors.accent
            } else if group.selected || renaming.is_some() {
                colors.foreground
            } else {
                muted
            }))
            .when(target, |label| label.bg(color(colors.accent).opacity(0.14)))
            .map(|label| match renaming {
                Some(rename) => label.child(self.render_label_input(rename, cx)),
                None => label
                    .cursor_pointer()
                    .hover(move |style| style.text_color(color(colors.foreground)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_sidebar_fold(&fold);
                        cx.notify();
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.open_workspace_menu(menu.clone(), event.position, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(group.label.clone()),
                    )
                    // Folded, a dot for what its cards most need and how many there are.
                    .when_some(group.state().filter(|_| !open), |label, state| {
                        label.child(
                            div()
                                .flex_none()
                                .size(px(7.0))
                                .rounded_full()
                                .bg(color(self.program_state_color(state))),
                        )
                    })
                    .when(!open && count > 0, |label| {
                        label.child(
                            div()
                                .flex_none()
                                .font_weight(FontWeight::NORMAL)
                                .child(count.to_string()),
                        )
                    })
                    // Down while open, right while folded, at the row's end.
                    .child(controls::glyph(
                        if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                        color(muted),
                    )),
            });
        self.drop_zone(label, &group.session, row, cx)
            .into_any_element()
    }

    /// The field renaming a workspace's label, or once confirmed, the name it is
    /// getting.
    fn render_label_input(&self, rename: &LabelRename, cx: &Context<Self>) -> AnyElement {
        if rename.committed {
            return div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(rename.input.read(cx).value())
                .into_any_element();
        }
        // The window keeps typing here from the shell (`sidebar_has_keyboard`); a key
        // listener stopping keys would keep them from the field too.
        div()
            .flex_1()
            .min_w_0()
            .child(
                Input::new(&rename.input)
                    .appearance(true)
                    .with_size(gpui_component::Size::Small),
            )
            .into_any_element()
    }

    /// A project's header in a workspace whose cards span projects: its icon, name and
    /// card count on one line, at the cards' indent, set apart by weight. It folds the
    /// project's cards; folded, it shows one dot for the most urgent of them.
    fn render_project_header(
        &self,
        row: usize,
        session: &SessionId,
        section: &ProjectSection,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let colors = *self.colors();
        let muted = modal_text_color(colors.muted, &colors);
        let icon = match &section.project {
            Some(project) => self.project_icon(Some(project), 16.0),
            // No project: an empty tile where a project's icon would be.
            None => div()
                .flex_none()
                .size(px(16.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(color(muted).opacity(0.6))
                .into_any_element(),
        };
        let name = section
            .project
            .as_ref()
            .map_or("No project", Project::name)
            .to_owned();
        let tooltip = section.project.as_ref().map(|project| {
            controls::tooltip(name.clone(), Some(project.root.clone().into()), None)
        });
        let dot = section.state().filter(|_| section.folded).map(|state| {
            div()
                .id(("sidebar-project-status", row))
                .flex_none()
                .size(px(7.0))
                .rounded_full()
                .bg(color(self.program_state_color(state)))
                .tooltip(controls::tooltip(
                    crate::program_status::state_label(state),
                    None,
                    None,
                ))
        });
        let (fold_session, fold_project) = (session.clone(), section.project.clone());
        let (menu_session, menu_project) = (session.clone(), section.project.clone());
        div()
            .id(("sidebar-project", row))
            .mx_2()
            .mt_1()
            .px_2p5()
            .py_1()
            .rounded(px(8.0))
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(color(if section.folded {
                muted
            } else {
                colors.foreground
            }))
            .cursor_pointer()
            .hover(move |style| style.bg(color(blend_rgb(colors.surface, colors.foreground, 0.05))))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_project_fold(&fold_session, fold_project.as_ref());
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.open_project_menu(
                        menu_session.clone(),
                        menu_project.clone(),
                        event.position,
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .when_some(
                tooltip.filter(|_| self.context_menu.is_none()),
                |header, tooltip| header.tooltip(tooltip),
            )
            .child(icon)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(name),
            )
            .children(dot)
            .when(section.folded, |header| {
                header.child(
                    div()
                        .flex_none()
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .font_weight(FontWeight::NORMAL)
                        .text_color(color(muted))
                        .child(section.cards.len().to_string()),
                )
            })
            .child(controls::glyph(
                if section.folded {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                },
                color(muted),
            ))
    }

    fn render_card(
        &self,
        index: usize,
        row: usize,
        card: SidebarCard,
        highlighted: bool,
        now: u64,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        // A card can be dragged onto another workspace.
        let drag = CardDrag {
            tab: card.tab.clone(),
            session: card.session.clone(),
            title: card.title.clone(),
            background: colors.surface,
            foreground: colors.foreground,
            border: colors.accent,
        };
        let session = card.session.clone();
        let (open, rename) = (card.tab.clone(), card.tab.clone());
        let (menu_tab, menu_session) = (card.tab.clone(), card.session.clone());
        let kind = if card.chat {
            img(controls::OMP_MARK)
                .flex_none()
                .size(px(12.0))
                .into_any_element()
        } else {
            controls::glyph(IconName::SquareTerminal, muted).into_any_element()
        };
        let dot = card.state.map(|state| {
            div()
                .id(("sidebar-card-status", index))
                .flex_none()
                .size(px(7.0))
                .rounded_full()
                .bg(color(self.program_state_color(state)))
                .tooltip(controls::tooltip(
                    crate::program_status::state_label(state),
                    None,
                    None,
                ))
        });
        let mut detail = Vec::new();
        if let Some(project) = &card.project {
            detail.push(format!("{} · {}", project.name(), project.root));
        }
        if card.pinned {
            detail.push("Pinned".to_owned());
        }
        if card.hidden {
            detail.push("Hidden".to_owned());
        }
        let tooltip = self.context_menu.is_none().then(|| {
            controls::tooltip(
                card.title.clone(),
                (!detail.is_empty()).then(|| detail.join("\n").into()),
                None,
            )
        });
        let background = if card.selected {
            Some(blend_rgb(colors.surface, colors.foreground, 0.09))
        } else if highlighted {
            Some(blend_rgb(colors.surface, colors.foreground, 0.06))
        } else {
            None
        };
        let element = div()
            .id(("sidebar-card", index))
            .mx_2()
            .mb_0p5()
            .px_2p5()
            .py_1p5()
            .rounded(px(8.0))
            .flex()
            .items_center()
            .gap_2()
            .text_color(color(if card.hidden {
                modal_text_color(colors.muted, &colors)
            } else {
                colors.foreground
            }))
            .when_some(background, |card, background| card.bg(color(background)))
            .when(highlighted, |card| {
                card.border_1()
                    .border_color(color(colors.accent).opacity(0.6))
            })
            .hover(move |style| {
                style
                    .bg(color(blend_rgb(colors.surface, colors.foreground, 0.06)))
                    .cursor_pointer()
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_card(open.clone(), window, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.open_card_menu(
                        menu_tab.clone(),
                        menu_session.clone(),
                        event.position,
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .on_drag(drag, |drag: &CardDrag, _, _, cx: &mut App| {
                cx.new(|_| drag.clone())
            })
            .when_some(tooltip, |card, tooltip| card.tooltip(tooltip))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(muted)
                            .child(self.project_icon(card.project.as_ref(), 14.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(card.folder.clone().unwrap_or_default()),
                            )
                            .children(card_note(&card, now).map(|note| {
                                div()
                                    .flex_none()
                                    .when(card.draft, |shown| shown.italic())
                                    .child(note)
                            })),
                    )
                    .child(
                        div()
                            .id(("sidebar-card-title", index))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .child(card.title)
                            // A double click on the title renames the tab.
                            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                if event.click_count() >= 2 {
                                    this.rename_tab(&rename);
                                    cx.stop_propagation();
                                    cx.notify();
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(14.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1p5()
                    .child(kind)
                    .child(match dot {
                        Some(dot) => dot.into_any_element(),
                        None => div().size(px(7.0)).into_any_element(),
                    }),
            );
        self.drop_zone(element, &session, row, cx)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::ContentEnvironment;

    fn project(root: &str) -> Project {
        Project {
            environment: ContentEnvironment::Host,
            root: root.into(),
        }
    }

    fn tab_card(id: &str, project: Option<&Project>) -> SidebarCard {
        SidebarCard {
            tab: TabId::from(id),
            session: SessionId::new("s"),
            title: String::new(),
            folder: None,
            project: project.cloned(),
            chat: false,
            draft: false,
            state: None,
            changed_ms: None,
            hidden: false,
            pinned: false,
            selected: false,
        }
    }

    #[test]
    fn a_draft_card_says_draft_where_a_thread_says_its_age() {
        let now = 10 * 3_600_000;
        let mut card = SidebarCard {
            chat: true,
            changed_ms: Some(now - 2 * 3_600_000),
            ..tab_card("t1", None)
        };
        assert_eq!(card_note(&card, now).as_deref(), Some("2h"));
        card.draft = true;
        assert_eq!(card_note(&card, now).as_deref(), Some("Draft"));
        card.changed_ms = None;
        assert_eq!(card_note(&card, now).as_deref(), Some("Draft"));
        card.draft = false;
        assert_eq!(card_note(&card, now), None);
    }

    /// A workspace whose cards are `ids`, all without a project.
    fn group(folded: bool, ids: &[&str]) -> SidebarGroup {
        let cards: Vec<SidebarCard> = ids.iter().map(|id| tab_card(id, None)).collect();
        headed_group(folded, cards, &[])
    }

    /// A workspace of `cards` by project as the sidebar builds it, with the projects
    /// in `folded_projects` folded.
    fn headed_group(
        folded: bool,
        cards: Vec<SidebarCard>,
        folded_projects: &[Option<&Project>],
    ) -> SidebarGroup {
        SidebarGroup {
            session: SessionId::new("s"),
            label: String::new(),
            selected: false,
            folded,
            sections: project_sections(cards, |card| card.project.as_ref())
                .into_iter()
                .map(|(project, cards)| ProjectSection {
                    folded: folded_projects.contains(&project.as_ref()),
                    project,
                    cards,
                })
                .collect(),
        }
    }

    fn targets<'a>(cards: impl Iterator<Item = &'a SidebarCard>) -> Vec<TabId> {
        cards.map(|card| card.tab.clone()).collect()
    }

    fn tab_targets(ids: &[&str]) -> Vec<TabId> {
        ids.iter().map(|id| TabId::from(*id)).collect()
    }

    fn session(id: &str, tabs: &[&str]) -> WorkspaceSession {
        WorkspaceSession {
            id: SessionId::from(id),
            label: id.into(),
            tabs: tabs
                .iter()
                .map(|tab| WorkspaceTab {
                    id: TabId::from(*tab),
                    label: (*tab).into(),
                    layout: LayoutNode::terminal(
                        PaneId::from(format!("{tab}-pane")),
                        compi_protocol::SurfaceId::from(format!("{tab}-surface")),
                    ),
                    previous_layout: None,
                    merge: None,
                    group: None,
                    project: None,
                })
                .collect(),
            groups: Vec::new(),
        }
    }

    #[test]
    fn cards_are_the_open_tabs_with_pins_first() {
        let order = card_order(
            &tab_targets(&["a", "b", "c", "d"]),
            &tab_targets(&["c", "gone", "a"]),
        );
        assert_eq!(order, tab_targets(&["c", "a", "b", "d"]));
    }

    #[test]
    fn a_cards_menu_follows_t3s_thread_row() {
        let labels = |rows: MenuRows<CardAction>| -> Vec<&'static str> {
            rows.iter()
                .map(|row| row.map_or("-", |(action, _)| action.label()))
                .collect()
        };
        assert_eq!(
            labels(card_menu_rows(false, true, false, false)),
            [
                "Pin",
                "Rename…",
                "-",
                "Copy path",
                "-",
                "Move to workspace",
                "Add to tab group",
                "Move to new window",
                "-",
                "Close",
            ]
        );
        // A pinned, grouped chat's card copies its thread's ID and leaves its group.
        assert_eq!(
            labels(card_menu_rows(true, true, true, true)),
            [
                "Unpin",
                "Rename…",
                "-",
                "Copy path",
                "Copy thread ID",
                "-",
                "Move to workspace",
                "Add to tab group",
                "Remove from tab group",
                "Move to new window",
                "-",
                "Close",
            ]
        );
        // Only Copy path waits for a folder.
        let disabled: Vec<CardAction> = card_menu_rows(false, false, true, false)
            .into_iter()
            .flatten()
            .filter(|(_, reason)| reason.is_some())
            .map(|(action, _)| action)
            .collect();
        assert_eq!(disabled, [CardAction::CopyPath]);
    }

    #[test]
    fn a_project_headers_menu_starts_work_at_its_root() {
        let compi = project("/work/compi");
        let rows = project_menu_rows(Some(&compi));
        assert_eq!(
            rows,
            [
                Some((ProjectAction::NewTerminal, None)),
                Some((ProjectAction::NewThread, None)),
                None,
                Some((ProjectAction::CopyPath, None)),
            ]
        );
        assert_eq!(
            rows.iter()
                .flatten()
                .map(|(action, _)| action.label())
                .collect::<Vec<_>>(),
            ["New terminal here", "New thread here", "Copy path"]
        );
        // The cards without a project have no root to start at.
        assert!(
            project_menu_rows(None)
                .into_iter()
                .flatten()
                .all(|(_, reason)| reason.is_some())
        );
    }

    #[test]
    fn a_workspace_labels_menu_folds_and_starts_threads_only_from_a_tab() {
        let labels = |rows: &MenuRows<WorkspaceAction>| -> Vec<&'static str> {
            rows.iter()
                .map(|row| row.map_or("-", |(action, _)| action.label()))
                .collect()
        };
        let open = workspace_menu_rows(false, false);
        assert_eq!(
            labels(&open),
            [
                "Rename…",
                "-",
                "New terminal",
                "New thread",
                "-",
                "Fold",
                "-",
                "Close workspace…",
            ]
        );
        assert!(open.iter().flatten().all(|(_, reason)| reason.is_none()));
        let folded_empty = workspace_menu_rows(true, true);
        assert_eq!(labels(&folded_empty)[5], "Unfold");
        let disabled: Vec<WorkspaceAction> = folded_empty
            .into_iter()
            .flatten()
            .filter(|(_, reason)| reason.is_some())
            .map(|(action, _)| action)
            .collect();
        assert_eq!(disabled, [WorkspaceAction::NewThread]);
    }

    #[test]
    fn the_search_matches_every_word_in_the_title_folder_or_project() {
        let fields = ["Fix the build", "compi", "compi-site"];
        assert!(matches_query("", &fields));
        assert!(matches_query("build", &fields));
        assert!(matches_query("fix site", &fields));
        assert!(matches_query("comp", &fields));
        assert!(!matches_query("fix deploy", &fields));
        let mut card = tab_card("a", None);
        card.title = "Release Notes".into();
        card.folder = Some("docs".into());
        assert!(card.matches("release"));
        assert!(card.matches("notes docs"));
        assert!(!card.matches("compi"));
        card.project = Some(project("/work/compi"));
        assert!(card.matches("compi"));
    }

    #[test]
    fn shortcuts_count_the_cards_shown_skipping_folded_workspaces() {
        let groups = [
            group(false, &["a", "b"]),
            group(true, &["c"]),
            group(false, &[]),
            group(false, &["d"]),
        ];
        assert_eq!(
            targets(shown_cards(&groups, false)),
            tab_targets(&["a", "b", "d"])
        );
        // A search lists matches in folded workspaces too.
        assert_eq!(shown_cards(&groups, true).count(), 4);
        // Rows: label a b | label | label "No tabs" | label d.
        assert_eq!(card_row(&groups, false, 0), Some(1));
        assert_eq!(card_row(&groups, false, 1), Some(2));
        assert_eq!(card_row(&groups, false, 2), Some(7));
        assert_eq!(card_row(&groups, false, 3), None);
        assert_eq!(card_row(&groups, true, 2), Some(4));
    }

    #[test]
    fn project_headers_show_only_when_cards_span_two_or_more_projects() {
        let (compi, site) = (project("/work/compi"), project("/work/site"));
        // One project, or none at all: plain cards.
        let one = headed_group(
            false,
            vec![tab_card("a", Some(&compi)), tab_card("b", Some(&compi))],
            &[],
        );
        assert!(!one.headed());
        assert!(!group(false, &["a", "b"]).headed());
        // Cards without a project count as one more.
        let mixed = headed_group(
            false,
            vec![tab_card("a", Some(&compi)), tab_card("b", None)],
            &[],
        );
        assert!(mixed.headed());
        let two = headed_group(
            false,
            vec![tab_card("a", Some(&compi)), tab_card("b", Some(&site))],
            &[],
        );
        assert!(two.headed());
    }

    #[test]
    fn project_headers_come_in_first_card_order_and_keep_the_cards_order() {
        let (compi, site) = (project("/work/compi"), project("/work/site"));
        let cards = vec![
            tab_card("a", Some(&site)),
            tab_card("b", None),
            tab_card("c", Some(&compi)),
            tab_card("d", Some(&site)),
            tab_card("e", Some(&compi)),
            tab_card("f", None),
        ];
        let group = headed_group(false, cards, &[]);
        let sections: Vec<(Option<&str>, Vec<TabId>)> = group
            .sections
            .iter()
            .map(|section| {
                (
                    section.project.as_ref().map(Project::name),
                    targets(section.cards.iter()),
                )
            })
            .collect();
        assert_eq!(
            sections,
            [
                (Some("site"), tab_targets(&["a", "d"])),
                (None, tab_targets(&["b", "f"])),
                (Some("compi"), tab_targets(&["c", "e"])),
            ]
        );
        // Shortcuts count in the order shown: by project.
        assert_eq!(
            targets(shown_cards(&[group], false)),
            tab_targets(&["a", "d", "b", "f", "c", "e"])
        );
    }

    #[test]
    fn a_folded_project_hides_its_cards_from_the_list_and_the_shortcuts() {
        let (compi, site) = (project("/work/compi"), project("/work/site"));
        let cards = || {
            vec![
                tab_card("a", Some(&compi)),
                tab_card("b", Some(&site)),
                tab_card("c", Some(&compi)),
            ]
        };
        let groups = [
            headed_group(false, cards(), &[Some(&compi)]),
            group(false, &["d"]),
        ];
        assert_eq!(
            targets(shown_cards(&groups, false)),
            tab_targets(&["b", "d"])
        );
        // Rows: label, compi header (folded), site header, b | label, d.
        assert_eq!(card_row(&groups, false, 0), Some(3));
        assert_eq!(card_row(&groups, false, 1), Some(5));
        // The quick count agrees, and a single project's fold does nothing.
        let projects = [Some(&compi), Some(&site), Some(&compi)];
        assert_eq!(shown_count(&projects, |project| project == Some(&compi)), 1);
        assert_eq!(shown_count(&projects, |_| false), 3);
        assert_eq!(
            shown_count(&[Some(&compi), Some(&compi)], |project| project
                == Some(&compi)),
            2
        );
        // A search shows every match, without headers.
        let searched = [SidebarGroup {
            sections: vec![ProjectSection {
                project: None,
                folded: false,
                cards: cards(),
            }],
            ..headed_group(true, Vec::new(), &[])
        }];
        assert_eq!(shown_cards(&searched, true).count(), 3);
    }

    #[test]
    fn a_folded_project_header_shows_its_most_urgent_card() {
        let mut cards = vec![
            tab_card("a", None),
            tab_card("b", None),
            tab_card("c", None),
        ];
        cards[0].state = Some(ProgramState::Done);
        cards[1].state = Some(ProgramState::Error);
        cards[2].state = Some(ProgramState::Working);
        let mut section = ProjectSection {
            project: None,
            folded: true,
            cards,
        };
        assert_eq!(section.state(), Some(ProgramState::Error));
        section.cards[0].state = Some(ProgramState::Blocked);
        assert_eq!(section.state(), Some(ProgramState::Blocked));
    }

    #[test]
    fn moving_a_tab_to_another_workspace_appends_it_there() {
        let sessions = [session("w1", &["a", "b"]), session("w2", &["c", "d", "e"])];
        assert_eq!(
            move_to_workspace(&sessions, &TabId::from("a"), &SessionId::from("w2")),
            Some(WorkspaceMutation::MoveTab {
                session_id: SessionId::from("w2"),
                tab_id: TabId::from("a"),
                index: 3,
                group: TabPlacement::Leave,
            })
        );
        // Its own workspace, or one that is gone, moves nothing.
        assert_eq!(
            move_to_workspace(&sessions, &TabId::from("a"), &SessionId::from("w1")),
            None
        );
        assert_eq!(
            move_to_workspace(&sessions, &TabId::from("a"), &SessionId::from("gone")),
            None
        );
        assert_eq!(
            move_to_workspace(&sessions, &TabId::from("gone"), &SessionId::from("w2")),
            None
        );
        // An empty workspace takes it first.
        let sessions = [session("w1", &["a"]), session("w2", &[])];
        assert!(matches!(
            move_to_workspace(&sessions, &TabId::from("a"), &SessionId::from("w2")),
            Some(WorkspaceMutation::MoveTab { index: 0, .. })
        ));
    }

    #[test]
    fn new_workspaces_are_numbered_past_the_ones_there() {
        assert_eq!(new_workspace_label(Vec::new()), "Workspace 1");
        assert_eq!(new_workspace_label(["Main", "Docs"]), "Workspace 3");
        assert_eq!(
            new_workspace_label(["Main", "Workspace 3", "Workspace 4"]),
            "Workspace 5"
        );
    }
}
