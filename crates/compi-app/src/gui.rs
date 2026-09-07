use crate::config::{FontSettings, LoadedConfig};
use crate::typography::TerminalTypography;
use base64::Engine as _;
#[cfg(windows)]
use compi_client_core::input::is_application_shortcut;
use compi_client_core::input::{
    self, Key, KeypadKey, Modifiers, encode_keystroke, encode_mouse, utf16_byte_index,
};
use compi_client_core::selection::{
    CtrlCBehavior, GridPoint, Selection, ctrl_c_behavior, line_selection, selected_text,
    word_selection,
};
use compi_client_core::theme::{
    ACCENT, BACKGROUND, BORDER, ERROR, FOREGROUND, MUTED, SELECTION, SURFACE, SURFACE_HOVER,
};
use compi_client_core::viewport::{
    hyperlink_at, inherited_working_directory, is_allowed_hyperlink, visible_row, visible_rows,
    visible_to_absolute,
};
use compi_client_core::{MirrorApply, ScreenMirror};
use compi_protocol::{
    Cell, ClientMessage, Color, CursorShape, CursorState, KittyImage, KittyPlacement, MouseMode,
    Row, ScreenMessage, ScreenSnapshot, ServerMessage, SurfaceId, SurfaceInfo, SurfaceStatus,
};
use compi_server::client::{DaemonClient, ServerEvent};
use compi_server::{perf, probe};
use gpui::{
    App, Application, Bounds, ClipboardItem, ContentMask, Context, Corners, ElementInputHandler,
    EntityInputHandler, FocusHandle, Focusable, FontId, FontStyle, FontWeight, GlyphId, Hsla,
    KeyBinding, KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathBuilder, Pixels, Point, Render, RenderImage, ScrollHandle, ScrollWheelEvent, SharedString,
    Subscription, TextRun, TitlebarOptions, UTF16Selection, UnderlineStyle, Window, WindowBounds,
    WindowControlArea, WindowOptions, actions, canvas, div, fill, point, prelude::*, px, rgb, size,
};
use image::{Frame as ImageFrame, RgbaImage};
#[cfg(target_os = "macos")]
use objc2::{class, msg_send};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use smallvec::smallvec;
use std::collections::{HashMap, HashSet, VecDeque, hash_map::DefaultHasher};
use std::env;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::Write as _;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, TryRecvError};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant};
#[cfg(windows)]
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
#[cfg(windows)]
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
#[cfg(windows)]
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
#[cfg(windows)]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, VK_ADD, VK_DECIMAL, VK_DIVIDE, VK_MULTIPLY, VK_NUMPAD0,
    VK_NUMPAD1, VK_NUMPAD2, VK_NUMPAD3, VK_NUMPAD4, VK_NUMPAD5, VK_NUMPAD6, VK_NUMPAD7, VK_NUMPAD8,
    VK_NUMPAD9, VK_SUBTRACT,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    HTCAPTION, PostMessageW, SW_RESTORE, ShowWindowAsync, WM_NCLBUTTONDOWN,
};

const DEFAULT_COLS: i16 = 100;
const DEFAULT_ROWS: i16 = 30;
const CHROME_HEIGHT: f32 = 40.0;
const TAB_WIDTH: f32 = 176.0;
#[cfg(windows)]
const WINDOW_CONTROLS_WIDTH: f32 = 138.0;
#[cfg(target_os = "macos")]
const WINDOW_CONTROLS_WIDTH: f32 = 0.0;
#[cfg(windows)]
const TITLEBAR_BRAND_WIDTH: f32 = 40.0;
#[cfg(target_os = "macos")]
const TITLEBAR_BRAND_WIDTH: f32 = 118.0;
#[cfg(windows)]
const UI_FONT: &str = "Segoe UI";
#[cfg(target_os = "macos")]
const UI_FONT: &str = ".SystemUIFont";
const NEW_TAB_WIDTH: f32 = 40.0;
const SESSION_SWITCHER_WIDTH: f32 = 40.0;
const TITLEBAR_DRAG_WIDTH: f32 = 16.0;
const TITLEBAR_ACTIONS_WIDTH: f32 = NEW_TAB_WIDTH + SESSION_SWITCHER_WIDTH + TITLEBAR_DRAG_WIDTH;
const TAB_DRAG_THRESHOLD: f32 = 4.0;
const TERMINAL_PADDING: f32 = 8.0;
const RECONNECT_DELAY: Duration = Duration::from_millis(350);
const UI_EVENT_BUDGET: Duration = Duration::from_millis(1);
const UI_EVENT_YIELD: Duration = Duration::from_micros(8_333);

pub fn run(
    instance: Option<String>,
    initial_working_directory: Option<String>,
    config: LoadedConfig,
) {
    let started_at = Instant::now();
    let application = Application::new();
    log_startup_metric("application_created_ms", started_at.elapsed());
    application.run(move |cx: &mut App| {
        #[cfg(windows)]
        cx.bind_keys([
            KeyBinding::new("ctrl-t", NewTab, Some("Terminal")),
            KeyBinding::new("ctrl-w", CloseTab, Some("Terminal")),
            KeyBinding::new("ctrl-tab", NextTab, Some("Terminal")),
            KeyBinding::new("ctrl-shift-tab", PreviousTab, Some("Terminal")),
            KeyBinding::new("ctrl-c", CopyOrInterrupt, Some("Terminal")),
            KeyBinding::new("ctrl-shift-c", CopySelection, Some("Terminal")),
            KeyBinding::new("ctrl-v", PasteClipboard, Some("Terminal")),
            KeyBinding::new("ctrl-shift-v", PasteClipboard, Some("Terminal")),
            KeyBinding::new("ctrl-shift-p", ToggleSessionSwitcher, Some("Terminal")),
        ]);
        #[cfg(target_os = "macos")]
        {
            cx.bind_keys([
                KeyBinding::new("cmd-t", NewTab, Some("Terminal")),
                KeyBinding::new("cmd-w", CloseTab, Some("Terminal")),
                KeyBinding::new("ctrl-tab", NextTab, Some("Terminal")),
                KeyBinding::new("ctrl-shift-tab", PreviousTab, Some("Terminal")),
                KeyBinding::new("cmd-shift-]", NextTab, Some("Terminal")),
                KeyBinding::new("cmd-shift-[", PreviousTab, Some("Terminal")),
                KeyBinding::new("cmd-c", CopySelection, Some("Terminal")),
                KeyBinding::new("cmd-v", PasteClipboard, Some("Terminal")),
                KeyBinding::new("cmd-shift-p", ToggleSessionSwitcher, Some("Terminal")),
                KeyBinding::new("cmd-q", Quit, None),
            ]);
            cx.on_action(|_: &Quit, cx| cx.quit());
        }

        let bounds = Bounds::centered(None, size(px(960.0), px(640.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Compi".into()),
                        appears_transparent: true,
                        #[cfg(target_os = "macos")]
                        traffic_light_position: Some(point(px(12.0), px(13.0))),
                        #[cfg(windows)]
                        traffic_light_position: None,
                    }),
                    focus: true,
                    ..Default::default()
                },
                move |window, cx| {
                    cx.new(|cx| {
                        CompiApp::new(
                            started_at,
                            instance,
                            initial_working_directory,
                            config,
                            window,
                            cx,
                        )
                    })
                },
            )
            .expect("failed to open Compi window");
        log_startup_metric("window_opened_ms", started_at.elapsed());

        window
            .update(cx, |view, window, cx| {
                window.set_window_title("Compi");
                window.focus(&view.focus_handle);
                cx.activate(true);
            })
            .expect("failed to activate Compi window");

        cx.on_window_closed(|cx| cx.quit()).detach();
    });
}

