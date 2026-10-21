#![cfg(unix)]

#[path = "support/kitty.rs"]
mod kitty;

use compi_client::{MirrorApply, ScreenMirror};
use compi_protocol::program_status::{BlockedKind, ProgramState, ProgramStatusRecord};
use compi_protocol::system_metrics::{
    MonitorSource, ProcessOwner, ProcessTable, QuitOutcome, SystemSample, TotalsSample,
};
use compi_protocol::{
    ClientMessage, ContentEnvironment, DaemonClient, DaemonError, ErrorCode, FolderContent,
    LayoutNode, PROTOCOL_VERSION, PaneContent, PaneSide, PaneView, ScreenSnapshot, ServerEvent,
    ServerMessage, SplitAxis, SplitGeometry, SurfaceId, SurfaceInfo, SurfaceStatus,
    WorkspaceMutation,
};
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);
static DAEMON_TEST_LOCK: Mutex<()> = Mutex::new(());
const TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
// Keep the macOS debug fixture above ordinary socket buffers without making
// every CI run serialize a 44 MiB frame. Linux and Windows retain 4K coverage.
#[cfg(target_os = "macos")]
const KITTY_TRANSFER_DIMENSIONS: (u32, u32) = (1024, 768);
#[cfg(not(target_os = "macos"))]
const KITTY_TRANSFER_DIMENSIONS: (u32, u32) = (3840, 2160);

#[test]
fn directory_rpc_reads_surface_filesystem_and_rejects_invalid_paths() {
    let mut daemon = DaemonGuard::start();
    let root = daemon.directory.join("tree");
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("source")).unwrap();
    fs::write(root.join("source").join("needle.rs"), b"from daemon").unwrap();
    let root = root.to_string_lossy().into_owned();
    let mut client = daemon.client();
    let surface = client.create_surface(80, 24, Some(root.clone())).unwrap();

    let entries = client
        .list_directory(surface.id.clone(), root.clone())
        .unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry.name == "source" && entry.is_directory)
    );
    let matches = client
        .search_directory(surface.id.clone(), root.clone(), "needle".into())
        .unwrap();
    assert!(
        matches.iter().any(|entry| {
            entry.path == format!("{root}/source/needle.rs") && !entry.is_directory
        })
    );

    let code = |error: compi_protocol::Error| error.downcast::<DaemonError>().unwrap().code;
    assert_eq!(
        code(
            client
                .list_directory(surface.id.clone(), "relative".into())
                .unwrap_err()
        ),
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        code(
            client
                .list_directory(surface.id.clone(), "/missing-compi-directory-825932".into())
                .unwrap_err()
        ),
        ErrorCode::Internal
    );
    assert_eq!(
        code(
            client
                .list_directory(SurfaceId::new("not-this-server"), root.clone())
                .unwrap_err()
        ),
        ErrorCode::SurfaceNotFound
    );
    assert_eq!(
        code(
            client
                .search_directory(surface.id.clone(), root, "".into())
                .unwrap_err()
        ),
        ErrorCode::InvalidRequest
    );
    daemon.shutdown();
}

struct DaemonGuard {
    child: Child,
    instance: String,
    directory: PathBuf,
    _test_lock: MutexGuard<'static, ()>,
}

