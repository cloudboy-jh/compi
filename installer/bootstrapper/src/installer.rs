use crate::Result;
use crate::doctor::{self, Finding, Fix};
use crate::machine::{self, Daemon, Inspection};
use crate::plain;
use crate::transaction::{self, Event, Outcome, Readiness};
use compi_client::theme::{ThemeColors, ThemePreset};
use gpui::{
    Animation, AnimationExt, App, Bounds, BoxShadow, Context, FocusHandle, Focusable, Hsla,
    IntoElement, ParentElement, PathBuilder, Pixels, Render, Role, SharedString, Styled, Toggled,
    Window, WindowAppearance, WindowBounds, WindowControlArea, WindowOptions, actions, canvas, div,
    point, prelude::*, px, rgb, size,
};
use std::env;
use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::mem::size_of;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::thread;
use std::time::Duration;
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;

const WINDOW_WIDTH: f32 = 420.0;
const WINDOW_HEIGHT: f32 = 480.0;
const MIN_WIDTH: f32 = 380.0;
const MIN_HEIGHT: f32 = 440.0;
/// The drag strip along the top, with the ×.
const TOP_STRIP: f32 = 36.0;
/// The square holding the glow, the ring and the mark.
const EMBLEM: f32 = 200.0;
const MARK: f32 = 136.0;
const RING_RADIUS: f32 = 90.0;
const RING_WIDTH: f32 = 3.0;
/// Room under the name for a status word or error, the button and two short lines.
const ACTION_HEIGHT: f32 = 132.0;
/// The doctor's side inset.
const INSET: f32 = 24.0;
/// Static Segoe UI has real semibold faces; the variable family renders them regular.
const DISPLAY_FONT: &str = "Segoe UI";
/// Shells listed by name before "and N more".
const LISTED_SHELLS: usize = 6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallerOperation {
    Install,
    /// Opens the doctor; its "Reinstall files" runs Windows Installer repair.
    Repair,
    Remove,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewState {
    Ready,
    Upgrade,
    Consent,
    Installing,
    Checking,
    Complete,
    Error,
    Remove,
    Doctor,
    Healthy,
}

#[derive(Clone)]
pub(crate) enum InstallerSource {
    Package(&'static [u8]),
    ProductCode(String),
}

#[derive(Clone)]
enum LaunchMode {
    Live {
        source: InstallerSource,
        operation: InstallerOperation,
    },
    Preview(PreviewState),
}

#[derive(Clone)]
enum SurfaceState {
    Checking,
    Ready,
    Installing,
    Complete,
    Error(String),
    Doctor { fix_error: Option<String> },
    Fixing,
}

struct InstallerApp {
    launch: LaunchMode,
    operation: InstallerOperation,
    state: SurfaceState,
    readiness: Readiness,
    inspection: Option<Inspection>,
    /// The failure happened after changes started, not while checking.
    attempted: bool,
    /// "Fix all" reinstalls files after its other fixes.
    pending_reinstall: bool,
    event_tx: async_channel::Sender<Event>,
    cancel: Arc<AtomicBool>,
    cancellable: bool,
    progress: Option<u32>,
    stage: String,
    outcome: Outcome,
    remove_data: bool,
    busy: Arc<AtomicBool>,
    focus_handle: FocusHandle,
    launch_failed: bool,
    look: Look,
}

actions!(
    compi_installer,
    [
        PrimaryAction,
        CloseInstaller,
        ToggleDataCleanup,
        FocusNext,
        FocusPrevious
    ]
);

pub fn run(msi: &'static [u8], operation: InstallerOperation) {
    run_mode(LaunchMode::Live {
        source: InstallerSource::Package(msi),
        operation,
    });
}

pub fn run_product_action(product_code: String, operation: InstallerOperation) {
    run_mode(LaunchMode::Live {
        source: InstallerSource::ProductCode(product_code),
        operation,
    });
}

pub fn run_preview(state: PreviewState) {
    run_mode(LaunchMode::Preview(state));
}

/// Unattended distribution qualification uses the same transaction as the actual surface.
/// It never ends shells: that needs the window's explicit consent.
pub fn run_silent(
    msi: Option<&'static [u8]>,
    product_code: Option<String>,
    operation: InstallerOperation,
    remove_data: bool,
    cancel_after_ms: Option<u64>,
) -> i32 {
    let source = match (msi, product_code) {
        (Some(bytes), _) => InstallerSource::Package(bytes),
        (_, Some(code)) => InstallerSource::ProductCode(code),
        _ => return 2,
    };
    let approved = match transaction::preflight(operation) {
        Ok(readiness) if readiness.plan.needs_consent() => {
            eprintln!(
                "This would end {} open shells; run Setup without --silent to approve.",
                readiness.plan.shells.len()
            );
            return 1;
        }
        Ok(readiness) => readiness.daemons,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    let cancel = Arc::new(AtomicBool::new(false));
    if let Some(delay) = cancel_after_ms {
        let cancel = cancel.clone();
        thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(delay));
            cancel.store(true, Ordering::Release);
        });
    }
    let (sender, receiver) = async_channel::unbounded();
    // Drain progress without blocking the worker, retaining real stage events in the log.
    thread::spawn(move || {
        while let Ok(event) = receiver.recv_blocking() {
            eprintln!("{event:?}");
        }
    });
    match transaction::perform(source, operation, remove_data, &approved, cancel, sender) {
        Ok(outcome) => {
            if let Some(Err(error)) = outcome.cleanup {
                eprintln!("Product removal succeeded; managed-data cleanup failed: {error}");
                1
            } else if outcome.restart_required {
                3010
            } else {
                0
            }
        }
        Err(error) => {
            eprintln!("{error}");
            error
                .downcast_ref::<transaction::MsiFailure>()
                .map(|failure| failure.code as i32)
                .unwrap_or(1)
        }
    }
}

pub fn run_msi_action(args: &[String]) -> i32 {
    match crate::msi_actions::action(args) {
        Ok(()) => 0,
        Err(error) => {
            transaction::log(&format!(
                "{}: {error}",
                args.first().map_or("", String::as_str)
            ));
            eprintln!("{}", plain::short(&error.to_string()));
            1
        }
    }
}

