use crate::client_state::{ClientState, SavedViewport, StateSlot, WindowGeometry};
use crate::commands::{self, Command};
use crate::config::{FontSettings, LoadedConfig};
use crate::input::{
    self, Key, KeypadKey, Modifiers, encode_keystroke, encode_mouse, utf16_byte_index,
};
use crate::layout::{self, LayoutMetrics, WorkspaceLayout};
use crate::probe;
use crate::selection::{GridPoint, Selection, line_selection, selected_text, word_selection};
use crate::theme::{ThemeColors, ThemePreset};
use compi_protocol::{
    LayoutNode, MutationId, MutationRequest, PaneId, SessionId, SplitAxis, TabId,
    WorkspaceMutation, WorkspaceSnapshot, WorkspaceTab,
};
use sha2::{Digest, Sha256};
mod workspace;
use crate::typography::TerminalTypography;
use crate::viewport::{
    hyperlink_at, inherited_working_directory, is_allowed_hyperlink, visible_row, visible_rows,
    visible_to_absolute,
};
use crate::{DaemonClient, MirrorApply, ScreenMirror, ServerEvent};
use base64::Engine as _;
use compi_protocol::perf;
use compi_protocol::{
    Cell, ClientMessage, Color, CursorShape, CursorState, KittyImage, KittyPlacement, MouseMode,
    Row, ScreenMessage, ServerMessage, SurfaceId, SurfaceStatus,
};
use gpui::{
    App, Application, Bounds, ClipboardItem, ContentMask, Context, Corners, ElementInputHandler,
    EntityInputHandler, FocusHandle, Focusable, FontId, FontStyle, FontWeight, GlyphId, Hsla,
    KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathBuilder, Pixels, Point, Render, RenderImage, ScrollHandle, ScrollWheelEvent, SharedString,
    Subscription, TextRun, TitlebarOptions, UTF16Selection, UnderlineStyle, Window, WindowBounds,
    WindowControlArea, WindowOptions, canvas, div, fill, point, prelude::*, px, rgb, size,
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
use workspace::{DividerDrag, Overlay, TransferSeed, open_compi_window};

const DEFAULT_COLS: i16 = 100;
const DEFAULT_ROWS: i16 = 30;
const CHROME_HEIGHT: f32 = 40.0;
const TAB_WIDTH: f32 = 176.0;
#[cfg(windows)]
const WINDOW_CONTROLS_WIDTH: f32 = 138.0;
#[cfg(target_os = "macos")]
const WINDOW_CONTROLS_WIDTH: f32 = 0.0;
#[cfg(windows)]
const TITLEBAR_BRAND_WIDTH: f32 = 80.0;
#[cfg(target_os = "macos")]
const TITLEBAR_BRAND_WIDTH: f32 = 158.0;
#[cfg(windows)]
const UI_FONT: &str = "Segoe UI";
#[cfg(target_os = "macos")]
const UI_FONT: &str = ".SystemUIFont";
const TAB_DRAG_THRESHOLD: f32 = 4.0;
const TERMINAL_PADDING: f32 = 8.0;
const UI_EVENT_BUDGET: Duration = Duration::from_millis(1);
const UI_EVENT_YIELD: Duration = Duration::from_micros(8_333);

pub fn run(
    instance: Option<String>,
    initial_working_directory: Option<String>,
    config: LoadedConfig,
    launch_requests: Option<std::sync::mpsc::Receiver<crate::window_host::LaunchRequest>>,
) {
    Application::new().run(move |cx: &mut App| {
        if let Err(error) = open_compi_window(
            instance.clone(),
            initial_working_directory,
            config,
            None,
            cx,
        ) {
            eprintln!("Could not open Compi: {error}");
            cx.quit();
            return;
        }
        if let Some(requests) = launch_requests {
            cx.spawn(async move |cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(40))
                        .await;
                    loop {
                        let request = match requests.try_recv() {
                            Ok(request) => request,
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => return,
                        };
                        let instance = instance.clone();
                        let _ = cx.update(|cx| {
                            if let Err(error) = open_compi_window(
                                instance,
                                request.initial_working_directory,
                                request.config,
                                None,
                                cx,
                            ) {
                                eprintln!("Could not open Compi window: {error}");
                            }
                        });
                    }
                }
            })
            .detach();
        }
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}

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
    }
}

