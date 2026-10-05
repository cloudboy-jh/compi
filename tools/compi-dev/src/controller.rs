//! Decides what a source change means for the running dev processes.
//!
//! Invariants:
//! - Build first, replace second: a failed build never touches the running preview.
//! - The dev daemon (and its shells) is only stopped on an explicit restart request.
//! - A client is only launched against a daemon speaking the same protocol.

use crate::watch::{Digest, Fingerprints};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Identity of the daemon binary installed in the dev runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStamp {
    pub daemon: Digest,
    pub protocol: Digest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Client,
    Daemon,
}

impl Target {
    pub fn label(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Daemon => "daemon",
        }
    }
}

#[derive(Debug)]
pub enum BuildOutcome {
    Built { elapsed: Duration, warnings: usize },
    Failed(String),
    Interrupted,
}

/// Side effects, separated so the decision logic is testable without processes.
pub trait Effects {
    fn say(&mut self, line: &str);
    fn build(&mut self, target: Target) -> BuildOutcome;
    /// Copy the fresh daemon build aside. Never touches the running daemon.
    fn stage_daemon(&mut self) -> Result<(), String>;
    fn daemon_running(&mut self) -> bool;
    /// Start the dev daemon, first installing the staged build when `install`.
    fn start_daemon(&mut self, install: Option<DaemonStamp>) -> Result<(), String>;
    /// Consent-gated stop of the dev daemon. Returns how many shells it ended.
    fn stop_daemon(&mut self) -> Result<usize, String>;
    fn preview_running(&mut self) -> bool;
    /// Replace the preview: prepare the new binary while the old preview still runs,
    /// close the old one, launch, and wait until the window is up.
    fn launch_preview(&mut self, install_build: bool) -> Result<Duration, String>;
    fn stop_preview(&mut self);
}

#[derive(Debug, Default, Clone, Copy)]
struct ClientState {
    /// Inputs of the last build attempt, successful or not.
    attempted: Option<Digest>,
    /// Inputs and protocol of the newest successful build.
    built: Option<(Digest, Digest)>,
    /// Inputs of the client in the preview.
    live: Option<Digest>,
    /// A build that compiled but could not open its window. It is not offered again
    /// until the inputs change and a new build replaces it.
    rejected: Option<Digest>,
}

#[derive(Debug, Default, Clone, Copy)]
struct DaemonState {
    attempted: Option<Digest>,
    /// Inputs of the build waiting in the stage.
    staged: Option<Digest>,
    /// The binary installed in the dev runtime (and running, if a daemon runs).
    installed: Option<DaemonStamp>,
}

pub struct Controller {
    runner_at_start: Digest,
    client: ClientState,
    daemon: DaemonState,
    /// Notices already shown, keyed by the fingerprints they describe.
    runner_notice: bool,
    stale_notice: Option<Digest>,
    hold_notice: Option<(Digest, Digest)>,
}

/// What a sync achieved, for latency reporting.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Sync {
    pub preview_replaced: bool,
}

impl Controller {
    pub fn new(fingerprints: &Fingerprints, installed: Option<DaemonStamp>) -> Self {
        Self {
            runner_at_start: fingerprints.runner,
            client: ClientState::default(),
            daemon: DaemonState {
                installed,
                ..DaemonState::default()
            },
            runner_notice: false,
            stale_notice: None,
            hold_notice: None,
        }
    }

    /// Bring builds and processes up to date with the sources.
    pub fn sync(&mut self, current: &Fingerprints, effects: &mut impl Effects) -> Sync {
        if current.runner != self.runner_at_start && !self.runner_notice {
            self.runner_notice = true;
            effects.say("Runner sources changed — restart `cargo dev` to use them");
        }
        let client_ready = self.ensure_client(current, effects);
        if self.daemon.installed.map(|stamp| stamp.daemon) != Some(current.daemon) {
            self.ensure_daemon(current, effects);
        }
        if !effects.daemon_running() && !self.start_idle_daemon(current, effects) {
            return Sync::default();
        }
        self.notify_stale(current, effects);

        let compatible = self.compatible_build();
        if compatible && self.fresh_build() {
            return self.launch(true, effects);
        }
        if client_ready && !compatible {
            self.notify_hold(current, effects);
        }
        Sync::default()
    }

