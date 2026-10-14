use super::settings::{RowCx, SettingsAction, SettingsHost, THEME_BAR_ROW, settings_card};
use super::*;
use crate::gui::controls::IconName;
use crate::theme::{ThemeDefinition, ThemeId};
use crate::theme_store::ThemeLibrary;
use gpui::Entity;
use gpui::PathPromptOptions;
use gpui_component::Sizable as _;

// One local store job at a time, including across native windows. The library also
// serializes mutations; this gate bounds the GUI's queued filesystem work.
static CATALOG_STORE_BUSY: AtomicBool = AtomicBool::new(false);
static CATALOG_OPERATION_ID: AtomicU64 = AtomicU64::new(1);

struct CatalogStorePermit;

impl CatalogStorePermit {
    fn acquire() -> Option<Self> {
        CATALOG_STORE_BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for CatalogStorePermit {
    fn drop(&mut self) {
        CATALOG_STORE_BUSY.store(false, Ordering::Release);
    }
}

/// Which themes the theme list shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ThemeFilter {
    #[default]
    All,
    Dark,
    Light,
    Favorites,
}

impl ThemeFilter {
    pub(super) const ALL: [Self; 4] = [Self::All, Self::Dark, Self::Light, Self::Favorites];

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::Favorites => "Favorites",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum CatalogTarget {
    Terminal,
    #[default]
    Both,
}

/// A theme session of one Settings pane: the theme tiles' or the theme list's live
/// preview, and the list's filter, search and store operations. It ends when the
/// preview is applied or cancelled with the list closed, and is cancelled when the
/// pane loses the keyboard, closes or shows something else.
pub(in crate::gui) struct CatalogState {
    pub(super) original: AppearanceSettings,
    accepted: AppearanceSettings,
    pub(super) candidate: Arc<ThemeDefinition>,
    pub(super) target: CatalogTarget,
    /// The Settings pane the session belongs to.
    pub(super) pane: PaneId,
    /// The pane shows the theme list rather than its section.
    pub(super) list: bool,
    /// The list's search, lowercased.
    pub(super) query: String,
    /// The pane had the keyboard since the session started; losing it cancels.
    pub(super) held: bool,
    ordered_themes: Vec<Arc<ThemeDefinition>>,
    scope: SettingsScope,
    pub(super) filter: ThemeFilter,
    pub(super) licenses: bool,
    pub(super) previewed: bool,
    pub(super) pending: bool,
    operation_id: u64,
    pub(super) error: Option<String>,
    pub(super) status: Option<String>,
    pub(super) remove_mode: bool,
    removal_candidate: Option<ThemeId>,
    pub(super) removal_confirmation: bool,
}

impl CatalogState {
    /// Whether the pane shows the Apply / Cancel bar: a preview or a removal waits.
    pub(super) fn awaiting_decision(&self) -> bool {
        self.previewed || self.remove_mode
    }

    /// The section the session's list and tiles live in.
    pub(super) fn section(&self) -> compi_protocol::SettingsSection {
        match self.target {
            CatalogTarget::Both => compi_protocol::SettingsSection::Appearance,
            CatalogTarget::Terminal => compi_protocol::SettingsSection::Advanced,
        }
    }
}

impl CompiApp {
    /// Starts a theme session for `target` in the Settings pane `pane`, the list open
    /// when `list`. A session already there for `target` is kept (opening its list);
    /// another one is cancelled first.
    pub(super) fn start_theme_session(&mut self, pane: PaneId, target: CatalogTarget, list: bool) {
        if let Some(catalog) = &mut self.theme_catalog
            && catalog.pane == pane
            && catalog.target == target
        {
            catalog.list |= list;
            return;
        }
        self.cancel_catalog_preview();
        let scope = self.settings_scope;
        let original = self.accepted_appearance();
        let accepted = if scope == SettingsScope::Global {
            self.config.configured_appearance.clone()
        } else {
            original.clone()
        };
        let candidate = self.resolve_theme(if target == CatalogTarget::Terminal {
            accepted.effective_terminal_theme()
        } else {
            &accepted.theme
        });
        let ordered_themes = self.catalog_ordered_themes();
        self.theme_catalog = Some(CatalogState {
            original,
            accepted,
            candidate,
            target,
            pane,
            list,
            query: String::new(),
            held: false,
            ordered_themes,
            scope,
            filter: ThemeFilter::All,
            licenses: false,
            previewed: false,
            pending: false,
            operation_id: 0,
            error: None,
            status: None,
            remove_mode: false,
            removal_candidate: None,
            removal_confirmation: false,
        });
    }

    fn restore_catalog_axes(&mut self, original: &AppearanceSettings) {
        self.set_theme(self.resolve_theme(&original.theme));
        self.set_terminal_theme(self.resolve_theme(original.effective_terminal_theme()));
        self.terminal_theme_override = original.terminal_theme_override;
    }

    /// Ends the theme session, restoring what it previewed.
    pub(super) fn cancel_catalog_preview(&mut self) {
        if let Some(catalog) = self.theme_catalog.take() {
            self.restore_catalog_axes(&catalog.original);
        }
    }

    /// Cancel in the Apply / Cancel bar, or Escape: the preview or removal is dropped.
    /// With the list open the session stays on it, back on the current theme.
    pub(super) fn cancel_theme_preview(&mut self) {
        let Some(catalog) = &self.theme_catalog else {
            return;
        };
        if catalog.pending {
            return;
        }
        if !catalog.list {
            self.cancel_catalog_preview();
            return;
        }
        let original = catalog.original.clone();
        self.restore_catalog_axes(&original);
        let current = self.catalog_current_theme();
        if let Some(catalog) = &mut self.theme_catalog {
            if let Some(current) = current {
                catalog.candidate = current;
            }
            catalog.previewed = false;
            catalog.remove_mode = false;
            catalog.removal_candidate = None;
            catalog.removal_confirmation = false;
            catalog.status = None;
            catalog.error = None;
        }
    }

    /// Back from the theme list: a waiting preview keeps its bar; otherwise the
    /// session ends.
    pub(super) fn close_theme_list(&mut self) {
        match &mut self.theme_catalog {
            Some(catalog) if catalog.previewed && !catalog.remove_mode => {
                catalog.list = false;
                catalog.licenses = false;
            }
            Some(_) => self.cancel_catalog_preview(),
            None => {}
        }
    }

    fn catalog_current_id(&self) -> Option<&ThemeId> {
        let catalog = self.theme_catalog.as_ref()?;
        Some(if catalog.target == CatalogTarget::Terminal {
            catalog.accepted.effective_terminal_theme()
        } else {
            &catalog.accepted.theme
        })
    }

    fn catalog_current_theme(&self) -> Option<Arc<ThemeDefinition>> {
        self.catalog_current_id().map(|id| self.resolve_theme(id))
    }

    fn catalog_ordered_themes(&self) -> Vec<Arc<ThemeDefinition>> {
        let mut themes: Vec<_> = self.theme_library.entries().cloned().collect();
        sort_themes(&mut themes);
        themes
    }

