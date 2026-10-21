//! The Processes page's inspector and what acts on a process: the selected row's
//! details beside the table when the page is wide enough (`layout::inspector_layout`),
//! else on a page of its own opened by Enter or a double click, with Back (Escape goes
//! Back first); the row menu (Inspect, Quit…, Force quit…, Open terminal, Copy PID,
//! Copy command); and Quit / Force quit, which ask first.
//!
//! A process's inspector shows what the samples carry (identity, user and owner, app,
//! executable, parent, children from the feed's parent pids, and the last 5 minutes of
//! CPU and memory from the window's history), and asks the daemon for the rest (state,
//! working directory, full arguments) on a thread when the selection changes and every
//! [`INSPECT_EVERY`] while shown. An app's row shows its sums and its processes.
//!
//! Quit and Force quit are aimed at the process's `(pid, start_time)`: the daemon
//! checks it is still that process right before signalling it, and signals nothing
//! else. Open terminal focuses the pane of the Compi terminal the process runs in.
use super::overview::dot_graph;
use super::processes::ROW_ICON;
use super::*;
use crate::monitor::apps::Group;
use crate::monitor::totals::{self, Combine};
use crate::monitor::{self as listing, ProcessKey};
use compi_protocol::system_metrics::{
    Process, ProcessInspection, ProcessOwner, ProcessState, QuitOutcome,
};
use compi_protocol::{PaneSide, SurfaceId, WorkspaceSnapshot};
use std::time::{SystemTime, UNIX_EPOCH};

/// How often an inspector shown asks the daemon again.
const INSPECT_EVERY: Duration = Duration::from_secs(10);
/// The height of the inspector's CPU and memory graphs.
const GRAPH_HEIGHT: f32 = 28.0;
/// Memory below this never fills the memory graph, so a small process stays low.
const MEMORY_SCALE_FLOOR: f64 = 64.0 * 1024.0 * 1024.0;
/// Children and app members listed by name; the rest are counted.
const LISTED_PROCESSES: usize = 50;
/// The inspector's icon.
const INSPECTOR_ICON: f32 = 28.0;

/// What the daemon said about a process beyond its samples.
pub(super) enum InspectAnswer {
    Reading,
    Read(ProcessInspection),
    /// It answered that the process is gone, or its pid names another one now.
    Exited,
    Failed(String),
}

/// The inspected process and the daemon's last answer about it.
pub(super) struct Inspection {
    key: ProcessKey,
    answer: InspectAnswer,
    asked: Instant,
    /// A request is on its way.
    asking: bool,
}

/// The pane holding terminal `surface`, with its workspace and tab.
fn surface_pane(
    workspace: &WorkspaceSnapshot,
    surface: &SurfaceId,
) -> Option<(SessionId, TabId, PaneId)> {
    for session in &workspace.sessions {
        for tab in &session.tabs {
            let mut leaves = Vec::new();
            collect_leaves(&tab.layout, &mut leaves);
            if let Some((pane_id, _)) = leaves
                .into_iter()
                .find(|(_, leaf)| leaf.as_ref() == Some(surface))
            {
                return Some((session.id.clone(), tab.id.clone(), pane_id));
            }
        }
    }
    None
}

/// What the window says once Quit or Force quit of `name` was answered; nothing when
/// the process was signalled.
fn quit_note(name: &str, outcome: Result<QuitOutcome, String>) -> Option<String> {
    match outcome {
        Ok(QuitOutcome::Sent) => None,
        Ok(QuitOutcome::Gone) => Some(format!("{name} has already exited")),
        Ok(QuitOutcome::Denied) => Some(format!("Compi isn't allowed to quit {name}")),
        Ok(QuitOutcome::NoWindow) => Some(format!("{name} has no window to close; use Force quit")),
        Ok(QuitOutcome::Refused { reason }) => Some(reason),
        Err(error) => Some(format!("Couldn't quit {name}: {error}")),
    }
}

