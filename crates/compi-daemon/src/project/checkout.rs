//! Project picker operations that run in the daemon's filesystem and Git environment.

use crate::content_files::Failure;
use compi_protocol::{ContentEnvironment, ErrorCode};
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const CLONE_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) fn environments() -> Vec<(ContentEnvironment, Option<String>)> {
    let host_home = std::env::home_dir()
        .and_then(|home| super::normalized(&ContentEnvironment::Host, home.to_str()?))
        .filter(|home| Path::new(home).is_dir());
    let host = (ContentEnvironment::Host, host_home);
    #[cfg(not(windows))]
    {
        vec![host]
    }
    #[cfg(windows)]
    {
        let mut environments = vec![host];
        let deadline = Instant::now() + Duration::from_secs(8);
        for distribution in compi_protocol::wsl::wsl2_distributions().unwrap_or_default() {
            let mut command = wsl_launcher(&distribution);
            command.args(["--cd", "~", "--exec", "pwd"]);
            let home = run(
                &mut command,
                PROBE_TIMEOUT.min(deadline.saturating_duration_since(Instant::now())),
                true,
            )
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| super::unix_folder(output.stdout.trim_end_matches(['\n', '\r'])));
            environments.push((ContentEnvironment::Wsl { distribution }, home));
        }
        environments
    }
}

pub(super) fn validate_folder(environment: &ContentEnvironment, path: &str) -> Result<(), Failure> {
    let not_directory = || invalid("The selected path is not a folder.");
    match environment {
        ContentEnvironment::Host => match fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => Ok(()),
            Ok(_) => Err(not_directory()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Err(invalid("The selected folder does not exist."))
            }
            Err(error) => Err(invalid(format!(
                "Could not open the selected folder: {error}"
            ))),
        },
        ContentEnvironment::Wsl { .. } => {
            require_environment(environment)?;
            let mut command = wsl_tool(environment, "stat")?;
            command.args(["-L", "-c", "%F", "--", path]);
            let output = run(&mut command, PROBE_TIMEOUT, true)
                .map_err(|error| invalid(format!("Could not check the WSL folder: {error}")))?;
            if output.status.success() {
                return if output.stdout.trim() == "directory" {
                    Ok(())
                } else {
                    Err(not_directory())
                };
            }
            Err(invalid(
                if output.stderr.contains("No such file or directory") {
                    "The selected folder does not exist in this WSL distribution."
                } else if output.stderr.contains("Permission denied") {
                    "You do not have permission to open the selected WSL folder."
                } else {
                    "Could not open the selected folder. Check that the WSL distribution is available."
                },
            ))
        }
    }
}

pub(super) fn clone_repository(
    environment: &ContentEnvironment,
    url: &str,
    destination: &str,
) -> Result<(), Failure> {
    validate_url(url)?;
    require_environment(environment)?;
    let (mut command, timeout) = clone_command(environment)?;
    // Git accepts an existing empty directory. Only clone into one we created ourselves,
    // atomically, so concurrent clones cannot share a destination.
    reserve_destination(environment, destination)?;
    command.args([
        "-C",
        destination,
        "clone",
        "--quiet",
        "--no-recurse-submodules",
        "--",
        url,
        ".",
    ]);
    let result = match run(&mut command, timeout, false) {
        Ok(output) if output.status.success() => return Ok(()),
        Ok(output)
            if matches!(output.status.code(), Some(124 | 137))
                && matches!(environment, ContentEnvironment::Wsl { .. }) =>
        {
            clone_timeout()
        }
        Ok(output) => clone_failure(&output),
        Err(error) if error.kind() == io::ErrorKind::TimedOut => clone_timeout(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            "Git is not installed or is not on PATH in the selected environment.".to_owned()
        }
        Err(error) => format!("Could not run Git: {error}"),
    };
    // Never remove a nonempty folder: a checkout or a user may already have put files here.
    let removed = match environment {
        ContentEnvironment::Host => fs::remove_dir(destination).is_ok(),
        ContentEnvironment::Wsl { .. } => {
            wsl_tool(environment, "rmdir").is_ok_and(|mut command| {
                command.args(["--", destination]);
                run(&mut command, PROBE_TIMEOUT, false).is_ok_and(|output| output.status.success())
            })
        }
    };
    let message = if removed {
        result
    } else {
        format!("{result} Check the destination for a partial clone before retrying.")
    };
    Err((ErrorCode::Internal, message))
}