    /// The theme grid's groups, as far as the filter and search let themes through;
    /// see `group_themes`. Removal lists the local themes, by appearance only.
    pub(super) fn catalog_groups(&self) -> Vec<ThemeGroup> {
        let Some(catalog) = self.theme_catalog.as_ref() else {
            return Vec::new();
        };
        let favorites = &self.config.theme_favorites;
        let query = catalog.query.as_str();
        let visible = catalog.ordered_themes.iter().filter(|theme| {
            if catalog.remove_mode {
                theme.is_imported() && theme_matches(theme, ThemeFilter::All, false, query)
            } else {
                theme_matches(
                    theme,
                    catalog.filter,
                    favorites.contains(theme.theme_id()),
                    query,
                )
            }
        });
        if catalog.remove_mode {
            return group_themes(visible, None, &[]);
        }
        let current = self.catalog_current_theme();
        group_themes(
            visible,
            current.as_ref().map(|theme| theme.theme_id()),
            favorites,
        )
    }

    fn preview_catalog_theme(&mut self, theme: Arc<ThemeDefinition>) {
        let Some(catalog) = self.theme_catalog.as_mut() else {
            return;
        };
        if catalog.pending || CATALOG_STORE_BUSY.load(Ordering::Acquire) {
            return;
        }
        catalog.removal_confirmation = false;
        self.refresh_catalog_preview(theme);
    }

    fn refresh_catalog_preview(&mut self, theme: Arc<ThemeDefinition>) {
        let locked = self.config.provenance.theme == crate::config::ValueSource::CommandLine;
        let Some(catalog) = self.theme_catalog.as_mut() else {
            return;
        };
        catalog.candidate = theme.clone();
        catalog.previewed = !locked;
        if locked {
            return;
        }
        let original = catalog.original.clone();
        let target = catalog.target;
        let terminal_override = original.terminal_theme_override;
        self.restore_catalog_axes(&original);
        if target == CatalogTarget::Both {
            self.set_theme(theme.clone());
        }
        if target == CatalogTarget::Terminal || !terminal_override {
            self.set_terminal_theme(theme);
            if target == CatalogTarget::Terminal {
                self.terminal_theme_override = true;
            }
        }
    }

    /// Moves the grid's selection one `step` in a grid of `columns`, previewing it;
    /// see `grid_step`.
    pub(super) fn move_catalog_selection(&mut self, step: GridStep, columns: usize) {
        let Some(catalog) = &self.theme_catalog else {
            return;
        };
        if catalog.pending || CATALOG_STORE_BUSY.load(Ordering::Acquire) {
            return;
        }
        let id = if catalog.remove_mode {
            catalog.removal_candidate.clone()
        } else {
            Some(catalog.candidate.theme_id().clone())
        };
        let groups = self.catalog_groups();
        let themes: Vec<_> = groups.iter().flat_map(|group| &group.themes).collect();
        let index = themes
            .iter()
            .position(|theme| Some(theme.theme_id()) == id.as_ref());
        if let Some(next) = grid_step(&grid_rows(&groups, columns), index, step) {
            self.select_catalog_theme(Arc::clone(themes[next]));
        }
    }

    /// A list row picked: previewed, or in removal the one to remove.
    pub(super) fn select_catalog_theme(&mut self, theme: Arc<ThemeDefinition>) {
        let Some(catalog) = &mut self.theme_catalog else {
            return;
        };
        if catalog.pending {
            return;
        }
        if catalog.remove_mode {
            catalog.removal_candidate = Some(theme.theme_id().clone());
            catalog.removal_confirmation = false;
        } else {
            self.preview_catalog_theme(theme);
        }
    }

    /// Whether `theme` is the list's selected row.
    pub(super) fn catalog_selected(&self, theme: &ThemeDefinition) -> bool {
        self.theme_catalog.as_ref().is_some_and(|catalog| {
            if catalog.remove_mode {
                catalog.removal_candidate.as_ref() == Some(theme.theme_id())
            } else {
                catalog.candidate.theme_id() == theme.theme_id()
            }
        })
    }

    /// The list's filter; picking one leaves removal and attribution.
    pub(super) fn set_catalog_filter(&mut self, filter: ThemeFilter) {
        if let Some(catalog) = &mut self.theme_catalog {
            if catalog.pending {
                return;
            }
            if catalog.remove_mode {
                catalog.status = None;
                catalog.error = None;
            }
            catalog.filter = filter;
            catalog.licenses = false;
            catalog.remove_mode = false;
            catalog.removal_confirmation = false;
        }
    }

    /// The bar's primary action: apply the preview, or in removal remove the selected
    /// local theme (asking first).
    pub(super) fn decide_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .theme_catalog
            .as_ref()
            .is_some_and(|catalog| catalog.remove_mode)
        {
            self.remove_catalog_theme(window, cx);
        } else {
            self.apply_catalog_theme(window, cx);
        }
    }

    /// Stars or unstars the list's selected theme.
    pub(super) fn toggle_selected_favorite(&mut self, window: &Window, cx: &mut Context<Self>) {
        if let Some(theme) = self.catalog_action_theme() {
            self.toggle_theme_favorite(theme.theme_id().clone(), window, cx);
        }
    }

