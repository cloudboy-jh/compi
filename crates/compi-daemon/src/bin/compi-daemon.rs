#[cfg(any(windows, unix))]
fn main() {
    if let Err(error) = run() {
        eprintln!("compi-daemon: {error}");
        std::process::exit(1);
    }
}

#[cfg(any(windows, unix))]
fn run() -> compi_daemon::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => compi_daemon::daemon::run(None),
        [flag, instance] if flag == "--instance" && !instance.is_empty() => {
            compi_daemon::daemon::run(Some(instance))
        }
        [flag] if flag == "--check-system" => compi_daemon::launch::check_system(),
        [flag] if flag == "--shutdown" => shutdown_daemon(),
        #[cfg(windows)]
        [flag] if flag == "--supervise" => {
            compi_daemon::supervisor::supervise(&std::env::current_exe()?)
        }
        #[cfg(windows)]
        [flag] if flag == "--install-task" => {
            compi_daemon::supervisor::install(&std::env::current_exe()?)?;
            println!("registered {}", compi_daemon::supervisor::TASK_NAME);
            Ok(())
        }
        #[cfg(windows)]
        [flag, path, user_sid] if flag == "--write-task-xml" => {
            compi_daemon::supervisor::write_task_xml(
                &std::env::current_exe()?,
                std::path::Path::new(path),
                user_sid,
            )
        }
        #[cfg(windows)]
        [flag, path] if flag == "--remove-task-xml" => {
            compi_daemon::supervisor::remove_task_xml(std::path::Path::new(path))
        }
        #[cfg(windows)]
        [flag] if flag == "--uninstall-task" => {
            compi_daemon::supervisor::uninstall()?;
            println!("removed {}", compi_daemon::supervisor::TASK_NAME);
            Ok(())
        }
        #[cfg(windows)]
        [flag] if flag == "--activate-task" => compi_daemon::supervisor::activate(),
        #[cfg(unix)]
        _ => Err("usage: compi-daemon [--instance <name> | --check-system | --shutdown]".into()),
        #[cfg(windows)]
        _ => Err(
            "usage: compi-daemon [--instance <name> | --check-system | --shutdown | --supervise | --install-task | --uninstall-task | --activate-task | --write-task-xml <path> <user-sid> | --remove-task-xml <path>]"
                .into(),
        ),
    }
}

#[cfg(any(windows, unix))]
fn shutdown_daemon() -> compi_daemon::Result<()> {
    use compi_protocol::DaemonClient;
    use std::thread;
    use std::time::{Duration, Instant};

    let Ok(mut client) = DaemonClient::connect(None, Duration::from_millis(250)) else {
        return Ok(());
    };
    client.shutdown_daemon()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while DaemonClient::connect(None, Duration::from_millis(100)).is_ok() {
        if Instant::now() >= deadline {
            return Err("daemon did not stop within ten seconds".into());
        }
        thread::sleep(Duration::from_millis(50));
    }
    thread::sleep(Duration::from_millis(250));
    Ok(())
}

#[cfg(not(any(windows, unix)))]
fn main() {
    eprintln!("compi-daemon requires Windows or Unix");
    std::process::exit(1);
}
