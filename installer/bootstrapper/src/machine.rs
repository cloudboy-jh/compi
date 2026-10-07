//! Reads the facts `doctor` decides on, and applies its fixes, on this Windows account.

use crate::doctor::{
    self, Facts, Finding, Fix, PAYLOAD_FILES, PayloadFact, ROOT_FILES, RunningDaemon,
    SelectionFact, TaskFact, TaskTarget, WslFact,
};
use crate::{Result, plain, transaction};
use compi_protocol::{DaemonClient, LifecycleConsent, LifecycleStatus};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use std::{env, fs};

const NO_WINDOW: u32 = 0x0800_0000;

/// A running daemon of this installation and the snapshot shown to the user. Stopping it
/// presents that snapshot back, so a daemon whose shells changed since is not stopped.
#[derive(Clone, Debug)]
pub(crate) struct Daemon {
    pub fact: RunningDaemon,
    pub consent: LifecycleConsent,
}

pub(crate) fn doctor_log() -> PathBuf {
    transaction::data_root()
        .unwrap_or_default()
        .join("doctor.log")
}

/// Runs `command`; a failure becomes "Couldn't {what}." with Windows' own reason.
pub(crate) fn run_checked(command: &mut Command, what: &str) -> Result<()> {
    let output = command.creation_flags(NO_WINDOW).output()?;
    if output.status.success() {
        return Ok(());
    }
    let detail = if output.stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    } else {
        String::from_utf8_lossy(&output.stderr).trim().to_owned()
    };
    Err(format!("Couldn't {what}. {detail}").into())
}