actions!(
    compi,
    [
        NewTab,
        CloseTab,
        NextTab,
        PreviousTab,
        CopyOrInterrupt,
        CopySelection,
        PasteClipboard,
        ToggleSessionSwitcher,
        Quit,
    ]
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectionState {
    Connecting,
    Attached,
    Reconnecting,
    Exited(u32),
    Failed,
}

struct TabTransport {
    commands: Sender<ClientMessage>,
    stop: Arc<AtomicBool>,
}

impl TabTransport {
    fn send(&self, message: ClientMessage) -> crate::Result<()> {
        let latency_id = match &message {
            ClientMessage::Input { latency_id, .. } => *latency_id,
            _ => None,
        };
        self.commands
            .send(message)
            .map_err(|_| "terminal connection writer stopped")?;
        if let Some(latency_id) = latency_id {
            perf::log_input_latency_stage(latency_id, "client_queued", None);
        }
        Ok(())
    }

    fn close(&self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.send(ClientMessage::Detach);
    }
}

struct TerminalTab {
    id: u64,
    surface_id: SurfaceId,
    mirror: ScreenMirror,
    state: ConnectionState,
    error: Option<String>,
    transport: Option<TabTransport>,
    scroll_offset: usize,
    selection: Option<Selection>,
    selecting: bool,
    image_cache: HashMap<u32, (String, Arc<RenderImage>)>,
    image_pending: HashMap<u32, String>,
    row_render_cache: Arc<Mutex<RowRenderCache>>,
    cols: i16,
    rows: i16,
}

impl TerminalTab {
    fn title(&self) -> String {
        self.mirror
            .snapshot()
            .map(|snapshot| snapshot.title.trim())
            .filter(|title| !title.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| short_surface_id(&self.surface_id))
    }

    fn send(&mut self, message: ClientMessage) {
        let Some(transport) = self.transport.as_ref() else {
            return;
        };
        if let Err(error) = transport.send(message) {
            self.error = Some(error.to_string());
            self.state = ConnectionState::Reconnecting;
        }
    }

    fn refresh_images(&mut self, tab_id: u64, sender: UiEventSender) {
        let Some(snapshot) = self.mirror.snapshot() else {
            self.image_cache.clear();
            self.image_pending.clear();
            return;
        };
        let images = snapshot.images.clone();
        let active_ids: Vec<u32> = images.iter().map(|image| image.id).collect();
        self.image_cache
            .retain(|image_id, _| active_ids.contains(image_id));
        self.image_pending
            .retain(|image_id, _| active_ids.contains(image_id));
        for image in images {
            let cached = self
                .image_cache
                .get(&image.id)
                .is_some_and(|(data, _)| data == &image.data);
            let pending = self
                .image_pending
                .get(&image.id)
                .is_some_and(|data| data == &image.data);
            if cached || pending {
                continue;
            }
            self.image_pending.insert(image.id, image.data.clone());
            let data = image.data.clone();
            thread::spawn({
                let sender = sender.clone();
                move || {
                    let result = decode_kitty_image(&image);
                    let _ = sender.send(UiEvent::KittyImageDecoded {
                        tab_id,
                        image_id: image.id,
                        data,
                        result,
                    });
                }
            });
        }
    }

    fn max_scroll_offset(&self) -> usize {
        self.mirror
            .snapshot()
            .map(|snapshot| snapshot.scrollback.len())
            .unwrap_or(0)
    }
}

enum UiEvent {
    SurfacesLoaded(Result<compi_protocol::WorkspaceSnapshot, String>),
    SurfaceCreated(Result<SurfaceInfo, String>),
    SurfaceEndFinished {
        surface_id: SurfaceId,
        result: Result<Vec<SurfaceInfo>, String>,
    },
    TabConnected {
        tab_id: u64,
        transport: TabTransport,
    },
    TabScreen {
        tab_id: u64,
        message: ScreenMessage,
    },
    KittyImageDecoded {
        tab_id: u64,
        image_id: u32,
        data: String,
        result: Result<Arc<RenderImage>, String>,
    },
    TabControl {
        tab_id: u64,
        message: ServerMessage,
    },
    TabDisconnected {
        tab_id: u64,
        error: String,
    },
}
#[derive(Clone)]
struct UiEventSender(async_channel::Sender<UiEvent>);

impl UiEventSender {
    fn send(&self, event: UiEvent) -> bool {
        self.0.send_blocking(event).is_ok()
    }
}

struct CompiApp {
    started_at: Instant,
    instance: Option<String>,
    initial_working_directory: Option<String>,
    empty_window: bool,
    perf_target_sessions: usize,
    first_snapshot_logged: bool,
    ready_probe_marker: Option<String>,
    ready_probe_sent_at: Option<Instant>,
    ready_probe_render_pending: bool,
    ready_probe_logged: bool,
    pending_present_latency_ids: Vec<u64>,
    window_title: String,
    focus_handle: FocusHandle,
    ime_text: String,
    ime_marked_range: Option<Range<usize>>,
    ime_selected_range: Range<usize>,
    tabs: Vec<TerminalTab>,
    active_tab: Option<u64>,
    tab_scroll_handle: ScrollHandle,
    tab_drag_origin: Option<Point<Pixels>>,
    next_tab_id: u64,
    surfaces: Vec<SurfaceInfo>,
    workspace_initialized: bool,
    switcher_open: bool,
    end_confirmation: Option<SurfaceId>,
    ending_surfaces: HashSet<SurfaceId>,
    close_after_end: HashSet<SurfaceId>,
    loading_surfaces: bool,
    attach_after_surface_list: bool,
    global_error: Option<String>,
    font_settings: FontSettings,
    typography: Arc<TerminalTypography>,
    typography_scale: f32,
    config_diagnostics: Vec<String>,
    global_warning: Option<String>,
    event_tx: UiEventSender,
    subscriptions: Vec<Subscription>,
    terminal_cols: i16,
    terminal_rows: i16,
}

impl CompiApp {
    fn new(
        started_at: Instant,
        instance: Option<String>,
        initial_working_directory: Option<String>,
        config: LoadedConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (event_tx, event_rx) = async_channel::unbounded();
        let empty_window = perf::empty_window_enabled();
        let ready_probe_marker =
            perf::ready_probe_enabled().then(|| format!("COMPI_READY_{}", std::process::id()));
        let perf_target_sessions = perf::target_session_count();
        let event_tx = UiEventSender(event_tx);
        let typography = Arc::new(TerminalTypography::resolve(&config.font, 1.0, window));
        let typography_scale = window.scale_factor();
        let global_warning = diagnostic_warning(&config.diagnostics, &typography.diagnostics);
        let mut this = Self {
            started_at,
            instance,
            initial_working_directory,
            empty_window,
            perf_target_sessions,
            first_snapshot_logged: false,
            ready_probe_marker,
            ready_probe_sent_at: None,
            ready_probe_render_pending: false,
            ready_probe_logged: false,
            pending_present_latency_ids: Vec::new(),
            window_title: String::from("Compi"),
            focus_handle: cx.focus_handle(),
            ime_text: String::new(),
            ime_marked_range: None,
            ime_selected_range: 0..0,
            tabs: Vec::new(),
            active_tab: None,
            tab_scroll_handle: ScrollHandle::new(),
            tab_drag_origin: None,
            next_tab_id: 1,
            surfaces: Vec::new(),
            workspace_initialized: false,
            switcher_open: false,
            end_confirmation: None,
            ending_surfaces: HashSet::new(),
            close_after_end: HashSet::new(),
            loading_surfaces: !empty_window,
            attach_after_surface_list: false,
            global_error: None,
            font_settings: config.font,
            typography,
            typography_scale,
            config_diagnostics: config.diagnostics,
            global_warning,
            event_tx,
            subscriptions: Vec::new(),
            terminal_cols: DEFAULT_COLS,
            terminal_rows: DEFAULT_ROWS,
        };

        this.update_dimensions(window);
        if !empty_window {
            this.refresh_surfaces(true);
        }
        this.subscriptions
            .push(cx.observe_window_bounds(window, |this, window, cx| {
                this.update_dimensions(window);
                cx.notify();
            }));
        this.subscriptions
            .push(cx.observe_window_activation(window, |this, window, cx| {
                this.report_focus(window.is_window_active());
                cx.notify();
            }));
        let first_frame_started_at = started_at;
        window.on_next_frame(move |_, _| {
            log_startup_metric("first_window_frame_ms", first_frame_started_at.elapsed());
        });
        if perf::enabled() {
            cx.spawn(async move |weak, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(6)).await;
                    if weak
                        .update(cx, |this, _| {
                            let workload = if this.empty_window {
                                "empty_window"
                            } else {
                                "terminal"
                            };
                            perf::log_resource_sample("client", workload, this.tabs.len());
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
        }

        cx.spawn(async move |weak, cx| {
            while let Ok(first) = event_rx.recv().await {
                let batch_started_at = Instant::now();
                if weak
                    .update(cx, |this, cx| {
                        this.handle_event(first, cx);
                        while batch_started_at.elapsed() < UI_EVENT_BUDGET {
                            let Ok(event) = event_rx.try_recv() else {
                                break;
                            };
                            this.handle_event(event, cx);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(UI_EVENT_YIELD).await;
            }
        })
        .detach();

        this
    }

    fn refresh_surfaces(&mut self, attach_initial: bool) {
        self.loading_surfaces = true;
        self.attach_after_surface_list |= attach_initial;
        let sender = self.event_tx.clone();
        let instance = self.instance.clone();
        thread::spawn(move || {
            let result = probe::connect_or_start(instance.as_deref())
                .and_then(|mut client| client.workspace())
                .map_err(|error| error.to_string());
            let _ = sender.send(UiEvent::SurfacesLoaded(result));
        });
    }

    fn create_surface(&mut self, working_directory: Option<String>) {
        let sender = self.event_tx.clone();
        let cols = self.terminal_cols;
        let rows = self.terminal_rows;
        let instance = self.instance.clone();
        thread::spawn(move || {
            let result = probe::connect_or_start(instance.as_deref())
                .and_then(|mut client| client.create_surface(cols, rows, working_directory))
                .map_err(|error| error.to_string());
            let _ = sender.send(UiEvent::SurfaceCreated(result));
        });
    }

    fn create_inherited_session(&mut self) {
        let working_directory =
            inherited_working_directory(self.active_tab().and_then(|tab| tab.mirror.snapshot()));
        self.create_surface(working_directory);
    }

    fn create_next_perf_session(&mut self) {
        if self.tabs.len() < self.perf_target_sessions {
            self.create_surface(None);
        }
    }

    fn begin_end_surface(&mut self, surface_id: SurfaceId) {
        if !self.ending_surfaces.contains(&surface_id)
            && self
                .surfaces
                .iter()
                .any(|surface| surface.id == surface_id && surface.status == SurfaceStatus::Running)
        {
            self.end_confirmation = Some(surface_id);
        }
    }

    fn cancel_end_surface(&mut self, surface_id: &SurfaceId) {
        if self.end_confirmation.as_ref() == Some(surface_id) {
            self.end_confirmation = None;
        }
    }

    fn confirm_end_surface(&mut self, surface_id: SurfaceId) {
        if self.ending_surfaces.contains(&surface_id)
            || !self
                .surfaces
                .iter()
                .any(|surface| surface.id == surface_id && surface.status == SurfaceStatus::Running)
        {
            return;
        }
        self.end_confirmation = None;
        self.ending_surfaces.insert(surface_id.clone());
        if self.tabs.iter().any(|tab| tab.surface_id == surface_id) {
            self.close_after_end.insert(surface_id.clone());
        }
        let sender = self.event_tx.clone();
        let instance = self.instance.clone();
        thread::spawn(move || {
            let result = terminate_surface_and_wait(instance.as_deref(), &surface_id);
            let _ = sender.send(UiEvent::SurfaceEndFinished { surface_id, result });
        });
    }

    fn attach_surface(&mut self, surface: SurfaceInfo) {
        if let Some(tab_id) = self
            .tabs
            .iter()
            .find(|tab| tab.surface_id == surface.id)
            .map(|tab| tab.id)
        {
            self.active_tab = Some(tab_id);
            self.reveal_tab(tab_id);
            self.switcher_open = false;
            return;
        }
        if surface.status != SurfaceStatus::Running || surface.attached {
            return;
        }
        let tab_id = self.next_tab_id;
        self.next_tab_id = self.next_tab_id.saturating_add(1);
        self.tabs.push(TerminalTab {
            id: tab_id,
            surface_id: surface.id.clone(),
            mirror: ScreenMirror::default(),
            state: ConnectionState::Connecting,
            error: None,
            transport: None,
            scroll_offset: 0,
            selection: None,
            selecting: false,
            image_cache: HashMap::new(),
            image_pending: HashMap::new(),
            cols: self.terminal_cols,
            row_render_cache: Arc::new(Mutex::new(RowRenderCache::default())),
            rows: self.terminal_rows,
        });
        self.active_tab = Some(tab_id);
        self.reveal_tab(tab_id);
        self.switcher_open = false;
        spawn_tab_worker(
            tab_id,
            surface.id,
            self.terminal_cols,
            self.terminal_rows,
            self.event_tx.clone(),
            self.instance.clone(),
        );
    }

    fn handle_event(&mut self, event: UiEvent, cx: &mut Context<Self>) {
        match event {
            UiEvent::SurfacesLoaded(Ok(workspace)) => {
                self.global_error = None;
                self.loading_surfaces = false;
                let needs_initial_tab = self.attach_after_surface_list && self.tabs.is_empty();
                self.attach_after_surface_list = false;
                self.workspace_initialized = workspace.initialized;
                self.surfaces = workspace.surfaces;
                if needs_initial_tab {
                    if let Some(working_directory) = self.initial_working_directory.take() {
                        self.create_surface(Some(working_directory));
                    } else if let Some(surface) = self
                        .surfaces
                        .iter()
                        .find(|surface| {
                            surface.status == SurfaceStatus::Running && !surface.attached
                        })
                        .cloned()
                    {
                        self.attach_surface(surface);
                        self.create_next_perf_session();
                    } else if !self.workspace_initialized {
                        self.create_surface(None);
                    }
                }
            }
            UiEvent::SurfacesLoaded(Err(error)) => {
                self.loading_surfaces = false;
                self.global_error = Some(error);
            }
            UiEvent::SurfaceCreated(Ok(surface)) => {
                self.global_error = None;
                self.workspace_initialized = true;
                self.surfaces.push(surface.clone());
                self.attach_surface(surface);
                self.create_next_perf_session();
            }
            UiEvent::SurfaceCreated(Err(error)) => self.global_error = Some(error),
            UiEvent::SurfaceEndFinished { surface_id, result } => {
                self.ending_surfaces.remove(&surface_id);
                match result {
                    Ok(surfaces) => {
                        self.global_error = None;
                        self.surfaces = surfaces;
                        if !self.tabs.iter().any(|tab| tab.surface_id == surface_id) {
                            self.close_after_end.remove(&surface_id);
                        }
                    }
                    Err(error) => {
                        self.close_after_end.remove(&surface_id);
                        self.global_error = Some(error);
                    }
                }
            }
            UiEvent::TabConnected { tab_id, transport } => {
                let (cols, rows) = (self.terminal_cols, self.terminal_rows);
                let ready_probe = self
                    .ready_probe_marker
                    .clone()
                    .filter(|_| self.ready_probe_sent_at.is_none());
                if ready_probe.is_some() {
                    self.ready_probe_sent_at = Some(Instant::now());
                }
                if let Some(tab) = self.tab_mut(tab_id) {
                    tab.transport = Some(transport);
                    tab.state = ConnectionState::Attached;
                    tab.error = None;
                    tab.send(ClientMessage::Resize { cols, rows });
                    if let Some(marker) = ready_probe {
                        tab.send(ClientMessage::Input {
                            data: format!("printf '%s\\n' '{marker}'\n").into_bytes(),
                            latency_id: None,
                        });
                    }
                }
            }
            UiEvent::TabScreen { tab_id, message } => {
                let clipboard_write = match &message {
                    ScreenMessage::Delta { delta } => delta.clipboard_writes.last().cloned(),
                    ScreenMessage::Snapshot { .. } => None,
                };
                let latency_ids = match &message {
                    ScreenMessage::Delta { delta } => delta.latency_ids.clone(),
                    ScreenMessage::Snapshot { .. } => Vec::new(),
                };
                let mut request_snapshot = false;
                let mut ready_probe_observed = false;
                let sender = self.event_tx.clone();
                let ready_probe_marker = self.ready_probe_marker.clone();
                if let Some(tab) = self.tab_mut(tab_id) {
                    request_snapshot = matches!(tab.mirror.apply(message), MirrorApply::Gap { .. });
                    if !request_snapshot {
                        tab.state = ConnectionState::Attached;
                        tab.refresh_images(tab_id, sender);
                        ready_probe_observed =
                            ready_probe_marker.as_deref().is_some_and(|marker| {
                                snapshot_contains_marker(tab.mirror.snapshot(), marker)
                            });
                    }
                }
                if !request_snapshot && let Some(text) = clipboard_write {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                if !request_snapshot {
                    self.pending_present_latency_ids.extend(latency_ids);
                }
                if request_snapshot {
                    if let Some(tab) = self.tab_mut(tab_id) {
                        tab.send(ClientMessage::RequestSnapshot);
                    }
                } else if !self.first_snapshot_logged {
                    self.first_snapshot_logged = true;
                    perf::log_startup_metric("first_terminal_frame_ms", self.started_at.elapsed());
                }
                self.ready_probe_render_pending |= ready_probe_observed;
            }
            UiEvent::KittyImageDecoded {
                tab_id,
                image_id,
                data,
                result,
            } => {
                if let Some(tab) = self.tab_mut(tab_id)
                    && tab.image_pending.get(&image_id) == Some(&data)
                {
                    tab.image_pending.remove(&image_id);
                    match result {
                        Ok(image) => {
                            tab.image_cache.insert(image_id, (data, image));
                        }
                        Err(error) => tab.error = Some(format!("Kitty image {image_id}: {error}")),
                    }
                }
            }
            UiEvent::TabControl { tab_id, message } => match message {
                ServerMessage::SurfaceExited {
                    identity,
                    exit_code,
                } => {
                    self.ending_surfaces.remove(&identity.surface_id);
                    if self.close_after_end.remove(&identity.surface_id) {
                        self.close_tab_id(tab_id);
                    } else if let Some(tab) = self.tab_mut(tab_id) {
                        tab.state = ConnectionState::Exited(exit_code);
                        tab.transport = None;
                    }
                    self.refresh_surfaces(false);
                }
                ServerMessage::Error { message, .. } => {
                    if let Some(tab) = self.tab_mut(tab_id) {
                        tab.error = Some(message);
                        tab.state = ConnectionState::Failed;
                    }
                }
                _ => {}
            },
            UiEvent::TabDisconnected { tab_id, error } => {
                if let Some(tab) = self.tab_mut(tab_id) {
                    tab.transport = None;
                    if !matches!(tab.state, ConnectionState::Exited(_)) {
                        tab.state = ConnectionState::Reconnecting;
                        tab.error = Some(error);
                    }
                }
            }
        }
    }

    fn tab_mut(&mut self, tab_id: u64) -> Option<&mut TerminalTab> {
        self.tabs.iter_mut().find(|tab| tab.id == tab_id)
    }

    fn active_tab(&self) -> Option<&TerminalTab> {
        let active = self.active_tab?;
        self.tabs.iter().find(|tab| tab.id == active)
    }

    fn active_tab_mut(&mut self) -> Option<&mut TerminalTab> {
        let active = self.active_tab?;
        self.tabs.iter_mut().find(|tab| tab.id == active)
    }

    fn update_dimensions(&mut self, window: &Window) {
        let viewport = window.viewport_size();
        let cell_width = self.typography.cell_width;
        let cell_height = self.typography.cell_height;
        let width = (f32::from(viewport.width) - TERMINAL_PADDING * 2.0).max(cell_width);
        let height =
            (f32::from(viewport.height) - CHROME_HEIGHT - TERMINAL_PADDING * 2.0).max(cell_height);
        let cols = (width / cell_width).floor().clamp(2.0, i16::MAX as f32) as i16;
        let rows = (height / cell_height).floor().clamp(1.0, i16::MAX as f32) as i16;
        if (cols, rows) == (self.terminal_cols, self.terminal_rows) {
            return;
        }
        self.terminal_cols = cols;
        self.terminal_rows = rows;
        for tab in &mut self.tabs {
            tab.cols = cols;
            tab.rows = rows;
            tab.send(ClientMessage::Resize { cols, rows });
        }
    }

    fn refresh_typography(&mut self, window: &Window) -> bool {
        let scale = window.scale_factor();
        if (scale - self.typography_scale).abs() <= f32::EPSILON {
            return false;
        }
        let typography = Arc::new(TerminalTypography::resolve(
            &self.font_settings,
            1.0,
            window,
        ));
        self.global_warning = diagnostic_warning(&self.config_diagnostics, &typography.diagnostics);
        self.typography = typography;
        self.typography_scale = scale;
        for tab in &self.tabs {
            if let Ok(mut cache) = tab.row_render_cache.lock() {
                cache.clear();
            }
        }
        true
    }

    fn handle_keystroke(&mut self, keystroke: &Keystroke) -> bool {
        if self.switcher_open {
            return true;
        }
        #[cfg(windows)]
        if is_application_shortcut(&terminal_key(keystroke)) {
            return true;
        }
        let Some(tab) = self.active_tab_mut() else {
            return false;
        };
        let (application_cursor, application_keypad) = tab
            .mirror
            .snapshot()
            .map(|snapshot| {
                (
                    snapshot.modes.application_cursor,
                    snapshot.modes.application_keypad,
                )
            })
            .unwrap_or_default();
        let keypad = application_keypad.then(active_keypad_key).flatten();
        #[cfg(target_os = "macos")]
        if keystroke.modifiers.platform
            || (keypad.is_none()
                && keystroke.key_char.is_some()
                && !keystroke.modifiers.control
                && !matches!(keystroke.key.as_str(), "enter" | "tab"))
        {
            // AppKit handles printable text, Option/dead keys and IME
            // composition. Commands must not leak into the PTY.
            return false;
        }
        if let Some(bytes) = encode_keystroke(&terminal_key(keystroke), application_cursor, keypad)
        {
            tab.scroll_offset = 0;
            let latency_id = perf::begin_input_latency();
            if let Some(latency_id) = latency_id {
                perf::log_input_latency_stage(latency_id, "key_receipt", None);
            }
            tab.send(ClientMessage::Input {
                data: bytes,
                latency_id,
            });
            return true;
        }
        false
    }

    fn new_tab(&mut self, _: &NewTab, _: &mut Window, cx: &mut Context<Self>) {
        self.create_inherited_session();
        cx.notify();
    }

    fn close_tab(&mut self, _: &CloseTab, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(active) = self.active_tab {
            self.close_tab_id(active);
            cx.notify();
        }
    }

    fn close_tab_id(&mut self, id: u64) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        if let Some(transport) = self.tabs[index].transport.as_ref() {
            transport.close();
        }
        self.tabs.remove(index);
        self.active_tab = if self.tabs.is_empty() {
            self.switcher_open = true;
            self.refresh_surfaces(false);
            None
        } else {
            Some(self.tabs[index.min(self.tabs.len() - 1)].id)
        };
        if let Some(active) = self.active_tab {
            self.reveal_tab(active);
        }
    }

    fn next_tab(&mut self, _: &NextTab, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(1);
        cx.notify();
    }

    fn previous_tab(&mut self, _: &PreviousTab, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(-1);
        cx.notify();
    }

    fn cycle_tab(&mut self, direction: isize) {
        if self.tabs.is_empty() {
            return;
        }
        let current = self
            .active_tab
            .and_then(|active| self.tabs.iter().position(|tab| tab.id == active))
            .unwrap_or(0) as isize;
        let next = (current + direction).rem_euclid(self.tabs.len() as isize) as usize;
        let tab_id = self.tabs[next].id;
        self.active_tab = Some(tab_id);
        self.reveal_tab(tab_id);
        self.switcher_open = false;
    }

    fn reveal_tab(&self, tab_id: u64) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.id == tab_id) {
            self.tab_scroll_handle.scroll_to_item(index);
        }
    }

    fn on_tab_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(32.0));
        let delta = if f32::from(delta.x).abs() > f32::EPSILON {
            f32::from(delta.x)
        } else {
            f32::from(delta.y)
        };
        let offset = self.tab_scroll_handle.offset();
        let max_offset = f32::from(self.tab_scroll_handle.max_offset().width);
        let x = (f32::from(offset.x) + delta).clamp(-max_offset, 0.0);
        self.tab_scroll_handle.set_offset(point(px(x), offset.y));
        cx.stop_propagation();
        cx.notify();
    }

    fn copy_or_interrupt(&mut self, _: &CopyOrInterrupt, _: &mut Window, cx: &mut Context<Self>) {
        match self
            .active_tab()
            .map(|tab| ctrl_c_behavior(tab.mirror.snapshot(), tab.selection))
        {
            Some(CtrlCBehavior::Copy(text)) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            Some(CtrlCBehavior::Interrupt) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.scroll_offset = 0;
                    tab.send(ClientMessage::Input {
                        data: vec![0x03],
                        latency_id: None,
                    });
                }
            }
            None => {}
        }
    }

    fn copy_selection(&mut self, _: &CopySelection, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self
            .active_tab()
            .and_then(|tab| selected_text(tab.mirror.snapshot(), tab.selection))
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste_clipboard(&mut self, _: &PasteClipboard, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let bracketed = tab
            .mirror
            .snapshot()
            .is_some_and(|snapshot| snapshot.modes.bracketed_paste);
        let data = input::encode_paste(&text, bracketed);
        tab.send(ClientMessage::Input {
            data,
            latency_id: None,
        });
    }

    fn toggle_switcher(
        &mut self,
        _: &ToggleSessionSwitcher,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.switcher_open = !self.switcher_open;
        if self.switcher_open {
            self.refresh_surfaces(false);
        }
        cx.notify();
    }

    fn report_focus(&mut self, focused: bool) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        if tab
            .mirror
            .snapshot()
            .is_some_and(|snapshot| snapshot.modes.focus_events)
        {
            tab.send(ClientMessage::Input {
                data: input::encode_focus(focused),
                latency_id: None,
            });
        }
    }

    fn on_terminal_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let Some(point) = self.grid_point(event.position) else {
            return;
        };
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let mouse_mode = tab
            .mirror
            .snapshot()
            .map(|snapshot| snapshot.modes.mouse)
            .unwrap_or_default();
        if mouse_mode != MouseMode::None && !event.modifiers.shift {
            if let Some(data) = encode_mouse(
                Some(terminal_button(event.button)),
                false,
                false,
                point.col,
                visible_row(tab.rows, point.row),
                terminal_modifiers(event.modifiers),
            ) {
                tab.send(ClientMessage::Input {
                    data,
                    latency_id: None,
                });
            }
            return;
        }
        let Some(absolute) = visible_to_absolute(tab.mirror.snapshot(), tab.scroll_offset, point)
        else {
            return;
        };
        tab.selection = match event.click_count {
            2 => word_selection(tab.mirror.snapshot(), absolute),
            count if count >= 3 => line_selection(tab.mirror.snapshot(), absolute),
            _ => Some(Selection {
                anchor: absolute,
                head: absolute,
            }),
        };
        tab.selecting = event.click_count == 1;
        cx.notify();
    }

    fn on_terminal_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(point) = self.grid_point(event.position) else {
            return;
        };
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let mouse_mode = tab
            .mirror
            .snapshot()
            .map(|snapshot| snapshot.modes.mouse)
            .unwrap_or_default();
        let report_motion = input::reports_mouse_motion(mouse_mode, event.pressed_button.is_some());
        if report_motion && !event.modifiers.shift {
            if let Some(data) = encode_mouse(
                event.pressed_button.map(terminal_button),
                false,
                true,
                point.col,
                visible_row(tab.rows, point.row),
                terminal_modifiers(event.modifiers),
            ) {
                tab.send(ClientMessage::Input {
                    data,
                    latency_id: None,
                });
            }
        } else if tab.selecting
            && event.dragging()
            && let Some(absolute) =
                visible_to_absolute(tab.mirror.snapshot(), tab.scroll_offset, point)
            && let Some(selection) = tab.selection.as_mut()
        {
            selection.head = absolute;
            cx.notify();
        }
    }

    fn on_terminal_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(point) = self.grid_point(event.position) else {
            return;
        };
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let mouse_mode = tab
            .mirror
            .snapshot()
            .map(|snapshot| snapshot.modes.mouse)
            .unwrap_or_default();
        if mouse_mode != MouseMode::None && !event.modifiers.shift {
            if let Some(data) = encode_mouse(
                Some(terminal_button(event.button)),
                true,
                false,
                point.col,
                visible_row(tab.rows, point.row),
                terminal_modifiers(event.modifiers),
            ) {
                tab.send(ClientMessage::Input {
                    data,
                    latency_id: None,
                });
            }
        } else {
            tab.selecting = false;
            let clicked_link = if let Some(selection) = tab.selection
                && selection.anchor == selection.head
            {
                tab.selection = None;
                (event.button == MouseButton::Left && event.modifiers.control)
                    .then(|| hyperlink_at(tab.mirror.snapshot(), selection.head))
                    .flatten()
                    .filter(|uri| is_allowed_hyperlink(uri))
            } else {
                None
            };
            if let Some(uri) = clicked_link {
                cx.open_url(uri);
            }
        }
        cx.notify();
    }

    fn on_terminal_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(point) = self.grid_point(event.position) else {
            return;
        };
        let cell_height = self.typography.cell_height;
        let delta = f32::from(event.delta.pixel_delta(px(cell_height)).y);
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let mouse_mode = tab
            .mirror
            .snapshot()
            .map(|snapshot| snapshot.modes.mouse)
            .unwrap_or_default();
        if mouse_mode != MouseMode::None && !event.modifiers.shift {
            let data = input::encode_mouse_wheel(
                delta > 0.0,
                point.col,
                visible_row(tab.rows, point.row),
                terminal_modifiers(event.modifiers),
            );
            tab.send(ClientMessage::Input {
                data,
                latency_id: None,
            });
        } else {
            let lines = (delta.abs() / cell_height).ceil().max(1.0) as usize;
            if delta > 0.0 {
                tab.scroll_offset = (tab.scroll_offset + lines).min(tab.max_scroll_offset());
            } else {
                tab.scroll_offset = tab.scroll_offset.saturating_sub(lines);
            }
            cx.notify();
        }
    }

    fn grid_point(&self, position: gpui::Point<Pixels>) -> Option<GridPoint> {
        let x = f32::from(position.x) - TERMINAL_PADDING;
        let y = f32::from(position.y) - CHROME_HEIGHT - TERMINAL_PADDING;
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let col = (x / self.typography.cell_width).floor() as usize;
        let row = (y / self.typography.cell_height).floor() as usize;
        if col >= self.terminal_cols as usize || row >= self.terminal_rows as usize {
            return None;
        }
        Some(GridPoint { row, col })
    }

    fn begin_titlebar_drag(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.tab_drag_origin = Some(event.position);
        window.focus(&self.focus_handle);
        #[cfg(target_os = "macos")]
        if event.click_count == 2 {
            self.tab_drag_origin = None;
            window.titlebar_double_click();
        }
    }

    fn begin_tab_drag(
        &mut self,
        tab_id: u64,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_tab = Some(tab_id);
        self.reveal_tab(tab_id);
        self.switcher_open = false;
        self.begin_titlebar_drag(event, window, cx);
        cx.stop_propagation();
        cx.notify();
    }

    fn on_titlebar_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            self.tab_drag_origin = None;
            return;
        }
        let Some(origin) = self.tab_drag_origin else {
            return;
        };
        if drag_threshold_crossed(origin, event.position) {
            self.tab_drag_origin = None;
            start_native_window_move(window);
            cx.stop_propagation();
        }
    }

    fn end_tab_drag(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.tab_drag_origin = None;
    }

    fn render_titlebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let multiple_tabs = self.tabs.len() > 1;
        let tabs = self.tabs.iter().map(|tab| {
            let id = tab.id;
            let active = self.active_tab == Some(id);
            let exceptional_state = match tab.state {
                ConnectionState::Connecting | ConnectionState::Reconnecting => Some(color(MUTED)),
                ConnectionState::Exited(_) => Some(color(MUTED)),
                ConnectionState::Failed => Some(color(ERROR)),
                ConnectionState::Attached => None,
            };
            div()
                .id(("tab", id as usize))
                .h_full()
                .flex_none()
                .w(px(TAB_WIDTH))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .border_b_2()
                .border_color(if multiple_tabs && active {
                    color(ACCENT)
                } else {
                    gpui::transparent_black()
                })
                .bg(if multiple_tabs && active {
                    color(SURFACE)
                } else {
                    color(BACKGROUND)
                })
                .text_color(if active {
                    color(FOREGROUND)
                } else {
                    color(MUTED)
                })
                .hover(|style| style.bg(color(SURFACE_HOVER)).cursor_pointer())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event, window, cx| {
                        this.begin_tab_drag(id, event, window, cx);
                    }),
                )
                .on_mouse_move(cx.listener(Self::on_titlebar_mouse_move))
                .on_mouse_up(MouseButton::Left, cx.listener(Self::end_tab_drag))
                .when_some(exceptional_state, |tab, state_color| {
                    tab.child(div().size(px(5.0)).rounded_full().bg(state_color))
                })
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(tab.title()),
                )
                .when(multiple_tabs && active, |tab| {
                    tab.child(
                        div()
                            .id(("close-tab", id as usize))
                            .size(px(22.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .text_color(color(MUTED))
                            .hover(|style| {
                                style
                                    .bg(color(BORDER))
                                    .text_color(color(FOREGROUND))
                                    .cursor_pointer()
                            })
                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_tab_id(id);
                                cx.stop_propagation();
                                cx.notify();
                            }))
                            .child(chrome_icon(ChromeIcon::Close, color(MUTED))),
                    )
                })
        });

        div()
            .h(px(CHROME_HEIGHT))
            .w_full()
            .relative()
            .overflow_hidden()
            .bg(color(BACKGROUND))
            .border_b_1()
            .border_color(color(BORDER).opacity(0.55))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::begin_titlebar_drag))
            .on_mouse_move(cx.listener(Self::on_titlebar_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::end_tab_drag))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::end_tab_drag))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(TITLEBAR_BRAND_WIDTH - 40.0))
                    .w(px(40.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::begin_titlebar_drag))
                    .on_mouse_move(cx.listener(Self::on_titlebar_mouse_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::end_tab_drag))
                    .child(chrome_icon(ChromeIcon::Mark, color(ACCENT))),
            )
            .child(
                div()
                    .id("tab-strip")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(TITLEBAR_BRAND_WIDTH))
                    .right(px(WINDOW_CONTROLS_WIDTH + TITLEBAR_ACTIONS_WIDTH))
                    .min_w(px(0.0))
                    .flex()
                    .overflow_x_scroll()
                    .track_scroll(&self.tab_scroll_handle)
                    .on_scroll_wheel(cx.listener(Self::on_tab_scroll))
                    .children(tabs)
                    .child(div().h_full().min_w(px(16.0)).flex_1()),
            )
            .child(
                div()
                    .id("new-tab")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(WINDOW_CONTROLS_WIDTH
                        + TITLEBAR_DRAG_WIDTH
                        + SESSION_SWITCHER_WIDTH))
                    .w(px(NEW_TAB_WIDTH))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(color(MUTED))
                    .hover(|style| {
                        style
                            .bg(color(SURFACE_HOVER))
                            .text_color(color(FOREGROUND))
                            .cursor_pointer()
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.create_inherited_session();
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .child(chrome_icon(ChromeIcon::Plus, color(MUTED))),
            )
            .child(
                div()
                    .id("session-switcher")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(WINDOW_CONTROLS_WIDTH + TITLEBAR_DRAG_WIDTH))
                    .w(px(SESSION_SWITCHER_WIDTH))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(color(MUTED))
                    .hover(|style| {
                        style
                            .bg(color(SURFACE_HOVER))
                            .text_color(color(FOREGROUND))
                            .cursor_pointer()
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.switcher_open = !this.switcher_open;
                        if this.switcher_open {
                            this.refresh_surfaces(false);
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .child(chrome_icon(ChromeIcon::Search, color(MUTED))),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(WINDOW_CONTROLS_WIDTH))
                    .w(px(TITLEBAR_DRAG_WIDTH)),
            )
            .when(cfg!(windows), |titlebar| {
                titlebar.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .w(px(WINDOW_CONTROLS_WIDTH))
                        .flex()
                        .bg(color(BACKGROUND))
                        .child(window_control(
                            "minimize-window",
                            ChromeIcon::Minimize,
                            WindowControlArea::Min,
                            false,
                        ))
                        .child(window_control(
                            "maximize-window",
                            ChromeIcon::Maximize,
                            WindowControlArea::Max,
                            false,
                        ))
                        .child(window_control(
                            "close-window",
                            ChromeIcon::Close,
                            WindowControlArea::Close,
                            true,
                        )),
                )
            })
    }

    fn render_switcher(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.surfaces.iter().map(|session| {
            let session = session.clone();
            let ending = self.ending_surfaces.contains(&session.id);
            let confirming = self.end_confirmation.as_ref() == Some(&session.id);
            let can_select = session.status == SurfaceStatus::Running && !ending;
            let status = if ending {
                "Ending…"
            } else {
                match session.status {
                    SurfaceStatus::Starting => "Starting",
                    SurfaceStatus::Ending => "Ending…",
                    SurfaceStatus::Running if session.attached => "Open",
                    SurfaceStatus::Running => "Detached",
                    SurfaceStatus::Exited => "Exited",
                    SurfaceStatus::Failed => "Failed",
                    SurfaceStatus::Lost => "Lost",
                }
            };
            let detail = (session.status == SurfaceStatus::Lost)
                .then(|| session.error.clone())
                .flatten();
            let session_for_attach = session.clone();
            let session_id_for_begin = session.id.clone();
            let session_id_for_cancel = session.id.clone();
            let session_id_for_confirm = session.id.clone();
            div()
                .id(("session", session.created_at_ms))
                .w_full()
                .mx_2()
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .justify_between()
                .rounded_sm()
                .when(can_select, |row| {
                    row.hover(|style| style.bg(color(SURFACE_HOVER)).cursor_pointer())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.attach_surface(session_for_attach.clone());
                            window.focus(&this.focus_handle);
                            cx.notify();
                        }))
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .flex_1()
                        .overflow_hidden()
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .when(session.status == SurfaceStatus::Running, |title| {
                                            title.child(div().size(px(5.0)).rounded_full().bg(
                                                color(if session.attached {
                                                    ACCENT
                                                } else {
                                                    MUTED
                                                }),
                                            ))
                                        })
                                        .child(short_surface_id(&session.id)),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_sm()
                                        .text_color(
                                            if matches!(
                                                session.status,
                                                SurfaceStatus::Failed | SurfaceStatus::Lost
                                            ) {
                                                color(ERROR)
                                            } else {
                                                color(MUTED)
                                            },
                                        )
                                        .child(status),
                                ),
                        )
                        .when_some(detail, |details, detail| {
                            details.child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_size(px(11.0))
                                    .text_color(color(MUTED))
                                    .child(detail),
                            )
                        })
                        .when(
                            session.status == SurfaceStatus::Running && !ending,
                            |details| {
                                details.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .when(!confirming, |actions| {
                                            actions.child(
                                                div()
                                                    .id(("end-session", session.created_at_ms))
                                                    .text_size(px(11.0))
                                                    .text_color(color(MUTED))
                                                    .hover(|style| {
                                                        style
                                                            .text_color(color(ERROR))
                                                            .cursor_pointer()
                                                    })
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.begin_end_surface(
                                                            session_id_for_begin.clone(),
                                                        );
                                                        cx.stop_propagation();
                                                        cx.notify();
                                                    }))
                                                    .child("End surface"),
                                            )
                                        })
                                        .when(confirming, |actions| {
                                            actions
                                                .child(
                                                    div()
                                                        .text_size(px(11.0))
                                                        .text_color(color(MUTED))
                                                        .child("End this surface?"),
                                                )
                                                .child(
                                                    div()
                                                        .id((
                                                            "cancel-end-session",
                                                            session.created_at_ms,
                                                        ))
                                                        .px_2()
                                                        .py_1()
                                                        .rounded_sm()
                                                        .text_size(px(11.0))
                                                        .text_color(color(MUTED))
                                                        .hover(|style| {
                                                            style
                                                                .bg(color(SURFACE_HOVER))
                                                                .cursor_pointer()
                                                        })
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.cancel_end_surface(
                                                                    &session_id_for_cancel,
                                                                );
                                                                cx.stop_propagation();
                                                                cx.notify();
                                                            },
                                                        ))
                                                        .child("Cancel"),
                                                )
                                                .child(
                                                    div()
                                                        .id((
                                                            "confirm-end-session",
                                                            session.created_at_ms,
                                                        ))
                                                        .px_2()
                                                        .py_1()
                                                        .rounded_sm()
                                                        .text_size(px(11.0))
                                                        .text_color(color(ERROR))
                                                        .hover(|style| {
                                                            style
                                                                .bg(color(SURFACE_HOVER))
                                                                .cursor_pointer()
                                                        })
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.confirm_end_surface(
                                                                    session_id_for_confirm.clone(),
                                                                );
                                                                cx.stop_propagation();
                                                                cx.notify();
                                                            },
                                                        ))
                                                        .child("End"),
                                                )
                                        }),
                                )
                            },
                        ),
                )
        });

        div()
            .absolute()
            .top(px(CHROME_HEIGHT + 12.0))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .w(px(520.0))
                    .max_h(px(420.0))
                    .bg(color(SURFACE))
                    .border_1()
                    .border_color(color(BORDER))
                    .rounded_md()
                    .overflow_hidden()
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(color(BORDER).opacity(0.7))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_color(color(FOREGROUND))
                                    .child(chrome_icon(ChromeIcon::Search, color(MUTED)))
                                    .child("Switch surface"),
                            )
                            .child(
                                div()
                                    .id("switcher-new-session")
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .text_color(color(ACCENT))
                                    .hover(|style| style.bg(color(SURFACE_HOVER)).cursor_pointer())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.create_inherited_session();
                                        cx.notify();
                                    }))
                                    .child("New"),
                            ),
                    )
                    .when(self.loading_surfaces, |panel| {
                        panel.child(
                            div()
                                .p_4()
                                .text_color(color(MUTED))
                                .child("Loading surfaces…"),
                        )
                    })
                    .children(rows),
            )
    }

    fn render_terminal(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paint = self
            .active_tab()
            .and_then(|tab| PaintModel::from_tab(tab, self.typography.clone()));
        let status = self.active_tab().map(|tab| tab.state);
        let error = self
            .active_tab()
            .and_then(|tab| tab.error.clone())
            .or_else(|| self.global_error.clone());
        let working_directory_warning = self.active_tab().and_then(|tab| {
            self.surfaces
                .iter()
                .find(|session| session.id == tab.surface_id)?
                .working_directory
                .as_ref()?
                .warning
                .clone()
        });
        let warning = error
            .is_none()
            .then(|| {
                [
                    self.global_warning.as_deref(),
                    working_directory_warning.as_deref(),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("\n")
            })
            .filter(|warning| !warning.is_empty());
        let input = cx.entity();
        let input_focus = self.focus_handle.clone();
        let composition =
            (!self.ime_text.is_empty()).then(|| SharedString::from(self.ime_text.clone()));
        div()
            .id("terminal")
            .size_full()
            .p(px(TERMINAL_PADDING))
            .bg(color(BACKGROUND))
            .overflow_hidden()
            .cursor(gpui::CursorStyle::IBeam)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_terminal_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_terminal_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_terminal_mouse_up))
            .on_mouse_move(cx.listener(Self::on_terminal_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_terminal_scroll))
            .child(
                canvas(
                    move |_, _, _| (),
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &input_focus,
                            ElementInputHandler::new(bounds, input.clone()),
                            cx,
                        );
                        if let Some(paint) = paint {
                            paint_terminal(bounds, &paint, window);
                            if let Some(composition) = composition {
                                paint_composition(
                                    bounds,
                                    paint.cursor,
                                    composition,
                                    &paint.typography,
                                    window,
                                    cx,
                                );
                            }
                        }
                    },
                )
                .size_full(),
            )
            .when(error.is_some(), |terminal| {
                terminal.child(
                    div()
                        .absolute()
                        .bottom_2()
                        .left_2()
                        .right_2()
                        .px_3()
                        .py_2()
                        .rounded_sm()
                        .bg(color(SURFACE))
                        .border_1()
                        .border_color(color(ERROR))
                        .text_sm()
                        .text_color(color(ERROR))
                        .child(error.unwrap_or_default()),
                )
            })
            .when(warning.is_some(), |terminal| {
                terminal.child(
                    div()
                        .absolute()
                        .bottom_2()
                        .left_2()
                        .right_2()
                        .px_3()
                        .py_2()
                        .rounded_sm()
                        .bg(color(SURFACE))
                        .border_1()
                        .border_color(color(ACCENT))
                        .text_sm()
                        .text_color(color(FOREGROUND))
                        .child(warning.unwrap_or_default()),
                )
            })
            .when(
                matches!(
                    status,
                    Some(ConnectionState::Connecting | ConnectionState::Reconnecting)
                ),
                |terminal| {
                    terminal.child(
                        div()
                            .absolute()
                            .top_2()
                            .right_2()
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .bg(color(SURFACE))
                            .text_sm()
                            .text_color(color(MUTED))
                            .child(if status == Some(ConnectionState::Connecting) {
                                "Connecting"
                            } else {
                                "Reconnecting"
                            }),
                    )
                },
            )
    }
}

