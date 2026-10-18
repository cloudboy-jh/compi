//! Monitor's watching connections: one for the host's totals while any Monitor pane is
//! drawn, one for its processes while a drawn pane shows Processes. Each runs on its own
//! thread and connection, reconnects after failures, stays released while an update
//! holds the window's connections, and stops (closing its connection) [`LINGER`] after
//! it stopped being wanted, so flicking between tabs or pages does not reconnect.
use super::*;
use compi_protocol::system_metrics::{
    HostInfo, Process, ProcessTable, SystemSample, TOTALS_INTERVAL_MS, TotalsSample,
};

/// Without a sample for three sampling intervals, the values shown are stale.
const STALE_AFTER: Duration = Duration::from_millis(3 * TOTALS_INTERVAL_MS);
/// A watcher outlives its last reason to run by this much.
pub(super) const LINGER: Duration = Duration::from_secs(2);
/// How often a watching connection looks for samples and for being stopped.
const WATCH_POLL: Duration = Duration::from_millis(50);
/// Wait before reconnecting after the connection failed or was released.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

/// What the processes watcher reports.
pub(in crate::gui) enum ProcessesEvent {
    /// Watching; no sample yet.
    Connecting,
    Sampled(ProcessesSample),
    /// No sample for [`STALE_AFTER`].
    Stale,
    /// The connection failed or ended; the watcher retries.
    Failed(String),
}

pub(in crate::gui) struct ProcessesSample {
    pub(super) processes: Vec<Process>,
    pub(super) logical_cpus: u32,
    pub(super) apps_known: bool,
}

/// What the totals watcher reports.
pub(in crate::gui) enum TotalsEvent {
    /// Watching: the host and the history the daemon kept, oldest first.
    Watching {
        host: HostInfo,
        history: Vec<TotalsSample>,
    },
    Sampled(TotalsSample),
    /// No sample for [`STALE_AFTER`].
    Stale,
    /// The connection failed or ended; the watcher retries.
    Failed(String),
}

/// The running watcher; dropping it stops the thread, which closes the connection.
struct Watcher {
    generation: u64,
    _stop: mpsc::Sender<()>,
}

/// One kind of watcher: running while wanted, and [`LINGER`] after.
#[derive(Default)]
pub(super) struct WatchSlot {
    watcher: Option<Watcher>,
    generation: u64,
    /// When it stopped being wanted.
    unwanted_since: Option<Instant>,
}

/// What [`WatchSlot::want`] did.
pub(super) enum SlotChange {
    Unchanged,
    /// A new watcher should start with this generation and stop signal.
    Start(u64, mpsc::Receiver<()>),
    /// It stopped being wanted: check [`WatchSlot::expire`] after [`LINGER`].
    Linger,
}

impl WatchSlot {
    /// Whether events of `generation` come from the running watcher.
    pub(super) fn is_current(&self, generation: u64) -> bool {
        self.watcher
            .as_ref()
            .is_some_and(|watcher| watcher.generation == generation)
    }

    pub(super) fn want(&mut self, wanted: bool) -> SlotChange {
        if wanted {
            self.unwanted_since = None;
            if self.watcher.is_some() {
                return SlotChange::Unchanged;
            }
            self.generation += 1;
            let (stop, stopped) = mpsc::channel();
            self.watcher = Some(Watcher {
                generation: self.generation,
                _stop: stop,
            });
            SlotChange::Start(self.generation, stopped)
        } else if self.watcher.is_some() && self.unwanted_since.is_none() {
            self.unwanted_since = Some(Instant::now());
            SlotChange::Linger
        } else {
            SlotChange::Unchanged
        }
    }

    /// Stops the watcher once unwanted for [`LINGER`]; whether it stopped.
    pub(super) fn expire(&mut self) -> bool {
        if self
            .unwanted_since
            .is_some_and(|since| since.elapsed() >= LINGER)
        {
            self.unwanted_since = None;
            self.watcher = None;
            true
        } else {
            false
        }
    }
}

/// What every watcher needs from its window.
#[derive(Clone)]
pub(super) struct WatchContext {
    pub(super) target: ConnectionTarget,
    pub(super) sender: UiEventSender,
    pub(super) quiesced: Arc<AtomicBool>,
    pub(super) connection_guard: Arc<Mutex<()>>,
}

enum WatchEnd {
    /// The window closed or stopped the watcher.
    Stopped,
    Failed(String),
}

/// The watcher has been dropped (or the window is gone).
fn stopped(stop: &mpsc::Receiver<()>) -> bool {
    matches!(stop.try_recv(), Err(mpsc::TryRecvError::Disconnected))
}

const PAUSED_FOR_UPDATE: &str = "Paused while Compi updates";