/// Logs the full error and returns the one sentence the window shows.
fn shown(error: &crate::Error, log: &std::path::Path) -> String {
    let detail = error.to_string();
    plain::log(log, &format!("Error: {detail}"));
    plain::short(&detail)
}

fn run_mode(launch: LaunchMode) {
    gpui_platform::application().run(move |cx: &mut App| {
        cx.bind_keys([
            gpui::KeyBinding::new("enter", PrimaryAction, Some("CompiInstaller")),
            gpui::KeyBinding::new("escape", CloseInstaller, Some("CompiInstaller")),
            gpui::KeyBinding::new("ctrl-d", ToggleDataCleanup, Some("CompiInstaller")),
            gpui::KeyBinding::new("tab", FocusNext, Some("CompiInstaller")),
            gpui::KeyBinding::new("shift-tab", FocusPrevious, Some("CompiInstaller")),
        ]);
        let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
        let busy = Arc::new(AtomicBool::new(false));
        let close_guard = busy.clone();
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Compi Setup".into()),
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    focus: true,
                    ..Default::default()
                },
                move |window, cx| cx.new(|cx| InstallerApp::new(launch, busy, window, cx)),
            )
            .expect("failed to open Compi Setup window");
        window
            .update(cx, |view, window, cx| {
                window.on_window_should_close(cx, move |_, _| !close_guard.load(Ordering::Acquire));
                window.focus(&view.focus_handle, cx);
                cx.activate(true);
            })
            .expect("failed to activate Compi Setup window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
    });
}

fn shells_word(count: usize) -> &'static str {
    if count == 1 { "shell" } else { "shells" }
}

/// "api · server, web, and 3 more".
fn shell_list(shells: &[String]) -> String {
    let mut listed = shells
        .iter()
        .take(LISTED_SHELLS)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if shells.len() > LISTED_SHELLS {
        listed.push_str(&format!(", and {} more", shells.len() - LISTED_SHELLS));
    }
    listed
}

fn fix_label(finding: &Finding) -> Option<String> {
    let shells = finding.shells().len();
    let action = match finding.fix()? {
        Fix::Reinstall => "reinstall",
        _ => "fix",
    };
    Some(if shells == 0 {
        let mut label = action.to_owned();
        label[..1].make_ascii_uppercase();
        label
    } else {
        format!("End {shells} {} and {action}", shells_word(shells))
    })
}

/// Fixes "Fix all" applies, in order, without duplicates.
fn all_fixes(findings: &[Finding]) -> Vec<Fix> {
    let mut fixes: Vec<Fix> = Vec::new();
    for fix in findings.iter().filter_map(Finding::fix) {
        if !fixes.contains(&fix) {
            fixes.push(fix);
        }
    }
    fixes
}

fn shells_of_fixes(findings: &[Finding]) -> Vec<String> {
    let mut shells: Vec<String> = Vec::new();
    for finding in findings.iter().filter(|finding| finding.fix().is_some()) {
        for shell in finding.shells() {
            if !shells.contains(shell) {
                shells.push(shell.clone());
            }
        }
    }
    shells
}

