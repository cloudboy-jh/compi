use super::*;
use gpui::{Animation, AnimationExt, ease_out_quint};

const OVERLAY_CLOSE_DURATION: Duration = Duration::from_millis(120);
const OVERLAY_OPEN_DURATION: Duration = Duration::from_millis(180);

/// How long a closing overlay fades out: not at all with reduced motion.
pub(in crate::gui) fn overlay_close_duration() -> Duration {
    if *super::focus::REDUCED_MOTION {
        Duration::ZERO
    } else {
        OVERLAY_CLOSE_DURATION
    }
}

impl CompiApp {
    /// Fades the overlay in as it opens and out as it closes; with reduced motion it
    /// appears and goes at once.
    pub(super) fn animate_overlay(&self, content: AnyElement) -> AnyElement {
        let wrapper = div().absolute().inset_0().child(content);
        if let Some(started) = self.overlay_closing_since {
            let duration = overlay_close_duration().as_secs_f32();
            let progress = if duration > 0.0 {
                started.elapsed().as_secs_f32() / duration
            } else {
                1.0
            };
            return wrapper
                .opacity((1.0 - progress).clamp(0.0, 1.0))
                .into_any_element();
        }
        if *super::focus::REDUCED_MOTION {
            return wrapper.into_any_element();
        }
        wrapper
            .with_animation(
                ("overlay-open", self.overlay_generation as usize),
                Animation::new(OVERLAY_OPEN_DURATION).with_easing(ease_out_quint()),
                |element, progress| element.opacity(progress),
            )
            .into_any_element()
    }

