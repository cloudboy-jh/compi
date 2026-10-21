//! Monitor's samples of a source, shared with the connections watching them: the daemon's
//! host, or on Windows a WSL 2 distribution, whose collector is a script streaming from
//! inside it.
//!
//! One collector runs per source while at least one connection watches and stops when the
//! last one leaves, so an unwatched source costs nothing (the host's totals excepted,
//! which keep an hour of history). Each connection is sent the newest sample whenever it
//! has room, never a backlog, and each process's details only once.

mod collector;
mod icons;
mod process;
mod procfs;
mod sessions;
mod totals;
mod wsl;

pub use icons::app_icon;
pub use totals::{SystemTotalsHub, SystemTotalsWatch};

use compi_protocol::system_metrics::{
    MonitorSource, Process, ProcessInspection, ProcessSample, QuitOutcome, SystemSample,
};
use std::collections::HashSet;
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

/// Time from the first sample to the second, which is the first with CPU values.
const FIRST_INTERVAL: Duration = Duration::from_millis(500);
/// Time between later samples.
const INTERVAL: Duration = Duration::from_secs(2);

/// What Monitor can watch from this daemon: the host, then on Windows each WSL 2
/// distribution.
pub fn monitor_sources() -> Vec<MonitorSource> {
    #[cfg(windows)]
    let distributions = compi_protocol::wsl::wsl2_distributions().unwrap_or_default();
    #[cfg(not(windows))]
    let distributions = Vec::<String>::new();
    std::iter::once(MonitorSource::Host)
        .chain(
            distributions
                .into_iter()
                .map(|distribution| MonitorSource::Wsl { distribution }),
        )
        .collect()
}

/// A source this daemon can watch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Host,
    #[cfg(windows)]
    Wsl(String),
}

impl Source {
    /// `source`, if this daemon has it: a WSL distribution only on Windows, and only one
    /// installed as WSL 2.
    pub fn resolve(source: &MonitorSource) -> Result<Self, String> {
        source.validate()?;
        match source {
            MonitorSource::Host => Ok(Self::Host),
            #[cfg(not(windows))]
            MonitorSource::Wsl { .. } => {
                Err("WSL distributions are only available on a Windows daemon".into())
            }
            #[cfg(windows)]
            MonitorSource::Wsl { distribution } => {
                let installed =
                    compi_protocol::wsl::wsl2_distributions().map_err(|error| error.to_string())?;
                if installed.contains(distribution) {
                    Ok(Self::Wsl(distribution.clone()))
                } else {
                    Err(format!("no WSL 2 distribution is named {distribution}"))
                }
            }
        }
    }

    /// Starts watching its processes; sampling runs until every watch is dropped.
    pub fn watch_processes(&self) -> SystemMetricsWatch {
        match self {
            Self::Host => SystemMetricsHub::shared().watch(),
            #[cfg(windows)]
            Self::Wsl(distribution) => wsl::hubs(distribution).0.watch(),
        }
    }

    /// Starts watching its totals; the watch opens once the source has described itself.
    pub fn watch_totals(&self) -> SystemTotalsWatch {
        match self {
            Self::Host => SystemTotalsHub::shared().watch(None),
            #[cfg(windows)]
            Self::Wsl(distribution) => {
                let (processes, totals) = wsl::hubs(distribution);
                totals.watch(Some(processes.sample()))
            }
        }
    }

    /// What a sample leaves out for the process `(pid, start_time)`; `None` when no
    /// process with that start time has the PID.
    pub fn inspect(&self, pid: u32, start_time: u64) -> Result<Option<ProcessInspection>, String> {
        match self {
            Self::Host => Ok(process::inspect(pid, start_time)),
            #[cfg(windows)]
            Self::Wsl(distribution) => wsl::inspect(distribution, pid, start_time),
        }
    }

    /// Asks the process `(pid, start_time)` to quit or, with `force`, ends it; only that
    /// process, and only if it still is the one that started then.
    pub fn quit(&self, pid: u32, start_time: u64, force: bool) -> Result<QuitOutcome, String> {
        match self {
            Self::Host => Ok(process::quit(pid, start_time, force)),
            #[cfg(windows)]
            Self::Wsl(distribution) => wsl::quit(distribution, pid, start_time, force),
        }
    }
}

/// One sample with every process's details, ready to be sent to any connection.
#[derive(Debug, Default)]
pub struct Collected {
    pub sampled_at_ms: u64,
    pub logical_cpus: u32,
    pub apps_known: bool,
    pub processes: Vec<Process>,
}

/// Takes samples; the first after construction has no CPU values.
pub trait Collect: Send {
    /// A new sample, or `None` when none is ready yet (a stream between samples).
    fn collect(&mut self) -> Option<Collected>;
}