fn reserve_destination(environment: &ContentEnvironment, destination: &str) -> Result<(), Failure> {
    match environment {
        ContentEnvironment::Host => {
            #[cfg(windows)]
            if !safe_windows_path(destination) {
                return Err(invalid(
                    "The clone destination contains an invalid Windows folder name.",
                ));
            }
            fs::create_dir(destination).map_err(|error| match error.kind() {
                io::ErrorKind::AlreadyExists => destination_exists(),
                io::ErrorKind::NotFound => {
                    invalid("The clone destination's parent folder does not exist.")
                }
                _ => invalid(format!("Could not create the clone destination: {error}")),
            })
        }
        ContentEnvironment::Wsl { .. } => {
            let mut command = wsl_tool(environment, "mkdir")?;
            command.args(["--", destination]);
            let output = run(&mut command, PROBE_TIMEOUT, false).map_err(|error| {
                invalid(format!(
                    "Could not create the WSL clone destination: {error}"
                ))
            })?;
            if output.status.success() {
                return Ok(());
            }
            Err(if output.stderr.contains("File exists") {
                destination_exists()
            } else if output.stderr.contains("No such file or directory") {
                invalid("The clone destination's parent folder does not exist in WSL.")
            } else if output.stderr.contains("Permission denied") {
                invalid("You do not have permission to create the WSL clone destination.")
            } else {
                invalid(
                    "Could not create the clone destination. Check the WSL distribution and parent folder.",
                )
            })
        }
    }
}

#[cfg(windows)]
fn safe_windows_path(path: &str) -> bool {
    use std::path::Component;
    Path::new(path).components().all(|component| {
        let Component::Normal(name) = component else {
            return true;
        };
        let Some(name) = name.to_str() else {
            return false;
        };
        !name.ends_with(['.', ' '])
            && !name.contains([':', '<', '>', '"', '|', '?', '*'])
            && !name.chars().any(char::is_control)
    })
}

fn destination_exists() -> Failure {
    (
        ErrorCode::FileConflict,
        "The clone destination already exists. Choose a new folder name.".into(),
    )
}

fn invalid(message: impl Into<String>) -> Failure {
    (ErrorCode::InvalidRequest, message.into())
}

