use super::catalog::CatalogTarget;
use super::*;

const SETTINGS_NAV_ITEMS: usize = SettingsSection::ALL.len();

#[derive(Clone, Copy)]
enum UpdateAction {
    Preference(crate::config::AutomaticUpdateChecks),
    Check,
    Download,
    Cancel,
    Review,
    Install,
    Defer,
    Retry,
    Reinstall,
    RestorePrior,
    RecoveryLog,
}

/// Content offsets 0.. of the Updates section; What's new controls follow.
const UPDATE_ACTIONS: [UpdateAction; 13] = [
    UpdateAction::Preference(crate::config::AutomaticUpdateChecks::Never),
    UpdateAction::Preference(crate::config::AutomaticUpdateChecks::OnLaunch),
    UpdateAction::Preference(crate::config::AutomaticUpdateChecks::Daily),
    UpdateAction::Check,
    UpdateAction::Download,
    UpdateAction::Cancel,
    UpdateAction::Review,
    UpdateAction::Install,
    UpdateAction::Defer,
    UpdateAction::Retry,
    UpdateAction::Reinstall,
    UpdateAction::RestorePrior,
    UpdateAction::RecoveryLog,
];
const WHATS_NEW_MORE_OFFSET: usize = UPDATE_ACTIONS.len();
const RELEASE_NOTES_OFFSET: usize = UPDATE_ACTIONS.len() + 1;

#[derive(Clone, Copy)]
enum SettingsAction {
    Scope(SettingsScope),
    BrowseThemes(CatalogTarget),
    ToggleTransparency,
    ToggleBlur,
    FollowTheme,
    ToggleFontPicker,
    Opacity(f32),
    ResetWindowAppearance,
    UiFont(UiFontPreset),
    TerminalFont(TerminalFontPreset),
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
    Update(UpdateAction),
    ToggleWhatsNew,
    OpenReleaseNotes,
    Prompt(super::prompt::PromptAction),
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

    fn font_picker_open(&self) -> bool {
        self.settings_font_picker == Some(self.settings_section)
            && matches!(
                self.settings_section,
                SettingsSection::Interface | SettingsSection::Terminal
            )
    }

    fn font_choice_count(&self) -> usize {
        if !self.font_picker_open() {
            return 0;
        }
        match self.settings_section {
            SettingsSection::Interface => UiFontPreset::ALL.len(),
            SettingsSection::Terminal => TerminalFontPreset::ALL.len(),
            _ => 0,
        }
    }

    fn settings_action_count(&self) -> usize {
        match self.settings_section {
            SettingsSection::Appearance => self.appearance_action_count(),
            SettingsSection::Interface => self.font_choice_count() + 3,
            SettingsSection::Terminal => self.prompt_offset_base() + self.prompt_actions().len(),
            SettingsSection::Keyboard => 2,
            SettingsSection::Performance => 3,
            SettingsSection::Updates => {
                UPDATE_ACTIONS.len() + crate::release_notes::current().map_or(0, |_| 2)
            }
            SettingsSection::Advanced => 8,
        }
    }

