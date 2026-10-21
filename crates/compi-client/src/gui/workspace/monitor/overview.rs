//! Monitor's Overview, drawn from the window's totals history (and Settings → Monitor,
//! which draws its cards), and what the resource pages (`resources`) draw with it: the
//! metrics, their cards' figures, the dot graphs and Memory by type.
//!
//! Each metric has a colour from the theme's own ANSI palette (CPU blue, Memory green,
//! GPU magenta, Disk yellow, Network cyan), made legible on the pane, and uses it for
//! its nav icon, card icon, dots and bars. History graphs are dot matrices painted as
//! quads: one column per time bucket of the selected range, filled from the bottom.
//! Percentages (CPU, memory, GPU) take a bucket's mean, rates (disk, network) its peak,
//! scaled to the highest value in the range with a floor so an idle link does not fill
//! a column. A bucket with no sample (a gap, or before the history begins) draws no
//! dots at all.
use super::*;
use crate::gui::workspace::settings::settings_card;
use crate::monitor::apps::RowKey;
use crate::monitor::layout::{self, OverviewCard, OverviewLayout};
use crate::monitor::totals::{self, Combine, Span, TotalsHistory};
use crate::monitor::treemap::{self, Rect};
use crate::monitor::{
    bytes_parts, format_bytes, format_percent, format_rate, format_uptime, format_whole_percent,
    percent_parts, rate_parts,
};
use compi_protocol::system_metrics::{
    HostInfo, MemoryKindName, MemoryPressure, MemoryTotals, TotalsSample,
};
use gpui::{Bounds, PathBuilder, fill, point, size};

/// A dot of a history graph, and the gap between dots.
const DOT: f32 = 3.0;
const DOT_GAP: f32 = 2.0;
/// Rows of dots in a card's strip.
const CARD_DOT_ROWS: usize = 8;
/// Rates below these never fill a column: an idle disk or link stays low.
pub(super) const DISK_RATE_FLOOR: f64 = 1024.0 * 1024.0;
pub(super) const NETWORK_RATE_FLOOR: f64 = 128.0 * 1024.0;
/// A card's padding and border on each side (`p_4` and `border_1`).
pub(super) const CARD_INSET: f32 = 17.0;
/// Between a card's figures.
const CARD_FIGURE_GAP: f32 = 12.0;
/// Between the ring and the legend of Memory by type.
const RING_GAP: f32 = 24.0;
/// The card Settings → Monitor shows while the totals are not there.
const SETTINGS_MESSAGE_HEIGHT: f32 = 160.0;
/// The GPU card's subtitle where the daemon reads no GPU (Linux).
const GPU_UNAVAILABLE: &str = "Unavailable on this host";

/// The window's totals, shared by its Monitor panes.
#[derive(Default)]
pub(super) struct TotalsFeed {
    pub(super) slot: WatchSlot,
    pub(super) state: FeedState,
    pub(super) host: Option<HostInfo>,
    history: TotalsHistory,
    /// What Monitor showed when it was paused.
    frozen: Option<TotalsHistory>,
}

impl TotalsFeed {
    /// The history drawn: frozen while paused, else live.
    pub(super) fn shown(&self) -> &TotalsHistory {
        self.frozen.as_ref().unwrap_or(&self.history)
    }

    /// Forgets the history: what is not current is not shown.
    pub(super) fn reset(&mut self, state: FeedState) {
        *self = Self {
            slot: std::mem::take(&mut self.slot),
            state,
            ..Self::default()
        };
    }

    pub(super) fn freeze(&mut self, paused: bool) {
        self.frozen = paused.then(|| self.history.clone());
    }
}

/// The hardware Monitor reports, each with a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Metric {
    Cpu,
    Memory,
    Gpu,
    Disk,
    Network,
}

impl Metric {
    pub(super) const ALL: [Self; 5] = [
        Self::Cpu,
        Self::Memory,
        Self::Gpu,
        Self::Disk,
        Self::Network,
    ];

    pub(super) const fn page(self) -> MonitorPage {
        match self {
            Self::Cpu => MonitorPage::Cpu,
            Self::Memory => MonitorPage::Memory,
            Self::Gpu => MonitorPage::Gpu,
            Self::Disk => MonitorPage::Disk,
            Self::Network => MonitorPage::Network,
        }
    }

    /// The metric's card on Overview.
    pub(super) const fn card(self) -> OverviewCard {
        match self {
            Self::Cpu => OverviewCard::Cpu,
            Self::Memory => OverviewCard::Memory,
            Self::Gpu => OverviewCard::Gpu,
            Self::Disk => OverviewCard::Disk,
            Self::Network => OverviewCard::Network,
        }
    }

