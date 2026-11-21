//! Development-only smoke test of one native agent chat, end to end, never distributed.
//!
//! It opens a chat with `--agent` in a throwaway folder on one side of the PC (`--env host`:
//! `%TEMP%\compi-agent-smoke-<random>`; `--env wsl`: `/tmp/compi-agent-smoke-<random>` in
//! the distribution), as a tab of the running Compi instance, and asks the agent to:
//!
//! 1. run one command with its shell tool that writes the operating system's name to
//!    `shell.txt`, and
//! 2. create `edit.txt` containing `compi smoke` with its file tool.
//!
//! It answers every permission the agent asks with its allow option (Compi's `Write`
//! approval level, so commands ask and edits do not) and turns down every question.
//! Once the turn ends it checks that the transcript holds a completed `execute` tool
//! call, that `shell.txt` names the chat's side (`Linux` in WSL; `Win32NT`, `Windows` or
//! a MSYS/MinGW/Cygwin `uname` on the host) and that `edit.txt` holds `compi smoke`.
//! Then it closes the tab, deletes the chat's thread and the folder (kept with `--keep`).
//!
//! It talks to a RUNNING Compi daemon (`--instance`, `compi-dev` by default) and never
//! starts one, and it spends a little of the agent's usage: one short turn of its
//! default model. Run it from Windows cargo, for each agent and side:
//!
//! ```text
//! cargo.exe run -p compi-client --example agent-smoke -- --agent codex --env wsl
//! cargo.exe run -p compi-client --example agent-smoke -- --agent claude --env host
//! ```
//!
//! Options: `--distribution <name>` (WSL only; the default WSL distribution otherwise),
//! `--instance <name>`, `--timeout <secs>` (300 by default, from opening the chat to the
//! end of the turn) and `--keep` (keep the folder, for debugging).
//!
//! Exit status: 0 when every check passes, 1 when one fails or the agent fails, 2 when
//! the test could not run (bad arguments, no running instance, no folder).

#[cfg(windows)]
mod smoke {
    use compi_protocol::chat::{
        ChatApproval, ChatContent, ChatDelivery, ChatEntry, ChatHarness, ChatPicks, ChatState,
        ChatStatus, ChatToolStatus,
    };
    use compi_protocol::{
        ClientMessage, ConnectionFailure, ConnectionFailureKind, ContentEnvironment, DaemonClient,
        PaneContent, Result, ServerEvent, ServerMessage, TabId, WorkspaceMutation,
    };
    use std::collections::HashSet;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    const PROMPT: &str = "Smoke test. In the current folder do exactly two things, then reply `done`. 1) With your shell/command tool (not a file tool), run one command that writes the operating system's name to shell.txt: in Bash `uname -s > shell.txt`; in PowerShell `[Environment]::OSVersion.Platform | Out-File -Encoding ascii shell.txt`. 2) With your file editing/writing tool (not the shell), create edit.txt containing exactly: compi smoke";

    const USAGE: &str = "Usage: agent-smoke --agent <program> --env wsl|host [--distribution <name>] [--instance <name>] [--timeout <secs>] [--keep]";

    /// The test could not run.
    const SETUP_FAILED: i32 = 2;

    struct Args {
        harness: ChatHarness,
        wsl: bool,
        distribution: Option<String>,
        instance: String,
        timeout: Duration,
        keep: bool,
    }

