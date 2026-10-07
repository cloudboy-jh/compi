use crate::{
    ConnectionFailure, ConnectionFailureKind, DaemonClient, LifecycleStatus, Result, identity,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

struct ProcessRecord {
    pid: u32,
    executable: PathBuf,
    instance: Option<String>,
    supervisor: bool,
}

/// A `compi-daemon` process owned by the current user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDaemonProcess {
    pub pid: u32,
    pub executable: PathBuf,
    /// The `--instance` it was launched with; `None` is the default instance.
    pub instance: Option<String>,
}

/// Every `compi-daemon` process owned by the current user, from any installation or
/// build directory. Development tooling uses this to find stray source previews.
pub fn local_daemon_processes() -> Result<Vec<LocalDaemonProcess>> {
    Ok(processes()?
        .into_iter()
        .map(|record| LocalDaemonProcess {
            pid: record.pid,
            executable: record.executable,
            instance: record.instance,
        })
        .collect())
}

fn scoped_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err("installation attribution requires an absolute path".into());
    }
    let path = match path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => return Err(error.into()),
    };
    #[cfg(windows)]
    {
        Ok(PathBuf::from(
            path.to_string_lossy()
                .trim_start_matches(r"\\?\")
                .to_lowercase(),
        ))
    }
    #[cfg(not(windows))]
    {
        Ok(path)
    }
}

struct InstallationRoots {
    current: PathBuf,
    retained: Vec<PathBuf>,
    mac_bundle: bool,
}

impl InstallationRoots {
    fn load(root: &Path) -> Result<Self> {
        #[cfg(target_os = "macos")]
        validate_plain_path(root)?;
        let current = scoped_path(root)?;
        #[cfg(target_os = "macos")]
        if current
            .extension()
            .is_some_and(|extension| extension == "app")
        {
            return Self::mac_bundle(current);
        }
        Ok(Self {
            current,
            retained: Vec::new(),
            mac_bundle: false,
        })
    }

    fn executable_root(&self, path: &Path) -> Result<Option<&Path>> {
        if self.mac_bundle {
            if !std::iter::once(&self.current)
                .chain(&self.retained)
                .any(|root| path.starts_with(root))
            {
                return Ok(None);
            }
            validate_plain_path(path)?;
        }
        let path = scoped_path(path)?;
        for root in std::iter::once(&self.current).chain(&self.retained) {
            if self.mac_bundle {
                if path == root.join("Contents/MacOS/compi-daemon") {
                    return Ok(Some(root));
                }
            } else if path.starts_with(root) {
                return Ok(Some(root));
            }
        }
        Ok(None)
    }