const fn state_label(state: ProcessState) -> &'static str {
    match state {
        ProcessState::Running => "Running",
        ProcessState::Sleeping => "Sleeping",
        ProcessState::Waiting => "Waiting (disk)",
        ProcessState::Stopped => "Stopped",
        ProcessState::Zombie => "Zombie",
        ProcessState::Idle => "Idle",
        ProcessState::Other => "Other",
    }
}

const fn owner_label(owner: ProcessOwner) -> &'static str {
    match owner {
        ProcessOwner::CurrentUser => "You",
        ProcessOwner::System => "System",
        ProcessOwner::Other => "Another user",
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

impl CompiApp {
    /// Before drawing: each pane of `shown` whose inspector shows a process asks the
    /// daemon about it when it is new, and again every [`INSPECT_EVERY`].
    pub(super) fn request_inspections(&mut self, shown: &[PaneId], cx: &mut Context<Self>) {
        for pane_id in shown {
            let Some(pane) = self.monitor.panes.get_mut(pane_id) else {
                continue;
            };
            let key = (pane.page == MonitorPage::Processes && pane.list.inspecting())
                .then(|| pane.list.selected_process())
                .flatten();
            let Some(key) = key else {
                pane.list.inspection = None;
                continue;
            };
            let ask = match &mut pane.list.inspection {
                Some(inspection) if inspection.key == key => {
                    let due = !inspection.asking
                        && !matches!(inspection.answer, InspectAnswer::Exited)
                        && inspection.asked.elapsed() >= INSPECT_EVERY;
                    if due {
                        inspection.asking = true;
                        inspection.asked = Instant::now();
                    }
                    due
                }
                inspection => {
                    *inspection = Some(Inspection {
                        key,
                        answer: InspectAnswer::Reading,
                        asked: Instant::now(),
                        asking: true,
                    });
                    true
                }
            };
            if ask {
                let source = pane.source.clone();
                self.inspect_process(pane_id.clone(), source, key, cx);
            }
        }
    }

    /// Asks the daemon about process `key` of `source` on a thread, for pane `pane_id`.
    fn inspect_process(
        &self,
        pane_id: PaneId,
        source: MonitorSource,
        key: ProcessKey,
        cx: &mut Context<Self>,
    ) {
        let target = self.target.clone();
        let asked = source.clone();
        cx.spawn(async move |this, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move {
                    (|| -> crate::Result<_> {
                        target.connect()?.inspect_process(asked, key.0, key.1)
                    })()
                    .map_err(|error| error.to_string())
                })
                .await;
            this.update(cx, |this, cx| {
                let Some(inspection) = this
                    .monitor
                    .panes
                    .get_mut(&pane_id)
                    .filter(|pane| pane.source == source)
                    .and_then(|pane| pane.list.inspection.as_mut())
                    .filter(|inspection| inspection.key == key)
                else {
                    return;
                };
                inspection.asking = false;
                inspection.asked = Instant::now();
                inspection.answer = match answer {
                    Ok(Some(read)) => InspectAnswer::Read(read),
                    Ok(None) => InspectAnswer::Exited,
                    Err(error) => InspectAnswer::Failed(error),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Quit… or (`force`) Force quit… on process `key` of pane `pane_id`'s source:
    /// asks first, with Cancel focused.
    pub(super) fn confirm_quit_process(&mut self, pane_id: &PaneId, key: ProcessKey, force: bool) {
        let Some(pane) = self.monitor.panes.get(pane_id) else {
            return;
        };
        let Some((_, process)) = self.monitor.feeds(&pane.source).processes.find(&key) else {
            self.global_error = Some("That process has already exited".into());
            return;
        };
        let overlay = Overlay::ConfirmQuitProcess {
            source: pane.source.clone(),
            pid: key.0,
            start_time: key.1,
            name: listing::display_line(&process.details.name),
            force,
            in_terminal: process.details.surface.is_some(),
        };
        self.open_overlay(overlay, "");
    }

    /// Quits (or, `force`, ends) process `key` of `source`, named `name`, on a thread;
    /// what came of it shows as the window's note unless it was signalled.
    pub(in crate::gui::workspace) fn quit_process(
        &mut self,
        source: MonitorSource,
        key: ProcessKey,
        name: String,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        let target = self.target.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    (|| -> crate::Result<_> {
                        target.connect()?.quit_process(source, key.0, key.1, force)
                    })()
                    .map_err(|error| error.to_string())
                })
                .await;
            this.update(cx, |this, cx| {
                if let Some(note) = quit_note(&name, outcome) {
                    this.global_error = Some(note);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Open terminal: shows the pane of terminal `surface` (its tab, and its shell
    /// where it shows a view) and focuses it.
    pub(super) fn open_process_terminal(
        &mut self,
        surface: &SurfaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((session, tab, pane_id)) = self
            .workspace
            .as_ref()
            .and_then(|workspace| surface_pane(workspace, surface))
        else {
            self.global_error = Some("That terminal is no longer open".into());
            return;
        };
        if self.selected_tab().map(|selected| &selected.id) != Some(&tab) {
            if let Some(workspace) = &self.workspace {
                self.state.select_session(workspace, &session);
            }
            let hidden = self.state.hidden_tabs.contains(&tab);
            self.select_terminal(tab, hidden);
        }
        let shows_view = self.workspace.as_ref().is_some_and(|workspace| {
            content::find_leaf(workspace, &pane_id)
                .and_then(LayoutNode::view)
                .is_some_and(|view| view.side == PaneSide::View)
        });
        if shows_view {
            self.set_pane_side(&pane_id, PaneSide::Terminal);
        }
        self.focus_pane(pane_id);
        window.focus(&self.focus_handle, cx);
        self.rebuild_layout(window, true);
        cx.notify();
    }

    /// Right-click on a row: it is selected, then Inspect, and for a process Quit…,
    /// Force quit…, Open terminal, Copy PID and Copy command; then the pane's actions.
    pub(super) fn open_process_row_menu(
        &mut self,
        pane_id: &PaneId,
        key: RowKey,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.monitor.panes.get_mut(pane_id) else {
            return;
        };
        pane.list.selected = Some(key.clone());
        window.focus(&pane.focus, cx);
        let inspect_pane = pane_id.clone();
        let mut entries = vec![
            controls::menu_item(
                "Inspect",
                cx.listener(move |this, _, _, cx| {
                    this.open_process_detail(&inspect_pane);
                    cx.notify();
                }),
            )
            .into(),
        ];
        let process = match &key {
            RowKey::Process(process) => self
                .monitor
                .panes
                .get(pane_id)
                .and_then(|pane| self.monitor.feeds(&pane.source).processes.find(process)),
            RowKey::Group(_) => None,
        };
        if let Some((_, process)) = process {
            let process_key = listing::key(process);
            let (quit_pane, force_pane) = (pane_id.clone(), pane_id.clone());
            let surface = process.details.surface.clone();
            let pid = process.sample.pid.to_string();
            let command = process.details.command.clone();
            entries.push(controls::MenuEntry::Separator);
            entries.push(
                controls::menu_item(
                    "Quit…",
                    cx.listener(move |this, _, _, cx| {
                        this.confirm_quit_process(&quit_pane, process_key, false);
                        cx.notify();
                    }),
                )
                .into(),
            );
            entries.push(
                controls::menu_item(
                    "Force quit…",
                    cx.listener(move |this, _, _, cx| {
                        this.confirm_quit_process(&force_pane, process_key, true);
                        cx.notify();
                    }),
                )
                .into(),
            );
            entries.push(
                controls::menu_item(
                    "Open terminal",
                    cx.listener({
                        let surface = surface.clone();
                        move |this, _, window, cx| {
                            if let Some(surface) = &surface {
                                this.open_process_terminal(surface, window, cx);
                            }
                        }
                    }),
                )
                .disabled_because(
                    surface
                        .is_none()
                        .then_some("It doesn't run in a Compi terminal"),
                )
                .into(),
            );
            entries.push(controls::MenuEntry::Separator);
            entries.push(
                controls::menu_item("Copy PID", move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(pid.clone()));
                })
                .into(),
            );
            entries.push(
                controls::menu_item("Copy command", {
                    let command = command.clone();
                    move |_, _, cx| {
                        if let Some(command) = &command {
                            cx.write_to_clipboard(ClipboardItem::new_string(command.clone()));
                        }
                    }
                })
                .disabled_because(
                    command
                        .is_none()
                        .then_some("This host doesn't give its command"),
                )
                .into(),
            );
        }
        entries.push(controls::MenuEntry::Separator);
        entries.push(self.pane_submenu(pane_id, cx));
        self.open_menu(position, Anchor::TopLeft, entries, window, cx);
    }

    /// A narrow pane's page of the selection: Back, then its inspector.
    pub(super) fn render_process_detail_page(
        &self,
        pane_id: &PaneId,
        pane: &MonitorPane,
        feeds: &SourceFeeds,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let back_pane = pane_id.clone();
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_4()
                    .py_2()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(color(colors.border))
                    .child(
                        controls::text_button("monitor-detail-back", "Back")
                            .icon(IconName::ChevronLeft)
                            .ghost()
                            .key("escape")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(pane) = this.monitor.panes.get_mut(&back_pane) {
                                    pane.list.detail = false;
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(self.render_inspector(pane_id, pane, feeds, cx))
            .into_any_element()
    }

    /// The selected row's inspector, scrolling: a process's, or an app's.
    pub(super) fn render_inspector(
        &self,
        pane_id: &PaneId,
        pane: &MonitorPane,
        feeds: &SourceFeeds,
        cx: &Context<Self>,
    ) -> AnyElement {
        let list = &pane.list;
        let content = match &list.selected {
            Some(RowKey::Process(key)) => match feeds.processes.find(key) {
                Some((at, process)) => {
                    self.render_process_inspector(pane_id, pane, feeds, at, process, cx)
                }
                None => self.monitor_message("This process has exited", None),
            },
            Some(RowKey::Group(id)) => list
                .table
                .groups
                .iter()
                .find(|group| group.id == *id)
                .map_or_else(
                    || div().into_any_element(),
                    |group| self.render_group_inspector(pane_id, group, feeds, cx),
                ),
            None => div().into_any_element(),
        };
        div()
            .id("monitor-inspector")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .child(content)
            .into_any_element()
    }

    /// A labelled fact of the inspector; `value` wraps.
    fn inspector_fact(&self, label: &'static str, value: impl IntoElement) -> Div {
        let colors = *self.colors();
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .text_color(color(modal_text_color(colors.muted, &colors)))
                    .child(label),
            )
            .child(div().min_w_0().child(value))
    }

    /// The icon and name heading an inspector, `detail` muted under the name.
    fn inspector_title(&self, app: Option<(&str, &str)>, name: String, detail: String) -> Div {
        let colors = *self.colors();
        div()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(self.app_icon(
                app.map(|(id, _)| id),
                app.map_or(name.as_str(), |(_, app)| app),
                INSPECTOR_ICON,
            ))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(UI_BODY_TEXT_SIZE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name),
                    )
                    .child(
                        div()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(color(modal_text_color(colors.muted, &colors)))
                            .child(detail),
                    ),
            )
    }

    /// A row naming `process` that selects it.
    fn process_link(
        &self,
        pane_id: &PaneId,
        id: (&'static str, usize),
        process: &Process,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let key = listing::key(process);
        let pane_id = pane_id.clone();
        let app = process.details.app_group.as_ref();
        div()
            .id(id)
            .min_w_0()
            .px_1()
            .py_0p5()
            .flex()
            .items_center()
            .gap_2()
            .rounded_sm()
            .cursor_pointer()
            .hover(move |style| style.bg(color(colors.surface_hover)))
            .child(self.app_icon(
                app.map(|app| app.id.as_str()),
                app.map_or(process.details.name.as_str(), |app| app.name.as_str()),
                ROW_ICON,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(listing::display_line(&process.details.name)),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .text_color(color(modal_text_color(colors.muted, &colors)))
                    .child(process.sample.pid.to_string()),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.select_process(&pane_id, key, window, cx);
                cx.notify();
            }))
            .into_any_element()
    }

    /// Up to [`LISTED_PROCESSES`] of `indices` as links, the rest counted.
    fn process_links(
        &self,
        pane_id: &PaneId,
        id: &'static str,
        processes: &[Process],
        indices: &[usize],
        cx: &Context<Self>,
    ) -> Div {
        let colors = *self.colors();
        let rest = indices.len().saturating_sub(LISTED_PROCESSES);
        div()
            .flex()
            .flex_col()
            .children(
                indices
                    .iter()
                    .take(LISTED_PROCESSES)
                    .enumerate()
                    .map(|(index, &at)| {
                        self.process_link(pane_id, (id, index), &processes[at], cx)
                    }),
            )
            .when(rest > 0, |links| {
                links.child(
                    div()
                        .px_1()
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .text_color(color(modal_text_color(colors.muted, &colors)))
                        .child(format!("and {rest} more")),
                )
            })
    }

    /// The last 5 minutes of `values` as a graph in `metric`'s colour, under `label`
    /// and the newest value.
    fn inspector_graph(
        &self,
        metric: Metric,
        label: &'static str,
        now: Option<String>,
        values: Vec<Option<f64>>,
        scale: f64,
    ) -> Div {
        let colors = *self.colors();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .text_color(color(modal_text_color(colors.muted, &colors)))
                    .child(label)
                    .child(now.unwrap_or_else(|| UNAVAILABLE.to_owned())),
            )
            .child(dot_graph(
                self.strip(metric, values, None, Combine::Mean, scale, 1.0),
                GRAPH_HEIGHT,
            ))
    }

    /// A process's inspector: who and what it is, Quit, Force quit and Open terminal,
    /// its recent CPU and memory, then its facts, parent and children.
    fn render_process_inspector(
        &self,
        pane_id: &PaneId,
        pane: &MonitorPane,
        feeds: &SourceFeeds,
        at: usize,
        process: &Process,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let feed = &feeds.processes;
        let details = &process.details;
        let sample = &process.sample;
        let key = listing::key(process);
        let name = listing::display_line(&details.name);
        let answer = pane
            .list
            .inspection
            .as_ref()
            .filter(|inspection| inspection.key == key)
            .map(|inspection| &inspection.answer);
        let read = match answer {
            Some(InspectAnswer::Read(read)) => Some(read),
            _ => None,
        };
        let exited = matches!(answer, Some(InspectAnswer::Exited));
        let unavailable = || UNAVAILABLE.to_owned();
        let app = details
            .app_group
            .as_ref()
            .map(|app| (app.id.as_str(), app.name.as_str()));
        let (quit_pane, force_pane) = (pane_id.clone(), pane_id.clone());
        let surface = details.surface.clone();
        let actions = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(
                controls::text_button("monitor-quit", "Quit…")
                    .disabled_because(exited.then_some("It has exited"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm_quit_process(&quit_pane, key, false);
                        cx.notify();
                    })),
            )
            .child(
                controls::text_button("monitor-force-quit", "Force quit…")
                    .danger()
                    .disabled_because(exited.then_some("It has exited"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm_quit_process(&force_pane, key, true);
                        cx.notify();
                    })),
            )
            .children(surface.map(|surface| {
                controls::text_button("monitor-open-terminal", "Open terminal")
                    .icon(IconName::SquareTerminal)
                    .detail("It runs in a Compi terminal")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_process_terminal(&surface, window, cx);
                    }))
            }));
        let ring = feeds.history.ring(&key);
        let cpu: Vec<Option<f64>> = ring.map_or_else(Vec::new, |ring| {
            ring.cpu()
                .map(|cpu| listing::cpu_share(cpu, feed.logical_cpus).map(f64::from))
                .collect()
        });
        let memory: Vec<Option<f64>> = ring.map_or_else(Vec::new, |ring| {
            ring.memory()
                .map(|bytes| bytes.map(|bytes| bytes as f64))
                .collect()
        });
        let memory_scale = totals::rate_scale(&memory, MEMORY_SCALE_FLOOR);
        let graphs = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.inspector_graph(
                Metric::Cpu,
                "CPU",
                listing::cpu_share(sample.cpu_percent, feed.logical_cpus).map(listing::format_cpu),
                cpu,
                100.0,
            ))
            .child(self.inspector_graph(
                Metric::Memory,
                "Memory",
                sample.memory_bytes.map(listing::format_bytes),
                memory,
                memory_scale,
            ));
        let state = match answer {
            Some(InspectAnswer::Read(read)) => read
                .state
                .map_or_else(unavailable, |state| state_label(state).to_owned()),
            Some(InspectAnswer::Exited) => "Exited".to_owned(),
            Some(InspectAnswer::Failed(error)) => format!("Couldn't read it: {error}"),
            Some(InspectAnswer::Reading) | None => "Reading…".to_owned(),
        };
        let started = format!(
            "{} · {}",
            listing::format_local_time(sample.start_time),
            listing::format_running_for(sample.start_time, unix_now())
        );
        let arguments = read.and_then(|read| read.arguments.as_ref());
        let cut = read.is_some_and(|read| read.arguments_cut);
        let copied = arguments
            .map(|arguments| arguments.join(" "))
            .or_else(|| details.command.clone());
        let argument_lines: Vec<String> = match arguments {
            Some(arguments) => arguments
                .iter()
                .map(|argument| listing::display_line(argument))
                .collect(),
            None => details
                .command
                .as_deref()
                .map(listing::display_line)
                .into_iter()
                .collect(),
        };
        let arguments_fact = self.inspector_fact(
            "Arguments",
            div()
                .flex()
                .flex_col()
                .gap_1()
                .when(argument_lines.is_empty(), |block| {
                    block.text_color(muted).child(unavailable())
                })
                .children(argument_lines.into_iter().map(|line| div().child(line)))
                .when(cut, |block| {
                    block.child(
                        div()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(muted)
                            .child("Cut: the arguments run past 64 KB"),
                    )
                })
                .children(copied.map(|copied| {
                    div().child(
                        controls::text_button("monitor-copy-arguments", "Copy")
                            .icon(IconName::Copy)
                            .ghost()
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
                            }),
                    )
                })),
        );
        let parent = listing::parent(&feed.processes, at);
        let children = listing::children(&feed.processes, at);
        let text = |value: Option<String>| -> AnyElement {
            match value {
                Some(value) => div().child(value).into_any_element(),
                None => div()
                    .text_color(muted)
                    .child(unavailable())
                    .into_any_element(),
            }
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.inspector_title(app, name, format!("PID {}", sample.pid)))
            .child(actions)
            .child(graphs)
            .child(self.inspector_fact("Started", started))
            .child(self.inspector_fact("State", state))
            .child(self.inspector_fact(
                "User",
                text(details.user.as_deref().map(listing::display_line)),
            ))
            .child(self.inspector_fact("Owner", owner_label(details.owner)))
            .when_some(details.app_group.as_ref(), |facts, app| {
                facts.child(self.inspector_fact("App", listing::display_line(&app.name)))
            })
            .child(
                self.inspector_fact(
                    "Working directory",
                    text(
                        read.and_then(|read| read.working_directory.as_deref())
                            .map(listing::display_line),
                    ),
                ),
            )
            .child(self.inspector_fact(
                "Executable",
                text(details.executable.as_deref().map(listing::display_line)),
            ))
            .child(arguments_fact)
            .when(details.restricted, |facts| {
                facts.child(
                    div()
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .text_color(muted)
                        .child("Owned by another user; this host withholds its details"),
                )
            })
            .child(self.inspector_fact(
                "Parent",
                match parent {
                    Some(parent) => self.process_link(
                        pane_id,
                        ("monitor-parent", 0),
                        &feed.processes[parent],
                        cx,
                    ),
                    None => text(sample.parent_pid.map(|pid| format!("PID {pid}"))),
                },
            ))
            .when(!children.is_empty(), |facts| {
                facts.child(self.inspector_fact(
                    "Children",
                    self.process_links(pane_id, "monitor-child", &feed.processes, &children, cx),
                ))
            })
            .into_any_element()
    }

    /// An app's inspector: its sums and its processes shown.
    fn render_group_inspector(
        &self,
        pane_id: &PaneId,
        group: &Group,
        feeds: &SourceFeeds,
        cx: &Context<Self>,
    ) -> AnyElement {
        let feed = &feeds.processes;
        let value = |value: Option<String>| value.unwrap_or_else(|| UNAVAILABLE.to_owned());
        let count = group.members.len();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.inspector_title(
                Some((group.id.as_str(), group.name.as_str())),
                listing::display_line(&group.name),
                format!(
                    "{count} {}",
                    if count == 1 { "process" } else { "processes" }
                ),
            ))
            .child(
                self.inspector_fact(
                    "CPU",
                    value(
                        listing::cpu_share(group.cpu_percent, feed.logical_cpus)
                            .map(listing::format_cpu),
                    ),
                ),
            )
            .child(self.inspector_fact(
                "Memory",
                value(group.memory_bytes.map(listing::format_bytes)),
            ))
            .child(self.inspector_fact(
                "Threads",
                value(group.threads.map(|threads| threads.to_string())),
            ))
            .child(self.inspector_fact(
                "CPU time",
                value(group.cpu_time_ms.map(listing::format_cpu_time)),
            ))
            .child(self.inspector_fact(
                "User",
                match &group.user {
                    crate::monitor::apps::GroupUser::Shared(user) => listing::display_line(user),
                    crate::monitor::apps::GroupUser::Mixed => "Several".to_owned(),
                },
            ))
            .child(self.inspector_fact(
                "Processes",
                self.process_links(
                    pane_id,
                    "monitor-member",
                    &feed.processes,
                    &group.members,
                    cx,
                ),
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::{ServerGeneration, ServerId, SplitAxis, WorkspaceSession, WorkspaceTab};

    #[test]
    fn open_terminal_finds_the_pane_of_the_surface_in_any_tab() {
        let tab = |id: &str, layout| WorkspaceTab {
            id: TabId::new(id),
            label: id.into(),
            layout,
            previous_layout: None,
            merge: None,
        };
        let workspace = WorkspaceSnapshot {
            server_id: ServerId::new("server"),
            server_generation: ServerGeneration::new("generation"),
            revision: 1,
            initialized: true,
            sessions: vec![
                WorkspaceSession {
                    id: SessionId::new("one"),
                    label: "one".into(),
                    tabs: vec![tab(
                        "a",
                        LayoutNode::terminal(PaneId::new("pane-a"), SurfaceId::new("surface-a")),
                    )],
                },
                WorkspaceSession {
                    id: SessionId::new("two"),
                    label: "two".into(),
                    tabs: vec![tab(
                        "b",
                        LayoutNode::Split {
                            axis: SplitAxis::Horizontal,
                            ratio: 0.5,
                            first: Box::new(LayoutNode::terminal(
                                PaneId::new("pane-b"),
                                SurfaceId::new("surface-b"),
                            )),
                            second: Box::new(LayoutNode::terminal(
                                PaneId::new("pane-c"),
                                SurfaceId::new("surface-c"),
                            )),
                        },
                    )],
                },
            ],
            surfaces: vec![],
            recovery_message: None,
        };
        assert_eq!(
            surface_pane(&workspace, &SurfaceId::new("surface-c")),
            Some((
                SessionId::new("two"),
                TabId::new("b"),
                PaneId::new("pane-c")
            ))
        );
        assert_eq!(surface_pane(&workspace, &SurfaceId::new("gone")), None);
    }

    #[test]
    fn quit_outcomes_read_as_notes_and_a_sent_one_says_nothing() {
        assert_eq!(quit_note("vim", Ok(QuitOutcome::Sent)), None);
        assert_eq!(
            quit_note("vim", Ok(QuitOutcome::NoWindow)).as_deref(),
            Some("vim has no window to close; use Force quit")
        );
        assert_eq!(
            quit_note(
                "launchd",
                Ok(QuitOutcome::Refused {
                    reason: "Compi won't quit PID 1".into()
                })
            )
            .as_deref(),
            Some("Compi won't quit PID 1")
        );
    }
}
