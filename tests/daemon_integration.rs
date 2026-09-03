#![cfg(windows)]

use compi::client::{DaemonClient, ServerEvent};
use compi::frame;
use compi::identity;
use compi::pipe;
use compi::protocol::{
    CONTROL_FRAME, ClientControl, ClientMessage, ErrorCode, ServerMessage, SessionStatus,
    decode_server, encode_client,
};
use compi::terminal::{Color, MirrorApply, ScreenMessage, ScreenMirror, ScreenSnapshot};
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    GetProcessHandleCount, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

static INSTANCE_COUNTER: AtomicU64 = AtomicU64::new(1);

struct DaemonGuard {
    child: Child,
    instance: String,
}

impl DaemonGuard {
    fn start() -> Self {
        Self::start_instance(unique_instance())
    }

    fn start_instance(instance: String) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_compi-daemon"))
            .args(["--instance", &instance])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if DaemonClient::connect(Some(&instance), Duration::from_millis(100)).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "daemon did not become ready");
            thread::sleep(Duration::from_millis(50));
        }
        Self { child, instance }
    }

    fn client(&self) -> DaemonClient {
        DaemonClient::connect(Some(&self.instance), Duration::from_secs(2)).unwrap()
    }

    fn shutdown(mut self) {
        let mut client = self.client();
        client.shutdown_daemon().unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "daemon did not stop");
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn crash(mut self) -> String {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
        self.instance.clone()
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn persistent_multi_session_lifecycle() {
    let daemon = DaemonGuard::start();
    let instance = daemon.instance.clone();
    reject_incompatible_protocol(&daemon.instance);

    let mut control = daemon.client();
    assert!(control.list_sessions().unwrap().is_empty());
    let first = control.create_session(80, 24, None).unwrap();
    let second = control.create_session(80, 24, None).unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(control.list_sessions().unwrap().len(), 2);

    let mut first_client = daemon.client();
    assert!(matches!(
        first_client
            .request(ClientMessage::Attach {
                session_id: first.id.clone(),
                cols: 80,
                rows: 24,
            })
            .unwrap(),
        ServerMessage::Attached { .. }
    ));
    first_client
        .request(ClientMessage::Input {
            data: b"echo FIRST_$((20+22))\rexit\r".to_vec(),
        })
        .unwrap();
    let (first_output, first_exit) = collect_until_exit(&mut first_client, &first.id);
    assert!(first_output.windows(8).any(|bytes| bytes == b"FIRST_42"));
    assert_eq!(first_exit, 0);

    let mut second_client = daemon.client();
    second_client
        .request(ClientMessage::Attach {
            session_id: second.id.clone(),
            cols: 100,
            rows: 40,
        })
        .unwrap();
    let mut competing = daemon.client();
    let conflict = competing
        .request(ClientMessage::Attach {
            session_id: second.id.clone(),
            cols: 80,
            rows: 24,
        })
        .unwrap_err()
        .to_string();
    assert!(conflict.contains("AlreadyAttached"));

    second_client
        .request(ClientMessage::Input {
            data: b"stty size; echo ATTACH_SIZE_$((20+22))\r".to_vec(),
        })
        .unwrap();
    let attach_output = collect_until_marker(&mut second_client, b"ATTACH_SIZE_42");
    assert!(attach_output.windows(6).any(|bytes| bytes == b"40 100"));
    let sequence = second_client.request_snapshot().unwrap();
    let recovered = read_snapshot(&mut second_client);
    assert_eq!(recovered.sequence, sequence);
    assert!(snapshot_text(&recovered).contains("ATTACH_SIZE_42"));

    second_client
        .request(ClientMessage::Resize {
            cols: 120,
            rows: 50,
        })
        .unwrap();
    thread::sleep(Duration::from_millis(200));
    second_client
        .request(ClientMessage::Input {
            data: b"stty size; echo ACTIVE_SIZE_$((20+22))\r".to_vec(),
        })
        .unwrap();
    let resize_output = collect_until_marker(&mut second_client, b"ACTIVE_SIZE_42");
    assert!(resize_output.windows(6).any(|bytes| bytes == b"50 120"));
    second_client
        .request(ClientMessage::Input {
            data: "printf '\\033[31mANSI_RED\\033[0m UNICODE_λ\\n'\r"
                .as_bytes()
                .to_vec(),
        })
        .unwrap();
    let styled = collect_snapshot_until_marker(&mut second_client, b"UNICODE_\xce\xbb");
    assert!(
        styled
            .cells
            .iter()
            .flat_map(|row| &row.cells)
            .any(|cell| { cell.text == "A" && cell.foreground == Color::Indexed(1) })
    );

    second_client
        .request(ClientMessage::Input {
            data: b"export TERM=xterm-256color; tput smcup; printf 'ALT_SCREEN'\r".to_vec(),
        })
        .unwrap();
    let alternate = collect_snapshot_until_marker(&mut second_client, b"ALT_SCREEN");
    assert!(alternate.modes.alternate_screen);
    second_client
        .request(ClientMessage::Input {
            data: b"tput rmcup; echo ALT_RETURNED\r".to_vec(),
        })
        .unwrap();
    let main = collect_snapshot_until_marker(&mut second_client, b"ALT_RETURNED");
    assert!(!main.modes.alternate_screen);

    second_client
        .request(ClientMessage::Input {
            data: b"TERM=xterm-256color top\r".to_vec(),
        })
        .unwrap();
    let top = collect_snapshot_until_marker(&mut second_client, b"Tasks:");
    assert!(snapshot_text(&top).contains("%Cpu"));
    second_client
        .request(ClientMessage::Input {
            data: b"q".to_vec(),
        })
        .unwrap();
    thread::sleep(Duration::from_millis(200));
    second_client
        .request(ClientMessage::Input {
            data: b"echo TOP_RETURNED\r".to_vec(),
        })
        .unwrap();
    collect_until_marker(&mut second_client, b"TOP_RETURNED");

    second_client.request(ClientMessage::Detach).unwrap();
    drop(second_client);
    let mut reattached = daemon.client();
    reattached
        .request(ClientMessage::Attach {
            session_id: second.id.clone(),
            cols: 90,
            rows: 30,
        })
        .unwrap();
    reattached
        .request(ClientMessage::Input {
            data: b"sleep 1; echo REATTACHED_$((20+22))\r".to_vec(),
        })
        .unwrap();
    drop(reattached);
    thread::sleep(Duration::from_secs(2));
    let mut after_crash = daemon.client();
    after_crash
        .request(ClientMessage::Attach {
            session_id: second.id.clone(),
            cols: 90,
            rows: 30,
        })
        .unwrap();
    let crash_output = collect_until_marker(&mut after_crash, b"REATTACHED_42");
    assert!(
        crash_output
            .windows(13)
            .any(|bytes| bytes == b"REATTACHED_42")
    );

    after_crash
        .request(ClientMessage::Input {
            data: b"yes COMPI_FLOOD\r".to_vec(),
        })
        .unwrap();
    thread::sleep(Duration::from_secs(3));
    assert!(
        control
            .list_sessions()
            .unwrap()
            .iter()
            .find(|session| session.id == second.id)
            .is_some_and(|session| session.attached),
        "output backpressure disconnected the attached client"
    );
    after_crash
        .request(ClientMessage::Input { data: vec![3] })
        .unwrap();
    after_crash
        .request(ClientMessage::Input {
            data: b"echo FLOOD_INTERRUPTED\r".to_vec(),
        })
        .unwrap();
    let flood_output = collect_until_marker(&mut after_crash, b"FLOOD_INTERRUPTED");
    assert!(
        flood_output
            .windows(17)
            .any(|bytes| bytes == b"FLOOD_INTERRUPTED")
    );
    after_crash
        .request(ClientMessage::Input {
            data: b"exit\r".to_vec(),
        })
        .unwrap();
    let (_, flood_exit) = collect_until_exit(&mut after_crash, &second.id);
    assert_eq!(flood_exit, 0);
    drop(after_crash);

    let sessions = control.list_sessions().unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().all(|session| {
        matches!(
            session.status,
            SessionStatus::Exited | SessionStatus::Failed
        ) && !session.attached
    }));

    drop(control);
    daemon.shutdown();
    cleanup_metadata(&instance);
}

