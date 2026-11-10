//! One process-wide updater. Checks, and optionally downloads, run automatically;
//! installing and restarting always wait for the user.
use crate::config::{LoadedConfig, UpdateSettings};
use crate::connection::ConnectionTarget;
use compi_protocol::{DaemonClient, LifecycleStatus, SurfaceId};
use compi_update::{
    AvailableRelease, Cancellation, InstallTarget, PreparedUpdate, UpdateEvent, UpdateManager,
    UpdatePhase,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Stable-named Setup on the latest release; the manual repair fallback.
pub const SETUP_DOWNLOAD_URL: &str =
    "https://github.com/cloudboy-jh/compi/releases/latest/download/Compi-Setup.exe";
const MAX_LOG_BYTES: u64 = 1024 * 1024;
/// Wait after a successful check before the next automatic one.
const CHECK_INTERVAL: u64 = 6 * 3_600;
/// Wait after a failed check before the next automatic one.
const RETRY_INTERVAL: u64 = 3_600;

#[derive(Clone, Debug)]
pub struct DaemonUpdateStatus {
    pub target: ConnectionTarget,
    /// Only daemon executables owned by the selected installation can be stopped.
    pub managed: bool,
    pub status: Result<LifecycleStatus, String>,
}

/// A user-visible updater operation; the affected-work review runs silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Check,
    Download,
    Install,
    /// Re-verifies the package retained by a rolled-back update.
    Reinstall,
    Repair,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub operation: Operation,
    /// The release a failed download or install was for; `None` for a check or repair.
    pub version: Option<String>,
    /// Short and plain; the full detail is in the update log.
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepairOutcome {
    Opened,
    /// No verified Setup could be fetched; the user downloads it by hand.
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preference {
    CheckForUpdates(bool),
    DownloadAutomatically(bool),
}

#[derive(Clone, Default)]
pub struct UpdateSnapshot {
    /// The candidate: the staged release once one is prepared.
    pub release: Option<Arc<AvailableRelease>>,
    /// Newest release found by the last check.
    pub available_release: Option<Arc<AvailableRelease>>,
    pub prepared: Option<PreparedUpdate>,
    pub progress: Option<UpdateEvent>,
    pub running: Option<Operation>,
    /// The install helper took over; this process is about to restart.
    pub installing: bool,
    pub failure: Option<Failure>,
    pub repair: Option<RepairOutcome>,
    pub preferences: UpdateSettings,
    pub daemons: Vec<DaemonUpdateStatus>,
    pub host_process_ids: Vec<u32>,
    pub recovery_available: bool,
    /// The service has read the staged update and last outcome from disk.
    restored: bool,
}

/// The single primary action of a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateButton {
    CheckNow,
    Download,
    Cancel,
    Restart,
    RestartEndShells,
    TryAgain,
    /// Opens the stable Setup download in the browser.
    DownloadSetup,
}

impl UpdateButton {
    /// `ending` is the number of shells `RestartEndShells` ends.
    pub fn label(self, ending: usize) -> String {
        match self {
            Self::CheckNow => "Check now".into(),
            Self::Download => "Download".into(),
            Self::Cancel => "Cancel".into(),
            Self::Restart => "Restart to update".into(),
            Self::RestartEndShells => format!("Restart and end {}", shell_count(ending)),
            Self::TryAgain => "Try again".into(),
            Self::DownloadSetup => "Download Setup".into(),
        }
    }
}

/// What Settings → Updates shows: the state, facts under it, and at most one action.
#[derive(Clone, Debug, PartialEq)]
pub struct UpdateView {
    pub status: String,
    /// When Compi last checked and, before a download, the download's size.
    pub facts: Option<String>,
    /// What installing the offered release ends or restarts, said before the restart.
    pub consequence: Option<String>,
    pub button: Option<UpdateButton>,
    /// Shells the restart ends, counted in the `RestartEndShells` label.
    pub ending: usize,
    /// Download fraction, only while bytes are arriving.
    pub progress: Option<f32>,
    pub failed: bool,
}

impl UpdateView {
    fn new(status: impl Into<String>, button: Option<UpdateButton>) -> Self {
        Self {
            status: status.into(),
            facts: None,
            consequence: None,
            button,
            ending: 0,
            progress: None,
            failed: false,
        }
    }

    pub fn button_label(&self) -> Option<String> {
        self.button.map(|button| button.label(self.ending))
    }
}

/// What the sidebar card offers besides Later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardAction {
    Update(UpdateButton),
    /// Opens Settings → Updates.
    Details,
}

/// The sidebar card: the offered release and its next step.
#[derive(Clone, Debug, PartialEq)]
pub struct UpdateCard {
    pub version: String,
    pub title: String,
    /// The notes' first summary line, the shells a restart ends, progress or the failure.
    pub line: Option<String>,
    pub progress: Option<f32>,
    /// The primary action first.
    pub actions: Vec<CardAction>,
    /// Shells the restart ends, counted in the `RestartEndShells` label.
    pub ending: usize,
    pub failed: bool,
    /// Later is offered: it hides the card and the dot for `version`.
    pub later: bool,
}

/// Whether a daemon reporting `status` must restart for the release `manifest`.
fn daemon_restart_needed(
    manifest: &compi_update::ReleaseManifest,
    status: &LifecycleStatus,
) -> bool {
    manifest.daemon_protocol != status.protocol_version
        || !manifest
            .qualified_daemon_versions
            .iter()
            .any(|version| version == &status.product_version)
}

/// Whether dotted numeric version `candidate` is above `than`; false when either isn't.
fn newer(candidate: &str, than: &str) -> bool {
    let parse = |version: &str| {
        version
            .split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .ok()
    };
    matches!((parse(candidate), parse(than)), (Some(a), Some(b)) if a > b)
}

fn shell_count(count: usize) -> String {
    format!("{count} {}", if count == 1 { "shell" } else { "shells" })
}

impl UpdateSnapshot {
    pub fn requires_daemon_restart(&self, status: &LifecycleStatus) -> bool {
        self.release
            .as_ref()
            .is_some_and(|release| daemon_restart_needed(&release.manifest, status))
    }
    // Used by desktop activation only; Linux has no in-app update activation.
    #[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
    fn replace_default_daemon(&self, remaining: &[LifecycleStatus]) -> bool {
        !remaining
            .iter()
            .any(|status| status.instance.is_none() && !self.requires_daemon_restart(status))
    }