impl InstallerApp {
    fn new(
        launch: LaunchMode,
        busy: Arc<AtomicBool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (event_tx, event_rx) = async_channel::unbounded();
        let (operation, state, readiness, inspection) = match &launch {
            LaunchMode::Live { operation, .. } => (
                *operation,
                SurfaceState::Checking,
                Readiness::default(),
                None,
            ),
            LaunchMode::Preview(preview) => preview_configuration(*preview),
        };
        let mut this = Self {
            launch,
            operation,
            state,
            readiness,
            inspection,
            attempted: false,
            pending_reinstall: false,
            event_tx,
            busy,
            cancel: Arc::new(AtomicBool::new(false)),
            cancellable: true,
            progress: None,
            stage: String::new(),
            outcome: Outcome::default(),
            remove_data: false,
            focus_handle: cx.focus_handle(),
            launch_failed: false,
            look: look(window.appearance()),
        };
        cx.observe_window_appearance(window, |this, window, cx| {
            this.look = look(window.appearance());
            cx.notify();
        })
        .detach();
        match this.launch {
            LaunchMode::Preview(PreviewState::Installing) => {
                this.busy.store(true, Ordering::Release);
                this.stage = "Copying files".into();
                this.progress = Some(42);
            }
            LaunchMode::Preview(PreviewState::Checking) => {
                this.busy.store(true, Ordering::Release);
            }
            _ => {}
        }
        cx.spawn(async move |weak, cx| {
            while let Ok(event) = event_rx.recv().await {
                if weak
                    .update(cx, |this, cx| {
                        this.handle(event, cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        if matches!(this.launch, LaunchMode::Live { .. }) {
            this.recheck();
        }
        this
    }

    fn handle(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Preflight(result) => {
                self.busy.store(false, Ordering::Release);
                match result {
                    Ok(readiness) => {
                        self.readiness = readiness;
                        self.state = SurfaceState::Ready;
                    }
                    Err(error) => {
                        self.attempted = false;
                        self.state = SurfaceState::Error(error);
                    }
                }
            }
            Event::Progress {
                stage,
                percent,
                cancellable,
            } => {
                self.stage = stage;
                self.progress = percent;
                self.cancellable = cancellable;
            }
            Event::Finished(result) => {
                self.busy.store(false, Ordering::Release);
                self.state = match result {
                    Ok(outcome) => {
                        self.outcome = outcome;
                        SurfaceState::Complete
                    }
                    Err(error) => {
                        self.attempted = true;
                        SurfaceState::Error(error)
                    }
                };
            }
            Event::Inspected {
                inspection,
                fix_error,
            } => {
                self.busy.store(false, Ordering::Release);
                match inspection {
                    Ok(inspection) => {
                        let reinstall = std::mem::take(&mut self.pending_reinstall)
                            && fix_error.is_none()
                            && !doctor::reinstall_plan(&daemon_facts(&inspection.daemons))
                                .needs_consent();
                        self.inspection = Some(inspection);
                        self.state = SurfaceState::Doctor { fix_error };
                        if reinstall {
                            self.start_reinstall(cx);
                        }
                    }
                    Err(error) => {
                        self.attempted = false;
                        self.state = SurfaceState::Error(error);
                    }
                }
            }
        }
    }

    /// Keyboard Enter. It never ends shells or cancels: those take a button. A focused
    /// button or checkbox handles Enter itself.
    fn primary_key(&mut self, _: &PrimaryAction, window: &mut Window, cx: &mut Context<Self>) {
        if window
            .focused(cx)
            .is_some_and(|focused| focused != self.focus_handle)
        {
            cx.propagate();
            return;
        }
        match &self.state {
            SurfaceState::Ready if self.readiness.plan.needs_consent() => {}
            SurfaceState::Installing => {}
            SurfaceState::Doctor { .. } => window.remove_window(),
            _ => self.primary_action(window, cx),
        }
    }

    fn primary_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.launch, LaunchMode::Preview(_)) {
            if matches!(
                self.state,
                SurfaceState::Complete | SurfaceState::Doctor { .. }
            ) {
                window.remove_window();
            }
            return;
        }
        match &self.state {
            SurfaceState::Ready => {
                let approved = self.readiness.daemons.clone();
                self.start_installation(self.operation, approved, cx)
            }
            SurfaceState::Installing | SurfaceState::Checking | SurfaceState::Fixing => {}
            SurfaceState::Doctor { .. } => self.fix_all(cx),
            SurfaceState::Complete => self.finish(window, cx),
            SurfaceState::Error(_) if self.launch_failed => {
                self.state = SurfaceState::Complete;
                self.finish(window, cx);
            }
            SurfaceState::Error(_) => self.recheck(),
        }
        cx.notify();
    }

    fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.operation == InstallerOperation::Remove || self.outcome.restart_required {
            window.remove_window();
            return;
        }
        let failure = match Command::new(installed_executable()).spawn() {
            Ok(mut child) => match child.try_wait() {
                Ok(Some(status)) if !status.success() => {
                    Some(format!("Compi closed right after starting ({status})"))
                }
                Err(error) => Some(error.to_string()),
                _ => None,
            },
            Err(error) => Some(error.to_string()),
        };
        match failure {
            None => window.remove_window(),
            Some(detail) => {
                transaction::log(&format!("Opening Compi failed: {detail}"));
                self.state = SurfaceState::Error(
                    "Compi didn't open. Try again, or run Setup's repair.".into(),
                );
                self.launch_failed = true;
                cx.notify();
            }
        }
    }

    fn close_installer(&mut self, _: &CloseInstaller, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(window, cx);
    }

    /// × and Escape: closes the window, or cancels the change while Windows Installer can
    /// still undo it. A preview always closes.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.busy.load(Ordering::Acquire) || matches!(self.launch, LaunchMode::Preview(_)) {
            window.remove_window();
        } else if matches!(self.state, SurfaceState::Installing) && self.cancellable {
            self.cancel.store(true, Ordering::Release);
            self.cancellable = false;
            self.stage = "Cancelling".into();
            cx.notify();
        }
    }

    fn recheck(&mut self) {
        if self.operation == InstallerOperation::Repair {
            self.run_doctor(Vec::new());
            return;
        }
        self.state = SurfaceState::Checking;
        self.busy.store(true, Ordering::Release);
        self.launch_failed = false;
        self.stage = "Checking your PC".into();
        self.progress = None;
        self.cancellable = false;
        let operation = self.operation;
        let sender = self.event_tx.clone();
        thread::spawn(move || {
            let log = transaction::log_path().unwrap_or_default();
            let result = transaction::preflight(operation).map_err(|error| shown(&error, &log));
            let _ = sender.send_blocking(Event::Preflight(result));
        });
    }

    /// Applies `fixes` (none = just check), then checks again.
    fn run_doctor(&mut self, fixes: Vec<Fix>) {
        self.busy.store(true, Ordering::Release);
        self.launch_failed = false;
        self.state = if fixes.is_empty() {
            SurfaceState::Checking
        } else {
            SurfaceState::Fixing
        };
        let inspection = self.inspection.clone();
        let sender = self.event_tx.clone();
        thread::spawn(move || {
            let log = machine::doctor_log();
            let mut fix_error = None;
            if let Some(inspection) = &inspection {
                for fix in fixes.iter().filter(|fix| **fix != Fix::Reinstall) {
                    if let Err(error) = machine::apply(fix, inspection) {
                        fix_error = Some(shown(&error, &log));
                        break;
                    }
                }
            }
            let inspection = machine::inspect().map_err(|error| shown(&error, &log));
            let _ = sender.send_blocking(Event::Inspected {
                inspection,
                fix_error,
            });
        });
    }

    fn fix_one(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(finding) = self
            .inspection
            .as_ref()
            .and_then(|inspection| inspection.findings.get(index))
            .cloned()
        else {
            return;
        };
        match finding.fix() {
            Some(Fix::Reinstall) => self.start_reinstall(cx),
            Some(fix) => self.run_doctor(vec![fix]),
            None => {}
        }
        cx.notify();
    }

    fn fix_all(&mut self, cx: &mut Context<Self>) {
        let Some(inspection) = &self.inspection else {
            return;
        };
        let fixes = all_fixes(&inspection.findings);
        if fixes.is_empty() {
            return;
        }
        if fixes == [Fix::Reinstall] {
            self.start_reinstall(cx);
            return;
        }
        self.pending_reinstall = fixes.contains(&Fix::Reinstall);
        self.run_doctor(fixes);
    }

    fn start_reinstall(&mut self, cx: &mut Context<Self>) {
        let approved = self
            .inspection
            .as_ref()
            .map(|inspection| inspection.daemons.clone())
            .unwrap_or_default();
        self.start_installation(InstallerOperation::Repair, approved, cx);
    }