type NewCollector = Box<dyn Fn() -> Box<dyn Collect> + Send + Sync>;

pub struct SystemMetricsHub {
    state: Mutex<HubState>,
    changed: Condvar,
    new_collector: NewCollector,
    first_interval: Duration,
    interval: Duration,
}

#[derive(Default)]
struct HubState {
    watchers: usize,
    collecting: bool,
    latest: Option<Arc<Collected>>,
    /// Counts samples, so a connection knows whether it has the newest.
    generation: u64,
}

impl SystemMetricsHub {
    /// The host's hub, which every connection of this daemon shares.
    pub fn shared() -> &'static Arc<Self> {
        static HUB: LazyLock<Arc<SystemMetricsHub>> = LazyLock::new(|| {
            Arc::new(SystemMetricsHub::new(
                Box::new(|| Box::new(collector::HostCollector::new())),
                FIRST_INTERVAL,
                INTERVAL,
            ))
        });
        &HUB
    }

    fn new(new_collector: NewCollector, first_interval: Duration, interval: Duration) -> Self {
        Self {
            state: Mutex::default(),
            changed: Condvar::new(),
            new_collector,
            first_interval,
            interval,
        }
    }

    /// Starts watching; sampling runs until every returned watch is dropped.
    pub fn watch(self: &Arc<Self>) -> SystemMetricsWatch {
        SystemMetricsWatch {
            sampling: self.sample(),
            generation: 0,
            delivered: HashSet::new(),
        }
    }

    /// Keeps the collector running while the result is held.
    fn sample(self: &Arc<Self>) -> Sampling {
        let mut state = self.lock();
        state.watchers += 1;
        if !state.collecting {
            state.collecting = true;
            let hub = self.clone();
            thread::Builder::new()
                .name("compi-system-metrics".into())
                .spawn(move || hub.run())
                .expect("spawn system metrics collector");
        }
        Sampling(self.clone())
    }

    fn lock(&self) -> MutexGuard<'_, HubState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn run(&self) {
        let mut collector = (self.new_collector)();
        let mut wait = self.first_interval;
        loop {
            let collected = collector.collect().map(Arc::new);
            let mut state = self.lock();
            if Self::stop_if_unwatched(&mut state) {
                return;
            }
            if let Some(collected) = collected {
                state.latest = Some(collected);
                state.generation += 1;
            }
            let (mut state, _) = self
                .changed
                .wait_timeout_while(state, wait, |state| state.watchers > 0)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if Self::stop_if_unwatched(&mut state) {
                return;
            }
            wait = self.interval;
        }
    }

    /// Decided under the same lock `watch` takes, so a new watch either keeps this
    /// collector or starts its own.
    fn stop_if_unwatched(state: &mut HubState) -> bool {
        if state.watchers > 0 {
            return false;
        }
        state.collecting = false;
        state.latest = None;
        true
    }

    fn release(&self) {
        let mut state = self.lock();
        state.watchers -= 1;
        if state.watchers == 0 {
            self.changed.notify_all();
        }
    }
}

/// Keeps a hub's collector running until dropped.
pub struct Sampling(Arc<SystemMetricsHub>);

impl Drop for Sampling {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// What one watching connection has been sent.
pub struct SystemMetricsWatch {
    sampling: Sampling,
    generation: u64,
    /// Processes whose details this connection has.
    delivered: HashSet<(u32, u64)>,
}

impl SystemMetricsWatch {
    /// The newest sample, if this connection has not been sent it.
    pub fn pending(&self) -> Option<(u64, Arc<Collected>)> {
        let state = self.sampling.0.lock();
        let latest = state.latest.as_ref()?;
        (state.generation != self.generation).then(|| (state.generation, latest.clone()))
    }

    /// The message for `collected`, with details only for processes new to this connection.
    pub fn message(&self, collected: &Collected) -> SystemSample {
        SystemSample {
            sampled_at_ms: collected.sampled_at_ms,
            logical_cpus: collected.logical_cpus,
            apps_known: collected.apps_known,
            processes: collected
                .processes
                .iter()
                .map(|process| {
                    let key = (process.sample.pid, process.sample.start_time);
                    ProcessSample {
                        details: (!self.delivered.contains(&key))
                            .then(|| Box::new((*process.details).clone())),
                        ..process.sample.clone()
                    }
                })
                .collect(),
        }
    }