    pub(super) fn render_choice_rows(&self, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let choices = self.overlay_choices(cx);
        let mut rows = Vec::new();
        let mut previous_group = None;
        for (index, choice) in choices.into_iter().enumerate() {
            let group_heading = if choice.group != previous_group {
                previous_group = choice.group;
                choice.group
            } else {
                None
            };
            let selected = index == self.overlay_index;
            let selected_background = blend_rgb(colors.surface, colors.foreground, 0.06);
            let description = if matches!(self.overlay, Some(Overlay::Palette)) {
                match &choice.action {
                    ChoiceAction::Command(command) => command.description(),
                    _ => None,
                }
            } else {
                None
            };
            let (shortcut, detail) = if matches!(&choice.action, ChoiceAction::Command(_)) {
                ((!choice.detail.is_empty()).then_some(choice.detail), None)
            } else {
                (None, (!choice.detail.is_empty()).then_some(choice.detail))
            };
            // Arrangement layouts only preview on click; double-click, Enter, or
            // Apply commits them. Clicking a tab to combine toggles it in place,
            // keeping the previewed layout selected. Other rows act on one click.
            let in_arrangements = matches!(self.overlay, Some(Overlay::Arrangements { .. }));
            let preview_only =
                in_arrangements && matches!(choice.action, ChoiceAction::Arrangement(_));
            let toggle_tab = match &choice.action {
                ChoiceAction::ArrangementTab(tab_id) if in_arrangements => Some(tab_id.clone()),
                _ => None,
            };
            let row = div()
                .id(("command-choice", index))
                .min_h(px(40.0))
                .px_3()
                .py_2()
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
                .text_color(color(modal_text_color(
                    if choice.reason.is_some() {
                        colors.muted
                    } else {
                        colors.foreground
                    },
                    &colors,
                )))
                .hover(move |style| style.bg(color(colors.surface_hover)).cursor_pointer())
                .on_click(
                    cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        if let Some(tab_id) = &toggle_tab {
                            this.toggle_arrangement_tab(tab_id);
                        } else {
                            this.overlay_index = index;
                            if !preview_only || event.click_count() >= 2 {
                                this.activate_overlay(window, cx);
                            }
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .flex()
                        .w_full()
                        .min_w_0()
                        .items_center()
                        .justify_between()
                        .gap_4()
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .font_weight(FontWeight::MEDIUM)
                                .child(choice.title),
                        )
                        .when_some(shortcut, |row, shortcut| {
                            row.child(
                                div()
                                    .flex_none()
                                    .text_size(px(UI_MICRO_TEXT_SIZE))
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(color(modal_text_color(colors.muted, &colors)))
                                    .child(shortcut),
                            )
                        }),
                )
                .when_some(description, |row, description| {
                    row.child(
                        div()
                            .w_full()
                            .min_w_0()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .text_color(color(modal_text_color(colors.muted, &colors)))
                            .child(description),
                    )
                })
                .when_some(detail, |row, detail| {
                    row.child(
                        div()
                            .w_full()
                            .min_w_0()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .text_color(color(modal_text_color(colors.muted, &colors)))
                            .child(detail),
                    )
                })
                .when_some(choice.reason, |row, reason| {
                    row.child(
                        div()
                            .w_full()
                            .min_w_0()
                            .pt_1()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(color(modal_text_color(colors.muted, &colors)))
                            .child(reason),
                    )
                });
            rows.push(
                div()
                    .when_some(group_heading, |item, group| {
                        item.child(
                            div()
                                .px_3()
                                .pt_3()
                                .pb_1()
                                .text_size(px(UI_MICRO_TEXT_SIZE))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(color(modal_text_color(colors.muted, &colors)))
                                .child(group),
                        )
                    })
                    .child(row)
                    .into_any_element(),
            );
        }
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
        let colors = *self.colors();
        let (title, message, confirm_label) = match &self.overlay {
            Some(Overlay::ConfirmDaemonRestart { details, .. }) => (
                "Restart daemon",
                details.clone(),
                "End work and restart daemon",
            ),
            Some(Overlay::Confirm { title, action, .. }) => (*action, title.clone(), *action),
            _ => ("", String::new(), ""),
        };
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
                        .text_size(px(UI_BODY_TEXT_SIZE))
                        .text_color(color(modal_text_color(colors.foreground, &colors)))
                        .child(message),
                )
                .child(
                    div()
                        .min_h(px(48.0))
                        .flex_none()
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

    /// Save / Don't save / Cancel for the dirty files an action would drop: each file's
    /// name over its folder and environment.
    fn render_unsaved_dialog(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let Some(Overlay::Unsaved { files, saving, .. }) = &self.overlay else {
            return div().into_any_element();
        };
        let saving = saving.is_some();
        let count = files.len();
        let message = if count == 1 {
            "This file has unsaved changes. Save them first?".to_owned()
        } else {
            format!("{count} files have unsaved changes. Save them first?")
        };
        let rows = files.iter().enumerate().map(|(index, file)| {
            div()
                .id(("unsaved-file", index))
                .py_1()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(UI_BODY_TEXT_SIZE))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(content::file_name(&file.path).to_owned()),
                )
                .child(
                    div()
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .text_color(color(colors.muted))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(file.place.clone()),
                )
        });
        let button = |id: &'static str, label: &'static str, choice: usize| {
            let run = cx.listener(move |this, _, window, cx| {
                this.answer_unsaved(choice, window, cx);
                cx.stop_propagation();
            });
            let focused = self.overlay_focus == choice;
            match choice {
                2 => self.settings_primary_button(id, label, focused, run),
                _ => self.settings_action_button(id, label, focused, choice == 1, run),
            }
        };
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
                .child(self.render_dialog_header("Unsaved changes", cx))
                .child(
                    div()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_color(color(modal_text_color(colors.foreground, &colors)))
                        .child(div().text_size(px(UI_BODY_TEXT_SIZE)).child(message))
                        .child(
                            div()
                                .id("unsaved-files")
                                .max_h(px(220.0))
                                .overflow_y_scroll()
                                .children(rows),
                        ),
                )
                .child(
                    div()
                        .min_h(px(48.0))
                        .flex_none()
                        .px_4()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .border_t_1()
                        .border_color(color(colors.border))
                        .child(button("unsaved-cancel", "Cancel", 0))
                        .child(button("unsaved-discard", "Don't save", 1))
                        .child(button(
                            "unsaved-save",
                            if saving { "Saving…" } else { "Save" },
                            2,
                        )),
                ),
            window,
            cx,
        )
    }

    /// Discard changes: the files by name over their folder, untracked ones marked
    /// `delete`. Cancel starts focused, so Enter and Escape both keep the files.
    fn render_discard_dialog(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let Some(Overlay::ConfirmDiscard {
            paths, untracked, ..
        }) = &self.overlay
        else {
            return div().into_any_element();
        };
        let count = paths.len() + untracked.len();
        let message = if count == 1 {
            "Discard changes to 1 file? This can't be undone.".to_owned()
        } else {
            format!("Discard changes to {count} files? This can't be undone.")
        };
        let files = paths
            .iter()
            .map(|path| (path, false))
            .chain(untracked.iter().map(|path| (path, true)));
        let rows = files.enumerate().map(|(index, (path, delete))| {
            let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
            div()
                .id(("discard-file", index))
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(UI_BODY_TEXT_SIZE))
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(name.to_owned()),
                        )
                        .when(!folder.is_empty(), |file| {
                            file.child(
                                div()
                                    .text_size(px(UI_MICRO_TEXT_SIZE))
                                    .text_color(color(colors.muted))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(folder.to_owned()),
                            )
                        }),
                )
                .when(delete, |row| {
                    row.child(
                        div()
                            .flex_none()
                            .px_1()
                            .rounded_sm()
                            .bg(color(colors.surface_hover))
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(color(colors.error))
                            .child("delete"),
                    )
                })
        });
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
                .child(self.render_dialog_header("Discard changes", cx))
                .child(
                    div()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_color(color(modal_text_color(colors.foreground, &colors)))
                        .child(div().text_size(px(UI_BODY_TEXT_SIZE)).child(message))
                        .child(
                            div()
                                .id("discard-files")
                                .max_h(px(220.0))
                                .overflow_y_scroll()
                                .children(rows),
                        ),
                )
                .child(
                    div()
                        .min_h(px(48.0))
                        .flex_none()
                        .px_4()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .border_t_1()
                        .border_color(color(colors.border))
                        .child(self.settings_action_button(
                            "discard-cancel",
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
                            "discard-apply",
                            "Discard",
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
        let colors = *self.colors();
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
                                .text_color(color(modal_text_color(colors.muted, &colors)))
                                .child("Name"),
                        )
                        .child(self.render_editor(cx))
                        .when(!valid, |body| {
                            body.child(
                                div()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.error, &colors)))
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
        let colors = *self.colors();
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
                        .text_color(color(modal_text_color(colors.muted, &colors)))
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
                                                &colors,
                                            )))
                                            .child(self.config_diagnostics.join("\n")),
                                    ),
                            )
                        })
                        .when_some(self.global_warning.clone(), |body, warning| {
                            body.child(
                                div()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.muted, &colors)))
                                    .child(warning),
                            )
                        })
                        .when_some(self.global_error.clone(), |body, error| {
                            body.child(
                                div()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.error, &colors)))
                                    .child(error),
                            )
                        })
                        .when_some(self.connection_error.clone(), |body, error| {
                            body.child(
                                div()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.error, &colors)))
                                    .child(error),
                            )
                        })
                        .children(self.surface_views.iter().filter_map(|view| {
                            let details = self.pane_diagnostics(view);
                            let metadata = self.pane_metadata(&view.surface_id);
                            if details.is_empty() && metadata.is_none() {
                                return None;
                            }
                            Some(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        self.settings_subheading(&format!("Pane {}", view.pane_id)),
                                    )
                                    .when(!details.is_empty(), |pane| {
                                        pane.child(
                                            div()
                                                .text_size(px(UI_SMALL_TEXT_SIZE))
                                                .text_color(color(modal_text_color(
                                                    colors.error,
                                                    &colors,
                                                )))
                                                .child(details),
                                        )
                                    })
                                    .when_some(metadata, |pane, metadata| {
                                        pane.child(
                                            div()
                                                .text_size(px(UI_SMALL_TEXT_SIZE))
                                                .text_color(color(modal_text_color(
                                                    colors.muted,
                                                    &colors,
                                                )))
                                                .child(crate::metadata::details(metadata)),
                                        )
                                    }),
                            )
                        })),
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

    pub(super) fn render_dialog_header(
        &self,
        title: &'static str,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
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
        let colors = *self.colors();
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
            .text_color(color(modal_text_color(colors.foreground, &colors)))
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
        let colors = *self.colors();
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
            Some(Overlay::Recover { .. }) => "Recover unsaved edits",
            _ => "Choose",
        };
        let editable = matches!(
            self.overlay,
            Some(
                Overlay::Palette
                    | Overlay::Workspaces
                    | Overlay::Tabs { .. }
                    | Overlay::Windows
                    | Overlay::Recover { .. }
            )
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
            .text_color(color(modal_text_color(colors.foreground, &colors)))
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
        if matches!(self.overlay, Some(Overlay::Arrangements { .. })) {
            return self.handle_arrangement_key(key, cx);
        }
        if matches!(
            self.overlay,
            Some(
                Overlay::Confirm { .. }
                    | Overlay::ConfirmDaemonRestart { .. }
                    | Overlay::ConfirmDiscard { .. }
            )
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
        if matches!(self.overlay, Some(Overlay::Unsaved { .. })) {
            // Cancel, Don't save, Save; Save starts focused, so Enter saves.
            match key.key.as_str() {
                "escape" => self.answer_unsaved(0, window, cx),
                "tab" if key.modifiers.shift => self.overlay_focus = (self.overlay_focus + 2) % 3,
                "tab" | "right" => self.overlay_focus = (self.overlay_focus + 1) % 3,
                "left" => self.overlay_focus = (self.overlay_focus + 2) % 3,
                "enter" | "space" => self.answer_unsaved(self.overlay_focus, window, cx),
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
        } else if matches!(
            self.overlay,
            Some(Overlay::Confirm { .. } | Overlay::ConfirmDaemonRestart { .. })
        ) {
            self.render_confirmation_dialog(window, cx)
        } else if matches!(self.overlay, Some(Overlay::Unsaved { .. })) {
            self.render_unsaved_dialog(window, cx)
        } else if matches!(self.overlay, Some(Overlay::ConfirmDiscard { .. })) {
            self.render_discard_dialog(window, cx)
        } else if matches!(self.overlay, Some(Overlay::Text { .. })) {
            self.render_text_dialog(window, cx)
        } else if matches!(self.overlay, Some(Overlay::Diagnostics)) {
            self.render_diagnostics_dialog(window, cx)
        } else if matches!(self.overlay, Some(Overlay::Arrangements { .. })) {
            self.render_arrangement_overlay(window, cx)
        } else {
            self.render_list_overlay(window, cx)
        };
        self.animate_overlay(content)
    }
}
