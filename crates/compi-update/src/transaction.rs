use crate::*;
use fs2::FileExt;
use rand::RngCore;
use std::{
    collections::HashSet,
    ffi::OsString,
    process::{Child, Command},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum InstallationKind {
    InstalledWindows,
    PortableWindows,
    MacBundle,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallTarget {
    pub kind: InstallationKind,
    pub root: PathBuf,
}
impl InstallTarget {
    pub fn detect() -> Result<Self> {
        let executable = std::env::current_exe()?.canonicalize()?;
        #[cfg(target_os = "macos")]
        {
            let root = executable
                .ancestors()
                .find(|p| p.extension().is_some_and(|e| e == "app"))
                .ok_or_else(|| err("Compi must run from an installed app bundle"))?
                .to_path_buf();
            if root.starts_with("/Volumes") {
                return Err(err(
                    "Move Compi.app from the read-only disk image to Applications before updating",
                ));
            }
            Ok(Self {
                kind: InstallationKind::MacBundle,
                root,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let parent = executable
                .parent()
                .ok_or_else(|| err("Cannot find installation directory"))?;
            let root = if parent
                .parent()
                .is_some_and(|p| p.file_name().is_some_and(|n| n == "versions"))
            {
                parent
                    .parent()
                    .and_then(Path::parent)
                    .unwrap()
                    .to_path_buf()
            } else {
                parent.to_path_buf()
            };
            let kind = if root.join("Compi-Setup.exe").is_file() {
                InstallationKind::InstalledWindows
            } else {
                InstallationKind::PortableWindows
            };
            Ok(Self { kind, root })
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub schema: u32,
    pub version: String,
    pub task_version: String,
}
pub(crate) fn version_path(root: &Path, version: &str) -> Result<PathBuf> {
    let parsed =
        semver::Version::parse(version).map_err(|_| err("Invalid selected payload version"))?;
    if parsed.to_string() != version {
        return Err(err("Selected payload version is not canonical"));
    }
    Ok(root.join("versions").join(version))
}
pub fn read_selection(root: &Path) -> Result<Option<Selection>> {
    let path = root.join("selection.json");
    if !path.exists() {
        return Ok(None);
    }
    let selection: Selection = read_json(&path, 16384)?;
    if selection.schema != 1 {
        return Err(err("Unsupported installed selection schema"));
    }
    version_path(root, &selection.version)?;
    version_path(root, &selection.task_version)?;
    Ok(Some(selection))
}
pub fn restore_selection(root: &Path, previous: Option<&Selection>) -> Result<()> {
    match previous {
        Some(selection) => atomic_json(&root.join("selection.json"), selection),
        None => {
            let path = root.join("selection.json");
            if path.exists() {
                fs::remove_file(path)?;
            }
            Ok(())
        }
    }
}
pub fn activate_msi_payload(root: &Path, version: &str, task_version: &str) -> Result<Selection> {
    let _lock = OperationLock::acquire(root)?;
    activate_msi_payload_locked(root, version, task_version)
}
/// Caller must own the installation operation lock for the entire MSI transaction.
pub fn activate_msi_payload_locked(
    root: &Path,
    version: &str,
    task_version: &str,
) -> Result<Selection> {
    let payload = version_path(root, version)?;
    if !payload.join("compi.exe").is_file() || !payload.join("compi-daemon.exe").is_file() {
        return Err(err("MSI payload is incomplete; selection was not changed"));
    }
    let task = version_path(root, task_version)?;
    if !task.join("compi-daemon.exe").is_file() {
        return Err(err("Scheduled-task daemon generation is missing"));
    }
    if let Some(previous) = read_selection(root)?
        && semver::Version::parse(version).unwrap()
            < semver::Version::parse(&previous.version).unwrap()
    {
        return Err(err("Refusing installer downgrade"));
    }
    let selection = Selection {
        schema: 1,
        version: version.into(),
        task_version: task_version.into(),
    };
    atomic_json(&root.join("selection.json"), &selection)?;
    Ok(selection)
}
pub fn selected_executable(root: &Path, daemon_generation: Option<&str>) -> Result<PathBuf> {
    let selection = read_selection(root)?
        .ok_or_else(|| err("Installation has no active payload selection; repair Compi"))?;
    let version = daemon_generation.unwrap_or(&selection.version);
    let payload = version_path(root, version)?;
    let executable = payload.join(if daemon_generation.is_some() {
        "compi-daemon.exe"
    } else {
        "compi.exe"
    });
    if !executable.is_file() {
        return Err(err("Selected payload executable is missing; repair Compi"));
    }
    Ok(executable)
}
pub(crate) fn state_dir(root: &Path) -> PathBuf {
    if root.extension().is_some_and(|e| e == "app") {
        root.parent().unwrap_or(root).join(format!(
            ".{}-update",
            root.file_name().unwrap_or_default().to_string_lossy()
        ))
    } else {
        root.join(".compi-update")
    }
}
pub struct OperationLock {
    file: File,
}
impl OperationLock {
    pub fn acquire(root: &Path) -> Result<Self> {
        let state = state_dir(root);
        fs::create_dir_all(&state)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(state.join("operation.lock"))?;
        file.try_lock_exclusive().map_err(|e| {
            err(format!(
                "Another install/update/repair/removal owns this installation: {e}"
            ))
        })?;
        Ok(Self { file })
    }
}
impl Drop for OperationLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}
pub(crate) fn unique_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub(crate) fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| err("Missing state directory"))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".write-{}", unique_id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        serde_json::to_writer(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        atomic_replace(&temp, path)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
fn atomic_replace(from: &Path, to: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(from, to)?;
        Ok(())
    }
}
pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path, max: u64) -> Result<T> {
    let file = File::open(path)?;
    if file.metadata()?.len() > max {
        return Err(err("State file exceeds size limit"));
    }
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(err("State file exceeds size limit"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedUpdate {
    pub journal_path: PathBuf,
    pub target: InstallTarget,
    pub version: String,
    pub stage: PathBuf,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelaunchHost {
    pub arguments: Vec<String>,
    pub receipt_path: PathBuf,
    pub receipt_token: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackHost {
    pub executable: PathBuf,
    pub version: String,
    pub host: RelaunchHost,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelaunchRequest {
    pub old_process_ids: Vec<u32>,
    pub hosts: Vec<RelaunchHost>,
    pub rollback_hosts: Vec<RollbackHost>,
    /// Preserve a compatible live default daemon; otherwise select the verified new generation.
    pub replace_default_daemon: bool,
    pub timeout_seconds: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessReceipt {
    pub token: String,
    pub version: String,
    pub attached: bool,
    pub error: Option<String>,
}
pub fn publish_readiness(path: &Path, receipt: &ReadinessReceipt) -> Result<()> {
    if receipt.token.len() < 32 || receipt.token.len() > 256 {
        return Err(err("Invalid update receipt token"));
    }
    if serde_json::to_vec(receipt)?.len() > 16384 {
        return Err(err("Update readiness receipt exceeds limit"));
    }
    atomic_json(path, receipt)
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum JournalStage {
    Prepared,
    Activating,
    Activated,
    Relaunching,
    Complete,
    RolledBack,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnedTaskRegistration {
    pub name: String,
    pub xml: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Journal {
    pub schema: u32,
    pub attempt: String,
    pub target: InstallTarget,
    pub manifest: ReleaseManifest,
    pub signed: SignedManifest,
    pub stage: PathBuf,
    pub prior_selection: Option<Selection>,
    pub phase: JournalStage,
    pub backup: Option<PathBuf>,
    #[serde(default)]
    pub retained_backups: Vec<PathBuf>,
    #[serde(default)]
    pub prior_task: Option<OwnedTaskRegistration>,
    pub error: Option<String>,
}
impl Journal {
    pub(crate) fn prepared(
        target: InstallTarget,
        attempt: String,
        manifest: ReleaseManifest,
        stage: PathBuf,
        signed: SignedManifest,
    ) -> Result<Self> {
        let prior_selection = read_selection(&target.root)?;
        let path = state_dir(&target.root).join("journal.json");
        let mut retained_backups = Vec::new();
        if path.exists() {
            let previous: Journal = read_json(&path, 1024 * 1024)?;
            validate_journal(&path, &previous)?;
            if previous.target.root != target.root || previous.target.kind != target.kind {
                return Err(err("Retained journal belongs to another installation"));
            }
            retained_backups = previous.retained_backups;
            if let Some(backup) = previous.backup
                && !retained_backups.contains(&backup)
            {
                retained_backups.push(backup);
            }
        }
        if retained_backups.len() > 128 {
            return Err(err("Retained bundle inventory exceeds safe bound"));
        }
        Ok(Self {
            schema: 1,
            attempt,
            target,
            manifest,
            signed,
            stage,
            prior_selection,
            phase: JournalStage::Prepared,
            backup: None,
            retained_backups,
            prior_task: None,
            error: None,
        })
    }
}
pub(crate) fn ensure_newer_than_selected(target: &InstallTarget, version: &str) -> Result<()> {
    if let Some(selected) = read_selection(&target.root)?
        && semver::Version::parse(version).map_err(|_| err("Invalid version"))?
            <= semver::Version::parse(&selected.version).map_err(|_| err("Invalid selection"))?
    {
        return Err(err("Refusing same-version or downgrade update"));
    }
    Ok(())
}
pub(crate) fn validate_payload(target: &InstallTarget, payload: &Path) -> Result<()> {
    match target.kind {
        InstallationKind::MacBundle => {
            let app = payload.join("Compi.app");
            for file in [
                "Contents/MacOS/compi",
                "Contents/MacOS/compi-daemon",
                "Contents/MacOS/compi-update-worker",
                "Contents/Info.plist",
            ] {
                if !app.join(file).is_file() {
                    return Err(err(format!("Complete macOS bundle missing {file}")));
                }
            }
        }
        _ => {
            for file in ["compi.exe", "compi-daemon.exe", "compi-update-worker.exe"] {
                if !payload.join(file).is_file() {
                    return Err(err(format!("Complete Windows payload missing {file}")));
                }
            }
        }
    }
    Ok(())
}
fn validate_journal(path: &Path, journal: &Journal) -> Result<()> {
    if journal.schema != 1
        || journal.attempt.len() != 32
        || !journal.attempt.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(err("Invalid operation journal"));
    }
    let state = state_dir(&journal.target.root);
    if path != state.join("journal.json")
        || journal.stage
            != state
                .join(format!("stage-{}", journal.attempt))
                .join("payload")
    {
        return Err(err("Journal staging ownership rejected"));
    }
    version_path(&journal.target.root, &journal.manifest.version)?;
    if !journal.target.root.is_absolute() {
        return Err(err("Journal installation root must be absolute"));
    }
    validate_no_symlink_components(path)?;
    validate_no_symlink_components(&journal.target.root)?;
    validate_no_symlink_components(&journal.stage)?;
    if journal.retained_backups.len() > 128
        || (journal.target.kind != InstallationKind::MacBundle
            && (!journal.retained_backups.is_empty() || journal.backup.is_some()))
        || serde_json::to_vec(journal)?.len() > 1024 * 1024
    {
        return Err(err("Invalid bounded retained bundle inventory"));
    }
    let mut retained = std::collections::HashSet::new();
    for backup in &journal.retained_backups {
        validate_retained_backup(&state, backup)?;
        if !retained.insert(backup) {
            return Err(err("Duplicate retained bundle attribution"));
        }
    }
    if let Some(selection) = &journal.prior_selection {
        if selection.schema != 1 {
            return Err(err("Invalid prior selection schema"));
        }
        version_path(&journal.target.root, &selection.version)?;
        version_path(&journal.target.root, &selection.task_version)?;
    }
    if let Some(task) = &journal.prior_task
        && (journal.target.kind != InstallationKind::InstalledWindows
            || task.name.len() > 256
            || task.xml.as_ref().is_some_and(|xml| xml.len() > 256 * 1024))
    {
        return Err(err("Invalid bounded prior task registration"));
    }
    if let Some(backup) = &journal.backup {
        if backup != &state.join(format!("previous-{}.app", journal.attempt)) {
            return Err(err("Journal backup ownership rejected"));
        }
        validate_retained_backup(&state, backup)?;
    }
    Ok(())
}
fn validate_no_symlink_components(path: &Path) -> Result<()> {
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(err("Journal ownership cannot traverse a parent component"));
    }
    for component in path.ancestors() {
        match fs::symlink_metadata(component) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(err("Journal ownership cannot traverse a symlink"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
fn validate_retained_backup(state: &Path, backup: &Path) -> Result<()> {
    let name = backup
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let attempt = name
        .strip_prefix("previous-")
        .and_then(|name| name.strip_suffix(".app"));
    if backup.parent() != Some(state)
        || attempt.is_none_or(|id| id.len() != 32 || !id.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        return Err(err("Retained bundle ownership rejected"));
    }
    validate_no_symlink_components(backup)?;
    Ok(())
}
fn validate_activation_baseline(journal: &Journal) -> Result<()> {
    if read_selection(&journal.target.root)? != journal.prior_selection {
        return Err(err(
            "Installation selection changed after staging; restage the verified update before installing",
        ));
    }
    Ok(())
}
fn selected_task_version(journal: &Journal, replace_default_daemon: bool) -> String {
    if replace_default_daemon {
        journal.manifest.version.clone()
    } else {
        journal
            .prior_selection
            .as_ref()
            .map(|selection| selection.task_version.clone())
            .unwrap_or_else(|| journal.manifest.version.clone())
    }
}

#[cfg(windows)]
fn task_powershell(script: &str) -> Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    let executable =
        PathBuf::from(std::env::var_os("SystemRoot").ok_or_else(|| err("SystemRoot is not set"))?)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    // try/catch reports only the exception message, never a formatted PowerShell error record.
    let script = format!(
        "try {{ {script} }} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}"
    );
    let output = Command::new(executable)
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x08000000)
        .output()?;
    if !output.status.success() {
        return Err(err(format!(
            "Cannot safely change owned daemon task: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    if output.stdout.len() > 256 * 1024 {
        return Err(err("Daemon task metadata exceeds safe bound"));
    }
    Ok(output)
}

#[cfg(windows)]
fn task_guard_script(journal: &Journal, include_new: bool) -> Result<String> {
    let previous = match &journal.prior_selection {
        Some(selection) => {
            version_path(&journal.target.root, &selection.task_version)?.join("compi-daemon.exe")
        }
        None => journal.target.root.join("compi-daemon.exe"),
    };
    let mut paths = vec![previous];
    if include_new {
        paths.push(
            version_path(&journal.target.root, &journal.manifest.version)?.join("compi-daemon.exe"),
        );
    }
    let paths = paths
        .iter()
        .map(|path| format!("'{}'", path.to_string_lossy().replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!(
        r#"
$ErrorActionPreference='Stop'
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$id=[Security.Principal.WindowsIdentity]::GetCurrent()
$name='Compi Daemon-'+$id.User.Value
$allowed=@({paths})
function Normalize-OwnedPath([string]$value) {{
    $path=$value
    if ($path.StartsWith('"') -and $path.EndsWith('"') -and $path.Length -ge 2) {{
        $path=$path.Substring(1,$path.Length-2)
    }}
    if ($path.Contains('"')) {{ throw 'Invalid quoted task executable' }}
    $path=$path.Replace('/','\')
    if ($path.StartsWith('\\?\UNC\',[StringComparison]::OrdinalIgnoreCase)) {{
        $path='\\'+$path.Substring(8)
    }} elseif ($path.StartsWith('\\?\',[StringComparison]::OrdinalIgnoreCase)) {{
        $path=$path.Substring(4)
    }}
    if ($path.StartsWith('\\.\') -or $path.StartsWith('\\?\') -or
        ($path -notmatch '^[A-Za-z]:\\' -and $path -notmatch '^\\\\[^\\]+\\[^\\]+\\')) {{
        throw 'Task executable must use an absolute drive or UNC path'
    }}
    # Extended paths preserve trailing dots/spaces and dot components that ordinary
    # Win32 paths can reinterpret. Refuse these instead of equating different files.
    foreach ($component in ($path -split '\\')) {{
        if ($component -match '[. ]$') {{ throw 'Ambiguous task executable path component' }}
    }}
    return [IO.Path]::GetFullPath($path)
}}
$allowed=@($allowed | ForEach-Object {{ Normalize-OwnedPath $_ }})
# Windows reports a task's principal as a SID, DOMAIN\user, or a bare user name
# ("johns"); compare the resolved SID so the account's own task is never "foreign".
function Assert-Owner($owner, $commands) {{
    $ownerSid=$owner
    if ($owner -notlike 'S-1-*') {{
        try {{ $ownerSid=([Security.Principal.NTAccount]$owner).Translate([Security.Principal.SecurityIdentifier]).Value }} catch {{ $ownerSid=$null }}
    }}
    $mine=($ownerSid -eq $id.User.Value)
    if (-not $mine) {{ throw 'Task belongs to another Windows account' }}
    if (@($commands).Count -ne 1) {{ throw 'Task does not have exactly one owned daemon action' }}
    foreach ($command in $commands) {{
        $path=Normalize-OwnedPath $command
        if (-not ($allowed | Where-Object {{ [string]::Equals($_,$path,[StringComparison]::OrdinalIgnoreCase) }})) {{
            throw 'Task belongs to another installation or generation'
        }}
    }}
}}
$t=Get-ScheduledTask -ErrorAction Stop | Where-Object {{ $_.TaskName -eq $name -and $_.TaskPath -eq '\' }}
if ($t) {{ Assert-Owner $t.Principal.UserId @($t.Actions | ForEach-Object {{ $_.Execute }}) }}
"#
    ))
}

#[cfg(windows)]
fn snapshot_owned_task(journal: &Journal) -> Result<OwnedTaskRegistration> {
    let script = task_guard_script(journal, false)?
        + r#"
$xml=$null
if ($t) { $xml=(Export-ScheduledTask -TaskName $name -TaskPath '\').Trim() }
@{name=$name;xml=$xml} | ConvertTo-Json -Compress
"#;
    Ok(serde_json::from_slice(&task_powershell(&script)?.stdout)?)
}

#[cfg(windows)]
fn install_verified_task(journal: &Journal) -> Result<()> {
    use std::os::windows::process::CommandExt;
    if Some(&snapshot_owned_task(journal)?) != journal.prior_task.as_ref() {
        return Err(err(
            "Daemon task changed during activation; registration was not replaced",
        ));
    }
    let executable =
        version_path(&journal.target.root, &journal.manifest.version)?.join("compi-daemon.exe");
    let output = Command::new(&executable)
        .arg("--install-task")
        .creation_flags(0x08000000)
        .output()?;
    if !output.status.success() {
        return Err(err(format!(
            "Verified daemon task registration failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn restore_owned_task(journal: &Journal, task: &OwnedTaskRegistration) -> Result<()> {
    let current_guard = task_guard_script(journal, true)?;
    // Validate the retained XML independently, not merely the currently registered action.
    let previous = match &journal.prior_selection {
        Some(selection) => {
            version_path(&journal.target.root, &selection.task_version)?.join("compi-daemon.exe")
        }
        None => journal.target.root.join("compi-daemon.exe"),
    };
    let previous = previous.to_string_lossy().replace('\'', "''");
    let path =
        state_dir(&journal.target.root).join(format!("task-rollback-{}.xml", journal.attempt));
    validate_no_symlink_components(&path)?;
    let quoted_path = path.to_string_lossy().replace('\'', "''");
    let quoted_name = task.name.replace('\'', "''");
    let restore = if let Some(xml) = &task.xml {
        let bytes: Vec<u8> = std::iter::once(0xfeffu16)
            .chain(xml.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect();
        fs::write(&path, bytes)?;
        OpenOptions::new().write(true).open(&path)?.sync_all()?;
        format!(
            r#"
$doc=[xml](Get-Content -LiteralPath '{quoted_path}' -Raw)
Assert-Owner $doc.Task.Principals.Principal.UserId @($doc.Task.Actions.Exec | ForEach-Object {{ $_.Command }})
if (@($doc.Task.Actions.ChildNodes).Count -ne 1) {{ throw 'Prior task contains unowned actions' }}
& "$env:SystemRoot\System32\schtasks.exe" /Create /TN $name /XML '{quoted_path}' /F | Out-Null
if ($LASTEXITCODE -ne 0) {{ throw 'Exact prior task registration could not be restored' }}
"#
        )
    } else {
        r#"
if ($t) {
    & "$env:SystemRoot\System32\schtasks.exe" /Delete /TN $name /F | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Previously absent task could not be removed' }
}
"#
        .to_owned()
    };
    // Accept only our old/new current action, but constrain retained XML to the old action.
    let script = format!(
        "{current_guard}\n$allowed=@(Normalize-OwnedPath '{previous}')\nif ($name -ne '{quoted_name}') {{ throw 'Prior task belongs to another user' }}\n{restore}"
    );
    let result = task_powershell(&script).map(|_| ());
    let _ = fs::remove_file(&path);
    result
}

#[cfg(not(windows))]
fn snapshot_owned_task(_: &Journal) -> Result<OwnedTaskRegistration> {
    Err(err("Windows task replacement requires Windows"))
}
#[cfg(not(windows))]
fn install_verified_task(_: &Journal) -> Result<()> {
    Err(err("Windows task replacement requires Windows"))
}
#[cfg(not(windows))]
fn restore_owned_task(_: &Journal, _: &OwnedTaskRegistration) -> Result<()> {
    Err(err("Windows task recovery requires Windows"))
}
pub fn recover(target: &InstallTarget) -> Result<()> {
    let _lock = OperationLock::acquire(&target.root)?;
    recover_locked(target)
}
pub(crate) fn recover_locked(target: &InstallTarget) -> Result<()> {
    let path = state_dir(&target.root).join("journal.json");
    if !path.exists() {
        return Ok(());
    }
    let mut journal: Journal = read_json(&path, 1024 * 1024)?;
    validate_journal(&path, &journal)?;
    if journal.target.root != target.root || journal.target.kind != target.kind {
        return Err(err("Recovery target does not match journal"));
    }
    if matches!(
        journal.phase,
        JournalStage::Activating | JournalStage::Activated | JournalStage::Relaunching
    ) {
        // Older/missing requests cannot prove the default daemon was preserved.
        let request_path =
            state_dir(&target.root).join(format!("request-{}.json", journal.attempt));
        let replaced = read_json::<RelaunchRequest>(&request_path, 1024 * 1024)
            .map_or(true, |request| request.replace_default_daemon);
        recover_interrupted(&path, &mut journal, &registry_directory()?, |journal| {
            if replaced {
                stop_replacement_daemons(journal)
            } else {
                Ok(())
            }
        })?;
    }
    Ok(())
}
fn recover_interrupted(
    path: &Path,
    journal: &mut Journal,
    inventory: &Path,
    stop_replacements: impl FnOnce(&Journal) -> Result<()>,
) -> Result<()> {
    validate_host_inventory_at(&journal.target, &[], inventory).map_err(|error| {
        err(format!(
            "Close the same-install GUI hosts before interrupted update recovery; {error}"
        ))
    })?;
    stop_replacements(journal)?;
    rollback(path, journal)
}
/// A failed attempt may leave a daemon started from the new payload; restored old GUIs
/// must not attach to it. The prior GUI build (present until rollback completes) stops
/// only idle daemons attributed to this installation via consent-checked lifecycle
/// shutdown; live work blocks rollback instead. This crate does not link the protocol.
fn stop_replacement_daemons(journal: &Journal) -> Result<()> {
    let helper = match journal.target.kind {
        InstallationKind::MacBundle => journal
            .backup
            .as_ref()
            .filter(|backup| backup.exists())
            .unwrap_or(&journal.target.root)
            .join("Contents/MacOS/compi"),
        _ => match &journal.prior_selection {
            Some(selection) => {
                version_path(&journal.target.root, &selection.version)?.join("compi.exe")
            }
            None => journal.target.root.join("compi.exe"),
        },
    };
    let mut command = Command::new(&helper);
    command
        .arg("--stop-idle-update-daemons")
        .arg(&journal.target.root)
        .arg(&journal.manifest.version)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().map_err(|e| {
        err(format!(
            "Cannot inspect daemons started by the failed update with {}: {e}",
            helper.display()
        ))
    })?;
    // Lifecycle inspection and conditional stop are internally bounded; this is a backstop.
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err("Replacement-daemon inspection did not finish"));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if status.success() {
        return Ok(());
    }
    let mut message = String::new();
    if let Some(stderr) = child.stderr.take() {
        use std::io::Read;
        let _ = stderr.take(16384).read_to_string(&mut message);
    }
    Err(err(format!(
        "Replacement daemon was not stopped: {}",
        message.trim()
    )))
}
fn rollback(path: &Path, journal: &mut Journal) -> Result<()> {
    match journal.target.kind {
        InstallationKind::MacBundle => {
            if let Some(backup) = &journal.backup
                && backup.exists()
            {
                let failed =
                    state_dir(&journal.target.root).join(format!("failed-{}.app", journal.attempt));
                if journal.target.root.exists() {
                    fs::rename(&journal.target.root, &failed).map_err(|e| {
                        err(format!("Rollback could not retain failed bundle: {e}"))
                    })?;
                }
                fs::rename(backup, &journal.target.root).map_err(|e| {
                    err(format!(
                        "Rollback could not restore old bundle at {}: {e}",
                        journal.target.root.display()
                    ))
                })?;
            }
        }
        _ => {
            if let Some(task) = &journal.prior_task {
                restore_owned_task(journal, task)?;
            }
            restore_selection(&journal.target.root, journal.prior_selection.as_ref())?;
            restore_root_files(journal);
        }
    }
    journal.phase = JournalStage::RolledBack;
    atomic_json(path, journal)
}
/// Root files an in-app update refreshes from its payload: (payload file, root file).
/// Packages before the launcher joined the payload leave the root launcher as it is.
const ROOT_REFRESH: [(&str, &str); 2] = [
    ("compi-launcher.exe", "compi.exe"),
    ("compi-update-worker.exe", "compi-update-worker.exe"),
];
fn root_backup(journal: &Journal, name: &str) -> PathBuf {
    state_dir(&journal.target.root).join(format!("root-previous-{}-{name}", journal.attempt))
}
/// Replaces the root launcher and helper with the activated payload's copies and keeps the
/// old ones for rollback. Never fails the update: a file that stays locked keeps its old copy.
/// Renames, not overwrites, so a running launcher or helper image can still be replaced.
fn refresh_root_files(journal: &Journal) {
    let root = &journal.target.root;
    let Ok(payload) = version_path(root, &journal.manifest.version) else {
        return;
    };
    for (source, name) in ROOT_REFRESH {
        let source = payload.join(source);
        let destination = root.join(name);
        if !source.is_file() || compare_payload(&source, &destination).is_ok() {
            continue;
        }
        let next = state_dir(root).join(format!("root-next-{}-{name}", journal.attempt));
        let backup = root_backup(journal, name);
        let result = (|| -> Result<()> {
            fs::copy(&source, &next)?;
            let previous = destination.is_file();
            if previous {
                rename_retrying(&destination, &backup)?;
            }
            if let Err(error) = rename_retrying(&next, &destination) {
                if previous {
                    rename_retrying(&backup, &destination)?;
                }
                return Err(error);
            }
            Ok(())
        })();
        let _ = fs::remove_file(&next);
        match result {
            Ok(()) => update_log(
                root,
                &format!("Refreshed {name} from {}", journal.manifest.version),
            ),
            Err(error) => update_log(root, &format!("Kept the previous {name}: {error}")),
        }
    }
}
/// Puts back what `refresh_root_files` replaced. Best effort: the newer launcher and helper
/// also work with the restored selection.
fn restore_root_files(journal: &Journal) {
    let root = &journal.target.root;
    for (_, name) in ROOT_REFRESH {
        let backup = root_backup(journal, name);
        if !backup.is_file() {
            continue;
        }
        let destination = root.join(name);
        let replaced = state_dir(root).join(format!("root-failed-{}-{name}", journal.attempt));
        let result = (|| -> Result<()> {
            let present = destination.is_file();
            if present {
                rename_retrying(&destination, &replaced)?;
            }
            if let Err(error) = rename_retrying(&backup, &destination) {
                if present {
                    rename_retrying(&replaced, &destination)?;
                }
                return Err(error);
            }
            Ok(())
        })();
        let _ = fs::remove_file(&replaced);
        if let Err(error) = result {
            update_log(
                root,
                &format!("Could not restore the previous {name}: {error}"),
            );
        }
    }
}
/// The root launcher exits right after starting Compi, and scanners hold new files briefly.
fn rename_retrying(from: &Path, to: &Path) -> Result<()> {
    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(_) if attempt < 20 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error.into()),
        }
    }
}
/// Appends to the plain-text log beside the journal, which the client also writes.
fn update_log(root: &Path, message: &str) {
    let path = state_dir(root).join("update.log");
    let _ = (|| -> std::io::Result<()> {
        if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > 1024 * 1024) {
            fs::rename(&path, path.with_extension("log.old"))?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        writeln!(file, "{now} {message}")
    })();
}
/// Check activation ownership before releasing GUI hosts or stopping the default daemon.
pub fn preflight_activation(prepared: &PreparedUpdate, replace_default_daemon: bool) -> Result<()> {
    let _lock = OperationLock::acquire(&prepared.target.root)?;
    preflight_activation_locked(prepared, replace_default_daemon).map(|_| ())
}
fn preflight_activation_locked(
    prepared: &PreparedUpdate,
    replace_default_daemon: bool,
) -> Result<Journal> {
    let journal: Journal = read_json(&prepared.journal_path, 1024 * 1024)?;
    validate_journal(&prepared.journal_path, &journal)?;
    if journal.phase != JournalStage::Prepared || journal.manifest.version != prepared.version {
        return Err(err("Update is not prepared for activation"));
    }
    if journal.target.root != prepared.target.root
        || journal.target.kind != prepared.target.kind
        || journal.stage != prepared.stage
    {
        return Err(err(
            "Prepared update no longer matches its installation journal",
        ));
    }
    validate_activation_baseline(&journal)?;
    if journal.target.kind == InstallationKind::InstalledWindows && replace_default_daemon {
        snapshot_owned_task(&journal)?;
    }
    Ok(journal)
}
pub fn launch_worker(prepared: &PreparedUpdate, request: &RelaunchRequest) -> Result<Child> {
    validate_request(request)?;
    let _lock = OperationLock::acquire(&prepared.target.root)?;
    let journal = preflight_activation_locked(prepared, request.replace_default_daemon)?;
    let request_path =
        state_dir(&prepared.target.root).join(format!("request-{}.json", journal.attempt));
    atomic_json(&request_path, request)?;
    let source = match prepared.target.kind {
        InstallationKind::MacBundle => std::env::current_exe()?
            .parent()
            .unwrap()
            .join("compi-update-worker"),
        _ => prepared.target.root.join("compi-update-worker.exe"),
    };
    if !source.is_file() {
        return Err(err(format!(
            "Update helper is missing at {}; repair installation",
            source.display()
        )));
    } // Run owned copy, never replace an executing helper image.
    let helper = state_dir(&prepared.target.root).join(format!(
        "worker-{}{}",
        journal.attempt,
        if cfg!(windows) { ".exe" } else { "" }
    ));
    fs::copy(source, &helper)?;
    let mut command = Command::new(helper);
    command
        .arg("apply")
        .arg("--journal")
        .arg(&prepared.journal_path)
        .arg("--request")
        .arg(request_path);
    let log = OpenOptions::new()
        .append(true)
        .create(true)
        .open(state_dir(&prepared.target.root).join(format!("worker-{}.log", journal.attempt)))?;
    command
        .stdout(std::process::Stdio::from(log.try_clone()?))
        .stderr(std::process::Stdio::from(log));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000 | 0x00000200);
    }
    drop(_lock);
    command.spawn().map_err(|e| {
        err(format!(
            "Could not launch update helper; current installation unchanged: {e}"
        ))
    })
}
fn validate_request(request: &RelaunchRequest) -> Result<()> {
    if request.hosts.is_empty()
        || request.hosts.len() > 32
        || request.rollback_hosts.len() != request.hosts.len()
        || request.old_process_ids.len() > 128
        || request.timeout_seconds == 0
        || request.timeout_seconds > 300
    {
        return Err(err(
            "Invalid bounded update relaunch request or missing prior-build handoffs",
        ));
    }
    let mut receipts = std::collections::HashSet::new();
    let mut tokens = std::collections::HashSet::new();
    for host in request
        .hosts
        .iter()
        .chain(request.rollback_hosts.iter().map(|old| &old.host))
    {
        if host.receipt_token.len() < 32
            || host.receipt_token.len() > 256
            || host.arguments.len() > 128
            || host.arguments.iter().any(|a| a.len() > 32768)
            || !receipts.insert(host.receipt_path.clone())
            || !tokens.insert(host.receipt_token.clone())
        {
            return Err(err("Invalid or duplicate relaunch receipt/token"));
        }
        if !host.arguments.iter().any(|a| a == "--update-restore") {
            return Err(err(
                "Updater relaunch must use an explicit attach-only restore handoff",
            ));
        }
        if host.receipt_path.exists() {
            return Err(err("Readiness receipt already exists; use a fresh attempt"));
        }
    }
    for old in &request.rollback_hosts {
        semver::Version::parse(&old.version).map_err(|_| err("Invalid prior GUI build version"))?;
        if !old.executable.is_absolute() {
            return Err(err(
                "Rollback GUI executable must have an explicit absolute product path",
            ));
        }
    }
    Ok(())
}
fn process_alive(pid: u32) -> Result<bool> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, WAIT_TIMEOUT},
            System::Threading::{OpenProcess, WaitForSingleObject},
        };
        let handle = unsafe { OpenProcess(0x00100000, 0, pid) };
        if handle.is_null() {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(87) {
                return Ok(false);
            }
            return Err(err(format!(
                "Could not establish old GUI process exit: {e}"
            )));
        }
        let wait = unsafe { WaitForSingleObject(handle, 0) };
        unsafe { CloseHandle(handle) };
        if wait == u32::MAX {
            return Err(err("Failed waiting for old GUI process"));
        }
        Ok(wait == WAIT_TIMEOUT)
    }
    #[cfg(not(windows))]
    {
        let status = Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .status()?;
        Ok(status.success())
    }
}
fn wait_for_old_hosts(request: &RelaunchRequest) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(request.timeout_seconds);
    loop {
        let mut alive = false;
        for &pid in &request.old_process_ids {
            alive |= process_alive(pid)?;
        }
        if !alive {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(err(
                "Old GUI hosts did not exit within deadline; close Compi hosts using this installation and retry",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
pub fn apply(
    journal_path: &Path,
    request: &RelaunchRequest,
    mut progress: impl FnMut(UpdateEvent),
) -> Result<()> {
    validate_request(request)?;
    let mut journal: Journal = read_json(journal_path, 1024 * 1024)?;
    validate_journal(journal_path, &journal)?;
    let _lock = OperationLock::acquire(&journal.target.root)?;
    journal = read_json(journal_path, 1024 * 1024)?;
    validate_journal(journal_path, &journal)?;
    if journal.phase != JournalStage::Prepared {
        return Err(err("Journal is not ready; run recovery before retrying"));
    }
    let config = ReleaseConfig::compiled()?;
    let verified = verify_manifest(&serde_json::to_vec(&journal.signed)?, &config)?;
    if serde_json::to_vec(&verified.manifest)? != serde_json::to_vec(&journal.manifest)? {
        return Err(err("Journal manifest differs from publisher signature"));
    }
    apply_authenticated(journal_path, request, &mut journal, &mut progress)
}
fn begin_activation(journal_path: &Path, journal: &mut Journal) -> Result<()> {
    // A retained Prepared failure can be retried with fresh handoffs. Clear it only
    // after this attempt's checks pass, before any replacement GUI can observe it.
    journal.error = None;
    journal.phase = JournalStage::Activating;
    atomic_json(journal_path, journal)
}
// Only entered after journal ownership and the retained publisher signature are authenticated.
fn apply_authenticated(
    journal_path: &Path,
    request: &RelaunchRequest,
    journal: &mut Journal,
    mut progress: impl FnMut(UpdateEvent),
) -> Result<()> {
    let mut activation_started = false;
    let mut new_gui_exit_confirmed = true;
    let result: Result<()> = (|| {
        validate_activation_baseline(journal)?;
        ensure_newer_than_selected(&journal.target, &journal.manifest.version)?;
        let package = journal.stage.parent().unwrap().join("package.zip");
        verify_package(&package, &journal.manifest.artifact)?;
        if journal.stage.exists() {
            fs::remove_dir_all(&journal.stage)?;
        }
        archive::extract(
            &package,
            &journal.stage,
            &Cancellation::new(),
            &mut progress,
        )?;
        validate_payload(&journal.target, &journal.stage)?;
        wait_for_old_hosts(request)?;
        validate_host_inventory(&journal.target, &[])?;
        progress(event(
            UpdatePhase::Activating,
            0,
            None,
            "Activating verified full payload; old daemon payload retained",
        ));
        if journal.target.kind == InstallationKind::MacBundle {
            journal.backup = Some(
                state_dir(&journal.target.root).join(format!("previous-{}.app", journal.attempt)),
            );
            let backup = journal.backup.as_ref().unwrap();
            if !journal.retained_backups.contains(backup) {
                journal.retained_backups.push(backup.clone());
            }
        }
        if journal.target.kind == InstallationKind::InstalledWindows
            && request.replace_default_daemon
        {
            journal.prior_task = Some(snapshot_owned_task(journal)?);
        }
        validate_journal(journal_path, journal)?;
        begin_activation(journal_path, journal)?;
        activation_started = true;
        match journal.target.kind {
            InstallationKind::MacBundle => {
                if journal.target.root.starts_with("/Volumes") {
                    return Err(err(
                        "Cannot update a mounted disk image; move Compi.app to Applications",
                    ));
                }
                fs::rename(&journal.target.root, journal.backup.as_ref().unwrap())?;
                fs::rename(journal.stage.join("Compi.app"), &journal.target.root)?;
            }
            _ => {
                let destination = version_path(&journal.target.root, &journal.manifest.version)?;
                fs::create_dir_all(destination.parent().unwrap())?;
                if destination.exists() {
                    compare_payload(&journal.stage, &destination)?;
                } else {
                    fs::rename(&journal.stage, &destination)?;
                }
                let selection = Selection {
                    schema: 1,
                    version: journal.manifest.version.clone(),
                    task_version: selected_task_version(journal, request.replace_default_daemon),
                };
                atomic_json(&journal.target.root.join("selection.json"), &selection)?;
                if journal.prior_task.is_some() {
                    install_verified_task(journal)?;
                }
                refresh_root_files(journal);
            }
        }
        journal.phase = JournalStage::Activated;
        atomic_json(journal_path, &journal)?;
        let executable = match journal.target.kind {
            InstallationKind::MacBundle => journal.target.root.join("Contents/MacOS/compi"),
            _ => selected_executable(&journal.target.root, None)?,
        };
        journal.phase = JournalStage::Relaunching;
        atomic_json(journal_path, &journal)?;
        progress(event(
            UpdatePhase::Relaunching,
            0,
            None,
            "Waiting for new build and restored attachments",
        ));
        let expected_version = journal.manifest.version.clone();
        relaunch_hosts(
            request.hosts.iter().map(|host| LaunchSpec {
                executable: &executable,
                version: &expected_version,
                host,
            }),
            request.timeout_seconds,
        )
        .map_err(|failure| {
            new_gui_exit_confirmed = failure.all_exited;
            failure.error
        })?;
        // Ready GUI hosts remain alive; a later journal-write failure must not restore underneath them.
        new_gui_exit_confirmed = false;
        journal.phase = JournalStage::Complete;
        atomic_json(journal_path, &journal)?;
        progress(event(
            UpdatePhase::Complete,
            1,
            Some(1),
            "New client build attached; prior payload retained",
        ));
        Ok(())
    })();
    if let Err(failure) = result {
        if !activation_started {
            journal.phase = JournalStage::Prepared;
        }
        journal.error = Some(failure.to_string());
        atomic_json(journal_path, &journal)?;
        if !activation_started {
            // The parent may still be releasing its hosts. Never create duplicate old views,
            // undo a changed selection, or alter a task that failed ownership validation.
            let safe = wait_for_old_hosts(request)
                .and_then(|_| validate_host_inventory(&journal.target, &[]))
                .and_then(|_| validate_activation_baseline(journal));
            if let Err(blocked) = safe {
                journal.error = Some(format!(
                    "{failure}; activation was not started; prior GUI reopen deferred: {blocked}; {}; handoffs retained; journal: {}",
                    manual_recovery(request),
                    journal_path.display()
                ));
                atomic_json(journal_path, journal)?;
                return Err(err(journal.error.clone().unwrap()));
            }
            return finish_prior_relaunch(journal_path, journal, request, &failure);
        }
        if !new_gui_exit_confirmed {
            return Err(err(format!(
                "{failure}; GUI exit is unconfirmed: close only the worker-spawned GUI hosts before recovery; unsafe rollback was not attempted; journal: {}",
                journal_path.display()
            )));
        }
        if request.replace_default_daemon
            && let Err(blocked) = stop_replacement_daemons(journal)
        {
            journal.error = Some(format!("{failure}; rollback deferred: {blocked}"));
            atomic_json(journal_path, &journal)?;
            return Err(err(format!(
                "{}; journal: {}",
                journal.error.as_deref().unwrap(),
                journal_path.display()
            )));
        }
        progress(event(
            UpdatePhase::RollingBack,
            0,
            None,
            "Restoring prior payload and exact previous GUI views",
        ));
        if let Err(rollback) = rollback(journal_path, journal) {
            return Err(err(format!(
                "{failure}; rollback also failed: {rollback}; journal: {}",
                journal_path.display()
            )));
        }
        return finish_prior_relaunch(journal_path, journal, request, &failure);
    }
    Ok(())
}
fn finish_prior_relaunch(
    journal_path: &Path,
    journal: &mut Journal,
    request: &RelaunchRequest,
    failure: &Error,
) -> Result<()> {
    journal.error = Some(match relaunch_prior(&journal.target, request) {
        Ok(()) => format!("{failure}; previous client build restored and exact views attached"),
        Err(restore) => format!(
            "{failure}; prior payload unchanged or restored, but old GUI attachment recovery failed: {restore}; {}; handoffs retained; journal: {}",
            manual_recovery(request),
            journal_path.display()
        ),
    });
    atomic_json(journal_path, journal)?;
    Err(err(journal.error.clone().unwrap()))
}
struct LaunchSpec<'a> {
    executable: &'a Path,
    version: &'a str,
    host: &'a RelaunchHost,
}
#[derive(Debug)]
struct RelaunchFailure {
    error: Error,
    all_exited: bool,
}
fn relaunch_hosts<'a>(
    hosts: impl Iterator<Item = LaunchSpec<'a>>,
    timeout_seconds: u64,
) -> std::result::Result<(), RelaunchFailure> {
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let mut children = Vec::new();
    let result: Result<()> = (|| {
        for spec in hosts {
            let child = Command::new(spec.executable)
                .args(&spec.host.arguments)
                .arg("--update-receipt")
                .arg(&spec.host.receipt_path)
                .arg("--update-receipt-token")
                .arg(&spec.host.receipt_token)
                .spawn()
                .map_err(|e| err(format!("Client launch failed: {e}")))?;
            children.push((child, spec, false));
        }
        loop {
            for (child, spec, ready) in &mut children {
                if *ready {
                    continue;
                }
                if spec.host.receipt_path.exists() {
                    let receipt: ReadinessReceipt = read_json(&spec.host.receipt_path, 16384)?;
                    if receipt.token != spec.host.receipt_token || receipt.version != spec.version {
                        return Err(err("Client readiness identity mismatch"));
                    }
                    if !receipt.attached || receipt.error.is_some() {
                        return Err(err(format!(
                            "Client attachment failed: {}",
                            receipt.error.unwrap_or_else(|| "not attached".into())
                        )));
                    }
                    if let Some(status) = child.try_wait()? {
                        return Err(err(format!("Client exited after readiness: {status}")));
                    }
                    *ready = true;
                } else if let Some(status) = child.try_wait()? {
                    return Err(err(format!(
                        "Client exited before attachment readiness: {status}"
                    )));
                }
            }
            if children.iter().all(|(_, _, ready)| *ready) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(err("Client did not confirm bounded attachment readiness"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })();
    let Err(failure) = result else {
        return Ok(());
    };
    let mut all_exited = true;
    let mut errors = Vec::new();
    // Only GUI processes created by this helper are targeted; daemon/shell processes are untouched.
    for (child, _, _) in &mut children {
        match child.try_wait() {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(e) => {
                errors.push(e.to_string());
            }
        }
        if let Err(e) = child.kill() {
            errors.push(format!("GUI {} terminate: {e}", child.id()));
        }
        let stop_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < stop_deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Ok(None) => {
                    all_exited = false;
                    errors.push(format!("GUI {} exit deadline exceeded", child.id()));
                    break;
                }
                Err(e) => {
                    all_exited = false;
                    errors.push(format!("GUI {} exit unconfirmed: {e}", child.id()));
                    break;
                }
            }
        }
    }
    let error = if errors.is_empty() {
        failure
    } else {
        err(format!("{failure}; {}", errors.join("; ")))
    };
    Err(RelaunchFailure { error, all_exited })
}
fn relaunch_prior(target: &InstallTarget, request: &RelaunchRequest) -> Result<()> {
    for old in &request.rollback_hosts {
        let expected = match target.kind {
            InstallationKind::MacBundle => target.root.join("Contents/MacOS/compi"),
            _ => version_path(&target.root, &old.version)?.join("compi.exe"),
        };
        if !old.executable.is_file() {
            return Err(err(format!(
                "Prior GUI executable is unavailable: {}",
                old.executable.display()
            )));
        }
        if old.executable.canonicalize()? != expected.canonicalize()? {
            return Err(err(
                "Prior GUI executable does not belong to its declared installation/build",
            ));
        }
    }
    relaunch_hosts(
        request.rollback_hosts.iter().map(|old| LaunchSpec {
            executable: &old.executable,
            version: &old.version,
            host: &old.host,
        }),
        request.timeout_seconds,
    )
    .map_err(|failure| failure.error)
}
fn manual_recovery(request: &RelaunchRequest) -> String {
    request
        .rollback_hosts
        .iter()
        .map(|old| {
            let mut arguments = old.host.arguments.clone();
            arguments.extend([
                "--update-receipt".into(),
                old.host.receipt_path.to_string_lossy().into_owned(),
                "--update-receipt-token".into(),
                old.host.receipt_token.clone(),
            ]);
            format!(
                "manual attach-only restore executable={:?}, arguments={:?}",
                old.executable, arguments
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}
pub(crate) fn verify_package(package: &Path, artifact: &Artifact) -> Result<()> {
    let mut file = File::open(package)?;
    if file.metadata()?.len() != artifact.size {
        return Err(err("Retained package size differs from signed metadata"));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    if format!("{:x}", hash.finalize()) != artifact.sha256.to_ascii_lowercase() {
        return Err(err("Retained package digest differs from signed metadata"));
    }
    Ok(())
}

/// Offline staging uses precisely the same publisher trust and payload checks as network staging.
pub fn prepare_local(
    release: &AvailableRelease,
    target: &InstallTarget,
    package: &Path,
    config: &ReleaseConfig,
    cancel: &Cancellation,
    mut progress: impl FnMut(UpdateEvent),
) -> Result<PreparedUpdate> {
    let envelope = SignedManifest {
        payload: STANDARD.encode(&release.manifest_bytes),
        signature: release.signature.clone(),
    };
    let verified = verify_manifest(&serde_json::to_vec(&envelope)?, config)?;
    let _lock = OperationLock::acquire(&target.root)?;
    recover_locked(target)?;
    ensure_newer_than_selected(target, &verified.manifest.version)?;
    cancel.check()?;
    verify_package(package, &verified.manifest.artifact)?;
    cancel.check()?;
    let attempt = unique_id();
    let state = state_dir(&target.root);
    let owned = state.join(format!("stage-{attempt}"));
    fs::create_dir(&owned)?;
    let result = (|| {
        let destination = owned.join("package.zip");
        fs::copy(package, &destination)?;
        OpenOptions::new()
            .write(true)
            .open(&destination)?
            .sync_all()?;
        cancel.check()?;
        let stage = owned.join("payload");
        archive::extract(&destination, &stage, cancel, &mut progress)?;
        validate_payload(target, &stage)?;
        let journal = Journal::prepared(
            target.clone(),
            attempt,
            verified.manifest.clone(),
            stage.clone(),
            envelope,
        )?;
        let journal_path = state.join("journal.json");
        atomic_json(&journal_path, &journal)?;
        progress(event(
            UpdatePhase::Ready,
            verified.manifest.artifact.size,
            Some(verified.manifest.artifact.size),
            "Verified package staged",
        ));
        Ok(PreparedUpdate {
            journal_path,
            target: target.clone(),
            version: verified.manifest.version,
            stage,
        })
    })();
    if result.is_err() {
        fs::remove_dir_all(owned)?;
    }
    result
}

pub fn new_receipt_token() -> String {
    unique_id()
}
pub fn operation_status(target: &InstallTarget) -> Result<Option<Journal>> {
    let path = state_dir(&target.root).join("journal.json");
    if !path.exists() {
        return Ok(None);
    }
    let journal: Journal = read_json(&path, 1024 * 1024)?;
    validate_journal(&path, &journal)?;
    if journal.target.root != target.root || journal.target.kind != target.kind {
        return Err(err("Operation journal targets another installation"));
    }
    Ok(Some(journal))
}
pub fn pending_update(target: &InstallTarget) -> Result<Option<PreparedUpdate>> {
    let _lock = OperationLock::acquire(&target.root)?;
    recover_locked(target)?;
    let Some(journal) = operation_status(target)? else {
        return Ok(None);
    };
    if journal.phase != JournalStage::Prepared {
        return Ok(None);
    }
    validate_activation_baseline(&journal)?;
    let config = ReleaseConfig::compiled()?;
    let release = verify_manifest(&serde_json::to_vec(&journal.signed)?, &config)?;
    ensure_newer_than_selected(target, &release.manifest.version)?;
    verify_package(
        &journal.stage.parent().unwrap().join("package.zip"),
        &release.manifest.artifact,
    )?;
    Ok(Some(PreparedUpdate {
        journal_path: state_dir(&target.root).join("journal.json"),
        target: target.clone(),
        version: release.manifest.version,
        stage: journal.stage,
    }))
}
fn compare_payload(expected: &Path, existing: &Path) -> Result<()> {
    let left = fs::symlink_metadata(expected)?;
    let right = fs::symlink_metadata(existing)?;
    if left.file_type() != right.file_type() {
        return Err(err(
            "Retained version payload differs from verified package",
        ));
    }
    if left.file_type().is_symlink() {
        if fs::read_link(expected)? != fs::read_link(existing)? {
            return Err(err("Retained version symlink differs"));
        }
        return Ok(());
    }
    if left.is_dir() {
        let mut entries = fs::read_dir(expected)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<std::io::Result<Vec<_>>>()?;
        let mut retained = fs::read_dir(existing)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort();
        retained.sort();
        if entries != retained {
            return Err(err(
                "Retained payload entry set differs from verified package",
            ));
        }
        for entry in entries {
            compare_payload(&expected.join(&entry), &existing.join(entry))?;
        }
        return Ok(());
    }
    if !left.is_file() || left.len() != right.len() {
        return Err(err("Retained payload file differs"));
    }
    let mut a = File::open(expected)?;
    let mut b = File::open(existing)?;
    let mut x = [0u8; 65536];
    let mut y = [0u8; 65536];
    loop {
        let n = a.read(&mut x)?;
        b.read_exact(&mut y[..n])?;
        if x[..n] != y[..n] {
            return Err(err("Retained payload bytes differ from verified package"));
        }
        if n == 0 {
            break;
        }
    }
    Ok(())
}

/// Ordinary launches do not create updater state or require writable product directories.
pub fn guard_client_launch() -> Result<Option<OperationLock>> {
    let target = InstallTarget::detect()?;
    let root = &target.root;
    if !state_dir(root).exists() {
        return Ok(None);
    }
    let lock = OperationLock::acquire(root)?;
    recover_locked(&target)?;
    Ok(Some(lock))
}
fn registry_directory() -> Result<PathBuf> {
    #[cfg(windows)]
    let data = match std::env::var_os("COMPI_DATA_DIR") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(
            std::env::var_os("LOCALAPPDATA").ok_or_else(|| err("LOCALAPPDATA is not set"))?,
        )
        .join("Compi"),
    };
    #[cfg(target_os = "macos")]
    let data = std::env::var_os("COMPI_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join("Library/Application Support/Compi")
        });
    #[cfg(not(any(windows, target_os = "macos")))]
    let data = std::env::var_os("COMPI_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
                })
                .join("compi")
        });
    Ok(data.join("update-gui-hosts-v1"))
}
fn validate_host_inventory(target: &InstallTarget, allowed_process_ids: &[u32]) -> Result<()> {
    validate_host_inventory_at(target, allowed_process_ids, &registry_directory()?)
}
fn validate_host_inventory_at(
    target: &InstallTarget,
    allowed_process_ids: &[u32],
    directory: &Path,
) -> Result<()> {
    #[derive(Deserialize)]
    struct Host {
        pid: u32,
        installation: PathBuf,
    }
    if !directory.exists() {
        return Ok(());
    }
    let mut count = 0;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.path().extension().is_none_or(|e| e != "json") {
            continue;
        }
        count += 1;
        if count > 1024 {
            return Err(err("GUI-host inventory exceeds safe bound"));
        }
        let host: Host = read_json(&entry.path(), 16384)?;
        if host.installation != target.root {
            continue;
        }
        let lock_path = entry.path().with_extension("lock");
        let file = match OpenOptions::new().read(true).write(true).open(&lock_path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        match file.try_lock_exclusive() {
            Ok(()) => {
                let _ = FileExt::unlock(&file);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if !allowed_process_ids.contains(&host.pid) {
                    return Err(err(format!(
                        "Another GUI host ({}) opened during update preparation; prepare all hosts again before installing",
                        host.pid
                    )));
                }
            }
            Err(e) => return Err(err(format!("Could not establish GUI-host ownership: {e}"))),
        }
    }
    Ok(())
}

/// Retry a failed transaction from the retained authenticated package, never from partial files.
/// Relaunch must use fresh one-time GUI handoffs and receipt tokens.
pub fn reinstall_verified(
    target: &InstallTarget,
    cancel: &Cancellation,
    progress: impl FnMut(UpdateEvent),
) -> Result<PreparedUpdate> {
    let journal = operation_status(target)?
        .ok_or_else(|| err("No retained update transaction is available"))?;
    if !matches!(
        journal.phase,
        JournalStage::RolledBack | JournalStage::Prepared
    ) {
        return Err(err(
            "Recover the interrupted transaction before retrying installation",
        ));
    }
    let config = ReleaseConfig::compiled()?;
    let release = verify_manifest(&serde_json::to_vec(&journal.signed)?, &config)?;
    prepare_local(
        &release,
        target,
        &journal.stage.parent().unwrap().join("package.zip"),
        &config,
        cancel,
        progress,
    )
}
pub fn relaunch_request(target: &InstallTarget) -> Result<Option<RelaunchRequest>> {
    let Some(journal) = operation_status(target)? else {
        return Ok(None);
    };
    let path = state_dir(&target.root).join(format!("request-{}.json", journal.attempt));
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(read_json(&path, 1024 * 1024)?))
}

pub fn operation_journal_path(target: &InstallTarget) -> PathBuf {
    state_dir(&target.root).join("journal.json")
}

/// Updater files that only matter while an activation runs or until the process that asked
/// for them reads them.
const TRANSIENT_PREFIXES: [&str; 9] = [
    "request-",
    "handoff-",
    "rollback-handoff-",
    "review-",
    "receipt-",
    "rollback-receipt-",
    "cli-install-",
    "worker-",
    ".write-",
];
#[derive(Default)]
struct Removed {
    count: u32,
    bytes: u64,
}
/// Deletes what no transaction needs: every `versions\<v>` except the selected client and
/// task generations and folders still in use, staging other than the pending or retryable
/// package, retained macOS bundles nothing runs from, rollback leftovers, and transient
/// files older than this process. Does nothing while another operation holds the lock or a
/// journal is mid-activation. Quiet: outcomes go to the update log only.
pub fn prune(target: &InstallTarget) -> Result<()> {
    let Ok(_lock) = OperationLock::acquire(&target.root) else {
        return Ok(());
    };
    prune_locked(
        target,
        process_started().and_then(|started| started.checked_sub(Duration::from_secs(60))),
        &directory_in_use,
    )
}
/// `transient_cutoff`: transient files last touched before it are stale; `None` keeps them.
fn prune_locked(
    target: &InstallTarget,
    transient_cutoff: Option<SystemTime>,
    in_use: &dyn Fn(&Path) -> bool,
) -> Result<()> {
    let root = &target.root;
    let state = state_dir(root);
    let journal =
        match validate_no_symlink_components(&state).and_then(|_| operation_status(target)) {
            Ok(journal) => journal,
            Err(error) => {
                update_log(root, &format!("Cleanup skipped: {error}"));
                return Ok(());
            }
        };
    if journal.as_ref().is_some_and(|journal| {
        matches!(
            journal.phase,
            JournalStage::Activating | JournalStage::Activated | JournalStage::Relaunching
        )
    }) {
        return Ok(());
    }
    let mut removed = Removed::default();
    if target.kind != InstallationKind::MacBundle {
        // The root helper applies updates, so a helper from before root refresh leaves the
        // root launcher and helper behind; bring them up to the completed update's payload.
        if let Some(journal) = &journal
            && journal.phase == JournalStage::Complete
            && read_selection(root)
                .ok()
                .flatten()
                .is_some_and(|selection| selection.version == journal.manifest.version)
        {
            refresh_root_files(journal);
        }
        prune_versions(target, &state, in_use, &mut removed);
    }
    prune_state(
        target,
        journal.as_ref(),
        transient_cutoff,
        in_use,
        &mut removed,
    )?;
    if target.kind == InstallationKind::MacBundle
        && let Some(mut journal) = journal
    {
        // Retained bundles stay attributed for daemon lifecycle until they are gone.
        let retained = journal.retained_backups.len();
        journal
            .retained_backups
            .retain(|backup| fs::symlink_metadata(backup).is_ok());
        let backup = journal
            .backup
            .take_if(|backup| fs::symlink_metadata(backup).is_err());
        if retained != journal.retained_backups.len() || backup.is_some() {
            atomic_json(&operation_journal_path(target), &journal)?;
        }
    }
    if removed.count > 0 {
        update_log(
            root,
            &format!(
                "Cleanup removed {} item(s), {} MB",
                removed.count,
                removed.bytes / 1_000_000
            ),
        );
    }
    Ok(())
}
fn prune_versions(
    target: &InstallTarget,
    state: &Path,
    in_use: &dyn Fn(&Path) -> bool,
    removed: &mut Removed,
) {
    let root = &target.root;
    let selection = match read_selection(root) {
        Ok(Some(selection)) => selection,
        Ok(None) => return,
        Err(error) => {
            update_log(root, &format!("Kept every version: {error}"));
            return;
        }
    };
    let Ok(entries) = fs::read_dir(root.join("versions")) else {
        return;
    };
    #[cfg(windows)]
    let registered = (target.kind == InstallationKind::InstalledWindows)
        .then(registered_msi_version)
        .flatten();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Some(version) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if version_path(root, &version).is_err()
            || version == selection.version
            || version == selection.task_version
        {
            continue;
        }
        if retire(root, state, &entry.path(), in_use, removed) {
            // The registered product keeps its key; uninstall removes it.
            #[cfg(windows)]
            if registered
                .as_deref()
                .is_some_and(|product| product != version)
            {
                forget_payload_key(&version);
            }
        }
    }
}
fn prune_state(
    target: &InstallTarget,
    journal: Option<&Journal>,
    transient_cutoff: Option<SystemTime>,
    in_use: &dyn Fn(&Path) -> bool,
    removed: &mut Removed,
) -> Result<()> {
    let root = &target.root;
    let state = state_dir(root);
    let mut kept_stage = None;
    let mut protected = HashSet::<OsString>::new();
    if let Some(journal) = journal {
        protected.insert(format!("worker-{}.log", journal.attempt).into());
        // A pending package installs from its stage; a rolled-back one retries from it.
        // Their requests name the handoffs a manual restore of the old views needs.
        if matches!(
            journal.phase,
            JournalStage::Prepared | JournalStage::RolledBack
        ) {
            kept_stage = Some(format!("stage-{}", journal.attempt));
            let request = format!("request-{}.json", journal.attempt);
            if let Ok(request) = read_json::<RelaunchRequest>(&state.join(&request), 1024 * 1024) {
                for host in request
                    .hosts
                    .iter()
                    .chain(request.rollback_hosts.iter().map(|old| &old.host))
                {
                    for path in host
                        .arguments
                        .iter()
                        .map(Path::new)
                        .chain([host.receipt_path.as_path()])
                    {
                        protected.extend(path.file_name().map(OsString::from));
                    }
                }
            }
            protected.insert(request.into());
            for suffix in ["", ".exe"] {
                protected.insert(format!("worker-{}{suffix}", journal.attempt).into());
            }
        }
    }
    for entry in fs::read_dir(&state)? {
        let entry = entry?;
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            if name.starts_with("prune-")
                || (name.starts_with("stage-") && kept_stage.as_deref() != Some(name))
            {
                remove_entry(root, &path, true, removed);
            } else if target.kind == InstallationKind::MacBundle
                && (name.starts_with("previous-") || name.starts_with("failed-"))
                && name.ends_with(".app")
            {
                retire(root, &state, &path, in_use, removed);
            }
        } else if kind.is_file() {
            let leftover = name.starts_with("root-")
                || (name.starts_with("task-rollback-") && name.ends_with(".xml"));
            let stale = TRANSIENT_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
                && !protected.contains(&file_name)
                && transient_cutoff.is_some_and(|cutoff| {
                    entry
                        .metadata()
                        .ok()
                        .and_then(|metadata| last_touched(&metadata))
                        .is_some_and(|touched| touched < cutoff)
                });
            if leftover || stale {
                remove_entry(root, &path, false, removed);
            }
        }
    }
    Ok(())
}
/// The later of creation and modification: a copy keeps its source's modification time.
fn last_touched(metadata: &fs::Metadata) -> Option<SystemTime> {
    [metadata.created().ok(), metadata.modified().ok()]
        .into_iter()
        .flatten()
        .max()
}
fn remove_entry(root: &Path, path: &Path, directory: bool, removed: &mut Removed) {
    let bytes = tree_size(path);
    let result = if directory {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    match result {
        Ok(()) => {
            removed.count += 1;
            removed.bytes += bytes;
        }
        Err(error) => update_log(
            root,
            &format!("Could not remove {}: {error}", path.display()),
        ),
    }
}
/// Moves a payload folder into the state directory, then deletes it, so nothing can start
/// from a half-deleted folder. Returns whether it left its place.
fn retire(
    root: &Path,
    state: &Path,
    path: &Path,
    in_use: &dyn Fn(&Path) -> bool,
    removed: &mut Removed,
) -> bool {
    if in_use(path) {
        update_log(root, &format!("Kept {}: in use", path.display()));
        return false;
    }
    let tombstone = state.join(format!("prune-{}", unique_id()));
    if let Err(error) = fs::rename(path, &tombstone) {
        update_log(root, &format!("Kept {}: {error}", path.display()));
        return false;
    }
    remove_entry(root, &tombstone, true, removed);
    true
}
fn tree_size(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() {
        return metadata.len();
    }
    fs::read_dir(path).map_or(0, |entries| {
        entries
            .flatten()
            .map(|entry| tree_size(&entry.path()))
            .sum()
    })
}
/// Windows: a running image or loaded DLL refuses an exclusive write open, as does any file
/// another process holds without full sharing. Checked per file because a folder rename
/// alone does not prove nothing runs below it.
#[cfg(windows)]
fn directory_in_use(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    fn walk(path: &Path) -> std::io::Result<bool> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let busy = if entry.file_type()?.is_dir() {
                walk(&entry.path())?
            } else {
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .share_mode(0)
                    .open(entry.path())
                    .is_err()
            };
            if busy {
                return Ok(true);
            }
        }
        Ok(false)
    }
    walk(path).unwrap_or(true)
}
/// macOS: renamed bundles keep their running images, so ask lsof for any open file below.
/// lsof exits 1 with no output when nothing is open; anything else keeps the bundle.
#[cfg(target_os = "macos")]
fn directory_in_use(path: &Path) -> bool {
    Command::new("/usr/sbin/lsof")
        .arg("-t")
        .arg("+D")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_or(true, |output| {
            output.status.code() != Some(1) || !output.stdout.is_empty()
        })
}
#[cfg(not(any(windows, target_os = "macos")))]
fn directory_in_use(_: &Path) -> bool {
    false
}
#[cfg(windows)]
fn process_started() -> Option<SystemTime> {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentProcess, GetProcessTimes},
    };
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: the pseudo-handle needs no closing; all four outputs are valid FILETIMEs.
    if unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return None;
    }
    // 100 ns ticks since 1601-01-01.
    let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    UNIX_EPOCH.checked_add(Duration::from_nanos(
        ticks.checked_sub(116_444_736_000_000_000)? * 100,
    ))
}
#[cfg(not(windows))]
fn process_started() -> Option<SystemTime> {
    let output = Command::new("ps")
        .args(["-o", "etime=", "-p", &std::process::id().to_string()])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let elapsed = parse_elapsed(String::from_utf8(output.stdout).ok()?.trim())?;
    SystemTime::now().checked_sub(Duration::from_secs(elapsed))
}
/// `ps` elapsed time: `[[dd-]hh:]mm:ss`.
#[cfg(not(windows))]
fn parse_elapsed(text: &str) -> Option<u64> {
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, text),
    };
    let mut seconds = 0u64;
    for part in clock.split(':') {
        seconds = seconds * 60 + part.parse::<u64>().ok()?;
    }
    Some(days * 86_400 + seconds)
}
/// Version of the product Windows Installer has registered, from the entry its MSI writes.
#[cfg(windows)]
fn registered_msi_version() -> Option<String> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Compi");
    let value = wide("DisplayVersion");
    let mut buffer = [0u16; 64];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    // SAFETY: NUL-terminated names; `size` is the buffer's byte length.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let length = buffer[..size as usize / 2]
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(size as usize / 2);
    String::from_utf16(&buffer[..length]).ok()
}
/// Deletes the key path of a removed payload's permanent MSI components. Only for versions
/// whose product is no longer registered; a later package of that version installs it again.
#[cfg(windows)]
fn forget_payload_key(version: &str) {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteTreeW};
    let key = wide(&format!("Software\\Compi\\Payloads\\{version}"));
    // SAFETY: a NUL-terminated key name; a missing key is reported, not created.
    unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, key.as_ptr()) };
}
#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    /// macOS temp dirs sit under the /var -> /private/var symlink, which journal
    /// ownership validation rejects by design; tests use the resolved path.
    struct TempRoot {
        _directory: tempfile::TempDir,
        path: PathBuf,
    }
    impl TempRoot {
        fn path(&self) -> &Path {
            &self.path
        }
    }
    fn tempdir() -> TempRoot {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let path = directory.path().canonicalize().unwrap();
        #[cfg(not(unix))]
        let path = directory.path().to_path_buf();
        TempRoot {
            _directory: directory,
            path,
        }
    }
    fn journal(target: InstallTarget, previous: Option<Selection>, phase: JournalStage) -> Journal {
        let attempt = unique_id();
        let stage = state_dir(&target.root)
            .join(format!("stage-{attempt}"))
            .join("payload");
        Journal {
            schema: 1,
            attempt,
            target,
            manifest: ReleaseManifest {
                schema: 1,
                product: "compi".into(),
                version: "2.0.0".into(),
                platform: "windows-x86_64".into(),
                minimum_os: "10".into(),
                release_notes: String::new(),
                daemon_protocol: 14,
                qualified_daemon_versions: vec![],
                minimum_persistence: SUPPORTED_PERSISTENCE_VERSION,
                artifact: Artifact {
                    url:
                        "https://github.com/cloudboy-jh/compi/releases/download/v2.0.0/package.zip"
                            .into(),
                    size: 1,
                    sha256: "00".repeat(32),
                },
                setup: None,
            },
            signed: SignedManifest {
                payload: String::new(),
                signature: String::new(),
            },
            stage,
            prior_selection: previous,
            phase,
            backup: None,
            retained_backups: Vec::new(),
            prior_task: None,
            error: None,
        }
    }
    #[test]
    fn stale_deferred_stage_cannot_undo_an_intervening_msi_selection() {
        let directory = tempdir();
        let target = InstallTarget {
            kind: InstallationKind::InstalledWindows,
            root: directory.path().join("Compi"),
        };
        let previous = Selection {
            schema: 1,
            version: "1.0.0".into(),
            task_version: "1.0.0".into(),
        };
        fs::create_dir_all(&target.root).unwrap();
        restore_selection(&target.root, Some(&previous)).unwrap();
        let staged = journal(target.clone(), Some(previous), JournalStage::Prepared);
        let path = state_dir(&target.root).join("journal.json");
        atomic_json(&path, &staged).unwrap();
        let intervening = Selection {
            schema: 1,
            version: "1.5.0".into(),
            task_version: "1.5.0".into(),
        };
        restore_selection(&target.root, Some(&intervening)).unwrap();
        let host = |name: &str| RelaunchHost {
            arguments: vec!["--update-restore".into(), format!("{name}-handoff.json")],
            receipt_path: directory.path().join(format!("{name}-receipt.json")),
            receipt_token: new_receipt_token(),
        };
        let request = RelaunchRequest {
            old_process_ids: Vec::new(),
            hosts: vec![host("new")],
            rollback_hosts: vec![RollbackHost {
                executable: target.root.join("versions/1.0.0/compi.exe"),
                version: "1.0.0".into(),
                host: host("prior"),
            }],
            replace_default_daemon: true,
            timeout_seconds: 1,
        };
        let prepared = PreparedUpdate {
            journal_path: path,
            target: target.clone(),
            version: staged.manifest.version,
            stage: staged.stage,
        };
        assert!(
            launch_worker(&prepared, &request)
                .unwrap_err()
                .to_string()
                .contains("restage")
        );
        assert_eq!(read_selection(&target.root).unwrap(), Some(intervening));
        assert_eq!(
            operation_status(&target).unwrap().unwrap().phase,
            JournalStage::Prepared
        );
    }

    #[test]
    fn windows_task_generation_changes_only_for_default_replacement() {
        let target = InstallTarget {
            kind: InstallationKind::InstalledWindows,
            root: PathBuf::from("installation"),
        };
        let previous = Selection {
            schema: 1,
            version: "1.0.0".into(),
            task_version: "0.9.0".into(),
        };
        let staged = journal(target, Some(previous), JournalStage::Prepared);
        assert_eq!(selected_task_version(&staged, false), "0.9.0");
        assert_eq!(selected_task_version(&staged, true), "2.0.0");
    }
    #[cfg(windows)]
    fn run_task_guard(
        root: &str,
        action: &str,
        foreign_account: bool,
        include_new: bool,
    ) -> Result<()> {
        let target = InstallTarget {
            kind: InstallationKind::InstalledWindows,
            root: PathBuf::from(root),
        };
        let previous = Selection {
            schema: 1,
            version: "1.0.0".into(),
            task_version: "1.0.0".into(),
        };
        let journal = journal(target, Some(previous), JournalStage::Prepared);
        let owner = if foreign_account {
            "'S-1-0-0'"
        } else {
            "([Security.Principal.WindowsIdentity]::GetCurrent()).User.Value"
        };
        // Shadow the read cmdlet only. Execute the production guard in native PowerShell
        // without consulting, registering, deleting, or stopping any real scheduled task.
        let script = format!(
            r#"
function Get-ScheduledTask {{
    param($ErrorAction)
    [pscustomobject]@{{
        TaskName=('Compi Daemon-'+([Security.Principal.WindowsIdentity]::GetCurrent()).User.Value)
        TaskPath='\'
        Principal=[pscustomobject]@{{UserId={owner}}}
        Actions=@([pscustomobject]@{{Execute='{action}'}})
    }}
}}
{guard}
$allowed=@($allowed | ForEach-Object {{ Normalize-OwnedPath ('"'+$_+'"') }})
if ($t) {{ Assert-Owner $t.Principal.UserId @($t.Actions | ForEach-Object {{ $_.Execute }}) }}
"#,
            action = action.replace('\'', "''"),
            guard = task_guard_script(&journal, include_new)?,
        );
        task_powershell(&script).map(|_| ())
    }
    #[cfg(windows)]
    #[test]
    fn powershell_task_guard_normalizes_both_drive_and_unc_paths() {
        for (normal, extended) in [
            (r"C:\Compi", r"\\?\C:\Compi"),
            (r"\\server\share\Compi", r"\\?\UNC\server\share\Compi"),
            (
                r"\\server.example\share\Compi",
                r"\\?\UNC\server.example\share\Compi",
            ),
        ] {
            for root in [normal, extended] {
                for action_root in [normal, extended] {
                    let action = format!(r"{action_root}\versions\1.0.0\compi-daemon.exe");
                    run_task_guard(root, &action, false, false).unwrap();
                    run_task_guard(
                        root,
                        &format!("\"{}\"", action.to_uppercase()),
                        false,
                        false,
                    )
                    .unwrap();
                }
            }
        }
    }
    #[cfg(windows)]
    #[test]
    fn powershell_task_guard_rejects_foreign_root_generation_account_and_device_paths() {
        for root in [r"C:\Compi", r"\\?\C:\Compi"] {
            for action in [
                r"C:\OtherCompi\versions\1.0.0\compi-daemon.exe",
                r"C:\Compi-foreign\versions\1.0.0\compi-daemon.exe",
                r"\\other\share\Compi\versions\1.0.0\compi-daemon.exe",
                r"C:\Compi\versions\0.9.0\compi-daemon.exe",
                r"C:\Compi\versions\2.0.0\compi-daemon.exe",
                r"versions\1.0.0\compi-daemon.exe",
                r"\\.\C:\Compi\versions\1.0.0\compi-daemon.exe",
                r"\\?\C:\Compi.\versions\1.0.0\compi-daemon.exe",
                r"\\?\C:\Compi \versions\1.0.0\compi-daemon.exe",
                r"\\?\C:\Other\..\Compi\versions\1.0.0\compi-daemon.exe",
            ] {
                assert!(
                    run_task_guard(root, action, false, false).is_err(),
                    "{action}"
                );
            }
            let old = r"C:\Compi\versions\1.0.0\compi-daemon.exe";
            assert!(run_task_guard(root, old, true, false).is_err());
            run_task_guard(
                root,
                r"C:\Compi\versions\2.0.0\compi-daemon.exe",
                false,
                true,
            )
            .unwrap();
            assert!(
                run_task_guard(
                    root,
                    r"C:\Other\versions\2.0.0\compi-daemon.exe",
                    false,
                    true
                )
                .is_err()
            );
        }
        for action in [
            r"\\other\share\Compi\versions\1.0.0\compi-daemon.exe",
            r"\\server\other\Compi\versions\1.0.0\compi-daemon.exe",
            r"\\server\share\Other\versions\1.0.0\compi-daemon.exe",
            r"\\?\UNC\server\share\Compi\versions\0.9.0\compi-daemon.exe",
        ] {
            assert!(run_task_guard(r"\\?\UNC\server\share\Compi", action, false, false).is_err());
        }
    }

    #[test]
    fn later_mac_staging_preserves_older_and_latest_retained_bundles() {
        let directory = tempdir();
        let target = InstallTarget {
            kind: InstallationKind::MacBundle,
            root: directory.path().join("Compi.app"),
        };
        fs::create_dir(&target.root).unwrap();
        let mut completed = journal(target.clone(), None, JournalStage::Complete);
        let older = state_dir(&target.root).join(format!("previous-{}.app", "a".repeat(32)));
        let latest = state_dir(&target.root).join(format!("previous-{}.app", completed.attempt));
        for backup in [&older, &latest] {
            fs::create_dir_all(backup).unwrap();
            fs::write(backup.join("daemon"), b"still runnable").unwrap();
        }
        completed.retained_backups.push(older.clone());
        completed.backup = Some(latest.clone());
        let path = state_dir(&target.root).join("journal.json");
        atomic_json(&path, &completed).unwrap();
        let attempt = unique_id();
        let next = Journal::prepared(
            target.clone(),
            attempt.clone(),
            completed.manifest,
            state_dir(&target.root).join(format!("stage-{attempt}/payload")),
            completed.signed,
        )
        .unwrap();
        atomic_json(&path, &next).unwrap();
        assert_eq!(
            operation_status(&target).unwrap().unwrap().retained_backups,
            vec![older.clone(), latest.clone()]
        );
        assert_eq!(fs::read(older.join("daemon")).unwrap(), b"still runnable");
        assert_eq!(fs::read(latest.join("daemon")).unwrap(), b"still runnable");
    }

    #[test]
    fn malformed_retained_attribution_blocks_recovery_before_bundle_mutation() {
        let directory = tempdir();
        let target = InstallTarget {
            kind: InstallationKind::MacBundle,
            root: directory.path().join("Compi.app"),
        };
        fs::create_dir(&target.root).unwrap();
        fs::write(target.root.join("client"), b"selected").unwrap();
        let mut interrupted = journal(target.clone(), None, JournalStage::Activated);
        let backup = state_dir(&target.root).join(format!("previous-{}.app", interrupted.attempt));
        fs::create_dir_all(&backup).unwrap();
        fs::write(backup.join("client"), b"previous").unwrap();
        interrupted.backup = Some(backup.clone());
        interrupted.retained_backups = vec![backup.clone(), directory.path().join("foreign.app")];
        let path = state_dir(&target.root).join("journal.json");
        atomic_json(&path, &interrupted).unwrap();
        assert!(recover(&target).is_err());
        assert_eq!(fs::read(target.root.join("client")).unwrap(), b"selected");
        assert_eq!(fs::read(backup.join("client")).unwrap(), b"previous");
        assert!(
            Journal::prepared(
                target.clone(),
                unique_id(),
                interrupted.manifest,
                interrupted.stage,
                interrupted.signed
            )
            .is_err()
        );
    }

    #[test]
    fn interrupted_recovery_refuses_a_live_host_then_recovers_after_exit() {
        let directory = tempdir();
        let target = InstallTarget {
            kind: InstallationKind::PortableWindows,
            root: directory.path().join("Compi"),
        };
        fs::create_dir(&target.root).unwrap();
        let previous = Selection {
            schema: 1,
            version: "1.0.0".into(),
            task_version: "1.0.0".into(),
        };
        let selected = Selection {
            schema: 1,
            version: "2.0.0".into(),
            task_version: "2.0.0".into(),
        };
        restore_selection(&target.root, Some(&selected)).unwrap();
        let mut interrupted = journal(
            target.clone(),
            Some(previous.clone()),
            JournalStage::Relaunching,
        );
        let path = state_dir(&target.root).join("journal.json");
        atomic_json(&path, &interrupted).unwrap();
        let inventory = directory.path().join("hosts");
        fs::create_dir(&inventory).unwrap();
        atomic_json(
            &inventory.join("host.json"),
            &serde_json::json!({
                "pid": 42, "installation": target.root
            }),
        )
        .unwrap();
        let host_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(inventory.join("host.lock"))
            .unwrap();
        host_lock.lock_exclusive().unwrap();
        let _operation = OperationLock::acquire(&target.root).unwrap();
        assert!(recover_interrupted(&path, &mut interrupted, &inventory, |_| Ok(())).is_err());
        assert_eq!(
            read_selection(&target.root).unwrap(),
            Some(selected.clone())
        );
        assert_eq!(
            operation_status(&target).unwrap().unwrap().phase,
            JournalStage::Relaunching
        );
        drop(host_lock);
        // A replacement daemon that cannot be stopped must block rollback, not race it.
        assert!(
            recover_interrupted(&path, &mut interrupted, &inventory, |_| Err(err(
                "live replacement"
            )))
            .is_err()
        );
        assert_eq!(read_selection(&target.root).unwrap(), Some(selected));
        assert_eq!(
            operation_status(&target).unwrap().unwrap().phase,
            JournalStage::Relaunching
        );
        recover_interrupted(&path, &mut interrupted, &inventory, |_| Ok(())).unwrap();
        assert_eq!(read_selection(&target.root).unwrap(), Some(previous));
        assert_eq!(
            operation_status(&target).unwrap().unwrap().phase,
            JournalStage::RolledBack
        );
    }
    #[test]
    fn crash_recovery_restores_prior_client_and_task_generation() {
        let directory = tempdir();
        let target = InstallTarget {
            kind: InstallationKind::PortableWindows,
            root: directory.path().join("Compi"),
        };
        fs::create_dir(&target.root).unwrap();
        let previous = Selection {
            schema: 1,
            version: "1.0.0".into(),
            task_version: "0.9.0".into(),
        };
        let changed = Selection {
            schema: 1,
            version: "2.0.0".into(),
            task_version: "0.9.0".into(),
        };
        restore_selection(&target.root, Some(&changed)).unwrap();
        let journal = journal(
            target.clone(),
            Some(previous.clone()),
            JournalStage::Relaunching,
        );
        atomic_json(&state_dir(&target.root).join("journal.json"), &journal).unwrap();
        preserved_daemon_request(&target, &journal);
        recover(&target).unwrap();
        assert_eq!(read_selection(&target.root).unwrap(), Some(previous));
        assert_eq!(
            operation_status(&target).unwrap().unwrap().phase,
            JournalStage::RolledBack
        );
        recover(&target).unwrap();
        assert_eq!(
            operation_status(&target).unwrap().unwrap().phase,
            JournalStage::RolledBack
        );
    }
    /// Crash tests model a preserved compatible daemon; replacement stopping needs a real build.
    fn preserved_daemon_request(target: &InstallTarget, journal: &Journal) {
        let request = RelaunchRequest {
            old_process_ids: Vec::new(),
            hosts: Vec::new(),
            rollback_hosts: Vec::new(),
            replace_default_daemon: false,
            timeout_seconds: 1,
        };
        let path = state_dir(&target.root).join(format!("request-{}.json", journal.attempt));
        atomic_json(&path, &request).unwrap();
    }
    #[test]
    fn mac_crash_between_bundle_renames_recovers_old_bundle_and_preserves_data() {
        let directory = tempdir();
        let target = InstallTarget {
            kind: InstallationKind::MacBundle,
            root: directory.path().join("Compi.app"),
        };
        let data = directory.path().join("user-data");
        fs::write(&data, "preserved").unwrap();
        let mut journal = journal(target.clone(), None, JournalStage::Activating);
        let backup = state_dir(&target.root).join(format!("previous-{}.app", journal.attempt));
        fs::create_dir_all(&backup).unwrap();
        fs::write(backup.join("old-client"), "old runnable payload").unwrap();
        journal.backup = Some(backup);
        atomic_json(&state_dir(&target.root).join("journal.json"), &journal).unwrap();
        preserved_daemon_request(&target, &journal);
        recover(&target).unwrap();
        assert_eq!(
            fs::read_to_string(target.root.join("old-client")).unwrap(),
            "old runnable payload"
        );
        assert_eq!(fs::read_to_string(data).unwrap(), "preserved");
    }
    #[test]
    fn relaunch_missing_executable_reports_failure_without_success_receipt() {
        let directory = tempdir();
        let receipt = directory.path().join("ready.json");
        let host = RelaunchHost {
            arguments: vec!["--update-restore".into(), "owned-handoff.json".into()],
            receipt_path: receipt.clone(),
            receipt_token: new_receipt_token(),
        };
        let missing = directory.path().join("missing-client.exe");
        let error = relaunch_hosts(
            std::iter::once(LaunchSpec {
                executable: &missing,
                version: "2.0.0",
                host: &host,
            }),
            1,
        )
        .unwrap_err();
        assert!(error.error.to_string().contains("launch failed"));
        assert!(error.all_exited);
        assert!(!receipt.exists());
    }
    fn early_failure_fixture(directory: &Path) -> (PathBuf, Journal, RelaunchRequest) {
        let target = InstallTarget {
            kind: InstallationKind::PortableWindows,
            root: directory.join("Compi"),
        };
        let previous = Selection {
            schema: 1,
            version: "1.0.0".into(),
            task_version: "0.9.0".into(),
        };
        fs::create_dir_all(&target.root).unwrap();
        restore_selection(&target.root, Some(&previous)).unwrap();
        let journal = journal(target.clone(), Some(previous), JournalStage::Prepared);
        let path = state_dir(&target.root).join("journal.json");
        atomic_json(&path, &journal).unwrap();
        let host = |name: &str| RelaunchHost {
            arguments: vec![
                "--update-restore".into(),
                directory
                    .join(format!("{name}-handoff.json"))
                    .to_string_lossy()
                    .into_owned(),
            ],
            receipt_path: directory.join(format!("{name}-receipt.json")),
            receipt_token: new_receipt_token(),
        };
        let request = RelaunchRequest {
            old_process_ids: Vec::new(),
            hosts: vec![host("new")],
            rollback_hosts: vec![RollbackHost {
                executable: target.root.join("versions/1.0.0/compi.exe"),
                version: "1.0.0".into(),
                host: host("prior"),
            }],
            replace_default_daemon: true,
            timeout_seconds: 1,
        };
        (path, journal, request)
    }
    #[test]
    fn authenticated_pre_activation_failure_persists_error_and_prior_reopen_failure() {
        let directory = tempdir();
        let (path, mut journal, request) = early_failure_fixture(directory.path());
        let previous = journal.prior_selection.clone();
        let _operation = OperationLock::acquire(&journal.target.root).unwrap();
        // Enter immediately after publisher authentication; the retained package is missing.
        let failure = apply_authenticated(&path, &request, &mut journal, |_| {}).unwrap_err();
        let saved: Journal = read_json(&path, 1024 * 1024).unwrap();
        assert_eq!(saved.phase, JournalStage::Prepared);
        assert_eq!(saved.error.as_deref(), Some(failure.to_string().as_str()));
        assert!(
            failure
                .to_string()
                .contains("Prior GUI executable is unavailable")
        );
        assert!(failure.to_string().contains("handoffs retained"));
        assert_eq!(read_selection(&journal.target.root).unwrap(), previous);
        assert!(saved.prior_task.is_none());
        assert!(!request.hosts[0].receipt_path.exists());
    }
    #[test]
    fn accepted_activation_retry_clears_previous_failure_before_relaunch() {
        let directory = tempdir();
        let (path, mut journal, request) = early_failure_fixture(directory.path());
        let _operation = OperationLock::acquire(&journal.target.root).unwrap();
        apply_authenticated(&path, &request, &mut journal, |_| {}).unwrap_err();
        assert!(
            read_json::<Journal>(&path, 1024 * 1024)
                .unwrap()
                .error
                .is_some()
        );
        // Exercise the actual accepted-retry commit separately from executable launch.
        begin_activation(&path, &mut journal).unwrap();
        let accepted: Journal = read_json(&path, 1024 * 1024).unwrap();
        assert_eq!(accepted.phase, JournalStage::Activating);
        assert!(accepted.error.is_none());
        assert!(journal.error.is_none());
    }
    #[test]
    fn authenticated_pre_activation_failure_does_not_duplicate_a_live_old_gui() {
        let directory = tempdir();
        let (path, mut journal, mut request) = early_failure_fixture(directory.path());
        request.old_process_ids.push(std::process::id());
        let previous = journal.prior_selection.clone();
        let _operation = OperationLock::acquire(&journal.target.root).unwrap();
        let failure = apply_authenticated(&path, &request, &mut journal, |_| {}).unwrap_err();
        assert!(failure.to_string().contains("prior GUI reopen deferred"));
        assert!(failure.to_string().contains("Old GUI hosts did not exit"));
        let saved: Journal = read_json(&path, 1024 * 1024).unwrap();
        assert_eq!(saved.phase, JournalStage::Prepared);
        assert_eq!(saved.error.as_deref(), Some(failure.to_string().as_str()));
        assert_eq!(read_selection(&journal.target.root).unwrap(), previous);
        assert!(!request.rollback_hosts[0].host.receipt_path.exists());
    }
    #[test]
    fn authenticated_stale_baseline_failure_never_restores_an_obsolete_selection() {
        let directory = tempdir();
        let (path, mut journal, request) = early_failure_fixture(directory.path());
        let intervening = Selection {
            schema: 1,
            version: "1.5.0".into(),
            task_version: "1.5.0".into(),
        };
        restore_selection(&journal.target.root, Some(&intervening)).unwrap();
        let _operation = OperationLock::acquire(&journal.target.root).unwrap();
        let failure = apply_authenticated(&path, &request, &mut journal, |_| {}).unwrap_err();
        assert!(failure.to_string().contains("restage"));
        assert!(failure.to_string().contains("prior GUI reopen deferred"));
        assert_eq!(
            read_selection(&journal.target.root).unwrap(),
            Some(intervening)
        );
        let saved: Journal = read_json(&path, 1024 * 1024).unwrap();
        assert_eq!(saved.error.as_deref(), Some(failure.to_string().as_str()));
        assert_eq!(saved.phase, JournalStage::Prepared);
    }
    #[cfg(unix)]
    #[test]
    fn authenticated_pre_activation_failure_reopens_exact_prior_views_and_waits_for_readiness() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempdir();
        let (path, mut journal, mut request) = early_failure_fixture(directory.path());
        let executable = &request.rollback_hosts[0].executable;
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(
            executable,
            r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
    case "$1" in
        --update-restore) restore="$2"; shift 2 ;;
        --view-marker) marker="$2"; shift 2 ;;
        --update-receipt) receipt="$2"; shift 2 ;;
        --update-receipt-token) token="$2"; shift 2 ;;
        *) exit 2 ;;
    esac
done
cat "$restore" > "$marker"
printf '{"token":"%s","version":"1.0.0","attached":true,"error":null}\n' "$token" > "$receipt"
count=0
while [ ! -e "$marker.stop" ] && [ "$count" -lt 200 ]; do
    sleep 0.05
    count=$((count+1))
done
"#,
        )
        .unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o700)).unwrap();
        struct StopHosts(Vec<PathBuf>);
        impl Drop for StopHosts {
            fn drop(&mut self) {
                for path in &self.0 {
                    let _ = fs::write(path, "");
                }
            }
        }
        let mut stop = StopHosts(Vec::new());
        let mut old = Vec::new();
        for index in 0..2 {
            let handoff = directory.path().join(format!("view-{index}.json"));
            let marker = directory.path().join(format!("restored-{index}.json"));
            fs::write(&handoff, format!("exact view {index}")).unwrap();
            stop.0.push(marker.with_extension("json.stop"));
            let mut host = request.rollback_hosts[0].clone();
            host.host.arguments = vec![
                "--update-restore".into(),
                handoff.to_string_lossy().into_owned(),
                "--view-marker".into(),
                marker.to_string_lossy().into_owned(),
            ];
            host.host.receipt_path = directory.path().join(format!("prior-{index}-ready.json"));
            host.host.receipt_token = new_receipt_token();
            old.push(host);
        }
        request.rollback_hosts = old;
        request.hosts.push(RelaunchHost {
            arguments: vec!["--update-restore".into(), "second-new-view.json".into()],
            receipt_path: directory.path().join("second-new-ready.json"),
            receipt_token: new_receipt_token(),
        });
        request.timeout_seconds = 5;
        validate_request(&request).unwrap();
        let _operation = OperationLock::acquire(&journal.target.root).unwrap();
        let failure = apply_authenticated(&path, &request, &mut journal, |_| {}).unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("previous client build restored and exact views attached")
        );
        for (index, old) in request.rollback_hosts.iter().enumerate() {
            assert_eq!(
                fs::read_to_string(directory.path().join(format!("restored-{index}.json")))
                    .unwrap(),
                format!("exact view {index}")
            );
            let ready: ReadinessReceipt = read_json(&old.host.receipt_path, 16384).unwrap();
            assert_eq!(ready.token, old.host.receipt_token);
            assert_eq!(ready.version, "1.0.0");
            assert!(ready.attached);
        }
        let saved: Journal = read_json(&path, 1024 * 1024).unwrap();
        assert_eq!(saved.phase, JournalStage::Prepared);
        assert_eq!(saved.error.as_deref(), Some(failure.to_string().as_str()));
        assert_eq!(
            read_selection(&journal.target.root).unwrap(),
            journal.prior_selection
        );
        assert!(request.hosts.iter().all(|host| !host.receipt_path.exists()));
    }
    #[test]
    fn journal_cannot_redirect_recovery_into_unowned_stage() {
        let directory = tempdir();
        let target = InstallTarget {
            kind: InstallationKind::PortableWindows,
            root: directory.path().join("Compi"),
        };
        fs::create_dir(&target.root).unwrap();
        let previous = Selection {
            schema: 1,
            version: "1.0.0".into(),
            task_version: "1.0.0".into(),
        };
        restore_selection(&target.root, Some(&previous)).unwrap();
        let mut journal = journal(target.clone(), None, JournalStage::Activated);
        journal.stage = directory.path().join("unrelated");
        atomic_json(&state_dir(&target.root).join("journal.json"), &journal).unwrap();
        assert!(recover(&target).is_err());
        assert_eq!(read_selection(&target.root).unwrap(), Some(previous));
    }
    #[test]
    fn operation_lock_serializes_and_releases_after_drop() {
        let directory = tempdir();
        let lock = OperationLock::acquire(directory.path()).unwrap();
        assert!(OperationLock::acquire(directory.path()).is_err());
        drop(lock);
        let _next = OperationLock::acquire(directory.path()).unwrap();
    }
    fn entries(directory: &Path) -> Vec<String> {
        let mut names = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        names
    }
    fn versions_fixture(target: &InstallTarget, versions: &[&str]) {
        for version in versions {
            let folder = target.root.join("versions").join(version);
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join("compi.exe"), version).unwrap();
        }
    }
    #[test]
    fn prune_keeps_selected_task_in_use_and_pending_stage_only() {
        let directory = tempdir();
        let target = InstallTarget {
            kind: InstallationKind::PortableWindows,
            root: directory.path().join("Compi"),
        };
        versions_fixture(&target, &["1.0.0", "1.5.0", "1.8.0", "2.0.0", "3.0.0"]);
        fs::create_dir(target.root.join("versions/not-a-version")).unwrap();
        let selected = Selection {
            schema: 1,
            version: "2.0.0".into(),
            task_version: "1.5.0".into(),
        };
        restore_selection(&target.root, Some(&selected)).unwrap();
        let pending = journal(target.clone(), Some(selected), JournalStage::Prepared);
        let state = state_dir(&target.root);
        fs::create_dir_all(&pending.stage).unwrap();
        atomic_json(&state.join("journal.json"), &pending).unwrap();
        let finished_stage = state.join(format!("stage-{}", "b".repeat(32)));
        fs::create_dir_all(finished_stage.join("payload")).unwrap();
        let pending_request = format!("request-{}.json", pending.attempt);
        let stale = [
            "request-old.json",
            "handoff-1-a.json",
            "review-rollback-1-a.json",
            "cli-install-a.json",
            "root-previous-old-compi.exe",
        ];
        let unrelated = ["notes.txt", "msi-snapshot.json"];
        for name in stale
            .iter()
            .chain(&unrelated)
            .chain([&pending_request.as_str()])
        {
            fs::write(state.join(name), b"{}").unwrap();
        }
        let busy = target.root.join("versions").join("1.8.0");
        let later = SystemTime::now() + Duration::from_secs(3600);
        prune_locked(&target, Some(later), &|path| path == busy).unwrap();
        assert_eq!(
            entries(&target.root.join("versions")),
            ["1.5.0", "1.8.0", "2.0.0", "not-a-version"]
        );
        assert!(pending.stage.is_dir());
        assert!(!finished_stage.exists());
        assert!(state.join(&pending_request).exists());
        assert!(stale.iter().all(|name| !state.join(name).exists()));
        assert!(unrelated.iter().all(|name| state.join(name).exists()));
        assert_eq!(
            operation_status(&target).unwrap().unwrap().phase,
            JournalStage::Prepared
        );
    }
    #[test]
    fn prune_changes_nothing_mid_activation_or_while_another_operation_runs() {
        let directory = tempdir();
        let target = InstallTarget {
            kind: InstallationKind::PortableWindows,
            root: directory.path().join("Compi"),
        };
        versions_fixture(&target, &["1.0.0", "2.0.0"]);
        let selected = Selection {
            schema: 1,
            version: "2.0.0".into(),
            task_version: "2.0.0".into(),
        };
        restore_selection(&target.root, Some(&selected)).unwrap();
        let state = state_dir(&target.root);
        let stage = state.join(format!("stage-{}", "b".repeat(32)));
        fs::create_dir_all(&stage).unwrap();
        {
            let _operation = OperationLock::acquire(&target.root).unwrap();
            prune(&target).unwrap();
        }
        assert!(stage.exists());
        let interrupted = journal(target.clone(), Some(selected), JournalStage::Relaunching);
        atomic_json(&state.join("journal.json"), &interrupted).unwrap();
        prune(&target).unwrap();
        assert!(stage.exists());
        assert_eq!(entries(&target.root.join("versions")), ["1.0.0", "2.0.0"]);
    }
    #[test]
    fn mac_prune_drops_removed_bundles_from_the_journal() {
        let directory = tempdir();
        let target = InstallTarget {
            kind: InstallationKind::MacBundle,
            root: directory.path().join("Compi.app"),
        };
        fs::create_dir(&target.root).unwrap();
        let mut completed = journal(target.clone(), None, JournalStage::Complete);
        let state = state_dir(&target.root);
        let running = state.join(format!("previous-{}.app", "a".repeat(32)));
        let latest = state.join(format!("previous-{}.app", completed.attempt));
        let failed = state.join(format!("failed-{}.app", "c".repeat(32)));
        for bundle in [&running, &latest, &failed] {
            fs::create_dir_all(bundle.join("Contents")).unwrap();
        }
        completed.retained_backups = vec![running.clone(), latest.clone()];
        completed.backup = Some(latest.clone());
        atomic_json(&state.join("journal.json"), &completed).unwrap();
        prune_locked(&target, None, &|path| path == running).unwrap();
        assert!(running.exists() && !latest.exists() && !failed.exists());
        let saved = operation_status(&target).unwrap().unwrap();
        assert_eq!(saved.retained_backups, [running]);
        assert!(saved.backup.is_none());
    }
    #[test]
    fn rollback_restores_the_root_launcher_and_helper_an_activation_refreshed() {
        let directory = tempdir();
        let target = InstallTarget {
            kind: InstallationKind::InstalledWindows,
            root: directory.path().join("Compi"),
        };
        let activated = journal(target.clone(), None, JournalStage::Activated);
        let payload = target.root.join("versions/2.0.0");
        fs::create_dir_all(&payload).unwrap();
        fs::create_dir_all(state_dir(&target.root)).unwrap();
        let launcher = target.root.join("compi.exe");
        let helper = target.root.join("compi-update-worker.exe");
        fs::write(&launcher, "old launcher").unwrap();
        fs::write(&helper, "old helper").unwrap();
        fs::write(payload.join("compi-update-worker.exe"), "new helper").unwrap();
        // A package from before the launcher joined the payload leaves it alone.
        refresh_root_files(&activated);
        assert_eq!(fs::read_to_string(&launcher).unwrap(), "old launcher");
        assert_eq!(fs::read_to_string(&helper).unwrap(), "new helper");
        fs::write(payload.join("compi-launcher.exe"), "new launcher").unwrap();
        refresh_root_files(&activated);
        assert_eq!(fs::read_to_string(&launcher).unwrap(), "new launcher");
        restore_root_files(&activated);
        assert_eq!(fs::read_to_string(&launcher).unwrap(), "old launcher");
        assert_eq!(fs::read_to_string(&helper).unwrap(), "old helper");
        assert!(
            entries(&state_dir(&target.root))
                .iter()
                .all(|name| !name.starts_with("root-"))
        );
    }
    #[test]
    fn prune_refreshes_root_files_only_for_the_selected_completed_update() {
        let directory = tempdir();
        let target = InstallTarget {
            kind: InstallationKind::InstalledWindows,
            root: directory.path().join("Compi"),
        };
        let payload = target.root.join("versions/2.0.0");
        fs::create_dir_all(&payload).unwrap();
        fs::write(payload.join("compi-launcher.exe"), "new launcher").unwrap();
        let launcher = target.root.join("compi.exe");
        fs::write(&launcher, "old launcher").unwrap();
        let completed = journal(target.clone(), None, JournalStage::Complete);
        atomic_json(&state_dir(&target.root).join("journal.json"), &completed).unwrap();
        // A later Setup selected another version; its root files are Setup's own.
        let other = Selection {
            schema: 1,
            version: "3.0.0".into(),
            task_version: "3.0.0".into(),
        };
        restore_selection(&target.root, Some(&other)).unwrap();
        prune_locked(&target, None, &|_| true).unwrap();
        assert_eq!(fs::read_to_string(&launcher).unwrap(), "old launcher");
        let selected = Selection {
            version: "2.0.0".into(),
            task_version: "2.0.0".into(),
            ..other
        };
        restore_selection(&target.root, Some(&selected)).unwrap();
        prune_locked(&target, None, &|_| false).unwrap();
        assert_eq!(fs::read_to_string(&launcher).unwrap(), "new launcher");
        assert!(
            entries(&state_dir(&target.root))
                .iter()
                .all(|name| !name.starts_with("root-"))
        );
    }
}
