use super::*;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ThemeFilter {
    #[default]
    All,
    Dark,
    Light,
    Favorites,
}

pub(in crate::gui) struct CatalogState {
    original: ThemePreset,
    candidate: ThemePreset,
    scope: SettingsScope,
    filter: ThemeFilter,
    licenses: bool,
}

impl CompiApp {
    pub(super) fn open_theme_catalog(&mut self, scope: SettingsScope) {
        let parent = self
            .overlay
            .clone()
            .filter(|overlay| matches!(overlay, Overlay::Settings | Overlay::QuickAppearance));
        self.open_overlay(Overlay::ThemeCatalog, "");
        self.overlay_return = parent;
        self.settings_scope = scope;
        self.theme_catalog = Some(CatalogState {
            original: self.theme,
            candidate: self.theme,
            scope,
            filter: ThemeFilter::All,
            licenses: false,
        });
    }

    pub(super) fn cancel_catalog_preview(&mut self) {
        if let Some(catalog) = self.theme_catalog.take() {
            self.set_theme(catalog.original);
        }
    }

    fn catalog_themes(&self) -> impl Iterator<Item = ThemePreset> + '_ {
        let filter = self
            .theme_catalog
            .as_ref()
            .map_or(ThemeFilter::All, |catalog| catalog.filter);
        ThemePreset::ALL.into_iter().filter(move |theme| {
            let visible = match filter {
                ThemeFilter::All => true,
                ThemeFilter::Dark => theme.is_dark(),
                ThemeFilter::Light => !theme.is_dark(),
                ThemeFilter::Favorites => self.config.theme_favorites.contains(theme),
            };
            visible
                && self.ime_text.split_whitespace().all(|word| {
                    [
                        theme.id(),
                        theme.label(),
                        theme.family(),
                        theme.description(),
                    ]
                    .into_iter()
                    .any(|text| {
                        text.as_bytes()
                            .windows(word.len())
                            .any(|part| part.eq_ignore_ascii_case(word.as_bytes()))
                    })
                })
        })
    }

    fn preview_catalog_theme(&mut self, theme: ThemePreset) {
        if let Some(catalog) = self.theme_catalog.as_mut() {
            catalog.candidate = theme;
            self.set_theme(theme);
        }
    }

    pub(super) fn move_catalog_selection(&mut self, backwards: bool) {
        let Some(catalog) = &self.theme_catalog else {
            return;
        };
        let count = self.catalog_themes().count();
        if count == 0 {
            return;
        }
        let index = self
            .catalog_themes()
            .position(|theme| theme == catalog.candidate);
        let next = match index {
            Some(index) if backwards => (index + count - 1) % count,
            Some(index) => (index + 1) % count,
            None if backwards => count - 1,
            None => 0,
        };
        let next_theme = self.catalog_themes().nth(next);
        if let Some(theme) = next_theme {
            self.preview_catalog_theme(theme);
            self.overlay_scroll.scroll_to_item(next);
        }
    }

    pub(super) fn handle_catalog_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(self.overlay, Some(Overlay::ThemeCatalog)) {
            return false;
        }
        match key.key.as_str() {
            "escape" => self.dismiss_overlay(),
            "tab" => {
                self.overlay_focus = if key.modifiers.shift {
                    (self.overlay_focus + 7) % 8
                } else {
                    (self.overlay_focus + 1) % 8
                };
            }
            "up" | "down" if self.overlay_focus == 0 => {
                self.move_catalog_selection(key.key == "up");
            }
            "left" | "right" if (1..=4).contains(&self.overlay_focus) => {
                self.overlay_focus = if key.key == "left" {
                    if self.overlay_focus == 1 {
                        4
                    } else {
                        self.overlay_focus - 1
                    }
                } else if self.overlay_focus == 4 {
                    1
                } else {
                    self.overlay_focus + 1
                };
                let filter = [
                    ThemeFilter::All,
                    ThemeFilter::Dark,
                    ThemeFilter::Light,
                    ThemeFilter::Favorites,
                ][self.overlay_focus - 1];
                if let Some(catalog) = &mut self.theme_catalog {
                    catalog.filter = filter;
                    catalog.licenses = false;
                }
                self.overlay_scroll.set_offset(point(px(0.0), px(0.0)));
            }
            "enter" | "space" => match self.overlay_focus {
                0 | 7 => self.apply_catalog_theme(window, cx),
                1..=4 => {
                    let filter = [
                        ThemeFilter::All,
                        ThemeFilter::Dark,
                        ThemeFilter::Light,
                        ThemeFilter::Favorites,
                    ][self.overlay_focus - 1];
                    if let Some(catalog) = &mut self.theme_catalog {
                        catalog.filter = filter;
                        catalog.licenses = false;
                    }
                    self.overlay_scroll.set_offset(point(px(0.0), px(0.0)));
                }
                5 => {
                    if let Some(catalog) = &mut self.theme_catalog {
                        catalog.licenses = !catalog.licenses;
                    }
                }
                6 => self.dismiss_overlay(),
                _ => {}
            },
            _ if self.overlay_focus == 0 => return false,
            _ if key.key_char.is_some()
                || matches!(key.key.as_str(), "backspace" | "delete" | "home" | "end")
                || ((key.modifiers.platform || key.modifiers.control)
                    && matches!(key.key.as_str(), "a" | "c" | "v")) =>
            {
                self.overlay_focus = 0;
                cx.notify();
                return false;
            }
            _ => return true,
        }
        cx.notify();
        true
    }

    pub(super) fn apply_catalog_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(catalog) = self.theme_catalog.take() else {
            return;
        };
        self.set_theme(catalog.original);
        if catalog.scope == SettingsScope::Window {
            let mut appearance = self.appearance();
            appearance.theme = catalog.candidate;
            self.apply_scoped_appearance(appearance, window, cx);
        } else {
            let appearance = AppearanceSettings {
                theme: catalog.candidate,
                ..self.config.configured_appearance
            };
            if let Err(error) = self.config.save_global_appearance(appearance) {
                self.global_error = Some(error);
                self.theme_catalog = Some(catalog);
                return;
            }
            self.state.appearance.theme = None;
            self.set_theme(self.config.appearance.theme);
            self.save_state();
            self.broadcast_global_appearance(window, cx);
        }
        self.dismiss_overlay();
        window.focus(&self.focus_handle);
        cx.notify();
    }

    pub(super) fn sync_global_appearance(
        &mut self,
        appearance: AppearanceSettings,
        favorites: Vec<ThemePreset>,
        ui_font: UiFontPreset,
        terminal_font_family: String,
        window: &mut Window,
    ) {
        let terminal_font_changed = self.config.configured_font.family != terminal_font_family;
        if self.config.configured_appearance == appearance
            && self.config.theme_favorites == favorites
            && self.config.ui_font == ui_font
            && !terminal_font_changed
        {
            return;
        }
        self.config.configured_appearance = appearance;
        self.config.theme_favorites = favorites;
        self.config.ui_font = ui_font;
        self.ui_font = crate::font_catalog::resolve_ui_font(ui_font, window.text_system());
        if terminal_font_changed {
            self.config.configured_font.family = terminal_font_family.clone();
            if self.config.provenance.font_family != crate::config::ValueSource::CommandLine {
                self.config.font.family = terminal_font_family;
                self.config.provenance.font_family = crate::config::ValueSource::Configuration;
                self.font_settings = self.config.font.clone();
                self.typography_scale = 0.0;
            }
        }
        if self.config.provenance.theme != crate::config::ValueSource::CommandLine {
            self.config.appearance.theme = appearance.theme;
            self.config.provenance.theme = crate::config::ValueSource::Configuration;
            let theme = self.state.appearance.theme.unwrap_or(appearance.theme);
            if let Some(catalog) = &mut self.theme_catalog {
                catalog.original = theme;
            } else {
                self.set_theme(theme);
            }
        }
        self.config.appearance.terminal_opacity = appearance.terminal_opacity;
        self.config.appearance.background_effect = appearance.background_effect;
        self.terminal_opacity = self
            .state
            .appearance
            .terminal_opacity
            .unwrap_or(appearance.terminal_opacity);
        self.background_effect = self
            .state
            .appearance
            .background_effect
            .unwrap_or(appearance.background_effect);
        self.apply_window_background(window);
    }

    pub(super) fn broadcast_global_appearance(&self, window: &Window, cx: &mut Context<Self>) {
        let current = Window::window_handle(window).window_id();
        let appearance = self.config.configured_appearance;
        let ui_font = self.config.ui_font;
        let terminal_font_family = self.config.configured_font.family.clone();
        for handle in cx.windows() {
            if handle.window_id() == current {
                continue;
            }
            let Some(target) = handle.downcast::<CompiApp>() else {
                continue;
            };
            let _ = target.update(cx, |other, target_window, target_cx| {
                if other.config.path == self.config.path {
                    other.sync_global_appearance(
                        appearance,
                        self.config.theme_favorites.clone(),
                        ui_font,
                        terminal_font_family.clone(),
                        target_window,
                    );
                    target_cx.notify();
                }
            });
        }
    }

    fn toggle_theme_favorite(
        &mut self,
        theme: ThemePreset,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let mut favorites = self.config.theme_favorites.clone();
        if let Some(index) = favorites.iter().position(|favorite| *favorite == theme) {
            favorites.remove(index);
        } else {
            favorites.push(theme);
        }
        if let Err(error) = self.config.save_theme_favorites(&favorites) {
            self.global_error = Some(error);
        } else {
            self.broadcast_global_appearance(window, cx);
        }
        cx.notify();
    }

    pub(super) fn render_theme_catalog_entry(
        &self,
        theme: ThemePreset,
        focused: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = self.colors();
        let locked = self.settings_scope == SettingsScope::Window
            && self.config.provenance.theme == crate::config::ValueSource::CommandLine;
        let row_background = if focused {
            blend_rgb(colors.surface, colors.accent, 0.1)
        } else {
            colors.surface
        };
        let button_background = blend_rgb(colors.surface, colors.foreground, 0.11);
        let button_hover = blend_rgb(colors.surface, colors.foreground, 0.17);
        div()
            .min_h(px(52.0))
            .py_1()
            .rounded_sm()
            .bg(color(row_background))
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(theme.label()))
                    .child(
                        div()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(color(modal_text_color(colors.muted, colors)))
                            .child(theme.description()),
                    ),
            )
            .child(
                div()
                    .id("browse-theme-catalog")
                    .min_h(px(32.0))
                    .px_2()
                    .rounded_sm()
                    .bg(color(button_background))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color(ui_text_color(
                        if locked {
                            colors.muted
                        } else {
                            colors.foreground
                        },
                        button_background,
                    )))
                    .when(!locked, |button| {
                        button
                            .cursor_pointer()
                            .hover(move |style| style.bg(color(button_hover)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_theme_catalog(this.settings_scope);
                                cx.stop_propagation();
                                cx.notify();
                            }))
                    })
                    .child("Change…"),
            )
            .into_any_element()
    }

    fn theme_sample(theme: ThemePreset) -> AnyElement {
        let colors = theme.colors();
        div()
            .w(gpui::relative(0.5))
            .flex_none()
            .rounded_sm()
            .border_1()
            .border_color(color(colors.border))
            .overflow_hidden()
            .bg(color(colors.background))
            .child(
                div()
                    .h(px(25.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .bg(color(colors.surface))
                    .child(chrome_icon(ChromeIcon::Mark, color(colors.accent)))
                    .child(
                        div()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(color(modal_text_color(colors.foreground, colors)))
                            .child("compi / src"),
                    ),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .flex()
                    .flex_col()
                    .font_family("monospace")
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_color(color(ui_text_color(
                                        colors.accent,
                                        colors.background,
                                    )))
                                    .child("$"),
                            )
                            .child(
                                div()
                                    .text_color(color(modal_text_color(colors.foreground, colors)))
                                    .child("git status"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(color(ui_text_color(
                                        colors.ansi[2],
                                        colors.background,
                                    )))
                                    .child("main"),
                            )
                            .child(
                                div()
                                    .text_color(color(modal_text_color(colors.muted, colors)))
                                    .child("working tree clean"),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_theme_catalog(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let Some(catalog) = &self.theme_catalog else {
            return div().into_any_element();
        };
        let colors = self.colors();
        let (viewport_width, viewport_height) = overlay_viewport_size(window);
        let overlay_height = viewport_height.max(1.0);
        let panel_width = (viewport_width - 64.0).clamp(1.0, 780.0);
        let panel_height = (overlay_height - 48.0).clamp(1.0, 560.0);
        let candidate = catalog.candidate;
        let filters = [
            (ThemeFilter::All, "All"),
            (ThemeFilter::Dark, "Dark"),
            (ThemeFilter::Light, "Light"),
            (ThemeFilter::Favorites, "Favorites"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (filter, label))| {
            self.settings_segment_button(
                ("theme-filter", index),
                label,
                catalog.filter == filter,
                self.overlay_focus == index + 1,
                cx.listener(move |this, _, _, cx| {
                    this.overlay_focus = index + 1;
                    if let Some(catalog) = &mut this.theme_catalog {
                        catalog.filter = filter;
                        catalog.licenses = false;
                    }
                    this.overlay_scroll.set_offset(point(px(0.0), px(0.0)));
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
        });
        let rows = self.catalog_themes().enumerate().map(|(index, theme)| {
            let selected = theme == candidate;
            let favorite = self.config.theme_favorites.contains(&theme);
            let selected_background = blend_rgb(colors.surface, colors.accent, 0.14);
            div()
                .id(("catalog-theme", index))
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_3()
                .border_b_1()
                .border_color(color(colors.border))
                .bg(color(if selected {
                    selected_background
                } else {
                    colors.surface
                }))
                .cursor_pointer()
                .hover(|style| style.bg(color(colors.surface_hover)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.overlay_focus = 0;
                    this.preview_catalog_theme(theme);
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .font_weight(if selected {
                                    FontWeight::SEMIBOLD
                                } else {
                                    FontWeight::MEDIUM
                                })
                                .text_color(color(modal_text_color(colors.foreground, colors)))
                                .child(theme.label()),
                        )
                        .child(
                            div()
                                .text_size(px(UI_SMALL_TEXT_SIZE))
                                .text_color(color(modal_text_color(colors.muted, colors)))
                                .child(format!(
                                    "{} · {}",
                                    theme.family(),
                                    if theme.is_dark() { "Dark" } else { "Light" }
                                )),
                        ),
                )
                .child(Self::theme_sample(theme))
                .child(
                    div()
                        .id(("theme-favorite", index))
                        .px_1()
                        .py_2()
                        .text_size(px(19.0))
                        .text_color(color(modal_text_color(
                            if favorite {
                                colors.accent
                            } else {
                                colors.muted
                            },
                            colors,
                        )))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.toggle_theme_favorite(theme, window, cx);
                            cx.stop_propagation();
                        }))
                        .child(if favorite { "★" } else { "☆" }),
                )
        });
        let body = if catalog.licenses {
            div()
                .id("theme-license-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_3()
                .flex()
                .flex_col()
                .gap_5()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(self.settings_subheading("Theme licenses"))
                        .child(crate::theme::THEME_ATTRIBUTION),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(self.settings_subheading("Bundled font licenses"))
                        .child(crate::font_catalog::FONT_ATTRIBUTION),
                )
                .into_any_element()
        } else {
            div()
                .id("theme-catalog-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.overlay_scroll)
                .children(rows)
                .when(self.catalog_themes().next().is_none(), |body| {
                    body.child(
                        div()
                            .p_4()
                            .text_color(color(modal_text_color(colors.muted, colors)))
                            .child("No matching themes. Try another search or filter."),
                    )
                })
                .into_any_element()
        };
        div()
            .absolute()
            .top_0()
            .left_0()
            .w(px(viewport_width))
            .h(px(overlay_height))
            .p_4()
            .flex()
            .justify_center()
            .items_center()
            .bg(color(colors.background).opacity(MODAL_SCRIM_OPACITY))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.dismiss_overlay();
                    window.focus(&this.focus_handle);
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(panel_width))
                    .h(px(panel_height))
                    .min_h_0()
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
                            .p_3()
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_size(px(18.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Theme catalog"),
                            )
                            .child(
                                div()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.muted, colors)))
                                    .child(if catalog.scope == SettingsScope::Global {
                                        "Global defaults · Esc cancels"
                                    } else {
                                        "This window · Esc cancels"
                                    }),
                            ),
                    )
                    .child(self.render_editor(cx))
                    .child(
                        div()
                            .flex_none()
                            .px_3()
                            .py_2()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .p(px(2.0))
                                    .rounded_md()
                                    .bg(color(blend_rgb(colors.surface, colors.foreground, 0.06)))
                                    .children(filters),
                            )
                            .child(self.settings_action_button(
                                "theme-licenses",
                                if catalog.licenses {
                                    "Back to themes"
                                } else {
                                    "Licenses"
                                },
                                self.overlay_focus == 5,
                                false,
                                cx.listener(|this, _, _, cx| {
                                    this.overlay_focus = 5;
                                    if let Some(catalog) = &mut this.theme_catalog {
                                        catalog.licenses = !catalog.licenses;
                                    }
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            )),
                    )
                    .child(body)
                    .child(
                        div()
                            .flex_none()
                            .p_3()
                            .border_t_1()
                            .border_color(color(colors.border))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.muted, colors)))
                                    .child(format!(
                                        "Previewing {}. Opacity stays unchanged.",
                                        candidate.label()
                                    )),
                            )
                            .child(self.settings_action_button(
                                "cancel-theme-preview",
                                "Cancel",
                                self.overlay_focus == 6,
                                false,
                                cx.listener(|this, _, window, cx| {
                                    this.overlay_focus = 6;
                                    this.dismiss_overlay();
                                    window.focus(&this.focus_handle);
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            ))
                            .child(self.settings_primary_button(
                                "apply-catalog-theme",
                                if catalog.scope == SettingsScope::Global {
                                    "Use globally"
                                } else {
                                    "Use in this window"
                                },
                                self.overlay_focus == 7,
                                cx.listener(|this, _, window, cx| {
                                    this.overlay_focus = 7;
                                    this.apply_catalog_theme(window, cx);
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            )),
                    ),
            )
            .into_any_element()
    }
}
