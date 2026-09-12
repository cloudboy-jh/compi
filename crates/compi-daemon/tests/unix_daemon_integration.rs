#![cfg(unix)]

use compi_client::{MirrorApply, ScreenMirror};
use compi_protocol::{
    ClientMessage, DaemonClient, PROTOCOL_VERSION, ScreenSnapshot, ServerEvent, ServerMessage,
    SurfaceId, SurfaceInfo, SurfaceStatus,
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
            })
            .unwrap();
    }

    fn until(&mut self, marker: &str) -> ScreenSnapshot {
        let started = Instant::now();
        let deadline = started + TIMEOUT;
        loop {
            // A previous wait may already have consumed the frame containing
            // this marker. Check the replica even when no new event arrives.
            if let Some(snapshot) = self.mirror.snapshot()
                && snapshot_text(snapshot).contains(marker)
            {
                return snapshot.clone();
            }
            assert!(
                Instant::now() < deadline,
                "timed out after {:?} waiting for {marker:?}; last screen: {:?}",
                started.elapsed(),
                self.mirror.snapshot().map(snapshot_text)
            );
            let event = if let Some(message) = self.client.take_pending_screen() {
                Some(ServerEvent::Screen(message))
            } else {
                self.client.poll_event().unwrap_or_else(|error| {
                    panic!(
                        "waiting for {marker:?} failed after {:?}: {error}; last screen: {:?}",
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
                        "waiting for {marker:?} after {:?}: daemon error {code:?}: {message}",
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
    snapshot
        .scrollback
        .iter()
        .chain(&snapshot.cells)
        .map(|row| {
            row.cells
                .iter()
                .filter(|cell| cell.width != 0)
                .map(|cell| cell.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    attached.input(b"printf 'RESIZED_%s ' \"$((20+22))\"; stty size\r");
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
