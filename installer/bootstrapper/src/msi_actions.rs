use crate::installer::InstallerOperation;
use crate::{Result, doctor, machine, transaction};
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Serialize, Deserialize)]
struct Snapshot {
    selection: Option<compi_update::Selection>,
    task: Option<String>,
    legacy_task: Option<String>,
}
fn snapshot_path(root: &Path) -> PathBuf {
    root.join(".compi-update/msi-snapshot.json")
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let next = path.with_extension("next");
    fs::write(&next, serde_json::to_vec(value)?)?;
    fs::OpenOptions::new().write(true).open(&next)?.sync_all()?;
    transaction::atomic_replace(&next, path)
}

#[derive(Serialize, Deserialize)]
struct RawLock {
    pid: u32,
    release: PathBuf,
}
fn raw_lock_path(root: &Path) -> PathBuf {
    root.join(".compi-update/msi-lock-owner.json")
}
fn owned_release<'a>(root: &Path, owner: &'a RawLock) -> Result<&'a Path> {
    use std::os::windows::fs::MetadataExt;
    use std::path::Component;
    let state = root.join(".compi-update");
    let relative = owner
        .release
        .strip_prefix(&state)
        .map_err(|_| "MSI lock receipt escapes owned staging")?;
    let mut parts = relative.components();
    let directory = match parts.next() {
        Some(Component::Normal(value)) => value.to_str().ok_or("Invalid staging directory")?,
        _ => return Err("Invalid MSI lock staging path".into()),
    };
    let suffix = directory
        .strip_prefix("msi-lock-")
        .ok_or("Unowned MSI staging directory")?;
    if suffix.is_empty()
        || !suffix.chars().all(|c| c.is_ascii_digit() || c == '-')
        || !matches!(parts.next(), Some(Component::Normal(value)) if value == "release")
        || parts.next().is_some()
    {
        return Err("Invalid MSI lock release path".into());
    }
    for path in [&state, &state.join(directory)] {
        if fs::symlink_metadata(path)?.file_attributes() & 0x400 != 0 {
            return Err("MSI lock staging is a reparse point; its target was not touched".into());
        }
    }
    Ok(&owner.release)
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
    fn GetExitCodeProcess(handle: isize, code: *mut u32) -> i32;
    fn CloseHandle(handle: isize) -> i32;
}
struct ProcessHandle(isize);
impl ProcessHandle {
    fn open(pid: u32) -> Result<Self> {
        let handle = unsafe { OpenProcess(0x1000, 0, pid) }; // QUERY_LIMITED_INFORMATION, no kill/terminate right
        if handle == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self(handle))
    }
    fn running(&self) -> Result<bool> {
        let mut code = 0u32;
        if unsafe { GetExitCodeProcess(self.0, &mut code) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(code == 259) // STILL_ACTIVE
    }
}
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn raw_lock_alive(root: &Path) -> Result<bool> {
    let path = raw_lock_path(root);
    if !path.is_file() {
        return Ok(false);
    }
    let owner: RawLock = serde_json::from_slice(&fs::read(path)?)?;
    // A stale receipt is not authority to bypass the shared operation lock.
    let process = match ProcessHandle::open(owner.pid) {
        Ok(process) => process,
        Err(error) => {
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.raw_os_error() == Some(87))
            {
                return Ok(false);
            }
            return Err(error);
        }
    };
    process.running()
}
fn begin_raw_lock(root: &Path) -> Result<()> {
    let process_id = std::process::id();
    let output = transaction::powershell(&format!(
        "$ErrorActionPreference='Stop'; $self=Get-CimInstance Win32_Process -Filter 'ProcessId={process_id}'; $p=Get-CimInstance Win32_Process -Filter ('ProcessId='+$self.ParentProcessId); if ($p.Name -ne 'msiexec.exe') {{ throw 'The lock owner must be Windows Installer' }}; $p.ProcessId"
    ))?;
    if !output.status.success() {
        transaction::log(&format!(
            "Lock owner check failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
        return Err("Windows Installer didn't start this step. Run Compi Setup instead.".into());
    }
    let parent: u32 = String::from_utf8(output.stdout)?.trim().parse()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let directory = root
        .join(".compi-update")
        .join(format!("msi-lock-{process_id}-{stamp}"));
    fs::create_dir(&directory)?;
    let executable = directory.join("Compi-Setup.exe");
    fs::copy(std::env::current_exe()?, &executable)?;
    let release = directory.join("release");
    fs::write(&release, b"owned MSI operation")?;
    let ready = directory.join("ready.json");
    let mut child = Command::new(executable)
        .arg("--msi-lock-worker")
        .arg(root)
        .arg(parent.to_string())
        .arg(&release)
        .arg(&ready)
        .creation_flags(0x08000000)
        .spawn()?;
    let started = std::time::Instant::now();
    loop {
        if ready.is_file() {
            let owner: RawLock = serde_json::from_slice(&fs::read(&ready)?)?;
            if let Err(error) = write_json(&raw_lock_path(root), &owner) {
                fs::remove_file(&release)?;
                return Err(error);
            }
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            transaction::log(&format!("Lock keeper exited early: {status}"));
            return Err(
                "Another Compi update or repair is running. Wait for it to finish, then try again."
                    .into(),
            );
        }
        if started.elapsed() > std::time::Duration::from_secs(10) {
            fs::remove_file(&release)?;
            return Err(
                "Another Compi update or repair is running. Wait for it to finish, then try again."
                    .into(),
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}
fn lock_worker(root: &Path, args: &[String]) -> Result<()> {
    let parent = ProcessHandle::open(
        args.get(2)
            .ok_or("Missing Windows Installer PID")?
            .parse()?,
    )?;
    let release = PathBuf::from(args.get(3).ok_or("Missing lock release path")?);
    let ready = PathBuf::from(args.get(4).ok_or("Missing lock receipt path")?);
    let _lock = compi_update::OperationLock::acquire(root)?;
    let owner = RawLock {
        pid: std::process::id(),
        release: release.clone(),
    };
    write_json(&ready, &owner)?;
    while release.exists() && parent.running()? {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Ok(())
}
fn release_raw_lock(root: &Path) -> Result<()> {
    let path = raw_lock_path(root);
    if !path.is_file() {
        return Ok(());
    }
    let owner: RawLock = serde_json::from_slice(&fs::read(&path)?)?;
    let release = owned_release(root, &owner)?;
    if release.exists() {
        fs::remove_file(release)?;
    }
    let started = std::time::Instant::now();
    while raw_lock_alive(root)? {
        if started.elapsed() > std::time::Duration::from_secs(10) {
            return Err(
                "Windows Installer is still finishing. Wait a moment before running Setup again."
                    .into(),
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    fs::remove_file(path)?;
    Ok(())
}

/// `msi-lock-<pid>-<nanos>` folders under `.compi-update`.
fn lock_folders(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root.join(".compi-update")) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_prefix("msi-lock-"))
                    .is_some_and(|suffix| {
                        !suffix.is_empty()
                            && suffix.bytes().all(|c| c.is_ascii_digit() || c == b'-')
                    })
        })
        .collect()
}
/// Lock receipts and keeper folders left by a Windows Installer run that has ended.
pub(crate) fn stale_lock_receipts(root: &Path) -> bool {
    !raw_lock_alive(root).unwrap_or(true)
        && (raw_lock_path(root).is_file() || !lock_folders(root).is_empty())
}
/// Caller owns the installation operation lock, so no keeper can be starting.
pub(crate) fn clear_stale_lock_receipts(root: &Path) -> Result<()> {
    if raw_lock_alive(root)? {
        return Ok(());
    }
    let receipt = raw_lock_path(root);
    if receipt.is_file() {
        fs::remove_file(receipt)?;
    }
    for folder in lock_folders(root) {
        transaction::clean_managed_data(&folder)?;
    }
    Ok(())
}

fn clean_legacy_product(root: &Path) -> Result<()> {
    let daemon = root.join("compi-daemon.exe");
    if !daemon.is_file() {
        return Ok(());
    }
    let statuses = compi_protocol::DaemonClient::local_lifecycle_statuses_for_install(root)?;
    if statuses
        .iter()
        .any(|status| Path::new(&status.daemon_executable) == daemon)
    {
        return Err(
            "An older Compi is still running. Quit it from its window, then run Setup again."
                .into(),
        );
    }
    transaction::remove_owned_legacy_task(root)?;
    for name in [
        "compi-daemon.exe",
        "conpty.dll",
        "OpenConsole.exe",
        "ConPTY-LICENSE.txt",
        "daemon-task.xml",
    ] {
        let path = root.join(name);
        if path.is_file() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

pub(crate) fn action(args: &[String]) -> Result<()> {
    let mode = args.first().ok_or("Missing MSI maintenance action")?;
    let root: PathBuf = PathBuf::from(args.get(1).ok_or("Missing install root")?)
        .components()
        .collect();
    let wrapper_locked = args.last().is_some_and(|value| {
        match (fs::canonicalize(root.as_path()), fs::canonicalize(value)) {
            (Ok(root), Ok(wrapper)) => root
                .as_os_str()
                .as_encoded_bytes()
                .eq_ignore_ascii_case(wrapper.as_os_str().as_encoded_bytes()),
            _ => false,
        }
    });
    if mode == "--msi-lock-worker" {
        return lock_worker(&root, args);
    }
    if mode == "--msi-complete" {
        let cleanup = clean_legacy_product(&root);
        let release = if wrapper_locked {
            Ok(())
        } else {
            release_raw_lock(&root)
        };
        return match (cleanup, release) {
            (Err(error), Err(release)) => Err(format!(
                "Legacy payload cleanup failed: {error}; lock release failed: {release}"
            )
            .into()),
            (Err(error), _) | (_, Err(error)) => Err(error),
            _ => Ok(()),
        };
    }
    let _lock = if !mode.starts_with("--msi-guard-")
        && mode != "--msi-snapshot"
        && !wrapper_locked
        && !raw_lock_alive(&root)?
    {
        Some(compi_update::OperationLock::acquire(&root)?)
    } else {
        None
    };
    match mode.as_str() {
        "--msi-guard-install" => {
            transaction::guard_version(&root, args.get(2).ok_or("Missing candidate version")?)?;
            let statuses = transaction::guard(&root, InstallerOperation::Install)?;
            // Setup stops an incompatible daemon with consent before Windows Installer
            // runs; a bare MSI can only stop idle ones, so open shells block it here.
            let daemons: Vec<_> = machine::describe(&root, statuses)
                .into_iter()
                .map(|daemon| daemon.fact)
                .collect();
            if doctor::upgrade_plan(compi_protocol::PROTOCOL_VERSION, None, false, &daemons)
                .needs_consent()
            {
                return Err("Compi has open shells on an older version. Run Compi Setup to update, or close those shells first.".into());
            }
            Ok(())
        }
        "--msi-guard-remove" => {
            transaction::refuse_running(&transaction::guard(&root, InstallerOperation::Remove)?)
        }
        "--msi-snapshot" => {
            fs::create_dir_all(root.join(".compi-update"))?;
            let old_snapshot = snapshot_path(&root);
            if old_snapshot.is_file() {
                fs::remove_file(old_snapshot)?;
            }
            if !wrapper_locked {
                begin_raw_lock(&root)?;
            }
            transaction::guard_task_ownership(&root)?;
            let task = transaction::task_snapshot()?;
            let snapshot = Snapshot {
                selection: compi_update::read_selection(&root)?,
                task,
                legacy_task: transaction::legacy_task_snapshot(&root)?,
            };
            write_json(&snapshot_path(&root), &snapshot)?;
            if snapshot.selection.is_none() && snapshot.legacy_task.is_some() {
                machine::run_checked(
                    Command::new("schtasks.exe").args([
                        "/Change",
                        "/TN",
                        "Compi Daemon",
                        "/DISABLE",
                    ]),
                    "pause the old background task",
                )?;
                transaction::guard(&root, InstallerOperation::Install)?;
            }
            Ok(())
        }
        "--msi-activate" => {
            let version = args.get(2).ok_or("Missing version")?;
            let sid = args.get(3).ok_or("Missing user SID")?;
            if *sid != compi_protocol::identity::current_user_sid_string()? {
                return Err("Windows Installer is running as a different account. Run Compi Setup from your own account.".into());
            }
            let snapshot: Snapshot = serde_json::from_slice(&fs::read(snapshot_path(&root))?)?;
            transaction::guard_task_ownership(&root)?;
            let daemons = machine::describe(
                &root,
                compi_protocol::DaemonClient::local_lifecycle_statuses_for_install(&root)?,
            );
            let facts: Vec<_> = daemons.iter().map(|daemon| daemon.fact.clone()).collect();
            // A compatible running daemon keeps its registered generation: replacing the
            // definition of a running task can disturb the scheduler's state. Otherwise the
            // task moves to this version, so the next start serves this client.
            let plan = doctor::upgrade_plan(
                compi_protocol::PROTOCOL_VERSION,
                snapshot
                    .selection
                    .as_ref()
                    .map(|selection| selection.task_version.as_str()),
                snapshot.task.is_some(),
                &facts,
            );
            if plan.needs_consent() {
                return Err("Compi has open shells on an older version. Run Compi Setup to update, or close those shells first.".into());
            }
            machine::stop_instances(&plan.stop, &daemons)?;
            let task_version = match &plan.keep_task {
                Some(kept) => kept.clone(),
                None => {
                    machine::register_task(&root, version)?;
                    version.clone()
                }
            };
            compi_update::activate_msi_payload_locked(&root, version, &task_version)?;
            Ok(())
        }
        "--msi-rollback" => {
            let result: Result<()> = (|| {
                let path = snapshot_path(&root);
                if !path.is_file() {
                    return Ok(());
                }
                let snapshot: Snapshot = serde_json::from_slice(&fs::read(path)?)?;
                transaction::guard_task_ownership(&root)?;
                transaction::restore_task(snapshot.task.as_deref(), &root.join(".compi-update"))?;
                compi_update::restore_selection(&root, snapshot.selection.as_ref())?;
                if let Some(xml) = snapshot.legacy_task.as_deref() {
                    transaction::restore_legacy_task(xml, &root.join(".compi-update"), &root)?;
                }
                Ok(())
            })();
            let release = if wrapper_locked {
                Ok(())
            } else {
                release_raw_lock(&root)
            };
            match (result, release) {
                (Err(error), Err(release)) => Err(format!(
                    "Registration rollback failed: {error}; lock release failed: {release}"
                )
                .into()),
                (Err(error), _) => Err(error),
                (_, Err(error)) => Err(error),
                _ => Ok(()),
            }
        }
        "--msi-remove-task" => {
            transaction::refuse_running(&transaction::guard(&root, InstallerOperation::Remove)?)?;
            transaction::guard_task_ownership(&root)?;
            let result = transaction::task_snapshot()?;
            if result.is_some() {
                machine::run_checked(
                    Command::new("schtasks.exe").args([
                        "/Delete",
                        "/TN",
                        &transaction::task_name()?,
                        "/F",
                    ]),
                    "remove the background service",
                )?;
            }
            Ok(())
        }
        "--msi-clean-product" => {
            transaction::refuse_running(&transaction::guard(&root, InstallerOperation::Remove)?)?;
            let versions = root.join("versions");
            if versions.is_dir() {
                transaction::clean_managed_data(&versions)?;
            }
            let selection = root.join("selection.json");
            if selection.is_file() {
                fs::remove_file(selection)?;
            }
            Ok(())
        }
        _ => Err("Unknown MSI maintenance action".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_parent_exit_releases_the_shared_lock_without_killing_work() {
        struct Temp(PathBuf);
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        struct OwnedParent(std::process::Child);
        impl Drop for OwnedParent {
            fn drop(&mut self) {
                if self.0.try_wait().ok().flatten().is_none() {
                    let _ = self.0.kill();
                }
                let _ = self.0.wait();
            }
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = Temp(
            std::env::temp_dir().join(format!("compi-msi-lock-{}-{stamp}", std::process::id())),
        );
        fs::create_dir(&root.0).unwrap();
        let release = root.0.join("lease");
        let ready = root.0.join("ready.json");
        fs::write(&release, b"owned test lease").unwrap();
        let parent_exit = root.0.join("parent-exit");
        let escaped = parent_exit.display().to_string().replace('\'', "''");
        let script = format!(
            "while (-not (Test-Path -LiteralPath '{escaped}')) {{ Start-Sleep -Milliseconds 25 }}"
        );
        let mut parent = OwnedParent(
            Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .creation_flags(0x08000000)
                .spawn()
                .unwrap(),
        );
        let worker_root = root.0.clone();
        let worker_args = vec![
            "--msi-lock-worker".into(),
            root.0.display().to_string(),
            parent.0.id().to_string(),
            release.display().to_string(),
            ready.display().to_string(),
        ];
        let keeper = std::thread::spawn(move || lock_worker(&worker_root, &worker_args));
        let started = std::time::Instant::now();
        while !ready.is_file() {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(5),
                "keeper did not acknowledge lock"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(compi_update::OperationLock::acquire(&root.0).is_err());
        fs::write(parent_exit, b"exit owned fixture").unwrap();
        assert!(parent.0.wait().unwrap().success());
        keeper.join().unwrap().unwrap();
        let lock = compi_update::OperationLock::acquire(&root.0).unwrap();
        drop(lock);
    }
}

#[cfg(test)]
mod journal_path_tests {
    use super::*;
    #[test]
    fn corrupted_lock_receipt_cannot_delete_a_project_file() {
        struct Temp(PathBuf);
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = Temp(
            std::env::temp_dir().join(format!("compi-msi-receipt-{}-{stamp}", std::process::id())),
        );
        let root = temp.0.join("install");
        fs::create_dir_all(root.join(".compi-update")).unwrap();
        let project = temp.0.join("project.rs");
        fs::write(&project, b"owned project source").unwrap();
        write_json(
            &raw_lock_path(&root),
            &RawLock {
                pid: std::process::id(),
                release: project.clone(),
            },
        )
        .unwrap();
        assert!(release_raw_lock(&root).is_err());
        assert_eq!(fs::read(project).unwrap(), b"owned project source");
    }
}
