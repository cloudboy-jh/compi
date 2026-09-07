use crate::Result;
#[cfg(windows)]
use crate::console;
use crate::{DaemonClient, MirrorApply, ScreenMirror, ServerEvent};
use compi_protocol::{
    ClientMessage, PaneId, ScreenMessage, ServerMessage, SessionId, SplitAxis, SurfaceId,
    SurfaceInfo, SurfaceStatus, TabId, WorkspaceMutation,
};
use std::env;
use std::fs::{self, OpenOptions};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
};

pub fn run() -> Result<()> {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let instance = if args.first().is_some_and(|arg| arg == "--instance") {
        if args.len() < 2 {
            return Err("--instance requires a name".into());
        }
        let instance = args.remove(1);
        args.remove(0);
        Some(instance)
    } else {
        None
    };
    let instance = instance.as_deref();

    match args.first().map(String::as_str) {
        None => start(instance, None),
        Some("start") => start(instance, optional_tail(&args, 1)?),
        Some("daemon-start") => start_daemon(instance),
        Some("workspace") => workspace(instance),
        Some("session") => session_command(instance, &args[1..]),
        Some("tab") => tab_command(instance, &args[1..]),
        Some("pane") => pane_command(instance, &args[1..]),
        Some("surface") => surface_command(instance, &args[1..]),
        Some("soak") => {
            if args.len() != 2 {
                return Err("soak requires exactly one duration in seconds".into());
            }
            let seconds = args[1]
                .parse::<u64>()
                .map_err(|_| "soak duration must be a positive integer")?;
            if seconds == 0 {
                return Err("soak duration must be a positive integer".into());
            }
            soak(instance, Duration::from_secs(seconds))
        }
        Some("shutdown") => shutdown(instance),
        Some("check-system") | Some("--check-system") => check_system(),
        Some("help") | Some("--help") | Some("-h") => {
            usage();
            Ok(())
        }
        Some(command) => Err(format!("unknown command {command:?}; run compi-probe help").into()),
    }
}

fn usage() {
    println!(
        "compi-probe - workspace protocol diagnostic client\n\n\
         Usage:\n  compi-probe start [working-directory]\n  \
         compi-probe workspace\n  \
         compi-probe session create <label>\n  \
         compi-probe session rename <session-id> <label>\n  \
         compi-probe session remove <session-id>\n  \
         compi-probe tab create <session-id> <label> [working-directory]\n  \
         compi-probe tab rename <tab-id> <label>\n  \
         compi-probe tab move <session-id> <tab-id> <index>\n  \
         compi-probe tab remove <tab-id>\n  \
         compi-probe pane split-right|split-down <pane-id> [working-directory]\n  \
         compi-probe pane remove <pane-id>\n  \
         compi-probe surface attach|inspect|end|restart <surface-id>\n  \
         compi-probe soak <seconds>\n  \
         compi-probe shutdown\n  \
         compi-probe check-system\n\n\
         Add `--instance <name>` before the command for an isolated daemon.\n\
         Press Ctrl+] to detach without stopping the shell."
    );
}

fn start(instance: Option<&str>, working_directory: Option<String>) -> Result<()> {
    let mut client = connect_or_start(instance)?;
    let (cols, rows) = console::dimensions();
    let surface = client.create_surface(cols, rows, working_directory)?;
    if surface.status != SurfaceStatus::Running {
        return Err(surface
            .error
            .unwrap_or_else(|| format!("surface is {:?}", surface.status).to_lowercase())
            .into());
    }
    console::attach(client, surface)
}

fn workspace(instance: Option<&str>) -> Result<()> {
    let mut client = DaemonClient::connect(instance, Duration::from_secs(2))?;
    let workspace = client.workspace()?;
    println!("{}", serde_json::to_string_pretty(&workspace)?);
    Ok(())
}

fn session_command(instance: Option<&str>, args: &[String]) -> Result<()> {
    let mut client = DaemonClient::connect(instance, Duration::from_secs(2))?;
    let operation = match args.first().map(String::as_str) {
        Some("create") if args.len() == 2 => WorkspaceMutation::CreateSession {
            label: args[1].clone(),
        },
        Some("rename") if args.len() == 3 => WorkspaceMutation::RenameSession {
            session_id: SessionId::new(args[1].clone()),
            label: args[2].clone(),
        },
        Some("remove") if args.len() == 2 => WorkspaceMutation::RemoveSession {
            session_id: SessionId::new(args[1].clone()),
        },
        _ => return Err("invalid session command; run compi-probe help".into()),
    };
    print_receipt(client.mutate(operation)?)
}