    fn start_installation(
        &mut self,
        operation: InstallerOperation,
        approved: Vec<Daemon>,
        cx: &mut Context<Self>,
    ) {
        if self.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let LaunchMode::Live { source, .. } = self.launch.clone() else {
            self.busy.store(false, Ordering::Release);
            return;
        };
        self.cancel.store(false, Ordering::Release);
        self.cancellable = true;
        self.progress = None;
        self.stage = "Getting ready".into();
        self.state = SurfaceState::Installing;
        cx.notify();
        let sender = self.event_tx.clone();
        let cancel = self.cancel.clone();
        let remove_data = self.remove_data;
        thread::spawn(move || {
            let log = transaction::log_path().unwrap_or_default();
            let result = transaction::perform(
                source,
                operation,
                remove_data,
                &approved,
                cancel,
                sender.clone(),
            )
            .map_err(|error| shown(&error, &log));
            let _ = sender.send_blocking(Event::Finished(result));
        });
    }

    /// The installed version this run updates from.
    fn updating_from(&self) -> Option<&str> {
        self.readiness.installed.as_deref().filter(|installed| {
            self.operation == InstallerOperation::Install && *installed != env!("CARGO_PKG_VERSION")
        })
    }

    /// Completion offers Open Compi unless Compi is gone or Windows must restart first.
    fn opens_compi(&self) -> bool {
        self.operation != InstallerOperation::Remove && !self.outcome.restart_required
    }

    fn primary_label(&self) -> Option<String> {
        let plan = &self.readiness.plan;
        Some(match (&self.state, self.operation) {
            (SurfaceState::Checking | SurfaceState::Fixing | SurfaceState::Installing, _) => {
                return None;
            }
            (SurfaceState::Ready, operation) if plan.needs_consent() => {
                let count = plan.shells.len();
                let action = match operation {
                    InstallerOperation::Install if self.updating_from().is_some() => "update",
                    InstallerOperation::Install | InstallerOperation::Repair => "reinstall",
                    InstallerOperation::Remove => "remove",
                };
                format!("End {count} {} and {action}", shells_word(count))
            }
            (SurfaceState::Ready, InstallerOperation::Install)
                if self.updating_from().is_some() =>
            {
                "Update".into()
            }
            (SurfaceState::Ready, InstallerOperation::Install)
                if self.readiness.installed.is_none() =>
            {
                "Install".into()
            }
            (SurfaceState::Ready, InstallerOperation::Install | InstallerOperation::Repair) => {
                "Reinstall".into()
            }
            (SurfaceState::Ready, InstallerOperation::Remove) => "Remove".into(),
            (SurfaceState::Complete, _) if self.opens_compi() => "Open Compi".into(),
            (SurfaceState::Complete, _) => return None,
            (SurfaceState::Error(_), _) if self.launch_failed => "Open Compi".into(),
            (SurfaceState::Error(_), _) => "Try again".into(),
            (SurfaceState::Doctor { .. }, _) => {
                let findings = self.findings();
                if all_fixes(findings).is_empty() {
                    "Done".into()
                } else {
                    let shells = shells_of_fixes(findings).len();
                    if shells == 0 {
                        "Fix all".into()
                    } else {
                        format!("End {shells} {} and fix all", shells_word(shells))
                    }
                }
            }
        })
    }

