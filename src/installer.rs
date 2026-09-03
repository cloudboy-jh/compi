use crate::theme::{ACCENT, BACKGROUND, BORDER, ERROR, FOREGROUND, MUTED, SURFACE};
use crate::{Error, Result};
use gpui::{
    App, Application, Bounds, Context, FocusHandle, Focusable, IntoElement, ParentElement,
    PathBuilder, Render, Styled, Window, WindowBounds, WindowControlArea, WindowOptions, actions,
    canvas, div, point, prelude::*, px, rgb, size,
};
use std::env;
use std::fs;
use std::mem::size_of;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

const WINDOW_WIDTH: f32 = 600.0;
const WINDOW_HEIGHT: f32 = 460.0;

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
enum InstallerSource {
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
}

struct InstallerApp {
    launch: LaunchMode,
    operation: InstallerOperation,
    state: SurfaceState,
    installed: bool,
    prerequisite_error: Option<String>,
    event_tx: async_channel::Sender<std::result::Result<(), String>>,
    busy: Arc<AtomicBool>,
    focus_handle: FocusHandle,
}

actions!(compi_installer, [PrimaryAction, CloseInstaller]);

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

fn run_mode(launch: LaunchMode) {
    Application::new().run(move |cx: &mut App| {
        cx.bind_keys([
            gpui::KeyBinding::new("enter", PrimaryAction, Some("CompiInstaller")),
            gpui::KeyBinding::new("escape", CloseInstaller, Some("CompiInstaller")),
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
        let (event_tx, event_rx) = async_channel::bounded(1);
        let installed = installed_executable().is_file();
        let (operation, state, prerequisite_error) = match &launch {
            LaunchMode::Live { operation, .. } => (
                *operation,
                SurfaceState::Ready,
                (*operation != InstallerOperation::Remove)
                    .then(|| {
                        ensure_supported_windows()
                            .and_then(|_| crate::wsl::ensure_default_wsl2())
                            .err()
                            .map(|error| error.to_string())
                    })
                    .flatten(),
            ),
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
            focus_handle: cx.focus_handle(),
        };
        if matches!(this.launch, LaunchMode::Preview(PreviewState::Installing)) {
            this.busy.store(true, Ordering::Release);
        }
        if matches!(this.launch, LaunchMode::Preview(PreviewState::Upgrade)) {
            this.installed = true;
        }
        cx.spawn(async move |weak, cx| {
            if let Ok(result) = event_rx.recv().await {
                let _ = weak.update(cx, |this, cx| {
                    this.busy.store(false, Ordering::Release);
                    this.state = match result {
                        Ok(()) => SurfaceState::Complete,
                        Err(error) => SurfaceState::Error(error),
                    };
                    cx.notify();
                });
            }
        })
        .detach();
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
            SurfaceState::Ready => self.start_installation(cx),
            SurfaceState::Installing => {}
            SurfaceState::Complete => {
                if self.operation == InstallerOperation::Remove {
                    window.remove_window();
                } else {
                    let _ = Command::new(installed_executable()).spawn();
                    window.remove_window();
                }
            }
            SurfaceState::Error(_) => {
                self.state = SurfaceState::Ready;
                cx.notify();
            }
        }
    }

    fn close_installer(&mut self, _: &CloseInstaller, window: &mut Window, _: &mut Context<Self>) {
        if !self.busy.load(Ordering::Acquire) {
            window.remove_window();
        }
    }

    fn start_installation(&mut self, cx: &mut Context<Self>) {
        if self.prerequisite_error.is_some() || self.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let LaunchMode::Live { source, operation } = self.launch.clone() else {
            return;
        };
        self.state = SurfaceState::Installing;
        cx.notify();
        let sender = self.event_tx.clone();
        thread::spawn(move || {
            let result = perform_operation(source, operation).map_err(|error| error.to_string());
            let _ = sender.send_blocking(result);
        });
    }

    fn action_label(&self) -> &'static str {
        match (&self.state, self.operation, self.installed) {
            (SurfaceState::Ready, InstallerOperation::Install, true) => "Update Compi",
            (SurfaceState::Ready, InstallerOperation::Install, false) => "Install Compi",
            (SurfaceState::Ready, InstallerOperation::Repair, _) => "Repair Compi",
            (SurfaceState::Ready, InstallerOperation::Remove, _) => "Remove Compi",
            (SurfaceState::Installing, _, _) => "Working…",
            (SurfaceState::Complete, InstallerOperation::Remove, _) => "Close",
            (SurfaceState::Complete, _, _) => "Open Compi",
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
            .bg(rgb(SURFACE))
            .border_b_1()
            .border_color(rgb(BORDER))
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
                        .text_color(rgb(MUTED))
                        .window_control_area(WindowControlArea::Close)
                        .hover(|style| {
                            style
                                .bg(rgb(0x4a1f22))
                                .text_color(rgb(FOREGROUND))
                                .cursor_pointer()
                        })
                        .on_click(|_, window, _| window.remove_window())
                        .child("×"),
                )
            })
    }

    fn render_ready(&self) -> impl IntoElement {
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
                "Remove Compi and its background task. Active terminal sessions will end; project files are never touched."
            }
        };
        let destination = installed_directory().display().to_string();
        let (first_check, second_check) = if self.operation == InstallerOperation::Remove {
            (
                "Application files and Start menu shortcut",
                "Background daemon task",
            )
        } else {
            (
                self.prerequisite_error
                    .as_deref()
                    .unwrap_or("WSL2 default distribution ready"),
                "Installs without administrator access",
            )
        };
        div()
            .flex_1()
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
                    .text_color(rgb(MUTED))
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
                                    .text_color(rgb(MUTED))
                                    .child(destination),
                            ),
                    ),
            )
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
                    .border_color(rgb(ACCENT))
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
                    .child(match self.operation {
                        InstallerOperation::Install => "Installing Compi",
                        InstallerOperation::Repair => "Repairing Compi",
                        InstallerOperation::Remove => "Removing Compi",
                    }),
            )
            .child(
                div()
                    .mt_2()
                    .text_size(px(13.0))
                    .text_color(rgb(MUTED))
                    .child("Windows Installer is applying files and session registration."),
            )
    }

    fn render_complete(&self) -> impl IntoElement {
        let (title, detail) = if self.operation == InstallerOperation::Remove {
            (
                "Compi removed",
                "Application files and daemon registration were removed.",
            )
        } else {
            (
                "Compi is ready",
                "Open a terminal now or find Compi in the Start menu.",
            )
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
                    .bg(rgb(ACCENT))
                    .text_color(rgb(BACKGROUND))
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
                    .text_color(rgb(MUTED))
                    .child(detail),
            )
    }

    fn render_error(&self, error: &str) -> impl IntoElement {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .px(px(38.0))
            .pt(px(46.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(ERROR))
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
                    .text_color(rgb(MUTED))
                    .child(error.to_owned()),
            )
            .child(
                div()
                    .mt_5()
                    .text_size(px(12.0))
                    .text_color(rgb(MUTED))
                    .child(format!("Detailed log: {}", installer_log_path().display())),
            )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled =
            !matches!(self.state, SurfaceState::Installing) && self.prerequisite_error.is_none();
        let label = self.action_label();
        div()
            .h(px(76.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px(px(38.0))
            .border_t_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child("Project files are never modified"),
            )
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
                    .bg(if enabled { rgb(ACCENT) } else { rgb(BORDER) })
                    .text_color(if enabled { rgb(BACKGROUND) } else { rgb(MUTED) })
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
            SurfaceState::Ready => self.render_ready().into_any_element(),
            SurfaceState::Installing => self.render_installing().into_any_element(),
            SurfaceState::Complete => self.render_complete().into_any_element(),
            SurfaceState::Error(error) => self.render_error(error).into_any_element(),
        };
        div()
            .key_context("CompiInstaller")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::primary_action))
            .on_action(cx.listener(Self::close_installer))
            .size_full()
            .flex()
            .flex_col()
            .font_family("Segoe UI Variable")
            .text_size(px(13.0))
            .text_color(rgb(FOREGROUND))
            .bg(rgb(BACKGROUND))
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
        .child(
            div()
                .w(px(8.0))
                .h(px(8.0))
                .rounded_full()
                .bg(rgb(if ok { ACCENT } else { ERROR })),
        )
        .child(
            div()
                .text_size(px(13.0))
                .text_color(rgb(if ok { FOREGROUND } else { ERROR }))
                .child(label.to_owned()),
        )
}

