use super::installer::{InstallerOperation, InstallerSource};
use crate::{Error, Result};
use compi_update::{OperationLock, read_selection};
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::{
    env,
    ffi::c_void,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
pub(crate) struct MsiFailure {
    pub code: u32,
    pub message: String,
}
impl std::fmt::Display for MsiFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for MsiFailure {}
#[derive(Clone, Debug, Default)]
pub(crate) struct Outcome {
    pub restart_required: bool,
    pub version: Option<String>,
    pub cleanup: Option<std::result::Result<(), String>>,
}
#[derive(Clone, Debug)]
pub(crate) enum Event {
    Preflight(std::result::Result<(), String>),
    Progress {
        stage: String,
        percent: Option<u32>,
        cancellable: bool,
    },
    Finished(std::result::Result<Outcome, String>),
}
#[derive(Serialize, Deserialize)]
struct Journal {
    stage: String,
    package: Option<PathBuf>,
    previous_package: Option<PathBuf>,
    task_xml: Option<String>,
    legacy_task_xml: Option<String>,
    previous_selection: Option<compi_update::Selection>,
    // Older journals lack this receipt, so absence is not proof of a fresh install.
    #[serde(default)]
    initial_install: Option<bool>,
}

pub(crate) fn root() -> Result<PathBuf> {
    let base = env::var_os("LOCALAPPDATA")
        .ok_or("LOCALAPPDATA is unavailable; use your signed-in Windows profile")?;
    Ok(PathBuf::from(base).join("Programs").join("Compi"))
}
pub(crate) fn data_root() -> Result<PathBuf> {
    Ok(
        PathBuf::from(env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is unavailable")?)
            .join("Compi"),
    )
}
pub(crate) fn log_path() -> Result<PathBuf> {
    Ok(data_root()?.join("installer.log"))
}
pub(crate) fn powershell(script: &str) -> Result<std::process::Output> {
    let exe = PathBuf::from(env::var_os("SystemRoot").ok_or("SystemRoot is unavailable")?)
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let script = format!("[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); {script}");
    Ok(Command::new(exe)
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x08000000)
        .output()?)
}

/// Inventory executable paths, not names alone: every named instance under this installation
/// is included, while another portable/installed Compi remains untouched.
pub(crate) fn guard(root: &Path, operation: InstallerOperation) -> Result<()> {
    let legacy = !root.join("selection.json").is_file() && root.join("compi-daemon.exe").is_file();
    if legacy
        && compi_protocol::DaemonClient::endpoint_available(
            None,
            std::time::Duration::from_millis(250),
        )?
    {
        return Err("Legacy migration is deferred: the old MSI removal hook addresses the default daemon even when it belongs to another installation. Let that instance's owner deliberately stop it before migration, or keep using the old install. Setup will not stop unrelated work.".into());
    }
    if operation == InstallerOperation::Install || root.join("Compi-Setup.exe").is_file() {
        guard_task_ownership(root)?;
    }
    let statuses = compi_protocol::DaemonClient::local_lifecycle_statuses_for_install(root)
        .map_err(|error| Error::from(format!("Cannot safely query this installation's named instances: {error}. Deliberately stop them before retrying; their shells will end. For the legacy default instance, run this installation's `compi-daemon.exe --shutdown` and let its supervisor exit. Stop named instances through the old app's explicit stop action, or deliberately end only their owned daemon PID in Task Manager. Close their windows, then Recheck. Setup never performs that stop or ends unrelated work.")))?;
    if operation != InstallerOperation::Install && !statuses.is_empty() {
        let work = statuses
            .iter()
            .map(|status| {
                format!(
                    "{}: {} attached clients, {} live surfaces",
                    status.instance.as_deref().unwrap_or("default"),
                    status.connected_clients.len(),
                    status.live_surfaces.len()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!("This installation still has active daemon instances:\n{work}\nDeliberately stop each local instance in Compi (this ends shells), close its windows, then Recheck. Other installations are not stopped.").into());
    }
    if operation == InstallerOperation::Install && !legacy {
        return Ok(());
    }
    let escaped = root.to_string_lossy().replace('\'', "''");
    let current = std::process::id();
    let script = format!(
        "$ErrorActionPreference='Stop'; $root=[IO.Path]::GetFullPath('{escaped}').TrimEnd('\\')+'\\'; Get-CimInstance Win32_Process | Where-Object {{ $_.Name -in @('compi.exe','compi-daemon.exe','compi-update-worker.exe') -and $_.ProcessId -ne {current} }} | ForEach-Object {{ if (-not $_.ExecutablePath) {{ throw ('Cannot account for process '+$_.ProcessId) }}; if ($_.ExecutablePath.StartsWith($root,[StringComparison]::OrdinalIgnoreCase)) {{ Write-Output ($_.Name+' PID '+$_.ProcessId+' '+$_.CommandLine) }} }}"
    );
    let result = powershell(&script)?;
    if !result.status.success() {
        return Err(format!("Cannot safely account for active Compi instances: {}. Stop Compi deliberately and recheck; no process was terminated.", String::from_utf8_lossy(&result.stderr)).into());
    }
    let active = String::from_utf8_lossy(&result.stdout);
    if !active.trim().is_empty() {
        return Err(format!("Compi still owns running work:\n{active}\nIn the existing Compi app, deliberately stop each listed local instance (this ends its shells), close its windows, then recheck. Legacy releases cannot provide safe lifecycle consent. Setup will not stop or force-kill them.").into());
    }
    Ok(())
}

pub(crate) fn guard_version(root: &Path, candidate: &str) -> Result<()> {
    if let Some(selected) = read_selection(root)? {
        if semver::Version::parse(candidate)? < semver::Version::parse(&selected.version)? {
            return Err(format!("This setup contains Compi {candidate}, but this installation selects newer version {}. Obtain setup for that version or newer, or use Compi Updates. Repair with an older MSI would downgrade the selected payload, so no mutation was started.", selected.version).into());
        }
    }
    Ok(())
}
pub(crate) fn preflight(operation: InstallerOperation) -> Result<()> {
    super::installer::ensure_supported_windows()?;
    let root = root()?;
    if operation != InstallerOperation::Remove {
        guard_version(&root, env!("CARGO_PKG_VERSION"))?;
    }
    guard(&root, operation)?;
    fs::create_dir_all(&root).map_err(|error| Error::from(format!(
        "Cannot prepare the installation folder: {error}. Check that the destination is a folder, not a file, and your account has write access, then Recheck."
    )))?;
    let probe = root.join(format!(".write-check-{}", std::process::id()));
    fs::write(&probe, b"access check").map_err(|error| Error::from(format!(
        "Cannot write to the installation folder: {error}. Check free disk space and your account's write permissions, then Recheck."
    )))?;
    fs::remove_file(&probe).map_err(|error| Error::from(format!(
        "Cannot remove the installation access-check file: {error}. Check your account's delete permissions for this folder, then Recheck."
    )))?;
    if operation != InstallerOperation::Install {
        return Ok(());
    }
    if !cfg!(target_arch = "x86_64") {
        return Err("This installer requires Windows x64; Windows ARM64 is not supported".into());
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            path: *const u16,
            available: *mut u64,
            total: *mut u64,
            free: *mut u64,
        ) -> i32;
    }
    let path = wide(root.as_os_str());
    let mut available = 0u64;
    if unsafe {
        GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    if available < 300 * 1024 * 1024 {
        return Err("Compi setup needs at least 300 MiB free on the installation drive for payload staging and rollback. Free space, then Recheck.".into());
    }
    compi_protocol::wsl::ensure_default_wsl2().map_err(|error| Error::from(format!("{error}\nInstall WSL with `wsl --install`, list distributions with `wsl --list --verbose`, select one with `wsl --set-default NAME`, or convert it with `wsl --set-version NAME 2`. Reboot if Windows requests it, then Recheck.")))?;
    let wsl = PathBuf::from(env::var_os("SystemRoot").ok_or("SystemRoot is unavailable")?)
        .join("System32/wsl.exe");
    let mut guest = Command::new(wsl)
        .args([
            "--exec",
            "/bin/sh",
            "-c",
            "test -r /proc/version && test -d /dev/pts",
        ])
        .creation_flags(0x08000000)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = guest.try_wait()? {
            if status.success() {
                return Ok(());
            }
            return Err(format!("The default WSL2 guest did not start successfully ({status}). Open `wsl` in Windows Terminal and finish its first-run setup or repair the guest, then Recheck.").into());
        }
        if started.elapsed() > std::time::Duration::from_secs(30) {
            // Only the prerequisite probe is terminated, never an MSI, daemon, or guest distribution.
            guest.kill()?;
            guest.wait()?;
            return Err("The default WSL2 guest did not respond within 30 seconds. Open `wsl` in Windows Terminal, resolve its startup prompt/error, then Recheck.".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

pub(crate) fn task_name() -> Result<String> {
    Ok(format!(
        "Compi Daemon-{}",
        compi_protocol::identity::current_user_sid_string()?
    ))
}
pub(crate) fn task_snapshot() -> Result<Option<String>> {
    task_snapshot_named(&task_name()?)
}
fn task_snapshot_named(name: &str) -> Result<Option<String>> {
    let name = name.replace('\'', "''");
    let result = powershell(&format!(
        "$ErrorActionPreference='Stop'; $t=Get-ScheduledTask -ErrorAction Stop | Where-Object {{ $_.TaskName -eq '{name}' -and $_.TaskPath -eq '\\' }}; if ($t) {{ Export-ScheduledTask -TaskName '{name}' -TaskPath '\\' }}"
    ))?;
    if !result.status.success() {
        return Err(format!(
            "Cannot back up daemon task: {}",
            String::from_utf8_lossy(&result.stderr)
        )
        .into());
    }
    let xml = String::from_utf8(result.stdout)?.trim().to_owned();
    Ok((!xml.is_empty()).then_some(xml))
}
pub(crate) fn legacy_task_snapshot(root: &Path) -> Result<Option<String>> {
    if !root.join("compi-daemon.exe").is_file() {
        return Ok(None);
    }
    guard_task_named(root, "Compi Daemon", true)?;
    task_snapshot_named("Compi Daemon")
}
pub(crate) fn remove_owned_legacy_task(root: &Path) -> Result<()> {
    if legacy_task_snapshot(root)?.is_none() {
        return Ok(());
    }
    let output = Command::new("schtasks.exe")
        .args(["/Delete", "/TN", "Compi Daemon", "/F"])
        .creation_flags(0x08000000)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "Could not remove the inactive, owned legacy task: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}
pub(crate) fn restore_task(xml: Option<&str>, attempt: &Path) -> Result<()> {
    restore_task_named(xml, attempt, &task_name()?)
}
pub(crate) fn restore_legacy_task(xml: &str, attempt: &Path, root: &Path) -> Result<()> {
    guard_task_named(root, "Compi Daemon", true)?;
    restore_task_named(Some(xml), attempt, "Compi Daemon")
}
fn restore_task_named(xml: Option<&str>, attempt: &Path, name: &str) -> Result<()> {
    if task_snapshot_named(name)?.as_deref() == xml {
        return Ok(());
    }
    let args = if let Some(xml) = xml {
        let path = attempt.join("previous-task.xml");
        let bytes: Vec<u8> = std::iter::once(0xfeffu16)
            .chain(xml.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect();
        fs::write(&path, bytes)?;
        vec![
            "/Create".to_owned(),
            "/TN".into(),
            name.into(),
            "/XML".into(),
            path.display().to_string(),
            "/F".into(),
        ]
    } else {
        vec!["/Delete".into(), "/TN".into(), name.into(), "/F".into()]
    };
    let output = Command::new("schtasks.exe")
        .args(args)
        .creation_flags(0x08000000)
        .output()?;
    if !output.status.success() && (xml.is_some() || task_snapshot_named(name)?.is_some()) {
        return Err(format!(
            "Task rollback failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}
pub(crate) fn guard_task_ownership(root: &Path) -> Result<()> {
    guard_task_named(root, &task_name()?, false)?;
    if root.join("compi-daemon.exe").is_file() {
        // The old MSI has uneditable fixed-name removal actions, so a foreign legacy
        // registration is a blocker only for that genuine migration, not fresh installs.
        guard_task_named(root, "Compi Daemon", true)?;
    }
    Ok(())
}
fn guard_task_named(root: &Path, name: &str, inactive: bool) -> Result<()> {
    let escaped = root.to_string_lossy().replace('\'', "''");
    let name = name.replace('\'', "''");
    let inactive_check = if inactive {
        "if ($t.State -notin @('Ready','Disabled')) { throw 'The legacy task is running or queued. Deliberately run the old installed compi-daemon.exe --shutdown (ends default-instance shells), wait for its supervisor to exit, and close its windows before migration.' }"
    } else {
        ""
    };
    let script = format!(
        "$ErrorActionPreference='Stop'; $t=Get-ScheduledTask -ErrorAction Stop | Where-Object {{ $_.TaskName -eq '{name}' -and $_.TaskPath -eq '\\' }}; if ($t) {{ $id=[Security.Principal.WindowsIdentity]::GetCurrent(); $owner=$t.Principal.UserId; if ($owner -ne $id.User.Value -and $owner -ne $id.Name) {{ throw ('The task {name} belongs to another Windows account: '+$owner+'. Ask its owner to migrate/remove that legacy registration safely; it was not changed.') }}; $root=[IO.Path]::GetFullPath('{escaped}').TrimEnd('\\')+'\\'; foreach ($a in $t.Actions) {{ if (-not $a.Execute -or -not $a.Execute.Trim('\"').StartsWith($root,[StringComparison]::OrdinalIgnoreCase)) {{ throw 'The daemon task belongs to another installation. Its registration was not changed.' }} }}; {inactive_check} }}"
    );
    let output = powershell(&script)?;
    if !output.status.success() {
        return Err(format!(
            "Cannot safely change daemon task registration: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}
fn persist(path: &Path, journal: &Journal) -> Result<()> {
    let next = path.with_extension("next");
    fs::write(&next, serde_json::to_vec(journal)?)?;
    fs::OpenOptions::new().write(true).open(&next)?.sync_all()?;
    atomic_replace(&next, path)
}
pub(crate) fn atomic_replace(source: &Path, target: &Path) -> Result<()> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let source = wide(source.as_os_str());
    let target = wide(target.as_os_str());
    if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), 1 | 8) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

struct MsiRecoveryGuard([isize; 2]);
impl MsiRecoveryGuard {
    fn acquire() -> Result<Self> {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn OpenMutexW(access: u32, inherit: i32, name: *const u16) -> isize;
            fn WaitForSingleObject(handle: isize, milliseconds: u32) -> u32;
        }
        let mut guard = Self([0; 2]);
        for (index, name) in ["Global\\_MSIExecute", "_MSIExecute"].iter().enumerate() {
            let name = wide(std::ffi::OsStr::new(name));
            let handle = unsafe { OpenMutexW(0x00100001, 0, name.as_ptr()) };
            if handle == 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(2) {
                    continue;
                }
                return Err(error.into());
            }
            let wait = unsafe { WaitForSingleObject(handle, 0) };
            if matches!(wait, 0 | 0x80) {
                guard.0[index] = handle;
            } else {
                #[link(name = "kernel32")]
                unsafe extern "system" {
                    fn CloseHandle(handle: isize) -> i32;
                }
                unsafe { CloseHandle(handle) };
                return Err("Windows Installer still owns an active transaction. Let it finish recovery, then retry Setup or Repair; no recovery state was changed.".into());
            }
        }
        Ok(guard)
    }
}
impl Drop for MsiRecoveryGuard {
    fn drop(&mut self) {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn ReleaseMutex(handle: isize) -> i32;
            fn CloseHandle(handle: isize) -> i32;
        }
        for handle in self.0.iter().filter(|handle| **handle != 0) {
            unsafe {
                ReleaseMutex(*handle);
                CloseHandle(*handle);
            }
        }
    }
}

fn selected_payload_is_runnable(root: &Path, selection: &compi_update::Selection) -> bool {
    let payload = root.join("versions").join(&selection.version);
    ["compi.exe", "compi-daemon.exe", "compi-update-worker.exe"]
        .iter()
        .all(|name| payload.join(name).is_file())
        && root
            .join("versions")
            .join(&selection.task_version)
            .join("compi-daemon.exe")
            .is_file()
}

fn recover_initial_install(
    root: &Path,
    journal: &mut Journal,
    journal_path: &Path,
    registered_version: Option<&str>,
) -> Result<()> {
    let selected = read_selection(root)?;
    if journal.initial_install == Some(false) || journal.legacy_task_xml.is_some() {
        return Err("Interrupted legacy installation has no verified prior payload. Its recovery journal was retained; repair the prior Windows Installer product before retrying migration.".into());
    }
    if journal.initial_install != Some(true) {
        // A committed, runnable native registration can establish the new layout even for
        // an older journal. An empty root cannot distinguish legacy loss from first rollback.
        let verified = selected.as_ref().is_some_and(|selection| {
            registered_version == Some(selection.version.as_str())
                && selected_payload_is_runnable(root, selection)
                && journal.previous_package.is_none()
                && journal.task_xml.is_none()
        });
        if !verified {
            return Err("Interrupted installation predates the initial-install receipt and has no verified current native payload. Recovery cannot distinguish first-install rollback from a lost legacy install; its journal and user data were retained.".into());
        }
    }
    if let Some(selection) = &selected {
        if registered_version != Some(selection.version.as_str())
            || !selected_payload_is_runnable(root, selection)
        {
            return Err("Interrupted installation has a selection that does not match a runnable registered MSI payload. Its selection and recovery journal were retained; Windows Installer must finish recovery before retrying.".into());
        }
    }
    // Do not replay pre-install task XML or remove a selection: MSI may have committed
    // successfully before Setup died. The next transaction still performs real repair/install.
    journal.stage = "reconciled-for-retry".into();
    persist(journal_path, journal)
}

fn recover(root: &Path, installer: &Path) -> Result<()> {
    let _msi = MsiRecoveryGuard::acquire()?;
    for entry in fs::read_dir(installer)? {
        let attempt = entry?.path();
        let path = attempt.join("journal.json");
        if !path.is_file() {
            continue;
        }
        let mut journal: Journal = serde_json::from_slice(&fs::read(&path)?)?;
        match journal.stage.as_str() {
            "applying" | "rolling-back" | "rollback-incomplete" => {
                let registered_version = owned_installed_product_version(root)?;
                guard_task_ownership(root)?;
                if journal.previous_selection.is_none() && !root.join("compi-daemon.exe").is_file()
                {
                    recover_initial_install(root, &mut journal, &path, registered_version.as_deref())?;
                    continue;
                } else {
                    if let Some(previous) = &journal.previous_selection {
                        if !selected_payload_is_runnable(root, previous) {
                            return Err("Interrupted upgrade's prior runnable payload is unavailable. Its selection, task, journal and user data were retained; no stale rollback was applied.".into());
                        }
                    }
                    restore_task(journal.task_xml.as_deref(), &attempt)?;
                    if let Some(xml) = journal.legacy_task_xml.as_deref() {
                        restore_legacy_task(xml, &attempt, root)?;
                    }
                    compi_update::restore_selection(root, journal.previous_selection.as_ref())?;
                    if let Some(previous) = &journal.previous_package {
                        let next = installer.join("Compi.recovering.msi");
                        fs::copy(previous, &next)?;
                        atomic_replace(&next, &installer.join("Compi.msi"))?;
                    }
                    journal.stage = "rolled-back".into();
                }
                persist(&path, &journal)?;
            }
            "staging" => {
                journal.stage = "cancelled-before-apply".into();
                persist(&path, &journal)?;
            }
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn perform(
    source: InstallerSource,
    operation: InstallerOperation,
    remove_data: bool,
    cancel: Arc<AtomicBool>,
    sender: async_channel::Sender<Event>,
) -> Result<Outcome> {
    if let InstallerSource::Package(bytes) = &source {
        if !bytes.starts_with(b"\xd0\xcf\x11\xe0") {
            return Err("embedded Windows Installer payload is invalid".into());
        }
    }
    preflight(operation)?;
    let root = root()?;
    let _lock = OperationLock::acquire(&root)?;
    let installer = data_root()?.join("installer");
    fs::create_dir_all(&installer)?;
    recover(&root, &installer)?;
    let previous_selection = read_selection(&root)?;
    let product_code = match &source {
        InstallerSource::ProductCode(code) => Some(code.clone()),
        _ => None,
    };
    let id = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let attempt = installer.join(&id);
    fs::create_dir(&attempt)?;
    let journal_path = attempt.join("journal.json");
    let cache = installer.join("Compi.msi");
    let previous_package = cache.is_file().then(|| attempt.join("previous.msi"));
    if let Some(path) = &previous_package {
        fs::copy(&cache, path)?;
    }
    let mut journal = Journal {
        stage: "staging".into(),
        package: None,
        previous_package,
        task_xml: task_snapshot()?,
        legacy_task_xml: legacy_task_snapshot(&root)?,
        previous_selection: previous_selection.clone(),
        initial_install: Some(
            previous_selection.is_none()
                && !root.join("compi-daemon.exe").is_file()
                && owned_installed_product_version(&root)?.is_none(),
        ),
    };
    let package = match source {
        InstallerSource::Package(bytes) => {
            let path = attempt.join("Compi.msi");
            fs::write(&path, bytes)?;
            journal.package = Some(path.clone());
            path
        }
        InstallerSource::ProductCode(code) => {
            if operation == InstallerOperation::Install {
                return Err("Installation requires a package".into());
            }
            PathBuf::from(code)
        }
    };
    persist(&journal_path, &journal)?;
    if cancel.load(Ordering::Acquire) {
        journal.stage = "cancelled-before-apply".into();
        persist(&journal_path, &journal)?;
        return Err(MsiFailure {
            code: 1602,
            message: "Cancelled before applying files. No product files were changed.".into(),
        }
        .into());
    }
    let _ = sender.send_blocking(Event::Progress {
        stage: "Applying Windows Installer transaction".into(),
        percent: None,
        cancellable: true,
    });
    journal.stage = "applying".into();
    persist(&journal_path, &journal)?;
    let snapshot = root.join(".compi-update/msi-snapshot.json");
    let before_snapshot = fs::metadata(&snapshot)
        .and_then(|metadata| metadata.modified())
        .ok();
    let code = match run_msi(
        &package,
        operation,
        journal.package.is_some(),
        &cancel,
        &sender,
    ) {
        Ok(code) => code,
        Err(error) => {
            journal.stage = "failed-before-apply".into();
            persist(&journal_path, &journal)?;
            return Err(error);
        }
    };
    if !matches!(code, 0 | 1641 | 3010) {
        journal.stage = "rolling-back".into();
        persist(&journal_path, &journal)?;
        let _ = sender.send_blocking(Event::Progress {
            stage: "Waiting for rollback".into(),
            percent: None,
            cancellable: false,
        });
        let mut failures = Vec::new();
        let after_snapshot = fs::metadata(&snapshot)
            .and_then(|metadata| metadata.modified())
            .ok();
        if after_snapshot != before_snapshot {
            if let Err(error) = restore_task(journal.task_xml.as_deref(), &attempt) {
                failures.push(error.to_string());
            }
            if let Some(xml) = journal.legacy_task_xml.as_deref() {
                if let Err(error) = restore_legacy_task(xml, &attempt, &root) {
                    failures.push(error.to_string());
                }
            }
            if let Err(error) = compi_update::restore_selection(&root, previous_selection.as_ref())
            {
                failures.push(error.to_string());
            }
        }
        journal.stage = if failures.is_empty() {
            "rolled-back"
        } else {
            "rollback-incomplete"
        }
        .into();
        persist(&journal_path, &journal)?;
        let reason = if code == 1602 {
            "Cancelled. Windows Installer completed rollback".to_owned()
        } else {
            format!("Windows Installer failed with code {code}")
        };
        return Err(MsiFailure {
            code,
            message: format!(
                "{reason}. {}\nLog: {}\nRecovery journal: {}",
                failures.join("; "),
                log_path()?.display(),
                journal_path.display()
            ),
        }
        .into());
    }
    if let Some(staged) = &journal.package {
        if operation != InstallerOperation::Remove {
            let next = installer.join(format!("Compi.{id}.next.msi"));
            fs::copy(staged, &next)?;
            atomic_replace(&next, &cache)?;
        }
    }
    journal.stage = "complete".into();
    persist(&journal_path, &journal)?;
    let version = if operation == InstallerOperation::Remove {
        None
    } else {
        Some(match product_code {
            Some(code) => installed_product_version(&code)
                .unwrap_or_else(|error| format!("unavailable ({error})")),
            None => env!("CARGO_PKG_VERSION").to_owned(),
        })
    };
    let mut outcome = Outcome {
        restart_required: code != 0,
        cleanup: None,
        version,
    };
    if operation == InstallerOperation::Remove && remove_data {
        // The managed directory is exact. Never traverse directory junctions/symlinks.
        outcome.cleanup = Some((|| {
            let active = powershell("$ErrorActionPreference='Stop'; Get-CimInstance Win32_Process | Where-Object { $_.Name -in @('compi.exe','compi-daemon.exe') } | ForEach-Object { Write-Output ($_.Name+' PID '+$_.ProcessId) }")?;
            if !active.status.success() || !active.stdout.is_empty() {
                return Err(Error::from("Product removed, but managed-data cleanup was deferred because another Compi installation/instance may still use this profile. Stop it deliberately before manually removing the displayed managed path."));
            }
            clean_profile_data(&data_root()?)
        })().map_err(|error: Error| error.to_string()));
    }
    Ok(outcome)
}

fn check_owned_tree(path: &Path) -> Result<()> {
    use std::os::windows::fs::MetadataExt;
    let meta = fs::symlink_metadata(path)?;
    if meta.file_attributes() & 0x400 != 0 {
        return Err(format!(
            "Cleanup refused a reparse point at {}. Remove it manually; its target was not touched",
            path.display()
        )
        .into());
    }
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            check_owned_tree(&entry?.path())?;
        }
    }
    Ok(())
}
pub(crate) fn clean_managed_data(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    check_owned_tree(path)?;
    fs::remove_dir_all(path)?;
    Ok(())
}

pub(crate) const MANAGED_DATA_DIRECTORIES: &[&str] = &[
    "client-state-v1",
    "themes",
    "themes-v1",
    "uploaded-images",
    "update-gui-hosts-v1",
];
fn managed_state_file(name: &str) -> bool {
    if name == "config.toml" {
        return true;
    }
    let name = name.strip_suffix(".migration-backup").unwrap_or(name);
    let name = name.strip_suffix(".tmp").unwrap_or(name);
    let Some(mut stem) = name.strip_suffix(".json") else {
        return false;
    };
    if let Some((original, suffix)) = stem.split_once(".corrupt-") {
        if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit() || c == '-') {
            return false;
        }
        stem = original;
    }
    for prefix in ["workspace", "sessions"] {
        let Some(rest) = stem.strip_prefix(prefix) else {
            continue;
        };
        let instance = rest.strip_suffix("-v1").or_else(|| {
            if prefix == "sessions" {
                rest.strip_suffix("-v2")
            } else {
                None
            }
        });
        let Some(instance) = instance else {
            continue;
        };
        if instance.is_empty() {
            return true;
        }
        if let Some(instance) = instance.strip_prefix('-') {
            return compi_protocol::identity::instance_names(Some(instance)).is_ok();
        }
    }
    false
}
pub(crate) fn clean_profile_data(root: &Path) -> Result<()> {
    use std::os::windows::fs::MetadataExt;
    if !root.exists() {
        return Ok(());
    }
    if fs::symlink_metadata(root)?.file_attributes() & 0x400 != 0 {
        return Err(
            "The Compi data directory is a reparse point; its target was not touched".into(),
        );
    }
    let mut selected = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        let managed_directory = MANAGED_DATA_DIRECTORIES.contains(&name);
        let managed_file = managed_state_file(name);
        if managed_directory || managed_file {
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(format!(
                    "Cleanup refused a reparse point at {}; external targets were not touched",
                    path.display()
                )
                .into());
            }
            if (managed_directory && metadata.is_dir()) || (managed_file && metadata.is_file()) {
                check_owned_tree(&path)?;
                selected.push((path, metadata.is_dir()));
            }
        }
    }
    // All selected trees are checked before the first deletion. Unrecognized project
    // directories and files remain, as do installer packages and recovery logs.
    for (path, directory) in selected {
        if directory {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

#[link(name = "msi")]
unsafe extern "system" {
    fn MsiSetInternalUI(level: u32, window: *mut isize) -> u32;
    fn MsiSetExternalUIRecord(
        callback: Option<unsafe extern "system" fn(*const c_void, u32, u32) -> i32>,
        filter: u32,
        context: *const c_void,
        previous: *mut *const c_void,
    ) -> u32;
    fn MsiRecordGetInteger(record: u32, field: u32) -> i32;
    fn MsiInstallProductW(package: *const u16, properties: *const u16) -> u32;
    fn MsiConfigureProductExW(
        product: *const u16,
        level: i32,
        state: i32,
        properties: *const u16,
    ) -> u32;
    fn MsiEnableLogW(mode: u32, path: *const u16, attributes: u32) -> u32;
    fn MsiGetProductInfoW(
        product: *const u16,
        property: *const u16,
        value: *mut u16,
        count: *mut u32,
    ) -> u32;
    fn MsiEnumRelatedProductsW(
        upgrade: *const u16,
        reserved: u32,
        index: u32,
        product: *mut u16,
    ) -> u32;
    fn MsiQueryProductStateW(product: *const u16) -> i32;
}
struct Callback {
    cancel: Arc<AtomicBool>,
    sender: async_channel::Sender<Event>,
    total: u32,
    completed: u32,
    step: u32,
    backward: bool,
    cancellable: bool,
}
unsafe extern "system" fn msi_callback(context: *const c_void, message: u32, record: u32) -> i32 {
    let context = unsafe { &mut *(context as *mut Callback) };
    let kind = message & 0xff000000;
    if kind == 0x0a000000 {
        let field = |i| unsafe { MsiRecordGetInteger(record, i) }.max(0) as u32;
        match field(1) {
            0 => {
                context.total = field(2);
                context.backward = field(3) != 0;
                context.completed = if context.backward { context.total } else { 0 };
            }
            1 => context.step = if field(3) != 0 { field(2) } else { 0 },
            2 => {
                let ticks = field(2);
                context.completed = if context.backward {
                    context.completed.saturating_sub(ticks)
                } else {
                    context.completed.saturating_add(ticks)
                };
            }
            3 => context.total = context.total.saturating_add(field(2)),
            _ => {}
        }
    } else if kind == 0x09000000 && context.step != 0 {
        context.completed = if context.backward {
            context.completed.saturating_sub(context.step)
        } else {
            context.completed.saturating_add(context.step)
        };
    }
    // COMMONDATA field 1 == 2 reports whether MSI permits cancellation.
    if kind == 0x0b000000 && unsafe { MsiRecordGetInteger(record, 1) } == 2 {
        context.cancellable = unsafe { MsiRecordGetInteger(record, 2) } != 0;
    }
    if matches!(kind, 0x0a000000 | 0x0b000000 | 0x08000000) {
        let _ = context.sender.try_send(Event::Progress {
            stage: if context.backward {
                "Rolling back"
            } else if !context.cancellable {
                "Committing, cancellation unavailable"
            } else {
                "Applying application files and registration"
            }
            .into(),
            percent: (context.total != 0).then(|| {
                ((u64::from(context.completed.min(context.total)) * 100) / u64::from(context.total))
                    as u32
            }),
            cancellable: context.cancellable && !context.backward,
        });
    }
    if context.cancel.load(Ordering::Acquire) && context.cancellable && !context.backward {
        2 /* IDCANCEL, supported MSI rollback */
    } else {
        0
    }
}
fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(Some(0)).collect()
}
fn installed_product_version(product: &str) -> Result<String> {
    installed_product_property(product, "VersionString")
}

fn installed_product_property(product: &str, property: &str) -> Result<String> {
    let product = wide(std::ffi::OsStr::new(product));
    let property = wide(std::ffi::OsStr::new(property));
    let mut count = 0u32;
    let code = unsafe {
        MsiGetProductInfoW(
            product.as_ptr(),
            property.as_ptr(),
            std::ptr::null_mut(),
            &mut count,
        )
    };
    if !matches!(code, 0 | 234) || count > 32768 {
        return Err(format!(
            "Windows Installer could not report product registration (code {code})"
        )
        .into());
    }
    let mut value = vec![0u16; count as usize + 1];
    count += 1;
    let code = unsafe {
        MsiGetProductInfoW(
            product.as_ptr(),
            property.as_ptr(),
            value.as_mut_ptr(),
            &mut count,
        )
    };
    if code != 0 {
        return Err(format!("Windows Installer registration query failed (code {code})").into());
    }
    Ok(String::from_utf16(&value[..count as usize])?)
}

fn owned_installed_product_version(root: &Path) -> Result<Option<String>> {
    let upgrade = wide(std::ffi::OsStr::new("{26B5FE7A-FDC6-4083-BEBD-947B3872311E}"));
    let mut product = [0u16; 39];
    let code = unsafe { MsiEnumRelatedProductsW(upgrade.as_ptr(), 0, 0, product.as_mut_ptr()) };
    if code == 259 {
        return Ok(None);
    }
    if code != 0 {
        return Err(format!("Cannot inventory native Compi MSI registration (code {code})").into());
    }
    if unsafe { MsiQueryProductStateW(product.as_ptr()) } != 5 {
        return Err("Compi MSI registration is not fully installed for this Windows account. Let Windows Installer finish recovery before retrying.".into());
    }
    let end = product.iter().position(|value| *value == 0).ok_or("Invalid MSI product code")?;
    let product_code = String::from_utf16(&product[..end])?;
    if installed_product_property(&product_code, "AssignmentType")? != "0" {
        return Err("Compi MSI registration belongs to a machine-wide installation, not this user's managed installation. It was not changed.".into());
    }
    let mut other = [0u16; 39];
    let code = unsafe { MsiEnumRelatedProductsW(upgrade.as_ptr(), 0, 1, other.as_mut_ptr()) };
    if code != 259 {
        return Err("Compi MSI registration is ambiguous or still recovering an upgrade. No recovery state was changed.".into());
    }
    // This MSI publishes its location/maintenance ownership in its per-user ARP entry,
    // rather than setting ARPINSTALLLOCATION on Windows Installer's hidden entry.
    let escaped = root.to_string_lossy().replace('\'', "''");
    let output = powershell(&format!(
        "$ErrorActionPreference='Stop'; $r=Get-ItemProperty 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Compi'; $root=[IO.Path]::GetFullPath('{escaped}').TrimEnd('\\'); if (-not $r.InstallLocation -or [IO.Path]::GetFullPath($r.InstallLocation).TrimEnd('\\') -ne $root) {{ throw 'Native Compi registration belongs to another installation' }}; $expected='\"'+$root+'\\Compi-Setup.exe\" --repair \"{product_code}\"'; if ($r.ModifyPath -ne $expected) {{ throw 'Native Compi maintenance ownership does not match its MSI product code' }}"
    ))?;
    if !output.status.success() {
        return Err(format!("Cannot verify native Compi installation ownership: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(Some(installed_product_version(&product_code)?))
}
fn run_msi(
    package: &Path,
    operation: InstallerOperation,
    is_package: bool,
    cancel: &Arc<AtomicBool>,
    sender: &async_channel::Sender<Event>,
) -> Result<u32> {
    fs::create_dir_all(data_root()?)?;
    let log = wide(log_path()?.as_os_str());
    let package = wide(package.as_os_str());
    let operation_properties = match operation {
        InstallerOperation::Install => "",
        InstallerOperation::Repair => "REINSTALL=ALL REINSTALLMODE=amus",
        InstallerOperation::Remove => "REMOVE=ALL",
    };
    let properties = format!(
        "REBOOT=ReallySuppress {operation_properties} COMPI_WRAPPER_ROOT=\"{}\"",
        root()?.display()
    );
    let properties = wide(std::ffi::OsStr::new(&properties));
    let mut callback = Callback {
        cancel: cancel.clone(),
        sender: sender.clone(),
        total: 0,
        completed: 0,
        step: 0,
        backward: false,
        cancellable: true,
    };
    unsafe {
        MsiSetInternalUI(2, std::ptr::null_mut()); // INSTALLUILEVEL_NONE
        let log_code = MsiEnableLogW(0x3fff, log.as_ptr(), 2);
        if log_code != 0 {
            return Err(format!("Cannot open MSI log (code {log_code})").into());
        }
        let registered = MsiSetExternalUIRecord(
            Some(msi_callback),
            0x7fffffff,
            &mut callback as *mut _ as _,
            std::ptr::null_mut(),
        );
        if registered != 0 {
            return Err(format!(
                "Cannot register Windows Installer progress/cancellation (code {registered})"
            )
            .into());
        }
        let code = if is_package {
            MsiInstallProductW(package.as_ptr(), properties.as_ptr())
        } else {
            MsiConfigureProductExW(
                package.as_ptr(),
                0,
                if operation == InstallerOperation::Remove {
                    2
                } else {
                    5
                },
                properties.as_ptr(),
            )
        };
        MsiSetExternalUIRecord(None, 0, std::ptr::null(), std::ptr::null_mut());
        Ok(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RecoveryTemp(PathBuf);
    impl RecoveryTemp {
        fn new() -> Self {
            let root = env::temp_dir().join(format!(
                "compi-initial-recovery-{}-{}",
                std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
            ));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for RecoveryTemp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn initial_journal(stage: &str) -> Journal {
        Journal {
            stage: stage.into(),
            package: None,
            previous_package: None,
            task_xml: None,
            legacy_task_xml: None,
            previous_selection: None,
            initial_install: Some(true),
        }
    }

    #[test]
    fn interrupted_first_install_preserves_committed_selection_and_allows_retry() {
        let root = RecoveryTemp::new();
        let selection = compi_update::Selection {
            schema: 1,
            version: "0.1.3".into(),
            task_version: "0.1.3".into(),
        };
        let payload = root.0.join("versions/0.1.3");
        fs::create_dir_all(&payload).unwrap();
        for name in ["compi.exe", "compi-daemon.exe", "compi-update-worker.exe"] {
            fs::write(payload.join(name), b"retained installed payload").unwrap();
        }
        compi_update::restore_selection(&root.0, Some(&selection)).unwrap();
        for stage in ["applying", "rolling-back", "rollback-incomplete"] {
            let path = root.0.join("journal.json");
            let mut journal = initial_journal(stage);
            // Pre-install task state is not authority to replace the committed task.
            journal.task_xml = Some("stale pre-install task".into());
            persist(&path, &journal).unwrap();
            recover_initial_install(&root.0, &mut journal, &path, Some("0.1.3")).unwrap();
            let recovered: Journal = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(recovered.stage, "reconciled-for-retry");
            assert_eq!(read_selection(&root.0).unwrap(), Some(selection.clone()));
            assert_eq!(
                fs::read(payload.join("compi-daemon.exe")).unwrap(),
                b"retained installed payload"
            );
        }
    }

    #[test]
    fn rolled_back_first_install_can_retry_but_unknown_legacy_cannot() {
        let root = RecoveryTemp::new();
        let path = root.0.join("journal.json");
        let mut journal = initial_journal("rollback-incomplete");
        persist(&path, &journal).unwrap();
        recover_initial_install(&root.0, &mut journal, &path, None).unwrap();
        let recovered: Journal = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(recovered.stage, "reconciled-for-retry");
        assert_eq!(read_selection(&root.0).unwrap(), None);
        journal.stage = "applying".into();
        journal.initial_install = None;
        persist(&path, &journal).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(recover_initial_install(&root.0, &mut journal, &path, None).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn native_registration_mismatch_retains_selection_and_pending_journal() {
        let root = RecoveryTemp::new();
        let selection = compi_update::Selection {
            schema: 1,
            version: "0.1.3".into(),
            task_version: "0.1.3".into(),
        };
        let payload = root.0.join("versions/0.1.3");
        fs::create_dir_all(&payload).unwrap();
        for name in ["compi.exe", "compi-daemon.exe", "compi-update-worker.exe"] {
            fs::write(payload.join(name), b"retained installed payload").unwrap();
        }
        compi_update::restore_selection(&root.0, Some(&selection)).unwrap();
        let path = root.0.join("journal.json");
        let mut journal = initial_journal("applying");
        persist(&path, &journal).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(recover_initial_install(&root.0, &mut journal, &path, Some("0.1.2")).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(read_selection(&root.0).unwrap(), Some(selection));
    }

    struct Record(u32);
    impl Record {
        fn new(fields: &[i32]) -> Self {
            #[link(name = "msi")]
            unsafe extern "system" {
                fn MsiCreateRecord(fields: u32) -> u32;
                fn MsiRecordSetInteger(record: u32, field: u32, value: i32) -> u32;
            }
            let record = unsafe { MsiCreateRecord(fields.len() as u32) };
            assert_ne!(record, 0);
            for (index, value) in fields.iter().enumerate() {
                assert_eq!(
                    unsafe { MsiRecordSetInteger(record, index as u32 + 1, *value) },
                    0
                );
            }
            Self(record)
        }
    }
    impl Drop for Record {
        fn drop(&mut self) {
            #[link(name = "msi")]
            unsafe extern "system" {
                fn MsiCloseHandle(handle: u32) -> u32;
            }
            unsafe {
                MsiCloseHandle(self.0);
            }
        }
    }
    fn dispatch(callback: &mut Callback, message: u32, values: &[i32]) -> i32 {
        let record = Record::new(values);
        unsafe { msi_callback(callback as *mut _ as _, message, record.0) }
    }
    fn callback() -> (Callback, async_channel::Receiver<Event>) {
        let (sender, receiver) = async_channel::unbounded();
        (
            Callback {
                cancel: Arc::new(AtomicBool::new(false)),
                sender,
                total: 0,
                completed: 0,
                step: 0,
                backward: false,
                cancellable: true,
            },
            receiver,
        )
    }

    #[test]
    fn msi_progress_reports_forward_and_rollback_percentages() {
        let (mut callback, receiver) = callback();
        dispatch(&mut callback, 0x0a000000, &[0, 100, 0, 0]);
        dispatch(&mut callback, 0x0a000000, &[2, 80]);
        let events: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        assert!(matches!(
            events.last(),
            Some(Event::Progress {
                percent: Some(80),
                cancellable: true,
                ..
            })
        ));
        dispatch(&mut callback, 0x0a000000, &[0, 100, 1, 0]);
        dispatch(&mut callback, 0x0a000000, &[2, 60]);
        let events: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        assert!(matches!(
            events.last(),
            Some(Event::Progress {
                percent: Some(40),
                cancellable: false,
                ..
            })
        ));
    }

    #[test]
    fn cancellation_only_interrupts_supported_stages_and_never_rollback() {
        let (mut callback, _) = callback();
        callback.cancel.store(true, Ordering::Release);
        assert_eq!(dispatch(&mut callback, 0x0b000000, &[2, 0]), 0);
        assert_eq!(dispatch(&mut callback, 0x0b000000, &[2, 1]), 2);
        assert_eq!(dispatch(&mut callback, 0x0a000000, &[0, 100, 1, 0]), 0);
    }

    #[test]
    fn explicit_cleanup_refuses_junction_targets_and_keeps_external_files() {
        struct Temp(PathBuf);
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let temp = Temp(env::temp_dir().join(format!(
                "compi-cleanup-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )));
        let managed = temp.0.join("managed");
        let external = temp.0.join("project");
        fs::create_dir_all(&managed).unwrap();
        fs::create_dir_all(&external).unwrap();
        fs::write(managed.join("settings.toml"), b"user settings").unwrap();
        fs::write(external.join("source.rs"), b"project source").unwrap();
        let junction = managed.join("linked-project");
        assert!(
            Command::new("cmd.exe")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(&junction)
                .arg(&external)
                .creation_flags(0x08000000)
                .status()
                .unwrap()
                .success()
        );
        assert!(clean_managed_data(&managed).is_err());
        assert_eq!(
            fs::read(managed.join("settings.toml")).unwrap(),
            b"user settings"
        );
        assert_eq!(
            fs::read(external.join("source.rs")).unwrap(),
            b"project source"
        );
        fs::remove_dir(&junction).unwrap();
        clean_managed_data(&managed).unwrap();
        assert!(!managed.exists());
        assert_eq!(
            fs::read(external.join("source.rs")).unwrap(),
            b"project source"
        );
    }
}

#[cfg(test)]
mod version_guard_tests {
    use super::*;

    #[test]
    fn repair_cannot_downgrade_an_updated_payload_selection() {
        let root = env::temp_dir().join(format!(
            "compi-msi-version-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        struct Temp(PathBuf);
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let root = Temp(root);
        fs::write(
            root.0.join("selection.json"),
            br#"{"schema":1,"version":"0.2.0","task_version":"0.1.3"}"#,
        )
        .unwrap();
        assert!(guard_version(&root.0, "0.1.3").is_err());
        assert!(guard_version(&root.0, "0.2.0").is_ok());
        assert!(guard_version(&root.0, "0.3.0").is_ok());
        let selected = read_selection(&root.0).unwrap().unwrap();
        assert_eq!(selected.version, "0.2.0");
        assert_eq!(selected.task_version, "0.1.3");
    }
}

#[cfg(test)]
mod profile_cleanup_tests {
    use super::*;
    #[test]
    fn opt_in_removes_only_named_managed_entries_not_projects_or_recovery_logs() {
        struct Temp(PathBuf);
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let root = Temp(env::temp_dir().join(format!(
                "compi-profile-cleanup-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )));
        fs::create_dir_all(root.0.join("themes")).unwrap();
        fs::create_dir_all(root.0.join("project")).unwrap();
        fs::create_dir_all(root.0.join("installer")).unwrap();
        fs::write(root.0.join("config.toml"), b"settings").unwrap();
        fs::write(root.0.join("workspace-build-v1.json"), b"workspace").unwrap();
        fs::write(
            root.0.join("sessions-build-v2.json.migration-backup"),
            b"legacy state",
        )
        .unwrap();
        fs::write(root.0.join("themes/custom.json"), b"custom theme").unwrap();
        fs::write(root.0.join("project/source.rs"), b"project").unwrap();
        fs::write(root.0.join("installer/journal.json"), b"recovery").unwrap();
        fs::write(root.0.join("notes.txt"), b"unrecognized user file").unwrap();
        clean_profile_data(&root.0).unwrap();
        assert!(!root.0.join("config.toml").exists());
        assert!(!root.0.join("workspace-build-v1.json").exists());
        assert!(
            !root
                .0
                .join("sessions-build-v2.json.migration-backup")
                .exists()
        );
        assert!(!root.0.join("themes").exists());
        assert_eq!(
            fs::read(root.0.join("project/source.rs")).unwrap(),
            b"project"
        );
        assert_eq!(
            fs::read(root.0.join("installer/journal.json")).unwrap(),
            b"recovery"
        );
        assert_eq!(
            fs::read(root.0.join("notes.txt")).unwrap(),
            b"unrecognized user file"
        );
    }
}
