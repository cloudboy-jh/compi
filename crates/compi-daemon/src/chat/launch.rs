//! One launcher for every agent, from its entry (`AgentEntry`): where its real program
//! is, how it starts where the chat is, and whether it is installed there.
//!
//! A chat in WSL runs the agent's Linux program when the distribution has one, else its
//! Windows program, through WSL. A chat in a Windows folder runs the Windows program,
//! else the Linux one of the default distribution, in that folder. Either way it is the
//! real program, never a script launcher (npm's `.cmd` and shell shims) whose
//! interpreter may be the other side's.
//!
//! A Linux program runs from the user's login shell, for the search path their profile
//! sets: the login shell, whatever its language, runs Compi's launch script in a POSIX
//! shell, the script travelling in the environment (`COMPI_CHAT_COMMAND`), so its
//! arguments need no quoting but the script's own. The script says which program it
//! started and the chat's folder as that program names it, on a line of its own before
//! the program's output (`LAUNCH_MARK`).
//!
//! The agent's shell is the soup: a Linux program's `bash -c` commands go through
//! Compi's hook (`compi-agent.sh`, its `BASH_ENV`), and a Windows program in WSL runs its
//! commands with Compi's agent shell (`crate::agent_shell`), which runs them in WSL
//! through the same hook. They land where the chat is, as commands typed in a Compi
//! terminal there: PowerShell commands in PowerShell, the rest in the chat's shell.
#[cfg(windows)]
use super::not_installed;
use compi_protocol::ContentEnvironment;
#[cfg(windows)]
use compi_protocol::chat::WindowsProgram;
use compi_protocol::chat::{AgentEntry, AgentProgram, ChatContent, ChatHarness};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How long looking for a harness may take; one that takes longer counts as missing.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
/// The variable carrying the launch script.
const SCRIPT_VARIABLE: &str = "COMPI_CHAT_COMMAND";
/// What the login shell runs: the launch script, in a POSIX shell.
const LOGIN_COMMAND: &str = r#"exec /bin/sh -c "$COMPI_CHAT_COMMAND""#;
/// The start of every launch script: it leaves the harness's environment as it found it,
/// and puts the shell bridge's folder first on the search path, so `compi` runs the CLI
/// as in a terminal.
const SCRIPT_START: &str = r#"unset COMPI_CHAT_COMMAND; PATH="$HOME/.compi/shell:$PATH""#;
/// What a launch script does before it starts a Linux program: its `bash -c` commands
/// go through Compi's hook, after the user's own `BASH_ENV`.
const LINUX_START: &str = r#"unset COMPI_AGENT_WINDOWS COMPI_AGENT_WINDOWS_NOTE COMPI_AGENT_WINDOWS_BASH COMPI_AGENT_WSLENV; if [ -f "$HOME/.compi/shell/compi-agent.sh" ]; then export COMPI_USER_BASH_ENV="${BASH_ENV-}" BASH_ENV="$HOME/.compi/shell/compi-agent.sh"; fi"#;
/// What a launch script does before it starts a Windows program: it gets the Windows
/// note, and Compi's variables reach it (`WSLENV`).
const WINDOWS_START: &str = r#"COMPI_AGENT_NOTE=${COMPI_AGENT_WINDOWS_NOTE-}; export COMPI_AGENT_NOTE WSLENV="${WSLENV:+$WSLENV:}$COMPI_AGENT_WSLENV"; unset COMPI_AGENT_WINDOWS COMPI_AGENT_WINDOWS_NOTE COMPI_AGENT_WINDOWS_BASH COMPI_AGENT_WSLENV"#;
/// The start of the line a launch script writes before the program's output:
/// `linux|windows <folder>`.
pub(super) const LAUNCH_MARK: &str = "\u{1e}compi-agent ";