fn brand_mark() -> impl IntoElement {
    canvas(
        move |_, _, _| (),
        move |bounds, _, window, _| {
            let x = |value: f32| bounds.left() + px(value);
            let y = |value: f32| bounds.top() + px(value);
            let mut path = PathBuilder::stroke(px(1.4));
            path.move_to(point(x(2.0), y(8.0)));
            path.line_to(point(x(8.0), y(3.0)));
            path.line_to(point(x(14.0), y(8.0)));
            path.line_to(point(x(8.0), y(13.0)));
            path.line_to(point(x(2.0), y(8.0)));
            path.move_to(point(x(6.0), y(11.0)));
            path.line_to(point(x(10.0), y(5.0)));
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(ACCENT));
            }
        },
    )
    .size(px(16.0))
}

fn perform_operation(source: InstallerSource, operation: InstallerOperation) -> Result<()> {
    let package_source = matches!(&source, InstallerSource::Package(_));
    let mut backup_path = None;
    let package_path = match source {
        InstallerSource::Package(msi) => {
            if msi.len() < 4 || &msi[..4] != b"\xd0\xcf\x11\xe0" {
                return Err("embedded Windows Installer payload is invalid".into());
            }
            let directory = application_data_directory().join("installer");
            fs::create_dir_all(&directory)?;
            let path = directory.join("Compi.msi");
            if path.is_file() {
                let previous = directory.join("Compi.previous.msi");
                let _ = fs::remove_file(&previous);
                fs::rename(&path, &previous)?;
                backup_path = Some(previous);
            }
            if let Err(error) = fs::write(&path, msi) {
                restore_previous_package(&path, backup_path.as_deref());
                return Err(error.into());
            }
            path
        }
        InstallerSource::ProductCode(product_code) => {
            if operation == InstallerOperation::Install {
                return Err("a package is required to install Compi".into());
            }
            PathBuf::from(product_code)
        }
    };
    let log_path = installer_log_path();
    let executable = env::var_os("SystemRoot")
        .map(PathBuf::from)
        .ok_or("SystemRoot is not set")?
        .join("System32")
        .join("msiexec.exe");
    let package_removal = package_source && operation == InstallerOperation::Remove;
    let operation_flag = match operation {
        InstallerOperation::Install => "/i",
        InstallerOperation::Repair => "/fa",
        InstallerOperation::Remove if package_removal => "/i",
        InstallerOperation::Remove => "/x",
    };
    let mut command = Command::new(executable);
    command.arg(operation_flag).arg(&package_path);
    if package_removal {
        command.args(["REMOVE=ALL", "Installed=1"]);
    }
    let status = command
        .args(["/qn", "/norestart", "/L*v"])
        .arg(&log_path)
        .creation_flags(CREATE_NO_WINDOW.0)
        .status();
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            if package_source {
                restore_previous_package(&package_path, backup_path.as_deref());
            }
            return Err(error.into());
        }
    };
    if status.success() || matches!(status.code(), Some(1641 | 3010)) {
        if let Some(previous) = backup_path {
            let _ = fs::remove_file(previous);
        }
        if operation == InstallerOperation::Remove {
            fs::remove_dir_all(application_data_directory())?;
        }
        Ok(())
    } else {
        if package_source {
            restore_previous_package(&package_path, backup_path.as_deref());
        }
        Err(Error::from(format!(
            "Windows Installer exited with {status}. Review {}",
            log_path.display()
        )))
    }
}

