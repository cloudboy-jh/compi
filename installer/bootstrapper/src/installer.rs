use crate::Result;
use crate::transaction::{self, Event, Outcome};
use compi_client::theme::{ThemeColors, ThemePreset};
use gpui::{
    App, Application, Bounds, Context, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    Styled, Window, WindowBounds, WindowControlArea, WindowOptions, actions, div, img, prelude::*,
    px, rgb, size,
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
const WINDOW_HEIGHT: f32 = 460.0;
const COLORS: &ThemeColors = ThemePreset::DarkGlass.colors();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallerOperation {
    Install,
    Repair,
    Remove,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewState {
    Ready,
    Upgrade,
    Installing,
    Complete,
    Error,
    Remove,
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
    Ready,
    Installing,
    Complete,
    Error(String),
    Checking,
}

struct InstallerApp {
    launch: LaunchMode,
    operation: InstallerOperation,
    state: SurfaceState,
    installed: bool,
    prerequisite_error: Option<String>,
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
    match transaction::perform(source, operation, remove_data, cancel, sender) {
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
            eprintln!("{error}");
            1
        }
    }
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

impl InstallerApp {
    fn new(
        launch: LaunchMode,
        busy: Arc<AtomicBool>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (event_tx, event_rx) = async_channel::unbounded();
        let installed = installed_executable().is_file();
        let (operation, state, prerequisite_error) = match &launch {
            LaunchMode::Live { operation, .. } => (*operation, SurfaceState::Checking, None),
            LaunchMode::Preview(preview) => preview_configuration(*preview),
        };
        let mut this = Self {
            launch,
            operation,
            state,
            installed,
            prerequisite_error,
            event_tx,
            busy,
            cancel: Arc::new(AtomicBool::new(false)),
            cancellable: true,
            progress: None,
            stage: "Checking Windows, installation access, and WSL guest readiness".into(),
            outcome: Outcome::default(),
            remove_data: false,
            focus_handle: cx.focus_handle(),
            launch_failed: false,
        };
        if matches!(this.launch, LaunchMode::Preview(PreviewState::Installing)) {
            this.busy.store(true, Ordering::Release);
        }
        if matches!(this.launch, LaunchMode::Preview(PreviewState::Upgrade)) {
            this.installed = true;
        }
        cx.spawn(async move |weak, cx| {
            while let Ok(event) = event_rx.recv().await {
                if weak
                    .update(cx, |this, cx| {
                        match event {
                            Event::Preflight(result) => {
                                this.busy.store(false, Ordering::Release);
                                this.prerequisite_error = result.err();
                                this.state = SurfaceState::Ready;
                            }
                            Event::Progress {
                                stage,
                                percent,
                                cancellable,
                            } => {
                                this.stage = stage;
                                this.progress = percent;
                                this.cancellable = cancellable;
                            }
                            Event::Finished(result) => {
                                this.busy.store(false, Ordering::Release);
                                this.state = match result {
                                    Ok(outcome) => {
                                        this.outcome = outcome;
                                        SurfaceState::Complete
                                    }
                                    Err(error) => SurfaceState::Error(error),
                                };
                            }
                        }
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

    fn primary_action(&mut self, _: &PrimaryAction, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.launch, LaunchMode::Preview(_)) {
            if matches!(self.state, SurfaceState::Complete) {
                window.remove_window();
            }
            return;
        }
        match &self.state {
            SurfaceState::Ready if self.prerequisite_error.is_some() => self.recheck(),
            SurfaceState::Ready => self.start_installation(cx),
            SurfaceState::Installing => {
                if self.cancellable {
                    self.cancel.store(true, Ordering::Release);
                    self.stage =
                        "Cancellation requested; waiting for Windows Installer rollback".into();
                    self.cancellable = false;
                    cx.notify();
                }
            }
            SurfaceState::Checking => {}
            SurfaceState::Complete => {
                if self.operation == InstallerOperation::Remove {
                    window.remove_window();
                } else if self.outcome.restart_required {
                    window.remove_window();
                } else {
                    match Command::new(installed_executable()).spawn() {
                        Ok(mut child) => match child.try_wait() {
                            Ok(Some(status)) if !status.success() => {
                                self.state = SurfaceState::Error(format!(
                                    "Compi exited immediately: {status}. Repair or review the installation log."
                                ));
                                self.launch_failed = true;
                                cx.notify();
                            }
                            Err(error) => {
                                self.state = SurfaceState::Error(format!(
                                    "Could not check Compi launch: {error}"
                                ));
                                self.launch_failed = true;
                                cx.notify();
                            }
                            _ => window.remove_window(),
                        },
                        Err(error) => {
                            self.state = SurfaceState::Error(format!(
                                "Compi could not launch: {error}. Installed version {}. Repair or try again.",
                                self.outcome.version.as_deref().unwrap_or("unavailable")
                            ));
                            self.launch_failed = true;
                            cx.notify();
                        }
                    }
                }
            }
            SurfaceState::Error(_) if self.launch_failed => {
                self.state = SurfaceState::Complete;
                self.primary_action(&PrimaryAction, window, cx);
            }
            SurfaceState::Error(_) => self.recheck(),
        }
        cx.notify();
    }

    fn close_installer(&mut self, _: &CloseInstaller, window: &mut Window, cx: &mut Context<Self>) {
        if !self.busy.load(Ordering::Acquire) {
            window.remove_window();
        } else if matches!(self.state, SurfaceState::Installing) && self.cancellable {
            self.cancel.store(true, Ordering::Release);
            self.cancellable = false;
            self.stage = "Cancellation requested; waiting for Windows Installer rollback".into();
            cx.notify();
        }
    }

    fn recheck(&mut self) {
        self.state = SurfaceState::Checking;
        self.prerequisite_error = None;
        self.busy.store(true, Ordering::Release);
        self.launch_failed = false;
        self.stage = if self.operation == InstallerOperation::Install {
            "Checking Windows, registration, access, disk space and WSL guest startup"
        } else {
            "Checking installation registration, access and running instances"
        }
        .into();
        self.progress = None;
        self.cancellable = false;
        let operation = self.operation;
        let sender = self.event_tx.clone();
        thread::spawn(move || {
            let result = transaction::preflight(operation).map_err(|error| error.to_string());
            let _ = sender.send_blocking(Event::Preflight(result));
        });
    }

    fn start_installation(&mut self, cx: &mut Context<Self>) {
        if self.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let LaunchMode::Live { source, operation } = self.launch.clone() else {
            return;
        };
        self.cancel.store(false, Ordering::Release);
        self.cancellable = true;
        self.progress = None;
        self.stage = "Staging embedded offline package".into();
        self.state = SurfaceState::Installing;
        cx.notify();
        let sender = self.event_tx.clone();
        let cancel = self.cancel.clone();
        let remove_data = self.remove_data;
        thread::spawn(move || {
            let result =
                transaction::perform(source, operation, remove_data, cancel, sender.clone())
                    .map_err(|error| error.to_string());
            let _ = sender.send_blocking(Event::Finished(result));
        });
    }

    fn action_label(&self) -> &'static str {
        match (&self.state, self.operation, self.installed) {
            (SurfaceState::Checking, _, _) => "Checking…",
            (SurfaceState::Ready, _, _) if self.prerequisite_error.is_some() => "Recheck",
            (SurfaceState::Ready, InstallerOperation::Install, true) => "Update Compi",
            (SurfaceState::Ready, InstallerOperation::Install, false) => "Install Compi",
            (SurfaceState::Ready, InstallerOperation::Repair, _) => "Repair Compi",
            (SurfaceState::Ready, InstallerOperation::Remove, _) => "Remove Compi",
            (SurfaceState::Installing, _, _) => {
                if self.cancellable {
                    "Cancel safely"
                } else {
                    "Working…"
                }
            }
            (SurfaceState::Complete, InstallerOperation::Remove, _) => "Close",
            (SurfaceState::Complete, _, _) if self.outcome.restart_required => {
                "Close, restart Windows"
            }
            (SurfaceState::Complete, _, _) => "Open Compi",
            (SurfaceState::Error(_), _, _) if self.launch_failed => "Retry Open Compi",
            (SurfaceState::Error(_), _, _) => "Try again",
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
        let title = match (self.operation, self.installed) {
            (InstallerOperation::Install, true) => "Update Compi",
            (InstallerOperation::Install, false) => "Your persistent WSL terminal",
            (InstallerOperation::Repair, _) => "Repair Compi",
            (InstallerOperation::Remove, _) => "Remove Compi",
        };
        let description = match self.operation {
            InstallerOperation::Install if self.installed => {
                "Install the latest build without changing your projects or terminal history."
            }
            InstallerOperation::Install => {
                "Native Windows glass for Bash sessions that keep running when the window closes."
            }
            InstallerOperation::Repair => {
                "Restore application files and per-user daemon registration."
            }
            InstallerOperation::Remove => {
                "Remove application files and its task. Keep settings, workspaces and themes by default. Running instances must be deliberately stopped first."
            }
        };
        let destination = installed_directory().display().to_string();
        let (first_check, second_check) = if self.operation == InstallerOperation::Remove {
            (
                self.prerequisite_error
                    .as_deref()
                    .unwrap_or("Application files and Start menu shortcut"),
                "Background daemon task",
            )
        } else {
            (
                self.prerequisite_error.as_deref().unwrap_or(
                    if self.operation == InstallerOperation::Repair {
                        "Repair does not require a working WSL guest"
                    } else {
                        "Default WSL2 guest startup verified"
                    },
                ),
                "Installs without administrator access",
            )
        };
        div()
            .flex_1()
            .id("installer-preflight-body")
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
                    .max_w(px(470.0))
                    .text_size(px(14.0))
                    .line_height(px(21.0))
                    .text_color(rgb(COLORS.muted))
                    .child(description),
            )
            .child(
                div()
                    .mt_6()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(status_row(self.prerequisite_error.is_none(), first_check))
                    .child(status_row(true, second_check))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(div().w(px(8.0)).h(px(8.0)))
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(rgb(COLORS.muted))
                                    .child(destination),
                            ),
                    ),
            )
            .when(self.operation == InstallerOperation::Remove, |content| {
                content.child(
                    div().id("remove-managed-data").tab_index(0).mt_4()
                        .text_size(px(12.0)).cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.remove_data = !this.remove_data;
                            cx.notify();
                        }))
                        .child(format!("{} Also delete settings, workspaces and custom themes (Ctrl+D): {}",
                            if self.remove_data { "[✓]" } else { "[ ]" },
                            application_data_directory().display())),
                )
                .when(self.remove_data, |content| {
                    content.child(div().mt_2().text_size(px(11.0)).text_color(rgb(COLORS.muted))
                        .child(format!("Exact managed entries: config.toml; workspace[-INSTANCE]-v1.json; sessions[-INSTANCE]-v1/v2.json and their temporary, corrupt and migration backups; {}. Unknown files/projects, installer recovery logs and external configuration/theme sources are kept.",
                            transaction::MANAGED_DATA_DIRECTORIES.join("/; ") + "/")))
                })
            })
    }

    fn render_installing(&self) -> impl IntoElement {
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
                    .child(if matches!(self.state, SurfaceState::Checking) {
                        "Checking prerequisites"
                    } else {
                        match self.operation {
                            InstallerOperation::Install => "Installing Compi",
                            InstallerOperation::Repair => "Repairing Compi",
                            InstallerOperation::Remove => "Removing Compi",
                        }
                    }),
            )
            .child(
                div()
                    .mt_2()
                    .text_size(px(13.0))
                    .text_color(rgb(COLORS.muted))
                    .child(match self.progress {
                        Some(percent) => format!("{} ({percent}%)", self.stage),
                        None => self.stage.clone(),
                    }),
            )
    }

    fn render_complete(&self) -> impl IntoElement {
        let (title, detail) = if self.operation == InstallerOperation::Remove {
            (
                "Compi removed",
                "Product removal succeeded. Managed user data was kept unless explicitly selected.",
            )
        } else {
            ("Compi is ready", "Open Compi or find it in the Start menu.")
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
                    .text_size(px(13.0))
                    .text_color(rgb(COLORS.muted))
                    .child(detail),
            )
            .child(
                div().mt_3().text_size(px(12.0)).text_color(rgb(COLORS.muted))
                    .child(if self.outcome.restart_required {
                        if self.operation == InstallerOperation::Remove {
                            "Product removed. Windows restart required to finish releasing application files.".into()
                        } else {
                            format!("Version {} installed. Windows restart required before normal use.", self.outcome.version.as_deref().unwrap_or(env!("CARGO_PKG_VERSION")))
                        }
                    } else if self.operation != InstallerOperation::Remove {
                        format!("Installed version {}", self.outcome.version.as_deref().unwrap_or(env!("CARGO_PKG_VERSION")))
                    } else {
                        "No Windows restart required.".into()
                    }),
            )
            .when(self.operation == InstallerOperation::Remove, |content| {
                content.child(div().mt_3().max_w(px(460.0)).text_size(px(12.0))
                    .text_color(rgb(if matches!(self.outcome.cleanup, Some(Err(_))) { COLORS.error } else { COLORS.muted }))
                    .child(match &self.outcome.cleanup {
                        Some(Ok(())) => "Selected managed data was removed.".into(),
                        Some(Err(error)) => format!("Product removed; managed-data cleanup failed: {error}"),
                        None => "Settings, workspaces and custom themes remain available for reinstall.".into(),
                    }))
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
                    .text_size(px(12.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(COLORS.error))
                    .child("INSTALLATION STOPPED"),
            )
            .child(
                div()
                    .mt_3()
                    .text_size(px(24.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Setup could not finish"),
            )
            .child(
                div()
                    .mt_3()
                    .max_w(px(480.0))
                    .text_size(px(13.0))
                    .line_height(px(20.0))
                    .text_color(rgb(COLORS.muted))
                    .child(error.to_owned()),
            )
            .child(
                div()
                    .mt_5()
                    .text_size(px(12.0))
                    .text_color(rgb(COLORS.muted))
                    .child(format!("Detailed log: {}", installer_log_path().display())),
            )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = !matches!(self.state, SurfaceState::Checking)
            && (!matches!(self.state, SurfaceState::Installing) || self.cancellable);
        let label = self.action_label();
        div()
            .h(px(76.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px(px(38.0))
            .border_t_1()
            .border_color(rgb(COLORS.border))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(COLORS.muted))
                    .child("Project files are never modified"),
            )
            .when(matches!(self.state, SurfaceState::Error(_)), |footer| {
                footer.child(
                    div()
                        .id("open-installer-log")
                        .tab_index(0)
                        .text_size(px(12.0))
                        .cursor_pointer()
                        .child("Open log")
                        .on_click(cx.listener(|this, _, _, cx| {
                            match Command::new("notepad.exe")
                                .arg(installer_log_path())
                                .spawn()
                            {
                                Ok(_) => {}
                                Err(error) => {
                                    this.state =
                                        SurfaceState::Error(format!("Cannot open log: {error}"));
                                    cx.notify();
                                }
                            }
                        })),
                )
            })
            .child(
                div()
                    .id("installer-primary-action")
                    .tab_index(0)
                    .min_w(px(142.0))
                    .h(px(38.0))
                    .px_4()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .bg(if enabled {
                        rgb(COLORS.accent)
                    } else {
                        rgb(COLORS.border)
                    })
                    .text_color(if enabled {
                        rgb(COLORS.background)
                    } else {
                        rgb(COLORS.muted)
                    })
                    .when(enabled, |button| {
                        button
                            .hover(|style| style.bg(rgb(0xcbea2f)).cursor_pointer())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.primary_action(&PrimaryAction, window, cx)
                            }))
                    })
                    .child(label),
            )
    }
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
            SurfaceState::Installing | SurfaceState::Checking => {
                self.render_installing().into_any_element()
            }
            SurfaceState::Complete => self.render_complete().into_any_element(),
            SurfaceState::Error(error) => self.render_error(error).into_any_element(),
        };
        div()
            .key_context("CompiInstaller")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::primary_action))
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
            .text_size(px(13.0))
            .text_color(rgb(COLORS.foreground))
            .bg(rgb(COLORS.background))
            .child(self.render_header())
            .child(content)
            .child(self.render_footer(cx))
    }
}

