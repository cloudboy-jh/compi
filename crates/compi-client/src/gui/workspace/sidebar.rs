//! Saved projects and open tabs. The sidebar shows a project even with no tabs.
//! Tabs whose projects aren't saved or are archived stay reachable under Loose tabs.
//! Workspace and tab layouts are independent of this presentation.
//!
//! Search matches project names, folders and tab titles. Ctrl+1–9 (Cmd+1–9 on
//! macOS) open the first nine cards shown, skipping folded sections.
use super::*;
use crate::chat::{age_label, now_ms, thread_title};
use crate::gui::controls::{IconName, MenuEntry, menu_item};
use compi_protocol::chat::{ChatContent, ChatHarness, ThreadInfo};
use compi_protocol::program_status::ProgramState;
use compi_protocol::{MutationReceipt, PaneContent, Project, TabPlacement, WorkspaceSession};
use gpui::{Entity, img};
use gpui_component::Sizable as _;
use gpui_component::input::{Input, InputEvent, InputState};
use std::collections::HashMap;

mod drag;
pub(in crate::gui) use drag::CardDrag;
use drag::{CardDrop, ProjectDrag, ProjectDrop, ProjectLine};
use gpui::DragMoveEvent;

/// The cards Go to sidebar card 1–9 reach.
const SHORTCUT_CARDS: usize = 9;
/// How far cards sit in from the list's edge, under their project's heading.
const CARD_INDENT: f32 = 32.0;
/// Where the line joining a project's cards runs: under the middle of its icon.
const CARD_GUIDE_X: f32 = 26.0;
/// How tall the bottom row's buttons are.
const FOOTER_BUTTON_HEIGHT: f32 = 32.0;

/// The sidebar's own state: its search, the threads last listed, a label being
/// renamed in place and a card or project heading being dragged. What is folded lives
/// in the window's state.
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
    /// Saved WSL projects were checked for a newer name this run.
    projects_renamed: bool,
    /// Where a dragged card would land.
    drop: Option<CardDrop>,
    /// Where a dragged project heading would land.
    project_drop: Option<ProjectDrop>,
    /// The workspace label being renamed in place.
    rename: Option<LabelRename>,
    /// The workspace New workspace asked for, until the daemon answers.
    creating: Option<NewWorkspace>,
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
            projects_renamed: false,
            drop: None,
            project_drop: None,
            rename: None,
            creating: None,
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

/// One card as the sidebar draws it: one tab.
struct SidebarCard {
    tab: TabId,
    /// The workspace whose group shows it.
    session: SessionId,
    title: String,
    /// The folder its panes show, for the tooltip and search.
    folder: Option<String>,
    /// The listed project whose section shows it: its saved project, else Home.
    project: Project,
    /// The agent of the chat in it, rather than a terminal.
    chat: Option<ChatHarness>,
    /// Its chat is a draft: nothing sent yet, so no thread is kept.
    draft: bool,
    /// What its programs most need the user for; nothing while idle.
    state: Option<ProgramState>,
    /// When its program status last changed, else its thread's transcript.
    changed_ms: Option<u64>,
    hidden: bool,
    pinned: bool,
    selected: bool,
    /// One line per pane: what it is and what runs in it, for the card's hover.
    panes: Vec<String>,
    /// How many panes the tab has, by kind, when it has more than one ("2 threads").
    count: Option<String>,
}

/// What a pane holds, for a card's count.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PaneKind {
    Thread,
    Terminal,
    Other,
}

/// The count a card shows for its tab's panes: none for one, "2 threads" or "2
/// terminals" when all are one kind, else "3 panes".
fn pane_count(kinds: &[PaneKind]) -> Option<String> {
    let count = kinds.len();
    if count < 2 {
        return None;
    }
    let noun = match kinds[0] {
        PaneKind::Thread if kinds.iter().all(|kind| *kind == PaneKind::Thread) => "threads",
        PaneKind::Terminal if kinds.iter().all(|kind| *kind == PaneKind::Terminal) => "terminals",
        _ => "panes",
    };
    Some(format!("{count} {noun}"))
}

impl SidebarCard {
    fn matches(&self, query: &str) -> bool {
        // Its folder's name too, when the project goes by another.
        let project = self.project.name();
        matches_query(
            query,
            &[&self.title, self.folder.as_deref().unwrap_or(""), project],
        )
    }
}