#[test]
fn creates_sessions_in_wsl_and_windows_working_directories() {
    let daemon = DaemonGuard::start();
    let instance = daemon.instance.clone();
    let windows_directory = std::env::temp_dir().join(format!(
        "compi-working-directory-{}-Agent Projects-π",
        std::process::id()
    ));
    fs::create_dir_all(&windows_directory).unwrap();
    let mut requested = windows_directory.to_string_lossy().into_owned();
    if requested.as_bytes().get(1) == Some(&b':') {
        let drive = requested[..1].to_ascii_lowercase();
        requested.replace_range(..1, &drive);
    }

    let mut control = daemon.client();
    let windows_session = control
        .create_session(80, 24, Some(requested.clone()))
        .unwrap();
    let directory = windows_session.working_directory.as_ref().unwrap();
    assert_eq!(directory.requested, requested);
    assert!(directory.resolved_wsl_path.starts_with('/'));

    let mut attached = daemon.client();
    attached
        .request(ClientMessage::Attach {
            session_id: windows_session.id.clone(),
            cols: 80,
            rows: 24,
        })
        .unwrap();
    attached
        .request(ClientMessage::Input {
            data: b"printf '\\033]7;file://localhost%s\\a' \"$PWD\"; echo WORKDIR_$((20+22))\r"
                .to_vec(),
        })
        .unwrap();
    let snapshot = collect_snapshot_until_marker(&mut attached, b"WORKDIR_42");
    assert_eq!(
        snapshot.current_directory.as_deref(),
        Some(directory.resolved_wsl_path.as_str())
    );
    attached
        .request(ClientMessage::Input {
            data: b"exit\r".to_vec(),
        })
        .unwrap();
    collect_until_exit(&mut attached, &windows_session.id);
    drop(attached);

    let wsl_session = control
        .create_session(80, 24, Some("/tmp".to_owned()))
        .unwrap();
    assert_eq!(
        wsl_session
            .working_directory
            .as_ref()
            .map(|directory| directory.resolved_wsl_path.as_str()),
        Some("/tmp")
    );
    control.kill_session(wsl_session.id).unwrap();
    assert!(
        control
            .create_session(
                80,
                24,
                Some("/definitely-missing-compi-working-directory".to_owned())
            )
            .unwrap_err()
            .to_string()
            .contains("does not exist")
    );

    drop(control);
    daemon.shutdown();
    cleanup_metadata(&instance);
    fs::remove_dir_all(windows_directory).unwrap();
}

