//! Real side effects and the watch loop behind `cargo dev`.

use crate::cargo;
use crate::classify::Component;
use crate::controller::{BuildOutcome, Controller, DaemonStamp, Effects, Target, count, seconds};
use crate::layout::{INSTANCE, Layout, executable, is_within, same_file};
use crate::process;
use crate::watch::{Debouncer, Tree, earliest};
use compi_protocol::{ConnectionFailure, ConnectionFailureKind, DaemonClient, LocalDaemonProcess};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

const POLL: Duration = Duration::from_millis(150);
const QUIET: Duration = Duration::from_millis(250);
const BURST_LIMIT: Duration = Duration::from_secs(2);
const CLOSE_GRACE: Duration = Duration::from_secs(5);
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Files the Windows daemon loads from its own directory.
#[cfg(windows)]
const DAEMON_RUNTIME: [&str; 3] = ["conpty.dll", "OpenConsole.exe", "ConPTY-LICENSE.txt"];
#[cfg(not(windows))]
const DAEMON_RUNTIME: [&str; 0] = [];

pub struct Options {
    pub stop: bool,
}

pub fn run(options: Options) -> Result<(), String> {
    if !cfg!(any(windows, target_os = "macos")) {
        return Err("`cargo dev` runs the native client, which exists on Windows and macOS".into());
    }
    let layout = Layout::discover()?;
    layout.prepare()?;
    let _runner_lock = layout.lock_runner()?;
    // Where ordinary (non-`cargo dev`) previews register their windows, resolved before
    // the override below.
    let shared_data = compi_protocol::paths::data_dir().ok();
    // SAFETY: still single-threaded and no child exists yet. Every dev process, and the
    // runner's own protocol calls, resolve state from this isolated directory.
    unsafe { std::env::set_var("COMPI_DATA_DIR", &layout.data) };
    if options.stop {
        return stop(layout, shared_data);
    }
    process::install_interrupt_handler();

    println!(
        "Compi dev · instance {INSTANCE} · runtime {}",
        relative(&layout.root, &layout.workspace)
    );
    let mut dev = Dev::new(layout, shared_data);
    dev.stop_strays();
    dev.close_stale_previews();
    let installed = dev.inspect_daemon()?;
    let mut tree = Tree::scan(&dev.layout.workspace);
    let mut controller = Controller::new(&tree.fingerprints(), installed);
    controller.sync(&tree.fingerprints(), &mut dev);
    if !process::interrupted() {
        println!("Watching sources · ⏎ reopen preview · r⏎ restart dev daemon · q⏎ quit");
    }

    let commands = spawn_command_reader();
    let mut debouncer = Debouncer::new(QUIET, BURST_LIMIT);
    let mut batch = Vec::new();
    let mut saved_at: Option<SystemTime> = None;
    while !process::interrupted() {
        match commands.recv_timeout(POLL) {
            Ok(Request::Quit) => break,
            Ok(Request::Restart) => controller.restart_daemon(&tree.fingerprints(), &mut dev),
            Ok(Request::Reopen) => controller.reopen(&mut dev),
            Ok(Request::Unknown(text)) => {
                println!(
                    "Unknown command {text:?} · ⏎ reopen preview · r⏎ restart dev daemon · q⏎ quit"
                )
            }
            Err(_) => {}
        }
        let changes = tree.refresh();
        if !changes.paths.is_empty() {
            debouncer.record(Instant::now());
            saved_at = earliest(saved_at, changes.saved_at);
            batch.extend(changes.paths);
        }
        if debouncer.due(Instant::now()) {
            debouncer.reset();
            if let Some(summary) = describe(&batch) {
                println!("{summary}");
            }
            batch.clear();
            let sync = controller.sync(&tree.fingerprints(), &mut dev);
            if sync.preview_replaced
                && let Some(elapsed) = saved_at.and_then(|saved| saved.elapsed().ok())
            {
                println!("Save → visible preview: {}", seconds(elapsed));
            }
            saved_at = None;
        }
        dev.notice_closed_preview();
    }

    controller.shutdown(&mut dev);
    match DaemonClient::lifecycle_status(Some(INSTANCE)) {
        Ok(status) => println!(
            "Preview closed. Dev daemon keeps running with {} — `cargo dev --stop` ends it.",
            count(status.live_surfaces.len(), "shell")
        ),
        Err(_) => println!("Preview closed."),
    }
    process::shutdown_done();
    Ok(())
}

