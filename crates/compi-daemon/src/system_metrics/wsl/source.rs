//! Runs the collector script in a WSL distribution: one long-lived stream per distribution
//! while some connection watches it, and one short run per inspection or quit.

use super::super::process::system_refusal;
use super::super::sessions::SurfaceSessions;
use super::super::totals::SystemTotalsHub;
use super::super::{Collect, Collected, SystemMetricsHub};
use super::{Event, SCRIPT, Stream, decode, parse_inspection, parse_quit};
use compi_protocol::system_metrics::{ProcessInspection, QuitOutcome};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const WSL_EXE: &str = r"C:\Windows\System32\wsl.exe";
/// How often a watch takes the stream's newest sample, which costs a lock.
const POLL: Duration = Duration::from_millis(250);
/// Time before a stream that ended is started again.
const RESTART_DELAY: Duration = Duration::from_secs(5);
/// Longest an inspection or quit may take, starting the distribution included.
const RUN_TIMEOUT: Duration = Duration::from_secs(8);
/// What the stream wrote before its header that is kept to explain a failure.
const MAX_FAILURE_BYTES: usize = 4096;

type Hubs = (Arc<SystemMetricsHub>, Arc<SystemTotalsHub>);

/// The process and totals hubs of `distribution`, which share one collector: watching
/// either runs it.
pub fn hubs(distribution: &str) -> Hubs {
    static HUBS: LazyLock<Mutex<HashMap<String, Hubs>>> = LazyLock::new(Mutex::default);
    let mut hubs = HUBS.lock().unwrap_or_else(PoisonError::into_inner);
    hubs.entry(distribution.to_owned())
        .or_insert_with(|| {
            let totals = Arc::new(SystemTotalsHub::new(None));
            let (name, fed) = (distribution.to_owned(), totals.clone());
            let processes = Arc::new(SystemMetricsHub::new(
                Box::new(move || Box::new(WslCollector::new(name.clone(), fed.clone()))),
                POLL,
                POLL,
            ));
            (processes, totals)
        })
        .clone()
}

/// The state, working directory and arguments of `(pid, start_time)` in `distribution`.
pub fn inspect(
    distribution: &str,
    pid: u32,
    start_time: u64,
) -> Result<Option<ProcessInspection>, String> {
    let output = run(
        distribution,
        &["inspect", &pid.to_string(), &start_time.to_string()],
    )?;
    parse_inspection(&output)
}

/// Sends `(pid, start_time)` in `distribution` SIGTERM, or with `force` SIGKILL, if it is
/// still the process that started then.
pub fn quit(
    distribution: &str,
    pid: u32,
    start_time: u64,
    force: bool,
) -> Result<QuitOutcome, String> {
    if let Some(reason) = system_refusal(pid) {
        return Ok(QuitOutcome::Refused {
            reason: reason.into(),
        });
    }
    let signal = if force { "KILL" } else { "TERM" };
    let output = run(
        distribution,
        &["quit", &pid.to_string(), &start_time.to_string(), signal],
    )?;
    parse_quit(&output)
}

/// `wsl.exe` running the script in `distribution` with `arguments`; the caller pipes the
/// script in.
fn command(distribution: &str, arguments: &[&str]) -> Command {
    let mut command = Command::new(WSL_EXE);
    command.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
    if let Some(path) = compi_protocol::wsl::launch_path() {
        command.env("PATH", path);
    }
    command
        .args(["--distribution", distribution, "--cd", "/", "--exec"])
        .args(["/bin/sh", "-s"])
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

/// Runs the script once and returns what it wrote.
fn run(distribution: &str, arguments: &[&str]) -> Result<Vec<u8>, String> {
    let mut child = command(distribution, arguments)
        .spawn()
        .map_err(|error| format!("could not run wsl.exe: {error}"))?;
    let mut input = child.stdin.take().expect("stdin is piped");
    let mut output = child.stdout.take().expect("stdout is piped");
    let writer = thread::spawn(move || {
        let _ = input.write_all(SCRIPT.as_bytes());
    });
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = output.read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "WSL distribution {distribution} did not answer within {} seconds",
                    RUN_TIMEOUT.as_secs()
                ));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    let _ = writer.join();
    Ok(reader.join().unwrap_or_default())
}