    /// The metric an Overview card shows; Memory by type and by app are not one.
    pub(super) const fn of_card(card: OverviewCard) -> Option<Self> {
        match card {
            OverviewCard::Cpu => Some(Self::Cpu),
            OverviewCard::Memory => Some(Self::Memory),
            OverviewCard::Gpu => Some(Self::Gpu),
            OverviewCard::Disk => Some(Self::Disk),
            OverviewCard::Network => Some(Self::Network),
            OverviewCard::MemoryByType | OverviewCard::MemoryByApp => None,
        }
    }

    pub(super) const fn of_page(page: MonitorPage) -> Option<Self> {
        match page {
            MonitorPage::Cpu => Some(Self::Cpu),
            MonitorPage::Memory => Some(Self::Memory),
            MonitorPage::Gpu => Some(Self::Gpu),
            MonitorPage::Disk => Some(Self::Disk),
            MonitorPage::Network => Some(Self::Network),
            MonitorPage::Overview | MonitorPage::Processes => None,
        }
    }

    pub(super) const fn title(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
            Self::Gpu => "GPU",
            Self::Disk => "Disk",
            Self::Network => "Network",
        }
    }

    pub(super) const fn icon(self) -> IconName {
        match self {
            Self::Cpu => IconName::Cpu,
            Self::Memory => IconName::MemoryStick,
            Self::Gpu => IconName::Gpu,
            Self::Disk => IconName::HardDrive,
            Self::Network => IconName::Network,
        }
    }

    /// The metric's colour in the theme's ANSI palette: blue, green, magenta, yellow,
    /// cyan.
    const fn palette_index(self) -> usize {
        match self {
            Self::Cpu => 4,
            Self::Memory => 2,
            Self::Gpu => 5,
            Self::Disk => 3,
            Self::Network => 6,
        }
    }

    /// The metric's value in `sample`, short enough for the nav.
    pub(super) fn nav_value(self, sample: &TotalsSample) -> Option<String> {
        match self {
            Self::Cpu => totals::cpu_used(sample).map(|cpu| format_whole_percent(cpu as f32)),
            Self::Memory => sample
                .memory
                .as_ref()
                .and_then(totals::memory_used_percent)
                .map(|used| format_whole_percent(used as f32)),
            Self::Gpu => {
                totals::gpu_utilization(sample).map(|busy| format_whole_percent(busy as f32))
            }
            Self::Disk => sample.disk.as_ref().and_then(disk_rate).map(format_rate),
            Self::Network => sample
                .network
                .and_then(|network| network.received_bytes_per_second)
                .map(format_rate),
        }
    }
}

/// Read and written together, where either is known.
fn disk_rate(disk: &compi_protocol::system_metrics::DiskTotals) -> Option<u64> {
    match (disk.read_bytes_per_second, disk.written_bytes_per_second) {
        (None, None) => None,
        (read, written) => Some(read.unwrap_or(0) + written.unwrap_or(0)),
    }
}

/// How the range reads in a graph's caption.
pub(super) const fn span_caption(span: Span) -> &'static str {
    match span {
        Span::Minute => "Last minute",
        Span::FiveMinutes => "Last 5 minutes",
        Span::FifteenMinutes => "Last 15 minutes",
        Span::Hour => "Last hour",
    }
}

