use crate::*;
use fs2::FileExt;
use rand::RngCore;
use std::{
    process::{Child, Command},
    time::{Duration, Instant},
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
    let output = Command::new(executable)
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
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
function Assert-Owner($owner, $commands) {{
    if ($owner -ne $id.User.Value -and $owner -ne $id.Name) {{ throw 'Task belongs to another Windows account' }}
    if (@($commands).Count -ne 1) {{ throw 'Task does not have exactly one owned daemon action' }}
    foreach ($command in $commands) {{
        $path=[IO.Path]::GetFullPath($command.Trim('"'))
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
        "{current_guard}\n$allowed=@('{previous}')\nif ($name -ne '{quoted_name}') {{ throw 'Prior task belongs to another user' }}\n{restore}"
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
        }
    }
    journal.phase = JournalStage::RolledBack;
    atomic_json(path, journal)
}
pub fn launch_worker(prepared: &PreparedUpdate, request: &RelaunchRequest) -> Result<Child> {
    validate_request(request)?;
    let _lock = OperationLock::acquire(&prepared.target.root)?;
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
    validate_activation_baseline(&journal)?;
    let config = ReleaseConfig::compiled()?;
    let verified = verify_manifest(&serde_json::to_vec(&journal.signed)?, &config)?;
    if serde_json::to_vec(&verified.manifest)? != serde_json::to_vec(&journal.manifest)? {
        return Err(err("Journal manifest differs from publisher signature"));
    }
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
    let deadline = Instant::now() + Duration::from_secs(request.timeout_seconds);
    while request
        .old_process_ids
        .iter()
        .map(|&pid| process_alive(pid))
        .collect::<Result<Vec<_>>>()?
        .iter()
        .any(|&alive| alive)
    {
        if Instant::now() >= deadline {
            return Err(err(
                "Old GUI hosts did not exit within deadline; close Compi hosts using this installation and retry",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    validate_host_inventory(&journal.target, &request.old_process_ids)?;
    progress(event(
        UpdatePhase::Activating,
        0,
        None,
        "Activating verified full payload; old daemon payload retained",
    ));
    if journal.target.kind == InstallationKind::MacBundle {
        journal.backup =
            Some(state_dir(&journal.target.root).join(format!("previous-{}.app", journal.attempt)));
        let backup = journal.backup.as_ref().unwrap();
        if !journal.retained_backups.contains(backup) {
            journal.retained_backups.push(backup.clone());
        }
    }
    if journal.target.kind == InstallationKind::InstalledWindows && request.replace_default_daemon {
        journal.prior_task = Some(snapshot_owned_task(&journal)?);
    }
    validate_journal(journal_path, &journal)?;
    journal.phase = JournalStage::Activating;
    atomic_json(journal_path, &journal)?;
    let mut new_gui_exit_confirmed = true;
    let result: Result<()> = (|| {
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
                    task_version: selected_task_version(&journal, request.replace_default_daemon),
                };
                atomic_json(&journal.target.root.join("selection.json"), &selection)?;
                if journal.prior_task.is_some() {
                    install_verified_task(&journal)?;
                }
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
        journal.error = Some(failure.to_string());
        atomic_json(journal_path, &journal)?;
        if !new_gui_exit_confirmed {
            return Err(err(format!(
                "{failure}; GUI exit is unconfirmed: close only the worker-spawned GUI hosts before recovery; unsafe rollback was not attempted; journal: {}",
                journal_path.display()
            )));
        }
        if request.replace_default_daemon
            && let Err(blocked) = stop_replacement_daemons(&journal)
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
        if let Err(rollback) = rollback(journal_path, &mut journal) {
            return Err(err(format!(
                "{failure}; rollback also failed: {rollback}; journal: {}",
                journal_path.display()
            )));
        }
        match relaunch_prior(&journal.target, request) {
            Ok(()) => {
                journal.error = Some(format!(
                    "{failure}; previous client build restored and exact views attached"
                ));
                atomic_json(journal_path, &journal)?;
                return Err(err(journal.error.clone().unwrap()));
            }
            Err(restore) => {
                let recovery = manual_recovery(request);
                journal.error = Some(format!(
                    "{failure}; prior payload restored, but old GUI attachment recovery failed: {restore}; {recovery}; handoffs retained; journal: {}",
                    journal_path.display()
                ));
                atomic_json(journal_path, &journal)?;
                return Err(err(journal.error.clone().unwrap()));
            }
        }
    }
    Ok(())
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
}