    /// Why the staged update cannot be installed from here, as one sentence with the fix.
    pub fn install_blocker(&self) -> Option<&'static str> {
        if cfg!(not(any(windows, target_os = "macos"))) {
            return Some("Install updates with your system's package manager.");
        }
        for daemon in &self.daemons {
            match &daemon.status {
                Err(_) => {
                    return Some(
                        "Can't see which shells are running. Reopen Compi, then try again.",
                    );
                }
                Ok(status) if !daemon.managed && self.requires_daemon_restart(status) => {
                    return Some(
                        "A remote host runs an older Compi. Update Compi on that host first.",
                    );
                }
                _ => {}
            }
        }
        None
    }

    /// The staged update, unless a newer release has been found since.
    fn ready(&self) -> Option<&PreparedUpdate> {
        self.prepared.as_ref().filter(|prepared| {
            self.available_release
                .as_ref()
                .is_none_or(|available| available.manifest.version == prepared.version)
        })
    }

    pub fn ready_version(&self) -> Option<&str> {
        if self.installing || self.kept_failure().is_some() {
            return None;
        }
        self.ready().map(|prepared| prepared.version.as_str())
    }

    /// The version on offer: staged and current, else the newest found.
    fn offered_version(&self) -> Option<&str> {
        self.ready()
            .map(|prepared| prepared.version.as_str())
            .or_else(|| {
                self.available_release
                    .as_ref()
                    .map(|release| release.manifest.version.as_str())
            })
    }

    /// The failure still shown: a newer offer replaces a failure for an older version,
    /// and any offer replaces a failed check.
    pub fn kept_failure(&self) -> Option<&Failure> {
        let failure = self.failure.as_ref()?;
        match (failure.version.as_deref(), self.offered_version()) {
            (Some(failed), Some(offered)) if newer(offered, failed) => None,
            (None, Some(_)) if failure.operation == Operation::Check => None,
            _ => Some(failure),
        }
    }

    /// The release `operation` works on, recorded with its failure.
    fn operation_version(&self, operation: Operation) -> Option<String> {
        let release = match operation {
            Operation::Download => self.available_release.as_ref().or(self.release.as_ref()),
            Operation::Install | Operation::Reinstall => self.release.as_ref(),
            Operation::Check | Operation::Repair => None,
        };
        release.map(|release| release.manifest.version.clone())
    }

    /// The release whose notes the page offers: staged, else newest available.
    pub fn candidate(&self) -> Option<&AvailableRelease> {
        if self.installing {
            return None;
        }
        if self.ready().is_some() {
            self.release.as_deref()
        } else {
            self.available_release.as_deref()
        }
    }

    /// Live shells of local daemons that the staged release must restart.
    pub fn ending_shells(&self) -> Vec<SurfaceId> {
        self.ending_shells_by_daemon()
            .map(|(_, surface)| surface.clone())
            .collect()
    }

    /// `ending_shells`, each with the local daemon that runs it.
    pub fn ending_shells_by_daemon(&self) -> impl Iterator<Item = (&ConnectionTarget, &SurfaceId)> {
        self.daemons
            .iter()
            .filter(|daemon| daemon.managed)
            .filter_map(|daemon| Some((&daemon.target, daemon.status.as_ref().ok()?)))
            .filter(|(_, status)| self.requires_daemon_restart(status))
            .flat_map(|(target, status)| {
                status
                    .live_surfaces
                    .iter()
                    .map(move |surface| (target, &surface.surface_id))
            })
    }

    /// What installing the candidate ends or restarts, from the last look at the local
    /// daemons; `None` before one, or when nothing restarts.
    fn consequence(&self) -> Option<String> {
        let candidate = self.candidate()?;
        let mut restarts = false;
        let mut shells = 0;
        for status in self
            .daemons
            .iter()
            .filter(|daemon| daemon.managed)
            .filter_map(|daemon| daemon.status.as_ref().ok())
            .filter(|status| daemon_restart_needed(&candidate.manifest, status))
        {
            restarts = true;
            shells += status.live_surfaces.len();
        }
        match shells {
            0 if restarts => Some("Updating restarts Compi's background service".into()),
            0 => None,
            count => Some(format!("Updating ends {}", shell_count(count))),
        }
    }

    fn shell_names(
        shells: &[SurfaceId],
        name: impl Fn(&SurfaceId) -> Option<String>,
    ) -> Vec<String> {
        shells
            .iter()
            .map(|shell| name(shell).unwrap_or_else(|| "Terminal".into()))
            .collect()
    }

    /// `name` resolves a shell to its pane or tab label when this window knows it.
    pub fn view(&self, now: u64, name: impl Fn(&SurfaceId) -> Option<String>) -> UpdateView {
        if self.installing || self.running == Some(Operation::Install) {
            return UpdateView::new("Installing…", None);
        }
        match self.running {
            Some(Operation::Check) => return UpdateView::new("Checking for updates…", None),
            // The retained package is re-verified locally; nothing is downloaded.
            Some(Operation::Reinstall) => {
                return UpdateView::new("Verifying…", Some(UpdateButton::Cancel));
            }
            Some(Operation::Download) => {
                let expected = self
                    .available_release
                    .as_ref()
                    .or(self.release.as_ref())
                    .map(|release| release.manifest.artifact.size);
                return transfer_view("Downloading", self.progress.as_ref(), expected, true);
            }
            Some(Operation::Repair) => {
                return transfer_view(
                    "Downloading repair tool",
                    self.progress.as_ref(),
                    None,
                    false,
                );
            }
            Some(Operation::Install) | None => {}
        }
        match self.repair {
            Some(RepairOutcome::Opened) => return UpdateView::new("Repair opened in Setup", None),
            Some(RepairOutcome::Unavailable) => {
                return UpdateView::new(
                    "Couldn't get the repair tool · download Setup and choose Repair",
                    Some(UpdateButton::DownloadSetup),
                );
            }
            None => {}
        }
        let checked = self
            .preferences
            .last_check_unix
            .map(|last| format!("Checked {}", time_since(last, now)));
        if let Some(failure) = self.kept_failure() {
            return UpdateView {
                facts: checked,
                failed: true,
                ..UpdateView::new(failure.message.clone(), Some(UpdateButton::TryAgain))
            };
        }
        if let Some(prepared) = self.ready() {
            if let Some(blocker) = self.install_blocker() {
                return UpdateView::new(blocker, None);
            }
            let shells = self.ending_shells();
            if shells.is_empty() {
                return UpdateView {
                    facts: checked,
                    consequence: self.consequence(),
                    ..UpdateView::new(
                        format!("Compi {} is ready", prepared.version),
                        Some(UpdateButton::Restart),
                    )
                };
            }
            return UpdateView {
                facts: checked,
                ending: shells.len(),
                ..UpdateView::new(
                    ending_shells_status(&Self::shell_names(&shells, name)),
                    Some(UpdateButton::RestartEndShells),
                )
            };
        }
        if let Some(release) = &self.available_release {
            let size = format_size(release.manifest.artifact.size);
            return UpdateView {
                facts: Some(match checked {
                    Some(checked) => format!("{checked} · {size}"),
                    None => size,
                }),
                consequence: self.consequence(),
                ..UpdateView::new(
                    format!("Compi {} is available", release.manifest.version),
                    Some(UpdateButton::Download),
                )
            };
        }
        UpdateView {
            facts: checked,
            ..UpdateView::new(
                if self.preferences.last_check_unix.is_some() {
                    "Up to date"
                } else {
                    "Not checked yet"
                },
                Some(UpdateButton::CheckNow),
            )
        }
    }

    /// The sidebar card, unless Later was chosen for its version. Failed checks and
    /// repairs stay on the Updates page.
    pub fn card(
        &self,
        now: u64,
        later: Option<&str>,
        name: impl Fn(&SurfaceId) -> Option<String>,
    ) -> Option<UpdateCard> {
        if self.installing || self.running == Some(Operation::Install) {
            return None;
        }
        let summary = |release: &AvailableRelease| {
            crate::release_notes::of_release(&release.manifest)
                .summary
                .into_iter()
                .next()
        };
        let card = |version: &str, title: String, line, actions| UpdateCard {
            version: version.to_owned(),
            title,
            line,
            progress: None,
            actions,
            ending: 0,
            failed: false,
            later: true,
        };
        let card = if let Some(failure) = self.kept_failure() {
            let version = failure.version.as_deref()?;
            UpdateCard {
                failed: true,
                ..card(
                    version,
                    format!("Compi {version}"),
                    Some(failure.message.clone()),
                    vec![
                        CardAction::Update(UpdateButton::TryAgain),
                        CardAction::Details,
                    ],
                )
            }
        } else if let Some(operation @ (Operation::Download | Operation::Reinstall)) = self.running
        {
            let version = self.operation_version(operation)?;
            let view = self.view(now, &name);
            UpdateCard {
                progress: view.progress,
                later: false,
                ..card(
                    &version,
                    format!("Compi {version}"),
                    Some(view.status),
                    vec![CardAction::Update(UpdateButton::Cancel)],
                )
            }
        } else if let Some(prepared) = self.ready() {
            let title = format!("Compi {} is ready", prepared.version);
            let shells = self.ending_shells();
            if let Some(blocker) = self.install_blocker() {
                card(
                    &prepared.version,
                    title,
                    Some(blocker.into()),
                    vec![CardAction::Details],
                )
            } else if shells.is_empty() {
                card(
                    &prepared.version,
                    title,
                    self.release.as_deref().and_then(summary),
                    vec![CardAction::Update(UpdateButton::Restart)],
                )
            } else {
                UpdateCard {
                    ending: shells.len(),
                    ..card(
                        &prepared.version,
                        title,
                        Some(ending_shells_status(&Self::shell_names(&shells, &name))),
                        vec![CardAction::Update(UpdateButton::RestartEndShells)],
                    )
                }
            }
        } else {
            let release = self.available_release.as_deref()?;
            let version = &release.manifest.version;
            card(
                version,
                format!("Compi {version} is available"),
                summary(release),
                vec![CardAction::Update(UpdateButton::Download)],
            )
        };
        (later != Some(card.version.as_str())).then_some(card)
    }

    /// The title-bar dot: a release is on offer and Later wasn't chosen for it.
    pub fn shows_dot(&self, later: Option<&str>) -> bool {
        !self.installing
            && self.kept_failure().is_none()
            && self
                .offered_version()
                .is_some_and(|version| later != Some(version))
    }
}