    /// Reopen the preview after it was closed, without rebuilding.
    pub fn reopen(&mut self, effects: &mut impl Effects) {
        if effects.preview_running() {
            effects.say("Preview is already open");
            return;
        }
        if !effects.daemon_running() {
            effects.say("Dev daemon is not running; save a file or press r⏎ to start it");
            return;
        }
        let install = self.compatible_build() && self.fresh_build();
        if !install && self.client.live.is_none() {
            effects.say("No client build compatible with the running dev daemon yet");
            return;
        }
        self.launch(install, effects);
    }

    /// Explicit request: replace the dev daemon with the current sources. Ends dev shells.
    pub fn restart_daemon(&mut self, current: &Fingerprints, effects: &mut impl Effects) {
        if self.daemon.installed.map(|stamp| stamp.daemon) != Some(current.daemon) {
            self.ensure_daemon(current, effects);
        }
        let fresh = DaemonStamp {
            daemon: current.daemon,
            protocol: current.protocol,
        };
        let install = if self.daemon.installed == Some(fresh) {
            None
        } else if self.daemon.staged == Some(current.daemon) {
            Some(fresh)
        } else {
            effects.say("Daemon build is failing — fix it before restarting the dev daemon");
            return;
        };
        let protocol_changes =
            self.daemon.installed.map(|stamp| stamp.protocol) != Some(current.protocol);
        if protocol_changes && !self.ensure_client(current, effects) {
            effects.say("Client build is failing — fix it before restarting the dev daemon");
            return;
        }
        effects.say("Restarting dev daemon…");
        effects.stop_preview();
        if effects.daemon_running() {
            match effects.stop_daemon() {
                Ok(shells) => effects.say(&format!(
                    "Dev daemon stopped ({} ended)",
                    count(shells, "dev shell")
                )),
                Err(error) => {
                    effects.say(&format!("Could not stop dev daemon: {error}"));
                    self.client.live = None;
                    self.relaunch_compatible(effects);
                    return;
                }
            }
        }
        if let Err(error) = effects.start_daemon(install) {
            effects.say(&format!("Could not start dev daemon: {error}"));
            return;
        }
        if let Some(stamp) = install {
            self.daemon.installed = Some(stamp);
        }
        self.stale_notice = None;
        self.hold_notice = None;
        self.client.live = None;
        self.relaunch_compatible(effects);
    }

    pub fn shutdown(&mut self, effects: &mut impl Effects) {
        effects.stop_preview();
    }

    fn relaunch_compatible(&mut self, effects: &mut impl Effects) {
        if self.compatible_build() {
            self.launch(self.fresh_build(), effects);
        }
    }

    fn launch(&mut self, install: bool, effects: &mut impl Effects) -> Sync {
        let action = if effects.preview_running() {
            "Refreshing preview…"
        } else {
            "Launching preview…"
        };
        effects.say(action);
        match effects.launch_preview(install) {
            Ok(elapsed) => {
                if install {
                    self.client.live = self.built_client();
                }
                effects.say(&format!("Preview ready in {}", seconds(elapsed)));
                Sync {
                    preview_replaced: true,
                }
            }
            Err(error) => {
                if install {
                    self.client.rejected = self.built_client();
                }
                effects.say(&format!("Preview failed to start: {error}"));
                Sync::default()
            }
        }
    }

    /// Returns whether a successful build for the current client inputs exists.
    fn ensure_client(&mut self, current: &Fingerprints, effects: &mut impl Effects) -> bool {
        if self.client.attempted != Some(current.client) {
            match self.build(Target::Client, effects) {
                Some(true) => self.client.built = Some((current.client, current.protocol)),
                Some(false) => {}
                None => return false,
            }
            self.client.attempted = Some(current.client);
        }
        self.built_client() == Some(current.client)
    }