/// Keep Git's user config, credential helpers, SSH identities and SSH configuration. The
/// protocol allowlist also applies if that configuration rewrites a URL via `insteadOf`.
fn clone_command(environment: &ContentEnvironment) -> Result<(Command, Duration), Failure> {
    const GIT_ENV: &[(&str, &str)] = &[
        ("LC_ALL", "C"),
        ("GIT_ALLOW_PROTOCOL", "http:https:ssh"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GCM_INTERACTIVE", "never"),
    ];
    const GIT_CONTEXT: &[&str] = &[
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ];
    match environment {
        ContentEnvironment::Host => {
            let mut command = Command::new("git");
            command.envs(GIT_ENV.iter().copied());
            for key in GIT_CONTEXT {
                command.env_remove(key);
            }
            Ok((command, CLONE_TIMEOUT))
        }
        // Never use Git for Windows here, even if the destination is on /mnt/c. A Linux
        // timeout also supervises Git's descendants inside WSL, across the Windows boundary.
        ContentEnvironment::Wsl { .. } => {
            let mut command = wsl_env(environment)?;
            for &key in GIT_CONTEXT {
                command.args(["-u", key]);
            }
            command
                .args(GIT_ENV.iter().map(|(key, value)| format!("{key}={value}")))
                .args(["timeout", "-k", "3"])
                .arg(CLONE_TIMEOUT.as_secs().to_string())
                .arg("git");
            Ok((command, CLONE_TIMEOUT + Duration::from_secs(10)))
        }
    }
}

fn require_environment(environment: &ContentEnvironment) -> Result<(), Failure> {
    match environment {
        ContentEnvironment::Host => Ok(()),
        #[cfg(windows)]
        ContentEnvironment::Wsl { distribution } => {
            crate::file_tree::wsl_directory_distribution(Some(distribution))
                .map(|_| ())
                .map_err(|error| invalid(format!("Could not use the WSL distribution: {error}")))
        }
        #[cfg(unix)]
        ContentEnvironment::Wsl { .. } => Err(invalid(
            "WSL projects are only available on a Windows daemon host.",
        )),
    }
}

#[cfg(windows)]
fn wsl_launcher(distribution: &str) -> Command {
    let mut command = Command::new(r"C:\Windows\System32\wsl.exe");
    command.args(["--distribution", distribution]);
    command
}

fn wsl_env(environment: &ContentEnvironment) -> Result<Command, Failure> {
    #[cfg(windows)]
    if let ContentEnvironment::Wsl { distribution } = environment {
        if distribution.is_empty()
            || distribution.starts_with('-')
            || distribution.len() > 128
            || distribution.chars().any(char::is_control)
        {
            return Err(invalid("Invalid WSL distribution."));
        }
        let mut command = wsl_launcher(distribution);
        command.args(["--exec", "/usr/bin/env"]);
        return Ok(command);
    }
    let _ = environment;
    Err(invalid(
        "WSL projects are only available on a Windows daemon host.",
    ))
}

fn wsl_tool(environment: &ContentEnvironment, tool: &str) -> Result<Command, Failure> {
    let mut command = wsl_env(environment)?;
    command.args(["LC_ALL=C", tool]);
    Ok(command)
}

fn validate_url(url: &str) -> Result<(), Failure> {
    let rejected = || {
        invalid(
            "Use an HTTP(S) or SSH Git URL (including git@host:repo), without embedded credentials, query strings or fragments.",
        )
    };
    if url.is_empty()
        || url.len() > 4096
        || url.chars().any(char::is_control)
        || url.starts_with('-')
    {
        return Err(rejected());
    }
    if let Some((scheme, rest)) = url.split_once("://") {
        if !matches!(scheme, "http" | "https" | "ssh")
            || rest.contains(['?', '#', '\\'])
            || rest.chars().any(char::is_whitespace)
        {
            return Err(rejected());
        }
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        if (scheme == "ssh" && path.is_empty())
            || !valid_authority(authority, scheme == "ssh", true)
        {
            return Err(rejected());
        }
        return Ok(());
    }
    // For scp-style URLs a colon is part of a bracketed IPv6 host only until the closing
    // bracket. Double-colon remote helper syntax and Windows drive syntax are not SSH URLs.
    let pair = if let Some((bracket, _)) = url.split_once(']') {
        url.get(bracket.len() + 1..)
            .and_then(|tail| tail.strip_prefix(':'))
            .map(|path| (&url[..bracket.len() + 1], path))
            .or_else(|| url.split_once(':'))
    } else {
        url.split_once(':')
    };
    if let Some((authority, path)) = pair
        && !(authority.len() == 1 && authority.as_bytes()[0].is_ascii_alphabetic())
        && !path.is_empty()
        && !path.starts_with(':')
        && valid_authority(authority, true, false)
    {
        return Ok(());
    }
    Err(rejected())
}

fn valid_authority(authority: &str, user_allowed: bool, port_allowed: bool) -> bool {
    let host_port = if let Some((user, host)) = authority.split_once('@') {
        if !user_allowed || user.is_empty() || user.starts_with('-') || !user.bytes().all(host_byte)
        {
            return false;
        }
        host
    } else {
        authority
    };
    let (valid_host, port) = if let Some(bracketed) = host_port.strip_prefix('[') {
        let Some((address, suffix)) = bracketed.split_once(']') else {
            return false;
        };
        (address.parse::<std::net::Ipv6Addr>().is_ok(), suffix)
    } else {
        let host = host_port.split(':').next().unwrap_or_default();
        (
            !host.is_empty() && !host.starts_with('-') && host.bytes().all(host_byte),
            &host_port[host.len()..],
        )
    };
    valid_host
        && (port.is_empty()
            || (port_allowed
                && port.strip_prefix(':').is_some_and(|port| {
                    !port.is_empty()
                        && port.bytes().all(|byte| byte.is_ascii_digit())
                        && port.parse::<u16>().is_ok()
                })))
}

fn host_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+')
}

