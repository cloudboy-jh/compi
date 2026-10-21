//! Monitor's resource pages (CPU, Memory, GPU, Disk, Network): the metric's figures,
//! large, in an equal grid; its history over the range; then what the newest sample
//! breaks down, as it is now: CPU's per-core bars, Memory by type, Disk's volumes and
//! Network's interfaces. GPU shows its memory as a bar under the figures, and says so
//! where the daemon reads no GPU.
//!
//! The tables (volumes, interfaces) give up columns when narrow the way Processes does
//! (`crate::monitor::detail`); a value the host does not give reads as unavailable.
use super::overview::{
    CARD_INSET, DISK_RATE_FLOOR, Figure, NETWORK_RATE_FLOOR, Strip, card, dot_graph, figure,
    span_caption, totals_display,
};
use super::*;
use crate::gui::workspace::settings::settings_card;
use crate::monitor::detail::{self, InterfaceColumn, VolumeColumn};
use crate::monitor::totals::{self, Combine};
use crate::monitor::{
    display_line, format_bytes, format_percent, format_rate, format_used_of, format_whole_percent,
};
use compi_protocol::system_metrics::{MemoryPressure, NetworkInterface, TotalsDetail, Volume};

/// A row of the interfaces table; the volumes' rows are taller, figures over a bar.
const ROW_HEIGHT: f32 = 24.0;
const VOLUME_ROW_HEIGHT: f32 = 38.0;
/// The thickness of a usage bar: a core's, a volume's, the GPU's memory.
const BAR_HEIGHT: f32 = 6.0;
/// The number and the value beside each per-core bar.
const CORE_LABEL_WIDTH: f32 = 20.0;
const CORE_VALUE_WIDTH: f32 = 36.0;
/// What the GPU page says where the daemon reads no GPU.
const GPU_UNAVAILABLE_NOTE: &str =
    "Compi reads GPU use on macOS and Windows. This host has no GPU it can read.";