    /// The More menu's actions: import, attribution, export and local removal. Like
    /// every list control, they wait while a dialog or the theme store is busy.
    pub(super) fn catalog_menu(&self, focused: bool, cx: &Context<Self>) -> AnyElement {
        let catalog = self.theme_catalog.as_ref();
        let busy = catalog.is_some_and(|catalog| catalog.pending);
        let licenses = catalog.is_some_and(|catalog| catalog.licenses);
        let remove_mode = catalog.is_some_and(|catalog| catalog.remove_mode);
        let open = self.popup == Some(Popup::CatalogMenu);
        let trigger = controls::icon_button("theme-menu-button", IconName::Ellipsis, "More")
            .header()
            .menu_open(open)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_popup(Popup::CatalogMenu, !open);
                cx.notify();
            }));
        controls::menu(
            "theme-menu",
            controls::highlight(focused, trigger, cx),
            open,
            [
                controls::menu_item(
                    "Import Zed theme…",
                    cx.listener(|this, _, window, cx| {
                        this.import_catalog_theme(window, cx);
                        cx.notify();
                    }),
                )
                .disabled(busy),
                controls::menu_item(
                    "Export selected…",
                    cx.listener(|this, _, _, cx| {
                        this.export_catalog_theme(cx);
                        cx.notify();
                    }),
                )
                .disabled(busy),
                controls::menu_item(
                    if remove_mode {
                        "Back to themes"
                    } else {
                        "Remove local theme"
                    },
                    cx.listener(|this, _, _, cx| {
                        this.toggle_catalog_removal();
                        cx.notify();
                    }),
                )
                .disabled(busy),
                controls::menu_item(
                    if licenses {
                        "Back to themes"
                    } else {
                        "Attribution"
                    },
                    cx.listener(|this, _, _, cx| {
                        if let Some(catalog) = &mut this.theme_catalog {
                            catalog.licenses = !catalog.licenses;
                        }
                        cx.notify();
                    }),
                )
                .disabled(busy),
            ],
        )
        .anchor(Anchor::TopRight)
        .on_open_change(cx.listener(|this, open: &bool, _, cx| {
            this.set_popup(Popup::CatalogMenu, *open);
            cx.notify();
        }))
        .into_any_element()
    }

    fn catalog_action_theme(&self) -> Option<Arc<ThemeDefinition>> {
        let catalog = self.theme_catalog.as_ref()?;
        if catalog.remove_mode {
            self.theme_library
                .resolve(catalog.removal_candidate.as_ref()?)
        } else {
            Some(catalog.candidate.clone())
        }
    }

    fn toggle_catalog_removal(&mut self) {
        let first = self
            .theme_library
            .entries()
            .find(|theme| theme.is_imported())
            .map(|theme| theme.theme_id().clone());
        let original = match &self.theme_catalog {
            Some(catalog) if !catalog.pending => catalog.original.clone(),
            _ => return,
        };
        // Removal does not preview: the panes go back to the accepted colors.
        self.restore_catalog_axes(&original);
        let current = self.catalog_current_theme();
        if let Some(catalog) = &mut self.theme_catalog {
            catalog.remove_mode = !catalog.remove_mode;
            catalog.licenses = false;
            catalog.previewed = false;
            if let Some(current) = current {
                catalog.candidate = current;
            }
            catalog.removal_candidate = first;
            catalog.removal_confirmation = false;
            catalog.error = None;
            catalog.status = catalog
                .remove_mode
                .then(|| "Select an unused local theme. Its source file is kept.".into());
        }
    }

    pub(super) fn apply_catalog_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .theme_catalog
            .as_ref()
            .is_none_or(|catalog| catalog.pending)
            || CATALOG_STORE_BUSY.load(Ordering::Acquire)
            || self.config.provenance.theme == crate::config::ValueSource::CommandLine
        {
            return;
        }
        let Some(mut catalog) = self.theme_catalog.take() else {
            return;
        };
        // The window settings as they are now; the session's copy predates any change
        // made beside the grid (transparency, opacity).
        let mut appearance = if catalog.scope == SettingsScope::Global {
            self.config.configured_appearance.clone()
        } else {
            self.accepted_appearance()
        };
        if catalog.target == CatalogTarget::Both {
            appearance.theme = catalog.candidate.theme_id().clone();
        }
        if catalog.target == CatalogTarget::Terminal {
            appearance.terminal_theme = catalog.candidate.theme_id().clone();
            appearance.terminal_theme_override = true;
        }
        if catalog.scope == SettingsScope::Window {
            self.restore_catalog_axes(&catalog.original);
            self.apply_scoped_appearance(appearance, window, cx);
        } else {
            if let Err(error) = self.config.save_global_appearance(appearance) {
                catalog.error = Some(error.clone());
                self.global_error = Some(error);
                self.theme_catalog = Some(catalog);
                cx.notify();
                return;
            }
            if catalog.target == CatalogTarget::Both {
                self.state.appearance.theme = None;
            }
            if catalog.target == CatalogTarget::Terminal {
                self.state.appearance.terminal_theme = None;
                self.state.appearance.terminal_theme_override = None;
            }
            let accepted = self.accepted_appearance();
            self.restore_catalog_axes(&accepted);
            self.save_state();
            self.broadcast_global_appearance(window, cx);
        }
        // With the list open the session goes on from the colors just applied.
        if catalog.list {
            self.start_theme_session(catalog.pane.clone(), catalog.target, true);
            if let Some(session) = &mut self.theme_catalog {
                session.filter = catalog.filter;
                session.query = catalog.query;
            }
        }
        cx.notify();
    }

    pub(super) fn sync_global_appearance(
        &mut self,
        appearance: AppearanceSettings,
        favorites: Vec<ThemeId>,
        ui_font: UiFontPreset,
        density: crate::theme::WorkspaceDensity,
        terminal_font_family: String,
        window: &mut Window,
    ) {
        let terminal_font_changed = self.config.configured_font.family != terminal_font_family;
        if self.config.configured_appearance == appearance
            && self.config.theme_favorites == favorites
            && self.config.ui_font == ui_font
            && self.config.density == density
            && !terminal_font_changed
        {
            return;
        }
        self.config.configured_appearance = appearance.clone();
        self.config.theme_favorites = favorites;
        self.config.ui_font = ui_font;
        self.ui_font = crate::font_catalog::resolve_ui_font(ui_font, window.text_system());
        if self.config.density != density {
            self.config.density = density;
            self.rebuild_layout(window, true);
        }
        if terminal_font_changed {
            self.config.configured_font.family = terminal_font_family.clone();
            if self.config.provenance.font_family != crate::config::ValueSource::CommandLine {
                self.config.font.family = terminal_font_family;
                self.config.provenance.font_family = crate::config::ValueSource::Configuration;
                self.font_settings = self.config.font.clone();
                self.typography_scale = 0.0;
            }
        }
        let cli = self.config.provenance.theme == crate::config::ValueSource::CommandLine;
        if !cli {
            self.config.appearance.theme = appearance.theme.clone();
            self.config.appearance.terminal_theme = appearance.terminal_theme.clone();
            self.config.appearance.terminal_theme_override = appearance.terminal_theme_override;
            self.config.provenance.theme = crate::config::ValueSource::Configuration;
        }
        self.config.appearance.terminal_opacity = appearance.terminal_opacity;
        self.config.appearance.background_effect = appearance.background_effect;
        self.config.appearance.transparent_background = appearance.transparent_background;
        let accepted = inherited_appearance(&self.config, &self.state.appearance);
        self.terminal_opacity = accepted.terminal_opacity;
        self.background_effect = accepted.background_effect;
        self.transparent_background = accepted.transparent_background;
        self.report_missing_theme_ids(&accepted);
        if let Some(catalog) = &mut self.theme_catalog {
            catalog.original = accepted.clone();
            catalog.accepted = if catalog.scope == SettingsScope::Global {
                appearance.clone()
            } else {
                accepted.clone()
            };
            let candidate = catalog.candidate.clone();
            let previewed = catalog.previewed;
            if previewed {
                self.refresh_catalog_preview(candidate);
            } else {
                self.restore_catalog_axes(&accepted);
            }
        } else {
            self.restore_catalog_axes(&accepted);
        }
        self.apply_window_background(window);
    }

    pub(super) fn broadcast_global_appearance(&self, window: &Window, cx: &mut Context<Self>) {
        let current = Window::window_handle(window).window_id();
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
                        self.config.configured_appearance.clone(),
                        self.config.theme_favorites.clone(),
                        self.config.ui_font,
                        self.config.density,
                        self.config.configured_font.family.clone(),
                        target_window,
                    );
                    target_cx.notify();
                }
            });
        }
    }

    pub(super) fn adopt_theme_library(
        &mut self,
        library: ThemeLibrary,
        mut diagnostics: Vec<String>,
    ) {
        self.theme_library = library;
        let accepted = self.accepted_appearance();
        diagnostics.extend(self.missing_theme_diagnostics(&accepted));
        self.restore_catalog_axes(&accepted);
        if !diagnostics.is_empty() {
            self.global_error = Some(diagnostics.join("\n"));
        }
        let ordered_themes = self
            .theme_catalog
            .as_ref()
            .map(|_| self.catalog_ordered_themes());
        if let Some(catalog) = &mut self.theme_catalog {
            catalog.ordered_themes = ordered_themes.unwrap_or_default();
            catalog.candidate = self
                .theme_library
                .resolve(catalog.candidate.theme_id())
                .unwrap_or_else(|| self.theme_library.fallback());
            let candidate = catalog.candidate.clone();
            let previewed = catalog.previewed;
            if previewed {
                self.refresh_catalog_preview(candidate);
            }
        }
    }

    fn missing_theme_diagnostics(&self, appearance: &AppearanceSettings) -> Vec<String> {
        [("Theme", &appearance.theme), ("Terminal colors", appearance.effective_terminal_theme())]
            .into_iter()
            .filter(|(_, id)| self.theme_library.resolve(id).is_none())
            .map(|(axis, id)| format!("{axis} theme '{}' is unavailable; using Compi Neutral. The saved selection is retained.", id.id()))
            .collect()
    }

    fn report_missing_theme_ids(&mut self, appearance: &AppearanceSettings) {
        let diagnostics = self.missing_theme_diagnostics(appearance);
        if !diagnostics.is_empty() && self.global_error.is_none() {
            self.global_error = Some(diagnostics.join("\n"));
        }
    }

    pub(super) fn broadcast_theme_library(&self, window: &Window, cx: &mut Context<Self>) {
        let current = Window::window_handle(window).window_id();
        for handle in cx.windows() {
            if handle.window_id() == current {
                continue;
            }
            let Some(target) = handle.downcast::<CompiApp>() else {
                continue;
            };
            let _ = target.update(cx, |other, _, target_cx| {
                // All local windows use the same managed store, regardless of config path.
                other.adopt_theme_library(self.theme_library.clone(), Vec::new());
                target_cx.notify();
            });
        }
    }

    fn set_catalog_notice(&mut self, error: Option<String>, status: Option<String>) {
        if let Some(catalog) = &mut self.theme_catalog {
            catalog.pending = false;
            catalog.error = error;
            catalog.status = status;
        }
    }

    fn finish_catalog_io(&mut self, operation: u64, error: Option<String>, status: Option<String>) {
        if self
            .theme_catalog
            .as_ref()
            .is_some_and(|catalog| catalog.operation_id == operation)
        {
            self.set_catalog_notice(error, status);
        }
    }

    fn begin_catalog_io(&mut self) -> Option<u64> {
        let catalog = self.theme_catalog.as_mut()?;
        if catalog.pending {
            return None;
        }
        catalog.pending = true;
        catalog.operation_id = CATALOG_OPERATION_ID.fetch_add(1, Ordering::Relaxed);
        catalog.error = None;
        catalog.status = None;
        Some(catalog.operation_id)
    }

    fn import_catalog_theme(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(operation) = self.begin_catalog_io() else {
            return;
        };
        let source_window = gpui::WindowHandle::<CompiApp>::new(window.window_handle().window_id());
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import Zed theme JSON".into()),
        });
        let mut library = self.theme_library.clone();
        cx.spawn(async move |weak, cx| {
            let path = match picker.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(error)) => {
                    let _ = weak.update(cx, |this, cx| {
                        this.finish_catalog_io(
                            operation,
                            Some(format!("Could not open Import dialog: {error}")),
                            None,
                        );
                        cx.notify();
                    });
                    return;
                }
                _ => None,
            };
            let Some(path) = path else {
                let _ = weak.update(cx, |this, cx| {
                    this.finish_catalog_io(operation, None, None);
                    cx.notify();
                });
                return;
            };
            let Some(permit) = CatalogStorePermit::acquire() else {
                let _ = weak.update(cx, |this, cx| {
                    this.finish_catalog_io(
                        operation,
                        Some("Theme store is busy. Try again shortly.".into()),
                        None,
                    );
                    cx.notify();
                });
                return;
            };
            let _permit = permit;
            let (library, result) = cx
                .background_executor()
                .spawn(async move {
                    let result = library.import_file(&path);
                    (library, result)
                })
                .await;
            let _ = source_window.update(cx, |this, window, cx| {
                match result {
                    Ok(themes) => {
                        this.adopt_theme_library(library, Vec::new());
                        this.finish_catalog_io(
                            operation,
                            None,
                            Some(format!(
                                "Imported {} ({} variant{}) locally. Not applied or published.",
                                themes[0].family(),
                                themes.len(),
                                if themes.len() == 1 { "" } else { "s" },
                            )),
                        );
                        this.broadcast_theme_library(window, cx);
                    }
                    Err(error) => this.finish_catalog_io(operation, Some(error), None),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn export_catalog_theme(&mut self, cx: &mut Context<Self>) {
        let Some(theme) = self.catalog_action_theme() else {
            return;
        };
        let Some(operation) = self.begin_catalog_io() else {
            return;
        };
        let filename: String = theme
            .label()
            .chars()
            .take(96)
            .map(|value| {
                if matches!(value, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                    '-'
                } else {
                    value
                }
            })
            .collect();
        let name = format!("{}.json", filename.trim_end_matches(['.', ' ']));
        let picker = cx.prompt_for_new_path(std::path::Path::new("."), Some(&name));
        let library = self.theme_library.clone();
        let id = theme.theme_id().clone();
        cx.spawn(async move |weak, cx| {
            let path = match picker.await {
                Ok(Ok(path)) => path,
                Ok(Err(error)) => {
                    let _ = weak.update(cx, |this, cx| {
                        this.finish_catalog_io(
                            operation,
                            Some(format!("Could not open Export dialog: {error}")),
                            None,
                        );
                        cx.notify();
                    });
                    return;
                }
                _ => None,
            };
            let Some(path) = path else {
                let _ = weak.update(cx, |this, cx| {
                    this.finish_catalog_io(operation, None, None);
                    cx.notify();
                });
                return;
            };
            let Some(permit) = CatalogStorePermit::acquire() else {
                let _ = weak.update(cx, |this, cx| {
                    this.finish_catalog_io(
                        operation,
                        Some("Theme store is busy. Try again shortly.".into()),
                        None,
                    );
                    cx.notify();
                });
                return;
            };
            let _permit = permit;
            let result = cx
                .background_executor()
                .spawn(async move { library.export_file(&id, &path) })
                .await;
            let _ = weak.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.finish_catalog_io(
                        operation,
                        None,
                        Some("Zed theme JSON exported. Existing attribution and notices are preserved.".into()),
                    ),
                    Err(error) => this.finish_catalog_io(operation, Some(error), None),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn theme_removal_blockers(&self, id: &ThemeId) -> Vec<&'static str> {
        let mut blockers = Vec::new();
        if &self.config.configured_appearance.theme == id
            || self.config.configured_appearance.effective_terminal_theme() == id
        {
            blockers.push("global defaults");
        }
        if self.config.provenance.theme == crate::config::ValueSource::CommandLine
            && (&self.config.appearance.theme == id
                || self.config.appearance.effective_terminal_theme() == id)
        {
            blockers.push("a command-line theme selection");
        }
        let accepted = self.accepted_appearance();
        if &accepted.theme == id || accepted.effective_terminal_theme() == id {
            blockers.push("an open window's accepted appearance");
        }
        if let Some(catalog) = &self.theme_catalog
            && (&catalog.original.theme == id
                || catalog.original.effective_terminal_theme() == id
                || &catalog.accepted.theme == id
                || catalog.accepted.effective_terminal_theme() == id
                || catalog.candidate.theme_id() == id)
        {
            blockers.push("an open theme preview");
        }
        blockers
    }

    fn remove_catalog_theme(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(theme) = self.catalog_action_theme() else {
            return;
        };
        if !theme.is_imported() {
            self.set_catalog_notice(Some("Bundled themes cannot be removed.".into()), None);
            return;
        }
        let id = theme.theme_id().clone();
        let mut blockers = self.theme_removal_blockers(&id);
        let current = Window::window_handle(window).window_id();
        let source_window = gpui::WindowHandle::<CompiApp>::new(current);
        for handle in cx.windows() {
            if handle.window_id() == current {
                continue;
            }
            let Some(target) = handle.downcast::<CompiApp>() else {
                continue;
            };
            let _ = target.update(cx, |other, _, _| {
                blockers.extend(other.theme_removal_blockers(&id))
            });
        }
        blockers.sort_unstable();
        blockers.dedup();
        if !blockers.is_empty() {
            self.set_catalog_notice(
                Some(format!(
                    "Cannot remove {}: used by {}. Switch away or cancel those previews first.",
                    theme.label(),
                    blockers.join(", ")
                )),
                None,
            );
            return;
        }
        let Some(catalog) = &mut self.theme_catalog else {
            return;
        };
        if catalog.pending {
            return;
        }
        if !catalog.removal_confirmation {
            catalog.removal_confirmation = true;
            catalog.error = None;
            catalog.status = Some(format!(
                "Remove {} from the local library? Confirm removal deletes only this variant; sibling variants and the external source file are kept.",
                theme.label()
            ));
            return;
        }
        let Some(permit) = CatalogStorePermit::acquire() else {
            self.set_catalog_notice(Some("Theme store is busy. Try again shortly.".into()), None);
            return;
        };
        let Some(operation) = self.begin_catalog_io() else {
            return;
        };
        let mut library = self.theme_library.clone();
        let mut config = self.config.clone();
        let previous_favorites = config.theme_favorites.clone();
        let favorites: Vec<_> = previous_favorites
            .iter()
            .filter(|favorite| *favorite != &id)
            .cloned()
            .collect();
        cx.spawn(async move |_, cx| {
            let _permit = permit;
            let (library, result, durable_favorites) = cx.background_executor().spawn(async move {
                let result = config.save_theme_favorites(&favorites).and_then(|()| {
                    library.remove(&id).map_err(|error| {
                        if let Err(rollback) = config.save_theme_favorites(&previous_favorites) {
                            format!("{error}\nFavorite rollback also failed: {rollback}. The theme remains in the library.")
                        } else { error }
                    })
                });
                (library, result, config.theme_favorites)
            }).await;
            let _ = source_window.update(cx, |this, window, cx| {
                this.config.theme_favorites = durable_favorites;
                if result.is_ok() { this.adopt_theme_library(library, Vec::new()); }
                match result {
                    Ok(()) => this.finish_catalog_io(operation, None, Some("Local theme removed. Its external source file is untouched.".into())),
                    Err(error) => this.finish_catalog_io(operation, Some(error), None),
                }
                if let Some(catalog) = &mut this.theme_catalog && catalog.operation_id == operation {
                    catalog.removal_confirmation = false;
                    catalog.removal_candidate = None;
                }
                this.broadcast_theme_library(window, cx);
                this.broadcast_global_appearance(window, cx);
                cx.notify();
            });
        }).detach();
    }

    fn toggle_theme_favorite(&mut self, theme: ThemeId, window: &Window, cx: &mut Context<Self>) {
        if self
            .theme_catalog
            .as_ref()
            .is_some_and(|catalog| catalog.pending)
            || CATALOG_STORE_BUSY.load(Ordering::Acquire)
        {
            return;
        }
        let mut favorites = self.config.theme_favorites.clone();
        if let Some(index) = favorites.iter().position(|favorite| favorite == &theme) {
            favorites.remove(index);
        } else {
            favorites.push(theme);
        }
        if let Err(error) = self.config.save_theme_favorites(&favorites) {
            self.global_error = Some(error.clone());
            if let Some(catalog) = &mut self.theme_catalog {
                catalog.error = Some(error);
            }
        } else {
            if let Some(catalog) = &mut self.theme_catalog {
                catalog.error = None;
            }
            self.broadcast_global_appearance(window, cx);
        }
        cx.notify();
    }

    /// The theme section: a header line (title, search, filter, More), notices, then
    /// every theme as a tile in `columns` equal columns, under its group's label (see
    /// `group_themes`), filling the rest of the page in its own scroll (`grid`, whose
    /// viewport `viewport` records). With `back`, a back control leads the header (the
    /// terminal colors page).
    pub(super) fn render_theme_section(
        &self,
        row: &RowCx,
        search: &Entity<gpui_component::input::InputState>,
        (grid, viewport): (&ScrollHandle, AnyElement),
        columns: usize,
        back: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(catalog) = &self.theme_catalog else {
            return div().into_any_element();
        };
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let terminal = catalog.target == CatalogTarget::Terminal;
        let locked = self.config.provenance.theme == crate::config::ValueSource::CommandLine;
        let current_id = self
            .catalog_current_theme()
            .map(|theme| theme.theme_id().clone());
        let list_focused = self.settings_focused(row, SettingsAction::ThemeList);
        let star_focused = self.settings_focused(row, SettingsAction::ThemeFavorite);
        let more_focused = self.settings_focused(row, SettingsAction::ThemeMore);
        let filters_focused = self.settings_focused(row, SettingsAction::ThemeFilter);
        let filter_row = row.clone();
        let filters = controls::segmented(
            "theme-filter",
            ThemeFilter::ALL
                .into_iter()
                .enumerate()
                .map(|(index, filter)| {
                    let row = filter_row.clone();
                    controls::text_button(("theme-filter", index), filter.label())
                        .selected(catalog.filter == filter && !catalog.remove_mode)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.focus_settings_control(&row, SettingsAction::ThemeFilter);
                            this.set_catalog_filter(filter);
                            cx.notify();
                        }))
                }),
        );
        let header = div()
            .flex_none()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_3()
            .gap_y_2()
            .when(back, |header| {
                header.child(
                    self.settings_control(
                        row,
                        SettingsAction::ThemeBack,
                        controls::icon_button((row.row, 0_usize), IconName::ChevronLeft, "Back")
                            .header()
                            .key("escape"),
                        cx,
                    ),
                )
            })
            .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(
                if catalog.remove_mode {
                    "Remove local themes"
                } else if terminal {
                    "Terminal colors"
                } else {
                    "Theme"
                },
            ))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w(px(120.0))
                    .max_w(px(280.0))
                    .child(
                        gpui_component::input::Input::new(search)
                            .appearance(true)
                            .cleanable(true)
                            .with_size(gpui_component::Size::Small),
                    )
                    .child(self.settings_focus_anchor(
                        self.settings_focused(row, SettingsAction::ThemeSearch),
                        cx,
                    )),
            )
            .child(
                controls::highlight(filters_focused, filters, cx)
                    .child(self.settings_focus_anchor(filters_focused, cx)),
            )
            .child(
                div()
                    .ml_auto()
                    .relative()
                    .child(self.catalog_menu(more_focused, cx))
                    .child(self.settings_focus_anchor(more_focused, cx)),
            );
        let override_note = !terminal && self.scoped_appearance().terminal_theme_override;
        let notices = (catalog.pending
            || catalog.error.is_some()
            || catalog.status.is_some()
            || locked
            || override_note)
            .then(|| {
                div()
                    .flex_none()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .line_height(px(UI_SMALL_LINE_HEIGHT))
                    .text_color(muted)
                    .when(locked, |notes| notes.child("Colors are fixed by --theme."))
                    .when(override_note && !locked, |notes| {
                        notes.child("Terminal colors are overridden in Advanced.")
                    })
                    .when(catalog.pending, |notes| {
                        notes.child("Waiting for dialog or local theme store…")
                    })
                    .when_some(catalog.error.clone(), |notes, error| {
                        notes.child(
                            div()
                                .text_color(color(modal_text_color(colors.error, &colors)))
                                .child(error),
                        )
                    })
                    .when_some(catalog.status.clone(), |notes, status| notes.child(status))
            });
        let content = if catalog.licenses {
            let attribution = self
                .catalog_action_theme()
                .unwrap_or_else(|| catalog.candidate.clone());
            settings_card(&colors)
                .p_4()
                .gap_4()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .line_height(px(UI_SMALL_LINE_HEIGHT))
                .child(self.settings_subheading("Selected theme attribution"))
                .child(attribution.attribution().to_owned())
                .child(self.settings_subheading("Bundled theme licenses"))
                .child(crate::theme::THEME_ATTRIBUTION)
                .child(self.settings_subheading("Bundled font licenses"))
                .child(crate::font_catalog::FONT_ATTRIBUTION)
                .into_any_element()
        } else {
            let groups = self.catalog_groups();
            if groups.is_empty() {
                div()
                    .text_color(muted)
                    .child(if catalog.remove_mode {
                        "No matching local themes to remove."
                    } else {
                        "No matching themes. Try another search or filter."
                    })
                    .into_any_element()
            } else {
                let columns = columns.max(1);
                let mut first = 0;
                let sections = groups.iter().map(|group| {
                    let start = first;
                    first += group.themes.len();
                    let rows = group
                        .themes
                        .chunks(columns)
                        .enumerate()
                        .map(|(line, chunk)| {
                            let tiles = chunk.iter().enumerate().map(|(offset, theme)| {
                                let selected = self.catalog_selected(theme);
                                let current = !catalog.remove_mode
                                    && current_id.as_ref() == Some(theme.theme_id());
                                self.render_theme_tile(
                                    row,
                                    (start + line * columns + offset, theme.clone()),
                                    (selected, current),
                                    (list_focused, star_focused),
                                    line == 0,
                                    cx,
                                )
                            });
                            div()
                                .flex()
                                .gap_3()
                                .children(tiles)
                                // Empty cells keep a short row's tiles as wide as the others.
                                .children((chunk.len()..columns).map(|_| div().flex_1().min_w_0()))
                        });
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(THEME_GROUP_LABEL_GAP))
                        .child(
                            div()
                                .text_size(px(UI_SMALL_TEXT_SIZE))
                                .line_height(px(UI_SMALL_LINE_HEIGHT))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(muted)
                                .child(group.label),
                        )
                        .child(div().flex().flex_col().gap_3().children(rows))
                });
                div()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .pb_2()
                    .children(sections)
                    .into_any_element()
            }
        };
        // No `min_h_0`: the header, notes and the grid's minimum height always fit, and a
        // short page scrolls around them.
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(header)
            .children(notices)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(160.0))
                    .child(
                        div()
                            .id("theme-grid")
                            .absolute()
                            .inset_0()
                            .overflow_y_scroll()
                            .track_scroll(grid)
                            // The scroll clips: room for a tile's ring on every edge.
                            .p(px(4.0))
                            .child(content),
                    )
                    .child(viewport),
            )
            .into_any_element()
    }

    /// One tile of the theme grid: the theme's swatch (an accent edge when selected, a
    /// check when current) over its name and star; the star shows while hovered,
    /// selected or starred. A tile in its group's `first_row` scrolls into view with
    /// the group's label.
    fn render_theme_tile(
        &self,
        row: &RowCx,
        (index, theme): (usize, Arc<ThemeDefinition>),
        (selected, current): (bool, bool),
        (list_focused, star_focused): (bool, bool),
        first_row: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let favorite = self.config.theme_favorites.contains(theme.theme_id());
        let favorite_id = theme.theme_id().clone();
        let star = controls::icon_button(
            ("theme-star", index),
            if favorite {
                IconName::StarFill
            } else {
                IconName::Star
            },
            if favorite {
                "Remove from favorites"
            } else {
                "Add to favorites"
            },
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            this.toggle_theme_favorite(favorite_id.clone(), window, cx);
        }));
        let pick = theme.clone();
        let pick_row = row.clone();
        let on_accent = ui_text_color(colors.background & 0x00ff_ffff, colors.accent);
        let tile = div()
            .id(("theme-tile", index))
            .group("theme-tile")
            .relative()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.focus_settings_control(&pick_row, SettingsAction::ThemeList);
                this.select_catalog_theme(pick.clone());
                cx.notify();
            }))
            .child(
                theme_swatch(&theme, 64.0)
                    .when(selected, |swatch| {
                        swatch.border_2().border_color(color(colors.accent))
                    })
                    .when(!selected, |swatch| {
                        swatch.hover(move |style| style.border_color(color(colors.muted)))
                    })
                    .when(current, |swatch| {
                        swatch.child(
                            div()
                                .absolute()
                                .top(px(6.0))
                                .right(px(6.0))
                                .size(px(16.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .bg(color(colors.accent))
                                .child(controls::glyph(IconName::Check, color(on_accent))),
                        )
                    }),
            )
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .line_height(px(UI_SMALL_LINE_HEIGHT))
                            .text_color(color(modal_text_color(
                                if selected || current {
                                    colors.foreground
                                } else {
                                    colors.muted
                                },
                                &colors,
                            )))
                            .when(selected || current, |name| {
                                name.font_weight(FontWeight::MEDIUM)
                            })
                            .child(theme.label().to_owned()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .when(!(favorite || selected), |star| {
                                star.invisible()
                                    .group_hover("theme-tile", |style| style.visible())
                            })
                            .child(controls::highlight(selected && star_focused, star, cx)),
                    ),
            );
        controls::highlight(selected && list_focused, tile, cx)
            .flex_1()
            .min_w_0()
            .when(selected, |tile| {
                // In a group's first row, scrolling into view brings its label too.
                let above = if first_row {
                    UI_SMALL_LINE_HEIGHT + THEME_GROUP_LABEL_GAP
                } else {
                    0.0
                };
                tile.child(
                    div()
                        .absolute()
                        .top(px(-above))
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .child(self.settings_focus_anchor(list_focused || star_focused, cx)),
                )
            })
            .into_any_element()
    }

    /// The Apply / Cancel bar at the foot of the Settings pane `pane_id`, while its
    /// theme session waits for a decision.
    pub(super) fn render_theme_bar(
        &self,
        pane_id: &PaneId,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let catalog = self
            .theme_catalog
            .as_ref()
            .filter(|catalog| &catalog.pane == pane_id && catalog.awaiting_decision())?;
        let colors = *self.colors();
        let row = RowCx {
            host: SettingsHost::Pane(pane_id.clone()),
            row: THEME_BAR_ROW,
            title: "Theme preview",
            compact: false,
            header: false,
        };
        let status = if catalog.remove_mode {
            "Removal does not change appearance. In-use themes are protected.".to_owned()
        } else {
            format!(
                "Previewing {}{}",
                catalog.candidate.label(),
                if catalog.target == CatalogTarget::Terminal {
                    " in terminals"
                } else {
                    ""
                }
            )
        };
        let decide = controls::text_button(
            "theme-apply",
            if catalog.remove_mode {
                if catalog.removal_confirmation {
                    "Confirm removal"
                } else {
                    "Remove selected"
                }
            } else {
                "Apply"
            },
        )
        .header()
        .key("enter")
        .loading(catalog.pending);
        Some(
            div()
                .flex_none()
                .px_4()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .border_t_1()
                .border_color(color(colors.border))
                .bg(color(colors.surface))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .text_color(color(modal_text_color(colors.muted, &colors)))
                        .child(status),
                )
                .child(
                    self.settings_control(
                        &row,
                        SettingsAction::ThemeCancel,
                        controls::text_button("theme-cancel", "Cancel")
                            .header()
                            .key("escape"),
                        cx,
                    ),
                )
                .child(self.settings_control(
                    &row,
                    SettingsAction::ThemeApply,
                    if catalog.remove_mode {
                        decide.danger()
                    } else {
                        decide.primary()
                    },
                    cx,
                ))
                .into_any_element(),
        )
    }
}