    /// `collected` reached the connection: every process in it has its details.
    pub fn delivered(&mut self, generation: u64, collected: &Collected) {
        self.generation = generation;
        self.delivered = collected
            .processes
            .iter()
            .map(|process| (process.sample.pid, process.sample.start_time))
            .collect();
    }
}

/// Cuts `text` to at most `max` bytes at a character boundary; whether it was cut.
fn cut_text(text: &mut String, max: usize) -> bool {
    if text.len() <= max {
        return false;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::system_metrics::{ProcessDetails, ProcessOwner};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    static STARTED: AtomicU64 = AtomicU64::new(0);

    /// Pid 1 always; pid 2 from the second sample of each collector.
    struct Fake {
        samples: u64,
    }

    impl Collect for Fake {
        fn collect(&mut self) -> Option<Collected> {
            self.samples += 1;
            let process = |pid| Process {
                sample: ProcessSample {
                    pid,
                    start_time: 7,
                    parent_pid: None,
                    details: None,
                    cpu_percent: (self.samples > 1).then_some(1.0),
                    memory_bytes: Some(self.samples),
                    threads: None,
                    cpu_time_ms: None,
                },
                details: Arc::new(ProcessDetails {
                    name: format!("p{pid}"),
                    command: None,
                    executable: None,
                    user: None,
                    owner: ProcessOwner::CurrentUser,
                    app: false,
                    restricted: false,
                    app_group: None,
                    surface: None,
                }),
            };
            let mut processes = vec![process(1)];
            if self.samples > 1 {
                processes.push(process(2));
            }
            Some(Collected {
                processes,
                ..Collected::default()
            })
        }
    }

    fn hub() -> Arc<SystemMetricsHub> {
        Arc::new(SystemMetricsHub::new(
            Box::new(|| {
                STARTED.fetch_add(1, Ordering::SeqCst);
                Box::new(Fake { samples: 0 })
            }),
            Duration::from_millis(5),
            Duration::from_millis(5),
        ))
    }

    fn until<T>(mut ready: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(value) = ready() {
                return value;
            }
            assert!(Instant::now() < deadline, "timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn details_go_once_per_connection_and_sampling_stops_with_the_last_watch() {
        let hub = hub();
        let started = STARTED.load(Ordering::SeqCst);
        let mut first = hub.watch();
        let (generation, collected) = until(|| first.pending());
        let message = first.message(&collected);
        assert!(
            message
                .processes
                .iter()
                .all(|sample| sample.details.is_some())
        );
        first.delivered(generation, &collected);

        // Once delivered, a sample is not pending again, and later ones carry details
        // only for processes this connection has not had.
        assert!(first.pending().is_none_or(|(newer, _)| newer > generation));
        let had: Vec<u32> = collected.processes.iter().map(|p| p.sample.pid).collect();
        let (generation, collected) = until(|| first.pending());
        let with_details: Vec<u32> = first
            .message(&collected)
            .processes
            .iter()
            .filter(|sample| sample.details.is_some())
            .map(|sample| sample.pid)
            .collect();
        let new: Vec<u32> = collected
            .processes
            .iter()
            .map(|p| p.sample.pid)
            .filter(|pid| !had.contains(pid))
            .collect();
        assert_eq!(with_details, new);
        assert!(!with_details.contains(&1));
        first.delivered(generation, &collected);

        // A second connection gets every process's details.
        let second = hub.watch();
        let (_, collected) = until(|| second.pending());
        assert!(
            second
                .message(&collected)
                .processes
                .iter()
                .all(|sample| sample.details.is_some())
        );
        assert_eq!(STARTED.load(Ordering::SeqCst), started + 1);

        drop((first, second));
        until(|| (!hub.lock().collecting).then_some(()));
        assert!(hub.lock().latest.is_none());

        // Watching again starts a fresh collector.
        let third = hub.watch();
        until(|| third.pending());
        assert_eq!(STARTED.load(Ordering::SeqCst), started + 2);
    }

    #[cfg(not(windows))]
    #[test]
    fn only_the_host_is_a_source_off_windows() {
        assert_eq!(monitor_sources(), [MonitorSource::Host]);
        assert_eq!(Source::resolve(&MonitorSource::Host), Ok(Source::Host));
        let wsl = MonitorSource::Wsl {
            distribution: "Ubuntu".into(),
        };
        assert!(Source::resolve(&wsl).unwrap_err().contains("Windows"));
        let invalid = MonitorSource::Wsl {
            distribution: "a/b".into(),
        };
        assert!(Source::resolve(&invalid).is_err());
    }

    #[test]
    fn text_is_cut_at_a_character_boundary() {
        let mut text = "aé".to_owned();
        assert!(cut_text(&mut text, 2));
        assert_eq!(text, "a");
        assert!(!cut_text(&mut text, 2));
    }
}