/// A value with its unit, or unavailable.
pub(super) type Reading = Option<(String, &'static str)>;

/// One small labelled figure of a card or page.
pub(super) struct Figure {
    label: &'static str,
    value: Option<String>,
}

pub(super) fn figure(label: &'static str, value: Option<String>) -> Figure {
    Figure { label, value }
}

/// What a card shows besides its graph.
pub(super) struct Card {
    metric: Metric,
    pub(super) subtitle: String,
    pub(super) value: Reading,
    badge: Option<MemoryPressure>,
    pub(super) figures: [Figure; 3],
}

pub(super) fn card(metric: Metric, sample: Option<&TotalsSample>, host: Option<&HostInfo>) -> Card {
    match metric {
        Metric::Cpu => {
            let cpu = sample.and_then(|sample| sample.cpu);
            Card {
                metric,
                subtitle: "Now".into(),
                value: sample
                    .and_then(totals::cpu_used)
                    .map(|used| percent_parts(used as f32)),
                badge: None,
                figures: [
                    figure("User", cpu.map(|cpu| format_percent(cpu.user))),
                    figure("System", cpu.map(|cpu| format_percent(cpu.system))),
                    figure(
                        "Cores",
                        host.map(|host| host.logical_cpus)
                            .filter(|cores| *cores > 0)
                            .map(|cores| cores.to_string()),
                    ),
                ],
            }
        }
        Metric::Memory => {
            let memory = sample.and_then(|sample| sample.memory.as_ref());
            let [first, second, third] = memory.map_or(
                [
                    figure("App", None),
                    figure("Wired", None),
                    figure("Compressed", None),
                ],
                |memory| {
                    totals::memory_figures(memory)
                        .map(|shown| figure(shown.label, shown.bytes.map(format_bytes)))
                },
            );
            Card {
                metric,
                subtitle: memory.map_or_else(
                    || "In use".into(),
                    |memory| format!("In use of {}", format_bytes(memory.total_bytes)),
                ),
                value: memory.map(|memory| bytes_parts(memory.used_bytes)),
                badge: memory
                    .and_then(|memory| memory.pressure)
                    .filter(|pressure| *pressure != MemoryPressure::Normal),
                figures: [first, second, third],
            }
        }
        Metric::Gpu => {
            let gpu = sample.and_then(|sample| sample.gpu.as_ref());
            Card {
                metric,
                subtitle: match gpu {
                    Some(gpu) => gpu
                        .name
                        .as_deref()
                        .map_or_else(|| "Busy".to_owned(), totals_display),
                    None => GPU_UNAVAILABLE.to_owned(),
                },
                value: sample
                    .and_then(totals::gpu_utilization)
                    .map(|busy| percent_parts(busy as f32)),
                badge: None,
                figures: [
                    figure(
                        "Memory used",
                        gpu.and_then(|gpu| gpu.memory_used_bytes).map(format_bytes),
                    ),
                    figure(
                        "Memory total",
                        gpu.and_then(|gpu| gpu.memory_total_bytes).map(format_bytes),
                    ),
                    figure(
                        "Memory in use",
                        gpu.and_then(totals::gpu_memory_percent)
                            .map(|used| format_percent(used as f32)),
                    ),
                ],
            }
        }
        Metric::Disk => {
            let disk = sample.and_then(|sample| sample.disk.as_ref());
            Card {
                metric,
                subtitle: disk.map_or_else(
                    || "Free".into(),
                    |disk| format!("Free of {}", format_bytes(disk.total_bytes)),
                ),
                value: disk.map(|disk| bytes_parts(disk.available_bytes)),
                badge: None,
                figures: [
                    figure(
                        "Reading",
                        disk.and_then(|disk| disk.read_bytes_per_second)
                            .map(format_rate),
                    ),
                    figure(
                        "Writing",
                        disk.and_then(|disk| disk.written_bytes_per_second)
                            .map(format_rate),
                    ),
                    figure(
                        "Written",
                        disk.and_then(|disk| disk.written_bytes).map(format_bytes),
                    ),
                ],
            }
        }
        Metric::Network => {
            let network = sample.and_then(|sample| sample.network);
            Card {
                metric,
                subtitle: "Downloading".into(),
                value: network
                    .and_then(|network| network.received_bytes_per_second)
                    .map(rate_parts),
                badge: None,
                figures: [
                    figure(
                        "Uploading",
                        network
                            .and_then(|network| network.sent_bytes_per_second)
                            .map(format_rate),
                    ),
                    figure(
                        "Received",
                        network.map(|network| format_bytes(network.received_bytes)),
                    ),
                    figure(
                        "Sent",
                        network.map(|network| format_bytes(network.sent_bytes)),
                    ),
                ],
            }
        }
    }
}

/// One history graph: a dense window of values (one per sample), how a column sums
/// them up and the value of a full column. `under` is drawn under `values` in its own
/// shade (CPU's system share under its total).
pub(super) struct Strip {
    values: Vec<Option<f64>>,
    pub(super) under: Option<Vec<Option<f64>>>,
    combine: Combine,
    scale: f64,
    pub(super) color: Hsla,
    pub(super) under_color: Hsla,
    faint: Hsla,
}

/// The metric's overview graph: CPU and memory in use, disk read and written together,
/// network received.
fn card_series(
    metric: Metric,
    history: &TotalsHistory,
    span: Span,
) -> (Vec<Option<f64>>, Combine, f64) {
    let samples = span.samples();
    match metric {
        Metric::Cpu => (
            history.window(samples, totals::cpu_used),
            Combine::Mean,
            100.0,
        ),
        Metric::Memory => (
            history.window(samples, |sample| {
                sample.memory.as_ref().and_then(totals::memory_used_percent)
            }),
            Combine::Mean,
            100.0,
        ),
        Metric::Gpu => (
            history.window(samples, totals::gpu_utilization),
            Combine::Mean,
            100.0,
        ),
        Metric::Disk => {
            let values = history.window(samples, |sample| {
                sample
                    .disk
                    .as_ref()
                    .and_then(disk_rate)
                    .map(|rate| rate as f64)
            });
            let scale = totals::rate_scale(&values, DISK_RATE_FLOOR);
            (values, Combine::Max, scale)
        }
        Metric::Network => {
            let values = history.window(samples, |sample| {
                sample
                    .network
                    .and_then(|network| network.received_bytes_per_second)
                    .map(|rate| rate as f64)
            });
            let scale = totals::rate_scale(&values, NETWORK_RATE_FLOOR);
            (values, Combine::Max, scale)
        }
    }
}

/// A dot-matrix graph `height` pixels tall filling its width: as many columns as fit,
/// the newest at the right.
pub(super) fn dot_graph(strip: Strip, height: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let pitch = DOT + DOT_GAP;
            let width = f32::from(bounds.size.width);
            let columns = ((width + DOT_GAP) / pitch).floor().max(0.0) as usize;
            let rows = ((f32::from(bounds.size.height) + DOT_GAP) / pitch)
                .floor()
                .max(0.0) as usize;
            if columns == 0 || rows == 0 {
                return;
            }
            let top = totals::buckets(&strip.values, columns, strip.combine);
            let under = strip
                .under
                .as_ref()
                .map(|under| totals::buckets(under, columns, strip.combine));
            // Right-aligned, so the newest column sits on the edge.
            let left = f32::from(bounds.origin.x) + width - (columns as f32 * pitch - DOT_GAP);
            let bottom = f32::from(bounds.origin.y) + f32::from(bounds.size.height);
            for (column, value) in top.iter().enumerate() {
                let Some(value) = value else {
                    continue;
                };
                let filled = totals::filled_dots(*value, strip.scale, rows);
                let beneath = under
                    .as_ref()
                    .and_then(|under| under[column])
                    .map_or(0, |under| totals::filled_dots(under, strip.scale, rows))
                    .min(filled);
                let x = left + column as f32 * pitch;
                for row in 0..rows {
                    let y = bottom - DOT - row as f32 * pitch;
                    let tint = if row < beneath {
                        strip.under_color
                    } else if row < filled {
                        strip.color
                    } else {
                        strip.faint
                    };
                    window.paint_quad(
                        fill(
                            Bounds::new(point(px(x), px(y)), size(px(DOT), px(DOT))),
                            tint,
                        )
                        .corner_radii(px(DOT / 2.0)),
                    );
                }
            }
        },
    )
    .w_full()
    .h(px(height))
}