#[derive(Clone, Copy)]
enum ChromeIcon {
    Mark,
    Plus,
    Search,
    Minimize,
    Maximize,
    Close,
}

fn chrome_icon(icon: ChromeIcon, tint: Hsla) -> impl IntoElement {
    canvas(
        move |_, _, _| (),
        move |bounds, _, window, _| {
            let x = |value: f32| bounds.left() + px(value);
            let y = |value: f32| bounds.top() + px(value);
            let mut path = PathBuilder::stroke(px(1.25));
            match icon {
                ChromeIcon::Mark => {
                    path.move_to(point(x(2.0), y(8.0)));
                    path.line_to(point(x(8.0), y(3.0)));
                    path.line_to(point(x(14.0), y(8.0)));
                    path.line_to(point(x(8.0), y(13.0)));
                    path.line_to(point(x(2.0), y(8.0)));
                    path.move_to(point(x(6.0), y(11.0)));
                    path.line_to(point(x(10.0), y(5.0)));
                }
                ChromeIcon::Plus => {
                    path.move_to(point(x(3.0), y(8.0)));
                    path.line_to(point(x(13.0), y(8.0)));
                    path.move_to(point(x(8.0), y(3.0)));
                    path.line_to(point(x(8.0), y(13.0)));
                }
                ChromeIcon::Search => {
                    path.move_to(point(x(9.5), y(3.5)));
                    path.line_to(point(x(6.0), y(2.5)));
                    path.line_to(point(x(3.0), y(4.5)));
                    path.line_to(point(x(2.5), y(8.0)));
                    path.line_to(point(x(4.5), y(11.0)));
                    path.line_to(point(x(8.0), y(11.5)));
                    path.line_to(point(x(10.5), y(9.5)));
                    path.line_to(point(x(11.0), y(6.0)));
                    path.line_to(point(x(9.5), y(3.5)));
                    path.move_to(point(x(10.0), y(10.0)));
                    path.line_to(point(x(14.0), y(14.0)));
                }
                ChromeIcon::Minimize => {
                    path.move_to(point(x(3.0), y(11.0)));
                    path.line_to(point(x(13.0), y(11.0)));
                }
                ChromeIcon::Maximize => {
                    path.move_to(point(x(3.5), y(3.5)));
                    path.line_to(point(x(12.5), y(3.5)));
                    path.line_to(point(x(12.5), y(12.5)));
                    path.line_to(point(x(3.5), y(12.5)));
                    path.line_to(point(x(3.5), y(3.5)));
                }
                ChromeIcon::Close => {
                    path.move_to(point(x(3.5), y(3.5)));
                    path.line_to(point(x(12.5), y(12.5)));
                    path.move_to(point(x(12.5), y(3.5)));
                    path.line_to(point(x(3.5), y(12.5)));
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, tint);
            }
        },
    )
    .size(px(16.0))
}