/// The stream of one distribution. A reader thread parses it, feeding totals to the hub
/// as they arrive and keeping the newest processes for `collect`.
struct WslCollector {
    distribution: String,
    totals: Arc<SystemTotalsHub>,
    epoch: u64,
    running: Option<Running>,
    restart_at: Option<Instant>,
}

struct Running {
    child: Child,
    reader: JoinHandle<()>,
    latest: Arc<Mutex<Option<Collected>>>,
}

impl WslCollector {
    fn new(distribution: String, totals: Arc<SystemTotalsHub>) -> Self {
        let epoch = totals.begin();
        let mut collector = Self {
            distribution,
            totals,
            epoch,
            running: None,
            restart_at: None,
        };
        collector.start();
        collector
    }

    fn start(&mut self) {
        let mut child = match command(&self.distribution, &["stream"]).spawn() {
            Ok(child) => child,
            Err(error) => {
                self.totals
                    .fail(self.epoch, format!("could not run wsl.exe: {error}"));
                self.restart_at = Some(Instant::now() + RESTART_DELAY);
                return;
            }
        };
        let input = child.stdin.take().expect("stdin is piped");
        let output = child.stdout.take().expect("stdout is piped");
        let latest = Arc::new(Mutex::new(None));
        let reader = {
            let (distribution, totals, epoch, latest) = (
                self.distribution.clone(),
                self.totals.clone(),
                self.epoch,
                latest.clone(),
            );
            thread::Builder::new()
                .name("compi-wsl-monitor".into())
                .spawn(move || read(input, output, &distribution, &totals, epoch, &latest))
                .expect("spawn WSL monitor reader")
        };
        self.running = Some(Running {
            child,
            reader,
            latest,
        });
    }

    fn stop(&mut self) {
        if let Some(mut running) = self.running.take() {
            let _ = running.child.kill();
            let _ = running.child.wait();
            let _ = running.reader.join();
        }
    }
}

impl Collect for WslCollector {
    fn collect(&mut self) -> Option<Collected> {
        if let Some(running) = &self.running {
            let latest = running
                .latest
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if running.reader.is_finished() {
                self.stop();
                self.restart_at = Some(Instant::now() + RESTART_DELAY);
            }
            return latest;
        }
        if self
            .restart_at
            .is_some_and(|restart_at| Instant::now() >= restart_at)
        {
            self.restart_at = None;
            self.start();
        }
        None
    }
}

impl Drop for WslCollector {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Pipes the script in, then parses what it streams until it ends.
fn read(
    mut input: ChildStdin,
    output: ChildStdout,
    distribution: &str,
    totals: &SystemTotalsHub,
    epoch: u64,
    latest: &Mutex<Option<Collected>>,
) {
    let piped = input.write_all(SCRIPT.as_bytes());
    drop(input);
    let mut stream = Stream::default();
    let mut output = BufReader::new(output);
    let mut line = Vec::new();
    // What came before the header: wsl.exe's own complaint when it could not start.
    let mut before_header = Vec::new();
    let mut described = false;
    while piped.is_ok() {
        line.clear();
        match output.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&line);
        let event = stream.line(text.trim_end_matches(['\n', '\r']), || {
            SurfaceSessions::current().wsl_shells(distribution)
        });
        match event {
            Some(Event::Described(host)) => {
                described = true;
                totals.describe(epoch, host);
            }
            Some(Event::Sampled {
                totals: sample,
                processes,
            }) => {
                totals.publish_numbered(epoch, *sample);
                *latest.lock().unwrap_or_else(PoisonError::into_inner) = Some(processes);
            }
            None if !described && before_header.len() < MAX_FAILURE_BYTES => {
                before_header.extend_from_slice(&line);
            }
            None => {}
        }
    }
    let said = decode(&before_header);
    let said = said.trim();
    totals.fail(
        epoch,
        if said.is_empty() {
            format!("Monitor could not start in WSL distribution {distribution}")
        } else {
            format!("Monitor could not start in WSL distribution {distribution}: {said}")
        },
    );
}