/// A ring of `segments` (each a share of the whole and its colour), clockwise from the
/// top, painted as filled paths.
fn ring(segments: Vec<(f64, Hsla)>, diameter: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let center_x = f32::from(bounds.origin.x) + f32::from(bounds.size.width) / 2.0;
            let center_y = f32::from(bounds.origin.y) + f32::from(bounds.size.height) / 2.0;
            let outer = f32::from(bounds.size.width).min(f32::from(bounds.size.height)) / 2.0;
            let inner = outer * 0.68;
            let at = |radius: f32, angle: f32| {
                point(
                    px(center_x + radius * angle.cos()),
                    px(center_y + radius * angle.sin()),
                )
            };
            let mut start = -std::f32::consts::FRAC_PI_2;
            for (share, tint) in &segments {
                let sweep = (*share as f32).clamp(0.0, 1.0) * std::f32::consts::TAU;
                if sweep <= 0.0 {
                    continue;
                }
                // About every 3 degrees, so the edge reads as a curve.
                let steps = ((sweep / 3f32.to_radians()).ceil() as usize).max(2);
                let mut points = Vec::with_capacity(2 * (steps + 1));
                for step in 0..=steps {
                    points.push(at(outer, start + sweep * step as f32 / steps as f32));
                }
                for step in (0..=steps).rev() {
                    points.push(at(inner, start + sweep * step as f32 / steps as f32));
                }
                let mut path = PathBuilder::fill();
                path.add_polygon(&points, true);
                if let Ok(path) = path.build() {
                    window.paint_path(path, *tint);
                }
                start += sweep;
            }
        },
    )
    .size(px(diameter))
}

/// The memory Memory by type divides, when the source reports its kinds.
fn overview_memory(feeds: &SourceFeeds) -> Option<&MemoryTotals> {
    feeds
        .totals
        .shown()
        .latest()
        .and_then(|sample| sample.memory.as_ref())
        .filter(|memory| !memory.kinds.is_empty())
}

impl CompiApp {
    /// The colour of `metric`, from the theme's ANSI palette, legible on the pane.
    pub(super) fn metric_color(&self, metric: Metric) -> u32 {
        let colors = self.colors();
        modal_text_color(self.theme.terminal().ansi[metric.palette_index()], colors)
    }

    pub(super) fn totals_event(
        &mut self,
        source: &MonitorSource,
        generation: u64,
        event: TotalsEvent,
    ) {
        let paused = self.monitor.paused;
        let Some(feed) = self
            .monitor
            .sources
            .get_mut(source)
            .map(|feeds| &mut feeds.totals)
            .filter(|feed| feed.slot.is_current(generation))
        else {
            // A watcher stopped since.
            return;
        };
        // While paused, what is drawn (`frozen`) stays as it was; only the live history
        // behind it changes.
        match event {
            TotalsEvent::Watching { host, history } => {
                // A new connection: its history replaces what the window had.
                feed.state = FeedState::Live;
                feed.host = Some(host);
                feed.history = TotalsHistory::seeded(history);
                if paused && feed.frozen.is_none() {
                    feed.freeze(true);
                }
            }
            TotalsEvent::Sampled(sample) => {
                feed.state = FeedState::Live;
                feed.history.push(*sample);
            }
            TotalsEvent::Stale => {
                if feed.state == FeedState::Live {
                    feed.state = FeedState::Stale;
                }
            }
            TotalsEvent::Failed(error) => {
                feed.state = FeedState::Failed(error);
                feed.history = TotalsHistory::default();
            }
        }
    }