fn tab_command(instance: Option<&str>, args: &[String]) -> Result<()> {
    let mut client = DaemonClient::connect(instance, Duration::from_secs(2))?;
    let (cols, rows) = console::dimensions();
    let operation = match args.first().map(String::as_str) {
        Some("create") if matches!(args.len(), 3 | 4) => WorkspaceMutation::CreateTab {
            session_id: SessionId::new(args[1].clone()),
            label: args[2].clone(),
            cols,
            rows,
            working_directory: args.get(3).cloned(),
        },
        Some("rename") if args.len() == 3 => WorkspaceMutation::RenameTab {
            tab_id: TabId::new(args[1].clone()),
            label: args[2].clone(),
        },
        Some("move") if args.len() == 4 => WorkspaceMutation::MoveTab {
            session_id: SessionId::new(args[1].clone()),
            tab_id: TabId::new(args[2].clone()),
            index: args[3]
                .parse()
                .map_err(|_| "tab index must be a non-negative integer")?,
        },
        Some("remove") if args.len() == 2 => WorkspaceMutation::RemoveTab {
            tab_id: TabId::new(args[1].clone()),
        },
        _ => return Err("invalid tab command; run compi-probe help".into()),
    };
    print_receipt(client.mutate(operation)?)
}

fn pane_command(instance: Option<&str>, args: &[String]) -> Result<()> {
    let mut client = DaemonClient::connect(instance, Duration::from_secs(2))?;
    let (cols, rows) = console::dimensions();
    let operation = match args.first().map(String::as_str) {
        Some("split-right") | Some("split-down") if matches!(args.len(), 2 | 3) => {
            WorkspaceMutation::SplitPane {
                pane_id: PaneId::new(args[1].clone()),
                axis: if args[0] == "split-right" {
                    SplitAxis::Horizontal
                } else {
                    SplitAxis::Vertical
                },
                cols,
                rows,
                working_directory: args.get(2).cloned(),
            }
        }
        Some("remove") if args.len() == 2 => WorkspaceMutation::RemovePane {
            pane_id: PaneId::new(args[1].clone()),
        },
        _ => return Err("invalid pane command; run compi-probe help".into()),
    };
    print_receipt(client.mutate(operation)?)
}

fn surface_command(instance: Option<&str>, args: &[String]) -> Result<()> {
    if args.len() != 2 {
        return Err("surface command requires an action and surface ID".into());
    }
    let surface_id = SurfaceId::new(args[1].clone());
    match args[0].as_str() {
        "attach" => {
            let mut client = DaemonClient::connect(instance, Duration::from_secs(2))?;
            let surface = find_surface(&mut client, &surface_id)?;
            console::attach(client, surface)
        }
        "inspect" => inspect(instance, surface_id),
        "end" => {
            let mut client = DaemonClient::connect(instance, Duration::from_secs(2))?;
            let surface = find_surface(&mut client, &surface_id)?;
            print_receipt(client.end_surface(&surface)?)
        }
        "restart" => {
            let mut client = DaemonClient::connect(instance, Duration::from_secs(2))?;
            let surface = find_surface(&mut client, &surface_id)?;
            let (cols, rows) = console::dimensions();
            let receipt = client.mutate(WorkspaceMutation::RestartSurface {
                surface_id: surface.id,
                expected_lifetime: surface.process_lifetime_id,
                cols,
                rows,
            })?;
            print_receipt(receipt)
        }
        _ => Err("unknown surface action; run compi-probe help".into()),
    }
}

fn optional_tail(args: &[String], required: usize) -> Result<Option<String>> {
    if args.len() > required + 1 {
        return Err("too many command arguments".into());
    }
    Ok(args.get(required).cloned())
}

fn find_surface(client: &mut DaemonClient, id: &SurfaceId) -> Result<SurfaceInfo> {
    client
        .workspace()?
        .surface(id)
        .cloned()
        .ok_or_else(|| format!("surface {id} was not found").into())
}