    /// The one word under the name while Setup works.
    fn status_word(&self) -> Option<&'static str> {
        Some(match &self.state {
            SurfaceState::Checking => "Checking",
            SurfaceState::Fixing => "Fixing",
            SurfaceState::Installing if self.cancel.load(Ordering::Acquire) => "Cancelling",
            SurfaceState::Installing => match self.operation {
                InstallerOperation::Install if self.updating_from().is_some() => "Updating",
                InstallerOperation::Install if self.readiness.installed.is_none() => "Installing",
                InstallerOperation::Install | InstallerOperation::Repair => "Reinstalling",
                InstallerOperation::Remove => "Removing",
            },
            _ => return None,
        })
    }

    fn findings(&self) -> &[Finding] {
        self.inspection
            .as_ref()
            .map_or(&[], |inspection| inspection.findings.as_slice())
    }

    fn log_path(&self) -> PathBuf {
        if self.operation == InstallerOperation::Repair {
            machine::doctor_log()
        } else {
            installer_log_path()
        }
    }

    /// The drag strip along the top and the ×, which closes, or cancels while Windows
    /// Installer can still undo the change.
    fn render_top(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.look.colors;
        let busy = self.busy.load(Ordering::Acquire);
        let closable =
            !busy || (matches!(self.state, SurfaceState::Installing) && self.cancellable);
        div()
            .absolute()
            .top_0()
            .left_0()
            .w_full()
            .h(px(TOP_STRIP))
            .flex()
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .window_control_area(WindowControlArea::Drag),
            )
            .when(closable, |strip| {
                strip.child(
                    div()
                        .id("installer-close")
                        .role(Role::Button)
                        .aria_label(if busy { "Cancel" } else { "Close" })
                        .w(px(44.0))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(move |style| {
                            style.bg(Hsla::from(rgb(colors.foreground)).alpha(0.08))
                        })
                        .on_click(cx.listener(|this, _, window, cx| this.dismiss(window, cx)))
                        .child(close_glyph(colors.muted)),
                )
            })
    }

    /// Every screen but the doctor: the mark, the name, and one action.
    fn render_stage(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.look.colors;
        let error = match &self.state {
            SurfaceState::Error(error) => Some(error.clone()),
            _ => None,
        };
        div()
            .flex_1()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .pt(px(TOP_STRIP))
            .child(self.render_emblem())
            .child(
                div()
                    .mt(px(12.0))
                    .text_size(px(27.0))
                    .line_height(px(34.0))
                    .font_family(DISPLAY_FONT)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Compi"),
            )
            // Fixed height, so the mark stays put from screen to screen.
            .child(
                div()
                    .h(px(ACTION_HEIGHT))
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .when_some(self.status_word(), |area, word| {
                        area.child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(colors.muted))
                                .child(word),
                        )
                    })
                    .when_some(error, |area, error| {
                        area.child(
                            div()
                                .max_w(px(320.0))
                                .px_4()
                                .text_center()
                                .line_clamp(2)
                                .text_size(px(13.0))
                                .line_height(px(18.0))
                                .text_color(Hsla::from(rgb(colors.error)).alpha(0.85))
                                .child(error),
                        )
                    })
                    .child(self.render_action(cx)),
            )
    }

    fn render_emblem(&self) -> impl IntoElement {
        let colors = self.look.colors;
        let reduced = *REDUCED_MOTION;
        let halo = glow(colors.accent, self.look.glow);
        let halo = if reduced {
            halo.opacity(0.85).into_any_element()
        } else {
            halo.with_animation(
                "installer-glow",
                Animation::new(Duration::from_secs(4)).repeat(),
                |halo, turn| halo.opacity(0.7 + 0.3 * (0.5 - 0.5 * (turn * TAU).cos())),
            )
            .into_any_element()
        };
        let progress = div()
            .id("installer-progress")
            .absolute()
            .inset_0()
            .role(Role::ProgressIndicator)
            .aria_label(self.status_word().unwrap_or("Working"));
        let ring = match &self.state {
            SurfaceState::Installing if self.progress.is_some() => {
                let percent = self.progress.unwrap_or(0).min(100);
                Some(
                    progress
                        .aria_numeric_value(f64::from(percent))
                        .aria_min_numeric_value(0.0)
                        .aria_max_numeric_value(100.0)
                        .child(ring(colors.accent, 0.0, percent as f32 / 100.0, true))
                        .into_any_element(),
                )
            }
            SurfaceState::Installing | SurfaceState::Checking | SurfaceState::Fixing => {
                Some(if reduced {
                    progress
                        .child(ring(colors.accent, 0.0, 0.0, true))
                        .into_any_element()
                } else {
                    let accent = colors.accent;
                    progress
                        .with_animation(
                            "installer-spin",
                            Animation::new(Duration::from_millis(1400)).repeat(),
                            move |progress, turn| progress.child(ring(accent, turn, 0.22, true)),
                        )
                        .into_any_element()
                })
            }
            SurfaceState::Complete if !reduced => Some(
                div()
                    .absolute()
                    .inset_0()
                    .child(glow(colors.accent, self.look.glow).with_animation(
                        "installer-complete-glow",
                        Animation::new(Duration::from_millis(1400)),
                        |glow, delta| glow.opacity((delta * PI).sin()),
                    ))
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .child(ring(colors.accent, 0.0, 1.0, false))
                            .with_animation(
                                "installer-complete-ring",
                                Animation::new(Duration::from_millis(1200))
                                    .with_easing(gpui::ease_in_out),
                                |ring, delta| ring.opacity(1.0 - delta),
                            ),
                    )
                    .into_any_element(),
            ),
            _ => None,
        };
        let inset = (EMBLEM - MARK) / 2.0;
        div()
            .relative()
            .flex_none()
            .size(px(EMBLEM))
            .child(halo)
            .children(ring)
            .child(
                div()
                    .absolute()
                    .top(px(inset))
                    .left(px(inset))
                    .child(mark(MARK, colors.accent)),
            )
    }

    /// The pill and the short lines under it.
    fn render_action(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.look.colors;
        let ready = matches!(self.state, SurfaceState::Ready);
        let consent = ready && self.readiness.plan.needs_consent();
        let mut action = div()
            .mt(px(20.0))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(10.0));
        if let Some(label) = self.primary_label() {
            let fill = if consent { colors.error } else { colors.accent };
            action = action.child(
                pill(label, fill, colors)
                    .on_click(cx.listener(|this, _, window, cx| this.primary_action(window, cx))),
            );
        } else if matches!(self.state, SurfaceState::Complete) {
            action = action.child(
                div()
                    .h(px(40.0))
                    .flex()
                    .items_center()
                    .text_size(px(14.0))
                    .text_color(rgb(colors.muted))
                    .child(if self.operation == InstallerOperation::Remove {
                        "Compi removed"
                    } else {
                        "Restart Windows to finish"
                    }),
            );
        }
        if consent {
            action = action.child(self.render_consent_line());
        }
        if ready && self.operation == InstallerOperation::Remove {
            action = action.child(self.render_data_checkbox(cx));
        }
        if let (SurfaceState::Complete, Some(Err(error))) = (&self.state, &self.outcome.cleanup) {
            action = action.child(
                div()
                    .max_w(px(320.0))
                    .text_center()
                    .text_size(px(12.0))
                    .text_color(Hsla::from(rgb(colors.error)).alpha(0.85))
                    .child(error.clone()),
            );
        }
        if matches!(self.state, SurfaceState::Error(_)) {
            action = action.child(self.render_log_link(cx));
        }
        action
    }

    /// Ending shells stays explicit: their names, cut to one line, all of them on hover.
    fn render_consent_line(&self) -> impl IntoElement {
        let colors = self.look.colors;
        let shells = &self.readiness.plan.shells;
        let line: SharedString = format!(
            "Ends {} {}: {}",
            shells.len(),
            shells_word(shells.len()),
            shells.join(", ")
        )
        .into();
        let tip = line.clone();
        div()
            .id("consent-shells")
            .max_w(px(320.0))
            .truncate()
            .text_size(px(12.0))
            .text_color(rgb(colors.muted))
            .aria_label(line.clone())
            .tooltip(move |_, cx| {
                cx.new(|_| Tip {
                    text: tip.clone(),
                    colors,
                })
                .into()
            })
            .child(line)
    }

    fn render_data_checkbox(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.look.colors;
        let checked = self.remove_data;
        let label = "Also delete settings and workspaces";
        div()
            .id("remove-managed-data")
            .tab_index(0)
            .role(Role::CheckBox)
            .aria_toggled(if checked {
                Toggled::True
            } else {
                Toggled::False
            })
            .aria_label(label)
            .aria_keyshortcuts("Control+D")
            .px(px(4.0))
            .py(px(2.0))
            .flex()
            .items_center()
            .gap_2()
            .rounded_sm()
            .border_1()
            .border_color(gpui::transparent_black())
            .focus_visible(move |style| style.border_color(rgb(colors.accent)))
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| {
                this.remove_data = !this.remove_data;
                cx.notify();
            }))
            .child(
                div()
                    .size(px(14.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(if checked { colors.accent } else { colors.muted }))
                    .text_size(px(10.0))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(colors.background))
                    .when(checked, |box_| box_.bg(rgb(colors.accent)).child("✓")),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(colors.muted))
                    .child(label),
            )
    }

    fn render_log_link(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.look.colors;
        div()
            .id("open-installer-log")
            .tab_index(0)
            .role(Role::Link)
            .aria_label("Open log")
            .text_size(px(12.0))
            .text_color(rgb(colors.muted))
            .cursor_pointer()
            .hover(move |style| style.text_color(rgb(colors.foreground)).underline())
            .focus_visible(move |style| style.text_color(rgb(colors.foreground)).underline())
            .child("Open log")
            .on_click(cx.listener(|this, _, _, cx| {
                if let Err(error) = Command::new("notepad.exe").arg(this.log_path()).spawn() {
                    transaction::log(&format!("Opening the log failed: {error}"));
                    this.state = SurfaceState::Error("The log couldn't be opened.".into());
                    cx.notify();
                }
            }))
    }

    fn render_doctor(&self, fix_error: Option<&str>, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.look.colors;
        let findings = self.findings();
        let healthy = findings.is_empty();
        let reinstall_shells: Vec<String> = self
            .inspection
            .as_ref()
            .map(|inspection| doctor::reinstall_plan(&daemon_facts(&inspection.daemons)).shells)
            .unwrap_or_default();
        let mut body = body("doctor-body").child(headline(if healthy {
            "No problems found".to_owned()
        } else if findings.len() == 1 {
            "Compi has 1 problem".to_owned()
        } else {
            format!("Compi has {} problems", findings.len())
        }));
        if let Some(error) = fix_error {
            body = body.child(
                div()
                    .mt_2()
                    .text_size(px(13.0))
                    .line_height(px(19.0))
                    .text_color(rgb(colors.error))
                    .child(format!("Couldn't fix it: {error}")),
            );
        }
        if healthy {
            return body
                .child(
                    div()
                        .mt_1()
                        .text_size(px(14.0))
                        .text_color(rgb(colors.muted))
                        .child("Compi is set up correctly."),
                )
                .when(!reinstall_shells.is_empty(), |body| {
                    body.child(
                        div()
                            .mt_4()
                            .text_size(px(13.0))
                            .line_height(px(19.0))
                            .text_color(rgb(colors.muted))
                            .child(format!(
                                "Reinstalling files ends {} {}: {}",
                                reinstall_shells.len(),
                                shells_word(reinstall_shells.len()),
                                shell_list(&reinstall_shells)
                            )),
                    )
                });
        }
        for (index, finding) in findings.iter().enumerate() {
            let label = fix_label(finding);
            let shells = finding.shells();
            body = body.child(
                div()
                    .mt_3()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(colors.border))
                    .bg(rgb(colors.surface))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(14.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(finding.title()),
                            )
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .line_height(px(19.0))
                                    .text_color(rgb(colors.muted))
                                    .child(finding.explanation()),
                            )
                            .when_some(finding.command(), |row, command| {
                                row.child(
                                    div()
                                        .mt_1()
                                        .px_2()
                                        .py_1()
                                        .rounded_sm()
                                        .bg(rgb(colors.background))
                                        .font_family("Cascadia Mono")
                                        .text_size(px(12.0))
                                        .child(command),
                                )
                            })
                            .when(!shells.is_empty(), |row| {
                                row.child(
                                    div()
                                        .text_size(px(13.0))
                                        .line_height(px(19.0))
                                        .text_color(rgb(colors.error))
                                        .child(format!("Ends: {}", shell_list(shells))),
                                )
                            }),
                    )
                    .when_some(label, |row, label| {
                        row.child(
                            button(("doctor-fix", index), label, false, colors).on_click(
                                cx.listener(move |this, _, _, cx| this.fix_one(index, cx)),
                            ),
                        )
                    }),
            );
        }
        body
    }

    fn render_doctor_actions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.look.colors;
        let healthy = self.findings().is_empty();
        let fixable = !all_fixes(self.findings()).is_empty();
        let reinstall_label = {
            let shells = self
                .inspection
                .as_ref()
                .map(|inspection| {
                    doctor::reinstall_plan(&daemon_facts(&inspection.daemons))
                        .shells
                        .len()
                })
                .unwrap_or(0);
            if shells == 0 {
                "Reinstall files".to_owned()
            } else {
                format!("End {shells} {} and reinstall", shells_word(shells))
            }
        };
        div()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px(px(INSET))
            .pt(px(12.0))
            .pb(px(20.0))
            .child(self.render_log_link(cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(healthy, |buttons| {
                        buttons.child(
                            button("doctor-reinstall", reinstall_label, false, colors).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.start_reinstall(cx);
                                    cx.notify();
                                }),
                            ),
                        )
                    })
                    .when(fixable, |buttons| {
                        buttons.child(
                            button("doctor-done", "Done", false, colors)
                                .on_click(cx.listener(|_, _, window, _| window.remove_window())),
                        )
                    })
                    .when(!healthy && !fixable, |buttons| {
                        buttons.child(
                            button("doctor-recheck", "Check again", false, colors).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.run_doctor(Vec::new());
                                    cx.notify();
                                }),
                            ),
                        )
                    })
                    .when_some(self.primary_label(), |buttons, label| {
                        let primary = button("installer-primary-action", label, true, colors);
                        buttons.child(if fixable {
                            primary.on_click(
                                cx.listener(|this, _, window, cx| this.primary_action(window, cx)),
                            )
                        } else {
                            primary.on_click(cx.listener(|_, _, window, _| window.remove_window()))
                        })
                    }),
            )
    }
}

