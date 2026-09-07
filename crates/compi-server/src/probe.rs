use crate::Result;
use crate::client::{DaemonClient, ServerEvent};
#[cfg(windows)]
use crate::console;
use compi_client_core::{MirrorApply, ScreenMirror};
use compi_protocol::{ClientMessage, ScreenMessage, ServerMessage, SessionInfo, SessionStatus};
use std::env;
use std::fs::{self, OpenOptions};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
#[cfg(windows)]
use windows::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};

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
        Some("start") => start(instance, optional_working_directory(&args, "start")?),
        Some("daemon-start") => start_daemon(instance),
        Some("create") => create(instance, optional_working_directory(&args, "create")?),
        Some("list") | Some("status") => list(instance),
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
        Some("attach") => {
            let id = args.get(1).ok_or("attach requires a session ID")?;
            attach(instance, id.clone())
        }
        Some("inspect") => {
            let id = args.get(1).ok_or("inspect requires a session ID")?;
            inspect(instance, id.clone())
        }
        Some("kill") => {
            let id = args.get(1).ok_or("kill requires a session ID")?;
            kill(instance, id.clone())
        }
        Some("shutdown") => shutdown(instance),
        Some("check-system") | Some("--check-system") => crate::launch::check_system(),
        Some("help") | Some("--help") | Some("-h") => {
            usage();
            Ok(())
        }
        Some(command) => Err(format!("unknown command {command:?}; run compi-probe help").into()),
    }
}

fn usage() {
    println!(
        "compi-probe - Milestone 2 diagnostic client\n\n\
         Usage:\n  compi-probe start [working-directory]     Create and attach a session\n  \
         compi-probe daemon-start                  Start the per-user daemon\n  \
         compi-probe create [working-directory]    Create a detached session\n  \
         compi-probe list                          List sessions\n  \
         compi-probe attach <id>           Attach to a session\n  \
         compi-probe inspect <id>          Print an authoritative screen snapshot\n  \
         compi-probe kill <id>             Kill a session\n  \
         compi-probe soak <seconds>        Run an attached sustained-output workload\n  \
         compi-probe shutdown              Stop the daemon and all sessions\n\n\
         compi-probe check-system          Check the native shell runtime\n\n\
         Add `--instance <name>` before the command for an isolated development daemon.\n\
         Press Ctrl+] to detach without stopping the shell."
    );
}

fn start(instance: Option<&str>, working_directory: Option<String>) -> Result<()> {
    let mut client = connect_or_start(instance)?;
    let (cols, rows) = console::dimensions();
    let session = client.create_session(cols, rows, working_directory)?;
    console::attach(client, session.id)
}

fn create(instance: Option<&str>, working_directory: Option<String>) -> Result<()> {
    let mut client = DaemonClient::connect(instance, Duration::from_secs(2))?;
    let (cols, rows) = console::dimensions();
    let session = client.create_session(cols, rows, working_directory)?;
    println!("{}", session.id);
    Ok(())
}

fn optional_working_directory(args: &[String], command: &str) -> Result<Option<String>> {
    if args.len() > 2 {
        return Err(format!("{command} accepts at most one working directory").into());
    }
    Ok(args.get(1).cloned())
}

fn list(instance: Option<&str>) -> Result<()> {
    let mut client = DaemonClient::connect(instance, Duration::from_secs(2))?;
    let sessions = client.list_sessions()?;
    if sessions.is_empty() {
        println!("no sessions");
        return Ok(());
    }
    for session in sessions {
        println!("{}", format_session_line(session));
    }
    Ok(())
}

fn format_session_line(session: SessionInfo) -> String {
    let status = match session.status {
        SessionStatus::Starting => "starting",
        SessionStatus::Running if session.attached => "attached",
        SessionStatus::Running => "detached",
        SessionStatus::Exited => "exited",
        SessionStatus::Failed => "failed",
        SessionStatus::Dead => "dead",
    };
    let detail = session
        .error
        .or_else(|| session.exit_code.map(|code| format!("exit {code}")))
        .unwrap_or_default();
    format!(
        "{}\t{}\t{}x{}\t{}",
        session.id, status, session.cols, session.rows, detail
    )
}