fn print_receipt(receipt: compi_protocol::MutationReceipt) -> Result<()> {
    println!(
        "mutation={} revision={} state={} surfaces={}",
        receipt.mutation_id,
        receipt.revision,
        receipt.operation_state,
        receipt
            .affected_surfaces
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
    Ok(())
}

fn inspect(instance: Option<&str>, surface_id: SurfaceId) -> Result<()> {
    let mut client = DaemonClient::connect(instance, Duration::from_secs(2))?;
    let surface = find_surface(&mut client, &surface_id)?;
    client.attach_surface(&surface, surface.cols, surface.rows)?;
    loop {
        let event = if let Some(message) = client.take_pending_screen() {
            Some(ServerEvent::Screen(message))
        } else {
            client.read_event()?
        };
        match event {
            Some(ServerEvent::Screen(ScreenMessage::Snapshot { snapshot })) => {
                println!("{}", serde_json::to_string_pretty(&snapshot)?);
                let _ = client.request(ClientMessage::Detach);
                return Ok(());
            }
            Some(ServerEvent::Control {
                message: ServerMessage::Error { code, message, .. },
                ..
            }) => return Err(format!("daemon error ({code:?}): {message}").into()),
            Some(_) => {}
            None => return Err("daemon disconnected before sending a snapshot".into()),
        }
    }
}

fn soak(instance: Option<&str>, duration: Duration) -> Result<()> {
    let mut client = DaemonClient::connect(instance, Duration::from_secs(2))?;
    let surface = client.create_surface(100, 30, None)?;
    client.attach_surface(&surface, 100, 30)?;
    client.request(ClientMessage::Input {
        data: b"i=0; while :; do printf 'COMPI_SOAK_%08d 0123456789abcdefghijklmnopqrstuvwxyz\\n' \"$i\"; i=$((i+1)); sleep 0.02; done\r".to_vec(),
        latency_id: None,
    })?;

    let deadline = Instant::now() + duration;
    let mut mirror = ScreenMirror::default();
    let mut frames = 0_u64;
    let mut gaps = 0_u64;
    while Instant::now() < deadline {
        let event = if let Some(message) = client.take_pending_screen() {
            Some(ServerEvent::Screen(message))
        } else {
            client.read_event()?
        };
        match event {
            Some(ServerEvent::Screen(message)) => {
                frames = frames.saturating_add(1);
                if matches!(mirror.apply(message), MirrorApply::Gap { .. }) {
                    gaps = gaps.saturating_add(1);
                    client.request_snapshot()?;
                }
            }
            Some(ServerEvent::Control {
                message: ServerMessage::SurfaceExited { exit_code, .. },
                ..
            }) => return Err(format!("soak shell exited early with code {exit_code}").into()),
            Some(ServerEvent::Control {
                message: ServerMessage::Error { code, message, .. },
                ..
            }) => return Err(format!("daemon error ({code:?}): {message}").into()),
            Some(_) => {}
            None => return Err("daemon disconnected during soak".into()),
        }
    }

    client.request(ClientMessage::Input {
        data: vec![3],
        latency_id: None,
    })?;
    client.request(ClientMessage::Input {
        data: b"true; exit\r".to_vec(),
        latency_id: None,
    })?;
    loop {
        match client.read_event()? {
            Some(ServerEvent::Control {
                message:
                    ServerMessage::SurfaceExited {
                        identity,
                        exit_code,
                    },
                ..
            }) if identity.surface_id == surface.id => {
                if exit_code != 0 {
                    return Err(format!("soak shell exited with code {exit_code}").into());
                }
                println!(
                    "surface={} frames={frames} recovered_gaps={gaps}",
                    surface.id
                );
                return Ok(());
            }
            Some(_) => {}
            None => return Err("daemon disconnected before the soak shell exited".into()),
        }
    }
}

fn shutdown(instance: Option<&str>) -> Result<()> {
    let mut client = DaemonClient::connect(instance, Duration::from_secs(2))?;
    client.shutdown_daemon()?;
    println!("daemon stopping");
    Ok(())
}
pub fn connect_or_start(instance: Option<&str>) -> Result<DaemonClient> {
    let started_at = Instant::now();
    match DaemonClient::connect(instance, Duration::from_millis(25)) {
        Ok(client) => {
            compi_protocol::perf::set_startup_kind("warm");
            compi_protocol::perf::log_startup_metric("daemon_connection_ms", started_at.elapsed());
            Ok(client)
        }
        Err(_) => {
            compi_protocol::perf::set_startup_kind("cold");
            start_daemon(instance)?;
            let client = DaemonClient::connect(instance, Duration::from_secs(5))?;
            compi_protocol::perf::log_startup_metric("daemon_connection_ms", started_at.elapsed());
            Ok(client)
        }
    }
}

fn start_daemon(instance: Option<&str>) -> Result<()> {
    if DaemonClient::connect(instance, Duration::ZERO).is_ok() {
        return Ok(());
    }

    #[cfg(windows)]
    if instance.is_none() {
        activate_daemon_task()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if DaemonClient::connect(None, Duration::from_millis(100)).is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(
                    "the registered Compi daemon task did not become available; inspect Task Scheduler or repair Compi"
                        .into(),
                );
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    let executable = daemon_executable()?;

    #[cfg(windows)]
    let directory = env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .ok_or("LOCALAPPDATA is not set")?
        .join("Compi");
    #[cfg(unix)]
    let directory = compi_protocol::paths::data_dir()?;
    fs::create_dir_all(&directory)?;
    let suffix = instance.map(|name| format!("-{name}")).unwrap_or_default();
    let log_path = directory.join(format!("daemon{suffix}.log"));
    let log = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&log_path)?;
    let log_error = log.try_clone()?;
    let mut command = Command::new(executable);
    if let Some(instance) = instance {
        command.arg("--instance").arg(instance);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_error));
    #[cfg(windows)]
    command.creation_flags(DETACHED_PROCESS.0 | CREATE_NEW_PROCESS_GROUP.0);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // The server must survive both the probe and its controlling terminal.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut child = command.spawn()?;

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if DaemonClient::connect(instance, Duration::from_millis(100)).is_ok() {
            #[cfg(unix)]
            thread::spawn(move || {
                let _ = child.wait();
            });
            #[cfg(windows)]
            drop(child);
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(format!(
                "daemon exited with {status}; inspect {}",
                log_path.display()
            )
            .into());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("daemon did not start; inspect {}", log_path.display()).into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn check_system() -> Result<()> {
    let status = Command::new(daemon_executable()?)
        .arg("--check-system")
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("compi-daemon system check failed with {status}").into())
    }
}

