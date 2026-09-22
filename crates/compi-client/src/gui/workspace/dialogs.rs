use super::*;
use gpui::{Animation, AnimationExt, ease_out_quint};

pub(in crate::gui) const OVERLAY_CLOSE_DURATION: Duration = Duration::from_millis(120);
const OVERLAY_OPEN_DURATION: Duration = Duration::from_millis(180);

impl CompiApp {
    pub(super) fn animate_overlay(&self, content: AnyElement) -> AnyElement {
        let wrapper = div().absolute().inset_0().child(content);
        if let Some(started) = self.overlay_closing_since {
            let progress = started.elapsed().as_secs_f32() / OVERLAY_CLOSE_DURATION.as_secs_f32();
            return wrapper
                .opacity((1.0 - progress).clamp(0.0, 1.0))
                .into_any_element();
        }
        wrapper
            .with_animation(
                ("overlay-open", self.overlay_generation as usize),
                Animation::new(OVERLAY_OPEN_DURATION).with_easing(ease_out_quint()),
                |element, progress| element.opacity(progress),
            )
            .into_any_element()
    }

    pub(super) fn render_pane_actions_menu(&self, cx: &Context<Self>) -> AnyElement {
        let colors = self.colors();
        let rows = self
            .overlay_choices(cx)
            .into_iter()
            .enumerate()
            .map(|(index, choice)| {
                let selected = index == self.overlay_index;
                let selected_background = blend_rgb(colors.surface, colors.foreground, 0.06);
                div()
                    .id(("pane-action-choice", index))
                    .min_h(px(40.0))
                    .px_3()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap_1()
                    .border_1()
                    .border_color(color(if selected {
                        colors.accent
                    } else {
                        colors.surface
                    }))
                    .bg(color(if selected {
                        selected_background
                    } else {
                        colors.surface
                    }))
                    .font_weight(if selected {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    })
                    .text_color(color(modal_text_color(
                        if choice.reason.is_some() {
                            colors.muted
                        } else {
                            colors.foreground
                        },
                        colors,
                    )))
                    .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.overlay_index = index;
                        this.activate_overlay(window, cx);
                        cx.stop_propagation();
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(choice.title)
                            .child(
                                div()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.muted, colors)))
                                    .child(choice.detail),
                            ),
                    )
                    .when_some(choice.reason, |row, reason| {
                        row.child(
                            div()
                                .text_size(px(UI_SMALL_TEXT_SIZE))
                                .text_color(color(modal_text_color(colors.muted, colors)))
                                .child(reason),
                        )
                    })
            });
        div()
            .absolute()
            .inset_0()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.dismiss_overlay();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .absolute()
                    .top(px(CHROME_HEIGHT + 6.0))
                    .right(px(WINDOW_CONTROLS_WIDTH + 6.0))
                    .w(px(276.0))
                    .p_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .rounded_md()
                    .border_1()
                    .border_color(color(colors.border))
                    .bg(color(colors.surface))
                    .text_color(color(modal_text_color(colors.foreground, colors)))
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .children(rows),
            )
            .into_any_element()
    }

    fn render_choice_rows(&self, cx: &Context<Self>) -> AnyElement {
        let colors = self.colors();
        let rows = self
            .overlay_choices(cx)
            .into_iter()
            .enumerate()
            .map(|(index, choice)| {
                let selected = index == self.overlay_index;
                let selected_background = blend_rgb(colors.surface, colors.foreground, 0.06);
                div()
                    .id(("command-choice", index))
                    .min_h(px(40.0))
                    .px_3()
                    .py_1()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap_1()
                    .border_1()
                    .border_color(color(if selected {
                        colors.accent
                    } else {
                        colors.surface
                    }))
                    .bg(color(if selected {
                        selected_background
                    } else {
                        colors.surface
                    }))
                    .font_weight(if selected {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    })
                    .text_color(color(modal_text_color(
                        if choice.reason.is_some() {
                            colors.muted
                        } else {
                            colors.foreground
                        },
                        colors,
                    )))
                    .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.overlay_index = index;
                        this.activate_overlay(window, cx);
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_4()
                            .child(div().min_w_0().flex_1().child(choice.title))
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.muted, colors)))
                                    .child(choice.detail),
                            ),
                    )
                    .when_some(choice.reason, |row, reason| {
                        row.child(
                            div()
                                .text_size(px(UI_SMALL_TEXT_SIZE))
                                .text_color(color(modal_text_color(colors.muted, colors)))
                                .child(reason),
                        )
                    })
            });
        div()
            .id("overlay-list")
            .min_h_0()
            .flex_1()
            .max_h(px(380.0))
            .overflow_y_scroll()
            .track_scroll(&self.overlay_scroll)
            .children(rows)
            .into_any_element()
    }

    fn render_confirmation_dialog(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let colors = self.colors();
        let daemon_restart = matches!(self.overlay, Some(Overlay::ConfirmDaemonRestart { .. }));
        let title = if daemon_restart {
            "Restart daemon"
        } else {
            "Confirm destructive action"
        };
        let message = self
            .overlay
            .as_ref()
            .and_then(|overlay| match overlay {
                Overlay::Confirm { title, .. } => Some(title.clone()),
                Overlay::ConfirmDaemonRestart { details, .. } => Some(details.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let confirm_label = if daemon_restart {
            "End work and restart daemon"
        } else {
            "Confirm"
        };
        self.render_centered_scrim(
            div()
                .w_full()
                .max_w(px(520.0))
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(color(colors.border))
                .bg(color(colors.surface))
                .overflow_hidden()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(self.render_dialog_header(title, cx))
                .child(
                    div()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .text_size(px(UI_BODY_TEXT_SIZE))
                                .text_color(color(modal_text_color(colors.foreground, colors)))
                                .child(message),
                        )
                        .child(
                            div()
                                .text_size(px(UI_SMALL_TEXT_SIZE))
                                .text_color(color(modal_text_color(colors.muted, colors)))
                                .child("This action cannot be undone."),
                        ),
                )
                .child(
                    div()
                        .min_h(px(48.0))
                        .px_4()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .border_t_1()
                        .border_color(color(colors.border))
                        .child(self.settings_action_button(
                            "confirmation-cancel",
                            "Cancel",
                            self.overlay_focus == 0,
                            false,
                            cx.listener(|this, _, _, cx| {
                                this.dismiss_overlay();
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        ))
                        .child(self.settings_action_button(
                            "confirmation-apply",
                            confirm_label,
                            self.overlay_focus == 1,
                            true,
                            cx.listener(|this, _, window, cx| {
                                this.activate_overlay(window, cx);
                                cx.stop_propagation();
                            }),
                        )),
                ),
            window,
            cx,
        )
    }

    fn render_text_dialog(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let colors = self.colors();
        let title = match &self.overlay {
            Some(Overlay::Text { title, .. }) => *title,
            _ => "Edit name",
        };
        let valid = !self.ime_text.trim().is_empty();
        self.render_centered_scrim(
            div()
                .w_full()
                .max_w(px(460.0))
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(color(colors.border))
                .bg(color(colors.surface))
                .overflow_hidden()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(self.render_dialog_header(title, cx))
                .child(
                    div()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(UI_SMALL_TEXT_SIZE))
                                .text_color(color(modal_text_color(colors.muted, colors)))
                                .child("Name"),
                        )
                        .child(self.render_editor(cx))
                        .when(!valid, |body| {
                            body.child(
                                div()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.error, colors)))
                                    .child("Enter a non-empty name."),
                            )
                        }),
                )
                .child(
                    div()
                        .min_h(px(48.0))
                        .px_4()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .border_t_1()
                        .border_color(color(colors.border))
                        .child(self.settings_action_button(
                            "text-cancel",
                            "Cancel",
                            self.overlay_focus == 1,
                            false,
                            cx.listener(|this, _, _, cx| {
                                this.dismiss_overlay();
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        ))
                        .when(valid, |footer| {
                            footer.child(self.settings_action_button(
                                "text-apply",
                                "Apply",
                                self.overlay_focus == 2,
                                false,
                                cx.listener(|this, _, window, cx| {
                                    this.activate_overlay(window, cx);
                                    cx.stop_propagation();
                                }),
                            ))
                        }),
                ),
            window,
            cx,
        )
    }

    fn render_diagnostics_dialog(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let colors = self.colors();
        let (viewport_width, viewport_height) = overlay_viewport_size(window);
        let overlay_height = (viewport_height - CHROME_HEIGHT).max(1.0);
        let panel_width = (viewport_width - 64.0).clamp(1.0, 620.0);
        let panel_height = (overlay_height - 48.0).clamp(1.0, 560.0);
        let row = |label: &'static str, value: String| {
            div()
                .min_h(px(40.0))
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .border_b_1()
                .border_color(color(colors.border))
                .child(
                    div()
                        .text_color(color(modal_text_color(colors.muted, colors)))
                        .child(label),
                )
                .child(div().min_w_0().child(value))
        };
        self.render_centered_scrim(
            div()
                .w(px(panel_width))
                .h(px(panel_height))
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(color(colors.border))
                .bg(color(colors.surface))
                .overflow_hidden()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(self.render_dialog_header("Diagnostics", cx))
                .child(
                    div()
                        .flex_1()
                        .id("diagnostics-scroll")
                        .min_h_0()
                        .overflow_y_scroll()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(row("Window slot", self.slot_id.clone()))
                                .child(row("Theme", self.theme.label().into()))
                                .child(row("Font", self.font_settings.family.clone())),
                        )
                        .when(!self.config_diagnostics.is_empty(), |body| {
                            body.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(self.settings_subheading("Configuration"))
                                    .child(
                                        div()
                                            .text_size(px(UI_SMALL_TEXT_SIZE))
                                            .text_color(color(modal_text_color(
                                                colors.muted,
                                                colors,
                                            )))
                                            .child(self.config_diagnostics.join("\n")),
                                    ),
                            )
                        })
                        .when_some(self.global_warning.clone(), |body, warning| {
                            body.child(
                                div()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.muted, colors)))
                                    .child(warning),
                            )
                        })
                        .when_some(self.global_error.clone(), |body, error| {
                            body.child(
                                div()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.error, colors)))
                                    .child(error),
                            )
                        }),
                )
                .child(
                    div()
                        .min_h(px(48.0))
                        .px_4()
                        .flex()
                        .items_center()
                        .justify_end()
                        .border_t_1()
                        .border_color(color(colors.border))
                        .child(self.settings_action_button(
                            "diagnostics-close",
                            "Close",
                            true,
                            false,
                            cx.listener(|this, _, _, cx| {
                                this.dismiss_overlay();
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        )),
                ),
            window,
            cx,
        )
    }

    fn render_dialog_header(&self, title: &'static str, cx: &Context<Self>) -> AnyElement {
        let colors = self.colors();
        div()
            .min_h(px(44.0))
            .px_4()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(color(colors.border))
            .child(
                div()
                    .text_size(px(18.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(self.overlay_close_button("dialog-close", cx))
            .into_any_element()
    }

    fn render_centered_scrim(
        &self,
        panel: impl IntoElement,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = self.colors();
        let (viewport_width, viewport_height) = overlay_viewport_size(window);
        let overlay_height = viewport_height.max(1.0);
        div()
            .absolute()
            .top_0()
            .left_0()
            .w(px(viewport_width))
            .h(px(overlay_height))
            .p_4()
            .flex()
            .items_center()
            .justify_center()
            .bg(color(colors.background).opacity(MODAL_SCRIM_OPACITY))
            .text_color(color(modal_text_color(colors.foreground, colors)))
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

    fn render_list_overlay(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let colors = self.colors();
        let (viewport_width, viewport_height) = overlay_viewport_size(window);
        let overlay_height = viewport_height.max(1.0);
        let panel_width = (viewport_width - 64.0).clamp(1.0, 620.0);
        let panel_height = (overlay_height - 48.0).clamp(1.0, 480.0);
        let title = match &self.overlay {
            Some(Overlay::Palette) => "Commands",
            Some(Overlay::Workspaces) => "Switch workspace",
            Some(Overlay::Tabs { hidden_only: true }) => "Restore hidden terminal tab",
            Some(Overlay::Tabs { .. }) => "Switch terminal tab",
            Some(Overlay::Windows) => "Move terminal tab to window",
            _ => "Choose",
        };
        let editable = matches!(
            self.overlay,
            Some(Overlay::Palette | Overlay::Workspaces | Overlay::Tabs { .. } | Overlay::Windows)
        );
        div()
            .absolute()
            .top_0()
            .left_0()
            .w(px(viewport_width))
            .h(px(overlay_height))
            .px_4()
            .pb_4()
            .pt(px(CHROME_HEIGHT + 12.0))
            .flex()
            .justify_center()
            .items_start()
            .bg(color(colors.background).opacity(MODAL_SCRIM_OPACITY))
            .text_color(color(modal_text_color(colors.foreground, colors)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.dismiss_overlay();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(panel_width))
                    .h(px(panel_height))
                    .min_h_0()
                    .mt_2()
                    .flex()
                    .flex_col()
                    .rounded_md()
                    .border_1()
                    .border_color(color(colors.border))
                    .bg(color(colors.surface))
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(self.render_dialog_header(title, cx))
                    .when(editable, |panel| panel.child(self.render_editor(cx)))
                    .when(matches!(self.overlay, Some(Overlay::Palette)), |panel| {
                        panel.child(
                            div()
                                .px_3()
                                .py_2()
                                .border_b_1()
                                .border_color(color(colors.border))
                                .child(self.command_button(
                                    "appearance-menu",
                                    "Quick Appearance",
                                    Command::OpenQuickAppearance,
                                    cx,
                                )),
                        )
                    })
                    .child(self.render_choice_rows(cx)),
            )
            .into_any_element()
    }

    pub(super) fn handle_dialog_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if matches!(
            self.overlay,
            Some(Overlay::Confirm { .. } | Overlay::ConfirmDaemonRestart { .. })
        ) {
            match key.key.as_str() {
                "escape" => self.dismiss_overlay(),
                "tab" | "left" | "right" => {
                    self.overlay_focus = usize::from(self.overlay_focus == 0)
                }
                "enter" | "space" if self.overlay_focus == 1 => self.activate_overlay(window, cx),
                "enter" | "space" => self.dismiss_overlay(),
                _ => return true,
            }
            cx.notify();
            return true;
        }
        if matches!(self.overlay, Some(Overlay::Text { .. })) {
            match key.key.as_str() {
                "escape" => self.dismiss_overlay(),
                "tab" => {
                    self.overlay_focus = if key.modifiers.shift {
                        match self.overlay_focus {
                            0 => 2,
                            1 => 0,
                            _ => 1,
                        }
                    } else {
                        (self.overlay_focus + 1) % 3
                    };
                }
                "enter" if self.overlay_focus == 1 => self.dismiss_overlay(),
                "enter" if !self.ime_text.trim().is_empty() => self.activate_overlay(window, cx),
                _ => return false,
            }
            cx.notify();
            return true;
        }
        if matches!(self.overlay, Some(Overlay::Diagnostics)) {
            if matches!(key.key.as_str(), "escape" | "enter" | "space") {
                self.dismiss_overlay();
                cx.notify();
            }
            return true;
        }
        false
    }

    pub(super) fn render_overlay(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let content = if matches!(self.overlay, Some(Overlay::ImageInspector)) {
            self.render_image_inspector(cx)
        } else if matches!(self.overlay, Some(Overlay::ThemeCatalog)) {
            self.render_theme_catalog(window, cx)
        } else if matches!(self.overlay, Some(Overlay::PaneActions)) {
            self.render_pane_actions_menu(cx)
        } else if matches!(
            self.overlay,
            Some(Overlay::QuickAppearance | Overlay::Settings)
        ) {
            self.render_settings_overlay(window, cx)
        } else if matches!(
            self.overlay,
            Some(Overlay::Confirm { .. } | Overlay::ConfirmDaemonRestart { .. })
        ) {
            self.render_confirmation_dialog(window, cx)
        } else if matches!(self.overlay, Some(Overlay::Text { .. })) {
            self.render_text_dialog(window, cx)
        } else if matches!(self.overlay, Some(Overlay::Diagnostics)) {
            self.render_diagnostics_dialog(window, cx)
        } else {
            self.render_list_overlay(window, cx)
        };
        self.animate_overlay(content)
    }
}
