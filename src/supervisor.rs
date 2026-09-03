use crate::{Error, Result};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

pub const TASK_NAME: &str = "Compi Daemon";
const MAX_RESTARTS: usize = 3;
const HEALTHY_RUN: Duration = Duration::from_secs(60);
const RESTART_DELAYS: [Duration; MAX_RESTARTS] = [
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(30),
];

pub fn supervise(daemon_executable: &Path) -> Result<()> {
    if !daemon_executable.is_absolute() || !daemon_executable.is_file() {
        return Err(format!(
            "daemon executable does not exist at {}",
            daemon_executable.display()
        )
        .into());
    }

    let directory = env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .ok_or("LOCALAPPDATA is not set")?
        .join("Compi");
    fs::create_dir_all(&directory)?;
    let log_path = directory.join("daemon.log");
    let mut restarts = 0;

    loop {
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        writeln!(log, "starting supervised Compi daemon")?;
        let stdout = log.try_clone()?;
        let stderr = log.try_clone()?;
        let started_at = Instant::now();
        let status = Command::new(daemon_executable)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .creation_flags(CREATE_NO_WINDOW.0)
            .status();

        if status.as_ref().is_ok_and(|status| status.success()) {
            writeln!(log, "Compi daemon stopped intentionally")?;
            return Ok(());
        }
        if started_at.elapsed() >= HEALTHY_RUN {
            restarts = 0;
        }
        let detail = status
            .map(|status| status.to_string())
            .unwrap_or_else(|error| error.to_string());
        if restarts >= MAX_RESTARTS {
            writeln!(
                log,
                "Compi daemon failed after {MAX_RESTARTS} restart attempts: {detail}"
            )?;
            return Err(format!(
                "daemon failed after {MAX_RESTARTS} restart attempts: {detail}; inspect {}",
                log_path.display()
            )
            .into());
        }

        let delay = RESTART_DELAYS[restarts];
        restarts += 1;
        writeln!(
            log,
            "Compi daemon failed ({detail}); restart {restarts}/{MAX_RESTARTS} in {} seconds",
            delay.as_secs()
        )?;
        drop(log);
        thread::sleep(delay);
    }
}

pub fn install(daemon_executable: &Path) -> Result<()> {
    if !daemon_executable.is_absolute() || !daemon_executable.is_file() {
        return Err(format!(
            "daemon executable does not exist at {}",
            daemon_executable.display()
        )
        .into());
    }

    let sid = crate::identity::current_user_sid_string()?;
    let xml = task_xml(daemon_executable, &sid);
    let directory = env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .ok_or("LOCALAPPDATA is not set")?
        .join("Compi");
    fs::create_dir_all(&directory)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let xml_path = directory.join(format!("daemon-task-{}-{nonce}.xml", std::process::id()));
    fs::write(&xml_path, xml)?;

    let output = run_schtasks([
        "/Create",
        "/TN",
        TASK_NAME,
        "/XML",
        xml_path
            .to_str()
            .ok_or("scheduled-task XML path is not valid Unicode")?,
        "/F",
    ]);
    let _ = fs::remove_file(&xml_path);
    require_success(
        output?,
        "register the per-user Compi daemon task; reinstall or repair Compi",
    )
}
pub fn write_task_xml(daemon_executable: &Path, xml_path: &Path, user_sid: &str) -> Result<()> {
    if !daemon_executable.is_absolute() || !daemon_executable.is_file() {
        return Err(format!(
            "daemon executable does not exist at {}",
            daemon_executable.display()
        )
        .into());
    }
    if user_sid.is_empty() {
        return Err("Windows Installer did not provide the current user SID".into());
    }
    if let Some(parent) = xml_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(xml_path, task_xml(daemon_executable, user_sid))?;
    Ok(())
}

pub fn remove_task_xml(xml_path: &Path) -> Result<()> {
    match fs::remove_file(xml_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn uninstall() -> Result<()> {
    let query = run_schtasks(["/Query", "/TN", TASK_NAME])?;
    if !query.status.success() {
        return Ok(());
    }
    require_success(
        run_schtasks(["/Delete", "/TN", TASK_NAME, "/F"])?,
        "remove the per-user Compi daemon task",
    )
}

pub fn activate() -> Result<()> {
    require_success(
        run_schtasks(["/Run", "/TN", TASK_NAME])?,
        "activate the registered Compi daemon task; reinstall or repair Compi",
    )
}

fn run_schtasks<const N: usize>(args: [&str; N]) -> Result<Output> {
    let executable = env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .ok_or("SystemRoot is not set")?
        .join("System32")
        .join("schtasks.exe");
    Ok(Command::new(executable)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW.0)
        .output()?)
}

fn require_success(output: Output, operation: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let detail = if output.stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    } else {
        String::from_utf8_lossy(&output.stderr).trim().to_owned()
    };
    let detail = if detail.is_empty() {
        format!("exit status {}", output.status)
    } else {
        detail
    };
    Err(Error::from(format!("could not {operation}: {detail}")))
}

fn task_xml(daemon_executable: &Path, user_sid: &str) -> String {
    let command = xml_escape(&daemon_executable.to_string_lossy());
    let working_directory = daemon_executable
        .parent()
        .map(|path| xml_escape(&path.to_string_lossy()))
        .unwrap_or_default();
    let user_sid = xml_escape(user_sid);
    format!(
        r#"<?xml version="1.0"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Keeps Compi terminal sessions available while the Windows user is signed in.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user_sid}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="CompiUser">
      <UserId>{user_sid}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="CompiUser">
    <Exec>
      <Command>{command}</Command>
      <Arguments>--supervise</Arguments>
      <WorkingDirectory>{working_directory}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_is_per_user_restartable_and_unprivileged() {
        let xml = task_xml(
            Path::new(r"C:\Apps & Tools\compi-daemon.exe"),
            "S-1-5-21-1000",
        );
        assert!(xml.contains("<UserId>S-1-5-21-1000</UserId>"));
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(xml.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"));
        assert!(xml.contains("<RestartOnFailure>"));
        assert!(xml.contains("<Interval>PT1M</Interval>"));
        assert!(xml.contains("<Count>3</Count>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(xml.contains("<Arguments>--supervise</Arguments>"));
        assert!(xml.contains(r"C:\Apps &amp; Tools\compi-daemon.exe"));
        assert!(!xml.contains("HighestAvailable"));
    }
}