fn daemon_facts(daemons: &[Daemon]) -> Vec<doctor::RunningDaemon> {
    daemons.iter().map(|daemon| daemon.fact.clone()).collect()
}

#[derive(Clone, Copy)]
struct Look {
    colors: &'static ThemeColors,
    /// Near-black, or near-white, tinted toward the accent.
    backdrop: u32,
    /// How strongly the accent glows on the backdrop.
    glow: f32,
}

/// Setup follows Windows' app mode: Compi's default dark theme, or a neutral light one.
fn look(appearance: WindowAppearance) -> Look {
    match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => Look {
            colors: ThemePreset::GitHubLight.colors(),
            backdrop: 0xf3f6fa,
            glow: 0.55,
        },
        WindowAppearance::Dark | WindowAppearance::VibrantDark => Look {
            colors: ThemePreset::DarkGlass.colors(),
            backdrop: 0x0f1209,
            glow: 1.0,
        },
    }
}

static REDUCED_MOTION: LazyLock<bool> = LazyLock::new(reduced_motion);

/// Windows turns off client-area animations under Settings → Accessibility →
/// Visual effects → Animation effects.
fn reduced_motion() -> bool {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn SystemParametersInfoW(
            action: u32,
            param: u32,
            value: *mut core::ffi::c_void,
            update: u32,
        ) -> i32;
    }
    const SPI_GETCLIENTAREAANIMATION: u32 = 0x1042;
    let mut enabled: i32 = 1;
    // SAFETY: the action writes one BOOL to the provided, correctly sized buffer.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut enabled as *mut i32).cast(),
            0,
        )
    };
    ok != 0 && enabled == 0
}

