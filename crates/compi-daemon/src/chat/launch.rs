//! How a chat's harness starts where the chat runs, and whether it is installed there.
//!
//! On a Windows host the program runs as it is, found where its installers put it. In WSL
//! and on a Unix host it runs from the user's login shell, for the search path their
//! profile sets: the login shell, whatever its language, runs Compi's launch script in a
//! POSIX shell, the script travelling in the environment (`COMPI_CHAT_COMMAND`), so its
//! arguments need no quoting but the script's own.
#[cfg(windows)]
use super::not_installed;
use compi_protocol::ContentEnvironment;
use compi_protocol::chat::{ChatContent, ChatHarness};
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
const SCRIPT_START: &str = r#"unset COMPI_CHAT_COMMAND; PATH="$HOME/.compi/shell:$PATH"#;
/// Where `omp` is installed when a login shell's search path lacks it: its install
/// script's and Bun's folders.
const OMP_FIND: &str = r#":$HOME/.local/bin:$HOME/.bun/bin""#;
/// Finds `claude`: a Linux one first (on the search path, else where its installers put
/// it), else the Windows one WSL reaches, on the Windows search path WSL appends or in
/// the Windows home folder (`COMPI_WINDOWS_HOME`, its Linux path): `$found` is the Linux
/// one, `$windows` the Windows one.
const CLAUDE_FIND: &str = concat!(
    r#":$HOME/.local/bin:$HOME/.claude/local"; found=; windows=; set -f; IFS=:; "#,
    r#"for dir in $PATH${COMPI_WINDOWS_HOME:+:$COMPI_WINDOWS_HOME/.local/bin:$COMPI_WINDOWS_HOME/AppData/Roaming/npm}; do "#,
    r#"case $dir in "#,
    r#"/mnt/[a-zA-Z]/*) if [ -z "$windows" ]; then "#,
    r#"if [ -f "$dir/claude.exe" ]; then windows=$dir/claude.exe; "#,
    r#"elif [ -f "$dir/claude" ]; then windows=$dir/claude; fi; fi;; "#,
    r#"*) if [ -f "$dir/claude" ] && [ -x "$dir/claude" ]; then found=$dir/claude; break; fi;; "#,
    r#"esac; done; unset IFS; set +f"#,
);

/// One of a harness's arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Arg {
    /// A word passed as it is.
    Word(String),
    /// A word the POSIX shell starting the harness expands (`"$HOME/…"`); a harness
    /// started without one, on a Windows host, takes none.
    Shell(&'static str),
}

impl Arg {
    pub(super) fn word(word: impl Into<String>) -> Self {
        Self::Word(word.into())
    }

    /// As the launch script writes it.
    fn shell(&self) -> String {
        match self {
            Self::Word(word) => quote(word),
            Self::Shell(word) => (*word).to_owned(),
        }
    }