/// What a resource page shows, before it is drawn.
struct ResourcePage {
    /// The first line's bold label, and the muted caption after it.
    label: String,
    caption: Option<String>,
    figures: Vec<Figure>,
    /// Under the figures, in their card: the GPU's memory bar.
    under_figures: Option<AnyElement>,
    graphs: Vec<(&'static str, Strip)>,
    /// A card after the history: the metric's breakdown.
    breakdown: Option<AnyElement>,
}

impl CompiApp {
    /// A resource page `width` wide of `pane`'s source: the metric's figures, its
    /// history over the range, then its breakdown.
    pub(super) fn render_monitor_resource(
        &self,
        pane_id: &PaneId,
        pane: &MonitorPane,
        metric: Metric,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let feed = &self.monitor.feeds(&pane.source).totals;
        if let Some(message) = self.totals_message(feed) {
            return message;
        }
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let (padding, content_width) = layout::page_frame(width);
        let gap = layout::gap(content_width);
        let inner = (content_width - 2.0 * CARD_INSET).max(1.0);
        let stale = feed.state == FeedState::Stale;
        let latest = feed.shown().latest();
        if metric == Metric::Gpu && latest.is_some_and(|latest| latest.gpu.is_none()) {
            return self.monitor_scroll_page(
                "monitor-resource",
                padding,
                content_width,
                stale,
                div().child(self.render_gpu_unavailable()),
            );
        }
        let data = card(metric, latest, feed.host.as_ref());
        let page = match metric {
            Metric::Cpu => self.cpu_page(feed, data.subtitle, inner, gap),
            Metric::Memory => self.memory_page(feed, data.subtitle, pane_id, content_width, cx),
            Metric::Gpu => self.gpu_page(feed, data.figures),
            Metric::Disk => self.disk_page(feed, data.subtitle, data.figures, inner),
            Metric::Network => self.network_page(feed, data.subtitle, data.figures, inner),
        };
        let figure_columns = layout::figure_columns(inner, page.figures.len());
        let figures_card = settings_card(&colors)
            .p_4()
            .gap_3()
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_none().child(controls::glyph(
                        metric.icon(),
                        color(self.metric_color(metric)),
                    )))
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(page.label),
                    )
                    .when_some(page.caption, |line, caption| {
                        line.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(UI_MICRO_TEXT_SIZE))
                                .text_color(muted)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(caption),
                        )
                    }),
            )
            .child(self.big_reading(data.value, layout::resource_value_size(content_width)))
            .child(self.figure_grid(&page.figures, figure_columns, inner, gap, 18.0))
            .children(page.under_figures);
        let graph_height = layout::graph_height(content_width);
        let graphs_card = self
            .detail_card("History", Some(span_caption(self.monitor.span).to_owned()))
            .gap_4()
            .children(page.graphs.into_iter().map(|(label, strip)| {
                let legend = if strip.under.is_some() {
                    vec![("User", strip.color), ("System", strip.under_color)]
                } else {
                    vec![(label, strip.color)]
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(muted)
                            .children(legend.into_iter().map(|(label, tint)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(div().size(px(8.0)).rounded_full().bg(tint))
                                    .child(label)
                            })),
                    )
                    .child(dot_graph(strip, graph_height))
            }));
        self.monitor_scroll_page(
            "monitor-resource",
            padding,
            content_width,
            stale,
            div()
                .flex()
                .flex_col()
                .gap(px(gap))
                .child(figures_card)
                .child(graphs_card)
                .children(page.breakdown),
        )
    }

    /// CPU: in use, user, system and the logical CPUs, the processor as caption; user
    /// over system; each logical CPU's bar.
    fn cpu_page(&self, feed: &TotalsFeed, label: String, inner: f32, gap: f32) -> ResourcePage {
        let history = feed.shown();
        let latest = history.latest();
        let samples = self.monitor.span.samples();
        let cpu = latest.and_then(|sample| sample.cpu);
        let host = feed.host.as_ref();
        let total = history.window(samples, totals::cpu_used);
        let system = history.window(samples, |sample| {
            sample.cpu.map(|cpu| f64::from(cpu.system))
        });
        ResourcePage {
            label,
            caption: host
                .and_then(|host| host.cpu.as_deref())
                .map(totals_display),
            figures: vec![
                figure(
                    "In use",
                    latest
                        .and_then(totals::cpu_used)
                        .map(|used| format_percent(used as f32)),
                ),
                figure("User", cpu.map(|cpu| format_percent(cpu.user))),
                figure("System", cpu.map(|cpu| format_percent(cpu.system))),
                figure(
                    "Logical CPUs",
                    host.map(|host| host.logical_cpus)
                        .filter(|cpus| *cpus > 0)
                        .map(|cpus| cpus.to_string()),
                ),
            ],
            under_figures: None,
            graphs: vec![(
                "User over system",
                self.strip(Metric::Cpu, total, Some(system), Combine::Mean, 100.0, 0.55),
            )],
            breakdown: Some(self.render_cores(history.detail(), inner, gap)),
        }
    }

    /// Memory: in use, available, total, swap and pressure; in use over time; Memory by
    /// type, where the host divides its memory into kinds.
    fn memory_page(
        &self,
        feed: &TotalsFeed,
        label: String,
        pane_id: &PaneId,
        content_width: f32,
        cx: &Context<Self>,
    ) -> ResourcePage {
        let history = feed.shown();
        let memory = history.latest().and_then(|sample| sample.memory.as_ref());
        let used = history.window(self.monitor.span.samples(), |sample| {
            sample.memory.as_ref().and_then(totals::memory_used_percent)
        });
        ResourcePage {
            label,
            caption: None,
            figures: vec![
                figure(
                    "In use",
                    memory.map(|memory| format_bytes(memory.used_bytes)),
                ),
                figure(
                    "Available",
                    memory
                        .and_then(|memory| memory.available_bytes)
                        .map(format_bytes),
                ),
                figure(
                    "Total",
                    memory.map(|memory| format_bytes(memory.total_bytes)),
                ),
                figure(
                    "Swap used",
                    memory.and_then(|memory| {
                        format_used_of(memory.swap_used_bytes, memory.swap_total_bytes)
                    }),
                ),
                figure(
                    "Pressure",
                    memory.and_then(|memory| memory.pressure).map(|pressure| {
                        match pressure {
                            MemoryPressure::Normal => "Normal",
                            MemoryPressure::Elevated => "Elevated",
                            MemoryPressure::Critical => "Critical",
                        }
                        .to_owned()
                    }),
                ),
            ],
            under_figures: None,
            graphs: vec![(
                "In use",
                self.strip(Metric::Memory, used, None, Combine::Mean, 100.0, 1.0),
            )],
            breakdown: memory
                .filter(|memory| !memory.kinds.is_empty())
                .map(|memory| {
                    self.render_memory_by_type(
                        &OverviewHost::Pane(pane_id.clone()),
                        memory,
                        content_width,
                        false,
                        cx,
                    )
                }),
        }
    }

    /// GPU: utilization with the GPU's name as caption, its memory as figures and a
    /// bar; utilization over time.
    fn gpu_page(&self, feed: &TotalsFeed, figures: [Figure; 3]) -> ResourcePage {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let history = feed.shown();
        let gpu = history.latest().and_then(|sample| sample.gpu.as_ref());
        let utilization = history.window(self.monitor.span.samples(), totals::gpu_utilization);
        let memory = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .gap_2()
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .text_color(muted)
                    .child(div().flex_1().min_w_0().child("Memory"))
                    .child(cell_text(
                        gpu.and_then(|gpu| {
                            format_used_of(gpu.memory_used_bytes, gpu.memory_total_bytes)
                        })
                        .unwrap_or_else(|| UNAVAILABLE.to_owned()),
                    )),
            )
            .child(
                self.usage_bar(
                    Metric::Gpu,
                    gpu.and_then(totals::gpu_memory_percent)
                        .map(|used| used / 100.0),
                ),
            );
        ResourcePage {
            label: "Utilization".to_owned(),
            caption: gpu.and_then(|gpu| gpu.name.as_deref()).map(totals_display),
            figures: figures.into(),
            under_figures: Some(memory.into_any_element()),
            graphs: vec![(
                "Utilization",
                self.strip(Metric::Gpu, utilization, None, Combine::Mean, 100.0, 1.0),
            )],
            breakdown: None,
        }
    }

    /// Disk: the system disk's free and total space, rates and bytes written, its name
    /// as caption; reading and writing over time; the volumes.
    fn disk_page(
        &self,
        feed: &TotalsFeed,
        label: String,
        rates: [Figure; 3],
        inner: f32,
    ) -> ResourcePage {
        let history = feed.shown();
        let samples = self.monitor.span.samples();
        let disk = history.latest().and_then(|sample| sample.disk.as_ref());
        let read = history.window(samples, |sample| {
            sample
                .disk
                .as_ref()
                .and_then(|disk| disk.read_bytes_per_second)
                .map(|rate| rate as f64)
        });
        let written = history.window(samples, |sample| {
            sample
                .disk
                .as_ref()
                .and_then(|disk| disk.written_bytes_per_second)
                .map(|rate| rate as f64)
        });
        // Both graphs share one scale, so their heights compare.
        let scale = totals::rate_scale(&read, DISK_RATE_FLOOR)
            .max(totals::rate_scale(&written, DISK_RATE_FLOOR));
        let mut figures = vec![
            figure("Free", disk.map(|disk| format_bytes(disk.available_bytes))),
            figure("Total", disk.map(|disk| format_bytes(disk.total_bytes))),
        ];
        figures.extend(rates);
        ResourcePage {
            label,
            caption: disk.map(|disk| totals_display(&disk.name)),
            figures,
            under_figures: None,
            graphs: vec![
                (
                    "Reading",
                    self.strip(Metric::Disk, read, None, Combine::Max, scale, 1.0),
                ),
                (
                    "Writing",
                    self.strip(Metric::Disk, written, None, Combine::Max, scale, 0.55),
                ),
            ],
            breakdown: Some(self.render_volumes(history.detail(), inner)),
        }
    }

    /// Network: downloading, uploading and the totals; both rates over time; the
    /// interfaces.
    fn network_page(
        &self,
        feed: &TotalsFeed,
        label: String,
        rest: [Figure; 3],
        inner: f32,
    ) -> ResourcePage {
        let history = feed.shown();
        let samples = self.monitor.span.samples();
        let network = history.latest().and_then(|sample| sample.network);
        let received = history.window(samples, |sample| {
            sample
                .network
                .and_then(|network| network.received_bytes_per_second)
                .map(|rate| rate as f64)
        });
        let sent = history.window(samples, |sample| {
            sample
                .network
                .and_then(|network| network.sent_bytes_per_second)
                .map(|rate| rate as f64)
        });
        let scale = totals::rate_scale(&received, NETWORK_RATE_FLOOR)
            .max(totals::rate_scale(&sent, NETWORK_RATE_FLOOR));
        let mut figures = vec![figure(
            "Downloading",
            network
                .and_then(|network| network.received_bytes_per_second)
                .map(format_rate),
        )];
        figures.extend(rest);
        ResourcePage {
            label,
            caption: None,
            figures,
            under_figures: None,
            graphs: vec![
                (
                    "Downloading",
                    self.strip(Metric::Network, received, None, Combine::Max, scale, 1.0),
                ),
                (
                    "Uploading",
                    self.strip(Metric::Network, sent, None, Combine::Max, scale, 0.55),
                ),
            ],
            breakdown: Some(self.render_interfaces(history.detail(), inner)),
        }
    }

    /// The GPU page where the daemon reads no GPU: a card that says so.
    fn render_gpu_unavailable(&self) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        settings_card(&colors)
            .p_4()
            .gap_2()
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_none().child(controls::glyph(
                        Metric::Gpu.icon(),
                        color(self.metric_color(Metric::Gpu)),
                    )))
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Unavailable"),
                    ),
            )
            .child(
                div()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .text_color(muted)
                    .child(GPU_UNAVAILABLE_NOTE),
            )
            .into_any_element()
    }

    /// A card of a resource page: a bold `title` line with `detail` at its right end.
    fn detail_card(&self, title: &'static str, detail: Option<String>) -> Div {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        settings_card(&colors).p_4().gap_3().child(
            div()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .children(detail.map(|detail| {
                    div()
                        .flex_none()
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .text_color(muted)
                        .child(detail)
                })),
        )
    }

    /// A muted line in place of a breakdown the host does not give.
    fn detail_note(&self, note: &'static str) -> AnyElement {
        let colors = *self.colors();
        div()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .text_color(color(modal_text_color(colors.muted, &colors)))
            .child(note)
            .into_any_element()
    }

    /// A bar across its width filled to `share` (0 to 1) in `metric`'s colour; any
    /// share above zero shows. Without a share, only the faint track.
    fn usage_bar(&self, metric: Metric, share: Option<f64>) -> Div {
        let tint = color(self.metric_color(metric));
        div()
            .w_full()
            .h(px(BAR_HEIGHT))
            .rounded_full()
            .overflow_hidden()
            .bg(tint.opacity(0.14))
            .children(share.filter(|share| *share > 0.0).map(|share| {
                div()
                    .h_full()
                    .min_w(px(BAR_HEIGHT))
                    .w(gpui::relative(share.clamp(0.0, 1.0) as f32))
                    .rounded_full()
                    .bg(tint)
            }))
    }

    /// Each logical CPU's busy share now, in the host's order, as numbered bars in a
    /// grid `inner` wide.
    fn render_cores(&self, detail: Option<&TotalsDetail>, inner: f32, gap: f32) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let cores = detail.map_or(&[][..], |detail| detail.cores.as_slice());
        let body = if cores.is_empty() {
            self.detail_note("Unavailable on this host")
        } else {
            let columns = layout::core_columns(inner, cores.len(), gap);
            let width = layout::cell_width(inner, columns, gap);
            div()
                .flex()
                .flex_col()
                .gap_2()
                .text_size(px(UI_MICRO_TEXT_SIZE))
                .children(cores.chunks(columns).enumerate().map(|(row, chunk)| {
                    div()
                        .flex()
                        .gap(px(gap))
                        .children(chunk.iter().enumerate().map(|(index, &busy)| {
                            div()
                                .flex_none()
                                .w(px(width))
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_none()
                                        .w(px(CORE_LABEL_WIDTH))
                                        .text_right()
                                        .text_color(muted)
                                        .child((row * columns + index + 1).to_string()),
                                )
                                .child(div().flex_1().min_w_0().child(self.usage_bar(
                                    Metric::Cpu,
                                    Some(f64::from(busy.clamp(0.0, 100.0)) / 100.0),
                                )))
                                .child(
                                    div()
                                        .flex_none()
                                        .w(px(CORE_VALUE_WIDTH))
                                        .text_right()
                                        .child(format_whole_percent(busy)),
                                )
                        }))
                }))
                .into_any_element()
        };
        self.detail_card("Per core", Some("Now".to_owned()))
            .child(body)
            .into_any_element()
    }

    /// The column headings of a breakdown table: `(label, is name, numeric, width)`.
    fn table_header(&self, columns: impl Iterator<Item = (&'static str, bool, bool, f32)>) -> Div {
        let colors = *self.colors();
        div()
            .flex_none()
            .h(px(ROW_HEIGHT + 4.0))
            .flex()
            .items_center()
            .border_b_1()
            .border_color(color(colors.border))
            .text_size(px(UI_MICRO_TEXT_SIZE))
            .text_color(color(modal_text_color(colors.muted, &colors)))
            .children(columns.map(|(label, name, numeric, width)| {
                table_cell(name, numeric, width).child(cell_text(label.to_owned()))
            }))
    }

    /// The volumes the daemon lists, the system volume first, in a table `inner` wide.
    fn render_volumes(&self, detail: Option<&TotalsDetail>, inner: f32) -> AnyElement {
        let body = match detail.map(|detail| detail.volumes.as_slice()) {
            None => self.detail_note("Unavailable on this host"),
            Some([]) => self.detail_note("No volumes"),
            Some(volumes) => {
                let columns = detail::volume_columns(inner);
                div()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .child(self.table_header(columns.iter().map(|&column| {
                        (
                            column.label(),
                            column == VolumeColumn::Name,
                            column.numeric(),
                            column.width(),
                        )
                    })))
                    .children(
                        volumes
                            .iter()
                            .map(|volume| self.render_volume_row(volume, &columns)),
                    )
                    .into_any_element()
            }
        };
        self.detail_card("Volumes", None)
            .child(body)
            .into_any_element()
    }

    /// One volume: its name (marked when removable), where it is mounted, its file
    /// system, what is in use as figures over a bar, and what is free.
    fn render_volume_row(&self, volume: &Volume, columns: &[VolumeColumn]) -> Div {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let used = detail::volume_used(volume);
        div()
            .h(px(VOLUME_ROW_HEIGHT))
            .flex()
            .items_center()
            .children(columns.iter().map(|&column| {
                let cell = table_cell(
                    column == VolumeColumn::Name,
                    column.numeric(),
                    column.width(),
                );
                match column {
                    VolumeColumn::Name => {
                        cell.gap_2()
                            .child(cell_text(display_line(&volume.name)))
                            .when(volume.removable, |cell| {
                                cell.child(div().flex_none().child(controls::chip(
                                    "Removable",
                                    controls::ChipTone::Neutral,
                                )))
                            })
                    }
                    VolumeColumn::MountPoint => cell
                        .text_color(muted)
                        .child(cell_text(display_line(&volume.mount_point))),
                    VolumeColumn::FileSystem => cell.text_color(muted).child(cell_text(
                        volume
                            .file_system
                            .as_deref()
                            .map_or_else(|| UNAVAILABLE.to_owned(), display_line),
                    )),
                    VolumeColumn::Used => {
                        cell.child(
                            div()
                                .w_full()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    cell_text(
                                        format_used_of(Some(used), Some(volume.total_bytes))
                                            .unwrap_or_default(),
                                    )
                                    .text_size(px(UI_MICRO_TEXT_SIZE)),
                                )
                                .child(self.usage_bar(
                                    Metric::Disk,
                                    detail::share(used, volume.total_bytes),
                                )),
                        )
                    }
                    VolumeColumn::Available => {
                        cell.child(cell_text(format_bytes(volume.available_bytes)))
                    }
                }
            }))
    }

    /// The interfaces the daemon counts, the busiest now first, in a table `inner`
    /// wide.
    fn render_interfaces(&self, detail: Option<&TotalsDetail>, inner: f32) -> AnyElement {
        let body = match detail.map(|detail| detail.interfaces.as_slice()) {
            None => self.detail_note("Unavailable on this host"),
            Some([]) => self.detail_note("No interfaces"),
            Some(interfaces) => {
                let columns = detail::interface_columns(inner);
                div()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .child(self.table_header(columns.iter().map(|&column| {
                        (
                            column.label(),
                            column == InterfaceColumn::Name,
                            column.numeric(),
                            column.width(),
                        )
                    })))
                    .children(
                        detail::interface_order(interfaces)
                            .into_iter()
                            .map(|index| self.render_interface_row(&interfaces[index], &columns)),
                    )
                    .into_any_element()
            }
        };
        self.detail_card("Interfaces", None)
            .child(body)
            .into_any_element()
    }

    /// One interface: its name, its rates now and what it moved since its counters
    /// started.
    fn render_interface_row(
        &self,
        interface: &NetworkInterface,
        columns: &[InterfaceColumn],
    ) -> Div {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let rate = |rate: Option<u64>| match rate {
            Some(rate) => cell_text(format_rate(rate)),
            None => cell_text(UNAVAILABLE.to_owned()).text_color(muted),
        };
        div()
            .h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .children(columns.iter().map(|&column| {
                let cell = table_cell(
                    column == InterfaceColumn::Name,
                    column.numeric(),
                    column.width(),
                );
                match column {
                    InterfaceColumn::Name => cell.child(cell_text(display_line(&interface.name))),
                    InterfaceColumn::Down => cell.child(rate(interface.received_bytes_per_second)),
                    InterfaceColumn::Up => cell.child(rate(interface.sent_bytes_per_second)),
                    InterfaceColumn::Received => {
                        cell.child(cell_text(format_bytes(interface.received_bytes)))
                    }
                    InterfaceColumn::Sent => {
                        cell.child(cell_text(format_bytes(interface.sent_bytes)))
                    }
                }
            }))
    }
}

/// A breakdown table's cell: the name takes the room the others leave (and gives way
/// first when even that is too little); amounts are right-aligned.
fn table_cell(name: bool, numeric: bool, width: f32) -> Div {
    let cell = div()
        .h_full()
        .px_2()
        .flex()
        .items_center()
        .overflow_hidden()
        .whitespace_nowrap();
    let cell = if name {
        cell.flex_1().min_w_0()
    } else {
        cell.flex_none().w(px(width))
    };
    if numeric { cell.justify_end() } else { cell }
}

/// A cell's text, which shrinks with its cell and ends in an ellipsis when cut.
fn cell_text(text: String) -> Div {
    div()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(text)
}