fn window_control(
    id: &'static str,
    icon: ChromeIcon,
    area: WindowControlArea,
    destructive: bool,
) -> impl IntoElement {
    div()
        .id(id)
        .h_full()
        .w(px(WINDOW_CONTROLS_WIDTH / 3.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .text_color(color(MUTED))
        .hover(move |style| {
            style
                .bg(if destructive {
                    color(ERROR)
                } else {
                    color(SURFACE_HOVER)
                })
                .text_color(color(FOREGROUND))
        })
        .window_control_area(area)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, _| match area {
            WindowControlArea::Min => window.minimize_window(),
            WindowControlArea::Max => toggle_window_maximized(window),
            WindowControlArea::Close => window.remove_window(),
            WindowControlArea::Drag => {}
        })
        .child(chrome_icon(icon, color(MUTED)))
}
fn drag_threshold_crossed(origin: Point<Pixels>, current: Point<Pixels>) -> bool {
    let dx = f32::from(current.x - origin.x);
    let dy = f32::from(current.y - origin.y);
    dx * dx + dy * dy >= TAB_DRAG_THRESHOLD * TAB_DRAG_THRESHOLD
}

#[cfg(windows)]
fn start_native_window_move(window: &Window) {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = HWND(handle.hwnd.get() as *mut core::ffi::c_void);
    unsafe {
        let _ = ReleaseCapture();
        let _ = PostMessageW(
            Some(hwnd),
            WM_NCLBUTTONDOWN,
            WPARAM(HTCAPTION as usize),
            LPARAM(0),
        );
    }
}

#[cfg(windows)]
fn toggle_window_maximized(window: &Window) {
    if !window.is_maximized() {
        window.zoom_window();
        return;
    }
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = HWND(handle.hwnd.get() as *mut core::ffi::c_void);
    unsafe {
        let _ = ShowWindowAsync(hwnd, SW_RESTORE);
    }
}

#[cfg(target_os = "macos")]
fn start_native_window_move(window: &Window) {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    // GPUI 0.2.2's Mac backend does not implement start_window_move.
    // The GPUI-owned NSView supplies the window and current drag event.
    unsafe {
        let view = handle.ns_view.as_ptr() as *mut objc2::runtime::AnyObject;
        let native_window: *mut objc2::runtime::AnyObject = msg_send![view, window];
        let event: *mut objc2::runtime::AnyObject = msg_send![native_window, currentEvent];
        if !event.is_null() {
            let _: () = msg_send![native_window, performWindowDragWithEvent: event];
        }
    }
}

#[cfg(target_os = "macos")]
fn toggle_window_maximized(window: &Window) {
    window.zoom_window();
}

impl Focusable for CompiApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for CompiApp {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let len = self.ime_text.encode_utf16().count();
        let start = range.start.min(len);
        let end = range.end.max(start).min(len);
        adjusted_range.replace(start..end);
        Some(
            self.ime_text
                [utf16_byte_index(&self.ime_text, start)..utf16_byte_index(&self.ime_text, end)]
                .to_owned(),
        )
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.ime_selected_range.clone(),
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.ime_marked_range.clone()
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = std::mem::take(&mut self.ime_text);
        self.replace_text_in_range(None, &text, window, cx);
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime_text.clear();
        self.ime_marked_range = None;
        self.ime_selected_range = 0..0;
        if !text.is_empty()
            && let Some(tab) = self.active_tab_mut()
        {
            tab.scroll_offset = 0;
            tab.send(ClientMessage::Input {
                data: text.as_bytes().to_vec(),
                latency_id: None,
            });
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let replace = range
            .or_else(|| self.ime_marked_range.clone())
            .unwrap_or_else(|| self.ime_selected_range.clone());
        let len = self.ime_text.encode_utf16().count();
        let replace = replace.start.min(len)..replace.end.max(replace.start).min(len);
        let start = utf16_byte_index(&self.ime_text, replace.start);
        let end = utf16_byte_index(&self.ime_text, replace.end);
        self.ime_text.replace_range(start..end, new_text);
        let inserted_len = new_text.encode_utf16().count();
        self.ime_marked_range = (!new_text.is_empty())
            .then_some(replace.start..replace.start.saturating_add(inserted_len));
        self.ime_selected_range = new_selected_range
            .map(|selected| {
                replace.start.saturating_add(selected.start)
                    ..replace.start.saturating_add(selected.end)
            })
            .unwrap_or_else(|| {
                let end = replace.start.saturating_add(inserted_len);
                end..end
            });
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let cursor = self
            .active_tab()
            .and_then(|tab| tab.mirror.snapshot())
            .map(|snapshot| snapshot.cursor)?;
        let typography = &self.typography;
        Some(Bounds::new(
            point(
                bounds.left() + px(f32::from(cursor.col) * typography.cell_width),
                bounds.top() + px(f32::from(cursor.row) * typography.cell_height),
            ),
            size(px(typography.cell_width), px(typography.cell_height)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.ime_selected_range.end)
    }
}

impl Render for CompiApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.refresh_typography(window) {
            self.update_dimensions(window);
        }
        let title = self
            .active_tab()
            .map(|tab| format!("{} · Compi", tab.title()))
            .unwrap_or_else(|| String::from("Compi"));
        if title != self.window_title {
            window.set_window_title(&title);
            self.window_title = title;
        }
        if self.ready_probe_render_pending && !self.ready_probe_logged {
            self.ready_probe_render_pending = false;
            self.ready_probe_logged = true;
            let started_at = self.started_at;
            let sent_at = self.ready_probe_sent_at;
            window.on_next_frame(move |_, _| {
                perf::log_startup_metric("ready_for_input_ms", started_at.elapsed());
                if let Some(sent_at) = sent_at {
                    perf::log_startup_metric("input_to_render_ms", sent_at.elapsed());
                }
            });
        }
        if !self.pending_present_latency_ids.is_empty() {
            let latency_ids = std::mem::take(&mut self.pending_present_latency_ids);
            window.on_next_frame(move |_, _| {
                for latency_id in latency_ids {
                    perf::log_input_latency_stage(latency_id, "frame_presented", None);
                }
            });
        }
        div()
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if this.handle_keystroke(&event.keystroke) {
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .font_family(UI_FONT)
            .text_size(px(13.0))
            .text_color(color(FOREGROUND))
            .bg(color(BACKGROUND))
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::copy_or_interrupt))
            .on_action(cx.listener(Self::copy_selection))
            .on_action(cx.listener(Self::paste_clipboard))
            .on_action(cx.listener(Self::toggle_switcher))
            .child(self.render_titlebar(cx))
            .child(self.render_terminal(cx))
            .when(self.switcher_open, |client| {
                client.child(self.render_switcher(cx))
            })
    }
}