fn clone_timeout() -> String {
    format!(
        "Git clone timed out after {} seconds. Check the network and credentials, then try again.",
        CLONE_TIMEOUT.as_secs()
    )
}

/// Git/SSH, servers, helpers, and Git config can all put credentials in stderr, including
/// rewritten URLs unavailable to Compi. Recognize useful diagnostics, never echo raw text.
fn clone_failure(output: &Output) -> String {
    let stderr = output.stderr.to_ascii_lowercase();
    let has = |terms: &[&str]| terms.iter().any(|term| stderr.contains(term));
    let message = if has(&[
        "host key verification failed",
        "remote host identification has changed",
    ]) {
        "SSH could not verify the host key. Check known_hosts in the selected environment."
    } else if has(&[
        "permission denied (publickey",
        "authentication failed",
        "could not read username",
        "could not read password",
        "terminal prompts disabled",
        "returned error: 401",
        "returned error: 403",
    ]) {
        "Git could not authenticate. Check the credential helper, SSH agent or keys in the selected environment."
    } else if has(&[
        "repository not found",
        "does not appear to be a git repository",
        "returned error: 404",
    ]) {
        "The repository was not found. Check the URL and your access to the repository."
    } else if has(&[
        "could not resolve host",
        "could not resolve hostname",
        "failed to connect",
        "connection refused",
        "network is unreachable",
        "connection timed out",
    ]) {
        "Git could not reach the server. Check the hostname, network and proxy settings."
    } else if has(&[
        "certificate problem",
        "certificate verification",
        "schannel:",
        "ssl certificate",
    ]) {
        "Git could not verify the HTTPS certificate. Check the certificate trust settings in the selected environment."
    } else if has(&["no space left on device", "disk full"]) {
        "There is not enough disk space at the clone destination."
    } else if has(&[
        "unable to checkout",
        "unable to create file",
        "permission denied",
        "access is denied",
    ]) {
        "Git could not write or check out the repository. Check permissions and available space at the destination."
    } else if has(&["transport '"]) && has(&["not allowed"]) {
        "Git's URL rewrite selected a disallowed transport. Check the Git insteadOf configuration."
    } else if output.status.code() == Some(127) {
        "Git is not installed or is not on PATH in the selected environment."
    } else {
        return format!(
            "Git clone failed ({}). Check the repository URL and your access in the selected environment.",
            output.status
        );
    };
    message.into()
}

