#![cfg(windows)]

use compi_client::{MirrorApply, ScreenMirror};
use compi_protocol::frame;
use compi_protocol::{
    CONTROL_FRAME, ClientControl, ClientMessage, Color, ErrorCode, MutationId, MutationRequest,
    ScreenMessage, ScreenSnapshot, ServerMessage, SurfaceId, SurfaceStatus, WorkspaceMutation,
    decode_server, encode_client,
};
use compi_protocol::{DaemonClient, ServerEvent, identity, pipe};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    GetProcessHandleCount, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

static INSTANCE_COUNTER: AtomicU64 = AtomicU64::new(1);
static DAEMON_LOCK: Mutex<()> = Mutex::new(());
const TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

struct DaemonGuard {
    child: Child,
    instance: String,
    _serial: MutexGuard<'static, ()>,
}

impl DaemonGuard {
    fn start() -> Self {
        Self::start_instance(unique_instance())
    }

    fn start_instance(instance: String) -> Self {
        let serial = DAEMON_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let child = Self::spawn(&instance);
        let mut daemon = Self {
            child,
            instance,
            _serial: serial,
        };
        daemon.wait_ready();
        daemon
    }

    fn spawn(instance: &str) -> Child {
        Command::new(env!("CARGO_BIN_EXE_compi-daemon"))
            .args(["--instance", instance])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap()
    }

