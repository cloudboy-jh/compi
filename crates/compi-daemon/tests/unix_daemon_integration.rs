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
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);
const TIMEOUT: Duration = Duration::from_secs(15);

struct DaemonGuard {
    child: Child,
    instance: String,
    directory: PathBuf,
}

impl DaemonGuard {
    fn start() -> Self {
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
        let deadline = Instant::now() + TIMEOUT;
        let names = compi_protocol::identity::instance_names(Some(&self.instance)).unwrap();
        loop {
            if compi_protocol::pipe::connect(&names.pipe, Duration::from_millis(100)).is_ok() {
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "daemon exited: {}",
                self.log()
            );
            assert!(
                Instant::now() < deadline,
                "daemon did not become ready: {}",
                self.log()
            );
            thread::sleep(Duration::from_millis(20));
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
        let deadline = Instant::now() + TIMEOUT;
        while self.child.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "daemon did not stop: {}",
                self.log()
            );
            thread::sleep(Duration::from_millis(20));
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
                thread::sleep(Duration::from_millis(20));
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
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let event = if let Some(message) = self.client.take_pending_screen() {
                Some(ServerEvent::Screen(message))
            } else {
                self.client.poll_event().unwrap()
            };
            match event {
                Some(ServerEvent::Screen(message)) => {
                    if matches!(self.mirror.apply(message), MirrorApply::Gap { .. }) {
                        self.client.request_snapshot().unwrap();
                    }
                    if let Some(snapshot) = self.mirror.snapshot()
                        && snapshot_text(snapshot).contains(marker)
                    {
                        return snapshot.clone();
                    }
                }
                Some(ServerEvent::Control {
                    message: ServerMessage::Error { code, message, .. },
                    ..
                }) => {
                    panic!("daemon error {code:?}: {message}");
                }
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "missing {marker:?}; last screen: {:?}",
                self.mirror.snapshot().map(snapshot_text)
            );
            thread::sleep(Duration::from_millis(5));
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
    let deadline = Instant::now() + TIMEOUT;
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
            "expected {expected:?}, got {surface:?}"
        );
        thread::sleep(Duration::from_millis(20));
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
    let deadline = Instant::now() + TIMEOUT;
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
            "disconnected controller remained attached"
        );
        thread::sleep(Duration::from_millis(20));
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
fn natural_exit_retains_exit_status_and_rejects_reattach() {
    let mut daemon = DaemonGuard::start();
    let mut control = daemon.client();
    let session = control.create_surface(80, 24, None).unwrap();
    let mut attached = Controller::attach(&daemon, &session, 80, 24);
    attached.input(b"printf 'NATURAL_%s\\n' \"$((20+22))\"; exit 23\r");
    let exited = wait_status(&mut control, &session.id, SurfaceStatus::Exited);
    assert_eq!(exited.exit_code, Some(23));
    assert!(!exited.attached);
    let error = control
        .attach_surface(&exited, 80, 24)
        .unwrap_err()
        .to_string();
    assert!(error.contains("SurfaceUnavailable"), "{error}");
    daemon.shutdown();
}

fn read_pid(path: &std::path::Path) -> libc::pid_t {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Ok(contents) = fs::read_to_string(path)
            && let Ok(pid) = contents.trim().parse::<libc::pid_t>()
        {
            assert!(pid > 1);
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "no PID recorded at {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(20));
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
    let deadline = Instant::now() + TIMEOUT;
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
            "session descendants survived explicit kill: {survivors:?}"
        );
        thread::sleep(Duration::from_millis(20));
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
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        if let Some(status) = duplicate.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = duplicate.kill();
            let _ = duplicate.wait();
            panic!("duplicate daemon did not reject the occupied instance");
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert!(!status.success());
    assert!(daemon.client().list_surfaces().unwrap().is_empty());
    daemon.shutdown();
}