#[test]
fn repeated_session_cycles_release_daemon_process_handles() {
    let daemon = DaemonGuard::start();
    let instance = daemon.instance.clone();
    let mut control = daemon.client();
    let mut cycle_client = daemon.client();
    let baseline = process_handle_count(daemon.child.id());

    for _ in 0..12 {
        let session = control.create_session(80, 24, None).unwrap();
        cycle_client
            .request(ClientMessage::Attach {
                session_id: session.id.clone(),
                cols: 80,
                rows: 24,
            })
            .unwrap();
        cycle_client
            .request(ClientMessage::Resize {
                cols: 100,
                rows: 32,
            })
            .unwrap();
        cycle_client.request(ClientMessage::Detach).unwrap();
        control.kill_session(session.id.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = control
                .list_sessions()
                .unwrap()
                .into_iter()
                .find(|candidate| candidate.id == session.id)
                .unwrap()
                .status;
            if matches!(status, SessionStatus::Exited | SessionStatus::Failed) {
                break;
            }
            assert!(Instant::now() < deadline, "killed session did not exit");
            thread::sleep(Duration::from_millis(25));
        }
    }
    for _ in 0..20 {
        let mut transient = daemon.client();
        transient.list_sessions().unwrap();
    }

    thread::sleep(Duration::from_millis(250));
    let final_count = process_handle_count(daemon.child.id());
    assert!(
        final_count <= baseline + 4,
        "daemon process handles grew from {baseline} to {final_count}"
    );

    drop(control);
    daemon.shutdown();
    cleanup_metadata(&instance);
}

#[test]
fn daemon_restart_reports_lost_sessions_as_dead() {
    let daemon = DaemonGuard::start();
    let mut client = daemon.client();
    let lost = client.create_session(80, 24, None).unwrap();
    drop(client);

    let instance = daemon.crash();
    let restarted = DaemonGuard::start_instance(instance.clone());
    let mut client = restarted.client();
    let dead = client
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == lost.id)
        .expect("lost session metadata was not retained");
    assert_eq!(dead.status, SessionStatus::Dead);
    assert!(!dead.attached);
    assert!(dead.error.as_deref().unwrap().contains("previous daemon"));

    let attach_error = client
        .request(ClientMessage::Attach {
            session_id: lost.id,
            cols: 80,
            rows: 24,
        })
        .unwrap_err()
        .to_string();
    assert!(attach_error.contains("SessionExited"));

    let replacement = client.create_session(80, 24, None).unwrap();
    drop(client);
    restarted.shutdown();

    let restarted = DaemonGuard::start_instance(instance.clone());
    let mut client = restarted.client();
    let replacement = client
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.id == replacement.id)
        .expect("intentional shutdown metadata was not retained");
    assert_eq!(replacement.status, SessionStatus::Dead);
    assert!(
        replacement
            .error
            .as_deref()
            .unwrap()
            .contains("stopped intentionally")
    );
    drop(client);
    restarted.shutdown();
    cleanup_metadata(&instance);
}

