use crate::Result;
use crate::doctor::{self, Finding, Fix};
use crate::machine::{self, Daemon, Inspection};
use crate::plain;
use crate::transaction::{self, Event, Outcome, Readiness};
use compi_client::theme::{ThemeColors, ThemePreset};
use gpui::{
    App, Application, Bounds, Context, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, WindowBounds, WindowControlArea, WindowOptions, actions, div,
    img, prelude::*, px, rgb, size,
};
use std::env;
use std::mem::size_of;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;

const WINDOW_WIDTH: f32 = 600.0;
const WINDOW_HEIGHT: f32 = 500.0;
const COLORS: &ThemeColors = ThemePreset::DarkGlass.colors();
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
    Fixing(String),
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
}

actions!(
    compi_installer,
    [PrimaryAction, CloseInstaller, ToggleDataCleanup]
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
    Application::new().run(move |cx: &mut App| {
        cx.bind_keys([
            gpui::KeyBinding::new("enter", PrimaryAction, Some("CompiInstaller")),
            gpui::KeyBinding::new("escape", CloseInstaller, Some("CompiInstaller")),
            gpui::KeyBinding::new("ctrl-d", ToggleDataCleanup, Some("CompiInstaller")),
        ]);
        let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
        let busy = Arc::new(AtomicBool::new(false));
        let close_guard = busy.clone();
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
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
                window.focus(&view.focus_handle);
                cx.activate(true);
            })
            .expect("failed to activate Compi Setup window");
        cx.on_window_closed(|cx| cx.quit()).detach();
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
        _: &mut Window,
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
        };
        if matches!(this.launch, LaunchMode::Preview(PreviewState::Installing)) {
            this.busy.store(true, Ordering::Release);
            this.stage = "Copying files".into();
            this.progress = Some(42);
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

    /// Keyboard Enter. It never ends shells: that takes a click on a button listing them.
    fn primary_key(&mut self, _: &PrimaryAction, window: &mut Window, cx: &mut Context<Self>) {
        match &self.state {
            SurfaceState::Ready if self.readiness.plan.needs_consent() => {}
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
            SurfaceState::Installing => {
                if self.cancellable {
                    self.cancel.store(true, Ordering::Release);
                    self.stage = "Cancelling".into();
                    self.cancellable = false;
                    cx.notify();
                }
            }
            SurfaceState::Checking | SurfaceState::Fixing(_) => {}
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
        if !self.busy.load(Ordering::Acquire) {
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
            self.run_doctor(Vec::new(), None);
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
    fn run_doctor(&mut self, fixes: Vec<Fix>, title: Option<String>) {
        self.busy.store(true, Ordering::Release);
        self.launch_failed = false;
        self.state = match title {
            Some(title) => SurfaceState::Fixing(title),
            None => SurfaceState::Checking,
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
            Some(fix) => self.run_doctor(vec![fix], Some(finding.title())),
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
        self.run_doctor(fixes, Some("Fixing Compi".into()));
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

    fn package_size(&self) -> Option<u64> {
        match &self.launch {
            LaunchMode::Live {
                source: InstallerSource::Package(bytes),
                ..
            } => Some(bytes.len() as u64),
            LaunchMode::Preview(_) => Some(43 * 1024 * 1024),
            _ => None,
        }
    }

    fn action_label(&self) -> String {
        let installed = self.readiness.installed.is_some();
        match (&self.state, self.operation) {
            (SurfaceState::Checking | SurfaceState::Fixing(_), _) => "Checking…".into(),
            (SurfaceState::Ready, operation) if self.readiness.plan.needs_consent() => {
                match operation {
                    InstallerOperation::Install => "End shells and update",
                    InstallerOperation::Repair => "End shells and reinstall",
                    InstallerOperation::Remove => "End shells and remove",
                }
                .into()
            }
            (SurfaceState::Ready, InstallerOperation::Install) if installed => {
                "Update Compi".into()
            }
            (SurfaceState::Ready, InstallerOperation::Install) => "Install Compi".into(),
            (SurfaceState::Ready, InstallerOperation::Repair) => "Reinstall files".into(),
            (SurfaceState::Ready, InstallerOperation::Remove) => "Remove Compi".into(),
            (SurfaceState::Installing, _) if self.cancellable => "Cancel".into(),
            (SurfaceState::Installing, _) => "Working…".into(),
            (SurfaceState::Complete, InstallerOperation::Remove) => "Close".into(),
            (SurfaceState::Complete, _) if self.outcome.restart_required => "Close".into(),
            (SurfaceState::Complete, _) => "Open Compi".into(),
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
        }
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

    fn render_header(&self) -> impl IntoElement {
        div()
            .h(px(44.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .pl_4()
            .bg(rgb(COLORS.surface))
            .border_b_1()
            .border_color(rgb(COLORS.border))
            .window_control_area(WindowControlArea::Drag)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(brand_mark())
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child("COMPI"),
                    ),
            )
            .when(!self.busy.load(Ordering::Acquire), |header| {
                header.child(
                    div()
                        .id("installer-close")
                        .w(px(46.0))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(19.0))
                        .text_color(rgb(COLORS.muted))
                        .window_control_area(WindowControlArea::Close)
                        .hover(|style| {
                            style
                                .bg(rgb(0x4a1f22))
                                .text_color(rgb(COLORS.foreground))
                                .cursor_pointer()
                        })
                        .on_click(|_, window, _| window.remove_window())
                        .child("×"),
                )
            })
    }

    fn render_ready(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let version = env!("CARGO_PKG_VERSION");
        let (title, description) = match (self.operation, &self.readiness.installed) {
            (InstallerOperation::Install, Some(installed)) if installed == version => (
                "Reinstall Compi".to_owned(),
                format!("Version {version} is already installed."),
            ),
            (InstallerOperation::Install, Some(installed)) => (
                "Update Compi".to_owned(),
                format!("Version {installed} → {version}"),
            ),
            (InstallerOperation::Install, None) => (
                "Install Compi".to_owned(),
                "A terminal for WSL. Your shells keep running when you close the window."
                    .to_owned(),
            ),
            (InstallerOperation::Repair, _) => (
                "Reinstall files".to_owned(),
                "Replaces Compi's program files. Your settings and workspaces are kept.".to_owned(),
            ),
            (InstallerOperation::Remove, _) => (
                "Remove Compi".to_owned(),
                "Your settings and workspaces are kept unless you choose to delete them."
                    .to_owned(),
            ),
        };
        let plan = &self.readiness.plan;
        let detail = match (self.operation, self.package_size()) {
            (InstallerOperation::Install, Some(bytes)) => {
                Some(format!("Compi {version} · {}", plain::megabytes(bytes)))
            }
            _ => None,
        };
        div()
            .flex_1()
            .id("installer-ready-body")
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .px(px(38.0))
            .pt(px(34.0))
            .child(
                div()
                    .text_size(px(25.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .mt_2()
                    .max_w(px(500.0))
                    .text_size(px(15.0))
                    .line_height(px(22.0))
                    .text_color(rgb(COLORS.muted))
                    .child(description),
            )
            .when_some(detail, |content, detail| {
                content.child(
                    div()
                        .mt_4()
                        .text_size(px(13.0))
                        .text_color(rgb(COLORS.muted))
                        .child(detail),
                )
            })
            .when(plan.needs_consent(), |content| {
                let count = plan.shells.len();
                let verb = match self.operation {
                    InstallerOperation::Install => "Updating",
                    InstallerOperation::Repair => "Reinstalling",
                    InstallerOperation::Remove => "Removing Compi",
                };
                content.child(
                    div()
                        .mt_5()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(15.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(rgb(COLORS.error))
                                .child(format!("{verb} ends {count} {}:", shells_word(count))),
                        )
                        .child(
                            div()
                                .text_size(px(14.0))
                                .line_height(px(21.0))
                                .child(shell_list(&plan.shells)),
                        )
                        .when(self.operation == InstallerOperation::Install, |content| {
                            content.child(
                                div()
                                    .mt_1()
                                    .text_size(px(13.0))
                                    .text_color(rgb(COLORS.muted))
                                    .child("They run an older version this update can't keep."),
                            )
                        }),
                )
            })
            .when(
                !plan.needs_consent() && plan.restarts_service(),
                |content| {
                    content.child(div().mt_5().text_size(px(14.0)).child(
                        if self.operation == InstallerOperation::Remove {
                            "Compi's background service will stop."
                        } else {
                            "Compi's background service will restart."
                        },
                    ))
                },
            )
            .when(self.operation == InstallerOperation::Remove, |content| {
                content.child(
                    div()
                        .id("remove-managed-data")
                        .tab_index(0)
                        .mt_5()
                        .text_size(px(14.0))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.remove_data = !this.remove_data;
                            cx.notify();
                        }))
                        .child(format!(
                            "{}  Also delete settings and workspaces",
                            if self.remove_data { "☑" } else { "☐" }
                        )),
                )
            })
    }

    fn render_progress(&self) -> impl IntoElement {
        let (title, line) = match &self.state {
            SurfaceState::Checking if self.operation == InstallerOperation::Repair => {
                ("Checking Compi…".to_owned(), String::new())
            }
            SurfaceState::Checking => ("Checking your PC…".to_owned(), String::new()),
            SurfaceState::Fixing(title) => (format!("{title}…"), String::new()),
            _ => (
                match self.operation {
                    InstallerOperation::Install => "Installing Compi",
                    InstallerOperation::Repair => "Reinstalling files",
                    InstallerOperation::Remove => "Removing Compi",
                }
                .to_owned(),
                match self.progress {
                    Some(percent) => format!("{} · {percent}%", self.stage),
                    None => self.stage.clone(),
                },
            ),
        };
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(px(44.0))
            .child(
                div()
                    .w(px(42.0))
                    .h(px(42.0))
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(COLORS.accent))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(brand_mark()),
            )
            .child(
                div()
                    .mt_5()
                    .text_size(px(22.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .when(!line.is_empty(), |content| {
                content.child(
                    div()
                        .mt_2()
                        .text_size(px(14.0))
                        .text_color(rgb(COLORS.muted))
                        .child(line),
                )
            })
    }

    fn render_complete(&self) -> impl IntoElement {
        let version = self
            .outcome
            .version
            .as_deref()
            .unwrap_or(env!("CARGO_PKG_VERSION"));
        let (title, detail) = match self.operation {
            InstallerOperation::Remove => (
                "Compi removed",
                match &self.outcome.cleanup {
                    Some(Ok(())) => "Your settings and workspaces were deleted.".to_owned(),
                    Some(Err(error)) => error.clone(),
                    None => "Your settings and workspaces were kept.".to_owned(),
                },
            ),
            InstallerOperation::Repair => ("Compi is repaired", format!("Version {version}")),
            InstallerOperation::Install => ("Compi is ready", format!("Version {version}")),
        };
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(px(44.0))
            .child(
                div()
                    .w(px(42.0))
                    .h(px(42.0))
                    .rounded_full()
                    .bg(rgb(COLORS.accent))
                    .text_color(rgb(COLORS.background))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(22.0))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("✓"),
            )
            .child(
                div()
                    .mt_5()
                    .text_size(px(22.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .mt_2()
                    .max_w(px(460.0))
                    .text_size(px(14.0))
                    .text_color(rgb(COLORS.muted))
                    .child(detail),
            )
            .when(self.outcome.restart_required, |content| {
                content.child(
                    div()
                        .mt_3()
                        .text_size(px(14.0))
                        .child("Restart Windows to finish."),
                )
            })
    }

    fn render_error(&self, error: &str) -> impl IntoElement {
        div()
            .flex_1()
            .id("installer-error-body")
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .px(px(38.0))
            .pt(px(46.0))
            .child(
                div()
                    .text_size(px(24.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(if self.attempted {
                        "Setup couldn't finish"
                    } else {
                        "Setup can't continue yet"
                    }),
            )
            .child(
                div()
                    .mt_3()
                    .max_w(px(500.0))
                    .text_size(px(15.0))
                    .line_height(px(22.0))
                    .child(error.to_owned()),
            )
    }

    fn render_doctor(&self, fix_error: Option<&str>, cx: &mut Context<Self>) -> impl IntoElement {
        let findings = self.findings();
        let healthy = findings.is_empty();
        let reinstall_shells: Vec<String> = self
            .inspection
            .as_ref()
            .map(|inspection| doctor::reinstall_plan(&daemon_facts(&inspection.daemons)).shells)
            .unwrap_or_default();
        let mut body = div()
            .flex_1()
            .id("doctor-body")
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .px(px(38.0))
            .pt(px(28.0))
            .pb(px(16.0))
            .child(
                div()
                    .text_size(px(24.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(if healthy {
                        "No problems found".to_owned()
                    } else if findings.len() == 1 {
                        "Compi has 1 problem".to_owned()
                    } else {
                        format!("Compi has {} problems", findings.len())
                    }),
            );
        if let Some(error) = fix_error {
            body = body.child(
                div()
                    .mt_3()
                    .text_size(px(14.0))
                    .line_height(px(20.0))
                    .text_color(rgb(COLORS.error))
                    .child(format!("Couldn't fix it: {error}")),
            );
        }
        if healthy {
            body = body
                .child(
                    div()
                        .mt_2()
                        .text_size(px(15.0))
                        .text_color(rgb(COLORS.muted))
                        .child("Compi is set up correctly."),
                )
                .when(!reinstall_shells.is_empty(), |body| {
                    body.child(
                        div()
                            .mt_5()
                            .text_size(px(13.0))
                            .line_height(px(19.0))
                            .text_color(rgb(COLORS.muted))
                            .child(format!(
                                "Reinstalling files ends {} {}: {}",
                                reinstall_shells.len(),
                                shells_word(reinstall_shells.len()),
                                shell_list(&reinstall_shells)
                            )),
                    )
                });
            return body;
        }
        for (index, finding) in findings.iter().enumerate() {
            let label = fix_label(finding);
            let shells = finding.shells();
            body =
                body.child(
                    div()
                        .mt_4()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(COLORS.border))
                        .bg(rgb(COLORS.surface))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_4()
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(px(15.0))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(finding.title()),
                                )
                                .child(
                                    div()
                                        .text_size(px(13.0))
                                        .line_height(px(19.0))
                                        .text_color(rgb(COLORS.muted))
                                        .child(finding.explanation()),
                                )
                                .when_some(finding.command(), |row, command| {
                                    row.child(
                                        div()
                                            .mt_1()
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .bg(rgb(COLORS.background))
                                            .font_family("Cascadia Mono")
                                            .text_size(px(13.0))
                                            .child(command),
                                    )
                                })
                                .when(!shells.is_empty(), |row| {
                                    row.child(
                                        div()
                                            .text_size(px(13.0))
                                            .line_height(px(19.0))
                                            .text_color(rgb(COLORS.error))
                                            .child(format!("Ends: {}", shell_list(shells))),
                                    )
                                }),
                        )
                        .when_some(label, |row, label| {
                            row.child(button(("doctor-fix", index), label, false).on_click(
                                cx.listener(move |this, _, _, cx| this.fix_one(index, cx)),
                            ))
                        }),
                );
        }
        body
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = !matches!(self.state, SurfaceState::Checking | SurfaceState::Fixing(_))
            && (!matches!(self.state, SurfaceState::Installing) || self.cancellable);
        let label = self.action_label();
        let doctor = matches!(self.state, SurfaceState::Doctor { .. });
        let healthy = doctor && self.findings().is_empty();
        let fixable = doctor && !all_fixes(self.findings()).is_empty();
        let show_log = doctor || matches!(self.state, SurfaceState::Error(_));
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
            .h(px(76.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px(px(38.0))
            .border_t_1()
            .border_color(rgb(COLORS.border))
            .child(if show_log {
                div()
                    .id("open-installer-log")
                    .tab_index(0)
                    .text_size(px(13.0))
                    .text_color(rgb(COLORS.muted))
                    .cursor_pointer()
                    .hover(|style| style.text_color(rgb(COLORS.foreground)))
                    .child("Open log")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Err(error) = Command::new("notepad.exe").arg(this.log_path()).spawn()
                        {
                            transaction::log(&format!("Opening the log failed: {error}"));
                            this.state = SurfaceState::Error("The log couldn't be opened.".into());
                            cx.notify();
                        }
                    }))
                    .into_any_element()
            } else {
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(COLORS.muted))
                    .child("Your projects are never touched")
                    .into_any_element()
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .when(healthy, |buttons| {
                        buttons.child(button("doctor-reinstall", reinstall_label, false).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.start_reinstall(cx);
                                cx.notify();
                            }),
                        ))
                    })
                    .when(fixable, |buttons| {
                        buttons.child(
                            button("doctor-done", "Done".to_owned(), false)
                                .on_click(cx.listener(|_, _, window, _| window.remove_window())),
                        )
                    })
                    .when(doctor && !healthy && !fixable, |buttons| {
                        buttons.child(
                            button("doctor-recheck", "Check again".to_owned(), false).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.run_doctor(Vec::new(), None);
                                    cx.notify();
                                }),
                            ),
                        )
                    })
                    .child({
                        let primary = button("installer-primary-action", label, true);
                        if !enabled {
                            primary
                                .bg(rgb(COLORS.border))
                                .text_color(rgb(COLORS.muted))
                                .into_any_element()
                        } else if doctor && !fixable {
                            primary
                                .on_click(cx.listener(|_, _, window, _| window.remove_window()))
                                .into_any_element()
                        } else {
                            primary
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.primary_action(window, cx)
                                }))
                                .into_any_element()
                        }
                    }),
            )
    }
}