fn normalized(path: &Path) -> String {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    path.to_string_lossy()
        .trim_start_matches(r"\\?\")
        .trim_matches('"')
        .trim_end_matches('\\')
        .to_lowercase()
}

/// `Some(version)` when `executable` is `<root>\versions\<version>\compi-daemon.exe`.
fn generation(root: &Path, executable: &Path) -> Option<String> {
    let versions = normalized(&root.join("versions"));
    let executable = normalized(executable);
    let rest = executable.strip_prefix(&versions)?.strip_prefix('\\')?;
    let (version, file) = rest.split_once('\\')?;
    (file == "compi-daemon.exe" && semver::Version::parse(version).is_ok())
        .then(|| version.to_owned())
}

fn inside(root: &Path, path: &Path) -> bool {
    let root = normalized(root);
    let path = normalized(path);
    path.strip_prefix(&root)
        .is_some_and(|rest| rest.starts_with('\\'))
}

/// Describes lifecycle statuses (from `transaction::guard`) for findings and consent.
pub(crate) fn describe(root: &Path, statuses: Vec<LifecycleStatus>) -> Vec<Daemon> {
    statuses
        .into_iter()
        .map(|status| Daemon {
            fact: RunningDaemon {
                instance: status.instance.clone(),
                protocol: status.protocol_version,
                version: generation(root, Path::new(&status.daemon_executable)),
                supervised: status.supervisor_pid.is_some_and(supervisor_alive),
                shells: shell_labels(status.instance.as_deref(), &status),
            },
            consent: status.consent(),
        })
        .collect()
}

fn shell_labels(instance: Option<&str>, status: &LifecycleStatus) -> Vec<String> {
    let live: Vec<String> = status
        .live_surfaces
        .iter()
        .filter_map(|surface| {
            serde_json::to_value(&surface.surface_id)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
        })
        .collect();
    let suffix = instance.map(|name| format!("-{name}")).unwrap_or_default();
    let workspace = compi_protocol::paths::data_dir()
        .ok()
        .and_then(|directory| fs::read(directory.join(format!("workspace{suffix}-v1.json"))).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(serde_json::Value::Null);
    doctor::shell_labels(&workspace, &live)
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
    fn GetExitCodeProcess(handle: isize, code: *mut u32) -> i32;
    fn QueryFullProcessImageNameW(handle: isize, flags: u32, name: *mut u16, size: *mut u32)
    -> i32;
    fn CloseHandle(handle: isize) -> i32;
}

/// A supervisor PID counts only while a `compi-daemon.exe` still runs under it.
fn supervisor_alive(pid: u32) -> bool {
    let handle = unsafe { OpenProcess(0x1000, 0, pid) }; // QUERY_LIMITED_INFORMATION
    if handle == 0 {
        return false;
    }
    let mut code = 0u32;
    let running = unsafe { GetExitCodeProcess(handle, &mut code) } != 0 && code == 259;
    let mut name = [0u16; 1024];
    let mut size = name.len() as u32;
    let image = (unsafe { QueryFullProcessImageNameW(handle, 0, name.as_mut_ptr(), &mut size) }
        != 0)
        .then(|| String::from_utf16_lossy(&name[..size as usize]));
    unsafe { CloseHandle(handle) };
    running && image.is_some_and(|image| image.to_lowercase().ends_with("\\compi-daemon.exe"))
}

/// Stops the planned instances with the snapshots the user approved.
pub(crate) fn stop_instances(stop: &[Option<String>], approved: &[Daemon]) -> Result<()> {
    for instance in stop {
        let daemon = approved
            .iter()
            .find(|daemon| &daemon.fact.instance == instance)
            .ok_or("Compi started more work while Setup was open. Check again.")?;
        DaemonClient::conditional_stop(
            instance.as_deref(),
            &daemon.consent,
            Duration::from_secs(30),
        )?;
    }
    Ok(())
}

/// Registers the per-user task to run `version`'s daemon.
pub(crate) fn register_task(root: &Path, version: &str) -> Result<()> {
    let daemon = root.join("versions").join(version).join("compi-daemon.exe");
    if !daemon.is_file() {
        return Err(format!("Version {version} isn't installed. Reinstall Compi.").into());
    }
    let xml = root.join(".compi-update").join("daemon-task.xml");
    fs::create_dir_all(root.join(".compi-update"))?;
    run_checked(
        Command::new(&daemon)
            .arg("--write-task-xml")
            .arg(&xml)
            .arg(compi_protocol::identity::current_user_sid_string()?),
        "prepare the background service",
    )?;
    run_checked(
        Command::new(system32("schtasks.exe")?)
            .args(["/Create", "/TN", &transaction::task_name()?, "/XML"])
            .arg(&xml)
            .arg("/F"),
        "register the background service",
    )
}

/// Starts the registered task now rather than at the next sign-in.
pub(crate) fn run_task() -> Result<()> {
    run_checked(
        Command::new(system32("schtasks.exe")?).args(["/Run", "/TN", &transaction::task_name()?]),
        "start the background service",
    )
}

fn system32(name: &str) -> Result<PathBuf> {
    Ok(
        PathBuf::from(env::var_os("SystemRoot").ok_or("SystemRoot is unavailable")?)
            .join("System32")
            .join(name),
    )
}

/// Protocol of `version`'s daemon: known for this Setup and older releases, otherwise asked.
fn protocol_of(root: &Path, version: &str) -> Option<u32> {
    if version == env!("CARGO_PKG_VERSION") {
        return Some(compi_protocol::PROTOCOL_VERSION);
    }
    if let Some(protocol) = doctor::released_protocol(version) {
        return Some(protocol);
    }
    let daemon = root.join("versions").join(version).join("compi-daemon.exe");
    let output = Command::new(daemon)
        .arg("--protocol-version")
        .creation_flags(NO_WINDOW)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

fn payloads(root: &Path) -> Vec<PayloadFact> {
    let Ok(entries) = fs::read_dir(root.join("versions")) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| semver::Version::parse(name).is_ok_and(|parsed| parsed.to_string() == *name))
        .map(|version| {
            let folder = root.join("versions").join(&version);
            PayloadFact {
                missing: PAYLOAD_FILES
                    .iter()
                    .copied()
                    .filter(|file| !folder.join(file).is_file())
                    .collect(),
                version,
            }
        })
        .collect()
}

fn task_fact(root: &Path) -> TaskFact {
    let script = format!(
        "{} $t=Get-ScheduledTask -ErrorAction Stop | Where-Object {{ $_.TaskName -eq '{}' -and $_.TaskPath -eq '\\' }}; if (-not $t) {{ '{{\"exists\":false}}' }} else {{ [pscustomobject]@{{exists=$true; enabled=[bool]$t.Settings.Enabled; mine=[bool](Test-Mine $t.Principal.UserId); execute=@($t.Actions | ForEach-Object {{ [string]$_.Execute }})}} | ConvertTo-Json -Compress }}",
        transaction::TEST_MINE,
        match transaction::task_name() {
            Ok(name) => name.replace('\'', "''"),
            Err(_) => return TaskFact::Unreadable,
        }
    );
    #[derive(serde::Deserialize)]
    struct Row {
        exists: bool,
        #[serde(default)]
        enabled: bool,
        #[serde(default)]
        mine: bool,
        #[serde(default)]
        execute: serde_json::Value,
    }
    let row = transaction::powershell(&script)
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice::<Row>(&output.stdout).ok());
    let Some(row) = row else {
        return TaskFact::Unreadable;
    };
    if !row.exists {
        return TaskFact::Missing;
    }
    // ConvertTo-Json writes a one-element array as a bare string.
    let actions: Vec<String> = match row.execute {
        serde_json::Value::String(path) => vec![path],
        serde_json::Value::Array(paths) => paths
            .into_iter()
            .filter_map(|path| path.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    };
    let target = match actions.as_slice() {
        [path] => {
            let path = Path::new(path.trim_matches('"'));
            match generation(root, path) {
                Some(version) => TaskTarget::Version(version),
                None if inside(root, path) => TaskTarget::Unknown,
                None => TaskTarget::OtherInstallation,
            }
        }
        _ => TaskTarget::Unknown,
    };
    TaskFact::Present {
        enabled: row.enabled,
        mine: row.mine,
        target,
    }
}

pub(crate) fn wsl_fact() -> WslFact {
    if let Err(error) = compi_protocol::wsl::ensure_default_wsl2() {
        transaction::log(&format!("WSL check failed: {error}"));
        return doctor::wsl_problem(&error.to_string());
    }
    match transaction::wsl_guest_starts() {
        Ok(true) => WslFact::Ready,
        _ => WslFact::NotStarting,
    }
}

const TERMINAL_STAGES: &[&str] = &[
    "complete",
    "rolled-back",
    "cancelled-before-apply",
    "failed-before-apply",
    "reconciled-for-retry",
];
const INTERRUPTED_STAGES: &[&str] = &["staging", "applying", "rolling-back", "rollback-incomplete"];

/// Setup attempt folders (`<pid>-<nanos>`) and the stage their journal records.
fn setup_attempts() -> Vec<(PathBuf, String)> {
    let Ok(installer) = transaction::data_root().map(|root| root.join("installer")) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(installer) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.split_once('-'))
                .is_some_and(|(pid, stamp)| {
                    !pid.is_empty()
                        && !stamp.is_empty()
                        && pid.bytes().all(|b| b.is_ascii_digit())
                        && stamp.bytes().all(|b| b.is_ascii_digit())
                })
        })
        .filter_map(|path| {
            let journal: serde_json::Value =
                serde_json::from_slice(&fs::read(path.join("journal.json")).ok()?).ok()?;
            let stage = journal.get("stage")?.as_str()?.to_owned();
            Some((path, stage))
        })
        .collect()
}

