use crate::app;
use crate::client::{DaemonClient, ServerEvent};
use crate::protocol::{ClientMessage, ServerMessage, SessionInfo, SessionStatus};
use crate::terminal::{
    Cell, Color, CursorShape, CursorState, KittyImage, KittyPlacement, MirrorApply, MouseMode, Row,
    ScreenMessage, ScreenMirror, ScreenSnapshot,
};
use crate::theme::{
    ACCENT, BACKGROUND, BORDER, ERROR, FOREGROUND, MUTED, SELECTION, SURFACE, SURFACE_HOVER,
};
use base64::Engine as _;
use gpui::{
    App, Application, Bounds, ClipboardItem, ContentMask, Context, Corners, ElementInputHandler,
    EntityInputHandler, FocusHandle, Focusable, FontStyle, FontWeight, Hsla, KeyBinding,
    KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathBuilder, Pixels, Point, Render, RenderImage, ScrollHandle, ScrollWheelEvent, ShapedLine,
    SharedString, StrikethroughStyle, Subscription, TextRun, TitlebarOptions, UTF16Selection,
    UnderlineStyle, Window, WindowBounds, WindowControlArea, WindowOptions, actions, canvas, div,
    fill, font, point, prelude::*, px, rgb, size,
};
use image::{Frame as ImageFrame, RgbaImage};
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
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, VK_ADD, VK_DECIMAL, VK_DIVIDE, VK_MULTIPLY, VK_NUMPAD0,
    VK_NUMPAD1, VK_NUMPAD2, VK_NUMPAD3, VK_NUMPAD4, VK_NUMPAD5, VK_NUMPAD6, VK_NUMPAD7, VK_NUMPAD8,
    VK_NUMPAD9, VK_SUBTRACT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HTCAPTION, PostMessageW, SW_RESTORE, ShowWindowAsync, WM_NCLBUTTONDOWN,
};

const DEFAULT_COLS: i16 = 100;
const DEFAULT_ROWS: i16 = 30;
const CELL_WIDTH: f32 = 8.45;
const CELL_HEIGHT: f32 = 18.0;
const CHROME_HEIGHT: f32 = 40.0;
const TAB_WIDTH: f32 = 176.0;
const WINDOW_CONTROLS_WIDTH: f32 = 138.0;
const TITLEBAR_BRAND_WIDTH: f32 = 40.0;
const NEW_TAB_WIDTH: f32 = 40.0;
const SESSION_SWITCHER_WIDTH: f32 = 40.0;
const TITLEBAR_DRAG_WIDTH: f32 = 16.0;
const TITLEBAR_ACTIONS_WIDTH: f32 = NEW_TAB_WIDTH + SESSION_SWITCHER_WIDTH + TITLEBAR_DRAG_WIDTH;
const TAB_DRAG_THRESHOLD: f32 = 4.0;
const TERMINAL_PADDING: f32 = 8.0;
const RECONNECT_DELAY: Duration = Duration::from_millis(350);
const UI_EVENT_BUDGET: Duration = Duration::from_millis(1);
const UI_EVENT_YIELD: Duration = Duration::from_micros(8_333);

