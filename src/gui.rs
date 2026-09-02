use crate::app;
use crate::client::{DaemonClient, ServerEvent};
use crate::protocol::{ClientMessage, ServerMessage, SessionInfo, SessionStatus};
use crate::terminal::{
    Cell, Color, CursorShape, KittyImage, KittyPlacement, MirrorApply, MouseMode, Row,
    ScreenMessage, ScreenMirror, ScreenSnapshot,
};
use base64::Engine as _;
use gpui::{
    App, Application, Bounds, ClipboardItem, ContentMask, Context, Corners, ElementInputHandler,
    EntityInputHandler, FocusHandle, Focusable, FontStyle, FontWeight, Hsla, KeyBinding,
    KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Render, RenderImage, ScrollHandle, ScrollWheelEvent, SharedString, StrikethroughStyle,
    Subscription, TextRun, TitlebarOptions, UTF16Selection, UnderlineStyle, Window, WindowBounds,
    WindowControlArea, WindowOptions, actions, canvas, div, fill, font, point, prelude::*, px, rgb,
    size,
};
use image::{Frame as ImageFrame, RgbaImage};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use smallvec::smallvec;
use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{SW_RESTORE, ShowWindowAsync};

const DEFAULT_COLS: i16 = 100;
const DEFAULT_ROWS: i16 = 30;
const CELL_WIDTH: f32 = 8.45;
const CELL_HEIGHT: f32 = 18.0;
const CHROME_HEIGHT: f32 = 42.0;
const TAB_WIDTH: f32 = 210.0;
const TAB_SCROLL_STEP: f32 = TAB_WIDTH;
const WINDOW_CONTROLS_WIDTH: f32 = 138.0;
const TITLEBAR_BRAND_WIDTH: f32 = 48.0;
const TAB_NAV_WIDTH: f32 = 56.0;
const NEW_TAB_WIDTH: f32 = 42.0;
const SESSION_SWITCHER_WIDTH: f32 = 80.0;
const TITLEBAR_DRAG_WIDTH: f32 = 48.0;
const TITLEBAR_ACTIONS_WIDTH: f32 =
    TAB_NAV_WIDTH + NEW_TAB_WIDTH + SESSION_SWITCHER_WIDTH + TITLEBAR_DRAG_WIDTH;
const TERMINAL_PADDING: f32 = 8.0;
const POLL_INTERVAL: Duration = Duration::from_millis(16);
const RECONNECT_DELAY: Duration = Duration::from_millis(350);

const BACKGROUND: u32 = 0x171614;
const SURFACE: u32 = 0x211f1c;
const SURFACE_HOVER: u32 = 0x2b2824;
const BORDER: u32 = 0x3a3630;
const FOREGROUND: u32 = 0xd8d2c7;
const MUTED: u32 = 0x928b80;
const ACCENT: u32 = 0xd19a66;
const ERROR: u32 = 0xe06c75;
const SELECTION: u32 = 0x5b4634;

