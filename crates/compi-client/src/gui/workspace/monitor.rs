//! Monitor: a content pane showing the daemon host's usage. A nav rail on the left lists
//! the pages (Overview, Processes, then the hardware with live values) above the host;
//! a header line names the page and holds the range and pause controls.
//!
//! The pane's page lives in the daemon (`PaneContent::Monitor`), so it persists with
//! the pane, and is sent the way Settings sends its section. The range, pause, search,
//! filter, sort and selection are this window's.
//!
//! Three watchers per window feed it (`watch`): the host's totals while any Monitor pane
//! or Settings → Monitor is drawn; its processes, and the apps' icons (`icons`), while a
//! drawn pane shows Overview (Memory by app) or Processes, or Settings → Monitor (which
//! draws Overview's cards) is drawn. Pausing freezes what is drawn while samples keep
//! arriving; resuming jumps to the newest.
use super::*;
use crate::gui::controls::IconName;
use crate::monitor::totals::Span;
use compi_protocol::{MonitorContent, MonitorPage, PaneContent};
use gpui::Entity;
use gpui_component::Sizable as _;
use gpui_component::input::{Input, InputEvent, InputState};
use std::cell::Cell;
use std::sync::mpsc;

mod icons;
mod overview;
mod processes;
mod watch;

use crate::monitor::apps::RowKey;
use crate::monitor::layout::{self, CardMove, OverviewCard, OverviewRows};
use icons::{ICON_QUEUE, IconCache, IconsEvent};
use overview::{Metric, TotalsFeed};
use processes::{ProcessFeed, ProcessList};
use watch::{
    LINGER, ProcessesEvent, ProcessesSample, SlotChange, TotalsEvent, WatchContext, WatchSlot,
};

const UNAVAILABLE: &str = "—";
/// The nav rail, with labels and values, and with icons only.
const NAV_WIDTH: f32 = 196.0;
const NAV_COMPACT_WIDTH: f32 = 52.0;
/// Panes narrower than this show the nav as icons only, which leaves a page of at
/// least the width a single column of cards needs.
const NAV_COMPACT_BELOW: f32 = 640.0;
/// Narrower than this, the header drops the host's name.
const HEADER_SUBTITLE_MIN_WIDTH: f32 = 440.0;
/// Over a page whose samples stopped coming.
const STALE_NOTE: &str =
    "Not updating: no new sample from the daemon for a while; the values shown are out of date.";

/// Where Overview's cards are drawn: in a Monitor pane, or in Settings → Monitor, whose
/// cards open Monitor.
#[derive(Clone, Debug, PartialEq, Eq)]
enum OverviewHost {
    Pane(PaneId),
    Settings,
}

/// What a click on Overview opens: a page, or Processes on an app of Memory by app.
#[derive(Clone, Debug, PartialEq, Eq)]
enum MonitorTarget {
    Page(MonitorPage),
    App(RowKey),
}

/// What a window's watchers report, tagged with the watcher's generation.
pub(in crate::gui) enum MonitorEvent {
    Processes {
        generation: u64,
        event: ProcessesEvent,
    },
    Totals {
        generation: u64,
        /// Boxed: a totals sample is several times the size of the other events.
        event: Box<TotalsEvent>,
    },
    Icons {
        generation: u64,
        event: IconsEvent,
    },
}

/// What the window knows from one watcher.
#[derive(Clone, Debug, Default, PartialEq)]
enum FeedState {
    #[default]
    Connecting,
    Live,
    Stale,
    Failed(String),
}

/// One Monitor pane's client state.
struct MonitorPane {
    /// Keyboard focus of the pane; the search field sits inside it.
    focus: FocusHandle,
    /// The page shown. It is sent to the daemon when it differs from `persisted`, the
    /// page the daemon last reported; `sent` is the one on its way.
    page: MonitorPage,
    persisted: MonitorPage,
    sent: Option<MonitorPage>,
    /// Overview's card with the keyboard highlight.
    card: Option<OverviewCard>,
    /// Overview's grid at its last draw: the highlight moves on it.
    card_rows: Cell<OverviewRows>,
    list: ProcessList,
}

