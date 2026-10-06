use super::*;
use crate::theme::{ThemeDefinition, ThemeId};
use crate::theme_store::ThemeLibrary;
use gpui::PathPromptOptions;

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

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ThemeFilter {
    #[default]
    All,
    Dark,
    Light,
    Favorites,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum CatalogTarget {
    Terminal,
    #[default]
    Both,
}

pub(in crate::gui) struct CatalogState {
    pub(super) original: AppearanceSettings,
    accepted: AppearanceSettings,
    pub(super) candidate: Arc<ThemeDefinition>,
    pub(super) target: CatalogTarget,
    ordered_themes: Vec<Arc<ThemeDefinition>>,
    scope: SettingsScope,
    filter: ThemeFilter,
    licenses: bool,
    menu_open: bool,
    previewed: bool,
    pending: bool,
    operation_id: u64,
    error: Option<String>,
    status: Option<String>,
    remove_mode: bool,
    removal_candidate: Option<ThemeId>,
    removal_confirmation: bool,
}

impl CompiApp {
    pub(super) fn open_theme_catalog(&mut self, scope: SettingsScope) {
        self.open_theme_catalog_target(scope, CatalogTarget::Both);
    }

    pub(super) fn open_theme_catalog_target(
        &mut self,
        scope: SettingsScope,
        target: CatalogTarget,
    ) {
        let parent = self
            .overlay
            .clone()
            .filter(|overlay| matches!(overlay, Overlay::Settings | Overlay::QuickAppearance));
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
        self.open_overlay(Overlay::ThemeCatalog, "");
        self.overlay_return = parent;
        self.settings_scope = scope;
        self.theme_catalog = Some(CatalogState {
            original,
            accepted,
            candidate,
            ordered_themes,
            scope,
            target,
            filter: ThemeFilter::All,
            licenses: false,
            menu_open: false,
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

    pub(super) fn cancel_catalog_preview(&mut self) {
        if let Some(catalog) = self.theme_catalog.take() {
            self.restore_catalog_axes(&catalog.original);
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
        themes.sort_by_cached_key(|theme| (theme.label().to_lowercase(), theme.id().to_owned()));
        themes
    }

    fn catalog_themes(&self) -> impl Iterator<Item = Arc<ThemeDefinition>> + '_ {
        let catalog = self.theme_catalog.as_ref();
        let filter = catalog.map_or(ThemeFilter::All, |catalog| catalog.filter);
        let remove_mode = catalog.is_some_and(|catalog| catalog.remove_mode);
        let ordered: &[Arc<ThemeDefinition>] =
            catalog.map_or(&[][..], |catalog| catalog.ordered_themes.as_slice());
        let current_theme = self.catalog_current_theme();
        let pinned = current_theme
            .clone()
            .into_iter()
            .filter(move |_| !remove_mode);
        let visible = move |theme: &Arc<ThemeDefinition>| {
            if !remove_mode
                && current_theme
                    .as_ref()
                    .is_some_and(|current| theme.theme_id() == current.theme_id())
            {
                return false;
            }
            let visible = if remove_mode {
                theme.is_imported()
            } else {
                match filter {
                    ThemeFilter::All => true,
                    ThemeFilter::Dark => theme.is_dark(),
                    ThemeFilter::Light => !theme.is_dark(),
                    ThemeFilter::Favorites => {
                        self.config.theme_favorites.contains(theme.theme_id())
                    }
                }
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
        };
        let favorite_visible = visible.clone();
        let favorites = ordered
            .iter()
            .filter(move |theme| {
                self.config.theme_favorites.contains(theme.theme_id()) && favorite_visible(theme)
            })
            .cloned();
        let others = ordered
            .iter()
            .filter(move |theme| {
                !self.config.theme_favorites.contains(theme.theme_id()) && visible(theme)
            })
            .cloned();
        pinned.chain(favorites).chain(others)
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

    pub(super) fn move_catalog_selection(&mut self, backwards: bool) {
        let Some(catalog) = &self.theme_catalog else {
            return;
        };
        if catalog.pending || CATALOG_STORE_BUSY.load(Ordering::Acquire) {
            return;
        }
        let remove_mode = catalog.remove_mode;
        let themes: Vec<_> = self.catalog_themes().collect();
        if themes.is_empty() {
            return;
        }
        let id = if catalog.remove_mode {
            catalog.removal_candidate.as_ref()
        } else {
            Some(catalog.candidate.theme_id())
        };
        let index = themes.iter().position(|theme| Some(theme.theme_id()) == id);
        let next = match index {
            Some(index) if backwards => (index + themes.len() - 1) % themes.len(),
            Some(index) => (index + 1) % themes.len(),
            None if backwards => themes.len() - 1,
            None => 0,
        };
        if catalog.remove_mode {
            if let Some(catalog) = &mut self.theme_catalog {
                catalog.removal_candidate = Some(themes[next].theme_id().clone());
                catalog.removal_confirmation = false;
            }
        } else {
            self.preview_catalog_theme(themes[next].clone());
        }
        self.overlay_scroll.scroll_to_item(if remove_mode {
            next
        } else {
            next.saturating_sub(1)
        });
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
        // Search/list, compact filters, import, menu, cancel, apply, selected star.
        // An open menu adds attribution, export and local removal.
        let menu_open = self
            .theme_catalog
            .as_ref()
            .is_some_and(|catalog| catalog.menu_open);
        let focus_count = if menu_open { 13 } else { 10 };
        match key.key.as_str() {
            "escape" if menu_open => {
                if let Some(catalog) = &mut self.theme_catalog {
                    catalog.menu_open = false;
                }
                self.overlay_focus = 6;
            }
            "escape" => self.dismiss_overlay(),
            "tab" => {
                self.overlay_focus = if key.modifiers.shift {
                    (self.overlay_focus + focus_count - 1) % focus_count
                } else {
                    (self.overlay_focus + 1) % focus_count
                };
                if self.overlay_focus == 9 {
                    let selected = self.catalog_action_theme();
                    let visible = selected.as_ref().is_some_and(|selected| {
                        self.catalog_themes()
                            .any(|theme| theme.theme_id() == selected.theme_id())
                    });
                    if !visible {
                        self.move_catalog_selection(false);
                    }
                }
            }
            "up" | "down" if self.overlay_focus == 0 || self.overlay_focus == 9 => {
                self.move_catalog_selection(key.key == "up");
            }
            "up" | "down" if menu_open && (10..=12).contains(&self.overlay_focus) => {
                self.overlay_focus = if key.key == "up" {
                    if self.overlay_focus == 10 {
                        12
                    } else {
                        self.overlay_focus - 1
                    }
                } else if self.overlay_focus == 12 {
                    10
                } else {
                    self.overlay_focus + 1
                };
            }
            "left" | "right" if (1..=4).contains(&self.overlay_focus) => {
                let (first, last) = (1, 4);
                self.overlay_focus = if key.key == "left" {
                    if self.overlay_focus == first {
                        last
                    } else {
                        self.overlay_focus - 1
                    }
                } else if self.overlay_focus == last {
                    first
                } else {
                    self.overlay_focus + 1
                };
                self.activate_catalog_control(self.overlay_focus, window, cx);
            }
            "space" if self.overlay_focus == 0 => return false,
            "enter" | "space" => self.activate_catalog_control(self.overlay_focus, window, cx),
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

    fn activate_catalog_control(
        &mut self,
        focus: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if focus != 7
            && self
                .theme_catalog
                .as_ref()
                .is_some_and(|catalog| catalog.pending)
        {
            return;
        }
        match focus {
            0 | 8 => {
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
            1..=4 => {
                if let Some(catalog) = &mut self.theme_catalog {
                    if catalog.remove_mode {
                        catalog.status = None;
                        catalog.error = None;
                    }
                    catalog.filter = [
                        ThemeFilter::All,
                        ThemeFilter::Dark,
                        ThemeFilter::Light,
                        ThemeFilter::Favorites,
                    ][focus - 1];
                    catalog.licenses = false;
                    catalog.remove_mode = false;
                    catalog.menu_open = false;
                    catalog.removal_confirmation = false;
                }
                self.overlay_scroll.set_offset(point(px(0.0), px(0.0)));
            }
            5 => self.import_catalog_theme(window, cx),
            6 => {
                if let Some(catalog) = &mut self.theme_catalog {
                    catalog.menu_open = !catalog.menu_open;
                    self.overlay_focus = if catalog.menu_open { 10 } else { 6 };
                }
            }
            7 => self.dismiss_overlay(),
            10 => {
                if let Some(catalog) = &mut self.theme_catalog {
                    catalog.licenses = !catalog.licenses;
                    catalog.menu_open = false;
                    self.overlay_focus = 6;
                }
            }
            9 => {
                if let Some(theme) = self.catalog_action_theme() {
                    self.toggle_theme_favorite(theme.theme_id().clone(), window, cx);
                }
            }
            11 => {
                if let Some(catalog) = &mut self.theme_catalog {
                    catalog.menu_open = false;
                }
                self.overlay_focus = 6;
                self.export_catalog_theme(cx);
            }
            12 => {
                self.toggle_catalog_removal();
                self.overlay_focus = 0;
            }
            _ => {}
        }
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
        if let Some(catalog) = &mut self.theme_catalog {
            if catalog.pending {
                return;
            }
            catalog.remove_mode = !catalog.remove_mode;
            catalog.licenses = false;
            catalog.menu_open = false;
            catalog.removal_candidate = first;
            catalog.removal_confirmation = false;
            catalog.error = None;
            catalog.status = catalog
                .remove_mode
                .then(|| "Select an unused local theme. Its source file is kept.".into());
        }
        self.ime_text.clear();
        self.overlay_scroll.set_offset(point(px(0.0), px(0.0)));
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
        let mut appearance = catalog.accepted.clone();
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
        self.dismiss_overlay();
        window.focus(&self.focus_handle);
        cx.notify();
    }

    pub(super) fn sync_global_appearance(
        &mut self,
        appearance: AppearanceSettings,
        favorites: Vec<ThemeId>,
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
        self.config.configured_appearance = appearance.clone();
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

    pub(super) fn render_theme_catalog_entry_target(
        &self,
        theme: Arc<ThemeDefinition>,
        target: CatalogTarget,
        focused: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let locked = self.config.provenance.theme == crate::config::ValueSource::CommandLine;
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
                div().flex_1().min_w_0().flex().flex_col().gap_1().child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(theme.label().to_owned()),
                ),
            )
            .child(
                div()
                    .id(("browse-theme-catalog", target as usize))
                    .flex_none()
                    .min_w(px(72.0))
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
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_theme_catalog_target(this.settings_scope, target);
                                cx.stop_propagation();
                                cx.notify();
                            }))
                    })
                    .child("Change"),
            )
            .into_any_element()
    }

    fn theme_sample(theme: &ThemeDefinition) -> AnyElement {
        let colors = theme.colors();
        let terminal = theme.terminal();
        // Show the opaque color baseline; window material is a separate preference.
        div()
            .w(gpui::relative(0.5))
            .flex_none()
            .rounded_sm()
            .border_1()
            .border_color(color(colors.border))
            .overflow_hidden()
            .bg(material_color(colors.background, 1.0))
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
                            .text_color(color(ui_text_color(
                                colors.foreground,
                                composite_rgb(colors.surface, colors.background & 0x00ff_ffff),
                            )))
                            .child("compi / src"),
                    ),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .bg(material_color(terminal.background, 1.0))
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
                                        terminal.ansi[6],
                                        terminal.background & 0x00ff_ffff,
                                    )))
                                    .child("$"),
                            )
                            .child(
                                div()
                                    .text_color(color(terminal.foreground))
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
                                        terminal.ansi[2],
                                        terminal.background & 0x00ff_ffff,
                                    )))
                                    .child("main"),
                            )
                            .child(
                                div()
                                    .text_color(color(terminal.foreground))
                                    .child("working tree clean"),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn catalog_action_button(
        &self,
        id: &'static str,
        label: &'static str,
        focus: usize,
        destructive: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        self.settings_action_button(
            id,
            label,
            self.overlay_focus == focus,
            destructive,
            cx.listener(move |this, _, window, cx| {
                this.overlay_focus = focus;
                this.activate_catalog_control(focus, window, cx);
                cx.stop_propagation();
                cx.notify();
            }),
        )
    }

    pub(super) fn render_theme_catalog(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let Some(catalog) = &self.theme_catalog else {
            return div().into_any_element();
        };
        let colors = *self.colors();
        let (viewport_width, viewport_height) = overlay_viewport_size(window);
        let panel_width = (viewport_width - 32.0).clamp(1.0, 780.0);
        let panel_height = (viewport_height - 32.0).clamp(1.0, 700.0);
        let compact_footer = panel_width < 520.0;
        let candidate = &catalog.candidate;
        let action_theme = self.catalog_action_theme();
        let current_theme = self.catalog_current_theme();
        let current_id = current_theme.as_ref().map(|theme| theme.theme_id());
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
                catalog.filter == filter && !catalog.remove_mode,
                self.overlay_focus == index + 1,
                cx.listener(move |this, _, window, cx| {
                    this.overlay_focus = index + 1;
                    this.activate_catalog_control(index + 1, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
        });
        let render_row = |theme: Arc<ThemeDefinition>| {
            let selected = if catalog.remove_mode {
                catalog.removal_candidate.as_ref() == Some(theme.theme_id())
            } else {
                theme.theme_id() == candidate.theme_id()
            };
            let current = Some(theme.theme_id()) == current_id;
            let favorite = self.config.theme_favorites.contains(theme.theme_id());
            let preview = theme.clone();
            let favorite_id = theme.theme_id().clone();
            let row_id = gpui::SharedString::from(format!("catalog-theme-{}", theme.id()));
            let star_id = gpui::SharedString::from(format!("catalog-favorite-{}", theme.id()));
            div()
                .id(row_id)
                .px_3()
                .py_2()
                .flex()
                .flex_wrap()
                .gap_3()
                .border_b_1()
                .border_color(color(colors.border))
                .bg(color(if selected {
                    blend_rgb(colors.surface, colors.accent, 0.14)
                } else {
                    colors.surface
                }))
                .cursor_pointer()
                .hover(move |style| style.bg(color(colors.surface_hover)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.overlay_focus = 0;
                    if let Some(catalog) = &mut this.theme_catalog {
                        if catalog.pending {
                            return;
                        }
                        if catalog.remove_mode {
                            catalog.removal_candidate = Some(preview.theme_id().clone());
                            catalog.removal_confirmation = false;
                        } else {
                            this.preview_catalog_theme(preview.clone());
                        }
                    }
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(100.0))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .min_w_0()
                                .w_full()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .text_ellipsis()
                                        .font_weight(if selected {
                                            FontWeight::SEMIBOLD
                                        } else {
                                            FontWeight::MEDIUM
                                        })
                                        .text_color(color(modal_text_color(
                                            colors.foreground,
                                            &colors,
                                        )))
                                        .child(theme.label().to_owned()),
                                )
                                .when(current, |title| {
                                    title.child(
                                        div()
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .text_size(px(UI_MICRO_TEXT_SIZE))
                                            .line_height(px(UI_MICRO_LINE_HEIGHT))
                                            .text_color(color(modal_text_color(
                                                colors.foreground,
                                                &colors,
                                            )))
                                            .child(
                                                div()
                                                    .size(px(14.0))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .rounded_full()
                                                    .border_1()
                                                    .border_color(color(colors.accent))
                                                    .child(
                                                        div()
                                                            .size(px(6.0))
                                                            .rounded_full()
                                                            .bg(color(colors.accent)),
                                                    ),
                                            )
                                            .child("Current"),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(UI_SMALL_TEXT_SIZE))
                                .line_height(px(UI_SMALL_LINE_HEIGHT))
                                .text_color(color(modal_text_color(colors.muted, &colors)))
                                .child(format!(
                                    "{} · {}{}",
                                    theme.family(),
                                    if theme.is_dark() { "Dark" } else { "Light" },
                                    if theme.is_imported() { " · Local" } else { "" }
                                )),
                        ),
                )
                .when(panel_width >= 440.0, |row| {
                    row.child(Self::theme_sample(&theme))
                })
                .child(
                    div()
                        .id(star_id)
                        .flex_none()
                        .min_w(px(32.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .when(selected && self.overlay_focus == 9, |star| {
                            star.bg(color(blend_rgb(colors.surface, colors.accent, 0.18)))
                        })
                        .px_1()
                        .py_2()
                        .text_size(px(19.0))
                        .text_color(color(modal_text_color(
                            if favorite {
                                colors.accent
                            } else {
                                colors.muted
                            },
                            &colors,
                        )))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.toggle_theme_favorite(favorite_id.clone(), window, cx);
                            cx.stop_propagation();
                        }))
                        .child(if favorite { "★" } else { "☆" }),
                )
                .into_any_element()
        };
        let mut themes = self
            .catalog_themes()
            .filter(|theme| catalog.remove_mode || Some(theme.theme_id()) != current_id)
            .peekable();
        let has_results = themes.peek().is_some();
        let rows = themes.map(&render_row);
        let body = if catalog.licenses {
            let selected_attribution = action_theme.as_ref().unwrap_or(candidate).attribution();
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
                .line_height(px(UI_SMALL_LINE_HEIGHT))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(self.settings_subheading("Selected theme attribution"))
                        .child(selected_attribution),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(self.settings_subheading("Bundled theme licenses"))
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
                .when(!has_results, |body| {
                    body.child(
                        div()
                            .px_3()
                            .py_3()
                            .text_color(color(modal_text_color(colors.muted, &colors)))
                            .child(if catalog.remove_mode {
                                "No matching local themes to remove."
                            } else {
                                "No matching themes. Try another search or filter."
                            }),
                    )
                })
                .into_any_element()
        };
        let preview_status = if catalog.remove_mode {
            "Removal does not change appearance. In-use themes are protected.".to_owned()
        } else if self.config.provenance.theme == crate::config::ValueSource::CommandLine {
            "Colors are fixed by --theme. Import, export and favorites remain available.".to_owned()
        } else {
            format!(
                "{} {}. Apply to keep these colors.",
                if catalog.previewed {
                    "Previewing"
                } else {
                    "Selected"
                },
                candidate.label(),
            )
        };
        div()
            .absolute()
            .top_0()
            .left_0()
            .w(px(viewport_width))
            .h(px(viewport_height.max(1.0)))
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
                    .text_size(px(UI_BODY_TEXT_SIZE))
                    .line_height(px(UI_BODY_LINE_HEIGHT))
                    .text_color(color(modal_text_color(colors.foreground, &colors)))
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex_none()
                            .p_3()
                            .flex()
                            .flex_wrap()
                            .justify_between()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(18.0))
                                    .line_height(px(24.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(if catalog.target == CatalogTarget::Terminal {
                                        "Terminal colors"
                                    } else {
                                        "Theme"
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .line_height(px(UI_SMALL_LINE_HEIGHT))
                                    .text_color(color(modal_text_color(colors.muted, &colors)))
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
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .when(compact_footer, |filters| filters.w_full().flex_wrap())
                                    .gap_1()
                                    .children(filters),
                            )
                            .child(div().flex_1().min_w_0())
                            .child(self.catalog_action_button(
                                "import-theme",
                                "Import",
                                5,
                                false,
                                cx,
                            ))
                            .child(self.catalog_action_button("theme-menu", "More", 6, false, cx)),
                    )
                    .when(catalog.menu_open, |panel| {
                        panel.child(
                            div()
                                .flex_none()
                                .px_3()
                                .py_2()
                                .flex()
                                .flex_wrap()
                                .gap_2()
                                .border_b_1()
                                .border_color(color(colors.border))
                                .child(self.catalog_action_button(
                                    "theme-licenses",
                                    if catalog.licenses {
                                        "Back to themes"
                                    } else {
                                        "Attribution"
                                    },
                                    10,
                                    false,
                                    cx,
                                ))
                                .child(self.catalog_action_button(
                                    "export-theme",
                                    "Export selected",
                                    11,
                                    false,
                                    cx,
                                ))
                                .child(self.catalog_action_button(
                                    "remove-themes",
                                    if catalog.remove_mode {
                                        "Back to themes"
                                    } else {
                                        "Remove local"
                                    },
                                    12,
                                    true,
                                    cx,
                                )),
                        )
                    })
                    .when(!catalog.licenses && !catalog.remove_mode, |panel| {
                        panel.child(
                            div()
                                .flex_none()
                                .when_some(current_theme.as_ref(), |section, theme| {
                                    section.child(render_row(theme.clone()))
                                }),
                        )
                    })
                    .child(body)
                    .when(
                        catalog.pending || catalog.error.is_some() || catalog.status.is_some(),
                        |panel| {
                            panel.child(
                                div()
                                    .flex_none()
                                    .min_w_0()
                                    .px_3()
                                    .py_3()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .line_height(px(UI_SMALL_LINE_HEIGHT))
                                    .text_color(color(modal_text_color(colors.muted, &colors)))
                                    .when(catalog.pending, |feedback| {
                                        feedback.child("Waiting for dialog or local theme store…")
                                    })
                                    .when_some(catalog.error.as_ref(), |feedback, error| {
                                        feedback.child(
                                            div()
                                                .text_color(color(modal_text_color(
                                                    colors.error,
                                                    &colors,
                                                )))
                                                .child(error.clone()),
                                        )
                                    })
                                    .when_some(catalog.status.as_ref(), |feedback, status| {
                                        feedback.child(div().child(status.clone()))
                                    }),
                            )
                        },
                    )
                    .child(
                        div()
                            .flex_none()
                            .p_3()
                            .border_t_1()
                            .border_color(color(colors.border))
                            .flex()
                            .gap_3()
                            .when(compact_footer, |footer| footer.flex_col().items_start())
                            .when(!compact_footer, |footer| footer.items_center())
                            .child(
                                div()
                                    .min_w_0()
                                    .when(compact_footer, |text| text.w_full())
                                    .when(!compact_footer, |text| text.flex_1())
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .line_height(px(UI_SMALL_LINE_HEIGHT))
                                    .text_color(color(modal_text_color(colors.muted, &colors)))
                                    .child(preview_status),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .when(compact_footer, |actions| actions.w_full().justify_end())
                                    .child(self.catalog_action_button(
                                        "cancel-theme-preview",
                                        "Cancel",
                                        7,
                                        false,
                                        cx,
                                    ))
                                    .child(self.settings_primary_button(
                                        "apply-catalog-theme",
                                        if catalog.pending {
                                            "Working…"
                                        } else if catalog.remove_mode {
                                            if catalog.removal_confirmation {
                                                "Confirm removal"
                                            } else {
                                                "Remove selected"
                                            }
                                        } else {
                                            "Apply"
                                        },
                                        self.overlay_focus == 8,
                                        cx.listener(|this, _, window, cx| {
                                            this.overlay_focus = 8;
                                            this.activate_catalog_control(8, window, cx);
                                            cx.stop_propagation();
                                            cx.notify();
                                        }),
                                    )),
                            ),
                    ),
            )
            .into_any_element()
    }
}