    fn settings_action(&self, offset: usize) -> Option<SettingsAction> {
        match self.settings_section {
            SettingsSection::Appearance => self.appearance_action(offset),
            SettingsSection::Interface | SettingsSection::Terminal => {
                if offset == 0 {
                    return Some(SettingsAction::ToggleFontPicker);
                }
                let choices = self.font_choice_count();
                if offset <= choices {
                    return match self.settings_section {
                        SettingsSection::Interface => UiFontPreset::ALL
                            .get(offset - 1)
                            .copied()
                            .map(SettingsAction::UiFont),
                        _ => TerminalFontPreset::ALL
                            .get(offset - 1)
                            .copied()
                            .map(SettingsAction::TerminalFont),
                    };
                }
                match (self.settings_section, offset - choices - 1) {
                    (SettingsSection::Interface, 0) => Some(SettingsAction::ResetSidebar),
                    (SettingsSection::Interface, 1) => Some(SettingsAction::ResetLayout),
                    (SettingsSection::Terminal, 0) => Some(SettingsAction::Zoom(Command::ZoomOut)),
                    (SettingsSection::Terminal, 1) => {
                        Some(SettingsAction::Zoom(Command::ZoomReset))
                    }
                    (SettingsSection::Terminal, 2) => Some(SettingsAction::Zoom(Command::ZoomIn)),
                    (SettingsSection::Terminal, 3) => Some(SettingsAction::OpenConfiguration),
                    (SettingsSection::Terminal, index) => self
                        .prompt_actions()
                        .get(index - 4)
                        .copied()
                        .map(SettingsAction::Prompt),
                    _ => None,
                }
            }
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
            SettingsSection::Updates => {
                let notes = crate::release_notes::current().is_some();
                match offset {
                    WHATS_NEW_MORE_OFFSET if notes => Some(SettingsAction::ToggleWhatsNew),
                    RELEASE_NOTES_OFFSET if notes => Some(SettingsAction::OpenReleaseNotes),
                    _ => UPDATE_ACTIONS
                        .get(offset)
                        .copied()
                        .map(SettingsAction::Update),
                }
            }
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
        if !matches!(
            self.overlay,
            Some(Overlay::Settings | Overlay::QuickAppearance)
        ) {
            return;
        }
        let base = if matches!(self.overlay, Some(Overlay::Settings)) {
            SETTINGS_NAV_ITEMS
        } else {
            0
        };
        let count = if base == 0 {
            self.appearance_action_count()
        } else {
            self.settings_action_count()
        };
        self.overlay_focus = self.overlay_focus.min(base + count.saturating_sub(1));
    }

    /// Terminal-section offset of the shell prompt group's first control.
    pub(super) fn prompt_offset_base(&self) -> usize {
        self.font_choice_count() + 5
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
            self.prompt_offset_base() + index,
            true,
            cx,
        )
    }

    fn select_settings_section(&mut self, section: SettingsSection) {
        self.settings_font_picker = None;
        self.settings_section = section;
        self.overlay_scroll.set_offset(point(px(0.0), px(0.0)));
        if section == SettingsSection::Terminal {
            self.enter_prompt_settings();
        }
    }

    fn toggle_settings_font_picker(&mut self) {
        self.settings_scroll_to_focus = true;
        if self.font_picker_open() {
            self.settings_font_picker = None;
            self.overlay_focus = SETTINGS_NAV_ITEMS;
            return;
        }
        self.settings_font_picker = Some(self.settings_section);
        let selected = match self.settings_section {
            SettingsSection::Interface => UiFontPreset::ALL
                .iter()
                .position(|preset| *preset == self.config.ui_font),
            SettingsSection::Terminal => TerminalFontPreset::ALL.iter().position(|preset| {
                self.config
                    .configured_font
                    .family
                    .eq_ignore_ascii_case(preset.family())
            }),
            _ => None,
        }
        .unwrap_or(0);
        self.overlay_focus = SETTINGS_NAV_ITEMS + selected + 1;
    }

