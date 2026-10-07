use super::installer::{InstallerOperation, InstallerSource};
use crate::doctor::{self, UpgradePlan};
use crate::machine::{self, Daemon};
use crate::{Error, Result, plain};
use compi_protocol::LifecycleStatus;
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
/// What Setup found before changing anything, shown to the user for approval.
#[derive(Clone, Debug, Default)]
pub(crate) struct Readiness {
    /// The version this installation runs now.
    pub installed: Option<String>,
    pub plan: UpgradePlan,
    /// Snapshots the user approves; stopping presents them back to each daemon.
    pub daemons: Vec<Daemon>,
}
#[derive(Clone, Debug)]
pub(crate) enum Event {
    Preflight(std::result::Result<Readiness, String>),
    Progress {
        stage: String,
        percent: Option<u32>,
        cancellable: bool,
    },
    Finished(std::result::Result<Outcome, String>),
    Inspected {
        inspection: std::result::Result<machine::Inspection, String>,
        fix_error: Option<String>,
    },
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
    let base = env::var_os("LOCALAPPDATA").ok_or(
        "Windows didn't provide your local app data folder. Sign in with your own account.",
    )?;
    Ok(PathBuf::from(base).join("Programs").join("Compi"))
}
pub(crate) fn data_root() -> Result<PathBuf> {
    Ok(PathBuf::from(
        env::var_os("LOCALAPPDATA").ok_or("Windows didn't provide your local app data folder.")?,
    )
    .join("Compi"))
}
pub(crate) fn log_path() -> Result<PathBuf> {
    Ok(data_root()?.join("installer.log"))
}
/// Windows Installer's own record of the latest run (UTF-16, rewritten each run). Kept apart
/// from installer.log: mixing its UTF-16 output with Setup's UTF-8 lines made both unreadable.
fn msi_log_path() -> Result<PathBuf> {
    Ok(data_root()?.join("installer-msi.log"))
}
/// Full detail for installer.log; the window shows only `plain::short`.
pub(crate) fn log(text: &str) {
    // Unit tests exercise failure paths; they must not write to this account's real log.
    if cfg!(test) {
        return;
    }
    if let Ok(path) = log_path() {
        // Setups up to 0.1.6 appended Windows Installer's UTF-16 output here; start that
        // garbled file over so the log opens as plain text.
        let mut head = [0u8; 2];
        if fs::File::open(&path)
            .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut head))
            .is_ok()
            && head == *b"\xff\xfe"
        {
            let _ = fs::remove_file(&path);
        }
        plain::log(&path, text);
    }
}
/// Runs a script whose failures reach stderr as the bare exception message, never a
/// formatted PowerShell error record ("At line:1 char:408 … CategoryInfo …").
pub(crate) fn powershell(script: &str) -> Result<std::process::Output> {
    let exe = PathBuf::from(env::var_os("SystemRoot").ok_or("SystemRoot is unavailable")?)
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let script = format!(
        "[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); try {{ {script} }} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}"
    );
    Ok(Command::new(exe)
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x08000000)
        .output()?)
}
/// The script's own message on failure, logged with `context`.
fn script_error(context: &str, output: &std::process::Output) -> Error {
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    log(&format!("{context}: {message}"));
    if message.is_empty() {
        context.into()
    } else {
        message.into()
    }
}
/// Defines `Test-Mine $owner`: Windows reports a task principal as a SID, DOMAIN\user or a
/// bare user name ("johns"); all of them name this account when they resolve to its SID.
pub(crate) const TEST_MINE: &str = r"$id=[Security.Principal.WindowsIdentity]::GetCurrent(); function Test-Mine($owner) { $sid=$owner; if ($owner -notlike 'S-1-*') { try { $sid=([Security.Principal.NTAccount]$owner).Translate([Security.Principal.SecurityIdentifier]).Value } catch { $sid=$null } }; ($sid -eq $id.User.Value) -or ($owner -eq $id.Name) -or ($owner -eq ($id.Name -split '\\')[-1]) };";