/// Soft light behind the mark: translucent discs with wide blurred shadows (GPUI has no
/// radial gradient).
fn glow(accent: u32, strength: f32) -> gpui::Div {
    let accent = Hsla::from(rgb(accent));
    let center = EMBLEM / 2.0;
    let disc = |size: f32, fill: f32, blur: f32, shadow: f32| {
        div()
            .absolute()
            .top(px(center - size / 2.0))
            .left(px(center - size / 2.0))
            .size(px(size))
            .rounded_full()
            .bg(accent.alpha(fill * strength))
            .shadow(vec![BoxShadow {
                color: accent.alpha(shadow * strength),
                offset: point(px(0.0), px(0.0)),
                blur_radius: px(blur),
                spread_radius: px(0.0),
                inset: false,
            }])
    };
    div()
        .absolute()
        .inset_0()
        .child(disc(150.0, 0.0, 110.0, 0.26))
        .child(disc(90.0, 0.0, 60.0, 0.30))
}

/// A ring around the mark: `sweep` turns of it from `start` (0 is twelve o'clock), over a
/// faint full track.
fn ring(accent: u32, start: f32, sweep: f32, track: bool) -> impl IntoElement {
    let accent = Hsla::from(rgb(accent));
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            if track {
                paint_arc(bounds, 0.0, 1.0, accent.alpha(0.16), window);
            }
            if sweep > 0.0 {
                paint_arc(bounds, start, sweep, accent, window);
            }
        },
    )
    .absolute()
    .inset_0()
}

fn paint_arc(bounds: Bounds<Pixels>, start: f32, sweep: f32, color: Hsla, window: &mut Window) {
    let center = bounds.center();
    let steps = ((sweep * 128.0).ceil() as usize).max(2);
    let mut path = PathBuilder::stroke(px(RING_WIDTH));
    for step in 0..=steps {
        let angle = (start + sweep * step as f32 / steps as f32) * TAU - FRAC_PI_2;
        let at = point(
            center.x + px(RING_RADIUS * angle.cos()),
            center.y + px(RING_RADIUS * angle.sin()),
        );
        if step == 0 {
            path.move_to(at);
        } else {
            path.line_to(at);
        }
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

struct Tip {
    text: SharedString,
    colors: &'static ThemeColors,
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let colors = self.colors;
        div()
            .max_w(px(300.0))
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.surface))
            .font_family("Segoe UI Variable")
            .text_size(px(12.0))
            .line_height(px(17.0))
            .text_color(rgb(colors.foreground))
            .child(self.text.clone())
    }
}

fn body(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_1()
        .min_h(px(0.0))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .px(px(INSET))
        .pt(px(TOP_STRIP))
        .pb(px(8.0))
}

fn headline(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(px(20.0))
        .line_height(px(28.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(text.into())
}

/// The wide primary button of every screen but the doctor.
fn pill(label: String, fill: u32, colors: &'static ThemeColors) -> gpui::Stateful<gpui::Div> {
    div()
        .id("installer-primary-action")
        .tab_index(0)
        .role(Role::Button)
        .aria_label(SharedString::from(label.clone()))
        .min_w(px(220.0))
        .h(px(40.0))
        .px(px(20.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_2()
        .border_color(gpui::transparent_black())
        .bg(rgb(fill))
        .text_color(rgb(colors.background))
        .text_size(px(14.0))
        .font_family(DISPLAY_FONT)
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|style| style.opacity(0.9))
        .focus_visible(move |style| style.border_color(rgb(colors.foreground)))
        .child(label)
}

/// The doctor's buttons.
fn button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    primary: bool,
    colors: &'static ThemeColors,
) -> gpui::Stateful<gpui::Div> {
    let label = label.into();
    div()
        .id(id)
        .tab_index(0)
        .role(Role::Button)
        .aria_label(label.clone())
        .flex_none()
        .min_w(px(80.0))
        .h(px(32.0))
        .px(px(14.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .text_size(px(13.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .map(|button| {
            if primary {
                button
                    .bg(rgb(colors.accent))
                    .border_color(rgb(colors.accent))
                    .text_color(rgb(colors.background))
                    .hover(|style| style.opacity(0.88))
                    .focus_visible(move |style| style.border_color(rgb(colors.foreground)))
            } else {
                button
                    .bg(rgb(colors.surface))
                    .border_color(rgb(colors.border))
                    .hover(move |style| style.bg(rgb(colors.surface_hover)))
                    .focus_visible(move |style| style.border_color(rgb(colors.accent)))
            }
        })
        .child(label)
}

/// The iris mark of Compi's title bar.
fn mark(size: f32, tint: u32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            compi_client::gui::brand::paint(bounds, Hsla::from(rgb(tint)), window)
        },
    )
    .size(px(size))
}

fn close_glyph(tint: u32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let at = |x: f32, y: f32| point(bounds.left() + px(x), bounds.top() + px(y));
            let mut path = PathBuilder::stroke(px(1.25));
            path.move_to(at(4.0, 4.0));
            path.line_to(at(12.0, 12.0));
            path.move_to(at(12.0, 4.0));
            path.line_to(at(4.0, 12.0));
            if let Ok(path) = path.build() {
                window.paint_path(path, Hsla::from(rgb(tint)));
            }
        },
    )
    .size(px(16.0))
}

impl Focusable for InstallerApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for InstallerApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.look.colors;
        let root = div()
            .key_context("CompiInstaller")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::primary_key))
            .on_action(cx.listener(Self::close_installer))
            .on_action(cx.listener(|this, _: &ToggleDataCleanup, _, cx| {
                if this.operation == InstallerOperation::Remove
                    && matches!(this.state, SurfaceState::Ready)
                {
                    this.remove_data = !this.remove_data;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|_, _: &FocusNext, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &FocusPrevious, window, cx| window.focus_prev(cx)))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .font_family("Segoe UI Variable")
            .text_size(px(14.0))
            .text_color(rgb(colors.foreground))
            .bg(rgb(self.look.backdrop));
        let root = match &self.state {
            SurfaceState::Doctor { fix_error } => {
                let fix_error = fix_error.clone();
                root.child(self.render_doctor(fix_error.as_deref(), cx))
                    .child(self.render_doctor_actions(cx))
            }
            _ => root.child(self.render_stage(cx)),
        };
        root.child(self.render_top(cx))
    }
}