/// Keeps a watching connection up until stopped: connects, lets `follow` run it, and
/// reports a failure through `failed` before reconnecting.
fn run(
    context: &WatchContext,
    stop: &mpsc::Receiver<()>,
    failed: impl Fn(String) -> bool,
    mut follow: impl FnMut(&mut DaemonClient) -> WatchEnd,
) {
    loop {
        if stopped(stop) {
            return;
        }
        let client = {
            let Ok(_guard) = context.connection_guard.lock() else {
                return;
            };
            if context.quiesced.load(Ordering::Acquire) {
                Err(PAUSED_FOR_UPDATE.to_owned())
            } else {
                context
                    .target
                    .connect()
                    .map_err(|error| format!("Couldn't connect to the Compi daemon: {error}"))
            }
        };
        let end = match client {
            Ok(mut client) => follow(&mut client),
            Err(error) => WatchEnd::Failed(error),
        };
        let WatchEnd::Failed(error) = end else {
            return;
        };
        if !failed(error) {
            return;
        }
        let retry = Instant::now() + RECONNECT_DELAY;
        while Instant::now() < retry {
            if stopped(stop) {
                return;
            }
            thread::sleep(WATCH_POLL);
        }
    }
}

/// Reads events until stopped, the update pause, an error, or `event` says the window
/// is gone; reports [`STALE_AFTER`] without a sample through `stale` once.
fn poll(
    client: &mut DaemonClient,
    context: &WatchContext,
    stop: &mpsc::Receiver<()>,
    stale: impl Fn() -> bool,
    mut sampled: impl FnMut(ServerMessage) -> Option<bool>,
) -> WatchEnd {
    let mut last_sample = Instant::now();
    let mut reported_stale = false;
    loop {
        if stopped(stop) {
            return WatchEnd::Stopped;
        }
        if context.quiesced.load(Ordering::Acquire) {
            return WatchEnd::Failed(PAUSED_FOR_UPDATE.into());
        }
        match client.poll_event() {
            Ok(Some(ServerEvent::Control { message, .. })) => match sampled(message) {
                Some(true) => {
                    last_sample = Instant::now();
                    reported_stale = false;
                }
                Some(false) => return WatchEnd::Stopped,
                // Not a sample: replies and workspace revisions.
                None => {}
            },
            Ok(Some(_)) => {}
            Ok(None) => {
                if !reported_stale && last_sample.elapsed() >= STALE_AFTER {
                    reported_stale = true;
                    if !stale() {
                        return WatchEnd::Stopped;
                    }
                }
                thread::sleep(WATCH_POLL);
            }
            Err(error) => {
                return WatchEnd::Failed(format!("Lost the connection to the daemon: {error}"));
            }
        }
    }
}

/// Starts the processes watcher. Samples go through a [`ProcessTable`] per connection,
/// since the daemon sends each process's details once per connection.
pub(super) fn watch_processes(context: WatchContext, generation: u64, stop: mpsc::Receiver<()>) {
    thread::spawn(move || {
        let send = |event| {
            context
                .sender
                .send(UiEvent::Monitor(MonitorEvent::Processes {
                    generation,
                    event,
                }))
        };
        run(
            &context,
            &stop,
            |error| send(ProcessesEvent::Failed(error)),
            |client| {
                if let Err(error) = client.watch_system_metrics() {
                    return WatchEnd::Failed(format!("The daemon can't list processes: {error}"));
                }
                if !send(ProcessesEvent::Connecting) {
                    return WatchEnd::Stopped;
                }
                let mut table = ProcessTable::default();
                poll(
                    client,
                    &context,
                    &stop,
                    || send(ProcessesEvent::Stale),
                    |message| {
                        let ServerMessage::SystemSampled { sample } = message else {
                            return None;
                        };
                        let SystemSample {
                            logical_cpus,
                            apps_known,
                            processes,
                            ..
                        } = sample;
                        Some(send(ProcessesEvent::Sampled(ProcessesSample {
                            processes: table.apply(processes),
                            logical_cpus,
                            apps_known,
                        })))
                    },
                )
            },
        );
    });
}

/// Starts the totals watcher; each connection begins with the history the daemon kept.
pub(super) fn watch_totals(context: WatchContext, generation: u64, stop: mpsc::Receiver<()>) {
    thread::spawn(move || {
        let send = |event: TotalsEvent| {
            context.sender.send(UiEvent::Monitor(MonitorEvent::Totals {
                generation,
                event: Box::new(event),
            }))
        };
        run(
            &context,
            &stop,
            |error| send(TotalsEvent::Failed(error)),
            |client| {
                let (host, history) = match client.watch_system_totals() {
                    Ok(watching) => watching,
                    Err(error) => {
                        return WatchEnd::Failed(format!(
                            "The daemon can't report the host's usage: {error}"
                        ));
                    }
                };
                if !send(TotalsEvent::Watching { host, history }) {
                    return WatchEnd::Stopped;
                }
                poll(
                    client,
                    &context,
                    &stop,
                    || send(TotalsEvent::Stale),
                    |message| {
                        let ServerMessage::SystemTotalsSampled { sample } = message else {
                            return None;
                        };
                        Some(send(TotalsEvent::Sampled(sample)))
                    },
                )
            },
        );
    });
}