fn update_interrupted(root: &Path) -> bool {
    let target = compi_update::InstallTarget {
        kind: compi_update::InstallationKind::InstalledWindows,
        root: root.to_path_buf(),
    };
    matches!(
        compi_update::operation_status(&target),
        Ok(Some(journal)) if matches!(
            journal.phase,
            compi_update::JournalStage::Activating
                | compi_update::JournalStage::Activated
                | compi_update::JournalStage::Relaunching
        )
    )
}

#[derive(Clone, Debug)]
pub(crate) struct Inspection {
    pub daemons: Vec<Daemon>,
    pub findings: Vec<Finding>,
}

/// One readable doctor.log line per check.
fn check_lines(facts: &Facts) -> Vec<String> {
    let mut lines = vec![match &facts.selection {
        SelectionFact::Present {
            version,
            task_version,
        } => format!("Current version: {version} (background service version {task_version})"),
        SelectionFact::Missing => "Current version: selection.json is missing".into(),
        SelectionFact::Invalid => "Current version: selection.json is damaged".into(),
    }];
    let versions: Vec<String> = facts
        .payloads
        .iter()
        .map(|payload| {
            if payload.missing.is_empty() {
                payload.version.clone()
            } else {
                format!(
                    "{} (missing {})",
                    payload.version,
                    payload.missing.join(", ")
                )
            }
        })
        .collect();
    lines.push(format!("Installed versions: {}", versions.join(", ")));
    lines.push(if facts.root_missing.is_empty() {
        "Program folder: complete".into()
    } else {
        format!("Program folder: missing {}", facts.root_missing.join(", "))
    });
    lines.push(match &facts.task {
        TaskFact::Missing => "Background task: not registered".into(),
        TaskFact::Unreadable => "Background task: Task Scheduler did not answer".into(),
        TaskFact::Present {
            enabled,
            mine,
            target,
        } => format!(
            "Background task: {}, {}, runs {}",
            if *enabled { "enabled" } else { "disabled" },
            if *mine {
                "this account's"
            } else {
                "another account's"
            },
            match target {
                TaskTarget::Version(version) => format!("version {version}"),
                TaskTarget::Unknown => "an old layout".into(),
                TaskTarget::OtherInstallation => "another copy of Compi".into(),
            }
        ),
    });
    match &facts.daemons {
        Err(error) => lines.push(format!("Background service: could not inspect ({error})")),
        Ok(daemons) if daemons.is_empty() => lines.push("Background service: not running".into()),
        Ok(daemons) => lines.extend(daemons.iter().map(|daemon| {
            format!(
                "Background service ({}): version {}, protocol {}, {}, {} shells",
                daemon.instance.as_deref().unwrap_or("default"),
                daemon.version.as_deref().unwrap_or("unknown"),
                daemon.protocol,
                if daemon.supervised {
                    "watched"
                } else {
                    "not watched"
                },
                daemon.shells.len()
            )
        })),
    }
    lines.push(format!(
        "Unfinished update or Setup: {}",
        if facts.unfinished { "yes" } else { "no" }
    ));
    lines.push(format!(
        "Leftover setup files: {}",
        if facts.leftovers { "yes" } else { "no" }
    ));
    lines.push(format!("WSL: {:?}", facts.wsl));
    lines
}

