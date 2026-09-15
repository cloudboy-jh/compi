use compi_protocol::{DaemonClient, IMAGE_UPLOAD_CHUNK_BYTES};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn stdio_relay_disconnect_does_not_own_daemon_lifetime() {
    let instance = format!("relay-{}", std::process::id());
    let executable = env!("CARGO_BIN_EXE_compi-daemon");

    let mut first_command = Command::new(executable);
    first_command
        .arg("--server-stdio")
        .arg("--instance")
        .arg(&instance);
    let mut first = DaemonClient::connect_command(&mut first_command).unwrap();
    let first_workspace = first.workspace().unwrap();
    let image = vec![0x5a; IMAGE_UPLOAD_CHUNK_BYTES + 17];
    let uploaded = first.upload_image("relay-test.png", &image).unwrap();
    assert_eq!(std::fs::read(&uploaded).unwrap(), image);
    drop(first);

    let mut local = DaemonClient::connect(Some(&instance), Duration::from_secs(2)).unwrap();
    assert_eq!(
        local.workspace().unwrap().server_id,
        first_workspace.server_id
    );
    drop(local);

    let mut second_command = Command::new(executable);
    second_command
        .arg("--server-stdio")
        .arg("--instance")
        .arg(&instance);
    let mut second = DaemonClient::connect_command(&mut second_command).unwrap();
    assert_eq!(
        second.workspace().unwrap().server_id,
        first_workspace.server_id
    );
    assert_eq!(
        second.upload_image("relay-test.png", &image).unwrap(),
        uploaded
    );
    second.shutdown_daemon().unwrap();
    drop(second);

    let deadline = Instant::now() + Duration::from_secs(10);
    while DaemonClient::connect(Some(&instance), Duration::from_millis(50)).is_ok() {
        assert!(Instant::now() < deadline, "relay-owned daemon did not stop");
        thread::sleep(Duration::from_millis(50));
    }
    std::fs::remove_file(uploaded).unwrap();
}