    fn ensure_daemon(&mut self, current: &Fingerprints, effects: &mut impl Effects) {
        if self.daemon.attempted == Some(current.daemon) {
            return;
        }
        match self.build(Target::Daemon, effects) {
            Some(true) => match effects.stage_daemon() {
                Ok(()) => self.daemon.staged = Some(current.daemon),
                Err(error) => effects.say(&format!("Could not stage daemon build: {error}")),
            },
            Some(false) => {}
            None => return,
        }
        self.daemon.attempted = Some(current.daemon);
    }

    /// `None` when the build was interrupted, so the same inputs are attempted again.
    fn build(&mut self, target: Target, effects: &mut impl Effects) -> Option<bool> {
        effects.say(&format!("Building {}…", target.label()));
        match effects.build(target) {
            BuildOutcome::Built { elapsed, warnings } => {
                let warnings = match warnings {
                    0 => String::new(),
                    1 => " (1 warning)".to_owned(),
                    count => format!(" ({count} warnings)"),
                };
                effects.say(&format!(
                    "Built {} in {}{warnings}",
                    target.label(),
                    seconds(elapsed)
                ));
                Some(true)
            }
            BuildOutcome::Failed(diagnostics) => {
                effects.say(&match target {
                    Target::Client => "Build failed — keeping previous preview running".to_owned(),
                    Target::Daemon => {
                        "Daemon build failed — dev daemon keeps running unchanged".to_owned()
                    }
                });
                effects.say(diagnostics.trim_end());
                Some(false)
            }
            BuildOutcome::Interrupted => None,
        }
    }

    /// No daemon is running, so no shells can be lost: start the newest good build.
    /// A live preview pins the installed daemon, because it may only speak that protocol.
    fn start_idle_daemon(&mut self, current: &Fingerprints, effects: &mut impl Effects) -> bool {
        let fresh = DaemonStamp {
            daemon: current.daemon,
            protocol: current.protocol,
        };
        let upgrade = self.daemon.staged == Some(current.daemon)
            && self.daemon.installed != Some(fresh)
            && !effects.preview_running();
        let install = if upgrade {
            Some(fresh)
        } else if self.daemon.installed.is_some() {
            None
        } else {
            effects.say("Dev daemon cannot start until its build succeeds");
            return false;
        };
        match effects.start_daemon(install) {
            Ok(()) => {
                if let Some(stamp) = install {
                    self.daemon.installed = Some(stamp);
                }
                true
            }
            Err(error) => {
                effects.say(&format!("Could not start dev daemon: {error}"));
                false
            }
        }
    }

    fn notify_stale(&mut self, current: &Fingerprints, effects: &mut impl Effects) {
        let Some(installed) = self.daemon.installed else {
            return;
        };
        if installed.daemon == current.daemon {
            self.stale_notice = None;
            return;
        }
        if installed.protocol != current.protocol || self.stale_notice == Some(current.daemon) {
            return;
        }
        self.stale_notice = Some(current.daemon);
        effects.say(
            "Daemon sources changed — the running dev daemon is stale; shells keep running. \
             Press r⏎ to restart it (ends dev shells).",
        );
    }

    fn notify_hold(&mut self, current: &Fingerprints, effects: &mut impl Effects) {
        let key = (current.client, current.protocol);
        if self.hold_notice == Some(key) {
            return;
        }
        self.hold_notice = Some(key);
        let preview = if effects.preview_running() {
            "keeping the current preview"
        } else {
            "not opening a preview"
        };
        effects.say(&format!(
            "Protocol changed — {preview} because the new client cannot talk to the running dev \
             daemon. Press r⏎ to restart the dev daemon (ends dev shells)."
        ));
    }

    fn built_client(&self) -> Option<Digest> {
        self.client.built.map(|(client, _)| client)
    }

    /// A successful build that is neither in the preview nor known to fail at launch.
    fn fresh_build(&self) -> bool {
        let built = self.built_client();
        built.is_some() && built != self.client.live && built != self.client.rejected
    }

    /// Whether the newest client build speaks the installed daemon's protocol.
    fn compatible_build(&self) -> bool {
        match (self.client.built, self.daemon.installed) {
            (Some((_, protocol)), Some(installed)) => protocol == installed.protocol,
            _ => false,
        }
    }
}