/// The narrowest a theme tile gets; the grid fits as many columns as this allows.
const THEME_TILE_MIN_WIDTH: f32 = 200.0;
/// The gap between theme tiles.
const THEME_TILE_GAP: f32 = 12.0;
/// The most columns the theme grid has, however wide.
const THEME_GRID_MAX_COLUMNS: usize = 4;
/// Between a group's label and its first row of tiles.
const THEME_GROUP_LABEL_GAP: f32 = 8.0;

/// How many columns the theme grid has `width` pixels across: as many tiles of the
/// minimum width as fit, at least one and at most four. They share the row.
pub(super) fn theme_grid_columns(width: f32) -> usize {
    let fit = ((width + THEME_TILE_GAP) / (THEME_TILE_MIN_WIDTH + THEME_TILE_GAP)).floor();
    (fit.max(1.0) as usize).min(THEME_GRID_MAX_COLUMNS)
}

/// A labelled run of the theme grid.
pub(super) struct ThemeGroup {
    pub(super) label: &'static str,
    pub(super) themes: Vec<Arc<ThemeDefinition>>,
}

/// `themes` in grid order: by family, then variant name, ignoring case, so a
/// family's variants stay together.
fn sort_themes(themes: &mut [Arc<ThemeDefinition>]) {
    themes.sort_by_cached_key(|theme| {
        (
            theme.family().to_lowercase(),
            theme.label().to_lowercase(),
            theme.id().to_owned(),
        )
    });
}

