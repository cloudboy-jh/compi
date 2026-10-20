use super::*;
use compi_protocol::{ProcessMetrics, RuntimeMetrics};

const HISTORY_SAMPLES: usize = 60;

#[derive(Clone)]
pub(in crate::gui) struct PerformanceSample {
    pub sampled_at: Instant,
    pub render: RenderPerformance,
    pub client: ProcessMetrics,
    pub daemon: RuntimeMetrics,
}

#[derive(Clone, Default)]
struct PerformanceView {
    render: RenderPerformance,
    client: ProcessMetrics,
    daemon: RuntimeMetrics,
    client_cpu: Option<f32>,
    daemon_cpu: Option<f32>,
}

#[derive(Default)]
pub(in crate::gui) struct PerformanceMonitor {
    latest: Option<PerformanceView>,
    previous_at: Option<Instant>,
    previous_client_cpu_ns: Option<u64>,
    previous_daemon_cpu_ns: Option<u64>,
    fps_history: VecDeque<f32>,
    client_memory_history_mib: VecDeque<f32>,
    daemon_cpu_history: VecDeque<f32>,
    error: Option<String>,
}

impl PerformanceMonitor {
    pub(in crate::gui) fn observe(&mut self, sample: PerformanceSample) {
        let elapsed = self
            .previous_at
            .map(|previous| sample.sampled_at.saturating_duration_since(previous));
        let cpu_percent = |previous: Option<u64>, current: Option<u64>| {
            let elapsed_ns = elapsed?.as_nanos() as f64;
            if elapsed_ns <= 0.0 {
                return None;
            }
            Some((current?.saturating_sub(previous?) as f64 * 100.0 / elapsed_ns) as f32)
        };
        let client_cpu = cpu_percent(self.previous_client_cpu_ns, sample.client.cpu_time_ns);
        let daemon_cpu = cpu_percent(
            self.previous_daemon_cpu_ns,
            sample.daemon.process.cpu_time_ns,
        );
        self.previous_at = Some(sample.sampled_at);
        self.previous_client_cpu_ns = sample.client.cpu_time_ns;
        self.previous_daemon_cpu_ns = sample.daemon.process.cpu_time_ns;
        push_sample(&mut self.fps_history, sample.render.updates_per_second);
        push_sample(
            &mut self.client_memory_history_mib,
            preferred_memory(&sample.client).unwrap_or_default() as f32 / 1_048_576.0,
        );
        push_sample(&mut self.daemon_cpu_history, daemon_cpu.unwrap_or_default());
        self.latest = Some(PerformanceView {
            render: sample.render,
            client: sample.client,
            daemon: sample.daemon,
            client_cpu,
            daemon_cpu,
        });
        self.error = None;
    }

    pub(in crate::gui) fn record_error(&mut self, error: String) {
        self.error = Some(error);
    }

    pub(in crate::gui) fn diagnostics(&self, cache: CacheMetrics) -> String {
        let Some(latest) = &self.latest else {
            return self
                .error
                .clone()
                .unwrap_or_else(|| "Performance metrics are not available yet.".into());
        };
        format!(
            "Rendering\n  updates_per_second: {:.1}\n  frame_p50_ms: {:.2}\n  frame_p95_ms: {:.2}\n  terminal_paint_p50_ms: {:.2}\n  terminal_paint_p95_ms: {:.2}\nResources\n  client_cpu_percent: {}\n  client_memory_bytes: {}\n  client_handles: {}\n  client_file_descriptors: {}\n  client_threads: {}\n  daemon_cpu_percent: {}\n  daemon_memory_bytes: {}\n  daemon_handles: {}\n  daemon_file_descriptors: {}\n  daemon_threads: {}\n  daemon_surfaces: {}\n  daemon_live_surfaces: {}\n  daemon_attached_surfaces: {}\nCaches\n  shaped_rows: {} / {}\n  decoded_image_bytes: {} / {}\n  pending_image_decodes: {}",
            latest.render.updates_per_second,
            latest.render.frame_p50_us as f32 / 1_000.0,
            latest.render.frame_p95_us as f32 / 1_000.0,
            latest.render.paint_p50_us as f32 / 1_000.0,
            latest.render.paint_p95_us as f32 / 1_000.0,
            optional_percent(latest.client_cpu),
            optional_u64(preferred_memory(&latest.client)),
            optional_u64(latest.client.handles),
            optional_u64(latest.client.file_descriptors),
            optional_u64(latest.client.threads),
            optional_percent(latest.daemon_cpu),
            optional_u64(preferred_memory(&latest.daemon.process)),
            optional_u64(latest.daemon.process.handles),
            optional_u64(latest.daemon.process.file_descriptors),
            optional_u64(latest.daemon.process.threads),
            latest.daemon.surfaces,
            latest.daemon.live_surfaces,
            latest.daemon.attached_surfaces,
            cache.shaped_rows,
            cache.shaped_capacity,
            cache.image_bytes,
            cache.image_capacity,
            cache.pending_decodes,
        )
    }
}