pub fn seconds(duration: Duration) -> String {
    format!("{:.1}s", duration.as_secs_f64())
}

pub fn count(number: usize, noun: &str) -> String {
    format!("{number} {noun}{}", if number == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Process stand-in that records every side effect.
    #[derive(Default)]
    struct Fake {
        log: Vec<String>,
        said: Vec<String>,
        failures: VecDeque<Target>,
        daemon_running: bool,
        preview_running: bool,
        stop_fails: bool,
        /// The next installed build compiles but cannot open; the old one is restored.
        launch_fails: bool,
    }

    impl Fake {
        fn fail_next(&mut self, target: Target) {
            self.failures.push_back(target);
        }
        fn take(&mut self) -> Vec<String> {
            std::mem::take(&mut self.log)
        }
        fn heard(&self, text: &str) -> bool {
            self.said.iter().any(|line| line.contains(text))
        }
    }

    impl Effects for Fake {
        fn say(&mut self, line: &str) {
            self.said.push(line.to_owned());
        }
        fn build(&mut self, target: Target) -> BuildOutcome {
            self.log.push(format!("build {}", target.label()));
            if self.failures.front() == Some(&target) {
                self.failures.pop_front();
                return BuildOutcome::Failed("error[E0308]: mismatched types".into());
            }
            BuildOutcome::Built {
                elapsed: Duration::from_millis(2800),
                warnings: 0,
            }
        }
        fn stage_daemon(&mut self) -> Result<(), String> {
            self.log.push("stage daemon".into());
            Ok(())
        }
        fn daemon_running(&mut self) -> bool {
            self.daemon_running
        }
        fn start_daemon(&mut self, install: Option<DaemonStamp>) -> Result<(), String> {
            self.log
                .push(format!("start daemon install={}", install.is_some()));
            self.daemon_running = true;
            Ok(())
        }
        fn stop_daemon(&mut self) -> Result<usize, String> {
            self.log.push("stop daemon".into());
            if self.stop_fails {
                return Err("consent changed".into());
            }
            self.daemon_running = false;
            Ok(2)
        }
        fn preview_running(&mut self) -> bool {
            self.preview_running
        }
        fn launch_preview(&mut self, install_build: bool) -> Result<Duration, String> {
            self.log
                .push(format!("launch preview install={install_build}"));
            self.preview_running = true;
            if install_build && std::mem::take(&mut self.launch_fails) {
                return Err("preview exited (exit code 101); previous preview restored".into());
            }
            Ok(Duration::from_millis(900))
        }
        fn stop_preview(&mut self) {
            self.log.push("stop preview".into());
            self.preview_running = false;
        }
    }

    fn sources(client: u8, daemon: u8, protocol: u8) -> Fingerprints {
        // Client and daemon inputs include the protocol, as `Tree::fingerprints` does.
        Fingerprints {
            client: Digest::of(client.wrapping_add(protocol.wrapping_mul(16))),
            daemon: Digest::of(
                daemon
                    .wrapping_add(protocol.wrapping_mul(16))
                    .wrapping_add(128),
            ),
            protocol: Digest::of(protocol),
            runner: Digest::of(0),
        }
    }

    /// A session that has started from scratch and shows a preview.
    fn running(fake: &mut Fake) -> Controller {
        let start = sources(1, 1, 1);
        let mut controller = Controller::new(&start, None);
        controller.sync(&start, fake);
        fake.take();
        fake.said.clear();
        controller
    }

    #[test]
    fn cold_start_builds_both_then_starts_daemon_before_preview() {
        let mut fake = Fake::default();
        let start = sources(1, 1, 1);
        let sync = Controller::new(&start, None).sync(&start, &mut fake);
        assert!(sync.preview_replaced);
        assert_eq!(
            fake.take(),
            [
                "build client",
                "build daemon",
                "stage daemon",
                "start daemon install=true",
                "launch preview install=true",
            ]
        );
    }

    #[test]
    fn reattaching_to_a_current_daemon_skips_daemon_build_and_keeps_it_running() {
        let mut fake = Fake {
            daemon_running: true,
            ..Fake::default()
        };
        let start = sources(1, 1, 1);
        let installed = DaemonStamp {
            daemon: start.daemon,
            protocol: start.protocol,
        };
        Controller::new(&start, Some(installed)).sync(&start, &mut fake);
        assert_eq!(fake.take(), ["build client", "launch preview install=true"]);
    }

    #[test]
    fn successful_client_build_replaces_only_the_preview() {
        let mut fake = Fake::default();
        let mut controller = running(&mut fake);
        let sync = controller.sync(&sources(2, 1, 1), &mut fake);
        assert!(sync.preview_replaced);
        assert_eq!(fake.take(), ["build client", "launch preview install=true"]);
        assert!(fake.daemon_running);
    }

    #[test]
    fn failed_client_build_keeps_previous_preview_until_fixed() {
        let mut fake = Fake::default();
        let mut controller = running(&mut fake);

        fake.fail_next(Target::Client);
        let sync = controller.sync(&sources(2, 1, 1), &mut fake);
        assert!(!sync.preview_replaced);
        assert_eq!(fake.take(), ["build client"]);
        assert!(fake.preview_running);
        assert!(fake.heard("Build failed — keeping previous preview running"));
        assert!(fake.heard("mismatched types"));

        // An unrelated poll with the same broken sources does not rebuild.
        controller.sync(&sources(2, 1, 1), &mut fake);
        assert!(fake.take().is_empty());

        controller.sync(&sources(3, 1, 1), &mut fake);
        assert_eq!(fake.take(), ["build client", "launch preview install=true"]);
    }

    #[test]
    fn reverting_to_the_live_client_does_not_relaunch() {
        let mut fake = Fake::default();
        let mut controller = running(&mut fake);
        fake.fail_next(Target::Client);
        controller.sync(&sources(2, 1, 1), &mut fake);
        fake.take();
        controller.sync(&sources(1, 1, 1), &mut fake);
        assert_eq!(fake.take(), ["build client"]);
    }

    #[test]
    fn daemon_change_builds_and_warns_but_never_stops_the_daemon() {
        let mut fake = Fake::default();
        let mut controller = running(&mut fake);
        controller.sync(&sources(1, 2, 1), &mut fake);
        assert_eq!(fake.take(), ["build daemon", "stage daemon"]);
        assert!(fake.daemon_running && fake.preview_running);
        assert!(fake.heard("dev daemon is stale"));

        // Further client work continues against the stale daemon; warning is not repeated.
        fake.said.clear();
        controller.sync(&sources(2, 2, 1), &mut fake);
        assert_eq!(fake.take(), ["build client", "launch preview install=true"]);
        assert!(!fake.heard("stale"));
    }

    #[test]
    fn protocol_change_holds_the_new_client_until_explicit_restart() {
        let mut fake = Fake::default();
        let mut controller = running(&mut fake);
        let changed = sources(1, 1, 2);

        controller.sync(&changed, &mut fake);
        assert_eq!(
            fake.take(),
            ["build client", "build daemon", "stage daemon"]
        );
        assert!(fake.preview_running && fake.daemon_running);
        assert!(fake.heard("Protocol changed — keeping the current preview"));

        controller.restart_daemon(&changed, &mut fake);
        assert_eq!(
            fake.take(),
            [
                "stop preview",
                "stop daemon",
                "start daemon install=true",
                "launch preview install=true",
            ]
        );
        assert!(fake.heard("2 dev shells ended"));

        // Now in step: ordinary client edits swap again.
        controller.sync(&sources(2, 1, 2), &mut fake);
        assert_eq!(fake.take(), ["build client", "launch preview install=true"]);
    }

    #[test]
    fn reverting_a_protocol_change_releases_the_hold_without_restart() {
        let mut fake = Fake::default();
        let mut controller = running(&mut fake);
        controller.sync(&sources(1, 1, 2), &mut fake);
        fake.take();
        controller.sync(&sources(1, 1, 1), &mut fake);
        assert_eq!(fake.take(), ["build client"]);
        assert!(fake.daemon_running && fake.preview_running);
    }

    #[test]
    fn restart_is_refused_while_the_daemon_build_fails() {
        let mut fake = Fake::default();
        let mut controller = running(&mut fake);
        fake.fail_next(Target::Daemon);
        controller.sync(&sources(1, 2, 1), &mut fake);
        fake.take();

        controller.restart_daemon(&sources(1, 2, 1), &mut fake);
        assert!(fake.take().is_empty(), "no stop without a good replacement");
        assert!(fake.daemon_running && fake.preview_running);
        assert!(fake.heard("Daemon build is failing"));
    }

    #[test]
    fn failed_consent_stop_reopens_the_preview_on_the_old_daemon() {
        let mut fake = Fake::default();
        let mut controller = running(&mut fake);
        controller.sync(&sources(1, 2, 1), &mut fake);
        fake.take();
        fake.stop_fails = true;

        controller.restart_daemon(&sources(1, 2, 1), &mut fake);
        assert_eq!(
            fake.take(),
            ["stop preview", "stop daemon", "launch preview install=true"]
        );
        assert!(fake.daemon_running && fake.preview_running);
    }

    #[test]
    fn build_that_cannot_open_is_not_relaunched_until_sources_change() {
        let mut fake = Fake::default();
        let mut controller = running(&mut fake);
        fake.launch_fails = true;
        let sync = controller.sync(&sources(2, 1, 1), &mut fake);
        assert!(!sync.preview_replaced);
        assert_eq!(fake.take(), ["build client", "launch preview install=true"]);
        assert!(fake.heard("previous preview restored"));

        // An unrelated daemon edit must not retry the crashing build.
        controller.sync(&sources(2, 2, 1), &mut fake);
        assert_eq!(fake.take(), ["build daemon", "stage daemon"]);

        controller.sync(&sources(3, 2, 1), &mut fake);
        assert_eq!(fake.take(), ["build client", "launch preview install=true"]);
    }

    #[test]
    fn closed_preview_reopens_on_request_without_rebuilding() {
        let mut fake = Fake::default();
        let mut controller = running(&mut fake);
        fake.preview_running = false;
        controller.reopen(&mut fake);
        assert_eq!(fake.take(), ["launch preview install=false"]);

        controller.reopen(&mut fake);
        assert!(fake.take().is_empty());
        assert!(fake.heard("already open"));
    }

    #[test]
    fn interrupted_build_is_silent_and_retried_on_next_change() {
        struct Interrupting(Fake);
        impl Effects for Interrupting {
            fn say(&mut self, line: &str) {
                self.0.say(line)
            }
            fn build(&mut self, target: Target) -> BuildOutcome {
                self.0.log.push(format!("build {}", target.label()));
                BuildOutcome::Interrupted
            }
            fn stage_daemon(&mut self) -> Result<(), String> {
                self.0.stage_daemon()
            }
            fn daemon_running(&mut self) -> bool {
                self.0.daemon_running()
            }
            fn start_daemon(&mut self, install: Option<DaemonStamp>) -> Result<(), String> {
                self.0.start_daemon(install)
            }
            fn stop_daemon(&mut self) -> Result<usize, String> {
                self.0.stop_daemon()
            }
            fn preview_running(&mut self) -> bool {
                self.0.preview_running()
            }
            fn launch_preview(&mut self, install_build: bool) -> Result<Duration, String> {
                self.0.launch_preview(install_build)
            }
            fn stop_preview(&mut self) {
                self.0.stop_preview()
            }
        }
        let mut fake = Fake::default();
        let mut controller = running(&mut fake);
        let mut interrupting = Interrupting(fake);
        controller.sync(&sources(2, 1, 1), &mut interrupting);
        assert_eq!(interrupting.0.take(), ["build client"]);
        assert!(!interrupting.0.heard("failed"));
        controller.sync(&sources(2, 1, 1), &mut interrupting);
        assert_eq!(interrupting.0.take(), ["build client"]);
    }
}