fn preview_configuration(
    preview: PreviewState,
) -> (InstallerOperation, SurfaceState, Option<String>) {
    match preview {
        PreviewState::Ready | PreviewState::Upgrade => {
            (InstallerOperation::Install, SurfaceState::Ready, None)
        }
        PreviewState::Installing => (InstallerOperation::Install, SurfaceState::Installing, None),
        PreviewState::Complete => (InstallerOperation::Install, SurfaceState::Complete, None),
        PreviewState::Error => (
            InstallerOperation::Install,
            SurfaceState::Error(
                "The package could not be applied. No application files were changed.".into(),
            ),
            None,
        ),
        PreviewState::Remove => (InstallerOperation::Remove, SurfaceState::Ready, None),
    }
}

fn status_row(ok: bool, label: &str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(rgb(if ok {
            COLORS.accent
        } else {
            COLORS.error
        })))
        .child(
            div()
                .max_w(px(460.0))
                .line_height(px(19.0))
                .text_size(px(13.0))
                .text_color(rgb(if ok { COLORS.foreground } else { COLORS.error }))
                .child(label.to_owned()),
        )
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
            format!("could not determine the Windows version (NTSTATUS {status:?})").into(),
        );
    }
    validate_windows_version(
        version.dwMajorVersion,
        version.dwMinorVersion,
        version.dwBuildNumber,
    )
}

fn validate_windows_version(major: u32, minor: u32, build: u32) -> Result<()> {
    if major > 10 || (major == 10 && build >= 19_041) {
        Ok(())
    } else {
        Err(format!(
            "Compi requires Windows 10 version 2004 (build 19041) or newer; this system reports {major}.{minor}.{build}"
        )
        .into())
    }
}

fn application_data_directory() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Compi")
}

fn installed_directory() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Programs")
        .join("Compi")
}

fn installed_executable() -> PathBuf {
    installed_directory().join("compi.exe")
}

fn installer_log_path() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Compi")
        .join("installer.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_windows_versions_before_windows_10_2004() {
        assert!(validate_windows_version(10, 0, 19_041).is_ok());
        assert!(validate_windows_version(10, 0, 18_363).is_err());
    }
}
