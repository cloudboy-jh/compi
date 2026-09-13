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
        self.open_overlay(Overlay::ThemeCatalog, "");
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
        window: &mut Window,
    ) {
        if self.config.configured_appearance == appearance
            && self.config.theme_favorites == favorites
        {
            return;
        }
        self.config.configured_appearance = appearance;
        self.config.theme_favorites = favorites;
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
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = self.colors();
        let locked = self.settings_scope == SettingsScope::Window
            && self.config.provenance.theme == crate::config::ValueSource::CommandLine;
        div()
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
                    .child(theme.label())
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(color(colors.muted))
                            .child(theme.description()),
                    ),
            )
            .child(
                div()
                    .id("browse-theme-catalog")
                    .px_3()
                    .py_2()
                    .rounded_sm()
                    .border_1()
                    .border_color(color(colors.border))
                    .text_color(color(if locked {
                        colors.muted
                    } else {
                        colors.foreground
                    }))
                    .when(!locked, |button| {
                        button
                            .cursor_pointer()
                            .hover(|style| style.bg(color(colors.surface_hover)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_theme_catalog(this.settings_scope);
                                cx.stop_propagation();
                                cx.notify();
                            }))
                    })
                    .child(format!("Browse {} themes", ThemePreset::ALL.len())),
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
                            .text_size(px(10.0))
                            .text_color(color(colors.foreground))
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
                    .text_size(px(10.0))
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(div().text_color(color(colors.accent)).child("$"))
                            .child(
                                div()
                                    .text_color(color(colors.foreground))
                                    .child("git status"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(div().text_color(color(colors.ansi[2])).child("main"))
                            .child(
                                div()
                                    .text_color(color(colors.muted))
                                    .child("working tree clean"),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_theme_catalog(&self, cx: &Context<Self>) -> AnyElement {
        let Some(catalog) = &self.theme_catalog else {
            return div().into_any_element();
        };
        let colors = self.colors();
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
            let active = catalog.filter == filter;
            div()
                .id(("theme-filter", index))
                .px_3()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .bg(color(if active {
                    colors.surface_hover
                } else {
                    colors.surface
                }))
                .border_1()
                .border_color(color(if active { colors.accent } else { colors.border }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(catalog) = &mut this.theme_catalog {
                        catalog.filter = filter;
                        catalog.licenses = false;
                    }
                    this.overlay_scroll.set_offset(point(px(0.0), px(0.0)));
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(label)
        });
        let rows = self.catalog_themes().enumerate().map(|(index, theme)| {
            let selected = theme == candidate;
            let favorite = self.config.theme_favorites.contains(&theme);
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
                    colors.surface_hover
                } else {
                    colors.surface
                }))
                .cursor_pointer()
                .hover(|style| style.bg(color(colors.surface_hover)))
                .on_click(cx.listener(move |this, _, _, cx| {
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
                                .text_color(color(if selected {
                                    colors.accent
                                } else {
                                    colors.foreground
                                }))
                                .child(theme.label()),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(color(colors.muted))
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
                        .text_color(color(if favorite {
                            colors.accent
                        } else {
                            colors.muted
                        }))
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
                .text_size(px(11.0))
                .child(crate::theme::THEME_ATTRIBUTION)
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
                            .text_color(color(colors.muted))
                            .child("No matching themes. Try another search or filter."),
                    )
                })
                .into_any_element()
        };
        div()
            .absolute()
            .top(px(CHROME_HEIGHT))
            .bottom_0()
            .left_0()
            .right_0()
            .p_4()
            .flex()
            .justify_center()
            .bg(color(colors.background).opacity(0.35))
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
                    .w_full()
                    .max_w(px(850.0))
                    .h_full()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .rounded_md()
                    .border_1()
                    .border_color(color(colors.border))
                    .bg(color(colors.surface))
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex_none()
                            .p_3()
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(div().text_size(px(16.0)).child("Theme catalog"))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(color(colors.muted))
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
                            .children(filters)
                            .child(
                                div()
                                    .id("theme-licenses")
                                    .px_2()
                                    .py_1()
                                    .cursor_pointer()
                                    .text_color(color(colors.muted))
                                    .child(if catalog.licenses {
                                        "Back to themes"
                                    } else {
                                        "Licenses"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(catalog) = &mut this.theme_catalog {
                                            catalog.licenses = !catalog.licenses;
                                        }
                                        cx.stop_propagation();
                                        cx.notify();
                                    })),
                            ),
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
                                    .text_size(px(11.0))
                                    .text_color(color(colors.muted))
                                    .child(format!(
                                        "Previewing {}. Opacity stays unchanged.",
                                        candidate.label()
                                    )),
                            )
                            .child(
                                div()
                                    .id("cancel-theme-preview")
                                    .px_3()
                                    .py_2()
                                    .cursor_pointer()
                                    .child("Cancel")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.dismiss_overlay();
                                        window.focus(&this.focus_handle);
                                        cx.stop_propagation();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .id("apply-catalog-theme")
                                    .px_3()
                                    .py_2()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .bg(color(colors.accent))
                                    .text_color(color(colors.background))
                                    .child(if catalog.scope == SettingsScope::Global {
                                        "Apply globally"
                                    } else {
                                        "Apply to this window"
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.apply_catalog_theme(window, cx);
                                        cx.stop_propagation();
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}
