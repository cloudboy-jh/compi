use super::*;

const SETTINGS_NAV_ITEMS: usize = 6;

#[derive(Clone, Copy)]
enum SettingsAction {
    Scope(SettingsScope),
    BrowseThemes,
    Background(BackgroundEffect),
    Opacity(f32),
    ResetWindowAppearance,
    UiFont(UiFontPreset),
    TerminalFont(TerminalFontPreset),
    ResetSidebar,
    ResetLayout,
    Zoom(Command),
    OpenConfiguration,
    OpenPalette,
    ToggleFps,
    RebuildRenderer,
    CopyPerformance,
    Reconnect,
    OpenDiagnostics,
    RestartDaemon,
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

    fn settings_action_count(&self) -> usize {
        match self.settings_section {
            SettingsSection::Appearance => {
                if self.settings_scope == SettingsScope::Window {
                    7
                } else {
                    6
                }
            }
            SettingsSection::Interface => UiFontPreset::ALL.len() + 2,
            SettingsSection::Terminal => TerminalFontPreset::ALL.len() + 4,
            SettingsSection::Keyboard => 2,
            SettingsSection::Performance => 3,
            SettingsSection::Advanced => 4,
        }
    }

    fn settings_action(&self, offset: usize) -> Option<SettingsAction> {
        match self.settings_section {
            SettingsSection::Appearance => match offset {
                0 => Some(SettingsAction::Scope(SettingsScope::Global)),
                1 => Some(SettingsAction::Scope(SettingsScope::Window)),
                2 => Some(SettingsAction::BrowseThemes),
                3 => Some(SettingsAction::Background(BackgroundEffect::Clear)),
                4 => Some(SettingsAction::Background(BackgroundEffect::Blurred)),
                5 => Some(SettingsAction::Opacity(0.05)),
                6 if self.settings_scope == SettingsScope::Window => {
                    Some(SettingsAction::ResetWindowAppearance)
                }
                _ => None,
            },
            SettingsSection::Interface => UiFontPreset::ALL
                .get(offset)
                .copied()
                .map(SettingsAction::UiFont)
                .or_else(|| match offset - UiFontPreset::ALL.len() {
                    0 => Some(SettingsAction::ResetSidebar),
                    1 => Some(SettingsAction::ResetLayout),
                    _ => None,
                }),
            SettingsSection::Terminal => TerminalFontPreset::ALL
                .get(offset)
                .copied()
                .map(SettingsAction::TerminalFont)
                .or_else(|| match offset - TerminalFontPreset::ALL.len() {
                    0 => Some(SettingsAction::Zoom(Command::ZoomOut)),
                    1 => Some(SettingsAction::Zoom(Command::ZoomReset)),
                    2 => Some(SettingsAction::Zoom(Command::ZoomIn)),
                    3 => Some(SettingsAction::OpenConfiguration),
                    _ => None,
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
            SettingsSection::Advanced => match offset {
                0 => Some(SettingsAction::OpenConfiguration),
                1 => Some(SettingsAction::Reconnect),
                2 => Some(SettingsAction::OpenDiagnostics),
                3 => Some(SettingsAction::RestartDaemon),
                _ => None,
            },
        }
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
            self.dismiss_overlay();
            cx.notify();
            return true;
        }
        let base = if full { SETTINGS_NAV_ITEMS } else { 0 };
        let action_count = if full {
            self.settings_action_count()
        } else if self.settings_scope == SettingsScope::Window {
            7
        } else {
            6
        };
        let total = base + action_count;
        match key.key.as_str() {
            "tab" => {
                if total > 0 {
                    self.overlay_focus = if key.modifiers.shift {
                        (self.overlay_focus + total - 1) % total
                    } else {
                        (self.overlay_focus + 1) % total
                    };
                }
            }
            "up" | "down" if full && self.overlay_focus < SETTINGS_NAV_ITEMS => {
                let current = self.overlay_focus;
                let next = if key.key == "up" {
                    (current + SETTINGS_NAV_ITEMS - 1) % SETTINGS_NAV_ITEMS
                } else {
                    (current + 1) % SETTINGS_NAV_ITEMS
                };
                self.overlay_focus = next;
                self.settings_section = SettingsSection::ALL[next];
            }
            "right" if full && self.overlay_focus < SETTINGS_NAV_ITEMS => {
                self.overlay_focus = SETTINGS_NAV_ITEMS;
            }
            "left" if full && self.overlay_focus >= SETTINGS_NAV_ITEMS => {
                self.overlay_focus = self.settings_section as usize;
            }
            "enter" | "space" => {
                if full && self.overlay_focus < SETTINGS_NAV_ITEMS {
                    self.settings_section = SettingsSection::ALL[self.overlay_focus];
                } else {
                    let offset = self.overlay_focus.saturating_sub(base);
                    let action = if full {
                        self.settings_action(offset)
                    } else {
                        self.appearance_action(offset)
                    };
                    if let Some(action) = action {
                        self.activate_settings_action(action, window, cx);
                    }
                }
            }
            "left" | "right" => {
                let offset = self.overlay_focus.saturating_sub(base);
                if matches!(
                    if full {
                        self.settings_action(offset)
                    } else {
                        self.appearance_action(offset)
                    },
                    Some(SettingsAction::Opacity(_))
                ) {
                    self.adjust_opacity(if key.key == "left" { -0.05 } else { 0.05 }, window, cx);
                }
            }
            _ => return true,
        }
        cx.notify();
        true
    }

    fn appearance_action(&self, offset: usize) -> Option<SettingsAction> {
        match offset {
            0 => Some(SettingsAction::Scope(SettingsScope::Global)),
            1 => Some(SettingsAction::Scope(SettingsScope::Window)),
            2 => Some(SettingsAction::BrowseThemes),
            3 => Some(SettingsAction::Background(BackgroundEffect::Clear)),
            4 => Some(SettingsAction::Background(BackgroundEffect::Blurred)),
            5 => Some(SettingsAction::Opacity(0.05)),
            6 if self.settings_scope == SettingsScope::Window => {
                Some(SettingsAction::ResetWindowAppearance)
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
            SettingsAction::Scope(scope) => self.settings_scope = scope,
            SettingsAction::BrowseThemes => self.open_theme_catalog(self.settings_scope),
            SettingsAction::Background(effect) => {
                let mut appearance = self.scoped_appearance();
                appearance.background_effect = effect;
                self.apply_scoped_appearance(appearance, window, cx);
            }
            SettingsAction::Opacity(delta) => self.adjust_opacity(delta, window, cx),
            SettingsAction::ResetWindowAppearance => self.reset_window_appearance(window),
            SettingsAction::UiFont(preset) => self.apply_ui_font(preset, window, cx),
            SettingsAction::TerminalFont(preset) => self.apply_terminal_font(preset, window, cx),
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
        }
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

    pub(super) fn settings_heading(
        &self,
        title: &'static str,
        description: &'static str,
    ) -> AnyElement {
        let colors = self.colors();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(px(18.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .max_w(px(620.0))
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color(modal_text_color(colors.muted, colors)))
                    .child(description),
            )
            .into_any_element()
    }

    pub(super) fn settings_subheading(&self, label: &'static str) -> AnyElement {
        let colors = self.colors();
        div()
            .pb_1()
            .text_size(px(UI_MICRO_TEXT_SIZE))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(color(modal_text_color(colors.muted, colors)))
            .child(label.to_uppercase())
            .into_any_element()
    }

    fn settings_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: &'static str,
        focused: bool,
        tone: SettingsButtonTone,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let colors = self.colors();
        let (background, hover, foreground) = match tone {
            SettingsButtonTone::Secondary => (
                blend_rgb(colors.surface, colors.foreground, 0.08),
                blend_rgb(colors.surface, colors.foreground, 0.13),
                colors.foreground,
            ),
            SettingsButtonTone::Primary => (
                blend_rgb(colors.surface, colors.accent, 0.34),
                blend_rgb(colors.surface, colors.accent, 0.44),
                colors.foreground,
            ),
            SettingsButtonTone::Destructive => (
                blend_rgb(colors.surface, colors.error, 0.14),
                blend_rgb(colors.surface, colors.error, 0.22),
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
            .min_h(px(32.0))
            .px_3()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
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
            .child(label)
            .into_any_element()
    }

    pub(super) fn settings_action_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: &'static str,
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
        label: &'static str,
        focused: bool,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        self.settings_button(id, label, focused, SettingsButtonTone::Primary, on_click)
    }

    pub(super) fn settings_segment_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: &'static str,
        active: bool,
        focused: bool,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let colors = self.colors();
        let group = blend_rgb(colors.surface, colors.foreground, 0.06);
        let selected = blend_rgb(colors.surface, colors.accent, 0.19);
        let background = if active { selected } else { group };
        div()
            .id(id)
            .min_h(px(32.0))
            .px_2()
            .rounded_sm()
            .border_1()
            .border_color(color(if focused { colors.accent } else { background }))
            .bg(color(background))
            .flex()
            .items_center()
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
            .child(label)
            .into_any_element()
    }

    pub(super) fn overlay_close_button(
        &self,
        id: impl Into<gpui::ElementId>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = self.colors();
        let background = blend_rgb(colors.surface, colors.foreground, 0.07);
        let hover = blend_rgb(colors.surface, colors.foreground, 0.13);
        div()
            .id(id)
            .size(px(36.0))
            .rounded_md()
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
        let colors = self.colors();
        let items = SettingsSection::ALL
            .into_iter()
            .enumerate()
            .map(|(index, section)| {
                let active = self.settings_section == section;
                let focused = self.overlay_focus == index;
                let active_background = blend_rgb(colors.surface, colors.accent, 0.16);
                let background = if active {
                    active_background
                } else {
                    colors.surface
                };
                div()
                    .id(("settings-nav", index))
                    .min_h(px(36.0))
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
                        colors,
                    )))
                    .font_weight(if active {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    })
                    .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_section = section;
                        this.overlay_focus = index;
                        this.overlay_scroll.set_offset(point(px(0.0), px(0.0)));
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

    fn render_settings_content(&self, cx: &Context<Self>) -> AnyElement {
        match self.settings_section {
            SettingsSection::Appearance => self.render_appearance_controls(true, cx),
            SettingsSection::Interface => self.render_interface_settings(cx),
            SettingsSection::Terminal => self.render_terminal_settings(cx),
            SettingsSection::Keyboard => self.render_keyboard_settings(cx),
            SettingsSection::Performance => self.render_performance_section(cx),
            SettingsSection::Advanced => self.render_advanced_settings(cx),
        }
    }

    fn render_interface_settings(&self, cx: &Context<Self>) -> AnyElement {
        let colors = self.colors();
        let selected_background = blend_rgb(colors.surface, colors.accent, 0.14);
        let fonts = UiFontPreset::ALL
            .into_iter()
            .enumerate()
            .map(|(index, preset)| {
                let active = self.config.ui_font == preset;
                let focused = self.overlay_focus == self.settings_content_focus(index);
                let background = if active {
                    selected_background
                } else {
                    colors.surface
                };
                div()
                    .id(("settings-ui-font", index))
                    .min_h(px(58.0))
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .rounded_sm()
                    .border_1()
                    .border_color(color(if focused {
                        colors.accent
                    } else if active {
                        blend_rgb(colors.border, colors.accent, 0.35)
                    } else {
                        colors.border
                    }))
                    .bg(color(background))
                    .font_family(preset.family())
                    .hover(move |style| {
                        style
                            .bg(color(if active {
                                selected_background
                            } else {
                                colors.surface_hover
                            }))
                            .cursor_pointer()
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.overlay_focus = this.settings_content_focus(index);
                        this.apply_ui_font(preset, window, cx);
                        cx.stop_propagation();
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(preset.label()),
                            )
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.muted, colors)))
                                    .child(format!("{} · Aa Bb 0123", preset.description())),
                            ),
                    )
                    .when(active, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .bg(color(blend_rgb(background, colors.accent, 0.18)))
                                .text_size(px(UI_MICRO_TEXT_SIZE))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(color(modal_text_color(colors.foreground, colors)))
                                .child("Selected"),
                        )
                    })
            });
        let reset_offset = UiFontPreset::ALL.len();
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(self.settings_heading(
                "Interface",
                "Choose application typography independently from the terminal grid.",
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(self.settings_subheading("Interface font"))
                    .children(fonts)
                    .child(
                        div()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(color(modal_text_color(colors.muted, colors)))
                            .child("Bundled families are licensed under the SIL Open Font License 1.1."),
                    ),
            )
            .child(
                div()
                    .min_h(px(44.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .border_b_1()
                    .border_color(color(colors.border))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child("Workspace sidebar")
                            .child(
                                div()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.muted, colors)))
                                    .child(format!("Current width: {:.0}px", self.sidebar_width)),
                            ),
                    )
                    .child(self.settings_action_button(
                        "settings-reset-sidebar",
                        "Reset width",
                        self.overlay_focus == self.settings_content_focus(reset_offset),
                        false,
                        cx.listener(|this, _, window, cx| {
                            this.execute(Command::ResetSidebarWidth, window, cx);
                            cx.stop_propagation();
                        }),
                    )),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .child(self.settings_action_button(
                        "settings-reset-layout",
                        "Reset client layout",
                        self.overlay_focus == self.settings_content_focus(reset_offset + 1),
                        false,
                        cx.listener(|this, _, window, cx| {
                            this.execute(Command::ResetClientLayout, window, cx);
                            cx.stop_propagation();
                        }),
                    )),
            )
            .into_any_element()
    }

    fn render_terminal_settings(&self, cx: &Context<Self>) -> AnyElement {
        let colors = self.colors();
        let selected_background = blend_rgb(colors.surface, colors.accent, 0.14);
        let fonts = TerminalFontPreset::ALL
            .into_iter()
            .enumerate()
            .map(|(index, preset)| {
                let active = self
                    .config
                    .configured_font
                    .family
                    .eq_ignore_ascii_case(preset.family());
                let focused = self.overlay_focus == self.settings_content_focus(index);
                let background = if active {
                    selected_background
                } else {
                    colors.surface
                };
                div()
                    .id(("settings-terminal-font", index))
                    .min_h(px(58.0))
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .rounded_sm()
                    .border_1()
                    .border_color(color(if focused {
                        colors.accent
                    } else if active {
                        blend_rgb(colors.border, colors.accent, 0.35)
                    } else {
                        colors.border
                    }))
                    .bg(color(background))
                    .font_family(preset.family())
                    .hover(move |style| {
                        style
                            .bg(color(if active {
                                selected_background
                            } else {
                                colors.surface_hover
                            }))
                            .cursor_pointer()
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.overlay_focus = this.settings_content_focus(index);
                        this.apply_terminal_font(preset, window, cx);
                        cx.stop_propagation();
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(preset.label()),
                            )
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.muted, colors)))
                                    .child(format!(
                                        "{} · $ cargo test  0O1l {{}} []",
                                        preset.description()
                                    )),
                            ),
                    )
                    .when(active, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .bg(color(blend_rgb(background, colors.accent, 0.18)))
                                .text_size(px(UI_MICRO_TEXT_SIZE))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(color(modal_text_color(colors.foreground, colors)))
                                .child("Selected"),
                        )
                    })
            });
        let zoom_offset = TerminalFontPreset::ALL.len();
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(self.settings_heading(
                "Terminal",
                "Choose fixed-cell typography independently from the application interface.",
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(self.settings_subheading("Terminal font"))
                    .children(fonts)
                    .child(
                        div()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(color(modal_text_color(colors.muted, colors)))
                            .child(
                                "Bundled families use the SIL Open Font License 1.1. Custom font.family values remain supported.",
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(self.settings_subheading("Typography"))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.font_settings.family.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .text_color(color(modal_text_color(colors.muted, colors)))
                            .child(format!(
                                "{:.1}px · {:.2} line height · {:.0}% zoom",
                                self.font_settings.size,
                                self.font_settings.line_height,
                                self.zoom * 100.0
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(self.settings_action_button(
                        "settings-zoom-out",
                        "Zoom out",
                        self.overlay_focus == self.settings_content_focus(zoom_offset),
                        false,
                        cx.listener(|this, _, window, cx| {
                            this.execute(Command::ZoomOut, window, cx);
                            cx.stop_propagation();
                        }),
                    ))
                    .child(self.settings_action_button(
                        "settings-zoom-reset",
                        "Reset zoom",
                        self.overlay_focus == self.settings_content_focus(zoom_offset + 1),
                        false,
                        cx.listener(|this, _, window, cx| {
                            this.execute(Command::ZoomReset, window, cx);
                            cx.stop_propagation();
                        }),
                    ))
                    .child(self.settings_action_button(
                        "settings-zoom-in",
                        "Zoom in",
                        self.overlay_focus == self.settings_content_focus(zoom_offset + 2),
                        false,
                        cx.listener(|this, _, window, cx| {
                            this.execute(Command::ZoomIn, window, cx);
                            cx.stop_propagation();
                        }),
                    )),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .child(self.settings_action_button(
                        "settings-terminal-config",
                        "Edit terminal configuration",
                        self.overlay_focus == self.settings_content_focus(zoom_offset + 3),
                        false,
                        cx.listener(|this, _, window, cx| {
                            this.execute(Command::OpenConfiguration, window, cx);
                            cx.stop_propagation();
                        }),
                    )),
            )
            .into_any_element()
    }

    fn render_keyboard_settings(&self, cx: &Context<Self>) -> AnyElement {
        let colors = self.colors();
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(self.settings_heading(
                "Keyboard",
                "Compi commands remain searchable and TOML keybindings override platform defaults.",
            ))
            .child(
                div()
                    .min_h(px(44.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(color(colors.border))
                    .child("Available commands")
                    .child(format!("{}", commands::REGISTRY.len())),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(self.settings_action_button(
                        "settings-open-palette",
                        "Open command palette",
                        self.overlay_focus == self.settings_content_focus(0),
                        false,
                        cx.listener(|this, _, window, cx| {
                            this.execute(Command::OpenPalette, window, cx);
                            cx.stop_propagation();
                        }),
                    ))
                    .child(self.settings_action_button(
                        "settings-keyboard-config",
                        "Edit keybindings",
                        self.overlay_focus == self.settings_content_focus(1),
                        false,
                        cx.listener(|this, _, window, cx| {
                            this.execute(Command::OpenConfiguration, window, cx);
                            cx.stop_propagation();
                        }),
                    )),
            )
            .into_any_element()
    }

    fn render_advanced_settings(&self, cx: &Context<Self>) -> AnyElement {
        let colors = self.colors();
        let live_surfaces = self.command_context(cx).live_surface_count;
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(self.settings_heading(
                "Advanced",
                "Configuration, connection recovery, diagnostics, and daemon lifecycle.",
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(self.settings_subheading("Configuration"))
                    .child(self.settings_action_button(
                        "settings-open-config",
                        "Open configuration file",
                        self.overlay_focus == self.settings_content_focus(0),
                        false,
                        cx.listener(|this, _, window, cx| {
                            this.execute(Command::OpenConfiguration, window, cx);
                            cx.stop_propagation();
                        }),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(self.settings_subheading("Recovery"))
                    .child(
                        div().text_size(px(UI_SMALL_TEXT_SIZE)).text_color(color(modal_text_color(colors.muted, colors)))
                            .child("Reconnect reloads terminal views without ending daemon-owned processes."),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(self.settings_action_button(
                                "settings-reconnect",
                                "Reconnect window",
                                self.overlay_focus == self.settings_content_focus(1),
                                false,
                                cx.listener(|this, _, window, cx| {
                                    this.execute(Command::Reconnect, window, cx);
                                    cx.stop_propagation();
                                }),
                            ))
                            .child(self.settings_action_button(
                                "settings-open-diagnostics",
                                "Open diagnostics",
                                self.overlay_focus == self.settings_content_focus(2),
                                false,
                                cx.listener(|this, _, window, cx| {
                                    this.execute(Command::OpenDiagnostics, window, cx);
                                    cx.stop_propagation();
                                }),
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .pt_3()
                    .border_t_1()
                    .border_color(color(colors.error).opacity(0.55))
                    .child(self.settings_subheading("Danger zone"))
                    .child(
                        div().text_size(px(UI_SMALL_TEXT_SIZE)).text_color(color(modal_text_color(colors.muted, colors)))
                            .child(format!(
                                "{} live surface{}. Restarting the daemon ends all live process trees.",
                                live_surfaces,
                                if live_surfaces == 1 { "" } else { "s" }
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .child(self.settings_action_button(
                                "settings-restart-daemon",
                                if live_surfaces == 0 {
                                    "Restart daemon"
                                } else {
                                    "Review and restart daemon"
                                },
                                self.overlay_focus == self.settings_content_focus(3),
                                live_surfaces > 0,
                                cx.listener(|this, _, window, cx| {
                                    this.execute(Command::RestartDaemon, window, cx);
                                    cx.stop_propagation();
                                }),
                            )),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_appearance_controls(&self, full: bool, cx: &Context<Self>) -> AnyElement {
        let colors = self.colors();
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
        let theme_locked = self.settings_scope == SettingsScope::Window
            && self.config.provenance.theme == crate::config::ValueSource::CommandLine;
        let scopes = [
            (SettingsScope::Global, "Global defaults"),
            (SettingsScope::Window, "This window"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (scope, label))| {
            self.settings_segment_button(
                ("settings-scope", index),
                label,
                self.settings_scope == scope,
                focus(index),
                cx.listener(move |this, _, _, cx| {
                    this.settings_scope = scope;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
        });
        let effects = BackgroundEffect::ALL
            .into_iter()
            .enumerate()
            .map(|(index, effect)| {
                self.settings_segment_button(
                    ("background-effect", index),
                    effect.label(),
                    appearance.background_effect == effect,
                    focus(3 + index),
                    cx.listener(move |this, _, window, cx| {
                        let mut next = this.scoped_appearance();
                        next.background_effect = effect;
                        this.apply_scoped_appearance(next, window, cx);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
            });
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
        div()
            .flex()
            .flex_col()
            .gap(px(if full { 16.0 } else { 12.0 }))
            .when(full, |section| {
                section.child(self.settings_heading(
                    "Appearance",
                    "Choose coordinated application and terminal colors without forcing accent decoration.",
                ))
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_1()
                    .child(self.settings_subheading("Scope"))
                    .child(
                        div().flex().gap_1().children(scopes),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(self.settings_subheading("Theme"))
                    .child(self.render_theme_catalog_entry(appearance.theme, focus(2), cx))
                    .when(theme_locked, |section| {
                        section.child(
                            div().text_size(px(UI_SMALL_TEXT_SIZE)).text_color(color(modal_text_color(colors.muted, colors)))
                                .child("Theme is fixed by the current command-line override."),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_1()
                    .child(self.settings_subheading("Background"))
                    .child(
                        div().flex().gap_1().children(effects),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Terminal opacity")
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
                            .border_color(color(if focus(5) {
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
                    .child(
                        div().text_size(px(UI_SMALL_TEXT_SIZE)).text_color(color(modal_text_color(colors.muted, colors)))
                            .child("10–100%. Terminal and header backgrounds fade together."),
                    ),
            )
            .when(self.settings_scope == SettingsScope::Window, |section| {
                section.child(
                    div().flex().justify_end().child(self.settings_action_button(
                        "reset-window-appearance",
                        "Use global defaults",
                        focus(6),
                        false,
                        cx.listener(|this, _, window, cx| {
                            this.reset_window_appearance(window);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )),
                )
            })
            .into_any_element()
    }

    pub(super) fn render_settings_overlay(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = self.colors();
        let full = matches!(self.overlay, Some(Overlay::Settings));
        let (responsive_width, responsive_height) = overlay_viewport_size(window);
        let compact = responsive_width < 620.0;
        let panel_width = (responsive_width - 64.0).clamp(1.0, if full { 760.0 } else { 520.0 });
        let overlay_height = responsive_height.max(1.0);
        let panel_height = (overlay_height - 48.0)
            .max(1.0)
            .min(if full { 560.0 } else { 500.0 });
        let panel = div()
            .w(px(panel_width))
            .h(px(panel_height))
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(color(colors.border))
            .bg(color(colors.surface))
            .text_color(color(modal_text_color(colors.foreground, colors)))
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
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.overlay_scroll)
                            .when(full, |content| content.p_4())
                            .when(!full, |content| content.p_3())
                            .child(if full {
                                self.render_settings_content(cx)
                            } else {
                                self.render_appearance_controls(false, cx)
                            }),
                    ),
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
                            false,
                            cx.listener(|this, _, _, cx| {
                                this.open_overlay(Overlay::Settings, "");
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
                    this.dismiss_overlay();
                    cx.notify();
                }),
            )
            .child(panel)
            .into_any_element()
    }
}