fn transfer_view(
    label: &str,
    progress: Option<&UpdateEvent>,
    expected: Option<u64>,
    percent: bool,
) -> UpdateView {
    let (completed, total) = match progress {
        Some(event) if event.phase == UpdatePhase::Downloading => {
            (event.completed, event.total.or(expected))
        }
        // Anything after the last byte is verification or staging.
        Some(event) if event.phase != UpdatePhase::Checking => {
            return UpdateView::new("Verifying…", Some(UpdateButton::Cancel));
        }
        _ => (0, expected),
    };
    let Some(total) = total.filter(|total| *total > 0) else {
        return UpdateView::new(format!("{label}…"), Some(UpdateButton::Cancel));
    };
    let fraction = (completed as f64 / total as f64).clamp(0.0, 1.0);
    let mut status = format!("{label} · {}", format_transfer(completed, total, " / "));
    if percent {
        status.push_str(&format!(" · {}%", (fraction * 100.0).floor() as u32));
    }
    UpdateView {
        progress: Some(fraction as f32),
        ..UpdateView::new(status, Some(UpdateButton::Cancel))
    }
}

fn ending_shells_status(names: &[String]) -> String {
    let count = names.len();
    let mut status = format!(
        "Updating ends {count} {}: {}",
        if count == 1 { "shell" } else { "shells" },
        names
            .iter()
            .take(3)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    );
    if count > 3 {
        status.push_str(&format!(" +{} more", count - 3));
    }
    status
}

/// Decimal sizes: "812 KB", "13.2 MB".
pub fn format_size(bytes: u64) -> String {
    if bytes < 1_000 {
        return format!("{bytes} bytes");
    }
    let kilobytes = (bytes + 500) / 1_000;
    if kilobytes < 1_000 {
        return format!("{kilobytes} KB");
    }
    let tenths = (bytes + 50_000) / 100_000;
    format!("{}.{} MB", tenths / 10, tenths % 10)
}

/// "6.1 / 13.2 MB" with `joiner` " / ", in the unit of the total.
pub(crate) fn format_transfer(completed: u64, total: u64, joiner: &str) -> String {
    let completed = completed.min(total);
    if total >= 1_000_000 {
        let tenths = |bytes: u64| (bytes + 50_000) / 100_000;
        let (done, all) = (tenths(completed), tenths(total));
        format!(
            "{}.{}{joiner}{}.{} MB",
            done / 10,
            done % 10,
            all / 10,
            all % 10
        )
    } else {
        format!(
            "{}{joiner}{} KB",
            (completed + 500) / 1_000,
            (total + 500) / 1_000
        )
    }
}

/// "just now", "5 min ago", "3 h ago", "yesterday", then a date.
pub(crate) fn time_since(then: u64, now: u64) -> String {
    let elapsed = now.saturating_sub(then);
    match elapsed {
        0..60 => "just now".into(),
        60..3_600 => format!("{} min ago", elapsed / 60),
        3_600..86_400 => format!("{} h ago", elapsed / 3_600),
        86_400..172_800 => "yesterday".into(),
        _ => {
            let (year, month, day) = civil_date(then);
            let name = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ][month as usize - 1];
            if year == civil_date(now).0 {
                format!("on {name} {day}")
            } else {
                format!("on {name} {day}, {year}")
            }
        }
    }
}