#[derive(Clone, Copy, Default)]
pub(in crate::gui) struct CacheMetrics {
    pub shaped_rows: usize,
    pub shaped_capacity: usize,
    pub image_bytes: usize,
    pub image_capacity: usize,
    pub pending_decodes: usize,
}

fn push_sample(samples: &mut VecDeque<f32>, value: f32) {
    if samples.len() == HISTORY_SAMPLES {
        samples.pop_front();
    }
    samples.push_back(value);
}

fn preferred_memory(metrics: &ProcessMetrics) -> Option<u64> {
    metrics
        .private_bytes
        .or(metrics.resident_bytes)
        .or(metrics.working_set_bytes)
}

fn optional_percent(value: Option<f32>) -> String {
    value.map_or_else(|| "unavailable".into(), |value| format!("{value:.1}"))
}

fn optional_u64(value: Option<u64>) -> String {
    value.map_or_else(|| "unavailable".into(), |value| value.to_string())
}

fn bytes_label(bytes: Option<u64>) -> String {
    bytes.map_or_else(
        || "Unavailable".into(),
        |bytes| format!("{:.1} MiB", bytes as f64 / 1_048_576.0),
    )
}

fn resource_count_label(metrics: Option<&ProcessMetrics>) -> String {
    metrics.map_or_else(
        || "Unavailable".into(),
        |metrics| {
            if let Some(handles) = metrics.handles {
                format!("{handles} handles")
            } else if let Some(file_descriptors) = metrics.file_descriptors {
                format!("{file_descriptors} file descriptors")
            } else {
                "Unavailable".into()
            }
        },
    )
}

impl CompiApp {
    pub(super) fn cache_metrics(&self) -> CacheMetrics {
        let mut metrics = CacheMetrics::default();
        for view in &self.surface_views {
            if let Ok(cache) = view.row_render_cache.lock() {
                metrics.shaped_rows += cache.entries.len();
                metrics.shaped_capacity += RowRenderCache::MAX_ROWS;
            }
            metrics.image_bytes = metrics.image_bytes.saturating_add(view.image_cache_bytes);
            metrics.image_capacity = metrics
                .image_capacity
                .saturating_add(SURFACE_IMAGE_CACHE_LIMIT);
            metrics.pending_decodes += view.image_pending.len();
        }
        metrics
    }

