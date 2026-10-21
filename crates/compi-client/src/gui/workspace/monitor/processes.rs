//! Monitor's Processes page: a source's processes in a virtualized table, one row per
//! app by default ("Group by app", per window) with its processes beneath it when
//! expanded, and the inspector of the selected row beside it (or on a page of its own
//! in a narrow pane; `inspector`).
//!
//! Which rows a pane shows, in what order and how their values read is pure
//! (`crate::monitor` and `crate::monitor::apps`); the search, filter, sort, expanded
//! apps and selection are this window's, per pane, and the columns (`columns`) the
//! window's. While the pointer is over the table its rows keep their order (values
//! update in place, newcomers go last, leavers go), and the table sorts again when the
//! pointer leaves or the sort, search or filter changes.
use super::inspector::Inspection;
use super::*;
use crate::monitor::apps::{
    self, Arranged, Group, GroupUser, MemoryByApp, RowKey, RowKind, TableRow, View,
};
use crate::monitor::layout::{Inspector, inspector_layout};
use crate::monitor::{self as listing, Column, Filter, ProcessKey, Search, Sort, Step};
use compi_protocol::system_metrics::{Process, ProcessDetails};
use gpui::{ScrollStrategy, UniformListScrollHandle, uniform_list};

/// One table row.
const ROW_HEIGHT: f32 = 24.0;
/// The table's inset from the page's edges.
pub(super) const TABLE_INSET: f32 = 12.0;
/// Narrower than this, the toolbar drops its process count.
const TOOLBAR_COUNT_MIN_WIDTH: f32 = 480.0;
/// Longest command line a name's tooltip shows.
const TOOLTIP_COMMAND_CHARS: usize = 400;
/// A row's app icon, and the room a fold chevron takes before it.
pub(super) const ROW_ICON: f32 = 16.0;
const FOLD_WIDTH: f32 = 16.0;
/// Apps Memory by app shows by name; the rest are Other.
const MEMORY_APPS: usize = 7;

/// A source's processes, shared by the window's Monitor panes showing it.
#[derive(Default)]
pub(super) struct ProcessFeed {
    pub(super) slot: WatchSlot,
    pub(super) state: FeedState,
    pub(super) processes: Vec<Process>,
    pub(super) logical_cpus: u32,
    apps_known: bool,
    /// The newest sample while Monitor is paused, shown when it resumes.
    pending: Option<ProcessesSample>,
    /// Memory by app over the processes shown, for Overview.
    pub(super) memory: MemoryByApp,
}

impl ProcessFeed {
    /// How many processes the source runs, once known.
    pub(super) fn count(&self) -> Option<usize> {
        matches!(self.state, FeedState::Live | FeedState::Stale).then_some(self.processes.len())
    }

    /// The process `key` names, while it runs.
    pub(super) fn find(&self, key: &ProcessKey) -> Option<(usize, &Process)> {
        self.processes
            .iter()
            .enumerate()
            .find(|(_, process)| listing::key(process) == *key)
    }
}

/// One pane's view of the processes.
pub(super) struct ProcessList {
    pub(super) search: Entity<InputState>,
    query: Search,
    filter: Filter,
    pub(super) sort: Sort,
    /// The rows shown, their keys and the apps they refer to.
    pub(super) table: Arranged,
    pub(super) selected: Option<RowKey>,
    /// Apps whose processes show beneath them, by group id.
    expanded: HashSet<String>,
    /// The pointer is over the table, so its rows keep their order.
    pointer: bool,
    scroll: UniformListScrollHandle,
    /// Rows the table showed at its last draw: Page Up and Page Down move by it.
    page_rows: usize,
    /// The page was wide enough at its last draw for the inspector beside the table.
    pub(super) wide: Cell<bool>,
    /// The selection's page of its own is open (narrow panes; Back closes it).
    pub(super) detail: bool,
    /// What the daemon said of the selected process, while an inspector shows it.
    pub(super) inspection: Option<Inspection>,
    /// The table's width at its last draw, for header drags.
    pub(super) table_width: Cell<f32>,
    _events: Subscription,
}

impl ProcessList {
    /// The process selected, when the selection is one rather than an app.
    pub(super) fn selected_process(&self) -> Option<ProcessKey> {
        match self.selected {
            Some(RowKey::Process(key)) => Some(key),
            _ => None,
        }
    }

    /// Whether an inspector shows the selection: beside a wide table, or on its page.
    pub(super) fn inspecting(&self) -> bool {
        self.selected.is_some() && (self.wide.get() || self.detail)
    }