/// `cargo dev --stop`: close dev previews and stray checkout instances, then deliberately
/// end the dev daemon.
fn stop(layout: Layout, shared_data: Option<PathBuf>) -> Result<(), String> {
    let mut dev = Dev::new(layout, shared_data);
    dev.stop_strays();
    dev.close_stale_previews();
    if !dev.daemon_running() {
        println!("Dev daemon is not running.");
        return Ok(());
    }
    let shells = dev.stop_daemon()?;
    println!("Dev daemon stopped ({} ended).", count(shells, "dev shell"));
    Ok(())
}

struct Preview {
    child: Child,
}

struct Dev {
    layout: Layout,
    /// The default state directory, where hand-launched previews register.
    shared_data: Option<PathBuf>,
    client_build: Option<PathBuf>,
    daemon_build: Option<PathBuf>,
    preview: Option<Preview>,
}

impl Dev {
    fn new(layout: Layout, shared_data: Option<PathBuf>) -> Self {
        Self {
            layout,
            shared_data,
            client_build: None,
            daemon_build: None,
            preview: None,
        }
    }

    /// Project rule: one dev instance per checkout. Previews and daemons someone started
    /// by hand from this checkout's target directory are stopped, ending their shells.
    /// Installed Compi and anything outside the target directory are never touched.
    fn stop_strays(&mut self) {
        let target = self.layout.target.clone();
        let own_client = self.layout.client_exe();
        let own_daemon = self.layout.daemon_exe();
        // Previews first: a live preview restarts its daemon within half a second.
        if let Some(shared) = self.shared_data.clone() {
            for (pid, executable) in live_hosts(&shared.join("update-gui-hosts-v1")) {
                if executable
                    .as_deref()
                    .is_some_and(|path| is_stray(path, &target, &own_client))
                {
                    self.say(&format!("Closing stray preview (pid {pid})"));
                    process::close_pid(pid, CLOSE_GRACE);
                }
            }
        }
        let daemons = match compi_protocol::local_daemon_processes() {
            Ok(daemons) => daemons,
            Err(error) => {
                self.say(&format!("Could not check for stray Compi daemons: {error}"));
                return;
            }
        };
        for daemon in daemons
            .iter()
            .filter(|daemon| is_stray(&daemon.executable, &target, &own_daemon))
        {
            let name = daemon.instance.as_deref().unwrap_or("default");
            let ended = match stop_daemon_process(daemon) {
                Some(shells) => count(shells, "shell"),
                None => "its shells".into(),
            };
            self.say(&format!(
                "Stopped stray dev instance `{name}` (pid {}, ended {ended})",
                daemon.pid
            ));
        }
    }

    /// Refuse to drive a `compi-dev` daemon that this checkout did not start.
    fn inspect_daemon(&mut self) -> Result<Option<DaemonStamp>, String> {
        let stamp = std::fs::read(&self.layout.stamp)
            .ok()
            .filter(|_| self.layout.daemon_exe().is_file())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        match DaemonClient::lifecycle_status(Some(INSTANCE)) {
            Ok(status) => {
                if !same_file(
                    Path::new(&status.daemon_executable),
                    &self.layout.daemon_exe(),
                ) {
                    return Err(format!(
                        "instance `{INSTANCE}` is served by {}, not this checkout's dev runtime. \
                         Stop it deliberately (its shells end) and run `cargo dev` again.",
                        status.daemon_executable
                    ));
                }
                self.say(&format!(
                    "Reattached to dev daemon (pid {}, {})",
                    status.daemon_pid,
                    count(status.live_surfaces.len(), "shell")
                ));
                Ok(stamp)
            }
            Err(error)
                if ConnectionFailure::kind(error.as_ref()) == ConnectionFailureKind::Absent =>
            {
                Ok(stamp)
            }
            Err(error) => Err(format!("cannot inspect the dev daemon: {error}")),
        }
    }

