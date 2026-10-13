use super::catalog::CatalogTarget;
use super::*;

const SETTINGS_NAV_ITEMS: usize = SettingsSection::ALL.len();
/// Focus stops of Settings → Interface; see `interface_action`.
const INTERFACE_ACTIONS: usize = 17;
/// Focus stops of Settings → Terminal before the shell prompt group; see
/// `terminal_action`.
pub(super) const PROMPT_FOCUS_OFFSET: usize = 5;

/// Focusable controls of Settings → Updates, in focus order. Which ones exist depends
/// on the update state, so offsets are positions in `update_controls`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum UpdateControl {
    Primary(crate::updates::UpdateButton),
    CheckForUpdates,
    DownloadAutomatically,
    CandidateMore,
    CandidateNotes,
    WhatsNewMore,
    ReleaseNotes,
    Advanced,
    Repair,
    OpenLog,
}

#[derive(Clone, Copy)]
enum SettingsAction {
    Scope(SettingsScope),
    BrowseThemes(CatalogTarget),
    ToggleTransparency,
    ToggleBlur,
    FollowTheme,
    /// Opens a font select's list.
    FontSelect(Popup),
    Opacity(f32),
    ResetWindowAppearance,
    ResetSidebar,
    ResetLayout,
    Zoom(Command),
    OpenConfiguration,
    OpenPalette,
    OpenSettings,
    ToggleFps,
    RebuildRenderer,
    CopyPerformance,
    Reconnect,
    OpenDiagnostics,
    RestartDaemon,
    Update(UpdateControl),
    Prompt(super::prompt::PromptAction),
    Metadata(usize),
    Density(crate::theme::WorkspaceDensity),
    ToggleConfirmClose,
    FocusIndicator(crate::theme::FocusIndicator),
    ProgramStatus(usize),
}

#[derive(Clone, Copy)]
enum SettingsButtonTone {
    Secondary,
    Primary,
    Destructive,
}

impl CompiApp {
    pub(super) fn settings_content_focus(&self, offset: usize) -> usize {
        SETTINGS_NAV_ITEMS + offset
    }

    /// Which surface the settings keyboard model serves: Settings (`true`) or Quick
    /// Appearance (`false`).
    fn settings_surface(&self) -> Option<bool> {
        if matches!(self.overlay, Some(Overlay::Settings)) {
            Some(true)
        } else if self.popup == Some(Popup::QuickAppearance) {
            Some(false)
        } else {
            None
        }
    }

    fn settings_action_count(&self) -> usize {
        match self.settings_section {
            SettingsSection::Appearance => self.appearance_action_count(),
            SettingsSection::Interface => INTERFACE_ACTIONS,
            SettingsSection::Terminal => PROMPT_FOCUS_OFFSET + self.prompt_actions().len(),
            SettingsSection::Keyboard => 2,
            SettingsSection::Performance => 3,
            SettingsSection::Updates => self.update_page().2.len(),
            SettingsSection::Advanced => 8,
        }
    }

    fn settings_action(&self, offset: usize) -> Option<SettingsAction> {
        match self.settings_section {
            SettingsSection::Appearance => self.appearance_action(offset),
            SettingsSection::Interface => interface_action(offset),
            SettingsSection::Terminal => terminal_action(offset).or_else(|| {
                self.prompt_actions()
                    .get(offset.checked_sub(PROMPT_FOCUS_OFFSET)?)
                    .copied()
                    .map(SettingsAction::Prompt)
            }),
            SettingsSection::Keyboard => match offset {
                0 => Some(SettingsAction::OpenPalette),
                1 => Some(SettingsAction::OpenConfiguration),
                _ => None,
            },
            SettingsSection::Performance => match offset {
                0 => Some(SettingsAction::ToggleFps),
                1 => Some(SettingsAction::RebuildRenderer),
                2 => Some(SettingsAction::CopyPerformance),
                _ => None,
            },
            SettingsSection::Updates => self
                .update_page()
                .2
                .get(offset)
                .copied()
                .map(SettingsAction::Update),
            SettingsSection::Advanced => match offset {
                0 => Some(SettingsAction::Scope(SettingsScope::Global)),
                1 => Some(SettingsAction::Scope(SettingsScope::Window)),
                2 => Some(SettingsAction::FollowTheme),
                3 => Some(SettingsAction::BrowseThemes(CatalogTarget::Terminal)),
                4 => Some(SettingsAction::OpenConfiguration),
                5 => Some(SettingsAction::Reconnect),
                6 => Some(SettingsAction::OpenDiagnostics),
                7 => Some(SettingsAction::RestartDaemon),
                _ => None,
            },
        }
    }

    fn normalize_settings_focus(&mut self) {
        let Some(full) = self.settings_surface() else {
            return;
        };
        let (base, count) = if full {
            (SETTINGS_NAV_ITEMS, self.settings_action_count())
        } else {
            (0, self.appearance_action_count())
        };
        self.overlay_focus = self.overlay_focus.min(base + count.saturating_sub(1));
    }

    pub(super) fn prompt_toggle(
        &self,
        enabled: bool,
        action: super::prompt::PromptAction,
        index: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        self.render_settings_toggle(
            enabled,
            SettingsAction::Prompt(action),
            PROMPT_FOCUS_OFFSET + index,
            true,
            cx,
        )
    }

    fn select_settings_section(&mut self, section: SettingsSection) {
        self.settings_section = section;
        self.overlay_scroll.set_offset(point(px(0.0), px(0.0)));
        if section == SettingsSection::Terminal {
            self.enter_prompt_settings();
        }
    }