    #[cfg(target_os = "macos")]
    fn mac_bundle(current: PathBuf) -> Result<Self> {
        use std::io::Read;
        validate_plain_path(&current)?;
        let state = current
            .parent()
            .ok_or("Mac installation has no parent directory")?
            .join(format!(
                ".{}-update",
                current
                    .file_name()
                    .ok_or("Mac installation has no name")?
                    .to_string_lossy()
            ));
        let journal_path = state.join("journal.json");
        validate_plain_path(&journal_path)?;
        let mut retained = Vec::new();
        let file = match std::fs::File::open(&journal_path) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if let Some(file) = file {
            const MAX_JOURNAL_BYTES: u64 = 1024 * 1024;
            if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_JOURNAL_BYTES {
                return Err("retained bundle journal exceeds metadata bounds".into());
            }
            let mut bytes = Vec::new();
            file.take(MAX_JOURNAL_BYTES + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_JOURNAL_BYTES {
                return Err("retained bundle journal exceeds metadata bounds".into());
            }
            let journal: RetainedJournal = serde_json::from_slice(&bytes)?;
            if journal.schema != 1
                || !valid_attempt(&journal.attempt)
                || journal.target.kind != "MacBundle"
                || journal.target.root != current
                || journal.stage
                    != state
                        .join(format!("stage-{}", journal.attempt))
                        .join("payload")
                || !matches!(
                    journal.phase.as_str(),
                    "Prepared"
                        | "Activating"
                        | "Activated"
                        | "Relaunching"
                        | "Complete"
                        | "RolledBack"
                )
                || journal.retained_backups.len() > 128
            {
                return Err("retained bundle journal ownership rejected".into());
            }
            let mut backups = BTreeSet::new();
            for backup in journal.retained_backups {
                validate_backup_name(&state, &backup)?;
                if !backups.insert(backup) {
                    return Err("duplicate retained bundle attribution".into());
                }
            }
            if let Some(backup) = journal.backup {
                if backup != state.join(format!("previous-{}.app", journal.attempt)) {
                    return Err("retained bundle journal backup ownership rejected".into());
                }
                backups.insert(backup);
            }
            for backup in backups {
                validate_plain_path(&backup)?;
                match std::fs::symlink_metadata(&backup) {
                    Ok(metadata) if metadata.is_dir() => retained.push(backup),
                    Ok(_) => return Err("retained bundle is not a directory".into()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(Self {
            current,
            retained,
            mac_bundle: true,
        })
    }
}

// Never canonicalize away an attribution escape. Missing paths are permitted
// for a cached pre-rename executable and backups already consumed by rollback.
fn validate_plain_path(path: &Path) -> Result<()> {
    use std::path::Component;
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err("installation ownership path must be absolute and normalized".into());
    }
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("installation ownership path contains a symlink".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[derive(serde::Deserialize)]
struct RetainedJournal {
    schema: u32,
    attempt: String,
    target: RetainedTarget,
    stage: PathBuf,
    phase: String,
    backup: Option<PathBuf>,
    #[serde(default)]
    retained_backups: Vec<PathBuf>,
}

#[cfg(target_os = "macos")]
#[derive(serde::Deserialize)]
struct RetainedTarget {
    kind: String,
    root: PathBuf,
}

#[cfg(target_os = "macos")]
fn valid_attempt(attempt: &str) -> bool {
    attempt.len() == 32 && attempt.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(target_os = "macos")]
fn validate_backup_name(state: &Path, backup: &Path) -> Result<()> {
    let valid_name = backup
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.strip_prefix("previous-")
                .and_then(|name| name.strip_suffix(".app"))
                .is_some_and(valid_attempt)
        });
    if backup.parent() != Some(state) || !valid_name {
        return Err("retained bundle path ownership rejected".into());
    }
    validate_plain_path(backup)
}

fn process_record(pid: u32, executable: PathBuf, args: &[String]) -> Result<ProcessRecord> {
    let instance = match args.iter().position(|arg| arg == "--instance") {
        Some(index) => {
            let instance = args
                .get(index + 1)
                .ok_or("daemon process has no instance argument")?;
            identity::instance_names(Some(instance))?;
            Some(instance.clone())
        }
        None => None,
    };
    Ok(ProcessRecord {
        pid,
        executable,
        instance,
        supervisor: args.iter().any(|arg| arg == "--supervise"),
    })
}

/// (pid, executable, instance, supervisor) of processes running from the installation.
type SelectedProcess = (u32, PathBuf, Option<String>, bool);

fn selected_processes(
    records: &[ProcessRecord],
    roots: &InstallationRoots,
) -> Result<BTreeSet<SelectedProcess>> {
    let mut selected = BTreeSet::new();
    for record in records {
        let executable = scoped_path(&record.executable)?;
        if roots.executable_root(&record.executable)?.is_some() {
            selected.insert((
                record.pid,
                executable,
                record.instance.clone(),
                record.supervisor,
            ));
        }
    }
    Ok(selected)
}

/// Inspect all known per-user endpoints, but only account for this installation.
/// OS process attribution lets an unrelated older daemon remain untouched even
/// when it cannot understand the lifecycle contract. Unknown attribution fails closed.
pub(crate) fn for_install(root: &Path) -> Result<Vec<LifecycleStatus>> {
    let roots = InstallationRoots::load(root)?;
    let inventory = processes()?;
    let selected_before = selected_processes(&inventory, &roots)?;
    let mut instances = BTreeSet::new();
    instances.insert(None);
    for process in &inventory {
        instances.insert(process.instance.clone());
    }
    match std::fs::read_dir(crate::paths::data_dir()?) {
        Ok(entries) => {
            for entry in entries {
                let name = entry?.file_name().to_string_lossy().into_owned();
                if let Some(instance) = name
                    .strip_prefix("workspace-")
                    .and_then(|name| name.strip_suffix("-v1.json"))
                {
                    identity::instance_names(Some(instance))?;
                    instances.insert(Some(instance.to_owned()));
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut statuses: Vec<LifecycleStatus> = Vec::new();
    for instance in instances {
        let owners: Vec<_> = inventory
            .iter()
            .filter(|process| process.instance == instance)
            .collect();
        let mut selected_owner = false;
        for owner in &owners {
            selected_owner |= roots.executable_root(&owner.executable)?.is_some();
        }
        if !selected_owner && !owners.is_empty() {
            continue;
        }
        // Stale workspace metadata is common; absence must not cost a full connect timeout.
        if owners.is_empty()
            && !DaemonClient::endpoint_available(instance.as_deref(), std::time::Duration::ZERO)?
        {
            continue;
        }
        match DaemonClient::lifecycle_status(instance.as_deref()) {
            Ok(status) => {
                let selected = attributed_status(&roots, &owners, instance.as_deref(), &status)?;
                if selected
                    && !statuses
                        .iter()
                        .any(|existing| existing.server_generation == status.server_generation)
                {
                    statuses.push(status);
                } else if selected_owner && !selected {
                    return Err("target installation has a competing or recovering daemon process; wait for it to exit before updating".into());
                }
            }
            Err(error) => {
                let absent =
                    ConnectionFailure::kind(error.as_ref()) == ConnectionFailureKind::Absent;
                if selected_owner {
                    let state = if owners.iter().any(|owner| owner.supervisor) {
                        "supervisor startup/recovery"
                    } else {
                        "legacy or starting daemon"
                    };
                    return Err(format!("Cannot safely inspect target installation {state} for instance {:?}: {error}. Wait for startup, or deliberately stop that installation using its old application before migration.", instance).into());
                }
                if !absent && owners.is_empty() {
                    return Err(format!("Cannot attribute existing daemon instance {:?} to an installation: {error}", instance).into());
                }
                // An OS-attributed unrelated daemon (including legacy A) is not
                // an update blocker and must never receive a shutdown request.
            }
        }
    }
    let roots_after = InstallationRoots::load(root)?;
    if roots_after.retained != roots.retained
        || selected_processes(&processes()?, &roots_after)? != selected_before
    {
        return Err("target installation daemon/supervisor processes changed during inspection; review update consent again".into());
    }
    Ok(statuses)
}

fn attributed_status(
    roots: &InstallationRoots,
    owners: &[&ProcessRecord],
    instance: Option<&str>,
    status: &LifecycleStatus,
) -> Result<bool> {
    if status.instance.as_deref() != instance {
        return Err("daemon endpoint instance does not match its lifecycle identity".into());
    }
    let Some(owner) = owners
        .iter()
        .find(|owner| owner.pid == status.daemon_pid && !owner.supervisor)
    else {
        let mut selected_owner = false;
        for owner in owners {
            selected_owner |= roots.executable_root(&owner.executable)?.is_some();
        }
        if roots
            .executable_root(Path::new(&status.daemon_executable))?
            .is_some()
            || selected_owner
        {
            return Err(
                "cannot match target daemon lifecycle identity to an observed OS process".into(),
            );
        }
        return Ok(false);
    };
    let Some(observed_root) = roots.executable_root(&owner.executable)? else {
        return Ok(false);
    };
    let cached_path = scoped_path(Path::new(&status.daemon_executable))?;
    let Some(cached_root) = roots.executable_root(Path::new(&status.daemon_executable))? else {
        return Err("target daemon lifecycle executable has unknown installation ownership".into());
    };
    let observed_path = scoped_path(&owner.executable)?;
    if observed_path.strip_prefix(observed_root)? != cached_path.strip_prefix(cached_root)?
        || (!roots.mac_bundle && observed_path != cached_path)
    {
        return Err(
            "target daemon lifecycle executable does not match its observed OS process".into(),
        );
    }
    for process in owners {
        if process.pid == status.daemon_pid && !process.supervisor {
            continue;
        }
        if process.supervisor
            && Some(process.pid) == status.supervisor_pid
            && roots.executable_root(&process.executable)?.is_some()
        {
            continue;
        }
        return Err("target installation has a competing or recovering daemon process; wait for it to exit before updating".into());
    }
    // A recorded supervisor that no longer runs left its daemon unsupervised:
    // nothing can restart it behind the update. (A supervisor killed by closing
    // its console window used to block updates here forever.) A live process
    // holding that PID without being this installation's supervisor was already
    // rejected as a competitor above.
    Ok(true)
}

#[cfg(windows)]
fn processes() -> Result<Vec<ProcessRecord>> {
    use serde::Deserialize;
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    #[derive(Deserialize)]
    struct Row {
        pid: u32,
        executable: String,
        command_line: String,
    }
    // Constant script, no shell interpolation of paths, instances, or user input.
    // CIM's kernel process identity plus GetOwnerSid limits legacy attribution
    // to the same user as the authenticated named-pipe endpoint.
    // try/catch reports only the exception message, never a formatted PowerShell error record.
    const SCRIPT: &str = r#"try { $ErrorActionPreference='Stop'; [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); $rows=@(Get-CimInstance Win32_Process -Filter "Name='compi-daemon.exe'" | ForEach-Object { $owner=Invoke-CimMethod -InputObject $_ -MethodName GetOwnerSid; if ($owner.Sid -eq $env:COMPI_LIFECYCLE_USER_SID) { if (!$_.ExecutablePath -or !$_.CommandLine) { throw 'Cannot attribute owned daemon process executable or arguments' }; [pscustomobject]@{pid=$_.ProcessId;executable=$_.ExecutablePath;command_line=$_.CommandLine} } }); ConvertTo-Json -InputObject $rows -Compress } catch { [Console]::Error.WriteLine($_.Exception.Message); exit 1 }"#;
    let output = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            SCRIPT,
        ])
        .env(
            "COMPI_LIFECYCLE_USER_SID",
            identity::current_user_sid_string()?,
        )
        .creation_flags(0x08000000)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "Cannot inventory per-user daemon processes: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let rows: Vec<Row> = serde_json::from_slice(&output.stdout)?;
    rows.into_iter()
        .map(|row| {
            let args: Vec<_> = row
                .command_line
                .split_whitespace()
                .map(|arg| arg.trim_matches('"').to_owned())
                .collect();
            process_record(row.pid, PathBuf::from(row.executable), &args)
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn processes() -> Result<Vec<ProcessRecord>> {
    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_listpids(kind: u32, typeinfo: u32, buffer: *mut std::ffi::c_void, size: i32)
        -> i32;
        fn proc_pidpath(pid: i32, buffer: *mut std::ffi::c_void, size: u32) -> i32;
    }
    const PROC_UID_ONLY: u32 = 4;
    let uid = unsafe { libc::geteuid() };
    let bytes = unsafe { proc_listpids(PROC_UID_ONLY, uid, std::ptr::null_mut(), 0) };
    if bytes <= 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut pids = vec![0_i32; bytes as usize / 4 + 128];
    let count = unsafe {
        proc_listpids(
            PROC_UID_ONLY,
            uid,
            pids.as_mut_ptr().cast(),
            (pids.len() * 4) as i32,
        )
    };
    if count < 0 || count as usize >= pids.len() * 4 {
        return Err(
            "per-user process inventory changed or could not be read; retry update inspection"
                .into(),
        );
    }
    let mut records = Vec::new();
    for pid in pids
        .into_iter()
        .take(count as usize / 4)
        .filter(|pid| *pid > 0)
    {
        let mut path = [0_u8; 4096];
        let length = unsafe { proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
        if length <= 0 {
            continue;
        } // A process may exit during enumeration.
        let path = std::ffi::CStr::from_bytes_until_nul(&path)?.to_str()?;
        let executable = PathBuf::from(path);
        if executable.file_name().and_then(|name| name.to_str()) != Some("compi-daemon") {
            continue;
        }
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut size: usize = 0;
        if unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        } == -1
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut bytes = vec![0_u8; size];
        if unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                bytes.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        } == -1
        {
            return Err(std::io::Error::last_os_error().into());
        }
        bytes.truncate(size);
        if bytes.len() < 4 {
            return Err("invalid daemon process argument inventory".into());
        }
        let argc = i32::from_ne_bytes(bytes[..4].try_into().unwrap());
        if argc <= 0 {
            return Err("invalid daemon process argument count".into());
        }
        let prefix = bytes[4..]
            .iter()
            .position(|byte| *byte == 0)
            .ok_or("unterminated daemon executable path")?
            + 5;
        let start = bytes[prefix..]
            .iter()
            .position(|byte| *byte != 0)
            .map(|index| prefix + index)
            .ok_or("missing daemon process arguments")?;
        let args: Vec<_> = bytes[start..]
            .split(|byte| *byte == 0)
            .take(argc as usize)
            .map(|arg| std::str::from_utf8(arg).map(str::to_owned))
            .collect::<std::result::Result<_, _>>()?;
        records.push(process_record(pid as u32, executable, &args)?);
    }
    Ok(records)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn processes() -> Result<Vec<ProcessRecord>> {
    use std::os::unix::fs::MetadataExt;
    let mut records = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if entry.metadata()?.uid() != unsafe { libc::geteuid() } {
            continue;
        }
        let executable = match std::fs::read_link(entry.path().join("exe")) {
            Ok(path) => path,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if executable.file_name().and_then(|name| name.to_str()) != Some("compi-daemon") {
            continue;
        }
        let bytes = std::fs::read(entry.path().join("cmdline"))?;
        let args: Vec<_> = bytes
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| std::str::from_utf8(arg).map(str::to_owned))
            .collect::<std::result::Result<_, _>>()?;
        records.push(process_record(pid, executable, &args)?);
    }
    Ok(records)
}

// Retained-bundle attribution is macOS-only; Windows paths are case-folded by design.
#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture {
        directory: PathBuf,
        root: PathBuf,
        state: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = std::env::temp_dir().join(format!(
                "compi-retained-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&directory).unwrap();
            let directory = scoped_path(&directory).unwrap();
            let root = directory.join("compi.app");
            let state = directory.join(".compi.app-update");
            std::fs::create_dir(&root).unwrap();
            std::fs::create_dir(&state).unwrap();
            Self {
                directory,
                root,
                state,
            }
        }

        fn backup(&self, attempt: &str) -> PathBuf {
            self.state.join(format!("previous-{attempt}.app"))
        }

        fn journal(
            &self,
            attempt: &str,
            backups: &[PathBuf],
            backup: Option<&Path>,
        ) -> serde_json::Value {
            serde_json::json!({
                "schema": 1,
                "attempt": attempt,
                "target": { "kind": "MacBundle", "root": self.root },
                "stage": self.state.join(format!("stage-{attempt}")).join("payload"),
                "phase": "Prepared",
                "backup": backup,
                "retained_backups": backups,
            })
        }

        fn write_journal(&self, journal: &serde_json::Value) {
            std::fs::write(
                self.state.join("journal.json"),
                serde_json::to_vec(journal).unwrap(),
            )
            .unwrap();
        }

        fn roots(&self) -> Result<InstallationRoots> {
            InstallationRoots::mac_bundle(self.root.clone())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    const FIRST: &str = "0123456789abcdef0123456789abcdef";
    const SECOND: &str = "abcdef0123456789abcdef0123456789";

    fn daemon(pid: u32, bundle: &Path) -> ProcessRecord {
        ProcessRecord {
            pid,
            executable: bundle.join("Contents/MacOS/compi-daemon"),
            instance: Some("work".into()),
            supervisor: false,
        }
    }

    fn status(record: &ProcessRecord) -> LifecycleStatus {
        LifecycleStatus {
            lifecycle_version: 1,
            product_version: "0.1.3".into(),
            protocol_version: 1,
            daemon_pid: record.pid,
            supervisor_pid: None,
            daemon_executable: record.executable.to_string_lossy().into_owned(),
            server_id: crate::ServerId::new("server"),
            server_generation: crate::ServerGeneration::new("generation"),
            instance: record.instance.clone(),
            workspace_revision: 1,
            connected_clients: Vec::new(),
            live_surfaces: Vec::new(),
        }
    }

    #[test]
    fn retained_daemon_survives_subsequent_staging_attached_and_detached() {
        let fixture = Fixture::new();
        let retained = fixture.backup(FIRST);
        std::fs::create_dir_all(fixture.root.join("Contents/MacOS")).unwrap();
        std::fs::write(
            fixture.root.join("Contents/MacOS/compi-daemon"),
            b"retained payload",
        )
        .unwrap();
        std::fs::rename(&fixture.root, &retained).unwrap();
        std::fs::create_dir(&fixture.root).unwrap();
        let original = daemon(10, &fixture.root);
        let observed = daemon(10, &retained);
        let records = [
            daemon(10, &retained),
            daemon(11, &fixture.directory.join("foreign.app")),
            daemon(12, &fixture.backup(SECOND)),
        ];
        std::fs::create_dir(fixture.backup(SECOND)).unwrap();
        for attached in [false, true] {
            let mut cached = status(&original);
            if attached {
                cached.connected_clients.push(1);
            }
            let mut older = fixture.journal(FIRST, &[], Some(&retained));
            older.as_object_mut().unwrap().remove("retained_backups");
            fixture.write_journal(&older);
            assert!(
                attributed_status(
                    &fixture.roots().unwrap(),
                    &[&observed],
                    Some("work"),
                    &cached
                )
                .unwrap()
            );
            fixture.write_journal(&fixture.journal(SECOND, std::slice::from_ref(&retained), None));
            let roots = fixture.roots().unwrap();
            assert!(attributed_status(&roots, &[&observed], Some("work"), &cached).unwrap());
            let selected = selected_processes(&records, &roots).unwrap();
            assert_eq!(
                selected,
                BTreeSet::from([(10, observed.executable.clone(), Some("work".into()), false)])
            );
        }
    }

    #[test]
    fn lifecycle_requires_matching_pid_instance_executable_and_no_competitors() {
        let fixture = Fixture::new();
        let retained = fixture.backup(FIRST);
        std::fs::create_dir(&retained).unwrap();
        fixture.write_journal(&fixture.journal(SECOND, std::slice::from_ref(&retained), None));
        let roots = fixture.roots().unwrap();
        let observed = daemon(10, &retained);
        let cached = status(&daemon(10, &fixture.root));
        let reused = daemon(20, &retained);
        assert!(attributed_status(&roots, &[&reused], Some("work"), &cached).is_err());
        assert!(attributed_status(&roots, &[], Some("work"), &cached).is_err());
        assert!(attributed_status(&roots, &[&observed], Some("other"), &cached).is_err());
        let mut foreign_cached = cached.clone();
        foreign_cached.daemon_executable = fixture
            .directory
            .join("foreign.app/Contents/MacOS/compi-daemon")
            .to_string_lossy()
            .into_owned();
        assert!(attributed_status(&roots, &[&observed], Some("work"), &foreign_cached).is_err());
        let competitor = daemon(11, &fixture.directory.join("foreign.app"));
        assert!(
            attributed_status(&roots, &[&observed, &competitor], Some("work"), &cached).is_err()
        );
        assert!(
            !attributed_status(&roots, &[&competitor], Some("work"), &status(&competitor)).unwrap()
        );
        let nested = daemon(12, &fixture.root.join("foreign.app"));
        assert!(roots.executable_root(&nested.executable).unwrap().is_none());
        let mut supervised = cached;
        supervised.supervisor_pid = Some(13);
        assert!(
            attributed_status(&roots, &[&observed], Some("work"), &supervised).unwrap(),
            "an exited supervisor leaves an unsupervised daemon that can be updated"
        );
        let mut supervisor = daemon(13, &retained);
        supervisor.supervisor = true;
        assert!(
            attributed_status(&roots, &[&observed, &supervisor], Some("work"), &supervised)
                .unwrap()
        );
        let impostor = daemon(13, &retained);
        assert!(
            attributed_status(&roots, &[&observed, &impostor], Some("work"), &supervised).is_err()
        );
    }

    #[test]
    fn malformed_or_foreign_journal_attribution_is_rejected_even_if_backup_missing() {
        let fixture = Fixture::new();
        let retained = fixture.backup(FIRST);
        let valid = fixture.journal(SECOND, std::slice::from_ref(&retained), None);
        for invalid in [
            {
                let mut value = valid.clone();
                value["schema"] = 2.into();
                value
            },
            {
                let mut value = valid.clone();
                value["target"]["root"] = serde_json::json!(fixture.directory.join("foreign.app"));
                value
            },
            {
                let mut value = valid.clone();
                value["target"]["kind"] = "PortableWindows".into();
                value
            },
            {
                let mut value = valid.clone();
                value["stage"] = serde_json::json!(fixture.state.join("foreign"));
                value
            },
            fixture.journal(
                SECOND,
                &[fixture
                    .directory
                    .join("previous-0123456789abcdef0123456789abcdef.app")],
                None,
            ),
            fixture.journal(SECOND, &[fixture.state.join("previous-short.app")], None),
            fixture.journal(SECOND, &[retained.clone(), retained.clone()], None),
            fixture.journal(SECOND, &vec![retained.clone(); 129], None),
            fixture.journal(SECOND, &[], Some(&retained)),
        ] {
            fixture.write_journal(&invalid);
            assert!(fixture.roots().is_err());
        }
        fixture.write_journal(&valid);
        assert!(fixture.roots().unwrap().retained.is_empty());
        std::fs::write(
            fixture.state.join("journal.json"),
            vec![b' '; 1024 * 1024 + 1],
        )
        .unwrap();
        assert!(fixture.roots().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn retained_paths_and_executables_cannot_escape_through_symlinks() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let retained = fixture.backup(FIRST);
        let foreign = fixture.directory.join("foreign.app");
        std::fs::create_dir(&foreign).unwrap();
        symlink(&foreign, &retained).unwrap();
        fixture.write_journal(&fixture.journal(SECOND, std::slice::from_ref(&retained), None));
        assert!(fixture.roots().is_err());
        std::fs::remove_file(&retained).unwrap();
        std::fs::create_dir(&retained).unwrap();
        symlink(&foreign, retained.join("Contents")).unwrap();
        let roots = fixture.roots().unwrap();
        assert!(
            roots
                .executable_root(&daemon(10, &retained).executable)
                .is_err()
        );
        std::fs::remove_file(fixture.state.join("journal.json")).unwrap();
        let external = fixture.directory.join("external.json");
        std::fs::write(
            &external,
            serde_json::to_vec(&fixture.journal(SECOND, &[], None)).unwrap(),
        )
        .unwrap();
        symlink(external, fixture.state.join("journal.json")).unwrap();
        assert!(fixture.roots().is_err());
    }
}
