//! One process-wide updater. Only checks run automatically; every mutation is explicit.
use crate::config::{AutomaticUpdateChecks, LoadedConfig, UpdateSettings};
use crate::connection::ConnectionTarget;
use compi_protocol::{DaemonClient, LifecycleStatus};
use compi_update::{
    AvailableRelease, Cancellation, InstallTarget, PreparedUpdate, UpdateEvent, UpdateManager,
    UpdatePhase,
};
use std::{
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug)]
pub struct DaemonUpdateStatus {
    pub target: ConnectionTarget,
    /// Only daemon executables owned by the selected installation can be stopped.
    pub managed: bool,
    pub status: Result<LifecycleStatus, String>,
}

#[derive(Clone, Default)]
pub struct UpdateSnapshot {
    pub release: Option<Arc<AvailableRelease>>,
    pub available_release: Option<Arc<AvailableRelease>>,
    pub release_notes: Arc<str>,
    pub prepared: Option<PreparedUpdate>,
    pub progress: Option<UpdateEvent>,
    pub busy: bool,
    pub deferred: bool,
    pub error: Option<String>,
    pub last_check_unix: Option<u64>,
    pub last_check_result: Option<String>,
    pub preferences: UpdateSettings,
    pub daemons: Vec<DaemonUpdateStatus>,
    pub consent_reviewed: bool,
    pub host_process_ids: Vec<u32>,
    pub recovery_available: bool,
    pub recovery_journal: Option<PathBuf>,
}

impl UpdateSnapshot {
    pub fn requires_daemon_restart(&self, status: &LifecycleStatus) -> bool {
        self.release.as_ref().is_some_and(|release| {
            release.manifest.daemon_protocol != status.protocol_version
                || !release
                    .manifest
                    .qualified_daemon_versions
                    .iter()
                    .any(|version| version == &status.product_version)
        })
    }
    fn replace_default_daemon(&self, remaining: &[LifecycleStatus]) -> bool {
        !remaining
            .iter()
            .any(|status| status.instance.is_none() && !self.requires_daemon_restart(status))
    }

    pub fn install_blocker(&self) -> Option<String> {
        if self.prepared.is_none() {
            return Some("Download and verify the update first.".into());
        }
        for daemon in &self.daemons {
            match &daemon.status {
                Err(error) => {
                    return Some(format!(
                        "Cannot account for running work: {error}. Open the previous Compi build and stop its daemon deliberately before updating."
                    ));
                }
                Ok(status) if !daemon.managed && self.requires_daemon_restart(status) => {
                    return Some("A connected daemon outside this installation is not qualified for the release. Update that host deliberately before installing locally; Compi will not deploy to or restart it.".into());
                }
                _ => {}
            }
        }
        None
    }
}

enum Request {
    Check,
    Download,
    Inventory { reviewed: bool },
    Configure(LoadedConfig),
    Preference(LoadedConfig, AutomaticUpdateChecks),
    Install,
    Reinstall,
    RecoverPrior,
}

pub struct UpdateService {
    state: Arc<Mutex<UpdateSnapshot>>,
    sender: mpsc::Sender<Request>,
    cancellation: Mutex<Cancellation>,
    targets: Arc<Mutex<Vec<ConnectionTarget>>>,
}

static SERVICE: LazyLock<Arc<UpdateService>> = LazyLock::new(UpdateService::start);
static READINESS: LazyLock<Mutex<Option<(PathBuf, String)>>> = LazyLock::new(|| Mutex::new(None));

pub fn set_readiness_receipt(path: PathBuf, token: String) {
    if let Ok(mut receipt) = READINESS.lock() {
        *receipt = Some((path, token));
    }
}

pub fn readiness_receipt() -> Option<(PathBuf, String)> {
    READINESS.lock().ok()?.clone()
}

pub fn shared(config: &LoadedConfig, target: &ConnectionTarget) -> Arc<UpdateService> {
    let service = SERVICE.clone();
    let mut targets = service
        .targets
        .lock()
        .expect("updater target registry poisoned");
    let first = targets.is_empty();
    if !targets.contains(target) {
        targets.push(target.clone());
    }
    drop(targets);
    if first {
        if let Ok(mut state) = service.state.lock() {
            state.preferences = config.updates.clone();
            state.last_check_unix = config.updates.last_check_unix;
        }
        let automatic = match config.updates.automatic_checks {
            AutomaticUpdateChecks::Never => false,
            AutomaticUpdateChecks::OnLaunch => true,
            AutomaticUpdateChecks::Daily => daily_check_due(config.updates.last_check_unix, now()),
        };
        // Carries the selected path to the serialized background writer, never writes UI state.
        let _ = service.sender.send(Request::Configure(config.clone()));
        if automatic {
            service.check();
        }
        let _ = service.sender.send(Request::Inventory { reviewed: false });
    }
    service
}