    /// Previews this runner does not own (a crashed runner, a manual launch) would
    /// swallow new launches through the single-window-host handoff. The isolated data
    /// directory only ever holds dev previews.
    fn close_stale_previews(&mut self) {
        let own = self.preview.as_ref().map(|preview| preview.child.id());
        for (pid, _) in live_hosts(&self.layout.data.join("update-gui-hosts-v1")) {
            if Some(pid) != own {
                self.say(&format!("Closing previous dev preview (pid {pid})"));
                process::close_pid(pid, CLOSE_GRACE);
            }
        }
    }

    fn notice_closed_preview(&mut self) {
        if let Some(preview) = &mut self.preview
            && preview.child.try_wait().ok().flatten().is_some()
        {
            self.preview = None;
            self.say("Preview closed — dev daemon and shells keep running. Press ⏎ to reopen.");
        }
    }

    fn spawn_preview(&mut self) -> Result<Child, String> {
        let mut command = Command::new(self.layout.client_exe());
        command
            .arg("--instance")
            .arg(INSTANCE)
            .arg("--config")
            .arg(&self.layout.config)
            .current_dir(&self.layout.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = process::spawn_client(&mut command)
            .map_err(|error| format!("{}: {error}", self.layout.client_exe().display()))?;
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    eprintln!("  client │ {line}");
                }
            });
        }
        Ok(child)
    }

    fn wait_until_ready(child: &mut Child, signal: &ReadySignal) -> Result<(), String> {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                return Err(format!(
                    "preview exited ({status}) before its window opened"
                ));
            }
            if signal.reached(child) {
                return Ok(());
            }
            if process::interrupted() {
                return Err("interrupted".into());
            }
            if Instant::now() >= deadline {
                return Err("preview did not open a window within 60 s".into());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn open_preview(&mut self) -> Result<(), String> {
        let signal = ReadySignal::capture();
        let mut child = self.spawn_preview()?;
        match Self::wait_until_ready(&mut child, &signal) {
            Ok(()) => {
                self.preview = Some(Preview { child });
                Ok(())
            }
            Err(error) => {
                process::close_child(&mut child, Duration::ZERO);
                Err(error)
            }
        }
    }
}

/// When a launched preview counts as visible.
#[cfg(windows)]
struct ReadySignal;

#[cfg(windows)]
impl ReadySignal {
    fn capture() -> Self {
        Self
    }

    /// A visible top-level window: the preview is on screen.
    fn reached(&self, child: &Child) -> bool {
        process::has_visible_window(child.id())
    }
}

/// macOS has no cheap foreign-window query; the client opens its window right after
/// attaching, so a new daemon session from the preview is the readiness signal.
#[cfg(not(windows))]
struct ReadySignal {
    before: BTreeSet<u64>,
}

#[cfg(not(windows))]
impl ReadySignal {
    fn connected() -> BTreeSet<u64> {
        DaemonClient::lifecycle_status(Some(INSTANCE))
            .map(|status| status.connected_clients.into_iter().collect())
            .unwrap_or_default()
    }

    fn capture() -> Self {
        Self {
            before: Self::connected(),
        }
    }

    fn reached(&self, _: &Child) -> bool {
        Self::connected().difference(&self.before).next().is_some()
    }
}

impl Effects for Dev {
    fn say(&mut self, line: &str) {
        println!("{line}");
    }

    fn build(&mut self, target: Target) -> BuildOutcome {
        let (outcome, built) = cargo::build(&self.layout.workspace, target, process::interrupted);
        if let Some(built) = built {
            match target {
                Target::Client => self.client_build = Some(built.executable),
                Target::Daemon => self.daemon_build = Some(built.executable),
            }
        }
        outcome
    }

