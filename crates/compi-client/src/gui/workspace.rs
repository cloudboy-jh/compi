use super::*;
use gpui::{AnyElement, AnyWindowHandle, WindowBackgroundAppearance, WindowHandle};
use sha2::{Digest, Sha256};
pub(in crate::gui) mod arrangements;
pub(super) mod catalog;
pub(in crate::gui) mod dialogs;
pub(super) mod media;
pub(in crate::gui) mod performance;
pub(in crate::gui) mod prompt;
pub(in crate::gui) mod settings;
pub(super) mod tree;
mod tree_ui;
mod whats_new;
static PREPARED_HANDOFF: LazyLock<Mutex<Option<std::path::PathBuf>>> =
    LazyLock::new(|| Mutex::new(None));

pub(super) fn prepare_update_handoff(
    path: &std::path::Path,
    rollback_path: &std::path::Path,
    version: &str,
    cx: &mut App,
) -> crate::Result<()> {
    if PREPARED_HANDOFF
        .lock()
        .map_err(|_| "Update handoff lock unavailable")?
        .as_deref()
        .is_some_and(|prepared| prepared != path)
    {
        return Err("Another update attempt already owns this GUI host".into());
    }
    let mut windows = Vec::new();
    let mut connection_guards = Vec::new();
    let result = (|| -> crate::Result<()> {
        for handle in cx.windows() {
            if let Some(typed) = handle.downcast::<CompiApp>() {
                let saved = typed.update(cx, |this, window, _| -> crate::Result<_> {
                    if this.mutation_pending || this.daemon_restarting {
                        return Err("Wait for the current workspace change or daemon restart before installing".into());
                    }
                    this.remember_geometry(window);
                    this.flush_state();
                    if let Some(error) = this.state_save_error.lock().ok().and_then(|error| error.clone()) {
                        return Err(error.into());
                    }
                    let workspace = this.workspace.as_ref().ok_or("Cannot restore a window without an authoritative workspace")?;
                    this.update_quiesced.store(true, Ordering::Release);
                    let mut config = this.config.clone();
                    if !config.path.is_absolute() {
                        if config.path.as_os_str().is_empty() { return Err("Cannot retain an unavailable configuration path".into()); }
                        config.path = std::env::current_dir()?.join(&config.path);
                    }
                    Ok((crate::update_restore::RestoreWindow {
                        slot_id: this.slot_id.clone(), server_id: workspace.server_id.clone(),
                        target: this.target.clone(), config,
                    }, this.update_connection_guard.clone()))
                })??;
                windows.push(saved.0);
                connection_guards.push(saved.1);
            }
        }
        if windows.is_empty() {
            return Err("No Compi windows can be handed off".into());
        }
        let path = path.to_owned();
        let rollback_path = rollback_path.to_owned();
        let version = version.to_owned();
        *PREPARED_HANDOFF
            .lock()
            .map_err(|_| "Update handoff lock unavailable")? = Some(path.clone());
        thread::spawn(move || {
            let result = (|| -> crate::Result<()> {
                // Wait off-thread for any control-only reconnect already in flight.
                let _drained = connection_guards
                    .iter()
                    .map(|guard| {
                        guard
                            .lock()
                            .map_err(|_| "Update connection barrier unavailable")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let selected = PREPARED_HANDOFF
                    .lock()
                    .map_err(|_| "Update handoff lock unavailable")?;
                if selected.as_deref() != Some(path.as_path()) {
                    return Ok(());
                }
                for window in &mut windows {
                    window.config.updates = crate::config::load(
                        Some(&window.config.path),
                        crate::config::FontOverrides::default(),
                    )
                    .updates;
                }
                crate::update_restore::Handoff::create(
                    &rollback_path,
                    env!("CARGO_PKG_VERSION"),
                    windows.clone(),
                )?;
                crate::update_restore::Handoff::create(&path, &version, windows)
            })();
            if let Err(error) = result {
                eprintln!("Cannot save update handoff: {error}");
            }
        });
        Ok(())
    })();
    if result.is_err() {
        for handle in cx.windows() {
            if let Some(typed) = handle.downcast::<CompiApp>() {
                let _ = typed.update(cx, |this, _, _| {
                    this.update_quiesced.store(false, Ordering::Release)
                });
            }
        }
    }
    result
}

pub(super) fn release_for_update(path: &std::path::Path, cx: &mut App) -> crate::Result<()> {
    if PREPARED_HANDOFF
        .lock()
        .map_err(|_| "Update handoff lock unavailable")?
        .as_deref()
        != Some(path)
    {
        return Err("Update release does not match the prepared handoff".into());
    }
    for handle in cx.windows() {
        if let Some(typed) = handle.downcast::<CompiApp>() {
            typed.update(cx, |this, _, _| {
                this.update_quiesced.store(true, Ordering::Release);
                for view in &mut this.surface_views {
                    view.stop.store(true, Ordering::Release);
                    if let Some(transport) = view.transport.take() {
                        transport.close();
                    }
                }
                this.flush_state();
            })?;
        }
    }
    cx.quit();
    Ok(())
}

pub(super) fn abort_update(path: &std::path::Path, cx: &mut App) -> crate::Result<()> {
    let mut prepared = PREPARED_HANDOFF
        .lock()
        .map_err(|_| "Update handoff lock unavailable")?;
    if prepared.as_deref() == Some(path) {
        *prepared = None;
        for handle in cx.windows() {
            if let Some(typed) = handle.downcast::<CompiApp>() {
                typed.update(cx, |this, _, cx| {
                    this.update_quiesced.store(false, Ordering::Release);
                    this.sync_visible_views();
                    cx.notify();
                })?;
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
pub(super) struct TransferSeed {
    tab_id: TabId,
    source_state: ClientState,
    display_appearance: AppearanceSettings,
    display_sidebar_width: f32,
    display_zoom: f32,
}

fn inherited_appearance(
    config: &LoadedConfig,
    overrides: &WindowAppearanceOverrides,
) -> AppearanceSettings {
    let cli = config.provenance.theme == crate::config::ValueSource::CommandLine;
    AppearanceSettings {
        theme: if cli {
            config.appearance.theme.clone()
        } else {
            overrides
                .theme
                .clone()
                .unwrap_or_else(|| config.configured_appearance.theme.clone())
        },
        terminal_theme: if cli {
            config.appearance.terminal_theme.clone()
        } else {
            overrides
                .terminal_theme
                .clone()
                .unwrap_or_else(|| config.configured_appearance.terminal_theme.clone())
        },
        terminal_theme_override: !cli
            && overrides
                .terminal_theme_override
                .unwrap_or(config.configured_appearance.terminal_theme_override),
        transparent_background: overrides
            .transparent_background
            .unwrap_or(config.configured_appearance.transparent_background),
        terminal_opacity: overrides
            .terminal_opacity
            .unwrap_or(config.configured_appearance.terminal_opacity),
        background_effect: overrides
            .background_effect
            .unwrap_or(config.configured_appearance.background_effect),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AppearanceField {
    TerminalOverride,
    Transparency,
    Effect,
}

fn commit_override<T: Clone + PartialEq>(
    slot: &mut Option<T>,
    previous: &T,
    next: &T,
    global: bool,
    forced: bool,
) {
    if forced || previous != next {
        *slot = if global { None } else { Some(next.clone()) };
    }
}

fn commit_appearance_overrides(
    overrides: &mut WindowAppearanceOverrides,
    previous: &AppearanceSettings,
    next: &AppearanceSettings,
    global: bool,
    colors_locked: bool,
    field: Option<AppearanceField>,
) {
    if !colors_locked {
        commit_override(
            &mut overrides.theme,
            &previous.theme,
            &next.theme,
            global,
            false,
        );
        commit_override(
            &mut overrides.terminal_theme,
            &previous.terminal_theme,
            &next.terminal_theme,
            global,
            false,
        );
        commit_override(
            &mut overrides.terminal_theme_override,
            &previous.terminal_theme_override,
            &next.terminal_theme_override,
            global,
            field == Some(AppearanceField::TerminalOverride),
        );
    }
    commit_override(
        &mut overrides.terminal_opacity,
        &previous.terminal_opacity,
        &next.terminal_opacity,
        global,
        false,
    );
    commit_override(
        &mut overrides.transparent_background,
        &previous.transparent_background,
        &next.transparent_background,
        global,
        field == Some(AppearanceField::Transparency),
    );
    commit_override(
        &mut overrides.background_effect,
        &previous.background_effect,
        &next.background_effect,
        global,
        field == Some(AppearanceField::Effect),
    );
}

fn material_opacity(transparent: bool, opacity: f32, native_available: bool) -> f32 {
    if transparent && native_available {
        opacity
    } else {
        1.0
    }
}

#[derive(Clone)]
pub(super) enum TextPurpose {
    CreateWorkspace,
    RenameWorkspace(SessionId),
    RenameTab(TabId),
    SaveArrangement(TabId),
}

const STALE_TAB_MENU: &str =
    "Workspace changed while the terminal menu was open. Reopen it to review the current panes.";

#[derive(Clone)]
pub(super) enum Overlay {
    Palette,
    PaneActions,
    /// Tab menus bind their target tab and the workspace `structure_key`, so size and
    /// status observations never invalidate them; hierarchy changes do.
    TabActions {
        position: Point<Pixels>,
        tab_id: TabId,
        structure: u64,
    },
    TabPaneActions {
        position: Point<Pixels>,
        tab_id: TabId,
        pane_id: PaneId,
        structure: u64,
    },
    HeaderActions {
        position: Point<Pixels>,
    },
    QuickAppearance,
    Settings,
    ThemeCatalog,
    ImageInspector,
    Text {
        purpose: TextPurpose,
        title: &'static str,
    },
    Confirm {
        operation: WorkspaceMutation,
        title: String,
        revision: u64,
    },
    Workspaces,
    ConfirmDaemonRestart {
        details: String,
        revision: u64,
        consent: compi_protocol::LifecycleConsent,
    },
    Tabs {
        hidden_only: bool,
    },
    Windows,
    Diagnostics,
    /// Bound to one tab; mirror/flip apply to whichever preset is selected.
    Arrangements {
        tab_id: TabId,
        transform: crate::arrangement::Transform,
        confirm_delete: Option<String>,
        /// Other tabs in this workspace to merge into `tab_id`, in tab order.
        merge: Vec<TabId>,
    },
}

pub(super) const MODAL_SCRIM_OPACITY: f32 = 0.72;

pub(super) fn overlay_viewport_size(window: &Window) -> (f32, f32) {
    let viewport = window.viewport_size();
    (f32::from(viewport.width), f32::from(viewport.height))
}

fn display_index_for_bounds(
    window_bounds: Bounds<Pixels>,
    displays: impl IntoIterator<Item = Bounds<Pixels>>,
) -> Option<usize> {
    let center = window_bounds.center();
    displays
        .into_iter()
        .position(|display_bounds| display_bounds.contains(&center))
}

#[cfg(windows)]
fn restore_native_window_geometry(
    window: &mut Window,
    geometry: &WindowGeometry,
    display_index: usize,
) {
    use windows::{
        Win32::{
            Foundation::{LPARAM, RECT},
            Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR},
            UI::{
                HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
                WindowsAndMessaging::{
                    GetWindowRect, SW_MAXIMIZE, SW_RESTORE, SWP_NOACTIVATE, SWP_NOSIZE,
                    SWP_NOZORDER, SetWindowPos, ShowWindow,
                },
            },
        },
        core::BOOL,
    };

    unsafe extern "system" fn collect_monitor(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        monitors: LPARAM,
    ) -> BOOL {
        let monitors = unsafe { &mut *(monitors.0 as *mut Vec<HMONITOR>) };
        monitors.push(monitor);
        true.into()
    }

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = HWND(handle.hwnd.get() as *mut core::ffi::c_void);
    let mut monitors = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut monitors as *mut _ as isize),
        );
    }
    let Some(&monitor) = monitors.get(display_index) else {
        return;
    };
    let (mut dpi_x, mut dpi_y) = (0, 0);
    if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_err()
        || dpi_x == 0
    {
        return;
    }
    let scale = dpi_x as f32 / 96.0;
    let x = (geometry.x * scale).round() as i32;
    let y = (geometry.y * scale).round() as i32;
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    window.resize(size(px(geometry.width), px(geometry.height)));
    let mut outer = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut outer) }.is_ok() {
        let current = window.window_bounds().get_bounds();
        let border_x = (f32::from(current.origin.x) * scale).round() as i32 - outer.left;
        let border_y = (f32::from(current.origin.y) * scale).round() as i32 - outer.top;
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                None,
                x - border_x,
                y - border_y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
    if geometry.maximized {
        unsafe {
            let _ = ShowWindow(hwnd, SW_MAXIMIZE);
        }
    }
}

pub(super) struct DividerDrag {
    revision: u64,
    index: usize,
    tree: LayoutNode,
}

/// Height of a floating pane's title strip, above its terminal body.
const FLOAT_TITLE_HEIGHT: f32 = 28.0;
const FLOAT_RESIZE_HANDLE: f32 = 6.0;
/// Transparent drag zone around a one-device-pixel seam (split or sidebar).
const SEAM_GRAB: f32 = 8.0;

/// A floating pane's frame (title strip included) and terminal body, in
/// terminal-area coordinates. Floats are outside the scrolled split canvas.
pub(super) struct FloatLayout {
    frame: layout::Rect,
    pane: layout::PaneLayout,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FloatDragMode {
    Move,
    Right,
    Bottom,
    Corner,
}

pub(super) struct FloatDrag {
    pane_id: PaneId,
    mode: FloatDragMode,
    origin: Point<Pixels>,
    start: layout::Rect,
    /// Placement before the drag; Escape restores it.
    before: FloatRect,
}

pub(super) fn open_compi_window(
    target: ConnectionTarget,
    initial_working_directory: Option<String>,
    config: LoadedConfig,
    transferred_seed: Option<TransferSeed>,
    cx: &mut App,
) -> crate::Result<WindowHandle<CompiApp>> {
    open_window_mode(
        target,
        initial_working_directory,
        config,
        transferred_seed,
        None,
        cx,
    )
}

pub(super) fn open_restore_window(
    saved: crate::update_restore::RestoreWindow,
    session: Arc<crate::update_restore::RestoreSession>,
    cx: &mut App,
) -> crate::Result<WindowHandle<CompiApp>> {
    open_window_mode(
        saved.target.clone(),
        None,
        saved.config.clone(),
        None,
        Some((saved, session)),
        cx,
    )
}

fn open_window_mode(
    target: ConnectionTarget,
    initial_working_directory: Option<String>,
    config: LoadedConfig,
    transferred_seed: Option<TransferSeed>,
    restored: Option<(
        crate::update_restore::RestoreWindow,
        Arc<crate::update_restore::RestoreSession>,
    )>,
    cx: &mut App,
) -> crate::Result<WindowHandle<CompiApp>> {
    let started_at = Instant::now();
    let mut client = target.connect()?;
    let mut snapshot = client.workspace()?;
    let target_sessions = if restored.is_some() {
        0
    } else {
        perf::target_session_count()
    };
    while snapshot.surfaces.len() < target_sessions {
        client.create_surface(DEFAULT_COLS, DEFAULT_ROWS, None)?;
        snapshot = client.workspace()?;
    }
    let defaults = ClientState {
        sidebar_width: config.configured_sidebar_width,
        ..ClientState::default()
    };
    let state_instance = target.state_instance();
    let mut slot = if let Some((saved, _)) = &restored {
        StateSlot::claim_exact(
            state_instance.as_deref(),
            &saved.server_id,
            &defaults,
            &saved.slot_id,
        )?
    } else {
        StateSlot::claim(state_instance.as_deref(), &snapshot.server_id, &defaults)?
    };
    slot.state.reconcile(None, &snapshot);
    if let Some(seed) = &transferred_seed
        && !slot
            .state
            .initialize_transfer(&snapshot, &seed.tab_id, &seed.source_state)
    {
        return Err("The transferred terminal tab no longer exists".into());
    }
    let restored_geometry = slot.state.geometry.clone();
    let displays = cx.displays();
    let restored_display_index = restored_geometry.as_ref().and_then(|geometry| {
        let bounds = Bounds::new(
            point(px(geometry.x), px(geometry.y)),
            size(px(geometry.width), px(geometry.height)),
        );
        display_index_for_bounds(bounds, displays.iter().map(|display| display.bounds()))
    });
    let display_id =
        restored_display_index.and_then(|index| displays.get(index).map(|display| display.id()));
    let bounds = restored_geometry
        .as_ref()
        .map(|geometry| {
            let bounds = Bounds::new(
                point(px(geometry.x), px(geometry.y)),
                size(px(geometry.width), px(geometry.height)),
            );
            if geometry.maximized {
                WindowBounds::Maximized(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            }
        })
        .unwrap_or_else(|| {
            WindowBounds::Windowed(Bounds::centered(None, size(px(960.0), px(640.0)), cx))
        });
    let window = cx.open_window(
        WindowOptions {
            window_bounds: Some(bounds),
            display_id,
            window_min_size: Some(size(px(420.0), px(280.0))),
            window_background: if native_material_available() {
                WindowBackgroundAppearance::Blurred
            } else {
                WindowBackgroundAppearance::Opaque
            },
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
                    target,
                    initial_working_directory,
                    config,
                    defaults,
                    slot,
                    snapshot,
                    transferred_seed,
                    restored.map(|(_, session)| session),
                    window,
                    cx,
                )
            })
        },
    )?;
    #[cfg(windows)]
    if let (Some(geometry), Some(display_index)) =
        (restored_geometry.as_ref(), restored_display_index)
    {
        window.update(cx, |_, window, _| {
            restore_native_window_geometry(window, geometry, display_index);
        })?;
    }
    window.update(cx, |view, window, cx| {
        window.focus(&view.focus_handle);
        cx.activate(true);
    })?;
    if perf::enabled() {
        let first_frame_started_at = started_at;
        window.update(cx, |_, window, _| {
            window.on_next_frame(move |_, _| {
                log_startup_metric("first_window_frame_ms", first_frame_started_at.elapsed());
            });
        })?;
    }
    Ok(window)
}

impl Drop for CompiApp {
    fn drop(&mut self) {
        self.flush_state();
        for view in &self.surface_views {
            view.stop.store(true, Ordering::Release);
            if let Some(transport) = &view.transport {
                transport.close();
            }
            if let Ok(mut routes) = EVENT_ROUTES.lock() {
                routes.remove(&view.id);
            }
        }
        // Application exit must not outrun the last durable state write.
        while self.state_writes.lock().is_ok_and(|pending| pending.1) {
            thread::sleep(Duration::from_millis(2));
        }
    }
}

impl CompiApp {
    #[allow(clippy::too_many_arguments)]
    fn new(
        started_at: Instant,
        target: ConnectionTarget,
        initial_working_directory: Option<String>,
        config: LoadedConfig,
        defaults: ClientState,
        slot: StateSlot,
        snapshot: WorkspaceSnapshot,
        transferred_seed: Option<TransferSeed>,
        restore_session: Option<Arc<crate::update_restore::RestoreSession>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (tx, rx) = async_channel::bounded(128);
        let state = slot.state.clone();
        let (theme_library, library_diagnostics) = ThemeLibrary::load();
        let inherited = inherited_appearance(&config, &state.appearance);
        let initial_appearance = transferred_seed
            .as_ref()
            .map(|seed| seed.display_appearance.clone())
            .unwrap_or(inherited);
        let theme = theme_library
            .resolve(&initial_appearance.theme)
            .unwrap_or_else(|| theme_library.fallback());
        let terminal_theme = theme_library
            .resolve(initial_appearance.effective_terminal_theme())
            .unwrap_or_else(|| theme_library.fallback());
        let ui_font = crate::font_catalog::resolve_ui_font(config.ui_font, window.text_system());
        let terminal_opacity = initial_appearance.terminal_opacity;
        let background_effect = initial_appearance.background_effect;
        let sidebar_width = transferred_seed
            .as_ref()
            .map(|seed| seed.display_sidebar_width)
            .unwrap_or_else(|| {
                if config.provenance.sidebar_width == crate::config::ValueSource::CommandLine {
                    config.sidebar_width
                } else {
                    state.sidebar_width
                }
            });
        let slot_id = slot.id().to_owned();
        let slot_diagnostics = slot.diagnostics.clone();
        let zoom = transferred_seed
            .as_ref()
            .map_or(state.font_zoom, |seed| seed.display_zoom);
        let typography = Arc::new(TerminalTypography::resolve(&config.font, zoom, window));
        let mut appearance_diagnostics = config.diagnostics.clone();
        appearance_diagnostics.extend(library_diagnostics);
        for id in [
            &initial_appearance.theme,
            initial_appearance.effective_terminal_theme(),
        ] {
            if theme_library.resolve(id).is_none() {
                appearance_diagnostics.push(format!("Theme '{}' is unavailable; using Compi Neutral without changing the saved selection.", id.id()));
            }
        }
        let global_warning = diagnostic_warning(&appearance_diagnostics, &typography.diagnostics);
        let performance_enabled = Arc::new(AtomicBool::new(state.show_fps));
        let updates = crate::updates::shared(&config, &target);
        let attach_only = restore_session.is_some();
        let mut this = Self {
            started_at,
            target,
            updates,
            update_quiesced: Arc::new(AtomicBool::new(false)),
            update_connection_guard: Arc::new(Mutex::new(())),
            restore_session,
            restore_ready: false,
            initial_working_directory,
            first_snapshot_logged: false,
            ready_probe_marker: (!attach_only && perf::ready_probe_enabled())
                .then(|| format!("COMPI_READY_{}", std::process::id())),
            ready_probe_sent_at: None,
            ready_probe_render_pending: false,
            ready_probe_logged: false,
            pending_present_latency_ids: Vec::new(),
            window_title: "Compi".into(),
            focus_handle: cx.focus_handle(),
            ime_text: String::new(),
            ime_marked_range: None,
            ime_selected_range: 0..0,
            surface_views: Vec::new(),
            surface_names: HashMap::new(),
            focused_view: None,
            file_tree: None,
            tree_scroll: ScrollHandle::new(),
            tab_scroll_handle: ScrollHandle::new(),
            workspace_scroll: ScrollHandle::new(),
            sidebar_scroll: ScrollHandle::new(),
            sidebar_open: false,
            whats_new_expanded: false,
            settings_whats_new_expanded: false,
            settings_update_notes_expanded: false,
            settings_update_advanced: false,
            update_dot: false,
            updates_page_open: false,
            sidebar_width,
            sidebar_drag: false,
            workspace_scroll_drag: None,
            expanded_workspaces: HashSet::new(),
            tab_drag_origin: None,
            dragging_tab: None,
            drag_position: None,
            titlebar_drag: false,
            workspace: Some(snapshot),
            state_slot: Some(Arc::new(Mutex::new(slot))),
            slot_id,
            state_writes: Arc::new(Mutex::new((None, false))),
            state_save_error: Arc::new(Mutex::new(None)),
            state,
            defaults,
            glass: terminal_opacity < 1.0,
            theme,
            terminal_theme,
            terminal_theme_override: initial_appearance.terminal_theme_override,
            transparent_background: initial_appearance.transparent_background,
            theme_library,
            ui_font,
            theme_catalog: None,
            pending_appearance_reload: None,
            pending_image_inputs: HashSet::new(),
            image_previews: VecDeque::new(),
            image_inspector: None,
            image_notice: None,
            rendered_images: HashMap::new(),
            rendered_image_ids: HashSet::new(),
            terminal_opacity,
            opacity_drag_origin: None,
            background_effect,
            settings_scope: SettingsScope::Global,
            daemon_restarting: false,
            zoom,
            overlay_return: None,
            overlay: None,
            overlay_index: 0,
            overlay_scroll: ScrollHandle::new(),
            overlay_focus: 0,
            overlay_generation: 0,
            overlay_closing_since: None,
            settings_section: SettingsSection::Appearance,
            settings_font_picker: None,
            settings_scroll_bounds: None,
            settings_scroll_to_focus: false,
            prompt_settings: prompt::PromptSettingsState::default(),
            performance_enabled: performance_enabled.clone(),
            performance: performance::PerformanceMonitor::default(),
            performance_notice: None,
            overlay_structure: None,
            pane_zoom: PaneZoomState::default(),
            layout: None,
            divider_drag: None,
            preview_layout: None,
            last_resize: Instant::now() - Duration::from_secs(1),
            loading_surfaces: false,
            zoom_layout: None,
            float_layouts: Vec::new(),
            float_area: layout::Size::default(),
            float_drag: None,
            mutation_pending: false,
            global_error: None,
            connection_error: None,
            font_settings: config.font.clone(),
            typography,
            typography_scale: window.scale_factor(),
            config_diagnostics: appearance_diagnostics,
            global_warning,
            config,
            event_tx: UiEventSender(tx),
            subscriptions: Vec::new(),
            terminal_cols: DEFAULT_COLS,
            terminal_rows: DEFAULT_ROWS,
            transferred_seed,
        };
        if let Some(workspace) = &this.workspace {
            this.global_error = workspace.recovery_message.clone();
            this.expanded_workspaces
                .extend(this.state.selected_session.clone());
        }
        if !slot_diagnostics.is_empty() {
            this.global_warning = Some(slot_diagnostics.join("\n"));
        }
        this.apply_window_background(window);
        this.rebuild_layout(window, true);
        if this.transferred_seed.is_none() {
            this.sync_visible_views();
        }
        this.subscriptions
            .push(cx.on_release(|this, _| this.flush_state()));
        this.subscriptions.push(cx.on_app_quit(|this, _| {
            this.flush_state();
            async {}
        }));
        let closing_view = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = closing_view.update(cx, |this, _| this.flush_state());
            true
        });
        this.subscriptions
            .push(cx.observe_window_bounds(window, |this, window, cx| {
                this.remember_geometry(window);
                this.rebuild_layout(window, false);
                this.save_state();
                cx.notify();
            }));
        this.subscriptions
            .push(cx.observe_window_activation(window, |this, window, cx| {
                this.report_focus(window.is_window_active() && this.overlay.is_none());
                cx.notify();
            }));
        cx.spawn(async move |weak, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                if weak
                    .update(cx, |this, cx| {
                        if this.restore_session.is_some() && !this.restore_ready {
                            this.acknowledge_update_restore();
                            cx.notify();
                        } else {
                            this.poll_updates(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |weak, cx| {
            while let Ok(first) = rx.recv().await {
                let started = Instant::now();
                if weak
                    .update(cx, |this, cx| {
                        this.handle_event(first, cx);
                        while started.elapsed() < UI_EVENT_BUDGET {
                            let Ok(event) = rx.try_recv() else {
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
        if perf::enabled() {
            let metrics_view = cx.entity().downgrade();
            cx.spawn(async move |_, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(6)).await;
                    if metrics_view
                        .update(cx, |this, _| {
                            let sessions = this
                                .workspace
                                .as_ref()
                                .map_or(0, |workspace| workspace.surfaces.len());
                            perf::log_resource_sample("client", "terminal", sessions);
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
        }
        // A control-only subscription keeps hidden work and empty windows authoritative.
        let sender = this.event_tx.clone();
        let target = this.target.clone();
        let appearance_path = this.config.path.clone();
        let performance_enabled = this.performance_enabled.clone();
        let mut polling_theme_library = this.theme_library.clone();
        let alive = cx.entity().downgrade();
        let quiesced = this.update_quiesced.clone();
        let connection_guard = this.update_connection_guard.clone();
        cx.spawn(async move |_, cx| {
            let mut appearance_stamp = fs::metadata(&appearance_path)
                .ok()
                .and_then(|metadata| Some((metadata.modified().ok()?, metadata.len())));
            let mut library_ticks = 0_u8;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                if alive.upgrade().is_none() {
                    break;
                }
                if quiesced.load(Ordering::Acquire) {
                    continue;
                }
                let sender = sender.clone();
                let target = target.clone();
                let appearance_path = appearance_path.clone();
                let previous_stamp = appearance_stamp;
                library_ticks = (library_ticks + 1) % 12;
                let reload_library = library_ticks == 0;
                let collect_performance = performance_enabled.load(Ordering::Acquire);
                let quiesced = quiesced.clone();
                let connection_guard = connection_guard.clone();
                (appearance_stamp, polling_theme_library) = cx
                    .background_executor()
                    .spawn(async move {
                        let _guard = connection_guard
                            .lock()
                            .expect("Update connection barrier poisoned");
                        if quiesced.load(Ordering::Acquire) {
                            return (previous_stamp, polling_theme_library);
                        }
                        match target.connect() {
                            Ok(mut client) => {
                                sender.send(UiEvent::SurfacesLoaded(
                                    client.workspace().map_err(|error| error.to_string()),
                                ));
                                if collect_performance {
                                    let sample = client.runtime_metrics().map(|daemon| {
                                        performance::PerformanceSample {
                                            sampled_at: Instant::now(),
                                            render: render_performance_snapshot(),
                                            client: perf::process_metrics(),
                                            daemon,
                                        }
                                    });
                                    sender.send(UiEvent::PerformanceSample(
                                        sample.map_err(|error| error.to_string()),
                                    ));
                                }
                            }
                            Err(error) => {
                                let error = error.to_string();
                                sender.send(UiEvent::SurfacesLoaded(Err(error.clone())));
                                if collect_performance {
                                    sender.send(UiEvent::PerformanceSample(Err(error)));
                                }
                            }
                        }
                        let stamp = fs::metadata(&appearance_path)
                            .ok()
                            .and_then(|metadata| Some((metadata.modified().ok()?, metadata.len())));
                        if reload_library || (stamp.is_some() && stamp != previous_stamp) {
                            let diagnostics = polling_theme_library.reload();
                            sender.send(UiEvent::ThemeLibraryReloaded {
                                library: polling_theme_library.clone(),
                                diagnostics,
                            });
                        }
                        if stamp.is_some() && stamp != previous_stamp {
                            let loaded = crate::config::load(
                                Some(&appearance_path),
                                crate::config::FontOverrides::default(),
                            );
                            sender.send(UiEvent::AppearanceReloaded {
                                appearance: loaded.configured_appearance,
                                favorites: loaded.theme_favorites,
                                ui_font: loaded.ui_font,
                                terminal_font_family: loaded.configured_font.family,
                                diagnostics: loaded.diagnostics,
                            });
                        }
                        (stamp, polling_theme_library)
                    })
                    .await;
            }
        })
        .detach();
        if this.restore_session.is_none()
            && !this
                .workspace
                .as_ref()
                .is_some_and(|workspace| workspace.initialized)
        {
            let working_directory = this.initial_working_directory.take();
            this.mutate(
                WorkspaceMutation::Initialize {
                    cols: this.terminal_cols,
                    rows: this.terminal_rows,
                    working_directory,
                },
                true,
            );
        } else if this.restore_session.is_none()
            && let Some(cwd) = this.initial_working_directory.take()
        {
            this.create_terminal(Some(cwd));
        }
        this
    }
    fn acknowledge_update_restore(&mut self) {
        if self.restore_ready {
            return;
        }
        let Some(session) = &self.restore_session else {
            return;
        };
        let Some(workspace) = &self.workspace else {
            return;
        };
        let mut selected = Vec::new();
        if let Some(tab) = self.selected_tab() {
            collect_leaves(&tab.layout, &mut selected);
        }
        let attached = selected.iter().all(|(_, id)| {
            workspace.surface(id).is_some_and(|surface| {
                !matches!(
                    surface.status,
                    SurfaceStatus::Starting | SurfaceStatus::Running
                ) || self.surface_views.iter().any(|view| {
                    &view.surface_id == id
                        && view.lifetime == surface.process_lifetime_id
                        && view.transport.is_some()
                        && view.mirror.snapshot().is_some()
                        && matches!(view.state, ConnectionState::Attached)
                })
            })
        });
        if !attached {
            return;
        }
        let result = session.mark_ready(&self.slot_id).and_then(|all_ready| {
            if all_ready {
                let (path, token) = crate::updates::readiness_receipt()
                    .ok_or("Update restore has no private readiness receipt")?;
                session.publish_readiness(&path, &token)?;
            }
            Ok(())
        });
        match result {
            Ok(()) => self.restore_ready = true,
            Err(error) => {
                self.global_error = Some(format!("Cannot acknowledge update restore: {error}"))
            }
        }
    }

    fn colors(&self) -> &ThemeColors {
        self.theme.colors()
    }

    pub(super) fn resolve_theme(&self, id: &ThemeId) -> Arc<ThemeDefinition> {
        self.theme_library
            .resolve(id)
            .unwrap_or_else(|| self.theme_library.fallback())
    }

    fn effective_background_opacity(&self) -> f32 {
        material_opacity(
            self.transparent_background,
            self.terminal_opacity,
            native_material_available(),
        )
    }

    fn apply_window_background(&mut self, window: &mut Window) {
        let translucent = self.effective_background_opacity() < 1.0;
        let appearance = if !translucent {
            WindowBackgroundAppearance::Opaque
        } else {
            match self.background_effect {
                BackgroundEffect::Clear => WindowBackgroundAppearance::Transparent,
                BackgroundEffect::Blurred => WindowBackgroundAppearance::Blurred,
                BackgroundEffect::Opaque => WindowBackgroundAppearance::Opaque,
            }
        };
        self.glass = translucent;
        window.set_background_appearance(appearance);
    }

    fn preview_terminal_opacity(&mut self, opacity: f32, window: &mut Window) {
        let was_translucent = self.effective_background_opacity() < 1.0;
        self.opacity_drag_origin
            .get_or_insert(self.terminal_opacity);
        self.terminal_opacity = opacity.clamp(
            crate::config::MIN_TERMINAL_OPACITY,
            crate::config::MAX_TERMINAL_OPACITY,
        );
        if was_translucent != (self.effective_background_opacity() < 1.0) {
            self.apply_window_background(window);
        }
    }

    pub(super) fn accepted_appearance(&self) -> AppearanceSettings {
        let mut appearance = self
            .theme_catalog
            .as_ref()
            .map(|catalog| catalog.original.clone())
            .unwrap_or_else(|| inherited_appearance(&self.config, &self.state.appearance));
        if let Some(opacity) = self.opacity_drag_origin {
            appearance.terminal_opacity = opacity;
        }
        appearance
    }

    fn scoped_appearance(&self) -> AppearanceSettings {
        match self.settings_scope {
            SettingsScope::Global => self.config.configured_appearance.clone(),
            SettingsScope::Window => self.accepted_appearance(),
        }
    }

    fn apply_scoped_appearance(
        &mut self,
        appearance: AppearanceSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_scoped_appearance_field(appearance, None, window, cx);
    }

    fn apply_scoped_appearance_field(
        &mut self,
        mut appearance: AppearanceSettings,
        field: Option<AppearanceField>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = self.scoped_appearance();
        let colors_locked = self.config.provenance.theme == crate::config::ValueSource::CommandLine;
        if colors_locked {
            appearance.theme = previous.theme.clone();
            appearance.terminal_theme = previous.terminal_theme.clone();
            appearance.terminal_theme_override = previous.terminal_theme_override;
        }
        let global = self.settings_scope == SettingsScope::Global;
        if global && let Err(error) = self.config.save_global_appearance(appearance.clone()) {
            self.global_error = Some(error);
            return;
        }
        commit_appearance_overrides(
            &mut self.state.appearance,
            &previous,
            &appearance,
            global,
            colors_locked,
            field,
        );
        let effective = inherited_appearance(&self.config, &self.state.appearance);
        self.set_theme(self.resolve_theme(&effective.theme));
        self.set_terminal_theme(self.resolve_theme(effective.effective_terminal_theme()));
        self.terminal_opacity = effective.terminal_opacity;
        self.background_effect = effective.background_effect;
        self.terminal_theme_override = effective.terminal_theme_override;
        self.transparent_background = effective.transparent_background;
        self.apply_window_background(window);
        self.save_state();
        if self.settings_scope == SettingsScope::Global {
            self.broadcast_global_appearance(window, cx);
        }
    }

    fn reset_window_appearance(&mut self, window: &mut Window) {
        self.state.appearance = WindowAppearanceOverrides::default();
        let effective = inherited_appearance(&self.config, &self.state.appearance);
        self.set_theme(self.resolve_theme(&effective.theme));
        self.set_terminal_theme(self.resolve_theme(effective.effective_terminal_theme()));
        self.terminal_opacity = self.config.configured_appearance.terminal_opacity;
        self.background_effect = self.config.configured_appearance.background_effect;
        self.terminal_theme_override = effective.terminal_theme_override;
        self.transparent_background = effective.transparent_background;
        self.apply_window_background(window);
        self.save_state();
    }

    pub(super) fn set_transparent_background(
        &mut self,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut appearance = self.scoped_appearance();
        appearance.transparent_background = enabled;
        self.apply_scoped_appearance_field(
            appearance,
            Some(AppearanceField::Transparency),
            window,
            cx,
        );
    }

    pub(super) fn set_blur_background(
        &mut self,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut appearance = self.scoped_appearance();
        appearance.background_effect = if enabled {
            BackgroundEffect::Blurred
        } else {
            BackgroundEffect::Clear
        };
        self.apply_scoped_appearance_field(appearance, Some(AppearanceField::Effect), window, cx);
    }

    pub(super) fn follow_theme_terminal_colors(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.config.provenance.theme == crate::config::ValueSource::CommandLine {
            return;
        }
        let mut appearance = self.scoped_appearance();
        appearance.terminal_theme_override = false;
        self.apply_scoped_appearance_field(
            appearance,
            Some(AppearanceField::TerminalOverride),
            window,
            cx,
        );
    }

    fn selected_tab(&self) -> Option<&WorkspaceTab> {
        self.state.selected_tab(self.workspace.as_ref()?)
    }
    fn zoomed_pane(&self) -> Option<&PaneId> {
        let tab = self.selected_tab()?;
        self.pane_zoom.pane(&tab.id)
    }

    fn pane_zoomed(&self) -> bool {
        self.zoomed_pane().is_some()
    }

    fn visible_layout(&self) -> Option<&WorkspaceLayout> {
        self.zoom_layout.as_ref().or(self.layout.as_ref())
    }

    fn reconcile_pane_zoom(&mut self, workspace: &WorkspaceSnapshot) {
        self.pane_zoom.retain_valid(|tab_id, pane_id| {
            workspace
                .sessions
                .iter()
                .flat_map(|session| &session.tabs)
                .find(|tab| &tab.id == tab_id)
                .is_some_and(|tab| layout_contains_pane(&tab.layout, pane_id))
        });
    }

    fn remember_geometry(&mut self, window: &Window) {
        let bounds = window.window_bounds().get_bounds();
        self.state.geometry = Some(WindowGeometry {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            width: f32::from(bounds.size.width),
            height: f32::from(bounds.size.height),
            maximized: window.is_maximized(),
        });
    }

    pub(super) fn save_state(&mut self) {
        self.capture_viewports();
        let Some(slot) = self.state_slot.clone() else {
            return;
        };
        let queue = self.state_writes.clone();
        if let Ok(mut pending) = queue.lock() {
            pending.0 = Some(self.state.clone());
            if pending.1 {
                return;
            }
            pending.1 = true;
        } else {
            self.global_error = Some("Window state write queue is unavailable".into());
            return;
        }
        let sender = self.event_tx.clone();
        let error_slot = self.state_save_error.clone();
        thread::spawn(move || {
            // Coalesce resize/navigation bursts; the exclusively owned slot remains
            // alive until its final queued state reaches the atomic file writer.
            thread::sleep(Duration::from_millis(160));
            loop {
                let state = match queue.lock() {
                    Ok(mut pending) => match pending.0.take() {
                        Some(state) => state,
                        None => {
                            pending.1 = false;
                            return;
                        }
                    },
                    Err(_) => return,
                };
                let result = slot
                    .lock()
                    .map_err(|_| "Window state lock unavailable".to_owned())
                    .and_then(|mut slot| {
                        slot.state = state;
                        slot.save().map_err(|error| error.to_string())
                    });
                if let Ok(mut latest) = error_slot.lock() {
                    *latest = result.err();
                }
                let _ = sender.0.try_send(UiEvent::StateSaveFinished);
            }
        });
    }

    pub(super) fn flush_state(&mut self) {
        self.capture_viewports();
        while self.state_writes.lock().is_ok_and(|pending| pending.1) {
            thread::sleep(Duration::from_millis(2));
        }
        let Some(slot) = &self.state_slot else {
            return;
        };
        let result = slot
            .lock()
            .map_err(|_| "Window state lock unavailable".to_owned())
            .and_then(|mut slot| {
                if slot.state != self.state || slot.is_unsaved() {
                    slot.state = self.state.clone();
                    slot.save().map_err(|error| error.to_string())?;
                }
                Ok(())
            });
        if let Ok(mut error) = self.state_save_error.lock() {
            *error = result.err();
        }
    }

    fn refresh_surfaces(&mut self, _: bool) {
        if self.update_quiesced.load(Ordering::Acquire) {
            return;
        }
        if self.loading_surfaces {
            return;
        }
        self.loading_surfaces = true;
        let sender = self.event_tx.clone();
        let target = self.target.clone();
        let quiesced = self.update_quiesced.clone();
        let connection_guard = self.update_connection_guard.clone();
        thread::spawn(move || {
            let connection = {
                let _guard = connection_guard
                    .lock()
                    .expect("Update connection barrier poisoned");
                if quiesced.load(Ordering::Acquire) {
                    return;
                }
                target.connect()
            };
            let result = connection
                .and_then(|mut client| client.workspace())
                .map_err(|error| error.to_string());
            sender.send(UiEvent::SurfacesLoaded(result));
        });
    }

    fn begin_daemon_restart(&mut self, consent: compi_protocol::LifecycleConsent) {
        if self.update_quiesced.load(Ordering::Acquire) {
            return;
        }
        if self.daemon_restarting {
            return;
        }
        self.daemon_restarting = true;
        self.loading_surfaces = true;
        self.global_error = None;
        let sender = self.event_tx.clone();
        let target = self.target.clone();
        thread::spawn(move || {
            let result = target
                .restart_daemon(&consent)
                .and_then(|mut client| client.workspace())
                .map_err(|error| error.to_string());
            sender.send(UiEvent::DaemonRestarted(result));
        });
    }

    fn mutate(&mut self, operation: WorkspaceMutation, select_created: bool) {
        if self.update_quiesced.load(Ordering::Acquire) {
            return;
        }
        if self.mutation_pending {
            self.global_error = Some("A workspace change is still pending".into());
            return;
        }
        let Some(workspace) = &self.workspace else {
            return;
        };
        let launches = matches!(
            operation,
            WorkspaceMutation::Initialize { .. }
                | WorkspaceMutation::CreateTab { .. }
                | WorkspaceMutation::SplitPane { .. }
                | WorkspaceMutation::RestartSurface { .. }
        );
        let launch = if launches {
            match &self.config.launch {
                Ok(launch) => Some(launch.clone()),
                Err(error) => {
                    self.global_error = Some(error.clone());
                    return;
                }
            }
        } else {
            None
        };
        let layout_guard = match &operation {
            WorkspaceMutation::SetSplitRatio { tab_id, .. } => workspace
                .sessions
                .iter()
                .flat_map(|session| &session.tabs)
                .find(|tab| &tab.id == tab_id)
                .map(|tab| (tab.id.clone(), tab.layout.clone())),
            _ => None,
        };
        let mutation = MutationRequest {
            server_id: workspace.server_id.clone(),
            expected_generation: workspace.server_generation.clone(),
            expected_revision: self
                .divider_drag
                .as_ref()
                .map_or(workspace.revision, |drag| drag.revision),
            mutation_id: MutationId::new(format!(
                "gui-{}-{:x}-{}",
                std::process::id(),
                *MUTATION_RUN_NONCE,
                NEXT_MUTATION_ID.fetch_add(1, Ordering::Relaxed)
            )),
            operation,
            launch: launch.map(Box::new),
        };
        self.mutation_pending = true;
        let sender = self.event_tx.clone();
        let target = self.target.clone();
        thread::spawn(move || {
            let result = (|| -> crate::Result<_> {
                let mut client = target.connect()?;
                let receipt = if let Some((tab_id, tree)) = layout_guard {
                    Self::commit_divider(&mut client, mutation, &tab_id, &tree)?
                } else {
                    client.submit_mutation(mutation)?
                };
                Ok((client.workspace()?, receipt))
            })()
            .map_err(|error| error.to_string());
            sender.send(UiEvent::MutationFinished {
                result,
                select_created,
            });
        });
    }

    fn commit_divider(
        client: &mut DaemonClient,
        mut mutation: MutationRequest,
        tab_id: &TabId,
        captured_tree: &LayoutNode,
    ) -> crate::Result<compi_protocol::MutationReceipt> {
        for attempt in 0..8 {
            let current = client.workspace()?;
            let same_tree = current
                .sessions
                .iter()
                .flat_map(|session| &session.tabs)
                .find(|tab| &tab.id == tab_id)
                .is_some_and(|tab| &tab.layout == captured_tree);
            if current.server_id != mutation.server_id
                || current.server_generation != mutation.expected_generation
                || !same_tree
            {
                return Err("The split tree changed; divider drag cancelled".into());
            }
            // Preview PTY resizes advance metadata revisions without changing the tree.
            // Only an explicit rejection can be retried; a lost reply remains uncertain.
            mutation.expected_revision = current.revision;
            match client.submit_mutation(mutation.clone()) {
                Ok(receipt) => return Ok(receipt),
                Err(error)
                    if error
                        .downcast_ref::<compi_protocol::DaemonError>()
                        .is_some_and(|error| {
                            error.code == compi_protocol::ErrorCode::RevisionConflict
                        })
                        && attempt < 7 =>
                {
                    mutation.mutation_id = MutationId::new(format!(
                        "gui-{}-{:x}-{}",
                        std::process::id(),
                        *MUTATION_RUN_NONCE,
                        NEXT_MUTATION_ID.fetch_add(1, Ordering::Relaxed)
                    ));
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error),
            }
        }
        Err("Workspace kept changing while committing the divider".into())
    }

    fn create_terminal(&mut self, working_directory: Option<String>) {
        let Some(session_id) = self.state.selected_session.clone() else {
            self.open_overlay(
                Overlay::Text {
                    purpose: TextPurpose::CreateWorkspace,
                    title: "New workspace",
                },
                "",
            );
            return;
        };
        self.mutate(
            WorkspaceMutation::CreateTab {
                session_id,
                label: String::new(),
                cols: self.terminal_cols,
                rows: self.terminal_rows,
                working_directory,
            },
            true,
        );
    }

    fn accept_workspace(&mut self, workspace: WorkspaceSnapshot) {
        self.loading_surfaces = false;
        if self.workspace.as_ref().is_some_and(|old| {
            old.server_id == workspace.server_id
                && old.server_generation == workspace.server_generation
                && old.revision > workspace.revision
        }) {
            return;
        }
        let generation_changed = self
            .workspace
            .as_ref()
            .is_some_and(|old| old.server_generation != workspace.server_generation);
        if generation_changed {
            self.pane_zoom.clear_all();
            self.surface_names.clear();
        } else {
            self.surface_names
                .retain(|id, _| workspace.surface(id).is_some());
        }
        self.reconcile_pane_zoom(&workspace);
        let changed = self.workspace.as_ref().is_none_or(|old| {
            old.server_generation != workspace.server_generation
                || old.revision != workspace.revision
        });
        if changed {
            if generation_changed {
                for view in &mut self.surface_views {
                    view.stop.store(true, Ordering::Release);
                    if let Some(transport) = view.transport.take() {
                        transport.close();
                    }
                    view.discard_replica();
                }
            }
            let selected_id = self.selected_tab().map(|tab| tab.id.clone());
            if let Some(drag) = &mut self.divider_drag {
                let same_tree = selected_id
                    .as_ref()
                    .and_then(|id| {
                        workspace
                            .sessions
                            .iter()
                            .flat_map(|session| &session.tabs)
                            .find(|tab| &tab.id == id)
                    })
                    .is_some_and(|tab| tab.layout == drag.tree);
                if same_tree {
                    drag.revision = workspace.revision;
                } else {
                    self.divider_drag = None;
                    self.preview_layout = None;
                    self.global_error = Some(
                        "The layout changed in another window; the divider preview was cancelled"
                            .into(),
                    );
                }
            }
            self.state.reconcile(self.workspace.as_ref(), &workspace);
            self.workspace = Some(workspace);
            self.sync_visible_views();
            self.save_state();
        } else {
            self.workspace = Some(workspace);
        }
        self.reap_retired_views();
    }

    fn handle_event(&mut self, event: UiEvent, cx: &mut Context<Self>) {
        if let Some(id) = event.surface_worker() {
            if self
                .surface_views
                .iter()
                .any(|view| view.id == id && view.stop.load(Ordering::Acquire))
            {
                if let UiEvent::TabConnected { transport, .. } = event {
                    transport.close();
                }
                return;
            }
            if !self.surface_views.iter().any(|view| view.id == id) {
                let route = EVENT_ROUTES
                    .lock()
                    .ok()
                    .and_then(|routes| routes.get(&id).cloned());
                if let Some(route) = route
                    && !route.same_channel(&self.event_tx.0)
                {
                    let _ = route.try_send(event);
                }
                return;
            }
        }
        match event {
            UiEvent::LifecycleLoaded(result) => match result {
                Ok(status) => {
                    let details = format!(
                        "Restarting daemon {} (protocol {}) ends {} live surfaces and affects {} attached clients in instance {}.\n\n{}\n\nTerminals are not resumed automatically.",
                        status.product_version,
                        status.protocol_version,
                        status.live_surfaces.len(),
                        status.connected_clients.len(),
                        status.instance.as_deref().unwrap_or("default"),
                        status
                            .live_surfaces
                            .iter()
                            .map(|surface| format!(
                                "{} · lifetime {}",
                                surface.surface_id, surface.process_lifetime_id
                            ))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                    self.open_overlay(
                        Overlay::ConfirmDaemonRestart {
                            details,
                            revision: status.workspace_revision,
                            consent: status.consent(),
                        },
                        "",
                    );
                }
                Err(error) => {
                    self.global_error = Some(format!("Cannot safely restart daemon: {error}"))
                }
            },
            UiEvent::SurfacesLoaded(_) if self.update_quiesced.load(Ordering::Acquire) => {}
            UiEvent::StateSaveFinished => {}
            UiEvent::ThemeLibraryReloaded {
                library,
                diagnostics,
            } => {
                self.adopt_theme_library(library, diagnostics);
            }
            UiEvent::AppearanceReloaded {
                appearance,
                favorites,
                ui_font,
                terminal_font_family,
                diagnostics,
            } => {
                self.pending_appearance_reload =
                    Some((appearance, favorites, ui_font, terminal_font_family));
                self.config_diagnostics = diagnostics;
                self.global_warning =
                    diagnostic_warning(&self.config_diagnostics, &self.typography.diagnostics);
            }
            UiEvent::PerformanceSample(Ok(sample)) => self.performance.observe(sample),
            UiEvent::PerformanceSample(Err(error)) => self.performance.record_error(error),
            UiEvent::ImagePrepared {
                request,
                origin,
                result,
            } => self.accept_prepared_image(request, origin, result),
            UiEvent::ImageInspectorLoaded { request, result } => {
                self.accept_inspector_image(request, result)
            }
            UiEvent::ImageClipboardReady(result) => match result {
                Ok(image) => {
                    cx.write_to_clipboard(gpui::ClipboardEntry::Image(image).into());
                    self.image_notice = Some("Image copied".into());
                }
                Err(error) => self.image_notice = Some(error),
            },
            UiEvent::ImageOperationFinished(result) => {
                self.image_notice = Some(match result {
                    Ok(message) => message,
                    Err(error) => error,
                })
            }
            UiEvent::TreeListed {
                pane_id,
                request,
                parent,
                result,
            } => {
                self.tree_listed(pane_id, request, parent, result);
            }
            UiEvent::TreeSearched {
                pane_id,
                request,
                result,
            } => {
                self.tree_searched(pane_id, request, result);
            }
            UiEvent::PromptResponded {
                job,
                request,
                result,
            } => self.prompt_responded(job, request, result),
            UiEvent::SurfacesLoaded(Ok(workspace)) => {
                self.connection_error = None;
                self.accept_workspace(workspace);
            }
            UiEvent::DaemonRestarted(result) => {
                self.daemon_restarting = false;
                self.loading_surfaces = false;
                match result {
                    Ok(workspace) => {
                        self.global_error = None;
                        self.connection_error = None;
                        self.accept_workspace(workspace);
                    }
                    Err(error) => {
                        self.connection_error = Some(format!(
                            "Daemon restart failed: {error}. Use Reconnect to retry."
                        ));
                    }
                }
            }
            UiEvent::SurfacesLoaded(Err(error)) => {
                self.loading_surfaces = false;
                self.connection_error =
                    Some(format!("Disconnected: {error}. Use Reconnect to retry."));
            }
            UiEvent::MutationFinished {
                result,
                select_created,
            } => {
                self.mutation_pending = false;
                self.preview_layout = None;
                self.divider_drag = None;
                match result {
                    Ok((workspace, receipt)) => {
                        self.global_error = None;
                        self.accept_workspace(workspace);
                        if select_created {
                            if let Some(tab) = receipt.affected_tabs.first() {
                                self.select_terminal(tab.clone(), true);
                            } else if let Some(session) = receipt.affected_sessions.first() {
                                if let Some(workspace) = &self.workspace {
                                    self.state.select_session(workspace, session);
                                }
                                self.sync_visible_views();
                                self.save_state();
                            }
                            if let Some(pane) = receipt.affected_panes.last() {
                                self.focus_pane(pane.clone());
                            }
                        }
                    }
                    Err(error) => {
                        self.global_error = Some(format!(
                            "Workspace change outcome is unconfirmed: {error}. Refreshing authoritative state; the operation will not be retried automatically."
                        ));
                        self.refresh_surfaces(false);
                    }
                }
            }
            UiEvent::TabConnected { tab_id, transport } => {
                let status = self
                    .surface_views
                    .iter()
                    .find(|view| view.id == tab_id)
                    .and_then(|view| self.workspace.as_ref()?.surface(&view.surface_id))
                    .map(|surface| (surface.status, surface.exit_code));
                let ready_probe = self
                    .ready_probe_marker
                    .clone()
                    .filter(|_| self.ready_probe_sent_at.is_none());
                if ready_probe.is_some() {
                    self.ready_probe_sent_at = Some(Instant::now());
                }
                if let Some(view) = self.surface_view_mut(tab_id) {
                    if view.stop.load(Ordering::Acquire) {
                        transport.close();
                        return;
                    }
                    view.transport = Some(transport);
                    view.state = match status {
                        Some((SurfaceStatus::Exited, code)) => {
                            ConnectionState::Exited(code.unwrap_or(0))
                        }
                        Some((SurfaceStatus::Failed, _)) => ConnectionState::Failed,
                        _ => ConnectionState::Attached,
                    };
                    view.error = None;
                    view.send(ClientMessage::Resize {
                        cols: view.cols,
                        rows: view.rows,
                    });
                    if let Some(marker) = ready_probe {
                        view.send(ClientMessage::Input {
                            data: format!("printf '%s\\n' '{marker}'\n").into_bytes(),
                            latency_id: None,
                        });
                    }
                }
            }
            UiEvent::TabScreen { tab_id, message } => {
                let clipboard = match &message {
                    ScreenMessage::Delta { delta } => delta.clipboard_writes.last().cloned(),
                    _ => None,
                };
                let shell_action = match &message {
                    ScreenMessage::Delta { delta } => delta.shell_action,
                    _ => None,
                };
                let images_changed = match &message {
                    ScreenMessage::Snapshot { .. } => true,
                    ScreenMessage::Delta { delta } => {
                        delta.images.is_some() || delta.placements.is_some()
                    }
                };
                let latency = match &message {
                    ScreenMessage::Delta { delta } => delta.latency_ids.clone(),
                    _ => Vec::new(),
                };
                let status = self
                    .surface_views
                    .iter()
                    .find(|view| view.id == tab_id)
                    .and_then(|view| self.workspace.as_ref()?.surface(&view.surface_id))
                    .map(|surface| (surface.status, surface.exit_code));
                let restore = self
                    .surface_views
                    .iter()
                    .find(|view| view.id == tab_id)
                    .filter(|view| {
                        view.mirror.snapshot().is_none()
                            && matches!(&message, ScreenMessage::Snapshot { .. })
                    })
                    .and_then(|view| {
                        let workspace = self.workspace.as_ref()?;
                        let identity = compi_protocol::TerminalIdentity {
                            server_id: workspace.server_id.clone(),
                            server_generation: workspace.server_generation.clone(),
                            surface_id: view.surface_id.clone(),
                            process_lifetime_id: view.lifetime.clone(),
                        };
                        Some((view.surface_id.clone(), identity))
                    })
                    .and_then(|(id, identity)| {
                        self.state
                            .viewports
                            .remove(id.as_str())
                            .map(|saved| (saved, identity))
                    });
                let ready_probe_marker = self.ready_probe_marker.clone();
                let mut ready_probe_observed = false;
                let mut observed_directory = None;
                if let Some(view) = self.surface_view_mut(tab_id) {
                    let incoming_directory = match &message {
                        ScreenMessage::Snapshot { snapshot } => {
                            snapshot.current_directory.as_deref()
                        }
                        ScreenMessage::Delta { delta } => delta.current_directory.as_deref(),
                    };
                    let directory_changed = incoming_directory.is_some()
                        && view
                            .mirror
                            .snapshot()
                            .and_then(|old| old.current_directory.as_deref())
                            != incoming_directory;
                    if let ScreenMessage::Delta { delta } = &message
                        && let Some(old) = view.mirror.snapshot()
                    {
                        let changed_geometry = old.cols != delta.cols || old.rows != delta.rows;
                        let history_invalidated = delta
                            .scrollback
                            .as_ref()
                            .is_some_and(|history| !history.starts_with(&old.scrollback));
                        if changed_geometry || history_invalidated {
                            view.selection = None;
                            view.scroll_offset = 0;
                            view.selecting = false;
                        } else if view.scroll_offset > 0
                            && let Some(history) = &delta.scrollback
                        {
                            view.scroll_offset = view
                                .scroll_offset
                                .saturating_add(history.len().saturating_sub(old.scrollback.len()));
                        }
                    }
                    if let ScreenMessage::Snapshot { snapshot } = &message {
                        view.viewport_fingerprint = None;
                        let valid = view.mirror.snapshot().is_some_and(|old| {
                            old.cols == snapshot.cols
                                && old.rows == snapshot.rows
                                && old.scrollback == snapshot.scrollback
                                && old.cells == snapshot.cells
                        });
                        if !valid {
                            view.selection = None;
                            view.scroll_offset = 0;
                            view.selecting = false;
                        }
                    }
                    if matches!(view.mirror.apply(message), MirrorApply::Gap { .. }) {
                        view.send(ClientMessage::RequestSnapshot);
                        return;
                    }
                    if let Some((saved, identity)) = restore {
                        view.restore_viewport(saved, &identity);
                    }
                    if view
                        .mirror
                        .snapshot()
                        .is_some_and(|snapshot| snapshot.modes.alternate_screen)
                    {
                        view.scroll_offset = 0;
                        view.selection = None;
                        view.selecting = false;
                    }
                    view.scroll_offset = view.scroll_offset.min(view.max_scroll_offset());
                    view.state = match status {
                        Some((SurfaceStatus::Exited, code)) => {
                            ConnectionState::Exited(code.unwrap_or(0))
                        }
                        Some((SurfaceStatus::Failed, _)) => ConnectionState::Failed,
                        _ if matches!(view.state, ConnectionState::Exited(_)) => view.state,
                        _ => ConnectionState::Attached,
                    };
                    view.images_dirty |= images_changed;
                    ready_probe_observed = ready_probe_marker.as_deref().is_some_and(|marker| {
                        snapshot_contains_marker(view.mirror.snapshot(), marker)
                    });
                    if directory_changed {
                        observed_directory = view
                            .mirror
                            .snapshot()
                            .and_then(|snapshot| snapshot.current_directory.clone());
                    }
                }
                if let Some(path) = observed_directory
                    && self.state.project_history.record(&path)
                {
                    self.save_state();
                }
                if let Some(action) = shell_action
                    && let Some(pane_id) = self
                        .surface_views
                        .iter()
                        .find(|view| view.id == tab_id)
                        .map(|view| view.pane_id.clone())
                {
                    if self.overlay.is_some() {
                        self.reply_to_shell(&pane_id, None);
                        self.global_error =
                            Some("Close the active dialog before browsing directories".into());
                    } else {
                        self.focus_pane(pane_id);
                        match action {
                            compi_protocol::ShellAction::BrowseFiles => {
                                self.open_file_tree_with_shell(true)
                            }
                            compi_protocol::ShellAction::JumpProject => {
                                self.open_project_jump_with_shell(true)
                            }
                        }
                    }
                }
                self.ready_probe_render_pending |= ready_probe_observed;
                if self.focused_view == Some(tab_id)
                    && matches!(
                        self.config.clipboard_policy,
                        crate::config::ClipboardPolicy::Allow
                    )
                    && let Some(text) = clipboard
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                self.pending_present_latency_ids.extend(latency);
                if !self.first_snapshot_logged {
                    self.first_snapshot_logged = true;
                    log_startup_metric("first_terminal_frame_ms", self.started_at.elapsed());
                }
            }
            UiEvent::KittyImageDecoded {
                tab_id,
                image_id,
                generation,
                result,
            } => {
                if let Some(view) = self.surface_view_mut(tab_id)
                    && view.image_pending.get(&image_id) == Some(&generation)
                {
                    view.image_pending.remove(&image_id);
                    view.images_dirty = true;
                    match result {
                        Ok(image) => {
                            let bytes = decoded_image_bytes(&image);
                            if view
                                .image_cache_bytes
                                .checked_add(bytes)
                                .is_none_or(|total| total > SURFACE_IMAGE_CACHE_LIMIT)
                            {
                                view.image_capacity_rejected.insert(image_id);
                                view.image_error = Some("Visible images exceed this pane's 64 MiB decoded cache. Original image data is retained.".into());
                            } else {
                                view.image_cache_bytes += bytes;
                                view.image_cache.insert(image_id, (generation, image));
                                if view.image_rejected.is_empty() {
                                    view.image_error = None;
                                }
                            }
                        }
                        Err(error) => {
                            view.image_rejected.insert(image_id, generation);
                            view.image_error = Some(format!("Image {image_id}: {error}"));
                        }
                    }
                }
            }
            UiEvent::TabControl { tab_id, message } => match message {
                ServerMessage::SurfaceExited { exit_code, .. } => {
                    if let Some(view) = self.surface_view_mut(tab_id) {
                        view.state = ConnectionState::Exited(exit_code);
                        // Exit releases the live controller. Reattach once for the
                        // authoritative final grid and read-only resize/history operations.
                        view.stop.store(true, Ordering::Release);
                        if let Some(transport) = view.transport.take() {
                            transport.close();
                        }
                        view.error = None;
                    }
                    self.refresh_surfaces(false);
                }
                ServerMessage::Error { message, .. } => {
                    if let Some(view) = self.surface_view_mut(tab_id) {
                        view.error = Some(message);
                        view.state = ConnectionState::Failed;
                    }
                }
                _ => {}
            },
            UiEvent::TabDisconnected { tab_id, error } => {
                if let Some(view) = self.surface_view_mut(tab_id) {
                    view.transport = None;
                    if !matches!(view.state, ConnectionState::Exited(_)) {
                        view.state = ConnectionState::Failed;
                        view.error = Some(format!(
                            "{error}. Retry attachment when the other window has released this pane."
                        ));
                    }
                }
            }
        }
    }

    fn sync_visible_views(&mut self) {
        if self.update_quiesced.load(Ordering::Acquire) {
            return;
        }
        self.capture_viewports();
        let mut leaves = Vec::new();
        if let Some(tab) = self.selected_tab() {
            collect_leaves(&tab.layout, &mut leaves);
        }
        // Floating panes stay attached while another tab or workspace is selected.
        for float in self.floating_leaves() {
            if !leaves.iter().any(|(pane, _)| pane == &float.0) {
                leaves.push(float);
            }
        }
        let wanted: HashSet<_> = leaves.iter().map(|(_, surface)| surface.clone()).collect();
        let Some(workspace) = &self.workspace else {
            return;
        };
        for view in &mut self.surface_views {
            if !wanted.contains(&view.surface_id) {
                if workspace.surface(&view.surface_id).is_some()
                    && let Some(snapshot) = view.mirror.snapshot()
                    && !snapshot.title.trim().is_empty()
                {
                    self.surface_names.insert(
                        view.surface_id.clone(),
                        (
                            view.lifetime.clone(),
                            concise_tab_title(&snapshot.title),
                            snapshot.current_directory.clone(),
                        ),
                    );
                }
                view.stop.store(true, Ordering::Release);
                if let Some(transport) = view.transport.take() {
                    transport.close();
                }
                if let Ok(mut routes) = EVENT_ROUTES.lock() {
                    routes.remove(&view.id);
                }
                view.discard_replica();
            }
        }
        let metrics = self.metrics();
        self.surface_views.retain(|view| {
            workspace.surface(&view.surface_id).is_some()
                && (wanted.contains(&view.surface_id) || !view.closed.load(Ordering::Acquire))
        });
        for (pane_id, surface_id) in leaves {
            let Some(surface) = workspace.surface(&surface_id) else {
                continue;
            };
            let dimensions = self
                .float_layouts
                .iter()
                .find(|float| float.pane.pane_id == pane_id)
                .map(|float| &float.pane)
                .or_else(|| {
                    self.layout
                        .as_ref()
                        .and_then(|layout| layout.pane(&pane_id))
                })
                .map(|pane| pane.grid_size(metrics))
                .unwrap_or((self.terminal_cols, self.terminal_rows));
            let existing = self
                .surface_views
                .iter()
                .position(|view| view.surface_id == surface_id);
            let index = if let Some(index) = existing {
                index
            } else {
                self.surface_views.push(SurfaceView {
                    id: NEXT_VIEW_ID.fetch_add(1, Ordering::Relaxed),
                    pane_id: pane_id.clone(),
                    surface_id: surface_id.clone(),
                    lifetime: surface.process_lifetime_id.clone(),
                    stop: Arc::new(AtomicBool::new(true)),
                    closed: Arc::new(AtomicBool::new(true)),
                    mirror: ScreenMirror::default(),
                    state: ConnectionState::Connecting,
                    error: None,
                    transport: None,
                    scroll_offset: 0,
                    selection: None,
                    selecting: false,
                    mouse_reporting_down: false,
                    image_cache: HashMap::new(),
                    image_pending: HashMap::new(),
                    image_rejected: HashMap::new(),
                    image_sources: HashMap::new(),
                    visible_images: HashSet::new(),
                    image_capacity_rejected: HashSet::new(),
                    image_viewport: None,
                    images_dirty: true,
                    image_retry: false,
                    image_cache_bytes: 0,
                    image_error: None,
                    row_render_cache: Arc::new(Mutex::new(RowRenderCache::default())),
                    viewport_fingerprint: None,
                    cols: dimensions.0,
                    rows: dimensions.1,
                });
                self.surface_views.len() - 1
            };
            let view = &mut self.surface_views[index];
            if view.lifetime != surface.process_lifetime_id {
                view.stop.store(true, Ordering::Release);
                if let Some(transport) = view.transport.take() {
                    transport.close();
                }
                view.discard_replica();
                view.lifetime = surface.process_lifetime_id.clone();
            }
            if surface.status == SurfaceStatus::Lost {
                view.error = Some(surface.error.clone().unwrap_or_else(|| {
                    format!("{:?}: restart this surface explicitly", surface.status)
                }));
                view.state = ConnectionState::Failed;
                continue;
            }
            if view.stop.load(Ordering::Acquire) {
                if let Ok(mut routes) = EVENT_ROUTES.lock() {
                    routes.remove(&view.id);
                }
                view.id = NEXT_VIEW_ID.fetch_add(1, Ordering::Relaxed);
                view.stop = Arc::new(AtomicBool::new(false));
                view.state = ConnectionState::Connecting;
                view.image_pending.clear();
                if let Ok(mut routes) = EVENT_ROUTES.lock() {
                    routes.insert(view.id, self.event_tx.0.clone());
                }
                let previous_closed = view.closed.clone();
                view.closed = Arc::new(AtomicBool::new(false));
                spawn_tab_worker(
                    view.id,
                    surface_id,
                    view.cols,
                    view.rows,
                    self.event_tx.clone(),
                    self.target.clone(),
                    WorkerLifecycle {
                        stop: view.stop.clone(),
                        previous_closed,
                        closed: view.closed.clone(),
                        update_quiesced: self.update_quiesced.clone(),
                        connection_guard: self.update_connection_guard.clone(),
                    },
                );
            }
        }
        self.focused_view = self.state.focused_pane(workspace).and_then(|pane| {
            self.surface_views
                .iter()
                .find(|view| &view.pane_id == pane)
                .map(|view| view.id)
        });
    }

    fn select_terminal(&mut self, id: TabId, restore: bool) {
        self.close_file_tree();
        self.report_focus(false);
        if let Some(workspace) = &self.workspace {
            if restore {
                self.state.restore_tab(workspace, &id);
            } else {
                self.state.select_tab(workspace, &id);
            }
        }
        self.workspace_scroll.set_offset(point(px(0.0), px(0.0)));
        self.sync_visible_views();
        self.save_state();
        if let Some(workspace) = &self.workspace
            && let Some(session) = self.state.selected_session(workspace)
        {
            let index = self
                .state
                .visible_tabs(session)
                .position(|tab| tab.id == id)
                .unwrap_or(0);
            self.tab_scroll_handle.scroll_to_item(index);
        }
    }

    fn hide_terminal(&mut self, id: &TabId) {
        if let Some(workspace) = &self.workspace {
            self.state.hide_tab(workspace, id);
        }
        self.sync_visible_views();
        self.save_state();
    }

    fn focus_pane(&mut self, pane: PaneId) {
        if self.focused_view().is_some_and(|view| view.pane_id == pane) {
            return;
        }
        if self
            .file_tree
            .as_ref()
            .is_some_and(|tree| tree.pane_id != pane)
        {
            self.close_file_tree();
        }
        let floating = self.state.is_floating(&pane);
        let tab_id = self.selected_tab().map(|tab| tab.id.clone());
        self.report_focus(false);
        if let Some(workspace) = &self.workspace {
            self.state.focus_pane(workspace, &pane);
        }
        if !floating && let Some(tab_id) = &tab_id {
            self.pane_zoom.retarget(tab_id, pane.clone());
        }
        self.focused_view = self
            .surface_views
            .iter()
            .find(|view| view.pane_id == pane)
            .map(|view| view.id);
        self.report_focus(self.overlay.is_none());
        if floating {
            // Floats sit outside the scrolled split canvas; nothing to reveal.
        } else if self.pane_zoomed() {
            self.workspace_scroll.set_offset(point(px(0.0), px(0.0)));
        } else if let Some(layout) = &self.layout {
            let offset = self.workspace_scroll.offset();
            let cursor = self
                .focused_view()
                .and_then(|view| view.mirror.snapshot())
                .and_then(|snapshot| {
                    layout.pane(&pane).map(|geometry| layout::Rect {
                        x: geometry.canvas.x
                            + f32::from(snapshot.cursor.col) * self.typography.cell_width,
                        y: geometry.canvas.y
                            + f32::from(snapshot.cursor.row) * self.typography.cell_height,
                        width: self.typography.cell_width,
                        height: self.typography.cell_height,
                    })
                });
            let reveal = layout.reveal_pane(
                &pane,
                cursor,
                layout::Point {
                    x: -f32::from(offset.x),
                    y: -f32::from(offset.y),
                },
            );
            self.workspace_scroll
                .set_offset(point(px(-reveal.x), px(-reveal.y)));
        }
        self.save_state();
    }

    /// One device pixel, so split and sidebar seams stay crisp at every display scale.
    fn seam_width(&self) -> f32 {
        1.0 / positive_scale(self.typography_scale)
    }

    /// Theme border pulled 40% toward the terminal background: a hairline that
    /// separates without drawing the eye. Hover and drag still use the full accent.
    fn seam_color(&self) -> u32 {
        blend_rgb(
            self.terminal_theme.terminal().background & 0x00ff_ffff,
            self.colors().border & 0x00ff_ffff,
            0.6,
        )
    }

    /// Logical width left of the terminal area: the device-aligned sidebar plus its seam.
    fn sidebar_extent(&self) -> f32 {
        if !self.sidebar_open {
            return 0.0;
        }
        let scale = positive_scale(self.typography_scale);
        (self.sidebar_width * scale).round() / scale + self.seam_width()
    }

    fn metrics(&self) -> LayoutMetrics {
        LayoutMetrics {
            cell_width: self.typography.cell_width,
            line_height: self.typography.cell_height,
            padding_x: TERMINAL_PADDING,
            padding_y: TERMINAL_PADDING,
            pane_chrome_height: 0.0,
            divider_thickness: self.seam_width(),
            scale_factor: self.typography_scale,
        }
    }

    fn rebuild_layout(&mut self, window: &Window, force: bool) -> bool {
        let (viewport_width, viewport_height) = logical_viewport_dimensions(window);
        let metrics = self.metrics();
        let available = layout::Size {
            width: (viewport_width - self.sidebar_extent()).max(1.0),
            height: (viewport_height - CHROME_HEIGHT).max(1.0),
        };
        self.float_area = available;
        self.float_layouts = self.compute_float_layouts(available, metrics);
        // Floating panes leave the selected tab's split for this window only; their
        // siblings fill the space and divider addresses still target the saved tree.
        let tiled = self.selected_tab().and_then(|tab| {
            let tree = self.preview_layout.as_ref().unwrap_or(&tab.layout);
            let floating = |pane: &PaneId| self.state.is_floating(pane);
            let pruned = if self
                .state
                .floating
                .iter()
                .any(|float| layout_contains_pane(tree, &float.pane_id))
            {
                std::borrow::Cow::Owned(layout::without_panes(tree, &floating)?)
            } else {
                std::borrow::Cow::Borrowed(tree)
            };
            let mut layout = layout::compute_layout(&pruned, available, metrics);
            if layout.has_overflow() {
                layout = layout::compute_layout(
                    &pruned,
                    layout::Size {
                        width: (available.width - 8.0).max(1.0),
                        height: (available.height - 8.0).max(1.0),
                    },
                    metrics,
                );
            }
            if matches!(pruned, std::borrow::Cow::Owned(_)) {
                for divider in &mut layout.dividers {
                    divider.path = layout::saved_path(tree, &divider.path, &floating);
                }
            }
            Some((tab.id.clone(), layout))
        });
        let zoom_layout = tiled.as_ref().and_then(|(tab_id, layout)| {
            let pane = layout.pane(self.pane_zoom.pane(tab_id)?)?;
            let tree = LayoutNode::Pane {
                pane_id: pane.pane_id.clone(),
                surface_id: pane.surface_id.clone(),
            };
            Some(layout::compute_layout(&tree, available, metrics))
        });
        if zoom_layout.is_none()
            && let Some(tab_id) = self.selected_tab().map(|tab| tab.id.clone())
        {
            self.pane_zoom.clear(&tab_id);
        }
        let resize_due = force || self.last_resize.elapsed() >= Duration::from_millis(32);
        let mut needs_frame = false;
        let tiled_panes = tiled.as_ref().map_or(&[][..], |(_, layout)| &layout.panes);
        let geometries = tiled_panes
            .iter()
            .map(|pane| {
                zoom_layout
                    .as_ref()
                    .and_then(|zoom| zoom.pane(&pane.pane_id))
                    .unwrap_or(pane)
            })
            .chain(self.float_layouts.iter().map(|float| &float.pane));
        for geometry in geometries {
            let (cols, rows) = geometry.grid_size(metrics);
            let Some(view) = self
                .surface_views
                .iter_mut()
                .find(|view| view.pane_id == geometry.pane_id)
            else {
                continue;
            };
            if (view.cols, view.rows) == (cols, rows) {
                continue;
            }
            if resize_due {
                view.cols = cols;
                view.rows = rows;
                view.send(ClientMessage::Resize { cols, rows });
            } else {
                needs_frame = true;
            }
        }
        if resize_due {
            self.last_resize = Instant::now();
        }
        if let Some(dimensions) = self.focused_view().map(|view| (view.cols, view.rows)) {
            self.terminal_cols = dimensions.0;
            self.terminal_rows = dimensions.1;
        }
        self.layout = tiled.map(|(_, layout)| layout);
        self.zoom_layout = zoom_layout;
        needs_frame
    }

    /// Floating frames from their saved fractions, never smaller than one minimum
    /// terminal plus the title strip unless the window itself is smaller.
    fn compute_float_layouts(
        &self,
        area: layout::Size,
        metrics: LayoutMetrics,
    ) -> Vec<FloatLayout> {
        let leaf = metrics.leaf_minimum();
        let minimum = layout::Size {
            width: leaf.width,
            height: leaf.height + FLOAT_TITLE_HEIGHT,
        };
        let mut leaves = self.floating_leaves().into_iter();
        self.state
            .floating
            .iter()
            .filter_map(|float| {
                let (pane_id, surface_id) = leaves.find(|(pane, _)| pane == &float.pane_id)?;
                let frame = layout::float_frame(
                    layout::Rect {
                        x: float.rect.x,
                        y: float.rect.y,
                        width: float.rect.width,
                        height: float.rect.height,
                    },
                    area,
                    minimum,
                );
                let body = layout::Size {
                    width: frame.width,
                    height: (frame.height - FLOAT_TITLE_HEIGHT).max(1.0),
                };
                let tree = LayoutNode::Pane {
                    pane_id,
                    surface_id,
                };
                let mut pane = layout::compute_layout(&tree, body, metrics).panes.pop()?;
                for rect in [&mut pane.rect, &mut pane.canvas] {
                    rect.x += frame.x;
                    rect.y += frame.y + FLOAT_TITLE_HEIGHT;
                }
                Some(FloatLayout { frame, pane })
            })
            .collect()
    }

    /// Floating panes that still exist anywhere in the workspace, back to front.
    fn floating_leaves(&self) -> Vec<(PaneId, SurfaceId)> {
        let Some(workspace) = &self.workspace else {
            return Vec::new();
        };
        self.state
            .floating
            .iter()
            .filter_map(|float| {
                workspace
                    .sessions
                    .iter()
                    .flat_map(|session| &session.tabs)
                    .find_map(|tab| pane_surface(&tab.layout, &float.pane_id))
                    .map(|surface| (float.pane_id.clone(), surface.clone()))
            })
            .collect()
    }

    pub(super) fn grid_point(&self, position: Point<Pixels>) -> Option<GridPoint> {
        let view = self.focused_view()?;
        self.grid_point_for_pane(position, &view.pane_id, false)
    }

    pub(super) fn grid_point_for_pane(
        &self,
        position: Point<Pixels>,
        pane_id: &PaneId,
        clamp: bool,
    ) -> Option<GridPoint> {
        let view = self
            .surface_views
            .iter()
            .find(|view| &view.pane_id == pane_id)?;
        // Floating frames are fixed to the terminal area, not the scrolled canvas.
        let (canvas, offset) = match self
            .float_layouts
            .iter()
            .find(|float| &float.pane.pane_id == pane_id)
        {
            Some(float) => (float.pane.canvas, point(px(0.0), px(0.0))),
            None => (
                self.visible_layout()?.pane(pane_id)?.canvas,
                self.workspace_scroll.offset(),
            ),
        };
        let x = f32::from(position.x) - self.sidebar_extent() - canvas.x - f32::from(offset.x);
        let y = f32::from(position.y) - CHROME_HEIGHT - canvas.y - f32::from(offset.y);
        if !x.is_finite() || !y.is_finite() || view.cols <= 0 || view.rows <= 0 {
            return None;
        }
        if !clamp && (x < 0.0 || y < 0.0) {
            return None;
        }
        let col = (x / self.typography.cell_width).floor();
        let row = (y / self.typography.cell_height).floor();
        if !clamp && (col >= f32::from(view.cols) || row >= f32::from(view.rows)) {
            return None;
        }
        Some(GridPoint {
            row: (row as usize).min(view.rows as usize - 1),
            col: (col as usize).min(view.cols as usize - 1),
        })
    }
}

fn logical_viewport_dimensions(window: &Window) -> (f32, f32) {
    let viewport = window.viewport_size();
    (f32::from(viewport.width), f32::from(viewport.height))
}

fn opacity_at_slider_position(position: Pixels, bounds: Bounds<Pixels>) -> f32 {
    let progress = (f32::from(position - bounds.origin.x) / f32::from(bounds.size.width).max(1.0))
        .clamp(0.0, 1.0);
    crate::config::MIN_TERMINAL_OPACITY
        + progress * (crate::config::MAX_TERMINAL_OPACITY - crate::config::MIN_TERMINAL_OPACITY)
}

fn collect_leaves(tree: &LayoutNode, output: &mut Vec<(PaneId, SurfaceId)>) {
    match tree {
        LayoutNode::Pane {
            pane_id,
            surface_id,
        } => output.push((pane_id.clone(), surface_id.clone())),
        LayoutNode::Split { first, second, .. } => {
            collect_leaves(first, output);
            collect_leaves(second, output);
        }
    }
}

fn set_ratio(tree: &mut LayoutNode, path: &[bool], value: f32) {
    if let LayoutNode::Split {
        ratio,
        first,
        second,
        ..
    } = tree
    {
        if let Some((branch, rest)) = path.split_first() {
            set_ratio(if *branch { second } else { first }, rest, value);
        } else {
            *ratio = value;
        }
    }
}

impl CompiApp {
    fn command_context(&self, cx: &App) -> commands::CommandContext {
        let workspace = self.workspace.as_ref();
        let session = workspace.and_then(|workspace| self.state.selected_session(workspace));
        let tab = self.selected_tab();
        let pane = self.focused_view().map(|view| &view.pane_id);
        let status = self
            .focused_view()
            .and_then(|view| workspace?.surface(&view.surface_id))
            .map(|surface| surface.status);
        let split_reason = |axis| {
            self.layout
                .as_ref()
                .and_then(|layout| pane.map(|pane| layout.split_feasibility(pane, axis).err()))
                .flatten()
        };
        let neighbor = |direction| {
            self.layout
                .as_ref()
                .and_then(|layout| pane.and_then(|pane| layout.focus_neighbor(pane, direction)))
                .is_some()
        };
        let live_surface_count = workspace.map_or(0, |workspace| {
            workspace
                .surfaces
                .iter()
                .filter(|surface| {
                    matches!(
                        surface.status,
                        SurfaceStatus::Starting | SurfaceStatus::Running | SurfaceStatus::Ending
                    )
                })
                .count()
        });
        let visible: Vec<_> = session
            .map(|session| self.state.visible_tabs(session).collect())
            .unwrap_or_default();
        commands::CommandContext {
            connected: workspace.is_some() && !self.loading_surfaces,
            workspace_count: workspace.map_or(0, |workspace| workspace.sessions.len()),
            tab_count: visible.len(),
            hidden_tab_count: self.state.hidden_tabs.len(),
            pane_count: self.layout.as_ref().map_or(0, |layout| layout.panes.len()),
            tab_pane_count: pane
                .and_then(|pane| {
                    workspace?
                        .sessions
                        .iter()
                        .flat_map(|session| &session.tabs)
                        .find(|tab| layout_contains_pane(&tab.layout, pane))
                })
                .map_or(0, |tab| leaf_count(&tab.layout)),
            pane_floating: pane.is_some_and(|pane| self.state.is_floating(pane)),
            floating_count: self.state.floating.len(),
            tiled_pane_available: workspace
                .and_then(|workspace| self.state.tiled_focus(workspace))
                .is_some(),
            has_workspace: session.is_some(),
            has_tab: tab.is_some(),
            has_pane: pane.is_some(),
            tab_index: tab
                .and_then(|tab| visible.iter().position(|item| item.id == tab.id))
                .unwrap_or(0),
            has_selection: self
                .focused_view()
                .and_then(|view| selected_text(view.mirror.snapshot(), view.selection))
                .is_some(),
            terminal_available: self
                .focused_view()
                .is_some_and(|view| view.transport.is_some()),
            can_paste: true,
            surface_status: status,
            live_surface_count,
            mutation_pending: self.mutation_pending,
            transfer_in_progress: self.transferred_seed.is_some(),
            daemon_restarting: self.daemon_restarting,
            remote_target: self.target.is_remote(),
            other_window_available: cx.windows().len() > 1,
            structure: self
                .overlay_structure
                .unwrap_or_else(|| workspace.map_or(0, structure_key)),
            current_structure: workspace.map_or(0, structure_key),
            split_right_reason: split_reason(SplitAxis::Horizontal),
            split_down_reason: split_reason(SplitAxis::Vertical),
            pane_zoomed: self.pane_zoomed(),
            resize_reason: if self
                .layout
                .as_ref()
                .is_some_and(|layout| !layout.dividers.is_empty())
            {
                None
            } else {
                Some("This terminal has no split divider")
            },
            focus_left: neighbor(layout::Direction::Left),
            focus_right: neighbor(layout::Direction::Right),
            focus_up: neighbor(layout::Direction::Up),
            focus_down: neighbor(layout::Direction::Down),
            arrangement_pane_count: tab.map_or(0, |tab| leaf_count(&tab.layout)),
            has_previous_arrangement: tab
                .is_some_and(|tab| tab.previous_layout.is_some() || tab.merge.is_some()),
            has_merged_tabs: tab.is_some_and(|tab| tab.merge.is_some()),
        }
    }

    fn open_overlay(&mut self, overlay: Overlay, text: &str) {
        self.close_file_tree();
        self.finish_dismiss_overlay();
        self.report_focus(false);
        self.overlay_structure = self.workspace.as_ref().map(structure_key);
        self.overlay_return = None;
        self.overlay = Some(overlay);
        self.overlay_index = 0;
        self.overlay_focus = 0;
        self.overlay_generation = self.overlay_generation.wrapping_add(1);
        self.overlay_closing_since = None;
        if matches!(
            self.overlay,
            Some(Overlay::QuickAppearance | Overlay::Settings)
        ) {
            self.settings_scope = SettingsScope::Global;
        }
        self.overlay_scroll.set_offset(point(px(0.0), px(0.0)));
        self.ime_text = text.to_owned();
        self.ime_marked_range = None;
        let end = text.encode_utf16().count();
        self.ime_selected_range = end..end;
        if matches!(self.overlay, Some(Overlay::Settings))
            && self.settings_section == SettingsSection::Terminal
        {
            self.enter_prompt_settings();
        }
    }

    fn dismiss_overlay(&mut self) {
        if matches!(self.overlay, Some(Overlay::ThemeCatalog))
            && let Some(parent) = self.overlay_return.take()
        {
            self.cancel_catalog_preview();
            self.overlay = Some(parent);
            self.overlay_generation = self.overlay_generation.wrapping_add(1);
            self.overlay_closing_since = None;
            self.overlay_focus = self.settings_section as usize;
            self.ime_text.clear();
            self.overlay_scroll.set_offset(point(px(0.0), px(0.0)));
            return;
        }
        if self.overlay.is_some() && self.overlay_closing_since.is_none() {
            self.cancel_catalog_preview();
            self.overlay_closing_since = Some(Instant::now());
        }
    }

    fn finish_dismiss_overlay(&mut self) {
        self.cancel_catalog_preview();
        self.image_inspector = None;
        self.overlay_return = None;
        self.overlay = None;
        self.overlay_closing_since = None;
        self.ime_text.clear();
        self.ime_marked_range = None;
        self.ime_selected_range = 0..0;
        self.overlay_structure = None;
        self.report_focus(true);
    }

    fn set_theme(&mut self, theme: Arc<ThemeDefinition>) {
        self.theme = theme;
    }

    fn set_terminal_theme(&mut self, theme: Arc<ThemeDefinition>) {
        let changed = self.terminal_theme.terminal() != theme.terminal();
        self.terminal_theme = theme;
        if !changed {
            return;
        }
        for view in &self.surface_views {
            if let Ok(mut cache) = view.row_render_cache.lock() {
                cache.clear();
            }
        }
    }

    fn handle_app_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = &event.keystroke;
        if self.inspector_key(key, window, cx) {
            return true;
        }
        if self.handle_tree_key(key, cx) {
            return true;
        }
        if key.key == "escape" && self.dragging_tab.is_some() {
            self.dragging_tab = None;
            self.tab_drag_origin = None;
            self.drag_position = None;
            cx.notify();
            return true;
        }
        if key.key == "escape" && self.divider_drag.is_some() {
            self.divider_drag = None;
            self.preview_layout = None;
            self.rebuild_layout(window, true);
            cx.notify();
            return true;
        }
        if key.key == "escape"
            && let Some(drag) = self.float_drag.take()
        {
            self.state.set_float_rect(&drag.pane_id, drag.before);
            self.rebuild_layout(window, true);
            cx.notify();
            return true;
        }
        if self.overlay.is_some() {
            if self.overlay_closing_since.is_some() {
                return true;
            }
            if self.handle_settings_key(key, window, cx)
                || self.handle_dialog_key(key, window, cx)
                || self.handle_catalog_key(key, window, cx)
            {
                return true;
            }
            match key.key.as_str() {
                "escape" => {
                    self.dismiss_overlay();
                    window.focus(&self.focus_handle);
                }
                "enter" => {
                    self.activate_overlay(window, cx);
                }
                "up" | "down" => {
                    let choices = self.overlay_choices(cx);
                    let count = choices.len().max(1);
                    self.overlay_index = if key.key == "up" {
                        (self.overlay_index + count - 1) % count
                    } else {
                        (self.overlay_index + 1) % count
                    };
                    if choices
                        .get(self.overlay_index)
                        .is_some_and(|choice| choice.reason.is_some())
                    {
                        self.overlay_scroll
                            .scroll_to_top_of_item(self.overlay_index);
                    } else {
                        self.overlay_scroll.scroll_to_item(self.overlay_index);
                    }
                }
                "backspace" | "delete"
                    if !matches!(
                        self.overlay,
                        Some(
                            Overlay::PaneActions
                                | Overlay::TabActions { .. }
                                | Overlay::TabPaneActions { .. }
                                | Overlay::HeaderActions { .. }
                                | Overlay::QuickAppearance
                                | Overlay::Settings
                                | Overlay::Confirm { .. }
                                | Overlay::Diagnostics
                        )
                    ) =>
                {
                    let mut range = self.ime_selected_range.clone();
                    if range.is_empty() {
                        let byte = utf16_byte_index(&self.ime_text, range.start);
                        if key.key == "backspace" {
                            if let Some(character) = self.ime_text[..byte].chars().next_back() {
                                range.start = range.start.saturating_sub(character.len_utf16());
                            }
                        } else if let Some(character) = self.ime_text[byte..].chars().next() {
                            range.end += character.len_utf16();
                        }
                    }
                    self.edit_overlay_text(Some(range), "");
                }
                "left" | "right" | "home" | "end" => {
                    let end = self.ime_text.encode_utf16().count();
                    let at = match key.key.as_str() {
                        "home" => 0,
                        "end" => end,
                        "left" => self.ime_selected_range.start.saturating_sub(1),
                        _ => (self.ime_selected_range.end + 1).min(end),
                    };
                    self.ime_selected_range = at..at;
                }
                "a" if key.modifiers.platform || key.modifiers.control => {
                    self.ime_selected_range = 0..self.ime_text.encode_utf16().count();
                }
                "v" if key.modifiers.platform || key.modifiers.control => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        self.edit_overlay_text(None, &text);
                    }
                }
                "c" if key.modifiers.platform || key.modifiers.control => {
                    let start = utf16_byte_index(&self.ime_text, self.ime_selected_range.start);
                    let end = utf16_byte_index(&self.ime_text, self.ime_selected_range.end);
                    if start < end {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            self.ime_text[start..end].to_owned(),
                        ));
                    }
                }
                _ => {
                    // Printable and composing input belongs to native text fields, never menus.
                    if !matches!(
                        self.overlay,
                        Some(
                            Overlay::PaneActions
                                | Overlay::TabActions { .. }
                                | Overlay::TabPaneActions { .. }
                                | Overlay::HeaderActions { .. }
                                | Overlay::Arrangements { .. }
                        )
                    ) && key.key_char.is_some()
                        && !key.modifiers.control
                        && !key.modifiers.platform
                    {
                        return false;
                    }
                }
            }
            cx.notify();
            return true;
        }
        let platform = if cfg!(target_os = "macos") {
            commands::Platform::Mac
        } else {
            commands::Platform::Windows
        };
        let route = commands::resolve_key(
            platform,
            commands::ShortcutKey {
                key: &key.key,
                control: key.modifiers.control,
                shift: key.modifiers.shift,
                alt: key.modifiers.alt,
                command: key.modifiers.platform,
            },
            commands::InputOwner::Terminal,
            &self.command_context(cx),
            &self.config.keybindings,
        );
        match route {
            commands::KeyRoute::Command(command) => {
                self.execute(command, window, cx);
                true
            }
            commands::KeyRoute::Disabled(_, reason) => {
                self.global_error = Some(reason.into());
                cx.notify();
                true
            }
            commands::KeyRoute::Owned => true,
            commands::KeyRoute::Terminal => false,
        }
    }

    pub(super) fn edit_overlay_text(&mut self, range: Option<Range<usize>>, text: &str) {
        if matches!(
            self.overlay,
            Some(
                Overlay::PaneActions
                    | Overlay::TabActions { .. }
                    | Overlay::TabPaneActions { .. }
                    | Overlay::HeaderActions { .. }
                    | Overlay::QuickAppearance
                    | Overlay::Settings
                    | Overlay::Confirm { .. }
                    | Overlay::Diagnostics
                    | Overlay::Arrangements { .. }
            )
        ) {
            return;
        }
        let range = range
            .or_else(|| self.ime_marked_range.clone())
            .unwrap_or_else(|| self.ime_selected_range.clone());
        let length = self.ime_text.encode_utf16().count();
        let start = range.start.min(length);
        let end = range.end.max(start).min(length);
        self.ime_text.replace_range(
            utf16_byte_index(&self.ime_text, start)..utf16_byte_index(&self.ime_text, end),
            text,
        );
        let cursor = start + text.encode_utf16().count();
        self.ime_selected_range = cursor..cursor;
        self.ime_marked_range = None;
        self.overlay_index = 0;
    }

    fn execute(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(reason) = command.disabled_reason(&self.command_context(cx)) {
            self.global_error = Some(reason.into());
            cx.notify();
            return;
        }
        let tab_id = self.selected_tab().map(|tab| tab.id.clone());
        let pane_id = self.focused_view().map(|view| view.pane_id.clone());
        let surface = self
            .focused_view()
            .and_then(|view| self.workspace.as_ref()?.surface(&view.surface_id))
            .cloned();
        match command {
            Command::OpenPalette => self.open_overlay(Overlay::Palette, ""),
            Command::CreateWorkspace => self.open_overlay(
                Overlay::Text {
                    purpose: TextPurpose::CreateWorkspace,
                    title: "New workspace",
                },
                "",
            ),
            Command::SwitchWorkspace => self.open_overlay(Overlay::Workspaces, ""),
            Command::RenameWorkspace => {
                if let Some(session) = self
                    .workspace
                    .as_ref()
                    .and_then(|workspace| self.state.selected_session(workspace))
                {
                    let id = session.id.clone();
                    let label = session.label.clone();
                    self.open_overlay(
                        Overlay::Text {
                            purpose: TextPurpose::RenameWorkspace(id),
                            title: "Rename workspace",
                        },
                        &label,
                    );
                }
            }
            Command::RemoveWorkspace => {
                if let Some(session_id) = self.state.selected_session.clone() {
                    self.confirm_removal(
                        WorkspaceMutation::RemoveSession { session_id },
                        "Remove workspace and end all its processes?",
                    );
                }
            }
            Command::NewTab => self.create_terminal(inherited_working_directory(
                self.focused_view().and_then(|view| view.mirror.snapshot()),
            )),
            Command::SwitchTab => self.open_overlay(Overlay::Tabs { hidden_only: false }, ""),
            Command::RestoreHiddenTab => self.open_overlay(Overlay::Tabs { hidden_only: true }, ""),
            Command::PreviousTab | Command::NextTab => {
                if let Some(session) = self
                    .workspace
                    .as_ref()
                    .and_then(|workspace| self.state.selected_session(workspace))
                {
                    let tabs: Vec<_> = self
                        .state
                        .visible_tabs(session)
                        .map(|tab| tab.id.clone())
                        .collect();
                    if !tabs.is_empty() {
                        let index = tabs
                            .iter()
                            .position(|id| Some(id) == tab_id.as_ref())
                            .unwrap_or(0);
                        let next = if command == Command::PreviousTab {
                            (index + tabs.len() - 1) % tabs.len()
                        } else {
                            (index + 1) % tabs.len()
                        };
                        self.select_terminal(tabs[next].clone(), false);
                    }
                }
            }
            Command::RenameTab => {
                if let Some(tab) = self.selected_tab() {
                    let id = tab.id.clone();
                    let label = tab.label.clone();
                    self.open_overlay(
                        Overlay::Text {
                            purpose: TextPurpose::RenameTab(id),
                            title: "Rename terminal tab",
                        },
                        &label,
                    );
                }
            }
            Command::MoveTabLeft | Command::MoveTabRight => {
                if let (Some(tab_id), Some(session_id)) =
                    (tab_id, self.state.selected_session.clone())
                {
                    let index = self
                        .workspace
                        .as_ref()
                        .and_then(|workspace| {
                            workspace
                                .sessions
                                .iter()
                                .find(|session| session.id == session_id)
                        })
                        .and_then(|session| session.tabs.iter().position(|tab| tab.id == tab_id))
                        .unwrap_or(0);
                    self.mutate(
                        WorkspaceMutation::MoveTab {
                            session_id,
                            tab_id,
                            index: if command == Command::MoveTabLeft {
                                index.saturating_sub(1)
                            } else {
                                index + 1
                            },
                        },
                        false,
                    );
                }
            }
            Command::DetachTab => {
                if let Some(tab_id) = tab_id {
                    self.hide_terminal(&tab_id);
                }
            }
            Command::RemoveTab => {
                if let Some(tab_id) = tab_id {
                    self.confirm_removal(
                        WorkspaceMutation::RemoveTab { tab_id },
                        "Remove terminal tab and end every process in its panes?",
                    );
                }
            }
            Command::NewWindow => {
                if let Err(error) =
                    open_compi_window(self.target.clone(), None, self.config.clone(), None, cx)
                {
                    self.global_error = Some(error.to_string());
                }
            }
            Command::MoveTabToNewWindow => {
                if let Some(tab_id) = tab_id {
                    self.transfer_tab(tab_id, None, window, cx);
                }
            }
            Command::MoveTabToWindow => self.open_overlay(Overlay::Windows, ""),
            Command::SplitRight | Command::SplitDown => {
                if tab_id
                    .as_ref()
                    .is_some_and(|tab_id| self.pane_zoom.clear(tab_id))
                {
                    self.zoom_layout = None;
                    self.workspace_scroll.set_offset(point(px(0.0), px(0.0)));
                    self.rebuild_layout(window, true);
                }
                if let Some(pane_id) = pane_id {
                    let axis = if command == Command::SplitRight {
                        SplitAxis::Horizontal
                    } else {
                        SplitAxis::Vertical
                    };
                    if let Some(layout) = &self.layout {
                        match layout.split_feasibility(&pane_id, axis) {
                            Ok(rect) => {
                                let metrics = self.metrics();
                                self.mutate(
                                    WorkspaceMutation::SplitPane {
                                        pane_id,
                                        axis,
                                        cols: self.terminal_cols,
                                        rows: self.terminal_rows,
                                        working_directory: inherited_working_directory(
                                            self.focused_view()
                                                .and_then(|view| view.mirror.snapshot()),
                                        ),
                                        geometry: compi_protocol::SplitGeometry {
                                            width: rect.width,
                                            height: rect.height,
                                            min_width: 20.0 * metrics.cell_width
                                                + 2.0 * metrics.padding_x,
                                            min_height: 4.0 * metrics.line_height
                                                + 2.0 * metrics.padding_y
                                                + metrics.pane_chrome_height,
                                            divider: metrics.divider_thickness,
                                        },
                                    },
                                    true,
                                );
                            }
                            Err(reason) => self.global_error = Some(reason.into()),
                        }
                    }
                }
            }
            Command::TogglePaneZoom => {
                if let (Some(tab_id), Some(pane_id)) = (tab_id, pane_id) {
                    self.pane_zoom.toggle(tab_id, pane_id);
                    self.divider_drag = None;
                    self.preview_layout = None;
                    self.workspace_scroll.set_offset(point(px(0.0), px(0.0)));
                }
            }
            Command::FocusLeft | Command::FocusRight | Command::FocusUp | Command::FocusDown => {
                let direction = match command {
                    Command::FocusLeft => layout::Direction::Left,
                    Command::FocusRight => layout::Direction::Right,
                    Command::FocusUp => layout::Direction::Up,
                    _ => layout::Direction::Down,
                };
                if let Some(next) = self
                    .layout
                    .as_ref()
                    .and_then(|layout| {
                        pane_id
                            .as_ref()
                            .and_then(|pane| layout.focus_neighbor(pane, direction))
                    })
                    .cloned()
                {
                    self.focus_pane(next);
                }
            }
            Command::ResizeSplitDecrease
            | Command::ResizeSplitIncrease
            | Command::ResetSplitRatio => {
                if let Some(layout) = &self.layout
                    && let Some(divider) = pane_id
                        .as_ref()
                        .and_then(|pane| layout.divider_for_pane(pane))
                {
                    let value = if command == Command::ResetSplitRatio {
                        0.5
                    } else {
                        divider.saved_ratio
                            + if command == Command::ResizeSplitDecrease {
                                -0.05
                            } else {
                                0.05
                            }
                    };
                    if divider.max_ratio <= divider.min_ratio {
                        self.global_error =
                            Some("The divider cannot move at this window size".into());
                    } else if let Some(tab_id) = tab_id {
                        self.mutate(
                            WorkspaceMutation::SetSplitRatio {
                                tab_id,
                                path: divider.path.clone(),
                                ratio: value.clamp(divider.min_ratio, divider.max_ratio),
                            },
                            false,
                        );
                    }
                }
            }
            Command::DetachPane => {
                if let Some(pane_id) = pane_id {
                    self.mutate(WorkspaceMutation::DetachPane { pane_id }, true);
                }
            }
            Command::TogglePaneFloat => {
                if let Some(pane_id) = pane_id {
                    if self.state.is_floating(&pane_id) {
                        self.dock_pane(&pane_id);
                    } else {
                        self.float_pane(pane_id);
                    }
                }
            }
            Command::ToggleFloatingFocus | Command::NextFloatingPane => {
                let target = self.workspace.as_ref().and_then(|workspace| {
                    let float_focused = self.state.focused_float(workspace).is_some();
                    match (command, float_focused) {
                        (Command::ToggleFloatingFocus, true) => {
                            self.state.tiled_focus(workspace).cloned()
                        }
                        // Raising the back-most float cycles through every float.
                        (Command::NextFloatingPane, true) => self
                            .state
                            .floating
                            .first()
                            .map(|float| float.pane_id.clone()),
                        _ => self
                            .state
                            .floating
                            .last()
                            .map(|float| float.pane_id.clone()),
                    }
                });
                if let Some(target) = target {
                    self.focus_pane(target);
                }
            }
            Command::ArrangePanes => self.open_arrangements(),
            Command::SaveArrangement => self.open_save_arrangement(),
            Command::MirrorArrangement
            | Command::FlipArrangement
            | Command::RestoreArrangement
            | Command::SplitMergedTabs
            | Command::SwapPaneLeft
            | Command::SwapPaneRight
            | Command::SwapPaneUp
            | Command::SwapPaneDown => self.run_arrangement_command(command, window),
            Command::RemovePane => {
                if let Some(pane_id) = pane_id {
                    self.confirm_removal(
                        WorkspaceMutation::RemovePane { pane_id },
                        "Remove pane and end its process tree?",
                    );
                }
            }
            Command::EndSurface => {
                if let Some(surface) = surface {
                    self.confirm_removal(
                        WorkspaceMutation::EndSurface {
                            surface_id: surface.id,
                            expected_lifetime: surface.process_lifetime_id,
                        },
                        "End this surface's process tree? The final grid will remain readable.",
                    );
                }
            }
            Command::RestartSurface => {
                if let Some(surface) = surface {
                    self.mutate(
                        WorkspaceMutation::RestartSurface {
                            surface_id: surface.id,
                            expected_lifetime: surface.process_lifetime_id,
                            cols: self.terminal_cols,
                            rows: self.terminal_rows,
                        },
                        false,
                    );
                }
            }
            Command::ToggleSidebar => {
                self.sidebar_open = !self.sidebar_open;
                if self.sidebar_open {
                    self.clear_whats_new_dot(window, cx);
                }
            }
            Command::ResetSidebarWidth => {
                self.sidebar_width = self.config.configured_sidebar_width;
                self.state.sidebar_width = self.sidebar_width;
                self.save_state();
            }
            Command::BrowseFiles => self.open_file_tree(),
            Command::JumpProject => self.open_project_jump(),
            Command::Copy => self.copy_selection(cx),
            Command::Paste => self.paste_clipboard(cx),
            Command::SelectAll => {
                if let Some(view) = self.focused_view_mut()
                    && let Some(snapshot) = view.mirror.snapshot()
                {
                    view.selection = Some(Selection {
                        anchor: GridPoint { row: 0, col: 0 },
                        head: GridPoint {
                            row: snapshot.scrollback.len() + snapshot.cells.len().saturating_sub(1),
                            col: usize::from(snapshot.cols).saturating_sub(1),
                        },
                    });
                }
                self.save_state();
            }
            Command::ClearScrollback => {
                if let Some(view) = self.focused_view_mut() {
                    view.selection = None;
                    view.scroll_offset = 0;
                    view.send(ClientMessage::ClearScrollback);
                }
            }
            Command::ZoomIn | Command::ZoomOut | Command::ZoomReset => {
                self.zoom = if command == Command::ZoomReset {
                    1.0
                } else {
                    (self.zoom
                        + if command == Command::ZoomIn {
                            0.1
                        } else {
                            -0.1
                        })
                    .clamp(0.5, 3.0)
                };
                self.state.font_zoom = self.zoom;
                self.font_settings = self.config.configured_font.clone();
                self.typography_scale = 0.0;
                self.refresh_typography(window);
                self.save_state();
            }
            Command::OpenQuickAppearance => {
                self.open_overlay(Overlay::QuickAppearance, "");
            }
            Command::OpenSettings => self.open_overlay(Overlay::Settings, ""),
            Command::CheckForUpdates => {
                self.settings_section = SettingsSection::Updates;
                self.open_overlay(Overlay::Settings, "");
                self.updates.check();
            }
            Command::OpenPromptSettings => {
                self.settings_section = SettingsSection::Terminal;
                self.settings_font_picker = None;
                self.open_overlay(Overlay::Settings, "");
                self.focus_prompt_group();
            }
            Command::OpenThemeCatalog => self.open_theme_catalog(SettingsScope::Global),
            Command::OpenConfiguration => {
                let path = self.config.path.clone();
                if !path.as_os_str().is_empty() {
                    if let Err(error) = open_local_path(&path) {
                        self.global_error = Some(error);
                    }
                } else {
                    self.global_error =
                        Some("No configuration path could be resolved; see Diagnostics".into());
                }
            }
            Command::ResetClientLayout => {
                self.state.reset(&self.defaults);
                for view in &mut self.surface_views {
                    view.scroll_offset = 0;
                    view.selection = None;
                }
                self.sidebar_width = self.state.sidebar_width;
                self.zoom = self.state.font_zoom;
                let effective = inherited_appearance(&self.config, &self.state.appearance);
                self.set_theme(self.resolve_theme(&effective.theme));
                self.set_terminal_theme(self.resolve_theme(effective.effective_terminal_theme()));
                self.terminal_opacity = effective.terminal_opacity;
                self.background_effect = effective.background_effect;
                self.terminal_theme_override = effective.terminal_theme_override;
                self.transparent_background = effective.transparent_background;
                self.apply_window_background(window);
                if let Some(workspace) = &self.workspace {
                    self.state.reconcile(None, workspace);
                }
                self.sync_visible_views();
                self.save_state();
                window.resize(size(px(960.0), px(640.0)));
            }
            Command::Reconnect => {
                for view in &mut self.surface_views {
                    view.stop.store(true, Ordering::Release);
                    if let Some(transport) = view.transport.take() {
                        transport.close();
                    }
                    view.image_pending.clear();
                    view.image_rejected.clear();
                    view.image_error = None;
                }
                self.global_error = None;
                self.sync_visible_views();
                self.refresh_surfaces(false);
            }
            Command::RestartDaemon => {
                let target = self.target.clone();
                let sender = self.event_tx.clone();
                thread::spawn(move || {
                    sender.send(UiEvent::LifecycleLoaded(
                        target.lifecycle_status().map_err(|error| error.to_string()),
                    ));
                });
            }
            Command::OpenDiagnostics => self.open_overlay(Overlay::Diagnostics, ""),
            Command::Quit => cx.quit(),
        }
        self.rebuild_layout(window, true);
        cx.notify();
    }

    fn confirm_removal(&mut self, operation: WorkspaceMutation, title: &str) {
        self.open_overlay(
            Overlay::Confirm {
                operation,
                title: title.into(),
                revision: self
                    .workspace
                    .as_ref()
                    .map_or(0, |workspace| workspace.revision),
            },
            "",
        );
    }

    fn pane_action_reason(
        &self,
        tab_id: &TabId,
        pane_id: &PaneId,
        action: PaneAction,
    ) -> Option<&'static str> {
        let Some((count, surface_id)) = self.tab_pane_target(tab_id, pane_id) else {
            return Some("This terminal is no longer in this tab");
        };
        match action {
            PaneAction::Detach if count < 2 => Some("The tab has only one pane"),
            PaneAction::Float
                if !self.state.is_floating(pane_id)
                    && self.state.floating.len() >= crate::client_state::MAX_FLOATING =>
            {
                Some("Dock a floating pane first")
            }
            PaneAction::End
                if self
                    .workspace
                    .as_ref()
                    .and_then(|workspace| workspace.surface(&surface_id))
                    .is_none_or(|surface| {
                        !matches!(
                            surface.status,
                            SurfaceStatus::Running | SurfaceStatus::Starting
                        )
                    }) =>
            {
                Some("The terminal process is not running")
            }
            _ => None,
        }
    }

    fn activate_tab_pane_action(
        &mut self,
        tab_id: TabId,
        pane_id: PaneId,
        action: PaneAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let valid_menu = matches!(
            &self.overlay,
            Some(Overlay::TabPaneActions {
                tab_id: clicked,
                pane_id: selected,
                structure,
                ..
            }) if clicked == &tab_id && selected == &pane_id
                && self.workspace.as_ref().is_some_and(|workspace| structure_key(workspace) == *structure)
        );
        if !valid_menu {
            self.dismiss_overlay();
            self.global_error = Some(STALE_TAB_MENU.into());
        } else if let Some(reason) = self.pane_action_reason(&tab_id, &pane_id, action) {
            self.global_error = Some(reason.into());
        } else {
            self.dismiss_overlay();
            match action {
                PaneAction::Detach => self.mutate(WorkspaceMutation::DetachPane { pane_id }, true),
                PaneAction::Float => {
                    if self.state.is_floating(&pane_id) {
                        self.dock_pane(&pane_id);
                    } else {
                        self.float_pane(pane_id);
                    }
                }
                PaneAction::End => {
                    if let Some((_, surface_id)) = self.tab_pane_target(&tab_id, &pane_id)
                        && let Some(surface) = self
                            .workspace
                            .as_ref()
                            .and_then(|workspace| workspace.surface(&surface_id))
                    {
                        self.confirm_removal(
                            WorkspaceMutation::EndSurface {
                                surface_id: surface.id.clone(),
                                expected_lifetime: surface.process_lifetime_id.clone(),
                            },
                            "End this terminal's process tree? The final grid will remain readable.",
                        );
                    }
                }
            }
        }
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn activate_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(overlay) = self.overlay.clone() else {
            return;
        };
        if let Overlay::TabActions { structure, .. } | Overlay::TabPaneActions { structure, .. } =
            &overlay
            && self
                .workspace
                .as_ref()
                .is_none_or(|workspace| structure_key(workspace) != *structure)
        {
            self.dismiss_overlay();
            self.global_error = Some(STALE_TAB_MENU.into());
            return;
        }
        match overlay {
            Overlay::ThemeCatalog => self.apply_catalog_theme(window, cx),
            Overlay::QuickAppearance | Overlay::Settings | Overlay::ImageInspector => {
                self.dismiss_overlay()
            }
            Overlay::Text { purpose, .. } => {
                let label = self.ime_text.trim().to_owned();
                if label.is_empty() {
                    self.global_error = Some("Enter a non-empty name".into());
                    return;
                }
                self.dismiss_overlay();
                let operation = match purpose {
                    TextPurpose::CreateWorkspace => WorkspaceMutation::CreateSession { label },
                    TextPurpose::RenameWorkspace(session_id) => {
                        WorkspaceMutation::RenameSession { session_id, label }
                    }
                    TextPurpose::RenameTab(tab_id) => {
                        WorkspaceMutation::RenameTab { tab_id, label }
                    }
                    TextPurpose::SaveArrangement(tab_id) => {
                        self.save_arrangement(&tab_id, &label);
                        window.focus(&self.focus_handle);
                        cx.notify();
                        return;
                    }
                };
                self.mutate(operation, true);
            }
            Overlay::Confirm {
                operation,
                revision,
                ..
            } => {
                if self
                    .workspace
                    .as_ref()
                    .is_none_or(|workspace| workspace.revision != revision)
                {
                    self.global_error = Some("Workspace changed while confirmation was open. Cancel and review the current targets before removing work.".into());
                    return;
                }
                self.dismiss_overlay();
                self.mutate(operation, false);
            }
            Overlay::ConfirmDaemonRestart {
                revision, consent, ..
            } => {
                if self
                    .workspace
                    .as_ref()
                    .is_none_or(|workspace| workspace.revision != revision)
                {
                    self.global_error = Some(
                        "Workspace changed while confirmation was open. Cancel and review the live surfaces before restarting the daemon."
                            .into(),
                    );
                    return;
                }
                self.dismiss_overlay();
                self.begin_daemon_restart(consent);
            }
            Overlay::Diagnostics => self.dismiss_overlay(),
            other => {
                let choices = self.overlay_choices(cx);
                if let Some(choice) = choices.get(self.overlay_index).cloned() {
                    // A tab menu's command rows act on the clicked tab, which right-click
                    // does not select; `execute` re-checks enablement after selecting it.
                    let menu_tab = match (&other, &choice.action) {
                        (
                            Overlay::TabActions { tab_id, .. }
                            | Overlay::TabPaneActions { tab_id, .. },
                            ChoiceAction::Command(_),
                        ) => Some(tab_id.clone()),
                        _ => None,
                    };
                    if menu_tab.is_none()
                        && let Some(reason) = choice.reason
                    {
                        self.global_error = Some(reason);
                        return;
                    }
                    if !matches!(
                        choice.action,
                        ChoiceAction::Pane { .. } | ChoiceAction::ArrangementTab(_)
                    ) {
                        self.dismiss_overlay();
                    }
                    if let Some(tab_id) = menu_tab
                        && self.selected_tab().is_none_or(|tab| tab.id != tab_id)
                    {
                        self.select_terminal(tab_id, false);
                        self.rebuild_layout(window, true);
                    }
                    match choice.action {
                        ChoiceAction::Command(command) => self.execute(command, window, cx),
                        ChoiceAction::Workspace(id) => {
                            if let Some(workspace) = &self.workspace {
                                self.state.select_session(workspace, &id);
                            }
                            self.sync_visible_views();
                            self.save_state();
                        }
                        ChoiceAction::Tab(id) => self.select_terminal(id, true),
                        ChoiceAction::Pane { tab_id, pane_id } => {
                            if let Overlay::TabActions {
                                position,
                                structure,
                                ..
                            }
                            | Overlay::TabPaneActions {
                                position,
                                structure,
                                ..
                            } = other
                            {
                                self.overlay = Some(Overlay::TabPaneActions {
                                    position,
                                    tab_id: tab_id.clone(),
                                    pane_id: pane_id.clone(),
                                    structure,
                                });
                                self.overlay_focus = self
                                    .next_pane_action(&tab_id, &pane_id, None, true)
                                    .unwrap_or(0);
                            }
                        }
                        ChoiceAction::Window(handle) => {
                            if let Some(tab) = self.selected_tab().map(|tab| tab.id.clone()) {
                                self.transfer_tab(tab, Some(handle), window, cx);
                            }
                        }
                        ChoiceAction::Arrangement(choice) => {
                            if let Overlay::Arrangements {
                                tab_id,
                                transform,
                                merge,
                                ..
                            } = &other
                            {
                                self.apply_arrangement_choice(
                                    tab_id, merge, &choice, *transform, window,
                                );
                            }
                        }
                        ChoiceAction::ArrangementTab(source) => {
                            self.toggle_arrangement_tab(&source);
                        }
                    }
                }
            }
        }
        window.focus(&self.focus_handle);
        cx.notify();
    }
}

#[derive(Clone)]
enum ChoiceAction {
    Command(Command),
    Workspace(SessionId),
    Tab(TabId),
    Pane {
        tab_id: TabId,
        pane_id: PaneId,
    },
    Window(AnyWindowHandle),
    Arrangement(arrangements::ArrangementChoice),
    /// Toggles whether another tab is merged by the arrangement picker.
    ArrangementTab(TabId),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum PaneAction {
    Detach,
    Float,
    End,
}

impl PaneAction {
    /// Bubble order; `overlay_focus` indexes this list.
    const ALL: [Self; 3] = [Self::Detach, Self::Float, Self::End];
}

impl CompiApp {
    /// The next enabled bubble action after `from` (or the first when `None`),
    /// wrapping in either direction; disabled actions are skipped.
    fn next_pane_action(
        &self,
        tab_id: &TabId,
        pane_id: &PaneId,
        from: Option<usize>,
        forward: bool,
    ) -> Option<usize> {
        let count = PaneAction::ALL.len();
        (1..=count)
            .map(|step| match (from, forward) {
                (None, _) => step - 1,
                (Some(start), true) => (start + step) % count,
                (Some(start), false) => (start + count * 2 - step) % count,
            })
            .find(|&index| {
                self.pane_action_reason(tab_id, pane_id, PaneAction::ALL[index])
                    .is_none()
            })
    }
}

#[derive(Clone)]
struct Choice {
    title: String,
    detail: String,
    group: Option<&'static str>,
    reason: Option<String>,
    action: ChoiceAction,
}

fn open_local_path(path: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file
            .write_all(b"version = 1\n")
            .map_err(|error| error.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    #[cfg(windows)]
    let result = std::process::Command::new("notepad.exe").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open")
        .arg("-e")
        .arg(path)
        .spawn();
    result.map(|_| ()).map_err(|error| error.to_string())
}

fn concise_path_title(path: &str) -> String {
    let path = path.trim();
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return if path.starts_with('/') {
            "/".into()
        } else {
            "Terminal".into()
        };
    }
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or("Terminal")
        .to_owned()
}

fn concise_tab_title(title: &str) -> String {
    let title = title.trim();
    let bytes = title.as_bytes();
    let is_drive_path = bytes.len() > 2 && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\');
    if title.starts_with('/') || title.starts_with("~/") || title.starts_with('\\') || is_drive_path
    {
        concise_path_title(title)
    } else {
        title.to_owned()
    }
}

#[derive(Clone)]
struct TabPane {
    pane_id: PaneId,
    title: String,
    directory: Option<String>,
    /// Floating in this window; shown in the tab's hover card and pane list.
    floating: bool,
}

fn tab_caption(custom: &str, panes: &[TabPane]) -> (String, Option<String>) {
    let custom = custom.trim();
    let primary = if custom.is_empty() {
        panes
            .first()
            .map(|pane| pane.title.clone())
            .unwrap_or_else(|| "Terminal".into())
    } else {
        custom.to_owned()
    };
    let secondary = match panes.len() {
        0 | 1 => None,
        2 if custom.is_empty() => Some(panes[1].title.clone()),
        2 => Some("2".into()),
        count => Some(format!("{count}+")),
    };
    (primary, secondary)
}

impl CompiApp {
    fn tab_panes(&self, tab: &WorkspaceTab) -> Vec<TabPane> {
        let mut leaves = Vec::new();
        collect_leaves(&tab.layout, &mut leaves);
        leaves
            .into_iter()
            .enumerate()
            .map(|(index, (pane_id, surface_id))| {
                let surface = self
                    .workspace
                    .as_ref()
                    .and_then(|workspace| workspace.surface(&surface_id));
                let view = self
                    .surface_views
                    .iter()
                    .find(|view| view.surface_id == surface_id);
                let snapshot = view.and_then(|view| view.mirror.snapshot());
                let cached = self
                    .surface_names
                    .get(&surface_id)
                    .filter(|(lifetime, _, _)| {
                        surface.is_some_and(|surface| surface.process_lifetime_id == *lifetime)
                    });
                let directory = snapshot
                    .and_then(|snapshot| snapshot.current_directory.clone())
                    .or_else(|| cached.and_then(|(_, _, directory)| directory.clone()))
                    .or_else(|| {
                        surface?
                            .working_directory
                            .as_ref()
                            .map(|cwd| cwd.resolved_wsl_path.clone())
                    });
                let title = snapshot
                    .map(|snapshot| snapshot.title.trim())
                    .filter(|title| !title.is_empty())
                    .map(concise_tab_title)
                    .or_else(|| cached.map(|(_, title, _)| title.clone()))
                    .or_else(|| directory.as_deref().map(concise_path_title))
                    .unwrap_or_else(|| format!("Terminal {}", index + 1));
                TabPane {
                    floating: self.state.is_floating(&pane_id),
                    pane_id,
                    title,
                    directory,
                }
            })
            .collect()
    }

    fn tab_by_id(&self, tab_id: &TabId) -> Option<&WorkspaceTab> {
        self.workspace
            .as_ref()?
            .sessions
            .iter()
            .flat_map(|session| &session.tabs)
            .find(|tab| &tab.id == tab_id)
    }

    fn tab_pane_target(&self, tab_id: &TabId, pane_id: &PaneId) -> Option<(usize, SurfaceId)> {
        let mut leaves = Vec::new();
        collect_leaves(&self.tab_by_id(tab_id)?.layout, &mut leaves);
        let count = leaves.len();
        leaves
            .into_iter()
            .find(|(id, _)| id == pane_id)
            .map(|(_, surface_id)| (count, surface_id))
    }

    fn tab_label(&self, tab: &WorkspaceTab) -> String {
        let panes = self.tab_panes(tab);
        let (primary, secondary) = tab_caption(&tab.label, &panes);
        match secondary {
            Some(secondary) => format!("{primary} · {secondary}"),
            None => primary,
        }
    }

    fn tab_status(&self, tab: &WorkspaceTab) -> String {
        let mut leaves = Vec::new();
        collect_leaves(&tab.layout, &mut leaves);
        let mut statuses = Vec::new();
        for (_, id) in &leaves {
            if let Some(surface) = self
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.surface(id))
            {
                let label = match surface.status {
                    SurfaceStatus::Running => "Running",
                    SurfaceStatus::Starting => "Starting",
                    SurfaceStatus::Ending => "Ending",
                    SurfaceStatus::Exited => "Exited",
                    SurfaceStatus::Failed => "Failed",
                    SurfaceStatus::Lost => "Ended",
                };
                if !statuses.contains(&label) {
                    statuses.push(label);
                }
            }
        }
        let mut status = statuses.join(", ");
        if self.state.hidden_tabs.contains(&tab.id) {
            status.push_str(" · Hidden");
        }
        if leaves.len() > 1 {
            status.push_str(&format!(" · {} panes", leaves.len()));
        }
        status
    }
    fn command_label(&self, command: Command) -> &'static str {
        if command == Command::TogglePaneZoom && self.pane_zoomed() {
            "Restore split layout"
        } else if command == Command::TogglePaneFloat
            && self
                .focused_view()
                .is_some_and(|view| self.state.is_floating(&view.pane_id))
        {
            "Dock pane"
        } else {
            command.spec().label
        }
    }

    fn configured_shortcut(&self, command: Command) -> Option<&str> {
        let platform = if cfg!(target_os = "macos") {
            commands::Platform::Mac
        } else {
            commands::Platform::Windows
        };
        command
            .spec()
            .configured_shortcut(platform, &self.config.keybindings)
    }

    fn overlay_choices(&self, cx: &App) -> Vec<Choice> {
        let query = self.ime_text.to_lowercase();
        let mut choices = Vec::new();
        match &self.overlay {
            Some(
                Overlay::PaneActions
                | Overlay::TabActions { .. }
                | Overlay::TabPaneActions { .. }
                | Overlay::HeaderActions { .. },
            ) => {
                let context = self.command_context(cx);
                let commands: &[Command] = match self.overlay.as_ref() {
                    Some(Overlay::PaneActions) => &[
                        Command::SplitRight,
                        Command::SplitDown,
                        Command::TogglePaneZoom,
                        Command::TogglePaneFloat,
                    ],
                    Some(Overlay::TabActions { .. } | Overlay::TabPaneActions { .. }) => &[
                        Command::NewTab,
                        Command::RenameTab,
                        Command::SplitRight,
                        Command::SplitDown,
                        Command::ArrangePanes,
                        Command::RestoreArrangement,
                        Command::MoveTabToNewWindow,
                        Command::DetachTab,
                        Command::RemoveTab,
                    ],
                    Some(Overlay::HeaderActions { .. }) => &[
                        Command::NewTab,
                        Command::CreateWorkspace,
                        Command::SplitRight,
                        Command::SplitDown,
                        Command::ToggleSidebar,
                    ],
                    _ => unreachable!(),
                };
                for &command in commands {
                    let title = match command {
                        Command::NewTab => "New Tab",
                        Command::CreateWorkspace => "New Workspace",
                        Command::RenameTab => "Rename Tab",
                        Command::SplitRight => "Split Right",
                        Command::SplitDown => "Split Down",
                        Command::ArrangePanes => "Arrange",
                        Command::RestoreArrangement => "Restore Previous Arrangement",
                        Command::MoveTabToNewWindow => "Move to New Window",
                        Command::DetachTab => "Hide Tab",
                        Command::RemoveTab => "Remove Tab",
                        Command::ToggleSidebar => "Toggle Sidebar",
                        _ => self.command_label(command),
                    };
                    choices.push(Choice {
                        title: title.into(),
                        detail: self.configured_shortcut(command).unwrap_or("").into(),
                        group: None,
                        reason: command.disabled_reason(&context).map(str::to_owned),
                        action: ChoiceAction::Command(command),
                    });
                }
                if let Some(
                    Overlay::TabActions { tab_id, .. } | Overlay::TabPaneActions { tab_id, .. },
                ) = &self.overlay
                    && let Some(tab) = self.tab_by_id(tab_id)
                {
                    for (index, pane) in self.tab_panes(tab).into_iter().enumerate() {
                        choices.push(Choice {
                            title: if pane.floating {
                                format!("{}. {} · Floating", index + 1, pane.title)
                            } else {
                                format!("{}. {}", index + 1, pane.title)
                            },
                            detail: pane.directory.unwrap_or_default(),
                            group: Some("Terminals"),
                            reason: None,
                            action: ChoiceAction::Pane {
                                tab_id: tab_id.clone(),
                                pane_id: pane.pane_id,
                            },
                        });
                    }
                }
            }
            Some(Overlay::QuickAppearance | Overlay::Settings | Overlay::ThemeCatalog) => {}
            Some(Overlay::Palette) => {
                let context = self.command_context(cx);
                for category in commands::CommandCategory::ALL {
                    for spec in commands::REGISTRY.iter().filter(|spec| {
                        spec.command != Command::OpenPalette
                            && spec.category() == category
                            && spec.matches_query(&self.ime_text)
                    }) {
                        choices.push(Choice {
                            title: self.command_label(spec.command).into(),
                            detail: self.configured_shortcut(spec.command).unwrap_or("").into(),
                            group: Some(category.label()),
                            reason: spec.command.disabled_reason(&context).map(str::to_owned),
                            action: ChoiceAction::Command(spec.command),
                        });
                    }
                }
                if let Some(workspace) = &self.workspace {
                    for session in &workspace.sessions {
                        for tab in &session.tabs {
                            let title = format!("{} / {}", session.label, self.tab_label(tab));
                            if !query.is_empty() && title.to_lowercase().contains(&query) {
                                choices.push(Choice {
                                    title,
                                    detail: self.tab_status(tab),
                                    group: Some("Open tabs"),
                                    reason: None,
                                    action: ChoiceAction::Tab(tab.id.clone()),
                                });
                            }
                        }
                    }
                }
            }
            Some(Overlay::Workspaces) => {
                if let Some(workspace) = &self.workspace {
                    for session in &workspace.sessions {
                        if session.label.to_lowercase().contains(&query) {
                            choices.push(Choice {
                                title: session.label.clone(),
                                detail: format!("{} terminal tabs", session.tabs.len()),
                                group: None,
                                reason: None,
                                action: ChoiceAction::Workspace(session.id.clone()),
                            });
                        }
                    }
                }
            }
            Some(Overlay::Tabs { hidden_only }) => {
                if let Some(workspace) = &self.workspace {
                    for session in &workspace.sessions {
                        for tab in &session.tabs {
                            let title = format!("{} / {}", session.label, self.tab_label(tab));
                            if (!hidden_only || self.state.hidden_tabs.contains(&tab.id))
                                && title.to_lowercase().contains(&query)
                            {
                                choices.push(Choice {
                                    title,
                                    detail: self.tab_status(tab),
                                    group: None,
                                    reason: None,
                                    action: ChoiceAction::Tab(tab.id.clone()),
                                });
                            }
                        }
                    }
                }
            }
            Some(Overlay::Windows) => {
                for handle in cx.windows() {
                    if let Some(typed) = handle.downcast::<CompiApp>()
                        && let Ok(other) = typed.read(cx)
                        && !std::ptr::eq(other, self)
                        && other.target == self.target
                    {
                        let title = other.window_title.clone();
                        if title.to_lowercase().contains(&query) {
                            choices.push(Choice {
                                title,
                                detail: other.slot_id.clone(),
                                group: None,
                                reason: None,
                                action: ChoiceAction::Window(handle),
                            });
                        }
                    }
                }
            }
            Some(Overlay::Arrangements {
                tab_id,
                confirm_delete,
                merge,
                ..
            }) => {
                choices = self.arrangement_choices(tab_id, merge, confirm_delete.as_deref());
            }
            _ => {}
        }
        choices
    }

    pub(super) fn render_workspace_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        record_frame_metrics();
        let performance_visible = matches!(self.overlay, Some(Overlay::Settings))
            && self.settings_section == SettingsSection::Performance;
        let sample_fps = self.state.show_fps || performance_visible;
        self.performance_enabled
            .store(sample_fps, Ordering::Release);
        if sample_fps {
            let view_id = cx.entity_id();
            window.on_next_frame(move |_, cx| cx.notify(view_id));
        }
        if let Some(started) = self.overlay_closing_since {
            if started.elapsed() >= dialogs::OVERLAY_CLOSE_DURATION {
                self.finish_dismiss_overlay();
            } else {
                let view_id = cx.entity_id();
                window.on_next_frame(move |_, cx| cx.notify(view_id));
            }
        }
        if let Some((appearance, favorites, ui_font, terminal_font_family)) =
            self.pending_appearance_reload.take()
        {
            self.sync_global_appearance(
                appearance,
                favorites,
                ui_font,
                terminal_font_family,
                window,
            );
        }
        if self.refresh_typography(window) {
            self.rebuild_layout(window, true);
        }
        if self.rebuild_layout(window, false) {
            let view_id = cx.entity_id();
            window.on_next_frame(move |_, cx| cx.notify(view_id));
        }
        for view in &mut self.surface_views {
            if !view.stop.load(Ordering::Acquire) {
                view.refresh_images(
                    self.typography.cell_width,
                    self.typography.cell_height,
                    self.event_tx.clone(),
                );
            }
        }
        if self.surface_views.iter().any(|view| view.image_retry) {
            let view_id = cx.entity_id();
            window.on_next_frame(move |_, cx| cx.notify(view_id));
        }
        // Release sprite-atlas entries as application caches release offscreen
        // graphics, removed previews, and closed inspections.
        self.rendered_image_ids.clear();
        let images = self
            .surface_views
            .iter()
            .filter(|view| !view.stop.load(Ordering::Acquire))
            .flat_map(|view| view.image_cache.values().map(|(_, image)| image))
            .chain(
                self.image_previews
                    .iter()
                    .map(|preview| &preview.image.thumbnail),
            )
            .chain(
                self.image_inspector
                    .iter()
                    .filter_map(|inspector| inspector.image.as_ref()),
            );
        for image in images {
            let id = Arc::as_ptr(image) as usize;
            self.rendered_image_ids.insert(id);
            self.rendered_images
                .entry(id)
                .or_insert_with(|| image.clone());
        }
        self.rendered_images.retain(|id, image| {
            if self.rendered_image_ids.contains(id) {
                true
            } else {
                let _ = window.drop_image(image.clone());
                false
            }
        });
        let colors = *self.colors();
        let title = self
            .selected_tab()
            .map(|tab| format!("{} · Compi", self.tab_label(tab)))
            .unwrap_or_else(|| "Compi".into());
        if self.window_title != title {
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
            let ids = std::mem::take(&mut self.pending_present_latency_ids);
            window.on_next_frame(move |_, _| {
                for id in ids {
                    perf::log_input_latency_stage(id, "frame_presented", None);
                }
            });
        }
        let notice = self
            .global_error
            .clone()
            .map(|error| (error, false, true))
            .or_else(|| {
                self.connection_error
                    .clone()
                    .map(|error| (error, true, true))
            })
            .or_else(|| {
                self.state_save_error
                    .lock()
                    .ok()?
                    .as_ref()
                    .map(|error| (format!("Window state is unsaved: {error}"), false, false))
            });
        let root = div()
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .when(self.effective_background_opacity() >= 1.0, |root| {
                root.bg(material_color(colors.background, 1.0))
            })
            .flex()
            .flex_col()
            .font_family(self.ui_font.family())
            .text_size(px(UI_BODY_TEXT_SIZE))
            .font_weight(FontWeight::MEDIUM)
            .text_color(color(colors.foreground))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.handle_app_key(event, window, cx)
                    || (this.overlay.is_none() && this.handle_keystroke(&event.keystroke))
                {
                    cx.stop_propagation();
                }
            }))
            .on_mouse_move(cx.listener(Self::on_workspace_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_workspace_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_workspace_mouse_up))
            .child(self.render_titlebar(window, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .when(self.sidebar_open, |row| row.child(self.render_sidebar(cx)))
                    .child(self.render_panes(cx)),
            )
            .when_some(notice, |root, (error, can_reconnect, dismissible)| {
                root.child(
                    div()
                        .mx_2()
                        .mb_2()
                        .flex_none()
                        .px_3()
                        .py_2()
                        .bg(color(colors.surface))
                        .border_1()
                        .border_color(color(colors.error))
                        .text_color(color(colors.error))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().min_w_0().child(error))
                        .when(can_reconnect || dismissible, |banner| {
                            banner.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .gap_3()
                                    .when(can_reconnect, |actions| {
                                        actions.child(self.command_button(
                                            "error-reconnect",
                                            "Reconnect",
                                            Command::Reconnect,
                                            cx,
                                        ))
                                    })
                                    .when(dismissible, |actions| {
                                        actions.child(
                                            div()
                                                .id("dismiss-error")
                                                .cursor_pointer()
                                                .child("Dismiss")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    if can_reconnect {
                                                        this.connection_error = None;
                                                    } else {
                                                        this.global_error = None;
                                                    }
                                                    cx.notify();
                                                })),
                                        )
                                    }),
                            )
                        }),
                )
            })
            .when(self.overlay.is_none(), |root| {
                root.child(self.render_image_previews(cx))
            })
            .when(self.state.show_fps, |root| {
                root.child(self.render_fps_overlay())
            })
            .when(self.overlay.is_some(), |root| {
                root.child(self.render_overlay(window, cx))
            })
            .when(
                self.dragging_tab.is_some() && self.drag_position.is_some(),
                |root| {
                    let position = self.drag_position.unwrap();
                    root.child(
                        div()
                            .absolute()
                            .left(position.x + px(12.0))
                            .top(position.y + px(12.0))
                            .px_3()
                            .py_2()
                            .bg(color(colors.surface))
                            .border_1()
                            .border_color(color(colors.accent))
                            .child("Move terminal tab"),
                    )
                },
            );
        root.into_any_element()
    }

    fn command_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: &'static str,
        command: Command,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let reason = command.disabled_reason(&self.command_context(cx));
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_sm()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .text_color(color(if reason.is_some() {
                colors.muted
            } else {
                colors.foreground
            }))
            .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.execute(command, window, cx);
                cx.stop_propagation();
            }))
            .child(label)
            .into_any_element()
    }

    fn command_icon_button(
        &self,
        id: impl Into<gpui::ElementId>,
        icon: ChromeIcon,
        command: Command,
        active: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let reason = command
            .disabled_reason(&self.command_context(cx))
            .map(str::to_owned);
        let disabled = reason.is_some();
        let label = self.command_label(command);
        let title = match self.configured_shortcut(command) {
            Some(shortcut) => format!("{label} · {shortcut}"),
            None => label.to_owned(),
        };
        div()
            .id(id)
            .size(px(32.0))
            .rounded_sm()
            .flex()
            .items_center()
            .justify_center()
            .text_color(color(if disabled {
                colors.muted
            } else if active {
                colors.accent
            } else {
                colors.muted
            }))
            .when(active, |button| button.bg(color(colors.surface_hover)))
            .hover(move |style| {
                if disabled {
                    style
                } else {
                    style.bg(color(colors.surface_hover)).cursor_pointer()
                }
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .when(!disabled, |button| {
                button.on_click(cx.listener(move |this, _, window, cx| {
                    this.execute(command, window, cx);
                    cx.stop_propagation();
                }))
            })
            .tooltip(move |_, cx| {
                cx.new(|_| HeaderTooltip {
                    title: title.clone(),
                    reason: reason.clone(),
                    colors,
                })
                .into()
            })
            .child(chrome_icon(
                icon,
                color(if active { colors.accent } else { colors.muted }),
            ))
            .into_any_element()
    }

    fn pane_actions_menu_button(&self, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let active = matches!(self.overlay, Some(Overlay::PaneActions));
        div()
            .id("pane-actions-menu")
            .size(px(32.0))
            .rounded_sm()
            .flex()
            .items_center()
            .justify_center()
            .when(active, |button| button.bg(color(colors.surface_hover)))
            .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| {
                this.open_overlay(Overlay::PaneActions, "");
                cx.stop_propagation();
                cx.notify();
            }))
            .tooltip(move |_, cx| {
                cx.new(|_| HeaderTooltip {
                    title: "Pane actions".into(),
                    reason: None,
                    colors,
                })
                .into()
            })
            .child(chrome_icon(
                ChromeIcon::PaneActions,
                color(if active { colors.accent } else { colors.muted }),
            ))
            .into_any_element()
    }

    fn render_titlebar(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let header_alpha = self.effective_background_opacity();
        let (window_width, _) = logical_viewport_dimensions(window);
        let metrics = header_metrics(window_width);
        let trailing_width =
            WINDOW_CONTROLS_WIDTH + HEADER_BUTTON_SLOT_WIDTH + metrics.pane_actions_width;
        let tab_region_right = (window_width - trailing_width).max(TITLEBAR_BRAND_WIDTH);
        let visible: Vec<_> = self
            .workspace
            .as_ref()
            .and_then(|workspace| self.state.selected_session(workspace))
            .map(|session| self.state.visible_tabs(session).collect())
            .unwrap_or_default();
        let tabs = visible.into_iter().enumerate().map(|(index, tab)| {
            let id = tab.id.clone();
            let context_id = id.clone();
            let close_id = id.clone();
            let selected = self
                .selected_tab()
                .is_some_and(|selected| selected.id == id);
            let panes = self.tab_panes(tab);
            let (primary, secondary) = tab_caption(&tab.label, &panes);
            let count_badge = panes.len() > 2 || (!tab.label.trim().is_empty() && panes.len() > 1);
            let tooltip_title = (!tab.label.trim().is_empty()).then(|| tab.label.clone());
            let tooltip_panes = panes
                .into_iter()
                .map(|pane| (pane.title, pane.directory, pane.floating))
                .collect::<Vec<_>>();
            div()
                .id(("terminal-tab", index))
                .group("terminal-tab")
                .relative()
                .h_full()
                .w(px(metrics.tab_width))
                .flex_none()
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .text_color(color(if selected {
                    colors.foreground
                } else {
                    colors.muted
                }))
                .hover(|style| style.cursor_pointer())
                .child(
                    div()
                        .absolute()
                        .left(px(2.0))
                        .right(px(2.0))
                        .top(px(4.0))
                        .bottom(px(4.0))
                        .rounded(px(4.0))
                        .when(selected, |tab| {
                            tab.bg(color(blend_rgb(colors.background, colors.foreground, 0.08))
                                .opacity(header_alpha))
                        })
                        .group_hover("terminal-tab", move |style| {
                            style.bg(color(blend_rgb(
                                colors.background,
                                colors.foreground,
                                if selected { 0.11 } else { 0.04 },
                            ))
                            .opacity(header_alpha))
                        }),
                )
                .when(self.overlay.is_none(), |tab| {
                    tab.tooltip(move |_, cx| {
                        cx.new(|_| TabTooltip {
                            title: tooltip_title.clone(),
                            panes: tooltip_panes.clone(),
                            colors,
                        })
                        .into()
                    })
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.select_terminal(id.clone(), false);
                        this.dragging_tab = Some(id.clone());
                        this.tab_drag_origin = Some(event.position);
                        this.titlebar_drag = false;
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        // The menu targets this tab without switching to it; its pane
                        // actions (including Float) apply to the clicked tab's panes.
                        this.open_overlay(
                            Overlay::TabActions {
                                position: event.position,
                                tab_id: context_id.clone(),
                                structure: this.workspace.as_ref().map_or(0, structure_key),
                            },
                            "",
                        );
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(primary),
                        )
                        .when_some(secondary, |label, secondary| {
                            label.child(div().flex_none().child("·")).child(
                                div()
                                    .min_w_0()
                                    .when(count_badge, |part| part.flex_none())
                                    .when(!count_badge, |part| part.flex_1())
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(secondary),
                            )
                        }),
                )
                .child(
                    div()
                        .id(("hide-tab", index))
                        .size(px(20.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(3.0))
                        .invisible()
                        .group_hover("terminal-tab", |style| style.visible())
                        .hover(move |style| {
                            style.bg(color(blend_rgb(colors.background, colors.foreground, 0.12)))
                        })
                        .cursor_pointer()
                        .text_color(color(colors.muted))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select_terminal(close_id.clone(), false);
                            this.execute(Command::RemoveTab, window, cx);
                            cx.stop_propagation();
                        }))
                        .child(chrome_icon(ChromeIcon::Close, color(colors.muted))),
                )
        });
        div()
            .h(px(CHROME_HEIGHT))
            .w_full()
            .flex_none()
            .relative()
            .bg(material_color(colors.background, header_alpha))
            .border_b_1()
            .border_color(color(colors.border))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, _| {
                    this.titlebar_drag = true;
                    this.tab_drag_origin = Some(event.position);
                    #[cfg(target_os = "macos")]
                    if event.click_count == 2 {
                        window.titlebar_double_click();
                        this.titlebar_drag = false;
                    }
                    #[cfg(windows)]
                    if event.click_count == 2 {
                        toggle_window_maximized(window);
                        this.titlebar_drag = false;
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.open_overlay(
                        Overlay::HeaderActions {
                            position: event.position,
                        },
                        "",
                    );
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id("app-mark-slot")
                    .absolute()
                    .left(px(TITLEBAR_BRAND_WIDTH - 80.0))
                    .top_0()
                    .h_full()
                    .w(px(40.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(chrome_icon(ChromeIcon::Mark, color(colors.accent))),
            )
            .child(
                div()
                    .id("sidebar-toggle-slot")
                    .absolute()
                    .left(px(TITLEBAR_BRAND_WIDTH - 40.0))
                    .top_0()
                    .h_full()
                    .w(px(40.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(self.command_icon_button(
                        "sidebar-toggle",
                        ChromeIcon::Sidebar,
                        Command::ToggleSidebar,
                        self.sidebar_open,
                        cx,
                    ))
                    .when(self.whats_new_dot() || self.update_dot, |slot| {
                        slot.child(
                            div()
                                .absolute()
                                .top(px(9.0))
                                .right(px(9.0))
                                .size(px(6.0))
                                .rounded_full()
                                .bg(color(colors.accent)),
                        )
                    }),
            )
            .when(
                self.drag_position
                    .is_some_and(|position| f32::from(position.y) <= CHROME_HEIGHT),
                |bar| {
                    let x = f32::from(self.drag_position.unwrap().x);
                    let insertion = (((x - TITLEBAR_BRAND_WIDTH) / metrics.tab_width).round()
                        * metrics.tab_width
                        + TITLEBAR_BRAND_WIDTH)
                        .clamp(TITLEBAR_BRAND_WIDTH, tab_region_right);
                    bar.child(
                        div()
                            .absolute()
                            .left(px(insertion))
                            .top(px(5.0))
                            .bottom(px(5.0))
                            .w(px(2.0))
                            .bg(color(colors.accent)),
                    )
                },
            )
            .child(
                div()
                    .id("terminal-tabs")
                    .absolute()
                    .left(px(TITLEBAR_BRAND_WIDTH))
                    .right(px(trailing_width))
                    .h_full()
                    .flex()
                    .overflow_x_scroll()
                    .track_scroll(&self.tab_scroll_handle)
                    .on_scroll_wheel(cx.listener(Self::on_tab_scroll))
                    .children(tabs),
            )
            .child(
                div()
                    .id("new-terminal-slot")
                    .absolute()
                    .right(px(WINDOW_CONTROLS_WIDTH + metrics.pane_actions_width))
                    .top_0()
                    .h_full()
                    .w(px(HEADER_BUTTON_SLOT_WIDTH))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(self.command_icon_button(
                        "new-terminal",
                        ChromeIcon::Add,
                        Command::NewTab,
                        false,
                        cx,
                    )),
            )
            .child(
                div()
                    .id("pane-actions-slot")
                    .absolute()
                    .right(px(WINDOW_CONTROLS_WIDTH))
                    .top_0()
                    .h_full()
                    .w(px(metrics.pane_actions_width))
                    .border_l_1()
                    .border_color(color(colors.border))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(match metrics.pane_actions {
                        PaneActionsMode::Full => div()
                            .flex()
                            .items_center()
                            .child(self.command_icon_button(
                                "split-right",
                                ChromeIcon::SplitRight,
                                Command::SplitRight,
                                false,
                                cx,
                            ))
                            .child(self.command_icon_button(
                                "split-down",
                                ChromeIcon::SplitDown,
                                Command::SplitDown,
                                false,
                                cx,
                            ))
                            .child(self.command_icon_button(
                                "toggle-pane-zoom",
                                if self.pane_zoomed() {
                                    ChromeIcon::PaneRestore
                                } else {
                                    ChromeIcon::PaneZoom
                                },
                                Command::TogglePaneZoom,
                                self.pane_zoomed(),
                                cx,
                            ))
                            .into_any_element(),
                        PaneActionsMode::Compact => self.pane_actions_menu_button(cx),
                    }),
            )
            .when(cfg!(windows), |bar| {
                bar.child(
                    div()
                        .absolute()
                        .right_0()
                        .top_0()
                        .h_full()
                        .w(px(WINDOW_CONTROLS_WIDTH))
                        .flex()
                        .child(window_control(
                            "minimize",
                            ChromeIcon::Minimize,
                            WindowControlArea::Min,
                            false,
                            colors,
                        ))
                        .child(window_control(
                            "maximize",
                            ChromeIcon::Maximize,
                            WindowControlArea::Max,
                            false,
                            colors,
                        ))
                        .child(window_control(
                            "close-window",
                            ChromeIcon::Close,
                            WindowControlArea::Close,
                            true,
                            colors,
                        )),
                )
            })
            .into_any_element()
    }

    fn render_sidebar(&self, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let mut rows = Vec::new();
        if let Some(workspace) = &self.workspace {
            for (index, session) in workspace.sessions.iter().enumerate() {
                let id = session.id.clone();
                let switch_id = id.clone();
                let expanded = self.expanded_workspaces.contains(&id);
                let active = self.state.selected_session.as_ref() == Some(&id);
                rows.push(
                    div()
                        .id(("workspace-heading", index))
                        .px_2()
                        .py_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .bg(color(if active {
                            colors.surface_hover
                        } else {
                            colors.surface
                        })
                        .opacity(if self.glass { 0.88 } else { 1.0 }))
                        .child(
                            div()
                                .id(("workspace-expand", index))
                                .w(px(20.0))
                                .cursor_pointer()
                                .child(if expanded { "⌄" } else { "›" })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.expanded_workspaces.remove(&id) {
                                        this.expanded_workspaces.insert(id.clone());
                                    }
                                    cx.stop_propagation();
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .flex_1()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(session.label.clone()),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(workspace) = &this.workspace {
                                this.state.select_session(workspace, &switch_id);
                            }
                            this.sync_visible_views();
                            this.save_state();
                            cx.notify();
                        }))
                        .into_any_element(),
                );
                if expanded {
                    for (tab_index, tab) in session.tabs.iter().enumerate() {
                        let tab_id = tab.id.clone();
                        let hidden = self.state.hidden_tabs.contains(&tab_id);
                        rows.push(
                            div()
                                .id(("workspace-terminal", index * 1_000_000 + tab_index))
                                .pl_5()
                                .pr_2()
                                .py_2()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .text_color(color(if hidden {
                                    colors.muted
                                } else {
                                    colors.foreground
                                }))
                                .hover(move |style| {
                                    style.bg(color(colors.surface_hover)).cursor_pointer()
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.select_terminal(tab_id.clone(), true);
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(self.tab_label(tab)),
                                )
                                .child(
                                    div()
                                        .text_size(px(UI_MICRO_TEXT_SIZE))
                                        .text_color(color(colors.muted))
                                        .child(self.tab_status(tab)),
                                )
                                .into_any_element(),
                        );
                    }
                }
            }
        }
        div()
            .w(px(self.sidebar_extent()))
            .h_full()
            .flex_none()
            .flex()
            .child(
                div()
                    .w(px(self.sidebar_extent() - self.seam_width()))
                    .h_full()
                    .flex()
                    .flex_col()
                    .bg(color(colors.surface).opacity(if self.glass { 0.9 } else { 1.0 }))
                    .child(
                        div()
                            .px_3()
                            .py_3()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child("Workspaces")
                            .child(self.command_button(
                                "create-workspace",
                                "New",
                                Command::CreateWorkspace,
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .id("workspace-sidebar-list")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.sidebar_scroll)
                            .children(rows),
                    )
                    .children(self.render_whats_new_card(cx))
                    .child(
                        div()
                            .p_2()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .border_t_1()
                            .border_color(color(colors.border))
                            .child(self.command_button(
                                "workspace-actions",
                                "Commands",
                                Command::OpenPalette,
                                cx,
                            ))
                            .child(self.command_button(
                                "appearance-sidebar",
                                "Appearance",
                                Command::OpenQuickAppearance,
                                cx,
                            ))
                            .child(self.command_button(
                                "settings-sidebar",
                                "Settings",
                                Command::OpenSettings,
                                cx,
                            )),
                    ),
            )
            .child(
                // One device pixel of seam; the transparent grab zone extends into the
                // sidebar's padding so the seam stays easy to drag.
                div()
                    .w(px(self.seam_width()))
                    .h_full()
                    .relative()
                    .bg(color(self.seam_color()))
                    .child(
                        div()
                            .id("sidebar-divider")
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .w(px(SEAM_GRAB))
                            .group("sidebar-seam")
                            .cursor(gpui::CursorStyle::ResizeLeftRight)
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .right_0()
                                    .w(px(self.seam_width()))
                                    .when(self.sidebar_drag, |line| line.bg(color(colors.accent)))
                                    .group_hover("sidebar-seam", move |style| {
                                        style.bg(color(colors.accent))
                                    }),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                    if event.click_count == 2 {
                                        this.sidebar_width = this.config.configured_sidebar_width;
                                        this.state.sidebar_width = this.sidebar_width;
                                        this.save_state();
                                    } else {
                                        this.sidebar_drag = true;
                                    }
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_panes(&self, cx: &Context<Self>) -> AnyElement {
        let tiled = self.render_tiled_panes(cx);
        if self.float_layouts.is_empty() {
            return tiled;
        }
        // Floats sit above the split canvas and below modal overlays.
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .relative()
            .flex()
            .child(tiled)
            .children(
                self.float_layouts
                    .iter()
                    .enumerate()
                    .map(|(index, float)| self.render_floating_pane(index, float, cx)),
            )
            .into_any_element()
    }

    fn render_tiled_panes(&self, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let Some(layout) = self.visible_layout() else {
            if let Some(element) = self.render_floated_tab_placeholder(cx) {
                return element;
            }
            return div()
                .flex_1()
                .size_full()
                .min_w_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(material_color(colors.background, self.effective_background_opacity()))
                .child(
                    div()
                        .w_full()
                        .max_w(px(680.0))
                        .px_4()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_3()
                        .text_center()
                        .child(div().text_size(px(18.0)).child("Your work stays here"))
                        .child(
                            div()
                                .w_full()
                                .text_color(color(colors.muted))
                                .child("Create a terminal tab or restore hidden work. Hiding a tab keeps its processes running."),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .justify_center()
                                .gap_2()
                                .child(self.command_button(
                                    "empty-new-terminal",
                                    "New terminal tab",
                                    Command::NewTab,
                                    cx,
                                ))
                                .child(self.command_button(
                                    "empty-new-workspace",
                                    "New workspace",
                                    Command::CreateWorkspace,
                                    cx,
                                ))
                                .child(self.command_button(
                                    "empty-restore",
                                    "Restore hidden tab",
                                    Command::RestoreHiddenTab,
                                    cx,
                                )),
                        ),
                )
                .into_any_element();
        };
        let panes = layout.panes.iter().enumerate().map(|(index, geometry)| {
            self.render_pane(("pane", index), index, geometry, geometry.rect, cx)
        });
        let dividers = layout.dividers.iter().enumerate().map(|(index, divider)| {
            let dragging = self
                .divider_drag
                .as_ref()
                .is_some_and(|drag| drag.index == index);
            let side_by_side = divider.axis == SplitAxis::Horizontal;
            // The visible seam is one device pixel (`divider.rect`). The transparent grab
            // zone around it is wider and overlaps only the panes' padding, never text.
            let zone = if side_by_side {
                layout::Rect {
                    x: divider.rect.x + (divider.rect.width - SEAM_GRAB) / 2.0,
                    width: SEAM_GRAB,
                    ..divider.rect
                }
            } else {
                layout::Rect {
                    y: divider.rect.y + (divider.rect.height - SEAM_GRAB) / 2.0,
                    height: SEAM_GRAB,
                    ..divider.rect
                }
            };
            div()
                .id(("split-divider", index))
                .absolute()
                .left(px(zone.x))
                .top(px(zone.y))
                .w(px(zone.width))
                .h(px(zone.height))
                .group("split-seam")
                .cursor(if side_by_side {
                    gpui::CursorStyle::ResizeLeftRight
                } else {
                    gpui::CursorStyle::ResizeUpDown
                })
                .child(
                    div()
                        .absolute()
                        .left(px(divider.rect.x - zone.x))
                        .top(px(divider.rect.y - zone.y))
                        .w(px(divider.rect.width))
                        .h(px(divider.rect.height))
                        .bg(color(if dragging {
                            colors.accent
                        } else {
                            self.seam_color()
                        }))
                        .group_hover("split-seam", move |style| style.bg(color(colors.accent))),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        if let (Some(workspace), Some(tab), Some(layout)) =
                            (&this.workspace, this.selected_tab(), &this.layout)
                        {
                            if let Some(reason) = layout.dividers[index].disabled_reason() {
                                this.global_error = Some(reason.into());
                            } else {
                                this.divider_drag = Some(DividerDrag {
                                    revision: workspace.revision,
                                    index,
                                    tree: tab.layout.clone(),
                                });
                            }
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
        });
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .relative()
            .overflow_hidden()
            .child(
                div()
                    .id("workspace-canvas-scroll")
                    .w(px(layout.viewport.width))
                    .h(px(layout.viewport.height))
                    .overflow_scroll()
                    .track_scroll(&self.workspace_scroll)
                    .child(
                        div()
                            .relative()
                            .w(px(layout.canvas.width))
                            .h(px(layout.canvas.height))
                            .children(panes)
                            .children(dividers),
                    ),
            )
            .when(layout.canvas.width > layout.viewport.width, |pane| {
                pane.child(self.render_workspace_scrollbar(true, cx))
            })
            .when(layout.canvas.height > layout.viewport.height, |pane| {
                pane.child(self.render_workspace_scrollbar(false, cx))
            })
            .into_any_element()
    }

    /// One pane's terminal (or file tree) at `rect` within its parent. Tiled and
    /// floating presentations share it, so both use the same attached view.
    fn render_pane(
        &self,
        id: impl Into<gpui::ElementId>,
        index: usize,
        geometry: &layout::PaneLayout,
        rect: layout::Rect,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let colors = *self.colors();
        let pane_id = geometry.pane_id.clone();
        let tree_active = self
            .file_tree
            .as_ref()
            .is_some_and(|tree| tree.pane_id == pane_id);
        let focus_id = pane_id.clone();
        let input_id = pane_id.clone();
        let scroll_id = pane_id.clone();
        let move_id = pane_id.clone();
        let release_id = pane_id.clone();
        let release_out_id = pane_id.clone();
        let view = self
            .surface_views
            .iter()
            .find(|view| view.pane_id == pane_id);
        let focused = view.is_some_and(|view| Some(view.id) == self.focused_view);
        let drop_view_id = view.map(|view| view.id);
        let paint = view.and_then(|view| {
            PaintModel::from_tab(
                view,
                self.typography.clone(),
                self.terminal_theme.clone(),
                focused && self.overlay.is_none(),
            )
        });
        let terminal_scroll = view.and_then(|view| {
            let snapshot = view.mirror.snapshot()?;
            if snapshot.modes.alternate_screen || snapshot.scrollback.is_empty() {
                return None;
            }
            let track = (geometry.rect.height - 4.0).max(1.0);
            let visible = snapshot.cells.len().max(1) as f32;
            let history = snapshot.scrollback.len() as f32;
            let thumb = (track * visible / (visible + history)).max(18.0).min(track);
            let progress = 1.0 - view.scroll_offset.min(snapshot.scrollback.len()) as f32 / history;
            Some(((track - thumb) * progress, thumb))
        });
        let surface = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.surface(&geometry.surface_id));
        let status = surface
            .map(|surface| match surface.status {
                SurfaceStatus::Starting => "Starting",
                SurfaceStatus::Running => {
                    if view.is_some_and(|view| view.transport.is_some()) {
                        "Running"
                    } else {
                        "Unavailable"
                    }
                }
                SurfaceStatus::Ending => "Ending…",
                SurfaceStatus::Exited => "Exited",
                SurfaceStatus::Failed => "Failed",
                SurfaceStatus::Lost => "Ended",
            })
            .unwrap_or("Removed");
        let error = view
            .and_then(|view| view.error.clone().or_else(|| view.image_error.clone()))
            .or_else(|| surface.and_then(|surface| surface.error.clone()));
        let input = cx.entity();
        let input_focus = self.focus_handle.clone();
        let composition = (focused && self.overlay.is_none() && !self.ime_text.is_empty())
            .then(|| SharedString::from(self.ime_text.clone()));
        div()
            .id(id)
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.width))
            .h(px(rect.height))
            .bg(material_color(
                if tree_active {
                    colors.background
                } else {
                    self.terminal_theme.terminal().background
                },
                self.effective_background_opacity(),
            ))
            .flex()
            .flex_col()
            .on_drop(
                cx.listener(move |this, paths: &gpui::ExternalPaths, _, cx| {
                    if let Some(view_id) = drop_view_id {
                        this.drop_image_files(paths, view_id, cx);
                    }
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    this.focus_pane(focus_id.clone());
                    this.open_overlay(Overlay::Palette, "");
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .when(!matches!(status, "Running"), |pane| {
                pane.child(
                    div()
                        .flex_none()
                        .min_h(px(36.0))
                        .px_2()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_end()
                        .gap_1()
                        .border_b_1()
                        .border_color(color(colors.border))
                        .bg(color(colors.surface))
                        .child(
                            div()
                                .px_2()
                                .py_1()
                                .text_size(px(UI_SMALL_TEXT_SIZE))
                                .text_color(color(if matches!(status, "Failed" | "Ended") {
                                    colors.error
                                } else {
                                    colors.muted
                                }))
                                .child(status),
                        )
                        .when(status == "Unavailable", |actions| {
                            actions.child(self.pane_command_button(
                                ("retry-pane", index),
                                "Retry attachment",
                                Command::Reconnect,
                                &pane_id,
                                cx,
                            ))
                        })
                        .when(matches!(status, "Exited" | "Failed" | "Ended"), |actions| {
                            actions.child(self.pane_command_button(
                                ("restart-pane", index),
                                "Restart shell",
                                Command::RestartSurface,
                                &pane_id,
                                cx,
                            ))
                        }),
                )
            })
            .when_some(error, |pane, error| {
                pane.child(
                    div()
                        .flex_none()
                        .px_2()
                        .py_1()
                        .border_b_1()
                        .border_color(color(colors.border))
                        .bg(color(colors.surface))
                        .text_color(color(colors.error))
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .child(error),
                )
            })
            .when(!tree_active, |pane| {
                pane.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .p(px(TERMINAL_PADDING))
                        .relative()
                        .overflow_hidden()
                        .cursor(gpui::CursorStyle::IBeam)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                this.focus_pane(input_id.clone());
                                this.on_terminal_mouse_down(event, window, cx);
                                cx.stop_propagation();
                            }),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |this, event, window, cx| {
                                this.on_terminal_mouse_up(event, &release_id, window, cx);
                            }),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            cx.listener(move |this, event, window, cx| {
                                this.on_terminal_mouse_up(event, &release_out_id, window, cx);
                            }),
                        )
                        .on_mouse_move(cx.listener(move |this, event, window, cx| {
                            this.on_terminal_mouse_move(event, &move_id, window, cx);
                        }))
                        .on_scroll_wheel(cx.listener(
                            move |this, event: &ScrollWheelEvent, window, cx| {
                                // Wheel input targets the hovered pane without changing keyboard focus.
                                let previous = this.focused_view;
                                this.focused_view = this
                                    .surface_views
                                    .iter()
                                    .find(|view| view.pane_id == scroll_id)
                                    .map(|view| view.id);
                                this.on_terminal_scroll(event, window, cx);
                                this.focused_view = previous;
                                cx.stop_propagation();
                            },
                        ))
                        .child(
                            canvas(
                                move |_, _, _| (),
                                move |bounds, _, window, cx| {
                                    if focused {
                                        window.handle_input(
                                            &input_focus,
                                            ElementInputHandler::new(bounds, input.clone()),
                                            cx,
                                        );
                                    }
                                    if let Some(paint) = paint {
                                        paint_terminal(bounds, &paint, window);
                                        if let Some(composition) = composition {
                                            paint_composition(
                                                bounds,
                                                paint.cursor,
                                                composition,
                                                &paint.typography,
                                                paint.theme.terminal(),
                                                window,
                                                cx,
                                            );
                                        }
                                    }
                                },
                            )
                            .size_full(),
                        ),
                )
            })
            .when(tree_active, |pane| pane.child(self.render_file_tree(cx)))
            .when_some(
                terminal_scroll.filter(|_| !tree_active),
                |pane, (position, length)| {
                    pane.child(
                        div()
                            .absolute()
                            .right(px(2.0))
                            .top(px(2.0 + position))
                            .w(px(2.0))
                            .h(px(length))
                            .rounded_sm()
                            .bg(color(colors.muted).opacity(0.32)),
                    )
                },
            )
    }

    /// A floating pane: title strip with explicit keyboard ownership and Dock,
    /// the shared pane body, and edge/corner resize handles.
    fn render_floating_pane(
        &self,
        index: usize,
        float: &FloatLayout,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let frame = float.frame;
        let pane_id = float.pane.pane_id.clone();
        let focused = self
            .focused_view()
            .is_some_and(|view| view.pane_id == pane_id);
        let (title, tab_label) = self.floating_caption(&pane_id);
        let body = layout::Rect {
            x: 0.0,
            y: FLOAT_TITLE_HEIGHT,
            width: frame.width,
            height: (frame.height - FLOAT_TITLE_HEIGHT).max(1.0),
        };
        let handle = |id: &'static str, mode: FloatDragMode, rect: layout::Rect| {
            let pane_id = pane_id.clone();
            div()
                .id((id, index))
                .absolute()
                .left(px(rect.x))
                .top(px(rect.y))
                .w(px(rect.width))
                .h(px(rect.height))
                .cursor(match mode {
                    FloatDragMode::Right => gpui::CursorStyle::ResizeLeftRight,
                    FloatDragMode::Bottom => gpui::CursorStyle::ResizeUpDown,
                    _ => gpui::CursorStyle::ResizeUpLeftDownRight,
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.start_float_drag(pane_id.clone(), mode, event.position);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
        };
        let raise_id = pane_id.clone();
        let move_id = pane_id.clone();
        let menu_id = pane_id.clone();
        div()
            .id(("floating-frame", index))
            .absolute()
            .left(px(frame.x))
            .top(px(frame.y))
            .w(px(frame.width))
            .h(px(frame.height))
            .occlude()
            // Occluding blocks the root's move handler while the pointer is over a
            // float; drags (move/resize, selections) must still see every event.
            .on_mouse_move(cx.listener(Self::on_workspace_mouse_move))
            .rounded_md()
            .shadow_lg()
            .bg(color(colors.surface))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.focus_pane(raise_id.clone());
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id(("floating-title", index))
                    .absolute()
                    .left(px(0.0))
                    .top(px(0.0))
                    .w(px(frame.width))
                    .h(px(FLOAT_TITLE_HEIGHT))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_t_md()
                    .border_b_1()
                    .border_color(color(colors.border))
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.focus_pane(move_id.clone());
                            this.start_float_drag(
                                move_id.clone(),
                                FloatDragMode::Move,
                                event.position,
                            );
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, cx| {
                            this.focus_pane(menu_id.clone());
                            this.open_overlay(Overlay::Palette, "");
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(color(if focused {
                                colors.foreground
                            } else {
                                colors.muted
                            }))
                            .child(match tab_label {
                                Some(tab) => format!("{title} · {tab}"),
                                None => title,
                            }),
                    )
                    .when(focused, |strip| {
                        // Keyboard ownership is explicit: only this pane receives typing.
                        strip.child(
                            div()
                                .flex_none()
                                .px_1()
                                .rounded_sm()
                                .text_color(color(colors.accent))
                                .child("Keyboard"),
                        )
                    })
                    .child(self.pane_command_button(
                        ("floating-dock", index),
                        "Dock",
                        Command::TogglePaneFloat,
                        &pane_id,
                        cx,
                    )),
            )
            .child(
                self.render_pane(("floating-pane", index), index, &float.pane, body, cx)
                    .rounded_b_md(),
            )
            .child(handle(
                "floating-resize-right",
                FloatDragMode::Right,
                layout::Rect {
                    x: frame.width - FLOAT_RESIZE_HANDLE,
                    y: FLOAT_TITLE_HEIGHT,
                    width: FLOAT_RESIZE_HANDLE,
                    height: body.height,
                },
            ))
            .child(handle(
                "floating-resize-bottom",
                FloatDragMode::Bottom,
                layout::Rect {
                    x: 0.0,
                    y: frame.height - FLOAT_RESIZE_HANDLE,
                    width: frame.width,
                    height: FLOAT_RESIZE_HANDLE,
                },
            ))
            .child(handle(
                "floating-resize-corner",
                FloatDragMode::Corner,
                layout::Rect {
                    x: frame.width - 2.0 * FLOAT_RESIZE_HANDLE,
                    y: frame.height - 2.0 * FLOAT_RESIZE_HANDLE,
                    width: 2.0 * FLOAT_RESIZE_HANDLE,
                    height: 2.0 * FLOAT_RESIZE_HANDLE,
                },
            ))
            .child(
                // Drawn last so the focus outline is never covered; it has no hitbox.
                div()
                    .absolute()
                    .inset_0()
                    .rounded_md()
                    .border_1()
                    .border_color(color(if focused {
                        colors.accent
                    } else {
                        colors.border
                    })),
            )
            .into_any_element()
    }

    /// The floating pane's title, plus its tab's label when that tab is not selected.
    fn floating_caption(&self, pane: &PaneId) -> (String, Option<String>) {
        let Some(tab) = self.workspace.as_ref().and_then(|workspace| {
            workspace
                .sessions
                .iter()
                .flat_map(|session| &session.tabs)
                .find(|tab| layout_contains_pane(&tab.layout, pane))
        }) else {
            return ("Terminal".into(), None);
        };
        let title = self
            .tab_panes(tab)
            .into_iter()
            .find(|item| &item.pane_id == pane)
            .map_or_else(|| "Terminal".into(), |item| item.title);
        let selected = self
            .selected_tab()
            .is_some_and(|selected| selected.id == tab.id);
        // Unnamed tabs have no useful label; the pane title already identifies them.
        let label = tab.label.trim();
        (
            title,
            (!selected && !label.is_empty()).then(|| label.to_owned()),
        )
    }

    /// The selected tab's panes are all floating: keep the tab, explain where
    /// its terminals are, and offer Dock rather than an empty-workspace state.
    fn render_floated_tab_placeholder(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let tab = self.selected_tab()?;
        let mut leaves = Vec::new();
        collect_leaves(&tab.layout, &mut leaves);
        if !leaves.iter().any(|(pane, _)| self.state.is_floating(pane)) {
            return None;
        }
        let colors = *self.colors();
        Some(
            div()
                .flex_1()
                .size_full()
                .min_w_0()
                .p_4()
                .flex()
                .flex_col()
                // Top-left stays clear of floats, which open on the right by default.
                .items_start()
                .gap_3()
                .bg(material_color(
                    colors.background,
                    self.effective_background_opacity(),
                ))
                .child(
                    div()
                        .text_color(color(colors.muted))
                        .child("This tab's terminal is floating. Its process keeps running."),
                )
                .children(
                    leaves
                        .iter()
                        .filter(|(pane, _)| self.state.is_floating(pane))
                        .enumerate()
                        .map(|(index, (pane, _))| {
                            self.pane_command_button(
                                ("dock-floated-tab", index),
                                "Dock pane",
                                Command::TogglePaneFloat,
                                pane,
                                cx,
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    fn start_float_drag(&mut self, pane_id: PaneId, mode: FloatDragMode, origin: Point<Pixels>) {
        let Some(float) = self
            .float_layouts
            .iter()
            .find(|float| float.pane.pane_id == pane_id)
        else {
            return;
        };
        let Some(before) = self
            .state
            .floating
            .iter()
            .find(|float| float.pane_id == pane_id)
            .map(|float| float.rect)
        else {
            return;
        };
        self.float_drag = Some(FloatDrag {
            pane_id,
            mode,
            origin,
            start: float.frame,
            before,
        });
    }

    /// Preview a move/resize locally; the next render's `rebuild_layout` coalesces
    /// the PTY resize, and the placement is saved on release.
    fn update_float_drag(&mut self, position: Point<Pixels>) {
        let Some(drag) = &self.float_drag else {
            return;
        };
        let dx = f32::from(position.x - drag.origin.x);
        let dy = f32::from(position.y - drag.origin.y);
        let area = self.float_area;
        let leaf = self.metrics().leaf_minimum();
        let mut frame = drag.start;
        if drag.mode == FloatDragMode::Move {
            // Moving never resizes the terminal; the frame stays inside the area.
            frame.x = (frame.x + dx).clamp(0.0, (area.width - frame.width).max(0.0));
            frame.y = (frame.y + dy).clamp(0.0, (area.height - frame.height).max(0.0));
        } else {
            if matches!(drag.mode, FloatDragMode::Right | FloatDragMode::Corner) {
                frame.width = (frame.width + dx).max(leaf.width).min(area.width - frame.x);
            }
            if matches!(drag.mode, FloatDragMode::Bottom | FloatDragMode::Corner) {
                frame.height = (frame.height + dy)
                    .max(leaf.height + FLOAT_TITLE_HEIGHT)
                    .min(area.height - frame.y);
            }
        }
        let fraction = layout::float_fraction(frame, self.float_area);
        let pane_id = drag.pane_id.clone();
        self.state.set_float_rect(
            &pane_id,
            FloatRect {
                x: fraction.x,
                y: fraction.y,
                width: fraction.width,
                height: fraction.height,
            },
        );
    }

    fn render_workspace_scrollbar(&self, horizontal: bool, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let layout = self
            .visible_layout()
            .expect("scrollbars render only with a visible layout");
        let offset = self.workspace_scroll.offset();
        let (viewport, canvas, scroll) = if horizontal {
            (
                layout.viewport.width,
                layout.canvas.width,
                -f32::from(offset.x),
            )
        } else {
            (
                layout.viewport.height,
                layout.canvas.height,
                -f32::from(offset.y),
            )
        };
        let length = (viewport * viewport / canvas).max(20.0);
        let position = (viewport - length) * scroll / (canvas - viewport).max(1.0);
        div()
            .id(if horizontal {
                "workspace-horizontal-scrollbar"
            } else {
                "workspace-vertical-scrollbar"
            })
            .absolute()
            .bg(color(colors.muted).opacity(0.04))
            .when(horizontal, |bar| {
                bar.left_0().bottom_0().w(px(viewport)).h(px(8.0))
            })
            .when(!horizontal, |bar| {
                bar.top_0().right_0().h(px(viewport)).w(px(8.0))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.workspace_scroll_drag = Some(horizontal);
                    this.scroll_workspace_to(event.position, horizontal);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .absolute()
                    .rounded_sm()
                    .bg(color(colors.muted).opacity(0.38))
                    .when(horizontal, |thumb| {
                        thumb
                            .left(px(position))
                            .top(px(3.0))
                            .w(px(length))
                            .h(px(2.0))
                    })
                    .when(!horizontal, |thumb| {
                        thumb
                            .top(px(position))
                            .left(px(3.0))
                            .h(px(length))
                            .w(px(2.0))
                    }),
            )
            .into_any_element()
    }
}

impl CompiApp {
    fn scroll_workspace_to(&mut self, position: Point<Pixels>, horizontal: bool) {
        let Some(layout) = self.visible_layout() else {
            return;
        };
        let old = self.workspace_scroll.offset();
        let x = f32::from(position.x) - self.sidebar_extent();
        let y = f32::from(position.y) - CHROME_HEIGHT;
        let offset = if horizontal {
            point(
                px(-(x / layout.viewport.width).clamp(0.0, 1.0)
                    * (layout.canvas.width - layout.viewport.width).max(0.0)),
                old.y,
            )
        } else {
            point(
                old.x,
                px(-(y / layout.viewport.height).clamp(0.0, 1.0)
                    * (layout.canvas.height - layout.viewport.height).max(0.0)),
            )
        };
        self.workspace_scroll.set_offset(offset);
    }

    fn on_workspace_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            return;
        }
        if let Some(pane_id) = self
            .surface_views
            .iter()
            .find(|view| view.selecting || view.mouse_reporting_down)
            .map(|view| view.pane_id.clone())
            && self
                .grid_point_for_pane(event.position, &pane_id, false)
                .is_none()
        {
            self.on_terminal_mouse_move(event, &pane_id, window, cx);
            return;
        }
        if self.float_drag.is_some() {
            self.update_float_drag(event.position);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if let Some(horizontal) = self.workspace_scroll_drag {
            self.scroll_workspace_to(event.position, horizontal);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.sidebar_drag {
            self.sidebar_width = f32::from(event.position.x).clamp(
                crate::config::MIN_SIDEBAR_WIDTH,
                crate::config::MAX_SIDEBAR_WIDTH,
            );
            self.rebuild_layout(window, false);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if let Some(drag) = &self.divider_drag {
            if let Some(layout) = &self.layout {
                let divider = &layout.dividers[drag.index];
                let offset = self.workspace_scroll.offset();
                let position = layout::Point {
                    x: f32::from(event.position.x - offset.x) - self.sidebar_extent(),
                    y: f32::from(event.position.y - offset.y) - CHROME_HEIGHT,
                };
                let ratio = divider.ratio_at(position);
                let mut tree = drag.tree.clone();
                set_ratio(&mut tree, &divider.path, ratio);
                self.preview_layout = Some(tree);
                self.rebuild_layout(window, false);
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if let Some(origin) = self.tab_drag_origin
            && drag_threshold_crossed(origin, event.position)
        {
            if self.titlebar_drag {
                self.tab_drag_origin = None;
                self.titlebar_drag = false;
                start_native_window_move(window);
            } else if self.dragging_tab.is_some() {
                self.drag_position = Some(event.position);
            }
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn on_workspace_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(origin) = self.opacity_drag_origin.take() {
            let opacity = self.terminal_opacity;
            self.terminal_opacity = origin;
            let mut next = self.scoped_appearance();
            next.terminal_opacity = opacity;
            self.apply_scoped_appearance(next, window, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if let Some(pane_id) = self
            .surface_views
            .iter()
            .find(|view| view.selecting || view.mouse_reporting_down)
            .map(|view| view.pane_id.clone())
        {
            self.on_terminal_mouse_up(event, &pane_id, window, cx);
            cx.notify();
            return;
        }
        if self.float_drag.take().is_some() {
            self.save_state();
            self.rebuild_layout(window, true);
            cx.notify();
            return;
        }
        if self.workspace_scroll_drag.take().is_some() {
            cx.notify();
            return;
        }
        if self.sidebar_drag {
            self.sidebar_drag = false;
            self.state.sidebar_width = self.sidebar_width;
            self.save_state();
            self.rebuild_layout(window, true);
        }
        if let Some(drag) = self.divider_drag.take() {
            if self.preview_layout.is_none() {
                cx.notify();
                return;
            }
            if self
                .workspace
                .as_ref()
                .is_some_and(|workspace| workspace.revision == drag.revision)
            {
                if let (Some(layout), Some(tab)) = (&self.layout, self.selected_tab()) {
                    let divider = &layout.dividers[drag.index];
                    let operation = WorkspaceMutation::SetSplitRatio {
                        tab_id: tab.id.clone(),
                        path: divider.path.clone(),
                        ratio: divider.effective_ratio,
                    };
                    self.mutate(operation, false);
                }
            } else {
                self.preview_layout = None;
                self.global_error =
                    Some("The divider changed in another window; preview cancelled".into());
            }
            self.rebuild_layout(window, true);
            cx.notify();
            return;
        }
        let moved = self.drag_position.take().is_some();
        self.tab_drag_origin = None;
        self.titlebar_drag = false;
        if let Some(tab_id) = self.dragging_tab.take()
            && moved
        {
            let (viewport_width, viewport_height) = logical_viewport_dimensions(window);
            let pointer_x = f32::from(event.position.x);
            let pointer_y = f32::from(event.position.y);
            let inside = pointer_x >= 0.0
                && pointer_y >= 0.0
                && pointer_x < viewport_width
                && pointer_y < viewport_height;
            if inside {
                let header = header_metrics(viewport_width);
                let tab_region_right = viewport_width
                    - WINDOW_CONTROLS_WIDTH
                    - HEADER_BUTTON_SLOT_WIDTH
                    - header.pane_actions_width;
                if pointer_y <= CHROME_HEIGHT
                    && pointer_x >= TITLEBAR_BRAND_WIDTH
                    && pointer_x < tab_region_right
                    && let Some(session_id) = self.state.selected_session.clone()
                {
                    let x = pointer_x
                        - TITLEBAR_BRAND_WIDTH
                        - f32::from(self.tab_scroll_handle.offset().x);
                    let visible_index = (x / header.tab_width).floor().max(0.0) as usize;
                    let session = self.workspace.as_ref().and_then(|workspace| {
                        workspace
                            .sessions
                            .iter()
                            .find(|session| session.id == session_id)
                    });
                    let target = session.and_then(|session| {
                        self.state
                            .visible_tabs(session)
                            .nth(visible_index)
                            .map(|tab| &tab.id)
                    });
                    let index = session
                        .map(|session| {
                            target
                                .and_then(|target| {
                                    session.tabs.iter().position(|tab| &tab.id == target)
                                })
                                .unwrap_or(session.tabs.len().saturating_sub(1))
                        })
                        .unwrap_or(0);
                    self.mutate(
                        WorkspaceMutation::MoveTab {
                            session_id,
                            tab_id,
                            index,
                        },
                        false,
                    );
                }
            } else {
                let global = window.bounds().origin + event.position;
                let source = Window::window_handle(window);
                let mut destination = None;
                for handle in cx.windows() {
                    if handle == source {
                        continue;
                    }
                    if let Some(typed) = handle.downcast::<CompiApp>() {
                        let matches = typed
                            .update(cx, |other, target, _| {
                                other.target == self.target && target.bounds().contains(&global)
                            })
                            .unwrap_or(false);
                        if matches {
                            destination = Some(handle);
                            break;
                        }
                    }
                }
                self.transfer_tab(tab_id, destination, window, cx);
            }
        }
        cx.notify();
    }

    fn transfer_tab(
        &mut self,
        tab_id: TabId,
        destination: Option<AnyWindowHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.capture_viewports();
        if self.mutation_pending || self.transferred_seed.is_some() {
            self.global_error =
                Some("Wait for the pending workspace change before transferring".into());
            return;
        }
        let Some(workspace) = self.workspace.clone() else {
            return;
        };
        let Some(tab) = workspace
            .sessions
            .iter()
            .flat_map(|session| &session.tabs)
            .find(|tab| tab.id == tab_id)
        else {
            self.global_error = Some("The terminal tab no longer exists".into());
            return;
        };
        let mut leaves = Vec::new();
        collect_leaves(&tab.layout, &mut leaves);
        let surfaces: HashSet<_> = leaves.iter().map(|(_, id)| id.clone()).collect();
        // Resolve creation and all fallible preconditions before releasing a source view.
        let is_new = destination.is_none();
        let target = if let Some(destination) = destination {
            let Some(target) = destination.downcast::<CompiApp>() else {
                self.global_error = Some("Destination is not a Compi window".into());
                return;
            };
            target
        } else {
            let mut config = self.config.clone();
            config.font = self.font_settings.clone();
            let display_appearance = self.accepted_appearance();
            let seed = TransferSeed {
                tab_id: tab_id.clone(),
                source_state: self.state.clone(),
                display_appearance,
                display_sidebar_width: self.sidebar_width,
                display_zoom: self.zoom,
            };
            match open_compi_window(self.target.clone(), None, config, Some(seed), cx) {
                Ok(target) => target,
                Err(error) => {
                    self.global_error = Some(format!(
                        "Could not create destination window; terminal stayed here: {error}"
                    ));
                    return;
                }
            }
        };
        if target.window_id() == Window::window_handle(window).window_id() {
            return;
        }
        let ready = target.update(cx, |other, _, _| {
            other.target == self.target
                && !other.mutation_pending
                && other.workspace.as_ref().is_some_and(|snapshot| {
                    snapshot.server_id == workspace.server_id
                        && snapshot.server_generation == workspace.server_generation
                })
        });
        if !matches!(ready, Ok(true)) {
            self.global_error =
                Some("Destination changed or is busy; terminal stayed in its source window".into());
            if is_new {
                let _ = target.update(cx, |_, window, _| window.remove_window());
            }
            return;
        }
        self.report_focus(false);
        let mut transferred = Some(
            self.surface_views
                .extract_if(.., |view| surfaces.contains(&view.surface_id))
                .collect::<Vec<_>>(),
        );
        let expected_count = leaves.len();
        if transferred
            .as_ref()
            .is_none_or(|views| views.len() != expected_count)
        {
            self.surface_views
                .extend(transferred.take().unwrap_or_default());
            self.global_error =
                Some("Not every pane is available for transfer; source view retained".into());
            if is_new {
                let _ = target.update(cx, |_, window, _| window.remove_window());
            }
            return;
        }
        let focus = self.state.focused_panes.get(tab_id.as_str()).cloned();
        let result = target.update(cx, |other, target_window, target_cx| {
            // Moving existing workers transfers their single controller; no attach, restart,
            // resize or input can interleave across this synchronous UI-thread boundary.
            other.capture_viewports();
            for view in &mut other.surface_views {
                view.stop.store(true, Ordering::Release);
                if let Some(transport) = view.transport.take() {
                    transport.close();
                }
                if let Ok(mut routes) = EVENT_ROUTES.lock() {
                    routes.remove(&view.id);
                }
                view.discard_replica();
            }
            other
                .surface_views
                .retain(|view| !surfaces.contains(&view.surface_id));
            let views = transferred.take().expect("transfer bundle owned by source");
            for view in &views {
                if let Ok(mut cache) = view.row_render_cache.lock() {
                    cache.clear();
                }
            }
            if let Ok(mut routes) = EVENT_ROUTES.lock() {
                for view in &views {
                    routes.insert(view.id, other.event_tx.0.clone());
                }
            }
            other.surface_views.extend(views);
            other.workspace = Some(workspace.clone());
            other.state.restore_tab(&workspace, &tab_id);
            other.pane_zoom.clear(&tab_id);
            other.zoom_layout = None;
            if let Some(focus) = &focus {
                other.state.focus_pane(&workspace, focus);
            }
            other.focused_view = other.state.focused_pane(&workspace).and_then(|pane| {
                other
                    .surface_views
                    .iter()
                    .find(|view| &view.pane_id == pane)
                    .map(|view| view.id)
            });
            other.transferred_seed = None;
            other.save_state();
            other.rebuild_layout(target_window, true);
            target_window.focus(&other.focus_handle);
            other.report_focus(true);
            target_cx.notify();
        });
        if let Err(error) = result {
            self.surface_views
                .extend(transferred.take().unwrap_or_default());
            for view in &self.surface_views {
                if let Ok(mut routes) = EVENT_ROUTES.lock() {
                    routes.insert(view.id, self.event_tx.0.clone());
                }
            }
            self.global_error = Some(format!(
                "Transfer failed; source ownership restored: {error}"
            ));
            self.report_focus(true);
            if is_new {
                let _ = target.update(cx, |_, window, _| window.remove_window());
            }
        } else {
            self.pane_zoom.clear(&tab_id);
            self.zoom_layout = None;
            self.state.hide_tab(&workspace, &tab_id);
            self.sync_visible_views();
            self.save_state();
            self.rebuild_layout(window, true);
        }
    }
}

#[cfg(target_os = "macos")]
fn native_material_available() -> bool {
    unsafe {
        let workspace: *mut objc2::runtime::AnyObject =
            msg_send![class!(NSWorkspace), sharedWorkspace];
        let reduced: bool = msg_send![workspace, accessibilityDisplayShouldReduceTransparency];
        !reduced
    }
}

#[cfg(windows)]
fn native_material_available() -> bool {
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            key: isize,
            subkey: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut core::ffi::c_void,
            size: *mut u32,
        ) -> i32;
    }
    let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
        .encode_utf16()
        .collect();
    let name: Vec<u16> = "EnableTransparency\0".encode_utf16().collect();
    let mut enabled = 0u32;
    let mut size = 4u32;
    // Query the user's accessibility/personalization preference. A missing or
    // unreadable setting chooses the readable opaque fallback.
    unsafe {
        RegGetValueW(
            0x8000_0001u32 as i32 as isize,
            subkey.as_ptr(),
            name.as_ptr(),
            0x10,
            std::ptr::null_mut(),
            (&mut enabled as *mut u32).cast(),
            &mut size,
        ) == 0
            && enabled != 0
    }
}

impl CompiApp {
    fn render_editor(&self, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let text = self.ime_text.clone();
        let placeholder = if matches!(self.overlay, Some(Overlay::ThemeCatalog)) {
            "Search themes by name or family…"
        } else {
            "Type to search or enter a name…"
        };
        let selection = self.ime_selected_range.clone();
        let marked = self.ime_marked_range.clone();
        let ui_font = self.ui_font.family();
        let input = cx.entity();
        let focus = self.focus_handle.clone();
        let editor_focused = !matches!(
            self.overlay,
            Some(Overlay::Text { .. } | Overlay::ThemeCatalog)
        ) || self.overlay_focus == 0;
        div()
            .relative()
            .mx_3()
            .mb_2()
            .px_3()
            .py_2()
            .h(px(44.0))
            .border_1()
            .border_color(color(if editor_focused {
                colors.accent
            } else {
                colors.border
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.overlay_focus = 0;
                    window.focus(&this.focus_handle);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(
                canvas(
                    move |_, _, _| (),
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, input.clone()),
                            cx,
                        );
                        let displayed: SharedString = if text.is_empty() {
                            placeholder.into()
                        } else {
                            text.clone().into()
                        };
                        let run = TextRun {
                            len: displayed.len(),
                            font: gpui::font(ui_font),
                            color: color(modal_text_color(
                                if text.is_empty() {
                                    colors.muted
                                } else {
                                    colors.foreground
                                },
                                &colors,
                            )),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        let line =
                            window
                                .text_system()
                                .shape_line(displayed, px(13.0), &[run], None);
                        window.with_content_mask(Some(ContentMask { bounds }), |window| {
                            let start = utf16_byte_index(&text, selection.start);
                            let end = utf16_byte_index(&text, selection.end);
                            let caret = if text.is_empty() {
                                px(0.0)
                            } else {
                                line.x_for_index(end)
                            };
                            let scroll = (caret - bounds.size.width + px(4.0)).max(px(0.0));
                            let origin = point(bounds.left() - scroll, bounds.top());
                            if start != end {
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(origin.x + line.x_for_index(start), origin.y),
                                        size(
                                            line.x_for_index(end) - line.x_for_index(start),
                                            px(20.0),
                                        ),
                                    ),
                                    color(colors.selection),
                                ));
                            }
                            let _ = line.paint(origin, px(20.0), window, cx);
                            if editor_focused {
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(origin.x + caret, origin.y),
                                        size(px(1.0), px(20.0)),
                                    ),
                                    color(colors.foreground),
                                ));
                            }
                            if editor_focused && let Some(marked) = marked {
                                let start = line.x_for_index(utf16_byte_index(&text, marked.start));
                                let end = line.x_for_index(utf16_byte_index(&text, marked.end));
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(origin.x + start, origin.y + px(19.0)),
                                        size(end - start, px(1.0)),
                                    ),
                                    color(colors.accent),
                                ));
                            }
                        });
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}

impl CompiApp {
    fn pane_command_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: &'static str,
        command: Command,
        pane: &PaneId,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let pane = pane.clone();
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_sm()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .text_color(color(colors.foreground))
            .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.focus_pane(pane.clone());
                this.execute(command, window, cx);
                cx.stop_propagation();
            }))
            .child(label)
            .into_any_element()
    }
}

impl CompiApp {
    fn capture_viewports(&mut self) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        for view in &mut self.surface_views {
            // A fresh replica must consume its saved anchor on the first
            // authoritative snapshot before an incidental window save clears it.
            if view.mirror.snapshot().is_none() {
                continue;
            }
            if view.scroll_offset == 0 && view.selection.is_none() {
                self.state.viewports.remove(view.surface_id.as_str());
                continue;
            }
            let Some(fingerprint) = view.viewport_fingerprint() else {
                continue;
            };
            let Some(snapshot) = view.mirror.snapshot() else {
                continue;
            };
            self.state.viewports.insert(
                view.surface_id.to_string(),
                SavedViewport {
                    identity: compi_protocol::TerminalIdentity {
                        server_id: workspace.server_id.clone(),
                        server_generation: workspace.server_generation.clone(),
                        surface_id: view.surface_id.clone(),
                        process_lifetime_id: view.lifetime.clone(),
                    },
                    cols: snapshot.cols,
                    rows: snapshot.rows,
                    scroll_offset: view.scroll_offset.min(snapshot.scrollback.len()),
                    selection: view.selection.map(|selection| {
                        [
                            [selection.anchor.row, selection.anchor.col],
                            [selection.head.row, selection.head.col],
                        ]
                    }),
                    fingerprint,
                    sequence: Some(snapshot.sequence),
                },
            );
        }
    }
}

struct FingerprintWriter(Sha256);

impl std::io::Write for FingerprintWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl SurfaceView {
    fn viewport_fingerprint(&mut self) -> Option<[u8; 32]> {
        let snapshot = self.mirror.snapshot()?;
        if let Some((sequence, fingerprint)) = self.viewport_fingerprint
            && sequence == snapshot.sequence
        {
            return Some(fingerprint);
        }
        let mut writer = FingerprintWriter(Sha256::new());
        serde_json::to_writer(
            &mut writer,
            &(
                snapshot.cols,
                snapshot.rows,
                snapshot.modes.alternate_screen,
                &snapshot.scrollback,
                &snapshot.cells,
            ),
        )
        .ok()?;
        let fingerprint = writer.0.finalize().into();
        self.viewport_fingerprint = Some((snapshot.sequence, fingerprint));
        Some(fingerprint)
    }

    fn restore_viewport(
        &mut self,
        saved: SavedViewport,
        identity: &compi_protocol::TerminalIdentity,
    ) {
        if &saved.identity != identity {
            return;
        }
        let Some(snapshot) = self.mirror.snapshot() else {
            return;
        };
        if saved.cols != snapshot.cols
            || saved.rows != snapshot.rows
            || saved.sequence != Some(snapshot.sequence)
            || saved.scroll_offset > snapshot.scrollback.len()
        {
            return;
        }
        if saved.selection.is_some_and(|points| {
            points.iter().any(|[row, col]| {
                let row = snapshot.scrollback.get(*row).or_else(|| {
                    row.checked_sub(snapshot.scrollback.len())
                        .and_then(|row| snapshot.cells.get(row))
                });
                *col >= usize::from(snapshot.cols) || row.is_none_or(|row| *col >= row.cells.len())
            })
        }) {
            return;
        }
        if self.viewport_fingerprint() != Some(saved.fingerprint) {
            return;
        }
        self.scroll_offset = saved.scroll_offset;
        self.selection = saved.selection.map(|[anchor, head]| Selection {
            anchor: GridPoint {
                row: anchor[0],
                col: anchor[1],
            },
            head: GridPoint {
                row: head[0],
                col: head[1],
            },
        });
        self.selecting = false;
    }
}

impl CompiApp {
    fn reap_retired_views(&mut self) {
        let floating: HashSet<_> = self
            .floating_leaves()
            .into_iter()
            .map(|(_, surface)| surface)
            .collect();
        let tab = self
            .workspace
            .as_ref()
            .and_then(|workspace| self.state.selected_tab(workspace));
        self.surface_views.retain(|view| {
            !view.closed.load(Ordering::Acquire)
                || tab.is_some_and(|tab| contains_surface(&tab.layout, &view.surface_id))
                || floating.contains(&view.surface_id)
        });
    }

    /// Present a pane above the window, reusing its attached view; never a new
    /// surface or a second attachment. The split tree on the server is unchanged.
    /// A pane from a tab that is not shown attaches here, as the same surface.
    fn float_pane(&mut self, pane: PaneId) {
        if self.workspace.is_none() {
            return;
        }
        self.report_focus(false);
        let floated = self
            .workspace
            .as_ref()
            .is_some_and(|workspace| self.state.float_pane(workspace, &pane));
        if !floated {
            self.report_focus(self.overlay.is_none());
            self.global_error = Some("This pane can no longer float".into());
            return;
        }
        self.divider_drag = None;
        self.preview_layout = None;
        self.sync_visible_views();
        self.report_focus(self.overlay.is_none());
        self.save_state();
    }

    /// Dock is presentation only: the pane returns to its current tile and its
    /// process keeps running. A view no longer visible releases its attachment.
    fn dock_pane(&mut self, pane: &PaneId) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        if !self.state.dock_pane(workspace, pane) {
            return;
        }
        if self
            .float_drag
            .as_ref()
            .is_some_and(|drag| &drag.pane_id == pane)
        {
            self.float_drag = None;
        }
        self.report_focus(false);
        self.sync_visible_views();
        self.report_focus(self.overlay.is_none());
        self.save_state();
    }
}

/// Identity of what menus and the palette target: server, hierarchy, labels, pane
/// leaves, and process lifetimes. Size, status, and split-ratio observations advance
/// the server revision but leave this unchanged, so they never stale an open menu.
fn structure_key(workspace: &WorkspaceSnapshot) -> u64 {
    fn layout(node: &LayoutNode, hasher: &mut DefaultHasher) {
        match node {
            LayoutNode::Pane {
                pane_id,
                surface_id,
            } => {
                0_u8.hash(hasher);
                pane_id.as_str().hash(hasher);
                surface_id.as_str().hash(hasher);
            }
            LayoutNode::Split {
                axis,
                first,
                second,
                ..
            } => {
                1_u8.hash(hasher);
                (*axis == SplitAxis::Horizontal).hash(hasher);
                layout(first, hasher);
                layout(second, hasher);
            }
        }
    }
    let mut hasher = DefaultHasher::new();
    workspace.server_id.as_str().hash(&mut hasher);
    workspace.server_generation.as_str().hash(&mut hasher);
    for session in &workspace.sessions {
        session.id.as_str().hash(&mut hasher);
        session.label.hash(&mut hasher);
        for tab in &session.tabs {
            tab.id.as_str().hash(&mut hasher);
            tab.label.hash(&mut hasher);
            layout(&tab.layout, &mut hasher);
        }
    }
    for surface in &workspace.surfaces {
        surface.id.as_str().hash(&mut hasher);
        surface.process_lifetime_id.as_str().hash(&mut hasher);
    }
    hasher.finish()
}

fn positive_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

fn leaf_count(tree: &LayoutNode) -> usize {
    match tree {
        LayoutNode::Pane { .. } => 1,
        LayoutNode::Split { first, second, .. } => leaf_count(first) + leaf_count(second),
    }
}

fn pane_surface<'a>(tree: &'a LayoutNode, pane: &PaneId) -> Option<&'a SurfaceId> {
    match tree {
        LayoutNode::Pane {
            pane_id,
            surface_id,
        } => (pane_id == pane).then_some(surface_id),
        LayoutNode::Split { first, second, .. } => {
            pane_surface(first, pane).or_else(|| pane_surface(second, pane))
        }
    }
}

fn layout_contains_pane(tree: &LayoutNode, pane: &PaneId) -> bool {
    match tree {
        LayoutNode::Pane { pane_id, .. } => pane_id == pane,
        LayoutNode::Split { first, second, .. } => {
            layout_contains_pane(first, pane) || layout_contains_pane(second, pane)
        }
    }
}

fn contains_surface(tree: &LayoutNode, surface: &SurfaceId) -> bool {
    match tree {
        LayoutNode::Pane { surface_id, .. } => surface_id == surface,
        LayoutNode::Split { first, second, .. } => {
            contains_surface(first, surface) || contains_surface(second, surface)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppearanceField, AppearanceSettings, BackgroundEffect, LoadedConfig, ThemeId,
        WindowAppearanceOverrides, commit_appearance_overrides, inherited_appearance,
        material_opacity,
    };
    use super::{
        COMPACT_TAB_WIDTH, HEADER_BUTTON_SLOT_WIDTH, PANE_ACTIONS_COMPACT_WIDTH,
        PANE_ACTIONS_FULL_WIDTH, PaneActionsMode, PaneZoomState, TAB_WIDTH, TITLEBAR_BRAND_WIDTH,
        TabPane, WINDOW_CONTROLS_WIDTH, concise_path_title, concise_tab_title,
        display_index_for_bounds, header_metrics, opacity_at_slider_position, tab_caption,
    };
    use compi_protocol::{PaneId, TabId};
    use gpui::{Bounds, point, px, size};

    #[test]
    fn split_overrides_inherit_independently_and_preserve_missing_theme_ids() {
        let mut config = LoadedConfig::default();
        config.configured_appearance.theme = ThemeId::parse("compi-neutral").unwrap();
        config.configured_appearance.terminal_theme = ThemeId::parse("dracula").unwrap();
        config.configured_appearance.terminal_theme_override = true;
        let application = ThemeId::parse("user-unavailable").unwrap();
        let overrides = WindowAppearanceOverrides {
            theme: Some(application.clone()),
            terminal_opacity: Some(0.7),
            ..WindowAppearanceOverrides::default()
        };
        let inherited = inherited_appearance(&config, &overrides);
        assert_eq!(inherited.theme, application);
        assert_eq!(
            inherited.terminal_theme,
            config.configured_appearance.terminal_theme
        );
        assert_eq!(inherited.terminal_opacity, 0.7);
        assert!(inherited.transparent_background);
        assert_eq!(
            material_opacity(
                inherited.transparent_background,
                inherited.terminal_opacity,
                true
            ),
            0.7
        );
        config.configured_appearance.terminal_theme = ThemeId::parse("dark-glass").unwrap();
        let updated = inherited_appearance(&config, &overrides);
        assert_eq!(updated.theme, application);
        assert_eq!(
            updated.terminal_theme,
            config.configured_appearance.terminal_theme
        );
    }

    #[test]
    fn cli_theme_locks_both_palettes_without_locking_window_material() {
        let mut config = LoadedConfig::default();
        let cli_id = ThemeId::parse("compi-neutral").unwrap();
        config.provenance.theme = crate::config::ValueSource::CommandLine;
        config.appearance = AppearanceSettings {
            theme: cli_id.clone(),
            terminal_theme: cli_id.clone(),
            ..config.appearance
        };
        let overrides = WindowAppearanceOverrides {
            theme: Some(ThemeId::parse("user-app").unwrap()),
            terminal_theme: Some(ThemeId::parse("user-terminal").unwrap()),
            background_effect: Some(BackgroundEffect::Clear),
            terminal_opacity: Some(0.4),
            terminal_theme_override: Some(true),
            transparent_background: Some(false),
        };
        let inherited = inherited_appearance(&config, &overrides);
        assert_eq!(inherited.theme, cli_id);
        assert_eq!(inherited.terminal_theme, cli_id);
        assert_eq!(inherited.background_effect, BackgroundEffect::Clear);
        assert_eq!(inherited.terminal_opacity, 0.4);
        assert!(!inherited.terminal_theme_override);
        assert!(!inherited.transparent_background);
    }

    #[test]
    fn follow_theme_uses_current_application_palette_and_remembers_override_across_scopes() {
        let mut config = LoadedConfig::default();
        config.configured_appearance.theme = ThemeId::parse("nord").unwrap();
        config.configured_appearance.terminal_theme = ThemeId::parse("dracula").unwrap();
        config.configured_appearance.terminal_theme_override = true;
        let mut overrides = WindowAppearanceOverrides {
            theme: Some(ThemeId::parse("warm-carbon").unwrap()),
            ..WindowAppearanceOverrides::default()
        };
        let previous = inherited_appearance(&config, &overrides);
        let mut next = previous.clone();
        next.terminal_theme_override = false;
        commit_appearance_overrides(
            &mut overrides,
            &previous,
            &next,
            false,
            false,
            Some(AppearanceField::TerminalOverride),
        );
        let followed = inherited_appearance(&config, &overrides);
        assert_eq!(followed.effective_terminal_theme().id(), "warm-carbon");
        assert_eq!(followed.terminal_theme.id(), "dracula");
        config.configured_appearance.theme = ThemeId::parse("catppuccin-latte").unwrap();
        let followed = inherited_appearance(&config, &overrides);
        assert_eq!(followed.effective_terminal_theme().id(), "warm-carbon");
        next = followed.clone();
        next.terminal_theme_override = true;
        commit_appearance_overrides(
            &mut overrides,
            &followed,
            &next,
            false,
            false,
            Some(AppearanceField::TerminalOverride),
        );
        assert_eq!(
            inherited_appearance(&config, &overrides)
                .effective_terminal_theme()
                .id(),
            "dracula"
        );
    }

    #[test]
    fn transparency_toggle_preserves_material_preferences_and_unrelated_overrides() {
        let config = LoadedConfig::default();
        let mut overrides = WindowAppearanceOverrides {
            theme: Some(ThemeId::parse("nord").unwrap()),
            terminal_theme: Some(ThemeId::parse("dracula").unwrap()),
            terminal_theme_override: Some(true),
            terminal_opacity: Some(0.43),
            background_effect: Some(BackgroundEffect::Clear),
            transparent_background: Some(true),
        };
        let original = overrides.clone();
        for enabled in [false, true] {
            let previous = inherited_appearance(&config, &overrides);
            let mut next = previous.clone();
            next.transparent_background = enabled;
            commit_appearance_overrides(
                &mut overrides,
                &previous,
                &next,
                false,
                false,
                Some(AppearanceField::Transparency),
            );
            let effective = inherited_appearance(&config, &overrides);
            assert_eq!(effective.terminal_opacity, 0.43);
            assert_eq!(effective.background_effect, BackgroundEffect::Clear);
            assert_eq!(effective.effective_terminal_theme().id(), "dracula");
            assert_eq!(
                material_opacity(
                    effective.transparent_background,
                    effective.terminal_opacity,
                    true
                ),
                if enabled { 0.43 } else { 1.0 }
            );
            assert_eq!(
                material_opacity(
                    effective.transparent_background,
                    effective.terminal_opacity,
                    false
                ),
                1.0
            );
        }
        assert_eq!(overrides, original);
        // Global has the requested value already: only the explicitly edited
        // window flag is cleared, never its remembered opacity, blur or colors.
        commit_appearance_overrides(
            &mut overrides,
            &config.configured_appearance,
            &config.configured_appearance,
            true,
            false,
            Some(AppearanceField::Transparency),
        );
        let mut expected = original;
        expected.transparent_background = None;
        assert_eq!(overrides, expected);
    }

    #[test]
    fn locked_color_commits_still_allow_scoped_material_changes() {
        let previous = AppearanceSettings::default();
        let mut next = previous.clone();
        next.theme = ThemeId::parse("nord").unwrap();
        next.terminal_theme = ThemeId::parse("dracula").unwrap();
        next.terminal_theme_override = true;
        next.transparent_background = false;
        next.terminal_opacity = 0.43;
        let mut overrides = WindowAppearanceOverrides {
            theme: Some(ThemeId::parse("warm-carbon").unwrap()),
            ..WindowAppearanceOverrides::default()
        };
        let preserved = overrides.clone();
        commit_appearance_overrides(&mut overrides, &previous, &next, false, true, None);
        assert_eq!(overrides.theme, preserved.theme);
        assert_eq!(overrides.terminal_theme, preserved.terminal_theme);
        assert_eq!(
            overrides.terminal_theme_override,
            preserved.terminal_theme_override
        );
        assert_eq!(overrides.transparent_background, Some(false));
        assert_eq!(overrides.terminal_opacity, Some(0.43));
    }

    #[test]
    fn restored_window_selects_the_display_containing_its_saved_center() {
        let displays = [
            Bounds::new(point(px(0.0), px(0.0)), size(px(1706.0), px(928.0))),
            Bounds::new(point(px(-3440.0), px(0.0)), size(px(3440.0), px(1392.0))),
        ];
        let saved = Bounds::new(point(px(-3275.0), px(368.0)), size(px(953.0), px(636.0)));

        assert_eq!(display_index_for_bounds(saved, displays), Some(1));
        assert_eq!(
            display_index_for_bounds(
                Bounds::new(point(px(9000.0), px(9000.0)), size(px(953.0), px(636.0)),),
                displays,
            ),
            None
        );
    }

    #[test]
    fn opacity_slider_maps_and_clamps_its_track() {
        let bounds = Bounds {
            origin: point(px(10.0), px(0.0)),
            size: size(px(100.0), px(24.0)),
        };
        assert_eq!(opacity_at_slider_position(px(0.0), bounds), 0.1);
        assert!((opacity_at_slider_position(px(37.3), bounds) - 0.3457).abs() < 0.000001);
        assert_eq!(opacity_at_slider_position(px(120.0), bounds), 1.0);
    }

    #[test]
    fn terminal_tab_titles_keep_identity_without_exposing_full_paths() {
        assert_eq!(concise_tab_title("/home/user/projects/compi"), "compi");
        assert_eq!(concise_tab_title(r"C:\Users\user\projects\compi"), "compi");
        assert_eq!(concise_path_title("/"), "/");
        assert_eq!(
            concise_tab_title("user@host: ~/compi"),
            "user@host: ~/compi"
        );
    }

    #[test]
    fn split_tab_caption_shows_both_names_then_a_total_count() {
        let panes: Vec<_> = ["shell", "server", "logs", "tests"]
            .into_iter()
            .enumerate()
            .map(|(index, title)| TabPane {
                pane_id: PaneId::new(format!("pane-{index}")),
                title: title.into(),
                directory: None,
                floating: false,
            })
            .collect();
        assert_eq!(tab_caption("", &panes[..1]), ("shell".into(), None));
        assert_eq!(
            tab_caption("", &panes[..2]),
            ("shell".into(), Some("server".into()))
        );
        assert_eq!(
            tab_caption("", &panes[..3]),
            ("shell".into(), Some("3+".into()))
        );
        assert_eq!(tab_caption("", &panes), ("shell".into(), Some("4+".into())));
        assert_eq!(
            tab_caption("my project", &panes[..2]),
            ("my project".into(), Some("2".into()))
        );
    }

    #[test]
    fn pane_zoom_tracks_tabs_and_directional_focus_independently() {
        let tab_a = TabId::from("tab-a");
        let tab_b = TabId::from("tab-b");
        let pane_a = PaneId::from("pane-a");
        let pane_b = PaneId::from("pane-b");
        let pane_c = PaneId::from("pane-c");
        let mut zoom = PaneZoomState::default();

        assert!(zoom.toggle(tab_a.clone(), pane_a));
        assert!(zoom.toggle(tab_b.clone(), pane_b.clone()));
        zoom.retarget(&tab_a, pane_c.clone());

        assert_eq!(zoom.pane(&tab_a), Some(&pane_c));
        assert_eq!(zoom.pane(&tab_b), Some(&pane_b));
    }

    #[test]
    fn split_and_authoritative_invalidation_clear_zoom_safely() {
        let tab_a = TabId::from("tab-a");
        let tab_b = TabId::from("tab-b");
        let pane_a = PaneId::from("pane-a");
        let pane_b = PaneId::from("pane-b");
        let mut zoom = PaneZoomState::default();
        zoom.toggle(tab_a.clone(), pane_a);
        zoom.toggle(tab_b.clone(), pane_b.clone());

        assert!(zoom.clear(&tab_a));
        assert!(zoom.pane(&tab_a).is_none());
        assert_eq!(zoom.pane(&tab_b), Some(&pane_b));

        zoom.retain_valid(|tab, pane| tab != &tab_b || pane != &pane_b);
        assert!(zoom.pane(&tab_b).is_none());
    }

    #[test]
    fn narrow_header_collapses_before_active_tab_becomes_a_sliver() {
        let wide_threshold = TITLEBAR_BRAND_WIDTH
            + WINDOW_CONTROLS_WIDTH
            + HEADER_BUTTON_SLOT_WIDTH
            + PANE_ACTIONS_FULL_WIDTH
            + TAB_WIDTH;
        assert_eq!(
            header_metrics(wide_threshold).pane_actions,
            PaneActionsMode::Full
        );
        assert_eq!(
            header_metrics(wide_threshold - 1.0).pane_actions,
            PaneActionsMode::Compact
        );

        let minimum = header_metrics(420.0);
        assert_eq!(minimum.pane_actions, PaneActionsMode::Compact);
        assert!(minimum.tab_width >= COMPACT_TAB_WIDTH);
        let occupied = TITLEBAR_BRAND_WIDTH
            + WINDOW_CONTROLS_WIDTH
            + HEADER_BUTTON_SLOT_WIDTH
            + PANE_ACTIONS_COMPACT_WIDTH
            + minimum.tab_width;
        assert!(occupied <= 420.0);
    }
}