pub fn daily_check_due(last: Option<u64>, current: u64) -> bool {
    last.is_none_or(|last| current >= last.saturating_add(86_400))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl UpdateService {
    fn start() -> Arc<Self> {
        let (sender, receiver) = mpsc::channel();
        let state = Arc::new(Mutex::new(UpdateSnapshot::default()));
        let targets = Arc::new(Mutex::new(Vec::new()));
        let service = Arc::new(Self {
            state: state.clone(),
            sender,
            cancellation: Mutex::new(Cancellation::default()),
            targets: targets.clone(),
        });
        let weak = Arc::downgrade(&service);
        thread::spawn(move || {
            let mut manager = None;
            let mut config: Option<LoadedConfig> = None;
            if let Err(error) = restore_pending(&state) {
                state.lock().expect("updater state poisoned").error = Some(error);
            }
            if readiness_receipt().is_some() {
                let refreshed = state.clone();
                thread::spawn(move || {
                    // The worker finalizes the journal after all actual host receipts arrive.
                    thread::sleep(Duration::from_secs(2));
                    if let Err(error) = restore_pending(&refreshed) {
                        refreshed.lock().expect("updater state poisoned").error = Some(error);
                    }
                });
            }
            loop {
                let request = match receiver.recv_timeout(Duration::from_secs(60)) {
                    Ok(request) => request,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let snapshot = state.lock().expect("updater state poisoned").clone();
                        if snapshot.preferences.automatic_checks == AutomaticUpdateChecks::Daily
                            && daily_check_due(snapshot.last_check_unix, now())
                        {
                            if let Some(service) = weak.upgrade() {
                                *service
                                    .cancellation
                                    .lock()
                                    .expect("updater cancellation poisoned") =
                                    Cancellation::default();
                            }
                            Request::Check
                        } else {
                            continue;
                        }
                    }
                };
                let Some(service) = weak.upgrade() else { break };
                if let Request::Configure(selected) = request {
                    config = Some(selected);
                    continue;
                }
                if let Request::Preference(mut selected, preference) = request {
                    let mut settings = selected.updates.clone();
                    settings.automatic_checks = preference;
                    settings.last_check_unix = state
                        .lock()
                        .expect("updater state poisoned")
                        .last_check_unix;
                    match selected.save_update_settings(settings.clone()) {
                        Ok(()) => {
                            state.lock().expect("updater state poisoned").preferences = settings
                        }
                        Err(error) => {
                            state.lock().expect("updater state poisoned").error = Some(error)
                        }
                    }
                    config = Some(selected);
                    continue;
                }
                let cancellation = service
                    .cancellation
                    .lock()
                    .expect("updater cancellation poisoned")
                    .clone();
                {
                    let mut snapshot = state.lock().expect("updater state poisoned");
                    snapshot.busy = true;
                    if !matches!(request, Request::Inventory { reviewed: false }) {
                        snapshot.error = None;
                    }
                    if matches!(request, Request::Download | Request::Install) {
                        snapshot.deferred = false;
                    }
                }
                let progress_state = state.clone();
                let report = move |event| {
                    progress_state
                        .lock()
                        .expect("updater state poisoned")
                        .progress = Some(event);
                };
                let is_check = matches!(request, Request::Check);
                let result = (|| -> Result<(), String> {
                    if !matches!(request, Request::Inventory { .. } | Request::RecoverPrior)
                        && manager.is_none()
                    {
                        manager = Some(UpdateManager::new().map_err(|error| error.to_string())?);
                    }
                    match request {
                        Request::Check => {
                            let release = manager
                                .as_mut()
                                .expect("manager initialized")
                                .check(&cancellation, report)
                                .map_err(|error| error.to_string())?;
                            let mut snapshot = state.lock().expect("updater state poisoned");
                            snapshot.last_check_result = Some(
                                if release.is_some() {
                                    "Update available"
                                } else {
                                    "Up to date"
                                }
                                .into(),
                            );
                            apply_checked_release(&mut snapshot, release);
                            snapshot.consent_reviewed = false;
                        }
                        Request::Download => {
                            let release = {
                                let snapshot = state.lock().expect("updater state poisoned");
                                snapshot
                                    .available_release
                                    .as_ref()
                                    .or(snapshot.release.as_ref())
                                    .cloned()
                                    .ok_or("Check for an available release first")?
                            };
                            let target =
                                InstallTarget::detect().map_err(|error| error.to_string())?;
                            let prepared = manager
                                .as_mut()
                                .expect("manager initialized")
                                .prepare(&release, &target, &cancellation, report)
                                .map_err(|error| error.to_string())?;
                            let mut snapshot = state.lock().expect("updater state poisoned");
                            snapshot.prepared = Some(prepared);
                            snapshot.release = Some(release);
                            snapshot.consent_reviewed = false;
                        }
                        Request::Inventory { reviewed } => {
                            if reviewed {
                                report(UpdateEvent { phase: UpdatePhase::Checking, completed: 0, total: None, message: "Inspecting affected GUI hosts and daemon-owned live work. No installation or shutdown has begun.".into() });
                            }
                            let mut registered =
                                targets.lock().expect("updater targets poisoned").clone();
                            if reviewed {
                                let prepared = state
                                    .lock()
                                    .expect("updater state poisoned")
                                    .prepared
                                    .clone()
                                    .ok_or("Download and verify before reviewing installation")?;
                                for target in inspect_host_targets(&prepared, &cancellation)? {
                                    if !registered.contains(&target) {
                                        registered.push(target);
                                    }
                                }
                            }
                            let mut daemons = Vec::new();
                            let installation =
                                InstallTarget::detect().map_err(|error| error.to_string())?;
                            match DaemonClient::local_lifecycle_statuses_for_install(
                                &installation.root,
                            ) {
                                Ok(statuses) => {
                                    for status in statuses {
                                        daemons.push(DaemonUpdateStatus {
                                            target: ConnectionTarget::Local {
                                                instance: status.instance.clone(),
                                            },
                                            managed: true,
                                            status: Ok(status),
                                        });
                                    }
                                }
                                Err(error) => daemons.push(DaemonUpdateStatus {
                                    target: ConnectionTarget::Local { instance: None },
                                    managed: true,
                                    status: Err(error.to_string()),
                                }),
                            }
                            for target in registered {
                                if daemons.iter().any(|daemon| daemon.target == target) {
                                    continue;
                                }
                                let status =
                                    target.lifecycle_status().map_err(|error| error.to_string());
                                daemons.push(DaemonUpdateStatus {
                                    target,
                                    managed: false,
                                    status,
                                });
                            }
                            let hosts = affected_host_ids()?;
                            let mut snapshot = state.lock().expect("updater state poisoned");
                            snapshot.daemons = daemons;
                            snapshot.consent_reviewed = reviewed;
                            snapshot.host_process_ids = hosts;
                        }
                        Request::Install => {
                            let snapshot = state.lock().expect("updater state poisoned").clone();
                            if !snapshot.consent_reviewed {
                                return Err("Review affected work before installing".into());
                            }
                            if let Some(error) = snapshot.install_blocker() {
                                return Err(error);
                            }
                            activate(
                                manager.as_ref().expect("manager initialized"),
                                &snapshot,
                                &cancellation,
                                report,
                            )?;
                            let mut snapshot = state.lock().expect("updater state poisoned");
                            snapshot.progress = Some(UpdateEvent { phase: UpdatePhase::Relaunching, completed: 0, total: None, message: "Helper started. Prepared client hosts are releasing their windows.".into() });
                        }
                        Request::Preference(_, _) | Request::Configure(_) => unreachable!(),
                        Request::Reinstall => {
                            let target =
                                InstallTarget::detect().map_err(|error| error.to_string())?;
                            let prepared =
                                compi_update::reinstall_verified(&target, &cancellation, report)
                                    .map_err(|error| error.to_string())?;
                            let journal = compi_update::operation_status(&target)
                                .map_err(|error| error.to_string())?
                                .ok_or("Verified retry did not retain an operation journal")?;
                            let release_config = compi_update::ReleaseConfig::compiled()
                                .map_err(|error| error.to_string())?;
                            let release = compi_update::verify_manifest(
                                &serde_json::to_vec(&journal.signed)
                                    .map_err(|error| error.to_string())?,
                                &release_config,
                            )
                            .map_err(|error| error.to_string())?;
                            let mut snapshot = state.lock().expect("updater state poisoned");
                            snapshot.prepared = Some(prepared);
                            snapshot.release = Some(Arc::new(release));
                            if snapshot.available_release.is_none() {
                                snapshot.release_notes = Arc::from(
                                    snapshot
                                        .release
                                        .as_ref()
                                        .expect("verified retry release")
                                        .manifest
                                        .release_notes
                                        .as_str(),
                                );
                            }
                            snapshot.recovery_available = false;
                            snapshot.consent_reviewed = false;
                            snapshot.deferred = true;
                        }
                        Request::RecoverPrior => {
                            let target =
                                InstallTarget::detect().map_err(|error| error.to_string())?;
                            compi_update::recover(&target).map_err(|error| error.to_string())?;
                            let mut snapshot = state.lock().expect("updater state poisoned");
                            snapshot.progress = Some(UpdateEvent { phase: UpdatePhase::Ready, completed: 0, total: None, message: "Prior selection restored. Running terminals were not restarted. Reinstall the verified package, then review affected work to retry the client update.".into() });
                            snapshot.recovery_available = true;
                            snapshot.consent_reviewed = false;
                        }
                    }
                    Ok(())
                })();
                let timestamp = now();
                let mut snapshot = state.lock().expect("updater state poisoned");
                snapshot.busy = false;
                if let Err(error) = result {
                    if is_check {
                        snapshot.last_check_result = Some(error.clone());
                    }
                    snapshot.error = Some(error);
                }
                if is_check {
                    snapshot.progress = None;
                    snapshot.last_check_unix = Some(timestamp);
                    snapshot.preferences.last_check_unix = Some(timestamp);
                    if let Some(config) = config.as_mut()
                        && let Err(error) =
                            config.save_update_settings(snapshot.preferences.clone())
                    {
                        snapshot.error = Some(error);
                    }
                }
            }
        });
        service
    }

    pub fn snapshot(&self) -> UpdateSnapshot {
        self.state.lock().expect("updater state poisoned").clone()
    }
    fn request(&self, request: Request) {
        let mut snapshot = self.state.lock().expect("updater state poisoned");
        if snapshot.busy {
            return;
        }
        snapshot.busy = true;
        *self
            .cancellation
            .lock()
            .expect("updater cancellation poisoned") = Cancellation::default();
        drop(snapshot);
        if let Err(error) = self.sender.send(request) {
            let mut snapshot = self.state.lock().expect("updater state poisoned");
            snapshot.busy = false;
            snapshot.error = Some(error.to_string());
        }
    }
    pub fn check(&self) {
        self.request(Request::Check);
    }
    pub fn download(&self) {
        self.request(Request::Download);
    }
    pub fn review(&self) {
        self.request(Request::Inventory { reviewed: true });
    }
    pub fn install(&self) {
        self.request(Request::Install);
    }
    pub fn reinstall(&self) {
        self.request(Request::Reinstall);
    }
    pub fn restore_prior(&self) {
        self.request(Request::RecoverPrior);
    }
    pub fn preference(&self, config: LoadedConfig, preference: AutomaticUpdateChecks) {
        let _ = self.sender.send(Request::Preference(config, preference));
    }
    pub fn cancel(&self) {
        self.cancellation
            .lock()
            .expect("updater cancellation poisoned")
            .cancel();
    }
    pub fn defer(&self) {
        let mut snapshot = self.state.lock().expect("updater state poisoned");
        if !snapshot.busy {
            snapshot.deferred = true;
            snapshot.consent_reviewed = false;
        }
    }
}
#[cfg(any(windows, target_os = "macos"))]
fn affected_host_ids() -> Result<Vec<u32>, String> {
    crate::window_host::update_hosts()
        .map(|hosts| {
            let mut ids: Vec<_> = hosts.iter().map(|host| host.pid).collect();
            ids.sort_unstable();
            ids
        })
        .map_err(|error| error.to_string())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn affected_host_ids() -> Result<Vec<u32>, String> {
    Err("Desktop update activation is unavailable on this platform".into())
}

#[cfg(any(windows, target_os = "macos"))]
fn activate(
    manager: &UpdateManager,
    snapshot: &UpdateSnapshot,
    cancellation: &Cancellation,
    mut report: impl FnMut(UpdateEvent),
) -> Result<(), String> {
    use crate::window_host;
    use compi_update::{RelaunchHost, RelaunchRequest, RollbackHost};
    let prepared = snapshot
        .prepared
        .as_ref()
        .ok_or("No verified update is staged")?;
    let hosts = window_host::update_hosts().map_err(|error| error.to_string())?;
    let mut host_ids: Vec<_> = hosts.iter().map(|host| host.pid).collect();
    host_ids.sort_unstable();
    if hosts.is_empty() || host_ids != snapshot.host_process_ids {
        return Err("GUI hosts changed since review. Review affected work again.".into());
    }
    report(UpdateEvent {
        phase: UpdatePhase::Ready,
        completed: 0,
        total: None,
        message: "Saving exact windows and preparing GUI hosts. You can still cancel.".into(),
    });
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let directory = prepared
        .journal_path
        .parent()
        .ok_or("Update journal has no parent")?;
    let mut requests = Vec::new();
    let mut rollback_requests = Vec::new();
    let mut prepared_hosts = Vec::new();
    let result = (|| -> Result<(), String> {
        for host in &hosts {
            if cancellation.is_cancelled() {
                return Err("Update cancelled before activation".into());
            }
            let handoff = directory.join(format!("handoff-{}-{nonce}.json", host.pid));
            let rollback_handoff =
                directory.join(format!("rollback-handoff-{}-{nonce}.json", host.pid));
            window_host::request_update_prepare(
                host,
                handoff.clone(),
                rollback_handoff.clone(),
                prepared.version.clone(),
            )
            .map_err(|error| error.to_string())?;
            prepared_hosts.push((host.clone(), handoff.clone()));
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while !handoff.is_file() {
                if cancellation.is_cancelled() {
                    return Err("Update cancelled while preparing windows".into());
                }
                if std::time::Instant::now() >= deadline {
                    return Err(format!(
                        "GUI host {} did not save its update handoff; no install was activated",
                        host.pid
                    ));
                }
                thread::sleep(Duration::from_millis(40));
            }
            let rollback_windows = crate::update_restore::Handoff::inspect(&rollback_handoff)
                .map_err(|error| error.to_string())?;
            if rollback_windows.is_empty() {
                return Err("GUI host did not save an exact rollback handoff".into());
            }
            for window in crate::update_restore::Handoff::inspect(&handoff)
                .map_err(|error| error.to_string())?
            {
                if window.target.is_remote() {
                    let status = window
                        .target
                        .lifecycle_status()
                        .map_err(|error| format!("Cannot inspect remote update target: {error}"))?;
                    if snapshot.requires_daemon_restart(&status) {
                        return Err(format!(
                            "Remote daemon {} (protocol {}) is not qualified for the update. Update that host deliberately before installing locally.",
                            status.product_version, status.protocol_version
                        ));
                    }
                    if !snapshot
                        .daemons
                        .iter()
                        .any(|daemon| daemon.target == window.target)
                    {
                        return Err("A remote target was not included in the displayed review. Review affected work again.".into());
                    }
                }
            }
            requests.push(RelaunchHost {
                arguments: vec![
                    "--update-restore".into(),
                    handoff.to_string_lossy().into_owned(),
                ],
                receipt_path: directory.join(format!("receipt-{}-{nonce}.json", host.pid)),
                receipt_token: compi_update::new_receipt_token(),
            });
            rollback_requests.push(RollbackHost {
                executable: host.executable.clone(),
                version: host.product_version.clone(),
                host: RelaunchHost {
                    arguments: vec![
                        "--update-restore".into(),
                        rollback_handoff.to_string_lossy().into_owned(),
                    ],
                    receipt_path: directory
                        .join(format!("rollback-receipt-{}-{nonce}.json", host.pid)),
                    receipt_token: compi_update::new_receipt_token(),
                },
            });
        }
        let current_hosts = window_host::update_hosts().map_err(|error| error.to_string())?;
        if current_hosts
            .iter()
            .map(|host| host.pid)
            .collect::<Vec<_>>()
            != hosts.iter().map(|host| host.pid).collect::<Vec<_>>()
        {
            return Err("GUI host inventory changed while saving windows. Review again.".into());
        }
        let current = DaemonClient::local_lifecycle_statuses_for_install(&prepared.target.root)
            .map_err(|error| error.to_string())?;
        let reviewed = snapshot
            .daemons
            .iter()
            .filter(|daemon| daemon.managed)
            .filter_map(|daemon| daemon.status.as_ref().ok())
            .collect::<Vec<_>>();
        if current.len() != reviewed.len()
            || current.iter().any(|status| {
                !reviewed.iter().any(|old| {
                    old.server_id == status.server_id && old.consent() == status.consent()
                })
            })
        {
            return Err(
                "Daemon-owned work changed since review. Review affected work again.".into(),
            );
        }
        if cancellation.is_cancelled() {
            return Err("Update cancelled before daemon shutdown or activation".into());
        }
        report(UpdateEvent {
            phase: UpdatePhase::Activating,
            completed: 0,
            total: None,
            message:
                "Committing the reviewed lifecycle decision. Cancellation is no longer available."
                    .into(),
        });
        for daemon in &snapshot.daemons {
            let status = daemon.status.as_ref().map_err(Clone::clone)?;
            if daemon.managed && snapshot.requires_daemon_restart(status) {
                match &daemon.target {
                    ConnectionTarget::Local { instance } => DaemonClient::conditional_stop(
                        instance.as_deref(),
                        &status.consent(),
                        Duration::from_secs(15),
                    )
                    .map_err(|error| error.to_string())?,
                    ConnectionTarget::Ssh { .. } => {
                        return Err("Local updates never restart remote daemons".into());
                    }
                }
            }
        }
        let remaining = DaemonClient::local_lifecycle_statuses_for_install(&prepared.target.root)
            .map_err(|error| error.to_string())?;
        if remaining.iter().any(|status| {
            snapshot.requires_daemon_restart(status)
                || !reviewed.iter().any(|old| old.server_id == status.server_id)
        }) {
            return Err("A daemon appeared during update consent. Installation remains deferred; review again.".into());
        }
        let relaunch = RelaunchRequest {
            old_process_ids: hosts.iter().map(|host| host.pid).collect(),
            hosts: requests,
            rollback_hosts: rollback_requests,
            replace_default_daemon: snapshot.replace_default_daemon(&remaining),
            timeout_seconds: 60,
        };
        let child = manager
            .launch_worker(prepared, &relaunch)
            .map_err(|error| error.to_string())?;
        drop(child);
        // The helper alone commits, after every owned GUI host releases attachment and slot locks.
        for (host, handoff) in prepared_hosts
            .iter()
            .filter(|(host, _)| host.pid != std::process::id())
            .chain(
                prepared_hosts
                    .iter()
                    .filter(|(host, _)| host.pid == std::process::id()),
            )
        {
            window_host::request_update_release(host, handoff.clone())
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    })();
    if result.is_err() {
        for (host, handoff) in &prepared_hosts {
            let _ = window_host::request_update_abort(host, handoff.clone());
        }
    }
    result
}

#[cfg(not(any(windows, target_os = "macos")))]
fn activate(
    _: &UpdateManager,
    _: &UpdateSnapshot,
    _: &Cancellation,
    _: impl FnMut(UpdateEvent),
) -> Result<(), String> {
    Err("Desktop update activation is unavailable on this platform".into())
}

fn restore_pending(state: &Arc<Mutex<UpdateSnapshot>>) -> Result<(), String> {
    let target = InstallTarget::detect().map_err(|error| error.to_string())?;
    let prepared = if readiness_receipt().is_none() {
        compi_update::pending_update(&target).map_err(|error| error.to_string())?
    } else {
        None
    };
    let Some(journal) =
        compi_update::operation_status(&target).map_err(|error| error.to_string())?
    else {
        return Ok(());
    };
    let mut snapshot = state.lock().expect("updater state poisoned");
    snapshot.recovery_journal = Some(compi_update::operation_journal_path(&target));
    snapshot.recovery_available = journal.phase == compi_update::JournalStage::RolledBack;
    if journal.phase == compi_update::JournalStage::Complete
        && journal.manifest.version == env!("CARGO_PKG_VERSION")
    {
        snapshot.progress = Some(UpdateEvent { phase: UpdatePhase::Complete, completed: 0, total: None, message: "Update complete. Every restored GUI host confirmed the selected build and actual terminal attachment.".into() });
    }
    if let Some(error) = journal.error {
        snapshot.error = Some(format!(
            "{error}. Recovery journal: {}",
            compi_update::operation_journal_path(&target).display()
        ));
    }
    if prepared.is_some() || snapshot.recovery_available {
        let config = compi_update::ReleaseConfig::compiled().map_err(|error| error.to_string())?;
        let release = compi_update::verify_manifest(
            &serde_json::to_vec(&journal.signed).map_err(|error| error.to_string())?,
            &config,
        )
        .map_err(|error| error.to_string())?;
        snapshot.release = Some(Arc::new(release));
        if snapshot.available_release.is_none() {
            snapshot.release_notes = Arc::from(
                snapshot
                    .release
                    .as_ref()
                    .expect("recovered release")
                    .manifest
                    .release_notes
                    .as_str(),
            );
        }
        snapshot.prepared = prepared;
        snapshot.deferred = true;
        snapshot.progress = Some(UpdateEvent {
            phase: UpdatePhase::Ready, completed: 0, total: None,
            message: if snapshot.recovery_available {
                "Previous installation restored. Retained package will be reverified before retry; installation still requires review and your action."
            } else {
                "Recovered and reverified the staged update. Installation still requires review and your action."
            }.into(),
        });
    }
    Ok(())
}
fn apply_checked_release(snapshot: &mut UpdateSnapshot, release: Option<AvailableRelease>) {
    let release = release.map(Arc::new);
    snapshot.release_notes = release
        .as_ref()
        .or_else(|| snapshot.prepared.as_ref().and(snapshot.release.as_ref()))
        .map(|release| Arc::from(release.manifest.release_notes.as_str()))
        .unwrap_or_default();
    snapshot.available_release = release.clone();
    // An automatic check never throws away an explicitly downloaded/deferred candidate.
    if snapshot.prepared.is_none() {
        snapshot.release = release;
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn inspect_host_targets(
    prepared: &PreparedUpdate,
    cancellation: &Cancellation,
) -> Result<Vec<ConnectionTarget>, String> {
    use crate::window_host;
    let directory = prepared
        .journal_path
        .parent()
        .ok_or("Update journal has no parent")?;
    let nonce = compi_update::new_receipt_token();
    let mut targets = Vec::new();
    for host in window_host::update_hosts().map_err(|error| error.to_string())? {
        let path = directory.join(format!("review-{}-{nonce}.json", host.pid));
        let rollback = directory.join(format!("review-rollback-{}-{nonce}.json", host.pid));
        window_host::request_update_prepare(
            &host,
            path.clone(),
            rollback.clone(),
            prepared.version.clone(),
        )
        .map_err(|error| error.to_string())?;
        let result = (|| -> Result<(), String> {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while !path.is_file() {
                if cancellation.is_cancelled() {
                    return Err("Update review cancelled".into());
                }
                if std::time::Instant::now() >= deadline {
                    return Err(format!(
                        "GUI host {} did not save a review handoff",
                        host.pid
                    ));
                }
                thread::sleep(Duration::from_millis(40));
            }
            for window in
                crate::update_restore::Handoff::inspect(&path).map_err(|error| error.to_string())?
            {
                if !targets.contains(&window.target) {
                    targets.push(window.target);
                }
            }
            Ok(())
        })();
        window_host::request_update_abort(&host, path.clone())
            .map_err(|error| error.to_string())?;
        if path.is_file() {
            std::fs::remove_file(&path).map_err(|error| error.to_string())?;
        }
        if rollback.is_file() {
            std::fs::remove_file(&rollback).map_err(|error| error.to_string())?;
        }
        result?;
    }
    Ok(targets)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn inspect_host_targets(
    _: &PreparedUpdate,
    _: &Cancellation,
) -> Result<Vec<ConnectionTarget>, String> {
    Err("Desktop update activation is unavailable on this platform".into())
}

/// Update-worker helper (`compi --stop-idle-update-daemons ROOT VERSION`); the worker
/// itself does not link the daemon protocol. Stops only idle daemons of `version` that
/// OS attribution places in `root`, through consent-checked lifecycle shutdown.
pub fn stop_idle_update_daemons(root: &std::path::Path, version: &str) -> Result<(), String> {
    let statuses =
        DaemonClient::local_lifecycle_statuses_for_install(root).map_err(|e| e.to_string())?;
    for status in statuses
        .iter()
        .filter(|status| status.product_version == version)
    {
        if !status.connected_clients.is_empty() || !status.live_surfaces.is_empty() {
            return Err(format!(
                "Replacement daemon {} ({}) still has {} connected client(s) and {} live shell(s); close them, then reopen Compi to finish rollback",
                status.daemon_pid,
                status.product_version,
                status.connected_clients.len(),
                status.live_surfaces.len()
            ));
        }
        DaemonClient::conditional_stop(
            status.instance.as_deref(),
            &status.consent(),
            Duration::from_secs(15),
        )
        .map_err(|e| {
            format!(
                "Could not stop idle replacement daemon {}: {e}",
                status.daemon_pid
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn daily_check_respects_boundary_and_backward_clock() {
        assert!(daily_check_due(None, 0));
        assert!(!daily_check_due(Some(100), 86_499));
        assert!(daily_check_due(Some(100), 86_500));
        assert!(!daily_check_due(Some(100), 99));
        assert!(!daily_check_due(Some(u64::MAX), 100));
    }

    fn candidate() -> UpdateSnapshot {
        UpdateSnapshot {
            release: Some(Arc::new(AvailableRelease {
                manifest: compi_update::ReleaseManifest {
                    schema: 1,
                    product: "compi".into(),
                    version: "0.2.0".into(),
                    platform: "windows-x86_64".into(),
                    minimum_os: "10".into(),
                    release_notes: String::new(),
                    daemon_protocol: 14,
                    qualified_daemon_versions: vec!["0.1.3".into()],
                    minimum_persistence: 1,
                    artifact: compi_update::Artifact {
                        url: String::new(),
                        size: 1,
                        sha256: String::new(),
                    },
                },
                manifest_bytes: Vec::new(),
                signature: String::new(),
            })),
            prepared: Some(PreparedUpdate {
                journal_path: "journal".into(),
                target: InstallTarget {
                    kind: compi_update::InstallationKind::PortableWindows,
                    root: "installation".into(),
                },
                version: "0.2.0".into(),
                stage: "stage".into(),
            }),
            ..UpdateSnapshot::default()
        }
    }

    fn daemon() -> LifecycleStatus {
        LifecycleStatus {
            lifecycle_version: 1,
            product_version: "0.1.3".into(),
            protocol_version: 14,
            daemon_pid: 1,
            supervisor_pid: None,
            daemon_executable: "installation/compi-daemon.exe".into(),
            server_id: compi_protocol::ServerId::new("server"),
            server_generation: compi_protocol::ServerGeneration::new("generation"),
            instance: None,
            workspace_revision: 1,
            connected_clients: vec![1],
            live_surfaces: Vec::new(),
        }
    }

    #[test]
    fn compatibility_requires_both_exact_protocol_and_qualified_product() {
        let candidate = candidate();
        let mut daemon = daemon();
        assert!(!candidate.requires_daemon_restart(&daemon));
        daemon.protocol_version = 15;
        assert!(candidate.requires_daemon_restart(&daemon));
        daemon.protocol_version = 14;
        daemon.product_version = "0.1.2".into();
        assert!(candidate.requires_daemon_restart(&daemon));
    }

    #[test]
    fn task_replacement_tracks_the_live_default_not_named_instances() {
        let candidate = candidate();
        let compatible = daemon();
        assert!(!candidate.replace_default_daemon(std::slice::from_ref(&compatible)));
        let mut named = compatible.clone();
        named.instance = Some("work".into());
        named.product_version = "0.1.2".into();
        assert!(!candidate.replace_default_daemon(&[compatible, named.clone()]));
        assert!(candidate.replace_default_daemon(&[named]));
        assert!(candidate.replace_default_daemon(&[]));
        let mut incompatible = daemon();
        incompatible.product_version = "0.1.2".into();
        assert!(candidate.replace_default_daemon(&[incompatible]));
    }

    #[test]
    fn incompatible_unmanaged_daemon_blocks_local_activation() {
        let mut candidate = candidate();
        let mut daemon = daemon();
        daemon.protocol_version = 15;
        candidate.daemons.push(DaemonUpdateStatus {
            target: ConnectionTarget::from_options(None, Some("example.test".into())).unwrap(),
            managed: false,
            status: Ok(daemon.clone()),
        });
        assert!(candidate.install_blocker().is_some());
        candidate.daemons[0].target = ConnectionTarget::Local { instance: None };
        assert!(candidate.install_blocker().is_some());
        candidate.daemons[0].managed = true;
        assert!(candidate.install_blocker().is_none());
        candidate.daemons[0].status = Err("Unauthenticated endpoint".into());
        assert!(candidate.install_blocker().is_some());
    }

    #[test]
    fn checking_newer_release_keeps_deferred_verified_candidate() {
        let mut snapshot = candidate();
        snapshot.deferred = true;
        let mut newer = snapshot.release.as_ref().unwrap().as_ref().clone();
        newer.manifest.version = "0.3.0".into();
        newer.manifest.daemon_protocol = 15;
        apply_checked_release(&mut snapshot, Some(newer));
        assert_eq!(
            snapshot
                .available_release
                .as_ref()
                .unwrap()
                .manifest
                .version,
            "0.3.0"
        );
        assert_eq!(snapshot.release.as_ref().unwrap().manifest.version, "0.2.0");
        assert_eq!(snapshot.prepared.as_ref().unwrap().version, "0.2.0");
        assert!(!snapshot.requires_daemon_restart(&daemon()));
        assert!(snapshot.deferred);
        apply_checked_release(&mut snapshot, None);
        assert_eq!(snapshot.release.as_ref().unwrap().manifest.version, "0.2.0");
        assert_eq!(snapshot.prepared.as_ref().unwrap().version, "0.2.0");
    }
}