pub fn run() {
    let started_at = Instant::now();
    Application::new().run(move |cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("ctrl-t", NewTab, Some("Terminal")),
            KeyBinding::new("ctrl-w", CloseTab, Some("Terminal")),
            KeyBinding::new("ctrl-tab", NextTab, Some("Terminal")),
            KeyBinding::new("ctrl-shift-tab", PreviousTab, Some("Terminal")),
            KeyBinding::new("ctrl-shift-c", CopySelection, Some("Terminal")),
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
                move |window, cx| cx.new(|cx| CompiApp::new(started_at, window, cx)),
            )
            .expect("failed to open Compi window");

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

struct TabTransport {
    commands: Sender<ClientMessage>,
    stop: Arc<AtomicBool>,
}

impl TabTransport {
    fn send(&self, message: ClientMessage) -> crate::Result<()> {
        self.commands
            .send(message)
            .map_err(|_| "terminal connection writer stopped".into())
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

    fn refresh_images(&mut self) {
        let Some(snapshot) = self.mirror.snapshot() else {
            self.image_cache.clear();
            return;
        };
        let active_ids: Vec<u32> = snapshot.images.iter().map(|image| image.id).collect();
        self.image_cache
            .retain(|image_id, _| active_ids.contains(image_id));
        for image in &snapshot.images {
            let unchanged = self
                .image_cache
                .get(&image.id)
                .is_some_and(|(data, _)| data == &image.data);
            if unchanged {
                continue;
            }
            match decode_kitty_image(image) {
                Ok(render_image) => {
                    self.image_cache
                        .insert(image.id, (image.data.clone(), render_image));
                }
                Err(error) => self.error = Some(format!("Kitty image {}: {error}", image.id)),
            }
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
    TabConnected {
        tab_id: u64,
        transport: TabTransport,
    },
    TabScreen {
        tab_id: u64,
        message: ScreenMessage,
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

struct CompiApp {
    started_at: Instant,
    first_snapshot_logged: bool,
    focus_handle: FocusHandle,
    ime_text: String,
    ime_marked_range: Option<Range<usize>>,
    ime_selected_range: Range<usize>,
    tabs: Vec<TerminalTab>,
    active_tab: Option<u64>,
    tab_scroll_handle: ScrollHandle,
    next_tab_id: u64,
    sessions: Vec<SessionInfo>,
    switcher_open: bool,
    loading_sessions: bool,
    attach_after_session_list: bool,
    global_error: Option<String>,
    event_tx: Sender<UiEvent>,
    event_rx: Receiver<UiEvent>,
    subscriptions: Vec<Subscription>,
    terminal_cols: i16,
    terminal_rows: i16,
}

impl CompiApp {
    fn new(started_at: Instant, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        let mut this = Self {
            started_at,
            first_snapshot_logged: false,
            focus_handle: cx.focus_handle(),
            ime_text: String::new(),
            ime_marked_range: None,
            ime_selected_range: 0..0,
            tabs: Vec::new(),
            active_tab: None,
            tab_scroll_handle: ScrollHandle::new(),
            next_tab_id: 1,
            sessions: Vec::new(),
            switcher_open: false,
            loading_sessions: true,
            attach_after_session_list: false,
            global_error: None,
            event_tx,
            event_rx,
            subscriptions: Vec::new(),
            terminal_cols: DEFAULT_COLS,
            terminal_rows: DEFAULT_ROWS,
        };

        this.update_dimensions(window);
        this.refresh_sessions(true);
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

        let timer = cx.background_executor().clone();
        cx.spawn(async move |weak, cx| {
            loop {
                timer.timer(POLL_INTERVAL).await;
                if weak
                    .update(cx, |this, cx| {
                        if this.drain_events() {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        this
    }

    fn refresh_sessions(&mut self, attach_initial: bool) {
        self.loading_sessions = true;
        self.attach_after_session_list |= attach_initial;
        let sender = self.event_tx.clone();
        thread::spawn(move || {
            let result = app::connect_or_start(None)
                .and_then(|mut client| client.list_sessions())
                .map_err(|error| error.to_string());
            let _ = sender.send(UiEvent::SessionsLoaded(result));
        });
    }

    fn create_session(&mut self) {
        let sender = self.event_tx.clone();
        let cols = self.terminal_cols;
        let rows = self.terminal_rows;
        thread::spawn(move || {
            let result = app::connect_or_start(None)
                .and_then(|mut client| client.create_session(cols, rows))
                .map_err(|error| error.to_string());
            let _ = sender.send(UiEvent::SessionCreated(result));
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
            cols: self.terminal_cols,
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
        );
    }

    fn drain_events(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.event_rx.try_recv() {
                Ok(event) => {
                    changed = true;
                    self.handle_event(event);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.global_error.is_none() {
                        self.global_error = Some("internal UI event channel disconnected".into());
                        changed = true;
                    }
                    break;
                }
            }
        }
        changed
    }

    fn handle_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::SessionsLoaded(Ok(sessions)) => {
                self.loading_sessions = false;
                let needs_initial_tab = self.attach_after_session_list && self.tabs.is_empty();
                self.attach_after_session_list = false;
                self.sessions = sessions;
                if needs_initial_tab {
                    if let Some(session) = self
                        .sessions
                        .iter()
                        .find(|session| {
                            session.status == SessionStatus::Running && !session.attached
                        })
                        .cloned()
                    {
                        self.attach_session(session);
                    } else {
                        self.create_session();
                    }
                }
            }
            UiEvent::SessionsLoaded(Err(error)) => {
                self.loading_sessions = false;
                self.global_error = Some(error);
            }
            UiEvent::SessionCreated(Ok(session)) => {
                self.sessions.push(session.clone());
                self.attach_session(session);
            }
            UiEvent::SessionCreated(Err(error)) => self.global_error = Some(error),
            UiEvent::TabConnected { tab_id, transport } => {
                let (cols, rows) = (self.terminal_cols, self.terminal_rows);
                if let Some(tab) = self.tab_mut(tab_id) {
                    tab.transport = Some(transport);
                    tab.state = ConnectionState::Attached;
                    tab.error = None;
                    tab.send(ClientMessage::Resize { cols, rows });
                }
            }
            UiEvent::TabScreen { tab_id, message } => {
                let mut request_snapshot = false;
                if let Some(tab) = self.tab_mut(tab_id) {
                    request_snapshot = matches!(tab.mirror.apply(message), MirrorApply::Gap { .. });
                    if !request_snapshot {
                        tab.state = ConnectionState::Attached;
                        tab.refresh_images();
                    }
                }
                if request_snapshot {
                    if let Some(tab) = self.tab_mut(tab_id) {
                        tab.send(ClientMessage::RequestSnapshot);
                    }
                } else if !self.first_snapshot_logged {
                    self.first_snapshot_logged = true;
                    log_startup_metric(self.started_at.elapsed());
                }
            }
            UiEvent::TabControl { tab_id, message } => match message {
                ServerMessage::SessionExited { exit_code, .. } => {
                    if let Some(tab) = self.tab_mut(tab_id) {
                        tab.state = ConnectionState::Exited(exit_code);
                        tab.transport = None;
                    }
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
        let application_cursor = tab
            .mirror
            .snapshot()
            .is_some_and(|snapshot| snapshot.modes.application_cursor);
        if let Some(bytes) = encode_keystroke(keystroke, application_cursor) {
            tab.scroll_offset = 0;
            tab.send(ClientMessage::Input { data: bytes });
        }
    }

    fn new_tab(&mut self, _: &NewTab, _: &mut Window, cx: &mut Context<Self>) {
        self.create_session();
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

    fn scroll_tabs(&mut self, direction: f32, cx: &mut Context<Self>) {
        let offset = self.tab_scroll_handle.offset();
        let max_offset = f32::from(self.tab_scroll_handle.max_offset().width);
        let x = (f32::from(offset.x) - direction * TAB_SCROLL_STEP).clamp(-max_offset, 0.0);
        self.tab_scroll_handle.set_offset(point(px(x), offset.y));
        cx.notify();
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
        tab.send(ClientMessage::Input { data });
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
                event.button,
                false,
                false,
                point.col,
                visible_row(tab, point.row),
                event.modifiers,
            ) {
                tab.send(ClientMessage::Input { data });
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
            let button = event.pressed_button.unwrap_or(MouseButton::Left);
            if let Some(data) = encode_mouse(
                button,
                false,
                true,
                point.col,
                visible_row(tab, point.row),
                event.modifiers,
            ) {
                tab.send(ClientMessage::Input { data });
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
                event.button,
                true,
                false,
                point.col,
                visible_row(tab, point.row),
                event.modifiers,
            ) {
                tab.send(ClientMessage::Input { data });
            }
        } else {
            tab.selecting = false;
            if let Some(selection) = tab.selection
                && selection.anchor == selection.head
            {
                tab.selection = None;
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
            tab.send(ClientMessage::Input { data });
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

    fn render_titlebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.tabs.iter().map(|tab| {
            let id = tab.id;
            let active = self.active_tab == Some(id);
            let status_color = match tab.state {
                ConnectionState::Attached => color(ACCENT),
                ConnectionState::Connecting | ConnectionState::Reconnecting => color(MUTED),
                ConnectionState::Exited(_) => color(MUTED),
                ConnectionState::Failed => color(ERROR),
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
                .border_r_1()
                .border_b_2()
                .border_color(if active { color(ACCENT) } else { color(BORDER) })
                .bg(if active {
                    color(SURFACE_HOVER)
                } else {
                    color(SURFACE)
                })
                .hover(|style| style.bg(color(SURFACE_HOVER)).cursor_pointer())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.active_tab = Some(id);
                    this.reveal_tab(id);
                    this.switcher_open = false;
                    window.focus(&this.focus_handle);
                    cx.notify();
                }))
                .child(div().size(px(6.0)).rounded_full().bg(status_color))
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(tab.title()),
                )
                .child(
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
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_tab_id(id);
                            cx.stop_propagation();
                            cx.notify();
                        }))
                        .child("×"),
                )
        });
        div()
            .h(px(CHROME_HEIGHT))
            .w_full()
            .relative()
            .overflow_hidden()
            .bg(color(BACKGROUND))
            .border_b_1()
            .border_color(color(BORDER))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(px(TITLEBAR_BRAND_WIDTH))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_weight(FontWeight::BOLD)
                    .text_color(color(ACCENT))
                    .window_control_area(WindowControlArea::Drag)
                    .child(">_"),
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
                    .child(
                        div()
                            .h_full()
                            .min_w(px(16.0))
                            .flex_1()
                            .window_control_area(WindowControlArea::Drag),
                    ),
            )
            .child(
                div()
                    .id("scroll-tabs-left")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(WINDOW_CONTROLS_WIDTH
                        + TITLEBAR_DRAG_WIDTH
                        + SESSION_SWITCHER_WIDTH
                        + NEW_TAB_WIDTH
                        + TAB_NAV_WIDTH / 2.0))
                    .w(px(28.0))
                    .flex_none()
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
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.scroll_tabs(-1.0, cx);
                    }))
                    .child("‹"),
            )
            .child(
                div()
                    .id("scroll-tabs-right")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(WINDOW_CONTROLS_WIDTH
                        + TITLEBAR_DRAG_WIDTH
                        + SESSION_SWITCHER_WIDTH
                        + NEW_TAB_WIDTH))
                    .w(px(28.0))
                    .flex_none()
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
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.scroll_tabs(1.0, cx);
                    }))
                    .child("›"),
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
                    .flex_none()
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
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.create_session();
                        cx.notify();
                    }))
                    .child("+"),
            )
            .child(
                div()
                    .id("session-switcher")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(WINDOW_CONTROLS_WIDTH + TITLEBAR_DRAG_WIDTH))
                    .flex_none()
                    .w(px(SESSION_SWITCHER_WIDTH))
                    .justify_center()
                    .px_3()
                    .flex()
                    .items_center()
                    .text_sm()
                    .text_color(color(MUTED))
                    .hover(|style| {
                        style
                            .bg(color(SURFACE_HOVER))
                            .text_color(color(FOREGROUND))
                            .cursor_pointer()
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.switcher_open = !this.switcher_open;
                        if this.switcher_open {
                            this.refresh_sessions(false);
                        }
                        cx.notify();
                    }))
                    .child("Sessions"),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(WINDOW_CONTROLS_WIDTH))
                    .w(px(TITLEBAR_DRAG_WIDTH))
                    .bg(color(BACKGROUND))
                    .window_control_area(WindowControlArea::Drag),
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
                        "—",
                        WindowControlArea::Min,
                        false,
                    ))
                    .child(window_control(
                        "maximize-window",
                        "□",
                        WindowControlArea::Max,
                        false,
                    ))
                    .child(window_control(
                        "close-window",
                        "×",
                        WindowControlArea::Close,
                        true,
                    )),
            )
    }

    fn render_switcher(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.sessions.iter().map(|session| {
            let session = session.clone();
            let can_attach = session.status == SessionStatus::Running && !session.attached;
            let status = match session.status {
                SessionStatus::Starting => "Starting",
                SessionStatus::Running if session.attached => "Open",
                SessionStatus::Running => "Detached",
                SessionStatus::Exited => "Exited",
                SessionStatus::Failed => "Failed",
            };
            let session_for_attach = session.clone();
            div()
                .id(("session", session.created_at_ms))
                .w_full()
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .justify_between()
                .border_b_1()
                .border_color(color(BORDER))
                .when(can_attach, |row| {
                    row.hover(|style| style.bg(color(SURFACE_HOVER)).cursor_pointer())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.attach_session(session_for_attach.clone());
                            window.focus(&this.focus_handle);
                            cx.notify();
                        }))
                })
                .child(short_session_id(&session.id))
                .child(
                    div()
                        .text_sm()
                        .text_color(if session.status == SessionStatus::Failed {
                            color(ERROR)
                        } else {
                            color(MUTED)
                        })
                        .child(status),
                )
        });
        div()
            .size_full()
            .bg(color(BACKGROUND))
            .p_6()
            .flex()
            .justify_center()
            .child(
                div()
                    .w(px(560.0))
                    .max_h(px(480.0))
                    .bg(color(SURFACE))
                    .border_1()
                    .border_color(color(BORDER))
                    .rounded_md()
                    .overflow_hidden()
                    .child(
                        div()
                            .px_3()
                            .py_3()
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(color(BORDER))
                            .child("Sessions")
                            .child(
                                div()
                                    .id("switcher-new-session")
                                    .px_3()
                                    .py_1()
                                    .rounded_sm()
                                    .bg(color(ACCENT))
                                    .text_color(color(BACKGROUND))
                                    .hover(|style| style.opacity(0.85).cursor_pointer())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.create_session();
                                        cx.notify();
                                    }))
                                    .child("New session"),
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