/// Reads everything the doctor decides on. Changes nothing.
pub(crate) fn inspect() -> Result<Inspection> {
    let root = transaction::root()?;
    let log = doctor_log();
    plain::log(&log, &format!("Checking Compi at {}", root.display()));
    let selection = match compi_update::read_selection(&root) {
        Ok(Some(selection)) => SelectionFact::Present {
            version: selection.version,
            task_version: selection.task_version,
        },
        Ok(None) => SelectionFact::Missing,
        Err(error) => {
            plain::log(&log, &format!("selection.json is unreadable: {error}"));
            SelectionFact::Invalid
        }
    };
    let payloads = payloads(&root);
    let root_missing = ROOT_FILES
        .iter()
        .copied()
        .filter(|file| !root.join(file).is_file())
        .collect();
    let task = task_fact(&root);
    let (daemons, daemon_facts) = match DaemonClient::local_lifecycle_statuses_for_install(&root) {
        Ok(statuses) => {
            let daemons = describe(&root, statuses);
            let facts = daemons.iter().map(|daemon| daemon.fact.clone()).collect();
            (daemons, Ok(facts))
        }
        Err(error) => {
            plain::log(
                &log,
                &format!("Background service inspection failed: {error}"),
            );
            (Vec::new(), Err(error.to_string()))
        }
    };
    let protocols = payloads
        .iter()
        .filter_map(|payload| {
            protocol_of(&root, &payload.version).map(|protocol| (payload.version.clone(), protocol))
        })
        .collect();
    // A running update or Setup holds the operation lock; its files are not leftovers.
    let idle = !root.is_dir() || compi_update::OperationLock::acquire(&root).is_ok();
    let attempts = setup_attempts();
    let unfinished = idle
        && (update_interrupted(&root)
            || attempts
                .iter()
                .any(|(_, stage)| INTERRUPTED_STAGES.contains(&stage.as_str())));
    let leftovers = idle
        && (attempts
            .iter()
            .any(|(_, stage)| TERMINAL_STAGES.contains(&stage.as_str()))
            || crate::msi_actions::stale_lock_receipts(&root));
    let facts = Facts {
        own_protocol: compi_protocol::PROTOCOL_VERSION,
        own_version: env!("CARGO_PKG_VERSION").into(),
        selection,
        payloads,
        root_missing,
        task,
        daemons: daemon_facts,
        protocols,
        unfinished,
        leftovers,
        wsl: wsl_fact(),
    };
    for line in check_lines(&facts) {
        plain::log(&log, &line);
    }
    let findings = doctor::findings(&facts);
    if findings.is_empty() {
        plain::log(&log, "No problems found");
    }
    for finding in &findings {
        plain::log(
            &log,
            &format!("Found: {} - {}", finding.title(), finding.explanation()),
        );
    }
    Ok(Inspection { daemons, findings })
}