    /// As a program started without a shell takes it.
    #[cfg(windows)]
    fn plain(&self) -> Result<&str, String> {
        match self {
            Self::Word(word) => Ok(word),
            Self::Shell(word) => Err(format!("{word} needs a POSIX shell")),
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

/// The launch script of `harness`: it runs it with `args`, exiting 127 when it is not
/// installed, or, with `locate`, only succeeds when it is installed.
fn script(harness: ChatHarness, args: &[Arg], locate: bool) -> String {
    let args: String = args.iter().map(|arg| format!(" {}", arg.shell())).collect();
    match harness {
        ChatHarness::Omp if locate => {
            format!("{SCRIPT_START}{OMP_FIND}; command -v omp >/dev/null")
        }
        ChatHarness::Omp => format!("{SCRIPT_START}{OMP_FIND}; exec omp{args}"),
        ChatHarness::Claude if locate => {
            format!(r#"{SCRIPT_START}{CLAUDE_FIND}; [ -n "$found$windows" ]"#)
        }
        // A Linux `claude` gets the WSL note; a Windows one, the note telling it its
        // commands belong in WSL.
        ChatHarness::Claude => format!(
            r#"{SCRIPT_START}{CLAUDE_FIND}; if [ -n "$found" ]; then exec "$found"{args} ${{COMPI_AGENT_NOTE:+--append-system-prompt "$COMPI_AGENT_NOTE"}}; elif [ -n "$windows" ]; then exec "$windows"{args} ${{COMPI_AGENT_WINDOWS_NOTE:+--append-system-prompt "$COMPI_AGENT_WINDOWS_NOTE"}}; else exit 127; fi"#
        ),
    }
}

/// The harness's command with `args` in its environment and folder, and `environment`
/// (Compi's shell context, from `shell_integration::cli_environment`) in its own.
pub(super) fn command(
    content: &ChatContent,
    environment: &[(&'static str, String)],
    args: &[Arg],
) -> Result<Command, String> {
    if matches!(content.environment, ContentEnvironment::Host)
        && !std::path::Path::new(&content.folder).is_dir()
    {
        return Err(format!("The folder {} doesn't exist.", content.folder));
    }
    match &content.environment {
        #[cfg(windows)]
        ContentEnvironment::Host => {
            use std::os::windows::process::CommandExt as _;
            let program = windows_program(content.harness).ok_or_else(|| not_installed(content))?;
            let args = args.iter().map(Arg::plain).collect::<Result<Vec<_>, _>>()?;
            // A `.cmd` launcher runs through `cmd.exe /c`, which `Command` does for it,
            // its arguments escaped for it.
            let mut command = Command::new(program);
            command
                .args(args)
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
            Ok(command)
        }
        #[cfg(windows)]
        ContentEnvironment::Wsl { distribution } => {
            let mut environment = environment.to_vec();
            if content.harness == ChatHarness::Claude {
                environment.extend(windows_home(distribution));
            }
            let script = script(content.harness, args, false);
            Ok(wsl_command(
                distribution,
                Some(&content.folder),
                &environment,
                &script,
            ))
        }
        #[cfg(unix)]
        ContentEnvironment::Host => {
            let mut command = unix_command(environment, &script(content.harness, args, false));
            command.current_dir(&content.folder);
            Ok(command)
        }
        #[cfg(unix)]
        ContentEnvironment::Wsl { .. } => Err("WSL chats need a Windows host".into()),
    }
}

/// Whether `harness` is installed in `environment`, as its launch finds it; no answer
/// within `LOOKUP_TIMEOUT` is a no.
pub(super) fn installed(environment: &ContentEnvironment, harness: ChatHarness) -> bool {
    let command = match environment {
        #[cfg(windows)]
        ContentEnvironment::Host => return windows_program(harness).is_some(),
        #[cfg(windows)]
        ContentEnvironment::Wsl { distribution } => {
            let environment: Vec<_> = match harness {
                ChatHarness::Claude => windows_home(distribution).into_iter().collect(),
                ChatHarness::Omp => Vec::new(),
            };
            wsl_command(
                distribution,
                None,
                &environment,
                &script(harness, &[], true),
            )
        }
        #[cfg(unix)]
        ContentEnvironment::Host => unix_command(&[], &script(harness, &[], true)),
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

/// `wsl.exe` running `script` in `distribution`'s login shell, in `folder` (the home
/// folder without one), with `environment`.
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

/// The Windows home folder's Linux path in `distribution`, for a Windows `claude` there.
#[cfg(windows)]
pub(super) fn windows_home(distribution: &str) -> Option<(&'static str, String)> {
    let home = std::env::var("USERPROFILE")
        .ok()
        .filter(|home| !home.is_empty())?;
    let linux = super::linux_home(distribution, &home).ok()?;
    Some(("COMPI_WINDOWS_HOME", linux))
}

/// The harness's program on a Windows host: on the search path, else where its
/// installers put it. OMP: `omp.exe`, else Bun's. Claude Code: `claude.exe` (its native
/// installer's, in `~\.local\bin`), else npm's `claude.cmd`, by the program it launches
/// when that is beside it.
#[cfg(windows)]
fn windows_program(harness: ChatHarness) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let path: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    let on_path = |file: &str| {
        path.iter()
            .map(|folder| folder.join(file))
            .find(|candidate| candidate.is_file())
    };
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let in_home = |parts: &[&str]| {
        let mut file = home.clone()?;
        file.extend(parts);
        file.is_file().then_some(file)
    };
    match harness {
        ChatHarness::Omp => on_path("omp.exe").or_else(|| in_home(&[".bun", "bin", "omp.exe"])),
        ChatHarness::Claude => on_path("claude.exe")
            .or_else(|| in_home(&[".local", "bin", "claude.exe"]))
            .or_else(|| {
                let launcher = on_path("claude.cmd").or_else(|| {
                    let file = PathBuf::from(std::env::var_os("APPDATA")?)
                        .join("npm")
                        .join("claude.cmd");
                    file.is_file().then_some(file)
                })?;
                let program = launcher
                    .parent()?
                    .join(r"node_modules\@anthropic-ai\claude-code\bin\claude.exe");
                Some(if program.is_file() { program } else { launcher })
            }),
    }
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
    fn omp_runs_as_it_always_did_its_extension_path_expanded() {
        let args = [
            Arg::word("acp"),
            Arg::word("--approval-mode"),
            Arg::word("write"),
            Arg::word("-e"),
            Arg::Shell(r#""$HOME/.compi/shell/compi-omp.js""#),
        ];
        assert_eq!(
            script(ChatHarness::Omp, &args, false),
            r#"unset COMPI_CHAT_COMMAND; PATH="$HOME/.compi/shell:$PATH:$HOME/.local/bin:$HOME/.bun/bin"; exec omp acp --approval-mode write -e "$HOME/.compi/shell/compi-omp.js""#
        );
        assert!(script(ChatHarness::Omp, &[], true).ends_with("; command -v omp >/dev/null"));
    }

    #[test]
    fn claude_prefers_a_linux_install_and_each_gets_its_own_note() {
        let args = [Arg::word("--resume"), Arg::word("s 1")];
        let run = script(ChatHarness::Claude, &args, false);
        assert!(run.contains(r#"for dir in $PATH${COMPI_WINDOWS_HOME:+:$COMPI_WINDOWS_HOME/.local/bin:$COMPI_WINDOWS_HOME/AppData/Roaming/npm}; do"#));
        assert!(run.ends_with(
            r#"if [ -n "$found" ]; then exec "$found" --resume 's 1' ${COMPI_AGENT_NOTE:+--append-system-prompt "$COMPI_AGENT_NOTE"}; elif [ -n "$windows" ]; then exec "$windows" --resume 's 1' ${COMPI_AGENT_WINDOWS_NOTE:+--append-system-prompt "$COMPI_AGENT_WINDOWS_NOTE"}; else exit 127; fi"#
        ));
        assert!(script(ChatHarness::Claude, &[], true).ends_with(r#"; [ -n "$found$windows" ]"#));
        // The script travels in a variable, so its single quotes reach the shell as typed.
        assert!(!LOGIN_COMMAND.contains('\''));
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
}