    /// The Overview page on a page `width` wide: the cards placed on the grid
    /// `layout::overview_layout` computes, in the page's scroll.
    pub(super) fn render_monitor_overview(
        &self,
        pane_id: &PaneId,
        pane: &MonitorPane,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let feeds = self.monitor.feeds(&pane.source);
        if let Some(message) = self.totals_message(&feeds.totals) {
            return message;
        }
        let grid = layout::overview_layout(width, overview_memory(feeds).is_some());
        // The keyboard highlight moves on the grid drawn.
        pane.card_rows.set(grid.rows);
        self.monitor_scroll_page(
            "monitor-overview",
            grid.padding,
            grid.content_width,
            feeds.totals.state == FeedState::Stale,
            self.overview_cards(&OverviewHost::Pane(pane_id.clone()), &grid, pane.card, cx),
        )
    }

    /// Settings → Monitor, `width` wide: Overview's cards of the daemon's host without
    /// the rail, header or a scroll of their own. A card opens Monitor on its page.
    pub(in crate::gui::workspace) fn render_settings_monitor(
        &self,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let feeds = self.monitor.feeds(&MonitorSource::Host);
        if let Some(message) = self.totals_message(&feeds.totals) {
            return settings_card(&colors)
                .h(px(SETTINGS_MESSAGE_HEIGHT))
                .child(message)
                .into_any_element();
        }
        let grid = layout::overview_grid(width, overview_memory(feeds).is_some());
        let stale = feeds.totals.state == FeedState::Stale;
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .when(stale, |page| {
                page.child(
                    div()
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .text_color(color(modal_text_color(colors.muted, &colors)))
                        .child(STALE_NOTE),
                )
            })
            .child(
                self.overview_cards(&OverviewHost::Settings, &grid, None, cx)
                    .w(px(grid.content_width)),
            )
            .into_any_element()
    }

    /// The feeds Overview draws where `host` shows it: its pane's source, or the
    /// daemon's host in Settings.
    fn overview_feeds(&self, host: &OverviewHost) -> &SourceFeeds {
        let source = match host {
            OverviewHost::Pane(pane_id) => self.monitor.panes.get(pane_id).map(|pane| &pane.source),
            OverviewHost::Settings => None,
        };
        self.monitor.feeds(source.unwrap_or(&MonitorSource::Host))
    }

    /// Overview's cards on `grid`, each at its exact width, rows of equal height;
    /// `highlighted` rings a card for the keyboard.
    fn overview_cards(
        &self,
        host: &OverviewHost,
        grid: &OverviewLayout,
        highlighted: Option<OverviewCard>,
        cx: &Context<Self>,
    ) -> Div {
        let feeds = self.overview_feeds(host);
        let feed = &feeds.totals;
        let history = feed.shown();
        let latest = history.latest();
        let memory = overview_memory(feeds);
        let rows = grid.rows.iter().map(|row| {
            div()
                .flex()
                .items_stretch()
                .gap(px(grid.gap))
                .children(row.iter().map(|placed| {
                    let width = grid.width(placed.span);
                    let highlighted = highlighted == Some(placed.card);
                    match Metric::of_card(placed.card) {
                        Some(metric) => {
                            let (values, combine, scale) =
                                card_series(metric, history, self.monitor.span);
                            self.render_metric_card(
                                host,
                                card(metric, latest, feed.host.as_ref()),
                                self.strip(metric, values, None, combine, scale, 1.0),
                                grid,
                                width,
                                highlighted,
                                cx,
                            )
                        }
                        None if placed.card == OverviewCard::MemoryByApp => {
                            self.render_memory_by_app(host, width, highlighted, cx)
                        }
                        None => memory.map_or_else(
                            || div().into_any_element(),
                            |memory| {
                                self.render_memory_by_type(host, memory, width, highlighted, cx)
                            },
                        ),
                    }
                }))
        });
        div().flex().flex_col().gap(px(grid.gap)).children(rows)
    }

    /// A message in place of a page that draws `feed`'s totals, while they are not there.
    pub(super) fn totals_message(&self, feed: &TotalsFeed) -> Option<AnyElement> {
        if !feed.shown().is_empty() {
            return None;
        }
        let colors = self.colors();
        Some(match &feed.state {
            FeedState::Failed(error) => self.monitor_message(
                "Can't read the host's usage",
                Some((format!("{error} (retrying)"), colors.error)),
            ),
            FeedState::Connecting | FeedState::Live | FeedState::Stale => {
                self.monitor_message("Reading the host's usage…", None)
            }
        })
    }