#[derive(Clone)]
struct PaintModel {
    visible_rows: Vec<Row>,
    base_row: usize,
    scrollback_len: usize,
    cursor: CursorState,
    alternate_screen: bool,
    placements: Vec<KittyPlacement>,
    scroll_offset: usize,
    selection: Option<Selection>,
    images: HashMap<u32, Arc<RenderImage>>,
    row_render_cache: Arc<Mutex<RowRenderCache>>,
    typography: Arc<TerminalTypography>,
}

impl PaintModel {
    fn from_tab(tab: &TerminalTab, typography: Arc<TerminalTypography>) -> Option<Self> {
        let snapshot = tab.mirror.snapshot()?;
        let (visible_rows, base_row) = visible_rows(snapshot, tab.scroll_offset);
        Some(Self {
            visible_rows: visible_rows.cloned().collect(),
            base_row,
            scrollback_len: snapshot.scrollback.len(),
            cursor: snapshot.cursor,
            alternate_screen: snapshot.modes.alternate_screen,
            placements: snapshot.placements.clone(),
            scroll_offset: tab.scroll_offset,
            selection: tab.selection,
            images: tab
                .image_cache
                .iter()
                .map(|(id, (_, image))| (*id, image.clone()))
                .collect(),
            row_render_cache: tab.row_render_cache.clone(),
            typography,
        })
    }
}

