//! The window's sidebar, with T3 Code's rounded cards and search bar in Compi's own
//! look. At the top, the search with New workspace, New tab and New thread beside it.
//! Under it, each workspace as a plain label that folds, over its cards: one per tab,
//! and one per thread no pane holds. At the bottom, a row of icon buttons, with the
//! update button at its end while a release is on offer.
//!
//! Cards keep their workspace's tab order, pinned tabs first in the order they were
//! pinned, then the workspace's closed threads, newest first; nothing reorders them on
//! its own. A card shows the project's icon and folder with how long ago its status
//! changed, the chat's or the tab's title, whether a chat or a terminal is in it, and
//! one dot for what its programs most need the user for.
//!
//! Typing in the search shows only the cards whose title, folder or project matches;
//! with the search holding the keyboard, Up and Down move through them, Enter opens
//! one, Escape clears the search and F2 renames the tab. Ctrl+1–9 (Cmd+1–9 on macOS)
//! open the first nine cards shown.
//!
//! Closed threads come from the daemon's thread list, asked for off the UI thread when
//! the workspace changes and every ten seconds while the sidebar shows. A thread goes
//! under the workspace whose tab last held it, else under the selected workspace.
use super::*;
use crate::chat::{age_label, now_ms, thread_title};
use crate::gui::controls::{IconName, MenuEntry, menu_item};
use compi_protocol::chat::{ChatContent, ThreadInfo};
use compi_protocol::program_status::ProgramState;
use compi_protocol::{PaneContent, Project};
use gpui::{Entity, img};
use gpui_component::Sizable as _;
use gpui_component::input::{Input, InputEvent, InputState};

/// How often the sidebar lists the daemon's threads while it shows.
const THREAD_LISTING_INTERVAL: Duration = Duration::from_secs(10);
/// The cards Go to sidebar card 1–9 reach.
const SHORTCUT_CARDS: usize = 9;

/// The sidebar's own state: its search, which workspaces are folded and the threads
/// last listed.
pub(in crate::gui) struct SidebarUi {
    search: Entity<InputState>,
    _search_events: Subscription,
    /// The search, trimmed and lowercased.
    query: String,
    /// The shown card Up and Down moved to while the search has the keyboard.
    highlight: Option<usize>,
    /// Workspaces folded to their label.
    folded: HashSet<SessionId>,
    scroll: ScrollHandle,
    /// The daemon's threads as last listed.
    threads: Vec<ThreadInfo>,
    /// A listing is under way.
    listing: bool,
    /// The workspace changed since the last listing began.
    listing_due: bool,
    /// When the last listing began.
    listed_at: Option<Instant>,
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
            folded: HashSet::new(),
            scroll: ScrollHandle::new(),
            threads: Vec::new(),
            listing: false,
            listing_due: true,
            listed_at: None,
        }
    }

    /// The workspace changed: the threads are listed again.
    pub(in crate::gui) fn workspace_changed(&mut self) {
        self.listing_due = true;
    }
}

/// What a card opens: a tab, or a thread no pane holds (by its chat's ID).
#[derive(Clone, Debug, PartialEq, Eq)]
enum CardTarget {
    Tab(TabId),
    Thread(String),
}