/// One of a harness's arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Arg {
    /// A word passed as it is.
    Word(String),
    /// A file of Compi's shell bridge (`~/.compi/shell/<file>`) after `flag`, by the
    /// path the running program reads it at; a program without the bridge (a Windows
    /// program in a Windows folder) takes neither.
    Bridge {
        flag: &'static str,
        file: &'static str,
    },
    /// The chat's note for the running program (`COMPI_AGENT_NOTE`) after `flag`, when
    /// there is one.
    Note(&'static str),
}

impl Arg {
    pub(super) fn word(word: impl Into<String>) -> Self {
        Self::Word(word.into())
    }

    /// As the launch script writes it for a Linux program, or a Windows one.
    fn shell(&self, windows: bool) -> String {
        match self {
            Self::Word(word) => quote(word),
            Self::Bridge { flag, file } if windows => {
                format!(r#"{flag} "$(wslpath -w "$HOME/.compi/shell/{file}")""#)
            }
            Self::Bridge { flag, file } => format!(r#"{flag} "$HOME/.compi/shell/{file}""#),
            Self::Note(flag) => format!(r#"${{COMPI_AGENT_NOTE:+{flag} "$COMPI_AGENT_NOTE"}}"#),
        }
    }
}

/// `word` as one POSIX shell word.
fn quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./:=@%+,".contains(&byte));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// The running program, as its launch reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Started {
    /// It is a Windows program.
    pub(super) windows: bool,
    /// The chat's folder, as it names it.
    pub(super) folder: String,
}

/// What a launch script's mark line (`LAUNCH_MARK`) says, when `line` is one.
pub(super) fn started(line: &str) -> Option<Started> {
    let (side, folder) = line.strip_prefix(LAUNCH_MARK)?.split_once(' ')?;
    Some(Started {
        windows: side == "windows",
        folder: folder.to_owned(),
    })
}

/// What a chat's start finds out before its program runs, on a thread of its own.
#[derive(Debug, Default)]
pub(super) struct Prepared {
    /// Compi's shell bridge is in place where a Linux program would run.
    pub(super) bridge: bool,
    /// What a Linux program in WSL is told at start.
    pub(super) linux_note: Option<String>,
    /// What a Windows program in a WSL chat is told at start.
    pub(super) windows_note: Option<String>,
    /// The agent's Windows program.
    #[cfg(windows)]
    windows_program: Option<std::path::PathBuf>,
    /// Compi's agent shell, for a Windows program in a WSL chat.
    #[cfg(windows)]
    agent_shell: Option<std::path::PathBuf>,
    /// The distribution a chat in a Windows folder runs the agent's Linux program in,
    /// when it has no Windows one.
    #[cfg(windows)]
    fallback: Option<String>,
}

impl Prepared {
    /// Whether the launch goes through WSL: a chat there, or one in a Windows folder
    /// whose agent has only a Linux program.
    pub(super) fn runs_in_wsl(&self, content: &ChatContent) -> bool {
        match content.environment {
            ContentEnvironment::Wsl { .. } => true,
            #[cfg(windows)]
            ContentEnvironment::Host => self.windows_program.is_none() && self.fallback.is_some(),
            #[cfg(unix)]
            ContentEnvironment::Host => false,
        }
    }
}

/// Where the agent's programs are for `content`: enough to start it, as the approval
/// lookup does, without the bridge and notes `prepare` adds.
pub(super) fn programs(content: &ChatContent) -> Prepared {
    #[cfg(windows)]
    {
        let windows_program = windows_program(content.harness.entry());
        let fallback = match &content.environment {
            ContentEnvironment::Host if windows_program.is_none() => {
                compi_protocol::wsl::directory_distribution(None).ok()
            }
            _ => None,
        };
        Prepared {
            windows_program,
            fallback,
            ..Prepared::default()
        }
    }
    #[cfg(unix)]
    {
        let _ = content;
        Prepared::default()
    }
}

/// Everything `content`'s agent needs before it starts: where its programs are, Compi's
/// shell bridge where a Linux one runs, the notes, and Compi's agent shell for a Windows
/// one in WSL. Failures are logged; the agent starts anyway.
pub(super) fn prepare(content: &ChatContent) -> Prepared {
    let mut prepared = programs(content);
    let log = |what: &str, error: &dyn std::fmt::Display| {
        eprintln!(
            "compi-daemon: chat {}: couldn't {what}: {error}",
            content.id
        );
    };
    #[cfg(windows)]
    {
        let distribution = match &content.environment {
            ContentEnvironment::Wsl { distribution } => Some(distribution.clone()),
            ContentEnvironment::Host => prepared.fallback.clone(),
        };
        if let Some(distribution) = distribution {
            match crate::shell_integration::install_wsl_bridge(&distribution) {
                Ok(()) => prepared.bridge = true,
                Err(error) => log("install the shell bridge", &error),
            }
            prepared.linux_note = super::wsl_note(&distribution);
        }
        if let (ContentEnvironment::Wsl { distribution }, Some(_)) =
            (&content.environment, &prepared.windows_program)
        {
            let paths = super::windows_paths_note(distribution);
            if content.harness.entry().windows_bash.is_some() {
                // Its shell is the distribution's, through the agent shell.
                prepared.windows_note = Some(match &prepared.linux_note {
                    Some(note) => format!("{note} {paths}"),
                    None => paths,
                });
                match crate::agent_shell::install() {
                    Ok(shell) => prepared.agent_shell = Some(shell),
                    Err(error) => log("put the agent shell in place", &error),
                }
            } else {
                prepared.windows_note = Some(paths);
            }
        }
    }
    #[cfg(unix)]
    if matches!(content.environment, ContentEnvironment::Host) {
        match crate::shell_integration::install_home_bridge() {
            Ok(()) => prepared.bridge = true,
            Err(error) => log("install the shell bridge", &error),
        }
    }
    prepared
}

/// The launch script of `entry`'s agent: it runs its Linux program with `args`, else
/// the Windows one `COMPI_AGENT_WINDOWS` names, exiting 127 when there is neither; with
/// `locate`, it only succeeds when there is a Linux one. A Linux program is looked for
/// on the search path, then where its installers put it, never in the Windows folders
/// WSL appends to the search path. Only the Windows program gets Compi's agent shell
/// (`COMPI_AGENT_WINDOWS_BASH`) in its variable (`AgentEntry::windows_bash`), which can
/// be one a Linux program reads too (OpenCode's `SHELL`).
fn script(entry: &AgentEntry, args: &[Arg], locate: bool) -> String {
    let folders: String = entry
        .linux_folders
        .iter()
        .map(|folder| format!(":$HOME/{folder}"))
        .collect();
    let program = entry.program;
    let find = format!(
        r#"found=; dirs="$PATH{folders}"; set -f; IFS=:; for dir in $dirs; do case $dir in /mnt/[a-zA-Z]/*) ;; *) if [ -f "$dir/{program}" ] && [ -x "$dir/{program}" ]; then found=$dir/{program}; break; fi;; esac; done; unset IFS; set +f"#
    );
    if locate {
        return format!(r#"{SCRIPT_START}; {find}; [ -n "$found" ]"#);
    }
    let linux: String = args
        .iter()
        .map(|arg| format!(" {}", arg.shell(false)))
        .collect();
    let windows: String = args
        .iter()
        .map(|arg| format!(" {}", arg.shell(true)))
        .collect();
    let bash = entry.windows_bash.map_or_else(String::new, |variable| {
        format!(
            r#"if [ -n "${{COMPI_AGENT_WINDOWS_BASH-}}" ]; then export {variable}="$COMPI_AGENT_WINDOWS_BASH"; fi; "#
        )
    });
    format!(
        r#"{SCRIPT_START}; {find}; if [ -n "$found" ]; then {LINUX_START}; printf '\036compi-agent linux %s\n' "$PWD"; exec "$found"{linux}; elif [ -n "${{COMPI_AGENT_WINDOWS-}}" ] && program=$(wslpath -u "$COMPI_AGENT_WINDOWS"); then {bash}{WINDOWS_START}; printf '\036compi-agent windows %s\n' "$(wslpath -w "$PWD")"; exec "$program"{windows}; else exit 127; fi"#
    )
}

/// The harness's command with `args` in its environment and folder, and `environment`
/// (Compi's shell context, from `shell_integration::cli_environment`) in its own; the
/// program it runs when that is known now, else its launch script says (`started`).
pub(super) fn command(
    content: &ChatContent,
    environment: &[(&'static str, String)],
    args: &[Arg],
    prepared: &Prepared,
) -> Result<(Command, Option<Started>), String> {
    if matches!(content.environment, ContentEnvironment::Host)
        && !std::path::Path::new(&content.folder).is_dir()
    {
        return Err(format!("The folder {} doesn't exist.", content.folder));
    }
    let entry = content.harness.entry();
    let mut environment = environment.to_vec();
    environment.extend(
        prepared
            .linux_note
            .clone()
            .map(|note| ("COMPI_AGENT_NOTE", note)),
    );
    match &content.environment {
        #[cfg(windows)]
        ContentEnvironment::Host => match (&prepared.windows_program, &prepared.fallback) {
            (Some(program), _) => {
                use std::os::windows::process::CommandExt as _;
                let mut command = Command::new(program);
                for arg in args {
                    match arg {
                        Arg::Word(word) => {
                            command.arg(word);
                        }
                        Arg::Bridge { .. } | Arg::Note(_) => {}
                    }
                }
                command
                    .current_dir(&content.folder)
                    .creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0)
                    .envs(environment.iter().map(|(key, value)| (*key, value)));
                // The CLI's folder comes first on the search path, as in a PowerShell pane.
                if let Some((_, cli)) = environment.iter().find(|(key, _)| *key == "COMPI_CLI")
                    && let Some(folder) = std::path::Path::new(cli).parent()
                {
                    let mut path = folder.as_os_str().to_owned();
                    if let Some(inherited) = std::env::var_os("PATH") {
                        path.push(";");
                        path.push(inherited);
                    }
                    command.env("PATH", path);
                }
                let started = Started {
                    windows: true,
                    folder: content.folder.clone(),
                };
                Ok((command, Some(started)))
            }
            // A Linux program works on the Windows folder from the default distribution.
            (None, Some(distribution)) => Ok((
                wsl_command(
                    distribution,
                    Some(&content.folder),
                    &environment,
                    &script(entry, args, false),
                ),
                None,
            )),
            (None, None) => Err(not_installed(content)),
        },
        #[cfg(windows)]
        ContentEnvironment::Wsl { distribution } => {
            if let Some(program) = &prepared.windows_program {
                environment.push(("COMPI_AGENT_WINDOWS", program.display().to_string()));
                environment.extend(
                    prepared
                        .windows_note
                        .clone()
                        .map(|note| ("COMPI_AGENT_WINDOWS_NOTE", note)),
                );
                let mut passed: Vec<&str> = environment
                    .iter()
                    .map(|(key, _)| *key)
                    .filter(|key| !key.starts_with("COMPI_AGENT_WINDOWS"))
                    .collect();
                if let (Some(shell), Some(variable)) = (&prepared.agent_shell, entry.windows_bash) {
                    // The script puts it in `variable` for the Windows program alone.
                    environment.extend([
                        ("COMPI_AGENT_WINDOWS_BASH", shell.display().to_string()),
                        (
                            crate::agent_shell::DISTRIBUTION_VARIABLE,
                            distribution.clone(),
                        ),
                    ]);
                    passed.extend([variable, crate::agent_shell::DISTRIBUTION_VARIABLE]);
                }
                passed.push("COMPI_AGENT_NOTE");
                passed.sort_unstable();
                passed.dedup();
                environment.push(("COMPI_AGENT_WSLENV", passed.join(":")));
            }
            Ok((
                wsl_command(
                    distribution,
                    Some(&content.folder),
                    &environment,
                    &script(entry, args, false),
                ),
                None,
            ))
        }
        #[cfg(unix)]
        ContentEnvironment::Host => {
            let mut command = unix_command(&environment, &script(entry, args, false));
            command.current_dir(&content.folder);
            Ok((command, None))
        }
        #[cfg(unix)]
        ContentEnvironment::Wsl { .. } => Err("WSL chats need a Windows host".into()),
    }
}

/// Whether `harness` is installed for a chat in `environment`, as its launch finds it:
/// a Linux program, or a Windows one where a chat there can run it (any chat on a
/// Windows PC); no answer within `LOOKUP_TIMEOUT` is a no.
pub(super) fn installed(environment: &ContentEnvironment, harness: ChatHarness) -> bool {
    let entry = harness.entry();
    let command = match environment {
        #[cfg(windows)]
        ContentEnvironment::Host => {
            if windows_program(entry).is_some() {
                return true;
            }
            let Ok(distribution) = compi_protocol::wsl::directory_distribution(None) else {
                return false;
            };
            wsl_command(&distribution, None, &[], &script(entry, &[], true))
        }
        #[cfg(windows)]
        ContentEnvironment::Wsl { distribution } => {
            if windows_program(entry).is_some() {
                return true;
            }
            wsl_command(distribution, None, &[], &script(entry, &[], true))
        }
        #[cfg(unix)]
        ContentEnvironment::Host => unix_command(&[], &script(entry, &[], true)),
        #[cfg(unix)]
        ContentEnvironment::Wsl { .. } => return false,
    };
    succeeds(command, LOOKUP_TIMEOUT)
}

/// Whether `command` ends well within `timeout`; it is killed after that.
fn succeeds(mut command: Command, timeout: Duration) -> bool {
    let Ok(mut child) = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// The start of the line a version lookup's script writes once it has found the agent's
/// Linux program, before what the program's `--version` says.
const VERSION_MARK: &str = "\u{1e}compi-agent-version";

/// The WSL distribution the Linux programs of `linux_install` are looked for in: the
/// default one, where a chat in a Windows folder runs them. `None` without WSL, and on a
/// Linux or macOS host, whose own programs they are.
pub(super) fn default_distribution() -> Option<String> {
    #[cfg(windows)]
    {
        compi_protocol::wsl::directory_distribution(None).ok()
    }
    #[cfg(unix)]
    {
        None
    }
}

/// `harness`'s Windows program, the one a chat's launch picks, with its version; `None`
/// when it has none (and off Windows).
pub(super) fn windows_install(harness: ChatHarness) -> Option<AgentProgram> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        let mut command = Command::new(windows_program(harness.entry())?);
        command
            .arg("--version")
            .creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
        Some(AgentProgram {
            version: output_within(command, LOOKUP_TIMEOUT)
                .as_deref()
                .and_then(version),
        })
    }
    #[cfg(unix)]
    {
        let _ = harness;
        None
    }
}

/// `harness`'s Linux program, found by the search its launch makes (in `distribution` on
/// Windows, on the host elsewhere), with its version; `None` when there is none, or no
/// answer within `LOOKUP_TIMEOUT`.
pub(super) fn linux_install(
    harness: ChatHarness,
    distribution: Option<&str>,
) -> Option<AgentProgram> {
    let lookup = version_script(harness.entry());
    #[cfg(windows)]
    let command = wsl_command(distribution?, None, &[], &lookup);
    #[cfg(unix)]
    let command = {
        let _ = distribution;
        unix_command(&[], &lookup)
    };
    let output = output_within(command, LOOKUP_TIMEOUT)?;
    let (_, said) = output.split_once(VERSION_MARK)?;
    Some(AgentProgram {
        version: version(said),
    })
}

/// The script that finds `entry`'s Linux program as its launch script does and, when
/// there is one, writes `VERSION_MARK` and runs it with `--version`.
fn version_script(entry: &AgentEntry) -> String {
    format!(
        r#"{} || exit 127; printf '\036compi-agent-version\n'; exec "$found" --version 2>&1"#,
        script(entry, &[], true)
    )
}

/// The version in what `<program> --version` wrote: its first word that starts with a
/// digit, after any `v`, and has a dot (`2.1.3` of `2.1.3 (Claude Code)`, `19.0.0` of
/// `omp/19.0.0`, `0.169.0` of `codex-cli 0.169.0`).
fn version(output: &str) -> Option<String> {
    output
        .split(|c: char| c.is_whitespace() || "/@,;()[]".contains(c))
        .map(|word| word.trim_end_matches(['.', ':']))
        .map(|word| word.strip_prefix(['v', 'V']).unwrap_or(word))
        .find(|word| word.starts_with(|c: char| c.is_ascii_digit()) && word.contains('.'))
        .map(str::to_owned)
}

/// What `command` writes to its standard output within `timeout`, after which it is
/// killed; `None` when it can't start.
fn output_within(mut command: Command, timeout: Duration) -> Option<String> {
    use std::io::Read as _;
    use std::sync::{Arc, Mutex, PoisonError, mpsc};
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let written = Arc::new(Mutex::new(Vec::new()));
    // The reader drops `reading` once the output ends, which ends the wait below.
    let (reading, ended) = mpsc::channel::<()>();
    let stdout = child.stdout.take();
    let reader = Arc::clone(&written);
    let _ = thread::Builder::new()
        .name("compi-chat-lookup".into())
        .spawn(move || {
            let _reading = reading;
            let Some(mut stdout) = stdout else {
                return;
            };
            let mut chunk = [0; 4096];
            while let Ok(read @ 1..) = stdout.read(&mut chunk) {
                reader
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extend_from_slice(&chunk[..read]);
            }
        });
    let _ = ended.recv_timeout(timeout);
    let _ = child.kill();
    let _ = child.wait();
    let written = written.lock().unwrap_or_else(PoisonError::into_inner);
    Some(String::from_utf8_lossy(&written).into_owned())
}

/// The login shell running `script`, with `environment`.
#[cfg(unix)]
fn unix_command(environment: &[(&'static str, String)], script: &str) -> Command {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
    let mut command = Command::new(shell);
    command
        .args(["-lc", LOGIN_COMMAND])
        .envs(environment.iter().map(|(key, value)| (*key, value)))
        .env(SCRIPT_VARIABLE, script);
    command
}

/// `wsl.exe` running `script` in `distribution`'s login shell, in `folder` (a Linux
/// path, or a Windows one WSL translates; the home folder without one), with
/// `environment`.
#[cfg(windows)]
fn wsl_command(
    distribution: &str,
    folder: Option<&str>,
    environment: &[(&'static str, String)],
    script: &str,
) -> Command {
    use std::os::windows::process::CommandExt as _;
    let mut command = Command::new(super::WSL_EXE);
    command.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
    if let Some(path) = compi_protocol::wsl::launch_path() {
        command.env("PATH", path);
    }
    command.args(wsl_arguments(distribution, folder, environment, script));
    command
}

/// `wsl.exe`'s arguments for `wsl_command`. The environment is set in Linux, as a WSL
/// terminal's is: Windows variables reach it only through `WSLENV`.
#[cfg(any(windows, test))]
fn wsl_arguments(
    distribution: &str,
    folder: Option<&str>,
    environment: &[(&'static str, String)],
    script: &str,
) -> Vec<String> {
    let mut args = vec![
        "--distribution".to_owned(),
        distribution.to_owned(),
        "--cd".to_owned(),
        folder.unwrap_or("~").to_owned(),
        "--exec".to_owned(),
        "/usr/bin/env".to_owned(),
    ];
    args.extend(
        environment
            .iter()
            .map(|(key, value)| format!("{key}={value}")),
    );
    args.push(format!("{SCRIPT_VARIABLE}={script}"));
    // The user's login shell, for the search path their profile sets.
    args.extend([
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        format!(
            r#"shell=$(getent passwd "$(id -u)" | cut -d: -f7); exec "${{shell:-/bin/sh}}" -lc '{LOGIN_COMMAND}'"#
        ),
    ]);
    args
}

/// The agent's Windows program: the first of its entry's places that has it. npm's
/// global folder is the one its launcher (`<program>.cmd`) is in on the search path,
/// else `%APPDATA%\npm`; the launcher itself never runs.
#[cfg(windows)]
fn windows_program(entry: &AgentEntry) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let path: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    let on_path = |file: &str| {
        path.iter()
            .map(|folder| folder.join(file))
            .find(|candidate| candidate.is_file())
    };
    let under = |folder: PathBuf, relative: &str| {
        let mut file = folder;
        file.extend(relative.split('/'));
        file.is_file().then_some(file)
    };
    let launcher = format!("{}.cmd", entry.program);
    entry.windows.iter().find_map(|place| match place {
        WindowsProgram::Path(file) => on_path(file),
        WindowsProgram::Home(relative) => {
            under(PathBuf::from(std::env::var_os("USERPROFILE")?), relative)
        }
        WindowsProgram::Npm(relative) => on_path(&launcher)
            .and_then(|launcher| launcher.parent().map(PathBuf::from))
            .and_then(|folder| under(folder, relative))
            .or_else(|| {
                let folder = PathBuf::from(std::env::var_os("APPDATA")?).join("npm");
                under(folder, relative)
            }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_quoted_for_a_posix_shell_only_when_they_need_it() {
        assert_eq!(quote("--model"), "--model");
        assert_eq!(quote("claude-opus-4-8"), "claude-opus-4-8");
        assert_eq!(quote("opus[1m]"), "'opus[1m]'");
        assert_eq!(quote("it's $HOME"), r"'it'\''s $HOME'");
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn every_agent_runs_its_linux_program_else_its_windows_one_each_with_its_note() {
        let args = [
            Arg::word("acp"),
            Arg::word("--approval-mode"),
            Arg::word("write"),
            Arg::Bridge {
                flag: "-e",
                file: "compi-omp.js",
            },
        ];
        let run = script(ChatHarness::Omp.entry(), &args, false);
        assert!(run.starts_with(
            r#"unset COMPI_CHAT_COMMAND; PATH="$HOME/.compi/shell:$PATH"; found=; dirs="$PATH:$HOME/.local/bin:$HOME/.bun/bin"; set -f; IFS=:; for dir in $dirs; do case $dir in /mnt/[a-zA-Z]/*) ;; *) if [ -f "$dir/omp" ] && [ -x "$dir/omp" ]; then found=$dir/omp; break; fi;; esac; done;"#
        ));
        assert!(run.contains(
            r#"printf '\036compi-agent linux %s\n' "$PWD"; exec "$found" acp --approval-mode write -e "$HOME/.compi/shell/compi-omp.js"; elif"#
        ));
        assert!(run.ends_with(
            r#"printf '\036compi-agent windows %s\n' "$(wslpath -w "$PWD")"; exec "$program" acp --approval-mode write -e "$(wslpath -w "$HOME/.compi/shell/compi-omp.js")"; else exit 127; fi"#
        ));
        let claude = script(
            ChatHarness::Claude.entry(),
            &[
                Arg::word("--resume"),
                Arg::word("s 1"),
                Arg::Note("--append-system-prompt"),
            ],
            false,
        );
        assert!(claude.contains(
            r#"exec "$found" --resume 's 1' ${COMPI_AGENT_NOTE:+--append-system-prompt "$COMPI_AGENT_NOTE"}; elif"#
        ));
        assert!(claude.contains(r#"COMPI_AGENT_NOTE=${COMPI_AGENT_WINDOWS_NOTE-}; export"#));
        // The agent shell goes into the Windows program's variable only: OpenCode's Linux
        // program reads `SHELL` too.
        let opencode = script(ChatHarness::OpenCode.entry(), &[Arg::word("serve")], false);
        let (linux, windows) = opencode.split_once("; elif ").unwrap();
        assert!(!linux.contains("export SHELL"));
        assert!(linux.contains(
            "unset COMPI_AGENT_WINDOWS COMPI_AGENT_WINDOWS_NOTE COMPI_AGENT_WINDOWS_BASH"
        ));
        assert!(windows.contains(
            r#"then if [ -n "${COMPI_AGENT_WINDOWS_BASH-}" ]; then export SHELL="$COMPI_AGENT_WINDOWS_BASH"; fi; COMPI_AGENT_NOTE="#
        ));
        assert!(
            script(ChatHarness::Codex.entry(), &[], true)
                .ends_with(r#"$HOME/.codex/packages/standalone/current/bin"; set -f; IFS=:; for dir in $dirs; do case $dir in /mnt/[a-zA-Z]/*) ;; *) if [ -f "$dir/codex" ] && [ -x "$dir/codex" ]; then found=$dir/codex; break; fi;; esac; done; unset IFS; set +f; [ -n "$found" ]"#)
        );
        // The script travels in a variable, so its single quotes reach the shell as typed.
        assert!(!LOGIN_COMMAND.contains('\''));
    }

    #[test]
    fn the_launch_mark_names_the_program_s_side_and_folder() {
        assert_eq!(
            started("\u{1e}compi-agent windows \\\\wsl.localhost\\Ubuntu\\home\\me\\a b"),
            Some(Started {
                windows: true,
                folder: r"\\wsl.localhost\Ubuntu\home\me\a b".into()
            })
        );
        assert_eq!(
            started("\u{1e}compi-agent linux /home/me"),
            Some(Started {
                windows: false,
                folder: "/home/me".into()
            })
        );
        assert_eq!(started(r#"{"jsonrpc":"2.0"}"#), None);
    }

    #[test]
    fn wsl_runs_the_script_from_the_login_shell_in_the_chat_folder() {
        let args = wsl_arguments(
            "Ubuntu",
            Some("/home/me/work"),
            &[("COMPI_INSTANCE", "dev".to_owned())],
            "exec omp",
        );
        assert_eq!(
            args,
            [
                "--distribution",
                "Ubuntu",
                "--cd",
                "/home/me/work",
                "--exec",
                "/usr/bin/env",
                "COMPI_INSTANCE=dev",
                "COMPI_CHAT_COMMAND=exec omp",
                "/bin/sh",
                "-c",
                r#"shell=$(getent passwd "$(id -u)" | cut -d: -f7); exec "${shell:-/bin/sh}" -lc 'exec /bin/sh -c "$COMPI_CHAT_COMMAND"'"#,
            ]
        );
        assert_eq!(wsl_arguments("Ubuntu", None, &[], "true")[3], "~");
    }

    #[test]
    fn a_version_lookup_finds_the_program_as_its_launch_does_and_reads_its_version() {
        let lookup = version_script(ChatHarness::Codex.entry());
        assert!(lookup.starts_with(&script(ChatHarness::Codex.entry(), &[], true)));
        assert!(lookup.ends_with(
            r#"[ -n "$found" ] || exit 127; printf '\036compi-agent-version\n'; exec "$found" --version 2>&1"#
        ));
        assert_eq!(version("2.1.326 (Claude Code)\n").as_deref(), Some("2.1.326"));
        assert_eq!(version("omp/19.0.0\n").as_deref(), Some("19.0.0"));
        assert_eq!(version("codex-cli 0.169.0\n").as_deref(), Some("0.169.0"));
        assert_eq!(version("opencode v1.19.0.\n").as_deref(), Some("1.19.0"));
        assert_eq!(version("\n"), None);
        assert_eq!(version("unknown option --version"), None);
    }
}