fn preview_configuration(
    preview: PreviewState,
) -> (
    InstallerOperation,
    SurfaceState,
    Readiness,
    Option<Inspection>,
) {
    let installed = |version: &str| Readiness {
        installed: Some(version.into()),
        ..Readiness::default()
    };
    match preview {
        PreviewState::Ready => (
            InstallerOperation::Install,
            SurfaceState::Ready,
            Readiness::default(),
            None,
        ),
        PreviewState::Upgrade => (
            InstallerOperation::Install,
            SurfaceState::Ready,
            Readiness {
                plan: doctor::UpgradePlan {
                    stop: vec![None],
                    ..Default::default()
                },
                ..installed("0.1.3")
            },
            None,
        ),
        PreviewState::Consent => (
            InstallerOperation::Install,
            SurfaceState::Ready,
            Readiness {
                plan: doctor::UpgradePlan {
                    stop: vec![None],
                    shells: vec!["api · server (2)".into(), "web".into(), "Shell".into()],
                    ..Default::default()
                },
                ..installed("0.1.3")
            },
            None,
        ),
        PreviewState::Installing => (
            InstallerOperation::Install,
            SurfaceState::Installing,
            Readiness::default(),
            None,
        ),
        PreviewState::Checking => (
            InstallerOperation::Install,
            SurfaceState::Checking,
            Readiness::default(),
            None,
        ),
        PreviewState::Complete => (
            InstallerOperation::Install,
            SurfaceState::Complete,
            Readiness::default(),
            None,
        ),
        PreviewState::Error => (
            InstallerOperation::Install,
            SurfaceState::Error(
                "Windows Installer couldn't finish. Nothing was changed. Try again.".into(),
            ),
            Readiness::default(),
            None,
        ),
        PreviewState::Remove => (
            InstallerOperation::Remove,
            SurfaceState::Ready,
            Readiness::default(),
            None,
        ),
        PreviewState::Doctor | PreviewState::Healthy => {
            let findings = if preview == PreviewState::Doctor {
                vec![
                    Finding::DaemonIncompatible {
                        instance: None,
                        shells: vec!["api · server".into(), "web".into()],
                    },
                    Finding::TaskWrongVersion {
                        selected: env!("CARGO_PKG_VERSION").into(),
                    },
                    Finding::Leftovers,
                    Finding::Wsl(doctor::WslProblem::NotWsl2 {
                        name: "Ubuntu".into(),
                    }),
                ]
            } else {
                Vec::new()
            };
            (
                InstallerOperation::Repair,
                SurfaceState::Doctor { fix_error: None },
                Readiness::default(),
                Some(Inspection {
                    daemons: Vec::new(),
                    findings,
                }),
            )
        }
    }
}

pub(crate) fn ensure_supported_windows() -> Result<()> {
    let mut version = OSVERSIONINFOW {
        dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
        ..OSVERSIONINFOW::default()
    };
    let status = unsafe { RtlGetVersion(&mut version) };
    if status.0 < 0 {
        return Err(
            "Setup couldn't read the Windows version. Restart Windows, then try again.".into(),
        );
    }
    validate_windows_version(
        version.dwMajorVersion,
        version.dwMinorVersion,
        version.dwBuildNumber,
    )
}

fn validate_windows_version(major: u32, _minor: u32, build: u32) -> Result<()> {
    if major > 10 || (major == 10 && build >= 19_041) {
        Ok(())
    } else {
        Err("Compi needs Windows 10 version 2004 or newer. Update Windows, then try again.".into())
    }
}

fn installed_executable() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Programs")
        .join("Compi")
        .join("compi.exe")
}

fn installer_log_path() -> PathBuf {
    transaction::log_path().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_windows_versions_before_windows_10_2004() {
        assert!(validate_windows_version(10, 0, 19_041).is_ok());
        assert!(validate_windows_version(10, 0, 18_363).is_err());
    }

    #[test]
    fn consent_labels_count_the_shells_they_end() {
        let finding = Finding::DaemonIncompatible {
            instance: None,
            shells: vec!["api".into()],
        };
        assert_eq!(fix_label(&finding).as_deref(), Some("End 1 shell and fix"));
        assert_eq!(
            fix_label(&Finding::PayloadDamaged {
                missing: vec!["conpty.dll"],
                shells: vec!["a".into(), "b".into()],
            })
            .as_deref(),
            Some("End 2 shells and reinstall")
        );
        assert_eq!(fix_label(&Finding::Leftovers).as_deref(), Some("Fix"));
        assert_eq!(fix_label(&Finding::TaskOtherInstallation), None);
        let many: Vec<String> = (1..=8).map(|n| format!("s{n}")).collect();
        assert_eq!(shell_list(&many), "s1, s2, s3, s4, s5, s6, and 2 more");
    }
}
