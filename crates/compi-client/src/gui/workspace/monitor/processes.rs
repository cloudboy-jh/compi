//! Monitor's Processes page: the host's processes in a virtualized table.
//!
//! Which processes a pane shows, in what order and how their values read is pure
//! (`crate::monitor`); the search, filter, sort and selection are this window's, per
//! pane. While the pointer is over the table its rows keep their order (values update in
//! place, newcomers go last, leavers go), and the table sorts again when the pointer
//! leaves or the sort, search or filter changes.
use super::*;
use crate::monitor::{self as listing, Column, Direction, Filter, ProcessKey, Search, Sort, Step};
use compi_protocol::system_metrics::{Process, ProcessDetails};
use gpui::{ScrollStrategy, UniformListScrollHandle, uniform_list};

/// One table row.
const ROW_HEIGHT: f32 = 24.0;
/// The table's inset from the page's edges.
const TABLE_INSET: f32 = 12.0;
/// Narrower than this, the toolbar drops its process count.
const TOOLBAR_COUNT_MIN_WIDTH: f32 = 480.0;
/// Longest command line a name's tooltip shows.
const TOOLTIP_COMMAND_CHARS: usize = 400;

/// The window's processes, shared by its Monitor panes.
#[derive(Default)]
pub(super) struct ProcessFeed {
    pub(super) slot: WatchSlot,
    pub(super) state: FeedState,
    pub(super) processes: Vec<Process>,
    logical_cpus: u32,
    apps_known: bool,
    /// The newest sample while Monitor is paused, shown when it resumes.
    pending: Option<ProcessesSample>,
}

/// One pane's view of the processes.
pub(super) struct ProcessList {
    pub(super) search: Entity<InputState>,
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
    page_rows: usize,
    _events: Subscription,
}

/// Filter that the feed can answer: Apps means nothing where the host can't tell apps.
fn effective_filter(filter: Filter, apps_known: bool) -> Filter {
    if filter == Filter::Apps && !apps_known {
        Filter::All
    } else {
        filter
    }
}