struct Output {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

fn run(command: &mut Command, timeout: Duration, want_stdout: bool) -> io::Result<Output> {
    if timeout.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    command
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(if want_stdout {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
    }
    let mut child = command.spawn()?;
    #[cfg(windows)]
    let job = unsafe { crate::conpty::create_kill_on_close_job(&child) }.ok();
    let stdout = child.stdout.take().map(read_pipe);
    let stderr = child.stderr.take().map(read_pipe);
    let deadline = Instant::now() + timeout;
    let status = loop {
        let error = match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Ok(None) => io::Error::new(io::ErrorKind::TimedOut, "process timed out"),
            Err(error) => error,
        };
        #[cfg(unix)]
        {
            // Terminate Git and its transport/helpers, not just the Git parent.
            unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
            let _ = child.kill();
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
        #[cfg(windows)]
        {
            drop(job);
            let _ = child.kill();
        }
        return Err(error);
    };
    #[cfg(windows)]
    drop(job);
    let grace = Instant::now() + Duration::from_millis(250);
    let collect = |reader: Option<thread::JoinHandle<Vec<u8>>>| {
        let Some(reader) = reader else {
            return String::new();
        };
        while !reader.is_finished() && Instant::now() < grace {
            thread::sleep(Duration::from_millis(5));
        }
        if !reader.is_finished() {
            // A custom helper can leave a background process holding the pipe open.
            return String::new();
        }
        crate::prompt::target::decode(&reader.join().unwrap_or_default())
    };
    Ok(Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

fn read_pipe(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = (&mut pipe).take(32 * 1024).read_to_end(&mut bytes);
        let _ = io::copy(&mut pipe, &mut io::sink());
        bytes
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_network_git_urls_without_embedded_credentials_are_accepted() {
        for url in [
            "https://github.com/team/project.git",
            "https://repo.example.test",
            "http://git.example.test:8080/project.git",
            "ssh://git@alias:2222/path/to/project.git",
            "ssh://git@[2001:db8::1]/project",
            "git@alias:team/project.git",
            "alias:team/project.git",
            "git@[2001:db8::1]:project.git",
        ] {
            assert!(validate_url(url).is_ok(), "rejected {url}");
        }
        for url in [
            "file:///home/user/repo",
            "C:/repo",
            "C:\\repo",
            "../repo",
            "--upload-pack=x",
            "git://github.com/team/project.git",
            "ext::ssh bad",
            "https::https://host/path",
            "ext::secret",
            "http://user:secret@host/repo",
            "https://secret@host/repo",
            "https://host/repo?token=secret",
            "ssh://git:secret@host/repo",
            "git@-option:repository",
            "git@host:\nsecret",
        ] {
            let (_, error) = validate_url(url).unwrap_err();
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn recognized_git_errors_do_not_return_server_or_helper_output() {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        #[cfg(windows)]
        use std::os::windows::process::ExitStatusExt;
        let output = Output {
            status: ExitStatus::from_raw(1),
            stdout: String::new(),
            stderr: "fatal: Authentication failed for 'https://user:secret@host/repo'".into(),
        };
        let error = clone_failure(&output);
        assert!(error.contains("credential helper"));
        assert!(!error.contains("secret"));
    }

    #[test]
    fn even_an_empty_existing_destination_is_not_used() {
        let path =
            std::env::temp_dir().join(format!("compi-clone-reservation-{}", std::process::id()));
        let path = path.to_str().unwrap();
        reserve_destination(&ContentEnvironment::Host, path).unwrap();
        assert_eq!(
            reserve_destination(&ContentEnvironment::Host, path)
                .unwrap_err()
                .0,
            ErrorCode::FileConflict
        );
        assert!(fs::read_dir(path).unwrap().next().is_none());
        fs::remove_dir(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_does_not_exit_hits_the_process_deadline() {
        let mut command = Command::new("sleep");
        command.arg("10");
        assert!(matches!(
            run(&mut command, Duration::from_millis(50), false),
            Err(error) if error.kind() == io::ErrorKind::TimedOut
        ));
    }

    #[cfg(windows)]
    #[test]
    fn wsl_git_stays_in_its_distribution_on_a_mounted_drive() {
        let environment = ContentEnvironment::Wsl {
            distribution: "Ubuntu".into(),
        };
        let (mut command, _) = clone_command(&environment).unwrap();
        command.args(["-C", "/mnt/c/src/new repo"]);
        assert!(command.get_program().to_string_lossy().ends_with("wsl.exe"));
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(
            &args[..4],
            ["--distribution", "Ubuntu", "--exec", "/usr/bin/env"]
        );
        assert!(args.contains(&"GIT_ALLOW_PROTOCOL=http:https:ssh"));
        assert_eq!(
            &args[args.len() - 3..],
            ["git", "-C", "/mnt/c/src/new repo"]
        );
    }
}