fn window_control(
    id: &'static str,
    label: &'static str,
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
        .on_click(move |_, window, _| match area {
            WindowControlArea::Min => window.minimize_window(),
            WindowControlArea::Max => toggle_window_maximized(window),
            WindowControlArea::Close => window.remove_window(),
            WindowControlArea::Drag => {}
        })
        .child(label)
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
        if let Some(tab) = self.active_tab() {
            window.set_window_title(&format!("{} · Compi", tab.title()));
        } else {
            window.set_window_title("Compi");
        }
        div()
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                this.handle_keystroke(&event.keystroke);
                cx.stop_propagation();
            }))
            .size_full()
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
            .on_action(cx.listener(Self::copy_selection))
            .on_action(cx.listener(Self::paste_clipboard))
            .on_action(cx.listener(Self::toggle_switcher))
            .child(self.render_titlebar(cx))
            .child(if self.switcher_open {
                self.render_switcher(cx).into_any_element()
            } else {
                self.render_terminal(cx).into_any_element()
            })
    }
}

#[derive(Clone)]
struct PaintModel {
    snapshot: ScreenSnapshot,
    scroll_offset: usize,
    selection: Option<Selection>,
    images: HashMap<u32, Arc<RenderImage>>,
}

impl PaintModel {
    fn from_tab(tab: &TerminalTab) -> Option<Self> {
        Some(Self {
            snapshot: tab.mirror.snapshot()?.clone(),
            scroll_offset: tab.scroll_offset,
            selection: tab.selection,
            images: tab
                .image_cache
                .iter()
                .map(|(id, (_, image))| (*id, image.clone()))
                .collect(),
        })
    }
}