fn restore_previous_package(package_path: &std::path::Path, backup_path: Option<&std::path::Path>) {
    let _ = fs::remove_file(package_path);
    if let Some(previous) = backup_path {
        let _ = fs::rename(previous, package_path);
    }
}

fn ensure_supported_windows() -> Result<()> {
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
    fn preview_states_are_side_effect_free() {
        for state in [
            PreviewState::Ready,
            PreviewState::Upgrade,
            PreviewState::Installing,
            PreviewState::Complete,
            PreviewState::Error,
            PreviewState::Remove,
        ] {
            let (_, surface, _) = preview_configuration(state);
            assert!(matches!(
                surface,
                SurfaceState::Ready
                    | SurfaceState::Installing
                    | SurfaceState::Complete
                    | SurfaceState::Error(_)
            ));
        }
    }

    #[test]
    fn rejects_non_msi_payload_before_running_installer() {
        let error = perform_operation(
            InstallerSource::Package(b"not an msi"),
            InstallerOperation::Install,
        )
        .unwrap_err();
        assert!(error.to_string().contains("payload is invalid"));
    }

    #[test]
    fn recognizes_the_current_supported_windows_version() {
        ensure_supported_windows().unwrap();
    }

    #[test]
    fn rejects_windows_versions_before_windows_10_2004() {
        assert!(validate_windows_version(10, 0, 19_041).is_ok());
        let error = validate_windows_version(10, 0, 18_363).unwrap_err();
        assert!(error.to_string().contains("build 19041"));
    }
}
