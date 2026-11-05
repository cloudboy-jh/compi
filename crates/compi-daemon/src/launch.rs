use crate::{Result, shell_integration};
use compi_protocol::WorkingDirectory;
use std::ffi::OsString;
use std::path::PathBuf;

/// A host command, not a shell command string. `argv` excludes the executable.
/// Environment entries override inherited values and are never persisted.
#[derive(Debug, Clone)]
pub struct LaunchDescription {
    pub executable: OsString,
    pub argv: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(OsString, OsString)>,
    pub metadata: Option<WorkingDirectory>,
    /// The WSL2 distribution a Windows daemon starts the shell in; `None` on the host.
    pub distribution: Option<String>,
}

impl LaunchDescription {
    pub fn command(&self) -> Result<portable_pty::CommandBuilder> {
        if self.executable.is_empty() {
            return Err("launch executable must not be empty".into());
        }
        if let Some(cwd) = &self.cwd
            && (!cwd.is_absolute() || !cwd.is_dir())
        {
            return Err(format!(
                "working directory must be an existing absolute directory: {}",
                cwd.display()
            )
            .into());
        }
        let mut command = portable_pty::CommandBuilder::new(&self.executable);
        command.args(&self.argv);
        if let Some(cwd) = &self.cwd {
            command.cwd(cwd);
        }
        command.env("TERM", "xterm-256color");
        for (key, value) in &self.env {
            command.env(key, value);
        }
        Ok(command)
    }
}

#[cfg(unix)]
fn resolve_unix_launch(working_directory: Option<&str>) -> Result<LaunchDescription> {
    let (account_shell, account_home) = user_defaults()?;
    let executable = std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .unwrap_or(account_shell);
    validate_executable(&executable)?;
    let cwd = match working_directory {
        Some("") => return Err("working directory must not be empty".into()),
        Some(path) => PathBuf::from(path),
        None => std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| account_home.clone()),
    };
    if !cwd.is_absolute() || !cwd.is_dir() {
        return Err(format!(
            "working directory must be an existing absolute directory: {}",
            cwd.display()
        )
        .into());
    }
    let cwd = std::fs::canonicalize(cwd)?;
    let mut argv = Vec::new();
    let login = cfg!(target_os = "macos");
    if login {
        argv.push("-l".into());
    }
    argv.push("-i".into());
    Ok(LaunchDescription {
        executable: executable.clone(),
        argv,
        cwd: Some(cwd),
        env: vec![("SHELL".into(), executable)],
        metadata: None,
        distribution: None,
    })
}

#[cfg(unix)]
fn validate_executable(executable: &std::ffi::OsStr) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::path::Path::new(executable);
    let name = std::ffi::CString::new(executable.as_bytes())?;
    if !path.is_absolute()
        || !path.is_file()
        || unsafe { libc::access(name.as_ptr(), libc::X_OK) } != 0
    {
        return Err(format!(
            "configured shell is not an executable absolute path: {}",
            path.display()
        )
        .into());
    }
    Ok(())
}

/// The shell an automatic launch starts, for prompt-settings detection.
#[cfg(unix)]
pub(crate) fn default_shell() -> String {
    std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .or_else(|| user_defaults().ok().map(|(shell, _)| shell))
        .map(|shell| shell.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(unix)]
fn user_defaults() -> Result<(OsString, PathBuf)> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    let mut storage = vec![0_u8; 4096];
    loop {
        let mut account = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let status = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                account.as_mut_ptr(),
                storage.as_mut_ptr().cast(),
                storage.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE && storage.len() < 1024 * 1024 {
            storage.resize(storage.len() * 2, 0);
            continue;
        }
        if status != 0 {
            return Err(std::io::Error::from_raw_os_error(status).into());
        }
        if result.is_null() {
            return Err("current user has no account database entry".into());
        }
        let account = unsafe { account.assume_init() };
        let shell = unsafe { CStr::from_ptr(account.pw_shell) };
        let home = unsafe { CStr::from_ptr(account.pw_dir) };
        return Ok((
            std::ffi::OsStr::from_bytes(shell.to_bytes()).to_owned(),
            PathBuf::from(std::ffi::OsStr::from_bytes(home.to_bytes())),
        ));
    }
}