/// `visible` (already filtered, in family then variant order) as the grid's groups:
/// Yours (the `current` theme first, then `favorites`), then Dark and Light without
/// those, each keeping the given order. Empty groups are left out.
fn group_themes<'a>(
    visible: impl IntoIterator<Item = &'a Arc<ThemeDefinition>>,
    current: Option<&ThemeId>,
    favorites: &[ThemeId],
) -> Vec<ThemeGroup> {
    let mut yours = Vec::new();
    let mut dark = Vec::new();
    let mut light = Vec::new();
    for theme in visible {
        if Some(theme.theme_id()) == current {
            yours.insert(0, Arc::clone(theme));
        } else if favorites.contains(theme.theme_id()) {
            yours.push(Arc::clone(theme));
        } else if theme.is_dark() {
            dark.push(Arc::clone(theme));
        } else {
            light.push(Arc::clone(theme));
        }
    }
    [("Yours", yours), ("Dark", dark), ("Light", light)]
        .into_iter()
        .filter(|(_, themes)| !themes.is_empty())
        .map(|(label, themes)| ThemeGroup { label, themes })
        .collect()
}

/// A keyboard move in the theme grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GridStep {
    /// Left: the previous theme in display order.
    Previous,
    /// Right: the next theme in display order.
    Next,
    /// The same column of the row above.
    Up,
    /// The same column of the row below.
    Down,
}

