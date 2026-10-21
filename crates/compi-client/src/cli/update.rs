//! `compi update`: Settings → Updates for a terminal. The check and the verified
//! download run here. A restart is handed to a running Compi window, whose reviewed
//! install path saves its windows, ends only the consented shells and reopens them.
use super::{CliError, Options, Output, bridged_terminal, interactive, read_answer};
use crate::{
    arrangement,
    connection::ConnectionTarget,
    updates::{self, CommandOutcome, Operation, UpdateService, UpdateSnapshot},
    window_host::{self, UpdateHost},
};
use compi_protocol::{SurfaceId, WorkspaceSnapshot};
use compi_update::{PreparedUpdate, UpdatePhase};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    io::{self, IsTerminal, Write},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const RUNNING: &str = env!("CARGO_PKG_VERSION");
/// Handoffs before giving up while shells keep appearing or a window is opened.
const ATTEMPTS: usize = 3;

pub(super) fn run(options: &Options) -> Result<Output, CliError> {
    compi_update::ReleaseConfig::compiled()
        .map_err(|error| CliError::new(1, "updates_unavailable", error.to_string()))?;
    let human = !options.json;
    // Human output, status line and prompt share stdout: the WSL bridge relays stdout
    // and stderr separately, so splitting them could reorder lines.
    let line = Line::new(human && (io::stdout().is_terminal() || bridged_terminal()));
    let service = updates::for_command();
    line.show("Checking for updates…");
    let checked = service.run(Operation::Check, |_| {});
    line.clear();
    let checked = finished(checked, "update_check_failed")?;
    let Some(release) = checked.candidate() else {
        return Ok(Output::Report {
            text: format!("Compi {RUNNING} is up to date.\n"),
            value: json!({"current": RUNNING, "latest": null, "update_available": false}),
        });
    };
    let manifest = release.manifest.clone();
    let version = manifest.version.clone();
    let ready = checked.ready_version() == Some(version.as_str());
    let summary = crate::release_notes::of_release(&manifest)
        .summary
        .into_iter()
        .next();
    let mut heading = format!(
        "Compi {RUNNING} · {version} {}\n",
        if ready { "ready" } else { "available" }
    );
    if let Some(summary) = &summary {
        heading.push_str(&format!("  {summary}\n"));
    }
    if options.switches.contains("--check") {
        return Ok(Output::Report {
            text: heading,
            value: json!({
                "current": RUNNING,
                "latest": version,
                "update_available": true,
                "downloaded": ready,
                "size": manifest.artifact.size,
                "summary": summary,
            }),
        });
    }
    if human {
        print!("{heading}");
        let _ = io::stdout().flush();
    }
    if !ready {
        let size = manifest.artifact.size;
        let downloaded = service.run(Operation::Download, |snapshot| {
            line.show(&download_line(snapshot, size))
        });
        line.clear();
        finished(downloaded, "update_download_failed")?;
        if human {
            println!("Downloaded and verified · {}", updates::format_size(size));
        }
    } else if retained_retry_needed(checked.prepared.as_ref().expect("ready update is prepared"))? {
        // Give this command a fresh transaction identity instead of mistaking the
        // previous Prepared error for the outcome of its new GUI handoff.
        let retried = service.run(Operation::Reinstall, |_| {});
        finished(retried, "update_download_failed")?;
    }
    restart(&service, options, &version)
}