fn paint_terminal(bounds: Bounds<Pixels>, model: &PaintModel, window: &mut Window) {
    let paint_started_at = performance_metrics().map(|_| Instant::now());
    let visible = &model.visible_rows;
    let base = model.base_row;
    let typography = &model.typography;
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        paint_images(bounds, model, base, false, typography, window);
        for (row_index, row) in visible.iter().enumerate() {
            paint_row_backgrounds(bounds, row_index, row, typography, window);
        }
        if let Some(selection) = model.selection {
            paint_selection(bounds, selection, base, visible.len(), typography, window);
        }
        paint_cursor(bounds, model, base, typography, window);
        for (row_index, row) in visible.iter().enumerate() {
            paint_row_text(
                bounds,
                row_index,
                row,
                &model.row_render_cache,
                typography,
                window,
            );
        }
        paint_images(bounds, model, base, true, typography, window);
    });
    if let Some(started_at) = paint_started_at {
        record_paint_metrics(started_at);
    }
}

fn paint_row_backgrounds(
    bounds: Bounds<Pixels>,
    row_index: usize,
    row: &Row,
    typography: &TerminalTypography,
    window: &mut Window,
) {
    let mut start = 0;
    while start < row.cells.len() {
        let background = effective_colors(&row.cells[start]).1;
        let mut end = start + 1;
        while end < row.cells.len() && effective_colors(&row.cells[end]).1 == background {
            end += 1;
        }
        if background != color(BACKGROUND) {
            window.paint_quad(fill(
                Bounds::new(
                    point(
                        bounds.left() + px(start as f32 * typography.cell_width),
                        bounds.top() + px(row_index as f32 * typography.cell_height),
                    ),
                    size(
                        px((end - start) as f32 * typography.cell_width),
                        px(typography.cell_height),
                    ),
                ),
                background,
            ));
        }
        start = end;
    }
}

#[derive(Clone)]
struct FixedGlyph {
    col: usize,
    x: Pixels,
    font_id: FontId,
    glyph_id: GlyphId,
    is_emoji: bool,
}

#[derive(Clone)]
struct ShapedRun {
    start: usize,
    end: usize,
    color: Hsla,
    underline: bool,
    strikethrough: bool,
    glyphs: Vec<FixedGlyph>,
}

struct CellMapping {
    bytes: Range<usize>,
    col: usize,
}

struct CachedRow {
    source: Row,
    runs: Arc<Vec<ShapedRun>>,
}

#[derive(Default)]
struct RowRenderCache {
    entries: HashMap<u64, CachedRow>,
    insertion_order: VecDeque<u64>,
}

impl RowRenderCache {
    const MAX_ROWS: usize = 512;

    fn get(&self, fingerprint: u64, row: &Row) -> Option<Arc<Vec<ShapedRun>>> {
        self.entries
            .get(&fingerprint)
            .filter(|cached| cached.source == *row)
            .map(|cached| cached.runs.clone())
    }

    fn insert(&mut self, fingerprint: u64, row: Row, runs: Arc<Vec<ShapedRun>>) {
        if !self.entries.contains_key(&fingerprint) {
            while self.entries.len() >= Self::MAX_ROWS {
                let Some(oldest) = self.insertion_order.pop_front() else {
                    break;
                };
                self.entries.remove(&oldest);
            }
            self.insertion_order.push_back(fingerprint);
        }
        self.entries
            .insert(fingerprint, CachedRow { source: row, runs });
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.insertion_order.clear();
    }
}

fn row_fingerprint(row: &Row) -> u64 {
    let mut hasher = DefaultHasher::new();
    row.hash(&mut hasher);
    hasher.finish()
}

fn paint_row_text(
    bounds: Bounds<Pixels>,
    row_index: usize,
    row: &Row,
    cache: &Arc<Mutex<RowRenderCache>>,
    typography: &TerminalTypography,
    window: &mut Window,
) {
    let fingerprint = row_fingerprint(row);
    let cached = cache
        .lock()
        .ok()
        .and_then(|cache| cache.get(fingerprint, row));
    let runs = cached.unwrap_or_else(|| {
        let shaped = Arc::new(shape_row(row, typography, window));
        if let Ok(mut cache) = cache.lock() {
            cache.insert(fingerprint, row.clone(), shaped.clone());
        }
        shaped
    });
    let row_top = bounds.top() + px(row_index as f32 * typography.cell_height);
    let baseline = row_top + px(typography.baseline);
    for run in runs.iter() {
        for glyph in &run.glyphs {
            let origin = point(
                bounds.left() + px(glyph.col as f32 * typography.cell_width) + glyph.x,
                baseline,
            );
            let _ = if glyph.is_emoji {
                window.paint_emoji(
                    origin,
                    glyph.font_id,
                    glyph.glyph_id,
                    px(typography.font_size),
                )
            } else {
                window.paint_glyph(
                    origin,
                    glyph.font_id,
                    glyph.glyph_id,
                    px(typography.font_size),
                    run.color,
                )
            };
        }
        let left = bounds.left() + px(run.start as f32 * typography.cell_width);
        let width = px((run.end - run.start) as f32 * typography.cell_width);
        if run.underline {
            window.paint_quad(fill(
                Bounds::new(
                    point(left, row_top + px(typography.cell_height - 1.0)),
                    size(width, px(1.0)),
                ),
                run.color,
            ));
        }
        if run.strikethrough {
            window.paint_quad(fill(
                Bounds::new(
                    point(left, baseline - px((typography.font_size * 0.3).round())),
                    size(width, px(1.0)),
                ),
                run.color,
            ));
        }
    }
}