    pub(super) fn handle_settings_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(self.overlay, Some(Overlay::Settings)) {
            return false;
        }
        if key.key == "escape" {
            // An open popup holds the keyboard and closes on Escape itself; should the
            // key still arrive here, it closes only the popup.
            if self.popup.take().is_none() && !self.close_prompt_dropdown() {
                self.dismiss_overlay();
            }
            cx.notify();
            return true;
        }
        self.settings_navigation_key(key, true, window, cx);
        true
    }

    /// Keys in the Quick Appearance popover; Escape closes it before reaching here.
    pub(super) fn quick_appearance_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_navigation_key(key, false, window, cx);
    }

    /// Moves the focus through Settings (`full`) or Quick Appearance, and activates
    /// the focused control.
    fn settings_navigation_key(
        &mut self,
        key: &Keystroke,
        full: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.normalize_settings_focus();
        let base = if full { SETTINGS_NAV_ITEMS } else { 0 };
        let offset = self.overlay_focus.saturating_sub(base);
        let action = if full {
            self.settings_action(offset)
        } else {
            self.appearance_action(offset)
        };
        if full
            && matches!(
                action,
                Some(SettingsAction::Prompt(super::prompt::PromptAction::Styles))
            )
            && matches!(key.key.as_str(), "up" | "down" | "home" | "end")
        {
            self.move_prompt_style(key.key.as_str());
            self.settings_scroll_to_focus = true;
            cx.notify();
            return;
        }
        let count = if full {
            self.settings_action_count()
        } else {
            self.appearance_action_count()
        };
        match key.key.as_str() {
            "tab" => {
                let total = base + count;
                self.overlay_focus = if key.modifiers.shift {
                    (self.overlay_focus + total - 1) % total
                } else {
                    (self.overlay_focus + 1) % total
                };
            }
            "up" | "down" if full && self.overlay_focus < SETTINGS_NAV_ITEMS => {
                let next = if key.key == "up" {
                    (self.overlay_focus + SETTINGS_NAV_ITEMS - 1) % SETTINGS_NAV_ITEMS
                } else {
                    (self.overlay_focus + 1) % SETTINGS_NAV_ITEMS
                };
                self.overlay_focus = next;
                self.select_settings_section(SettingsSection::ALL[next]);
            }
            "left" | "right"
                if matches!(action, Some(SettingsAction::Opacity(_)))
                    && self.overlay_focus >= base =>
            {
                self.adjust_opacity(if key.key == "left" { -0.05 } else { 0.05 }, window, cx);
            }
            "right" if full && self.overlay_focus < SETTINGS_NAV_ITEMS => {
                self.overlay_focus = SETTINGS_NAV_ITEMS;
            }
            "left" if full && self.overlay_focus >= SETTINGS_NAV_ITEMS => {
                self.overlay_focus = self.settings_section as usize;
            }
            "up" | "down" if self.overlay_focus >= base => {
                self.overlay_focus = base
                    + if key.key == "up" {
                        (offset + count - 1) % count
                    } else {
                        (offset + 1) % count
                    };
            }
            "enter" | "space" => {
                if full && self.overlay_focus < SETTINGS_NAV_ITEMS {
                    self.select_settings_section(SettingsSection::ALL[self.overlay_focus]);
                } else if let Some(action) = action {
                    self.activate_settings_action(action, window, cx);
                }
            }
            _ => return,
        }
        self.normalize_settings_focus();
        self.settings_scroll_to_focus = true;
        cx.notify();
    }

    fn appearance_action_count(&self) -> usize {
        4 + usize::from(self.scoped_appearance().transparent_background) * 2
            + usize::from(self.settings_scope == SettingsScope::Window)
            + usize::from(self.settings_surface() == Some(false))
    }

    fn appearance_action(&self, offset: usize) -> Option<SettingsAction> {
        match offset {
            0 => Some(SettingsAction::Scope(SettingsScope::Global)),
            1 => Some(SettingsAction::Scope(SettingsScope::Window)),
            2 => Some(SettingsAction::BrowseThemes(CatalogTarget::Both)),
            3 => Some(SettingsAction::ToggleTransparency),
            4 if self.scoped_appearance().transparent_background => {
                Some(SettingsAction::Opacity(0.05))
            }
            5 if self.scoped_appearance().transparent_background => {
                Some(SettingsAction::ToggleBlur)
            }
            value
                if value
                    == 4 + usize::from(self.scoped_appearance().transparent_background) * 2
                    && self.settings_scope == SettingsScope::Window =>
            {
                Some(SettingsAction::ResetWindowAppearance)
            }
            value
                if self.settings_surface() == Some(false)
                    && value == self.appearance_action_count() - 1 =>
            {
                Some(SettingsAction::OpenSettings)
            }
            _ => None,
        }
    }

    fn activate_settings_action(
        &mut self,
        action: SettingsAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            SettingsAction::Update(control) => self.activate_update_control(control, window, cx),
            SettingsAction::Metadata(index) => {
                let mut settings = self.config.metadata.clone();
                match index {
                    0 => settings.directory = !settings.directory,
                    1 => settings.process = !settings.process,
                    2 => settings.git = !settings.git,
                    _ => settings.dimensions = !settings.dimensions,
                }
                if let Err(error) = self.config.save_metadata_settings(settings) {
                    self.global_error = Some(error);
                }
            }
            SettingsAction::ToggleConfirmClose => {
                if let Err(error) = self.config.save_confirm_close(!self.config.confirm_close) {
                    self.global_error = Some(error);
                }
            }
            SettingsAction::FocusIndicator(indicator) => {
                if let Err(error) = self.config.save_focus_indicator(indicator) {
                    self.global_error = Some(error);
                }
            }
            SettingsAction::ProgramStatus(index) => {
                let mut settings = self.config.program_status.clone();
                match index {
                    0 => settings.tabs = !settings.tabs,
                    1 => settings.panes = !settings.panes,
                    _ => settings.sidebar = !settings.sidebar,
                }
                if let Err(error) = self.config.save_program_status_settings(settings) {
                    self.global_error = Some(error);
                }
            }
            SettingsAction::Scope(scope) => self.settings_scope = scope,
            SettingsAction::BrowseThemes(target) => {
                self.open_theme_catalog_target(self.settings_scope, target)
            }
            SettingsAction::ToggleTransparency => {
                self.set_transparent_background(
                    !self.scoped_appearance().transparent_background,
                    window,
                    cx,
                );
            }
            SettingsAction::ToggleBlur => {
                self.set_blur_background(
                    self.scoped_appearance().background_effect != BackgroundEffect::Blurred,
                    window,
                    cx,
                );
            }
            SettingsAction::FollowTheme => self.follow_theme_terminal_colors(window, cx),
            SettingsAction::FontSelect(popup) => self.popup = Some(popup),
            SettingsAction::Opacity(delta) => self.adjust_opacity(delta, window, cx),
            SettingsAction::ResetWindowAppearance => self.reset_window_appearance(window),
            SettingsAction::ResetSidebar => self.execute(Command::ResetSidebarWidth, window, cx),
            SettingsAction::ResetLayout => self.execute(Command::ResetClientLayout, window, cx),
            SettingsAction::Zoom(command) => self.execute(command, window, cx),
            SettingsAction::OpenConfiguration => {
                self.execute(Command::OpenConfiguration, window, cx)
            }
            SettingsAction::OpenPalette => self.execute(Command::OpenPalette, window, cx),
            SettingsAction::ToggleFps => {
                self.state.show_fps = !self.state.show_fps;
                self.save_state();
            }
            SettingsAction::RebuildRenderer => self.rebuild_renderer(window),
            SettingsAction::CopyPerformance => self.copy_performance_diagnostics(cx),
            SettingsAction::Reconnect => self.execute(Command::Reconnect, window, cx),
            SettingsAction::OpenDiagnostics => self.execute(Command::OpenDiagnostics, window, cx),
            SettingsAction::RestartDaemon => self.execute(Command::RestartDaemon, window, cx),
            SettingsAction::Prompt(action) => self.activate_prompt_action(action),
            SettingsAction::Density(density) => self.apply_density(density, window, cx),
            SettingsAction::OpenSettings => self.open_overlay(Overlay::Settings, ""),
        }
        self.normalize_settings_focus();
        self.settings_scroll_to_focus = true;
    }

    fn adjust_opacity(&mut self, delta: f32, window: &mut Window, cx: &mut Context<Self>) {
        let mut appearance = self.scoped_appearance();
        appearance.terminal_opacity = (appearance.terminal_opacity + delta).clamp(
            crate::config::MIN_TERMINAL_OPACITY,
            crate::config::MAX_TERMINAL_OPACITY,
        );
        self.apply_scoped_appearance(appearance, window, cx);
    }

    fn apply_ui_font(&mut self, preset: UiFontPreset, window: &Window, cx: &mut Context<Self>) {
        if self.config.ui_font == preset {
            return;
        }
        if let Err(error) = self.config.save_ui_font(preset) {
            self.global_error = Some(error);
            return;
        }
        self.ui_font = crate::font_catalog::resolve_ui_font(preset, window.text_system());
        self.broadcast_global_appearance(window, cx);
        cx.notify();
    }

    fn apply_density(
        &mut self,
        density: crate::theme::WorkspaceDensity,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.config.density == density {
            return;
        }
        if let Err(error) = self.config.save_density(density) {
            self.global_error = Some(error);
            return;
        }
        // Geometry changes go through the ordinary resize path; shells keep running.
        self.rebuild_layout(window, true);
        self.broadcast_global_appearance(window, cx);
        cx.notify();
    }

    fn apply_terminal_font(
        &mut self,
        preset: TerminalFontPreset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .config
            .configured_font
            .family
            .eq_ignore_ascii_case(preset.family())
        {
            return;
        }
        if let Err(error) = self.config.save_terminal_font(preset) {
            self.global_error = Some(error);
            return;
        }
        self.font_settings = self.config.font.clone();
        self.typography_scale = 0.0;
        if self.refresh_typography(window) {
            self.rebuild_layout(window, true);
        }
        self.broadcast_global_appearance(window, cx);
        cx.notify();
    }

    pub(super) fn rebuild_renderer(&mut self, window: &mut Window) {
        for view in &mut self.surface_views {
            if let Ok(mut cache) = view.row_render_cache.lock() {
                cache.clear();
            }
            view.image_cache.clear();
            view.image_pending.clear();
            view.image_rejected.clear();
            view.image_capacity_rejected.clear();
            view.image_cache_bytes = 0;
            view.image_viewport = None;
            view.images_dirty = true;
            view.image_retry = true;
            view.image_error = None;
        }
        for (_, image) in self.rendered_images.drain() {
            let _ = window.drop_image(image);
        }
        self.rendered_image_ids.clear();
        self.typography_scale = 0.0;
        self.refresh_typography(window);
        self.rebuild_layout(window, true);
        self.performance_notice =
            Some("Renderer rebuilt. Visual caches will repopulate as needed.".into());
    }

    pub(super) fn settings_heading(&self, title: &'static str) -> AnyElement {
        div()
            .text_size(px(18.0))
            .font_weight(FontWeight::SEMIBOLD)
            .child(title)
            .into_any_element()
    }

    pub(super) fn settings_subheading(&self, label: &str) -> AnyElement {
        let colors = *self.colors();
        div()
            .pb_1()
            .text_size(px(UI_MICRO_TEXT_SIZE))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(color(modal_text_color(colors.muted, &colors)))
            .child(label.to_uppercase())
            .into_any_element()
    }

    fn settings_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        focused: bool,
        tone: SettingsButtonTone,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        self.settings_button_face(id, label, focused, tone)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(on_click)
            .into_any_element()
    }

    /// A settings button that opens a popup: the popup's trigger takes its clicks.
    pub(super) fn settings_popup_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        focused: bool,
        destructive: bool,
    ) -> AnyElement {
        self.settings_button_face(
            id,
            label,
            focused,
            if destructive {
                SettingsButtonTone::Destructive
            } else {
                SettingsButtonTone::Secondary
            },
        )
        .into_any_element()
    }

    fn settings_button_face(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        focused: bool,
        tone: SettingsButtonTone,
    ) -> Stateful<Div> {
        let colors = *self.colors();
        let (background, hover, foreground) = match tone {
            SettingsButtonTone::Secondary => (
                blend_rgb(colors.surface, colors.foreground, 0.04),
                blend_rgb(colors.surface, colors.foreground, 0.09),
                colors.foreground,
            ),
            SettingsButtonTone::Primary => (
                blend_rgb(colors.surface, colors.accent, 0.12),
                blend_rgb(colors.surface, colors.accent, 0.20),
                colors.foreground,
            ),
            SettingsButtonTone::Destructive => (
                blend_rgb(colors.surface, colors.error, 0.06),
                blend_rgb(colors.surface, colors.error, 0.12),
                colors.error,
            ),
        };
        let border = if focused {
            if matches!(tone, SettingsButtonTone::Primary) {
                colors.foreground
            } else {
                colors.accent
            }
        } else if matches!(tone, SettingsButtonTone::Primary) {
            colors.accent
        } else {
            background
        };
        div()
            .id(id)
            .h(px(32.0))
            .min_w(px(72.0))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(UI_BODY_TEXT_SIZE))
            .line_height(px(UI_BODY_LINE_HEIGHT))
            .text_center()
            .whitespace_nowrap()
            .rounded_sm()
            .border_1()
            .border_color(color(border))
            .bg(color(background))
            .font_weight(if matches!(tone, SettingsButtonTone::Secondary) {
                FontWeight::MEDIUM
            } else {
                FontWeight::SEMIBOLD
            })
            .text_color(color(ui_text_color(foreground, background)))
            .hover(move |style| style.bg(color(hover)).cursor_pointer())
            .child(Into::<SharedString>::into(label))
    }

    pub(super) fn settings_action_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        focused: bool,
        destructive: bool,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        self.settings_button(
            id,
            label,
            focused,
            if destructive {
                SettingsButtonTone::Destructive
            } else {
                SettingsButtonTone::Secondary
            },
            on_click,
        )
    }

    pub(super) fn settings_primary_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        focused: bool,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        self.settings_button(id, label, focused, SettingsButtonTone::Primary, on_click)
    }

    pub(super) fn settings_segment_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        active: bool,
        focused: bool,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let colors = *self.colors();
        let group = colors.surface;
        let selected = blend_rgb(colors.surface, colors.accent, 0.08);
        let background = if active { selected } else { group };
        div()
            .id(id)
            .h(px(32.0))
            .min_w(px(72.0))
            .flex_none()
            .px_3()
            .rounded_sm()
            .border_1()
            .border_color(color(if focused || active {
                colors.accent
            } else {
                colors.border
            }))
            .bg(color(background))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(UI_BODY_TEXT_SIZE))
            .line_height(px(UI_BODY_LINE_HEIGHT))
            .text_center()
            .whitespace_nowrap()
            .font_weight(if active {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::MEDIUM
            })
            .text_color(color(ui_text_color(
                if active {
                    colors.foreground
                } else {
                    colors.muted
                },
                background,
            )))
            .hover(move |style| {
                style
                    .bg(color(if active {
                        selected
                    } else {
                        colors.surface_hover
                    }))
                    .cursor_pointer()
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(on_click)
            .child(Into::<SharedString>::into(label))
            .into_any_element()
    }

    pub(super) fn overlay_close_button(
        &self,
        id: impl Into<gpui::ElementId>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let background = blend_rgb(colors.surface, colors.foreground, 0.07);
        let hover = blend_rgb(colors.surface, colors.foreground, 0.13);
        div()
            .id(id)
            .size(px(36.0))
            .rounded_sm()
            .flex()
            .items_center()
            .justify_center()
            .hover(move |style| style.bg(color(hover)).cursor_pointer())
            .tooltip(controls::tooltip("Close", None, Some("escape")))
            .on_click(cx.listener(|this, _, _, cx| {
                this.dismiss_overlay();
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                div()
                    .size(px(28.0))
                    .rounded_sm()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(color(background))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(16.0))
                    .text_color(color(ui_text_color(colors.foreground, background)))
                    .child("×"),
            )
            .into_any_element()
    }

    fn render_settings_navigation(&self, compact: bool, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let items = SettingsSection::ALL
            .into_iter()
            .enumerate()
            .map(|(index, section)| {
                let active = self.settings_section == section;
                let focused = self.overlay_focus == index;
                let active_background = blend_rgb(colors.surface, colors.foreground, 0.06);
                let background = if active {
                    active_background
                } else {
                    colors.surface
                };
                div()
                    .id(("settings-nav", index))
                    .min_h(px(36.0))
                    .flex_none()
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(color(if focused { colors.accent } else { background }))
                    .bg(color(background))
                    .text_color(color(modal_text_color(
                        if active {
                            colors.foreground
                        } else {
                            colors.muted
                        },
                        &colors,
                    )))
                    .font_weight(if active {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    })
                    .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.overlay_focus = index;
                        this.select_settings_section(section);
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .child(section.label())
            });
        if compact {
            div()
                .flex_none()
                .px_4()
                .py_2()
                .flex()
                .flex_wrap()
                .gap_1()
                .border_b_1()
                .border_color(color(colors.border))
                .children(items)
                .into_any_element()
        } else {
            div()
                .w(px(184.0))
                .flex_none()
                .p_3()
                .flex()
                .flex_col()
                .gap_1()
                .border_r_1()
                .border_color(color(colors.border))
                .children(items)
                .into_any_element()
        }
    }

    pub(super) fn settings_focus_anchor(&self, focused: bool, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity();
        canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                if !focused {
                    return;
                }
                entity.update(cx, |this, _| {
                    if !this.settings_scroll_to_focus {
                        return;
                    }
                    let Some(viewport) = this.settings_scroll_bounds else {
                        return;
                    };
                    this.settings_scroll_to_focus = false;
                    let top = viewport.top() + px(8.0);
                    let bottom = viewport.bottom() - px(8.0);
                    let delta = if bounds.top() < top {
                        top - bounds.top()
                    } else if bounds.bottom() > bottom {
                        bottom - bounds.bottom()
                    } else {
                        px(0.0)
                    };
                    if delta != px(0.0) {
                        let offset = this.overlay_scroll.offset();
                        this.overlay_scroll
                            .set_offset(point(offset.x, offset.y + delta));
                        window.refresh();
                    }
                });
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }
    fn render_settings_toggle(
        &self,
        enabled: bool,
        action: SettingsAction,
        offset: usize,
        full: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let base = if full { SETTINGS_NAV_ITEMS } else { 0 };
        let focused = self.overlay_focus == base + offset;
        let colors = *self.colors();
        div()
            .id(("settings-toggle", offset))
            .relative()
            .flex_none()
            .min_w(px(92.0))
            .min_h(px(36.0))
            .px_2()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .rounded_sm()
            .border_1()
            .border_color(color(if focused {
                colors.accent
            } else {
                colors.border
            }))
            .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.overlay_focus = base + offset;
                this.activate_settings_action(action, window, cx);
                cx.stop_propagation();
                cx.notify();
            }))
            .child(if enabled { "On" } else { "Off" })
            .child(
                div()
                    .w(px(32.0))
                    .h(px(18.0))
                    .p(px(2.0))
                    .flex()
                    .items_center()
                    .rounded_full()
                    .bg(color(if enabled {
                        colors.accent
                    } else {
                        colors.border
                    }))
                    .when(enabled, |switch| switch.justify_end())
                    .child(
                        div()
                            .size(px(14.0))
                            .rounded_full()
                            .bg(color(colors.foreground)),
                    ),
            )
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element()
    }

    pub(super) fn settings_row(
        &self,
        label: &'static str,
        detail: String,
        control: AnyElement,
        compact: bool,
    ) -> AnyElement {
        let colors = *self.colors();
        div()
            .min_w_0()
            .flex()
            .gap_3()
            .py_3()
            .border_b_1()
            .border_color(color(colors.border))
            .when(compact, |row| row.flex_col().items_start())
            .when(!compact, |row| row.items_center().justify_between())
            .child(
                div()
                    .min_w_0()
                    // Basis auto, not `flex_1`'s zero basis: with a zero basis Taffy sizes
                    // the row as if every character wrapped, which leaves blank scroll space
                    // below rows nested deeper in a column (the shell prompt group).
                    .when(!compact, |label| label.flex_auto())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().font_weight(FontWeight::MEDIUM).child(label))
                    .when(!detail.is_empty(), |label| {
                        label.child(
                            div()
                                .text_size(px(UI_SMALL_TEXT_SIZE))
                                .text_color(color(modal_text_color(colors.muted, &colors)))
                                .child(detail),
                        )
                    }),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_none()
                    .when(compact, |control| control.w_full())
                    .child(control),
            )
            .into_any_element()
    }

    fn settings_control_button(
        &self,
        label: &'static str,
        action: SettingsAction,
        offset: usize,
        destructive: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let focused = self.overlay_focus == self.settings_content_focus(offset);
        div()
            .relative()
            .flex_none()
            .child(self.settings_action_button(
                ("settings-action", offset),
                label,
                focused,
                destructive,
                cx.listener(move |this, _, window, cx| {
                    this.overlay_focus = this.settings_content_focus(offset);
                    this.activate_settings_action(action, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }),
            ))
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element()
    }

    fn render_settings_scope(&self, full: bool, compact: bool, cx: &Context<Self>) -> AnyElement {
        let base = if full { SETTINGS_NAV_ITEMS } else { 0 };
        let choices = [
            (SettingsScope::Global, "Global defaults"),
            (SettingsScope::Window, "This window"),
        ];
        let controls = div()
            .flex()
            .when(compact, |controls| controls.flex_col().items_start())
            .gap_2()
            .children(
                choices
                    .into_iter()
                    .enumerate()
                    .map(|(index, (scope, label))| {
                        let focused = self.overlay_focus == base + index;
                        div()
                            .relative()
                            .flex_none()
                            .child(self.settings_segment_button(
                                ("settings-scope", index),
                                label,
                                self.settings_scope == scope,
                                focused,
                                cx.listener(move |this, _, window, cx| {
                                    this.overlay_focus = base + index;
                                    this.activate_settings_action(
                                        SettingsAction::Scope(scope),
                                        window,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            ))
                            .child(self.settings_focus_anchor(focused, cx))
                    }),
            )
            .into_any_element();
        self.settings_row(
            "Scope",
            match self.settings_scope {
                SettingsScope::Global => "Defaults for windows without an override.",
                SettingsScope::Window => "Only this window; global defaults stay unchanged.",
            }
            .into(),
            controls,
            compact,
        )
    }

    /// Interface font or Terminal font: a select of the bundled presets, each shown in
    /// its own font. A custom terminal family from the configuration file shows as the
    /// value with no preset checked.
    fn render_font_select(&self, cx: &Context<Self>) -> AnyElement {
        let focused = self.overlay_focus == self.settings_content_focus(0);
        let (popup, options, selected, value): (_, Vec<_>, _, SharedString) =
            match self.settings_section {
                SettingsSection::Interface => (
                    Popup::UiFont,
                    UiFontPreset::ALL
                        .iter()
                        .map(|preset| {
                            controls::select_option(preset.label())
                                .detail("Aa Bb 0123")
                                .font(preset.family())
                        })
                        .collect(),
                    UiFontPreset::ALL
                        .iter()
                        .position(|preset| *preset == self.config.ui_font),
                    self.config.ui_font.label().into(),
                ),
                _ => (
                    Popup::TerminalFont,
                    TerminalFontPreset::ALL
                        .iter()
                        .map(|preset| {
                            controls::select_option(preset.label())
                                .detail("0O1l {} [] $ cargo")
                                .font(preset.family())
                        })
                        .collect(),
                    TerminalFontPreset::ALL.iter().position(|preset| {
                        self.config
                            .configured_font
                            .family
                            .eq_ignore_ascii_case(preset.family())
                    }),
                    self.config.configured_font.family.clone().into(),
                ),
            };
        let select = controls::select(
            match popup {
                Popup::UiFont => "ui-font-select",
                _ => "terminal-font-select",
            },
            options,
            selected,
            self.popup == Some(popup),
        )
        .placeholder(value)
        .focused(focused)
        .anchor(Anchor::TopRight)
        .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
            this.overlay_focus = this.settings_content_focus(0);
            this.set_popup(popup, *open);
            cx.notify();
        }))
        .on_pick(cx.listener(move |this, index: &usize, window, cx| {
            if popup == Popup::UiFont {
                if let Some(preset) = UiFontPreset::ALL.get(*index) {
                    this.apply_ui_font(*preset, window, cx);
                }
            } else if let Some(preset) = TerminalFontPreset::ALL.get(*index) {
                this.apply_terminal_font(*preset, window, cx);
            }
            cx.notify();
        }));
        div()
            .relative()
            .flex_none()
            .child(select)
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element()
    }

    fn render_settings_content(&self, compact: bool, cx: &Context<Self>) -> AnyElement {
        match self.settings_section {
            SettingsSection::Appearance => self.render_appearance_controls(true, compact, cx),
            SettingsSection::Interface => self.render_interface_settings(compact, cx),
            SettingsSection::Terminal => self.render_terminal_settings(compact, cx),
            SettingsSection::Keyboard => self.render_keyboard_settings(compact, cx),
            SettingsSection::Performance => self.render_performance_section(cx),
            SettingsSection::Updates => self.render_update_settings(compact, cx),
            SettingsSection::Advanced => self.render_advanced_settings(compact, cx),
        }
    }

    /// Focus offsets follow `interface_action`.
    fn render_interface_settings(&self, compact: bool, cx: &Context<Self>) -> AnyElement {
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.settings_heading("Interface"))
            .child(self.settings_row(
                "Interface font",
                String::new(),
                self.render_font_select(cx),
                compact,
            ))
            .child(self.render_density_setting(1, compact, cx))
            .child(self.settings_row(
                "Sidebar width",
                format!("{:.0}px", self.sidebar_width),
                self.settings_control_button(
                    "Reset width",
                    SettingsAction::ResetSidebar,
                    3,
                    false,
                    cx,
                ),
                compact,
            ))
            .child(self.settings_row(
                "Window layout",
                "Restore the saved layout defaults.".into(),
                self.settings_control_button(
                    "Reset layout",
                    SettingsAction::ResetLayout,
                    4,
                    false,
                    cx,
                ),
                compact,
            ))
            .child(self.settings_subheading("Tab metadata"))
            .children(
                [
                    (
                        "Directory",
                        "Show the current directory reported by the shell.",
                        self.config.metadata.directory,
                    ),
                    (
                        "Active process",
                        "Show the terminal foreground process, not the launcher.",
                        self.config.metadata.process,
                    ),
                    (
                        "Git branch and changes",
                        "Show branch and change state in the terminal's environment.",
                        self.config.metadata.git,
                    ),
                    (
                        "Dimensions",
                        "Show terminal columns and rows.",
                        self.config.metadata.dimensions,
                    ),
                ]
                .into_iter()
                .enumerate()
                .map(|(index, (label, description, enabled))| {
                    self.settings_row(
                        label,
                        description.into(),
                        self.render_settings_toggle(
                            enabled,
                            SettingsAction::Metadata(index),
                            5 + index,
                            true,
                            cx,
                        ),
                        compact,
                    )
                }),
            )
            .child(self.settings_subheading("Closing"))
            .child(
                self.settings_row(
                    "Confirm before closing",
                    "Ask before closing a tab or pane ends its processes. Off: one click closes."
                        .into(),
                    self.render_settings_toggle(
                        self.config.confirm_close,
                        SettingsAction::ToggleConfirmClose,
                        9,
                        true,
                        cx,
                    ),
                    compact,
                ),
            )
            .child(self.settings_subheading("Focus"))
            .child(self.render_focus_indicator_setting(10, compact, cx))
            .child(self.settings_subheading("Program status"))
            .children(
                [
                    (
                        "Tabs",
                        "Show what programs report on each tab: blocked, failed, working, done or idle.",
                        self.config.program_status.tabs,
                    ),
                    (
                        "Panes",
                        "Show it on each pane's top edge and floating title strip.",
                        self.config.program_status.panes,
                    ),
                    (
                        "Sidebar and palette",
                        "Start each tab's status line with it.",
                        self.config.program_status.sidebar,
                    ),
                ]
                .into_iter()
                .enumerate()
                .map(|(index, (label, description, enabled))| {
                    self.settings_row(
                        label,
                        description.into(),
                        self.render_settings_toggle(
                            enabled,
                            SettingsAction::ProgramStatus(index),
                            14 + index,
                            true,
                            cx,
                        ),
                        compact,
                    )
                }),
            )
            .into_any_element()
    }

    fn render_focus_indicator_setting(
        &self,
        first: usize,
        compact: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let base = self.settings_content_focus(0);
        let controls = div()
            .flex()
            .when(compact, |controls| controls.flex_col().items_start())
            .gap_2()
            .children(
                crate::theme::FocusIndicator::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(index, indicator)| {
                        let offset = first + index;
                        let focused = self.overlay_focus == base + offset;
                        div()
                            .relative()
                            .flex_none()
                            .child(self.settings_segment_button(
                                ("settings-focus-indicator", index),
                                indicator.label(),
                                self.config.focus_indicator == indicator,
                                focused,
                                cx.listener(move |this, _, window, cx| {
                                    this.overlay_focus = base + offset;
                                    this.activate_settings_action(
                                        SettingsAction::FocusIndicator(indicator),
                                        window,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            ))
                            .child(self.settings_focus_anchor(focused, cx))
                    }),
            )
            .into_any_element();
        self.settings_row(
            "Focus indicator",
            match self.config.focus_indicator {
                crate::theme::FocusIndicator::Marker => {
                    "A short accent bar on the focused pane; an outline flashes when focus moves."
                }
                crate::theme::FocusIndicator::Outline => {
                    "An accent outline around the focused pane."
                }
                crate::theme::FocusIndicator::Dim => "Other panes are dimmed.",
                crate::theme::FocusIndicator::None => {
                    "Only the cursor, which unfocused panes do not draw."
                }
            }
            .into(),
            controls,
            compact,
        )
    }

    fn render_density_setting(
        &self,
        first: usize,
        compact: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let base = self.settings_content_focus(0);
        let controls = div()
            .flex()
            .when(compact, |controls| controls.flex_col().items_start())
            .gap_2()
            .children(
                crate::theme::WorkspaceDensity::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(index, density)| {
                        let offset = first + index;
                        let focused = self.overlay_focus == base + offset;
                        div()
                            .relative()
                            .flex_none()
                            .child(self.settings_segment_button(
                                ("settings-density", index),
                                density.label(),
                                self.config.density == density,
                                focused,
                                cx.listener(move |this, _, window, cx| {
                                    this.overlay_focus = base + offset;
                                    this.activate_settings_action(
                                        SettingsAction::Density(density),
                                        window,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            ))
                            .child(self.settings_focus_anchor(focused, cx))
                    }),
            )
            .into_any_element();
        self.settings_row(
            "Pane density",
            match self.config.density {
                crate::theme::WorkspaceDensity::Comfy => {
                    "Rounded panes with gaps between them, in every window."
                }
                crate::theme::WorkspaceDensity::Compact => {
                    "Edge-to-edge panes for the most terminal space, in every window."
                }
            }
            .into(),
            controls,
            compact,
        )
    }

    /// Focus offsets follow `terminal_action`, then the shell prompt group's.
    fn render_terminal_settings(&self, compact: bool, cx: &Context<Self>) -> AnyElement {
        let zoom = div()
            .flex()
            .gap_2()
            .child(self.settings_control_button(
                "−",
                SettingsAction::Zoom(Command::ZoomOut),
                1,
                false,
                cx,
            ))
            .child(self.settings_control_button(
                "Reset",
                SettingsAction::Zoom(Command::ZoomReset),
                2,
                false,
                cx,
            ))
            .child(self.settings_control_button(
                "+",
                SettingsAction::Zoom(Command::ZoomIn),
                3,
                false,
                cx,
            ))
            .into_any_element();
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.settings_heading("Terminal"))
            .child(self.settings_row(
                "Terminal font",
                String::new(),
                self.render_font_select(cx),
                compact,
            ))
            .child(self.settings_row(
                "Zoom",
                format!(
                    "{:.0}% · {:.1}px · {:.2} line height",
                    self.zoom * 100.0,
                    self.font_settings.size,
                    self.font_settings.line_height
                ),
                zoom,
                compact,
            ))
            .child(self.settings_row(
                "Custom typography",
                "Set a custom font, size, or line height in the configuration file.".into(),
                self.settings_control_button(
                    "Edit configuration",
                    SettingsAction::OpenConfiguration,
                    4,
                    false,
                    cx,
                ),
                compact,
            ))
            .child(self.render_prompt_group(compact, cx))
            .into_any_element()
    }

    fn render_keyboard_settings(&self, compact: bool, cx: &Context<Self>) -> AnyElement {
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.settings_heading("Keyboard"))
            .child(self.settings_row(
                "Commands",
                format!(
                    "Search {} commands and their shortcuts.",
                    commands::REGISTRY.len()
                ),
                self.settings_control_button(
                    "Open palette",
                    SettingsAction::OpenPalette,
                    0,
                    false,
                    cx,
                ),
                compact,
            ))
            .child(self.settings_row(
                "Keybindings",
                "Custom shortcuts replace the platform defaults.".into(),
                self.settings_control_button(
                    "Edit keybindings",
                    SettingsAction::OpenConfiguration,
                    1,
                    false,
                    cx,
                ),
                compact,
            ))
            .into_any_element()
    }

    /// Pane or tab label of a shell this window shows.
    fn shell_label(&self, surface: &SurfaceId) -> Option<String> {
        let workspace = self.workspace.as_ref()?;
        for tab in workspace.sessions.iter().flat_map(|session| &session.tabs) {
            let mut leaves = Vec::new();
            collect_leaves(&tab.layout, &mut leaves);
            let Some(index) = leaves
                .iter()
                .position(|(_, id)| id.as_ref() == Some(surface))
            else {
                continue;
            };
            let custom = tab.label.trim();
            if leaves.len() == 1 && !custom.is_empty() {
                return Some(custom.to_owned());
            }
            return self
                .tab_panes(tab)
                .into_iter()
                .nth(index)
                .map(|pane| pane.title);
        }
        None
    }

    /// Every 200 ms: keeps the open Updates page live and the sidebar dot current.
    pub(super) fn poll_updates(&mut self, cx: &mut Context<Self>) {
        let page = matches!(self.overlay, Some(Overlay::Settings))
            && self.settings_section == SettingsSection::Updates;
        if page {
            if !self.updates_page_open {
                self.updates.refresh_review();
            }
            self.updates.acknowledge_ready();
        }
        self.updates_page_open = page;
        let dot = self.updates.shows_dot();
        if page || dot != self.update_dot {
            self.update_dot = dot;
            cx.notify();
        }
    }

    /// The update state, what the page shows for it, and its controls in focus order.
    fn update_page(
        &self,
    ) -> (
        crate::updates::UpdateSnapshot,
        crate::updates::UpdateView,
        Vec<UpdateControl>,
    ) {
        let snapshot = self.updates.snapshot();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let view = snapshot.view(now, |surface| self.shell_label(surface));
        let mut controls: Vec<_> = view
            .button
            .map(UpdateControl::Primary)
            .into_iter()
            .collect();
        controls.extend([
            UpdateControl::CheckForUpdates,
            UpdateControl::DownloadAutomatically,
        ]);
        if let Some(release) = snapshot.candidate() {
            if !crate::release_notes::of_release(&release.manifest)
                .details
                .is_empty()
            {
                controls.push(UpdateControl::CandidateMore);
            }
            controls.push(UpdateControl::CandidateNotes);
        }
        if crate::release_notes::current().is_some() {
            controls.extend([UpdateControl::WhatsNewMore, UpdateControl::ReleaseNotes]);
        }
        controls.push(UpdateControl::Advanced);
        if self.settings_update_advanced {
            if cfg!(windows) {
                controls.push(UpdateControl::Repair);
            }
            controls.push(UpdateControl::OpenLog);
        }
        (snapshot, view, controls)
    }

    fn activate_update_control(
        &mut self,
        control: UpdateControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::updates::{Preference, UpdateButton};
        match control {
            UpdateControl::Primary(button) => {
                let (snapshot, view, _) = self.update_page();
                // The state moved on since this button was drawn; the next frame shows it.
                if view.button != Some(button) {
                    return;
                }
                match button {
                    UpdateButton::CheckNow => self.updates.check(),
                    UpdateButton::Download => self.updates.download(),
                    UpdateButton::Cancel => self.updates.cancel(),
                    // Installing restarts Compi: unsaved edits in any window ask first.
                    UpdateButton::Restart => self.guard_unsaved(
                        super::unsaved::CloseScope::App,
                        super::unsaved::AfterUnsaved::Install(Vec::new()),
                        window,
                        cx,
                    ),
                    // The listed shells are exactly what this click consents to end.
                    UpdateButton::RestartEndShells => self.guard_unsaved(
                        super::unsaved::CloseScope::App,
                        super::unsaved::AfterUnsaved::Install(snapshot.ending_shells()),
                        window,
                        cx,
                    ),
                    UpdateButton::TryAgain => self.updates.retry(),
                    UpdateButton::DownloadSetup => {
                        if let Err(error) = open_web_url(crate::updates::SETUP_DOWNLOAD_URL) {
                            self.global_error = Some(error);
                        }
                    }
                }
            }
            UpdateControl::CheckForUpdates => {
                let enabled = self.updates.snapshot().preferences.check_for_updates;
                self.updates
                    .set_preference(self.config.clone(), Preference::CheckForUpdates(!enabled));
            }
            UpdateControl::DownloadAutomatically => {
                let enabled = self
                    .updates
                    .snapshot()
                    .preferences
                    .download_updates_automatically;
                self.updates.set_preference(
                    self.config.clone(),
                    Preference::DownloadAutomatically(!enabled),
                );
            }
            UpdateControl::CandidateMore => {
                self.settings_update_notes_expanded = !self.settings_update_notes_expanded
            }
            UpdateControl::CandidateNotes => {
                if let Some(release) = self.updates.snapshot().candidate() {
                    self.open_release_notes(&crate::release_notes::of_release(&release.manifest));
                }
            }
            UpdateControl::WhatsNewMore => {
                self.settings_whats_new_expanded = !self.settings_whats_new_expanded
            }
            UpdateControl::ReleaseNotes => {
                if let Some(notes) = crate::release_notes::current() {
                    self.open_release_notes(notes);
                }
            }
            UpdateControl::Advanced => {
                self.settings_update_advanced = !self.settings_update_advanced
            }
            UpdateControl::Repair => self.updates.repair(),
            UpdateControl::OpenLog => {
                if let Err(error) = open_update_log() {
                    self.global_error = Some(error);
                }
            }
        }
    }

    fn render_update_primary(
        &self,
        button: crate::updates::UpdateButton,
        offset: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        use crate::updates::UpdateButton;
        let focused = self.overlay_focus == self.settings_content_focus(offset);
        let tone = match button {
            UpdateButton::RestartEndShells => SettingsButtonTone::Destructive,
            UpdateButton::Cancel => SettingsButtonTone::Secondary,
            _ => SettingsButtonTone::Primary,
        };
        div()
            .relative()
            .flex_none()
            .child(self.settings_button(
                ("update-primary", offset),
                button.label(),
                focused,
                tone,
                cx.listener(move |this, _, window, cx| {
                    this.overlay_focus = this.settings_content_focus(offset);
                    this.activate_settings_action(
                        SettingsAction::Update(UpdateControl::Primary(button)),
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                    cx.notify();
                }),
            ))
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element()
    }

    fn render_update_disclosure(&self, offset: usize, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let focused = self.overlay_focus == self.settings_content_focus(offset);
        div()
            .id(("update-advanced", offset))
            .relative()
            .min_h(px(32.0))
            .px_2()
            .flex()
            .items_center()
            .rounded_sm()
            .border_1()
            .border_color(color(if focused {
                colors.accent
            } else {
                colors.surface
            }))
            .font_weight(FontWeight::MEDIUM)
            .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.overlay_focus = this.settings_content_focus(offset);
                this.activate_settings_action(
                    SettingsAction::Update(UpdateControl::Advanced),
                    window,
                    cx,
                );
                cx.stop_propagation();
                cx.notify();
            }))
            .child(if self.settings_update_advanced {
                "Advanced ▾"
            } else {
                "Advanced ▸"
            })
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element()
    }

    fn render_update_settings(&self, compact: bool, cx: &Context<Self>) -> AnyElement {
        let (snapshot, view, controls) = self.update_page();
        let colors = *self.colors();
        let offset = |control: UpdateControl| controls.iter().position(|item| *item == control);
        let control = |label: &'static str, control: UpdateControl| {
            offset(control).map(|offset| {
                self.settings_control_button(
                    label,
                    SettingsAction::Update(control),
                    offset,
                    false,
                    cx,
                )
            })
        };
        let toggle = |enabled: bool, control: UpdateControl| {
            let offset = offset(control).expect("toggles are always present");
            self.render_settings_toggle(enabled, SettingsAction::Update(control), offset, true, cx)
        };
        let header = div()
            .min_w_0()
            .min_h(px(32.0))
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("Compi {}", env!("CARGO_PKG_VERSION"))),
            )
            .children(view.button.and_then(|button| {
                offset(UpdateControl::Primary(button))
                    .map(|offset| self.render_update_primary(button, offset, cx))
            }));
        let status = div()
            .min_w_0()
            .text_color(color(modal_text_color(
                if view.failed {
                    colors.error
                } else {
                    colors.muted
                },
                &colors,
            )))
            .child(view.status);
        let mut summary = div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .pb_3()
            .border_b_1()
            .border_color(color(colors.border))
            .child(header)
            .child(status);
        if let Some(fraction) = view.progress {
            summary = summary.child(
                div()
                    .h(px(4.0))
                    .rounded_full()
                    .bg(color(colors.border))
                    .child(
                        div()
                            .h(px(4.0))
                            .rounded_full()
                            .bg(color(colors.accent))
                            .w(gpui::relative(fraction)),
                    ),
            );
        }
        let mut content = div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.settings_heading("Updates"))
            .child(summary)
            .child(self.settings_row(
                "Check for updates",
                String::new(),
                toggle(
                    snapshot.preferences.check_for_updates,
                    UpdateControl::CheckForUpdates,
                ),
                compact,
            ))
            .child(self.settings_row(
                "Download updates automatically",
                String::new(),
                toggle(
                    snapshot.preferences.download_updates_automatically,
                    UpdateControl::DownloadAutomatically,
                ),
                compact,
            ));
        if let Some(release) = snapshot.candidate() {
            let notes = crate::release_notes::of_release(&release.manifest);
            let expanded = self.settings_update_notes_expanded;
            let more = control(
                if expanded { "Less" } else { "More" },
                UpdateControl::CandidateMore,
            )
            .unwrap_or_else(|| div().into_any_element());
            let link = control("Release notes", UpdateControl::CandidateNotes)
                .unwrap_or_else(|| div().into_any_element());
            content = content
                .child(self.settings_subheading(&format!("What's new in {}", notes.version)))
                .child(self.render_release_notes_view(&notes, expanded, more, link));
        }
        if let Some(notes) = crate::release_notes::current() {
            let expanded = self.settings_whats_new_expanded;
            let more = control(
                if expanded { "Less" } else { "More" },
                UpdateControl::WhatsNewMore,
            )
            .unwrap_or_else(|| div().into_any_element());
            let link = control("Release notes", UpdateControl::ReleaseNotes)
                .unwrap_or_else(|| div().into_any_element());
            content = content
                .child(self.settings_subheading(&format!("What's new in {}", notes.version)))
                .child(self.render_release_notes_view(notes, expanded, more, link));
        }
        if let Some(offset) = offset(UpdateControl::Advanced) {
            content = content.child(
                div()
                    .flex()
                    .child(self.render_update_disclosure(offset, cx)),
            );
        }
        if self.settings_update_advanced {
            content = content.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(control("Repair Compi", UpdateControl::Repair))
                    .children(control("Open update log", UpdateControl::OpenLog)),
            );
        }
        content.into_any_element()
    }

    fn render_advanced_settings(&self, compact: bool, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let appearance = self.scoped_appearance();
        let live_processes = self.command_context(cx).live_surface_count;
        let theme_locked = self.config.provenance.theme == crate::config::ValueSource::CommandLine;
        let terminal_colors = div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().font_weight(FontWeight::MEDIUM).child(
                if appearance.terminal_theme_override {
                    self.resolve_theme(&appearance.terminal_theme)
                        .label()
                        .to_string()
                } else {
                    "Follow theme".into()
                },
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(self.settings_control_button(
                        "Follow theme",
                        SettingsAction::FollowTheme,
                        2,
                        false,
                        cx,
                    ))
                    .child(self.settings_control_button(
                        "Choose override",
                        SettingsAction::BrowseThemes(CatalogTarget::Terminal),
                        3,
                        false,
                        cx,
                    )),
            )
            .into_any_element();
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.settings_heading("Advanced"))
            .child(self.render_settings_scope(true, compact, cx))
            .child(self.settings_row(
                "Terminal colors",
                if theme_locked {
                    "Colors are fixed by --theme.".into()
                } else {
                    "Override the theme's terminal colors; colors set by programs stay unchanged."
                        .into()
                },
                terminal_colors,
                true,
            ))
            .child(self.settings_row(
                "Configuration",
                String::new(),
                self.settings_control_button(
                    "Open file",
                    SettingsAction::OpenConfiguration,
                    4,
                    false,
                    cx,
                ),
                compact,
            ))
            .child(self.settings_row(
                "Connection",
                "Reconnect without stopping running processes.".into(),
                self.settings_control_button("Reconnect", SettingsAction::Reconnect, 5, false, cx),
                compact,
            ))
            .child(self.settings_row(
                "Diagnostics",
                "Inspect connection and rendering details.".into(),
                self.settings_control_button(
                    "Open diagnostics",
                    SettingsAction::OpenDiagnostics,
                    6,
                    false,
                    cx,
                ),
                compact,
            ))
            .child(
                div()
                    .pt_3()
                    .border_t_1()
                    .border_color(color(colors.error).opacity(0.55))
                    .child(self.settings_row(
                        "Restart daemon",
                        format!(
                            "{} running terminal{}. Restarting stops all running terminals.",
                            live_processes,
                            if live_processes == 1 { "" } else { "s" }
                        ),
                        self.settings_control_button(
                            if live_processes == 0 {
                                "Restart"
                            } else {
                                "Review restart"
                            },
                            SettingsAction::RestartDaemon,
                            7,
                            live_processes > 0,
                            cx,
                        ),
                        compact,
                    )),
            )
            .into_any_element()
    }

    pub(super) fn render_appearance_controls(
        &self,
        full: bool,
        compact: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let mut appearance = self.scoped_appearance();
        if self.opacity_drag_origin.is_some() {
            appearance.terminal_opacity = self.terminal_opacity;
        }
        let focus = |offset: usize| {
            self.overlay_focus
                == if full {
                    self.settings_content_focus(offset)
                } else {
                    offset
                }
        };
        let theme_locked = self.config.provenance.theme == crate::config::ValueSource::CommandLine;
        let opacity_progress = ((appearance.terminal_opacity
            - crate::config::MIN_TERMINAL_OPACITY)
            / (crate::config::MAX_TERMINAL_OPACITY - crate::config::MIN_TERMINAL_OPACITY))
            .clamp(0.0, 1.0);
        let opacity_input = cx.entity();
        let opacity_slider_input = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                window.on_mouse_event({
                    let input = opacity_input.clone();
                    move |event: &MouseDownEvent, _, window, cx| {
                        if event.button != MouseButton::Left || !bounds.contains(&event.position) {
                            return;
                        }
                        let opacity = opacity_at_slider_position(event.position.x, bounds);
                        input.update(cx, |this, cx| {
                            this.overlay_focus = if full { SETTINGS_NAV_ITEMS + 4 } else { 4 };
                            this.preview_terminal_opacity(opacity, window);
                            cx.stop_propagation();
                            cx.notify();
                        });
                    }
                });
                window.on_mouse_event({
                    let input = opacity_input.clone();
                    move |event: &MouseMoveEvent, _, window, cx| {
                        if !event.dragging() {
                            return;
                        }
                        let opacity = opacity_at_slider_position(event.position.x, bounds);
                        input.update(cx, |this, cx| {
                            if this.opacity_drag_origin.is_some() {
                                this.preview_terminal_opacity(opacity, window);
                                cx.stop_propagation();
                                cx.notify();
                            }
                        });
                    }
                });
            },
        )
        .absolute()
        .inset_0();
        let theme_control = div()
            .relative()
            .min_w_0()
            .child(self.render_theme_catalog_entry_target(
                self.resolve_theme(&appearance.theme),
                CatalogTarget::Both,
                focus(2),
                cx,
            ))
            .child(self.settings_focus_anchor(focus(2), cx))
            .into_any_element();
        let opacity_control = div()
            .relative()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap_3()
                    .child("Opacity")
                    .child(format!("{:.0}%", appearance.terminal_opacity * 100.0)),
            )
            .child(
                div()
                    .id("opacity-slider")
                    .relative()
                    .mx_2()
                    .h(px(44.0))
                    .rounded_sm()
                    .border_1()
                    .border_color(color(if focus(4) {
                        colors.accent
                    } else {
                        colors.surface
                    }))
                    .cursor(gpui::CursorStyle::ResizeLeftRight)
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .top(px(20.0))
                            .h(px(4.0))
                            .rounded_full()
                            .bg(color(colors.border)),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top(px(20.0))
                            .w(gpui::relative(opacity_progress))
                            .h(px(4.0))
                            .rounded_full()
                            .bg(color(colors.accent)),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(gpui::relative(opacity_progress))
                            .top(px(14.0))
                            .ml(px(-7.0))
                            .size(px(14.0))
                            .rounded_full()
                            .border_1()
                            .border_color(color(colors.foreground))
                            .bg(color(colors.accent)),
                    )
                    .child(opacity_slider_input),
            )
            .child(self.settings_focus_anchor(focus(4), cx))
            .into_any_element();
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .when(full, |section| {
                section.child(self.settings_heading("Appearance"))
            })
            .child(self.render_settings_scope(full, compact, cx))
            .child(self.settings_row(
                "Theme",
                if theme_locked {
                    "Colors are fixed by --theme.".into()
                } else {
                    String::new()
                },
                theme_control,
                true,
            ))
            .when(appearance.terminal_theme_override, |section| {
                section.child(
                    div()
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .text_color(color(modal_text_color(colors.muted, &colors)))
                        .child("Terminal colors overridden. Change or reset in Advanced."),
                )
            })
            .child(self.settings_row(
                "Transparent background",
                String::new(),
                self.render_settings_toggle(
                    appearance.transparent_background,
                    SettingsAction::ToggleTransparency,
                    3,
                    full,
                    cx,
                ),
                compact,
            ))
            .when(appearance.transparent_background, |section| {
                section.child(opacity_control).child(self.settings_row(
                    "Blur background",
                    "Soften what's behind the window.".into(),
                    self.render_settings_toggle(
                        appearance.background_effect == BackgroundEffect::Blurred,
                        SettingsAction::ToggleBlur,
                        5,
                        full,
                        cx,
                    ),
                    compact,
                ))
            })
            .when(self.settings_scope == SettingsScope::Window, |section| {
                let offset = 4 + usize::from(appearance.transparent_background) * 2;
                let control = div()
                    .relative()
                    .flex_none()
                    .child(self.settings_action_button(
                        "reset-window-appearance",
                        "Use global defaults",
                        focus(offset),
                        false,
                        cx.listener(move |this, _, window, cx| {
                            this.overlay_focus = if full {
                                SETTINGS_NAV_ITEMS + offset
                            } else {
                                offset
                            };
                            this.activate_settings_action(
                                SettingsAction::ResetWindowAppearance,
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    ))
                    .child(self.settings_focus_anchor(focus(offset), cx))
                    .into_any_element();
                section.child(self.settings_row(
                    "Window overrides",
                    "Reset this window's appearance to the global defaults.".into(),
                    control,
                    compact,
                ))
            })
            .into_any_element()
    }

    pub(super) fn render_settings_overlay(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let (responsive_width, responsive_height) = overlay_viewport_size(window);
        let compact = responsive_width < 620.0;
        let panel_width = (responsive_width - 64.0).clamp(1.0, 760.0);
        let overlay_height = responsive_height.max(1.0);
        let panel_height = (overlay_height - 48.0).clamp(1.0, 560.0);
        let content_width = panel_width - if compact { 0.0 } else { 184.0 } - 32.0;
        let stacked = content_width < 480.0;
        let panel = div()
            .w(px(panel_width))
            .h(px(panel_height))
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(color(colors.border))
            .bg(color(colors.surface))
            .text_color(color(modal_text_color(colors.foreground, &colors)))
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_none()
                    .min_h(px(44.0))
                    .px_4()
                    .flex()
                    .justify_between()
                    .items_center()
                    .border_b_1()
                    .border_color(color(colors.border))
                    .child(
                        div()
                            .text_size(px(18.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Settings"),
                    )
                    .child(self.overlay_close_button("settings-dismiss", cx)),
            )
            .when(compact, |panel| {
                panel.child(self.render_settings_navigation(true, cx))
            })
            .child(
                div()
                    .relative()
                    .min_w_0()
                    .flex()
                    .min_h_0()
                    .flex_1()
                    .when(!compact, |body| {
                        body.child(self.render_settings_navigation(false, cx))
                    })
                    .child(
                        div()
                            .id("settings-scroll")
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.overlay_scroll)
                            .p_4()
                            .child(self.render_settings_content(stacked, cx)),
                    )
                    .child(self.settings_scroll_viewport(cx)),
            );
        div()
            .absolute()
            .top_0()
            .left_0()
            .w(px(responsive_width))
            .h(px(overlay_height))
            .p_4()
            .flex()
            .items_center()
            .justify_center()
            .bg(color(colors.background).opacity(MODAL_SCRIM_OPACITY))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.dismiss_overlay();
                    cx.notify();
                }),
            )
            .child(panel)
            .into_any_element()
    }

    /// Records the scrolled settings area, so focus moves can scroll a control into it.
    fn settings_scroll_viewport(&self, cx: &Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        canvas(
            |_, _, _| (),
            move |bounds, _, _, cx| {
                entity.update(cx, |this, _| this.settings_scroll_bounds = Some(bounds));
            },
        )
        .absolute()
        .inset_0()
    }

    /// Quick Appearance: the Appearance controls in a popover on `trigger`, with a way
    /// on to all settings. It holds the keyboard while open; see `quick_appearance_key`.
    pub(super) fn render_quick_appearance(
        &self,
        id: &'static str,
        trigger: AnyElement,
        anchor: Anchor,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let open = self.popup == Some(Popup::QuickAppearance);
        let popover = controls::popover(id, trigger, open)
            .anchor(anchor)
            .on_open_change(cx.listener(|this, open: &bool, _, cx| {
                if *open {
                    this.open_quick_appearance();
                } else {
                    this.set_popup(Popup::QuickAppearance, false);
                }
                cx.notify();
            }))
            .on_key(cx.listener(|this, key: &Keystroke, window, cx| {
                this.quick_appearance_key(key, window, cx);
            }));
        if !open {
            return popover.into_any_element();
        }
        let (_, viewport_height) = overlay_viewport_size(window);
        let last = self.appearance_action_count() - 1;
        popover
            .content(
                div()
                    .w(px(360.0))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Quick Appearance"),
                    )
                    .child(
                        div()
                            .relative()
                            .min_h_0()
                            .child(
                                div()
                                    .id("quick-appearance-scroll")
                                    .max_h(px((viewport_height - 160.0).max(160.0)))
                                    .overflow_y_scroll()
                                    .track_scroll(&self.overlay_scroll)
                                    .child(self.render_appearance_controls(false, false, cx)),
                            )
                            .child(self.settings_scroll_viewport(cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .child(self.settings_primary_button(
                                "open-full-settings",
                                "All settings…",
                                self.overlay_focus == last,
                                cx.listener(move |this, _, window, cx| {
                                    this.overlay_focus = last;
                                    this.activate_settings_action(
                                        SettingsAction::OpenSettings,
                                        window,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            )),
                    ),
            )
            .into_any_element()
    }
}

/// Settings → Interface, by focus offset.
fn interface_action(offset: usize) -> Option<SettingsAction> {
    match offset {
        0 => Some(SettingsAction::FontSelect(Popup::UiFont)),
        1..=2 => crate::theme::WorkspaceDensity::ALL
            .get(offset - 1)
            .copied()
            .map(SettingsAction::Density),
        3 => Some(SettingsAction::ResetSidebar),
        4 => Some(SettingsAction::ResetLayout),
        5..=8 => Some(SettingsAction::Metadata(offset - 5)),
        9 => Some(SettingsAction::ToggleConfirmClose),
        10..=13 => crate::theme::FocusIndicator::ALL
            .get(offset - 10)
            .copied()
            .map(SettingsAction::FocusIndicator),
        14..=16 => Some(SettingsAction::ProgramStatus(offset - 14)),
        _ => None,
    }
}

/// Settings → Terminal before the shell prompt group, by focus offset.
fn terminal_action(offset: usize) -> Option<SettingsAction> {
    match offset {
        0 => Some(SettingsAction::FontSelect(Popup::TerminalFont)),
        1 => Some(SettingsAction::Zoom(Command::ZoomOut)),
        2 => Some(SettingsAction::Zoom(Command::ZoomReset)),
        3 => Some(SettingsAction::Zoom(Command::ZoomIn)),
        4 => Some(SettingsAction::OpenConfiguration),
        _ => None,
    }
}

fn open_update_log() -> Result<(), String> {
    let path = crate::updates::update_log_path()
        .filter(|path| path.is_file())
        .ok_or("No update log yet.")?;
    #[cfg(windows)]
    let result = std::process::Command::new("notepad.exe").arg(&path).spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open")
        .arg("-e")
        .arg(&path)
        .spawn();
    result.map(|_| ()).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Interface and fixed Terminal focus stop has an action and none lies past
    /// the count, so Tab and the arrows visit each control once.
    #[test]
    fn every_settings_focus_stop_has_one_action() {
        assert!((0..INTERFACE_ACTIONS).all(|offset| interface_action(offset).is_some()));
        assert!(interface_action(INTERFACE_ACTIONS).is_none());
        assert!((0..PROMPT_FOCUS_OFFSET).all(|offset| terminal_action(offset).is_some()));
        assert!(terminal_action(PROMPT_FOCUS_OFFSET).is_none());
    }
}