/// Orders `list`'s rows from `feed`: held in their last order while the pointer is over
/// the table and `hold`, else sorted. A selection no longer shown is cleared.
fn arrange(list: &mut ProcessList, feed: &ProcessFeed, hold: bool) {
    let filter = effective_filter(list.filter, feed.apps_known);
    list.rows = if hold && list.pointer {
        listing::arrange_held(&feed.processes, &list.keys, filter, &list.query, list.sort)
    } else {
        listing::arrange(&feed.processes, filter, &list.query, list.sort)
    };
    list.keys = list
        .rows
        .iter()
        .map(|&row| listing::key(&feed.processes[row]))
        .collect();
    if list
        .selected
        .is_some_and(|selected| !list.keys.contains(&selected))
    {
        list.selected = None;
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
    pub(super) fn new_process_list(
        &self,
        pane_id: &PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ProcessList {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search processes"));
        let changed = pane_id.clone();
        let events = cx.subscribe(&search, move |this, search, event: &InputEvent, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let query = Search::new(&search.read(cx).value());
            let MonitorUi {
                panes, processes, ..
            } = &mut this.monitor;
            let Some(list) = panes.get_mut(&changed).map(|pane| &mut pane.list) else {
                return;
            };
            if list.query != query {
                list.query = query;
                arrange(list, processes, false);
                list.scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            }
        });
        let mut list = ProcessList {
            search,
            query: Search::default(),
            filter: Filter::All,
            sort: Sort::default(),
            rows: Vec::new(),
            keys: Vec::new(),
            selected: None,
            pointer: false,
            scroll: UniformListScrollHandle::new(),
            page_rows: 1,
            _events: events,
        };
        arrange(&mut list, &self.monitor.processes, false);
        list
    }

    /// Forgets every process: what is not current is not shown.
    pub(super) fn reset_process_feed(&mut self, state: FeedState) {
        let MonitorUi {
            panes, processes, ..
        } = &mut self.monitor;
        processes.state = state;
        processes.processes.clear();
        processes.pending = None;
        for pane in panes.values_mut() {
            arrange(&mut pane.list, processes, false);
        }
    }

    pub(super) fn processes_event(&mut self, generation: u64, event: ProcessesEvent) {
        if !self.monitor.processes.slot.is_current(generation) {
            // A watcher stopped since.
            return;
        }
        match event {
            ProcessesEvent::Connecting => self.reset_process_feed(FeedState::Connecting),
            ProcessesEvent::Failed(error) => self.reset_process_feed(FeedState::Failed(error)),
            ProcessesEvent::Stale => {
                let feed = &mut self.monitor.processes;
                if feed.state == FeedState::Live {
                    feed.state = FeedState::Stale;
                }
            }
            ProcessesEvent::Sampled(sample) => {
                // Paused, the table keeps what it shows until Monitor resumes; the first
                // sample always shows, so a pause never leaves it empty.
                if self.monitor.paused && !self.monitor.processes.processes.is_empty() {
                    self.monitor.processes.pending = Some(sample);
                } else {
                    self.show_process_sample(sample);
                }
            }
        }
    }

    fn show_process_sample(&mut self, sample: ProcessesSample) {
        let MonitorUi {
            panes, processes, ..
        } = &mut self.monitor;
        processes.state = FeedState::Live;
        processes.processes = sample.processes;
        processes.logical_cpus = sample.logical_cpus;
        processes.apps_known = sample.apps_known;
        for pane in panes.values_mut() {
            arrange(&mut pane.list, processes, true);
        }
    }

    /// Monitor resumed: the newest sample received while paused shows.
    pub(super) fn resume_processes(&mut self) {
        if let Some(sample) = self.monitor.processes.pending.take() {
            self.show_process_sample(sample);
        }
    }

    /// How many processes the host runs, once known.
    pub(super) fn process_count(&self) -> Option<usize> {
        let feed = &self.monitor.processes;
        matches!(feed.state, FeedState::Live | FeedState::Stale).then_some(feed.processes.len())
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
            .and_then(|selected| list.keys.iter().position(|key| *key == selected));
        if let Some(next) = listing::step_selection(current, list.keys.len(), step) {
            list.selected = Some(list.keys[next]);
            list.scroll.scroll_to_item(next, ScrollStrategy::Nearest);
        }
    }

    fn set_process_filter(&mut self, pane_id: &PaneId, filter: Filter) {
        let MonitorUi {
            panes, processes, ..
        } = &mut self.monitor;
        if let Some(list) = panes.get_mut(pane_id).map(|pane| &mut pane.list)
            && list.filter != filter
        {
            list.filter = filter;
            arrange(list, processes, false);
            list.scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
    }

    fn sort_process_table(&mut self, pane_id: &PaneId, column: Column) {
        let MonitorUi {
            panes, processes, ..
        } = &mut self.monitor;
        if let Some(list) = panes.get_mut(pane_id).map(|pane| &mut pane.list) {
            list.sort = list.sort.clicked(column);
            arrange(list, processes, false);
            if let Some(at) = list
                .selected
                .and_then(|selected| list.keys.iter().position(|key| *key == selected))
            {
                list.scroll.scroll_to_item(at, ScrollStrategy::Nearest);
            }
        }
    }

    /// The pointer entered or left the table; leaving sorts it again.
    fn hover_process_table(&mut self, pane_id: &PaneId, hovered: bool) -> bool {
        let MonitorUi {
            panes, processes, ..
        } = &mut self.monitor;
        let Some(list) = panes.get_mut(pane_id).map(|pane| &mut pane.list) else {
            return false;
        };
        if list.pointer == hovered {
            return false;
        }
        list.pointer = hovered;
        if !hovered {
            arrange(list, processes, false);
        }
        !hovered
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
        let modifiers = &key.modifiers;
        let plain = !modifiers.control && !modifiers.alt && !modifiers.platform;
        let step = match key.key.as_str() {
            "up" => Some(Step::Up),
            "down" => Some(Step::Down),
            "pageup" => Some(Step::PageUp(list.page_rows)),
            "pagedown" => Some(Step::PageDown(list.page_rows)),
            "home" => Some(Step::Home),
            "end" => Some(Step::End),
            _ => None,
        }
        .filter(|_| plain && !modifiers.shift);
        if let Some(step) = step {
            self.step_process_selection(pane_id, step);
        } else if key.key == "escape" && plain && !list.search.read(cx).value().is_empty() {
            list.search
                .update(cx, |search, cx| search.set_value("", window, cx));
        } else if key.key == "escape" && plain && list.selected.is_some() {
            if let Some(pane) = self.monitor.panes.get_mut(pane_id) {
                pane.list.selected = None;
            }
        } else if let Some(text) = key
            .key_char
            .as_deref()
            .filter(|text| plain && !text.trim().is_empty() && !text.chars().any(char::is_control))
        {
            // Typing searches.
            let query = format!("{}{text}", list.search.read(cx).value());
            list.search
                .update(cx, |search, cx| search.set_value(query, window, cx));
            self.focus_process_search(pane_id, window, cx);
        } else {
            return false;
        }
        true
    }

    /// The Processes page `width` pixels wide: search, filters and count, then the table
    /// or what stands in for it.
    pub(super) fn render_processes_page(
        &self,
        pane_id: &PaneId,
        pane: &MonitorPane,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let feed = &self.monitor.processes;
        let list = &pane.list;
        let body = match &feed.state {
            FeedState::Connecting => self.monitor_message("Reading processes…", None),
            FeedState::Failed(error) => self.monitor_message(
                "Can't read processes",
                Some((format!("{error} (retrying)"), colors.error)),
            ),
            FeedState::Live | FeedState::Stale if feed.processes.is_empty() => {
                self.monitor_message("No processes reported", None)
            }
            FeedState::Live | FeedState::Stale if list.rows.is_empty() => {
                let text = list.search.read(cx).value();
                let text = text.trim();
                self.monitor_message(
                    "No processes match",
                    (!text.is_empty()).then(|| (format!("Nothing matches “{text}”"), colors.muted)),
                )
            }
            FeedState::Live | FeedState::Stale => {
                self.render_process_table(pane_id, list, width, cx)
            }
        };
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .child(self.render_process_toolbar(pane_id, list, width, cx))
            .when(feed.state == FeedState::Stale, |page| {
                page.child(self.monitor_stale_note())
            })
            .child(body)
            .into_any_element()
    }

    /// Search, filters and the process count.
    fn render_process_toolbar(
        &self,
        pane_id: &PaneId,
        list: &ProcessList,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let feed = &self.monitor.processes;
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
            .when(counted, |toolbar| {
                toolbar.child(
                    div()
                        .flex_none()
                        .ml_auto()
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .text_color(color(modal_text_color(colors.muted, &colors)))
                        .child(listing::format_count(list.rows.len(), feed.processes.len())),
                )
            })
            .into_any_element()
    }

    /// The column headings (click to sort) over the rows, drawn as they scroll in.
    fn render_process_table(
        &self,
        pane_id: &PaneId,
        list: &ProcessList,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let columns = listing::visible_columns(width - 2.0 * TABLE_INSET);
        let muted = color(modal_text_color(colors.muted, &colors));
        let headings = columns.iter().enumerate().map(|(index, &column)| {
            let sorted = list.sort.column == column;
            let arrow = sorted.then(|| {
                controls::glyph(
                    match list.sort.direction {
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
            process_cell(div().id(("monitor-column", index)), column)
                .gap_1()
                .cursor_pointer()
                .rounded_sm()
                .hover(move |style| style.bg(color(colors.surface_hover)))
                .when(sorted, |cell| cell.font_weight(FontWeight::SEMIBOLD))
                .children(before)
                .child(column.label())
                .children(after)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.sort_process_table(&pane_id, column);
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
        let rows = uniform_list(
            "monitor-rows",
            list.rows.len(),
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

    /// The table rows in `range`. The selection is filled in the accent while the table
    /// has the keyboard (`keyboard`), quieter otherwise.
    fn render_process_rows(
        &self,
        pane_id: &PaneId,
        columns: &[Column],
        range: Range<usize>,
        keyboard: bool,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(list) = self.monitor.panes.get(pane_id).map(|pane| &pane.list) else {
            return Vec::new();
        };
        let feed = &self.monitor.processes;
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
                let process = feed.processes.get(*list.rows.get(index)?)?;
                let key = listing::key(process);
                let selected = list.selected == Some(key);
                let details = &process.details;
                let sample = &process.sample;
                let cells = columns.iter().enumerate().map(|(position, &column)| {
                    let id = index * width + position;
                    let value = match column {
                        Column::Name => Some(listing::display_line(&details.name)),
                        Column::Cpu => listing::cpu_share(sample.cpu_percent, feed.logical_cpus)
                            .map(listing::format_cpu),
                        Column::Memory => sample.memory_bytes.map(listing::format_bytes),
                        Column::Threads => sample.threads.map(|threads| threads.to_string()),
                        Column::CpuTime => sample.cpu_time_ms.map(listing::format_cpu_time),
                        Column::Pid => Some(sample.pid.to_string()),
                        Column::User => details.user.as_deref().map(listing::display_line),
                    };
                    let cell = process_cell(div().id(("monitor-cell", id)), column);
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
                                ui.list.selected = Some(key);
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
fn process_cell(cell: Stateful<Div>, column: Column) -> Stateful<Div> {
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