/// Resolve explicit client launch context without persisting environment values.
pub fn resolve_profile(
    request: &compi_protocol::LaunchRequest,
    context: Option<&compi_protocol::LaunchContext>,
) -> Result<LaunchDescription> {
    let empty = compi_protocol::LaunchProfile::default();
    let profile = context
        .map(|context| &context.profile)
        .or(request.profile.as_deref())
        .unwrap_or(&empty);
    let directory = request
        .working_directory
        .as_deref()
        .or(profile.working_directory.as_deref());
    let mut args: Vec<OsString> = Vec::new();
    if profile.login.unwrap_or(
        cfg!(target_os = "macos") && profile.executable.is_none() && profile.args.is_empty(),
    ) {
        args.push("-l".into());
    }
    if profile.args.is_empty() && profile.executable.is_none() {
        args.push("-i".into());
    } else {
        args.extend(profile.args.iter().map(OsString::from));
    }
    #[cfg(unix)]
    {
        if profile.distribution.is_some() {
            return Err("WSL distribution is not supported on this host".into());
        }
        let (account_shell, account_home) = user_defaults()?;
        let executable = profile
            .executable
            .as_ref()
            .map(OsString::from)
            .or_else(|| std::env::var_os("SHELL").filter(|shell| !shell.is_empty()))
            .unwrap_or(account_shell);
        validate_executable(&executable)?;
        let cwd = directory
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|home| !home.is_empty())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| account_home.clone());
        if !cwd.is_absolute() || !cwd.is_dir() {
            return Err(format!(
                "working directory must be an existing absolute directory: {}",
                cwd.display()
            )
            .into());
        }
        let mut env = vec![("SHELL".into(), executable.clone())];
        if let Some(context) = context {
            env.extend(
                context
                    .env
                    .iter()
                    .map(|(key, value)| (key.into(), value.into())),
            );
        }
        let automatic = profile.executable.is_none() && profile.args.is_empty();
        if automatic {
            let home = env
                .iter()
                .rev()
                .find(|(key, _)| key == "HOME")
                .map(|(_, value)| PathBuf::from(value))
                .or_else(|| {
                    std::env::var_os("HOME")
                        .filter(|home| !home.is_empty())
                        .map(PathBuf::from)
                })
                .unwrap_or_else(|| account_home.clone());
            let login = args.iter().any(|arg| arg == "-l");
            shell_integration::prepare_unix(&executable, &home, &mut env, &mut args, login)?;
        }
        Ok(LaunchDescription {
            executable,
            argv: args,
            cwd: Some(std::fs::canonicalize(cwd)?),
            env,
            metadata: None,
            distribution: None,
        })
    }
    #[cfg(windows)]
    {
        if profile.windows_shell == compi_protocol::WindowsShell::PowerShell {
            return resolve_powershell(profile, directory, context);
        }
        let resolved =
            compi_protocol::wsl::resolve_launch(directory, profile.distribution.as_deref())?;
        let executable = profile.executable.as_deref().unwrap_or("/bin/bash");
        if !executable.starts_with('/') {
            return Err("configured WSL executable must be an absolute Linux path".into());
        }
        let mut argv: Vec<OsString> = Vec::new();
        let distribution = resolved.distribution.as_str();
        argv.extend(["--distribution".into(), distribution.into()]);
        argv.extend(["--cd".into(), resolved.directory.into(), "--exec".into()]);
        let mut env: Vec<OsString> = context
            .map(|context| {
                context
                    .env
                    .iter()
                    .map(|(key, value)| OsString::from(format!("{key}={value}")))
                    .collect()
            })
            .unwrap_or_default();
        let integrated = profile.executable.is_none() && profile.args.is_empty();
        if integrated && let Some(folders) = shell_integration::windows_folders() {
            env.push(format!("COMPI_WINDOWS_FOLDERS={folders}").into());
        }
        if !env.is_empty() {
            argv.push("/usr/bin/env".into());
            argv.extend(env);
        }
        if integrated {
            let login = args.iter().any(|arg| arg == "-l");
            shell_integration::prepare_wsl(
                Some(distribution),
                context.and_then(|context| context.env.get("HOME").map(String::as_str)),
                context.and_then(|context| context.env.get("PROMPT_COMMAND").map(String::as_str)),
                login,
                &mut argv,
            )?;
        } else {
            argv.push(executable.into());
        }
        argv.extend(args);
        Ok(LaunchDescription {
            executable: r"C:\Windows\System32\wsl.exe".into(),
            argv,
            cwd: None,
            env: Vec::new(),
            metadata: resolved.metadata,
            distribution: Some(distribution.to_owned()),
        })
    }
}