fn daemon_executable() -> Result<std::path::PathBuf> {
    let name = if cfg!(windows) {
        "compi-daemon.exe"
    } else {
        "compi-daemon"
    };
    let current = env::current_exe()?;
    let mut executable = current.with_file_name(name);
    if !executable.is_file()
        && current
            .parent()
            .is_some_and(|parent| parent.ends_with("examples"))
    {
        executable = current
            .parent()
            .and_then(|parent| parent.parent())
            .ok_or("probe executable has no build directory")?
            .join(name);
    }
    if executable.is_file() {
        Ok(executable)
    } else {
        Err(format!(
            "{} was not found; build compi-daemon before starting it",
            executable.display()
        )
        .into())
    }
}

#[cfg(windows)]
fn activate_daemon_task() -> Result<()> {
    let status = Command::new(daemon_executable()?)
        .arg("--activate-task")
        .creation_flags(CREATE_NO_WINDOW.0)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("failed to activate the registered Compi daemon task: {status}").into())
    }
}

#[cfg(unix)]
mod console {
    use super::*;
    use compi_protocol::{Color, ScreenSnapshot};
    use std::io::{self, Read, Write};

    pub fn dimensions() -> (i16, i16) {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0
            && size.ws_col > 0
            && size.ws_row > 0
        {
            (
                size.ws_col.min(i16::MAX as u16) as i16,
                size.ws_row.min(i16::MAX as u16) as i16,
            )
        } else {
            (80, 24)
        }
    }

    struct TerminalState(libc::termios);