fn paint_composition(
    bounds: Bounds<Pixels>,
    cursor: CursorState,
    text: SharedString,
    typography: &TerminalTypography,
    window: &mut Window,
    cx: &mut App,
) {
    let run = TextRun {
        len: text.len(),
        font: typography.font.clone(),
        color: color(FOREGROUND),
        background_color: None,
        underline: Some(UnderlineStyle {
            color: Some(color(ACCENT)),
            thickness: px(1.0),
            wavy: false,
        }),
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(text, px(typography.font_size), &[run], None);
    let cell_top = bounds.top() + px(f32::from(cursor.row) * typography.cell_height);
    let origin = point(
        bounds.left() + px(f32::from(cursor.col) * typography.cell_width),
        cell_top + px(typography.baseline)
            - ((px(typography.cell_height) - line.ascent - line.descent) / 2.0 + line.ascent),
    );
    window.paint_quad(fill(
        Bounds::new(
            point(origin.x, cell_top),
            size(line.width, px(typography.cell_height)),
        ),
        color(BACKGROUND),
    ));
    let _ = line.paint(origin, px(typography.cell_height), window, cx);
}

fn shape_row(row: &Row, typography: &TerminalTypography, window: &mut Window) -> Vec<ShapedRun> {
    let mut shaped = Vec::new();
    let mut start = 0;
    while start < row.cells.len() {
        if row.cells[start].width == 0 {
            start += 1;
            continue;
        }
        let style = &row.cells[start];
        let mut end = start;
        let mut text = String::new();
        let mut cells = Vec::new();
        while end < row.cells.len() {
            let cell = &row.cells[end];
            if cell.width == 0 {
                end += 1;
                continue;
            }
            if end != start && !same_text_style(style, cell) {
                break;
            }
            let byte_start = text.len();
            if cell.attributes.hidden {
                text.push(' ');
            } else {
                text.push_str(&cell.text);
            }
            cells.push(CellMapping {
                bytes: byte_start..text.len(),
                col: end,
            });
            end += usize::from(cell.width.max(1));
        }
        let (foreground, _) = effective_colors(style);
        let color = if style.attributes.dim {
            foreground.opacity(0.58)
        } else {
            foreground
        };
        let mut terminal_font = typography.font.clone();
        terminal_font.weight = if style.attributes.bold {
            FontWeight::BOLD
        } else {
            FontWeight::NORMAL
        };
        terminal_font.style = if style.attributes.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        };
        let run = TextRun {
            len: text.len(),
            font: terminal_font,
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window.text_system().shape_line(
            SharedString::from(text),
            px(typography.font_size),
            &[run],
            None,
        );
        let mut glyphs = Vec::new();
        for font_run in &line.runs {
            for glyph in &font_run.glyphs {
                let cell_index = cells
                    .partition_point(|cell| cell.bytes.start <= glyph.index)
                    .saturating_sub(1);
                let Some(cell) = cells
                    .get(cell_index)
                    .filter(|cell| cell.bytes.contains(&glyph.index))
                else {
                    continue;
                };
                glyphs.push(FixedGlyph {
                    col: cell.col,
                    x: glyph.position.x - line.x_for_index(cell.bytes.start),
                    font_id: font_run.font_id,
                    glyph_id: glyph.id,
                    is_emoji: glyph.is_emoji,
                });
            }
        }
        shaped.push(ShapedRun {
            start,
            end,
            color,
            underline: style.attributes.underline || style.hyperlink.is_some(),
            strikethrough: style.attributes.strike,
            glyphs,
        });
        start = end;
    }
    shaped
}

fn paint_selection(
    bounds: Bounds<Pixels>,
    selection: Selection,
    base: usize,
    visible_rows: usize,
    typography: &TerminalTypography,
    window: &mut Window,
) {
    let (start, end) = selection.ordered();
    for absolute_row in start.row..=end.row {
        if absolute_row < base || absolute_row >= base + visible_rows {
            continue;
        }
        let visible_row = absolute_row - base;
        let first_col = if absolute_row == start.row {
            start.col
        } else {
            0
        };
        let last_col = if absolute_row == end.row {
            end.col.saturating_add(1)
        } else {
            usize::MAX
        };
        let last_col = last_col
            .min(((f32::from(bounds.size.width) / typography.cell_width).floor() as usize).max(1));
        if last_col <= first_col {
            continue;
        }
        window.paint_quad(fill(
            Bounds::new(
                point(
                    bounds.left() + px(first_col as f32 * typography.cell_width),
                    bounds.top() + px(visible_row as f32 * typography.cell_height),
                ),
                size(
                    px((last_col - first_col) as f32 * typography.cell_width),
                    px(typography.cell_height),
                ),
            ),
            color(SELECTION).opacity(0.82),
        ));
    }
}

fn paint_cursor(
    bounds: Bounds<Pixels>,
    model: &PaintModel,
    base: usize,
    typography: &TerminalTypography,
    window: &mut Window,
) {
    if !model.cursor.visible || model.scroll_offset != 0 {
        return;
    }
    let absolute_row = model.scrollback_len + usize::from(model.cursor.row);
    if absolute_row < base {
        return;
    }
    let row = absolute_row - base;
    let col = usize::from(model.cursor.col);
    let (x, y) = (
        bounds.left() + px(col as f32 * typography.cell_width),
        bounds.top() + px(row as f32 * typography.cell_height),
    );
    let cursor_bounds = match model.cursor.shape {
        CursorShape::Block => Bounds::new(
            point(x, y),
            size(px(typography.cell_width), px(typography.cell_height)),
        ),
        CursorShape::Underline => Bounds::new(
            point(x, y + px(typography.cell_height - 2.0)),
            size(px(typography.cell_width), px(2.0)),
        ),
        CursorShape::Bar => Bounds::new(point(x, y), size(px(2.0), px(typography.cell_height))),
    };
    window.paint_quad(fill(cursor_bounds, color(ACCENT).opacity(0.78)));
}

fn paint_images(
    bounds: Bounds<Pixels>,
    model: &PaintModel,
    base: usize,
    foreground: bool,
    typography: &TerminalTypography,
    window: &mut Window,
) {
    let mut placements: Vec<KittyPlacement> = model
        .placements
        .iter()
        .copied()
        .filter(|placement| (placement.z_index > 0) == foreground)
        .collect();
    placements.sort_by_key(|placement| placement.z_index);
    for placement in placements {
        if placement.alternate_screen != model.alternate_screen {
            continue;
        }
        let Some(image) = model.images.get(&placement.image_id).cloned() else {
            continue;
        };
        let absolute_row = if placement.alternate_screen {
            if placement.row < 0 {
                continue;
            }
            placement.row as usize
        } else {
            let row = model.scrollback_len as i64 + i64::from(placement.row);
            if row < 0 {
                continue;
            }
            row as usize
        };
        if absolute_row < base {
            continue;
        }
        let visible_row = absolute_row - base;
        let rows = placement.rows.unwrap_or_else(|| {
            ((image.size(0).height.0 as f32 / typography.cell_height).ceil() as u16).max(1)
        });
        let cols = placement.cols.unwrap_or_else(|| {
            ((image.size(0).width.0 as f32 / typography.cell_width).ceil() as u16).max(1)
        });
        let image_bounds = Bounds::new(
            point(
                bounds.left() + px(f32::from(placement.col) * typography.cell_width),
                bounds.top() + px(visible_row as f32 * typography.cell_height),
            ),
            size(
                px(f32::from(cols) * typography.cell_width),
                px(f32::from(rows) * typography.cell_height),
            ),
        );
        let _ = window.paint_image(image_bounds, Corners::default(), image, 0, false);
    }
}

fn same_text_style(left: &Cell, right: &Cell) -> bool {
    left.foreground == right.foreground
        && left.background == right.background
        && left.attributes == right.attributes
        && left.hyperlink == right.hyperlink
}

fn effective_colors(cell: &Cell) -> (Hsla, Hsla) {
    let foreground = terminal_color(cell.foreground, true);
    let background = terminal_color(cell.background, false);
    if cell.attributes.inverse {
        (background, foreground)
    } else {
        (foreground, background)
    }
}
fn diagnostic_warning(config: &[String], typography: &[String]) -> Option<String> {
    let warning = config
        .iter()
        .chain(typography)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    (!warning.is_empty()).then_some(warning)
}

fn terminal_color(value: Color, foreground: bool) -> Hsla {
    match value {
        Color::Default => color(if foreground { FOREGROUND } else { BACKGROUND }),
        Color::Rgb(red, green, blue) => {
            color((u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue))
        }
        Color::Indexed(index) => color(ansi_color(index)),
    }
}

fn ansi_color(index: u8) -> u32 {
    const ANSI: [u32; 16] = [
        0x1d1b18, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xd8d2c7, 0x5c5660,
        0xff7a85, 0xb4d88a, 0xffd68a, 0x84c4ff, 0xdd91e8, 0x78dce8, 0xf5f0e8,
    ];
    if index < 16 {
        return ANSI[index as usize];
    }
    if index < 232 {
        let value = index - 16;
        let component = |part: u8| {
            if part == 0 {
                0
            } else {
                55 + 40 * u32::from(part)
            }
        };
        let red = component(value / 36);
        let green = component((value % 36) / 6);
        let blue = component(value % 6);
        return (red << 16) | (green << 8) | blue;
    }
    let gray = 8 + 10 * u32::from(index - 232);
    (gray << 16) | (gray << 8) | gray
}

fn color(value: u32) -> Hsla {
    rgb(value).into()
}

#[cfg(windows)]
fn active_keypad_key() -> Option<KeypadKey> {
    [
        (VK_NUMPAD0, KeypadKey::Digit(0)),
        (VK_NUMPAD1, KeypadKey::Digit(1)),
        (VK_NUMPAD2, KeypadKey::Digit(2)),
        (VK_NUMPAD3, KeypadKey::Digit(3)),
        (VK_NUMPAD4, KeypadKey::Digit(4)),
        (VK_NUMPAD5, KeypadKey::Digit(5)),
        (VK_NUMPAD6, KeypadKey::Digit(6)),
        (VK_NUMPAD7, KeypadKey::Digit(7)),
        (VK_NUMPAD8, KeypadKey::Digit(8)),
        (VK_NUMPAD9, KeypadKey::Digit(9)),
        (VK_DECIMAL, KeypadKey::Decimal),
        (VK_DIVIDE, KeypadKey::Divide),
        (VK_MULTIPLY, KeypadKey::Multiply),
        (VK_SUBTRACT, KeypadKey::Subtract),
        (VK_ADD, KeypadKey::Add),
    ]
    .into_iter()
    .find_map(|(virtual_key, keypad)| {
        (unsafe { GetKeyState(virtual_key.0 as i32) } < 0).then_some(keypad)
    })
}

#[cfg(target_os = "macos")]
fn active_keypad_key() -> Option<KeypadKey> {
    // GPUI normalizes keypad digits to text, so recover the physical key
    // only when the terminal has enabled application-keypad mode.
    let key_code: u16 = unsafe {
        let application: *mut objc2::runtime::AnyObject =
            msg_send![class!(NSApplication), sharedApplication];
        let event: *mut objc2::runtime::AnyObject = msg_send![application, currentEvent];
        if event.is_null() {
            return None;
        }
        let event_type: usize = msg_send![event, type];
        if event_type != 10 {
            return None;
        }
        msg_send![event, keyCode]
    };
    match key_code {
        82 => Some(KeypadKey::Digit(0)),
        83 => Some(KeypadKey::Digit(1)),
        84 => Some(KeypadKey::Digit(2)),
        85 => Some(KeypadKey::Digit(3)),
        86 => Some(KeypadKey::Digit(4)),
        87 => Some(KeypadKey::Digit(5)),
        88 => Some(KeypadKey::Digit(6)),
        89 => Some(KeypadKey::Digit(7)),
        91 => Some(KeypadKey::Digit(8)),
        92 => Some(KeypadKey::Digit(9)),
        65 => Some(KeypadKey::Decimal),
        75 => Some(KeypadKey::Divide),
        67 => Some(KeypadKey::Multiply),
        78 => Some(KeypadKey::Subtract),
        69 => Some(KeypadKey::Add),
        _ => None,
    }
}

fn terminal_key(keystroke: &Keystroke) -> Key<'_> {
    Key {
        key: keystroke.key.as_str(),
        key_char: keystroke.key_char.as_deref(),
        modifiers: terminal_modifiers(keystroke.modifiers),
    }
}

