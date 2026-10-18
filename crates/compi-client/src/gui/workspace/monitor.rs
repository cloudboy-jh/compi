//! Monitor: a content pane listing the daemon host's processes.
//!
//! Which processes a pane shows, in what order and how their values read is pure
//! (`crate::monitor`); this draws it and keeps it fed. One watching connection per
//! window carries the samples, and it runs only while a Monitor pane is drawn (in
//! the tab shown, or floating): [`LINGER`] after the last one goes out of sight it
//! closes, which stops the daemon's sampling unless another window still watches.
//! Samples go through a [`ProcessTable`] per connection, since the daemon sends each
//! process's details once per connection.
//!
//! The pane itself (`PaneContent::Monitor`) holds nothing; the search, filter, sort
//! and selection are this window's, per pane. While the pointer is over the table its
//! rows keep their order (values update in place, newcomers go last, leavers go), and
//! the table sorts again when the pointer leaves or the sort, search or filter changes.
use super::*;
use crate::gui::controls::IconName;
use crate::monitor::{self as listing, Column, Direction, Filter, ProcessKey, Search, Sort, Step};
use compi_protocol::system_metrics::{Process, ProcessDetails, ProcessTable, SystemSample};
use compi_protocol::{MonitorContent, PaneContent};
use gpui::{Entity, ScrollStrategy, UniformListScrollHandle, uniform_list};
use gpui_component::Sizable as _;
use gpui_component::input::{Input, InputEvent, InputState};
use std::sync::mpsc;

/// How often the daemon samples while watched.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);
/// Without a sample for this long, the values shown are stale.
const STALE_AFTER: Duration = Duration::from_secs(3 * SAMPLE_INTERVAL.as_secs());
/// The watcher outlives the last shown pane by this much, so flicking between tabs does
/// not reconnect.
const LINGER: Duration = Duration::from_secs(2);
/// How often the watching connection looks for samples and for being stopped.
const WATCH_POLL: Duration = Duration::from_millis(50);
/// Wait before reconnecting after the connection failed or was released.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);
/// One table row.
const ROW_HEIGHT: f32 = 24.0;
/// The table's inset from the pane's edges.
const TABLE_INSET: f32 = 12.0;
/// Narrower than this, the header drops its title, then its process count.
const HEADER_TITLE_MIN_WIDTH: f32 = 600.0;
const HEADER_COUNT_MIN_WIDTH: f32 = 480.0;
/// Longest command line a name's tooltip shows.
const TOOLTIP_COMMAND_CHARS: usize = 400;
const UNAVAILABLE: &str = "—";

/// What the window's watcher reports, tagged with the watcher's generation.
pub(in crate::gui) enum MonitorEvent {
    /// Watching; no sample yet.
    Connecting,
    Sampled(MonitorSample),
    /// No sample for [`STALE_AFTER`].
    Stale,
    /// The connection failed or ended; the watcher retries.
    Failed(String),
}

pub(in crate::gui) struct MonitorSample {
    processes: Vec<Process>,
    logical_cpus: u32,
    apps_known: bool,
}

/// What the window knows of the host's processes.
#[derive(Clone, Debug, Default, PartialEq)]
enum FeedState {
    #[default]
    Connecting,
    Live,
    Stale,
    Failed(String),
}

#[derive(Default)]
struct Feed {
    state: FeedState,
    processes: Vec<Process>,
    logical_cpus: u32,
    apps_known: bool,
}

/// The running watcher; dropping it stops the thread, which closes the connection.
struct Watcher {
    generation: u64,
    _stop: mpsc::Sender<()>,
}

/// One Monitor pane's client state.
struct MonitorPane {
    /// Keyboard focus of the table; the search field sits inside it.
    focus: FocusHandle,
    search: Entity<InputState>,
    query: Search,
    filter: Filter,
    sort: Sort,
    /// Indices of the feed's processes, in the order shown, and their keys.
    rows: Vec<usize>,
    keys: Vec<ProcessKey>,
    selected: Option<ProcessKey>,
    /// The pointer is over the table, so its rows keep their order.
    pointer: bool,
    scroll: UniformListScrollHandle,
    /// Rows the table showed at its last draw: Page Up and Page Down move by it.
    page: usize,
    _events: Subscription,
}