pub fn run(instance: Option<String>, initial_working_directory: Option<String>) {
    let started_at = Instant::now();
    let application = Application::new();
    log_startup_metric("application_created_ms", started_at.elapsed());
    application.run(move |cx: &mut App| {
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

        let bounds = Bounds::centered(None, size(px(960.0), px(640.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Compi".into()),
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    focus: true,
                    ..Default::default()
                },
                move |window, cx| {
                    cx.new(|cx| {
                        CompiApp::new(started_at, instance, initial_working_directory, window, cx)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GridPoint {
    row: usize,
    col: usize,
}

#[derive(Clone, Copy, Debug)]
struct Selection {
    anchor: GridPoint,
    head: GridPoint,
}

impl Selection {
    fn ordered(self) -> (GridPoint, GridPoint) {
        if (self.anchor.row, self.anchor.col) <= (self.head.row, self.head.col) {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CtrlCBehavior {
    Copy(String),
    Interrupt,
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
            crate::perf::log_input_latency_stage(latency_id, "client_queued", None);
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
    session_id: String,
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
            .unwrap_or_else(|| short_session_id(&self.session_id))
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
    SessionsLoaded(Result<Vec<SessionInfo>, String>),
    SessionCreated(Result<SessionInfo, String>),
    SessionEndFinished {
        session_id: String,
        result: Result<Vec<SessionInfo>, String>,
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
    sessions: Vec<SessionInfo>,
    switcher_open: bool,
    end_confirmation: Option<String>,
    ending_sessions: HashSet<String>,
    close_after_end: HashSet<String>,
    loading_sessions: bool,
    attach_after_session_list: bool,
    global_error: Option<String>,
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (event_tx, event_rx) = async_channel::unbounded();
        let empty_window = crate::perf::empty_window_enabled();
        let ready_probe_marker = crate::perf::ready_probe_enabled()
            .then(|| format!("COMPI_READY_{}", std::process::id()));
        let perf_target_sessions = crate::perf::target_session_count();
        let event_tx = UiEventSender(event_tx);
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
            sessions: Vec::new(),
            switcher_open: false,
            end_confirmation: None,
            ending_sessions: HashSet::new(),
            close_after_end: HashSet::new(),
            loading_sessions: !empty_window,
            attach_after_session_list: false,
            global_error: None,
            event_tx,
            subscriptions: Vec::new(),
            terminal_cols: DEFAULT_COLS,
            terminal_rows: DEFAULT_ROWS,
        };

        this.update_dimensions(window);
        if !empty_window {
            this.refresh_sessions(true);
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
        if crate::perf::enabled() {
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
                            crate::perf::log_resource_sample("client", workload, this.tabs.len());
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

    fn refresh_sessions(&mut self, attach_initial: bool) {
        self.loading_sessions = true;
        self.attach_after_session_list |= attach_initial;
        let sender = self.event_tx.clone();
        let instance = self.instance.clone();
        thread::spawn(move || {
            let result = app::connect_or_start(instance.as_deref())
                .and_then(|mut client| client.list_sessions())
                .map_err(|error| error.to_string());
            let _ = sender.send(UiEvent::SessionsLoaded(result));
        });
    }

    fn create_session(&mut self, working_directory: Option<String>) {
        let sender = self.event_tx.clone();
        let cols = self.terminal_cols;
        let rows = self.terminal_rows;
        let instance = self.instance.clone();
        thread::spawn(move || {
            let result = app::connect_or_start(instance.as_deref())
                .and_then(|mut client| client.create_session(cols, rows, working_directory))
                .map_err(|error| error.to_string());
            let _ = sender.send(UiEvent::SessionCreated(result));
        });
    }

    fn create_inherited_session(&mut self) {
        let working_directory =
            inherited_working_directory(self.active_tab().and_then(|tab| tab.mirror.snapshot()));
        self.create_session(working_directory);
    }

    fn create_next_perf_session(&mut self) {
        if self.tabs.len() < self.perf_target_sessions {
            self.create_session(None);
        }
    }

    fn begin_end_session(&mut self, session_id: String) {
        if !self.ending_sessions.contains(&session_id)
            && self
                .sessions
                .iter()
                .any(|session| session.id == session_id && session.status == SessionStatus::Running)
        {
            self.end_confirmation = Some(session_id);
        }
    }

    fn cancel_end_session(&mut self, session_id: &str) {
        if self.end_confirmation.as_deref() == Some(session_id) {
            self.end_confirmation = None;
        }
    }

    fn confirm_end_session(&mut self, session_id: String) {
        if self.ending_sessions.contains(&session_id)
            || !self
                .sessions
                .iter()
                .any(|session| session.id == session_id && session.status == SessionStatus::Running)
        {
            return;
        }
        self.end_confirmation = None;
        self.ending_sessions.insert(session_id.clone());
        if self.tabs.iter().any(|tab| tab.session_id == session_id) {
            self.close_after_end.insert(session_id.clone());
        }
        let sender = self.event_tx.clone();
        let instance = self.instance.clone();
        thread::spawn(move || {
            let result = terminate_session_and_wait(instance.as_deref(), &session_id);
            let _ = sender.send(UiEvent::SessionEndFinished { session_id, result });
        });
    }

    fn attach_session(&mut self, session: SessionInfo) {
        if let Some(tab_id) = self
            .tabs
            .iter()
            .find(|tab| tab.session_id == session.id)
            .map(|tab| tab.id)
        {
            self.active_tab = Some(tab_id);
            self.reveal_tab(tab_id);
            self.switcher_open = false;
            return;
        }
        if session.status != SessionStatus::Running || session.attached {
            return;
        }
        let tab_id = self.next_tab_id;
        self.next_tab_id = self.next_tab_id.saturating_add(1);
        self.tabs.push(TerminalTab {
            id: tab_id,
            session_id: session.id.clone(),
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
            session.id,
            self.terminal_cols,
            self.terminal_rows,
            self.event_tx.clone(),
            self.instance.clone(),
        );
    }

    fn handle_event(&mut self, event: UiEvent, cx: &mut Context<Self>) {
        match event {
            UiEvent::SessionsLoaded(Ok(sessions)) => {
                self.global_error = None;
                self.loading_sessions = false;
                let needs_initial_tab = self.attach_after_session_list && self.tabs.is_empty();
                self.attach_after_session_list = false;
                self.sessions = sessions;
                if needs_initial_tab {
                    if let Some(working_directory) = self.initial_working_directory.take() {
                        self.create_session(Some(working_directory));
                    } else if let Some(session) = self
                        .sessions
                        .iter()
                        .find(|session| {
                            session.status == SessionStatus::Running && !session.attached
                        })
                        .cloned()
                    {
                        self.attach_session(session);
                        self.create_next_perf_session();
                    } else {
                        self.create_session(None);
                    }
                }
            }
            UiEvent::SessionsLoaded(Err(error)) => {
                self.loading_sessions = false;
                self.global_error = Some(error);
            }
            UiEvent::SessionCreated(Ok(session)) => {
                self.global_error = None;
                self.sessions.push(session.clone());
                self.attach_session(session);
                self.create_next_perf_session();
            }
            UiEvent::SessionCreated(Err(error)) => self.global_error = Some(error),
            UiEvent::SessionEndFinished { session_id, result } => {
                self.ending_sessions.remove(&session_id);
                match result {
                    Ok(sessions) => {
                        self.global_error = None;
                        self.sessions = sessions;
                        if !self.tabs.iter().any(|tab| tab.session_id == session_id) {
                            self.close_after_end.remove(&session_id);
                        }
                    }
                    Err(error) => {
                        self.close_after_end.remove(&session_id);
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
                    crate::perf::log_startup_metric(
                        "first_terminal_frame_ms",
                        self.started_at.elapsed(),
                    );
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
                ServerMessage::SessionExited {
                    session_id,
                    exit_code,
                } => {
                    self.ending_sessions.remove(&session_id);
                    if self.close_after_end.remove(&session_id) {
                        self.close_tab_id(tab_id);
                    } else if let Some(tab) = self.tab_mut(tab_id) {
                        tab.state = ConnectionState::Exited(exit_code);
                        tab.transport = None;
                    }
                    self.refresh_sessions(false);
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
        let width = (f32::from(viewport.width) - TERMINAL_PADDING * 2.0).max(CELL_WIDTH);
        let height =
            (f32::from(viewport.height) - CHROME_HEIGHT - TERMINAL_PADDING * 2.0).max(CELL_HEIGHT);
        let cols = (width / CELL_WIDTH).floor().clamp(2.0, i16::MAX as f32) as i16;
        let rows = (height / CELL_HEIGHT).floor().clamp(1.0, i16::MAX as f32) as i16;
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

    fn handle_keystroke(&mut self, keystroke: &Keystroke) {
        if self.switcher_open || is_application_shortcut(keystroke) {
            return;
        }
        let Some(tab) = self.active_tab_mut() else {
            return;
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
        if let Some(bytes) = encode_keystroke(keystroke, application_cursor, keypad) {
            tab.scroll_offset = 0;
            let latency_id = crate::perf::begin_input_latency();
            if let Some(latency_id) = latency_id {
                crate::perf::log_input_latency_stage(latency_id, "key_receipt", None);
            }
            tab.send(ClientMessage::Input {
                data: bytes,
                latency_id,
            });
        }
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
            self.refresh_sessions(false);
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
        match self.active_tab().map(ctrl_c_behavior) {
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
        if let Some(text) = self.active_tab().and_then(selected_text) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste_clipboard(&mut self, _: &PasteClipboard, _: &mut Window, cx: &mut Context<Self>) {
        let Some(mut text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        text = text.replace("\r\n", "\n").replace('\n', "\r");
        let bracketed = tab
            .mirror
            .snapshot()
            .is_some_and(|snapshot| snapshot.modes.bracketed_paste);
        let data = if bracketed {
            let mut data = b"\x1b[200~".to_vec();
            data.extend_from_slice(text.as_bytes());
            data.extend_from_slice(b"\x1b[201~");
            data
        } else {
            text.into_bytes()
        };
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
            self.refresh_sessions(false);
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
                data: if focused {
                    b"\x1b[I".to_vec()
                } else {
                    b"\x1b[O".to_vec()
                },
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
                Some(event.button),
                false,
                false,
                point.col,
                visible_row(tab, point.row),
                event.modifiers,
            ) {
                tab.send(ClientMessage::Input {
                    data,
                    latency_id: None,
                });
            }
            return;
        }
        let Some(absolute) = visible_to_absolute(tab, point) else {
            return;
        };
        tab.selection = match event.click_count {
            2 => word_selection(tab, absolute),
            count if count >= 3 => line_selection(tab, absolute),
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
        let report_motion = match mouse_mode {
            MouseMode::None | MouseMode::Normal => false,
            MouseMode::ButtonMotion => event.pressed_button.is_some(),
            MouseMode::AnyMotion => true,
        };
        if report_motion && !event.modifiers.shift {
            if let Some(data) = encode_mouse(
                event.pressed_button,
                false,
                true,
                point.col,
                visible_row(tab, point.row),
                event.modifiers,
            ) {
                tab.send(ClientMessage::Input {
                    data,
                    latency_id: None,
                });
            }
        } else if tab.selecting
            && event.dragging()
            && let Some(absolute) = visible_to_absolute(tab, point)
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
                Some(event.button),
                true,
                false,
                point.col,
                visible_row(tab, point.row),
                event.modifiers,
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
                    .then(|| hyperlink_at(tab, selection.head))
                    .flatten()
                    .filter(|uri| is_allowed_hyperlink(uri))
            } else {
                None
            };
            if let Some(uri) = clicked_link {
                cx.open_url(&uri);
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
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let delta = f32::from(event.delta.pixel_delta(px(CELL_HEIGHT)).y);
        let mouse_mode = tab
            .mirror
            .snapshot()
            .map(|snapshot| snapshot.modes.mouse)
            .unwrap_or_default();
        if mouse_mode != MouseMode::None && !event.modifiers.shift {
            let button_code = if delta > 0.0 { 64 } else { 65 };
            let data = encode_sgr_mouse_code(
                button_code,
                point.col,
                visible_row(tab, point.row),
                event.modifiers,
                true,
            );
            tab.send(ClientMessage::Input {
                data,
                latency_id: None,
            });
        } else {
            let lines = (delta.abs() / CELL_HEIGHT).ceil().max(1.0) as usize;
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
        let col = (x / CELL_WIDTH).floor() as usize;
        let row = (y / CELL_HEIGHT).floor() as usize;
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
                    .left_0()
                    .w(px(TITLEBAR_BRAND_WIDTH))
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
                            this.refresh_sessions(false);
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
            .child(
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
    }

    fn render_switcher(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.sessions.iter().map(|session| {
            let session = session.clone();
            let ending = self.ending_sessions.contains(&session.id);
            let confirming = self.end_confirmation.as_deref() == Some(session.id.as_str());
            let can_select = session.status == SessionStatus::Running && !ending;
            let status = if ending {
                "Ending…"
            } else {
                match session.status {
                    SessionStatus::Starting => "Starting",
                    SessionStatus::Running if session.attached => "Open",
                    SessionStatus::Running => "Detached",
                    SessionStatus::Exited => "Exited",
                    SessionStatus::Failed => "Failed",
                    SessionStatus::Dead => "Dead",
                }
            };
            let detail = (session.status == SessionStatus::Dead)
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
                            this.attach_session(session_for_attach.clone());
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
                                        .when(session.status == SessionStatus::Running, |title| {
                                            title.child(div().size(px(5.0)).rounded_full().bg(
                                                color(if session.attached {
                                                    ACCENT
                                                } else {
                                                    MUTED
                                                }),
                                            ))
                                        })
                                        .child(short_session_id(&session.id)),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_sm()
                                        .text_color(
                                            if matches!(
                                                session.status,
                                                SessionStatus::Failed | SessionStatus::Dead
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
                            session.status == SessionStatus::Running && !ending,
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
                                                        this.begin_end_session(
                                                            session_id_for_begin.clone(),
                                                        );
                                                        cx.stop_propagation();
                                                        cx.notify();
                                                    }))
                                                    .child("End session"),
                                            )
                                        })
                                        .when(confirming, |actions| {
                                            actions
                                                .child(
                                                    div()
                                                        .text_size(px(11.0))
                                                        .text_color(color(MUTED))
                                                        .child("End this session?"),
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
                                                                this.cancel_end_session(
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
                                                                this.confirm_end_session(
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
                                    .child("Switch session"),
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
                    .when(self.loading_sessions, |panel| {
                        panel.child(
                            div()
                                .p_4()
                                .text_color(color(MUTED))
                                .child("Loading sessions…"),
                        )
                    })
                    .children(rows),
            )
    }

    fn render_terminal(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paint = self.active_tab().and_then(PaintModel::from_tab);
        let status = self.active_tab().map(|tab| tab.state);
        let error = self
            .active_tab()
            .and_then(|tab| tab.error.clone())
            .or_else(|| self.global_error.clone());
        let warning = error
            .is_none()
            .then(|| {
                let session_id = self.active_tab().map(|tab| tab.session_id.as_str())?;
                self.sessions
                    .iter()
                    .find(|session| session.id == session_id)?
                    .working_directory
                    .as_ref()?
                    .warning
                    .clone()
            })
            .flatten();
        let input = cx.entity();
        let input_focus = self.focus_handle.clone();
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
                            paint_terminal(bounds, &paint, window, cx);
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

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.ime_marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        _: &mut Context<Self>,
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
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        let replace = range
            .or_else(|| self.ime_marked_range.clone())
            .unwrap_or_else(|| self.ime_selected_range.clone());
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
        Some(Bounds::new(
            point(
                bounds.left() + px(f32::from(cursor.col) * CELL_WIDTH),
                bounds.top() + px(f32::from(cursor.row) * CELL_HEIGHT),
            ),
            size(px(CELL_WIDTH), px(CELL_HEIGHT)),
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
                crate::perf::log_startup_metric("ready_for_input_ms", started_at.elapsed());
                if let Some(sent_at) = sent_at {
                    crate::perf::log_startup_metric("input_to_render_ms", sent_at.elapsed());
                }
            });
        }
        if !self.pending_present_latency_ids.is_empty() {
            let latency_ids = std::mem::take(&mut self.pending_present_latency_ids);
            window.on_next_frame(move |_, _| {
                for latency_id in latency_ids {
                    crate::perf::log_input_latency_stage(latency_id, "frame_presented", None);
                }
            });
        }
        div()
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                this.handle_keystroke(&event.keystroke);
                cx.stop_propagation();
            }))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .font_family("Segoe UI")
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
}

impl PaintModel {
    fn from_tab(tab: &TerminalTab) -> Option<Self> {
        let snapshot = tab.mirror.snapshot()?;
        let (visible_rows, base_row) = visible_rows(snapshot, tab.scroll_offset);
        Some(Self {
            visible_rows: visible_rows.into_iter().cloned().collect(),
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
        })
    }
}

fn paint_terminal(bounds: Bounds<Pixels>, model: &PaintModel, window: &mut Window, cx: &mut App) {
    let paint_started_at = performance_metrics().map(|_| Instant::now());
    let visible = &model.visible_rows;
    let base = model.base_row;
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        paint_images(bounds, model, base, false, window);
        for (row_index, row) in visible.iter().enumerate() {
            paint_row_backgrounds(bounds, row_index, row, window);
        }
        if let Some(selection) = model.selection {
            paint_selection(bounds, selection, base, visible.len(), window);
        }
        paint_cursor(bounds, model, base, window);
        for (row_index, row) in visible.iter().enumerate() {
            paint_row_text(bounds, row_index, row, &model.row_render_cache, window, cx);
        }
        paint_images(bounds, model, base, true, window);
    });
    if let Some(started_at) = paint_started_at {
        record_paint_metrics(started_at);
    }
}

fn paint_row_backgrounds(bounds: Bounds<Pixels>, row_index: usize, row: &Row, window: &mut Window) {
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
                        bounds.left() + px(start as f32 * CELL_WIDTH),
                        bounds.top() + px(row_index as f32 * CELL_HEIGHT),
                    ),
                    size(px((end - start) as f32 * CELL_WIDTH), px(CELL_HEIGHT)),
                ),
                background,
            ));
        }
        start = end;
    }
}

#[derive(Clone)]
struct ShapedRun {
    start: usize,
    line: ShapedLine,
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
    window: &mut Window,
    cx: &mut App,
) {
    let fingerprint = row_fingerprint(row);
    let cached = cache
        .lock()
        .ok()
        .and_then(|cache| cache.get(fingerprint, row));
    let runs = cached.unwrap_or_else(|| {
        let shaped = Arc::new(shape_row(row, window));
        if let Ok(mut cache) = cache.lock() {
            cache.insert(fingerprint, row.clone(), shaped.clone());
        }
        shaped
    });
    for run in runs.iter() {
        let origin = point(
            bounds.left() + px(run.start as f32 * CELL_WIDTH),
            bounds.top() + px(row_index as f32 * CELL_HEIGHT),
        );
        let _ = run.line.paint(origin, px(CELL_HEIGHT), window, cx);
    }
}

fn shape_row(row: &Row, window: &mut Window) -> Vec<ShapedRun> {
    let mut shaped = Vec::new();
    let mut start = 0;
    while start < row.cells.len() {
        if row.cells[start].width == 0 {
            start += 1;
            continue;
        }
        let style = &row.cells[start];
        let mut end = start + usize::from(style.width.max(1));
        let mut text = if style.attributes.hidden {
            " ".repeat(usize::from(style.width.max(1)))
        } else {
            style.text.to_string()
        };
        while end < row.cells.len() {
            let cell = &row.cells[end];
            if cell.width == 0 || !same_text_style(style, cell) {
                if cell.width == 0 {
                    end += 1;
                    continue;
                }
                break;
            }
            text.push_str(if cell.attributes.hidden {
                " "
            } else {
                &cell.text
            });
            end += usize::from(cell.width.max(1));
        }
        let (foreground, _) = effective_colors(style);
        let mut terminal_font = font("Cascadia Mono");
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
        let underline =
            (style.attributes.underline || style.hyperlink.is_some()).then_some(UnderlineStyle {
                color: Some(foreground),
                thickness: px(1.0),
                wavy: false,
            });
        let strikethrough = style.attributes.strike.then_some(StrikethroughStyle {
            color: Some(foreground),
            thickness: px(1.0),
        });
        let run = TextRun {
            len: text.len(),
            font: terminal_font,
            color: if style.attributes.dim {
                foreground.opacity(0.58)
            } else {
                foreground
            },
            background_color: None,
            underline,
            strikethrough,
        };
        shaped.push(ShapedRun {
            start,
            line: window
                .text_system()
                .shape_line(SharedString::from(text), px(14.0), &[run], None),
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
        let last_col =
            last_col.min(((f32::from(bounds.size.width) / CELL_WIDTH).floor() as usize).max(1));
        if last_col <= first_col {
            continue;
        }
        window.paint_quad(fill(
            Bounds::new(
                point(
                    bounds.left() + px(first_col as f32 * CELL_WIDTH),
                    bounds.top() + px(visible_row as f32 * CELL_HEIGHT),
                ),
                size(
                    px((last_col - first_col) as f32 * CELL_WIDTH),
                    px(CELL_HEIGHT),
                ),
            ),
            color(SELECTION).opacity(0.82),
        ));
    }
}

fn paint_cursor(bounds: Bounds<Pixels>, model: &PaintModel, base: usize, window: &mut Window) {
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
        bounds.left() + px(col as f32 * CELL_WIDTH),
        bounds.top() + px(row as f32 * CELL_HEIGHT),
    );
    let cursor_bounds = match model.cursor.shape {
        CursorShape::Block => Bounds::new(point(x, y), size(px(CELL_WIDTH), px(CELL_HEIGHT))),
        CursorShape::Underline => Bounds::new(
            point(x, y + px(CELL_HEIGHT - 2.0)),
            size(px(CELL_WIDTH), px(2.0)),
        ),
        CursorShape::Bar => Bounds::new(point(x, y), size(px(2.0), px(CELL_HEIGHT))),
    };
    window.paint_quad(fill(cursor_bounds, color(ACCENT).opacity(0.78)));
}

fn paint_images(
    bounds: Bounds<Pixels>,
    model: &PaintModel,
    base: usize,
    foreground: bool,
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
            ((image.size(0).height.0 as f32 / CELL_HEIGHT).ceil() as u16).max(1)
        });
        let cols = placement
            .cols
            .unwrap_or_else(|| ((image.size(0).width.0 as f32 / CELL_WIDTH).ceil() as u16).max(1));
        let image_bounds = Bounds::new(
            point(
                bounds.left() + px(f32::from(placement.col) * CELL_WIDTH),
                bounds.top() + px(visible_row as f32 * CELL_HEIGHT),
            ),
            size(
                px(f32::from(cols) * CELL_WIDTH),
                px(f32::from(rows) * CELL_HEIGHT),
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
fn inherited_working_directory(snapshot: Option<&ScreenSnapshot>) -> Option<String> {
    snapshot.and_then(|snapshot| snapshot.current_directory.clone())
}

fn color(value: u32) -> Hsla {
    rgb(value).into()
}

fn visible_rows(snapshot: &ScreenSnapshot, scroll_offset: usize) -> (Vec<&Row>, usize) {
    let mut all = Vec::with_capacity(snapshot.scrollback.len() + snapshot.cells.len());
    all.extend(snapshot.scrollback.iter());
    all.extend(snapshot.cells.iter());
    let end = all
        .len()
        .saturating_sub(scroll_offset.min(snapshot.scrollback.len()));
    let start = end.saturating_sub(snapshot.cells.len());
    (all[start..end].to_vec(), start)
}

fn visible_to_absolute(tab: &TerminalTab, point: GridPoint) -> Option<GridPoint> {
    let snapshot = tab.mirror.snapshot()?;
    let (_, base) = visible_rows(snapshot, tab.scroll_offset);
    Some(GridPoint {
        row: base + point.row,
        col: point.col.min(snapshot.cols.saturating_sub(1) as usize),
    })
}

fn hyperlink_at(tab: &TerminalTab, point: GridPoint) -> Option<String> {
    let snapshot = tab.mirror.snapshot()?;
    snapshot
        .scrollback
        .iter()
        .chain(&snapshot.cells)
        .nth(point.row)?
        .cells
        .get(point.col)?
        .hyperlink
        .as_ref()
        .map(ToString::to_string)
}

fn is_allowed_hyperlink(uri: &str) -> bool {
    if uri
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return false;
    }
    let lowercase = uri.to_ascii_lowercase();
    ["https://", "http://"].into_iter().any(|prefix| {
        lowercase
            .strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
    })
}

fn visible_row(tab: &TerminalTab, row: usize) -> usize {
    row.min(tab.rows.saturating_sub(1) as usize)
}

fn selected_text(tab: &TerminalTab) -> Option<String> {
    let selection = tab.selection?;
    let snapshot = tab.mirror.snapshot()?;
    let mut rows = Vec::with_capacity(snapshot.scrollback.len() + snapshot.cells.len());
    rows.extend(snapshot.scrollback.iter());
    rows.extend(snapshot.cells.iter());
    let (start, end) = selection.ordered();
    if start == end || start.row >= rows.len() {
        return None;
    }
    let mut result = String::new();
    let last_row = end.row.min(rows.len() - 1);
    for (row_index, row) in rows.iter().enumerate().take(last_row + 1).skip(start.row) {
        let first = if row_index == start.row { start.col } else { 0 };
        let last = if row_index == end.row {
            end.col.saturating_add(1)
        } else {
            row.cells.len()
        }
        .min(row.cells.len());
        for cell in &row.cells[first.min(last)..last] {
            if cell.width > 0 {
                result.push_str(&cell.text);
            }
        }
        while result.ends_with(' ') {
            result.pop();
        }
        if row_index != end.row && !row.wrapped {
            result.push('\n');
        }
    }
    (!result.is_empty()).then_some(result)
}

fn ctrl_c_behavior(tab: &TerminalTab) -> CtrlCBehavior {
    selected_text(tab)
        .map(CtrlCBehavior::Copy)
        .unwrap_or(CtrlCBehavior::Interrupt)
}

fn word_selection(tab: &TerminalTab, point: GridPoint) -> Option<Selection> {
    let snapshot = tab.mirror.snapshot()?;
    let mut rows = Vec::with_capacity(snapshot.scrollback.len() + snapshot.cells.len());
    rows.extend(snapshot.scrollback.iter());
    rows.extend(snapshot.cells.iter());
    let row = rows.get(point.row)?;
    let is_word = |cell: &Cell| cell.width > 0 && !cell.text.chars().all(char::is_whitespace);
    let mut start = point.col.min(row.cells.len().saturating_sub(1));
    let mut end = start;
    while start > 0 && is_word(&row.cells[start - 1]) {
        start -= 1;
    }
    while end + 1 < row.cells.len() && is_word(&row.cells[end + 1]) {
        end += 1;
    }
    Some(Selection {
        anchor: GridPoint {
            row: point.row,
            col: start,
        },
        head: GridPoint {
            row: point.row,
            col: end,
        },
    })
}

fn line_selection(tab: &TerminalTab, point: GridPoint) -> Option<Selection> {
    let snapshot = tab.mirror.snapshot()?;
    let mut rows = Vec::with_capacity(snapshot.scrollback.len() + snapshot.cells.len());
    rows.extend(snapshot.scrollback.iter());
    rows.extend(snapshot.cells.iter());
    let row = rows.get(point.row)?;
    Some(Selection {
        anchor: GridPoint {
            row: point.row,
            col: 0,
        },
        head: GridPoint {
            row: point.row,
            col: row.cells.len().saturating_sub(1),
        },
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeypadKey {
    Digit(u8),
    Decimal,
    Divide,
    Multiply,
    Subtract,
    Add,
}

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

fn encode_keystroke(
    keystroke: &Keystroke,
    application_cursor: bool,
    keypad: Option<KeypadKey>,
) -> Option<Vec<u8>> {
    if let Some(keypad) = keypad {
        return Some(encode_application_keypad(keypad));
    }

    let key = keystroke.key.as_str();
    let modifier = xterm_modifier(&keystroke.modifiers);
    let special = match key {
        "tab" if keystroke.modifiers.shift => Some(if modifier == 2 {
            b"\x1b[Z".to_vec()
        } else {
            format!("\x1b[1;{modifier}Z").into_bytes()
        }),
        "up" => Some(encode_cursor_key(b'A', application_cursor, modifier)),
        "down" => Some(encode_cursor_key(b'B', application_cursor, modifier)),
        "right" => Some(encode_cursor_key(b'C', application_cursor, modifier)),
        "left" => Some(encode_cursor_key(b'D', application_cursor, modifier)),
        "home" => Some(encode_cursor_key(b'H', application_cursor, modifier)),
        "end" => Some(encode_cursor_key(b'F', application_cursor, modifier)),
        "insert" => Some(encode_tilde_key(2, modifier)),
        "delete" => Some(encode_tilde_key(3, modifier)),
        "pageup" => Some(encode_tilde_key(5, modifier)),
        "pagedown" => Some(encode_tilde_key(6, modifier)),
        "f1" => Some(encode_function_key(b'P', modifier)),
        "f2" => Some(encode_function_key(b'Q', modifier)),
        "f3" => Some(encode_function_key(b'R', modifier)),
        "f4" => Some(encode_function_key(b'S', modifier)),
        "f5" => Some(encode_tilde_key(15, modifier)),
        "f6" => Some(encode_tilde_key(17, modifier)),
        "f7" => Some(encode_tilde_key(18, modifier)),
        "f8" => Some(encode_tilde_key(19, modifier)),
        "f9" => Some(encode_tilde_key(20, modifier)),
        "f10" => Some(encode_tilde_key(21, modifier)),
        "f11" => Some(encode_tilde_key(23, modifier)),
        "f12" => Some(encode_tilde_key(24, modifier)),
        _ => None,
    };
    if special.is_some() {
        return special;
    }

    let mut bytes = if keystroke.modifiers.control {
        vec![control_byte(key)?]
    } else {
        match key {
            "enter" => b"\r".to_vec(),
            "tab" => b"\t".to_vec(),
            "space" => b" ".to_vec(),
            "backspace" => vec![0x7f],
            "escape" => vec![0x1b],
            _ => keystroke.key_char.as_ref()?.as_bytes().to_vec(),
        }
    };
    if keystroke.modifiers.alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

fn encode_cursor_key(key: u8, application_cursor: bool, modifier: u8) -> Vec<u8> {
    if modifier > 1 {
        format!("\x1b[1;{modifier}{}", key as char).into_bytes()
    } else if application_cursor {
        vec![0x1b, b'O', key]
    } else {
        vec![0x1b, b'[', key]
    }
}

fn encode_function_key(key: u8, modifier: u8) -> Vec<u8> {
    if modifier > 1 {
        format!("\x1b[1;{modifier}{}", key as char).into_bytes()
    } else {
        vec![0x1b, b'O', key]
    }
}

fn encode_tilde_key(key: u8, modifier: u8) -> Vec<u8> {
    if modifier > 1 {
        format!("\x1b[{key};{modifier}~").into_bytes()
    } else {
        format!("\x1b[{key}~").into_bytes()
    }
}

fn encode_application_keypad(key: KeypadKey) -> Vec<u8> {
    let key = match key {
        KeypadKey::Digit(0) => b'p',
        KeypadKey::Digit(1) => b'q',
        KeypadKey::Digit(2) => b'r',
        KeypadKey::Digit(3) => b's',
        KeypadKey::Digit(4) => b't',
        KeypadKey::Digit(5) => b'u',
        KeypadKey::Digit(6) => b'v',
        KeypadKey::Digit(7) => b'w',
        KeypadKey::Digit(8) => b'x',
        KeypadKey::Digit(9) => b'y',
        KeypadKey::Digit(_) => unreachable!(),
        KeypadKey::Decimal => b'n',
        KeypadKey::Divide => b'o',
        KeypadKey::Multiply => b'j',
        KeypadKey::Subtract => b'm',
        KeypadKey::Add => b'k',
    };
    vec![0x1b, b'O', key]
}

fn xterm_modifier(modifiers: &gpui::Modifiers) -> u8 {
    1 + u8::from(modifiers.shift) + 2 * u8::from(modifiers.alt) + 4 * u8::from(modifiers.control)
}

fn control_byte(key: &str) -> Option<u8> {
    if key == "space" {
        return Some(0);
    }
    let [byte] = key.as_bytes() else {
        return None;
    };
    match byte.to_ascii_uppercase() {
        b'@' => Some(0),
        byte @ b'A'..=b'Z' => Some(byte & 0x1f),
        byte @ b'['..=b'_' => Some(byte & 0x1f),
        b'?' => Some(0x7f),
        _ => None,
    }
}

fn is_application_shortcut(keystroke: &Keystroke) -> bool {
    if !keystroke.modifiers.control {
        return false;
    }
    matches!(
        (keystroke.modifiers.shift, keystroke.key.as_str()),
        (false, "c" | "t" | "v" | "w" | "tab") | (true, "tab" | "c" | "v" | "p")
    )
}

fn utf16_byte_index(text: &str, target: usize) -> usize {
    let mut utf16_offset = 0;
    for (byte_offset, character) in text.char_indices() {
        if utf16_offset >= target {
            return byte_offset;
        }
        utf16_offset += character.len_utf16();
        if utf16_offset >= target {
            return byte_offset + character.len_utf8();
        }
    }
    text.len()
}

fn encode_mouse(
    button: Option<MouseButton>,
    release: bool,
    motion: bool,
    col: usize,
    row: usize,
    modifiers: gpui::Modifiers,
) -> Option<Vec<u8>> {
    let mut code = match button {
        Some(MouseButton::Left) => 0,
        Some(MouseButton::Middle) => 1,
        Some(MouseButton::Right) => 2,
        None => 3,
        Some(MouseButton::Navigate(_)) => return None,
    };
    if motion {
        code += 32;
    }
    Some(encode_sgr_mouse_code(code, col, row, modifiers, !release))
}

fn encode_sgr_mouse_code(
    mut code: u8,
    col: usize,
    row: usize,
    modifiers: gpui::Modifiers,
    press: bool,
) -> Vec<u8> {
    if modifiers.shift {
        code += 4;
    }
    if modifiers.alt {
        code += 8;
    }
    if modifiers.control {
        code += 16;
    }
    format!(
        "\x1b[<{code};{};{}{}",
        col.saturating_add(1),
        row.saturating_add(1),
        if press { 'M' } else { 'm' }
    )
    .into_bytes()
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
    session_id: String,
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
                &session_id,
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
    session_id: &str,
    cols: i16,
    rows: i16,
    stop: Arc<AtomicBool>,
    sender: &UiEventSender,
    instance: Option<&str>,
) -> crate::Result<()> {
    let mut client = app::connect_or_start(instance)?;
    let Some(session) = client
        .list_sessions()?
        .into_iter()
        .find(|session| session.id == session_id)
    else {
        stop.store(true, Ordering::Release);
        let _ = sender.send(UiEvent::TabControl {
            tab_id,
            message: ServerMessage::Error {
                code: crate::protocol::ErrorCode::SessionNotFound,
                message: "session no longer exists".into(),
            },
        });
        return Ok(());
    };
    if session.status != SessionStatus::Running {
        stop.store(true, Ordering::Release);
        let message = if let Some(exit_code) = session.exit_code {
            ServerMessage::SessionExited {
                session_id: session.id,
                exit_code,
            }
        } else {
            ServerMessage::Error {
                code: crate::protocol::ErrorCode::SessionExited,
                message: session.error.unwrap_or_else(|| "session exited".into()),
            }
        };
        let _ = sender.send(UiEvent::TabControl { tab_id, message });
        return Ok(());
    }
    match client.request(ClientMessage::Attach {
        session_id: session_id.to_owned(),
        cols,
        rows,
    })? {
        ServerMessage::Attached { .. } => {}
        message => return Err(format!("unexpected attach response: {message:?}").into()),
    }
    while let Some(message) = client.take_pending_screen() {
        let _ = sender.send(UiEvent::TabScreen { tab_id, message });
    }
    let (connection, next_request_id, pending) = client.into_parts();
    for message in pending {
        let _ = sender.send(UiEvent::TabScreen { tab_id, message });
    }
    let (command_tx, command_rx) = mpsc::channel();
    let transport = TabTransport {
        commands: command_tx,
        stop: stop.clone(),
    };
    if !sender.send(UiEvent::TabConnected { tab_id, transport }) {
        return Err("UI closed while attaching terminal".into());
    }
    let mut reader = DaemonClient::from_parts(connection, next_request_id);
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
                        crate::perf::log_input_latency_stage(latency_id, "client_sent", None);
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
                let detached = matches!(message, ServerMessage::Detached { .. });
                let exited = matches!(message, ServerMessage::SessionExited { .. });
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

fn terminate_session_and_wait(
    instance: Option<&str>,
    session_id: &str,
) -> Result<Vec<SessionInfo>, String> {
    let mut client = DaemonClient::connect(instance, Duration::from_secs(2))
        .map_err(|error| error.to_string())?;
    let kill_error = client
        .kill_session(session_id.to_owned())
        .err()
        .map(|error| error.to_string());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let sessions = client.list_sessions().map_err(|error| error.to_string())?;
        let running = sessions
            .iter()
            .any(|session| session.id == session_id && session.status == SessionStatus::Running);
        if !running {
            return Ok(sessions);
        }
        if let Some(error) = kill_error.as_ref() {
            return Err(error.clone());
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out ending session {session_id}"));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn short_session_id(id: &str) -> String {
    id.rsplit('-')
        .next()
        .unwrap_or(id)
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
    crate::perf::log_startup_metric(name, elapsed);
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
    let (private_bytes, handles) = current_process_metrics();
    log_performance_sample(
        frame_p50,
        frame_p95,
        paint_p50,
        paint_p95,
        private_bytes,
        handles,
    );
    metrics.frame_intervals_us.clear();
    metrics.paint_times_us.clear();
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    sorted[(sorted.len().saturating_sub(1) * percentile) / 100]
}

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
    private_bytes: usize,
    handles: u32,
) {
    let Some(local_app_data) = env::var_os("LOCALAPPDATA") else {
        return;
    };
    let directory = std::path::PathBuf::from(local_app_data).join("Compi");
    if fs::create_dir_all(&directory).is_err() {
        return;
    }
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("client-perf.log"))
    {
        let _ = writeln!(
            file,
            "frame_p50_us={frame_p50_us} frame_p95_us={frame_p95_us} \
             paint_p50_us={paint_p50_us} paint_p95_us={paint_p95_us} \
             private_bytes={private_bytes} handles={handles}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{CursorState, TerminalModes, TextAttributes};

    fn row(text: &str, wrapped: bool) -> Row {
        Row {
            cells: text
                .chars()
                .map(|character| Cell {
                    text: character.to_string().into(),
                    width: 1,
                    foreground: Color::Default,
                    background: Color::Default,
                    attributes: TextAttributes::default(),
                    hyperlink: None,
                })
                .collect(),
            wrapped,
        }
    }

    fn snapshot(scrollback: Vec<Row>, cells: Vec<Row>) -> ScreenSnapshot {
        ScreenSnapshot {
            sequence: 1,
            cols: 4,
            rows: cells.len() as u16,
            cells,
            scrollback,
            cursor: CursorState::default(),
            modes: TerminalModes::default(),
            title: String::new(),
            current_directory: None,
            images: Vec::new(),
            placements: Vec::new(),
        }
    }

    #[test]
    fn inherits_current_directory_from_active_screen_state() {
        let mut screen = snapshot(Vec::new(), vec![row("", false)]);
        screen.current_directory = Some("/home/dev/project".to_owned());
        assert_eq!(
            inherited_working_directory(Some(&screen)),
            Some("/home/dev/project".to_owned())
        );
        assert_eq!(inherited_working_directory(None), None);
    }

    #[test]
    fn maps_terminal_control_modified_and_keypad_keys() {
        let ctrl_c = Keystroke {
            modifiers: gpui::Modifiers {
                control: true,
                ..Default::default()
            },
            key: "c".into(),
            key_char: None,
        };
        assert_eq!(encode_keystroke(&ctrl_c, false, None), Some(vec![3]));
        let space = Keystroke {
            key: "space".into(),
            ..Default::default()
        };
        assert_eq!(encode_keystroke(&space, false, None), Some(b" ".to_vec()));
        let up = Keystroke {
            key: "up".into(),
            ..Default::default()
        };
        assert_eq!(encode_keystroke(&up, false, None), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode_keystroke(&up, true, None), Some(b"\x1bOA".to_vec()));
        let application_home = Keystroke {
            key: "home".into(),
            ..Default::default()
        };
        assert_eq!(
            encode_keystroke(&application_home, true, None),
            Some(b"\x1bOH".to_vec())
        );
        let modified_up = Keystroke {
            modifiers: gpui::Modifiers {
                control: true,
                shift: true,
                ..Default::default()
            },
            key: "up".into(),
            key_char: None,
        };
        assert_eq!(
            encode_keystroke(&modified_up, true, None),
            Some(b"\x1b[1;6A".to_vec())
        );
        let ctrl_space = Keystroke {
            modifiers: gpui::Modifiers {
                control: true,
                ..Default::default()
            },
            key: "space".into(),
            key_char: None,
        };
        assert_eq!(encode_keystroke(&ctrl_space, false, None), Some(vec![0]));
        assert_eq!(
            encode_keystroke(&space, false, Some(KeypadKey::Digit(7))),
            Some(b"\x1bOw".to_vec())
        );
    }

    #[test]
    fn reserves_clipboard_and_paste_shortcuts() {
        let shortcut = |key: &str, shift: bool| Keystroke {
            modifiers: gpui::Modifiers {
                control: true,
                shift,
                ..Default::default()
            },
            key: key.into(),
            key_char: None,
        };

        assert!(is_application_shortcut(&shortcut("v", false)));
        assert!(is_application_shortcut(&shortcut("v", true)));
        assert!(is_application_shortcut(&shortcut("c", false)));
        assert!(is_application_shortcut(&shortcut("c", true)));
    }

    #[test]
    fn starts_tab_drag_only_after_crossing_threshold() {
        let origin = point(px(10.0), px(10.0));

        assert!(!drag_threshold_crossed(origin, point(px(12.0), px(12.0))));
        assert!(drag_threshold_crossed(origin, point(px(14.0), px(10.0))));
    }

    #[test]
    fn encodes_sgr_mouse_coordinates_and_modifiers() {
        let data = encode_sgr_mouse_code(
            0,
            2,
            4,
            gpui::Modifiers {
                control: true,
                ..Default::default()
            },
            true,
        );

        assert_eq!(data, b"\x1b[<16;3;5M");
    }
    #[test]
    fn allows_only_web_hyperlinks() {
        assert!(is_allowed_hyperlink("https://example.com/docs?q=1"));
        assert!(is_allowed_hyperlink("HTTP://localhost:8080/"));
        assert!(!is_allowed_hyperlink("file:///etc/passwd"));
        assert!(!is_allowed_hyperlink("javascript:alert(1)"));
        assert!(!is_allowed_hyperlink("https:///missing-host"));
        assert!(!is_allowed_hyperlink("https://example.com/\nheader"));
    }

    #[test]
    fn encodes_any_motion_without_a_pressed_button() {
        let data = encode_mouse(None, false, true, 0, 0, gpui::Modifiers::default());
        assert_eq!(data.as_deref(), Some(b"\x1b[<35;1;1M".as_slice()));
    }

    #[test]
    fn extracts_wrapped_and_multiline_selection() {
        let snapshot = snapshot(vec![row("abcd", true)], vec![row("efgh", false)]);
        let mut mirror = ScreenMirror::default();
        mirror.apply(ScreenMessage::Snapshot { snapshot });
        let mut tab = TerminalTab {
            id: 1,
            session_id: "session".into(),
            mirror,
            state: ConnectionState::Attached,
            error: None,
            transport: None,
            scroll_offset: 0,
            selection: Some(Selection {
                anchor: GridPoint { row: 0, col: 1 },
                head: GridPoint { row: 1, col: 2 },
            }),
            selecting: false,
            image_cache: HashMap::new(),
            image_pending: HashMap::new(),
            row_render_cache: Arc::new(Mutex::new(RowRenderCache::default())),
            cols: 4,
            rows: 1,
        };
        assert_eq!(selected_text(&tab).as_deref(), Some("bcdefg"));
        assert_eq!(
            ctrl_c_behavior(&tab),
            CtrlCBehavior::Copy("bcdefg".to_owned())
        );
        tab.selection = None;
        assert_eq!(ctrl_c_behavior(&tab), CtrlCBehavior::Interrupt);
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
