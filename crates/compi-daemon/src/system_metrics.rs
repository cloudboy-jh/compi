//! Monitor's samples of this host, shared with the connections watching them.
//!
//! One collector thread runs while at least one connection watches and stops when the
//! last one leaves, so an unwatched daemon spends nothing on it. Each connection is sent
//! the newest sample whenever it has room, never a backlog, and each process's details
//! only once.

mod collector;
mod totals;

pub use totals::{SystemTotalsHub, SystemTotalsWatch};

use compi_protocol::system_metrics::{Process, ProcessSample, SystemSample};
use std::collections::HashSet;
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

/// Time from the first sample to the second, which is the first with CPU values.
const FIRST_INTERVAL: Duration = Duration::from_millis(500);
/// Time between later samples.
const INTERVAL: Duration = Duration::from_secs(2);

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
    fn collect(&mut self) -> Collected;
}

pub struct SystemMetricsHub {
    state: Mutex<HubState>,
    changed: Condvar,
    new_collector: fn() -> Box<dyn Collect>,
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
    /// The hub every connection of this daemon shares.
    pub fn shared() -> &'static Arc<Self> {
        static HUB: LazyLock<Arc<SystemMetricsHub>> = LazyLock::new(|| {
            Arc::new(SystemMetricsHub::new(
                || Box::new(collector::HostCollector::new()),
                FIRST_INTERVAL,
                INTERVAL,
            ))
        });
        &HUB
    }

    fn new(
        new_collector: fn() -> Box<dyn Collect>,
        first_interval: Duration,
        interval: Duration,
    ) -> Self {
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
        SystemMetricsWatch {
            hub: self.clone(),
            generation: 0,
            delivered: HashSet::new(),
        }
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
            let collected = Arc::new(collector.collect());
            let mut state = self.lock();
            if Self::stop_if_unwatched(&mut state) {
                return;
            }
            state.latest = Some(collected);
            state.generation += 1;
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

/// What one watching connection has been sent.
pub struct SystemMetricsWatch {
    hub: Arc<SystemMetricsHub>,
    generation: u64,
    /// Processes whose details this connection has.
    delivered: HashSet<(u32, u64)>,
}

impl SystemMetricsWatch {
    /// The newest sample, if this connection has not been sent it.
    pub fn pending(&self) -> Option<(u64, Arc<Collected>)> {
        let state = self.hub.lock();
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

impl Drop for SystemMetricsWatch {
    fn drop(&mut self) {
        self.hub.release();
    }
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
        fn collect(&mut self) -> Collected {
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
                }),
            };
            let mut processes = vec![process(1)];
            if self.samples > 1 {
                processes.push(process(2));
            }
            Collected {
                processes,
                ..Collected::default()
            }
        }
    }

    fn hub() -> Arc<SystemMetricsHub> {
        Arc::new(SystemMetricsHub::new(
            || {
                STARTED.fetch_add(1, Ordering::SeqCst);
                Box::new(Fake { samples: 0 })
            },
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
}