fn paint_terminal(bounds: Bounds<Pixels>, model: &PaintModel, window: &mut Window, cx: &mut App) {
    let (visible, base) = visible_rows(&model.snapshot, model.scroll_offset);
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
            paint_row_text(bounds, row_index, row, window, cx);
        }
        paint_images(bounds, model, base, true, window);
    });
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

fn paint_row_text(
    bounds: Bounds<Pixels>,
    row_index: usize,
    row: &Row,
    window: &mut Window,
    cx: &mut App,
) {
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
            style.text.clone()
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
        let underline = style.attributes.underline.then_some(UnderlineStyle {
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
        let line =
            window
                .text_system()
                .shape_line(SharedString::from(text), px(14.0), &[run], None);
        let origin = point(
            bounds.left() + px(start as f32 * CELL_WIDTH),
            bounds.top() + px(row_index as f32 * CELL_HEIGHT),
        );
        let _ = line.paint(origin, px(CELL_HEIGHT), window, cx);
        start = end;
    }
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
    if !model.snapshot.cursor.visible || model.scroll_offset != 0 {
        return;
    }
    let absolute_row = model.snapshot.scrollback.len() + usize::from(model.snapshot.cursor.row);
    if absolute_row < base {
        return;
    }
    let row = absolute_row - base;
    let col = usize::from(model.snapshot.cursor.col);
    let (x, y) = (
        bounds.left() + px(col as f32 * CELL_WIDTH),
        bounds.top() + px(row as f32 * CELL_HEIGHT),
    );
    let cursor_bounds = match model.snapshot.cursor.shape {
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
        .snapshot
        .placements
        .iter()
        .copied()
        .filter(|placement| (placement.z_index > 0) == foreground)
        .collect();
    placements.sort_by_key(|placement| placement.z_index);
    for placement in placements {
        if placement.alternate_screen != model.snapshot.modes.alternate_screen {
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
            let row = model.snapshot.scrollback.len() as i64 + i64::from(placement.row);
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

fn encode_keystroke(keystroke: &Keystroke, application_cursor: bool) -> Option<Vec<u8>> {
    let key = keystroke.key.as_str();
    let cursor = |normal: &'static [u8], application: &'static [u8]| {
        if application_cursor {
            application
        } else {
            normal
        }
        .to_vec()
    };
    let mut bytes = match key {
        "enter" => b"\r".to_vec(),
        "tab" if keystroke.modifiers.shift => b"\x1b[Z".to_vec(),
        "tab" => b"\t".to_vec(),
        "space" => b" ".to_vec(),
        "backspace" => vec![0x7f],
        "escape" => vec![0x1b],
        "up" => cursor(b"\x1b[A", b"\x1bOA"),
        "down" => cursor(b"\x1b[B", b"\x1bOB"),
        "right" => cursor(b"\x1b[C", b"\x1bOC"),
        "left" => cursor(b"\x1b[D", b"\x1bOD"),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "insert" => b"\x1b[2~".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "f3" => b"\x1bOR".to_vec(),
        "f4" => b"\x1bOS".to_vec(),
        "f5" => b"\x1b[15~".to_vec(),
        "f6" => b"\x1b[17~".to_vec(),
        "f7" => b"\x1b[18~".to_vec(),
        "f8" => b"\x1b[19~".to_vec(),
        "f9" => b"\x1b[20~".to_vec(),
        "f10" => b"\x1b[21~".to_vec(),
        "f11" => b"\x1b[23~".to_vec(),
        "f12" => b"\x1b[24~".to_vec(),
        _ if keystroke.modifiers.control && key.len() == 1 => {
            let byte = key.as_bytes()[0].to_ascii_uppercase();
            if byte.is_ascii_alphabetic() {
                vec![byte & 0x1f]
            } else {
                return None;
            }
        }
        _ => keystroke.key_char.as_ref()?.as_bytes().to_vec(),
    };
    if keystroke.modifiers.alt && key != "escape" {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

fn is_application_shortcut(keystroke: &Keystroke) -> bool {
    if !keystroke.modifiers.control {
        return false;
    }
    matches!(
        (keystroke.modifiers.shift, keystroke.key.as_str()),
        (false, "t" | "w" | "tab") | (true, "tab" | "c" | "v" | "p")
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
    button: MouseButton,
    release: bool,
    motion: bool,
    col: usize,
    row: usize,
    modifiers: gpui::Modifiers,
) -> Option<Vec<u8>> {
    let mut code = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::Navigate(_) => return None,
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
            for pixel in bytes.chunks_exact(3) {
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
    for pixel in raw.chunks_exact_mut(4) {
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
    sender: Sender<UiEvent>,
) {
    thread::spawn(move || {
        let stop = Arc::new(AtomicBool::new(false));
        loop {
            if stop.load(Ordering::Acquire) {
                return;
            }
            let result = run_tab_connection(tab_id, &session_id, cols, rows, stop.clone(), &sender);
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
    sender: &Sender<UiEvent>,
) -> crate::Result<()> {
    let mut client = app::connect_or_start(None)?;
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
    sender
        .send(UiEvent::TabConnected { tab_id, transport })
        .map_err(|_| "UI closed while attaching terminal")?;
    let mut reader = DaemonClient::from_parts(connection, next_request_id);
    loop {
        loop {
            match command_rx.try_recv() {
                Ok(message) => {
                    reader.send(message)?;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        match reader.poll_event()? {
            Some(ServerEvent::Screen(message)) => {
                if sender.send(UiEvent::TabScreen { tab_id, message }).is_err() {
                    return Ok(());
                }
            }
            Some(ServerEvent::Control { message, .. }) => {
                let detached = matches!(message, ServerMessage::Detached { .. });
                let exited = matches!(message, ServerMessage::SessionExited { .. });
                if exited {
                    stop.store(true, Ordering::Release);
                }
                if sender
                    .send(UiEvent::TabControl { tab_id, message })
                    .is_err()
                {
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

fn short_session_id(id: &str) -> String {
    id.rsplit('-')
        .next()
        .unwrap_or(id)
        .chars()
        .take(8)
        .collect()
}

fn log_startup_metric(elapsed: Duration) {
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
        .open(directory.join("client.log"))
    {
        let _ = writeln!(file, "first_snapshot_ms={}", elapsed.as_millis());
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
                    text: character.to_string(),
                    width: 1,
                    foreground: Color::Default,
                    background: Color::Default,
                    attributes: TextAttributes::default(),
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
            images: Vec::new(),
            placements: Vec::new(),
        }
    }

    #[test]
    fn maps_terminal_keys_without_stealing_ctrl_c() {
        let ctrl_c = Keystroke {
            modifiers: gpui::Modifiers {
                control: true,
                ..Default::default()
            },
            key: "c".into(),
            key_char: None,
        };
        assert_eq!(encode_keystroke(&ctrl_c, false), Some(vec![3]));
        let space = Keystroke {
            key: "space".into(),
            ..Default::default()
        };
        assert_eq!(encode_keystroke(&space, false), Some(b" ".to_vec()));
        let up = Keystroke {
            key: "up".into(),
            ..Default::default()
        };
        assert_eq!(encode_keystroke(&up, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode_keystroke(&up, true), Some(b"\x1bOA".to_vec()));
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
    fn extracts_wrapped_and_multiline_selection() {
        let snapshot = snapshot(vec![row("abcd", true)], vec![row("efgh", false)]);
        let mut mirror = ScreenMirror::default();
        mirror.apply(ScreenMessage::Snapshot { snapshot });
        let tab = TerminalTab {
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
            cols: 4,
            rows: 1,
        };
        assert_eq!(selected_text(&tab).as_deref(), Some("bcdefg"));
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