fn daemon_facts(daemons: &[Daemon]) -> Vec<doctor::RunningDaemon> {
    daemons.iter().map(|daemon| daemon.fact.clone()).collect()
}

fn button(
    id: impl Into<gpui::ElementId>,
    label: String,
    primary: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .tab_index(0)
        .flex_none()
        .min_w(px(if primary { 142.0 } else { 96.0 }))
        .h(px(38.0))
        .px_4()
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(14.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .cursor_pointer()
        .when(primary, |button| {
            button
                .bg(rgb(COLORS.accent))
                .text_color(rgb(COLORS.background))
                .hover(|style| style.bg(rgb(0xcbea2f)))
        })
        .when(!primary, |button| {
            button
                .border_1()
                .border_color(rgb(COLORS.border))
                .hover(|style| style.bg(rgb(COLORS.surface)))
        })
        .child(SharedString::from(label))
}

impl Focusable for InstallerApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for InstallerApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.state {
            SurfaceState::Ready => self.render_ready(cx).into_any_element(),
            SurfaceState::Installing | SurfaceState::Checking | SurfaceState::Fixing(_) => {
                self.render_progress().into_any_element()
            }
            SurfaceState::Complete => self.render_complete().into_any_element(),
            SurfaceState::Error(error) => self.render_error(error).into_any_element(),
            SurfaceState::Doctor { fix_error } => {
                let fix_error = fix_error.clone();
                self.render_doctor(fix_error.as_deref(), cx)
                    .into_any_element()
            }
        };
        div()
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
            .size_full()
            .flex()
            .flex_col()
            .font_family("Segoe UI Variable")
            .text_size(px(14.0))
            .text_color(rgb(COLORS.foreground))
            .bg(rgb(COLORS.background))
            .child(self.render_header())
            .child(content)
            .child(self.render_footer(cx))
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
                    Finding::Wsl(doctor::WslFact::NotWsl2 {
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

fn brand_mark() -> impl IntoElement {
    static MARK: std::sync::LazyLock<Arc<gpui::RenderImage>> = std::sync::LazyLock::new(|| {
        let mut rgba = image::load_from_memory(include_bytes!(
            "../../../assets/Compi-desktopappicon-v4.png"
        ))
        .expect("approved embedded v4 artwork")
        .into_rgba8();
        for pixel in rgba.pixels_mut() {
            pixel.0.swap(0, 2);
        }
        Arc::new(gpui::RenderImage::new(smallvec::smallvec![
            image::Frame::new(rgba)
        ]))
    });
    img(MARK.clone()).size(px(24.0))
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
