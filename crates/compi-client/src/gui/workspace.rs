use super::*;
use gpui::{AnyElement, AnyWindowHandle, WindowBackgroundAppearance, WindowHandle};
use sha2::{Digest, Sha256};
pub(super) mod catalog;
pub(in crate::gui) mod dialogs;
pub(super) mod media;
pub(in crate::gui) mod performance;
pub(in crate::gui) mod settings;

#[derive(Clone)]
pub(super) struct TransferSeed {
    tab_id: TabId,
    source_state: ClientState,
    display_theme: ThemePreset,
    display_terminal_opacity: f32,
    display_background_effect: BackgroundEffect,
    display_sidebar_width: f32,
    display_zoom: f32,
}

#[derive(Clone)]
pub(super) enum TextPurpose {
    CreateWorkspace,
    RenameWorkspace(SessionId),
    RenameTab(TabId),
}

#[derive(Clone)]
pub(super) enum Overlay {
    Palette,
    PaneActions,
    TabActions {
        position: Point<Pixels>,
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
    },
    Tabs {
        hidden_only: bool,
    },
    Windows,
    Diagnostics,
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

pub(super) fn open_compi_window(
    target: ConnectionTarget,
    initial_working_directory: Option<String>,
    config: LoadedConfig,
    transferred_seed: Option<TransferSeed>,
    cx: &mut App,
) -> crate::Result<WindowHandle<CompiApp>> {
    let started_at = Instant::now();
    let mut client = target.connect()?;
    let mut snapshot = client.workspace()?;
    let target_sessions = perf::target_session_count();
    while snapshot.surfaces.len() < target_sessions {
        client.create_surface(DEFAULT_COLS, DEFAULT_ROWS, None)?;
        snapshot = client.workspace()?;
    }
    let defaults = ClientState {
        sidebar_width: config.configured_sidebar_width,
        ..ClientState::default()
    };
    let state_instance = target.state_instance();
    let mut slot = StateSlot::claim(state_instance.as_deref(), &snapshot.server_id, &defaults)?;
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (tx, rx) = async_channel::bounded(128);
        let state = slot.state.clone();
        let theme = transferred_seed
            .as_ref()
            .map(|seed| seed.display_theme)
            .unwrap_or_else(|| {
                if config.provenance.theme == crate::config::ValueSource::CommandLine {
                    config.appearance.theme
                } else {
                    state
                        .appearance
                        .theme
                        .unwrap_or(config.configured_appearance.theme)
                }
            });
        let ui_font = crate::font_catalog::resolve_ui_font(config.ui_font, window.text_system());
        let terminal_opacity = transferred_seed
            .as_ref()
            .map(|seed| seed.display_terminal_opacity)
            .unwrap_or_else(|| {
                state
                    .appearance
                    .terminal_opacity
                    .unwrap_or(config.configured_appearance.terminal_opacity)
            });
        let background_effect = transferred_seed
            .as_ref()
            .map(|seed| seed.display_background_effect)
            .unwrap_or_else(|| {
                state
                    .appearance
                    .background_effect
                    .unwrap_or(config.configured_appearance.background_effect)
            });
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
        let global_warning = diagnostic_warning(&config.diagnostics, &typography.diagnostics);
        let performance_enabled = Arc::new(AtomicBool::new(state.show_fps));
        let mut this = Self {
            started_at,
            target,
            initial_working_directory,
            first_snapshot_logged: false,
            ready_probe_marker: perf::ready_probe_enabled()
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
            focused_view: None,
            tab_scroll_handle: ScrollHandle::new(),
            workspace_scroll: ScrollHandle::new(),
            sidebar_scroll: ScrollHandle::new(),
            sidebar_open: false,
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
            performance_enabled: performance_enabled.clone(),
            performance: performance::PerformanceMonitor::default(),
            performance_notice: None,
            overlay_revision: None,
            pane_zoom: PaneZoomState::default(),
            layout: None,
            divider_drag: None,
            preview_layout: None,
            last_resize: Instant::now() - Duration::from_secs(1),
            loading_surfaces: false,
            zoom_layout: None,
            mutation_pending: false,
            global_error: None,
            connection_error: None,
            font_settings: config.font.clone(),
            typography,
            typography_scale: window.scale_factor(),
            config_diagnostics: config.diagnostics.clone(),
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
        let alive = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let mut appearance_stamp = fs::metadata(&appearance_path)
                .ok()
                .and_then(|metadata| Some((metadata.modified().ok()?, metadata.len())));
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                if alive.upgrade().is_none() {
                    break;
                }
                let sender = sender.clone();
                let target = target.clone();
                let appearance_path = appearance_path.clone();
                let previous_stamp = appearance_stamp;
                let collect_performance = performance_enabled.load(Ordering::Acquire);
                appearance_stamp = cx
                    .background_executor()
                    .spawn(async move {
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
                        stamp
                    })
                    .await;
            }
        })
        .detach();
        if !this
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
        } else if let Some(cwd) = this.initial_working_directory.take() {
            this.create_terminal(Some(cwd));
        }
        this
    }

    fn colors(&self) -> &'static ThemeColors {
        self.theme.colors()
    }

    fn apply_window_background(&mut self, window: &mut Window) {
        let translucent = self.terminal_opacity < 1.0 && native_material_available();
        let appearance = if !translucent {
            WindowBackgroundAppearance::Opaque
        } else {
            match self.background_effect {
                BackgroundEffect::Clear => WindowBackgroundAppearance::Transparent,
                BackgroundEffect::Blurred => WindowBackgroundAppearance::Blurred,
            }
        };
        self.glass = translucent;
        window.set_background_appearance(appearance);
    }

    fn preview_terminal_opacity(&mut self, opacity: f32, window: &mut Window) {
        let was_translucent = self.terminal_opacity < 1.0;
        self.opacity_drag_origin
            .get_or_insert(self.terminal_opacity);
        self.terminal_opacity = opacity.clamp(
            crate::config::MIN_TERMINAL_OPACITY,
            crate::config::MAX_TERMINAL_OPACITY,
        );
        if was_translucent != (self.terminal_opacity < 1.0) {
            self.apply_window_background(window);
        }
    }

    fn appearance(&self) -> AppearanceSettings {
        AppearanceSettings {
            theme: self.theme,
            terminal_opacity: self.terminal_opacity,
            background_effect: self.background_effect,
        }
    }

    fn scoped_appearance(&self) -> AppearanceSettings {
        match self.settings_scope {
            SettingsScope::Global => self.config.configured_appearance,
            SettingsScope::Window => self.appearance(),
        }
    }

    fn apply_scoped_appearance(
        &mut self,
        appearance: AppearanceSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = self.appearance();
        match self.settings_scope {
            SettingsScope::Global => {
                if let Err(error) = self.config.save_global_appearance(appearance) {
                    self.global_error = Some(error);
                    return;
                }
                self.state.appearance = WindowAppearanceOverrides::default();
                self.set_theme(self.config.appearance.theme);
                self.terminal_opacity = appearance.terminal_opacity;
                self.background_effect = appearance.background_effect;
            }
            SettingsScope::Window => {
                if self.config.provenance.theme != crate::config::ValueSource::CommandLine
                    && appearance.theme != previous.theme
                {
                    self.state.appearance.theme = Some(appearance.theme);
                }
                if appearance.terminal_opacity != previous.terminal_opacity {
                    self.state.appearance.terminal_opacity = Some(appearance.terminal_opacity);
                }
                if appearance.background_effect != previous.background_effect {
                    self.state.appearance.background_effect = Some(appearance.background_effect);
                }
                if self.config.provenance.theme != crate::config::ValueSource::CommandLine {
                    self.set_theme(appearance.theme);
                }
                self.terminal_opacity = appearance.terminal_opacity;
                self.background_effect = appearance.background_effect;
            }
        }
        self.apply_window_background(window);
        self.save_state();
        if self.settings_scope == SettingsScope::Global {
            self.broadcast_global_appearance(window, cx);
        }
    }

    fn reset_window_appearance(&mut self, window: &mut Window) {
        self.state.appearance = WindowAppearanceOverrides::default();
        self.set_theme(self.config.appearance.theme);
        self.terminal_opacity = self.config.configured_appearance.terminal_opacity;
        self.background_effect = self.config.configured_appearance.background_effect;
        self.apply_window_background(window);
        self.save_state();
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
        if self.loading_surfaces {
            return;
        }
        self.loading_surfaces = true;
        let sender = self.event_tx.clone();
        let target = self.target.clone();
        thread::spawn(move || {
            let result = target
                .connect()
                .and_then(|mut client| client.workspace())
                .map_err(|error| error.to_string());
            sender.send(UiEvent::SurfacesLoaded(result));
        });
    }

    fn begin_daemon_restart(&mut self) {
        if self.daemon_restarting {
            return;
        }
        self.daemon_restarting = true;
        self.loading_surfaces = true;
        self.global_error = None;
        for view in &mut self.surface_views {
            view.stop.store(true, Ordering::Release);
            if let Some(transport) = view.transport.take() {
                transport.close();
            }
        }
        let sender = self.event_tx.clone();
        let target = self.target.clone();
        thread::spawn(move || {
            let result = target
                .restart_daemon()
                .and_then(|mut client| client.workspace())
                .map_err(|error| error.to_string());
            sender.send(UiEvent::DaemonRestarted(result));
        });
    }

    fn live_surface_details(&self) -> (usize, String) {
        let surfaces = self
            .workspace
            .iter()
            .flat_map(|workspace| &workspace.surfaces)
            .filter(|surface| {
                matches!(
                    surface.status,
                    SurfaceStatus::Starting | SurfaceStatus::Running | SurfaceStatus::Ending
                )
            })
            .collect::<Vec<_>>();
        let details = surfaces
            .iter()
            .map(|surface| format!("• {}", surface.id))
            .collect::<Vec<_>>()
            .join("\n");
        (surfaces.len(), details)
    }

    fn mutate(&mut self, operation: WorkspaceMutation, select_created: bool) {
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
            UiEvent::StateSaveFinished => {}
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
                if let Some(view) = self.surface_view_mut(tab_id) {
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
        self.capture_viewports();
        let mut leaves = Vec::new();
        if let Some(tab) = self.selected_tab() {
            collect_leaves(&tab.layout, &mut leaves);
        }
        let wanted: HashSet<_> = leaves.iter().map(|(_, surface)| surface.clone()).collect();
        for view in &mut self.surface_views {
            if !wanted.contains(&view.surface_id) {
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
        let Some(workspace) = &self.workspace else {
            return;
        };
        self.surface_views.retain(|view| {
            workspace.surface(&view.surface_id).is_some()
                && (wanted.contains(&view.surface_id) || !view.closed.load(Ordering::Acquire))
        });
        for (pane_id, surface_id) in leaves {
            let Some(surface) = workspace.surface(&surface_id) else {
                continue;
            };
            let dimensions = self
                .layout
                .as_ref()
                .and_then(|layout| layout.pane(&pane_id))
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
        let tab_id = self.selected_tab().map(|tab| tab.id.clone());
        self.report_focus(false);
        if let Some(workspace) = &self.workspace {
            self.state.focus_pane(workspace, &pane);
        }
        if let Some(tab_id) = &tab_id {
            self.pane_zoom.retarget(tab_id, pane.clone());
        }
        self.focused_view = self
            .surface_views
            .iter()
            .find(|view| view.pane_id == pane)
            .map(|view| view.id);
        self.report_focus(self.overlay.is_none());
        if self.pane_zoomed() {
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

    fn metrics(&self) -> LayoutMetrics {
        LayoutMetrics {
            cell_width: self.typography.cell_width,
            line_height: self.typography.cell_height,
            padding_x: TERMINAL_PADDING,
            padding_y: TERMINAL_PADDING,
            pane_chrome_height: 0.0,
            divider_thickness: 5.0,
            scale_factor: self.typography_scale,
        }
    }

    fn rebuild_layout(&mut self, window: &Window, force: bool) -> bool {
        let Some(tab_id) = self.selected_tab().map(|tab| tab.id.clone()) else {
            self.layout = None;
            self.zoom_layout = None;
            return false;
        };
        let (viewport_width, viewport_height) = logical_viewport_dimensions(window);
        let metrics = self.metrics();
        let available = layout::Size {
            width: (viewport_width
                - if self.sidebar_open {
                    self.sidebar_width + 5.0
                } else {
                    0.0
                })
            .max(1.0),
            height: (viewport_height - CHROME_HEIGHT).max(1.0),
        };
        let mut layout = {
            let tab = self
                .selected_tab()
                .expect("the selected tab was resolved above");
            let tree = self.preview_layout.as_ref().unwrap_or(&tab.layout);
            layout::compute_layout(tree, available, metrics)
        };
        if layout.has_overflow() {
            let tab = self
                .selected_tab()
                .expect("the selected tab was resolved above");
            let tree = self.preview_layout.as_ref().unwrap_or(&tab.layout);
            layout = layout::compute_layout(
                tree,
                layout::Size {
                    width: (available.width - 8.0).max(1.0),
                    height: (available.height - 8.0).max(1.0),
                },
                metrics,
            );
        }
        let zoom_tree = self
            .pane_zoom
            .pane(&tab_id)
            .and_then(|pane_id| layout.pane(pane_id))
            .map(|pane| LayoutNode::Pane {
                pane_id: pane.pane_id.clone(),
                surface_id: pane.surface_id.clone(),
            });
        if self.pane_zoom.pane(&tab_id).is_some() && zoom_tree.is_none() {
            self.pane_zoom.clear(&tab_id);
        }
        let zoom_layout = zoom_tree
            .as_ref()
            .map(|tree| layout::compute_layout(tree, available, metrics));
        let mut needs_frame = false;
        if force || self.last_resize.elapsed() >= Duration::from_millis(32) {
            for pane in &layout.panes {
                let geometry = zoom_layout
                    .as_ref()
                    .and_then(|zoom| zoom.pane(&pane.pane_id))
                    .unwrap_or(pane);
                let (cols, rows) = geometry.grid_size(metrics);
                if let Some(view) = self
                    .surface_views
                    .iter_mut()
                    .find(|view| view.pane_id == pane.pane_id)
                    && (view.cols, view.rows) != (cols, rows)
                {
                    view.cols = cols;
                    view.rows = rows;
                    view.send(ClientMessage::Resize { cols, rows });
                }
            }
            self.last_resize = Instant::now();
        } else if layout.panes.iter().any(|pane| {
            let geometry = zoom_layout
                .as_ref()
                .and_then(|zoom| zoom.pane(&pane.pane_id))
                .unwrap_or(pane);
            self.surface_views.iter().any(|view| {
                view.pane_id == pane.pane_id
                    && (view.cols, view.rows) != geometry.grid_size(metrics)
            })
        }) {
            needs_frame = true;
        }
        if let Some(dimensions) = self.focused_view().map(|view| (view.cols, view.rows)) {
            self.terminal_cols = dimensions.0;
            self.terminal_rows = dimensions.1;
        }
        self.layout = Some(layout);
        self.zoom_layout = zoom_layout;
        needs_frame
    }

    pub(super) fn grid_point(&self, position: Point<Pixels>) -> Option<GridPoint> {
        let view = self.focused_view()?;
        let pane = self.visible_layout()?.pane(&view.pane_id)?;
        let offset = self.workspace_scroll.offset();
        let x = pointer_coordinate(position.x, self.typography_scale)
            - if self.sidebar_open {
                self.sidebar_width + 5.0
            } else {
                0.0
            }
            - pane.canvas.x
            - f32::from(offset.x);
        let y = pointer_coordinate(position.y, self.typography_scale)
            - CHROME_HEIGHT
            - pane.canvas.y
            - f32::from(offset.y);
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let col = (x / self.typography.cell_width).floor() as usize;
        let row = (y / self.typography.cell_height).floor() as usize;
        (col < view.cols as usize && row < view.rows as usize).then_some(GridPoint { row, col })
    }
}

fn logical_viewport_dimensions(window: &Window) -> (f32, f32) {
    let viewport = window.viewport_size();
    (f32::from(viewport.width), f32::from(viewport.height))
}

fn pointer_coordinate(value: Pixels, scale_factor: f32) -> f32 {
    #[cfg(windows)]
    {
        // GPUI 0.2.2 reports Windows pointer positions in device pixels while
        // viewport dimensions and element layout already use logical pixels.
        f32::from(value) / scale_factor.max(1.0)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = scale_factor;
        f32::from(value)
    }
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
            other_window_available: cx.windows().len() > 1,
            revision: self
                .overlay_revision
                .unwrap_or_else(|| workspace.map_or(0, |workspace| workspace.revision)),
            current_revision: workspace.map_or(0, |workspace| workspace.revision),
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
        }
    }

    fn open_overlay(&mut self, overlay: Overlay, text: &str) {
        self.finish_dismiss_overlay();
        self.report_focus(false);
        self.overlay_revision = self.workspace.as_ref().map(|workspace| workspace.revision);
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
        self.overlay_revision = None;
        self.report_focus(true);
    }

    fn set_theme(&mut self, theme: ThemePreset) {
        if self.theme == theme {
            return;
        }
        self.theme = theme;
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
                                | Overlay::HeaderActions { .. }
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
                    | Overlay::HeaderActions { .. }
                    | Overlay::QuickAppearance
                    | Overlay::Settings
                    | Overlay::Confirm { .. }
                    | Overlay::Diagnostics
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
            Command::ToggleSidebar => self.sidebar_open = !self.sidebar_open,
            Command::ResetSidebarWidth => {
                self.sidebar_width = self.config.configured_sidebar_width;
                self.state.sidebar_width = self.sidebar_width;
                self.save_state();
            }
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
                self.set_theme(
                    self.state
                        .appearance
                        .theme
                        .unwrap_or(self.config.configured_appearance.theme),
                );
                self.terminal_opacity = self
                    .state
                    .appearance
                    .terminal_opacity
                    .unwrap_or(self.config.configured_appearance.terminal_opacity);
                self.background_effect = self
                    .state
                    .appearance
                    .background_effect
                    .unwrap_or(self.config.configured_appearance.background_effect);
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
                let (count, surfaces) = self.live_surface_details();
                if count == 0 {
                    self.begin_daemon_restart();
                } else {
                    self.open_overlay(
                        Overlay::ConfirmDaemonRestart {
                            details: format!(
                                "Restarting now ends {count} live surface{} and all processes inside them:\n\n{surfaces}",
                                if count == 1 { "" } else { "s" }
                            ),
                            revision: self
                                .workspace
                                .as_ref()
                                .map_or(0, |workspace| workspace.revision),
                        },
                        "",
                    );
                }
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

    fn activate_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(overlay) = self.overlay.clone() else {
            return;
        };
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
                self.mutate(
                    match purpose {
                        TextPurpose::CreateWorkspace => WorkspaceMutation::CreateSession { label },
                        TextPurpose::RenameWorkspace(session_id) => {
                            WorkspaceMutation::RenameSession { session_id, label }
                        }
                        TextPurpose::RenameTab(tab_id) => {
                            WorkspaceMutation::RenameTab { tab_id, label }
                        }
                    },
                    true,
                );
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
            Overlay::ConfirmDaemonRestart { revision, .. } => {
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
                self.begin_daemon_restart();
            }
            Overlay::Diagnostics => self.dismiss_overlay(),
            _ => {
                let choices = self.overlay_choices(cx);
                if let Some(choice) = choices.get(self.overlay_index).cloned() {
                    if let Some(reason) = choice.reason {
                        self.global_error = Some(reason);
                        return;
                    }
                    self.dismiss_overlay();
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
                        ChoiceAction::Window(handle) => {
                            if let Some(tab) = self.selected_tab().map(|tab| tab.id.clone()) {
                                self.transfer_tab(tab, Some(handle), window, cx);
                            }
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
    Window(AnyWindowHandle),
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

impl CompiApp {
    fn tab_label(&self, tab: &WorkspaceTab) -> String {
        if !tab.label.trim().is_empty() {
            return tab.label.clone();
        }
        let mut leaves = Vec::new();
        collect_leaves(&tab.layout, &mut leaves);
        leaves
            .iter()
            .find_map(|(_, id)| {
                self.surface_views
                    .iter()
                    .find(|view| &view.surface_id == id)
                    .map(|view| concise_tab_title(&view.title()))
            })
            .or_else(|| {
                leaves.first().and_then(|(_, id)| {
                    self.workspace
                        .as_ref()?
                        .surface(id)?
                        .working_directory
                        .as_ref()
                        .map(|cwd| concise_path_title(&cwd.resolved_wsl_path))
                })
            })
            .unwrap_or_else(|| "Terminal".into())
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
                    SurfaceStatus::Lost => "Lost",
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
                Overlay::PaneActions | Overlay::TabActions { .. } | Overlay::HeaderActions { .. },
            ) => {
                let context = self.command_context(cx);
                let commands: &[Command] = match self.overlay.as_ref() {
                    Some(Overlay::PaneActions) => &[
                        Command::SplitRight,
                        Command::SplitDown,
                        Command::TogglePaneZoom,
                    ],
                    Some(Overlay::TabActions { .. }) => &[
                        Command::NewTab,
                        Command::RenameTab,
                        Command::SplitRight,
                        Command::SplitDown,
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
                        Command::CreateWorkspace => "New Workspace…",
                        Command::RenameTab => "Rename Tab…",
                        Command::SplitRight => "Split Right",
                        Command::SplitDown => "Split Down",
                        Command::MoveTabToNewWindow => "Move to New Window",
                        Command::DetachTab => "Hide Tab",
                        Command::RemoveTab => "Remove Tab…",
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
        let colors = self.colors();
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
        let colors = self.colors();
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
        let colors = self.colors();
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
        let colors = self.colors();
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
        let colors = self.colors();
        let header_alpha = if self.glass {
            self.terminal_opacity
        } else {
            1.0
        };
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
            div()
                .id(("terminal-tab", index))
                .h_full()
                .w(px(metrics.tab_width))
                .flex_none()
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .bg(color(if selected {
                    colors.surface
                } else {
                    colors.background
                })
                .opacity(header_alpha))
                .border_b_1()
                .border_color(color(if selected {
                    colors.accent
                } else {
                    colors.border
                }))
                .text_color(color(if selected {
                    colors.foreground
                } else {
                    colors.muted
                }))
                .hover(move |style| {
                    style
                        .bg(color(colors.surface_hover).opacity(header_alpha))
                        .cursor_pointer()
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
                        this.select_terminal(context_id.clone(), false);
                        this.open_overlay(
                            Overlay::TabActions {
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
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(self.tab_label(tab)),
                )
                .child(
                    div()
                        .id(("hide-tab", index))
                        .px_1()
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
            .bg(color(colors.background).opacity(header_alpha))
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
                    )),
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
        let colors = self.colors();
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
            .w(px(self.sidebar_width + 5.0))
            .h_full()
            .flex_none()
            .flex()
            .child(
                div()
                    .w(px(self.sidebar_width))
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
                div()
                    .id("sidebar-divider")
                    .w(px(5.0))
                    .h_full()
                    .bg(color(colors.border))
                    .cursor(gpui::CursorStyle::ResizeLeftRight)
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
            )
            .into_any_element()
    }

    fn render_panes(&self, cx: &Context<Self>) -> AnyElement {
        let colors = self.colors();
        let Some(layout) = self.visible_layout() else {
            return div()
                .flex_1()
                .size_full()
                .min_w_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(color(colors.background).opacity(self.terminal_opacity))
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
            let pane_id = geometry.pane_id.clone();
            let focus_id = pane_id.clone();
            let input_id = pane_id.clone();
            let scroll_id = pane_id.clone();
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
                    self.theme,
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
                let progress =
                    1.0 - view.scroll_offset.min(snapshot.scrollback.len()) as f32 / history;
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
                    SurfaceStatus::Lost => "Lost",
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
                .id(("pane", index))
                .absolute()
                .left(px(geometry.rect.x))
                .top(px(geometry.rect.y))
                .w(px(geometry.rect.width))
                .h(px(geometry.rect.height))
                .bg(color(colors.background).opacity(self.terminal_opacity))
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
                                    .text_color(color(if matches!(status, "Failed" | "Lost") {
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
                            .when(matches!(status, "Exited" | "Failed" | "Lost"), |actions| {
                                actions.child(self.pane_command_button(
                                    ("restart-pane", index),
                                    "Restart surface",
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
                .child(
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
                        .on_mouse_up(MouseButton::Left, cx.listener(Self::on_terminal_mouse_up))
                        .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_terminal_mouse_up))
                        .on_mouse_move(cx.listener(Self::on_terminal_mouse_move))
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
                                                paint.theme,
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
                .when_some(terminal_scroll, |pane, (position, length)| {
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
                })
        });
        let dividers = layout.dividers.iter().enumerate().map(|(index, divider)| {
            div()
                .id(("split-divider", index))
                .absolute()
                .left(px(divider.rect.x))
                .top(px(divider.rect.y))
                .w(px(divider.rect.width))
                .h(px(divider.rect.height))
                .bg(color(colors.border))
                .hover(move |style| style.bg(color(colors.accent)))
                .cursor(if divider.axis == SplitAxis::Horizontal {
                    gpui::CursorStyle::ResizeLeftRight
                } else {
                    gpui::CursorStyle::ResizeUpDown
                })
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

    fn render_workspace_scrollbar(&self, horizontal: bool, cx: &Context<Self>) -> AnyElement {
        let colors = self.colors();
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
        let x = f32::from(position.x)
            - if self.sidebar_open {
                self.sidebar_width + 5.0
            } else {
                0.0
            };
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
                    x: f32::from(event.position.x - offset.x)
                        - if self.sidebar_open {
                            self.sidebar_width + 5.0
                        } else {
                            0.0
                        },
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
            let pointer_x = pointer_coordinate(event.position.x, self.typography_scale);
            let pointer_y = pointer_coordinate(event.position.y, self.typography_scale);
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
            let display_theme = self.theme;
            let seed = TransferSeed {
                tab_id: tab_id.clone(),
                source_state: self.state.clone(),
                display_theme,
                display_terminal_opacity: self.terminal_opacity,
                display_background_effect: self.background_effect,
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
        let colors = self.colors();
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
                                colors,
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
                                    color(colors.cursor),
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
        let colors = self.colors();
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
        let tab = self
            .workspace
            .as_ref()
            .and_then(|workspace| self.state.selected_tab(workspace));
        self.surface_views.retain(|view| {
            !view.closed.load(Ordering::Acquire)
                || tab.is_some_and(|tab| contains_surface(&tab.layout, &view.surface_id))
        });
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
    #[cfg(windows)]
    use super::pointer_coordinate;
    use super::{
        COMPACT_TAB_WIDTH, HEADER_BUTTON_SLOT_WIDTH, PANE_ACTIONS_COMPACT_WIDTH,
        PANE_ACTIONS_FULL_WIDTH, PaneActionsMode, PaneZoomState, TAB_WIDTH, TITLEBAR_BRAND_WIDTH,
        WINDOW_CONTROLS_WIDTH, concise_path_title, concise_tab_title, display_index_for_bounds,
        header_metrics, opacity_at_slider_position,
    };
    use compi_protocol::{PaneId, TabId};
    use gpui::{Bounds, point, px, size};

    #[cfg(windows)]
    #[test]
    fn windows_pointer_coordinates_follow_logical_terminal_geometry() {
        assert_eq!(pointer_coordinate(px(114.0), 1.5), 76.0);
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