    fn stage_daemon(&mut self) -> Result<(), String> {
        let built = self.daemon_build.as_ref().ok_or("no daemon build")?;
        let source = built.parent().ok_or("daemon build has no directory")?;
        install(built, &self.layout.pending.join(executable("compi-daemon")))?;
        for file in DAEMON_RUNTIME {
            let from = source.join(file);
            if !from.is_file() {
                return Err(format!(
                    "{} is missing; run tools/prepare-conpty.ps1, then save again",
                    from.display()
                ));
            }
            install(&from, &self.layout.pending.join(file))?;
        }
        Ok(())
    }

    fn daemon_running(&mut self) -> bool {
        DaemonClient::endpoint_available(Some(INSTANCE), Duration::from_millis(100))
            .unwrap_or(false)
    }

    fn start_daemon(&mut self, install_stamp: Option<DaemonStamp>) -> Result<(), String> {
        if let Some(stamp) = install_stamp {
            for file in std::iter::once(executable("compi-daemon"))
                .chain(DAEMON_RUNTIME.iter().map(|file| (*file).to_owned()))
            {
                install(
                    &self.layout.pending.join(&file),
                    &self.layout.bin.join(&file),
                )?;
            }
            let json = serde_json::to_vec_pretty(&stamp).map_err(|error| error.to_string())?;
            std::fs::write(&self.layout.stamp, json).map_err(|error| error.to_string())?;
        }
        let log_path = self.layout.daemon_log();
        let log = std::fs::File::create(&log_path)
            .map_err(|error| format!("{}: {error}", log_path.display()))?;
        let mut daemon = process::spawn_daemon(
            &self.layout.daemon_exe(),
            &["--instance", INSTANCE],
            &self.layout.workspace,
            log,
        )
        .map_err(|error| format!("{}: {error}", self.layout.daemon_exe().display()))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.daemon_running() {
                self.say(&format!("Dev daemon started (pid {})", daemon.id()));
                daemon.release();
                return Ok(());
            }
            if let Some(status) = daemon.exit_status() {
                // A preview may have started the same daemon a moment earlier.
                if self.daemon_running() {
                    return Ok(());
                }
                return Err(format!(
                    "daemon exited ({status}); see {}",
                    log_path.display()
                ));
            }
            if Instant::now() >= deadline {
                return Err(format!("daemon did not start; see {}", log_path.display()));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn stop_daemon(&mut self) -> Result<usize, String> {
        let status =
            DaemonClient::lifecycle_status(Some(INSTANCE)).map_err(|error| error.to_string())?;
        if !same_file(
            Path::new(&status.daemon_executable),
            &self.layout.daemon_exe(),
        ) {
            return Err(format!(
                "instance `{INSTANCE}` is served by {}, not this checkout",
                status.daemon_executable
            ));
        }
        let shells = status.live_surfaces.len();
        DaemonClient::conditional_stop(Some(INSTANCE), &status.consent(), Duration::from_secs(15))
            .map_err(|error| error.to_string())?;
        Ok(shells)
    }

    fn preview_running(&mut self) -> bool {
        self.preview
            .as_mut()
            .is_some_and(|preview| preview.child.try_wait().ok().flatten().is_none())
    }

    fn launch_preview(&mut self, install_build: bool) -> Result<Duration, String> {
        let client = self.layout.client_exe();
        let next = self.layout.bin.join(executable("compi.next"));
        let previous = self.layout.bin.join(executable("compi.previous"));
        if install_build {
            // Prepare while the old preview is still on screen.
            let built = self.client_build.as_ref().ok_or("no client build")?;
            install(built, &next)?;
        }
        let swap = Instant::now();
        self.stop_preview();
        self.close_stale_previews();
        let can_roll_back = install_build && client.is_file();
        if can_roll_back {
            replace(&client, &previous)?;
        }
        if install_build {
            replace(&next, &client)?;
        }
        let error = match self.open_preview() {
            Ok(()) => return Ok(swap.elapsed()),
            Err(error) => error,
        };
        if !can_roll_back {
            return Err(error);
        }
        // A build that compiles but cannot open must not cost the working preview.
        replace(&previous, &client)?;
        match self.open_preview() {
            Ok(()) => Err(format!("{error}; previous preview restored")),
            Err(restore) => Err(format!("{error}; previous build also failed: {restore}")),
        }
    }

    fn stop_preview(&mut self) {
        if let Some(mut preview) = self.preview.take() {
            process::close_child(&mut preview.child, CLOSE_GRACE);
        }
    }
}

impl Drop for Dev {
    /// Never leave an orphaned preview, even on an unexpected error or panic.
    fn drop(&mut self) {
        self.stop_preview();
    }
}

/// Copy beside the destination, then rename over it: a reader never sees a partial file,
/// and on macOS the running binary's inode is never rewritten in place.
fn install(from: &Path, to: &Path) -> Result<(), String> {
    let staging = to.with_extension("partial");
    retry(|| std::fs::copy(from, &staging).map(drop))
        .map_err(|error| format!("copy {} → {}: {error}", from.display(), staging.display()))?;
    replace(&staging, to)
}

/// Rename with retries: antivirus and sync clients briefly hold new executables open.
fn replace(from: &Path, to: &Path) -> Result<(), String> {
    retry(|| std::fs::rename(from, to))
        .map_err(|error| format!("replace {}: {error}", to.display()))
}

/// A process running from the Cargo target directory that is not this dev runtime's own
/// binary: a hand-launched source preview or its daemon.
fn is_stray(executable: &Path, target: &Path, own: &Path) -> bool {
    is_within(executable, target) && !same_file(executable, own)
}

/// Live GUI hosts (pid, executable) registered in a Compi state directory. A host holds
/// its registration lock for as long as it runs.
fn live_hosts(registry: &Path) -> Vec<(u32, Option<PathBuf>)> {
    let Ok(entries) = std::fs::read_dir(registry) else {
        return Vec::new();
    };
    let mut hosts = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let Ok(lock) = std::fs::File::open(path.with_extension("lock")) else {
            continue;
        };
        if lock.try_lock().is_ok() {
            continue; // Host already exited.
        }
        let Some(host) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        else {
            continue;
        };
        if let Some(pid) = host["pid"].as_u64().and_then(|pid| u32::try_from(pid).ok()) {
            hosts.push((pid, host["executable"].as_str().map(PathBuf::from)));
        }
    }
    hosts
}

/// Stop through the consent-gated lifecycle path when the instance's endpoint is served
/// by exactly this process; otherwise terminate the process. Returns the shells ended,
/// when known.
fn stop_daemon_process(daemon: &LocalDaemonProcess) -> Option<usize> {
    let instance = daemon.instance.as_deref();
    if let Ok(status) = DaemonClient::lifecycle_status(instance)
        && status.daemon_pid == daemon.pid
        && DaemonClient::conditional_stop(instance, &status.consent(), Duration::from_secs(15))
            .is_ok()
    {
        return Some(status.live_surfaces.len());
    }
    process::terminate(daemon.pid);
    process::wait_pid(daemon.pid, Duration::from_secs(5));
    None
}

fn retry(mut action: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match action() {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Request {
    Reopen,
    Restart,
    Quit,
    Unknown(String),
}

fn spawn_command_reader() -> mpsc::Receiver<Request> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if sender.send(parse_request(&line)).is_err() {
                return;
            }
        }
    });
    receiver
}

