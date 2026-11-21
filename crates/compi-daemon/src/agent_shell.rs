//! Compi's agent shell: the `bash.exe` a Windows agent in a WSL chat runs its commands
//! with (Claude Code's `CLAUDE_CODE_GIT_BASH_PATH`, OMP's bash tool through Compi's
//! extension), so they run in the chat's distribution, in its folder, as commands typed
//! in a Compi terminal there.
//!
//! It is the daemon's own program under the name `bash.exe` (a link to it, else a copy,
//! in the data folder's `agent-shell`): started by that name, the daemon runs `bash` in
//! the distribution `COMPI_AGENT_DISTRIBUTION` names, with the same arguments, through
//! Compi's hook (`compi-agent.sh`, as `BASH_ENV`). The hook goes to the agent's folder
//! (this program's, a `\\wsl.localhost` path), finds the Git Bash paths of Windows drives
//! a Windows agent writes (`/c/Users/…`) where WSL mounts them, and sends a PowerShell
//! command to PowerShell. Compi's variables (`COMPI_*`) go along.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The variable naming the distribution the agent shell runs commands in.
pub(crate) const DISTRIBUTION_VARIABLE: &str = "COMPI_AGENT_DISTRIBUTION";
/// What the agent shell is called: what Windows agents accept as a Bash.
const NAME: &str = "bash.exe";

/// Whether the running program was started as the agent shell.
pub fn invoked() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_name().map(|name| name.eq_ignore_ascii_case(NAME)))
        .unwrap_or(false)
}

/// Runs the agent shell's command and returns its exit code.
pub fn run() -> i32 {
    match command().and_then(|mut command| {
        let child = command
            .spawn()
            .map_err(|error| format!("couldn't start wsl.exe: {error}"))?;
        // The command ends with this shell, which its agent may kill.
        let job = unsafe { crate::conpty::create_kill_on_close_job(&child) }.ok();
        let mut child = child;
        let status = child
            .wait()
            .map_err(|error| format!("wsl.exe failed: {error}"))?;
        drop(job);
        Ok(status.code().unwrap_or(1))
    }) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("compi agent shell: {message}");
            127
        }
    }
}

/// `wsl.exe` running `bash` with this program's arguments in the agent's distribution.
fn command() -> Result<Command, String> {
    let distribution = std::env::var(DISTRIBUTION_VARIABLE)
        .ok()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| format!("{DISTRIBUTION_VARIABLE} names no WSL distribution"))?;
    let folder = std::env::current_dir()
        .map(|folder| plain_path(&folder))
        .unwrap_or_default();
    let compi: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(key, _)| {
            key.to_str()
                .is_some_and(|key| key.starts_with("COMPI_") && !key.starts_with("COMPI_AGENT_"))
        })
        .collect();
    let mut command = Command::new(crate::chat::WSL_EXE);
    if let Some(path) = compi_protocol::wsl::launch_path() {
        command.env("PATH", path);
    }
    command.args(arguments(
        &distribution,
        &folder,
        &compi,
        std::env::args_os().skip(1),
    ));
    Ok(command)
}

/// `wsl.exe`'s arguments: the distribution's `bash` with `args`, Compi's hook as its
/// `BASH_ENV`, `folder` to go to, and `compi`, Compi's variables.
fn arguments(
    distribution: &str,
    folder: &str,
    compi: &[(OsString, OsString)],
    args: impl Iterator<Item = OsString>,
) -> Vec<OsString> {
    let mut all: Vec<OsString> = [
        "--distribution",
        distribution,
        "--cd",
        "~",
        "--exec",
        "/usr/bin/env",
        "COMPI_AGENT_MSYS=1",
    ]
    .map(OsString::from)
    .into();
    all.push(format!("COMPI_AGENT_CWD={folder}").into());
    all.extend(compi.iter().map(|(key, value)| {
        let mut pair = key.clone();
        pair.push("=");
        pair.push(value);
        pair
    }));
    all.extend(
        [
            "/bin/sh",
            "-c",
            r#"export COMPI_USER_BASH_ENV="${BASH_ENV-}" BASH_ENV="$HOME/.compi/shell/compi-agent.sh"; exec bash "$@""#,
            "sh",
        ]
        .map(OsString::from),
    );
    all.extend(args);
    all
}

