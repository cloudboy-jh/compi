//! Real side effects and the watch loop behind `cargo dev`.

use crate::cargo;
use crate::classify::Component;
use crate::controller::{
    BuildOutcome, Controller, DaemonStamp, Effects, Note, Target, count, seconds,
};
use crate::layout::{INSTANCE, Layout, executable, is_within, same_file};
use crate::log::{self, Tag, Tone};
use crate::process;
use crate::watch::{Debouncer, Tree};
use compi_protocol::{ConnectionFailure, ConnectionFailureKind, DaemonClient, LocalDaemonProcess};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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
    #[cfg(target_os = "linux")]
    if std::env::var_os("WSL_DISTRO_NAME").is_some() {
        return run_on_windows(&options);
    }
    if !cfg!(any(windows, target_os = "macos")) {
        return Err(
            "the preview is the native client, which runs on Windows and macOS only".into(),
        );
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
    log::init();
    log::header(INSTANCE, &relative(&layout.root, &layout.workspace));
    let mut dev = Dev::new(layout, shared_data);
    dev.stop_strays();
    dev.close_stale_previews();
    let installed = dev.inspect_daemon()?;
    let mut tree = Tree::scan(&dev.layout.workspace);
    let mut controller = Controller::new(&tree.fingerprints(), installed);
    controller.sync(&tree.fingerprints(), &mut dev);
    if !process::interrupted() {
        log::line(Tag::Dev, Tone::Quiet, "watching sources");
    }

    let commands = spawn_command_reader();
    let mut debouncer = Debouncer::new(QUIET, BURST_LIMIT);
    let mut batch = Vec::new();
    while !process::interrupted() {
        match commands.recv_timeout(POLL) {
            Ok(Request::Quit) => break,
            Ok(Request::Restart) => controller.restart_daemon(&tree.fingerprints(), &mut dev),
            Ok(Request::Reopen) => controller.reopen(&mut dev),
            Ok(Request::Unknown(text)) => log::line(
                Tag::Dev,
                Tone::Warn,
                &format!(
                    "unknown command {text:?} · ⏎ reopen preview · r⏎ restart dev daemon · q⏎ quit"
                ),
            ),
            Err(_) => {}
        }
        let changes = tree.refresh();
        if !changes.paths.is_empty() {
            debouncer.record(Instant::now());
            log::saved(changes.saved_at);
            batch.extend(changes.paths);
        }
        if debouncer.due(Instant::now()) {
            debouncer.reset();
            if let Some(summary) = describe(&batch) {
                log::line(Tag::Change, Tone::Plain, &summary);
            }
            batch.clear();
            controller.sync(&tree.fingerprints(), &mut dev);
            log::forget_save();
        }
        dev.notice_closed_preview();
    }

    controller.shutdown(&mut dev);
    log::finish();
    log::line(Tag::Preview, Tone::Plain, "closed");
    if let Ok(status) = DaemonClient::lifecycle_status(Some(INSTANCE)) {
        log::line(
            Tag::Daemon,
            Tone::Plain,
            &format!(
                "keeps running · {} · `cargo dev --stop` ends it",
                count(status.live_surfaces.len(), "shell")
            ),
        );
    }
    process::shutdown_done();
    Ok(())
}

/// Inside WSL the preview is the Windows client, so `cargo dev` runs as Windows' own
/// `cargo dev` on the same checkout, which WSL starts in the matching Windows folder.
#[cfg(target_os = "linux")]
fn run_on_windows(options: &Options) -> Result<(), String> {
    let here = std::env::current_dir().map_err(|error| error.to_string())?;
    let mut parts = here.components().skip(1);
    let on_windows_drive = parts.next().is_some_and(|part| part.as_os_str() == "mnt")
        && parts.next().is_some_and(|part| {
            let drive = part.as_os_str().as_encoded_bytes();
            drive.len() == 1 && drive[0].is_ascii_alphabetic()
        });
    if !on_windows_drive {
        return Err(format!(
            "{} is in WSL's own file system; the preview is a Windows program, so run \
             `cargo dev` from a checkout on a Windows drive (/mnt/c/…)",
            here.display()
        ));
    }
    let mut command = Command::new("cargo.exe");
    command.arg("dev");
    if options.stop {
        command.arg("--stop");
    }
    let status = command.status().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "cargo.exe is not on PATH; install Rust on Windows, or run `cargo dev` in \
             PowerShell"
                .to_owned()
        } else {
            format!("could not start Windows' cargo.exe: {error}")
        }
    })?;
    // Windows' runner has already said what went wrong.
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

