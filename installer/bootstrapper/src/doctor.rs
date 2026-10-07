//! What is wrong with an installation, decided from injected facts. Everything here is pure:
//! `machine` gathers the facts from Windows and applies the fixes.

/// Files a version folder needs to run at all.
pub(crate) const RUNNABLE_FILES: [&str; 3] =
    ["compi.exe", "compi-daemon.exe", "compi-update-worker.exe"];
/// Files a version folder needs to work fully.
pub(crate) const PAYLOAD_FILES: [&str; 5] = [
    "compi.exe",
    "compi-daemon.exe",
    "compi-update-worker.exe",
    "conpty.dll",
    "OpenConsole.exe",
];
/// Files Setup installs next to the version folders.
pub(crate) const ROOT_FILES: [&str; 3] =
    ["compi.exe", "compi-update-worker.exe", "Compi-Setup.exe"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SelectionFact {
    Missing,
    Invalid,
    Present {
        version: String,
        task_version: String,
    },
}

/// One `versions\<version>` folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PayloadFact {
    pub version: String,
    pub missing: Vec<&'static str>,
}

impl PayloadFact {
    pub(crate) fn runnable(&self) -> bool {
        RUNNABLE_FILES
            .iter()
            .all(|file| !self.missing.contains(file))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TaskTarget {
    /// `<root>\versions\<version>\compi-daemon.exe`.
    Version(String),
    /// Inside this installation, but not a version's daemon (an old layout).
    Unknown,
    /// Outside this installation.
    OtherInstallation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TaskFact {
    Missing,
    Present {
        enabled: bool,
        mine: bool,
        target: TaskTarget,
    },
    Unreadable,
}

/// A daemon of this installation, as its lifecycle endpoint reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunningDaemon {
    /// `None` is the default instance, the one the scheduled task runs.
    pub instance: Option<String>,
    pub protocol: u32,
    /// The `versions\<version>` generation it runs from.
    pub version: Option<String>,
    pub supervised: bool,
    /// Labels of its live shells.
    pub shells: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WslFact {
    Ready,
    NoDistribution,
    NotWsl2 { name: String },
    NotStarting,
    Unavailable,
}

#[derive(Clone, Debug)]
pub(crate) struct Facts {
    /// Protocol of the daemon this Setup carries.
    pub own_protocol: u32,
    pub own_version: String,
    pub selection: SelectionFact,
    pub payloads: Vec<PayloadFact>,
    pub root_missing: Vec<&'static str>,
    pub task: TaskFact,
    pub daemons: Result<Vec<RunningDaemon>, String>,
    /// Known daemon protocol per installed version.
    pub protocols: Vec<(String, u32)>,
    /// An update or Setup run was interrupted and still needs rolling back.
    pub unfinished: bool,
    /// Finished Setup runs and dead lock receipts left on disk.
    pub leftovers: bool,
    pub wsl: WslFact,
}

impl Facts {
    fn protocol_of(&self, version: &str) -> Option<u32> {
        if version == self.own_version {
            return Some(self.own_protocol);
        }
        self.protocols
            .iter()
            .find(|(known, _)| known == version)
            .map(|(_, protocol)| *protocol)
    }

    fn payload(&self, version: &str) -> Option<&PayloadFact> {
        self.payloads
            .iter()
            .find(|payload| payload.version == version)
    }

    fn newest_runnable(&self, except: Option<&str>) -> Option<String> {
        self.payloads
            .iter()
            .filter(|payload| payload.runnable() && Some(payload.version.as_str()) != except)
            .filter_map(|payload| {
                semver::Version::parse(&payload.version)
                    .ok()
                    .map(|parsed| (parsed, &payload.version))
            })
            .max_by(|a, b| a.0.cmp(&b.0))
            .map(|(_, version)| version.clone())
    }

    fn all_shells(&self) -> Vec<String> {
        self.daemons
            .as_ref()
            .map(|daemons| {
                daemons
                    .iter()
                    .flat_map(|daemon| daemon.shells.iter().cloned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Fix {
    /// Windows Installer repair; ends `shells` first.
    Reinstall,
    PointSelection(String),
    FinishOperation,
    ClearLeftovers,
    RegisterTask,
    RestartDaemon {
        instance: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Finding {
    NotInstalled {
        shells: Vec<String>,
    },
    SelectionBroken {
        newest: Option<String>,
        shells: Vec<String>,
    },
    PayloadDamaged {
        missing: Vec<&'static str>,
        shells: Vec<String>,
    },
    Unfinished,
    Leftovers,
    TaskMissing,
    TaskDisabled,
    TaskOtherOwner,
    TaskWrongVersion {
        selected: String,
    },
    TaskOtherInstallation,
    TaskUnreadable,
    DaemonIncompatible {
        instance: Option<String>,
        shells: Vec<String>,
    },
    DaemonUnsupervised {
        shells: Vec<String>,
    },
    DaemonUninspectable,
    Wsl(WslFact),
}

impl Finding {
    pub(crate) fn title(&self) -> String {
        match self {
            Self::NotInstalled { .. } => "Compi isn't installed".into(),
            Self::SelectionBroken { .. } => "Compi can't find its current version".into(),
            Self::PayloadDamaged { .. } => "Program files are missing".into(),
            Self::Unfinished => "An update didn't finish".into(),
            Self::Leftovers => "Leftover setup files".into(),
            Self::TaskMissing => "Background service isn't set up".into(),
            Self::TaskDisabled => "Background service is turned off".into(),
            Self::TaskOtherOwner => "Background service has the wrong owner".into(),
            Self::TaskWrongVersion { .. } => "Background service uses the wrong version".into(),
            Self::TaskOtherInstallation => "Background service belongs to another copy".into(),
            Self::TaskUnreadable => "Can't read the background service settings".into(),
            Self::DaemonIncompatible { instance: None, .. } => {
                "Background service is out of date".into()
            }
            Self::DaemonIncompatible {
                instance: Some(name),
                ..
            } => format!("Instance \u{201c}{name}\u{201d} is out of date"),
            Self::DaemonUnsupervised { .. } => "Background service isn't watched".into(),
            Self::DaemonUninspectable => "Can't check the background service".into(),
            Self::Wsl(WslFact::NoDistribution | WslFact::Unavailable) => "WSL isn't set up".into(),
            Self::Wsl(WslFact::NotWsl2 { .. }) => "Your Linux uses WSL 1".into(),
            Self::Wsl(WslFact::NotStarting) => "Linux doesn't start".into(),
            Self::Wsl(WslFact::Ready) => "WSL is ready".into(),
        }
    }

    pub(crate) fn explanation(&self) -> String {
        match self {
            Self::NotInstalled { .. } => "Its program files are missing.".into(),
            Self::SelectionBroken {
                newest: Some(version),
                ..
            } => format!("It will switch to version {version}, which is installed."),
            Self::SelectionBroken { newest: None, .. } => {
                "No usable version is installed. Reinstalling fixes this.".into()
            }
            Self::PayloadDamaged { missing, .. } => format!("Missing: {}.", missing.join(", ")),
            Self::Unfinished => "Compi will undo it and keep your current version.".into(),
            Self::Leftovers => "Files from earlier updates are taking up space.".into(),
            Self::TaskMissing => "Your shells won't start when you sign in.".into(),
            Self::TaskDisabled => "It's disabled in Task Scheduler.".into(),
            Self::TaskOtherOwner => "It's registered to a different Windows account.".into(),
            Self::TaskWrongVersion { selected } => {
                format!("It will be set to version {selected}.")
            }
            Self::TaskOtherInstallation => {
                "Another copy of Compi uses it. Remove that copy, then check again.".into()
            }
            Self::TaskUnreadable => {
                "Task Scheduler didn't answer. Restart Windows, then check again.".into()
            }
            Self::DaemonIncompatible { .. } => {
                "It runs an older version this Compi can't connect to.".into()
            }
            Self::DaemonUnsupervised { .. } => "If it stops, it won't restart on its own.".into(),
            Self::DaemonUninspectable => "Close Compi, wait a moment, then check again.".into(),
            Self::Wsl(WslFact::NoDistribution | WslFact::Unavailable) => {
                "Run this in PowerShell, then restart Windows:".into()
            }
            Self::Wsl(WslFact::NotWsl2 { .. }) => {
                "Compi needs WSL 2. Run this in PowerShell:".into()
            }
            Self::Wsl(WslFact::NotStarting) => "Run this in PowerShell to finish its setup:".into(),
            Self::Wsl(WslFact::Ready) => String::new(),
        }
    }

    /// The exact command to run by hand, for problems Setup must not change itself.
    pub(crate) fn command(&self) -> Option<String> {
        match self {
            Self::Wsl(WslFact::NoDistribution | WslFact::Unavailable) => {
                Some("wsl --install".into())
            }
            Self::Wsl(WslFact::NotWsl2 { name }) => Some(format!("wsl --set-version {name} 2")),
            Self::Wsl(WslFact::NotStarting) => Some("wsl".into()),
            _ => None,
        }
    }

    pub(crate) fn fix(&self) -> Option<Fix> {
        match self {
            Self::NotInstalled { .. } | Self::PayloadDamaged { .. } => Some(Fix::Reinstall),
            Self::SelectionBroken {
                newest: Some(version),
                ..
            } => Some(Fix::PointSelection(version.clone())),
            Self::SelectionBroken { newest: None, .. } => Some(Fix::Reinstall),
            Self::Unfinished => Some(Fix::FinishOperation),
            Self::Leftovers => Some(Fix::ClearLeftovers),
            Self::TaskMissing
            | Self::TaskDisabled
            | Self::TaskOtherOwner
            | Self::TaskWrongVersion { .. } => Some(Fix::RegisterTask),
            Self::DaemonIncompatible { instance, .. } => Some(Fix::RestartDaemon {
                instance: instance.clone(),
            }),
            Self::DaemonUnsupervised { .. } => Some(Fix::RestartDaemon { instance: None }),
            Self::TaskOtherInstallation
            | Self::TaskUnreadable
            | Self::DaemonUninspectable
            | Self::Wsl(_) => None,
        }
    }

    /// Shells the fix ends. A fix that ends shells needs its own explicit click.
    pub(crate) fn shells(&self) -> &[String] {
        match self {
            Self::NotInstalled { shells }
            | Self::PayloadDamaged { shells, .. }
            | Self::DaemonIncompatible { shells, .. }
            | Self::DaemonUnsupervised { shells } => shells,
            Self::SelectionBroken {
                newest: None,
                shells,
            } => shells,
            _ => &[],
        }
    }
}

/// Every problem the facts show, in the order "Fix all" applies their fixes.
pub(crate) fn findings(facts: &Facts) -> Vec<Finding> {
    let mut found = Vec::new();
    let installed_anything = !facts.payloads.is_empty()
        || facts.root_missing.len() < ROOT_FILES.len()
        || facts.selection != SelectionFact::Missing;
    if !installed_anything {
        found.push(Finding::NotInstalled {
            shells: facts.all_shells(),
        });
        push_wsl(facts, &mut found);
        return found;
    }

    let selected = match &facts.selection {
        SelectionFact::Present {
            version,
            task_version,
        } if facts.payload(version).is_some_and(PayloadFact::runnable) => {
            Some((version.as_str(), task_version.as_str()))
        }
        SelectionFact::Present { version, .. } => {
            let newest = facts.newest_runnable(Some(version));
            found.push(Finding::SelectionBroken {
                shells: if newest.is_none() {
                    facts.all_shells()
                } else {
                    Vec::new()
                },
                newest,
            });
            None
        }
        SelectionFact::Missing | SelectionFact::Invalid => {
            let newest = facts.newest_runnable(None);
            found.push(Finding::SelectionBroken {
                shells: if newest.is_none() {
                    facts.all_shells()
                } else {
                    Vec::new()
                },
                newest,
            });
            None
        }
    };

    let mut missing: Vec<&'static str> = facts.root_missing.clone();
    if let Some((version, _)) = selected
        && let Some(payload) = facts.payload(version)
    {
        for file in &payload.missing {
            if !missing.contains(file) {
                missing.push(file);
            }
        }
    }
    if !missing.is_empty() {
        found.push(Finding::PayloadDamaged {
            missing,
            shells: facts.all_shells(),
        });
    }
    if facts.unfinished {
        found.push(Finding::Unfinished);
    }
    if facts.leftovers {
        found.push(Finding::Leftovers);
    }

    // Task and daemon checks compare against a usable selection; fix that first.
    if let Some((version, task_version)) = selected {
        let selected_protocol = facts.protocol_of(version);
        let default_running = facts
            .daemons
            .as_ref()
            .is_ok_and(|daemons| daemons.iter().any(|daemon| daemon.instance.is_none()));
        match &facts.task {
            TaskFact::Unreadable => found.push(Finding::TaskUnreadable),
            TaskFact::Missing => found.push(Finding::TaskMissing),
            TaskFact::Present { mine: false, .. } => found.push(Finding::TaskOtherOwner),
            TaskFact::Present {
                target: TaskTarget::OtherInstallation,
                ..
            } => found.push(Finding::TaskOtherInstallation),
            TaskFact::Present { enabled: false, .. } => found.push(Finding::TaskDisabled),
            TaskFact::Present { target, .. } => {
                let wrong = match target {
                    TaskTarget::Version(target) => {
                        target != task_version
                            || !facts.payload(target).is_some_and(|payload| {
                                !payload.missing.contains(&"compi-daemon.exe")
                            })
                            || (!default_running
                                && matches!(
                                    (facts.protocol_of(target), selected_protocol),
                                    (Some(a), Some(b)) if a != b
                                ))
                    }
                    TaskTarget::Unknown | TaskTarget::OtherInstallation => true,
                };
                if wrong {
                    found.push(Finding::TaskWrongVersion {
                        selected: version.to_owned(),
                    });
                }
            }
        }
        match &facts.daemons {
            Err(_) => found.push(Finding::DaemonUninspectable),
            Ok(daemons) => {
                for daemon in daemons {
                    let incompatible =
                        selected_protocol.is_some_and(|ours| daemon.protocol != ours);
                    if incompatible {
                        found.push(Finding::DaemonIncompatible {
                            instance: daemon.instance.clone(),
                            shells: daemon.shells.clone(),
                        });
                    } else if daemon.instance.is_none() && !daemon.supervised {
                        found.push(Finding::DaemonUnsupervised {
                            shells: daemon.shells.clone(),
                        });
                    }
                }
            }
        }
    }
    push_wsl(facts, &mut found);
    found
}

fn push_wsl(facts: &Facts, found: &mut Vec<Finding>) {
    if facts.wsl != WslFact::Ready {
        found.push(Finding::Wsl(facts.wsl.clone()));
    }
}

/// What installing a new version does to the background service.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UpgradePlan {
    /// Keep the task on this generation: a compatible daemon runs from it.
    pub keep_task: Option<String>,
    /// Instances to stop first; they cannot serve the new version.
    pub stop: Vec<Option<String>>,
    /// Shells that stopping them ends.
    pub shells: Vec<String>,
}

impl UpgradePlan {
    /// Ending shells requires an explicit click on a button that lists them.
    pub(crate) fn needs_consent(&self) -> bool {
        !self.shells.is_empty()
    }

    pub(crate) fn restarts_service(&self) -> bool {
        !self.stop.is_empty()
    }
}

/// Compatible upgrades leave a running daemon (and its task generation) alone. A daemon that
/// cannot serve the new protocol is stopped, and the task moves to the new version.
pub(crate) fn upgrade_plan(
    new_protocol: u32,
    task_version: Option<&str>,
    task_registered: bool,
    daemons: &[RunningDaemon],
) -> UpgradePlan {
    let stopping: Vec<&RunningDaemon> = daemons
        .iter()
        .filter(|daemon| daemon.protocol != new_protocol)
        .collect();
    let keep_task = task_version
        .filter(|version| {
            task_registered
                && daemons.iter().any(|daemon| {
                    daemon.instance.is_none()
                        && daemon.protocol == new_protocol
                        && daemon.version.as_deref() == Some(*version)
                })
        })
        .map(str::to_owned);
    UpgradePlan {
        keep_task,
        stop: stopping
            .iter()
            .map(|daemon| daemon.instance.clone())
            .collect(),
        shells: stopping
            .iter()
            .flat_map(|daemon| daemon.shells.iter().cloned())
            .collect(),
    }
}

/// Reinstalling files replaces executables the daemons run from, so all of them stop.
pub(crate) fn reinstall_plan(daemons: &[RunningDaemon]) -> UpgradePlan {
    UpgradePlan {
        keep_task: None,
        stop: daemons
            .iter()
            .map(|daemon| daemon.instance.clone())
            .collect(),
        shells: daemons
            .iter()
            .flat_map(|daemon| daemon.shells.iter().cloned())
            .collect(),
    }
}

/// Classifies `compi_protocol::wsl::ensure_default_wsl2`'s error.
pub(crate) fn wsl_problem(message: &str) -> WslFact {
    if message.contains("no default WSL distribution") {
        WslFact::NoDistribution
    } else if let Some((name, _)) = message
        .strip_prefix("the default WSL distribution ")
        .and_then(|rest| rest.split_once(" uses WSL"))
    {
        WslFact::NotWsl2 { name: name.into() }
    } else {
        WslFact::Unavailable
    }
}

/// Daemon protocols of published releases that predate `compi-daemon --protocol-version`.
pub(crate) fn released_protocol(version: &str) -> Option<u32> {
    match version {
        "0.1.1" | "0.1.2" | "0.1.3" => Some(14),
        "0.1.4" | "0.1.5" => Some(16),
        _ => None,
    }
}

/// Readable names for live shells, from the daemon's saved workspace
/// (`{"workspace":{"sessions":[{"label","tabs":[{"label","layout"}]}]}}`).
pub(crate) fn shell_labels(workspace: &serde_json::Value, live: &[String]) -> Vec<String> {
    fn panes<'a>(node: &'a serde_json::Value, out: &mut Vec<&'a str>) {
        if let Some(surface) = node.get("surface_id").and_then(serde_json::Value::as_str) {
            out.push(surface);
        }
        for child in ["first", "second"] {
            if let Some(child) = node.get(child) {
                panes(child, out);
            }
        }
    }
    let mut names: Vec<(String, String)> = Vec::new();
    let sessions = workspace
        .pointer("/workspace/sessions")
        .and_then(serde_json::Value::as_array);
    for session in sessions.into_iter().flatten() {
        let session_label = session
            .get("label")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();
        let tabs = session.get("tabs").and_then(serde_json::Value::as_array);
        for tab in tabs.into_iter().flatten() {
            let tab_label = tab
                .get("label")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim();
            let label = match (session_label, tab_label) {
                ("", "") => "Shell".to_owned(),
                (session, "") => session.to_owned(),
                ("", tab) => tab.to_owned(),
                (session, tab) if session == tab => tab.to_owned(),
                (session, tab) => format!("{session} \u{b7} {tab}"),
            };
            let mut surfaces = Vec::new();
            if let Some(layout) = tab.get("layout") {
                panes(layout, &mut surfaces);
            }
            names.extend(
                surfaces
                    .into_iter()
                    .map(|surface| (surface.to_owned(), label.clone())),
            );
        }
    }
    let mut labels: Vec<(String, usize)> = Vec::new();
    for surface in live {
        let label = names
            .iter()
            .find(|(id, _)| id == surface)
            .map_or("Shell", |(_, label)| label.as_str());
        match labels.iter_mut().find(|(existing, _)| existing == label) {
            Some((_, count)) => *count += 1,
            None => labels.push((label.to_owned(), 1)),
        }
    }
    labels
        .into_iter()
        .map(|(label, count)| {
            if count == 1 {
                label
            } else {
                format!("{label} ({count})")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(version: &str, missing: &[&'static str]) -> PayloadFact {
        PayloadFact {
            version: version.into(),
            missing: missing.to_vec(),
        }
    }

    fn daemon(instance: Option<&str>, protocol: u32, version: &str) -> RunningDaemon {
        RunningDaemon {
            instance: instance.map(str::to_owned),
            protocol,
            version: Some(version.into()),
            supervised: true,
            shells: Vec::new(),
        }
    }

    fn healthy() -> Facts {
        Facts {
            own_protocol: 16,
            own_version: "0.1.6".into(),
            selection: SelectionFact::Present {
                version: "0.1.6".into(),
                task_version: "0.1.6".into(),
            },
            payloads: vec![payload("0.1.3", &[]), payload("0.1.6", &[])],
            root_missing: Vec::new(),
            task: TaskFact::Present {
                enabled: true,
                mine: true,
                target: TaskTarget::Version("0.1.6".into()),
            },
            daemons: Ok(vec![daemon(None, 16, "0.1.6")]),
            protocols: vec![("0.1.3".into(), 14)],
            unfinished: false,
            leftovers: false,
            wsl: WslFact::Ready,
        }
    }

    #[test]
    fn a_healthy_installation_has_no_findings() {
        assert_eq!(findings(&healthy()), Vec::new());
        let mut idle = healthy();
        idle.daemons = Ok(Vec::new());
        assert_eq!(findings(&idle), Vec::new());
    }

    #[test]
    fn nothing_installed_offers_reinstall_only() {
        let facts = Facts {
            selection: SelectionFact::Missing,
            payloads: Vec::new(),
            root_missing: ROOT_FILES.to_vec(),
            task: TaskFact::Missing,
            daemons: Ok(Vec::new()),
            ..healthy()
        };
        let found = findings(&facts);
        assert_eq!(found, vec![Finding::NotInstalled { shells: Vec::new() }]);
        assert_eq!(found[0].fix(), Some(Fix::Reinstall));
    }

    #[test]
    fn missing_or_broken_selection_points_at_the_newest_runnable_payload() {
        let mut facts = healthy();
        facts.selection = SelectionFact::Missing;
        facts.payloads = vec![
            payload("0.1.10", &[]),
            payload("0.1.9", &[]),
            payload("0.2.0", &["compi.exe"]),
        ];
        let found = findings(&facts);
        assert_eq!(
            found,
            vec![Finding::SelectionBroken {
                newest: Some("0.1.10".into()),
                shells: Vec::new()
            }]
        );
        assert_eq!(found[0].fix(), Some(Fix::PointSelection("0.1.10".into())));

        facts.selection = SelectionFact::Present {
            version: "0.2.0".into(),
            task_version: "0.2.0".into(),
        };
        assert_eq!(
            findings(&facts)[0].fix(),
            Some(Fix::PointSelection("0.1.10".into()))
        );
    }

    #[test]
    fn selection_without_any_runnable_payload_needs_reinstall_and_lists_shells() {
        let mut facts = healthy();
        facts.selection = SelectionFact::Invalid;
        facts.payloads = vec![payload("0.1.6", &["compi-daemon.exe"])];
        facts.daemons = Ok(vec![RunningDaemon {
            shells: vec!["api".into()],
            ..daemon(None, 16, "0.1.6")
        }]);
        let found = findings(&facts);
        assert_eq!(
            found,
            vec![Finding::SelectionBroken {
                newest: None,
                shells: vec!["api".into()]
            }]
        );
        assert_eq!(found[0].fix(), Some(Fix::Reinstall));
        assert_eq!(found[0].shells(), ["api".to_owned()]);
    }

    #[test]
    fn damaged_payload_and_root_files_are_reinstalled() {
        let mut facts = healthy();
        facts.payloads = vec![payload("0.1.6", &["conpty.dll", "OpenConsole.exe"])];
        facts.root_missing = vec!["Compi-Setup.exe"];
        let found = findings(&facts);
        assert_eq!(
            found,
            vec![Finding::PayloadDamaged {
                missing: vec!["Compi-Setup.exe", "conpty.dll", "OpenConsole.exe"],
                shells: Vec::new()
            }]
        );
        assert_eq!(found[0].fix(), Some(Fix::Reinstall));
    }

    #[test]
    fn task_problems_are_detected_and_re_registered() {
        let cases = [
            (TaskFact::Missing, Finding::TaskMissing),
            (
                TaskFact::Present {
                    enabled: false,
                    mine: true,
                    target: TaskTarget::Version("0.1.6".into()),
                },
                Finding::TaskDisabled,
            ),
            (
                TaskFact::Present {
                    enabled: true,
                    mine: false,
                    target: TaskTarget::Version("0.1.6".into()),
                },
                Finding::TaskOtherOwner,
            ),
            (
                TaskFact::Present {
                    enabled: true,
                    mine: true,
                    target: TaskTarget::Version("0.1.3".into()),
                },
                Finding::TaskWrongVersion {
                    selected: "0.1.6".into(),
                },
            ),
            (
                TaskFact::Present {
                    enabled: true,
                    mine: true,
                    target: TaskTarget::Unknown,
                },
                Finding::TaskWrongVersion {
                    selected: "0.1.6".into(),
                },
            ),
        ];
        for (task, expected) in cases {
            let facts = Facts { task, ..healthy() };
            let found = findings(&facts);
            assert_eq!(found, vec![expected]);
            assert_eq!(found[0].fix(), Some(Fix::RegisterTask));
        }
    }

    #[test]
    fn task_pointing_at_a_missing_payload_is_wrong() {
        let mut facts = healthy();
        facts.selection = SelectionFact::Present {
            version: "0.1.6".into(),
            task_version: "0.1.5".into(),
        };
        facts.task = TaskFact::Present {
            enabled: true,
            mine: true,
            target: TaskTarget::Version("0.1.5".into()),
        };
        facts.daemons = Ok(Vec::new());
        assert_eq!(
            findings(&facts),
            vec![Finding::TaskWrongVersion {
                selected: "0.1.6".into()
            }]
        );
    }

    #[test]
    fn compatible_older_task_generation_is_left_alone() {
        let mut facts = healthy();
        facts.selection = SelectionFact::Present {
            version: "0.1.6".into(),
            task_version: "0.1.5".into(),
        };
        facts.payloads.push(payload("0.1.5", &[]));
        facts.protocols.push(("0.1.5".into(), 16));
        facts.task = TaskFact::Present {
            enabled: true,
            mine: true,
            target: TaskTarget::Version("0.1.5".into()),
        };
        facts.daemons = Ok(vec![daemon(None, 16, "0.1.5")]);
        assert_eq!(findings(&facts), Vec::new());
        facts.daemons = Ok(Vec::new());
        assert_eq!(findings(&facts), Vec::new());
    }

    #[test]
    fn idle_task_generation_that_cannot_serve_the_client_is_re_registered() {
        let mut facts = healthy();
        facts.selection = SelectionFact::Present {
            version: "0.1.6".into(),
            task_version: "0.1.3".into(),
        };
        facts.task = TaskFact::Present {
            enabled: true,
            mine: true,
            target: TaskTarget::Version("0.1.3".into()),
        };
        facts.daemons = Ok(Vec::new());
        assert_eq!(
            findings(&facts),
            vec![Finding::TaskWrongVersion {
                selected: "0.1.6".into()
            }]
        );
    }

    #[test]
    fn task_owned_by_another_copy_has_no_automatic_fix() {
        let facts = Facts {
            task: TaskFact::Present {
                enabled: true,
                mine: true,
                target: TaskTarget::OtherInstallation,
            },
            ..healthy()
        };
        let found = findings(&facts);
        assert_eq!(found, vec![Finding::TaskOtherInstallation]);
        assert_eq!(found[0].fix(), None);
        let unreadable = Facts {
            task: TaskFact::Unreadable,
            ..healthy()
        };
        assert_eq!(findings(&unreadable)[0].fix(), None);
    }

    #[test]
    fn old_protocol_daemon_is_restarted_with_its_shells_listed() {
        // The observed failure: 0.1.3 (protocol 14) still serving after installing 0.1.6.
        let mut facts = healthy();
        facts.selection = SelectionFact::Present {
            version: "0.1.6".into(),
            task_version: "0.1.3".into(),
        };
        facts.task = TaskFact::Present {
            enabled: true,
            mine: true,
            target: TaskTarget::Version("0.1.3".into()),
        };
        facts.daemons = Ok(vec![RunningDaemon {
            supervised: false,
            shells: vec!["api".into(), "web".into()],
            ..daemon(None, 14, "0.1.3")
        }]);
        let found = findings(&facts);
        assert_eq!(
            found,
            vec![Finding::DaemonIncompatible {
                instance: None,
                shells: vec!["api".into(), "web".into()]
            }]
        );
        assert_eq!(found[0].fix(), Some(Fix::RestartDaemon { instance: None }));
        assert_eq!(found[0].shells().len(), 2);
    }

    #[test]
    fn unsupervised_default_daemon_is_restarted_but_named_instances_are_not() {
        let mut facts = healthy();
        facts.daemons = Ok(vec![
            RunningDaemon {
                supervised: false,
                ..daemon(None, 16, "0.1.6")
            },
            RunningDaemon {
                supervised: false,
                ..daemon(Some("work"), 16, "0.1.6")
            },
        ]);
        let found = findings(&facts);
        assert_eq!(
            found,
            vec![Finding::DaemonUnsupervised { shells: Vec::new() }]
        );
        assert_eq!(found[0].fix(), Some(Fix::RestartDaemon { instance: None }));
    }

    #[test]
    fn incompatible_named_instance_is_its_own_finding() {
        let mut facts = healthy();
        facts.daemons = Ok(vec![
            daemon(None, 16, "0.1.6"),
            daemon(Some("work"), 14, "0.1.3"),
        ]);
        assert_eq!(
            findings(&facts),
            vec![Finding::DaemonIncompatible {
                instance: Some("work".into()),
                shells: Vec::new()
            }]
        );
    }

    #[test]
    fn uninspectable_daemons_have_no_automatic_fix() {
        let facts = Facts {
            daemons: Err("pipe busy".into()),
            ..healthy()
        };
        let found = findings(&facts);
        assert_eq!(found, vec![Finding::DaemonUninspectable]);
        assert_eq!(found[0].fix(), None);
    }

    #[test]
    fn interrupted_operations_and_leftovers_are_separate_fixes() {
        let facts = Facts {
            unfinished: true,
            leftovers: true,
            ..healthy()
        };
        let found = findings(&facts);
        assert_eq!(found, vec![Finding::Unfinished, Finding::Leftovers]);
        assert_eq!(found[0].fix(), Some(Fix::FinishOperation));
        assert_eq!(found[1].fix(), Some(Fix::ClearLeftovers));
    }

    #[test]
    fn wsl_problems_show_the_exact_command_and_never_auto_fix() {
        let cases = [
            (WslFact::NoDistribution, "wsl --install"),
            (WslFact::Unavailable, "wsl --install"),
            (
                WslFact::NotWsl2 {
                    name: "Ubuntu".into(),
                },
                "wsl --set-version Ubuntu 2",
            ),
            (WslFact::NotStarting, "wsl"),
        ];
        for (wsl, command) in cases {
            let found = findings(&Facts { wsl, ..healthy() });
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].command().as_deref(), Some(command));
            assert_eq!(found[0].fix(), None);
        }
    }

    #[test]
    fn wsl_errors_are_classified_for_their_command() {
        assert_eq!(
            wsl_problem("the default WSL distribution Ubuntu-24.04 uses WSL1; Compi requires WSL2"),
            WslFact::NotWsl2 {
                name: "Ubuntu-24.04".into()
            }
        );
        assert_eq!(
            wsl_problem("no default WSL distribution was found; Compi requires WSL2"),
            WslFact::NoDistribution
        );
        assert_eq!(
            wsl_problem("could not inspect WSL distributions: not installed"),
            WslFact::Unavailable
        );
    }

    #[test]
    fn broken_selection_defers_task_and_daemon_checks_but_keeps_the_rest() {
        let facts = Facts {
            selection: SelectionFact::Missing,
            task: TaskFact::Missing,
            daemons: Ok(vec![daemon(None, 14, "0.1.3")]),
            leftovers: true,
            wsl: WslFact::NotStarting,
            ..healthy()
        };
        assert_eq!(
            findings(&facts),
            vec![
                Finding::SelectionBroken {
                    newest: Some("0.1.6".into()),
                    shells: Vec::new()
                },
                Finding::Leftovers,
                Finding::Wsl(WslFact::NotStarting),
            ]
        );
    }

    #[test]
    fn compatible_upgrade_keeps_the_running_task_generation() {
        let plan = upgrade_plan(16, Some("0.1.4"), true, &[daemon(None, 16, "0.1.4")]);
        assert_eq!(plan.keep_task.as_deref(), Some("0.1.4"));
        assert!(!plan.restarts_service() && !plan.needs_consent());
    }

    #[test]
    fn protocol_bump_restarts_and_needs_consent_only_with_shells() {
        let idle = upgrade_plan(16, Some("0.1.3"), true, &[daemon(None, 14, "0.1.3")]);
        assert_eq!(idle.keep_task, None);
        assert_eq!(idle.stop, vec![None]);
        assert!(idle.restarts_service() && !idle.needs_consent());

        let busy = upgrade_plan(
            16,
            Some("0.1.3"),
            true,
            &[RunningDaemon {
                shells: vec!["api".into()],
                ..daemon(None, 14, "0.1.3")
            }],
        );
        assert!(busy.restarts_service() && busy.needs_consent());
        assert_eq!(busy.shells, vec!["api".to_owned()]);
    }

    #[test]
    fn idle_or_unregistered_upgrade_moves_the_task_forward_without_a_restart() {
        let none = upgrade_plan(16, Some("0.1.3"), true, &[]);
        assert_eq!(none, UpgradePlan::default());
        let missing_task = upgrade_plan(16, Some("0.1.4"), false, &[daemon(None, 16, "0.1.4")]);
        assert_eq!(missing_task, UpgradePlan::default());
        // A compatible daemon from another generation is neither stopped nor pinned.
        let elsewhere = upgrade_plan(16, Some("0.1.3"), true, &[daemon(None, 16, "0.1.5")]);
        assert_eq!(elsewhere, UpgradePlan::default());
    }

    #[test]
    fn reinstall_stops_every_daemon_and_lists_all_shells() {
        let plan = reinstall_plan(&[
            RunningDaemon {
                shells: vec!["api".into()],
                ..daemon(None, 16, "0.1.6")
            },
            RunningDaemon {
                shells: vec!["db".into()],
                ..daemon(Some("work"), 16, "0.1.6")
            },
        ]);
        assert_eq!(plan.stop, vec![None, Some("work".into())]);
        assert_eq!(plan.shells, vec!["api".to_owned(), "db".to_owned()]);
    }

    #[test]
    fn shell_labels_come_from_session_and_tab_names() {
        let workspace = serde_json::json!({
            "format_version": 1,
            "workspace": {"sessions": [
                {"label": "api", "tabs": [
                    {"label": "server", "layout": {"type": "split", "axis": "vertical", "ratio": 0.5,
                        "first": {"type": "pane", "pane_id": "p1", "surface_id": "s1"},
                        "second": {"type": "pane", "pane_id": "p2", "surface_id": "s2"}}},
                    {"label": "api", "layout": {"type": "pane", "pane_id": "p3", "surface_id": "s3"}}
                ]},
                {"label": "", "tabs": [
                    {"label": "", "layout": {"type": "pane", "pane_id": "p4", "surface_id": "s4"}}
                ]}
            ]}
        });
        let live: Vec<String> = ["s1", "s2", "s3", "s4", "gone"]
            .iter()
            .map(|id| id.to_string())
            .collect();
        assert_eq!(
            shell_labels(&workspace, &live),
            vec!["api \u{b7} server (2)", "api", "Shell (2)"]
        );
    }
}