/// The Monitor panes of this window and what they share.
#[derive(Default)]
pub(in crate::gui) struct MonitorUi {
    panes: HashMap<PaneId, MonitorPane>,
    processes: ProcessFeed,
    totals: TotalsFeed,
    icons: IconCache,
    /// How much history the graphs show.
    span: Span,
    paused: bool,
    /// What the next Monitor pane drawn opens on: set when Settings → Monitor opens
    /// Monitor from a card before the pane has client state.
    pending: Option<MonitorTarget>,
}

impl MonitorUi {
    /// The app icons this window holds, so its sprite atlas keeps them while it does.
    pub(in crate::gui) fn icon_images(&self) -> impl Iterator<Item = &Arc<RenderImage>> {
        self.icons.images()
    }
}

/// The Monitor panes of `node`: content panes, and terminal views on either side.
fn monitor_leaves<'a>(node: &'a LayoutNode, output: &mut Vec<&'a PaneId>) {
    match node {
        LayoutNode::Split { first, second, .. } => {
            monitor_leaves(first, output);
            monitor_leaves(second, output);
        }
        leaf => {
            if let (Some(pane_id), Some(PaneContent::Monitor(_))) =
                (leaf.leaf_pane(), leaf.content())
            {
                output.push(pane_id);
            }
        }
    }
}

const fn page_title(page: MonitorPage) -> &'static str {
    match page {
        MonitorPage::Overview => "Overview",
        MonitorPage::Processes => "Processes",
        MonitorPage::Cpu => "CPU",
        MonitorPage::Memory => "Memory",
        MonitorPage::Disk => "Disk",
        MonitorPage::Network => "Network",
    }
}