/// The grid's rows as (first index, length) in display order: each group's themes
/// in rows of `columns`, every group starting a new row.
fn grid_rows(groups: &[ThemeGroup], columns: usize) -> Vec<(usize, usize)> {
    let columns = columns.max(1);
    let mut rows = Vec::new();
    let mut start = 0;
    for group in groups {
        let end = start + group.themes.len();
        rows.extend(
            (start..end)
                .step_by(columns)
                .map(|row| (row, columns.min(end - row))),
        );
        start = end;
    }
    rows
}

/// Where `step` takes the selection at `index` in a grid of `rows`. Left and Right
/// cross group boundaries and stop at the ends; Up and Down keep the column, taking
/// a shorter row's last tile, and stay on the first or last row. With nothing
/// selected, Left and Up pick the last theme and Right and Down the first.
fn grid_step(rows: &[(usize, usize)], index: Option<usize>, step: GridStep) -> Option<usize> {
    let &(start, length) = rows.last()?;
    let last = start + length - 1;
    let Some(index) = index.filter(|index| *index <= last) else {
        return Some(match step {
            GridStep::Previous | GridStep::Up => last,
            GridStep::Next | GridStep::Down => 0,
        });
    };
    let row = rows
        .iter()
        .position(|&(start, length)| (start..start + length).contains(&index))?;
    let target = match step {
        GridStep::Previous => return Some(index.saturating_sub(1)),
        GridStep::Next => return Some((index + 1).min(last)),
        GridStep::Up => row.checked_sub(1),
        GridStep::Down => Some(row + 1).filter(|row| *row < rows.len()),
    };
    Some(target.map_or(index, |target| {
        let (start, length) = rows[target];
        start + (index - rows[row].0).min(length - 1)
    }))
}