/// UTC calendar date of a Unix timestamp (Howard Hinnant's days-from-civil inverse).
pub(crate) fn civil_date(unix: u64) -> (i64, u32, u32) {
    let days = (unix / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn plain_failure(operation: Operation, detail: &str) -> String {
    let head = match operation {
        Operation::Check => "Couldn't check for updates",
        Operation::Download => "Download failed",
        Operation::Install | Operation::Reinstall => "Update didn't install",
        Operation::Repair => "Couldn't get the repair tool",
    };
    let detail = detail.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| detail.contains(needle));
    let reason = if has(&[
        "error sending request",
        "connect",
        "dns",
        "timed out",
        "timeout",
        "network",
        "os error 10054",
    ]) {
        Some(if operation == Operation::Check {
            "no connection"
        } else {
            "connection lost"
        })
    } else if has(&["digest", "mismatch", "signature", "exceeds signed size"]) {
        Some("the file didn't verify")
    } else if has(&["access is denied", "permission denied", "os error 5)"]) {
        Some("permission denied")
    } else if has(&["not enough space", "no space", "disk full", "os error 112"]) {
        Some("disk full")
    } else {
        None
    };
    match reason {
        Some(reason) => format!("{head} · {reason}"),
        None => head.into(),
    }
}

/// Plain-text log next to the update journal; holds the detail the page leaves out.
pub fn update_log_path() -> Option<PathBuf> {
    let target = InstallTarget::detect().ok()?;
    Some(compi_update::operation_journal_path(&target).with_file_name("update.log"))
}

fn log(message: &str) {
    if let Some(path) = update_log_path() {
        let _ = append_log(&path, message);
    }
}

fn append_log(path: &Path, message: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::metadata(path).is_ok_and(|metadata| metadata.len() > MAX_LOG_BYTES) {
        fs::rename(path, path.with_extension("log.old"))?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{} {message}", now())
}

enum Request {
    Check,
    Download,
    /// Silent refresh of the affected-work review; `full` also asks every GUI host
    /// which remote targets its windows use.
    Review {
        full: bool,
    },
    Configure(LoadedConfig),
    /// Writes the in-memory preferences through the selected configuration.
    SavePreferences(LoadedConfig),
    /// `consented` are the shells the user saw listed when choosing to restart.
    Install {
        consented: Vec<SurfaceId>,
        command: Option<CommandInstall>,
    },
    Reinstall,
    Repair,
}

/// A restart `compi update` handed to this GUI host.
struct CommandInstall {
    version: String,
    outcome: PathBuf,
}

/// What happened to a restart handed over by `compi update`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CommandOutcome {
    /// The update helper took over; this is a handoff, not installed/reopened success.
    Started,
    /// More shells would end than the command listed; it asks again.
    ShellsChanged,
    Failed {
        message: String,
    },
}

impl Request {
    fn operation(&self) -> Option<Operation> {
        match self {
            Self::Check => Some(Operation::Check),
            Self::Download => Some(Operation::Download),
            Self::Install { .. } => Some(Operation::Install),
            Self::Reinstall => Some(Operation::Reinstall),
            Self::Repair => Some(Operation::Repair),
            Self::Review { .. } | Self::Configure(_) | Self::SavePreferences(_) => None,
        }
    }
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
    *READINESS.lock() = Some((path, token));
}

pub fn readiness_receipt() -> Option<(PathBuf, String)> {
    READINESS.lock().clone()
}

pub fn shared(config: &LoadedConfig, target: &ConnectionTarget) -> Arc<UpdateService> {
    let service = SERVICE.clone();
    let mut targets = service.targets.lock();
    let first = targets.is_empty();
    if !targets.contains(target) {
        targets.push(target.clone());
    }
    drop(targets);
    if first {
        service.state.lock().preferences = config.updates.clone();
        // Carries the selected path to the serialized background writer, never writes UI state.
        let _ = service.sender.send(Request::Configure(config.clone()));
        if check_due(&config.updates, now()) {
            service.check();
        }
        let _ = service.sender.send(Request::Review { full: false });
    }
    service
}

/// The updater for one `compi update`: no window registers a target, nothing runs
/// unless the command asks, and saved preferences stay untouched.
pub fn for_command() -> Arc<UpdateService> {
    let service = SERVICE.clone();
    service.state.lock().preferences = UpdateSettings {
        check_for_updates: false,
        download_updates_automatically: false,
        last_check_unix: None,
        last_check_failed_unix: None,
    };
    service
}

/// Where this GUI host reports on a restart handed over by `compi update`: a fresh
/// file beside the update journal. Leftovers of commands that ended with their shell
/// are removed.
pub fn command_outcome_path(prepared: &PreparedUpdate) -> Result<PathBuf, String> {
    let directory = prepared
        .journal_path
        .parent()
        .ok_or("Update journal has no parent")?;
    if let Ok(entries) = fs::read_dir(directory) {
        for entry in entries.flatten() {
            if command_outcome_name(&entry.file_name().to_string_lossy()) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    Ok(directory.join(format!(
        "cli-install-{}.json",
        compi_update::new_receipt_token()
    )))
}

fn command_outcome_name(name: &str) -> bool {
    name.len() <= 128 && name.starts_with("cli-install-") && name.ends_with(".json")
}

/// Reads and removes the GUI host's report, once written.
pub fn take_command_outcome(path: &Path) -> Option<CommandOutcome> {
    let outcome = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    let _ = fs::remove_file(path);
    Some(outcome)
}

fn write_command_outcome(path: &Path, outcome: &CommandOutcome) -> std::io::Result<()> {
    let partial = path.with_extension("partial");
    fs::write(&partial, serde_json::to_vec(outcome)?)?;
    fs::rename(partial, path)
}

/// `compi update` handed its restart to this GUI host: install the update it staged,
/// provided the shells it listed are still all that a restart ends.
pub fn install_for_command(
    version: String,
    consented: Vec<SurfaceId>,
    outcome: PathBuf,
) -> Result<(), String> {
    let target = InstallTarget::detect().map_err(|error| error.to_string())?;
    if outcome.parent() != compi_update::operation_journal_path(&target).parent()
        || !outcome
            .file_name()
            .is_some_and(|name| command_outcome_name(&name.to_string_lossy()))
    {
        return Err("compi update must report beside this installation's update journal".into());
    }
    let accepted = SERVICE.request(Request::Install {
        consented,
        command: Some(CommandInstall {
            version,
            outcome: outcome.clone(),
        }),
    });
    if !accepted {
        let busy = CommandOutcome::Failed {
            message: "Compi is busy with another update step. Try again in a moment.".into(),
        };
        write_command_outcome(&outcome, &busy).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Takes over the update a command downloaded, which this process has not seen.
fn adopt_staged(state: &Mutex<UpdateSnapshot>, version: &str) -> Result<(), String> {
    let target = InstallTarget::detect().map_err(|error| error.to_string())?;
    let prepared = compi_update::pending_update(&target)
        .map_err(|error| error.to_string())?
        .filter(|prepared| prepared.version == version)
        .ok_or("The downloaded update changed. Run compi update again.")?;
    let journal = compi_update::operation_status(&target)
        .map_err(|error| error.to_string())?
        .ok_or("The downloaded update has no journal")?;
    let release = Arc::new(verified_journal_release(&journal)?);
    let mut snapshot = state.lock();
    snapshot.available_release = Some(release.clone());
    snapshot.release = Some(release);
    snapshot.prepared = Some(prepared);
    snapshot.recovery_available = false;
    Ok(())
}

/// An automatic check is due 6 h after the last successful one, or 1 h after a failed
/// one; never while checks are off. A clock set back waits until it passes them again.
fn check_due(settings: &UpdateSettings, now: u64) -> bool {
    if !settings.check_for_updates {
        return false;
    }
    let failed = settings
        .last_check_failed_unix
        .filter(|failed| settings.last_check_unix.is_none_or(|last| *failed >= last));
    match failed {
        Some(failed) => now >= failed.saturating_add(RETRY_INTERVAL),
        None => settings
            .last_check_unix
            .is_none_or(|last| now >= last.saturating_add(CHECK_INTERVAL)),
    }
}

/// Removes what finished updates and Setup runs left behind; logs, never shows.
fn prune_installation() {
    let result = InstallTarget::detect()
        .map_err(|error| error.to_string())
        .and_then(|target| compi_update::prune(&target).map_err(|error| error.to_string()));
    if let Err(error) = result {
        log(&format!("Removing old update files failed: {error}"));
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn manager(slot: &mut Option<UpdateManager>) -> Result<&mut UpdateManager, String> {
    if slot.is_none() {
        *slot = Some(UpdateManager::new().map_err(|error| error.to_string())?);
    }
    Ok(slot.as_mut().expect("manager initialized"))
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
            let mut manager_slot = None;
            let mut config: Option<LoadedConfig> = None;
            if let Err(error) = restore_pending(&state) {
                log(&format!("Restoring update state failed: {error}"));
            }
            state.lock().restored = true;
            if readiness_receipt().is_some() {
                let refreshed = state.clone();
                thread::spawn(move || {
                    // The worker finalizes only after all new GUI hosts actually attach.
                    let deadline = std::time::Instant::now() + Duration::from_secs(180);
                    loop {
                        thread::sleep(Duration::from_secs(2));
                        if let Err(error) = restore_pending(&refreshed) {
                            log(&format!("Confirming reopened update failed: {error}"));
                            let mut snapshot = refreshed.lock();
                            snapshot.installing = false;
                            snapshot.failure = Some(Failure {
                                operation: Operation::Install,
                                version: snapshot.operation_version(Operation::Install),
                                message: "Couldn't confirm that the update installed and reopened"
                                    .into(),
                            });
                            break;
                        }
                        let mut snapshot = refreshed.lock();
                        if !snapshot.installing {
                            break;
                        }
                        if std::time::Instant::now() >= deadline {
                            log("Update completion was not confirmed within 180 seconds");
                            snapshot.installing = false;
                            snapshot.failure = Some(Failure {
                                operation: Operation::Install,
                                version: snapshot.operation_version(Operation::Install),
                                message: "Update installation and reopening weren't confirmed"
                                    .into(),
                            });
                            break;
                        }
                    }
                });
            }
            // Once per process, when no update step runs or waits.
            let mut pruned = false;
            loop {
                if !pruned {
                    let idle = {
                        let snapshot = state.lock();
                        snapshot.running.is_none() && !snapshot.installing
                    };
                    if idle {
                        pruned = true;
                        prune_installation();
                    }
                }
                let request = match receiver.recv_timeout(Duration::from_secs(60)) {
                    Ok(request) => request,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let mut snapshot = state.lock();
                        if snapshot.running.is_some()
                            || snapshot.installing
                            || !check_due(&snapshot.preferences, now())
                        {
                            continue;
                        }
                        snapshot.running = Some(Operation::Check);
                        drop(snapshot);
                        if let Some(service) = weak.upgrade() {
                            *service.cancellation.lock() = Cancellation::default();
                        }
                        Request::Check
                    }
                };
                let Some(service) = weak.upgrade() else { break };
                let request = match request {
                    Request::Configure(selected) => {
                        config = Some(selected);
                        continue;
                    }
                    Request::SavePreferences(mut selected) => {
                        let settings = state.lock().preferences.clone();
                        if let Err(error) = selected.save_update_settings(settings) {
                            log(&format!("Saving update settings failed: {error}"));
                        }
                        config = Some(selected);
                        continue;
                    }
                    request => request,
                };
                let operation = request.operation();
                let cancellation = service.cancellation.lock().clone();
                let progress_state = state.clone();
                let report = move |event| progress_state.lock().progress = Some(event);
                let mut follow_up = None;
                let result = (|| -> Result<(), String> {
                    match request {
                        Request::Check => {
                            let release = manager(&mut manager_slot)?
                                .check(&cancellation, report)
                                .map_err(|error| error.to_string())?;
                            log(&match &release {
                                Some(release) => {
                                    format!("Check: {} available", release.manifest.version)
                                }
                                None => "Check: up to date".into(),
                            });
                            let mut snapshot = state.lock();
                            apply_checked_release(&mut snapshot, release);
                            let offered =
                                snapshot.available_release.is_some() && snapshot.ready().is_none();
                            if offered && snapshot.preferences.download_updates_automatically {
                                follow_up = Some(Request::Download);
                            }
                            drop(snapshot);
                            // The page says what updating ends before anything downloads.
                            if offered
                                && follow_up.is_none()
                                && let Err(error) = review(&state, &targets, false, &cancellation)
                            {
                                log(&format!("Reviewing affected work failed: {error}"));
                            }
                        }
                        Request::Download => {
                            let release = {
                                let snapshot = state.lock();
                                snapshot
                                    .available_release
                                    .as_ref()
                                    .or(snapshot.release.as_ref())
                                    .cloned()
                                    .ok_or("No release to download; check first")?
                            };
                            let target =
                                InstallTarget::detect().map_err(|error| error.to_string())?;
                            let prepared = manager(&mut manager_slot)?
                                .prepare(&release, &target, &cancellation, report)
                                .map_err(|error| error.to_string())?;
                            log(&format!(
                                "Download: {} verified and ready",
                                release.manifest.version
                            ));
                            {
                                let mut snapshot = state.lock();
                                snapshot.prepared = Some(prepared);
                                snapshot.release = Some(release);
                                snapshot.recovery_available = false;
                            }
                            // The page names the shells a restart would end.
                            if let Err(error) = review(&state, &targets, true, &cancellation) {
                                log(&format!("Reviewing affected work failed: {error}"));
                            }
                        }
                        Request::Review { full } => {
                            if let Err(error) = review(&state, &targets, full, &cancellation) {
                                log(&format!("Reviewing affected work failed: {error}"));
                            }
                        }
                        Request::Install { consented, command } => {
                            let outcome = (|| -> Result<CommandOutcome, String> {
                                if let Some(command) = &command {
                                    adopt_staged(&state, &command.version)?;
                                }
                                review(&state, &targets, true, &cancellation)?;
                                let snapshot = state.lock().clone();
                                if let Some(blocker) = snapshot.install_blocker() {
                                    log(&format!("Install blocked: {blocker}"));
                                    return Ok(CommandOutcome::Failed {
                                        message: blocker.into(),
                                    });
                                }
                                if snapshot
                                    .ending_shells()
                                    .iter()
                                    .any(|shell| !consented.contains(shell))
                                {
                                    // The page now lists the extra shells; the next click consents.
                                    log("Install paused: more shells would end than were shown");
                                    return Ok(CommandOutcome::ShellsChanged);
                                }
                                activate(
                                    manager(&mut manager_slot)?,
                                    &snapshot,
                                    &cancellation,
                                    report,
                                )?;
                                log(&format!(
                                    "Install: helper started for {}",
                                    snapshot
                                        .prepared
                                        .as_ref()
                                        .map_or("", |prepared| prepared.version.as_str())
                                ));
                                state.lock().installing = true;
                                Ok(CommandOutcome::Started)
                            })();
                            if let Some(command) = &command {
                                let reported = match &outcome {
                                    Ok(outcome) => outcome.clone(),
                                    Err(detail) => CommandOutcome::Failed {
                                        message: detail.clone(),
                                    },
                                };
                                if let Err(error) =
                                    write_command_outcome(&command.outcome, &reported)
                                {
                                    log(&format!("Reporting to compi update failed: {error}"));
                                }
                            }
                            outcome?;
                        }
                        Request::Reinstall => {
                            let target =
                                InstallTarget::detect().map_err(|error| error.to_string())?;
                            let prepared =
                                compi_update::reinstall_verified(&target, &cancellation, report)
                                    .map_err(|error| error.to_string())?;
                            let journal = compi_update::operation_status(&target)
                                .map_err(|error| error.to_string())?
                                .ok_or("Verified retry did not retain an operation journal")?;
                            let release = verified_journal_release(&journal)?;
                            log(&format!(
                                "Reinstall: {} re-verified and ready",
                                release.manifest.version
                            ));
                            {
                                let mut snapshot = state.lock();
                                snapshot.prepared = Some(prepared);
                                snapshot.release = Some(Arc::new(release));
                                snapshot.recovery_available = false;
                            }
                            if let Err(error) = review(&state, &targets, true, &cancellation) {
                                log(&format!("Reviewing affected work failed: {error}"));
                            }
                        }
                        Request::Repair => {
                            let setup = repair(manager(&mut manager_slot)?, &cancellation, report)?;
                            log(&format!("Repair: opened {}", setup.display()));
                            state.lock().repair = Some(RepairOutcome::Opened);
                        }
                        Request::Configure(_) | Request::SavePreferences(_) => unreachable!(),
                    }
                    Ok(())
                })();
                let Some(operation) = operation else {
                    continue;
                };
                let mut snapshot = state.lock();
                snapshot.running = None;
                snapshot.progress = None;
                let succeeded = result.is_ok();
                if let Err(detail) = result {
                    if cancellation.is_cancelled() {
                        log(&format!("{operation:?} cancelled: {detail}"));
                    } else {
                        log(&format!("{operation:?} failed: {detail}"));
                        if operation == Operation::Repair {
                            snapshot.repair = Some(RepairOutcome::Unavailable);
                        } else if operation != Operation::Check
                            || snapshot
                                .failure
                                .as_ref()
                                .is_none_or(|failure| failure.version.is_none())
                        {
                            // A failed update stays in view over a failed check.
                            snapshot.failure = Some(Failure {
                                operation,
                                version: snapshot.operation_version(operation),
                                message: plain_failure(operation, &detail),
                            });
                        }
                    }
                }
                if operation == Operation::Check {
                    let checked = Some(now());
                    if succeeded {
                        snapshot.preferences.last_check_unix = checked;
                        snapshot.preferences.last_check_failed_unix = None;
                    } else {
                        snapshot.preferences.last_check_failed_unix = checked;
                    }
                    let preferences = snapshot.preferences.clone();
                    drop(snapshot);
                    if let Some(config) = config.as_mut()
                        && let Err(error) = config.save_update_settings(preferences)
                    {
                        log(&format!("Saving update settings failed: {error}"));
                    }
                } else {
                    drop(snapshot);
                }
                if let Some(follow_up) = follow_up {
                    service.request(follow_up);
                }
            }
        });
        service
    }

    pub fn snapshot(&self) -> UpdateSnapshot {
        self.state.lock().clone()
    }

    /// Whether the request was queued; operations wait for the one running.
    fn request(&self, request: Request) -> bool {
        let Some(operation) = request.operation() else {
            return self.sender.send(request).is_ok();
        };
        let mut snapshot = self.state.lock();
        if snapshot.running.is_some() || snapshot.installing {
            return false;
        }
        snapshot.running = Some(operation);
        // A check keeps a failed update; only a newer release found replaces it.
        if operation != Operation::Check
            || snapshot
                .failure
                .as_ref()
                .is_none_or(|failure| failure.version.is_none())
        {
            snapshot.failure = None;
        }
        snapshot.repair = None;
        snapshot.progress = None;
        *self.cancellation.lock() = Cancellation::default();
        drop(snapshot);
        if let Err(error) = self.sender.send(request) {
            log(&format!("{operation:?} failed: {error}"));
            let mut snapshot = self.state.lock();
            snapshot.running = None;
            snapshot.failure = Some(Failure {
                operation,
                version: snapshot.operation_version(operation),
                message: plain_failure(operation, ""),
            });
            return false;
        }
        true
    }

    pub fn check(&self) {
        self.request(Request::Check);
    }

    pub fn download(&self) {
        self.request(Request::Download);
    }

    /// Restarts into the staged update; `consented` are the shells the page listed.
    pub fn install(&self, consented: Vec<SurfaceId>) {
        self.request(Request::Install {
            consented,
            command: None,
        });
    }

    /// Runs a check, download or retained-package retry for `compi update` and waits
    /// for it; `observe` sees the state while it runs.
    pub fn run(
        &self,
        operation: Operation,
        mut observe: impl FnMut(&UpdateSnapshot),
    ) -> Result<UpdateSnapshot, String> {
        let request = match operation {
            Operation::Check => Request::Check,
            Operation::Download => Request::Download,
            Operation::Reinstall => Request::Reinstall,
            _ => return Err(format!("{operation:?} does not run from a command")),
        };
        while {
            let snapshot = self.state.lock();
            !snapshot.restored || snapshot.running.is_some()
        } {
            thread::sleep(Duration::from_millis(20));
        }
        // A failure restored from the last update must not pass for this operation's.
        self.state.lock().failure = None;
        if !self.request(request) {
            return Err(plain_failure(operation, ""));
        }
        loop {
            let snapshot = self.snapshot();
            if snapshot.running.is_none() {
                return Ok(snapshot);
            }
            observe(&snapshot);
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Refreshes which local shells a restart would end, for a command waiting on it.
    pub fn review_now(&self) -> Result<UpdateSnapshot, String> {
        review(&self.state, &self.targets, false, &Cancellation::default())?;
        Ok(self.snapshot())
    }

    /// Re-runs whatever failed. A failed install returns to the Ready state with a fresh
    /// review instead of restarting without a new click.
    pub fn retry(&self) {
        let mut snapshot = self.state.lock();
        let Some(failure) = snapshot.kept_failure().cloned() else {
            return;
        };
        match failure.operation {
            Operation::Check | Operation::Repair => {
                drop(snapshot);
                self.check();
            }
            Operation::Download => {
                drop(snapshot);
                self.download();
            }
            Operation::Reinstall => {
                drop(snapshot);
                self.request(Request::Reinstall);
            }
            Operation::Install => {
                snapshot.failure = None;
                drop(snapshot);
                self.request(Request::Review { full: true });
            }
        }
    }

    pub fn repair(&self) {
        self.request(Request::Repair);
    }

    /// Takes effect at once; the file is written when the worker is free.
    pub fn set_preference(&self, config: LoadedConfig, preference: Preference) {
        let mut snapshot = self.state.lock();
        match preference {
            Preference::CheckForUpdates(enabled) => {
                snapshot.preferences.check_for_updates = enabled
            }
            Preference::DownloadAutomatically(enabled) => {
                snapshot.preferences.download_updates_automatically = enabled
            }
        }
        drop(snapshot);
        let _ = self.sender.send(Request::SavePreferences(config));
    }

    pub fn cancel(&self) {
        self.cancellation.lock().cancel();
    }

    /// The title-bar dot; `later` is the version Later was chosen for.
    pub fn shows_dot(&self, later: Option<&str>) -> bool {
        self.state.lock().shows_dot(later)
    }

    /// Refreshes which shells a restart would end; called when the page opens.
    pub fn refresh_review(&self) {
        let snapshot = self.state.lock();
        if (snapshot.prepared.is_some() || snapshot.available_release.is_some())
            && snapshot.running.is_none()
            && !snapshot.installing
        {
            drop(snapshot);
            self.request(Request::Review { full: false });
        }
    }
}

/// Asks the daemons a restart concerns for their state. Before a download only local
/// daemons are asked, for the consequence the page states; remote hosts keep what an
/// earlier review found.
fn review(
    state: &Mutex<UpdateSnapshot>,
    targets: &Mutex<Vec<ConnectionTarget>>,
    full: bool,
    cancellation: &Cancellation,
) -> Result<(), String> {
    let (prepared, mut registered, remote) = {
        let snapshot = state.lock();
        if snapshot.prepared.is_none() && snapshot.available_release.is_none() {
            return Ok(());
        }
        let remote: Vec<DaemonUpdateStatus> = snapshot
            .daemons
            .iter()
            .filter(|daemon| !daemon.managed)
            .cloned()
            .collect();
        let mut registered = targets.lock().clone();
        // A quick refresh keeps remote targets found by an earlier full review.
        for daemon in &remote {
            if !registered.contains(&daemon.target) {
                registered.push(daemon.target.clone());
            }
        }
        (snapshot.prepared.clone(), registered, remote)
    };
    let root = match &prepared {
        Some(prepared) => prepared.target.root.clone(),
        None => {
            InstallTarget::detect()
                .map_err(|error| error.to_string())?
                .root
        }
    };
    if full && let Some(prepared) = &prepared {
        for target in inspect_host_targets(prepared, cancellation)? {
            if !registered.contains(&target) {
                registered.push(target);
            }
        }
    }
    let mut daemons = Vec::new();
    match DaemonClient::local_lifecycle_statuses_for_install(&root) {
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
    if prepared.is_some() {
        for target in registered {
            if daemons.iter().any(|daemon| daemon.target == target) {
                continue;
            }
            let status = target.lifecycle_status().map_err(|error| error.to_string());
            daemons.push(DaemonUpdateStatus {
                target,
                managed: false,
                status,
            });
        }
    } else {
        daemons.extend(remote);
    }
    for daemon in &daemons {
        if let Err(error) = &daemon.status {
            log(&format!("Cannot inspect daemon: {error}"));
        }
    }
    let hosts = match prepared {
        Some(_) => Some(affected_host_ids()?),
        None => None,
    };
    let mut snapshot = state.lock();
    snapshot.daemons = daemons;
    if let Some(hosts) = hosts {
        snapshot.host_process_ids = hosts;
    }
    Ok(())
}

#[cfg(windows)]
fn repair(
    manager: &mut UpdateManager,
    cancellation: &Cancellation,
    report: impl FnMut(UpdateEvent),
) -> Result<PathBuf, String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};
    let release = manager
        .latest_setup_release(cancellation)
        .map_err(|error| error.to_string())?;
    let directory = std::env::temp_dir().join("compi-repair");
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let setup = compi_update::download_verified_setup(&release, &directory, cancellation, report)
        .map_err(|error| error.to_string())?;
    if cancellation.is_cancelled() {
        return Err("Repair cancelled before Setup started".into());
    }
    // The shell honors Setup's elevation manifest, unlike CreateProcess.
    let file = HSTRING::from(setup.as_os_str());
    let instance = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &file,
            w!("--repair"),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if instance.0 as isize <= 32 {
        return Err(format!(
            "Setup did not start (ShellExecute {})",
            instance.0 as isize
        ));
    }
    Ok(setup)
}

#[cfg(not(windows))]
fn repair(
    _: &mut UpdateManager,
    _: &Cancellation,
    _: impl FnMut(UpdateEvent),
) -> Result<PathBuf, String> {
    Err("Repair through Setup is available on Windows only".into())
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
        let replace_default_daemon = snapshot.replace_default_daemon(&current);
        compi_update::preflight_activation(prepared, replace_default_daemon)
            .map_err(|error| error.to_string())?;
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
            replace_default_daemon,
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

fn verified_journal_release(journal: &compi_update::Journal) -> Result<AvailableRelease, String> {
    let config = compi_update::ReleaseConfig::compiled().map_err(|error| error.to_string())?;
    compi_update::verify_manifest(
        &serde_json::to_vec(&journal.signed).map_err(|error| error.to_string())?,
        &config,
    )
    .map_err(|error| error.to_string())
}

fn restore_pending(state: &Mutex<UpdateSnapshot>) -> Result<(), String> {
    let target = InstallTarget::detect().map_err(|error| error.to_string())?;
    let Some(mut journal) =
        compi_update::operation_status(&target).map_err(|error| error.to_string())?
    else {
        return Ok(());
    };
    // Surface a persisted failure even if verifying the retained package now fails.
    restore_journal_status(&mut state.lock(), &journal);
    let prepared = if readiness_receipt().is_none() {
        let prepared = compi_update::pending_update(&target).map_err(|error| error.to_string())?;
        // Recovery may have changed an interrupted transaction to RolledBack.
        journal = compi_update::operation_status(&target)
            .map_err(|error| error.to_string())?
            .ok_or("Update journal disappeared during recovery")?;
        prepared
    } else {
        None
    };
    if let Some(error) = &journal.error {
        log(&format!(
            "Last update failed: {error}. Journal: {}",
            compi_update::operation_journal_path(&target).display()
        ));
    }
    let release = if prepared.is_some() || journal.phase == compi_update::JournalStage::RolledBack {
        Some(Arc::new(verified_journal_release(&journal)?))
    } else {
        None
    };
    let mut snapshot = state.lock();
    restore_journal_status(&mut snapshot, &journal);
    if release.is_some() {
        snapshot.release = release;
        snapshot.prepared = prepared;
    }
    Ok(())
}

fn restore_journal_status(snapshot: &mut UpdateSnapshot, journal: &compi_update::Journal) {
    use compi_update::JournalStage;
    snapshot.recovery_available = journal.phase == JournalStage::RolledBack;
    snapshot.installing = journal.error.is_none()
        && matches!(
            journal.phase,
            JournalStage::Activating | JournalStage::Activated | JournalStage::Relaunching
        );
    if journal.error.is_some() || snapshot.recovery_available {
        let version = Some(journal.manifest.version.clone());
        snapshot.failure = Some(if snapshot.recovery_available {
            Failure {
                operation: Operation::Reinstall,
                version,
                message: "Last update didn't install · Compi was restored".into(),
            }
        } else {
            Failure {
                operation: Operation::Install,
                version,
                message: "Last update didn't finish installing and reopening Compi".into(),
            }
        });
    } else if journal.phase == JournalStage::Complete {
        snapshot.failure = None;
    }
}

fn apply_checked_release(snapshot: &mut UpdateSnapshot, release: Option<AvailableRelease>) {
    let release = release.map(Arc::new);
    // A successful check retires a failed check, and a newer release a failed update.
    let retired =
        snapshot
            .failure
            .as_ref()
            .is_some_and(|failure| match (&failure.version, &release) {
                (None, _) => failure.operation == Operation::Check,
                (Some(failed), Some(found)) => newer(&found.manifest.version, failed),
                (Some(_), None) => false,
            });
    if retired {
        snapshot.failure = None;
    }
    snapshot.available_release = release.clone();
    // A check never throws away a downloaded candidate; Download replaces it explicitly.
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
    fn checks_are_due_six_hours_after_success_and_one_hour_after_failure() {
        assert!(check_due(&UpdateSettings::default(), 0));
        let mut settings = UpdateSettings {
            last_check_unix: Some(1_000),
            ..UpdateSettings::default()
        };
        assert!(!check_due(&settings, 1_000 + 5 * 3_600 + 59 * 60));
        assert!(check_due(&settings, 1_000 + 6 * 3_600));
        // A clock set back waits.
        assert!(!check_due(&settings, 999));
        settings.last_check_failed_unix = Some(50_000);
        assert!(!check_due(&settings, 50_000 + 59 * 60));
        assert!(check_due(&settings, 50_000 + 60 * 60));
        // A failure before the last success no longer counts.
        settings.last_check_unix = Some(60_000);
        assert!(!check_due(&settings, 60_000 + 3_600));
        assert!(check_due(&settings, 60_000 + 6 * 3_600));
        settings.check_for_updates = false;
        assert!(!check_due(&settings, 60_000 + 6 * 3_600));
        settings.last_check_unix = None;
        settings.last_check_failed_unix = None;
        assert!(!check_due(&settings, 0));
    }

    fn release(version: &str, size: u64) -> Arc<AvailableRelease> {
        Arc::new(AvailableRelease {
            manifest: compi_update::ReleaseManifest {
                schema: 1,
                product: "compi".into(),
                version: version.into(),
                platform: "windows-x86_64".into(),
                minimum_os: "10".into(),
                release_notes: String::new(),
                daemon_protocol: 14,
                qualified_daemon_versions: vec!["0.1.3".into()],
                minimum_persistence: 1,
                artifact: compi_update::Artifact {
                    url: String::new(),
                    size,
                    sha256: String::new(),
                },
                setup: None,
            },
            manifest_bytes: Vec::new(),
            signature: String::new(),
        })
    }

    fn candidate() -> UpdateSnapshot {
        UpdateSnapshot {
            release: Some(release("0.2.0", 13_200_000)),
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

    fn status_journal(phase: compi_update::JournalStage) -> compi_update::Journal {
        let snapshot = candidate();
        let prepared = snapshot.prepared.unwrap();
        compi_update::Journal {
            schema: 1,
            attempt: "a".repeat(32),
            target: prepared.target,
            manifest: snapshot.release.unwrap().manifest.clone(),
            signed: compi_update::SignedManifest {
                payload: String::new(),
                signature: String::new(),
            },
            stage: prepared.stage,
            prior_selection: None,
            phase,
            backup: None,
            retained_backups: Vec::new(),
            prior_task: None,
            error: None,
        }
    }

    #[test]
    fn reopened_gui_keeps_installing_until_journal_completes() {
        let mut snapshot = UpdateSnapshot::default();
        for phase in [
            compi_update::JournalStage::Activating,
            compi_update::JournalStage::Activated,
            compi_update::JournalStage::Relaunching,
        ] {
            restore_journal_status(&mut snapshot, &status_journal(phase));
            let view = snapshot.view(0, |_| None);
            assert!(snapshot.installing);
            assert_eq!(view.button, None);
            assert!(!view.failed);
        }
        restore_journal_status(
            &mut snapshot,
            &status_journal(compi_update::JournalStage::Complete),
        );
        assert!(!snapshot.installing);
        assert!(!snapshot.view(0, |_| None).failed);
    }

    #[test]
    fn persisted_prepared_and_rollback_failures_are_visible_in_gui() {
        let mut snapshot = candidate();
        for phase in [
            compi_update::JournalStage::Prepared,
            compi_update::JournalStage::Relaunching,
            compi_update::JournalStage::RolledBack,
        ] {
            let mut journal = status_journal(phase);
            journal.error = Some("Task ownership or GUI reopening failed".into());
            snapshot.installing = true;
            restore_journal_status(&mut snapshot, &journal);
            let view = snapshot.view(0, |_| None);
            assert!(!snapshot.installing);
            assert!(view.failed);
            assert_eq!(view.button, Some(UpdateButton::TryAgain));
            assert_eq!(
                snapshot.recovery_available,
                phase == compi_update::JournalStage::RolledBack
            );
        }
        restore_journal_status(
            &mut snapshot,
            &status_journal(compi_update::JournalStage::RolledBack),
        );
        assert!(snapshot.view(0, |_| None).failed);
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

    /// A local daemon the staged release must restart, running `shells` live shells.
    #[cfg(any(windows, target_os = "macos"))]
    fn ending(shells: usize) -> DaemonUpdateStatus {
        let mut status = daemon();
        status.protocol_version = 15;
        status.live_surfaces = (0..shells)
            .map(|index| compi_protocol::LiveSurface {
                surface_id: SurfaceId::new(format!("s{index}")),
                process_lifetime_id: compi_protocol::ProcessLifetimeId::new("lifetime"),
                status: compi_protocol::SurfaceStatus::Running,
            })
            .collect();
        DaemonUpdateStatus {
            target: ConnectionTarget::Local { instance: None },
            managed: true,
            status: Ok(status),
        }
    }

    fn shown(snapshot: &UpdateSnapshot) -> (String, Option<UpdateButton>) {
        let names = |id: &SurfaceId| {
            let index: usize = id.as_str()[1..].parse().unwrap();
            (index != 1).then(|| ["pwsh", "", "vim", "cargo", "ssh"][index].to_owned())
        };
        let view = snapshot.view(10_000, names);
        (view.status, view.button)
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

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn unqualified_remote_or_unreadable_daemon_blocks_with_one_plain_sentence() {
        let mut candidate = candidate();
        let mut daemon = daemon();
        daemon.protocol_version = 15;
        candidate.daemons.push(DaemonUpdateStatus {
            target: ConnectionTarget::from_options(None, Some("example.test".into())).unwrap(),
            managed: false,
            status: Ok(daemon),
        });
        assert_eq!(
            shown(&candidate),
            (
                "A remote host runs an older Compi. Update Compi on that host first.".into(),
                None
            )
        );
        candidate.daemons[0].managed = true;
        assert!(candidate.install_blocker().is_none());
        candidate.daemons[0].status = Err("Unauthenticated endpoint".into());
        assert_eq!(
            shown(&candidate),
            (
                "Can't see which shells are running. Reopen Compi, then try again.".into(),
                None
            )
        );
    }

    #[test]
    fn idle_available_and_checking_states() {
        let mut snapshot = UpdateSnapshot::default();
        assert_eq!(
            shown(&snapshot),
            ("Not checked yet".into(), Some(UpdateButton::CheckNow))
        );
        snapshot.preferences.last_check_unix = Some(10_000 - 300);
        assert_eq!(
            shown(&snapshot),
            ("Up to date".into(), Some(UpdateButton::CheckNow))
        );
        assert_eq!(
            snapshot.view(10_000, |_| None).facts.as_deref(),
            Some("Checked 5 min ago")
        );
        snapshot.running = Some(Operation::Check);
        assert_eq!(shown(&snapshot), ("Checking for updates…".into(), None));
        snapshot.running = None;
        snapshot.available_release = Some(release("0.1.7", 13_200_000));
        let view = snapshot.view(10_000, |_| None);
        assert_eq!(
            (view.status.as_str(), view.facts.as_deref(), view.button),
            (
                "Compi 0.1.7 is available",
                Some("Checked 5 min ago · 13.2 MB"),
                Some(UpdateButton::Download)
            )
        );
        // No daemon seen yet: nothing to say about what updating ends.
        assert_eq!(view.consequence, None);
        let mut daemon = daemon();
        daemon.protocol_version = 15;
        snapshot.daemons.push(DaemonUpdateStatus {
            target: ConnectionTarget::Local { instance: None },
            managed: true,
            status: Ok(daemon),
        });
        assert_eq!(
            snapshot.view(10_000, |_| None).consequence.as_deref(),
            Some("Updating restarts Compi's background service")
        );
    }

    #[test]
    fn downloading_reports_bytes_then_verifying() {
        let mut snapshot = UpdateSnapshot {
            available_release: Some(release("0.1.7", 13_200_000)),
            running: Some(Operation::Download),
            ..UpdateSnapshot::default()
        };
        let view = snapshot.view(0, |_| None);
        assert_eq!(view.status, "Downloading · 0.0 / 13.2 MB · 0%");
        assert_eq!(view.button, Some(UpdateButton::Cancel));
        assert_eq!(view.progress, Some(0.0));
        snapshot.progress = Some(UpdateEvent {
            phase: UpdatePhase::Downloading,
            completed: 6_100_000,
            total: Some(13_200_000),
            message: String::new(),
        });
        let view = snapshot.view(0, |_| None);
        assert_eq!(view.status, "Downloading · 6.1 / 13.2 MB · 46%");
        assert!(
            view.progress
                .is_some_and(|fraction| (0.46..0.47).contains(&fraction))
        );
        snapshot.progress.as_mut().unwrap().phase = UpdatePhase::Verifying;
        let view = snapshot.view(0, |_| None);
        assert_eq!(
            (view.status.as_str(), view.button, view.progress),
            ("Verifying…", Some(UpdateButton::Cancel), None)
        );
        snapshot.running = Some(Operation::Repair);
        snapshot.progress = Some(UpdateEvent {
            phase: UpdatePhase::Downloading,
            completed: 812_000,
            total: Some(9_000_000),
            message: String::new(),
        });
        assert_eq!(
            shown(&snapshot),
            (
                "Downloading repair tool · 0.8 / 9.0 MB".into(),
                Some(UpdateButton::Cancel)
            )
        );
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn ready_names_ending_shells_and_folds_consent_into_the_button() {
        let mut snapshot = candidate();
        assert_eq!(
            shown(&snapshot),
            ("Compi 0.2.0 is ready".into(), Some(UpdateButton::Restart))
        );
        snapshot.daemons = vec![ending(0)];
        assert_eq!(
            shown(&snapshot),
            ("Compi 0.2.0 is ready".into(), Some(UpdateButton::Restart))
        );
        snapshot.daemons = vec![ending(1)];
        assert_eq!(
            shown(&snapshot),
            (
                "Updating ends 1 shell: pwsh".into(),
                Some(UpdateButton::RestartEndShells)
            )
        );
        snapshot.daemons = vec![ending(3)];
        assert_eq!(
            shown(&snapshot).0,
            "Updating ends 3 shells: pwsh, Terminal, vim"
        );
        // The card offers the same consent, counted in its label.
        let card = snapshot.card(0, None, |_| None).unwrap();
        assert_eq!(
            card.actions,
            [CardAction::Update(UpdateButton::RestartEndShells)]
        );
        assert_eq!(
            UpdateButton::RestartEndShells.label(card.ending),
            "Restart and end 3 shells"
        );
        snapshot.daemons = vec![ending(5)];
        assert_eq!(
            shown(&snapshot).0,
            "Updating ends 5 shells: pwsh, Terminal, vim +2 more"
        );
        // Shells of a daemon the release keeps are not ended.
        snapshot.daemons[0]
            .status
            .as_mut()
            .unwrap()
            .protocol_version = 14;
        assert_eq!(shown(&snapshot).1, Some(UpdateButton::Restart));
        snapshot.running = Some(Operation::Install);
        assert_eq!(shown(&snapshot), ("Installing…".into(), None));
        snapshot.running = None;
        snapshot.installing = true;
        assert_eq!(shown(&snapshot), ("Installing…".into(), None));
    }

    #[test]
    fn failure_and_repair_outcomes_offer_one_action() {
        let mut snapshot = candidate();
        snapshot.failure = Some(Failure {
            operation: Operation::Download,
            version: Some("0.2.0".into()),
            message: plain_failure(
                Operation::Download,
                "error sending request for url (https://github.com/...)",
            ),
        });
        let view = snapshot.view(0, |_| None);
        assert_eq!(
            (view.status.as_str(), view.button, view.failed),
            (
                "Download failed · connection lost",
                Some(UpdateButton::TryAgain),
                true
            )
        );
        assert_eq!(
            plain_failure(Operation::Check, "Operation timed out"),
            "Couldn't check for updates · no connection"
        );
        assert_eq!(
            plain_failure(Operation::Install, "GUI host 4242 did not save its handoff"),
            "Update didn't install"
        );
        snapshot.repair = Some(RepairOutcome::Unavailable);
        assert_eq!(
            shown(&snapshot),
            (
                "Couldn't get the repair tool · download Setup and choose Repair".into(),
                Some(UpdateButton::DownloadSetup)
            )
        );
        snapshot.repair = Some(RepairOutcome::Opened);
        assert_eq!(shown(&snapshot), ("Repair opened in Setup".into(), None));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn checking_newer_release_keeps_staged_candidate_but_offers_the_newer_one() {
        let mut snapshot = candidate();
        let mut newer = snapshot.release.as_ref().unwrap().as_ref().clone();
        newer.manifest.version = "0.3.0".into();
        newer.manifest.daemon_protocol = 15;
        apply_checked_release(&mut snapshot, Some(newer));
        assert_eq!(snapshot.release.as_ref().unwrap().manifest.version, "0.2.0");
        assert_eq!(snapshot.prepared.as_ref().unwrap().version, "0.2.0");
        assert!(!snapshot.requires_daemon_restart(&daemon()));
        assert_eq!(
            shown(&snapshot),
            (
                "Compi 0.3.0 is available".into(),
                Some(UpdateButton::Download)
            )
        );
        assert_eq!(snapshot.candidate().unwrap().manifest.version, "0.3.0");
        apply_checked_release(&mut snapshot, None);
        assert_eq!(snapshot.release.as_ref().unwrap().manifest.version, "0.2.0");
        assert_eq!(shown(&snapshot).1, Some(UpdateButton::Restart));
    }

    #[test]
    fn sizes_use_decimal_units_without_rounding_into_the_next_unit() {
        assert_eq!(format_size(0), "0 bytes");
        assert_eq!(format_size(999), "999 bytes");
        assert_eq!(format_size(1_000), "1 KB");
        assert_eq!(format_size(812_400), "812 KB");
        assert_eq!(format_size(999_499), "999 KB");
        assert_eq!(format_size(999_500), "1.0 MB");
        assert_eq!(format_size(13_249_999), "13.2 MB");
        assert_eq!(format_size(13_250_000), "13.3 MB");
        assert_eq!(format_transfer(400_000, 812_000, " / "), "400 / 812 KB");
        assert_eq!(
            format_transfer(20_000_000, 13_200_000, " / "),
            "13.2 / 13.2 MB"
        );
    }

    #[test]
    fn check_times_read_as_relative_phrases_then_dates() {
        let now = 1_709_164_800; // 2024-02-29 00:00 UTC
        assert_eq!(time_since(now, now), "just now");
        assert_eq!(time_since(now + 30, now), "just now");
        assert_eq!(time_since(now - 59, now), "just now");
        assert_eq!(time_since(now - 60, now), "1 min ago");
        assert_eq!(time_since(now - 3_599, now), "59 min ago");
        assert_eq!(time_since(now - 3_600, now), "1 h ago");
        assert_eq!(time_since(now - 86_399, now), "23 h ago");
        assert_eq!(time_since(now - 86_400, now), "yesterday");
        assert_eq!(time_since(now - 172_799, now), "yesterday");
        assert_eq!(time_since(now - 172_800, now), "on Feb 27");
        assert_eq!(time_since(1_672_531_200, now), "on Jan 1, 2023");
        assert_eq!(civil_date(now), (2024, 2, 29));
    }

    #[test]
    fn ready_dot_waits_for_the_page_and_ignores_superseded_or_failed_states() {
        let mut snapshot = candidate();
        assert_eq!(snapshot.ready_version(), Some("0.2.0"));
        snapshot.failure = Some(Failure {
            operation: Operation::Install,
            version: Some("0.2.0".into()),
            message: String::new(),
        });
        assert_eq!(snapshot.ready_version(), None);
        snapshot.failure = None;
        snapshot.available_release = Some(release("0.3.0", 1));
        assert_eq!(snapshot.ready_version(), None);
    }

    fn failed(version: &str) -> Failure {
        Failure {
            operation: Operation::Reinstall,
            version: Some(version.into()),
            message: "Last update didn't install · Compi was restored".into(),
        }
    }

    #[test]
    fn a_failed_update_keeps_try_again_while_its_version_is_offered() {
        let mut snapshot = UpdateSnapshot {
            failure: Some(failed("0.7.3")),
            ..UpdateSnapshot::default()
        };
        apply_checked_release(&mut snapshot, Some(release("0.7.3", 1).as_ref().clone()));
        let view = snapshot.view(0, |_| None);
        assert!(view.failed);
        assert_eq!(view.button, Some(UpdateButton::TryAgain));
        assert!(!snapshot.shows_dot(None));
        let card = snapshot.card(0, None, |_| None).unwrap();
        assert_eq!(
            (card.version.as_str(), card.failed, card.actions[0]),
            ("0.7.3", true, CardAction::Update(UpdateButton::TryAgain))
        );
    }

    #[test]
    fn a_newer_release_replaces_a_failed_update_for_an_older_one() {
        let mut snapshot = UpdateSnapshot {
            failure: Some(failed("0.7.3")),
            ..UpdateSnapshot::default()
        };
        // Found by an earlier check: shown ahead of the failure before any new check.
        snapshot.available_release = Some(release("0.7.4", 13_200_000));
        assert_eq!(
            shown(&snapshot),
            (
                "Compi 0.7.4 is available".into(),
                Some(UpdateButton::Download)
            )
        );
        apply_checked_release(
            &mut snapshot,
            Some(release("0.7.4", 13_200_000).as_ref().clone()),
        );
        assert_eq!(snapshot.failure, None);
        assert!(snapshot.shows_dot(None));
        let card = snapshot.card(0, None, |_| None).unwrap();
        assert_eq!(
            (card.title.as_str(), card.actions.as_slice(), card.later),
            (
                "Compi 0.7.4 is available",
                [CardAction::Update(UpdateButton::Download)].as_slice(),
                true
            )
        );
        // Later hides the card and the dot for this version only.
        assert!(snapshot.card(0, Some("0.7.4"), |_| None).is_none());
        assert!(!snapshot.shows_dot(Some("0.7.4")));
        assert!(snapshot.shows_dot(Some("0.7.3")));
        // A failed check stays on the Updates page, behind the offer.
        snapshot.failure = Some(Failure {
            operation: Operation::Check,
            version: None,
            message: plain_failure(Operation::Check, "timed out"),
        });
        assert_eq!(shown(&snapshot).1, Some(UpdateButton::Download));
        snapshot.available_release = None;
        snapshot.release = None;
        assert_eq!(shown(&snapshot).1, Some(UpdateButton::TryAgain));
        assert!(snapshot.card(0, None, |_| None).is_none());
    }
}