/// Windows PowerShell on the host, in the requested folder or the user's profile folder.
#[cfg(windows)]
fn resolve_powershell(
    profile: &compi_protocol::LaunchProfile,
    directory: Option<&str>,
    context: Option<&compi_protocol::LaunchContext>,
) -> Result<LaunchDescription> {
    let executable = match &profile.executable {
        Some(executable) => PathBuf::from(executable),
        None => PathBuf::from(std::env::var_os("SystemRoot").ok_or("SystemRoot is not set")?)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
    };
    if !executable.is_absolute() || !executable.is_file() {
        return Err(format!(
            "configured shell is not an absolute Windows path: {}",
            executable.display()
        )
        .into());
    }
    // The default shell gets Compi's integration; a configured one runs as configured.
    let argv = if profile.executable.is_none() && profile.args.is_empty() {
        shell_integration::powershell_argv()
    } else {
        profile.args.iter().map(OsString::from).collect()
    };
    // A Linux folder, say a WSL pane's, opens as its Windows folder; one WSL lacks, at home.
    let cwd = match directory.and_then(|path| windows_directory(path).or_else(|| wsl_folder(path)))
    {
        Some(cwd) => PathBuf::from(cwd),
        None => PathBuf::from(
            std::env::var_os("USERPROFILE")
                .filter(|home| !home.is_empty())
                .ok_or("USERPROFILE is not set")?,
        ),
    };
    let env = context
        .map(|context| {
            context
                .env
                .iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect()
        })
        .unwrap_or_default();
    Ok(LaunchDescription {
        executable: executable.into(),
        argv,
        cwd: Some(cwd),
        env,
        metadata: None,
        distribution: None,
    })
}

/// The Windows folder of the default WSL distribution's Linux folder `/x`, through
/// `wslpath`, which follows Linux links into Windows drives: `C:\…` or
/// `\\wsl.localhost\<distribution>\x`. `None` without WSL or that folder.
#[cfg(windows)]
fn wsl_folder(path: &str) -> Option<String> {
    if !path.starts_with('/') {
        return None;
    }
    let distribution = compi_protocol::wsl::directory_distribution(None).ok()?;
    let folder = compi_protocol::wsl::windows_path_for_wsl(path, &distribution).ok()?;
    folder
        .is_dir()
        .then(|| folder.to_string_lossy().into_owned())
}

/// The Windows folder a requested path names: a Windows path as it is, and a WSL drive
/// mount (`/mnt/c/x`) or a `file://` drive path (`/C:/x`) as `C:\x`. `None` for a path
/// only Linux has.
#[cfg(any(windows, test))]
pub(crate) fn windows_directory(path: &str) -> Option<String> {
    let drive = |letter: u8, rest: &str| {
        letter.is_ascii_alphabetic().then(|| {
            format!(
                "{}:\\{}",
                letter.to_ascii_uppercase() as char,
                rest.replace('/', "\\")
            )
        })
    };
    match path.as_bytes() {
        [letter, b':', b'\\' | b'/', ..] if letter.is_ascii_alphabetic() => {
            Some(path.replace('/', "\\"))
        }
        [b'\\', b'\\', ..] => Some(path.to_owned()),
        [b'/', letter, b':'] => drive(*letter, ""),
        [b'/', letter, b':', b'/', ..] => drive(*letter, &path[4..]),
        _ => {
            let rest = path.strip_prefix("/mnt/")?;
            let (mount, tail) = rest.split_once('/').unwrap_or((rest, ""));
            match mount.as_bytes() {
                [letter] => drive(*letter, tail),
                _ => None,
            }
        }
    }
}

pub fn check_system() -> Result<()> {
    #[cfg(unix)]
    let _ = resolve_unix_launch(None)?;
    #[cfg(unix)]
    validate_executable(std::ffi::OsStr::new("/bin/ps"))?;
    // Validate PTY availability too, without starting a shell or interpreting
    // startup files. Handles are closed immediately on success.
    let _ = portable_pty::native_pty_system().openpty(portable_pty::PtySize::default())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_starts_in_the_windows_folder_a_path_names() {
        for (path, folder) in [
            (r"C:\Users\me\proj", Some(r"C:\Users\me\proj")),
            ("d:/work", Some(r"d:\work")),
            (
                r"\\wsl.localhost\Ubuntu\home",
                Some(r"\\wsl.localhost\Ubuntu\home"),
            ),
            ("/mnt/c/Users/me", Some(r"C:\Users\me")),
            ("/mnt/d", Some(r"D:\")),
            ("/C:/Users/me", Some(r"C:\Users\me")),
            ("/c:", Some(r"C:\")),
            ("/home/me", None),
            ("/mnt/wsl/x", None),
            ("~", None),
        ] {
            assert_eq!(windows_directory(path).as_deref(), folder, "{path}");
        }
    }

    #[test]
    fn explicit_non_shell_program_ignores_shell_integration() {
        let mut context = compi_protocol::LaunchContext::default();
        context.profile.executable = Some("/bin/echo".into());
        context.profile.args = vec!["unchanged".into()];
        context.profile.login = Some(false);
        context
            .env
            .insert("HOME".into(), "/compi-nonexistent-home-directory".into());
        let request = compi_protocol::LaunchRequest {
            working_directory: None,
            profile: None,
        };
        let launch = resolve_profile(&request, Some(&context)).unwrap();
        let output = std::process::Command::new(launch.executable)
            .args(launch.argv)
            .envs(launch.env)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"unchanged\n");
    }
}