    impl TerminalState {
        fn configure() -> Result<Self> {
            let mut original = unsafe { std::mem::zeroed() };
            if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut original) } != 0 {
                return Err(io::Error::last_os_error().into());
            }
            let mut raw = original;
            unsafe { libc::cfmakeraw(&mut raw) };
            if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) } != 0 {
                return Err(io::Error::last_os_error().into());
            }
            Ok(Self(original))
        }
    }

    impl Drop for TerminalState {
        fn drop(&mut self) {
            unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.0) };
            let _ = io::stdout().write_all(b"\x1b[0m\x1b[?25h\r\n");
        }
    }

    pub fn attach(mut client: DaemonClient, surface: SurfaceInfo) -> Result<()> {
        let _terminal = TerminalState::configure()?;
        let mut size = dimensions();
        client.attach_surface(&surface, size.0, size.1)?;
        let mut mirror = ScreenMirror::default();
        let mut input = io::stdin().lock();
        let mut output = io::stdout().lock();
        let mut bytes = [0_u8; 4096];
        loop {
            let event = if let Some(message) = client.take_pending_screen() {
                Some(ServerEvent::Screen(message))
            } else {
                client.poll_event()?
            };
            match event {
                Some(ServerEvent::Screen(message)) => match mirror.apply(message) {
                    MirrorApply::Applied => {
                        if let Some(snapshot) = mirror.snapshot() {
                            render_snapshot(&mut output, snapshot)?;
                        }
                    }
                    MirrorApply::Gap { .. } => {
                        client.request_snapshot()?;
                    }
                },
                Some(ServerEvent::Control {
                    message: ServerMessage::SurfaceExited { exit_code, .. },
                    ..
                }) => {
                    write!(output, "\r\n[compi: shell exited {exit_code}]\r\n")?;
                    output.flush()?;
                    return Ok(());
                }
                Some(ServerEvent::Control {
                    message: ServerMessage::Error { code, message, .. },
                    ..
                }) => return Err(format!("daemon error ({code:?}): {message}").into()),
                _ => {}
            }
            let current_size = dimensions();
            if current_size != size {
                client.send(ClientMessage::Resize {
                    cols: current_size.0,
                    rows: current_size.1,
                })?;
                size = current_size;
            }
            let mut poll = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll, 1, 5) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            if ready > 0 {
                let count = input.read(&mut bytes)?;
                let detach = bytes[..count].iter().position(|byte| *byte == 0x1d);
                let length = detach.unwrap_or(count);
                if length > 0 {
                    client.send(ClientMessage::Input {
                        data: bytes[..length].to_vec(),
                        latency_id: None,
                    })?;
                }
                if count == 0 || detach.is_some() {
                    client.request(ClientMessage::Detach)?;
                    return Ok(());
                }
            }
        }
    }

    fn render_snapshot(output: &mut impl Write, snapshot: &ScreenSnapshot) -> Result<()> {
        output.write_all(b"\x1b[?25l\x1b[2J\x1b[H")?;
        let mut previous = None;
        for (row_index, row) in snapshot.cells.iter().enumerate() {
            if row_index != 0 {
                output.write_all(b"\r\n")?;
            }
            for cell in &row.cells {
                if cell.width == 0 {
                    continue;
                }
                let style = (cell.foreground, cell.background, &cell.attributes);
                if previous != Some(style) {
                    output.write_all(b"\x1b[0m")?;
                    for (enabled, code) in [
                        (cell.attributes.bold, 1),
                        (cell.attributes.dim, 2),
                        (cell.attributes.italic, 3),
                        (cell.attributes.underline, 4),
                        (cell.attributes.blink, 5),
                        (cell.attributes.inverse, 7),
                        (cell.attributes.hidden, 8),
                        (cell.attributes.strike, 9),
                    ] {
                        if enabled {
                            write!(output, "\x1b[{code}m")?;
                        }
                    }
                    write_color(output, cell.foreground, true)?;
                    write_color(output, cell.background, false)?;
                    previous = Some(style);
                }
                output.write_all(cell.text.as_bytes())?;
            }
        }
        write!(
            output,
            "\x1b[0m\x1b[{};{}H{}",
            snapshot.cursor.row + 1,
            snapshot.cursor.col + 1,
            if snapshot.cursor.visible {
                "\x1b[?25h"
            } else {
                "\x1b[?25l"
            }
        )?;
        output.flush()?;
        Ok(())
    }

    fn write_color(output: &mut impl Write, color: Color, foreground: bool) -> io::Result<()> {
        let code = if foreground { 38 } else { 48 };
        match color {
            Color::Default => write!(output, "\x1b[{}m", code + 1),
            Color::Indexed(index) => write!(output, "\x1b[{code};5;{index}m"),
            Color::Rgb(red, green, blue) => write!(output, "\x1b[{code};2;{red};{green};{blue}m"),
        }
    }
}