/// Lists what a restart ends, asks, and hands the restart to a Compi window.
fn restart(service: &UpdateService, options: &Options, version: &str) -> Result<Output, CliError> {
    let human = !options.json;
    let instance = options.instance.as_deref().filter(|name| !name.is_empty());
    let mut consented: Option<Vec<SurfaceId>> = None;
    let mut listed = Vec::new();
    let mut opened = false;
    for _ in 0..ATTEMPTS {
        let snapshot = service.review_now().map_err(failed)?;
        let prepared = snapshot
            .prepared
            .clone()
            .filter(|prepared| prepared.version == version)
            .ok_or_else(|| {
                failed("The downloaded update is gone. Run compi update again.".into())
            })?;
        if let Some(blocker) = snapshot.install_blocker() {
            return Err(CliError::new(1, "update_blocked", blocker));
        }
        let ending = snapshot.ending_shells();
        if consented
            .as_ref()
            .is_none_or(|shown| ending.iter().any(|shell| !shown.contains(shell)))
        {
            listed = name_shells(&snapshot, options.surface.as_deref());
            if human {
                println!("{}", describe(&listed));
            }
            confirm(human)?;
            consented = Some(ending);
        }
        let hosts = window_host::update_hosts().map_err(|error| failed(error.to_string()))?;
        let Some(host) = choose(hosts, instance) else {
            if opened {
                break;
            }
            open_compi(options, instance)?;
            opened = true;
            continue;
        };
        let outcome = updates::command_outcome_path(&prepared).map_err(failed)?;
        window_host::request_update_install(
            &host,
            version.to_owned(),
            consented.clone().unwrap_or_default(),
            outcome.clone(),
        )
        .map_err(|error| failed(format!("Compi couldn't take over the restart: {error}")))?;
        if human {
            println!("Restarting into {version}…");
            let _ = io::stdout().flush();
        }
        match wait_for_restart(&prepared, &outcome)? {
            RestartOutcome::Complete => {
                return Ok(Output::Report {
                    text: format!("Compi {version} installed and reopened.\n"),
                    value: json!({
                        "current": version,
                        "previous": RUNNING,
                        "version": version,
                        "restarting": false,
                        "applied": true,
                        "reopened": true,
                        "ending_shells": listed.iter().map(Shell::json).collect::<Vec<_>>(),
                    }),
                });
            }
            RestartOutcome::ShellsChanged => {
                if human {
                    println!("More shells are running now.");
                }
            }
        }
    }
    Err(failed(
        "Compi couldn't start the restart. Try again, or restart from Settings → Updates.".into(),
    ))
}

fn retained_retry_needed(prepared: &PreparedUpdate) -> Result<bool, CliError> {
    let journal = compi_update::operation_status(&prepared.target)
        .map_err(|error| failed(format!("Couldn't read update progress: {error}")))?;
    Ok(journal.is_some_and(|journal| {
        journal.phase == compi_update::JournalStage::Prepared && journal.error.is_some()
    }))
}

fn failed(message: String) -> CliError {
    CliError::new(1, "update_failed", message)
}

/// A finished check or download, or its failure with where the detail is.
fn finished(
    result: Result<UpdateSnapshot, String>,
    code: &'static str,
) -> Result<UpdateSnapshot, CliError> {
    let snapshot = result.map_err(|message| CliError::new(1, code, message))?;
    let Some(failure) = &snapshot.failure else {
        return Ok(snapshot);
    };
    let message = match updates::update_log_path() {
        Some(log) => format!("{}. Details: {}", failure.message, log.display()),
        None => failure.message.clone(),
    };
    Err(CliError::new(1, code, message))
}

/// Asks on stdout beside the listing; under --json, stdout carries only the result.
fn confirm(human: bool) -> Result<(), CliError> {
    if !interactive() {
        return consent(None);
    }
    const PROMPT: &str = "Restart now? [y/N] ";
    if human {
        print!("{PROMPT}");
        let _ = io::stdout().flush();
    } else {
        eprint!("{PROMPT}");
        let _ = io::stderr().flush();
    }
    consent(Some(&read_answer()?))
}

/// Only an interactive yes restarts; there is no bypass for scripts.
fn consent(answer: Option<&str>) -> Result<(), CliError> {
    match answer.map(|answer| answer.trim().to_ascii_lowercase()) {
        None => Err(CliError::new(
            4,
            "confirmation_required",
            "Update ready; run `compi update` in a terminal or restart from Settings → Updates",
        )),
        Some(answer) if answer == "y" || answer == "yes" => Ok(()),
        Some(_) => Err(CliError::new(
            4,
            "cancelled",
            "Not restarted; the update stays ready in Settings → Updates",
        )),
    }
}

/// The host of this shell's instance, else any; each restarts every window.
fn choose(mut hosts: Vec<UpdateHost>, instance: Option<&str>) -> Option<UpdateHost> {
    let index = hosts
        .iter()
        .position(|host| host.instance.as_deref() == instance)
        .unwrap_or(0);
    (index < hosts.len()).then(|| hosts.swap_remove(index))
}