/// Inventory executable paths, not names alone: every named instance under this installation
/// is included, while another portable/installed Compi remains untouched. Returns this
/// installation's running daemons.
pub(crate) fn guard(root: &Path, operation: InstallerOperation) -> Result<Vec<LifecycleStatus>> {
    let legacy = !root.join("selection.json").is_file() && root.join("compi-daemon.exe").is_file();
    if legacy
        && compi_protocol::DaemonClient::endpoint_available(
            None,
            std::time::Duration::from_millis(250),
        )?
    {
        // The old MSI's removal hook addresses the default daemon even when it belongs to
        // another installation, so this migration never stops it on the user's behalf.
        return Err(
            "An older Compi is still running. Quit it from its window, then run Setup again."
                .into(),
        );
    }
    if operation == InstallerOperation::Install || root.join("Compi-Setup.exe").is_file() {
        guard_task_ownership(root)?;
    }
    let statuses = compi_protocol::DaemonClient::local_lifecycle_statuses_for_install(root)?;
    if operation == InstallerOperation::Install && !legacy {
        return Ok(statuses);
    }
    // Repair and removal replace or delete files that open windows run from. Setup stops
    // this installation's daemons itself, with consent, so only windows block it here.
    let names = if legacy {
        "@('compi.exe','compi-daemon.exe','compi-update-worker.exe')"
    } else {
        "@('compi.exe','compi-update-worker.exe')"
    };
    let escaped = root.to_string_lossy().replace('\'', "''");
    let current = std::process::id();
    let script = format!(
        "$ErrorActionPreference='Stop'; $root=[IO.Path]::GetFullPath('{escaped}').TrimEnd('\\')+'\\'; Get-CimInstance Win32_Process | Where-Object {{ $_.Name -in {names} -and $_.ProcessId -ne {current} }} | ForEach-Object {{ if (-not $_.ExecutablePath) {{ throw 'Windows hid a running Compi process from Setup.' }}; if ($_.ExecutablePath.StartsWith($root,[StringComparison]::OrdinalIgnoreCase)) {{ Write-Output ($_.Name+' PID '+$_.ProcessId+' '+$_.CommandLine) }} }}"
    );
    let result = powershell(&script)?;
    if !result.status.success() {
        return Err(script_error(
            "Running Compi processes could not be listed",
            &result,
        ));
    }
    let active = String::from_utf8_lossy(&result.stdout);
    if !active.trim().is_empty() {
        log(&format!("Running Compi processes:\n{active}"));
        return Err(if legacy {
            "An older Compi is still running. Quit it from its window, then run Setup again."
        } else {
            "Compi is still open. Close it, then try again."
        }
        .into());
    }
    Ok(statuses)
}

/// Windows Installer cannot ask for consent: running daemons block a bare MSI removal.
pub(crate) fn refuse_running(statuses: &[LifecycleStatus]) -> Result<()> {
    if statuses.is_empty() {
        return Ok(());
    }
    let shells: usize = statuses
        .iter()
        .map(|status| status.live_surfaces.len())
        .sum();
    log(&format!(
        "Removal blocked by {} running daemon(s)",
        statuses.len()
    ));
    Err(if shells == 0 {
        "Compi's background service is still running. Use Compi Setup to remove Compi.".to_owned()
    } else {
        format!("Compi still has {shells} open shells. Use Compi Setup to remove Compi.")
    }
    .into())
}