/// The Monitor panes of this window and the samples they share.
#[derive(Default)]
pub(in crate::gui) struct MonitorUi {
    panes: HashMap<PaneId, MonitorPane>,
    watcher: Option<Watcher>,
    generation: u64,
    /// When the last shown pane stopped showing; the watcher stops [`LINGER`] later.
    hidden_since: Option<Instant>,
    feed: Feed,
}

/// Filter that the feed can answer: Apps means nothing where the host can't tell apps.
fn effective_filter(filter: Filter, apps_known: bool) -> Filter {
    if filter == Filter::Apps && !apps_known {
        Filter::All
    } else {
        filter
    }
}

/// Orders `pane`'s rows from `feed`: held in their last order while the pointer is
/// over the table and `hold`, else sorted. A selection no longer shown is cleared.
fn arrange_pane(pane: &mut MonitorPane, feed: &Feed, hold: bool) {
    let filter = effective_filter(pane.filter, feed.apps_known);
    pane.rows = if hold && pane.pointer {
        listing::arrange_held(&feed.processes, &pane.keys, filter, &pane.query, pane.sort)
    } else {
        listing::arrange(&feed.processes, filter, &pane.query, pane.sort)
    };
    pane.keys = pane
        .rows
        .iter()
        .map(|&row| listing::key(&feed.processes[row]))
        .collect();
    if pane
        .selected
        .is_some_and(|selected| !pane.keys.contains(&selected))
    {
        pane.selected = None;
    }
}

#[derive(PartialEq, Eq)]
enum WatchEnd {
    /// The window closed or stopped the watcher.
    Stopped,
    Failed(String),
}

/// The watcher has been dropped (or the window is gone).
fn stopped(stop: &mpsc::Receiver<()>) -> bool {
    matches!(stop.try_recv(), Err(mpsc::TryRecvError::Disconnected))
}

/// Watches the host's processes on a connection of its own until stopped, reconnecting
/// after failures; while an update holds `quiesced` it stays disconnected.
fn watch(
    generation: u64,
    target: ConnectionTarget,
    sender: UiEventSender,
    stop: mpsc::Receiver<()>,
    quiesced: Arc<AtomicBool>,
    connection_guard: Arc<Mutex<()>>,
) {
    loop {
        if stopped(&stop) {
            return;
        }
        let client = {
            let Ok(_guard) = connection_guard.lock() else {
                return;
            };
            if quiesced.load(Ordering::Acquire) {
                Err("Paused while Compi updates".to_owned())
            } else {
                target
                    .connect()
                    .map_err(|error| format!("Couldn't connect to the Compi daemon: {error}"))
            }
        };
        let end = match client {
            Ok(mut client) => follow(&mut client, generation, &sender, &stop, &quiesced),
            Err(error) => WatchEnd::Failed(error),
        };
        let WatchEnd::Failed(error) = end else {
            return;
        };
        if !sender.send(UiEvent::Monitor {
            generation,
            event: MonitorEvent::Failed(error),
        }) {
            return;
        }
        let retry = Instant::now() + RECONNECT_DELAY;
        while Instant::now() < retry {
            if stopped(&stop) {
                return;
            }
            thread::sleep(WATCH_POLL);
        }
    }
}