    fn wait_ready(&mut self) {
        let started = Instant::now();
        loop {
            let connection =
                DaemonClient::connect(Some(&self.instance), Duration::from_millis(100));
            if connection.is_ok() {
                return;
            }
            let status = self.child.try_wait().unwrap();
            assert!(
                status.is_none() && started.elapsed() < TIMEOUT,
                "daemon {} not ready after {:?}; process: {status:?}; connection: {:?}",
                self.instance,
                started.elapsed(),
                connection.err()
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn client(&self) -> DaemonClient {
        DaemonClient::connect(Some(&self.instance), Duration::from_secs(2)).unwrap()
    }

    fn shutdown(&mut self) {
        let mut client = self.client();
        client.shutdown_daemon().unwrap();
        let started = Instant::now();
        while self.child.try_wait().unwrap().is_none() {
            assert!(
                started.elapsed() < TIMEOUT,
                "daemon {} still running after {:?}",
                self.instance,
                started.elapsed()
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn crash(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }

    fn restart(&mut self) {
        assert!(self.child.try_wait().unwrap().is_some());
        self.child = Self::spawn(&self.instance);
        self.wait_ready();
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
fn persistent_multi_surface_lifecycle() {
    let mut daemon = DaemonGuard::start();
    let instance = daemon.instance.clone();
    reject_incompatible_protocol(&daemon.instance);

    let mut control = daemon.client();
    assert!(control.list_surfaces().unwrap().is_empty());
    let first = control.create_surface(80, 24, None).unwrap();
    let output_directory = std::env::temp_dir().join(format!("compi-output-{instance}"));
    fs::create_dir_all(&output_directory).unwrap();
    let second = control
        .create_surface(
            80,
            24,
            Some(output_directory.to_string_lossy().into_owned()),
        )
        .unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(control.list_surfaces().unwrap().len(), 2);

    let mut first_client = daemon.client();
    first_client.attach_surface(&first, 80, 24).unwrap();
    first_client
        .send(ClientMessage::Input {
            data: b"echo FIRST_$((20+22))\rexit\r".to_vec(),
            latency_id: None,
        })
        .unwrap();
    let (first_output, first_exit) = collect_until_exit(&daemon, &mut first_client, &first.id);
    assert!(first_output.windows(8).any(|bytes| bytes == b"FIRST_42"));
    assert_eq!(first_exit, 0);

    let mut second_client = daemon.client();
    second_client.attach_surface(&second, 100, 40).unwrap();
    let mut competing = daemon.client();
    let conflict = competing
        .attach_surface(&second, 80, 24)
        .unwrap_err()
        .to_string();
    assert!(conflict.contains("AlreadyAttached"));

    second_client
        .request(ClientMessage::Input {
            data: b"for ((i=0;i<3000;i++)); do if [ \"$(stty size)\" = '40 100' ]; then stty size; echo ATTACH_SIZE_$((20+22)); break; fi; sleep 0.01; done\r".to_vec(),
            latency_id: None,
        })
        .unwrap();
    let attach_output = collect_until_marker(&mut second_client, b"ATTACH_SIZE_42");
    assert!(attach_output.windows(6).any(|bytes| bytes == b"40 100"));
    let sequence = second_client.request_snapshot().unwrap();
    let recovered = read_snapshot(&mut second_client, sequence);
    assert_eq!(recovered.sequence, sequence);
    assert!(snapshot_text(&recovered).contains("ATTACH_SIZE_42"));

    second_client
        .request(ClientMessage::Resize {
            cols: 120,
            rows: 50,
        })
        .unwrap();
    second_client
        .request(ClientMessage::Input {
            data: b"for ((i=0;i<3000;i++)); do if [ \"$(stty size)\" = '50 120' ]; then stty size; echo ACTIVE_SIZE_$((20+22)); break; fi; sleep 0.01; done\r".to_vec(),
            latency_id: None,
        })
        .unwrap();
    let resize_output = collect_until_marker(&mut second_client, b"ACTIVE_SIZE_42");
    assert!(resize_output.windows(6).any(|bytes| bytes == b"50 120"));
    second_client
        .request(ClientMessage::Input {
            data: "printf '\\033[31mANSI_%s\\033[0m UNICODE_%s\\n' RED λ\r"
                .as_bytes()
                .to_vec(),
            latency_id: None,
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
            data: b"printf '\\033[?1049hALT_%s\\n' SCREEN; read -r reply; printf '\\033[?1049lALT_%s:%s\\n' RETURNED \"$reply\"\r".to_vec(),
            latency_id: None,
        })
        .unwrap();
    let alternate = collect_snapshot_until_marker(&mut second_client, b"ALT_SCREEN");
    assert!(alternate.modes.alternate_screen);
    second_client
        .request(ClientMessage::Input {
            data: b"interactive-complete\r".to_vec(),
            latency_id: None,
        })
        .unwrap();
    let main =
        collect_snapshot_until_marker(&mut second_client, b"ALT_RETURNED:interactive-complete");
    assert!(!main.modes.alternate_screen);

    second_client.request(ClientMessage::Detach).unwrap();
    drop(second_client);
    let mut reattached = daemon.client();
    reattached.attach_surface(&second, 90, 30).unwrap();
    reattached
        .request(ClientMessage::Input {
            data: b"for ((i=0;i<3000;i++)); do if [ -e detached-release ]; then echo REATTACHED_$((20+22)); printf done > detached-complete; break; fi; sleep 0.01; done\r".to_vec(),
            latency_id: None,
        })
        .unwrap();
    drop(reattached);
    wait_for_attachment(&mut control, &second.id, false);
    fs::write(output_directory.join("detached-release"), b"release").unwrap();
    wait_for_file(&output_directory.join("detached-complete"));
    let mut after_crash = daemon.client();
    after_crash.attach_surface(&second, 90, 30).unwrap();
    let crash_output = collect_until_marker(&mut after_crash, b"REATTACHED_42");
    assert!(
        crash_output
            .windows(13)
            .any(|bytes| bytes == b"REATTACHED_42")
    );

    after_crash
        .send(ClientMessage::Input {
            data: b"printf -v flood '%1024s' ''; flood=${flood// /X}; for ((i=0;i<16384;i++)); do printf '\\033[H%08d:%s' \"$i\" \"$flood\"; done; printf '\\r\\n'; printf done > flood-complete\r".to_vec(),
            latency_id: None,
        })
        .unwrap();
    // Do not read or request on after_crash until the shell has written over
    // 16 MiB through the PTY. The file is an independent completion channel.
    // Repaint in place so this measures transport pressure, not unbounded
    // scrollback/reflow work in an unrelated debug-build throughput benchmark.
    wait_for_file(&output_directory.join("flood-complete"));
    assert!(
        control
            .list_surfaces()
            .unwrap()
            .iter()
            .find(|session| session.id == second.id)
            .is_some_and(|session| session.attached),
        "output backpressure disconnected the attached client"
    );
    after_crash
        .send(ClientMessage::Input {
            data: b"echo FLOOD_RECOVERED_$((20+22))\r".to_vec(),
            latency_id: None,
        })
        .unwrap();
    let flood_output = collect_until_marker(&mut after_crash, b"FLOOD_RECOVERED_42");
    assert!(
        flood_output
            .windows(18)
            .any(|bytes| bytes == b"FLOOD_RECOVERED_42")
    );
    after_crash
        .send(ClientMessage::Input {
            data: b"exit\r".to_vec(),
            latency_id: None,
        })
        .unwrap();
    let (_, flood_exit) = collect_until_exit(&daemon, &mut after_crash, &second.id);
    assert_eq!(flood_exit, 0);
    drop(after_crash);

    wait_for_attachment(&mut control, &first.id, false);
    wait_for_attachment(&mut control, &second.id, false);
    let sessions = control.list_surfaces().unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().all(|session| {
        matches!(
            session.status,
            SurfaceStatus::Exited | SurfaceStatus::Failed
        ) && !session.attached
    }));

    drop(control);
    daemon.shutdown();
    cleanup_metadata(&instance);
    fs::remove_dir_all(output_directory).unwrap();
}

#[test]
fn creates_surfaces_in_wsl_and_windows_working_directories() {
    let mut daemon = DaemonGuard::start();
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
        .create_surface(80, 24, Some(requested.clone()))
        .unwrap();
    let directory = windows_session.working_directory.as_ref().unwrap();
    assert_eq!(directory.requested, requested);
    assert!(directory.resolved_wsl_path.starts_with('/'));

    let mut attached = daemon.client();
    attached.attach_surface(&windows_session, 80, 24).unwrap();
    attached
        .request(ClientMessage::Input {
            data: b"printf '\\033]7;file://localhost%s\\a' \"$PWD\"; echo WORKDIR_$((20+22))\r"
                .to_vec(),
            latency_id: None,
        })
        .unwrap();
    let snapshot = collect_snapshot_until_marker(&mut attached, b"WORKDIR_42");
    assert_eq!(
        snapshot.current_directory.as_deref(),
        Some(directory.resolved_wsl_path.as_str())
    );
    attached
        .send(ClientMessage::Input {
            data: b"exit\r".to_vec(),
            latency_id: None,
        })
        .unwrap();
    collect_until_exit(&daemon, &mut attached, &windows_session.id);
    drop(attached);

    let wsl_session = control
        .create_surface(80, 24, Some("/tmp".to_owned()))
        .unwrap();
    assert_eq!(
        wsl_session
            .working_directory
            .as_ref()
            .map(|directory| directory.resolved_wsl_path.as_str()),
        Some("/tmp")
    );
    control.end_surface(&wsl_session).unwrap();
    let invalid = control
        .create_surface(
            80,
            24,
            Some("/definitely-missing-compi-working-directory".to_owned()),
        )
        .unwrap();
    assert_eq!(invalid.status, SurfaceStatus::Failed);
    assert!(
        invalid
            .error
            .as_deref()
            .is_some_and(|error| error.contains("does not exist"))
    );

    drop(control);
    daemon.shutdown();
    cleanup_metadata(&instance);
    fs::remove_dir_all(windows_directory).unwrap();
}

#[test]
fn repeated_surface_cycles_release_daemon_process_handles() {
    let mut daemon = DaemonGuard::start();
    let instance = daemon.instance.clone();
    let mut control = daemon.client();
    let mut cycle_client = daemon.client();
    let baseline = process_handle_count(daemon.child.id());

    for _ in 0..12 {
        let session = control.create_surface(80, 24, None).unwrap();
        cycle_client.attach_surface(&session, 80, 24).unwrap();
        cycle_client
            .request(ClientMessage::Resize {
                cols: 100,
                rows: 32,
            })
            .unwrap();
        cycle_client.request(ClientMessage::Detach).unwrap();
        control.end_surface(&session).unwrap();
        let started = Instant::now();
        let condition = format!("surface {:?} to exit", session.id);
        loop {
            let status = query_surfaces(&mut control, started, &condition)
                .into_iter()
                .find(|candidate| candidate.id == session.id)
                .unwrap()
                .status;
            if matches!(status, SurfaceStatus::Exited | SurfaceStatus::Failed) {
                break;
            }
            assert!(
                started.elapsed() < TIMEOUT,
                "surface {:?} not exited after {:?}; last status: {status:?}",
                session.id,
                started.elapsed()
            );
            thread::sleep(POLL_INTERVAL);
        }
    }
    for _ in 0..20 {
        let mut transient = daemon.client();
        transient.list_surfaces().unwrap();
    }

    let started = Instant::now();
    loop {
        let final_count = process_handle_count(daemon.child.id());
        if final_count <= baseline + 4 {
            break;
        }
        assert!(
            started.elapsed() < TIMEOUT,
            "daemon process handles did not return to baseline {baseline} + 4 after {:?}; last count: {final_count}",
            started.elapsed()
        );
        thread::sleep(POLL_INTERVAL);
    }

    drop(control);
    daemon.shutdown();
    cleanup_metadata(&instance);
}

#[test]
fn daemon_restart_reports_active_surfaces_as_lost() {
    let mut daemon = DaemonGuard::start();
    let mut client = daemon.client();
    let lost = client.create_surface(80, 24, None).unwrap();
    drop(client);

    let instance = daemon.instance.clone();
    daemon.crash();
    daemon.restart();
    let mut client = daemon.client();
    let dead = client
        .list_surfaces()
        .unwrap()
        .into_iter()
        .find(|session| session.id == lost.id)
        .expect("lost session metadata was not retained");
    assert_eq!(dead.status, SurfaceStatus::Lost);
    assert!(!dead.attached);
    assert!(dead.error.as_deref().unwrap().contains("previous daemon"));

    let attach_error = client
        .attach_surface(&dead, 80, 24)
        .unwrap_err()
        .to_string();
    assert!(attach_error.contains("SurfaceUnavailable"));

    let replacement = client.create_surface(80, 24, None).unwrap();
    drop(client);
    daemon.shutdown();

    daemon.restart();
    let mut client = daemon.client();
    let replacement = client
        .list_surfaces()
        .unwrap()
        .into_iter()
        .find(|session| session.id == replacement.id)
        .expect("intentional shutdown metadata was not retained");
    assert_eq!(replacement.status, SurfaceStatus::Lost);
    assert!(
        replacement
            .error
            .as_deref()
            .unwrap()
            .contains("previous daemon")
    );
    drop(client);
    daemon.shutdown();
    cleanup_metadata(&instance);
}

#[test]
fn mutation_publication_precedes_ack_and_restart_changes_lifetime() {
    let mut daemon = DaemonGuard::start();
    let instance = daemon.instance.clone();
    let mut client = daemon.client();
    let workspace = client.workspace().unwrap();
    let mutation_id = MutationId::new("integration-initialize");
    let request_id = client
        .send(ClientMessage::Mutate {
            mutation: MutationRequest {
                server_id: workspace.server_id,
                expected_generation: workspace.server_generation,
                mutation_id: mutation_id.clone(),
                expected_revision: workspace.revision,
                operation: WorkspaceMutation::Initialize {
                    cols: 80,
                    rows: 24,
                    working_directory: None,
                },
                launch: None,
            },
        })
        .unwrap();
    let mut published_revision = None;
    let started = Instant::now();
    let mut last_event = None;
    let receipt = loop {
        let event = client.poll_event().unwrap_or_else(|error| {
            panic!(
                "waiting for MutationCommitted({request_id}) after {:?}: {error}; published: {published_revision:?}; last event: {last_event:?}",
                started.elapsed()
            )
        });
        match event {
            Some(ServerEvent::Control {
                request_id: None,
                message: ServerMessage::WorkspaceChanged { revision },
            }) => published_revision = Some(revision),
            Some(ServerEvent::Control {
                request_id: Some(response_id),
                message: ServerMessage::MutationCommitted { receipt },
            }) if response_id == request_id => break receipt,
            Some(event) => last_event = Some(describe_event(event)),
            None => thread::sleep(POLL_INTERVAL),
        }
        assert!(
            started.elapsed() < TIMEOUT,
            "missing MutationCommitted({request_id}) after {:?}; published: {published_revision:?}; last event: {last_event:?}",
            started.elapsed()
        );
    };
    assert_eq!(published_revision, Some(receipt.revision));
    assert_eq!(
        client.mutation_outcome(mutation_id).unwrap(),
        receipt,
        "outcome lookup must return the durable receipt"
    );

    let surface_id = receipt.affected_surfaces[0].clone();
    let original = client.wait_for_surface(&surface_id, TIMEOUT).unwrap();
    client.end_surface(&original).unwrap();
    let exited = wait_for_surface_status(&mut client, &surface_id, SurfaceStatus::Exited);
    client
        .mutate(WorkspaceMutation::RestartSurface {
            surface_id: surface_id.clone(),
            expected_lifetime: exited.process_lifetime_id.clone(),
            cols: 80,
            rows: 24,
        })
        .unwrap();
    let restarted = client.wait_for_surface(&surface_id, TIMEOUT).unwrap();
    assert_eq!(restarted.id, original.id);
    assert_ne!(restarted.process_lifetime_id, original.process_lifetime_id);
    let stale = client
        .attach_surface(&original, 80, 24)
        .unwrap_err()
        .to_string();
    assert!(stale.contains("StaleLifetime"), "{stale}");
    client.attach_surface(&restarted, 80, 24).unwrap();
    client.request(ClientMessage::Detach).unwrap();
    client.end_surface(&restarted).unwrap();
    wait_for_surface_status(&mut client, &surface_id, SurfaceStatus::Exited);

    drop(client);
    daemon.shutdown();
    cleanup_metadata(&instance);
}

fn wait_for_surface_status(
    client: &mut DaemonClient,
    surface_id: &SurfaceId,
    expected: SurfaceStatus,
) -> compi_protocol::SurfaceInfo {
    let started = Instant::now();
    let condition = format!("surface {surface_id:?} status {expected:?}");
    loop {
        let surface = query_surfaces(client, started, &condition)
            .into_iter()
            .find(|surface| &surface.id == surface_id)
            .unwrap();
        if surface.status == expected {
            return surface;
        }
        assert!(
            started.elapsed() < TIMEOUT,
            "surface {surface_id:?}: expected {expected:?} after {:?}; last state: {surface:?}",
            started.elapsed()
        );
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_attachment(client: &mut DaemonClient, surface_id: &SurfaceId, attached: bool) {
    let started = Instant::now();
    let condition = format!("surface {surface_id:?} attached={attached}");
    loop {
        let surface = query_surfaces(client, started, &condition)
            .into_iter()
            .find(|surface| &surface.id == surface_id);
        if surface
            .as_ref()
            .is_some_and(|surface| surface.attached == attached)
        {
            return;
        }
        assert!(
            started.elapsed() < TIMEOUT,
            "surface {surface_id:?}: expected attached={attached} after {:?}; last state: {surface:?}",
            started.elapsed()
        );
        thread::sleep(POLL_INTERVAL);
    }
}

fn query_surfaces(
    client: &mut DaemonClient,
    started: Instant,
    condition: &str,
) -> Vec<compi_protocol::SurfaceInfo> {
    let request_id = client.send(ClientMessage::GetWorkspace).unwrap();
    let mut last_event = None;
    loop {
        match poll_event(client, started, condition) {
            Some(ServerEvent::Control {
                request_id: Some(response_id),
                message: ServerMessage::Workspace { workspace },
            }) if response_id == request_id => return workspace.surfaces,
            Some(event) => last_event = Some(describe_event(event)),
            None => thread::sleep(POLL_INTERVAL),
        }
        assert!(
            started.elapsed() < TIMEOUT,
            "waiting for {condition}: missing workspace response after {:?}; last event: {last_event:?}",
            started.elapsed()
        );
    }
}

fn wait_for_file(path: &Path) {
    let started = Instant::now();
    loop {
        let contents = fs::read(path);
        if contents
            .as_deref()
            .is_ok_and(|contents| contents == b"done")
        {
            return;
        }
        assert!(
            started.elapsed() < TIMEOUT,
            "missing completion marker {path:?} after {:?}; last file state: {contents:?}",
            started.elapsed()
        );
        thread::sleep(POLL_INTERVAL);
    }
}

#[test]
fn daemon_quarantines_malformed_workspace_metadata() {
    let instance = unique_instance();
    let path = metadata_path(&instance);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"{not-json").unwrap();

    let mut daemon = DaemonGuard::start_instance(instance.clone());
    let mut client = daemon.client();
    let workspace = client.workspace().unwrap();
    assert!(workspace.surfaces.is_empty());
    assert!(
        workspace
            .recovery_message
            .as_deref()
            .is_some_and(|message| message.contains("quarantined"))
    );
    assert!(path.is_file());
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
        .join(format!("workspace-{instance}-v1.json"))
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
        target: None,
        message: ClientMessage::Hello {
            protocol_version: 999,
        },
    })
    .unwrap();
    frame::write(&mut connection, CONTROL_FRAME, &payload).unwrap();
    let started = Instant::now();
    let mut reader = pipe::PipeReader::default();
    let response = loop {
        if let Some(response) = reader.poll(&connection).unwrap() {
            break response;
        }
        assert!(
            started.elapsed() < TIMEOUT,
            "missing incompatible-protocol response after {:?}; last state: no complete frame",
            started.elapsed()
        );
        thread::sleep(POLL_INTERVAL);
    };
    let response = decode_server(&response.payload).unwrap();
    assert!(matches!(
        response.message,
        ServerMessage::Error {
            code: ErrorCode::IncompatibleProtocol,
            ..
        }
    ));
}

fn read_snapshot(client: &mut DaemonClient, sequence: u64) -> ScreenSnapshot {
    let started = Instant::now();
    let mut last_event = None;
    let condition = format!("snapshot sequence {sequence}");
    loop {
        let event = poll_event(client, started, &condition);
        match event {
            Some(ServerEvent::Screen(ScreenMessage::Snapshot { snapshot }))
                if snapshot.sequence == sequence =>
            {
                return snapshot;
            }
            Some(event) => last_event = Some(describe_event(event)),
            None => thread::sleep(POLL_INTERVAL),
        }
        assert!(
            started.elapsed() < TIMEOUT,
            "missing snapshot sequence {sequence} after {:?}; last event/output: {last_event:?}",
            started.elapsed()
        );
    }
}

fn collect_until_marker(client: &mut DaemonClient, marker: &[u8]) -> Vec<u8> {
    snapshot_text(&collect_snapshot_until_marker(client, marker)).into_bytes()
}

fn collect_snapshot_until_marker(client: &mut DaemonClient, marker: &[u8]) -> ScreenSnapshot {
    let mut mirror = ScreenMirror::default();
    let mut recovering = false;
    let started = Instant::now();
    let condition = format!("screen marker {:?}", String::from_utf8_lossy(marker));
    loop {
        match poll_event(client, started, &condition) {
            Some(ServerEvent::Screen(message)) => {
                apply_screen(client, &mut mirror, &mut recovering, message);
                let output = mirror_text(&mirror);
                if !recovering && output.windows(marker.len()).any(|bytes| bytes == marker) {
                    return mirror.snapshot().unwrap().clone();
                }
            }
            Some(_) => {}
            None => thread::sleep(POLL_INTERVAL),
        }
        assert!(
            started.elapsed() < TIMEOUT,
            "missing {condition} after {:?}; recovering: {recovering}; last output: {:?}",
            started.elapsed(),
            String::from_utf8_lossy(&mirror_text(&mirror))
        );
    }
}

fn collect_until_exit(
    daemon: &DaemonGuard,
    client: &mut DaemonClient,
    surface_id: &SurfaceId,
) -> (Vec<u8>, u32) {
    let mut mirror = ScreenMirror::default();
    let started = Instant::now();
    let condition = format!("SurfaceExited for {surface_id:?}");
    let exit_code = loop {
        match poll_event(client, started, &condition) {
            Some(ServerEvent::Screen(message)) => {
                // Exit releases the controller. Do not issue gap recovery on
                // an attachment that may already have been retired.
                mirror.apply(message);
            }
            Some(ServerEvent::Control {
                message:
                    ServerMessage::SurfaceExited {
                        identity,
                        exit_code,
                    },
                ..
            }) if &identity.surface_id == surface_id => break exit_code,
            Some(_) => {}
            None => thread::sleep(POLL_INTERVAL),
        }
        assert!(
            started.elapsed() < TIMEOUT,
            "missing {condition} after {:?}; last output: {:?}",
            started.elapsed(),
            String::from_utf8_lossy(&mirror_text(&mirror))
        );
    };
    let mut final_client = daemon.client();
    let condition = format!("read-only final screen for {surface_id:?} after exit {exit_code}");
    let exited = query_surfaces(&mut final_client, started, &condition)
        .into_iter()
        .find(|surface| &surface.id == surface_id)
        .expect("exited surface metadata must be retained");
    let request_id = final_client
        .send(ClientMessage::Attach {
            surface_id: exited.id,
            expected_lifetime: exited.process_lifetime_id,
            cols: exited.cols,
            rows: exited.rows,
        })
        .unwrap();
    let mut sequence = None;
    let mut last_event = None;
    loop {
        match poll_event(&mut final_client, started, &condition) {
            Some(ServerEvent::Control {
                request_id: Some(response_id),
                message:
                    ServerMessage::Attached {
                        sequence: baseline, ..
                    },
            }) if response_id == request_id => sequence = Some(baseline),
            Some(ServerEvent::Screen(ScreenMessage::Snapshot { snapshot }))
                if sequence == Some(snapshot.sequence) =>
            {
                return (snapshot_text(&snapshot).into_bytes(), exit_code);
            }
            Some(event) => last_event = Some(describe_event(event)),
            None => thread::sleep(POLL_INTERVAL),
        }
        assert!(
            started.elapsed() < TIMEOUT,
            "missing {condition} after {:?}; last event: {last_event:?}",
            started.elapsed()
        );
    }
}

fn poll_event(client: &mut DaemonClient, started: Instant, condition: &str) -> Option<ServerEvent> {
    let event = if let Some(pending) = client.take_pending_screen() {
        Some(ServerEvent::Screen(pending))
    } else {
        client.poll_event().unwrap_or_else(|error| {
            panic!(
                "waiting for {condition} after {:?}: {error}",
                started.elapsed()
            )
        })
    };
    if let Some(ServerEvent::Control {
        message: ServerMessage::Error { code, message, .. },
        ..
    }) = &event
    {
        panic!(
            "waiting for {condition} after {:?}: daemon error ({code:?}): {message}",
            started.elapsed()
        );
    }
    event
}

fn describe_event(event: ServerEvent) -> String {
    match event {
        ServerEvent::Control {
            request_id,
            message,
        } => {
            format!("control {request_id:?}: {message:?}")
        }
        ServerEvent::Screen(message) => format!("screen {message:?}"),
    }
}

fn apply_screen(
    client: &mut DaemonClient,
    mirror: &mut ScreenMirror,
    recovering: &mut bool,
    message: ScreenMessage,
) {
    if matches!(message, ScreenMessage::Snapshot { .. }) {
        *recovering = false;
    }
    if matches!(mirror.apply(message), MirrorApply::Gap { .. }) && !*recovering {
        client.send(ClientMessage::RequestSnapshot).unwrap();
        *recovering = true;
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