impl CompiApp {
    /// Opens Monitor: focuses this window's Monitor pane (showing it where a terminal
    /// carries it), else shows it in place of the focused terminal, else opens one
    /// beside the focused pane (in a tab of its own when the window has none).
    pub(super) fn open_monitor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_monitor_on(None, window, cx);
    }

    /// Opens Monitor as `open_monitor` does, then shows `target` in it.
    fn open_monitor_on(
        &mut self,
        target: Option<MonitorTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            self.reveal_view(&pane_id);
            self.focus_pane(pane_id.clone());
            match target {
                Some(target) if self.monitor.panes.contains_key(&pane_id) => {
                    self.show_monitor_target(&pane_id, target);
                }
                target => self.monitor.pending = target,
            }
            return;
        }
        self.monitor.pending = target;
        let content = PaneContent::Monitor(MonitorContent::default());
        if let Some(terminal) = self.focused_terminal() {
            self.show_view(&terminal, content, window, cx);
            return;
        }
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

    /// Monitor moved from `from` to pane `to` with its view: its page, search and
    /// selection go along.
    pub(super) fn move_monitor_pane(&mut self, from: &PaneId, to: &PaneId, cx: &mut Context<Self>) {
        if let Some(mut pane) = self.monitor.panes.remove(from) {
            pane.list.moved_to(to, cx);
            self.monitor.panes.insert(to.clone(), pane);
        }
    }

    /// A click on Overview: in a pane it shows `target` there; in Settings it opens
    /// Monitor on `target`.
    fn open_overview_target(
        &mut self,
        host: &OverviewHost,
        target: MonitorTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match host {
            OverviewHost::Pane(pane_id) => self.show_monitor_target(pane_id, target),
            OverviewHost::Settings => self.open_monitor_on(Some(target), window, cx),
        }
    }

    /// Shows `target` in the Monitor pane `pane_id`.
    fn show_monitor_target(&mut self, pane_id: &PaneId, target: MonitorTarget) {
        match target {
            MonitorTarget::Page(page) => self.set_monitor_page(pane_id, page),
            MonitorTarget::App(key) => self.reveal_process_row(pane_id, key),
        }
    }

    /// Before drawing: state for each Monitor pane shown, its page sent to the daemon
    /// when it changed, and the watchers running while wanted (Settings → Monitor
    /// wants the ones a pane showing Overview does).
    pub(super) fn prepare_monitor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let page_of = |pane: &PaneId| match content::find_pane_content(workspace, pane) {
            Some(PaneContent::Monitor(monitor)) => Some(monitor.page),
            _ => None,
        };
        let moving = self.views.moving_from();
        self.monitor
            .panes
            .retain(|pane, _| page_of(pane).is_some() || moving == Some(pane));
        let shown: Vec<(PaneId, MonitorPage)> = self
            .shown_content_panes()
            .filter_map(|pane| Some((pane.clone(), page_of(pane)?)))
            .collect();
        for (pane_id, daemon) in &shown {
            let daemon = *daemon;
            match self.monitor.panes.get_mut(pane_id) {
                None => {
                    let pane = self.new_monitor_pane(pane_id, daemon, window, cx);
                    self.monitor.panes.insert(pane_id.clone(), pane);
                    if let Some(target) = self.monitor.pending.take() {
                        self.show_monitor_target(pane_id, target);
                    }
                }
                Some(pane) => {
                    if pane.sent == Some(daemon) {
                        pane.sent = None;
                        pane.persisted = daemon;
                    } else if pane.sent.is_some() && !self.mutation_pending {
                        // The change did not apply; show what the daemon has.
                        pane.sent = None;
                        pane.persisted = daemon;
                        pane.page = daemon;
                    } else if pane.sent.is_none() && daemon != pane.persisted {
                        pane.persisted = daemon;
                        pane.page = daemon;
                    }
                }
            }
            let Some(pane) = self.monitor.panes.get_mut(pane_id) else {
                continue;
            };
            if pane.page != pane.persisted && pane.sent.is_none() && !self.mutation_pending {
                pane.sent = Some(pane.page);
                let content = PaneContent::Monitor(MonitorContent { page: pane.page });
                self.mutate(
                    WorkspaceMutation::UpdateContent {
                        pane_id: pane_id.clone(),
                        content,
                    },
                    false,
                );
            }
        }
        let settings = self.settings_row_shown(super::settings::MONITOR_ROW);
        // Overview's Memory by app needs the processes too; resource pages do not.
        let processes_wanted = settings
            || shown.iter().any(|(pane_id, _)| {
                self.monitor.panes.get(pane_id).is_some_and(|pane| {
                    matches!(pane.page, MonitorPage::Overview | MonitorPage::Processes)
                })
            });
        let context = WatchContext {
            target: self.target.clone(),
            sender: self.event_tx.clone(),
            quiesced: self.update_quiesced.clone(),
            connection_guard: self.update_connection_guard.clone(),
        };
        let mut linger = false;
        match self.monitor.totals.slot.want(settings || !shown.is_empty()) {
            SlotChange::Start(generation, stop) => {
                self.monitor.totals.reset(FeedState::Connecting);
                watch::watch_totals(context.clone(), generation, stop);
            }
            SlotChange::Linger => linger = true,
            SlotChange::Unchanged => {}
        }
        match self.monitor.processes.slot.want(processes_wanted) {
            SlotChange::Start(generation, stop) => {
                self.reset_process_feed(FeedState::Connecting);
                watch::watch_processes(context.clone(), generation, stop);
            }
            SlotChange::Linger => linger = true,
            SlotChange::Unchanged => {}
        }
        match self.monitor.icons.slot.want(processes_wanted) {
            SlotChange::Start(generation, stop) => {
                let (requests, asked) = mpsc::sync_channel(ICON_QUEUE);
                self.monitor.icons.start(requests);
                watch::watch_icons(context, generation, stop, asked);
            }
            SlotChange::Linger => linger = true,
            SlotChange::Unchanged => {}
        }
        if linger {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(LINGER).await;
                this.update(cx, |this, cx| {
                    if this.expire_monitor_watchers() {
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    fn new_monitor_pane(
        &self,
        pane_id: &PaneId,
        page: MonitorPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> MonitorPane {
        MonitorPane {
            focus: cx.focus_handle(),
            page,
            persisted: page,
            sent: None,
            card: None,
            card_rows: Cell::new(&[]),
            list: self.new_process_list(pane_id, window, cx),
        }
    }

    /// Stops each watcher no pane has wanted for [`LINGER`]; whether one stopped.
    fn expire_monitor_watchers(&mut self) -> bool {
        let totals = self.monitor.totals.slot.expire();
        if totals {
            self.monitor.totals.reset(FeedState::Connecting);
        }
        let processes = self.monitor.processes.slot.expire();
        if processes {
            self.reset_process_feed(FeedState::Connecting);
        }
        let icons = self.monitor.icons.slot.expire();
        if icons {
            self.monitor.icons.stop();
        }
        totals || processes || icons
    }

    pub(super) fn monitor_event(&mut self, event: MonitorEvent) {
        match event {
            MonitorEvent::Processes { generation, event } => {
                self.processes_event(generation, event)
            }
            MonitorEvent::Totals { generation, event } => self.totals_event(generation, *event),
            MonitorEvent::Icons { generation, event } => {
                if self.monitor.icons.slot.is_current(generation) {
                    self.monitor.icons.event(event);
                }
            }
        }
    }

    /// Shows `page` in `pane_id`; the daemon learns it before the next draw.
    fn set_monitor_page(&mut self, pane_id: &PaneId, page: MonitorPage) {
        if let Some(pane) = self.monitor.panes.get_mut(pane_id) {
            pane.page = page;
            pane.card = None;
        }
    }

    fn set_monitor_paused(&mut self, paused: bool) {
        self.monitor.paused = paused;
        self.monitor.totals.freeze(paused);
        if !paused {
            self.resume_processes();
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
                || pane.list.search.focus_handle(cx) == *focused
        })
    }

    /// Where the keyboard should go in the focused Monitor pane `pane_id`, or `None`
    /// when it is there: the search field keeps it on the Processes page (it may not be
    /// drawn yet when Ctrl/Cmd+F just switched page), and the pane takes it otherwise.
    pub(super) fn monitor_keyboard(
        &self,
        pane_id: &PaneId,
        window: &Window,
        cx: &App,
    ) -> Option<FocusHandle> {
        let pane = self.monitor.panes.get(pane_id)?;
        if pane.list.search.focus_handle(cx).is_focused(window) {
            return (pane.page != MonitorPage::Processes).then(|| pane.focus.clone());
        }
        (!pane.focus.contains_focused(window, cx)).then(|| pane.focus.clone())
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
        if pane.list.search.focus_handle(cx).is_focused(window) {
            // Text belongs to the field; Escape, Down, Enter and Tab were taken before it.
            return;
        }
        let page = pane.page;
        let key = &event.keystroke;
        let modifiers = &key.modifiers;
        let plain = !modifiers.control && !modifiers.alt && !modifiers.platform && !modifiers.shift;
        let handled = if key.key == "f"
            && (modifiers.control || modifiers.platform)
            && !modifiers.alt
            && !modifiers.shift
        {
            // Ctrl/Cmd+F searches the processes, from any page.
            self.set_monitor_page(pane_id, MonitorPage::Processes);
            self.focus_process_search(pane_id, window, cx);
            true
        } else if page == MonitorPage::Processes {
            self.processes_key_down(pane_id, key, window, cx)
        } else {
            plain && self.monitor_page_key(pane_id, page, &key.key)
        };
        // Esc that let go of everything shows a terminal's shell again.
        if !handled && !self.handle_app_key(event, window, cx) && key.key == "escape" && plain {
            self.escape_to_shell(pane_id);
        }
        // Nothing typed in Monitor reaches a shell.
        cx.stop_propagation();
        cx.notify();
    }

    /// Keys on Overview (arrows move between cards, Enter opens one, Escape lets go)
    /// and on a resource page (Escape goes back to Overview).
    fn monitor_page_key(&mut self, pane_id: &PaneId, page: MonitorPage, key: &str) -> bool {
        let Some(pane) = self.monitor.panes.get_mut(pane_id) else {
            return false;
        };
        if let Some(metric) = Metric::of_page(page) {
            if key != "escape" {
                return false;
            }
            pane.page = MonitorPage::Overview;
            pane.card = Some(metric.card());
            return true;
        }
        let rows = pane.card_rows.get();
        let drawn = |card: OverviewCard| {
            rows.iter()
                .flat_map(|row| row.iter())
                .any(|placed| placed.card == card)
        };
        let step = match key {
            "escape" if pane.card.is_none() => return false,
            "escape" => {
                pane.card = None;
                return true;
            }
            "enter" | "space" => {
                let Some(card) = pane.card.filter(|card| drawn(*card)) else {
                    return false;
                };
                let page = overview::card_page(card);
                self.set_monitor_page(pane_id, page);
                return true;
            }
            "left" => CardMove::Left,
            "right" => CardMove::Right,
            "up" => CardMove::Up,
            "down" => CardMove::Down,
            "home" => CardMove::Home,
            "end" => CardMove::End,
            _ => return false,
        };
        pane.card = layout::move_card(rows, pane.card, step);
        true
    }

    /// A centred message in place of a page's content.
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

    /// The line over a page whose samples stopped coming.
    fn monitor_stale_note(&self) -> AnyElement {
        let colors = *self.colors();
        div()
            .flex_none()
            .px_4()
            .py_1()
            .border_b_1()
            .border_color(color(colors.border))
            .text_size(px(UI_MICRO_TEXT_SIZE))
            .text_color(color(modal_text_color(colors.muted, &colors)))
            .child(STALE_NOTE)
            .into_any_element()
    }

    /// One Monitor pane `width` pixels wide: the nav rail, then the header and page.
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
        let compact = width < NAV_COMPACT_BELOW;
        // The rail's border is inside its width (border-box) and the content pane has no
        // padding or border of its own, so the page gets exactly the rest.
        let page_width = (width
            - if compact {
                NAV_COMPACT_WIDTH
            } else {
                NAV_WIDTH
            })
        .max(1.0);
        let page = match pane.page {
            MonitorPage::Overview => self.render_monitor_overview(pane_id, pane, page_width, cx),
            MonitorPage::Processes => self.render_processes_page(pane_id, pane, page_width, cx),
            page => Metric::of_page(page).map_or_else(
                || div().into_any_element(),
                |metric| self.render_monitor_resource(pane_id, metric, page_width, cx),
            ),
        };
        let key_pane = pane_id.clone();
        let click_pane = pane_id.clone();
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .track_focus(&pane.focus)
            .text_color(color(modal_text_color(colors.foreground, &colors)))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                this.monitor_key_down(&key_pane, event, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    // The search field takes focus itself; elsewhere the pane does.
                    if !window.default_prevented()
                        && let Some(pane) = this.monitor.panes.get(&click_pane)
                    {
                        window.focus(&pane.focus, cx);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(self.render_monitor_nav(pane_id, pane, compact, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(self.render_monitor_header(pane, page_width, cx))
                    .child(page),
            )
            .into_any_element()
    }

    /// The nav rail: Overview, Processes, then the hardware with live values, and the
    /// host at the foot. `compact` shows icons only, their names in tooltips.
    fn render_monitor_nav(
        &self,
        pane_id: &PaneId,
        pane: &MonitorPane,
        compact: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let latest = self.monitor.totals.shown().latest();
        let entry = |index: usize,
                     page: MonitorPage,
                     icon: IconName,
                     tint: Option<u32>,
                     value: Option<Option<String>>| {
            let selected = pane.page == page;
            let label = page_title(page);
            let pane_id = pane_id.clone();
            let value_text = value
                .clone()
                .map(|value| value.unwrap_or_else(|| UNAVAILABLE.to_owned()));
            div()
                .id(("monitor-nav", index))
                .flex_none()
                .h(px(30.0))
                .rounded_md()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .when(compact, |entry| entry.justify_center())
                .when(!compact, |entry| entry.px_2())
                .when(selected, |entry| {
                    entry
                        .bg(color(colors.surface_hover))
                        .font_weight(FontWeight::MEDIUM)
                })
                .when(!selected, |entry| {
                    entry
                        .text_color(muted)
                        .hover(move |style| style.bg(color(colors.surface_hover)))
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_monitor_page(&pane_id, page);
                    cx.notify();
                }))
                .child(controls::glyph(
                    icon,
                    tint.map_or_else(
                        || {
                            if selected {
                                color(modal_text_color(colors.foreground, &colors))
                            } else {
                                muted
                            }
                        },
                        color,
                    ),
                ))
                .when(compact, |entry| {
                    entry.tooltip(controls::tooltip(
                        label,
                        value_text.clone().map(SharedString::from),
                        None,
                    ))
                })
                .when(!compact, |entry| {
                    entry
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(label),
                        )
                        .children(value.map(|value| {
                            div()
                                .flex_none()
                                .text_size(px(UI_MICRO_TEXT_SIZE))
                                .text_color(muted)
                                .whitespace_nowrap()
                                .child(value.unwrap_or_else(|| UNAVAILABLE.to_owned()))
                        }))
                })
        };
        let hardware = Metric::ALL.into_iter().enumerate().map(|(index, metric)| {
            entry(
                2 + index,
                metric.page(),
                metric.icon(),
                Some(self.metric_color(metric)),
                latest.map(|latest| metric.nav_value(latest)),
            )
        });
        let host = self.monitor.totals.host.as_ref();
        let host_name = host
            .and_then(|host| host.host_name.as_deref())
            .map(crate::monitor::display_line);
        let detail = self.monitor_host_detail();
        div()
            .flex_none()
            .w(px(if compact {
                NAV_COMPACT_WIDTH
            } else {
                NAV_WIDTH
            }))
            .h_full()
            .flex()
            .flex_col()
            .gap_0p5()
            .px(px(if compact { 8.0 } else { 10.0 }))
            .py_3()
            .border_r_1()
            .border_color(color(colors.border))
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .child(entry(
                0,
                MonitorPage::Overview,
                IconName::LayoutDashboard,
                None,
                None,
            ))
            .child(entry(
                1,
                MonitorPage::Processes,
                IconName::List,
                None,
                self.process_count().map(|count| Some(count.to_string())),
            ))
            .child(if compact {
                div()
                    .my_2()
                    .mx_1()
                    .h(px(1.0))
                    .bg(color(colors.border))
                    .into_any_element()
            } else {
                div()
                    .px_2()
                    .pt_3()
                    .pb_1()
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .text_color(muted)
                    .child("Hardware")
                    .into_any_element()
            })
            .children(hardware)
            .child(div().flex_1())
            .when(
                !compact && (host_name.is_some() || detail.is_some()),
                |nav| {
                    nav.child(
                        div()
                            .px_2()
                            .pt_2()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .border_t_1()
                            .border_color(color(colors.border))
                            .children(host_name.map(|name| {
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(name)
                            }))
                            .children(detail.map(|detail| {
                                div()
                                    .text_size(px(UI_MICRO_TEXT_SIZE))
                                    .text_color(muted)
                                    .child(detail)
                            })),
                    )
                },
            )
            .into_any_element()
    }

    /// The header line: the page's title and the host, then the range and pause.
    fn render_monitor_header(
        &self,
        pane: &MonitorPane,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let paused = self.monitor.paused;
        let host = self
            .monitor
            .totals
            .host
            .as_ref()
            .and_then(|host| host.host_name.as_deref())
            .map(crate::monitor::display_line)
            .filter(|_| width >= HEADER_SUBTITLE_MIN_WIDTH);
        let spans = (pane.page != MonitorPage::Processes).then(|| {
            controls::segmented(
                "monitor-span",
                Span::ALL.into_iter().enumerate().map(|(index, span)| {
                    controls::text_button(("monitor-span", index), span.label())
                        .selected(self.monitor.span == span)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.monitor.span = span;
                            cx.notify();
                        }))
                }),
            )
        });
        let pause = controls::icon_button(
            "monitor-pause",
            if paused {
                IconName::Play
            } else {
                IconName::Pause
            },
            if paused { "Resume" } else { "Pause" },
        )
        .header()
        .selected(paused)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_monitor_paused(!paused);
            cx.notify();
        }));
        div()
            .flex_none()
            .h(px(44.0))
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(color(colors.border))
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(page_title(pane.page)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .text_color(muted)
                    .child(match (host, paused) {
                        (Some(host), true) => format!("{host} · Paused"),
                        (Some(host), false) => host,
                        (None, true) => "Paused".to_owned(),
                        (None, false) => String::new(),
                    }),
            )
            .children(spans)
            .child(pause)
            .into_any_element()
    }
}