    /// The pane now shows another source's `feed`: what was selected and expanded
    /// there means nothing here.
    pub(super) fn show_source(&mut self, feed: &ProcessFeed, flat: bool) {
        self.selected = None;
        self.detail = false;
        self.inspection = None;
        self.expanded.clear();
        arrange(self, feed, flat, false);
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }
}

/// Filter that the feed can answer: Apps means nothing where the host can't tell apps.
fn effective_filter(filter: Filter, apps_known: bool) -> Filter {
    if filter == Filter::Apps && !apps_known {
        Filter::All
    } else {
        filter
    }
}

/// Orders `list`'s rows from `feed`, by app unless `flat`: held in their last order
/// while the pointer is over the table and `hold`, else sorted. A selection no longer
/// shown moves to the row that now shows it (an app that became a single process),
/// else is cleared.
fn arrange(list: &mut ProcessList, feed: &ProcessFeed, flat: bool, hold: bool) {
    let view = View {
        filter: effective_filter(list.filter, feed.apps_known),
        search: &list.query,
        sort: list.sort,
        grouped: !flat,
        expanded: &list.expanded,
    };
    let held = (hold && list.pointer).then_some(list.table.keys.as_slice());
    let table = apps::arrange_rows(&feed.processes, view, held);
    list.table = table;
    if let Some(selected) = list.selected.take() {
        list.selected = apps::find_row(&list.table, &feed.processes, &selected)
            .map(|row| list.table.keys[row].clone());
    }
    if list.selected.is_none() {
        list.detail = false;
    }
}

/// Pane `pane_id`'s list, the processes of its source and whether the window lists
/// them one row per process.
fn pane_list<'a>(
    ui: &'a mut MonitorUi,
    pane_id: &PaneId,
) -> Option<(&'a mut ProcessList, &'a ProcessFeed, bool)> {
    let MonitorUi {
        panes,
        sources,
        unread,
        flat,
        ..
    } = ui;
    let pane = panes.get_mut(pane_id)?;
    let feed = &sources.get(&pane.source).unwrap_or(unread).processes;
    Some((&mut pane.list, feed, *flat))
}