    fn parse_args() -> std::result::Result<Args, String> {
        let mut harness = None;
        let mut wsl = None;
        let mut distribution = None;
        let mut instance = "compi-dev".to_owned();
        let mut timeout = Duration::from_secs(300);
        let mut keep = false;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            if arg == "--keep" {
                keep = true;
                continue;
            }
            let value = match arg.as_str() {
                "--agent" | "--env" | "--distribution" | "--instance" | "--timeout" => {
                    args.next().ok_or_else(|| format!("{arg} needs a value"))?
                }
                _ => return Err(format!("unknown argument {arg:?}")),
            };
            match arg.as_str() {
                "--agent" => {
                    harness = Some(ChatHarness::parse(&value).ok_or_else(|| {
                        let programs: Vec<_> = ChatHarness::ALL
                            .iter()
                            .map(|harness| harness.program())
                            .collect();
                        format!("unknown agent {value:?}; agents: {}", programs.join(", "))
                    })?)
                }
                "--env" => {
                    wsl = Some(match value.as_str() {
                        "wsl" => true,
                        "host" => false,
                        _ => return Err(format!("unknown environment {value:?}; wsl or host")),
                    })
                }
                "--distribution" => distribution = Some(value),
                "--instance" => instance = value,
                _ => {
                    let seconds: u64 = value
                        .parse()
                        .ok()
                        .filter(|seconds| *seconds > 0)
                        .ok_or_else(|| {
                            format!("--timeout {value:?} is not a positive number of seconds")
                        })?;
                    timeout = Duration::from_secs(seconds);
                }
            }
        }
        let harness = harness.ok_or("--agent is required")?;
        let wsl = wsl.ok_or("--env is required")?;
        if !wsl && distribution.is_some() {
            return Err("--distribution applies to --env wsl only".into());
        }
        Ok(Args {
            harness,
            wsl,
            distribution,
            instance,
            timeout,
            keep,
        })
    }

    /// The throwaway folder the agent works in.
    enum Folder {
        Host(PathBuf),
        Wsl { distribution: String, path: String },
    }

    impl Folder {
        fn create(wsl: bool, distribution: Option<&str>, name: &str) -> Result<Self> {
            if !wsl {
                let path = std::env::temp_dir().join(name);
                fs::create_dir(&path)?;
                return Ok(Self::Host(path));
            }
            let distribution = compi_protocol::wsl::directory_distribution(distribution)?;
            let path = format!("/tmp/{name}");
            wsl_run(&distribution, &["mkdir", "-p", "--", &path])?;
            Ok(Self::Wsl { distribution, path })
        }

        fn environment(&self) -> ContentEnvironment {
            match self {
                Self::Host(_) => ContentEnvironment::Host,
                Self::Wsl { distribution, .. } => ContentEnvironment::Wsl {
                    distribution: distribution.clone(),
                },
            }
        }

        /// The folder as the chat names it, in its environment.
        fn chat_path(&self) -> Result<String> {
            match self {
                Self::Host(path) => Ok(path
                    .to_str()
                    .ok_or("the temporary folder's path is not Unicode")?
                    .to_owned()),
                Self::Wsl { path, .. } => Ok(path.clone()),
            }
        }

        /// The file `name` in the folder, read from Windows.
        fn read(&self, name: &str) -> std::io::Result<Vec<u8>> {
            let path = match self {
                Self::Host(path) => path.join(name),
                Self::Wsl { distribution, path } => PathBuf::from(format!(
                    r"\\wsl.localhost\{distribution}{}\{name}",
                    path.replace('/', r"\")
                )),
            };
            fs::read(path)
        }

        /// Deletes the folder. The agent, whose working folder it is, stops shortly
        /// after its tab closes, and Windows refuses to delete a folder in use until
        /// then: a few tries, a second apart.
        fn remove(&self) -> Result<()> {
            match self {
                Self::Host(path) => {
                    let mut tries = 0;
                    loop {
                        match fs::remove_dir_all(path) {
                            Ok(()) => return Ok(()),
                            Err(_) if tries < 10 => {
                                tries += 1;
                                std::thread::sleep(Duration::from_secs(1));
                            }
                            Err(error) => return Err(error.into()),
                        }
                    }
                }
                Self::Wsl { distribution, path } => {
                    wsl_run(distribution, &["rm", "-rf", "--", path])
                }
            }
        }
    }

    /// Runs `command` in `distribution` without a shell.
    fn wsl_run(distribution: &str, command: &[&str]) -> Result<()> {
        let output = Command::new("wsl.exe")
            .args(["--distribution", distribution, "--exec"])
            .args(command)
            .output()?;
        if output.status.success() {
            return Ok(());
        }
        Err(format!(
            "`{}` in WSL {distribution} failed ({}): {}",
            command.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr)
                .replace('\0', "")
                .trim()
        )
        .into())
    }

    /// A file's text: UTF-16 when it starts with its byte order mark (PowerShell's `>`),
    /// else UTF-8.
    fn text(bytes: &[u8]) -> String {
        if let Some(utf16) = bytes.strip_prefix(b"\xFF\xFE") {
            let units: Vec<u16> = utf16
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect();
            return String::from_utf16_lossy(&units);
        }
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        String::from_utf8_lossy(bytes).into_owned()
    }

    /// Its first line, at most 120 characters.
    fn line(text: &str) -> String {
        let first = text.lines().next().unwrap_or_default().trim();
        match first.char_indices().nth(120) {
            Some((end, _)) => format!("{}…", &first[..end]),
            None => first.to_owned(),
        }
    }

    fn status_name(status: ChatToolStatus) -> &'static str {
        match status {
            ChatToolStatus::Pending => "pending",
            ChatToolStatus::Running => "running",
            ChatToolStatus::Completed => "completed",
            ChatToolStatus::Failed => "failed",
            ChatToolStatus::Stopped => "stopped",
        }
    }

    /// The chat under test: its watch, the connection that sends it requests and what
    /// was already answered and printed.
    struct Chat {
        id: String,
        watch: DaemonClient,
        state: ChatState,
        /// A turn ran since the prompt was sent.
        worked: bool,
        permissions: HashSet<u64>,
        questions: HashSet<u64>,
        printed: HashSet<String>,
    }

    impl Chat {
        /// Applies every change that arrived.
        fn pump(&mut self) -> Result<()> {
            while let Some(event) = self.watch.poll_event()? {
                if let ServerEvent::Control {
                    message: ServerMessage::ChatChanged { event },
                    ..
                } = event
                {
                    if let compi_protocol::chat::ChatEvent::Status {
                        status: ChatStatus::Working { .. },
                    } = &event
                    {
                        self.worked = true;
                    }
                    self.state.apply(event);
                }
            }
            Ok(())
        }

        /// Allows each permission asked, turns down each question and prints each tool
        /// call that ended.
        fn respond(&mut self, control: &mut DaemonClient) {
            if let Some(permission) = &self.state.permission
                && self.permissions.insert(permission.id)
            {
                let option = ["allow_once", "allow_always"]
                    .iter()
                    .find_map(|kind| {
                        permission
                            .options
                            .iter()
                            .find(|option| option.kind == *kind)
                    })
                    .or_else(|| {
                        permission
                            .options
                            .iter()
                            .find(|option| option.kind.starts_with("allow"))
                    });
                println!(
                    "permission: {} → {}",
                    line(&permission.title),
                    option.map_or("declined (no allow option)", |option| option.name.as_str())
                );
                if let Err(error) = control.chat_request(ClientMessage::AnswerChatPermission {
                    chat: self.id.clone(),
                    permission: permission.id,
                    option: option.map(|option| option.id.clone()),
                }) {
                    println!("  could not answer it: {error}");
                }
            }
            if let Some(question) = &self.state.question
                && self.questions.insert(question.id)
            {
                println!("question: {} → turned down", line(&question.message));
                if let Err(error) = control.chat_request(ClientMessage::AnswerChatQuestion {
                    chat: self.id.clone(),
                    question: question.id,
                    answers: None,
                }) {
                    println!("  could not answer it: {error}");
                }
            }
            for entry in &self.state.entries {
                if let ChatEntry::Tool(tool) = entry
                    && !tool.status.is_open()
                    && self.printed.insert(tool.id.clone())
                {
                    println!(
                        "tool: {} · {} · {}",
                        tool.kind,
                        line(&tool.title),
                        status_name(tool.status)
                    );
                }
            }
        }
    }

    struct Check {
        name: &'static str,
        pass: bool,
        found: String,
    }

    /// How the turn went.
    enum Turn {
        Ended,
        Failed(String),
        TimedOut,
    }

    pub fn main() -> i32 {
        let args = match parse_args() {
            Ok(args) => args,
            Err(error) => {
                eprintln!("agent-smoke: {error}\n{USAGE}");
                return SETUP_FAILED;
            }
        };
        let program = args.harness.program();
        let env = if args.wsl { "wsl" } else { "host" };
        let mut control =
            match DaemonClient::connect(Some(args.instance.as_str()), Duration::from_secs(5)) {
                Ok(client) => client,
                Err(error) => {
                    if ConnectionFailure::kind(error.as_ref()) == ConnectionFailureKind::Absent {
                        eprintln!(
                            "agent-smoke: Compi instance {:?} isn't running",
                            args.instance
                        );
                    } else {
                        eprintln!(
                            "agent-smoke: could not connect to Compi instance {:?}: {error}",
                            args.instance
                        );
                    }
                    return SETUP_FAILED;
                }
            };
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos() as u64);
        let random = format!("{nanos:x}-{:x}", std::process::id());
        let folder = match Folder::create(
            args.wsl,
            args.distribution.as_deref(),
            &format!("compi-agent-smoke-{random}"),
        ) {
            Ok(folder) => folder,
            Err(error) => {
                eprintln!("agent-smoke: could not make the folder: {error}");
                return SETUP_FAILED;
            }
        };
        let code = match folder.chat_path() {
            Ok(path) => {
                println!(
                    "agent-smoke: {} ({program}) in {} at {path}, instance {}",
                    args.harness.name(),
                    folder.environment(),
                    args.instance
                );
                let chat = ChatContent {
                    id: format!("agent-smoke-{random}"),
                    harness: args.harness,
                    environment: folder.environment(),
                    folder: path,
                    session: None,
                };
                in_tab(&mut control, chat, &folder, &args)
            }
            Err(error) => {
                eprintln!("agent-smoke: {error}");
                Err(SETUP_FAILED)
            }
        };
        if args.keep {
            match &folder {
                Folder::Host(path) => println!("kept folder: {}", path.display()),
                Folder::Wsl { distribution, path } => {
                    println!("kept folder: {path} in WSL {distribution}")
                }
            }
        } else if let Err(error) = folder.remove() {
            eprintln!("agent-smoke: could not delete the folder: {error}");
        }
        match code {
            Ok(true) => {
                println!("PASS agent={program} env={env}");
                0
            }
            Ok(false) => {
                println!("FAIL agent={program} env={env}");
                1
            }
            Err(code) => code,
        }
    }

    /// Opens `chat` in a new tab, tests it and closes the tab: whether every check
    /// passed, or the exit status when the test could not run.
    fn in_tab(
        control: &mut DaemonClient,
        chat: ChatContent,
        folder: &Folder,
        args: &Args,
    ) -> std::result::Result<bool, i32> {
        let tab = match open_tab(control, &chat, args) {
            Ok(tab) => tab,
            Err(error) => {
                eprintln!("agent-smoke: could not open the chat: {error}");
                return Err(SETUP_FAILED);
            }
        };
        let (result, thread) = test(control, &chat, folder, args);
        if let Err(error) = control.mutate(WorkspaceMutation::RemoveTab { tab_id: tab }) {
            eprintln!("agent-smoke: could not close the chat's tab: {error}");
        } else if thread && let Err(error) = control.delete_thread(&chat.id) {
            eprintln!("agent-smoke: could not delete the chat's thread: {error}");
        }
        result
    }

    fn open_tab(control: &mut DaemonClient, chat: &ChatContent, args: &Args) -> Result<TabId> {
        let workspace = control.workspace()?;
        let session = workspace
            .sessions
            .first()
            .ok_or("the instance has no session; open its window first")?;
        let receipt = control.mutate(WorkspaceMutation::CreateContentTab {
            session_id: session.id.clone(),
            label: format!("Agent smoke: {}", args.harness.program()),
            content: PaneContent::Chat(chat.clone()),
        })?;
        Ok(receipt
            .affected_tabs
            .into_iter()
            .next()
            .ok_or("the daemon opened no tab")?)
    }

    /// Runs the chat's turn and checks what it did: whether every check passed (or the
    /// exit status when the test could not run), and whether the chat became a thread.
    fn test(
        control: &mut DaemonClient,
        content: &ChatContent,
        folder: &Folder,
        args: &Args,
    ) -> (std::result::Result<bool, i32>, bool) {
        let mut watch =
            match DaemonClient::connect(Some(args.instance.as_str()), Duration::from_secs(5)) {
                Ok(watch) => watch,
                Err(error) => {
                    eprintln!("agent-smoke: could not connect to watch the chat: {error}");
                    return (Err(SETUP_FAILED), false);
                }
            };
        // Its own connection: replies to other requests on a watching one skip its events.
        let state =
            match watch.watch_chat(content.clone(), ChatApproval::Write, ChatPicks::default()) {
                Ok(state) => state,
                Err(error) => {
                    eprintln!("agent-smoke: could not watch the chat: {error}");
                    return (Err(SETUP_FAILED), false);
                }
            };
        let mut chat = Chat {
            id: content.id.clone(),
            watch,
            state,
            worked: false,
            permissions: HashSet::new(),
            questions: HashSet::new(),
            printed: HashSet::new(),
        };
        let result = run_turn(control, &mut chat, folder, args);
        let thread = chat.state.thread || !chat.state.entries.is_empty();
        (result, thread)
    }

    fn run_turn(
        control: &mut DaemonClient,
        chat: &mut Chat,
        folder: &Folder,
        args: &Args,
    ) -> std::result::Result<bool, i32> {
        let deadline = Instant::now() + args.timeout;
        let watch_failed = |error: compi_protocol::Error| {
            eprintln!("agent-smoke: lost the chat's watch: {error}");
            SETUP_FAILED
        };
        loop {
            chat.pump().map_err(watch_failed)?;
            chat.respond(control);
            match &chat.state.status {
                ChatStatus::Ready => break,
                ChatStatus::Failed { message } => {
                    println!("FAIL start: the agent failed: {message}");
                    return Ok(false);
                }
                ChatStatus::Starting | ChatStatus::Working { .. } => {}
            }
            if Instant::now() >= deadline {
                println!(
                    "FAIL start: still not ready after {}s",
                    args.timeout.as_secs()
                );
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        println!(
            "ready: model {}, approval mode {}",
            chat.state
                .option("model")
                .map_or("(not reported)", |option| option.current_name()),
            chat.state
                .approval_mode
                .as_deref()
                .unwrap_or("(not reported)")
        );
        chat.worked = false;
        if let Err(error) = control.chat_request(ClientMessage::SendChat {
            chat: chat.id.clone(),
            text: PROMPT.into(),
            files: Vec::new(),
            images: Vec::new(),
            delivery: ChatDelivery::Prompt,
        }) {
            println!("FAIL prompt: the chat refused it: {error}");
            return Ok(false);
        }
        let turn = loop {
            chat.pump().map_err(watch_failed)?;
            chat.respond(control);
            match &chat.state.status {
                ChatStatus::Ready if chat.worked => break Turn::Ended,
                ChatStatus::Failed { message } => break Turn::Failed(message.clone()),
                _ => {}
            }
            if Instant::now() >= deadline {
                break Turn::TimedOut;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let mut checks = Vec::new();
        match turn {
            Turn::Ended => {}
            Turn::Failed(message) => checks.push(Check {
                name: "turn",
                pass: false,
                found: format!("the agent failed: {message}"),
            }),
            Turn::TimedOut => checks.push(Check {
                name: "turn",
                pass: false,
                found: format!("still running after {}s", args.timeout.as_secs()),
            }),
        }
        checks.push(execute_check(&chat.state));
        checks.push(file_check(folder, "shell.txt", |text| {
            if args.wsl {
                text.contains("Linux")
            } else {
                let text = text.to_lowercase();
                ["win32nt", "mingw", "msys", "cygwin", "windows"]
                    .iter()
                    .any(|name| text.contains(name))
            }
        }));
        checks.push(file_check(folder, "edit.txt", |text| {
            text.contains("compi smoke")
        }));
        for entry in chat.state.entries.iter().rev() {
            match entry {
                ChatEntry::Agent { text } => {
                    println!("reply: {}", text.trim());
                    break;
                }
                ChatEntry::Error { message } => {
                    println!("error: {}", message.trim());
                    break;
                }
                ChatEntry::User { .. } => {
                    println!("reply: (none)");
                    break;
                }
                _ => {}
            }
        }
        for check in &checks {
            println!(
                "{} {}: {}",
                if check.pass { "PASS" } else { "FAIL" },
                check.name,
                check.found
            );
        }
        Ok(checks.iter().all(|check| check.pass))
    }

    /// The transcript holds a completed `execute` tool call.
    fn execute_check(state: &ChatState) -> Check {
        let tools: Vec<_> = state
            .entries
            .iter()
            .filter_map(|entry| match entry {
                ChatEntry::Tool(tool) => Some(tool),
                _ => None,
            })
            .collect();
        if let Some(tool) = tools
            .iter()
            .find(|tool| tool.kind == "execute" && tool.status == ChatToolStatus::Completed)
        {
            return Check {
                name: "execute",
                pass: true,
                found: format!("{} completed", line(&tool.title)),
            };
        }
        let seen: Vec<String> = tools
            .iter()
            .map(|tool| format!("{} {}", tool.kind, status_name(tool.status)))
            .collect();
        Check {
            name: "execute",
            pass: false,
            found: if seen.is_empty() {
                "no tool calls".into()
            } else {
                format!("no completed execute call; calls: {}", seen.join(", "))
            },
        }
    }

    /// File `name` in the folder exists and its text passes `expected`.
    fn file_check(folder: &Folder, name: &'static str, expected: impl Fn(&str) -> bool) -> Check {
        match folder.read(name) {
            Ok(bytes) => {
                let text = text(&bytes);
                Check {
                    name,
                    pass: expected(text.as_str()),
                    found: format!("{:?}", text.trim()),
                }
            }
            Err(error) => Check {
                name,
                pass: false,
                found: format!("not read: {error}"),
            },
        }
    }
}

#[cfg(windows)]
fn main() {
    std::process::exit(smoke::main());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Compi's agent smoke test runs on Windows, against a running Compi daemon");
    std::process::exit(2);
}
