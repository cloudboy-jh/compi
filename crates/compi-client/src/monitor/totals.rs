//! Monitor's totals, kept pure: the window's history of the host's totals samples, how
//! a range of it divides into the columns of a dot graph, and which figures the cards
//! show. The pane (`gui::workspace::monitor`) draws them.
//!
//! The history is indexed by the samples' `sequence`, which goes up by one per sample. A
//! sequence the window never received (its connection fell behind) is a gap, drawn as
//! an empty column, never as a zero.

use compi_protocol::system_metrics::{
    MemoryKindName, MemoryTotals, TOTALS_HISTORY, TOTALS_INTERVAL_MS, TotalsSample,
};
use std::collections::VecDeque;

/// How much history the graphs show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Span {
    Minute,
    #[default]
    FiveMinutes,
    FifteenMinutes,
    Hour,
}

impl Span {
    pub const ALL: [Self; 4] = [
        Self::Minute,
        Self::FiveMinutes,
        Self::FifteenMinutes,
        Self::Hour,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Minute => "1m",
            Self::FiveMinutes => "5m",
            Self::FifteenMinutes => "15m",
            Self::Hour => "1h",
        }
    }

    pub const fn seconds(self) -> u64 {
        match self {
            Self::Minute => 60,
            Self::FiveMinutes => 300,
            Self::FifteenMinutes => 900,
            Self::Hour => 3600,
        }
    }

    /// Samples in the span.
    pub const fn samples(self) -> usize {
        (self.seconds() * 1000 / TOTALS_INTERVAL_MS) as usize
    }
}

/// The totals samples a window received, oldest first, at most an hour's worth of
/// sequences.
#[derive(Clone, Debug, Default)]
pub struct TotalsHistory {
    samples: VecDeque<TotalsSample>,
}

impl TotalsHistory {
    /// The history a watch began with (oldest first).
    pub fn seeded(history: Vec<TotalsSample>) -> Self {
        let mut seeded = Self::default();
        for sample in history {
            seeded.push(sample);
        }
        seeded
    }

    /// Adds the next sample. One that does not follow the latest (a repeat, or out of
    /// order) is dropped; samples more than an hour of sequences older go.
    pub fn push(&mut self, sample: TotalsSample) {
        if self
            .latest()
            .is_some_and(|latest| sample.sequence <= latest.sequence)
        {
            return;
        }
        let oldest_kept = sample.sequence.saturating_sub(TOTALS_HISTORY as u64 - 1);
        self.samples.push_back(sample);
        while self
            .samples
            .front()
            .is_some_and(|front| front.sequence < oldest_kept)
        {
            self.samples.pop_front();
        }
    }

    pub fn latest(&self) -> Option<&TotalsSample> {
        self.samples.back()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// One value per sequence of the `span` samples ending at the latest, oldest first:
    /// `None` where no sample was received (or the history does not reach back that
    /// far) and where `value` has none.
    pub fn window(
        &self,
        span: usize,
        value: impl Fn(&TotalsSample) -> Option<f64>,
    ) -> Vec<Option<f64>> {
        let mut values = vec![None; span];
        let Some(end) = self.latest().map(|latest| latest.sequence) else {
            return values;
        };
        // The sequence of the window's first entry; before the daemon's first sample
        // when the span reaches back further.
        let start = i128::from(end) + 1 - span as i128;
        for sample in self.samples.iter().rev() {
            let index = i128::from(sample.sequence) - start;
            if index < 0 {
                break;
            }
            if let Some(slot) = values.get_mut(index as usize) {
                *slot = value(sample);
            }
        }
        values
    }
}

/// How a column of the graph sums up the samples it covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    /// Percentages: the column's average level.
    Mean,
    /// Rates: the column's peak, so a short burst still shows.
    Max,
}

/// `values` (one per sample, oldest first) divided into `columns` equal buckets, each
/// combined from the values it covers that are present; `None` where it has none.
/// With fewer samples than columns each sample spans several columns.
pub fn buckets(values: &[Option<f64>], columns: usize, combine: Combine) -> Vec<Option<f64>> {
    let span = values.len();
    if span == 0 {
        return vec![None; columns];
    }
    (0..columns)
        .map(|column| {
            let low = column * span / columns;
            let high = ((column + 1) * span / columns).clamp(low + 1, span);
            let mut present = values[low..high].iter().flatten().copied();
            let first = present.next()?;
            Some(match combine {
                Combine::Max => present.fold(first, f64::max),
                Combine::Mean => {
                    let (sum, count) = present.fold((first, 1.0), |(sum, count), value| {
                        (sum + value, count + 1.0)
                    });
                    sum / count
                }
            })
        })
        .collect()
}

/// The full height of a rate graph: the highest bucket shown, but at least `floor`, so
/// an idle link does not draw a full column for a few bytes.
pub fn rate_scale(buckets: &[Option<f64>], floor: f64) -> f64 {
    buckets.iter().flatten().copied().fold(floor, f64::max)
}