/// Every list showing `source`, rearranged from its feed.
fn arrange_source(ui: &mut MonitorUi, source: &MonitorSource, hold: bool) {
    let MonitorUi {
        panes,
        sources,
        unread,
        flat,
        ..
    } = ui;
    let feed = &sources.get(source).unwrap_or(unread).processes;
    for pane in panes.values_mut().filter(|pane| pane.source == *source) {
        arrange(&mut pane.list, feed, *flat, hold);
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

/// What a value cell shows.
enum CellText {
    Value(String),
    /// A fact about the row rather than a value: a count of processes.
    Quiet(String),
    /// Not available; `withheld` when the host refused it.
    Unavailable {
        withheld: bool,
    },
}

fn value_or_unavailable(value: Option<String>, withheld: bool) -> CellText {
    value.map_or(CellText::Unavailable { withheld }, CellText::Value)
}

/// `column` of a process's row.
fn process_cell_text(process: &Process, column: Column, logical_cpus: u32) -> CellText {
    let sample = &process.sample;
    let withheld = process.details.restricted;
    match column {
        Column::Name => CellText::Value(listing::display_line(&process.details.name)),
        Column::Cpu => value_or_unavailable(
            listing::cpu_share(sample.cpu_percent, logical_cpus).map(listing::format_cpu),
            withheld,
        ),
        Column::Memory => {
            value_or_unavailable(sample.memory_bytes.map(listing::format_bytes), withheld)
        }
        Column::Threads => {
            value_or_unavailable(sample.threads.map(|threads| threads.to_string()), withheld)
        }
        Column::CpuTime => {
            value_or_unavailable(sample.cpu_time_ms.map(listing::format_cpu_time), withheld)
        }
        Column::Pid => CellText::Value(sample.pid.to_string()),
        Column::User => value_or_unavailable(
            process.details.user.as_deref().map(listing::display_line),
            withheld,
        ),
    }
}

/// `column` of an app's row: its sums, its process count and its user.
fn group_cell_text(group: &Group, column: Column, logical_cpus: u32) -> CellText {
    let unavailable = CellText::Unavailable { withheld: false };
    match column {
        Column::Name => CellText::Value(listing::display_line(&group.name)),
        Column::Cpu => listing::cpu_share(group.cpu_percent, logical_cpus)
            .map_or(unavailable, |share| {
                CellText::Value(listing::format_cpu(share))
            }),
        Column::Memory => group.memory_bytes.map_or(unavailable, |bytes| {
            CellText::Value(listing::format_bytes(bytes))
        }),
        Column::Threads => group
            .threads
            .map_or(unavailable, |threads| CellText::Value(threads.to_string())),
        Column::CpuTime => group.cpu_time_ms.map_or(unavailable, |time| {
            CellText::Value(listing::format_cpu_time(time))
        }),
        Column::Pid => CellText::Quiet(format!("{} processes", group.members.len())),
        Column::User => match &group.user {
            GroupUser::Shared(user) => CellText::Value(listing::display_line(user)),
            GroupUser::Mixed => unavailable,
        },
    }
}

impl ProcessList {
    /// The list's pane moved to `pane_id` with its view: its search follows.
    pub(super) fn moved_to(&mut self, pane_id: &PaneId, cx: &mut Context<CompiApp>) {
        self._events = process_search_events(pane_id, &self.search, cx);
    }
}

/// Typing in Monitor pane `pane_id`'s process search narrows its table.
fn process_search_events(
    pane_id: &PaneId,
    search: &Entity<InputState>,
    cx: &mut Context<CompiApp>,
) -> Subscription {
    let changed = pane_id.clone();
    cx.subscribe(search, move |this, search, event: &InputEvent, cx| {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let query = Search::new(&search.read(cx).value());
        let Some((list, feed, flat)) = pane_list(&mut this.monitor, &changed) else {
            return;
        };
        if list.query != query {
            list.query = query;
            arrange(list, feed, flat, false);
            list.scroll.scroll_to_item(0, ScrollStrategy::Top);
            cx.notify();
        }
    })
}

impl CompiApp {
    pub(super) fn new_process_list(
        &self,
        pane_id: &PaneId,
        source: &MonitorSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ProcessList {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search processes"));
        let events = process_search_events(pane_id, &search, cx);
        let mut list = ProcessList {
            search,
            query: Search::default(),
            filter: Filter::All,
            sort: Sort::default(),
            table: Arranged::default(),
            selected: None,
            expanded: HashSet::new(),
            pointer: false,
            scroll: UniformListScrollHandle::new(),
            page_rows: 1,
            wide: Cell::new(false),
            detail: false,
            inspection: None,
            table_width: Cell::new(0.0),
            _events: events,
        };
        arrange(
            &mut list,
            &self.monitor.feeds(source).processes,
            self.monitor.flat,
            false,
        );
        list
    }

    /// Forgets every process of `source`: what is not current is not shown.
    pub(super) fn reset_process_feed(&mut self, source: &MonitorSource, state: FeedState) {
        let Some(feeds) = self.monitor.sources.get_mut(source) else {
            return;
        };
        let processes = &mut feeds.processes;
        processes.state = state;
        processes.processes.clear();
        processes.memory = MemoryByApp::default();
        processes.pending = None;
        feeds.history.clear();
        arrange_source(&mut self.monitor, source, false);
    }

    pub(super) fn processes_event(
        &mut self,
        source: &MonitorSource,
        generation: u64,
        event: ProcessesEvent,
    ) {
        let paused = self.monitor.paused;
        let Some(feeds) = self
            .monitor
            .sources
            .get_mut(source)
            .filter(|feeds| feeds.processes.slot.is_current(generation))
        else {
            // A watcher stopped since.
            return;
        };
        match event {
            ProcessesEvent::Connecting => self.reset_process_feed(source, FeedState::Connecting),
            ProcessesEvent::Failed(error) => {
                self.reset_process_feed(source, FeedState::Failed(error));
            }
            ProcessesEvent::Stale => {
                if feeds.processes.state == FeedState::Live {
                    feeds.processes.state = FeedState::Stale;
                }
            }
            ProcessesEvent::Sampled(sample) => {
                // History records every sample, paused or not.
                feeds.history.record(&sample.processes);
                // Paused, the table and Memory by app keep what they show until Monitor
                // resumes; the first sample always shows, so a pause never leaves them
                // empty.
                if paused && !feeds.processes.processes.is_empty() {
                    feeds.processes.pending = Some(sample);
                } else {
                    self.show_process_sample(source, sample);
                }
            }
        }
    }

    fn show_process_sample(&mut self, source: &MonitorSource, sample: ProcessesSample) {
        let Some(processes) = self
            .monitor
            .sources
            .get_mut(source)
            .map(|feeds| &mut feeds.processes)
        else {
            return;
        };
        processes.state = FeedState::Live;
        processes.processes = sample.processes;
        processes.logical_cpus = sample.logical_cpus;
        processes.apps_known = sample.apps_known;
        processes.memory = apps::memory_by_app(&processes.processes, MEMORY_APPS);
        arrange_source(&mut self.monitor, source, true);
    }

    /// Monitor resumed: the newest sample of each source received while paused shows.
    pub(super) fn resume_processes(&mut self) {
        let pending: Vec<(MonitorSource, ProcessesSample)> = self
            .monitor
            .sources
            .iter_mut()
            .filter_map(|(source, feeds)| Some((source.clone(), feeds.processes.pending.take()?)))
            .collect();
        for (source, sample) in pending {
            self.show_process_sample(&source, sample);
        }
    }

    /// Opens the Processes page on `key` (an app from Memory by app, or a process the
    /// inspector links to): its app expanded, its row selected and scrolled into view.
    /// Whether a row shows it; the search or filter may hide it.
    pub(super) fn reveal_process_row(&mut self, pane_id: &PaneId, key: RowKey) -> bool {
        self.set_monitor_page(pane_id, MonitorPage::Processes);
        let Some((list, feed, flat)) = pane_list(&mut self.monitor, pane_id) else {
            return false;
        };
        let app = match &key {
            RowKey::Group(id) => Some(id.clone()),
            RowKey::Process(key) => feed
                .find(key)
                .and_then(|(_, process)| process.details.app_group.as_ref())
                .map(|app| app.id.clone()),
        };
        if let Some(app) = app.filter(|_| !flat) {
            list.expanded.insert(app);
        }
        arrange(list, feed, flat, false);
        let Some(row) = apps::find_row(&list.table, &feed.processes, &key) else {
            return false;
        };
        list.selected = Some(list.table.keys[row].clone());
        list.scroll.scroll_to_item(row, ScrollStrategy::Center);
        true
    }

    /// Selects process `key` in pane `pane_id`, clearing the search and filter when
    /// they hide it.
    pub(super) fn select_process(
        &mut self,
        pane_id: &PaneId,
        key: ProcessKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reveal_process_row(pane_id, RowKey::Process(key)) {
            return;
        }
        let Some(pane) = self.monitor.panes.get_mut(pane_id) else {
            return;
        };
        pane.list.filter = Filter::All;
        pane.list.query = Search::default();
        let search = pane.list.search.clone();
        search.update(cx, |search, cx| search.set_value("", window, cx));
        self.reveal_process_row(pane_id, RowKey::Process(key));
    }

    /// The focused Monitor pane's search field has the keyboard.
    pub(in crate::gui::workspace) fn monitor_search_has_keyboard(
        &self,
        window: &Window,
        cx: &App,
    ) -> bool {
        self.focused_pane()
            .and_then(|pane| self.monitor.panes.get(pane))
            .is_some_and(|pane| pane.list.search.focus_handle(cx).is_focused(window))
    }

    /// Keys in the focused Monitor pane's search field, before the field's own:
    /// Escape clears it (an empty one hands the keyboard to the table), Down, Enter and
    /// Tab go to the table.
    pub(in crate::gui::workspace) fn monitor_search_key(
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
            "escape" if !pane.list.search.read(cx).value().is_empty() => {
                pane.list
                    .search
                    .update(cx, |search, cx| search.set_value("", window, cx));
                true
            }
            "escape" => {
                window.focus(&pane.focus, cx);
                true
            }
            "down" | "enter" | "tab" if !modifiers.shift => {
                window.focus(&pane.focus, cx);
                if pane.list.selected.is_none() {
                    self.step_process_selection(&pane_id, Step::Home);
                }
                true
            }
            _ => false,
        }
    }

    pub(super) fn focus_process_search(&self, pane_id: &PaneId, window: &mut Window, cx: &mut App) {
        if let Some(pane) = self.monitor.panes.get(pane_id) {
            let handle = pane.list.search.focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    fn step_process_selection(&mut self, pane_id: &PaneId, step: Step) {
        let Some(list) = self
            .monitor
            .panes
            .get_mut(pane_id)
            .map(|pane| &mut pane.list)
        else {
            return;
        };
        let current = list
            .selected
            .as_ref()
            .and_then(|selected| list.table.keys.iter().position(|key| key == selected));
        if let Some(next) = listing::step_selection(current, list.table.keys.len(), step) {
            list.selected = Some(list.table.keys[next].clone());
            list.scroll.scroll_to_item(next, ScrollStrategy::Nearest);
        }
    }

    fn set_process_filter(&mut self, pane_id: &PaneId, filter: Filter) {
        if let Some((list, feed, flat)) = pane_list(&mut self.monitor, pane_id)
            && list.filter != filter
        {
            list.filter = filter;
            arrange(list, feed, flat, false);
            list.scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
    }

    /// Turns "Group by app" on or off in this window.
    fn set_group_by_app(&mut self, grouped: bool) {
        let MonitorUi {
            panes,
            sources,
            unread,
            flat,
            ..
        } = &mut self.monitor;
        if *flat != grouped {
            return;
        }
        *flat = !grouped;
        for pane in panes.values_mut() {
            let feed = &sources.get(&pane.source).unwrap_or(unread).processes;
            arrange(&mut pane.list, feed, *flat, false);
        }
    }

    /// Expands or collapses app `id` (`open`, or the other way when `None`). The rows
    /// keep their order while the pointer is over the table.
    fn fold_process_group(&mut self, pane_id: &PaneId, id: &str, open: Option<bool>) {
        let Some((list, feed, flat)) = pane_list(&mut self.monitor, pane_id) else {
            return;
        };
        let open = open.unwrap_or(!list.expanded.contains(id));
        let changed = if open {
            list.expanded.insert(id.to_owned())
        } else {
            list.expanded.remove(id)
        };
        if changed {
            arrange(list, feed, flat, true);
            if let Some(row) = list
                .selected
                .as_ref()
                .and_then(|selected| list.table.keys.iter().position(|key| key == selected))
            {
                list.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
            }
        }
    }

    /// Right, Left and Enter on the selected row: expand, collapse (from one of its
    /// processes, go up to the app) or toggle an app. Whether a key was taken.
    fn fold_key(&mut self, pane_id: &PaneId, key: &str) -> bool {
        let Some(list) = self.monitor.panes.get(pane_id).map(|pane| &pane.list) else {
            return false;
        };
        let Some(row) = list
            .selected
            .as_ref()
            .and_then(|selected| list.table.keys.iter().position(|key| key == selected))
        else {
            return false;
        };
        match (list.table.rows[row], key) {
            (
                TableRow {
                    kind: RowKind::Group(group),
                    ..
                },
                "right" | "left" | "enter",
            ) => {
                let id = list.table.groups[group].id.clone();
                let open = match key {
                    "right" => Some(true),
                    "left" => Some(false),
                    _ => None,
                };
                self.fold_process_group(pane_id, &id, open);
                true
            }
            (TableRow { member: true, .. }, "left") => {
                // The app is the nearest app row above.
                let parent = list.table.rows[..row]
                    .iter()
                    .rposition(|row| matches!(row.kind, RowKind::Group(_)));
                if let Some(parent) = parent
                    && let Some(list) = self
                        .monitor
                        .panes
                        .get_mut(pane_id)
                        .map(|pane| &mut pane.list)
                {
                    list.selected = Some(list.table.keys[parent].clone());
                    list.scroll.scroll_to_item(parent, ScrollStrategy::Nearest);
                }
                true
            }
            _ => false,
        }
    }

    pub(super) fn sort_process_table(&mut self, pane_id: &PaneId, column: Column) {
        if let Some((list, feed, flat)) = pane_list(&mut self.monitor, pane_id) {
            list.sort = list.sort.clicked(column);
            arrange(list, feed, flat, false);
            if let Some(at) = list
                .selected
                .as_ref()
                .and_then(|selected| list.table.keys.iter().position(|key| key == selected))
            {
                list.scroll.scroll_to_item(at, ScrollStrategy::Nearest);
            }
        }
    }

    /// The pointer entered or left the table; leaving sorts it again.
    fn hover_process_table(&mut self, pane_id: &PaneId, hovered: bool) -> bool {
        let Some((list, feed, flat)) = pane_list(&mut self.monitor, pane_id) else {
            return false;
        };
        if list.pointer == hovered {
            return false;
        }
        list.pointer = hovered;
        if !hovered {
            arrange(list, feed, flat, false);
        }
        !hovered
    }

    /// Opens the selection's page of its own, where the pane is too narrow for the
    /// inspector beside the table; whether it opened.
    pub(super) fn open_process_detail(&mut self, pane_id: &PaneId) -> bool {
        let Some(list) = self
            .monitor
            .panes
            .get_mut(pane_id)
            .map(|pane| &mut pane.list)
            .filter(|list| list.selected.is_some() && !list.wide.get())
        else {
            return false;
        };
        list.detail = true;
        true
    }

    /// Keys on the Processes page that its search field left alone; whether it took
    /// the key.
    pub(super) fn processes_key_down(
        &mut self,
        pane_id: &PaneId,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(list) = self.monitor.panes.get(pane_id).map(|pane| &pane.list) else {
            return false;
        };
        // The selection's page shows only in a narrow pane.
        let (page_rows, search, has_selection, detail) = (
            list.page_rows,
            list.search.clone(),
            list.selected.is_some(),
            list.detail && !list.wide.get(),
        );
        let process_selected = list.selected_process().is_some();
        let modifiers = &key.modifiers;
        let plain = !modifiers.control && !modifiers.alt && !modifiers.platform;
        if detail && key.key == "escape" && plain {
            // Back to the table first.
            if let Some(pane) = self.monitor.panes.get_mut(pane_id) {
                pane.list.detail = false;
            }
            return true;
        }
        let step = match key.key.as_str() {
            "up" => Some(Step::Up),
            "down" => Some(Step::Down),
            "pageup" => Some(Step::PageUp(page_rows)),
            "pagedown" => Some(Step::PageDown(page_rows)),
            "home" => Some(Step::Home),
            "end" => Some(Step::End),
            _ => None,
        }
        .filter(|_| plain && !modifiers.shift);
        if let Some(step) = step {
            self.step_process_selection(pane_id, step);
            return true;
        }
        if plain && !modifiers.shift && self.fold_key(pane_id, &key.key) {
            return true;
        }
        if key.key == "enter" && plain && !modifiers.shift && process_selected {
            return self.open_process_detail(pane_id);
        }
        if key.key == "escape" && plain && !search.read(cx).value().is_empty() {
            search.update(cx, |search, cx| search.set_value("", window, cx));
            return true;
        }
        if key.key == "escape" && plain && has_selection {
            if let Some(pane) = self.monitor.panes.get_mut(pane_id) {
                pane.list.selected = None;
            }
            return true;
        }
        if let Some(text) = key
            .key_char
            .as_deref()
            .filter(|text| plain && !text.trim().is_empty() && !text.chars().any(char::is_control))
        {
            // Typing searches.
            let query = format!("{}{text}", search.read(cx).value());
            search.update(cx, |search, cx| search.set_value(query, window, cx));
            self.focus_process_search(pane_id, window, cx);
            return true;
        }
        false
    }

    /// The Processes page `width` pixels wide: search, filters and count, then the table
    /// or what stands in for it, with the selection's inspector beside it on a wide
    /// page. A narrow page shows the selection's page of its own instead once opened.
    pub(super) fn render_processes_page(
        &self,
        pane_id: &PaneId,
        pane: &MonitorPane,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let feeds = self.monitor.feeds(&pane.source);
        let feed = &feeds.processes;
        let list = &pane.list;
        let placement = inspector_layout(width);
        list.wide.set(placement != Inspector::FullPage);
        let live = matches!(feed.state, FeedState::Live | FeedState::Stale);
        if live && list.detail && list.selected.is_some() && placement == Inspector::FullPage {
            return self.render_process_detail_page(pane_id, pane, feeds, cx);
        }
        let beside = match placement {
            Inspector::Beside { table_width } if live && list.selected.is_some() => {
                Some(table_width)
            }
            _ => None,
        };
        let body = match &feed.state {
            FeedState::Connecting => self.monitor_message("Reading processes…", None),
            FeedState::Failed(error) => self.monitor_message(
                "Can't read processes",
                Some((format!("{error} (retrying)"), colors.error)),
            ),
            FeedState::Live | FeedState::Stale if feed.processes.is_empty() => {
                self.monitor_message("No processes reported", None)
            }
            FeedState::Live | FeedState::Stale if list.table.rows.is_empty() => {
                let text = list.search.read(cx).value();
                let text = text.trim();
                self.monitor_message(
                    "No processes match",
                    (!text.is_empty()).then(|| (format!("Nothing matches “{text}”"), colors.muted)),
                )
            }
            FeedState::Live | FeedState::Stale => {
                self.render_process_table(pane_id, list, beside.unwrap_or(width), cx)
            }
        };
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .child(self.render_process_toolbar(pane_id, list, feed, width, cx))
            .when(feed.state == FeedState::Stale, |page| {
                page.child(self.monitor_stale_note())
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .child(div().flex_1().min_w_0().min_h_0().flex().child(body))
                    .when(beside.is_some(), |page| {
                        page.child(
                            div()
                                .flex_none()
                                .w(px(layout::INSPECTOR_WIDTH))
                                .min_h_0()
                                .flex()
                                .flex_col()
                                .border_l_1()
                                .border_color(color(colors.border))
                                .child(self.render_inspector(pane_id, pane, feeds, cx)),
                        )
                    }),
            )
            .into_any_element()
    }

    /// Search, filters, Group by app and the process count.
    fn render_process_toolbar(
        &self,
        pane_id: &PaneId,
        list: &ProcessList,
        feed: &ProcessFeed,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let current = effective_filter(list.filter, feed.apps_known);
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
                            this.set_process_filter(&pane_id, filter);
                            cx.notify();
                        }))
                }),
        );
        let grouped = !self.monitor.flat;
        let group_switch = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .child(
                controls::switch("monitor-group-by-app", grouped)
                    .label("Group by app")
                    .on_toggle(cx.listener(move |this, on: &bool, _, cx| {
                        this.set_group_by_app(*on);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .whitespace_nowrap()
                    .child("Group by app"),
            );
        let counted = matches!(feed.state, FeedState::Live | FeedState::Stale)
            && width >= TOOLBAR_COUNT_MIN_WIDTH;
        div()
            .flex_none()
            .px_4()
            .py_2()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_3()
            .gap_y_2()
            .border_b_1()
            .border_color(color(colors.border))
            .child(
                div().flex_1().min_w(px(120.0)).max_w(px(280.0)).child(
                    Input::new(&list.search)
                        .appearance(true)
                        .cleanable(true)
                        .with_size(gpui_component::Size::Small),
                ),
            )
            .child(filters)
            .child(group_switch)
            .when(counted, |toolbar| {
                toolbar.child(
                    div()
                        .flex_none()
                        .ml_auto()
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .text_color(muted)
                        .child(listing::format_count(
                            list.table.process_count(),
                            feed.processes.len(),
                        )),
                )
            })
            .into_any_element()
    }

    /// The column headings (click to sort, drag to move, drag a seam to resize,
    /// right-click to show or hide columns) over the rows, drawn as they scroll in.
    fn render_process_table(
        &self,
        pane_id: &PaneId,
        list: &ProcessList,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let layout = &self.state.monitor_columns;
        let table_width = (width - 2.0 * TABLE_INSET).max(0.0);
        list.table_width.set(table_width);
        let columns: Vec<(Column, f32)> = layout
            .visible_columns(table_width)
            .into_iter()
            .map(|column| (column, layout.width(column)))
            .collect();
        let muted = color(modal_text_color(colors.muted, &colors));
        let headings = columns.iter().enumerate().map(|(index, &(column, width))| {
            self.render_column_header(pane_id, list, (index, column, width), &columns, cx)
        });
        let header = div()
            .id("monitor-columns")
            .flex_none()
            .h(px(ROW_HEIGHT + 4.0))
            .px(px(TABLE_INSET))
            .flex()
            .items_center()
            .border_b_1()
            .border_color(color(colors.border))
            .text_size(px(UI_MICRO_TEXT_SIZE))
            .text_color(muted)
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.open_columns_menu(event.position, window, cx);
                    cx.stop_propagation();
                }),
            )
            .children(headings);
        let list_pane = pane_id.clone();
        let hover_pane = pane_id.clone();
        let rows = uniform_list(
            "monitor-rows",
            list.table.rows.len(),
            cx.processor(move |this, range: Range<usize>, window, cx| {
                if let Some(pane) = this.monitor.panes.get_mut(&list_pane) {
                    pane.list.page_rows = range.len().saturating_sub(1).max(1);
                }
                let keyboard = this
                    .monitor
                    .panes
                    .get(&list_pane)
                    .is_some_and(|pane| pane.focus.is_focused(window));
                this.render_process_rows(&list_pane, &columns, range, keyboard, cx)
            }),
        )
        .track_scroll(&list.scroll)
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
                if this.hover_process_table(&hover_pane, *hovered) {
                    cx.notify();
                }
            }))
            .child(header)
            .child(rows)
            .into_any_element()
    }

    /// The Name cell of row `index`: the fold chevron of an app, the app's icon (an app
    /// row, or a top-level process) and the name; an app's processes sit indented under
    /// its name. Hover shows where a process runs from, or an app's path.
    #[allow(clippy::too_many_arguments)]
    fn render_name_cell(
        &self,
        pane_id: &PaneId,
        index: usize,
        row: TableRow,
        table: &Arranged,
        processes: &[Process],
        grouped: bool,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let cell = process_cell(
            div().id(("monitor-name", index)),
            Column::Name,
            listing::NAME_MIN_WIDTH,
        )
        .gap_1();
        match row.kind {
            RowKind::Group(group) => {
                let group = &table.groups[group];
                let id = group.id.clone();
                let pane = pane_id.clone();
                let fold = div()
                    .id(("monitor-fold", index))
                    .flex_none()
                    .w(px(FOLD_WIDTH))
                    .flex()
                    .justify_center()
                    .cursor_pointer()
                    .child(controls::glyph(
                        if group.expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                        muted,
                    ))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.fold_process_group(&pane, &id, None);
                        cx.stop_propagation();
                        cx.notify();
                    }));
                let (title, detail): (SharedString, SharedString) = (
                    listing::display_line(&group.name).into(),
                    listing::display_line(&group.id).into(),
                );
                cell.child(fold)
                    .child(self.app_icon(Some(&group.id), &group.name, ROW_ICON))
                    .child(cell_text(listing::display_line(&group.name)).ml_1())
                    .child(
                        div()
                            .flex_none()
                            .text_color(muted)
                            .child(group.members.len().to_string()),
                    )
                    .tooltip(controls::tooltip(title, Some(detail), None))
            }
            RowKind::Process(process) => {
                let process = &processes[process];
                let details = process.details.clone();
                let app = details.app_group.as_ref();
                let cell = if row.member {
                    cell.child(div().flex_none().w(px(FOLD_WIDTH + ROW_ICON + 8.0)))
                } else {
                    cell.when(grouped, |cell| {
                        cell.child(div().flex_none().w(px(FOLD_WIDTH)))
                    })
                    .child(self.app_icon(
                        app.map(|app| app.id.as_str()),
                        app.map_or(details.name.as_str(), |app| app.name.as_str()),
                        ROW_ICON,
                    ))
                };
                let name = listing::display_line(&details.name);
                cell.child(cell_text(name).ml_1())
                    .tooltip(move |window, cx| {
                        let (title, detail) = name_tooltip(&details);
                        controls::tooltip(title, detail, None)(window, cx)
                    })
            }
        }
    }

    /// The table rows in `range`, with `columns` at their widths. The selection is
    /// filled in the accent while the table has the keyboard (`keyboard`), quieter
    /// otherwise. A double click opens a narrow pane's page of the row; a right click
    /// its menu.
    fn render_process_rows(
        &self,
        pane_id: &PaneId,
        columns: &[(Column, f32)],
        range: Range<usize>,
        keyboard: bool,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(pane) = self.monitor.panes.get(pane_id) else {
            return Vec::new();
        };
        let list = &pane.list;
        let feed = &self.monitor.feeds(&pane.source).processes;
        let table = &list.table;
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
                let row = *table.rows.get(index)?;
                let key = table.keys.get(index)?.clone();
                let selected = list.selected.as_ref() == Some(&key);
                let cells = columns
                    .iter()
                    .enumerate()
                    .map(|(position, &(column, cell_width))| {
                        if column == Column::Name {
                            return self
                                .render_name_cell(
                                    pane_id,
                                    index,
                                    row,
                                    table,
                                    &feed.processes,
                                    !self.monitor.flat,
                                    cx,
                                )
                                .into_any_element();
                        }
                        let text = match row.kind {
                            RowKind::Process(process) => process_cell_text(
                                &feed.processes[process],
                                column,
                                feed.logical_cpus,
                            ),
                            RowKind::Group(group) => {
                                group_cell_text(&table.groups[group], column, feed.logical_cpus)
                            }
                        };
                        let cell = process_cell(
                            div().id(("monitor-cell", index * width + position)),
                            column,
                            cell_width,
                        );
                        match text {
                            CellText::Value(text) => cell.child(cell_text(text)),
                            CellText::Quiet(text) => cell.text_color(muted).child(cell_text(text)),
                            CellText::Unavailable { withheld: true } => cell
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
                            CellText::Unavailable { withheld: false } => {
                                cell.text_color(muted).child(UNAVAILABLE)
                            }
                        }
                        .into_any_element()
                    });
                let (pane, menu_pane, menu_key) = (pane_id.clone(), pane_id.clone(), key.clone());
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
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            if let Some(ui) = this.monitor.panes.get_mut(&pane) {
                                ui.list.selected = Some(key.clone());
                                window.focus(&ui.focus, cx);
                            }
                            if event.click_count() == 2 {
                                this.open_process_detail(&pane);
                            }
                            cx.notify();
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                this.open_process_row_menu(
                                    &menu_pane,
                                    menu_key.clone(),
                                    event.position,
                                    window,
                                    cx,
                                );
                                cx.stop_propagation();
                            }),
                        )
                        .children(cells)
                        .into_any_element(),
                )
            })
            .collect()
    }
}

/// A table cell of `column`, `width` wide: Name takes the room the others leave, at
/// least `width`; amounts are right-aligned. One line, cut with an ellipsis.
pub(super) fn process_cell(cell: Stateful<Div>, column: Column, width: f32) -> Stateful<Div> {
    let cell = cell
        .h_full()
        .px_2()
        .flex()
        .items_center()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis();
    let cell = if column == Column::Name {
        cell.flex_1().min_w(px(width))
    } else {
        cell.flex_none().w(px(width))
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