/// Where a card is drawn, for drags: its section's project, the card after it there,
/// and the drop line it shows (`Some(true)` under it, `Some(false)` over it).
struct CardPlace {
    section: Project,
    next: Option<TabId>,
    line: Option<bool>,
}

/// The quiet note at a card's top right: "Draft" for a thread not sent yet, else how
/// long before `now` it last changed.
fn card_note(card: &SidebarCard, now: u64) -> Option<String> {
    if card.draft {
        return Some("Draft".to_owned());
    }
    card.changed_ms.map(|then| age_label(then, now))
}

/// A listed project's cards.
struct ProjectSection {
    project: Project,
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

/// Group cards by the listed projects (`projects`, Home among them), in that list's
/// order and each card's order. Every listed project has a section, with cards or not;
/// a card whose project is not listed is in `home`'s.
fn project_sections(
    projects: Vec<Project>,
    home: &Project,
    cards: impl IntoIterator<Item = SidebarCard>,
    folded: impl Fn(&Project) -> bool,
) -> Vec<ProjectSection> {
    let indices: HashMap<_, _> = projects
        .iter()
        .enumerate()
        .map(|(index, project)| (project, index))
        .collect();
    let home = indices.get(home).copied();
    let mut sections: Vec<_> = projects.iter().map(|_| Vec::new()).collect();
    for card in cards {
        if let Some(index) = indices.get(&card.project).copied().or(home) {
            sections[index].push(card);
        }
    }
    projects
        .into_iter()
        .zip(sections)
        .map(|(project, cards)| ProjectSection {
            folded: folded(&project),
            project,
            cards,
        })
        .collect()
}

/// Whether every word of `query` (lowercase) is in one of `fields`.
fn matches_query(query: &str, fields: &[&str]) -> bool {
    let fields: Vec<String> = fields.iter().map(|field| field.to_lowercase()).collect();
    query
        .split_whitespace()
        .all(|word| fields.iter().any(|field| field.contains(word)))
}

fn searching(query: &str) -> bool {
    !query.is_empty()
}

/// Search temporarily expands matching sections.
fn shown_cards(sections: &[ProjectSection]) -> impl Iterator<Item = &SidebarCard> {
    sections
        .iter()
        .filter(|section| !section.folded)
        .flat_map(|section| &section.cards)
}

/// Every section has one header; folded sections show no cards.
fn card_row(sections: &[ProjectSection], mut shown: usize) -> Option<usize> {
    let mut row = 0;
    for section in sections {
        row += 1;
        if section.folded {
            continue;
        }
        if shown < section.cards.len() {
            return Some(row + shown);
        }
        row += section.cards.len();
        shown -= section.cards.len();
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
    Settings,
}

impl ProjectAction {
    fn label(self) -> &'static str {
        match self {
            Self::NewTerminal => "New terminal here",
            Self::NewThread => "New thread here",
            Self::CopyPath => "Copy path",
            Self::Settings => "Project settings…",
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

fn project_menu_rows(has_workspace: bool) -> MenuRows<ProjectAction> {
    let reason = (!has_workspace).then_some("Open a workspace first");
    vec![
        Some((ProjectAction::NewTerminal, reason)),
        Some((ProjectAction::NewThread, reason)),
        None,
        Some((ProjectAction::CopyPath, None)),
        None,
        Some((ProjectAction::Settings, None)),
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
    fn sidebar_sections(&self) -> Vec<ProjectSection> {
        let query = &self.sidebar.query;
        let searching = searching(query);
        let projects = self
            .listed_projects()
            .into_iter()
            .map(|listed| listed.project)
            .filter(|project| !self.project_archived(project))
            .collect();
        let home = self.home_project();
        let selected = self.selected_tab().map(|tab| &tab.id);
        let cards = self
            .workspace
            .iter()
            .flat_map(|workspace| &workspace.sessions)
            .flat_map(|session| {
                let tabs: Vec<_> = session.tabs.iter().map(|tab| tab.id.clone()).collect();
                card_order(&tabs, &self.state.pinned_tabs)
                    .into_iter()
                    .filter_map(move |id| {
                        let tab = session.tabs.iter().find(|tab| tab.id == id)?;
                        Some(self.tab_card(&session.id, tab, selected))
                    })
            })
            .filter(|card| !searching || card.matches(query));
        let mut sections = project_sections(projects, &home, cards, |project| {
            !searching && self.state.project_folded(Some(project))
        });
        if searching {
            sections.retain(|section| {
                !section.cards.is_empty()
                    || matches_query(
                        query,
                        &[&self.project_name(&section.project), &section.project.root],
                    )
            });
        }
        sections
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
                .thread_name(&chat.id)
                .unwrap_or_else(|| "New thread".to_owned()),
            None => self.tab_label(tab),
        };
        let project = self.section_project(tab.project.as_ref());
        let folder = self.tab_folder(tab);
        let state = self
            .tab_program_headline(tab)
            .filter(|_| self.config.program_status.sidebar)
            .map(|headline| headline.state)
            .filter(|state| *state != ProgramState::Idle);
        let panes = self.tab_pane_lines(tab);
        SidebarCard {
            tab: tab.id.clone(),
            session: session.clone(),
            title,
            folder,
            project,
            chat: chat.map(|chat| chat.harness),
            draft,
            state,
            changed_ms: self
                .tab_status_changed_ms(tab)
                .or_else(|| listed.map(|thread| thread.updated_ms)),
            hidden: self.state.hidden_tabs.contains(&tab.id),
            pinned: self.state.pinned_tabs.contains(&tab.id),
            selected: selected_tab == Some(&tab.id),
            count: pane_count(&panes.iter().map(|(kind, _)| *kind).collect::<Vec<_>>()),
            panes: panes.into_iter().map(|(_, line)| line).collect(),
        }
    }

    /// Each of the tab's panes, in layout order: its kind, and a line naming it and what
    /// runs in it ("OMP · fix login · Working", "Terminal · npm run dev").
    fn tab_pane_lines(&self, tab: &WorkspaceTab) -> Vec<(PaneKind, String)> {
        crate::arrangement::leaves(&tab.layout)
            .into_iter()
            .zip(self.tab_panes(tab))
            .map(|(leaf, pane)| {
                let state = self
                    .pane_program_state(&pane.pane_id)
                    .filter(|state| *state != ProgramState::Idle)
                    .map(crate::program_status::state_label);
                // A terminal showing a chat (its view) counts as the thread it shows.
                let (kind, mut line) = match leaf.shown_content() {
                    Some(PaneContent::Chat(chat)) => {
                        let title = self
                            .thread_name(&chat.id)
                            .unwrap_or_else(|| "New thread".to_owned());
                        (
                            PaneKind::Thread,
                            format!("{} · {title}", chat.harness.name()),
                        )
                    }
                    Some(content) => {
                        let label = content.label();
                        let line = if pane.title == label {
                            label.to_owned()
                        } else {
                            format!("{label} · {}", pane.title)
                        };
                        (PaneKind::Other, line)
                    }
                    None => (PaneKind::Terminal, format!("Terminal · {}", pane.title)),
                };
                if let Some(state) = state {
                    line.push_str(" · ");
                    line.push_str(state);
                }
                (kind, line)
            })
            .collect()
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
        if searching(&self.sidebar.query) {
            return shown_cards(&self.sidebar_sections())
                .take(SHORTCUT_CARDS)
                .count();
        }
        self.workspace
            .iter()
            .flat_map(|workspace| &workspace.sessions)
            .flat_map(|session| &session.tabs)
            .filter(|tab| {
                let project = self.section_project(tab.project.as_ref());
                !self.state.project_folded(Some(&project))
            })
            .take(SHORTCUT_CARDS)
            .count()
    }

    /// Go to sidebar card `number` (1-based), counted among the cards shown.
    pub(super) fn open_sidebar_card(
        &mut self,
        number: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sections = self.sidebar_sections();
        let tab = shown_cards(&sections)
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

    /// Asks to rename tab `id`: a chat's thread, whose name shows wherever the thread
    /// does, else the tab's label.
    pub(super) fn rename_tab(&mut self, id: &TabId) {
        let Some(tab) = self.tab_by_id(id) else {
            return;
        };
        let (purpose, title, name) = match self.tab_chat_content(tab) {
            Some(chat) => {
                let name = self.thread_name(&chat.id).unwrap_or_default();
                (
                    TextPurpose::RenameThread(chat.id.clone()),
                    "Rename thread",
                    name,
                )
            }
            None => (
                TextPurpose::RenameTab(id.clone()),
                "Rename terminal tab",
                tab.label.clone(),
            ),
        };
        self.open_overlay(Overlay::Text { purpose, title }, &name);
    }

    /// Thread `chat`'s name: its header's while this window watches it, else the
    /// daemon's list.
    pub(super) fn thread_name(&self, chat: &str) -> Option<String> {
        self.watched_chat_title(chat).or_else(|| {
            self.sidebar
                .threads
                .iter()
                .find(|thread| thread.chat.id == chat)
                .map(|thread| thread_title(thread).to_owned())
        })
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
        // Keep the pending name field visible.
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
            let count = shown_cards(&self.sidebar_sections()).count();
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

    /// Once a run, saved WSL projects take the name the daemon gives their folder now:
    /// a folder behind a link to a Windows drive (`~/proj/x` → `/mnt/c/…/x`) is named by
    /// the drive, so the project kept under the link's name follows, settings and all,
    /// and finds its tabs again. A folder already kept under another name stays as is.
    pub(super) fn rename_linked_projects(&mut self, cx: &mut Context<Self>) {
        if self.sidebar.projects_renamed || self.connection_error.is_some() {
            return;
        }
        self.sidebar.projects_renamed = true;
        let linked: Vec<Project> = self
            .listed_projects()
            .into_iter()
            .map(|listed| listed.project)
            .filter(|project| {
                matches!(
                    project.environment,
                    compi_protocol::ContentEnvironment::Wsl { .. }
                ) && !project.root.starts_with("/mnt/")
            })
            .collect();
        if linked.is_empty() {
            return;
        }
        let target = self.target.clone();
        cx.spawn(async move |this, cx| {
            let renamed = cx
                .background_executor()
                .spawn(async move {
                    let Ok(mut client) = target.connect() else {
                        return Vec::new();
                    };
                    linked
                        .into_iter()
                        .filter_map(|old| {
                            let new = client
                                .resolve_project_folder(old.environment.clone(), old.root.clone())
                                .ok()?;
                            (new != old).then_some((old, new))
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                for (old, new) in renamed {
                    if this.config.kept_project(&new).is_some() {
                        continue;
                    }
                    let Some(settings) = this.project_settings(&old).cloned() else {
                        continue;
                    };
                    if this.persist_project_settings(&new, settings, cx) {
                        this.persist_project_settings(&old, Default::default(), cx);
                    }
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
                let sections = self.sidebar_sections();
                let count = shown_cards(&sections).count();
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
                if let Some(row) = card_row(&sections, next) {
                    self.sidebar.scroll.scroll_to_item(row);
                }
                true
            }
            "enter" => {
                let sections = self.sidebar_sections();
                let tab = self
                    .sidebar
                    .highlight
                    .or(searching.then_some(0))
                    .and_then(|at| shown_cards(&sections).nth(at))
                    .map(|card| card.tab.clone());
                if let Some(tab) = tab {
                    self.clear_sidebar_search(window, cx);
                    self.open_card(tab, window, cx);
                } else if searching && sections.len() == 1 {
                    let project = sections[0].project.clone();
                    self.clear_sidebar_search(window, cx);
                    self.open_project_settings(&project, window, cx);
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
                let sections = self.sidebar_sections();
                let tab = self
                    .sidebar
                    .highlight
                    .and_then(|at| shown_cards(&sections).nth(at))
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

    fn toggle_project_fold(&mut self, project: Option<&Project>) {
        self.state.toggle_project_folded(project);
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

    /// Project actions keep the workspace selection separate from the project folder.
    fn open_project_menu(
        &mut self,
        project: Project,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = self
            .workspace
            .as_ref()
            .and_then(|workspace| self.state.selected_session(workspace))
            .map(|session| session.id.clone());
        let rows = project_menu_rows(session.is_some());
        let entries = menu_entries(rows, |action, reason| {
            if action == ProjectAction::CopyPath {
                return copy_entry(action.label(), Some(project.root.clone()), reason);
            }
            let (session, project) = (session.clone(), project.clone());
            menu_item(
                action.label(),
                cx.listener(move |this, _, window, cx| {
                    match action {
                        ProjectAction::NewTerminal => {
                            if let Some(session) = &session {
                                this.new_project_terminal(session.clone(), &project);
                            }
                        }
                        ProjectAction::NewThread => {
                            if let Some(session) = &session {
                                this.new_project_thread(session.clone(), &project);
                            }
                        }
                        ProjectAction::Settings => this.open_project_settings(&project, window, cx),
                        ProjectAction::CopyPath => {}
                    }
                    cx.notify();
                }),
            )
            .disabled_because(reason)
            .into()
        });
        self.open_menu(position, Anchor::TopLeft, entries, window, cx);
    }

    /// A new terminal tab of workspace `session` whose shell starts at `project`'s
    /// folder: in the project's WSL distribution for a WSL project, else with the shell
    /// Settings → General picks (WSL reaches a Windows folder by its drive).
    pub(super) fn new_project_terminal(&mut self, session: SessionId, project: &Project) {
        use compi_protocol::{ContentEnvironment, WindowsShell};
        let distribution = match &project.environment {
            ContentEnvironment::Wsl { distribution } => Some(distribution.clone()),
            ContentEnvironment::Host => None,
        };
        self.mutate_with_launch(
            WorkspaceMutation::CreateTab {
                session_id: session,
                label: String::new(),
                cols: self.terminal_cols,
                rows: self.terminal_rows,
                working_directory: Some(project.root.clone()),
            },
            true,
            |launch| {
                if let Some(distribution) = distribution {
                    let profile = &mut launch.profile;
                    if profile.windows_shell.for_directory(Some(&project.root)) != WindowsShell::Wsl
                    {
                        profile.executable = None;
                        profile.args.clear();
                    }
                    profile.windows_shell = WindowsShell::Wsl;
                    profile.distribution = Some(distribution);
                }
            },
        );
    }

    /// New thread here: a chat at the project's root with the agent its new threads
    /// start with, in a new tab of workspace `session`.
    pub(super) fn new_project_thread(&mut self, session: SessionId, project: &Project) {
        let harness = self.new_thread_agent(Some(project));
        let chat = crate::chat::new_chat(
            harness,
            project.environment.clone(),
            project.root.clone(),
            self.config.chat_prefer(harness),
        );
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
                            .on_drag_move(cx.listener(
                                |this, event: &DragMoveEvent<CardDrag>, _, cx| {
                                    this.leave_card_drop(event, cx);
                                },
                            ))
                            .on_drop(cx.listener(|this, drag: &CardDrag, _, cx| {
                                this.drop_card(drag, cx);
                                cx.stop_propagation();
                            }))
                            .on_drag_move(cx.listener(
                                |this, event: &DragMoveEvent<ProjectDrag>, _, cx| {
                                    this.leave_project_drop(event, cx);
                                },
                            ))
                            .on_drop(cx.listener(|this, drag: &ProjectDrag, _, cx| {
                                this.drop_project(drag, cx);
                                cx.stop_propagation();
                            }))
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
        let add_project_reason = self.workspace.is_none().then_some("Not connected");
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
                self.sidebar_button_face(
                    "sidebar-add-project",
                    IconName::FolderPlus,
                    "Add project…".to_owned(),
                    add_project_reason,
                    false,
                    false,
                )
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .when(add_project_reason.is_none(), |button| {
                    button.on_click(cx.listener(|this, _, window, cx| {
                        this.open_add_project(false, window, cx);
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

    /// Bring the added (or already saved) project into view without starting work.
    pub(super) fn reveal_sidebar_project(
        &mut self,
        project: &Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_sidebar_search(window, cx);
        if self.state.project_folded(Some(project)) {
            self.toggle_project_fold(Some(project));
        }
        let mut row = 0;
        for section in self.sidebar_sections() {
            if section.project == *project {
                let scroll = self.sidebar.scroll.clone();
                window.on_next_frame(move |_, _| scroll.scroll_to_item(row));
                break;
            }
            row += 1 + if section.folded {
                0
            } else {
                section.cards.len()
            };
        }
        cx.notify();
    }

    fn render_sidebar_rows(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let colors = *self.colors();
        let sections = self.sidebar_sections();
        let now = now_ms();
        let mut rows = Vec::new();
        let mut shown = 0;
        if sections.is_empty() {
            rows.push(
                div()
                    .px_4()
                    .py_2()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .text_color(color(modal_text_color(colors.muted, &colors)))
                    .child(if searching(&self.sidebar.query) {
                        "No matches"
                    } else {
                        "No projects or open tabs"
                    })
                    .into_any_element(),
            );
        }
        let drop = self.card_drop(cx).cloned();
        let project_line = self.project_drop_line(cx).cloned();
        let count = sections.len();
        for (at, section) in sections.into_iter().enumerate() {
            let target = drop.as_ref().filter(|drop| drop.project == section.project);
            // A dragged heading's line sits on the heading its project lands before, or
            // under the list's last row.
            let last = at + 1 == count && project_line == Some(ProjectLine::Last);
            let cards_shown = !section.folded && !section.cards.is_empty();
            let header_line = match &project_line {
                Some(ProjectLine::Before(project)) if *project == section.project => Some(false),
                _ => (last && !cards_shown).then_some(true),
            };
            rows.push(
                self.render_project_header(
                    rows.len(),
                    &section,
                    target.is_some_and(|drop| drop.joins),
                    header_line,
                    cx,
                )
                .into_any_element(),
            );
            if section.folded {
                continue;
            }
            let order: Vec<TabId> = section.cards.iter().map(|card| card.tab.clone()).collect();
            for (index, card) in section.cards.into_iter().enumerate() {
                let highlighted = self.sidebar.highlight == Some(shown);
                let next = order.get(index + 1).cloned();
                // The line sits on the card a drop lands before, or under the last one.
                let line = target
                    .and_then(|drop| match &drop.before {
                        Some(before) if *before == card.tab => Some(false),
                        None if next.is_none() => Some(true),
                        _ => None,
                    })
                    .or((last && next.is_none()).then_some(true));
                let place = CardPlace {
                    section: section.project.clone(),
                    next,
                    line,
                };
                rows.push(self.render_card(shown, card, highlighted, place, now, cx));
                shown += 1;
            }
        }
        if let Some(rename) = &self.sidebar.rename {
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

    /// A saved project (even before a tab is open), or the Loose tabs section. While a
    /// card from another section is dragged over it, `drop_target` lights it; while a
    /// heading is dragged, `line` marks a dragged project landing over it (`false`) or
    /// under it (`true`).
    fn render_project_header(
        &self,
        row: usize,
        section: &ProjectSection,
        drop_target: bool,
        line: Option<bool>,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let colors = *self.colors();
        let muted = modal_text_color(colors.muted, &colors);
        let icon = self.project_icon(Some(&section.project), 16.0);
        let name = self.project_name(&section.project);
        let tooltip = controls::tooltip(
            name.clone(),
            Some(section.project.root.clone().into()),
            None,
        );
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
        let fold_project = section.project.clone();
        let menu_project = section.project.clone();
        let drop_project = section.project.clone();
        let over_project = section.project.clone();
        let dragged = self.project_drag(&section.project);
        div()
            .id(("sidebar-project", row))
            .relative()
            .mx_2()
            // Room between one project's cards and the next heading.
            .mt(px(if row == 0 { 4.0 } else { 14.0 }))
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
            .when(!section.cards.is_empty(), |header| {
                header
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_project_fold(Some(&fold_project));
                        cx.notify();
                    }))
            })
            .hover(move |style| style.bg(color(blend_rgb(colors.surface, colors.foreground, 0.05))))
            .when(drop_target, |header| {
                header
                    .bg(color(colors.accent).opacity(0.16))
                    .border_1()
                    .border_color(color(colors.accent).opacity(0.6))
            })
            // A card dropped on the heading joins the section at its end.
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<CardDrag>, _, cx| {
                    this.hover_card_drop(event, drop_project.clone(), None, cx);
                }),
            )
            // Dragging the heading moves the project among the others.
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<ProjectDrag>, _, cx| {
                    this.hover_project_drop(event, &over_project, cx);
                }),
            )
            .children(line.map(|bottom| self.card_drop_line(bottom)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.open_project_menu(menu_project.clone(), event.position, window, cx);
                    cx.stop_propagation();
                }),
            )
            .when(self.context_menu.is_none(), |header| {
                header.tooltip(tooltip)
            })
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
            .when(section.folded && !section.cards.is_empty(), |header| {
                header.child(
                    div()
                        .flex_none()
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .font_weight(FontWeight::NORMAL)
                        .text_color(color(muted))
                        .child(section.cards.len().to_string()),
                )
            })
            .when(!section.cards.is_empty(), |header| {
                header.child(controls::glyph(
                    if section.folded {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronDown
                    },
                    color(muted),
                ))
            })
            .child({
                let project = section.project.clone();
                controls::icon_button(
                    ("sidebar-project-actions", row),
                    IconName::Ellipsis,
                    "Project actions",
                )
                .on_click(cx.listener(
                    move |this, event: &ClickEvent, window, cx| {
                        this.open_project_menu(project.clone(), event.position(), window, cx);
                        cx.stop_propagation();
                    },
                ))
            })
    }

    fn render_card(
        &self,
        index: usize,
        card: SidebarCard,
        highlighted: bool,
        place: CardPlace,
        now: u64,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let (open, rename) = (card.tab.clone(), card.tab.clone());
        let (menu_tab, menu_session) = (card.tab.clone(), card.session.clone());
        let dragged = self.card_drag(&card);
        let CardPlace {
            section,
            next,
            line,
        } = place;
        let this_card = card.tab.clone();
        // Its kind leads the line: the agent's mark for a thread, else a terminal.
        let kind = if let Some(harness) = card.chat {
            img(super::chat::harness_mark(harness))
                .flex_none()
                .size(px(14.0))
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
        let mut detail = vec![format!(
            "{} · {}",
            self.project_name(&card.project),
            card.folder.as_deref().unwrap_or(&card.project.root)
        )];
        if card.pinned {
            detail.push("Pinned".to_owned());
        }
        if card.hidden {
            detail.push("Hidden".to_owned());
        }
        // What runs in each pane comes first, then where the tab is.
        let hover: Vec<String> = card
            .panes
            .iter()
            .cloned()
            .chain(std::iter::once(String::new()))
            .chain(detail)
            .collect();
        let tooltip = self
            .context_menu
            .is_none()
            .then(|| controls::tooltip(card.title.clone(), Some(hover.join("\n").into()), None));
        let background = if card.selected {
            Some(blend_rgb(colors.surface, colors.foreground, 0.09))
        } else if highlighted {
            Some(blend_rgb(colors.surface, colors.foreground, 0.06))
        } else {
            None
        };
        let row = div()
            .id(("sidebar-card", index))
            .relative()
            .px_2()
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
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            // Over its upper half a card lands before this one, over its lower half
            // before the next.
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<CardDrag>, _, cx| {
                    let lower = event.event.position.y > event.bounds.center().y;
                    let before = if lower {
                        next.clone()
                    } else {
                        Some(this_card.clone())
                    };
                    this.hover_card_drop(event, section.clone(), before, cx);
                }),
            )
            .children(line.map(|bottom| self.card_drop_line(bottom)))
            .when_some(tooltip, |card, tooltip| card.tooltip(tooltip))
            .child(
                div()
                    .flex_none()
                    .w(px(14.0))
                    .flex()
                    .justify_center()
                    .child(kind),
            )
            .child(
                div()
                    .id(("sidebar-card-title", index))
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .child(card.title.clone())
                    // A double click on the title renames the tab.
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        if event.click_count() >= 2 {
                            this.rename_tab(&rename);
                            cx.stop_propagation();
                            cx.notify();
                        }
                    })),
            )
            .children(card.count.clone().map(|count| {
                div()
                    .flex_none()
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .text_color(muted)
                    .child(count)
            }))
            .children(card_note(&card, now).map(|note| {
                div()
                    .flex_none()
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .text_color(muted)
                    .when(card.draft, |shown| shown.italic())
                    .child(note)
            }))
            .children(dot);
        // Indented under its project's heading, along a faint line from its icon.
        div()
            .relative()
            .pl(px(CARD_INDENT))
            .pr_2()
            .pb_0p5()
            .child(
                div()
                    .absolute()
                    .left(px(CARD_GUIDE_X))
                    .top_0()
                    .bottom_0()
                    .w(px(1.0))
                    .bg(color(blend_rgb(colors.surface, colors.foreground, 0.12))),
            )
            .child(row)
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

    /// Home, in these tests.
    fn home() -> Project {
        project("/home/me")
    }

    fn tab_card(id: &str, project: &Project) -> SidebarCard {
        SidebarCard {
            tab: TabId::from(id),
            session: SessionId::new("s"),
            title: String::new(),
            folder: None,
            project: project.clone(),
            chat: None,
            draft: false,
            state: None,
            changed_ms: None,
            hidden: false,
            pinned: false,
            selected: false,
            panes: Vec::new(),
            count: None,
        }
    }

    #[test]
    fn a_card_counts_its_panes_by_kind_when_it_has_several() {
        use PaneKind::{Other, Terminal, Thread};
        assert_eq!(pane_count(&[Thread]), None);
        assert_eq!(pane_count(&[Thread, Thread]).as_deref(), Some("2 threads"));
        assert_eq!(
            pane_count(&[Terminal, Terminal]).as_deref(),
            Some("2 terminals")
        );
        assert_eq!(
            pane_count(&[Thread, Terminal, Other]).as_deref(),
            Some("3 panes")
        );
    }

    #[test]
    fn a_draft_card_says_draft_where_a_thread_says_its_age() {
        let now = 10 * 3_600_000;
        let mut card = SidebarCard {
            chat: Some(ChatHarness::Omp),
            changed_ms: Some(now - 2 * 3_600_000),
            ..tab_card("t1", &home())
        };
        assert_eq!(card_note(&card, now).as_deref(), Some("2h"));
        card.draft = true;
        assert_eq!(card_note(&card, now).as_deref(), Some("Draft"));
        card.changed_ms = None;
        assert_eq!(card_note(&card, now).as_deref(), Some("Draft"));
        card.draft = false;
        assert_eq!(card_note(&card, now), None);
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
        let rows = project_menu_rows(true);
        assert_eq!(
            rows,
            [
                Some((ProjectAction::NewTerminal, None)),
                Some((ProjectAction::NewThread, None)),
                None,
                Some((ProjectAction::CopyPath, None)),
                None,
                Some((ProjectAction::Settings, None)),
            ]
        );
        assert_eq!(
            rows.iter()
                .flatten()
                .map(|(action, _)| action.label())
                .collect::<Vec<_>>(),
            [
                "New terminal here",
                "New thread here",
                "Copy path",
                "Project settings…"
            ]
        );
        let unavailable: Vec<_> = project_menu_rows(false)
            .into_iter()
            .flatten()
            .filter_map(|(action, reason)| reason.map(|_| action))
            .collect();
        assert_eq!(
            unavailable,
            [ProjectAction::NewTerminal, ProjectAction::NewThread]
        );
    }

    #[test]
    fn the_search_matches_every_word_in_the_title_folder_or_project() {
        let fields = ["Fix the build", "compi", "compi-site"];
        assert!(matches_query("", &fields));
        assert!(matches_query("build", &fields));
        assert!(matches_query("fix site", &fields));
        assert!(matches_query("comp", &fields));
        assert!(!matches_query("fix deploy", &fields));
        let mut card = tab_card("a", &home());
        card.title = "Release Notes".into();
        card.folder = Some("docs".into());
        assert!(card.matches("release"));
        assert!(card.matches("notes docs"));
        assert!(!card.matches("compi"));
        card.project = project("/work/compi");
        assert!(card.matches("compi"));
    }

    #[test]
    fn saved_projects_without_tabs_stay_visible_and_unknown_tabs_are_in_home() {
        let empty = project("/work/compi");
        let saved = project("/work/site");
        let unsaved = project("/work/other");
        let sections = project_sections(
            vec![empty.clone(), saved.clone(), home()],
            &home(),
            [
                tab_card("home", &home()),
                tab_card("site", &saved),
                tab_card("other", &unsaved),
            ],
            |_| false,
        );
        let headers: Vec<_> = sections.iter().map(|section| &section.project).collect();
        assert_eq!(headers, [&empty, &saved, &home()]);
        assert!(sections[0].cards.is_empty());
        assert_eq!(
            targets(sections[2].cards.iter()),
            tab_targets(&["home", "other"])
        );
        assert_eq!(
            targets(shown_cards(&sections)),
            tab_targets(&["site", "home", "other"])
        );
        assert_eq!(card_row(&sections, 0), Some(2));
        assert_eq!(card_row(&sections, 1), Some(4));
    }

    #[test]
    fn folded_project_cards_are_skipped_by_sidebar_shortcuts() {
        let compi = project("/work/compi");
        let sections = project_sections(
            vec![compi.clone(), home()],
            &home(),
            [tab_card("a", &compi), tab_card("b", &home())],
            |project| *project == compi,
        );
        assert_eq!(targets(shown_cards(&sections)), tab_targets(&["b"]));
        assert_eq!(card_row(&sections, 0), Some(2));
        assert_eq!(card_row(&sections, 1), None);
    }

    #[test]
    fn a_folded_project_header_shows_its_most_urgent_card() {
        let mut cards = vec![
            tab_card("a", &home()),
            tab_card("b", &home()),
            tab_card("c", &home()),
        ];
        cards[0].state = Some(ProgramState::Done);
        cards[1].state = Some(ProgramState::Error);
        cards[2].state = Some(ProgramState::Working);
        let mut section = ProjectSection {
            project: home(),
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
