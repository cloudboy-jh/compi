#[cfg(windows)]
fn main() {
    if let Err(error) = run() {
        eprintln!("compi-daemon: {error}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn run() -> compi::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => compi::daemon::run(None),
        [flag, instance] if flag == "--instance" && !instance.is_empty() => {
            compi::daemon::run(Some(instance))
        }
        [flag] if flag == "--check-system" => compi::wsl::ensure_default_wsl2(),
        [flag] if flag == "--shutdown" => shutdown_daemon(),
        [flag] if flag == "--supervise" => {
            compi::supervisor::supervise(&std::env::current_exe()?)
        }
        [flag] if flag == "--install-task" => {
            compi::supervisor::install(&std::env::current_exe()?)?;
            println!("registered {}", compi::supervisor::TASK_NAME);
            Ok(())
        }
        [flag, path, user_sid] if flag == "--write-task-xml" => {
            compi::supervisor::write_task_xml(
                &std::env::current_exe()?,
                std::path::Path::new(path),
                user_sid,
            )
        }
        [flag, path] if flag == "--remove-task-xml" => {
            compi::supervisor::remove_task_xml(std::path::Path::new(path))
        }
        [flag] if flag == "--uninstall-task" => {
            compi::supervisor::uninstall()?;
            println!("removed {}", compi::supervisor::TASK_NAME);
            Ok(())
        }
        [flag] if flag == "--activate-task" => compi::supervisor::activate(),
        _ => Err(
            "usage: compi-daemon [--instance <name> | --check-system | --shutdown | --supervise | --install-task | --uninstall-task | --activate-task | --write-task-xml <path> <user-sid> | --remove-task-xml <path>]"
                .into(),
        ),
    }
}

#[cfg(windows)]
fn shutdown_daemon() -> compi::Result<()> {
    use compi::client::DaemonClient;
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

#[cfg(not(windows))]
fn main() {
    eprintln!("compi-daemon only runs on Windows");
    std::process::exit(1);
}