fn terminal_modifiers(modifiers: gpui::Modifiers) -> Modifiers {
    Modifiers {
        shift: modifiers.shift,
        alt: modifiers.alt,
        control: modifiers.control,
    }
}

fn terminal_button(button: MouseButton) -> input::MouseButton {
    match button {
        MouseButton::Left => input::MouseButton::Left,
        MouseButton::Middle => input::MouseButton::Middle,
        MouseButton::Right => input::MouseButton::Right,
        MouseButton::Navigate(_) => input::MouseButton::Unsupported,
    }
}

fn decode_kitty_image(image: &KittyImage) -> Result<Arc<RenderImage>, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&image.data)
        .map_err(|error| error.to_string())?;
    let mut buffer = match image.format {
        32 => RgbaImage::from_raw(image.width, image.height, bytes)
            .ok_or_else(|| "RGBA payload length does not match dimensions".to_string())?,
        24 => {
            let expected = image.width as usize * image.height as usize * 3;
            if bytes.len() != expected {
                return Err("RGB payload length does not match dimensions".into());
            }
            let mut rgba = Vec::with_capacity(image.width as usize * image.height as usize * 4);
            for pixel in bytes.as_chunks::<3>().0 {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
            }
            RgbaImage::from_raw(image.width, image.height, rgba)
                .ok_or_else(|| "could not create RGB image".to_string())?
        }
        100 => image::load_from_memory(&bytes)
            .map_err(|error| error.to_string())?
            .into_rgba8(),
        format => return Err(format!("unsupported Kitty image format {format}")),
    };
    let raw: &mut [u8] = buffer.as_mut();
    for pixel in raw.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new(smallvec![ImageFrame::new(
        buffer
    )])))
}

fn spawn_tab_worker(
    tab_id: u64,
    surface_id: SurfaceId,
    cols: i16,
    rows: i16,
    sender: UiEventSender,
    instance: Option<String>,
) {
    thread::spawn(move || {
        let stop = Arc::new(AtomicBool::new(false));
        loop {
            if stop.load(Ordering::Acquire) {
                return;
            }
            let result = run_tab_connection(
                tab_id,
                &surface_id,
                cols,
                rows,
                stop.clone(),
                &sender,
                instance.as_deref(),
            );
            if stop.load(Ordering::Acquire) {
                return;
            }
            let error = result
                .err()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "daemon connection closed".into());
            let _ = sender.send(UiEvent::TabDisconnected { tab_id, error });
            thread::sleep(RECONNECT_DELAY);
        }
    });
}

fn run_tab_connection(
    tab_id: u64,
    surface_id: &SurfaceId,
    cols: i16,
    rows: i16,
    stop: Arc<AtomicBool>,
    sender: &UiEventSender,
    instance: Option<&str>,
) -> crate::Result<()> {
    let mut client = probe::connect_or_start(instance)?;
    let workspace = client.workspace()?;
    let Some(surface) = workspace.surface(surface_id).cloned() else {
        stop.store(true, Ordering::Release);
        let _ = sender.send(UiEvent::TabControl {
            tab_id,
            message: ServerMessage::Error {
                code: compi_protocol::ErrorCode::SurfaceNotFound,
                message: "surface no longer exists".into(),
                current_revision: Some(workspace.revision),
            },
        });
        return Ok(());
    };
    if surface.status != SurfaceStatus::Running {
        stop.store(true, Ordering::Release);
        let _ = sender.send(UiEvent::TabControl {
            tab_id,
            message: ServerMessage::Error {
                code: compi_protocol::ErrorCode::SurfaceUnavailable,
                message: surface
                    .error
                    .unwrap_or_else(|| format!("surface is {:?}", surface.status).to_lowercase()),
                current_revision: Some(workspace.revision),
            },
        });
        return Ok(());
    }
    client.attach_surface(&surface, cols, rows)?;
    while let Some(message) = client.take_pending_screen() {
        let _ = sender.send(UiEvent::TabScreen { tab_id, message });
    }
    let (connection, next_request_id, pending, target, workspace) = client.into_parts();
    for message in pending {
        let _ = sender.send(UiEvent::TabScreen { tab_id, message });
    }
    let target = target.ok_or("terminal attachment target was not established")?;
    let (command_tx, command_rx) = mpsc::channel();
    let transport = TabTransport {
        commands: command_tx,
        stop: stop.clone(),
    };
    if !sender.send(UiEvent::TabConnected { tab_id, transport }) {
        return Err("UI closed while attaching terminal".into());
    }
    let mut reader =
        DaemonClient::from_attached_parts(connection, next_request_id, target, workspace);
    loop {
        loop {
            match command_rx.try_recv() {
                Ok(message) => {
                    let latency_id = match &message {
                        ClientMessage::Input { latency_id, .. } => *latency_id,
                        _ => None,
                    };
                    reader.send(message)?;
                    if let Some(latency_id) = latency_id {
                        perf::log_input_latency_stage(latency_id, "client_sent", None);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        match reader.poll_event()? {
            Some(ServerEvent::Screen(message)) => {
                if !sender.send(UiEvent::TabScreen { tab_id, message }) {
                    return Ok(());
                }
            }
            Some(ServerEvent::Control { message, .. }) => {
                let detached = matches!(&message, ServerMessage::Detached { .. });
                let exited = matches!(&message, ServerMessage::SurfaceExited { .. });
                if exited {
                    stop.store(true, Ordering::Release);
                }
                if !sender.send(UiEvent::TabControl { tab_id, message }) {
                    return Ok(());
                }
                if detached && stop.load(Ordering::Acquire) || exited {
                    return Ok(());
                }
            }
            None => thread::sleep(Duration::from_millis(2)),
        }
    }
}

fn terminate_surface_and_wait(
    instance: Option<&str>,
    surface_id: &SurfaceId,
) -> Result<Vec<SurfaceInfo>, String> {
    let mut client = DaemonClient::connect(instance, Duration::from_secs(2))
        .map_err(|error| error.to_string())?;
    let surface = client
        .workspace()
        .map_err(|error| error.to_string())?
        .surface(surface_id)
        .cloned()
        .ok_or_else(|| format!("surface {surface_id} was not found"))?;
    client
        .end_surface(&surface)
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let surfaces = client.list_surfaces().map_err(|error| error.to_string())?;
        let ending = surfaces.iter().any(|surface| {
            &surface.id == surface_id
                && matches!(
                    surface.status,
                    SurfaceStatus::Starting | SurfaceStatus::Running | SurfaceStatus::Ending
                )
        });
        if !ending {
            return Ok(surfaces);
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out ending surface {surface_id}"));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn short_surface_id(id: &SurfaceId) -> String {
    id.as_str()
        .rsplit('-')
        .next()
        .unwrap_or(id.as_str())
        .chars()
        .take(8)
        .collect()
}

fn snapshot_contains_marker(snapshot: Option<&ScreenSnapshot>, marker: &str) -> bool {
    let Some(snapshot) = snapshot else {
        return false;
    };
    snapshot
        .scrollback
        .iter()
        .chain(&snapshot.cells)
        .any(|row| {
            row.cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .contains(marker)
        })
}

fn log_startup_metric(name: &str, elapsed: Duration) {
    perf::log_startup_metric(name, elapsed);
}

#[derive(Default)]
struct PaintMetrics {
    last_frame: Option<Instant>,
    frame_intervals_us: Vec<u64>,
    paint_times_us: Vec<u64>,
}

static PERFORMANCE_METRICS: LazyLock<Option<Mutex<PaintMetrics>>> = LazyLock::new(|| {
    env::var_os("COMPI_PERF_LOG")
        .is_some()
        .then(|| Mutex::new(PaintMetrics::default()))
});

fn performance_metrics() -> Option<&'static Mutex<PaintMetrics>> {
    PERFORMANCE_METRICS.as_ref()
}
fn record_paint_metrics(started_at: Instant) {
    let finished_at = Instant::now();
    let Some(metrics) = performance_metrics() else {
        return;
    };
    let Ok(mut metrics) = metrics.lock() else {
        return;
    };
    if let Some(last_frame) = metrics.last_frame {
        let interval = finished_at.saturating_duration_since(last_frame);
        if interval <= Duration::from_millis(100) {
            metrics.frame_intervals_us.push(interval.as_micros() as u64);
        }
    }
    metrics.last_frame = Some(finished_at);
    metrics.paint_times_us.push(
        finished_at
            .saturating_duration_since(started_at)
            .as_micros() as u64,
    );
    if metrics.paint_times_us.len() < 240 || metrics.frame_intervals_us.len() < 120 {
        return;
    }

    metrics.frame_intervals_us.sort_unstable();
    metrics.paint_times_us.sort_unstable();
    let frame_p50 = percentile(&metrics.frame_intervals_us, 50);
    let frame_p95 = percentile(&metrics.frame_intervals_us, 95);
    let paint_p50 = percentile(&metrics.paint_times_us, 50);
    let paint_p95 = percentile(&metrics.paint_times_us, 95);
    #[cfg(windows)]
    let (private_bytes, handles) = current_process_metrics();
    log_performance_sample(
        frame_p50,
        frame_p95,
        paint_p50,
        paint_p95,
        #[cfg(windows)]
        private_bytes,
        #[cfg(windows)]
        handles,
    );
    metrics.frame_intervals_us.clear();
    metrics.paint_times_us.clear();
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    sorted[(sorted.len().saturating_sub(1) * percentile) / 100]
}

#[cfg(windows)]
fn current_process_metrics() -> (usize, u32) {
    let process = unsafe { GetCurrentProcess() };
    let mut memory = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let memory_result = unsafe {
        GetProcessMemoryInfo(
            process,
            &mut memory as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    };
    let mut handles = 0;
    let handle_result = unsafe { GetProcessHandleCount(process, &mut handles) };
    let private_bytes = if memory_result.is_ok() {
        memory.PrivateUsage
    } else {
        0
    };
    let handles = if handle_result.is_ok() { handles } else { 0 };
    (private_bytes, handles)
}

fn log_performance_sample(
    frame_p50_us: u64,
    frame_p95_us: u64,
    paint_p50_us: u64,
    paint_p95_us: u64,
    #[cfg(windows)] private_bytes: usize,
    #[cfg(windows)] handles: u32,
) {
    let Ok(directory) = compi_server::paths::data_dir() else {
        return;
    };
    if fs::create_dir_all(&directory).is_err() {
        return;
    }
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("client-perf.log"))
    {
        #[cfg(windows)]
        let _ = writeln!(
            file,
            "frame_p50_us={frame_p50_us} frame_p95_us={frame_p95_us} \
             paint_p50_us={paint_p50_us} paint_p95_us={paint_p95_us} \
             private_bytes={private_bytes} handles={handles}"
        );
        #[cfg(target_os = "macos")]
        let _ = writeln!(
            file,
            "frame_p50_us={frame_p50_us} frame_p95_us={frame_p95_us} \
             paint_p50_us={paint_p50_us} paint_p95_us={paint_p95_us}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn starts_tab_drag_only_after_crossing_threshold() {
        let origin = point(px(10.0), px(10.0));

        assert!(!drag_threshold_crossed(origin, point(px(12.0), px(12.0))));
        assert!(drag_threshold_crossed(origin, point(px(14.0), px(10.0))));
    }

    #[test]
    fn decodes_raw_kitty_rgba_for_gpui() {
        let image = KittyImage {
            id: 1,
            format: 32,
            width: 1,
            height: 1,
            data: base64::engine::general_purpose::STANDARD.encode([1, 2, 3, 4]),
        };
        let decoded = decode_kitty_image(&image).unwrap();
        assert_eq!(decoded.as_bytes(0), Some([3, 2, 1, 4].as_slice()));
    }
}