/// Applies one fix (not `Reinstall`, which runs Windows Installer through `transaction`).
pub(crate) fn apply(fix: &Fix, inspection: &Inspection) -> Result<()> {
    let root = transaction::root()?;
    let log = doctor_log();
    plain::log(&log, &format!("Fixing: {fix:?}"));
    let result = apply_inner(&root, fix, inspection);
    match &result {
        Ok(()) => plain::log(&log, "Fixed"),
        Err(error) => plain::log(&log, &format!("Fix failed: {error}")),
    }
    result
}

fn apply_inner(root: &Path, fix: &Fix, inspection: &Inspection) -> Result<()> {
    match fix {
        Fix::Reinstall => Err("Reinstalling runs through Setup".into()),
        Fix::PointSelection(version) => {
            let _lock = compi_update::OperationLock::acquire(root)?;
            let previous_task = compi_update::read_selection(root)
                .ok()
                .flatten()
                .map(|selection| selection.task_version)
                .filter(|task| {
                    root.join("versions")
                        .join(task)
                        .join("compi-daemon.exe")
                        .is_file()
                });
            compi_update::restore_selection(
                root,
                Some(&compi_update::Selection {
                    schema: 1,
                    version: version.clone(),
                    task_version: previous_task.unwrap_or_else(|| version.clone()),
                }),
            )?;
            Ok(())
        }
        Fix::FinishOperation => {
            {
                let _lock = compi_update::OperationLock::acquire(root)?;
                transaction::recover(root, &transaction::data_root()?.join("installer"))?;
            }
            compi_update::recover(&compi_update::InstallTarget {
                kind: compi_update::InstallationKind::InstalledWindows,
                root: root.to_path_buf(),
            })?;
            Ok(())
        }
        Fix::ClearLeftovers => {
            let _lock = compi_update::OperationLock::acquire(root)?;
            crate::msi_actions::clear_stale_lock_receipts(root)?;
            for (attempt, stage) in setup_attempts() {
                if TERMINAL_STAGES.contains(&stage.as_str()) {
                    transaction::clean_managed_data(&attempt)?;
                }
            }
            Ok(())
        }
        Fix::RegisterTask => {
            let _lock = compi_update::OperationLock::acquire(root)?;
            move_task_to_selection(root)
        }
        Fix::RestartDaemon { instance } => {
            stop_instances(std::slice::from_ref(instance), &inspection.daemons)?;
            if instance.is_none() {
                let _lock = compi_update::OperationLock::acquire(root)?;
                move_task_to_selection(root)?;
                run_task()?;
            }
            Ok(())
        }
    }
}

/// Registers the task to the selected version and records that generation.
fn move_task_to_selection(root: &Path) -> Result<()> {
    let selection = compi_update::read_selection(root)?
        .ok_or("Compi has no current version. Reinstall Compi.")?;
    register_task(root, &selection.version)?;
    compi_update::restore_selection(
        root,
        Some(&compi_update::Selection {
            task_version: selection.version.clone(),
            ..selection
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_generation_is_read_from_its_version_folder_only() {
        let root = Path::new(r"C:\Users\someone\AppData\Local\Programs\Compi");
        assert_eq!(
            generation(
                root,
                Path::new(
                    r"c:\users\SOMEONE\appdata\local\programs\compi\versions\0.1.3\compi-daemon.exe"
                )
            )
            .as_deref(),
            Some("0.1.3")
        );
        assert_eq!(
            generation(
                root,
                Path::new(r"C:\Users\someone\AppData\Local\Programs\Compi\compi-daemon.exe")
            ),
            None
        );
        assert_eq!(
            generation(
                root,
                Path::new(r"C:\Other\Compi\versions\0.1.3\compi-daemon.exe")
            ),
            None
        );
        assert!(inside(
            root,
            Path::new(r"C:\Users\someone\AppData\Local\Programs\Compi\compi-daemon.exe")
        ));
        assert!(!inside(
            root,
            Path::new(r"C:\Users\someone\AppData\Local\Programs\Compi-old\compi-daemon.exe")
        ));
    }
}