/// Terminals can inject escape sequences (focus or key reports) into the line; only the
/// typed text counts.
fn parse_request(line: &str) -> Request {
    let mut text = String::new();
    let mut chars = line.chars();
    while let Some(char) = chars.next() {
        if char != '\u{1b}' {
            if !char.is_control() {
                text.push(char);
            }
            continue;
        }
        match chars.next() {
            // CSI: parameters, then one final byte in @..~.
            Some('[') => {
                for char in chars.by_ref() {
                    if ('@'..='~').contains(&char) {
                        break;
                    }
                }
            }
            // SS3: exactly one more character (e.g. ESC O R).
            Some('O') => {
                chars.next();
            }
            _ => {}
        }
    }
    match text.trim() {
        "" => Request::Reopen,
        "r" | "R" => Request::Restart,
        "q" | "Q" => Request::Quit,
        other => Request::Unknown(other.to_owned()),
    }
}

/// One line naming what changed, e.g. `UI change: workspace.rs (+1 more)`. Runner-only
/// edits get none: the controller's one-time restart notice covers them.
fn describe(paths: &[String]) -> Option<String> {
    let unique: BTreeSet<&str> = paths.iter().map(String::as_str).collect();
    let components: BTreeSet<Component> = unique
        .iter()
        .flat_map(|path| crate::classify::classify(path).iter().copied())
        .collect();
    let kind = if components.contains(&Component::Protocol) {
        "Protocol change"
    } else if components.contains(&Component::Daemon) && components.contains(&Component::Client) {
        "Client + daemon change"
    } else if components.contains(&Component::Daemon) {
        "Daemon change"
    } else if components.contains(&Component::Client) {
        "UI change"
    } else {
        return None;
    };
    let first = unique
        .iter()
        .next()
        .map(|path| path.rsplit('/').next().unwrap_or(path))
        .unwrap_or_default();
    Some(match unique.len() {
        0 | 1 => format!("{kind}: {first}"),
        count => format!("{kind}: {first} (+{} more)", count - 1),
    })
}