pub(crate) fn guard_version(root: &Path, candidate: &str) -> Result<()> {
    if let Some(selected) = read_selection(root)?
        && semver::Version::parse(candidate)? < semver::Version::parse(&selected.version)?
    {
        return Err(format!(
            "This Setup has Compi {candidate}, but {} is installed. Download the latest Setup.",
            selected.version
        )
        .into());
    }
    Ok(())
}
/// Checks everything Setup needs and decides what happens to running daemons. Changes nothing.
pub(crate) fn preflight(operation: InstallerOperation) -> Result<Readiness> {
    super::installer::ensure_supported_windows()?;
    let root = root()?;
    if operation != InstallerOperation::Remove {
        guard_version(&root, env!("CARGO_PKG_VERSION"))?;
    }
    let statuses = guard(&root, operation)?;
    let daemons = machine::describe(&root, statuses);
    let facts: Vec<_> = daemons.iter().map(|daemon| daemon.fact.clone()).collect();
    let selection = read_selection(&root).ok().flatten();
    let plan = match operation {
        InstallerOperation::Install => doctor::upgrade_plan(
            compi_protocol::PROTOCOL_VERSION,
            selection
                .as_ref()
                .map(|selection| selection.task_version.as_str()),
            task_snapshot()?.is_some(),
            &facts,
        ),
        InstallerOperation::Repair | InstallerOperation::Remove => doctor::reinstall_plan(&facts),
    };
    let readiness = Readiness {
        installed: selection.map(|selection| selection.version),
        plan,
        daemons,
    };
    fs::create_dir_all(&root).map_err(|error| {
        log(&format!("Cannot create {}: {error}", root.display()));
        Error::from("Setup can't create Compi's folder. Check you can write to your user folder, then try again.")
    })?;
    let probe = root.join(format!(".write-check-{}", std::process::id()));
    fs::write(&probe, b"access check")
        .and_then(|()| fs::remove_file(&probe))
        .map_err(|error| {
            log(&format!("Write check in {} failed: {error}", root.display()));
            Error::from("Setup can't write to Compi's folder. Check free space and permissions, then try again.")
        })?;
    if operation != InstallerOperation::Install {
        return Ok(readiness);
    }
    if !cfg!(target_arch = "x86_64") {
        return Err("Compi needs 64-bit Windows on an Intel or AMD processor.".into());
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
        return Err(format!(
            "Setup needs 300 MB free, but only {} is free. Free some space, then try again.",
            plain::megabytes(available)
        )
        .into());
    }
    let wsl = machine::wsl_fact();
    if wsl != doctor::WslFact::Ready {
        let finding = doctor::Finding::Wsl(wsl);
        return Err(format!(
            "{}. {} {}",
            finding.title(),
            finding.explanation(),
            finding.command().unwrap_or_default()
        )
        .into());
    }
    Ok(readiness)
}

/// Whether the default WSL2 guest starts within 30 seconds.
pub(crate) fn wsl_guest_starts() -> Result<bool> {
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
            if !status.success() {
                log(&format!("WSL guest probe exited with {status}"));
            }
            return Ok(status.success());
        }
        if started.elapsed() > std::time::Duration::from_secs(30) {
            // Only the prerequisite probe is terminated, never an MSI, daemon, or guest distribution.
            guest.kill()?;
            guest.wait()?;
            log("WSL guest probe did not finish within 30 seconds");
            return Ok(false);
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
        return Err(script_error(
            "Task Scheduler didn't answer. Restart Windows, then try again.",
            &result,
        ));
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
fn schtasks(args: &[String], what: &str) -> Result<()> {
    machine::run_checked(Command::new("schtasks.exe").args(args), what)
}
pub(crate) fn remove_owned_legacy_task(root: &Path) -> Result<()> {
    if legacy_task_snapshot(root)?.is_none() {
        return Ok(());
    }
    schtasks(
        &[
            "/Delete".into(),
            "/TN".into(),
            "Compi Daemon".into(),
            "/F".into(),
        ],
        "remove the old background task",
    )
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
    let result = schtasks(&args, "restore the background task");
    if result.is_err() && xml.is_none() && task_snapshot_named(name)?.is_none() {
        return Ok(());
    }
    result
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
        "if ($t.State -notin @('Ready','Disabled')) { throw 'An older Compi is still running. Quit it from its window, then run Setup again.' }"
    } else {
        ""
    };
    let script = format!(
        "$ErrorActionPreference='Stop'; {TEST_MINE} $t=Get-ScheduledTask -ErrorAction Stop | Where-Object {{ $_.TaskName -eq '{name}' -and $_.TaskPath -eq '\\' }}; if ($t) {{ if (-not (Test-Mine $t.Principal.UserId)) {{ throw 'Compi''s background task belongs to another Windows account. Sign in as that account to change it.' }}; $root=[IO.Path]::GetFullPath('{escaped}').TrimEnd('\\')+'\\'; foreach ($a in $t.Actions) {{ if (-not $a.Execute -or -not $a.Execute.Trim('\"').StartsWith($root,[StringComparison]::OrdinalIgnoreCase)) {{ throw 'Compi''s background task belongs to another copy of Compi. Remove that copy, then try again.' }} }}; {inactive_check} }}"
    );
    let output = powershell(&script)?;
    if !output.status.success() {
        return Err(script_error(
            "Task Scheduler didn't answer. Restart Windows, then try again.",
            &output,
        ));
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
        log("Recovery: interrupted legacy migration has no verified prior payload; journal kept");
        return Err("An earlier Setup was interrupted and can't be undone automatically. Remove Compi in Settings > Apps, then install again.".into());
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
            log(
                "Recovery: journal predates the initial-install receipt and no verified native payload exists; journal kept",
            );
            return Err("An earlier Setup was interrupted and can't be undone automatically. Remove Compi in Settings > Apps, then install again.".into());
        }
    }
    if let Some(selection) = &selected
        && (registered_version != Some(selection.version.as_str())
            || !selected_payload_is_runnable(root, selection))
    {
        log("Recovery: selection does not match a runnable registered MSI payload; journal kept");
        return Err(
            "An earlier Setup was interrupted. Restart Windows so it can finish, then try again."
                .into(),
        );
    }
    // Do not replay pre-install task XML or remove a selection: MSI may have committed
    // successfully before Setup died. The next transaction still performs real repair/install.
    journal.stage = "reconciled-for-retry".into();
    persist(journal_path, journal)
}

