use crate::Result;
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

#[cfg(windows)]
pub fn resolve_launch(working_directory: Option<&str>) -> Result<LaunchDescription> {
    let launch = compi_platform::wsl::resolve_launch(working_directory)?;
    let mut argv = Vec::new();
    if let Some(distribution) = launch.distribution {
        argv.extend([OsString::from("--distribution"), distribution.into()]);
    }
    argv.extend([
        "--cd".into(),
        launch.directory.into(),
        "--exec".into(),
        "/bin/bash".into(),
        "-i".into(),
    ]);
    Ok(LaunchDescription {
        executable: r"C:\Windows\System32\wsl.exe".into(),
        argv,
        cwd: None,
        env: Vec::new(),
        metadata: launch.metadata,
    })
}

#[cfg(unix)]
pub fn resolve_launch(working_directory: Option<&str>) -> Result<LaunchDescription> {
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
            .unwrap_or(account_home),
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
    if cfg!(target_os = "macos") {
        argv.push("-l".into());
    }
    argv.push("-i".into());
    Ok(LaunchDescription {
        executable: executable.clone(),
        argv,
        cwd: Some(cwd),
        env: vec![("SHELL".into(), executable)],
        // v7's directory metadata is WSL-specific; native paths stay in the
        // launch description rather than pretending to be a distribution.
        metadata: None,
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

pub fn check_system() -> Result<()> {
    #[cfg(windows)]
    compi_platform::wsl::ensure_default_wsl2()?;
    #[cfg(unix)]
    let _ = resolve_launch(None)?;
    #[cfg(unix)]
    validate_executable(std::ffi::OsStr::new("/bin/ps"))?;
    // Validate PTY availability too, without starting a shell or interpreting
    // startup files. Handles are closed immediately on success.
    let _ = portable_pty::native_pty_system().openpty(portable_pty::PtySize::default())?;
    Ok(())
}