/// `cargo dev --stop`: close dev previews and stray checkout instances, then deliberately
/// end the dev daemon.
fn stop(layout: Layout, shared_data: Option<PathBuf>) -> Result<(), String> {
    log::init();
    let mut dev = Dev::new(layout, shared_data);
    dev.stop_strays();
    dev.close_stale_previews();
    if !dev.daemon_running() {
        log::line(Tag::Daemon, Tone::Plain, "not running");
        return Ok(());
    }
    log::progress(Tag::Daemon, "stopping…");
    let shells = dev.stop_daemon().inspect_err(|_| log::finish())?;
    render(Note::DaemonStopped { shells });
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
                    log::line(
                        Tag::Dev,
                        Tone::Plain,
                        &format!("closing stray preview · pid {pid}"),
                    );
                    process::close_pid(pid, CLOSE_GRACE);
                }
            }
        }
        let daemons = match compi_protocol::local_daemon_processes() {
            Ok(daemons) => daemons,
            Err(error) => {
                log::line(
                    Tag::Dev,
                    Tone::Warn,
                    &format!("could not check for stray daemons · {error}"),
                );
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
            log::line(
                Tag::Dev,
                Tone::Plain,
                &format!(
                    "stopped stray instance `{name}` · pid {} · ended {ended}",
                    daemon.pid
                ),
            );
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
                log::line(
                    Tag::Daemon,
                    Tone::Plain,
                    &format!(
                        "reattached · pid {} · {}",
                        status.daemon_pid,
                        count(status.live_surfaces.len(), "shell")
                    ),
                );
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
                log::line(
                    Tag::Dev,
                    Tone::Plain,
                    &format!("closing previous preview · pid {pid}"),
                );
                process::close_pid(pid, CLOSE_GRACE);
            }
        }
    }

    fn notice_closed_preview(&mut self) {
        if let Some(preview) = &mut self.preview
            && preview.child.try_wait().ok().flatten().is_some()
        {
            self.preview = None;
            log::line(
                Tag::Preview,
                Tone::Plain,
                "closed · dev daemon and shells keep running · ⏎ reopens it",
            );
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
                    log::output(Tag::Client, &line);
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
    fn note(&mut self, note: Note) {
        render(note);
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
                log::line(
                    Tag::Daemon,
                    Tone::Plain,
                    &format!("started · pid {}", daemon.id()),
                );
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
            #[cfg(target_os = "macos")]
            sign(&next)?;
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

/// The linker signs `executable` ad hoc under a hash-derived identifier, and macOS
/// services such as notifications refuse a bundle whose code identifier differs from its
/// bundle identifier; re-sign it under the bundle's.
#[cfg(target_os = "macos")]
fn sign(executable: &Path) -> Result<(), String> {
    let output = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-", "--identifier"])
        .arg(crate::layout::BUNDLE_IDENTIFIER)
        .arg(executable)
        .output()
        .map_err(|error| format!("codesign {}: {error}", executable.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "codesign {}: {}",
            executable.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
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

/// What changed, e.g. `workspace.rs +1 · ui`, naming the most disruptive kind. Runner-only
/// edits get none: the controller's one-time restart notice covers them.
fn describe(paths: &[String]) -> Option<String> {
    let unique: BTreeSet<&str> = paths.iter().map(String::as_str).collect();
    let components: BTreeSet<Component> = unique
        .iter()
        .flat_map(|path| crate::classify::classify(path).iter().copied())
        .collect();
    let kind = if components.contains(&Component::Protocol) {
        "protocol"
    } else if components.contains(&Component::Daemon) && components.contains(&Component::Client) {
        "client + daemon"
    } else if components.contains(&Component::Daemon) {
        "daemon"
    } else if components.contains(&Component::Client) {
        "ui"
    } else {
        return None;
    };
    let first = unique
        .iter()
        .next()
        .map(|path| path.rsplit('/').next().unwrap_or(path))
        .unwrap_or_default();
    Some(match unique.len() {
        0 | 1 => format!("{first} · {kind}"),
        count => format!("{first} +{} · {kind}", count - 1),
    })
}

fn render(note: Note) {
    let restart = "r⏎ restarts the dev daemon (ends dev shells)";
    let (tag, tone, text) = match note {
        Note::RunnerChanged => (
            Tag::Dev,
            Tone::Warn,
            "runner sources changed · restart `cargo dev` to use them".to_owned(),
        ),
        Note::PreviewAlreadyOpen => (Tag::Preview, Tone::Plain, "already open".to_owned()),
        Note::NoDaemon => (
            Tag::Daemon,
            Tone::Warn,
            "not running · save a file or press r⏎ to start it".to_owned(),
        ),
        Note::NoCompatibleClient => (
            Tag::Preview,
            Tone::Warn,
            "no client build speaks the running daemon's protocol yet".to_owned(),
        ),
        Note::RestartBlocked(target) => (
            target_tag(target),
            Tone::Error,
            "build failing · fix it before restarting the dev daemon".to_owned(),
        ),
        Note::Restarting => return log::progress(Tag::Daemon, "restarting…"),
        Note::DaemonStopped { shells } => (
            Tag::Daemon,
            Tone::Plain,
            format!("stopped · ended {}", count(shells, "dev shell")),
        ),
        Note::DaemonStopFailed(error) => (
            Tag::Daemon,
            Tone::Error,
            format!("could not stop · {error}"),
        ),
        Note::DaemonStartFailed(error) => (
            Tag::Daemon,
            Tone::Error,
            format!("could not start · {error}"),
        ),
        Note::DaemonWaitsForBuild => (
            Tag::Daemon,
            Tone::Warn,
            "cannot start until its build succeeds".to_owned(),
        ),
        Note::Building(target) => return log::progress(target_tag(target), "building…"),
        Note::Built {
            target,
            elapsed,
            warnings,
        } => {
            let mut text = format!("built · {}", seconds(elapsed));
            if warnings > 0 {
                text.push_str(&format!(" · {}", count(warnings, "warning")));
            }
            let tone = if warnings > 0 {
                Tone::Warn
            } else {
                Tone::Plain
            };
            (target_tag(target), tone, text)
        }
        Note::BuildFailed {
            target,
            diagnostics,
        } => {
            let kept = match target {
                Target::Client => "preview unchanged",
                Target::Daemon => "dev daemon unchanged",
            };
            log::line(
                target_tag(target),
                Tone::Error,
                &format!("build failed · {kept}"),
            );
            return log::detail(diagnostics.trim_end());
        }
        Note::StageFailed(error) => (
            Tag::Daemon,
            Tone::Error,
            format!("could not stage build · {error}"),
        ),
        Note::PreviewStarting { refresh } => {
            return log::progress(
                Tag::Preview,
                if refresh {
                    "refreshing…"
                } else {
                    "opening…"
                },
            );
        }
        Note::PreviewReady { refresh, elapsed } => {
            let mut text = format!(
                "{} · {}",
                if refresh { "refreshed" } else { "open" },
                seconds(elapsed)
            );
            if let Some(latency) = log::take_save_latency() {
                text.push_str(&format!(" · {} from save", seconds(latency)));
            }
            (Tag::Preview, Tone::Plain, text)
        }
        Note::PreviewFailed(error) => (
            Tag::Preview,
            Tone::Error,
            format!("failed to start · {error}"),
        ),
        Note::DaemonStale => (
            Tag::Daemon,
            Tone::Warn,
            format!("stale · its sources changed, shells keep running · {restart}"),
        ),
        Note::ProtocolHold { preview_open } => (
            Tag::Preview,
            Tone::Warn,
            format!(
                "{} · protocol changed · {restart}",
                if preview_open { "kept" } else { "not opened" }
            ),
        ),
    };
    log::line(tag, tone, &text);
}

fn target_tag(target: Target) -> Tag {
    match target {
        Target::Client => Tag::Client,
        Target::Daemon => Tag::Daemon,
    }
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
            "workspace.rs · ui"
        );
        assert_eq!(
            describe(&paths(&[
                "crates/compi-client/src/gui.rs",
                "crates/compi-protocol/src/lib.rs",
                "crates/compi-client/src/gui.rs",
            ]))
            .unwrap(),
            "gui.rs +1 · protocol"
        );
        assert_eq!(
            describe(&paths(&["crates/compi-daemon/src/daemon.rs"])).unwrap(),
            "daemon.rs · daemon"
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