/// No window can perform the restart, so open one and wait until it shows its workspace.
fn open_compi(options: &Options, instance: Option<&str>) -> Result<(), CliError> {
    let unopened = |detail: String| failed(format!("Couldn't open Compi to restart it: {detail}"));
    let executable = std::env::current_exe().map_err(|error| unopened(error.to_string()))?;
    let mut command = Command::new(executable);
    if let Some(instance) = instance {
        command.args(["--instance", instance]);
    }
    if let Some(config) = &options.config {
        command.arg("--config").arg(config);
    }
    command
        .env_remove("COMPI_SURFACE_ID")
        .env_remove("COMPI_SHELL_CWD")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: the window outlives this shell.
        command.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    command
        .spawn()
        .map_err(|error| unopened(error.to_string()))?;
    let target = ConnectionTarget::Local {
        instance: instance.map(str::to_owned),
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let hosts = window_host::update_hosts().map_err(|error| unopened(error.to_string()))?;
        if !hosts.is_empty()
            && target
                .lifecycle_status()
                .is_ok_and(|status| !status.connected_clients.is_empty())
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(unopened("no window appeared within 30 s".into()));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RestartOutcome {
    Complete,
    ShellsChanged,
}

/// A helper handoff is only progress. Complete is persisted after every new GUI attaches.
fn restart_status(
    prepared: &PreparedUpdate,
    journal: Option<&compi_update::Journal>,
    outcome: Option<CommandOutcome>,
) -> Result<Option<RestartOutcome>, CliError> {
    if let Some(journal) = journal {
        if journal.manifest.version != prepared.version
            || journal.stage != prepared.stage
            || journal.target.root != prepared.target.root
            || journal.target.kind != prepared.target.kind
        {
            return Err(failed(
                "The update transaction changed before this restart was confirmed.".into(),
            ));
        }
        if let Some(error) = &journal.error {
            let context = if journal.phase == compi_update::JournalStage::RolledBack {
                "The update failed and was rolled back"
            } else {
                "The update did not finish installing and reopening Compi"
            };
            return Err(failed(format!("{context}: {error}")));
        }
        match journal.phase {
            compi_update::JournalStage::Complete => return Ok(Some(RestartOutcome::Complete)),
            compi_update::JournalStage::RolledBack => {
                return Err(failed(
                    "The update did not install; Compi was rolled back.".into(),
                ));
            }
            _ => {}
        }
    }
    match outcome {
        Some(CommandOutcome::ShellsChanged) => Ok(Some(RestartOutcome::ShellsChanged)),
        Some(CommandOutcome::Failed { message }) => Err(failed(message)),
        Some(CommandOutcome::Started) | None => Ok(None),
    }
}

fn wait_for_restart(prepared: &PreparedUpdate, outcome: &Path) -> Result<RestartOutcome, CliError> {
    wait_for_restart_until(prepared, outcome, Instant::now() + Duration::from_secs(180))
}

fn wait_for_restart_until(
    prepared: &PreparedUpdate,
    outcome: &Path,
    deadline: Instant,
) -> Result<RestartOutcome, CliError> {
    loop {
        let journal = compi_update::operation_status(&prepared.target)
            .map_err(|error| failed(format!("Couldn't read update progress: {error}")))?;
        if let Some(finished) = restart_status(
            prepared,
            journal.as_ref(),
            updates::take_command_outcome(outcome),
        )? {
            return Ok(finished);
        }
        if Instant::now() >= deadline {
            return Err(CliError::new(
                1,
                "update_unconfirmed",
                format!(
                    "Compi didn't confirm that {} installed and reopened. Journal: {}",
                    prepared.version,
                    prepared.journal_path.display()
                ),
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

struct Shell {
    id: SurfaceId,
    name: String,
    this: bool,
}

impl Shell {
    fn json(&self) -> Value {
        json!({"surface_id": self.id, "name": self.name, "this_shell": self.this})
    }
}

/// The shells a restart ends, named by their tabs; `this` is the invoking shell.
fn name_shells(snapshot: &UpdateSnapshot, this: Option<&str>) -> Vec<Shell> {
    let mut workspaces: Vec<(&ConnectionTarget, Option<WorkspaceSnapshot>)> = Vec::new();
    let mut shells = Vec::new();
    for (target, surface) in snapshot.ending_shells_by_daemon() {
        if !workspaces.iter().any(|(known, _)| *known == target) {
            let workspace = target
                .connect_existing()
                .ok()
                .and_then(|mut client| client.workspace().ok());
            workspaces.push((target, workspace));
        }
        let name = workspaces
            .iter()
            .find(|(known, _)| *known == target)
            .and_then(|(_, workspace)| tab_label(workspace.as_ref()?, surface))
            .unwrap_or_else(|| "Terminal".into());
        shells.push(Shell {
            id: surface.clone(),
            name,
            this: this == Some(surface.as_str()),
        });
    }
    shells
}

fn tab_label(workspace: &WorkspaceSnapshot, surface: &SurfaceId) -> Option<String> {
    let tab = workspace
        .sessions
        .iter()
        .flat_map(|session| &session.tabs)
        .find(|tab| {
            arrangement::leaves(&tab.layout)
                .iter()
                .any(|leaf| leaf.surface_id() == Some(surface))
        })?;
    let label = tab.label.trim();
    (!label.is_empty()).then(|| label.to_owned())
}

fn describe(shells: &[Shell]) -> String {
    let count = shells.len();
    if count == 0 {
        return "Your shells keep running through the restart.".into();
    }
    let this = shells.iter().any(|shell| shell.this);
    let mut names: Vec<&str> = shells
        .iter()
        .filter(|shell| shell.this)
        .chain(shells.iter().filter(|shell| !shell.this))
        .map(|shell| shell.name.as_str())
        .take(6)
        .collect();
    let more = count.saturating_sub(names.len());
    let more = (more > 0).then(|| format!("+{more} more"));
    names.extend(more.as_deref());
    let subject = match (count, this) {
        (1, true) => "this shell".to_owned(),
        (1, false) => "1 shell".to_owned(),
        (_, true) => format!("{count} shells, including this one"),
        (_, false) => format!("{count} shells"),
    };
    format!("Restarting ends {subject}: {}", names.join(", "))
}

fn download_line(snapshot: &UpdateSnapshot, expected: u64) -> String {
    match &snapshot.progress {
        Some(event) if event.phase == UpdatePhase::Downloading => {
            let total = event.total.unwrap_or(expected).max(1);
            let fraction = (event.completed as f64 / total as f64).clamp(0.0, 1.0);
            let filled = (fraction * 16.0).floor() as usize;
            format!(
                "Downloading  {}{}  {:>3}% · {}",
                "█".repeat(filled),
                "░".repeat(16 - filled),
                (fraction * 100.0).floor() as u32,
                updates::format_transfer(event.completed, total, " of ")
            )
        }
        Some(event) if event.phase != UpdatePhase::Checking => "Verifying…".into(),
        _ => "Downloading…".into(),
    }
}

/// One status line on a terminal, redrawn in place.
struct Line {
    enabled: bool,
    width: Cell<usize>,
    shown: RefCell<String>,
}

impl Line {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            width: Cell::new(0),
            shown: RefCell::new(String::new()),
        }
    }

    fn show(&self, text: &str) {
        if !self.enabled || *self.shown.borrow() == text {
            return;
        }
        let width = text.chars().count();
        let padding = " ".repeat(self.width.get().saturating_sub(width));
        print!("\r{text}{padding}");
        let _ = io::stdout().flush();
        self.width.set(width);
        *self.shown.borrow_mut() = text.to_owned();
    }

    fn clear(&self) {
        if !self.enabled || self.width.get() == 0 {
            return;
        }
        print!("\r{}\r", " ".repeat(self.width.get()));
        let _ = io::stdout().flush();
        self.width.set(0);
        self.shown.borrow_mut().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_interactive_yes_restarts() {
        let refused = consent(None).unwrap_err();
        assert_eq!((refused.exit, refused.code), (4, "confirmation_required"));
        for answer in ["y\n", "Y\r\n", "yes\n"] {
            assert!(consent(Some(answer)).is_ok(), "{answer:?}");
        }
        for answer in ["\n", "", "n\n", "no\n", "yy\n", "restart\n"] {
            let cancelled = consent(Some(answer)).unwrap_err();
            assert_eq!(
                (cancelled.exit, cancelled.code),
                (4, "cancelled"),
                "{answer:?}"
            );
        }
    }

    struct RestartFixture {
        root: std::path::PathBuf,
        prepared: PreparedUpdate,
        outcome: std::path::PathBuf,
        journal: compi_update::Journal,
    }

    impl RestartFixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "compi-cli-update-{}",
                compi_update::new_receipt_token()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let state = root.join(".compi-update");
            std::fs::create_dir_all(&state).unwrap();
            let attempt = "a".repeat(32);
            let target = compi_update::InstallTarget {
                kind: compi_update::InstallationKind::InstalledWindows,
                root: root.clone(),
            };
            let prepared = PreparedUpdate {
                journal_path: state.join("journal.json"),
                target: target.clone(),
                version: "0.3.0".into(),
                stage: state.join(format!("stage-{attempt}")).join("payload"),
            };
            let journal = compi_update::Journal {
                schema: 1,
                attempt,
                target,
                manifest: compi_update::ReleaseManifest {
                    schema: 1,
                    product: "compi".into(),
                    version: prepared.version.clone(),
                    platform: "windows-x86_64".into(),
                    minimum_os: "10".into(),
                    release_notes: String::new(),
                    daemon_protocol: 14,
                    qualified_daemon_versions: vec!["0.2.0".into()],
                    minimum_persistence: 1,
                    artifact: compi_update::Artifact {
                        url: String::new(),
                        size: 1,
                        sha256: String::new(),
                    },
                    setup: None,
                },
                signed: compi_update::SignedManifest {
                    payload: String::new(),
                    signature: String::new(),
                },
                stage: prepared.stage.clone(),
                prior_selection: None,
                phase: compi_update::JournalStage::Prepared,
                backup: None,
                retained_backups: Vec::new(),
                prior_task: None,
                error: None,
            };
            Self {
                root,
                prepared,
                outcome: state.join("command-outcome.json"),
                journal,
            }
        }

        fn observe(&self, outcome: Option<CommandOutcome>) -> Result<RestartOutcome, CliError> {
            std::fs::write(
                &self.prepared.journal_path,
                serde_json::to_vec(&self.journal).unwrap(),
            )
            .unwrap();
            if let Some(outcome) = outcome {
                std::fs::write(&self.outcome, serde_json::to_vec(&outcome).unwrap()).unwrap();
            }
            // Exercise the real persisted-status consumer, without sleeps or live processes.
            wait_for_restart_until(&self.prepared, &self.outcome, Instant::now())
        }
    }

    impl Drop for RestartFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn retained_prepared_failure_is_restaged_before_a_new_cli_handoff() {
        let mut fixture = RestartFixture::new();
        fixture.observe(None).unwrap_err();
        assert!(!retained_retry_needed(&fixture.prepared).unwrap());
        fixture.journal.error = Some("previous activation failed".into());
        fixture.observe(None).unwrap_err();
        assert!(retained_retry_needed(&fixture.prepared).unwrap());
        // Do not turn an in-progress or terminal transaction into an implicit retry.
        for phase in [
            compi_update::JournalStage::Activating,
            compi_update::JournalStage::Relaunching,
            compi_update::JournalStage::Complete,
            compi_update::JournalStage::RolledBack,
        ] {
            fixture.journal.phase = phase;
            fixture.observe(None).unwrap_err();
            assert!(
                !retained_retry_needed(&fixture.prepared).unwrap(),
                "{phase:?}"
            );
        }
    }

    #[test]
    fn helper_handoff_and_pending_journal_never_report_installed_success() {
        let mut fixture = RestartFixture::new();
        for phase in [
            compi_update::JournalStage::Prepared,
            compi_update::JournalStage::Activating,
            compi_update::JournalStage::Activated,
            compi_update::JournalStage::Relaunching,
        ] {
            fixture.journal.phase = phase;
            for outcome in [None, Some(CommandOutcome::Started)] {
                let error = fixture.observe(outcome).unwrap_err();
                assert_eq!(error.code, "update_unconfirmed", "{phase:?}");
            }
        }
    }

    #[test]
    fn only_matching_completed_candidate_reports_success() {
        let mut fixture = RestartFixture::new();
        fixture.journal.phase = compi_update::JournalStage::Complete;
        assert_eq!(fixture.observe(None).unwrap(), RestartOutcome::Complete);
        fixture.journal.manifest.version = "0.2.0".into();
        let error = fixture.observe(Some(CommandOutcome::Started)).unwrap_err();
        assert_eq!(error.code, "update_failed");
        fixture.journal.manifest.version = fixture.prepared.version.clone();
        fixture.prepared.stage = fixture.root.join("another-stage");
        assert_eq!(fixture.observe(None).unwrap_err().code, "update_failed");
    }

    #[test]
    fn persisted_activation_and_reopen_failures_override_helper_started() {
        let mut fixture = RestartFixture::new();
        for phase in [
            compi_update::JournalStage::Prepared,
            compi_update::JournalStage::Activating,
            compi_update::JournalStage::Relaunching,
            compi_update::JournalStage::Complete,
            compi_update::JournalStage::RolledBack,
        ] {
            fixture.journal.phase = phase;
            fixture.journal.error = Some("owned task or GUI attachment failed".into());
            let error = fixture.observe(Some(CommandOutcome::Started)).unwrap_err();
            assert_eq!(error.code, "update_failed");
            assert!(
                error
                    .message
                    .contains("owned task or GUI attachment failed")
            );
        }
        fixture.journal.error = None;
        let error = fixture.observe(None).unwrap_err();
        assert_eq!(error.code, "update_failed");
    }

    #[test]
    fn restart_handoff_preserves_consent_and_host_failures() {
        let fixture = RestartFixture::new();
        assert_eq!(
            fixture
                .observe(Some(CommandOutcome::ShellsChanged))
                .unwrap(),
            RestartOutcome::ShellsChanged
        );
        let error = fixture
            .observe(Some(CommandOutcome::Failed {
                message: "Task belongs to another installation or generation".into(),
            }))
            .unwrap_err();
        assert_eq!(error.code, "update_failed");
        assert!(error.message.contains("another installation or generation"));
    }
}