fn follow(
    client: &mut DaemonClient,
    generation: u64,
    sender: &UiEventSender,
    stop: &mpsc::Receiver<()>,
    quiesced: &AtomicBool,
) -> WatchEnd {
    let send = |event| sender.send(UiEvent::Monitor { generation, event });
    if let Err(error) = client.watch_system_metrics() {
        return WatchEnd::Failed(format!("The daemon can't list processes: {error}"));
    }
    if !send(MonitorEvent::Connecting) {
        return WatchEnd::Stopped;
    }
    // Details come once per connection, so a new connection starts a new table.
    let mut table = ProcessTable::default();
    let mut last_sample = Instant::now();
    let mut stale = false;
    loop {
        if stopped(stop) {
            return WatchEnd::Stopped;
        }
        if quiesced.load(Ordering::Acquire) {
            return WatchEnd::Failed("Paused while Compi updates".into());
        }
        match client.poll_event() {
            Ok(Some(ServerEvent::Control {
                message: ServerMessage::SystemSampled { sample },
                ..
            })) => {
                let SystemSample {
                    logical_cpus,
                    apps_known,
                    processes,
                    ..
                } = sample;
                let processes = table.apply(processes);
                last_sample = Instant::now();
                stale = false;
                if !send(MonitorEvent::Sampled(MonitorSample {
                    processes,
                    logical_cpus,
                    apps_known,
                })) {
                    return WatchEnd::Stopped;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                if !stale && last_sample.elapsed() >= STALE_AFTER {
                    stale = true;
                    if !send(MonitorEvent::Stale) {
                        return WatchEnd::Stopped;
                    }
                }
                thread::sleep(WATCH_POLL);
            }
            Err(error) => {
                return WatchEnd::Failed(format!("Lost the connection to the daemon: {error}"));
            }
        }
    }
}

/// The Monitor leaves of `node`.
fn monitor_leaves<'a>(node: &'a LayoutNode, output: &mut Vec<&'a PaneId>) {
    match node {
        LayoutNode::Content {
            pane_id,
            content: PaneContent::Monitor(_),
        } => output.push(pane_id),
        LayoutNode::Content { .. } | LayoutNode::Pane { .. } => {}
        LayoutNode::Split { first, second, .. } => {
            monitor_leaves(first, output);
            monitor_leaves(second, output);
        }
    }
}

/// A name's tooltip: the name, then where it runs from and how it was started.
fn name_tooltip(details: &ProcessDetails) -> (String, Option<SharedString>) {
    let mut lines = Vec::new();
    if let Some(executable) = &details.executable {
        lines.push(listing::display_line(executable));
    }
    if let Some(command) = &details.command {
        let mut command = listing::display_line(command);
        if let Some((cut, _)) = command.char_indices().nth(TOOLTIP_COMMAND_CHARS) {
            command.truncate(cut);
            command.push('…');
        }
        if lines.first() != Some(&command) {
            lines.push(command);
        }
    }
    if details.restricted {
        lines.push("Owned by another user; this host withholds its details".into());
    }
    let detail = (!lines.is_empty()).then(|| lines.join("\n").into());
    (listing::display_line(&details.name), detail)
}