/// Caller owns the installation operation lock.
pub(crate) fn recover(root: &Path, installer: &Path) -> Result<()> {
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
                let registered_version =
                    owned_installed_product(root)?.map(|installed| installed.version);
                guard_task_ownership(root)?;
                if journal.previous_selection.is_none() && !root.join("compi-daemon.exe").is_file()
                {
                    recover_initial_install(
                        root,
                        &mut journal,
                        &path,
                        registered_version.as_deref(),
                    )?;
                    continue;
                } else {
                    if let Some(previous) = &journal.previous_selection
                        && !selected_payload_is_runnable(root, previous)
                    {
                        log(
                            "Recovery: prior payload of the interrupted upgrade is missing; journal kept",
                        );
                        return Err("An earlier update was interrupted and the previous version is missing. Remove Compi in Settings > Apps, then install again.".into());
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

/// Applies the operation. `approved` holds the daemon snapshots the user saw: any daemon
/// the plan stops must be among them, and stops only if its shells are still as shown.
pub(crate) fn perform(
    source: InstallerSource,
    operation: InstallerOperation,
    remove_data: bool,
    approved: &[Daemon],
    cancel: Arc<AtomicBool>,
    sender: async_channel::Sender<Event>,
) -> Result<Outcome> {
    log(&format!(
        "Compi Setup {}: {operation:?} started",
        env!("CARGO_PKG_VERSION")
    ));
    if let InstallerSource::Package(bytes) = &source
        && !bytes.starts_with(b"\xd0\xcf\x11\xe0")
    {
        log("Embedded Windows Installer package is not a compound file");
        return Err("This Setup file is damaged. Download it again.".into());
    }
    let readiness = preflight(operation)?;
    let root = root()?;
    let _lock = OperationLock::acquire(&root)?;
    let installer = data_root()?.join("installer");
    fs::create_dir_all(&installer)?;
    recover(&root, &installer)?;
    let previous_selection = read_selection(&root)?;
    // Removing always targets the registered product: each Setup build carries its own
    // product code, and Windows Installer refuses to remove a package it never installed.
    let source = match source {
        InstallerSource::Package(_) if operation == InstallerOperation::Remove => {
            let installed = owned_installed_product(&root)?.ok_or("Compi isn't installed.")?;
            log(&format!(
                "Removing installed Compi {} through its registered product",
                installed.version
            ));
            InstallerSource::ProductCode(installed.code)
        }
        source => source,
    };
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
                && owned_installed_product(&root)?.is_none(),
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
            message: plain::installer_code(1602).into(),
        }
        .into());
    }
    // Old daemons go before Windows Installer runs: it must not register a task generation
    // that a still-running incompatible daemon would keep serving.
    let stopped_default = readiness.plan.stop.contains(&None);
    if readiness.plan.restarts_service() {
        let _ = sender.send_blocking(Event::Progress {
            stage: "Stopping Compi's background service".into(),
            percent: None,
            cancellable: false,
        });
        log(&format!(
            "Stopping {} daemon instance(s) ending {} shell(s)",
            readiness.plan.stop.len(),
            readiness.plan.shells.len()
        ));
        if let Err(error) = machine::stop_instances(&readiness.plan.stop, approved) {
            journal.stage = "cancelled-before-apply".into();
            persist(&journal_path, &journal)?;
            restart_service(stopped_default);
            return Err(error);
        }
    }
    let _ = sender.send_blocking(Event::Progress {
        stage: match operation {
            InstallerOperation::Install => "Installing",
            InstallerOperation::Repair => "Reinstalling files",
            InstallerOperation::Remove => "Removing",
        }
        .into(),
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
            restart_service(stopped_default);
            return Err(error);
        }
    };
    if !matches!(code, 0 | 1641 | 3010) {
        journal.stage = "rolling-back".into();
        persist(&journal_path, &journal)?;
        let _ = sender.send_blocking(Event::Progress {
            stage: "Undoing changes".into(),
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
            if let Some(xml) = journal.legacy_task_xml.as_deref()
                && let Err(error) = restore_legacy_task(xml, &attempt, &root)
            {
                failures.push(error.to_string());
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
        restart_service(stopped_default);
        log(&format!(
            "Windows Installer returned {code}. Rollback problems: {}. Recovery journal: {}",
            if failures.is_empty() {
                "none".to_owned()
            } else {
                failures.join("; ")
            },
            journal_path.display()
        ));
        return Err(MsiFailure {
            code,
            message: if failures.is_empty() {
                plain::installer_code(code).into()
            } else {
                "Windows Installer couldn't finish, and Setup couldn't undo every change. Run Setup again to finish undoing it.".into()
            },
        }
        .into());
    }
    if let Some(staged) = &journal.package
        && operation != InstallerOperation::Remove
    {
        let next = installer.join(format!("Compi.{id}.next.msi"));
        fs::copy(staged, &next)?;
        atomic_replace(&next, &cache)?;
    }
    journal.stage = "complete".into();
    persist(&journal_path, &journal)?;
    if operation != InstallerOperation::Remove {
        // The stopped service comes back on the version Windows Installer just registered.
        restart_service(stopped_default);
    }
    let version = if operation == InstallerOperation::Remove {
        None
    } else {
        match product_code {
            Some(code) => installed_product_version(&code)
                .inspect_err(|error| log(&format!("Installed version unavailable: {error}")))
                .ok(),
            None => Some(env!("CARGO_PKG_VERSION").to_owned()),
        }
    };
    let mut outcome = Outcome {
        restart_required: code != 0,
        cleanup: None,
        version,
    };
    if operation == InstallerOperation::Remove {
        // Update journals, staging and worker copies under the program folder belong
        // to the removed product, not to the user's settings. Release the operation
        // lock (it lives in that folder) before deleting it.
        drop(_lock);
        if let Err(error) = clean_managed_data(&root.join(".compi-update")) {
            log(&format!(
                "Could not remove {}: {error}",
                root.join(".compi-update").display()
            ));
        }
        let _ = fs::remove_dir(&root);
    }
    if operation == InstallerOperation::Remove && remove_data {
        // The managed directory is exact. Never traverse directory junctions/symlinks.
        outcome.cleanup = Some((|| {
            let active = powershell("$ErrorActionPreference='Stop'; Get-CimInstance Win32_Process | Where-Object { $_.Name -in @('compi.exe','compi-daemon.exe') } | ForEach-Object { Write-Output ($_.Name+' PID '+$_.ProcessId) }")?;
            if !active.status.success() || !active.stdout.is_empty() {
                log(&format!("Data cleanup deferred; running: {}", String::from_utf8_lossy(&active.stdout)));
                return Err(Error::from("Another copy of Compi is running, so your settings were kept. Delete them later from the folder below."));
            }
            clean_profile_data(&data_root()?)
        })().map_err(|error: Error| plain::short(&error.to_string())));
    }
    Ok(outcome)
}

/// Starts the default daemon's task again after Setup stopped it.
fn restart_service(stopped_default: bool) {
    if stopped_default && let Err(error) = machine::run_task() {
        log(&format!("Background service did not start: {error}"));
    }
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
                "Undoing changes"
            } else if !context.cancellable {
                "Finishing"
            } else {
                "Copying files"
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
        log(&format!(
            "MsiGetProductInfo({property:?}) size query returned {code}"
        ));
        return Err(
            "Windows Installer didn't report Compi's version. Restart Windows, then try again."
                .into(),
        );
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
        log(&format!("MsiGetProductInfo returned {code}"));
        return Err(
            "Windows Installer didn't report Compi's version. Restart Windows, then try again."
                .into(),
        );
    }
    Ok(String::from_utf16(&value[..count as usize])?)
}

struct InstalledProduct {
    code: String,
    version: String,
}

fn owned_installed_product(root: &Path) -> Result<Option<InstalledProduct>> {
    let upgrade = wide(std::ffi::OsStr::new(
        "{26B5FE7A-FDC6-4083-BEBD-947B3872311E}",
    ));
    let mut product = [0u16; 39];
    let code = unsafe { MsiEnumRelatedProductsW(upgrade.as_ptr(), 0, 0, product.as_mut_ptr()) };
    if code == 259 {
        return Ok(None);
    }
    if code != 0 {
        log(&format!("MsiEnumRelatedProducts returned {code}"));
        return Err("Windows Installer didn't list Compi. Restart Windows, then try again.".into());
    }
    if unsafe { MsiQueryProductStateW(product.as_ptr()) } != 5 {
        return Err("Windows Installer is still finishing an earlier Compi change. Restart Windows, then try again.".into());
    }
    let end = product
        .iter()
        .position(|value| *value == 0)
        .ok_or("Invalid MSI product code")?;
    let product_code = String::from_utf16(&product[..end])?;
    if installed_product_property(&product_code, "AssignmentType")? != "0" {
        return Err("Compi is installed for all users on this PC. Remove that copy from Settings > Apps first.".into());
    }
    let mut other = [0u16; 39];
    let code = unsafe { MsiEnumRelatedProductsW(upgrade.as_ptr(), 0, 1, other.as_mut_ptr()) };
    if code != 259 {
        return Err("Windows lists more than one Compi. Restart Windows, then try again.".into());
    }
    // This MSI publishes its location/maintenance ownership in its per-user ARP entry,
    // rather than setting ARPINSTALLLOCATION on Windows Installer's hidden entry.
    let escaped = root.to_string_lossy().replace('\'', "''");
    let output = powershell(&format!(
        "$ErrorActionPreference='Stop'; $r=Get-ItemProperty 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Compi'; $root=[IO.Path]::GetFullPath('{escaped}').TrimEnd('\\'); if (-not $r.InstallLocation -or [IO.Path]::GetFullPath($r.InstallLocation).TrimEnd('\\') -ne $root) {{ throw 'Windows registers Compi in another folder. Remove that copy from Settings > Apps first.' }}; $expected='\"'+$root+'\\Compi-Setup.exe\" --repair \"{product_code}\"'; if ($r.ModifyPath -ne $expected) {{ throw 'Compi''s Windows registration is damaged. Remove Compi from Settings > Apps, then install again.' }}"
    ))?;
    if !output.status.success() {
        return Err(script_error(
            "Compi's Windows registration could not be read",
            &output,
        ));
    }
    let version = installed_product_version(&product_code)?;
    Ok(Some(InstalledProduct {
        code: product_code,
        version,
    }))
}
fn run_msi(
    package: &Path,
    operation: InstallerOperation,
    is_package: bool,
    cancel: &Arc<AtomicBool>,
    sender: &async_channel::Sender<Event>,
) -> Result<u32> {
    fs::create_dir_all(data_root()?)?;
    let log_file = wide(msi_log_path()?.as_os_str());
    let package = wide(package.as_os_str());
    // A newer Setup "repairs" an older install by upgrading it: REINSTALL applies only to
    // the product version already installed.
    let reinstall = operation == InstallerOperation::Repair
        && (!is_package
            || owned_installed_product(&root()?)?
                .is_some_and(|installed| installed.version == env!("CARGO_PKG_VERSION")));
    let operation_properties = match operation {
        InstallerOperation::Install => "",
        InstallerOperation::Repair if reinstall => "REINSTALL=ALL REINSTALLMODE=amus",
        InstallerOperation::Repair => "",
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
        // Everything but the verbose/debug channels (which dump every product on the PC);
        // 2 = flush each line, no append flag so each run replaces the previous record.
        let log_code = MsiEnableLogW(0x0fff, log_file.as_ptr(), 2);
        if log_code != 0 {
            return Err(
                "Windows Installer couldn't write its log. Check free space, then try again."
                    .into(),
            );
        }
        let registered = MsiSetExternalUIRecord(
            Some(msi_callback),
            0x7fffffff,
            &mut callback as *mut _ as _,
            std::ptr::null_mut(),
        );
        if registered != 0 {
            log(&format!("MsiSetExternalUIRecord returned {registered}"));
            return Err("Windows Installer didn't start. Restart Windows, then try again.".into());
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
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
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