/// `path` without the `\\?\` prefix Windows can give the current folder.
fn plain_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if let Some(share) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{share}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned()
    }
}

/// Puts the agent shell in place, the running daemon under its name, and returns its
/// path: a link where the file system allows one, else a copy, renewed when the daemon
/// changed. A stale one still running is moved aside.
pub(crate) fn install() -> Result<PathBuf, String> {
    let daemon =
        std::env::current_exe().map_err(|error| format!("where is the daemon? {error}"))?;
    let folder = compi_protocol::paths::data_dir()
        .map_err(|error| error.to_string())?
        .join("agent-shell");
    std::fs::create_dir_all(&folder)
        .map_err(|error| format!("couldn't create {}: {error}", folder.display()))?;
    let shell = folder.join(NAME);
    if same_program(&daemon, &shell) {
        return Ok(shell);
    }
    if shell.exists() && std::fs::remove_file(&shell).is_err() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| time.as_millis());
        let _ = std::fs::rename(&shell, folder.join(format!("bash.{stamp}.old")));
    }
    // Moved-aside copies go once nothing runs them.
    if let Ok(entries) = std::fs::read_dir(&folder) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().ends_with(".old") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    std::fs::hard_link(&daemon, &shell)
        .or_else(|_| std::fs::copy(&daemon, &shell).map(drop))
        .map_err(|error| {
            format!(
                "couldn't put the agent shell at {}: {error}",
                shell.display()
            )
        })?;
    Ok(shell)
}

/// Whether `shell` is `daemon`: the same size and last change, as a link or copy keeps.
fn same_program(daemon: &Path, shell: &Path) -> bool {
    let (Ok(daemon), Ok(shell)) = (std::fs::metadata(daemon), std::fs::metadata(shell)) else {
        return false;
    };
    daemon.len() == shell.len() && daemon.modified().ok() == shell.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_runs_in_the_distribution_through_compis_hook_with_its_arguments() {
        let args = arguments(
            "Ubuntu",
            r"\\wsl.localhost\Ubuntu\home\me\work",
            &[("COMPI_INSTANCE".into(), "dev".into())],
            ["-c".into(), "-l".into(), "eval 'ls' && pwd -P".into()].into_iter(),
        );
        let args: Vec<_> = args.iter().map(|arg| arg.to_string_lossy()).collect();
        assert_eq!(
            args,
            [
                "--distribution",
                "Ubuntu",
                "--cd",
                "~",
                "--exec",
                "/usr/bin/env",
                "COMPI_AGENT_MSYS=1",
                r"COMPI_AGENT_CWD=\\wsl.localhost\Ubuntu\home\me\work",
                "COMPI_INSTANCE=dev",
                "/bin/sh",
                "-c",
                r#"export COMPI_USER_BASH_ENV="${BASH_ENV-}" BASH_ENV="$HOME/.compi/shell/compi-agent.sh"; exec bash "$@""#,
                "sh",
                "-c",
                "-l",
                "eval 'ls' && pwd -P",
            ]
        );
    }

    #[test]
    fn a_verbatim_folder_loses_its_prefix() {
        assert_eq!(
            plain_path(Path::new(r"\\?\UNC\wsl.localhost\Ubuntu\home")),
            r"\\wsl.localhost\Ubuntu\home"
        );
        assert_eq!(plain_path(Path::new(r"\\?\C:\work")), r"C:\work");
        assert_eq!(plain_path(Path::new(r"C:\work")), r"C:\work");
    }
}