    pub(super) fn copy_performance_diagnostics(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(
            self.performance.diagnostics(self.cache_metrics()),
        ));
        self.performance_notice = Some((
            super::settings::DIAGNOSTICS_ROW,
            "Performance diagnostics copied.",
        ));
    }

    pub(super) fn render_fps_overlay(&self) -> AnyElement {
        let colors = *self.colors();
        let label = self.performance.latest.as_ref().map_or_else(
            || "-- FPS".to_owned(),
            |latest| format!("{:.0} FPS", latest.render.updates_per_second),
        );
        div()
            .absolute()
            .top(px(CHROME_HEIGHT + 10.0))
            .right(px(10.0))
            .px_2()
            .py_1()
            .rounded_sm()
            .border_1()
            .border_color(color(colors.border))
            .bg(color(colors.surface))
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .text_color(color(modal_text_color(colors.muted, &colors)))
            .child(label)
            .into_any_element()
    }

    /// Settings → Advanced → Compi's resource use: the frame rate, the resources of the
    /// window and the terminal server, and the renderer's caches.
    pub(super) fn render_compi_resources(&self) -> AnyElement {
        let colors = *self.colors();
        let cache = self.cache_metrics();
        let latest = self.performance.latest.as_ref();
        let metric = |label: &'static str, value: String, divided: bool| {
            div()
                .w_full()
                .min_w_0()
                .py_2()
                .flex()
                .flex_col()
                .gap_1()
                .when(divided, |metric| {
                    metric.border_b_1().border_color(color(colors.border))
                })
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color(modal_text_color(colors.muted, &colors)))
                        .child(label),
                )
                .child(
                    div()
                        .min_w_0()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(value),
                )
        };
        let render = latest.map(|latest| &latest.render);
        let client = latest.map(|latest| &latest.client);
        let daemon = latest.map(|latest| &latest.daemon);
        let cpu_label = |value: Option<f32>| {
            value.map_or_else(|| "Unavailable".into(), |value| format!("{value:.1}%"))
        };
        let process_summary = |cpu: Option<f32>, process: Option<&ProcessMetrics>| {
            format!(
                "CPU {} · Memory {} · {}",
                cpu_label(cpu),
                bytes_label(process.and_then(preferred_memory)),
                resource_count_label(process),
            )
        };
        div()
            .w_full()
            .min_w_0()
            .py_3()
            .flex()
            .flex_col()
            .gap_4()
            .child(metric(
                "Frame rate",
                render.map_or_else(
                    || "Waiting for measurements…".into(),
                    |render| format!("{:.0} FPS", render.updates_per_second),
                ),
                true,
            ))
            .child(self.render_sparkline(&self.performance.fps_history))
            .child(self.settings_subheading("Resources"))
            .child(metric(
                "Compi window",
                process_summary(latest.and_then(|latest| latest.client_cpu), client),
                true,
            ))
            .child(metric(
                "Terminal server",
                process_summary(
                    latest.and_then(|latest| latest.daemon_cpu),
                    daemon.map(|daemon| &daemon.process),
                ),
                true,
            ))
            .child(metric(
                "Terminals",
                daemon.map_or_else(
                    || "Waiting for measurements…".into(),
                    |daemon| {
                        format!(
                            "{} running · {} connected · {} total",
                            daemon.live_surfaces, daemon.attached_surfaces, daemon.surfaces
                        )
                    },
                ),
                true,
            ))
            .child(self.settings_subheading("Renderer"))
            .child(metric(
                "Cached text rows",
                format!("{} / {}", cache.shaped_rows, cache.shaped_capacity),
                true,
            ))
            .child(metric(
                "Cached images",
                format!(
                    "{:.1} / {:.0} MiB",
                    cache.image_bytes as f64 / 1_048_576.0,
                    cache.image_capacity as f64 / 1_048_576.0
                ),
                true,
            ))
            .child(metric(
                "Images waiting to decode",
                cache.pending_decodes.to_string(),
                false,
            ))
            .when_some(self.performance.error.clone(), |section, error| {
                section.child(
                    div()
                        .min_w_0()
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .text_color(color(modal_text_color(colors.error, &colors)))
                        .child(error),
                )
            })
            .into_any_element()
    }

    /// Rebuild renderer or Copy diagnostics: the row's button, and what it last did
    /// under it.
    pub(super) fn render_performance_button(
        &self,
        row: &super::settings::RowCx,
        label: &'static str,
        action: super::settings::SettingsAction,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let notice = self
            .performance_notice
            .filter(|(notice_row, _)| *notice_row == row.row)
            .map(|(_, notice)| notice);
        let button = self.render_button_row(
            row,
            super::settings::row_description(row.row).into(),
            label,
            action,
            false,
            cx,
        );
        let Some(notice) = notice else {
            return button;
        };
        div()
            .min_w_0()
            .pb_3()
            .flex()
            .flex_col()
            .child(button)
            .child(
                div()
                    .min_w_0()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .text_color(color(modal_text_color(colors.muted, &colors)))
                    .child(notice),
            )
            .into_any_element()
    }

    fn render_sparkline(&self, values: &VecDeque<f32>) -> AnyElement {
        let colors = *self.colors();
        let max = values.iter().copied().fold(1.0_f32, f32::max);
        // Newest samples sit on the right; a narrow card clips the oldest.
        div()
            .min_w_0()
            .overflow_hidden()
            .h(px(24.0))
            .pt_2()
            .flex()
            .items_end()
            .justify_end()
            .gap(px(2.0))
            .children(values.iter().enumerate().map(|(index, value)| {
                div()
                    .id(("performance-spark", index))
                    .w(px(4.0))
                    .flex_none()
                    .h(px(2.0 + 14.0 * (*value / max).clamp(0.0, 1.0)))
                    .rounded_sm()
                    .bg(color(colors.muted).opacity(0.55))
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_bounded() {
        let mut history = VecDeque::new();
        for value in 0..100 {
            push_sample(&mut history, value as f32);
        }
        assert_eq!(history.len(), HISTORY_SAMPLES);
        assert_eq!(history.front(), Some(&40.0));
        assert_eq!(history.back(), Some(&99.0));
    }

    #[test]
    fn cpu_percentage_uses_process_time_delta() {
        let mut monitor = PerformanceMonitor::default();
        let started = Instant::now();
        monitor.observe(PerformanceSample {
            sampled_at: started,
            render: RenderPerformance::default(),
            client: ProcessMetrics {
                cpu_time_ns: Some(10),
                ..Default::default()
            },
            daemon: RuntimeMetrics {
                process: ProcessMetrics {
                    cpu_time_ns: Some(20),
                    ..Default::default()
                },
                ..Default::default()
            },
        });
        monitor.observe(PerformanceSample {
            sampled_at: started + Duration::from_secs(1),
            render: RenderPerformance::default(),
            client: ProcessMetrics {
                cpu_time_ns: Some(500_000_010),
                ..Default::default()
            },
            daemon: RuntimeMetrics {
                process: ProcessMetrics {
                    cpu_time_ns: Some(250_000_020),
                    ..Default::default()
                },
                ..Default::default()
            },
        });
        let latest = monitor.latest.unwrap();
        assert_eq!(latest.client_cpu, Some(50.0));
        assert_eq!(latest.daemon_cpu, Some(25.0));
    }
}