fn relative(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{Request, describe, is_stray, parse_request};
    use std::path::PathBuf;

    #[test]
    fn only_hand_launched_processes_from_the_target_dir_are_strays() {
        let root = std::env::temp_dir().join("compi-dev-stray");
        let target = root.join("checkout").join("target");
        let own = target
            .join("compi-dev")
            .join("bin")
            .join("compi-daemon.exe");
        let stray = |path: PathBuf| is_stray(&path, &target, &own);

        assert!(stray(target.join("debug").join("compi-daemon.exe")));
        assert!(stray(
            target
                .join("compi-latest")
                .join("release")
                .join("compi-daemon.exe")
        ));
        // The dev runtime's own daemon is the one allowed instance.
        assert!(!stray(own.clone()));
        // Installed Compi, other checkouts, and look-alike siblings are never touched.
        assert!(!stray(
            root.join("Programs").join("Compi").join("compi-daemon.exe")
        ));
        assert!(!stray(
            root.join("other")
                .join("target")
                .join("debug")
                .join("compi-daemon.exe")
        ));
        assert!(!stray(
            root.join("checkout")
                .join("target-old")
                .join("compi-daemon.exe")
        ));
        if cfg!(windows) {
            let upper = PathBuf::from(target.to_string_lossy().to_uppercase());
            assert!(stray(upper.join("DEBUG").join("COMPI-DAEMON.EXE")));
            let own_upper = PathBuf::from(own.to_string_lossy().to_uppercase());
            assert!(!stray(own_upper));
        }
    }

    #[test]
    fn change_summary_names_the_most_disruptive_kind() {
        let paths = |items: &[&str]| {
            items
                .iter()
                .map(|item| (*item).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            describe(&paths(&["crates/compi-client/src/gui/workspace.rs"])).unwrap(),
            "UI change: workspace.rs"
        );
        assert_eq!(
            describe(&paths(&[
                "crates/compi-client/src/gui.rs",
                "crates/compi-protocol/src/lib.rs",
                "crates/compi-client/src/gui.rs",
            ]))
            .unwrap(),
            "Protocol change: gui.rs (+1 more)"
        );
        assert_eq!(
            describe(&paths(&["crates/compi-daemon/src/daemon.rs"])).unwrap(),
            "Daemon change: daemon.rs"
        );
        assert_eq!(describe(&paths(&["tools/compi-dev/src/runtime.rs"])), None);
    }

    #[test]
    fn commands_ignore_terminal_injected_escape_sequences() {
        assert_eq!(parse_request("\u{1b}ORr"), Request::Restart);
        assert_eq!(parse_request("\u{1b}[Iq\r"), Request::Quit);
        assert_eq!(parse_request("  \r"), Request::Reopen);
        assert_eq!(parse_request("\u{1b}[O"), Request::Reopen);
        assert_eq!(parse_request("restart"), Request::Unknown("restart".into()));
    }
}