fn attach(instance: Option<&str>, session_id: String) -> Result<()> {
    let client = DaemonClient::connect(instance, Duration::from_secs(2))?;
    console::attach(client, session_id)
}

fn kill(instance: Option<&str>, session_id: String) -> Result<()> {
    let mut client = DaemonClient::connect(instance, Duration::from_secs(2))?;
    client.kill_session(session_id.clone())?;
    println!("kill requested for {session_id}");
    Ok(())
}

fn inspect(instance: Option<&str>, session_id: String) -> Result<()> {
    let mut client = DaemonClient::connect(instance, Duration::from_secs(2))?;
    let session = client
        .list_sessions()?
        .into_iter()
        .find(|session| session.id == session_id)
        .ok_or("session was not found")?;
    match client.request(ClientMessage::Attach {
        session_id,
        cols: session.cols,
        rows: session.rows,
    })? {
        ServerMessage::Attached { .. } => {}
        message => return Err(format!("unexpected attach response: {message:?}").into()),
    }
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
                message: ServerMessage::Error { code, message },
                ..
            }) => return Err(format!("daemon error ({code:?}): {message}").into()),
            Some(_) => {}
            None => return Err("daemon disconnected before sending a snapshot".into()),
        }
    }
}

fn soak(instance: Option<&str>, duration: Duration) -> Result<()> {
    let mut client = DaemonClient::connect(instance, Duration::from_secs(2))?;
    let session = client.create_session(100, 30, None)?;
    match client.request(ClientMessage::Attach {
        session_id: session.id.clone(),
        cols: 100,
        rows: 30,
    })? {
        ServerMessage::Attached { .. } => {}
        message => return Err(format!("unexpected attach response: {message:?}").into()),
    }
    client.request(ClientMessage::Input { data: b"i=0; while :; do printf 'COMPI_SOAK_%08d 0123456789abcdefghijklmnopqrstuvwxyz\\n' \"$i\"; i=$((i+1)); sleep 0.02; done\r".to_vec(), latency_id: None })?;

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
                message: ServerMessage::SessionExited { exit_code, .. },
                ..
            }) => return Err(format!("soak shell exited early with code {exit_code}").into()),
            Some(ServerEvent::Control {
                message: ServerMessage::Error { code, message },
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
                    ServerMessage::SessionExited {
                        session_id,
                        exit_code,
                    },
                ..
            }) if session_id == session.id => {
                if exit_code != 0 {
                    return Err(format!("soak shell exited with code {exit_code}").into());
                }
                println!(
                    "session={} frames={frames} recovered_gaps={gaps}",
                    session.id
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
            crate::perf::set_startup_kind("warm");
            crate::perf::log_startup_metric("daemon_connection_ms", started_at.elapsed());
            Ok(client)
        }
        Err(_) => {
            crate::perf::set_startup_kind("cold");
            start_daemon(instance)?;
            let client = DaemonClient::connect(instance, Duration::from_secs(5))?;
            crate::perf::log_startup_metric("daemon_connection_ms", started_at.elapsed());
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
        crate::supervisor::activate()?;
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

    crate::launch::check_system()?;
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
    if !executable.is_file() {
        return Err(format!(
            "{} was not found; build compi-daemon before starting it",
            executable.display()
        )
        .into());
    }
    #[cfg(windows)]
    let directory = env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .ok_or("LOCALAPPDATA is not set")?
        .join("Compi");
    #[cfg(unix)]
    let directory = crate::paths::data_dir()?;
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

    pub fn attach(mut client: DaemonClient, session_id: String) -> Result<()> {
        let _terminal = TerminalState::configure()?;
        let mut size = dimensions();
        match client.request(ClientMessage::Attach {
            session_id,
            cols: size.0,
            rows: size.1,
        })? {
            ServerMessage::Attached { .. } => {}
            response => return Err(format!("unexpected attach response: {response:?}").into()),
        }
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
                    message: ServerMessage::SessionExited { exit_code, .. },
                    ..
                }) => {
                    write!(output, "\r\n[compi: shell exited {exit_code}]\r\n")?;
                    output.flush()?;
                    return Ok(());
                }
                Some(ServerEvent::Control {
                    message: ServerMessage::Error { code, message },
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