impl DaemonGuard {
    fn start() -> Self {
        let test_lock = DAEMON_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let instance = format!(
            "unix-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
            NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed)
        );
        let directory = std::env::temp_dir().join(format!("compi-{instance}"));
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let child = Self::spawn(&instance, &directory);
        let mut daemon = Self {
            child,
            instance,
            directory,
            _test_lock: test_lock,
        };
        daemon.wait_ready();
        daemon
    }

    fn spawn(instance: &str, directory: &std::path::Path) -> Child {
        let log = fs::File::create(directory.join("daemon.log")).unwrap();
        Command::new(env!("CARGO_BIN_EXE_compi-daemon"))
            .args(["--instance", instance])
            .env("COMPI_DATA_DIR", directory)
            .env("SHELL", "/bin/bash")
            .env("HOME", directory)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap()
    }

    fn wait_ready(&mut self) {
        let started = Instant::now();
        let deadline = started + TIMEOUT;
        let names = compi_protocol::identity::instance_names(Some(&self.instance)).unwrap();
        loop {
            if compi_protocol::pipe::connect(&names.pipe, Duration::from_millis(100)).is_ok() {
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "daemon exited while waiting for endpoint after {:?}: {}",
                started.elapsed(),
                self.log()
            );
            assert!(
                Instant::now() < deadline,
                "timed out after {:?} waiting for endpoint {}: {}",
                started.elapsed(),
                names.pipe,
                self.log()
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn log(&self) -> String {
        fs::read_to_string(self.directory.join("daemon.log")).unwrap_or_default()
    }

    fn client(&self) -> DaemonClient {
        let mut client = self.unhandshaken_client();
        assert!(matches!(
            client
                .request(ClientMessage::Hello {
                    protocol_version: PROTOCOL_VERSION,
                })
                .unwrap(),
            ServerMessage::Hello {
                protocol_version: PROTOCOL_VERSION
            }
        ));
        client
    }

    fn unhandshaken_client(&self) -> DaemonClient {
        let names = compi_protocol::identity::instance_names(Some(&self.instance)).unwrap();
        let connection =
            compi_protocol::pipe::connect(&names.pipe, Duration::from_secs(2)).unwrap();
        let timeout = libc::timeval {
            tv_sec: 5,
            tv_usec: 0,
        };
        for option in [libc::SO_RCVTIMEO, libc::SO_SNDTIMEO] {
            assert_eq!(
                unsafe {
                    libc::setsockopt(
                        connection.as_raw_fd(),
                        libc::SOL_SOCKET,
                        option,
                        &timeout as *const _ as *const libc::c_void,
                        std::mem::size_of_val(&timeout) as libc::socklen_t,
                    )
                },
                0
            );
        }
        DaemonClient::from_parts(connection, 1)
    }

    fn crash_and_restart(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
        let names = compi_protocol::identity::instance_names(Some(&self.instance)).unwrap();
        assert!(
            fs::symlink_metadata(&names.pipe)
                .unwrap()
                .file_type()
                .is_socket(),
            "abrupt daemon death should leave the stale endpoint to recover"
        );
        self.child = Self::spawn(&self.instance, &self.directory);
        self.wait_ready();
    }

    fn shutdown(&mut self) {
        self.client().shutdown_daemon().unwrap();
        let started = Instant::now();
        let deadline = started + TIMEOUT;
        while self.child.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "timed out after {:?} waiting for daemon shutdown: {}",
                started.elapsed(),
                self.log()
            );
            thread::sleep(POLL_INTERVAL);
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            if let Ok(names) = compi_protocol::identity::instance_names(Some(&self.instance))
                && let Ok(connection) =
                    compi_protocol::pipe::connect(&names.pipe, Duration::from_millis(100))
            {
                let mut client = DaemonClient::from_parts(connection, 1);
                let _ = client.send(ClientMessage::Hello {
                    protocol_version: PROTOCOL_VERSION,
                });
                let _ = client.send(ClientMessage::ShutdownDaemon);
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                thread::sleep(POLL_INTERVAL);
            }
            if self.child.try_wait().ok().flatten().is_none() {
                eprintln!(
                    "daemon did not stop within 2s during cleanup; forcing exit: {}",
                    self.log()
                );
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

struct Controller {
    client: DaemonClient,
    mirror: ScreenMirror,
}

impl Controller {
    fn attach(daemon: &DaemonGuard, surface: &SurfaceInfo, cols: i16, rows: i16) -> Self {
        let mut client = daemon.client();
        client.attach_surface(surface, cols, rows).unwrap();
        Self {
            client,
            mirror: ScreenMirror::default(),
        }
    }

    fn input(&mut self, data: impl AsRef<[u8]>) {
        self.client
            .request(ClientMessage::Input {
                data: data.as_ref().to_vec(),
                latency_id: None,
                alternate_scroll: false,
            })
            .unwrap();
    }

    fn until(&mut self, marker: &str) -> ScreenSnapshot {
        self.until_matching(&format!("{marker:?}"), |snapshot| {
            snapshot_text(snapshot).contains(marker)
        })
    }

    fn until_marker_and_image(&mut self, marker: &str, image_id: u32) -> ScreenSnapshot {
        self.until_matching(
            &format!("{marker:?} and Kitty image {image_id}"),
            |snapshot| {
                snapshot_text(snapshot).contains(marker)
                    && snapshot.images.iter().any(|image| image.id == image_id)
            },
        )
    }

    fn until_matching(
        &mut self,
        description: &str,
        ready: impl Fn(&ScreenSnapshot) -> bool,
    ) -> ScreenSnapshot {
        let started = Instant::now();
        let deadline = started + TIMEOUT;
        loop {
            // A previous wait may already have consumed the frame satisfying
            // this condition. Check the replica even when no new event arrives.
            if let Some(snapshot) = self.mirror.snapshot()
                && ready(snapshot)
            {
                return snapshot.clone();
            }
            assert!(
                Instant::now() < deadline,
                "timed out after {:?} waiting for {description}; last screen: {:?}",
                started.elapsed(),
                self.mirror.snapshot().map(snapshot_text)
            );
            let event = if let Some(message) = self.client.take_pending_screen() {
                Some(ServerEvent::Screen(message))
            } else {
                self.client.poll_event().unwrap_or_else(|error| {
                    panic!(
                        "waiting for {description} failed after {:?}: {error}; last screen: {:?}",
                        started.elapsed(),
                        self.mirror.snapshot().map(snapshot_text)
                    )
                })
            };
            match event {
                Some(ServerEvent::Screen(message)) => {
                    if matches!(self.mirror.apply(message), MirrorApply::Gap { .. }) {
                        self.client.request_snapshot().unwrap();
                    }
                }
                Some(ServerEvent::Control {
                    message: ServerMessage::Error { code, message, .. },
                    ..
                }) => {
                    panic!(
                        "waiting for {description} after {:?}: daemon error {code:?}: {message}",
                        started.elapsed()
                    );
                }
                _ => {}
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

fn snapshot_text(snapshot: &ScreenSnapshot) -> String {
    let mut text = String::new();
    let cols = usize::from(snapshot.cols);
    let history = snapshot.scrollback.iter().map(|row| row.to_row(cols));
    for row in history.chain(snapshot.cells.iter().cloned()) {
        for cell in row.cells.iter().filter(|cell| cell.width != 0) {
            text.push_str(&cell.text);
        }
        if !row.wrapped {
            text.push('\n');
        }
    }
    text
}

fn wait_status(client: &mut DaemonClient, id: &SurfaceId, expected: SurfaceStatus) -> SurfaceInfo {
    let started = Instant::now();
    let deadline = started + TIMEOUT;
    loop {
        let surface = client
            .list_surfaces()
            .unwrap()
            .into_iter()
            .find(|surface| &surface.id == id)
            .unwrap();
        if surface.status == expected {
            return surface;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {:?} waiting for {id} status {expected:?}; last surface: {surface:?}",
            started.elapsed()
        );
        thread::sleep(POLL_INTERVAL);
    }
}

#[test]
fn kitty_payload_and_placement_survive_detach_and_reconnect() {
    let mut daemon = DaemonGuard::start();
    let (width, height) = KITTY_TRANSFER_DIMENSIONS;
    let encoded = kitty::write_transfer(&daemon.directory.join("kitty-transfer"), width, height);
    let mut control = daemon.client();
    let surface = control
        .create_surface(
            80,
            24,
            Some(daemon.directory.to_string_lossy().into_owned()),
        )
        .unwrap();
    let mut attached = Controller::attach(&daemon, &surface, 80, 24);
    attached.input(b"cat kitty-transfer\r");
    let before = attached.until_marker_and_image("KITTY_READY_42", 42);
    assert_eq!(before.images.len(), 1);
    assert!(
        before.images[0].data.as_ref() == encoded,
        "transmitted Kitty payload changed"
    );
    assert_eq!(
        (before.images[0].width, before.images[0].height),
        (width, height)
    );
    assert_eq!(before.placements.len(), 1);
    assert_eq!(before.placements[0].image_id, 42);
    assert_eq!(before.placements[0].placement_id, Some(7));
    attached.client.request(ClientMessage::Detach).unwrap();
    drop(attached);
    let mut reattached = Controller::attach(&daemon, &surface, 80, 24);
    let after = reattached.until_marker_and_image("KITTY_READY_42", 42);
    assert!(
        after.images == before.images,
        "reattach changed image payload or identity"
    );
    assert_eq!(after.placements, before.placements);
    assert_eq!(
        control.list_surfaces().unwrap()[0].process_lifetime_id,
        surface.process_lifetime_id
    );
    daemon.shutdown();
}

#[test]
fn native_shell_persists_across_controllers_and_resizes_in_cwd_with_spaces() {
    let mut daemon = DaemonGuard::start();
    let cwd = daemon.directory.join("working directory with spaces");
    fs::create_dir(&cwd).unwrap();
    let cwd = fs::canonicalize(cwd).unwrap();
    let mut control = daemon.client();
    let session = control
        .create_surface(80, 24, Some(cwd.to_str().unwrap().to_owned()))
        .unwrap();
    let mut attached = Controller::attach(&daemon, &session, 90, 30);
    let mut competing = daemon.client();
    let error = competing
        .attach_surface(&session, 80, 24)
        .unwrap_err()
        .to_string();
    assert!(error.contains("AlreadyAttached"), "{error}");

    attached.input(b"stty -echo; saved_pid=$$; saved_value=41; printf '%s' \"$PWD\" > actual-cwd; printf 'NATIVE_%s SIZE=' \"$((saved_value+1))\"; stty size\r");
    attached.until("NATIVE_42");
    assert_eq!(
        fs::read_to_string(cwd.join("actual-cwd")).unwrap(),
        cwd.to_str().unwrap()
    );
    // Wait for the complete size output, not merely the preceding marker.
    attached.until("30 90");
    attached
        .client
        .request(ClientMessage::Resize {
            cols: 112,
            rows: 37,
        })
        .unwrap();
    // Force the size marker across a soft wrap, as long host prompts do on CI.
    attached.input(b"printf 'RESIZED_%s \\033[112G' \"$((20+22))\"; stty size\r");
    attached.until("37 112");
    attached.input(b"read -r answer; printf 'INPUT_%s\\n' \"$answer\"\r");
    attached.input(b"interactive-value\r");
    attached.until("INPUT_interactive-value");
    attached.client.request(ClientMessage::Detach).unwrap();
    drop(attached);

    let mut attached = Controller::attach(&daemon, &session, 112, 37);
    attached.until("INPUT_interactive-value");
    attached.input(
        b"test \"$saved_pid\" = \"$$\" && printf 'SAME_PROCESS_%s\\n' \"$((saved_value+1))\"\r",
    );
    attached.until("SAME_PROCESS_42");
    // A dropped controller is also a detach, not a shell termination.
    drop(attached);
    let started = Instant::now();
    let deadline = started + TIMEOUT;
    loop {
        let current = control
            .list_surfaces()
            .unwrap()
            .into_iter()
            .find(|item| item.id == session.id)
            .unwrap();
        if !current.attached {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {:?} waiting for controller disconnect; last surface: {current:?}",
            started.elapsed()
        );
        thread::sleep(POLL_INTERVAL);
    }
    let mut attached = Controller::attach(&daemon, &session, 80, 24);
    attached.input(b"printf 'DISCONNECT_%s\\n' \"$((saved_value+2))\"\r");
    attached.until("DISCONNECT_43");
    control.end_surface(&session).unwrap();
    wait_status(&mut control, &session.id, SurfaceStatus::Exited);
    daemon.shutdown();
}

#[test]
fn alternate_scroll_filters_only_synthetic_arrows_when_disabled() {
    let mut daemon = DaemonGuard::start();
    let mut control = daemon.client();
    let session = control.create_surface(80, 24, None).unwrap();
    let mut attached = Controller::attach(&daemon, &session, 80, 24);
    // Read raw arrow bytes as canonical lines with echo off. Hex output makes
    // accidental delivery observable without interpreting the arrows as output.
    attached.input(
        b"stty -echo; printf '\\033[?1049h\\033[?1007lDISABLED_%s\\n' \"$((20+22))\"; IFS= read -r answer; printf '%s' \"$answer\" | od -An -tx1; printf '\\033[?1007hENABLED_%s\\n' \"$((20+22))\"; IFS= read -r answer; printf '%s' \"$answer\" | od -An -tx1; stty echo\r",
    );
    attached.until("DISABLED_42");
    attached
        .client
        .request(ClientMessage::Input {
            data: b"\x1b[A".to_vec(),
            latency_id: None,
            alternate_scroll: true,
        })
        .unwrap();
    attached.input(b"\x1b[B\r");
    let disabled = snapshot_text(&attached.until("ENABLED_42"));
    let bytes: Vec<_> = disabled.split_whitespace().collect();
    assert!(
        bytes.windows(3).any(|bytes| bytes == ["1b", "5b", "42"]),
        "{disabled}"
    );
    assert!(
        !bytes.windows(3).any(|bytes| bytes == ["1b", "5b", "41"]),
        "{disabled}"
    );

    attached
        .client
        .request(ClientMessage::Input {
            data: b"\x1b[A".to_vec(),
            latency_id: None,
            alternate_scroll: true,
        })
        .unwrap();
    attached.input(b"\r");
    attached.until_matching("synthetic Up arrow bytes", |snapshot| {
        let text = snapshot_text(snapshot);
        let bytes: Vec<_> = text.split_whitespace().collect();
        bytes.windows(3).any(|bytes| bytes == ["1b", "5b", "41"])
    });
    control.end_surface(&session).unwrap();
    wait_status(&mut control, &session.id, SurfaceStatus::Exited);
    daemon.shutdown();
}

#[test]
fn incompatible_protocol_is_rejected_without_affecting_other_clients() {
    let mut daemon = DaemonGuard::start();
    let mut incompatible = daemon.unhandshaken_client();
    let error = incompatible
        .request(ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION + 1,
        })
        .unwrap_err()
        .to_string();
    assert!(error.contains("IncompatibleProtocol"), "{error}");
    assert!(daemon.client().list_surfaces().unwrap().is_empty());
    daemon.shutdown();
}

#[test]
fn natural_exit_retains_read_only_grid_and_rejects_input() {
    let mut daemon = DaemonGuard::start();
    let mut control = daemon.client();
    let session = control.create_surface(80, 24, None).unwrap();
    let mut attached = Controller::attach(&daemon, &session, 80, 24);
    attached.input(b"printf 'NATURAL_%s\\n' \"$((20+22))\"; exit 23\r");
    let exited = wait_status(&mut control, &session.id, SurfaceStatus::Exited);
    assert_eq!(exited.exit_code, Some(23));
    assert!(!exited.attached);
    drop(attached);
    // Exited grids remain readable for the native client's final-screen view;
    // only process input is unavailable. Reattachment must not respawn a shell.
    let mut final_view = Controller::attach(&daemon, &exited, 100, 32);
    final_view.until("NATURAL_42");
    // No further output is coming: a repeated wait must inspect its saved grid.
    final_view.until("NATURAL_42");
    let error = final_view
        .client
        .request(ClientMessage::Input {
            data: b"printf SHOULD_NOT_RUN\\n\r".to_vec(),
            latency_id: None,
            alternate_scroll: false,
        })
        .unwrap_err()
        .to_string();
    assert!(error.contains("SurfaceUnavailable"), "{error}");
    final_view
        .client
        .request(ClientMessage::Resize {
            cols: 112,
            rows: 37,
        })
        .unwrap();
    let retained = wait_status(&mut control, &session.id, SurfaceStatus::Exited);
    assert_eq!(retained.exit_code, Some(23));
    assert_eq!(retained.process_lifetime_id, session.process_lifetime_id);
    final_view.client.request(ClientMessage::Detach).unwrap();
    let mut resized = Controller::attach(&daemon, &retained, 112, 37);
    let snapshot = resized.until("NATURAL_42");
    assert_eq!((snapshot.cols, snapshot.rows), (112, 37));
    daemon.shutdown();
}

fn read_pid(path: &std::path::Path) -> libc::pid_t {
    let started = Instant::now();
    let deadline = started + TIMEOUT;
    loop {
        if let Ok(contents) = fs::read_to_string(path)
            && let Ok(pid) = contents.trim().parse::<libc::pid_t>()
        {
            assert!(pid > 1);
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {:?} waiting for valid PID at {}",
            started.elapsed(),
            path.display()
        );
        thread::sleep(POLL_INTERVAL);
    }
}

fn process_running(pid: libc::pid_t) -> bool {
    // Reparented zombies can remain briefly on Linux CI; they have exited and
    // cannot retain PTY ownership or execute descendants.
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .unwrap();
    let state = String::from_utf8(output.stdout).unwrap();
    output.status.success() && !state.trim().is_empty() && !state.trim().starts_with('Z')
}

#[test]
fn explicit_kill_stops_shell_and_foreground_and_background_descendants() {
    let mut daemon = DaemonGuard::start();
    let mut control = daemon.client();
    let session = control
        .create_surface(80, 24, Some(daemon.directory.to_str().unwrap().to_owned()))
        .unwrap();
    let mut attached = Controller::attach(&daemon, &session, 80, 24);
    attached.input(b"stty -echo; echo $$ > shell.pid; sleep 300 & echo $! > background.pid; sh -c 'echo $$ > foreground.pid; exec sleep 300'\r");
    let pids = ["shell.pid", "background.pid", "foreground.pid"]
        .map(|name| read_pid(&daemon.directory.join(name)));
    assert!(pids.iter().all(|pid| process_running(*pid)));
    control.end_surface(&session).unwrap();
    wait_status(&mut control, &session.id, SurfaceStatus::Exited);
    let started = Instant::now();
    let deadline = started + TIMEOUT;
    loop {
        let survivors: Vec<_> = pids
            .iter()
            .copied()
            .filter(|pid| process_running(*pid))
            .collect();
        if survivors.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {:?} waiting for session descendants to exit; survivors: {survivors:?}",
            started.elapsed()
        );
        thread::sleep(POLL_INTERVAL);
    }
    daemon.shutdown();
}

#[test]
fn daemon_restart_preserves_lost_surface_metadata_without_claiming_liveness() {
    let mut daemon = DaemonGuard::start();
    let mut control = daemon.client();
    let session = control.create_surface(80, 24, None).unwrap();
    let mut attached = Controller::attach(&daemon, &session, 80, 24);
    attached.input(b"printf 'BEFORE_CRASH_%s\\n' \"$((20+22))\"\r");
    attached.until("BEFORE_CRASH_42");
    drop(attached);
    drop(control);
    daemon.crash_and_restart();
    let mut control = daemon.client();
    let lost = wait_status(&mut control, &session.id, SurfaceStatus::Lost);
    assert!(!lost.attached);
    assert!(lost.error.is_some());
    let error = control
        .attach_surface(&lost, 80, 24)
        .unwrap_err()
        .to_string();
    assert!(error.contains("SurfaceUnavailable"), "{error}");
    let replacement = control.create_surface(80, 24, None).unwrap();
    let mut attached = Controller::attach(&daemon, &replacement, 80, 24);
    attached.input(b"printf 'AFTER_RESTART_%s\\n' \"$((20+22))\"\r");
    attached.until("AFTER_RESTART_42");
    daemon.shutdown();
}

#[test]
fn content_pane_survives_daemon_restart_and_closes_without_a_process() {
    let mut daemon = DaemonGuard::start();
    let mut control = daemon.client();
    let surface = control.create_surface(80, 24, None).unwrap();
    let terminal = control.workspace().unwrap().sessions[0].tabs[0]
        .layout
        .leaf_pane()
        .unwrap()
        .clone();
    let project = daemon.directory.join("project");
    let notes = project.join("notes.md").to_string_lossy().into_owned();
    let content = PaneContent::Files(FolderContent {
        environment: ContentEnvironment::Host,
        root: project.to_string_lossy().into_owned(),
        tabs: vec![compi_protocol::ContentTab::edit(notes.clone())],
        active: Some(compi_protocol::ContentTab::edit(notes)),
    });
    let opened = control
        .mutate(WorkspaceMutation::OpenContent {
            pane_id: terminal.clone(),
            axis: SplitAxis::Horizontal,
            geometry: SplitGeometry {
                width: 800.0,
                height: 480.0,
                min_width: 160.0,
                min_height: 80.0,
                divider: 4.0,
            },
            content: content.clone(),
        })
        .unwrap();
    assert_eq!(opened.operation_state, "committed");
    assert!(opened.affected_surfaces.is_empty());
    let content_leaf = LayoutNode::Content {
        pane_id: opened.affected_panes[1].clone(),
        content,
    };
    drop(control);
    daemon.crash_and_restart();

    let mut control = daemon.client();
    let lost = wait_status(&mut control, &surface.id, SurfaceStatus::Lost);
    let LayoutNode::Split { first, second, .. } = control.workspace().unwrap().sessions[0].tabs[0]
        .layout
        .clone()
    else {
        panic!("the tab keeps its terminal and content pane");
    };
    assert_eq!(first.leaf_pane(), Some(&terminal));
    assert_eq!(*second, content_leaf);

    // Closing content ends no process, even beside a running shell.
    control
        .mutate(WorkspaceMutation::RestartSurface {
            surface_id: surface.id.clone(),
            expected_lifetime: lost.process_lifetime_id,
            cols: 80,
            rows: 24,
        })
        .unwrap();
    wait_status(&mut control, &surface.id, SurfaceStatus::Running);
    let removed = control
        .mutate(WorkspaceMutation::RemovePane {
            pane_id: content_leaf.leaf_pane().unwrap().clone(),
        })
        .unwrap();
    assert_eq!(removed.operation_state, "removed");
    assert!(removed.affected_surfaces.is_empty());
    let workspace = control.workspace().unwrap();
    assert_eq!(
        workspace.sessions[0].tabs[0].layout.leaf_pane(),
        Some(&terminal)
    );
    assert_eq!(
        workspace.surface(&surface.id).unwrap().status,
        SurfaceStatus::Running
    );
    daemon.shutdown();
}

#[test]
fn terminal_view_survives_daemon_restart_and_leaves_the_shell_its_size() {
    let mut daemon = DaemonGuard::start();
    let mut control = daemon.client();
    let surface = control.create_surface(80, 24, None).unwrap();
    let mut attached = Controller::attach(&daemon, &surface, 80, 24);
    let terminal = control.workspace().unwrap().sessions[0].tabs[0]
        .layout
        .leaf_pane()
        .unwrap()
        .clone();
    let project = daemon.directory.join("project");
    let notes = project.join("notes.md").to_string_lossy().into_owned();
    let content = PaneContent::Files(FolderContent {
        environment: ContentEnvironment::Host,
        root: project.to_string_lossy().into_owned(),
        tabs: vec![compi_protocol::ContentTab::edit(notes.clone())],
        active: Some(compi_protocol::ContentTab::edit(notes)),
    });
    let shown = control
        .mutate(WorkspaceMutation::ShowView {
            pane_id: terminal.clone(),
            content: content.clone(),
        })
        .unwrap();
    assert_eq!(shown.operation_state, "committed");
    assert!(shown.affected_surfaces.is_empty());
    let leaf = LayoutNode::Pane {
        pane_id: terminal,
        surface_id: surface.id.clone(),
        view: Some(PaneView {
            content,
            side: PaneSide::View,
        }),
    };
    assert_eq!(
        control.workspace().unwrap().sessions[0].tabs[0].layout,
        leaf
    );
    // The shell keeps running under its view, at the size it had.
    attached.input(b"printf 'SIZE_%s\\n' \"$(stty size | tr ' ' x)\"\r");
    attached.until("SIZE_24x80");
    drop(attached);
    drop(control);
    daemon.crash_and_restart();

    let mut control = daemon.client();
    let lost = wait_status(&mut control, &surface.id, SurfaceStatus::Lost);
    assert_eq!((lost.cols, lost.rows), (80, 24));
    assert_eq!(
        control.workspace().unwrap().sessions[0].tabs[0].layout,
        leaf
    );
    daemon.shutdown();
}

#[test]
fn local_endpoint_is_private_and_duplicate_daemon_cannot_take_it_over() {
    let mut daemon = DaemonGuard::start();
    let names = compi_protocol::identity::instance_names(Some(&daemon.instance)).unwrap();
    let endpoint = std::path::Path::new(&names.pipe);
    let socket = fs::symlink_metadata(endpoint).unwrap();
    assert!(socket.file_type().is_socket());
    assert_eq!(socket.mode() & 0o777, 0o600);
    assert_eq!(socket.uid(), unsafe { libc::geteuid() });
    let parent = fs::symlink_metadata(endpoint.parent().unwrap()).unwrap();
    assert!(parent.is_dir());
    assert_eq!(parent.mode() & 0o777, 0o700);
    assert_eq!(parent.uid(), unsafe { libc::geteuid() });
    let mut duplicate = Command::new(env!("CARGO_BIN_EXE_compi-daemon"))
        .args(["--instance", &daemon.instance])
        .env("COMPI_DATA_DIR", &daemon.directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let deadline = started + TIMEOUT;
    let status = loop {
        if let Some(status) = duplicate.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = duplicate.kill();
            let _ = duplicate.wait();
            panic!(
                "timed out after {:?} waiting for duplicate daemon to reject the occupied instance {}",
                started.elapsed(),
                daemon.instance
            );
        }
        thread::sleep(POLL_INTERVAL);
    };
    assert!(!status.success());
    assert!(daemon.client().list_surfaces().unwrap().is_empty());
    daemon.shutdown();
}

#[test]
fn program_status_reaches_watchers_through_the_pty_and_follows_its_lifecycle() {
    let mut daemon = DaemonGuard::start();
    let mut control = daemon.client();
    let surface = control.create_surface(80, 24, None).unwrap();
    let mut watcher = daemon.client();
    watcher.watch_program_status().unwrap();
    assert!(watcher.program_status().is_empty());
    let mut attached = Controller::attach(&daemon, &surface, 80, 24);

    // Feature detection: the reply travels back into the program.
    attached.input(
        "printf '\\e]7501;?\\e\\\\'; IFS= read -rs -d '\\' -t 5 reply; r=${reply#*7501;}; echo \"REPLY<${r%$'\\e'}>\"\r",
    );
    attached
        .until("REPLY<?:states=idle,working,done,blocked,error:kinds=permission,question,auth>");

    // The bridge's next prompt ends work in progress.
    attached.input("printf '\\e]7501;state=working:app=fixture\\e\\\\'; sleep 1\r");
    let seen = wait_for_program_status(&mut watcher, &surface.id, "prompt clears", |records| {
        records.is_empty()
    });
    assert_eq!(root_states(&seen), [Some(ProgramState::Working), None]);

    attached.input(concat!(
        r"printf '\e]7501;state=blocked:kind=permission:app=fixture:msg=QXBwcm92ZT8=\e\\'; sleep 1; ",
        r"printf '\e]7501;state=working:app=fixture:progress=50\e\\'; sleep 1; ",
        r"printf '\e]7501;state=done:app=fixture:msg=RG9uZQ==\e\\'",
        "\r",
    ));
    let seen = wait_for_program_status(&mut watcher, &surface.id, "done", |records| {
        records
            .first()
            .is_some_and(|record| record.state == ProgramState::Done)
    });
    assert_eq!(
        root_states(&seen),
        [
            Some(ProgramState::Blocked),
            Some(ProgramState::Working),
            Some(ProgramState::Done)
        ]
    );
    let blocked = &seen[0][0];
    assert_eq!(blocked.kind, Some(BlockedKind::Permission));
    assert_eq!(blocked.message.as_deref(), Some("Approve?"));
    assert_eq!(seen[1][0].progress, Some(50));

    // A client connecting later is given what is current, which outlived the prompt.
    let mut reconnected = daemon.client();
    reconnected.watch_program_status().unwrap();
    let current = &reconnected.program_status()[&surface.id];
    assert_eq!(current.process_lifetime_id, surface.process_lifetime_id);
    assert_eq!(
        current.records,
        [ProgramStatusRecord {
            id: None,
            state: ProgramState::Done,
            kind: None,
            progress: None,
            app: Some("fixture".into()),
            title: None,
            message: Some("Done".into()),
        }]
    );
    reconnected.acknowledge_program_status(&surface).unwrap();
    wait_for_program_status(&mut watcher, &surface.id, "acknowledged", |records| {
        records.is_empty()
    });

    // Exit drops work in progress and keeps the failure for the user to find.
    attached.input(
        "printf '\\e]7501;state=error:app=fixture\\e\\\\\\e]7501;state=working:id=worker\\e\\\\'; sleep 1; exit\r",
    );
    let seen = wait_for_program_status(&mut watcher, &surface.id, "exit", |records| {
        records.len() == 1
    });
    assert_eq!(seen.first().map(Vec::len), Some(2));
    assert_eq!(root_states(&seen).last(), Some(&Some(ProgramState::Error)));
    wait_status(&mut control, &surface.id, SurfaceStatus::Exited);
    drop(attached);

    // Closing the pane removes what is left, even for a connection that never asks
    // for the workspace.
    let tab_id = control
        .workspace()
        .unwrap()
        .sessions
        .iter()
        .flat_map(|session| &session.tabs)
        .find(|tab| {
            matches!(&tab.layout, LayoutNode::Pane { surface_id, .. } if surface_id == &surface.id)
        })
        .unwrap()
        .id
        .clone();
    control
        .mutate(WorkspaceMutation::RemoveTab { tab_id })
        .unwrap();
    wait_for_program_status(&mut watcher, &surface.id, "pane closed", |records| {
        records.is_empty()
    });

    drop((control, watcher, reconnected));
    daemon.shutdown();
}

/// Every change to `surface_id`'s records, until `done` accepts one.
fn wait_for_program_status(
    client: &mut DaemonClient,
    surface_id: &SurfaceId,
    condition: &str,
    done: impl Fn(&[ProgramStatusRecord]) -> bool,
) -> Vec<Vec<ProgramStatusRecord>> {
    let started = Instant::now();
    let mut seen = Vec::new();
    loop {
        match client.poll_event().unwrap_or_else(|error| {
            panic!(
                "waiting for program status {condition} after {:?}: {error}",
                started.elapsed()
            )
        }) {
            Some(ServerEvent::Control {
                message: ServerMessage::ProgramStatusChanged { status },
                ..
            }) if &status.surface_id == surface_id => {
                let finished = done(&status.records);
                seen.push(status.records);
                if finished {
                    return seen;
                }
            }
            Some(ServerEvent::Control {
                message: ServerMessage::Error { code, message, .. },
                ..
            }) => panic!(
                "waiting for program status {condition} after {:?}: daemon error {code:?}: {message}",
                started.elapsed()
            ),
            Some(_) => {}
            None => thread::sleep(POLL_INTERVAL),
        }
        assert!(
            started.elapsed() < TIMEOUT,
            "missing program status {condition} after {:?}; saw {seen:?}",
            started.elapsed()
        );
    }
}

fn root_states(seen: &[Vec<ProgramStatusRecord>]) -> Vec<Option<ProgramState>> {
    seen.iter()
        .map(|records| {
            records
                .iter()
                .find(|record| record.id.is_none())
                .map(|record| record.state)
        })
        .collect()
}

#[test]
fn watched_system_metrics_report_the_daemon_and_send_details_once_per_connection() {
    let mut daemon = DaemonGuard::start();
    let daemon_pid = daemon.child.id();
    let mut watcher = daemon.client();
    watcher.watch_system_metrics(MonitorSource::Host).unwrap();
    let mut table = ProcessTable::default();

    // The first sample names every process; the daemon is the user's own.
    let first = next_sample(&mut watcher);
    assert!(first.logical_cpus >= 1);
    assert!(
        first
            .processes
            .iter()
            .all(|sample| sample.details.is_some())
    );
    let processes = table.apply(first.processes);
    let own = processes
        .iter()
        .find(|process| process.sample.pid == daemon_pid)
        .expect("the daemon samples itself");
    assert_eq!(own.details.owner, ProcessOwner::CurrentUser);
    assert!(own.details.name.starts_with("compi-daemon"));
    assert!(own.sample.memory_bytes.is_some_and(|bytes| bytes > 0));
    assert!(own.sample.threads.is_some_and(|threads| threads >= 1));

    // Later samples carry no details for processes already described, and measure CPU.
    let second = next_sample(&mut watcher);
    let own = second
        .processes
        .iter()
        .find(|sample| sample.pid == daemon_pid)
        .unwrap();
    assert!(own.details.is_none());
    assert!(own.cpu_percent.is_some());
    assert!(
        table
            .apply(second.processes)
            .iter()
            .any(|process| process.sample.pid == daemon_pid)
    );

    // Another connection is told everything again.
    let mut other = daemon.client();
    other.watch_system_metrics(MonitorSource::Host).unwrap();
    let sample = next_sample(&mut other);
    assert!(
        sample
            .processes
            .iter()
            .all(|sample| sample.details.is_some())
    );

    drop((watcher, other));
    daemon.shutdown();
}

#[test]
fn processes_in_a_surface_name_it_and_can_be_inspected_and_quit() {
    let mut daemon = DaemonGuard::start();
    let daemon_pid = daemon.child.id();
    let mut control = daemon.client();
    let session = control
        .create_surface(80, 24, Some(daemon.directory.to_str().unwrap().to_owned()))
        .unwrap();
    let mut attached = Controller::attach(&daemon, &session, 80, 24);
    attached.input(b"stty -echo; sleep 300 & echo $! > sleeper.pid\r");
    let pid = read_pid(&daemon.directory.join("sleeper.pid")) as u32;

    // A process in the surface's session names the surface; the daemon is in none.
    let mut watcher = daemon.client();
    watcher.watch_system_metrics(MonitorSource::Host).unwrap();
    let processes = ProcessTable::default().apply(next_sample(&mut watcher).processes);
    let sleeper = processes
        .iter()
        .find(|process| process.sample.pid == pid)
        .expect("the sleeper is sampled");
    assert_eq!(sleeper.details.surface.as_ref(), Some(&session.id));
    let own = processes
        .iter()
        .find(|process| process.sample.pid == daemon_pid)
        .unwrap();
    assert_eq!(own.details.surface, None);

    let started = sleeper.sample.start_time;
    let inspection = control
        .inspect_process(MonitorSource::Host, pid, started)
        .unwrap()
        .expect("the sleeper is running");
    assert_eq!(
        inspection.arguments,
        Some(vec!["sleep".to_owned(), "300".to_owned()])
    );
    assert_eq!(
        inspection.working_directory.map(PathBuf::from),
        Some(daemon.directory.canonicalize().unwrap())
    );
    assert_eq!(
        control
            .inspect_process(MonitorSource::Host, pid, started + 1)
            .unwrap(),
        None
    );

    // Quitting checks the start time first and never signals the daemon itself.
    let quit = |control: &mut DaemonClient, pid: u32, started: u64| {
        control
            .quit_process(MonitorSource::Host, pid, started, false)
            .unwrap()
    };
    assert_eq!(quit(&mut control, pid, started + 1), QuitOutcome::Gone);
    assert!(process_running(pid as libc::pid_t));
    assert!(matches!(
        quit(&mut control, daemon_pid, own.sample.start_time),
        QuitOutcome::Refused { .. }
    ));
    assert_eq!(quit(&mut control, pid, started), QuitOutcome::Sent);
    let deadline = Instant::now() + TIMEOUT;
    while process_running(pid as libc::pid_t) {
        assert!(Instant::now() < deadline, "the sleeper did not quit");
        thread::sleep(POLL_INTERVAL);
    }

    // A Unix daemon has no WSL distributions to watch.
    assert_eq!(control.monitor_sources().unwrap(), [MonitorSource::Host]);
    let wsl = MonitorSource::Wsl {
        distribution: "Ubuntu".into(),
    };
    let error = control.watch_system_totals(wsl).unwrap_err();
    assert_eq!(
        error.downcast::<DaemonError>().unwrap().code,
        ErrorCode::InvalidRequest
    );
    drop((attached, watcher));
    daemon.shutdown();
}

fn next_sample(client: &mut DaemonClient) -> SystemSample {
    let started = Instant::now();
    loop {
        match client.poll_event().unwrap() {
            Some(ServerEvent::Control {
                message: ServerMessage::SystemSampled { sample },
                ..
            }) => return sample,
            Some(_) => {}
            None => thread::sleep(POLL_INTERVAL),
        }
        assert!(
            started.elapsed() < TIMEOUT,
            "no system sample after {:?}",
            started.elapsed()
        );
    }
}

#[test]
fn system_totals_arrive_in_order_after_the_kept_history() {
    let mut daemon = DaemonGuard::start();
    let mut watcher = daemon.client();
    let (host, history) = watcher.watch_system_totals(MonitorSource::Host).unwrap();
    assert!(host.logical_cpus >= 1);
    // The daemon samples from the start, whether or not anyone watches.
    let mut previous = history.last().map(|sample| sample.sequence);
    let mut later = Vec::new();
    while later.len() < 2 {
        let sample = next_totals(&mut watcher);
        if let Some(previous) = previous {
            assert_eq!(sample.sequence, previous + 1);
        }
        previous = Some(sample.sequence);
        later.push(sample);
    }
    let last = later.last().unwrap();
    assert!(last.cpu.is_some(), "a later sample measures CPU");
    let memory = last.memory.as_ref().unwrap();
    assert!(memory.used_bytes > 0 && memory.used_bytes <= memory.total_bytes);
    assert!(memory.available_bytes.is_some());
    assert!(last.disk.as_ref().is_some_and(|disk| disk.total_bytes > 0));
    // Each sample sent carries the breakdowns as they are now.
    let detail = last.detail.as_ref().expect("the newest sample's detail");
    assert_eq!(detail.cores.len(), host.logical_cpus as usize);
    assert!(!detail.volumes.is_empty(), "at least the system volume");

    // A later watcher is given the history the first one saw arrive, with the detail on
    // the newest sample only.
    let mut other = daemon.client();
    let (_, history) = other.watch_system_totals(MonitorSource::Host).unwrap();
    assert!(
        history
            .iter()
            .any(|sample| sample.sequence == last.sequence)
    );
    let (newest, kept) = history.split_last().unwrap();
    assert!(newest.detail.is_some());
    assert!(kept.iter().all(|sample| sample.detail.is_none()));
    drop((watcher, other));
    daemon.shutdown();
}

fn next_totals(client: &mut DaemonClient) -> TotalsSample {
    let started = Instant::now();
    loop {
        match client.poll_event().unwrap() {
            Some(ServerEvent::Control {
                message: ServerMessage::SystemTotalsSampled { sample },
                ..
            }) => return sample,
            Some(_) => {}
            None => thread::sleep(POLL_INTERVAL),
        }
        assert!(
            started.elapsed() < TIMEOUT,
            "no totals sample after {:?}",
            started.elapsed()
        );
    }
}