/// One card as the sidebar draws it.
struct SidebarCard {
    target: CardTarget,
    /// The workspace whose group shows it.
    session: SessionId,
    title: String,
    /// The project's name, else the folder's.
    folder: Option<String>,
    project: Option<Project>,
    /// A chat is in it, rather than a terminal.
    chat: bool,
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

/// One workspace's label and the cards under it that the search matches.
struct SidebarGroup {
    session: SessionId,
    label: String,
    selected: bool,
    folded: bool,
    cards: Vec<SidebarCard>,
}

/// A workspace's cards in order: pinned tabs in the order pinned, the other tabs in
/// the workspace's order, then its closed `threads` as given.
fn card_order(tabs: &[TabId], pinned: &[TabId], threads: &[&ThreadInfo]) -> Vec<CardTarget> {
    pinned
        .iter()
        .filter(|id| tabs.contains(id))
        .chain(tabs.iter().filter(|id| !pinned.contains(id)))
        .map(|id| CardTarget::Tab(id.clone()))
        .chain(
            threads
                .iter()
                .map(|thread| CardTarget::Thread(thread.chat.id.clone())),
        )
        .collect()
}

/// The threads no pane holds, by the workspace whose group shows them, newest first:
/// the workspace whose tab last held a thread when it is one of `sessions`, else
/// `fallback`. `held` says a pane holds a thread although the listing is older.
fn closed_threads<'a>(
    threads: &'a [ThreadInfo],
    sessions: &[SessionId],
    fallback: Option<&SessionId>,
    held: impl Fn(&str) -> bool,
) -> HashMap<SessionId, Vec<&'a ThreadInfo>> {
    let mut grouped: HashMap<SessionId, Vec<&ThreadInfo>> = HashMap::new();
    for thread in threads
        .iter()
        .filter(|thread| !thread.open && !held(&thread.chat.id))
    {
        let session = thread
            .workspace
            .as_ref()
            .filter(|workspace| sessions.contains(workspace))
            .or(fallback);
        if let Some(session) = session {
            grouped.entry(session.clone()).or_default().push(thread);
        }
    }
    for threads in grouped.values_mut() {
        threads.sort_by(|a, b| {
            b.created_ms
                .cmp(&a.created_ms)
                .then_with(|| a.chat.id.cmp(&b.chat.id))
        });
    }
    grouped
}

/// Whether every word of `query` (lowercase) is in one of `fields`.
fn matches_query(query: &str, fields: &[&str]) -> bool {
    let fields: Vec<String> = fields.iter().map(|field| field.to_lowercase()).collect();
    query
        .split_whitespace()
        .all(|word| fields.iter().any(|field| field.contains(word)))
}

/// Whether `groups` are listed for a search: every group shows its matching cards,
/// folded or not.
fn searching(query: &str) -> bool {
    !query.is_empty()
}

/// The cards shown, top to bottom: while searching every match, otherwise the cards of
/// the workspaces that are not folded.
fn shown_cards(groups: &[SidebarGroup], searching: bool) -> impl Iterator<Item = &SidebarCard> {
    groups
        .iter()
        .filter(move |group| searching || !group.folded)
        .flat_map(|group| &group.cards)
}