#[test]
fn daemon_quarantines_malformed_session_metadata() {
    let instance = unique_instance();
    let path = metadata_path(&instance);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"{not-json").unwrap();

    let daemon = DaemonGuard::start_instance(instance.clone());
    assert!(daemon.client().list_sessions().unwrap().is_empty());
    assert!(!path.exists());
    let prefix = path.file_stem().unwrap().to_string_lossy().into_owned();
    assert!(fs::read_dir(path.parent().unwrap()).unwrap().any(|entry| {
        entry.ok().is_some_and(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("{prefix}.corrupt-"))
        })
    }));
    daemon.shutdown();
    cleanup_metadata(&instance);
}

fn unique_instance() -> String {
    format!(
        "i{:x}{:x}",
        std::process::id(),
        INSTANCE_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn metadata_path(instance: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap())
        .join("Compi")
        .join(format!("sessions-{instance}-v2.json"))
}

fn cleanup_metadata(instance: &str) {
    let path = metadata_path(instance);
    let prefix = path.file_stem().unwrap().to_string_lossy().into_owned();
    if let Ok(entries) = fs::read_dir(path.parent().unwrap()) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

fn process_handle_count(process_id: u32) -> u32 {
    let process =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }.unwrap();
    let mut count = 0;
    unsafe { GetProcessHandleCount(process, &mut count) }.unwrap();
    unsafe { CloseHandle(process) }.unwrap();
    count
}

fn reject_incompatible_protocol(instance: &str) {
    let names = identity::instance_names(Some(instance)).unwrap();
    let mut connection = pipe::connect(&names.pipe, Duration::from_secs(2)).unwrap();
    let payload = encode_client(&ClientControl {
        request_id: 9,
        message: ClientMessage::Hello {
            protocol_version: 999,
        },
    })
    .unwrap();
    frame::write(&mut connection, CONTROL_FRAME, &payload).unwrap();
    let response = frame::read(&mut connection).unwrap().unwrap();
    let response = decode_server(&response.payload).unwrap();
    assert!(matches!(
        response.message,
        ServerMessage::Error {
            code: ErrorCode::IncompatibleProtocol,
            ..
        }
    ));
}

fn read_snapshot(client: &mut DaemonClient) -> ScreenSnapshot {
    loop {
        let event = if let Some(pending) = client.take_pending_screen() {
            Some(ServerEvent::Screen(pending))
        } else {
            client.read_event().unwrap()
        };
        match event.unwrap() {
            ServerEvent::Screen(ScreenMessage::Snapshot { snapshot }) => return snapshot,
            ServerEvent::Control {
                message: ServerMessage::Error { code, message },
                ..
            } => panic!("daemon error ({code:?}): {message}"),
            _ => {}
        }
    }
}

fn collect_until_marker(client: &mut DaemonClient, marker: &[u8]) -> Vec<u8> {
    snapshot_text(&collect_snapshot_until_marker(client, marker)).into_bytes()
}

fn collect_snapshot_until_marker(client: &mut DaemonClient, marker: &[u8]) -> ScreenSnapshot {
    let mut mirror = ScreenMirror::default();
    loop {
        let message = if let Some(pending) = client.take_pending_screen() {
            Some(ServerEvent::Screen(pending))
        } else {
            client.read_event().unwrap()
        };
        match message.unwrap() {
            ServerEvent::Screen(message) => {
                apply_screen(client, &mut mirror, message);
                let output = mirror_text(&mirror);
                if output.windows(marker.len()).any(|bytes| bytes == marker) {
                    return mirror.snapshot().unwrap().clone();
                }
            }
            ServerEvent::Control {
                message: ServerMessage::Error { code, message },
                ..
            } => panic!("daemon error ({code:?}): {message}"),
            ServerEvent::Control { .. } => {}
        }
    }
}

fn collect_until_exit(client: &mut DaemonClient, session_id: &str) -> (Vec<u8>, u32) {
    let mut mirror = ScreenMirror::default();
    loop {
        let message = if let Some(pending) = client.take_pending_screen() {
            Some(ServerEvent::Screen(pending))
        } else {
            client.read_event().unwrap()
        };
        match message.unwrap() {
            ServerEvent::Screen(message) => apply_screen(client, &mut mirror, message),
            ServerEvent::Control {
                message:
                    ServerMessage::SessionExited {
                        session_id: exited,
                        exit_code,
                    },
                ..
            } if exited == session_id => return (mirror_text(&mirror), exit_code),
            ServerEvent::Control { .. } => {}
        }
    }
}

fn apply_screen(client: &mut DaemonClient, mirror: &mut ScreenMirror, message: ScreenMessage) {
    if matches!(mirror.apply(message), MirrorApply::Gap { .. }) {
        client.request_snapshot().unwrap();
    }
}

fn mirror_text(mirror: &ScreenMirror) -> Vec<u8> {
    let Some(snapshot) = mirror.snapshot() else {
        return Vec::new();
    };
    snapshot_text(snapshot).into_bytes()
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