struct SurfaceView {
    id: u64,
    surface_id: SurfaceId,
    pane_id: PaneId,
    lifetime: compi_protocol::ProcessLifetimeId,
    stop: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    mirror: ScreenMirror,
    state: ConnectionState,
    error: Option<String>,
    transport: Option<TabTransport>,
    scroll_offset: usize,
    selection: Option<Selection>,
    selecting: bool,
    image_cache: HashMap<u32, ([u8; 32], Arc<RenderImage>)>,
    image_pending: HashMap<u32, [u8; 32]>,
    image_rejected: HashMap<u32, [u8; 32]>,
    image_cache_bytes: usize,
    image_error: Option<String>,
    row_render_cache: Arc<Mutex<RowRenderCache>>,
    viewport_fingerprint: Option<(u64, [u8; 32])>,
    cols: i16,
    rows: i16,
}

impl SurfaceView {
    fn title(&self) -> String {
        self.mirror
            .snapshot()
            .map(|snapshot| snapshot.title.trim())
            .filter(|title| !title.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| short_surface_id(&self.surface_id))
    }

    fn send(&mut self, message: ClientMessage) {
        if matches!(message, ClientMessage::Input { .. }) && self.state != ConnectionState::Attached
        {
            return;
        }
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
            return;
        };
        let active_ids: HashSet<u32> = snapshot.images.iter().map(|image| image.id).collect();
        self.image_cache.retain(|id, _| active_ids.contains(id));
        self.image_pending.retain(|id, _| active_ids.contains(id));
        self.image_rejected.retain(|id, _| active_ids.contains(id));
        self.image_cache_bytes = self
            .image_cache
            .values()
            .map(|(_, image)| decoded_image_bytes(image))
            .sum();
        if self.image_rejected.is_empty() {
            self.image_error = None;
        }
        for image in &snapshot.images {
            let mut hash = Sha256::new();
            hash.update(image.format.to_le_bytes());
            hash.update(image.width.to_le_bytes());
            hash.update(image.height.to_le_bytes());
            hash.update(image.data.as_bytes());
            let fingerprint: [u8; 32] = hash.finalize().into();
            if self
                .image_cache
                .get(&image.id)
                .is_some_and(|(cached, _)| *cached == fingerprint)
                || self.image_pending.get(&image.id) == Some(&fingerprint)
                || self.image_rejected.get(&image.id) == Some(&fingerprint)
            {
                continue;
            }
            if let Some((_, previous)) = self.image_cache.remove(&image.id) {
                self.image_cache_bytes = self
                    .image_cache_bytes
                    .saturating_sub(decoded_image_bytes(&previous));
            }
            self.image_pending.remove(&image.id);
            self.image_rejected.remove(&image.id);
            let job = ImageDecodeJob {
                tab_id,
                image: image.clone(),
                fingerprint,
                sender: sender.clone(),
            };
            if IMAGE_DECODERS.try_send(job).is_ok() {
                self.image_pending.insert(image.id, fingerprint);
            } else {
                self.image_rejected.insert(image.id, fingerprint);
                self.image_error = Some(
                    "Image decoding queue is full. Reconnect to retry unchanged images.".into(),
                );
            }
        }
    }

    fn discard_replica(&mut self) {
        self.mirror = ScreenMirror::default();
        self.viewport_fingerprint = None;
        self.selection = None;
        self.scroll_offset = 0;
        self.selecting = false;
        self.image_cache.clear();
        self.image_pending.clear();
        self.image_rejected.clear();
        self.image_cache_bytes = 0;
        self.image_error = None;
        if let Ok(mut cache) = self.row_render_cache.lock() {
            cache.clear();
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
    StateSaveFinished,
    SurfacesLoaded(Result<compi_protocol::WorkspaceSnapshot, String>),
    MutationFinished {
        result: Result<(WorkspaceSnapshot, compi_protocol::MutationReceipt), String>,
        select_created: bool,
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
        data: [u8; 32],
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
        let route = event
            .surface_worker()
            .and_then(|id| EVENT_ROUTES.lock().ok()?.get(&id).cloned());
        route
            .unwrap_or_else(|| self.0.clone())
            .send_blocking(event)
            .is_ok()
    }
}

static NEXT_VIEW_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_MUTATION_ID: AtomicU64 = AtomicU64::new(1);
static MUTATION_RUN_NONCE: LazyLock<u128> = LazyLock::new(|| {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
});
static EVENT_ROUTES: LazyLock<Mutex<HashMap<u64, async_channel::Sender<UiEvent>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

const SURFACE_IMAGE_CACHE_LIMIT: usize = 64 * 1024 * 1024;

struct ImageDecodeJob {
    tab_id: u64,
    image: KittyImage,
    fingerprint: [u8; 32],
    sender: UiEventSender,
}

static IMAGE_DECODERS: LazyLock<mpsc::SyncSender<ImageDecodeJob>> = LazyLock::new(|| {
    let (sender, receiver) = mpsc::sync_channel::<ImageDecodeJob>(8);
    let receiver = Arc::new(Mutex::new(receiver));
    for _ in 0..2 {
        let receiver = receiver.clone();
        thread::spawn(move || {
            loop {
                let job = {
                    let Ok(receiver) = receiver.lock() else {
                        return;
                    };
                    receiver.recv()
                };
                let Ok(job) = job else {
                    return;
                };
                let result = decode_kitty_image(&job.image);
                let _ = job.sender.send(UiEvent::KittyImageDecoded {
                    tab_id: job.tab_id,
                    image_id: job.image.id,
                    data: job.fingerprint,
                    result,
                });
            }
        });
    }
    sender
});

fn decoded_image_bytes(image: &RenderImage) -> usize {
    image
        .as_bytes(0)
        .map_or(SURFACE_IMAGE_CACHE_LIMIT + 1, <[u8]>::len)
}

impl UiEvent {
    fn surface_worker(&self) -> Option<u64> {
        match self {
            Self::TabConnected { tab_id, .. }
            | Self::TabScreen { tab_id, .. }
            | Self::TabControl { tab_id, .. }
            | Self::TabDisconnected { tab_id, .. }
            | Self::KittyImageDecoded { tab_id, .. } => Some(*tab_id),
            _ => None,
        }
    }
}

struct CompiApp {
    started_at: Instant,
    instance: Option<String>,
    initial_working_directory: Option<String>,
    first_snapshot_logged: bool,
    pending_present_latency_ids: Vec<u64>,
    window_title: String,
    focus_handle: FocusHandle,
    ime_text: String,
    ime_marked_range: Option<Range<usize>>,
    ime_selected_range: Range<usize>,
    surface_views: Vec<SurfaceView>,
    focused_view: Option<u64>,
    tab_scroll_handle: ScrollHandle,
    workspace_scroll: ScrollHandle,
    sidebar_scroll: ScrollHandle,
    sidebar_open: bool,
    sidebar_width: f32,
    sidebar_drag: bool,
    workspace_scroll_drag: Option<bool>,
    expanded_workspaces: HashSet<SessionId>,
    tab_drag_origin: Option<Point<Pixels>>,
    dragging_tab: Option<TabId>,
    drag_position: Option<Point<Pixels>>,
    titlebar_drag: bool,
    workspace: Option<WorkspaceSnapshot>,
    state_slot: Option<Arc<Mutex<StateSlot>>>,
    slot_id: String,
    state_writes: Arc<Mutex<(Option<ClientState>, bool)>>,
    state_save_error: Arc<Mutex<Option<String>>>,
    state: ClientState,
    defaults: ClientState,
    config: LoadedConfig,
    theme: ThemePreset,
    glass: bool,
    zoom: f32,
    overlay: Option<Overlay>,
    overlay_index: usize,
    overlay_scroll: ScrollHandle,
    overlay_revision: Option<u64>,
    layout: Option<WorkspaceLayout>,
    divider_drag: Option<DividerDrag>,
    preview_layout: Option<LayoutNode>,
    last_resize: Instant,
    loading_surfaces: bool,
    mutation_pending: bool,
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
    transferred_seed: Option<TransferSeed>,
}

impl CompiApp {
    fn surface_view_mut(&mut self, tab_id: u64) -> Option<&mut SurfaceView> {
        self.surface_views.iter_mut().find(|tab| tab.id == tab_id)
    }

    fn focused_view(&self) -> Option<&SurfaceView> {
        let active = self.focused_view?;
        self.surface_views.iter().find(|tab| tab.id == active)
    }

    fn focused_view_mut(&mut self) -> Option<&mut SurfaceView> {
        let active = self.focused_view?;
        self.surface_views.iter_mut().find(|tab| tab.id == active)
    }
    fn refresh_typography(&mut self, window: &Window) -> bool {
        let scale = window.scale_factor();
        if (scale - self.typography_scale).abs() <= f32::EPSILON {
            return false;
        }
        let typography = Arc::new(TerminalTypography::resolve(
            &self.font_settings,
            self.zoom,
            window,
        ));
        self.global_warning = diagnostic_warning(&self.config_diagnostics, &typography.diagnostics);
        self.typography = typography;
        self.typography_scale = scale;
        for tab in &self.surface_views {
            if let Ok(mut cache) = tab.row_render_cache.lock() {
                cache.clear();
            }
        }
        true
    }

    fn handle_keystroke(&mut self, keystroke: &Keystroke) -> bool {
        if self.overlay.is_some() {
            return true;
        }
        let Some(tab) = self.focused_view_mut() else {
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

    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self
            .focused_view()
            .and_then(|tab| selected_text(tab.mirror.snapshot(), tab.selection))
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let Some(tab) = self.focused_view_mut() else {
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
    fn report_focus(&mut self, focused: bool) {
        let Some(tab) = self.focused_view_mut() else {
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
        let Some(tab) = self.focused_view_mut() else {
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
        let Some(tab) = self.focused_view_mut() else {
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
        let Some(tab) = self.focused_view_mut() else {
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
        self.save_state();
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
        cx.stop_propagation();
        let cell_height = self.typography.cell_height;
        let delta = f32::from(event.delta.pixel_delta(px(cell_height)).y);
        let Some(tab) = self.focused_view_mut() else {
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
            self.save_state();
            cx.notify();
        }
    }
}

#[derive(Clone, Copy)]
enum ChromeIcon {
    Mark,
    Sidebar,
    Add,
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
                }
                ChromeIcon::Sidebar => {
                    path.move_to(point(x(2.5), y(3.0)));
                    path.line_to(point(x(13.5), y(3.0)));
                    path.line_to(point(x(13.5), y(13.0)));
                    path.line_to(point(x(2.5), y(13.0)));
                    path.line_to(point(x(2.5), y(3.0)));
                    path.move_to(point(x(6.0), y(3.0)));
                    path.line_to(point(x(6.0), y(13.0)));
                }
                ChromeIcon::Add => {
                    path.move_to(point(x(3.0), y(8.0)));
                    path.line_to(point(x(13.0), y(8.0)));
                    path.move_to(point(x(8.0), y(3.0)));
                    path.line_to(point(x(8.0), y(13.0)));
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
    colors: &'static ThemeColors,
) -> impl IntoElement {
    div()
        .id(id)
        .h_full()
        .w(px(WINDOW_CONTROLS_WIDTH / 3.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .text_color(color(colors.muted))
        .hover(move |style| {
            style
                .bg(if destructive {
                    color(colors.error)
                } else {
                    color(colors.surface_hover)
                })
                .text_color(color(colors.foreground))
        })
        .window_control_area(area)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| match area {
            WindowControlArea::Min => window.minimize_window(),
            WindowControlArea::Max => toggle_window_maximized(window),
            WindowControlArea::Close => {
                if let Some(view) = window.root::<CompiApp>().flatten() {
                    view.update(cx, |this, _| this.flush_state());
                }
                window.remove_window();
            }
            WindowControlArea::Drag => {}
        })
        .child(chrome_icon(icon, color(colors.muted)))
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
        if self.overlay.is_some() {
            self.ime_marked_range = None;
            cx.notify();
        } else {
            let text = std::mem::take(&mut self.ime_text);
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlay.is_some() {
            self.edit_overlay_text(range, text);
            cx.notify();
            return;
        }
        self.ime_text.clear();
        self.ime_marked_range = None;
        self.ime_selected_range = 0..0;
        if !text.is_empty()
            && let Some(tab) = self.focused_view_mut()
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
        if self.overlay.is_some() {
            return Some(Bounds::new(bounds.origin, size(px(2.0), px(24.0))));
        }
        let cursor = self
            .focused_view()
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
        self.render_workspace_window(window, cx)
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
    theme: ThemePreset,
    focused: bool,
}

impl PaintModel {
    fn from_tab(
        tab: &SurfaceView,
        typography: Arc<TerminalTypography>,
        theme: ThemePreset,
        focused: bool,
    ) -> Option<Self> {
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
            theme,
            focused,
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
            paint_row_backgrounds(bounds, row_index, row, typography, model.theme, window);
        }
        if let Some(selection) = model.selection {
            paint_selection(
                bounds,
                selection,
                base,
                visible.len(),
                typography,
                model.theme,
                window,
            );
        }
        paint_cursor(bounds, model, base, typography, window);
        for (row_index, row) in visible.iter().enumerate() {
            paint_row_text(
                bounds,
                row_index,
                row,
                &model.row_render_cache,
                typography,
                model.theme,
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
    theme: ThemePreset,
    window: &mut Window,
) {
    let mut start = 0;
    while start < row.cells.len() {
        let background = effective_colors(&row.cells[start], theme).1;
        let mut end = start + 1;
        while end < row.cells.len() && effective_colors(&row.cells[end], theme).1 == background {
            end += 1;
        }
        if background != color(theme.colors().background) {
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
    theme: ThemePreset,
    window: &mut Window,
) {
    let fingerprint = row_fingerprint(row);
    let cached = cache
        .lock()
        .ok()
        .and_then(|cache| cache.get(fingerprint, row));
    let runs = cached.unwrap_or_else(|| {
        let shaped = Arc::new(shape_row(row, typography, theme, window));
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
    theme: ThemePreset,
    window: &mut Window,
    cx: &mut App,
) {
    let run = TextRun {
        len: text.len(),
        font: typography.font.clone(),
        color: color(theme.colors().foreground),
        background_color: None,
        underline: Some(UnderlineStyle {
            color: Some(color(theme.colors().accent)),
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
        color(theme.colors().background),
    ));
    let _ = line.paint(origin, px(typography.cell_height), window, cx);
}

fn shape_row(
    row: &Row,
    typography: &TerminalTypography,
    theme: ThemePreset,
    window: &mut Window,
) -> Vec<ShapedRun> {
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
        let (foreground, _) = effective_colors(style, theme);
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
    theme: ThemePreset,
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
            color(theme.colors().selection).opacity(0.82),
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
    if !model.focused || !model.cursor.visible || model.scroll_offset != 0 {
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
    window.paint_quad(fill(
        cursor_bounds,
        color(model.theme.colors().cursor).opacity(0.78),
    ));
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

fn effective_colors(cell: &Cell, theme: ThemePreset) -> (Hsla, Hsla) {
    let foreground = terminal_color(cell.foreground, true, theme);
    let background = terminal_color(cell.background, false, theme);
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

fn terminal_color(value: Color, foreground: bool, theme: ThemePreset) -> Hsla {
    match value {
        Color::Default => color(if foreground {
            theme.colors().foreground
        } else {
            theme.colors().background
        }),
        Color::Rgb(red, green, blue) => {
            color((u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue))
        }
        Color::Indexed(index) => color(theme.colors().indexed(index)),
    }
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
    const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
    let decoded_size = (image.width as usize)
        .checked_mul(image.height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes <= MAX_IMAGE_BYTES)
        .ok_or("Image dimensions exceed the 64 MiB decoded image limit")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&image.data)
        .map_err(|error| error.to_string())?;
    let mut buffer = match image.format {
        32 => RgbaImage::from_raw(image.width, image.height, bytes)
            .ok_or("RGBA payload length does not match dimensions")?,
        24 => {
            let expected = decoded_size / 4 * 3;
            if bytes.len() != expected {
                return Err("RGB payload length does not match dimensions".into());
            }
            let mut rgba = Vec::with_capacity(decoded_size);
            for pixel in bytes.as_chunks::<3>().0 {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
            }
            RgbaImage::from_raw(image.width, image.height, rgba)
                .ok_or("Could not create RGB image")?
        }
        100 => {
            let (width, height) = image::ImageReader::new(std::io::Cursor::new(bytes.as_slice()))
                .with_guessed_format()
                .map_err(|error| error.to_string())?
                .into_dimensions()
                .map_err(|error| error.to_string())?;
            if u64::from(width) * u64::from(height) * 4 > MAX_IMAGE_BYTES as u64 {
                return Err("PNG dimensions exceed the 64 MiB decoded image limit".into());
            }
            let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
                .with_guessed_format()
                .map_err(|error| error.to_string())?;
            let mut limits = image::Limits::default();
            limits.max_alloc = Some(MAX_IMAGE_BYTES as u64);
            limits.max_image_width = Some(8192);
            limits.max_image_height = Some(8192);
            reader.limits(limits);
            reader
                .decode()
                .map_err(|error| error.to_string())?
                .into_rgba8()
        }
        format => return Err(format!("Unsupported Kitty image format {format}")),
    };
    let raw: &mut [u8] = buffer.as_mut();
    for pixel in raw.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new(smallvec![ImageFrame::new(
        buffer
    )])))
}
struct WorkerLifecycle {
    stop: Arc<AtomicBool>,
    previous_closed: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
}

fn spawn_tab_worker(
    tab_id: u64,
    surface_id: SurfaceId,
    cols: i16,
    rows: i16,
    sender: UiEventSender,
    instance: Option<String>,
    lifecycle: WorkerLifecycle,
) {
    let WorkerLifecycle {
        stop,
        previous_closed,
        closed,
    } = lifecycle;
    thread::spawn(move || {
        while !previous_closed.load(Ordering::Acquire) {
            if stop.load(Ordering::Acquire) {
                closed.store(true, Ordering::Release);
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        if stop.load(Ordering::Acquire) {
            closed.store(true, Ordering::Release);
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
        closed.store(true, Ordering::Release);
        if stop.load(Ordering::Acquire) {
            return;
        }
        let error = result
            .err()
            .map(|error| error.to_string())
            .unwrap_or_else(|| "Daemon connection closed".into());
        let _ = sender.send(UiEvent::TabDisconnected { tab_id, error });
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
    if surface.status == SurfaceStatus::Lost {
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
    if stop.load(Ordering::Acquire) {
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
        if stop.load(Ordering::Acquire) {
            // A successor waits for this worker, so acknowledge release before
            // allowing another connection to request this surface's controller.
            let _ = reader.request(ClientMessage::Detach)?;
            return Ok(());
        }
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
                if !sender.send(UiEvent::TabControl { tab_id, message }) {
                    return Ok(());
                }
                if detached && stop.load(Ordering::Acquire) {
                    return Ok(());
                }
            }
            None => thread::sleep(Duration::from_millis(2)),
        }
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
    let Ok(directory) = compi_protocol::paths::data_dir() else {
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