/// The row of the sidebar's list that shows card `shown` (an index into
/// [`shown_cards`]): each group is its label, then its cards, or a placeholder when it
/// shows none.
fn card_row(groups: &[SidebarGroup], searching: bool, shown: usize) -> Option<usize> {
    let mut row = 0;
    let mut card = 0;
    for group in groups {
        row += 1;
        if group.folded && !searching {
            continue;
        }
        if group.cards.is_empty() {
            row += 1;
            continue;
        }
        if shown < card + group.cards.len() {
            return Some(row + shown - card);
        }
        row += group.cards.len();
        card += group.cards.len();
    }
    None
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

impl CompiApp {
    /// The sidebar's groups, each with the cards the search matches; while searching,
    /// only groups with a match.
    fn sidebar_groups(&self) -> Vec<SidebarGroup> {
        let Some(workspace) = &self.workspace else {
            return Vec::new();
        };
        let query = &self.sidebar.query;
        let selected_session = self
            .state
            .selected_session(workspace)
            .map(|session| session.id.clone());
        let selected_tab = self.selected_tab().map(|tab| tab.id.clone());
        let sessions: Vec<SessionId> = workspace
            .sessions
            .iter()
            .map(|session| session.id.clone())
            .collect();
        let threads = closed_threads(
            &self.sidebar.threads,
            &sessions,
            selected_session.as_ref(),
            |chat| program_status::chat_place(workspace, chat).is_some(),
        );
        workspace
            .sessions
            .iter()
            .filter_map(|session| {
                let tabs: Vec<TabId> = session.tabs.iter().map(|tab| tab.id.clone()).collect();
                let closed = threads.get(&session.id).map_or(&[][..], Vec::as_slice);
                let cards: Vec<SidebarCard> = card_order(&tabs, &self.state.pinned_tabs, closed)
                    .into_iter()
                    .filter_map(|target| match target {
                        CardTarget::Tab(id) => {
                            let tab = session.tabs.iter().find(|tab| tab.id == id)?;
                            Some(self.tab_card(&session.id, tab, selected_tab.as_ref()))
                        }
                        CardTarget::Thread(chat) => {
                            let thread = closed.iter().find(|thread| thread.chat.id == chat)?;
                            Some(self.thread_card(&session.id, thread))
                        }
                    })
                    .filter(|card| card.matches(query))
                    .collect();
                if searching(query) && cards.is_empty() {
                    return None;
                }
                Some(SidebarGroup {
                    session: session.id.clone(),
                    label: session.label.clone(),
                    selected: selected_session.as_ref() == Some(&session.id),
                    folded: self.sidebar.folded.contains(&session.id),
                    cards,
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
        let title = match chat {
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
            target: CardTarget::Tab(tab.id.clone()),
            session: session.clone(),
            title,
            folder,
            project: tab.project.clone(),
            chat: chat.is_some(),
            state,
            changed_ms: self
                .tab_status_changed_ms(tab)
                .or_else(|| listed.map(|thread| thread.updated_ms)),
            hidden: self.state.hidden_tabs.contains(&tab.id),
            pinned: self.state.pinned_tabs.contains(&tab.id),
            selected: selected_tab == Some(&tab.id),
        }
    }

    fn thread_card(&self, session: &SessionId, thread: &ThreadInfo) -> SidebarCard {
        let state = self
            .chat_program_headline(&thread.chat.id)
            .filter(|_| self.config.program_status.sidebar)
            .map(|headline| headline.state)
            .filter(|state| *state != ProgramState::Idle);
        SidebarCard {
            target: CardTarget::Thread(thread.chat.id.clone()),
            session: session.clone(),
            title: thread_title(thread).to_owned(),
            folder: Some(match &thread.project {
                Some(project) => project.name().to_owned(),
                None => folder_name(&thread.chat.folder).to_owned(),
            }),
            project: thread.project.clone(),
            chat: true,
            state,
            changed_ms: Some(thread.updated_ms),
            hidden: false,
            pinned: false,
            selected: false,
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
        let sessions: Vec<SessionId> = workspace
            .sessions
            .iter()
            .map(|session| session.id.clone())
            .collect();
        let fallback = self
            .state
            .selected_session(workspace)
            .map(|session| session.id.clone());
        let threads = closed_threads(
            &self.sidebar.threads,
            &sessions,
            fallback.as_ref(),
            |chat| program_status::chat_place(workspace, chat).is_some(),
        );
        workspace
            .sessions
            .iter()
            .filter(|session| !self.sidebar.folded.contains(&session.id))
            .map(|session| session.tabs.len() + threads.get(&session.id).map_or(0, Vec::len))
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
        let card = shown_cards(&groups, searching(&self.sidebar.query))
            .nth(number.saturating_sub(1))
            .map(|card| (card.target.clone(), card.session.clone()));
        if let Some((target, session)) = card {
            self.open_card(&target, &session, window, cx);
        }
    }

    /// Opens a card: selects its tab, or opens its thread in its workspace; then gives
    /// the panes the keyboard.
    fn open_card(
        &mut self,
        target: &CardTarget,
        session: &SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            CardTarget::Tab(id) => self.select_terminal(id.clone(), true),
            CardTarget::Thread(chat) => {
                let thread = self
                    .sidebar
                    .threads
                    .iter()
                    .find(|thread| &thread.chat.id == chat)
                    .cloned();
                if let Some(thread) = thread {
                    self.open_thread(
                        thread.chat,
                        thread.terminal,
                        Some(session.clone()),
                        window,
                        cx,
                    );
                }
            }
        }
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

    /// Takes the daemon's thread list.
    pub(super) fn sidebar_threads_listed(&mut self, threads: Vec<ThreadInfo>) {
        self.sidebar.threads = threads;
        self.clamp_sidebar_highlight();
    }

    /// Thread `chat` was deleted.
    pub(super) fn sidebar_thread_deleted(&mut self, chat: &str) {
        self.sidebar.threads.retain(|thread| thread.chat.id != chat);
        self.clamp_sidebar_highlight();
    }

    fn clamp_sidebar_highlight(&mut self) {
        if let Some(highlight) = self.sidebar.highlight {
            let groups = self.sidebar_groups();
            let count = shown_cards(&groups, searching(&self.sidebar.query)).count();
            self.sidebar.highlight = (count > 0).then(|| highlight.min(count - 1));
        }
    }

    /// Lists the daemon's threads off the UI thread once the workspace changed, and
    /// every `THREAD_LISTING_INTERVAL` while the sidebar shows; one listing at a time.
    pub(super) fn poll_sidebar_threads(&mut self, cx: &mut Context<Self>) {
        let sidebar = &self.sidebar;
        let interval_due = self.sidebar_open
            && sidebar
                .listed_at
                .is_none_or(|at| at.elapsed() >= THREAD_LISTING_INTERVAL);
        if sidebar.listing
            || self.workspace.is_none()
            || self.connection_error.is_some()
            || !(sidebar.listing_due || interval_due)
        {
            return;
        }
        self.sidebar.listing = true;
        self.sidebar.listing_due = false;
        self.sidebar.listed_at = Some(Instant::now());
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

    /// The sidebar's search has the keyboard.
    pub(super) fn sidebar_search_has_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.sidebar_open
            && self.overlay.is_none()
            && self.sidebar.search.focus_handle(cx).is_focused(window)
    }

    /// Keys in the sidebar's search, before the field's own: Up and Down move through
    /// the cards shown, Enter opens the highlighted one (else the first match), Escape
    /// clears the search or, when it is empty, gives the panes the keyboard, and F2
    /// renames the highlighted tab, else the selected one.
    pub(super) fn sidebar_search_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.sidebar_search_has_keyboard(window, cx) {
            return false;
        }
        let modifiers = &key.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.shift {
            return false;
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
                let card = self
                    .sidebar
                    .highlight
                    .or(searching.then_some(0))
                    .and_then(|at| shown_cards(&groups, searching).nth(at))
                    .map(|card| (card.target.clone(), card.session.clone()));
                if let Some((target, session)) = card {
                    self.clear_sidebar_search(window, cx);
                    self.open_card(&target, &session, window, cx);
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
                let highlighted = self
                    .sidebar
                    .highlight
                    .and_then(|at| shown_cards(&groups, searching).nth(at))
                    .map(|card| card.target.clone());
                let tab = match highlighted {
                    Some(CardTarget::Tab(id)) => Some(id),
                    Some(CardTarget::Thread(_)) => None,
                    None => self.selected_tab().map(|tab| tab.id.clone()),
                };
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
        if !self.sidebar.folded.remove(session) {
            self.sidebar.folded.insert(session.clone());
        }
        self.sidebar.highlight = None;
    }

    /// Right-click on a card: Pin or Unpin, Rename… and Close for a tab; Open and Delete
    /// for a closed thread.
    fn open_card_menu(
        &mut self,
        target: CardTarget,
        session: SessionId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries: Vec<MenuEntry> = match target {
            CardTarget::Tab(id) => {
                let pinned = self.state.pinned_tabs.contains(&id);
                let (pin, rename, close) = (id.clone(), id.clone(), id);
                vec![
                    menu_item(
                        if pinned { "Unpin" } else { "Pin" },
                        cx.listener(move |this, _, _, cx| {
                            this.state.set_pinned(&pin, !pinned);
                            this.save_state();
                            cx.notify();
                        }),
                    )
                    .into(),
                    menu_item(
                        "Rename…",
                        cx.listener(move |this, _, _, cx| {
                            this.rename_tab(&rename);
                            cx.notify();
                        }),
                    )
                    .into(),
                    MenuEntry::Separator,
                    menu_item(
                        "Close",
                        cx.listener(move |this, _, window, cx| {
                            this.close_work(
                                WorkspaceMutation::RemoveTab {
                                    tab_id: close.clone(),
                                },
                                "Close this tab and end every process in its panes?",
                                "Close tab",
                                window,
                                cx,
                            );
                            cx.notify();
                        }),
                    )
                    .into(),
                ]
            }
            CardTarget::Thread(chat) => {
                let (open, delete) = (CardTarget::Thread(chat.clone()), chat);
                vec![
                    menu_item(
                        "Open",
                        cx.listener(move |this, _, window, cx| {
                            this.open_card(&open, &session, window, cx);
                        }),
                    )
                    .into(),
                    MenuEntry::Separator,
                    menu_item(
                        "Delete",
                        cx.listener(move |this, _, _, cx| {
                            this.delete_thread(delete.clone(), cx);
                            cx.notify();
                        }),
                    )
                    .into(),
                ]
            }
        };
        self.open_menu(position, Anchor::TopLeft, entries, window, cx);
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
            .child(self.sidebar_command(
                "sidebar-new-workspace",
                IconName::FolderPlus,
                "New workspace",
                Command::CreateWorkspace,
                context,
                cx,
            ))
            .child(self.sidebar_command(
                "sidebar-new-tab",
                IconName::SquareTerminal,
                "New tab",
                Command::NewTab,
                context,
                cx,
            ))
            .child(self.sidebar_command(
                "sidebar-new-thread",
                IconName::MessageSquarePlus,
                "New thread",
                Command::OpenChat,
                context,
                cx,
            ))
    }

    /// Settings, Commands, Appearance, Changes and Monitor, then the update button.
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
            .px_2()
            .py_1p5()
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
                context,
                cx,
            ))
            .child(self.sidebar_command(
                "workspace-actions",
                IconName::Command,
                "Commands",
                Command::OpenPalette,
                context,
                cx,
            ))
            .child(
                self.render_quick_appearance(
                    "quick-appearance",
                    self.sidebar_button_face(
                        "appearance-sidebar",
                        IconName::Palette,
                        self.command_title("Appearance", Command::OpenQuickAppearance),
                        None,
                        appearance_open,
                    )
                    .into_any_element(),
                    Anchor::BottomLeft,
                    window,
                    cx,
                ),
            )
            .child(self.sidebar_command(
                "changes-sidebar",
                IconName::FileDiff,
                "Changes",
                Command::ShowChanges,
                context,
                cx,
            ))
            .child(self.sidebar_command(
                "monitor-sidebar",
                IconName::Gauge,
                "Monitor",
                Command::OpenMonitor,
                context,
                cx,
            ))
            .child(div().flex_1())
            .children(self.render_update_button(cx))
    }

    /// `label`, with `command`'s shortcut when it has one.
    fn command_title(&self, label: &str, command: Command) -> String {
        match self.configured_shortcut(command) {
            Some(shortcut) => format!("{label} · {}", controls::key_label(shortcut)),
            None => label.to_owned(),
        }
    }

    /// An icon button of the sidebar, without its click: also a popover's trigger.
    fn sidebar_button_face(
        &self,
        id: &'static str,
        icon: IconName,
        title: String,
        reason: Option<&'static str>,
        active: bool,
    ) -> Stateful<Div> {
        let colors = *self.colors();
        let disabled = reason.is_some();
        let tint = if active {
            colors.accent
        } else {
            modal_text_color(colors.muted, &colors)
        };
        div()
            .id(id)
            .flex_none()
            .size(px(26.0))
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
            .child(controls::glyph(icon, color(tint)).with_size(gpui_component::Size::Small))
    }

    /// An icon button running `command`, named `label` in its tooltip with its shortcut,
    /// or why it cannot run.
    fn sidebar_command(
        &self,
        id: &'static str,
        icon: IconName,
        label: &str,
        command: Command,
        context: &commands::CommandContext,
        cx: &Context<Self>,
    ) -> AnyElement {
        let reason = command.disabled_reason(context);
        self.sidebar_button_face(id, icon, self.command_title(label, command), reason, false)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .when(reason.is_none(), |button| {
                button.on_click(cx.listener(move |this, _, window, cx| {
                    this.execute(command, window, cx);
                    cx.stop_propagation();
                }))
            })
            .into_any_element()
    }

    /// The list's rows: each workspace's label, then its cards, or a line saying it has
    /// none.
    fn render_sidebar_rows(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let colors = *self.colors();
        let muted = modal_text_color(colors.muted, &colors);
        let searching = searching(&self.sidebar.query);
        let groups = self.sidebar_groups();
        let now = now_ms();
        let mut rows = Vec::new();
        let mut shown = 0;
        for (index, group) in groups.into_iter().enumerate() {
            let open = searching || !group.folded;
            let fold = group.session.clone();
            let count = group.cards.len();
            rows.push(
                div()
                    .id(("sidebar-group", index))
                    .px_3()
                    .pt_3()
                    .pb_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color(if group.selected {
                        colors.foreground
                    } else {
                        muted
                    }))
                    .cursor_pointer()
                    .hover(move |style| style.text_color(color(colors.foreground)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_sidebar_fold(&fold);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(group.label.clone()),
                    )
                    .when(!open && count > 0, |label| {
                        label.child(div().flex_none().child(count.to_string()))
                    })
                    .into_any_element(),
            );
            if !open {
                continue;
            }
            if group.cards.is_empty() {
                let session = group.session.clone();
                rows.push(
                    div()
                        .id(("sidebar-empty", index))
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
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(workspace) = &this.workspace {
                                this.state.select_session(workspace, &session);
                            }
                            this.sync_visible_views();
                            this.save_state();
                            cx.notify();
                        }))
                        .child("No tabs")
                        .into_any_element(),
                );
                continue;
            }
            for card in group.cards {
                let highlighted = self.sidebar.highlight == Some(shown);
                rows.push(self.render_card(shown, card, highlighted, now, cx));
                shown += 1;
            }
        }
        rows
    }

    fn render_card(
        &self,
        index: usize,
        card: SidebarCard,
        highlighted: bool,
        now: u64,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let tab = match &card.target {
            CardTarget::Tab(id) => Some(id.clone()),
            CardTarget::Thread(_) => None,
        };
        let (open_target, open_session) = (card.target.clone(), card.session.clone());
        let (menu_target, menu_session) = (card.target.clone(), card.session.clone());
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
        div()
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
                this.open_card(&open_target, &open_session, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.open_card_menu(
                        menu_target.clone(),
                        menu_session.clone(),
                        event.position,
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
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
                            .children(
                                card.changed_ms
                                    .map(|then| div().flex_none().child(age_label(then, now))),
                            ),
                    )
                    .child(
                        div()
                            .id(("sidebar-card-title", index))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .child(card.title)
                            // A double click on a tab's title renames the tab.
                            .when_some(tab, |title, tab| {
                                title.on_click(cx.listener(
                                    move |this, event: &ClickEvent, _, cx| {
                                        if event.click_count() >= 2 {
                                            this.rename_tab(&tab);
                                            cx.stop_propagation();
                                            cx.notify();
                                        }
                                    },
                                ))
                            }),
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
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::ContentEnvironment;
    use compi_protocol::chat::ThreadActivity;

    fn tabs(ids: &[&str]) -> Vec<TabId> {
        ids.iter().map(|id| TabId::from(*id)).collect()
    }

    fn thread(id: &str, workspace: Option<&str>, created_ms: u64, open: bool) -> ThreadInfo {
        let mut chat = crate::chat::new_chat(ContentEnvironment::Host, "/work/compi".into());
        chat.id = id.into();
        ThreadInfo {
            chat,
            title: None,
            created_ms,
            updated_ms: created_ms,
            activity: ThreadActivity::Idle,
            terminal: None,
            open,
            project: None,
            workspace: workspace.map(SessionId::new),
        }
    }

    fn card(target: CardTarget) -> SidebarCard {
        SidebarCard {
            target,
            session: SessionId::new("s"),
            title: String::new(),
            folder: None,
            project: None,
            chat: false,
            state: None,
            changed_ms: None,
            hidden: false,
            pinned: false,
            selected: false,
        }
    }

    fn group(folded: bool, ids: &[&str]) -> SidebarGroup {
        SidebarGroup {
            session: SessionId::new("s"),
            label: String::new(),
            selected: false,
            folded,
            cards: ids
                .iter()
                .map(|id| card(CardTarget::Tab(TabId::from(*id))))
                .collect(),
        }
    }

    #[test]
    fn cards_keep_the_tab_order_with_pins_first_then_closed_threads() {
        let older = thread("older", None, 1, false);
        let newer = thread("newer", None, 2, false);
        let order = card_order(
            &tabs(&["a", "b", "c", "d"]),
            &tabs(&["c", "gone", "a"]),
            &[&newer, &older],
        );
        assert_eq!(
            order,
            [
                CardTarget::Tab(TabId::from("c")),
                CardTarget::Tab(TabId::from("a")),
                CardTarget::Tab(TabId::from("b")),
                CardTarget::Tab(TabId::from("d")),
                CardTarget::Thread("newer".into()),
                CardTarget::Thread("older".into()),
            ]
        );
    }

    #[test]
    fn closed_threads_go_under_the_workspace_that_last_held_them_newest_first() {
        let threads = [
            thread("one", Some("w1"), 1, false),
            thread("two", Some("w2"), 2, false),
            thread("three", Some("w1"), 3, false),
            thread("open", Some("w1"), 4, true),
            thread("gone", Some("removed"), 5, false),
            thread("nowhere", None, 6, false),
            thread("reopened", Some("w2"), 7, false),
        ];
        let sessions = [SessionId::new("w1"), SessionId::new("w2")];
        let selected = SessionId::new("w2");
        let grouped = closed_threads(&threads, &sessions, Some(&selected), |chat| {
            chat == "reopened"
        });
        let ids = |session: &str| -> Vec<&str> {
            grouped[&SessionId::new(session)]
                .iter()
                .map(|thread| thread.chat.id.as_str())
                .collect()
        };
        assert_eq!(ids("w1"), ["three", "one"]);
        // A workspace that is gone, or none, puts the thread under the selected one.
        assert_eq!(ids("w2"), ["nowhere", "gone", "two"]);
        assert_eq!(grouped.len(), 2);
    }

    #[test]
    fn the_search_matches_every_word_in_the_title_folder_or_project() {
        let fields = ["Fix the build", "compi", "compi-site"];
        assert!(matches_query("", &fields));
        assert!(matches_query("build", &fields));
        assert!(matches_query("fix site", &fields));
        assert!(matches_query("comp", &fields));
        assert!(!matches_query("fix deploy", &fields));
        let mut card = card(CardTarget::Tab(TabId::from("a")));
        card.title = "Release Notes".into();
        card.folder = Some("docs".into());
        assert!(card.matches("release"));
        assert!(card.matches("notes docs"));
        assert!(!card.matches("compi"));
        card.project = Some(Project {
            environment: ContentEnvironment::Host,
            root: "/work/compi".into(),
        });
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
        let shown = |searching| -> Vec<CardTarget> {
            shown_cards(&groups, searching)
                .map(|card| card.target.clone())
                .collect()
        };
        assert_eq!(
            shown(false),
            [
                CardTarget::Tab(TabId::from("a")),
                CardTarget::Tab(TabId::from("b")),
                CardTarget::Tab(TabId::from("d")),
            ]
        );
        // A search lists matches in folded workspaces too.
        assert_eq!(shown(true).len(), 4);
        // Rows: label a b | label | label "No tabs" | label d.
        assert_eq!(card_row(&groups, false, 0), Some(1));
        assert_eq!(card_row(&groups, false, 1), Some(2));
        assert_eq!(card_row(&groups, false, 2), Some(7));
        assert_eq!(card_row(&groups, false, 3), None);
        assert_eq!(card_row(&groups, true, 2), Some(4));
    }
}