/// Dots filled from the bottom of a column of `rows` for `value` of `scale`. Any value
/// above zero fills at least one, so activity never reads as none.
pub fn filled_dots(value: f64, scale: f64, rows: usize) -> usize {
    if value <= 0.0 || scale <= 0.0 || rows == 0 {
        return 0;
    }
    ((value / scale * rows as f64).round() as usize).clamp(1, rows)
}

/// CPU in use: user and system together.
pub fn cpu_used(sample: &TotalsSample) -> Option<f64> {
    sample
        .cpu
        .map(|cpu| f64::from(cpu.user + cpu.system).clamp(0.0, 100.0))
}

/// Memory in use, as a percentage of all memory.
pub fn memory_used_percent(memory: &MemoryTotals) -> Option<f64> {
    (memory.total_bytes > 0).then(|| memory.used_bytes as f64 / memory.total_bytes as f64 * 100.0)
}

pub const fn kind_label(kind: MemoryKindName) -> &'static str {
    match kind {
        MemoryKindName::App => "App",
        MemoryKindName::Wired => "Wired",
        MemoryKindName::Compressed => "Compressed",
        MemoryKindName::Used => "Used",
        MemoryKindName::Cached => "Cached",
        MemoryKindName::Free => "Free",
    }
}

/// One small labelled figure of a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteFigure {
    pub label: &'static str,
    pub bytes: Option<u64>,
}

/// The Memory card's three figures: App, Wired and Compressed where the host divides
/// memory that way (macOS), else Used, Cached and Swap used.
pub fn memory_figures(memory: &MemoryTotals) -> [ByteFigure; 3] {
    let kind = |name: MemoryKindName| {
        memory
            .kinds
            .iter()
            .find(|kind| kind.kind == name)
            .map(|kind| kind.bytes)
    };
    let figure = |name: MemoryKindName| ByteFigure {
        label: kind_label(name),
        bytes: kind(name),
    };
    let detailed = [
        MemoryKindName::App,
        MemoryKindName::Wired,
        MemoryKindName::Compressed,
    ];
    if detailed.into_iter().any(|name| kind(name).is_some()) {
        detailed.map(figure)
    } else {
        [
            ByteFigure {
                label: "Used",
                bytes: kind(MemoryKindName::Used).or(Some(memory.used_bytes)),
            },
            figure(MemoryKindName::Cached),
            ByteFigure {
                label: "Swap used",
                bytes: memory.swap_used_bytes,
            },
        ]
    }
}