    /// A page that scrolls: `padding` around `content`, which is `content_width` wide
    /// and centred, under the stale note when its samples stopped coming (`stale`).
    pub(super) fn monitor_scroll_page(
        &self,
        id: &'static str,
        padding: f32,
        content_width: f32,
        stale: bool,
        content: Div,
    ) -> AnyElement {
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .when(stale, |page| page.child(self.monitor_stale_note()))
            .child(
                div()
                    .id(id)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(padding))
                    .child(content.w(px(content_width)).mx_auto()),
            )
            .into_any_element()
    }

    /// A graph of `values` in `metric`'s colour (`shade` of it), with `under` beneath in
    /// the full colour.
    pub(super) fn strip(
        &self,
        metric: Metric,
        values: Vec<Option<f64>>,
        under: Option<Vec<Option<f64>>>,
        combine: Combine,
        scale: f64,
        shade: f32,
    ) -> Strip {
        let tint = color(self.metric_color(metric));
        Strip {
            values,
            under,
            combine,
            scale,
            color: tint.opacity(shade),
            under_color: tint,
            faint: tint.opacity(0.14),
        }
    }

    /// The icon, title and chevron line of a card, with `detail` before the chevron; the
    /// title gives way first.
    fn card_title(&self, metric: Metric, title: &'static str, detail: Option<String>) -> Div {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
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
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .children(detail.map(|detail| {
                div()
                    .flex_none()
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .text_color(muted)
                    .whitespace_nowrap()
                    .child(detail)
            }))
            .child(
                div()
                    .flex_none()
                    .child(controls::glyph(IconName::ChevronRight, muted)),
            )
    }

    /// Memory by app in a card `width` wide: the largest apps (and the rest as Other) as
    /// a treemap of tiles in proportion to their memory. A tile opens Processes on its
    /// app; the card, on the table as it was.
    fn render_memory_by_app(
        &self,
        host: &OverviewHost,
        width: f32,
        highlighted: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let feed = &self.overview_feeds(host).processes;
        let memory = &feed.memory;
        let inner = (width - 2.0 * CARD_INSET).max(1.0);
        let height = layout::treemap_height(inner);
        let has_data = !feed.processes.is_empty();
        let title = self.card_title(
            Metric::Memory,
            "Memory by app",
            has_data.then(|| format!("{} in all", format_bytes(memory.total))),
        );
        let tiles = if has_data {
            let tint = color(self.metric_color(Metric::Memory));
            let mut values: Vec<f64> = memory.apps.iter().map(|app| app.bytes as f64).collect();
            if memory.other > 0 {
                values.push(memory.other as f64);
            }
            let rects = treemap::squarify(
                &values,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: inner,
                    height,
                },
            );
            let tiles = rects.into_iter().enumerate().map(|(index, rect)| {
                let app = memory.apps.get(index);
                let (name, bytes) = app.map_or(("Other".to_owned(), memory.other), |app| {
                    (crate::monitor::display_line(&app.name), app.bytes)
                });
                self.render_app_tile(
                    host,
                    index,
                    rect,
                    app.map(|app| &app.key),
                    name,
                    bytes,
                    tint,
                    cx,
                )
            });
            div()
                .relative()
                .w(px(inner))
                .h(px(height))
                .children(tiles)
                .into_any_element()
        } else {
            div()
                .h(px(height))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .text_color(muted)
                .child("Reading processes…")
                .into_any_element()
        };
        let body = div().flex().flex_col().gap_3().child(title).child(tiles);
        self.clickable_card(
            host,
            OverviewCard::MemoryByApp,
            width,
            highlighted,
            body,
            cx,
        )
    }

    /// One tile of Memory by app at `rect` (1 px inside it, so tiles keep a gap): the
    /// app's icon, name and memory where it fits, the icon alone where only that fits.
    #[allow(clippy::too_many_arguments)]
    fn render_app_tile(
        &self,
        host: &OverviewHost,
        index: usize,
        rect: Rect,
        key: Option<&RowKey>,
        name: String,
        bytes: u64,
        tint: Hsla,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let (width, height) = ((rect.width - 1.0).max(0.0), (rect.height - 1.0).max(0.0));
        let icon_id = match key {
            Some(RowKey::Group(id)) => Some(id.as_str()),
            _ => None,
        };
        let fill = if key.is_some() {
            tint.opacity(0.14)
        } else {
            color(colors.border).opacity(0.5)
        };
        let content = if width >= 96.0 && height >= 56.0 {
            Some(
                div()
                    .size_full()
                    .p(px(6.0))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(key.is_some(), |content| {
                        content.child(self.app_icon(icon_id, &name, 24.0))
                    })
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .font_weight(FontWeight::MEDIUM)
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(muted)
                            .child(format_bytes(bytes)),
                    )
                    .into_any_element(),
            )
        } else if width >= 28.0 && height >= 28.0 && key.is_some() {
            Some(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(self.app_icon(
                        icon_id,
                        &name,
                        24.0_f32.min(width - 4.0).min(height - 4.0),
                    ))
                    .into_any_element(),
            )
        } else {
            None
        };
        let host = host.clone();
        let target = key.map_or(MonitorTarget::Page(MonitorPage::Processes), |key| {
            MonitorTarget::App(key.clone())
        });
        let detail: SharedString = format_bytes(bytes).into();
        div()
            .id(("monitor-app-tile", index))
            .absolute()
            .left(px(rect.x + 0.5))
            .top(px(rect.y + 0.5))
            .w(px(width))
            .h(px(height))
            .overflow_hidden()
            .rounded_sm()
            .bg(fill)
            .hover(move |style| style.bg(fill.opacity(fill.a * 1.8)))
            .children(content)
            .tooltip(controls::tooltip(name, Some(detail), None))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_overview_target(&host, target.clone(), window, cx);
                cx.stop_propagation();
                cx.notify();
            }))
            .into_any_element()
    }

    /// A card `width` wide that opens `card`'s page on click; `highlighted` rings it for
    /// the keyboard. It fills its row's height, and `body` fills the card.
    fn clickable_card(
        &self,
        host: &OverviewHost,
        card: OverviewCard,
        width: f32,
        highlighted: bool,
        body: Div,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let host = host.clone();
        let page = card_page(card);
        let surface = settings_card(&colors)
            .id(("monitor-card", card as usize))
            .flex_1()
            .p_4()
            .cursor_pointer()
            .hover(move |style| style.border_color(color(colors.accent).opacity(0.6)))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_overview_target(&host, MonitorTarget::Page(page), window, cx);
                cx.notify();
            }))
            .child(body.flex_1().min_w_0());
        controls::highlight(highlighted, surface, cx)
            .w(px(width))
            .flex()
            .flex_col()
            .into_any_element()
    }

    /// A metric's card on `grid`, `width` wide: title and subtitle, the big value, three
    /// figures, and the graph at the bottom.
    #[allow(clippy::too_many_arguments)]
    fn render_metric_card(
        &self,
        host: &OverviewHost,
        data: Card,
        strip: Strip,
        grid: &OverviewLayout,
        width: f32,
        highlighted: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let compact = grid.compact;
        let badge = data.badge.map(|pressure| match pressure {
            MemoryPressure::Critical => controls::chip("Critical", controls::ChipTone::Danger),
            MemoryPressure::Elevated | MemoryPressure::Normal => controls::chip(
                "Elevated",
                controls::ChipTone::Tint(self.metric_color(Metric::Disk)),
            ),
        });
        // Compact, the badge goes under the subtitle; otherwise beside the value.
        let (badge_above, badge_beside) = if compact {
            (badge, None)
        } else {
            (None, badge)
        };
        let inner = (width - 2.0 * CARD_INSET).max(1.0);
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(self.card_title(data.metric, data.metric.title(), None))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(muted)
                            .child(data.subtitle),
                    )
                    .children(badge_above.map(|badge| div().flex().child(badge))),
            )
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child(self.big_reading(data.value, if compact { 24.0 } else { 28.0 }))
                    .children(badge_beside.map(|badge| div().flex_none().child(badge))),
            )
            .child(self.figure_grid(
                &data.figures,
                3,
                inner,
                CARD_FIGURE_GAP,
                if compact {
                    UI_MICRO_TEXT_SIZE
                } else {
                    UI_SMALL_TEXT_SIZE
                },
            ))
            // The graphs of a row line up at the bottom of their cards.
            .child(div().flex_1())
            .child(dot_graph(
                strip,
                CARD_DOT_ROWS as f32 * (DOT + DOT_GAP) - DOT_GAP,
            ));
        self.clickable_card(host, data.metric.card(), width, highlighted, body, cx)
    }

    /// A large value and its unit, or a muted dash.
    pub(super) fn big_reading(&self, reading: Reading, size: f32) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        match reading {
            Some((value, unit)) => div()
                .min_w_0()
                .overflow_hidden()
                .flex()
                .items_baseline()
                .gap_1()
                .whitespace_nowrap()
                .child(
                    div()
                        .text_size(px(size))
                        .line_height(px(size * 1.15))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(value),
                )
                .child(
                    div()
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .text_color(muted)
                        .child(unit),
                )
                .into_any_element(),
            None => div()
                .text_size(px(size))
                .line_height(px(size * 1.15))
                .text_color(muted)
                .child(UNAVAILABLE)
                .into_any_element(),
        }
    }

    /// `figures` in an equal grid of `columns` across `width`, rows as needed. Labels
    /// and values stay on one line and end in an ellipsis when cut.
    pub(super) fn figure_grid(
        &self,
        figures: &[Figure],
        columns: usize,
        width: f32,
        gap: f32,
        value_size: f32,
    ) -> Div {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let cell_width = layout::cell_width(width, columns, gap);
        let line = || {
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .children(figures.chunks(columns.max(1)).map(|row| {
                div().flex().gap(px(gap)).children(row.iter().map(|figure| {
                    div()
                        .flex_none()
                        .w(px(cell_width))
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            line()
                                .text_size(px(UI_MICRO_TEXT_SIZE))
                                .text_color(muted)
                                .child(figure.label),
                        )
                        .child(match &figure.value {
                            Some(value) => line()
                                .text_size(px(value_size))
                                .font_weight(FontWeight::MEDIUM)
                                .child(value.clone()),
                            None => line()
                                .text_size(px(value_size))
                                .text_color(muted)
                                .child(UNAVAILABLE),
                        })
                }))
            }))
    }

    /// How memory divides, in a card `width` wide: a ring of the kinds in the host's
    /// order with the share in use in its middle, and a legend beside it (under it, the
    /// ring centred, when narrow).
    pub(super) fn render_memory_by_type(
        &self,
        host: &OverviewHost,
        memory: &MemoryTotals,
        width: f32,
        highlighted: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let (beside, diameter) = layout::ring_layout(width);
        let inner = (width - 2.0 * CARD_INSET).max(1.0);
        let whole: u64 = memory.kinds.iter().map(|kind| kind.bytes).sum();
        let kinds: Vec<(MemoryKindName, u64, Hsla)> = memory
            .kinds
            .iter()
            .map(|kind| (kind.kind, kind.bytes, self.memory_kind_color(kind.kind)))
            .collect();
        let segments = kinds
            .iter()
            .map(|&(_, bytes, tint)| (bytes as f64 / whole.max(1) as f64, tint))
            .collect();
        let in_use = totals::memory_used_percent(memory).map_or_else(
            || UNAVAILABLE.to_owned(),
            |used| format_whole_percent(used as f32),
        );
        let legend = div()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .map(|legend| {
                if beside {
                    legend.w(px((inner - diameter - RING_GAP).max(1.0)))
                } else {
                    legend.w_full()
                }
            })
            .children(kinds.iter().map(|&(kind, bytes, tint)| {
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_none().size(px(8.0)).rounded_full().bg(tint))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(totals::kind_label(kind)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(muted)
                            .whitespace_nowrap()
                            .child(format_bytes(bytes)),
                    )
            }));
        let ring = div()
            .relative()
            .flex_none()
            .size(px(diameter))
            .child(ring(segments, diameter))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_size(px(if diameter < 120.0 { 18.0 } else { 20.0 }))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(in_use),
                    )
                    .child(
                        div()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(muted)
                            .child("in use"),
                    ),
            );
        let content = if beside {
            div()
                .flex()
                .items_center()
                .gap(px(RING_GAP))
                .child(ring)
                .child(legend)
        } else {
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(div().flex().justify_center().child(ring))
                .child(legend)
        };
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.card_title(Metric::Memory, "Memory by type", None))
            .child(content);
        self.clickable_card(
            host,
            OverviewCard::MemoryByType,
            width,
            highlighted,
            body,
            cx,
        )
    }

    /// Each kind of memory in its own colour of the theme's palette; free memory faint.
    fn memory_kind_color(&self, kind: MemoryKindName) -> Hsla {
        let colors = self.colors();
        let palette = self.theme.terminal();
        let index = match kind {
            MemoryKindName::App | MemoryKindName::Used => 2,
            MemoryKindName::Wired => 3,
            MemoryKindName::Compressed => 5,
            MemoryKindName::Cached => 6,
            MemoryKindName::Free => return color(colors.border),
        };
        color(modal_text_color(palette.ansi[index], colors))
    }

    /// The host line of Monitor's nav: `feed`'s processor and how long its host has been
    /// up.
    pub(super) fn monitor_host_detail(&self, feed: &TotalsFeed) -> Option<String> {
        let host = feed.host.as_ref()?;
        let uptime = host
            .booted_at_ms
            .zip(feed.shown().latest().map(|latest| latest.sampled_at_ms))
            .map(|(booted, now)| {
                format!("Up {}", format_uptime(totals::uptime_seconds(booted, now)))
            });
        let cpu = host.cpu.as_deref().map(totals_display);
        match (cpu, uptime) {
            (Some(cpu), Some(uptime)) => Some(format!("{cpu} · {uptime}")),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        }
    }
}

/// The page an Overview card opens: its metric's, Memory for Memory by type, Processes
/// for Memory by app.
pub(super) fn card_page(card: OverviewCard) -> MonitorPage {
    match card {
        OverviewCard::MemoryByApp => MonitorPage::Processes,
        card => Metric::of_card(card).map_or(MonitorPage::Memory, Metric::page),
    }
}

/// Text the host chose (a disk or processor name), as one line.
pub(super) fn totals_display(text: &str) -> String {
    crate::monitor::display_line(text)
}