    pub(super) fn handle_settings_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let full = matches!(self.overlay, Some(Overlay::Settings));
        let quick = matches!(self.overlay, Some(Overlay::QuickAppearance));
        if !full && !quick {
            return false;
        }
        if key.key == "escape" {
            if full
                && self.settings_section == SettingsSection::Terminal
                && self.close_prompt_dropdown()
            {
                cx.notify();
                return true;
            }
            if full && self.font_picker_open() {
                self.settings_font_picker = None;
                self.overlay_focus = SETTINGS_NAV_ITEMS;
                self.settings_scroll_to_focus = true;
            } else {
                self.settings_font_picker = None;
                self.dismiss_overlay();
            }
            cx.notify();
            return true;
        }
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
            return true;
        }
        if full
            && self.font_picker_open()
            && self.overlay_focus >= base
            && matches!(key.key.as_str(), "up" | "down" | "home" | "end")
        {
            let count = self.font_choice_count();
            let selected = offset.saturating_sub(1).min(count - 1);
            let next = match key.key.as_str() {
                "up" => (selected + count - 1) % count,
                "down" => (selected + 1) % count,
                "home" => 0,
                _ => count - 1,
            };
            self.overlay_focus = base + next + 1;
            self.settings_scroll_to_focus = true;
            cx.notify();
            return true;
        }
        match key.key.as_str() {
            "tab" => {
                if full && self.font_picker_open() {
                    let choices = self.font_choice_count();
                    self.settings_font_picker = None;
                    self.overlay_focus = if offset <= choices {
                        base
                    } else {
                        base + offset.saturating_sub(match self.settings_section {
                            SettingsSection::Interface => UiFontPreset::ALL.len(),
                            _ => TerminalFontPreset::ALL.len(),
                        })
                    };
                }
                let count = if full {
                    self.settings_action_count()
                } else {
                    self.appearance_action_count()
                };
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
                self.settings_font_picker = None;
                self.overlay_focus = self.settings_section as usize;
            }
            "up" | "down" if self.overlay_focus >= base => {
                let count = if full {
                    self.settings_action_count()
                } else {
                    self.appearance_action_count()
                };
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
            _ => return true,
        }
        self.normalize_settings_focus();
        self.settings_scroll_to_focus = true;
        cx.notify();
        true
    }

    fn appearance_action_count(&self) -> usize {
        4 + usize::from(self.scoped_appearance().transparent_background) * 2
            + usize::from(self.settings_scope == SettingsScope::Window)
            + usize::from(matches!(self.overlay, Some(Overlay::QuickAppearance)))
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
                if matches!(self.overlay, Some(Overlay::QuickAppearance))
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
            SettingsAction::Update(action) => {
                let snapshot = self.updates.snapshot();
                if let Some(reason) = update_action_reason(action, &snapshot) {
                    self.global_error = Some(reason);
                } else {
                    match action {
                        UpdateAction::Preference(preference) => {
                            self.updates.preference(self.config.clone(), preference)
                        }
                        UpdateAction::Check => self.updates.check(),
                        UpdateAction::Download => self.updates.download(),
                        UpdateAction::Cancel => self.updates.cancel(),
                        UpdateAction::Review => self.updates.review(),
                        UpdateAction::Install => self.updates.install(),
                        UpdateAction::Defer => self.updates.defer(),
                        UpdateAction::Reinstall => self.updates.reinstall(),
                        UpdateAction::RestorePrior => self.updates.restore_prior(),
                        UpdateAction::RecoveryLog => {
                            if let Some(path) = &snapshot.recovery_journal
                                && let Err(error) = open_local_path(path)
                            {
                                self.global_error = Some(error);
                            }
                        }
                        UpdateAction::Retry => {
                            if snapshot.recovery_available {
                                self.updates.reinstall();
                            } else if snapshot.prepared.is_some() {
                                self.updates.review();
                            } else if snapshot.release.is_some() {
                                self.updates.download();
                            } else {
                                self.updates.check();
                            }
                        }
                    }
                }
            }
            SettingsAction::Scope(scope) => self.settings_scope = scope,
            SettingsAction::ToggleWhatsNew => {
                self.settings_whats_new_expanded = !self.settings_whats_new_expanded
            }
            SettingsAction::OpenReleaseNotes => {
                if let Some(notes) = crate::release_notes::current() {
                    self.open_release_notes(notes);
                }
            }
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
            SettingsAction::ToggleFontPicker => self.toggle_settings_font_picker(),
            SettingsAction::Opacity(delta) => self.adjust_opacity(delta, window, cx),
            SettingsAction::ResetWindowAppearance => self.reset_window_appearance(window),
            SettingsAction::UiFont(preset) => {
                self.apply_ui_font(preset, window, cx);
                self.settings_font_picker = None;
                self.overlay_focus = SETTINGS_NAV_ITEMS;
            }
            SettingsAction::TerminalFont(preset) => {
                self.apply_terminal_font(preset, window, cx);
                self.settings_font_picker = None;
                self.overlay_focus = SETTINGS_NAV_ITEMS;
            }
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
            SettingsAction::OpenSettings => {
                self.settings_font_picker = None;
                self.open_overlay(Overlay::Settings, "");
            }
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
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(on_click)
            .child(Into::<SharedString>::into(label))
            .into_any_element()
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
            .tooltip(move |_, cx| {
                cx.new(move |_| HeaderTooltip {
                    title: "Close · Esc".into(),
                    reason: None,
                    colors,
                })
                .into()
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.settings_font_picker = None;
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

    fn render_font_selector(&self, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let focused = self.overlay_focus == self.settings_content_focus(0);
        let label = match self.settings_section {
            SettingsSection::Interface => self.config.ui_font.label().to_string(),
            _ => self.config.configured_font.family.clone(),
        };
        div()
            .id("settings-font-selector")
            .relative()
            .min_w_0()
            .w_full()
            .min_h(px(38.0))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .rounded_sm()
            .border_1()
            .border_color(color(if focused {
                colors.accent
            } else {
                colors.border
            }))
            .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
            .on_click(cx.listener(|this, _, _, cx| {
                this.toggle_settings_font_picker();
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(label),
            )
            .child(div().flex_none().child(if self.font_picker_open() {
                "▴"
            } else {
                "▾"
            }))
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element()
    }

    fn render_font_option(
        &self,
        index: usize,
        (label, family): (&'static str, &'static str),
        sample: &'static str,
        active: bool,
        action: SettingsAction,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let focused = self.overlay_focus == self.settings_content_focus(index + 1);
        div()
            .id(("settings-font-option", index))
            .relative()
            .min_w_0()
            .min_h(px(52.0))
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_3()
            .border_1()
            .border_color(color(if focused {
                colors.accent
            } else {
                colors.surface
            }))
            .bg(color(if active {
                blend_rgb(colors.surface, colors.accent, 0.08)
            } else {
                colors.surface
            }))
            .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.activate_settings_action(action, window, cx);
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .font_family(family)
                    .child(div().font_weight(FontWeight::MEDIUM).child(label))
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .text_color(color(modal_text_color(colors.muted, &colors)))
                            .child(sample),
                    ),
            )
            .child(
                div()
                    .w(px(16.0))
                    .flex_none()
                    .child(if active { "✓" } else { "" }),
            )
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

    fn render_interface_settings(&self, compact: bool, cx: &Context<Self>) -> AnyElement {
        let choices = self.font_choice_count();
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.settings_heading("Interface"))
            .child(self.settings_row(
                "Interface font",
                String::new(),
                self.render_font_selector(cx),
                true,
            ))
            .when(self.font_picker_open(), |section| {
                section.child(
                    div().flex().flex_col().children(
                        UiFontPreset::ALL
                            .into_iter()
                            .enumerate()
                            .map(|(index, preset)| {
                                self.render_font_option(
                                    index,
                                    (preset.label(), preset.family()),
                                    "Aa Bb 0123",
                                    self.config.ui_font == preset,
                                    SettingsAction::UiFont(preset),
                                    cx,
                                )
                            }),
                    ),
                )
            })
            .child(self.settings_row(
                "Sidebar width",
                format!("{:.0}px", self.sidebar_width),
                self.settings_control_button(
                    "Reset width",
                    SettingsAction::ResetSidebar,
                    choices + 1,
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
                    choices + 2,
                    false,
                    cx,
                ),
                compact,
            ))
            .into_any_element()
    }

    fn render_terminal_settings(&self, compact: bool, cx: &Context<Self>) -> AnyElement {
        let choices = self.font_choice_count();
        let zoom = div()
            .flex()
            .gap_2()
            .child(self.settings_control_button(
                "−",
                SettingsAction::Zoom(Command::ZoomOut),
                choices + 1,
                false,
                cx,
            ))
            .child(self.settings_control_button(
                "Reset",
                SettingsAction::Zoom(Command::ZoomReset),
                choices + 2,
                false,
                cx,
            ))
            .child(self.settings_control_button(
                "+",
                SettingsAction::Zoom(Command::ZoomIn),
                choices + 3,
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
                self.render_font_selector(cx),
                true,
            ))
            .when(self.font_picker_open(), |section| {
                section.child(
                    div().flex().flex_col().children(
                        TerminalFontPreset::ALL
                            .into_iter()
                            .enumerate()
                            .map(|(index, preset)| {
                                self.render_font_option(
                                    index,
                                    (preset.label(), preset.family()),
                                    "0O1l {} [] $ cargo",
                                    self.config
                                        .configured_font
                                        .family
                                        .eq_ignore_ascii_case(preset.family()),
                                    SettingsAction::TerminalFont(preset),
                                    cx,
                                )
                            }),
                    ),
                )
            })
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
                    choices + 4,
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
    fn render_update_button(
        &self,
        label: &'static str,
        action: UpdateAction,
        offset: usize,
        snapshot: &crate::updates::UpdateSnapshot,
        cx: &Context<Self>,
    ) -> AnyElement {
        let reason = update_action_reason(action, snapshot);
        let enabled = reason.is_none();
        let focused = self.overlay_focus == self.settings_content_focus(offset);
        let colors = *self.colors();
        let active = matches!(action, UpdateAction::Preference(value) if value == snapshot.preferences.automatic_checks);
        let primary = matches!(action, UpdateAction::Install);
        let destructive = primary
            && snapshot.daemons.iter().any(|daemon| {
                daemon.managed
                    && daemon
                        .status
                        .as_ref()
                        .is_ok_and(|status| snapshot.requires_daemon_restart(status))
            });
        let background = if active || primary {
            blend_rgb(colors.surface, colors.accent, 0.12)
        } else {
            blend_rgb(colors.surface, colors.foreground, 0.04)
        };
        div()
            .id(("update-action", offset))
            .relative()
            .min_h(px(32.0))
            .px_3()
            .flex_none()
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
            .font_weight(if active || primary {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::MEDIUM
            })
            .text_color(color(ui_text_color(
                if !enabled {
                    colors.muted
                } else if destructive {
                    colors.error
                } else {
                    colors.foreground
                },
                background,
            )))
            .when(enabled, |button| {
                button.hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
            })
            .tooltip(move |_, cx| {
                let reason = reason.clone();
                cx.new(move |_| HeaderTooltip {
                    title: label.into(),
                    reason,
                    colors,
                })
                .into()
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.overlay_focus = this.settings_content_focus(offset);
                if enabled {
                    this.activate_settings_action(SettingsAction::Update(action), window, cx);
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .child(label)
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element()
    }

    fn render_update_settings(&self, compact: bool, cx: &Context<Self>) -> AnyElement {
        let snapshot = self.updates.snapshot();
        let colors = *self.colors();
        let available = snapshot
            .available_release
            .as_ref()
            .or(snapshot.release.as_ref())
            .map(|release| release.manifest.version.as_str())
            .unwrap_or(if snapshot.last_check_unix.is_some() {
                "None identified"
            } else {
                "Not checked"
            });
        let automatic = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children(
                crate::config::AutomaticUpdateChecks::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(index, preference)| {
                        self.render_update_button(
                            preference.label(),
                            UpdateAction::Preference(preference),
                            index,
                            &snapshot,
                            cx,
                        )
                    }),
            )
            .into_any_element();
        let check_detail = match snapshot.last_check_unix {
            Some(timestamp) => {
                let elapsed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .saturating_sub(timestamp);
                format!(
                    "{} · {} minutes ago (Unix {})",
                    snapshot.last_check_result.as_deref().unwrap_or("Checked"),
                    elapsed / 60,
                    timestamp
                )
            }
            None => "No update check yet.".into(),
        };
        let actions = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(self.render_update_button("Download", UpdateAction::Download, 4, &snapshot, cx))
            .child(self.render_update_button("Cancel", UpdateAction::Cancel, 5, &snapshot, cx))
            .child(self.render_update_button(
                "Review affected work",
                UpdateAction::Review,
                6,
                &snapshot,
                cx,
            ))
            .child(self.render_update_button("Defer", UpdateAction::Defer, 8, &snapshot, cx))
            .child(self.render_update_button("Retry", UpdateAction::Retry, 9, &snapshot, cx));
        let mut content = div().min_w_0().flex().flex_col().gap_3()
            .child(self.settings_heading("Updates"))
            .child(self.settings_row("Client", format!("Current {} · Available {available}", env!("CARGO_PKG_VERSION")), self.render_update_button("Check now", UpdateAction::Check, 3, &snapshot, cx), compact))
            .child(div().text_size(px(UI_SMALL_TEXT_SIZE)).text_color(color(modal_text_color(colors.muted, &colors))).child(check_detail))
            .child(self.settings_row("Automatic checks", "Checks only. Downloads, installation, and restarts always require your action.".into(), automatic, true))
            .child(actions);
        if let Some(progress) = &snapshot.progress {
            let detail = match progress.total {
                Some(total) if total > 0 => format!(
                    "{:?} · {} / {} bytes · {:.0}%",
                    progress.phase,
                    progress.completed,
                    total,
                    (progress.completed as f64 / total as f64 * 100.0).min(100.0)
                ),
                _ => format!("{:?}", progress.phase),
            };
            content = content.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(detail)
                    .child(progress.message.clone()),
            );
            if let Some(total) = progress.total.filter(|total| *total > 0) {
                content = content.child(
                    div()
                        .h(px(4.0))
                        .rounded_full()
                        .bg(color(colors.border))
                        .child(div().h(px(4.0)).rounded_full().bg(color(colors.accent)).w(
                            gpui::relative(
                                (progress.completed as f32 / total as f32).clamp(0.0, 1.0),
                            ),
                        )),
                );
            }
        }
        if let Some(error) = &snapshot.error {
            content = content.child(
                div()
                    .text_color(color(modal_text_color(colors.error, &colors)))
                    .child(error.clone()),
            );
        }
        if snapshot.deferred {
            content = content
                .child("Deferred. Verified staged files are retained; terminals keep running.");
        }
        if let Some(prepared) = &snapshot.prepared {
            content = content.child(format!("Verified staged client {}. Install and restart applies this version; Download explicitly replaces it with the available release.", prepared.version));
        }
        if let Some(notes) = crate::release_notes::current() {
            let expanded = self.settings_whats_new_expanded;
            let more = self.settings_control_button(
                if expanded { "Less" } else { "More" },
                SettingsAction::ToggleWhatsNew,
                WHATS_NEW_MORE_OFFSET,
                false,
                cx,
            );
            let link = self.settings_control_button(
                "Release notes",
                SettingsAction::OpenReleaseNotes,
                RELEASE_NOTES_OFFSET,
                false,
                cx,
            );
            content = content
                .child(self.settings_subheading(&format!("What's new in {}", notes.version)))
                .child(self.render_release_notes_view(notes, expanded, more, link));
        }
        if !snapshot.release_notes.is_empty() {
            let candidate = snapshot
                .available_release
                .as_ref()
                .or(snapshot.release.as_ref())
                .map(|release| release.manifest.version.as_str());
            let label = match candidate {
                Some(version) => format!("{version} · Release notes"),
                None => "Release notes".to_owned(),
            };
            content = content.child(self.settings_subheading(&label)).child(
                div()
                    .min_w_0()
                    .child(SharedString::new(snapshot.release_notes.clone())),
            );
        }
        content = content.child(self.settings_subheading("Connected daemons"));
        if snapshot.daemons.is_empty() {
            content = content.child("Choose Review affected work to inspect local instances, detached terminals, remote targets, and all GUI hosts.");
        }
        for daemon in &snapshot.daemons {
            let detail = match &daemon.status {
                Ok(status) => format!(
                    "{} · instance {} · daemon {} · protocol {} · generation {} · revision {} · {} attached clients · {} live surfaces{}",
                    if daemon.target.is_remote() {
                        "Remote"
                    } else {
                        "Local"
                    },
                    status.instance.as_deref().unwrap_or("default"),
                    status.product_version,
                    status.protocol_version,
                    status.server_generation,
                    status.workspace_revision,
                    status.connected_clients.len(),
                    status.live_surfaces.len(),
                    if snapshot.requires_daemon_restart(status) {
                        " · daemon restart required"
                    } else {
                        " · keeps running"
                    },
                ),
                Err(error) => format!("Cannot inspect daemon: {error}"),
            };
            content = content.child(div().min_w_0().child(detail));
            if let Ok(status) = &daemon.status
                && snapshot.consent_reviewed
                && snapshot.requires_daemon_restart(status)
            {
                content = content.children(status.live_surfaces.iter().map(|surface| {
                    div().text_size(px(UI_SMALL_TEXT_SIZE)).child(format!(
                        "{} · lifetime {} · {:?}",
                        surface.surface_id, surface.process_lifetime_id, surface.status
                    ))
                }));
            }
        }
        content = content.child(div().text_size(px(UI_SMALL_TEXT_SIZE)).child(format!(
                "Affected GUI processes: {}",
                snapshot
                    .host_process_ids
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        let restart = if snapshot.daemons.iter().any(|daemon| {
            daemon.managed
                && daemon
                    .status
                    .as_ref()
                    .is_ok_and(|status| snapshot.requires_daemon_restart(status))
        }) {
            format!(
                "Installing restarts {} GUI hosts and stops the listed local daemons. Every listed live terminal ends and becomes Lost. It is not resumed automatically. Remote hosts are never updated. No operating-system restart is requested.",
                snapshot.host_process_ids.len()
            )
        } else {
            format!(
                "Installing restarts {} GUI hosts. Qualified daemons and terminal process lifetimes keep running. A newer daemon can be restarted deliberately later. No operating-system restart is requested.",
                snapshot.host_process_ids.len()
            )
        };
        content = content.child(self.settings_row(
            "Install and restart",
            restart,
            self.render_update_button(
                if snapshot.daemons.iter().any(|daemon| {
                    daemon.managed
                        && daemon
                            .status
                            .as_ref()
                            .is_ok_and(|status| snapshot.requires_daemon_restart(status))
                }) {
                    "Stop listed work and install"
                } else {
                    "Install and restart client"
                },
                UpdateAction::Install,
                7,
                &snapshot,
                cx,
            ),
            true,
        ));
        if let Some(reason) = snapshot.install_blocker() {
            content = content.child(
                div()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .text_color(color(modal_text_color(colors.muted, &colors)))
                    .child(reason),
            );
        }
        if snapshot.recovery_journal.is_some() {
            content = content.child(self.settings_subheading("Recovery")).child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(self.render_update_button(
                        "Reinstall verified package",
                        UpdateAction::Reinstall,
                        10,
                        &snapshot,
                        cx,
                    ))
                    .child(self.render_update_button(
                        "Restore prior selection",
                        UpdateAction::RestorePrior,
                        11,
                        &snapshot,
                        cx,
                    ))
                    .child(self.render_update_button(
                        "Open recovery journal",
                        UpdateAction::RecoveryLog,
                        12,
                        &snapshot,
                        cx,
                    )),
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
        let full = matches!(self.overlay, Some(Overlay::Settings));
        let (responsive_width, responsive_height) = overlay_viewport_size(window);
        let compact = responsive_width < 620.0;
        let panel_width = (responsive_width - 64.0).clamp(1.0, if full { 760.0 } else { 520.0 });
        let overlay_height = responsive_height.max(1.0);
        let panel_height = (overlay_height - 48.0)
            .max(1.0)
            .min(if full { 560.0 } else { 500.0 });
        let content_width = panel_width
            - if full && !compact { 184.0 } else { 0.0 }
            - if full { 32.0 } else { 24.0 };
        let stacked = content_width < 480.0;
        let scroll_entity = cx.entity();
        let scroll_viewport = canvas(
            |_, _, _| (),
            move |bounds, _, _, cx| {
                scroll_entity.update(cx, |this, _| this.settings_scroll_bounds = Some(bounds));
            },
        )
        .absolute()
        .inset_0();
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
                            .child(if full { "Settings" } else { "Quick Appearance" }),
                    )
                    .child(self.overlay_close_button("settings-dismiss", cx)),
            )
            .when(full && compact, |panel| {
                panel.child(self.render_settings_navigation(true, cx))
            })
            .child(
                div()
                    .relative()
                    .min_w_0()
                    .flex()
                    .min_h_0()
                    .flex_1()
                    .when(full && !compact, |body| {
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
                            .when(full, |content| content.p_4())
                            .when(!full, |content| content.p_3())
                            .child(if full {
                                self.render_settings_content(stacked, cx)
                            } else {
                                self.render_appearance_controls(false, stacked, cx)
                            }),
                    )
                    .child(scroll_viewport),
            )
            .when(!full, |panel| {
                panel.child(
                    div()
                        .flex_none()
                        .min_h(px(44.0))
                        .px_4()
                        .flex()
                        .items_center()
                        .justify_end()
                        .border_t_1()
                        .border_color(color(colors.border))
                        .child(self.settings_primary_button(
                            "open-full-settings",
                            "Open full settings",
                            self.overlay_focus == self.appearance_action_count() - 1,
                            cx.listener(|this, _, window, cx| {
                                this.activate_settings_action(
                                    SettingsAction::OpenSettings,
                                    window,
                                    cx,
                                );
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        )),
                )
            });
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
                    this.settings_font_picker = None;
                    this.dismiss_overlay();
                    cx.notify();
                }),
            )
            .child(panel)
            .into_any_element()
    }
}

fn update_action_reason(
    action: UpdateAction,
    snapshot: &crate::updates::UpdateSnapshot,
) -> Option<String> {
    if matches!(action, UpdateAction::Cancel) {
        return if snapshot.busy
            && !matches!(
                snapshot.progress.as_ref().map(|progress| progress.phase),
                Some(
                    compi_update::UpdatePhase::Activating | compi_update::UpdatePhase::Relaunching
                )
            ) {
            None
        } else {
            Some("No cancellable update operation is running.".into())
        };
    }
    if snapshot.busy {
        return Some("Wait for this update operation, or cancel it first.".into());
    }
    match action {
        UpdateAction::Review if snapshot.prepared.is_none() => {
            Some("Download and verify before reviewing installation.".into())
        }
        UpdateAction::Download if snapshot.release.is_none() => {
            Some("Check for an available release first.".into())
        }
        UpdateAction::Install if !snapshot.consent_reviewed => {
            Some("Review affected work before installing.".into())
        }
        UpdateAction::Install => snapshot.install_blocker(),
        UpdateAction::Defer if snapshot.prepared.is_none() => {
            Some("No verified download is staged.".into())
        }
        UpdateAction::Retry if snapshot.error.is_none() => {
            Some("There is no failed update operation to retry.".into())
        }
        UpdateAction::Reinstall | UpdateAction::RestorePrior if !snapshot.recovery_available => {
            Some("No rolled-back update requires recovery.".into())
        }
        UpdateAction::RecoveryLog if snapshot.recovery_journal.is_none() => {
            Some("No update recovery journal is available.".into())
        }
        _ => None,
    }
}