impl CompiApp {
    /// Opens Monitor: focuses this window's Monitor pane, or opens one
    /// beside the focused pane (in a tab of its own when the window has none).
    pub(super) fn open_monitor(&mut self) {
        if self.overlay.is_some() {
            self.dismiss_overlay();
        }
        self.popup = None;
        if let Some((session, tab, pane_id)) = self.window_monitor_pane() {
            if self.selected_tab().map(|tab| &tab.id) != Some(&tab) {
                if let Some(workspace) = &self.workspace {
                    self.state.select_session(workspace, &session);
                }
                self.select_terminal(tab, false);
            }
            self.focus_pane(pane_id);
            return;
        }
        let content = PaneContent::Monitor(MonitorContent::default());
        let beside = self.focused_pane().cloned().or_else(|| {
            let mut leaves = Vec::new();
            collect_leaves(&self.selected_tab()?.layout, &mut leaves);
            leaves.into_iter().next().map(|(pane, _)| pane)
        });
        match beside {
            Some(beside) => {
                self.open_content_beside(&beside, content);
            }
            // No tab to open beside: Monitor gets a tab of its own.
            None => {
                let Some(session_id) = self
                    .state
                    .selected_session
                    .clone()
                    .or_else(|| Some(self.workspace.as_ref()?.sessions.first()?.id.clone()))
                else {
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

    /// The Monitor pane of this window: in the selected tab if it has one, else in
    /// another tab the window shows.
    fn window_monitor_pane(&self) -> Option<(SessionId, TabId, PaneId)> {
        let workspace = self.workspace.as_ref()?;
        let selected = self.selected_tab().map(|tab| &tab.id);
        let mut found = None;
        for session in &workspace.sessions {
            for tab in self.state.visible_tabs(session) {
                let mut panes = Vec::new();
                monitor_leaves(&tab.layout, &mut panes);
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

    /// Before drawing: state for each Monitor pane shown, and the watcher running
    /// while any is (stopping [`LINGER`] after the last one went out of sight).
    pub(super) fn prepare_monitor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let is_monitor = |pane: &PaneId| {
            content::find_pane_content(workspace, pane).is_some_and(PaneContent::is_monitor)
        };
        self.monitor.panes.retain(|pane, _| is_monitor(pane));
        let shown: Vec<PaneId> = self
            .shown_content_panes()
            .filter(|pane| is_monitor(pane))
            .cloned()
            .collect();
        for pane_id in &shown {
            if !self.monitor.panes.contains_key(pane_id) {
                let pane = self.new_monitor_pane(pane_id, window, cx);
                self.monitor.panes.insert(pane_id.clone(), pane);
            }
        }
        if !shown.is_empty() {
            self.monitor.hidden_since = None;
            if self.monitor.watcher.is_none() {
                self.start_monitor_watcher();
            }
        } else if self.monitor.watcher.is_some() && self.monitor.hidden_since.is_none() {
            self.monitor.hidden_since = Some(Instant::now());
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(LINGER).await;
                this.update(cx, |this, _| this.expire_monitor_watcher())
                    .ok();
            })
            .detach();
        }
    }

    fn new_monitor_pane(
        &mut self,
        pane_id: &PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> MonitorPane {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search processes"));
        let changed = pane_id.clone();
        let events = cx.subscribe(&search, move |this, search, event: &InputEvent, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let query = Search::new(&search.read(cx).value());
            let MonitorUi { panes, feed, .. } = &mut this.monitor;
            let Some(pane) = panes.get_mut(&changed) else {
                return;
            };
            if pane.query != query {
                pane.query = query;
                arrange_pane(pane, feed, false);
                pane.scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            }
        });
        let mut pane = MonitorPane {
            focus: cx.focus_handle(),
            search,
            query: Search::default(),
            filter: Filter::All,
            sort: Sort::default(),
            rows: Vec::new(),
            keys: Vec::new(),
            selected: None,
            pointer: false,
            scroll: UniformListScrollHandle::new(),
            page: 1,
            _events: events,
        };
        arrange_pane(&mut pane, &self.monitor.feed, false);
        pane
    }

    fn start_monitor_watcher(&mut self) {
        self.monitor.generation += 1;
        let generation = self.monitor.generation;
        let (stop, stopped) = mpsc::channel();
        let target = self.target.clone();
        let sender = self.event_tx.clone();
        let quiesced = self.update_quiesced.clone();
        let connection_guard = self.update_connection_guard.clone();
        thread::spawn(move || {
            watch(
                generation,
                target,
                sender,
                stopped,
                quiesced,
                connection_guard,
            )
        });
        self.monitor.watcher = Some(Watcher {
            generation,
            _stop: stop,
        });
        self.reset_monitor_feed(FeedState::Connecting);
    }

    /// Stops the watcher once no pane has shown for [`LINGER`].
    fn expire_monitor_watcher(&mut self) {
        if self
            .monitor
            .hidden_since
            .is_some_and(|since| since.elapsed() >= LINGER)
        {
            self.monitor.hidden_since = None;
            self.monitor.watcher = None;
            self.reset_monitor_feed(FeedState::Connecting);
        }
    }

    /// Forgets every process: what is not current is not shown.
    fn reset_monitor_feed(&mut self, state: FeedState) {
        let MonitorUi { panes, feed, .. } = &mut self.monitor;
        *feed = Feed {
            state,
            ..Feed::default()
        };
        for pane in panes.values_mut() {
            arrange_pane(pane, feed, false);
        }
    }

    pub(super) fn monitor_event(&mut self, generation: u64, event: MonitorEvent) {
        if self
            .monitor
            .watcher
            .as_ref()
            .is_none_or(|watcher| watcher.generation != generation)
        {
            // A watcher stopped since.
            return;
        }
        match event {
            MonitorEvent::Connecting => self.reset_monitor_feed(FeedState::Connecting),
            MonitorEvent::Failed(error) => self.reset_monitor_feed(FeedState::Failed(error)),
            MonitorEvent::Stale => {
                let feed = &mut self.monitor.feed;
                if feed.state == FeedState::Live {
                    feed.state = FeedState::Stale;
                }
            }
            MonitorEvent::Sampled(sample) => {
                let MonitorUi { panes, feed, .. } = &mut self.monitor;
                *feed = Feed {
                    state: FeedState::Live,
                    processes: sample.processes,
                    logical_cpus: sample.logical_cpus,
                    apps_known: sample.apps_known,
                };
                for pane in panes.values_mut() {
                    arrange_pane(pane, feed, true);
                }
            }
        }
    }

    /// Whether `pane` is a Monitor pane with client state, and its focus handle.
    pub(super) fn monitor_pane_focus(&self, pane: &PaneId) -> Option<&FocusHandle> {
        self.monitor.panes.get(pane).map(|pane| &pane.focus)
    }

    /// Whether `focused` is inside one of this window's Monitor panes.
    pub(super) fn monitor_owns(&self, focused: &FocusHandle, window: &Window, cx: &App) -> bool {
        self.monitor.panes.values().any(|pane| {
            *focused == pane.focus
                || pane.focus.contains_focused(window, cx)
                || pane.search.focus_handle(cx) == *focused
        })
    }

    /// The focused Monitor pane's search field has the keyboard.
    pub(super) fn monitor_search_has_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.focused_pane()
            .and_then(|pane| self.monitor.panes.get(pane))
            .is_some_and(|pane| pane.search.focus_handle(cx).is_focused(window))
    }

    /// Keys in the focused Monitor pane's search field, before the field's own:
    /// Escape clears it (an empty one hands the keyboard to the table), Down, Enter and
    /// Tab go to the table.
    pub(super) fn monitor_search_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.monitor_search_has_keyboard(window, cx) {
            return false;
        }
        let modifiers = &key.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform {
            return false;
        }
        let Some(pane_id) = self.focused_pane().cloned() else {
            return false;
        };
        let Some(pane) = self.monitor.panes.get(&pane_id) else {
            return false;
        };
        match key.key.as_str() {
            "escape" if !pane.search.read(cx).value().is_empty() => {
                pane.search
                    .update(cx, |search, cx| search.set_value("", window, cx));
                true
            }
            "escape" => {
                window.focus(&pane.focus, cx);
                true
            }
            "down" | "enter" | "tab" if !modifiers.shift => {
                window.focus(&pane.focus, cx);
                if pane.selected.is_none() {
                    self.step_monitor_selection(&pane_id, Step::Home);
                }
                true
            }
            _ => false,
        }
    }

    fn focus_monitor_search(&self, pane_id: &PaneId, window: &mut Window, cx: &mut App) {
        if let Some(pane) = self.monitor.panes.get(pane_id) {
            let handle = pane.search.focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    fn step_monitor_selection(&mut self, pane_id: &PaneId, step: Step) {
        let Some(pane) = self.monitor.panes.get_mut(pane_id) else {
            return;
        };
        let current = pane
            .selected
            .and_then(|selected| pane.keys.iter().position(|key| *key == selected));
        if let Some(next) = listing::step_selection(current, pane.keys.len(), step) {
            pane.selected = Some(pane.keys[next]);
            pane.scroll.scroll_to_item(next, ScrollStrategy::Nearest);
        }
    }

    fn set_monitor_filter(&mut self, pane_id: &PaneId, filter: Filter) {
        let MonitorUi { panes, feed, .. } = &mut self.monitor;
        if let Some(pane) = panes.get_mut(pane_id)
            && pane.filter != filter
        {
            pane.filter = filter;
            arrange_pane(pane, feed, false);
            pane.scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
    }

    fn sort_monitor_table(&mut self, pane_id: &PaneId, column: Column) {
        let MonitorUi { panes, feed, .. } = &mut self.monitor;
        if let Some(pane) = panes.get_mut(pane_id) {
            pane.sort = pane.sort.clicked(column);
            arrange_pane(pane, feed, false);
            if let Some(at) = pane
                .selected
                .and_then(|selected| pane.keys.iter().position(|key| *key == selected))
            {
                pane.scroll.scroll_to_item(at, ScrollStrategy::Nearest);
            }
        }
    }

    /// The pointer entered or left the table; leaving sorts it again.
    fn hover_monitor_table(&mut self, pane_id: &PaneId, hovered: bool) -> bool {
        let MonitorUi { panes, feed, .. } = &mut self.monitor;
        let Some(pane) = panes.get_mut(pane_id) else {
            return false;
        };
        if pane.pointer == hovered {
            return false;
        }
        pane.pointer = hovered;
        if !hovered {
            arrange_pane(pane, feed, false);
        }
        !hovered
    }

    /// Keys in a Monitor pane that its search field left alone.
    fn monitor_key_down(
        &mut self,
        pane_id: &PaneId,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.monitor.panes.get(pane_id) else {
            return;
        };
        if pane.search.focus_handle(cx).is_focused(window) {
            // Text belongs to the field; Escape, Down, Enter and Tab were taken before it.
            return;
        }
        let key = &event.keystroke;
        let modifiers = &key.modifiers;
        let plain = !modifiers.control && !modifiers.alt && !modifiers.platform;
        let step = match key.key.as_str() {
            "up" => Some(Step::Up),
            "down" => Some(Step::Down),
            "pageup" => Some(Step::PageUp(pane.page)),
            "pagedown" => Some(Step::PageDown(pane.page)),
            "home" => Some(Step::Home),
            "end" => Some(Step::End),
            _ => None,
        }
        .filter(|_| plain && !modifiers.shift);
        if let Some(step) = step {
            self.step_monitor_selection(pane_id, step);
        } else if key.key == "f"
            && (modifiers.control || modifiers.platform)
            && !modifiers.alt
            && !modifiers.shift
        {
            self.focus_monitor_search(pane_id, window, cx);
        } else if key.key == "escape" && plain && !pane.search.read(cx).value().is_empty() {
            pane.search
                .update(cx, |search, cx| search.set_value("", window, cx));
        } else if key.key == "escape" && plain && pane.selected.is_some() {
            if let Some(pane) = self.monitor.panes.get_mut(pane_id) {
                pane.selected = None;
            }
        } else if let Some(text) = key
            .key_char
            .as_deref()
            .filter(|text| plain && !text.trim().is_empty() && !text.chars().any(char::is_control))
        {
            // Typing searches.
            let query = format!("{}{text}", pane.search.read(cx).value());
            pane.search
                .update(cx, |search, cx| search.set_value(query, window, cx));
            self.focus_monitor_search(pane_id, window, cx);
        } else {
            self.handle_app_key(event, window, cx);
        }
        // Nothing typed in Monitor reaches a shell.
        cx.stop_propagation();
        cx.notify();
    }

    /// One Monitor pane `width` pixels wide: its header line, then the table or
    /// what stands in for it.
    pub(super) fn render_monitor_pane(
        &self,
        pane_id: &PaneId,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(pane) = self.monitor.panes.get(pane_id) else {
            return div().into_any_element();
        };
        let colors = *self.colors();
        let feed = &self.monitor.feed;
        let muted = color(modal_text_color(colors.muted, &colors));
        let body = match &feed.state {
            FeedState::Connecting => self.monitor_message("Reading processes…", None),
            FeedState::Failed(error) => self.monitor_message(
                "Can't read processes",
                Some((format!("{error} (retrying)"), colors.error)),
            ),
            FeedState::Live | FeedState::Stale if feed.processes.is_empty() => {
                self.monitor_message("No processes reported", None)
            }
            FeedState::Live | FeedState::Stale if pane.rows.is_empty() => {
                let text = pane.search.read(cx).value();
                let text = text.trim();
                self.monitor_message(
                    "No processes match",
                    (!text.is_empty()).then(|| (format!("Nothing matches “{text}”"), colors.muted)),
                )
            }
            FeedState::Live | FeedState::Stale => {
                self.render_monitor_table(pane_id, pane, width, cx)
            }
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
                this.monitor_key_down(&key_pane, event, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    // The search field takes focus itself; elsewhere the table does.
                    if !window.default_prevented()
                        && let Some(pane) = this.monitor.panes.get(&click_pane)
                    {
                        window.focus(&pane.focus, cx);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(self.render_monitor_header(pane_id, pane, width, cx))
            .when(feed.state == FeedState::Stale, |pane| {
                pane.child(
                    div()
                        .flex_none()
                        .px_4()
                        .py_1()
                        .border_b_1()
                        .border_color(color(colors.border))
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .text_color(muted)
                        .child("Not updating: no new sample from the daemon for a while; the values shown are out of date."),
                )
            })
            .child(body)
            .into_any_element()
    }

    /// The header line: title, search, filters and the process count.
    fn render_monitor_header(
        &self,
        pane_id: &PaneId,
        pane: &MonitorPane,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let feed = &self.monitor.feed;
        let current = effective_filter(pane.filter, feed.apps_known);
        let filters = controls::segmented(
            "monitor-filter",
            Filter::ALL
                .into_iter()
                .filter(|filter| *filter != Filter::Apps || feed.apps_known)
                .enumerate()
                .map(|(index, filter)| {
                    let pane_id = pane_id.clone();
                    controls::text_button(("monitor-filter", index), filter.label())
                        .selected(current == filter)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_monitor_filter(&pane_id, filter);
                            cx.notify();
                        }))
                }),
        );
        let counted = matches!(feed.state, FeedState::Live | FeedState::Stale)
            && width >= HEADER_COUNT_MIN_WIDTH;
        div()
            .flex_none()
            .min_h(px(44.0))
            .px_4()
            .py(px(10.0))
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_3()
            .gap_y_2()
            .border_b_1()
            .border_color(color(colors.border))
            .when(width >= HEADER_TITLE_MIN_WIDTH, |header| {
                header.child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Monitor"),
                )
            })
            .child(
                div().flex_1().min_w(px(120.0)).max_w(px(280.0)).child(
                    Input::new(&pane.search)
                        .appearance(true)
                        .cleanable(true)
                        .with_size(gpui_component::Size::Small),
                ),
            )
            .child(filters)
            .when(counted, |header| {
                header.child(
                    div()
                        .flex_none()
                        .ml_auto()
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .text_color(color(modal_text_color(colors.muted, &colors)))
                        .child(listing::format_count(pane.rows.len(), feed.processes.len())),
                )
            })
            .into_any_element()
    }

    /// A centred message in place of the table.
    fn monitor_message(&self, title: &'static str, detail: Option<(String, u32)>) -> AnyElement {
        let colors = *self.colors();
        div()
            .flex_1()
            .min_h_0()
            .p_4()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .text_color(color(modal_text_color(colors.muted, &colors)))
            .child(div().text_size(px(UI_BODY_TEXT_SIZE)).child(title))
            .when_some(detail, |message, (detail, tint)| {
                message.child(
                    div()
                        .max_w(px(480.0))
                        .text_center()
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .text_color(color(modal_text_color(tint, &colors)))
                        .child(detail),
                )
            })
            .into_any_element()
    }

    /// The column headings (click to sort) over the rows, drawn as they scroll in.
    fn render_monitor_table(
        &self,
        pane_id: &PaneId,
        pane: &MonitorPane,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let columns = listing::visible_columns(width - 2.0 * TABLE_INSET);
        let muted = color(modal_text_color(colors.muted, &colors));
        let headings = columns.iter().enumerate().map(|(index, &column)| {
            let sorted = pane.sort.column == column;
            let arrow = sorted.then(|| {
                controls::glyph(
                    match pane.sort.direction {
                        Direction::Ascending => IconName::ArrowUp,
                        Direction::Descending => IconName::ArrowDown,
                    },
                    muted,
                )
            });
            // The arrow sits on the inner side: before right-aligned labels, after others.
            let (before, after) = if column.numeric() {
                (arrow, None)
            } else {
                (None, arrow)
            };
            let pane_id = pane_id.clone();
            monitor_cell(div().id(("monitor-column", index)), column)
                .h_full()
                .gap_1()
                .cursor_pointer()
                .rounded_sm()
                .hover(move |style| style.bg(color(colors.surface_hover)))
                .when(sorted, |cell| cell.font_weight(FontWeight::SEMIBOLD))
                .children(before)
                .child(column.label())
                .children(after)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.sort_monitor_table(&pane_id, column);
                    cx.notify();
                }))
        });
        let header = div()
            .flex_none()
            .h(px(ROW_HEIGHT + 4.0))
            .px(px(TABLE_INSET))
            .flex()
            .items_center()
            .border_b_1()
            .border_color(color(colors.border))
            .text_size(px(UI_MICRO_TEXT_SIZE))
            .text_color(muted)
            .children(headings);
        let list_pane = pane_id.clone();
        let hover_pane = pane_id.clone();
        let list = uniform_list(
            "monitor-rows",
            pane.rows.len(),
            cx.processor(move |this, range: Range<usize>, window, cx| {
                if let Some(pane) = this.monitor.panes.get_mut(&list_pane) {
                    pane.page = range.len().saturating_sub(1).max(1);
                }
                let keyboard = this
                    .monitor
                    .panes
                    .get(&list_pane)
                    .is_some_and(|pane| pane.focus.is_focused(window));
                this.render_monitor_rows(&list_pane, &columns, range, keyboard, cx)
            }),
        )
        .track_scroll(&pane.scroll)
        .flex_1()
        .min_h_0()
        .px(px(TABLE_INSET))
        .py_1();
        div()
            .id("monitor-table")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if this.hover_monitor_table(&hover_pane, *hovered) {
                    cx.notify();
                }
            }))
            .child(header)
            .child(list)
            .into_any_element()
    }

    /// The table rows in `range`. The selection is filled in the accent while the table
    /// has the keyboard (`keyboard`), quieter otherwise.
    fn render_monitor_rows(
        &self,
        pane_id: &PaneId,
        columns: &[Column],
        range: Range<usize>,
        keyboard: bool,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(pane) = self.monitor.panes.get(pane_id) else {
            return Vec::new();
        };
        let feed = &self.monitor.feed;
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let selected_fill = if keyboard {
            color(colors.accent).opacity(0.22)
        } else {
            color(colors.surface_hover)
        };
        let width = columns.len();
        range
            .filter_map(|index| {
                let process = feed.processes.get(*pane.rows.get(index)?)?;
                let key = listing::key(process);
                let selected = pane.selected == Some(key);
                let details = &process.details;
                let sample = &process.sample;
                let cells = columns.iter().enumerate().map(|(position, &column)| {
                    let id = index * width + position;
                    let value = match column {
                        Column::Name => Some(listing::display_line(&details.name)),
                        Column::Cpu => listing::cpu_share(sample.cpu_percent, feed.logical_cpus)
                            .map(listing::format_cpu),
                        Column::Memory => sample.memory_bytes.map(listing::format_memory),
                        Column::Threads => sample.threads.map(|threads| threads.to_string()),
                        Column::CpuTime => sample.cpu_time_ms.map(listing::format_cpu_time),
                        Column::Pid => Some(sample.pid.to_string()),
                        Column::User => details.user.as_deref().map(listing::display_line),
                    };
                    let cell = monitor_cell(div().id(("monitor-cell", id)), column);
                    match value {
                        Some(text) if column == Column::Name => {
                            let details = details.clone();
                            cell.child(cell_text(text)).tooltip(move |window, cx| {
                                let (title, detail) = name_tooltip(&details);
                                controls::tooltip(title, detail, None)(window, cx)
                            })
                        }
                        Some(text) => cell.child(cell_text(text)),
                        None if details.restricted => cell
                            .text_color(muted)
                            .child(UNAVAILABLE)
                            .tooltip(controls::tooltip(
                                "Not available",
                                Some(
                                    "This host doesn't let Compi read other users' processes"
                                        .into(),
                                ),
                                None,
                            )),
                        None => cell.text_color(muted).child(UNAVAILABLE),
                    }
                });
                let pane = pane_id.clone();
                Some(
                    div()
                        .id(("monitor-row", index))
                        .h(px(ROW_HEIGHT))
                        .w_full()
                        .flex()
                        .items_center()
                        .rounded_sm()
                        .when(selected, |row| row.bg(selected_fill))
                        .when(!selected, |row| {
                            row.hover(move |style| style.bg(color(colors.surface_hover)))
                        })
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            if let Some(ui) = this.monitor.panes.get_mut(&pane) {
                                ui.selected = Some(key);
                                window.focus(&ui.focus, cx);
                            }
                            cx.notify();
                        }))
                        .children(cells)
                        .into_any_element(),
                )
            })
            .collect()
    }
}

/// A table cell of `column`: Name takes the room the others leave; amounts are
/// right-aligned. One line, cut with an ellipsis.
fn monitor_cell(cell: Stateful<Div>, column: Column) -> Stateful<Div> {
    let cell = cell
        .h_full()
        .px_2()
        .flex()
        .items_center()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis();
    let cell = if column == Column::Name {
        cell.flex_1().min_w(px(listing::NAME_MIN_WIDTH))
    } else {
        cell.flex_none().w(px(column.width()))
    };
    if column.numeric() {
        cell.justify_end()
    } else {
        cell
    }
}

/// A cell's value, which shrinks with its cell and ends in an ellipsis when cut.
fn cell_text(text: String) -> Div {
    div()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(text)
}