/// A theme at a glance on its own colors: the iris in its accent on its background,
/// with a few of its terminal colors. The opaque baseline; window material is a
/// separate preference.
fn theme_swatch(theme: &ThemeDefinition, height: f32) -> Stateful<Div> {
    let colors = theme.colors();
    let terminal = theme.terminal();
    div()
        .id(SharedString::from(format!("swatch-{}", theme.id())))
        .relative()
        .flex_none()
        .w_full()
        .h(px(height))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(color(colors.border))
        .overflow_hidden()
        .bg(material_color(colors.background, 1.0))
        .child(chrome_icon(ChromeIcon::Mark, color(colors.accent)))
        .child(
            div()
                .absolute()
                .left(px(8.0))
                .bottom(px(7.0))
                .flex()
                .gap(px(3.0))
                .children([1, 2, 3, 4, 5].map(|index| {
                    div()
                        .size(px(6.0))
                        .rounded_full()
                        .bg(color(terminal.ansi[index]))
                })),
        )
}

/// Whether `theme` passes the list's `filter` (`favorite` says whether it is starred)
/// and every word of the lowercased `query`, which may be in its id, name, family or
/// description, ignoring ASCII case.
pub(super) fn theme_matches(
    theme: &ThemeDefinition,
    filter: ThemeFilter,
    favorite: bool,
    query: &str,
) -> bool {
    let passes = match filter {
        ThemeFilter::All => true,
        ThemeFilter::Dark => theme.is_dark(),
        ThemeFilter::Light => !theme.is_dark(),
        ThemeFilter::Favorites => favorite,
    };
    passes
        && query.split_whitespace().all(|word| {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemePreset;

    #[test]
    fn theme_list_filters_and_searches() {
        let themes: Vec<_> = ThemePreset::ALL
            .iter()
            .map(|preset| preset.definition())
            .collect();
        let dark = themes.iter().find(|theme| theme.is_dark()).unwrap();
        let light = themes.iter().find(|theme| !theme.is_dark()).unwrap();
        assert!(theme_matches(dark, ThemeFilter::All, false, ""));
        assert!(theme_matches(dark, ThemeFilter::Dark, false, ""));
        assert!(!theme_matches(dark, ThemeFilter::Light, false, ""));
        assert!(theme_matches(light, ThemeFilter::Light, false, ""));
        assert!(!theme_matches(light, ThemeFilter::Favorites, false, ""));
        assert!(theme_matches(light, ThemeFilter::Favorites, true, ""));
        // Words match the name or family in any case, and all must match.
        let name = dark.label().to_lowercase();
        let first = name.split_whitespace().next().unwrap();
        assert!(theme_matches(dark, ThemeFilter::All, false, first));
        assert!(theme_matches(
            dark,
            ThemeFilter::All,
            false,
            &dark.family().to_uppercase()
        ));
        assert!(!theme_matches(
            dark,
            ThemeFilter::All,
            false,
            &format!("{first} zzzqqq")
        ));
        assert!(!theme_matches(dark, ThemeFilter::Dark, false, "zzzqqq"));
    }

    #[test]
    fn the_grid_groups_yours_then_dark_and_light_with_families_together() {
        let mut themes: Vec<_> = ThemePreset::ALL
            .into_iter()
            .map(ThemePreset::definition)
            .collect();
        sort_themes(&mut themes);
        let current = themes.iter().find(|theme| !theme.is_dark()).unwrap();
        let favorite = themes.iter().rev().find(|theme| theme.is_dark()).unwrap();
        let (current, favorite) = (current.theme_id().clone(), favorite.theme_id().clone());
        let groups = group_themes(&themes, Some(&current), std::slice::from_ref(&favorite));
        let labels: Vec<_> = groups.iter().map(|group| group.label).collect();
        assert_eq!(labels, ["Yours", "Dark", "Light"]);
        let ids: Vec<_> = groups[0]
            .themes
            .iter()
            .map(|theme| theme.theme_id().clone())
            .collect();
        assert_eq!(ids, [current.clone(), favorite.clone()]);
        // Every theme shows once: Dark and Light leave out what Yours holds.
        let shown: usize = groups.iter().map(|group| group.themes.len()).sum();
        assert_eq!(shown, themes.len());
        assert!(groups[1].themes.iter().all(|theme| theme.is_dark()));
        assert!(groups[2].themes.iter().all(|theme| !theme.is_dark()));
        assert!(groups[1..].iter().all(|group| {
            group
                .themes
                .iter()
                .all(|theme| ![&current, &favorite].contains(&theme.theme_id()))
        }));
        // A family's variants stay together: family, then variant order.
        for group in &groups[1..] {
            let keys: Vec<_> = group
                .themes
                .iter()
                .map(|theme| (theme.family().to_lowercase(), theme.label().to_lowercase()))
                .collect();
            assert!(keys.windows(2).all(|pair| pair[0] <= pair[1]));
        }
        // Empty groups are left out.
        let dark: Vec<_> = themes
            .iter()
            .filter(|theme| theme.is_dark())
            .cloned()
            .collect();
        let labels: Vec<_> = group_themes(&dark, None, &[])
            .iter()
            .map(|group| group.label)
            .collect();
        assert_eq!(labels, ["Dark"]);
        assert!(group_themes(&[], Some(&current), &[]).is_empty());
    }

    #[test]
    fn arrows_cross_groups_and_up_and_down_keep_the_column() {
        let theme = ThemePreset::CompiNeutral.definition();
        let groups: Vec<_> = [("Yours", 1), ("Dark", 5), ("Light", 2)]
            .into_iter()
            .map(|(label, count)| ThemeGroup {
                label,
                themes: vec![theme.clone(); count],
            })
            .collect();
        // Each group starts a row: [0] / [1 2 3] [4 5] / [6 7].
        let rows = grid_rows(&groups, 3);
        assert_eq!(rows, [(0, 1), (1, 3), (4, 2), (6, 2)]);
        let step = |index, step| grid_step(&rows, Some(index), step).unwrap();
        // Left and Right follow display order across groups and stop at the ends.
        assert_eq!(step(0, GridStep::Next), 1);
        assert_eq!(step(1, GridStep::Previous), 0);
        assert_eq!(step(0, GridStep::Previous), 0);
        assert_eq!(step(7, GridStep::Next), 7);
        // Up and Down keep the column across group boundaries, clamped to short rows.
        assert_eq!(step(0, GridStep::Down), 1);
        assert_eq!(step(3, GridStep::Down), 5);
        assert_eq!(step(5, GridStep::Down), 7);
        assert_eq!(step(4, GridStep::Down), 6);
        assert_eq!(step(3, GridStep::Up), 0);
        assert_eq!(step(6, GridStep::Up), 4);
        assert_eq!(step(7, GridStep::Down), 7);
        assert_eq!(step(0, GridStep::Up), 0);
        // Nothing selected: Up and Left start at the end, Down and Right at the start.
        assert_eq!(grid_step(&rows, None, GridStep::Up), Some(7));
        assert_eq!(grid_step(&rows, None, GridStep::Down), Some(0));
        assert_eq!(grid_step(&[], None, GridStep::Next), None);
        // Columns: tiles of at least 200 px, one to four of them.
        assert_eq!(theme_grid_columns(0.0), 1);
        assert_eq!(theme_grid_columns(411.0), 1);
        assert_eq!(theme_grid_columns(412.0), 2);
        assert_eq!(theme_grid_columns(5000.0), 4);
    }
}