/// Seconds the host has been up at `now_ms` (the host's clock, from a sample).
pub fn uptime_seconds(booted_at_ms: u64, now_ms: u64) -> u64 {
    now_ms.saturating_sub(booted_at_ms) / 1000
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::system_metrics::{CpuTotals, MemoryKind};

    fn sample(sequence: u64, cpu: Option<f32>) -> TotalsSample {
        TotalsSample {
            sequence,
            sampled_at_ms: 1_000_000 + sequence * TOTALS_INTERVAL_MS,
            cpu: cpu.map(|user| CpuTotals { user, system: 0.0 }),
            ..TotalsSample::default()
        }
    }

    fn cpu_window(history: &TotalsHistory, span: usize) -> Vec<Option<f64>> {
        history.window(span, cpu_used)
    }

    #[test]
    fn spans_count_their_samples() {
        assert_eq!(Span::Minute.samples(), 30);
        assert_eq!(Span::Hour.samples(), TOTALS_HISTORY);
        assert_eq!(Span::default(), Span::FiveMinutes);
    }

    #[test]
    fn history_keeps_order_drops_repeats_and_stays_within_an_hour() {
        let mut history = TotalsHistory::seeded(vec![sample(5, Some(1.0)), sample(6, None)]);
        history.push(sample(6, Some(9.0)));
        history.push(sample(4, Some(9.0)));
        assert_eq!(history.len(), 2);
        assert_eq!(history.latest().map(|latest| latest.sequence), Some(6));
        history.push(sample(5 + TOTALS_HISTORY as u64, Some(2.0)));
        // The hour ending at the newest still holds sequence 6, no longer 5.
        assert_eq!(history.len(), 2);
        history.push(sample(6 + TOTALS_HISTORY as u64, Some(2.0)));
        assert_eq!(history.len(), 2);
        assert_eq!(cpu_window(&history, 2), [Some(2.0), Some(2.0)]);
    }

    #[test]
    fn windows_leave_gaps_and_unreached_history_empty() {
        let history = TotalsHistory::seeded(vec![
            sample(10, Some(10.0)),
            sample(11, None),
            sample(13, Some(30.0)),
        ]);
        assert_eq!(
            cpu_window(&history, 5),
            [None, Some(10.0), None, None, Some(30.0)]
        );
        // Further back than the daemon has samples: the early part stays empty.
        let young = TotalsHistory::seeded(vec![sample(0, Some(5.0)), sample(1, Some(6.0))]);
        assert_eq!(cpu_window(&young, 4), [None, None, Some(5.0), Some(6.0)]);
        assert_eq!(cpu_window(&TotalsHistory::default(), 2), [None, None]);
    }

    #[test]
    fn buckets_take_means_or_peaks_of_what_is_present() {
        let values = [Some(10.0), Some(30.0), None, Some(50.0), None, None];
        assert_eq!(
            buckets(&values, 3, Combine::Mean),
            [Some(20.0), Some(50.0), None]
        );
        assert_eq!(
            buckets(&values, 3, Combine::Max),
            [Some(30.0), Some(50.0), None]
        );
        assert_eq!(buckets(&values, 1, Combine::Max), [Some(50.0)]);
        // Fewer samples than columns: each sample spans columns, gaps stay gaps.
        assert_eq!(
            buckets(&[Some(1.0), None], 4, Combine::Mean),
            [Some(1.0), Some(1.0), None, None]
        );
        assert_eq!(buckets(&[], 2, Combine::Mean), [None, None]);
        assert!(buckets(&values, 0, Combine::Mean).is_empty());
        // A partial range: only the newest columns have data.
        let mut partial = vec![None; 8];
        partial[6] = Some(4.0);
        partial[7] = Some(8.0);
        assert_eq!(
            buckets(&partial, 4, Combine::Mean),
            [None, None, None, Some(6.0)]
        );
    }

    #[test]
    fn dots_fill_in_proportion_with_a_floor_for_rates() {
        assert_eq!(filled_dots(0.0, 100.0, 8), 0);
        assert_eq!(filled_dots(0.1, 100.0, 8), 1);
        assert_eq!(filled_dots(50.0, 100.0, 8), 4);
        assert_eq!(filled_dots(100.0, 100.0, 8), 8);
        assert_eq!(filled_dots(400.0, 100.0, 8), 8);
        assert_eq!(filled_dots(5.0, 0.0, 8), 0);
        assert_eq!(rate_scale(&[None, Some(10.0), Some(3.0)], 100.0), 100.0);
        assert_eq!(rate_scale(&[None, Some(500.0)], 100.0), 500.0);
        assert_eq!(rate_scale(&[], 64.0), 64.0);
    }

    #[test]
    fn memory_figures_follow_how_the_host_divides_memory() {
        let kinds = |kinds: &[(MemoryKindName, u64)]| {
            kinds
                .iter()
                .map(|&(kind, bytes)| MemoryKind { kind, bytes })
                .collect()
        };
        let mac = MemoryTotals {
            total_bytes: 100,
            used_bytes: 60,
            kinds: kinds(&[
                (MemoryKindName::App, 30),
                (MemoryKindName::Wired, 20),
                (MemoryKindName::Cached, 25),
                (MemoryKindName::Free, 25),
            ]),
            swap_total_bytes: Some(10),
            swap_used_bytes: Some(2),
            pressure: None,
        };
        assert_eq!(
            memory_figures(&mac),
            [
                ByteFigure {
                    label: "App",
                    bytes: Some(30)
                },
                ByteFigure {
                    label: "Wired",
                    bytes: Some(20)
                },
                ByteFigure {
                    label: "Compressed",
                    bytes: None
                },
            ]
        );
        let windows = MemoryTotals {
            kinds: kinds(&[(MemoryKindName::Used, 55), (MemoryKindName::Free, 45)]),
            swap_used_bytes: None,
            ..mac.clone()
        };
        assert_eq!(
            memory_figures(&windows),
            [
                ByteFigure {
                    label: "Used",
                    bytes: Some(55)
                },
                ByteFigure {
                    label: "Cached",
                    bytes: None
                },
                ByteFigure {
                    label: "Swap used",
                    bytes: None
                },
            ]
        );
        let bare = MemoryTotals {
            kinds: Vec::new(),
            ..mac.clone()
        };
        assert_eq!(memory_figures(&bare)[0].bytes, Some(60));
        assert_eq!(memory_used_percent(&mac), Some(60.0));
        assert_eq!(memory_used_percent(&MemoryTotals::default()), None);
    }

    #[test]
    fn cpu_use_adds_user_and_system_and_uptime_counts_from_boot() {
        let busy = TotalsSample {
            cpu: Some(CpuTotals {
                user: 70.0,
                system: 45.0,
            }),
            ..TotalsSample::default()
        };
        assert_eq!(cpu_used(&busy), Some(100.0));
        assert_eq!(cpu_used(&TotalsSample::default()), None);
        assert_eq!(uptime_seconds(1_000, 61_000), 60);
        assert_eq!(uptime_seconds(5_000, 1_000), 0);
    }
}
