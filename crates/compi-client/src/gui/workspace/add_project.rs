//! The shared Add project palette. Folder completions and writes always go to the
//! selected daemon, independently of the active terminal or any open shell.

use super::*;
use crate::content_path;
use crate::gui::controls::IconName;
use compi_protocol::{ContentEnvironment, Project};
use gpui::{ScrollStrategy, Task, UniformListScrollHandle, uniform_list};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Icon, ThemeStyled as _};

const ROW_HEIGHT: f32 = 28.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Folder,
    Clone,
}

struct FolderName {
    name: SharedString,
    search: String,
}

struct Listing {
    folders: Option<Result<Vec<FolderName>, String>>,
    parent: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Work {
    Browse,
    Resolve,
    Clone,
}

pub(in crate::gui) struct AddProject {
    from_settings: bool,
    source: Source,
    environments: Vec<(ContentEnvironment, Option<String>)>,
    environment: usize,
    environments_loading: bool,
    environments_error: Option<String>,
    base: Option<String>,
    windows: bool,
    path: gpui::Entity<InputState>,
    url: gpui::Entity<InputState>,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    browse_available: bool,
    listing: Option<Listing>,
    listing_task: Option<Task<()>>,
    matches: Vec<usize>,
    highlight: Option<usize>,
    scroll: UniformListScrollHandle,
    work: Option<Work>,
    error: Option<String>,
    // A clone can succeed before saving the configuration fails. Retry the save,
    // not the clone, until the user changes the form.
    resolved: Option<Project>,
    _events: [Subscription; 2],
}

impl AddProject {
    fn environment(&self) -> &ContentEnvironment {
        &self.environments[self.environment].0
    }

    fn separator(&self) -> char {
        if self.windows { '\\' } else { '/' }
    }

    fn is_separator(&self, c: char) -> bool {
        c == '/' || self.windows && c == '\\'
    }

    fn folder_input<'a>(&self, path: &'a str) -> (Option<&str>, &'a str) {
        if let Some(rest) = path.strip_prefix('~')
            && (rest.is_empty() || rest.starts_with(|c| self.is_separator(c)))
        {
            return (
                self.environments[self.environment].1.as_deref(),
                rest.trim_start_matches(|c| self.is_separator(c)),
            );
        }
        (self.base.as_deref(), path)
    }

    fn folder(&self, path: &str) -> Option<String> {
        if path.is_empty() {
            return None;
        }
        let (base, path) = self.folder_input(path);
        if self.windows
            && let Some(root) = drive_root(path)
        {
            return Some(root);
        }
        content_path::resolve(base, path, self.windows).ok()
    }

    /// Split before normalizing so the last component can filter its siblings.
    fn completion_query<'a>(&self, path: &'a str) -> Option<(String, &'a str)> {
        let (base, rest) = self.folder_input(path);
        if self.windows {
            if let Some(root) = drive_root(rest) {
                return Some((root, ""));
            }
            // A bare UNC share is an absolute root, not a completion prefix.
            if (rest.starts_with("\\\\") || rest.starts_with("//"))
                && let Ok(root) = content_path::resolve(None, rest, true)
                && root.ends_with('\\')
            {
                return Some((root, ""));
            }
        }
        let (parent, prefix) = match rest.rfind(|c| self.is_separator(c)) {
            Some(at) => (&rest[..=at], &rest[at + 1..]),
            None if matches!(rest, "." | "..") => (rest, ""),
            None => ("", rest),
        };
        Some((
            content_path::resolve(base, parent, self.windows).ok()?,
            prefix,
        ))
    }

    fn destination(&self, cx: &App) -> Option<String> {
        let folder = self.folder(self.path.read(cx).value().as_str())?;
        if self.source == Source::Folder {
            return Some(folder);
        }
        let url = self.url.read(cx).value();
        let name = clone_name(url.trim())?;
        content_path::resolve(Some(&folder), name, self.windows).ok()
    }

    fn selected_folder(&self, index: usize) -> Option<String> {
        let listing = self.listing.as_ref()?;
        let folder = listing
            .folders
            .as_ref()?
            .as_ref()
            .ok()?
            .get(*self.matches.get(index)?)?;
        content_path::resolve(Some(&listing.parent), folder.name.as_str(), self.windows).ok()
    }

    fn filter(&mut self, prefix: &str) {
        self.matches.clear();
        self.highlight = None;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        let Some(Ok(folders)) = self
            .listing
            .as_ref()
            .and_then(|listing| listing.folders.as_ref())
        else {
            return;
        };
        let prefix = prefix.to_lowercase();
        self.matches.extend(
            folders
                .iter()
                .enumerate()
                .filter_map(|(index, folder)| folder.search.starts_with(&prefix).then_some(index)),
        );
    }
}

/// For the preview only. The daemon validates the Git transport before cloning.
fn clone_name(url: &str) -> Option<&str> {
    if url.is_empty() || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let path = if let Some((scheme, rest)) = url.split_once("://") {
        if !["https", "http", "ssh"]
            .iter()
            .any(|supported| scheme.eq_ignore_ascii_case(supported))
        {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        if authority.is_empty() || rest.contains(['?', '#']) {
            return None;
        }
        path
    } else {
        let colon = url.rfind("]:").map(|at| at + 1).or_else(|| url.find(':'))?;
        let host = &url[..colon];
        if host.len() < 2 || host.starts_with('-') || host.contains(['/', '\\']) {
            return None;
        }
        &url[colon + 1..]
    };
    let name = path.trim_end_matches('/').rsplit('/').next()?;
    let name = name.strip_suffix(".git").unwrap_or(name);
    (!matches!(name, "" | "." | "..") && !name.contains(['\\', ':'])).then_some(name)
}

fn directory_text(mut path: String, separator: char) -> String {
    if !path.ends_with(separator) {
        path.push(separator);
    }
    path
}

fn drive_root(path: &str) -> Option<String> {
    match path.as_bytes() {
        [drive, b':'] if drive.is_ascii_alphabetic() => Some(format!("{path}\\")),
        _ => None,
    }
}

#[cfg(windows)]
fn native_folder(environment: &ContentEnvironment, path: String) -> crate::Result<String> {
    let ContentEnvironment::Wsl { distribution } = environment else {
        return Ok(path);
    };
    if let Some(rest) = path.strip_prefix("\\\\") {
        let mut components = rest.splitn(3, '\\');
        if let Some(server) = components.next()
            && (server.eq_ignore_ascii_case("wsl.localhost") || server.eq_ignore_ascii_case("wsl$"))
        {
            if !components
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(distribution))
            {
                return Err(format!(
                    "Choose a folder in {distribution}, or change the environment"
                )
                .into());
            }
            return Ok(format!(
                "/{}",
                components.next().unwrap_or("").replace('\\', "/")
            ));
        }
    }
    compi_protocol::wsl::wsl_path_for_windows(&path, distribution)
}

#[cfg(not(windows))]
fn native_folder(_: &ContentEnvironment, path: String) -> crate::Result<String> {
    Ok(path)
}

impl CompiApp {
    pub(super) fn open_add_project(
        &mut self,
        from_settings: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.add_project.is_some() {
            return;
        }
        let active = self.selected_project().cloned().or_else(|| {
            self.file_scope().ok().map(|scope| Project {
                environment: scope.environment,
                root: scope.folder,
            })
        });
        let (environment, base) = match active {
            Some(project) => (project.environment, Some(project.root)),
            None => {
                let environment = self
                    .focused_view()
                    .and_then(|view| self.workspace.as_ref()?.surface(&view.surface_id))
                    .map_or(ContentEnvironment::Host, ContentEnvironment::of_surface);
                (environment, None)
            }
        };
        let windows = self.windows_paths(&environment)
            || (environment == ContentEnvironment::Host
                && self.target.is_remote()
                && base
                    .as_ref()
                    .is_some_and(|path| content_path::resolve(None, path, true).is_ok()));
        let base = base.and_then(|path| content_path::resolve(None, &path, windows).ok());
        let initial = base
            .as_ref()
            .map(|path| directory_text(path.clone(), if windows { '\\' } else { '/' }))
            .unwrap_or_default();
        let path = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Enter a folder path")
                .default_value(initial)
        });
        let url = cx
            .new(|cx| InputState::new(window, cx).placeholder("https://host/owner/repository.git"));
        let path_events = cx.subscribe(&path, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.add_project_edited(cx);
                this.refresh_add_project_listing(cx);
            }
        });
        let url_events = cx.subscribe(&url, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.add_project_edited(cx);
            }
        });
        let previous_focus = window.focused(cx);
        let focus = cx.focus_handle();
        self.open_overlay(Overlay::AddProject, "");
        self.add_project = Some(AddProject {
            from_settings,
            source: Source::Folder,
            environments: vec![(environment, None)],
            environment: 0,
            environments_loading: true,
            environments_error: None,
            base,
            windows,
            path: path.clone(),
            url,
            focus,
            previous_focus,
            browse_available: !self.target.is_remote(),
            listing: None,
            listing_task: None,
            matches: Vec::new(),
            highlight: None,
            scroll: UniformListScrollHandle::new(),
            work: None,
            error: None,
            resolved: None,
            _events: [path_events, url_events],
        });
        path.update(cx, |path, cx| {
            path.focus(window, cx);
            path.select_all(window, cx);
        });
        self.refresh_add_project_listing(cx);
        let target = self.target.clone();
        let generation = self.overlay_generation;
        cx.spawn_in(window, async move |this, cx| {
            let environments = cx
                .background_executor()
                .spawn(async move {
                    target
                        .connect()
                        .and_then(|mut client| client.project_environments())
                        .map_err(|error| error.to_string())
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                if this.overlay_generation != generation || this.overlay_closing_since.is_some() {
                    return;
                }
                let Some(picker) = this.add_project.as_mut() else {
                    return;
                };
                picker.environments_loading = false;
                match environments {
                    Ok(mut environments) => {
                        let selected = picker.environment();
                        picker.environment = environments
                            .iter()
                            .position(|(environment, _)| environment == selected)
                            .unwrap_or_else(|| {
                                environments.push((selected.clone(), None));
                                environments.len() - 1
                            });
                        picker.environments = environments;
                        // Do not replace a folder the user has already typed.
                        if picker.base.is_none() {
                            this.set_add_project_base(window, cx);
                        } else {
                            this.refresh_add_project_listing(cx);
                        }
                    }
                    Err(error) => picker.environments_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn add_project_environment_label(&self, environment: &ContentEnvironment) -> String {
        if *environment != ContentEnvironment::Host || self.target.is_remote() {
            return self.environment_label(environment);
        }
        if cfg!(windows) {
            "Windows".into()
        } else if cfg!(target_os = "macos") {
            "macOS".into()
        } else {
            "Local host".into()
        }
    }

    fn set_add_project_base(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let windows_host = self.windows_paths(&ContentEnvironment::Host);
        let Some(picker) = self.add_project.as_mut() else {
            return;
        };
        let (environment, home) = &picker.environments[picker.environment];
        picker.windows = *environment == ContentEnvironment::Host
            && (windows_host || home.as_ref().is_some_and(|home| !home.starts_with('/')));
        picker.base = home
            .clone()
            .or_else(|| (!picker.windows).then(|| "/".into()));
        if picker.path.read(cx).value().is_empty()
            && let Some(base) = &picker.base
        {
            let path = directory_text(base.clone(), picker.separator());
            picker.path.update(cx, |field, cx| {
                field.set_value(path, window, cx);
                field.select_all(window, cx);
            });
        }
        self.refresh_add_project_listing(cx);
    }

    fn select_add_project_environment(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self.add_project.as_mut() else {
            return;
        };
        if picker.work.is_some()
            || index >= picker.environments.len()
            || index == picker.environment
        {
            return;
        }
        picker.environment = index;
        picker.listing_task = None;
        picker.listing = None;
        picker
            .path
            .update(cx, |field, cx| field.set_value("", window, cx));
        self.add_project_edited(cx);
        self.set_add_project_base(window, cx);
        cx.notify();
    }

    fn set_add_project_source(
        &mut self,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self.add_project.as_mut() else {
            return;
        };
        if picker.source == source || picker.work.is_some() {
            return;
        }
        picker.source = source;
        picker.highlight = None;
        let input = if source == Source::Clone {
            &picker.url
        } else {
            &picker.path
        };
        input.update(cx, |input, cx| input.focus(window, cx));
        self.add_project_edited(cx);
    }

    fn add_project_edited(&mut self, cx: &mut Context<Self>) {
        if let Some(picker) = self.add_project.as_mut() {
            picker.error = None;
            picker.resolved = None;
            cx.notify();
        }
    }

    fn refresh_add_project_listing(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.add_project.as_mut() else {
            return;
        };
        let path = picker.path.read(cx).value();
        let Some((parent, prefix)) = picker.completion_query(path.as_str()) else {
            picker.listing_task = None;
            picker.listing = None;
            picker.filter("");
            cx.notify();
            return;
        };
        if picker
            .listing
            .as_ref()
            .is_some_and(|listing| listing.parent == parent)
        {
            picker.filter(prefix);
            cx.notify();
            return;
        }
        let target = self.target.clone();
        let environment = picker.environment().clone();
        picker.listing = Some(Listing {
            parent: parent.clone(),
            folders: None,
        });
        picker.filter("");
        picker.listing_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let (parent, folders) = cx
                .background_executor()
                .spawn(async move {
                    let folders = target
                        .connect()
                        .and_then(|mut client| {
                            client.list_content_directory(environment, parent.clone())
                        })
                        .map(|entries| {
                            entries
                                .into_iter()
                                .filter(|entry| entry.is_directory)
                                .map(|entry| FolderName {
                                    search: entry.name.to_lowercase(),
                                    name: entry.name.into(),
                                })
                                .collect()
                        })
                        .map_err(|error| error.to_string());
                    (parent, folders)
                })
                .await;
            this.update(cx, |this, cx| {
                let Some(picker) = this.add_project.as_mut() else {
                    return;
                };
                let value = picker.path.read(cx).value();
                let Some((current, prefix)) = picker.completion_query(value.as_str()) else {
                    return;
                };
                if current == parent {
                    picker.listing = Some(Listing {
                        parent,
                        folders: Some(folders),
                    });
                    picker.filter(prefix);
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn navigate_add_project_folder(
        &mut self,
        folder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self.add_project.as_mut() else {
            return;
        };
        if picker.work.is_some() || self.overlay_closing_since.is_some() {
            return;
        }
        let path = directory_text(folder, picker.separator());
        picker.highlight = None;
        picker.path.update(cx, |field, cx| {
            field.set_value(path, window, cx);
            field.focus(window, cx);
        });
        self.add_project_edited(cx);
        self.refresh_add_project_listing(cx);
    }

    fn add_project_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = &self.add_project else {
            return;
        };
        let Some((folder, _)) = picker.completion_query(picker.path.read(cx).value().as_str())
        else {
            return;
        };
        let Ok(parent) = content_path::resolve(Some(&folder), "..", picker.windows) else {
            return;
        };
        if parent != folder {
            self.navigate_add_project_folder(parent, window, cx);
        }
    }

    fn browse_add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.add_project.as_mut() else {
            return;
        };
        if !picker.browse_available || picker.work.is_some() || self.overlay_closing_since.is_some()
        {
            return;
        }
        picker.work = Some(Work::Browse);
        picker.error = None;
        let environment = picker.environment().clone();
        let generation = self.overlay_generation;
        let prompt = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = match prompt.await {
                Ok(Ok(Some(folders))) => match folders.into_iter().next() {
                    Some(folder) => {
                        cx.background_executor()
                            .spawn(async move {
                                native_folder(&environment, folder.to_string_lossy().into_owned())
                                    .map(Some)
                                    .map_err(|error| error.to_string())
                            })
                            .await
                    }
                    None => Ok(None),
                },
                Ok(Ok(None)) => Ok(None),
                Ok(Err(error)) => Err(format!("Could not open the folder dialog: {error}")),
                Err(_) => Err("The folder dialog is unavailable. Enter a path instead.".into()),
            };
            this.update_in(cx, |this, window, cx| {
                if this.overlay_generation != generation || this.overlay_closing_since.is_some() {
                    return;
                }
                let Some(picker) = this.add_project.as_mut() else {
                    return;
                };
                picker.work = None;
                picker.path.update(cx, |field, cx| field.focus(window, cx));
                match result {
                    Ok(Some(folder)) => this.navigate_add_project_folder(folder, window, cx),
                    Ok(None) => {}
                    Err(error) => picker.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn close_add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.add_project.as_mut() else {
            return;
        };
        if picker.work.is_none() && self.overlay_closing_since.is_none() {
            picker.listing_task = None;
            window.focus(
                picker.previous_focus.as_ref().unwrap_or(&self.focus_handle),
                cx,
            );
            self.dismiss_overlay();
            cx.notify();
        }
    }

    fn registered_clone_destination(
        &self,
        environment: &ContentEnvironment,
        destination: &str,
    ) -> Option<Project> {
        let candidate = Project {
            environment: environment.clone(),
            root: destination.to_owned(),
        };
        self.project_settings(&candidate).map(|_| candidate)
    }

    fn submit_add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.add_project.as_ref() else {
            return;
        };
        if picker.work.is_some() || self.overlay_closing_since.is_some() {
            return;
        }
        if let Some(project) = picker.resolved.clone() {
            self.complete_add_project(project, window, cx);
            return;
        }
        let Some(destination) = picker.destination(cx) else {
            return;
        };
        let environment = picker.environment().clone();
        let url =
            (picker.source == Source::Clone).then(|| picker.url.read(cx).value().trim().to_owned());
        if url.is_some()
            && let Some(project) = self.registered_clone_destination(&environment, &destination)
        {
            self.complete_add_project(project, window, cx);
            return;
        }
        let Some(picker) = self.add_project.as_mut() else {
            return;
        };
        picker.work = Some(if url.is_some() {
            Work::Clone
        } else {
            Work::Resolve
        });
        picker.error = None;
        let target = self.target.clone();
        let generation = self.overlay_generation;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    (|| -> crate::Result<Project> {
                        let mut client = target.connect()?;
                        match url {
                            Some(url) => client.clone_project(environment, url, destination),
                            None => client.resolve_project_folder(environment, destination),
                        }
                    })()
                    .map_err(|error| error.to_string())
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                if this.overlay_generation != generation || this.overlay_closing_since.is_some() {
                    return;
                }
                let Some(picker) = this.add_project.as_mut() else {
                    return;
                };
                picker.work = None;
                match result {
                    Ok(project) => this.complete_add_project(project, window, cx),
                    Err(error) => picker.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn complete_add_project(
        &mut self,
        project: Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.save_added_project(&project, cx) {
            if let Some(picker) = self.add_project.as_mut() {
                picker.resolved = Some(project);
                picker.error = Some(
                    self.global_error
                        .take()
                        .unwrap_or_else(|| "Could not save the project".into()),
                );
            }
            cx.notify();
            return;
        }
        let Some(picker) = &self.add_project else {
            return;
        };
        let from_settings = picker.from_settings;
        let focus = picker.previous_focus.clone();
        self.finish_dismiss_overlay();
        window.focus(focus.as_ref().unwrap_or(&self.focus_handle), cx);
        if from_settings {
            self.open_project_settings(&project, window, cx);
        } else {
            self.reveal_sidebar_project(&project, window, cx);
        }
        cx.notify();
    }

    pub(super) fn add_project_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(picker) = self.add_project.as_mut() else {
            return false;
        };
        if self.overlay_closing_since.is_some() || picker.work.is_some() {
            return true;
        }
        if self.popup == Some(Popup::ProjectEnvironment) {
            return false;
        }
        let path_focused = picker.path.focus_handle(cx).is_focused(window);
        let url_focused =
            picker.source == Source::Clone && picker.url.focus_handle(cx).is_focused(window);
        let modified = key.modifiers.alt || key.modifiers.control || key.modifiers.platform;
        match key.key.as_str() {
            "escape" => self.close_add_project(window, cx),
            "e" if key.modifiers.alt
                && !key.modifiers.control
                && !key.modifiers.platform
                && picker.environments.len() > 1 =>
            {
                self.set_popup(Popup::ProjectEnvironment, true);
            }
            "tab" => {
                if key.modifiers.shift {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
                if !picker.focus.contains_focused(window, cx) {
                    picker.path.update(cx, |field, cx| field.focus(window, cx));
                }
            }
            "up" if path_focused && key.modifiers.alt => self.add_project_up(window, cx),
            "up" | "down" if path_focused && !modified => {
                let count = picker.matches.len();
                if count > 0 {
                    let index = if key.key == "up" {
                        picker
                            .highlight
                            .map_or(count - 1, |index| (index + count - 1) % count)
                    } else {
                        picker.highlight.map_or(0, |index| (index + 1) % count)
                    };
                    picker.highlight = Some(index);
                    picker.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
                }
            }
            "enter" if path_focused || url_focused => {
                let folder = (path_focused && !modified)
                    .then(|| picker.highlight)
                    .flatten()
                    .and_then(|index| picker.selected_folder(index));
                if let Some(folder) = folder {
                    self.navigate_add_project_folder(folder, window, cx);
                } else {
                    self.submit_add_project(window, cx);
                }
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    pub(super) fn render_add_project(
        &self,
        picker: &AddProject,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let (width, height) = overlay_viewport_size(window);
        let top = CHROME_HEIGHT + 6.0;
        let busy = picker.work.is_some() || self.overlay_closing_since.is_some();
        let blocked = busy.then_some("Please wait");
        let clone = picker.source == Source::Clone;
        let path_caption = if clone {
            "Destination parent folder"
        } else {
            "Project folder"
        };
        let destination = picker.destination(cx);
        let url = clone.then(|| picker.url.read(cx).value());
        let invalid_url = url
            .as_ref()
            .is_some_and(|url| clone_name(url.trim()).is_none());
        let unavailable = if invalid_url {
            Some("Enter an HTTPS or SSH repository URL")
        } else {
            destination.is_none().then_some("Enter a folder path")
        };
        let existing = url.as_ref().and_then(|_| {
            self.registered_clone_destination(picker.environment(), destination.as_deref()?)
        });
        let action = if let Some(existing) = &existing {
            if self.project_archived(existing) {
                "Restore project"
            } else {
                "Select project"
            }
        } else if clone && picker.resolved.is_none() {
            "Clone and add"
        } else {
            "Add project"
        };
        let caption = |label: &'static str| {
            div()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .text_color(muted)
                .child(label)
        };
        let environment = if busy || picker.environments.len() < 2 {
            div()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .child(self.add_project_environment_label(picker.environment()))
                .into_any_element()
        } else {
            controls::select(
                "add-project-environment",
                picker.environments.iter().map(|(environment, _)| {
                    controls::select_option(self.add_project_environment_label(environment))
                }),
                Some(picker.environment),
                self.popup == Some(Popup::ProjectEnvironment),
            )
            .on_open_change(cx.listener(|this, open, _, cx| {
                this.set_popup(Popup::ProjectEnvironment, *open);
                cx.notify();
            }))
            .on_pick(cx.listener(|this, index, window, cx| {
                this.select_add_project_environment(*index, window, cx)
            }))
            .into_any_element()
        };
        let progress = match picker.work {
            Some(Work::Browse) => Some("Waiting for the folder dialog…"),
            Some(Work::Resolve) => Some("Checking folder…"),
            Some(Work::Clone) => Some("Cloning repository…"),
            None => None,
        };
        let sources = div().flex().flex_wrap().items_center().gap_2().children(
            [
                (
                    "add-project-open-source",
                    "Open folder",
                    Icon::new(IconName::FolderOpen),
                    Source::Folder,
                ),
                (
                    "add-project-clone-source",
                    "Clone from URL",
                    Icon::default().path("lucide/github.svg"),
                    Source::Clone,
                ),
            ]
            .into_iter()
            .map(|(id, label, icon, source)| {
                controls::text_button(id, label)
                    .header()
                    .icon(icon)
                    .selected(picker.source == source)
                    .disabled_because(blocked)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_add_project_source(source, window, cx);
                    }))
            }),
        );
        let path_field = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&picker.path)
                        .aria_label(path_caption)
                        .appearance(true)
                        .disabled(busy),
                ),
            )
            .when(picker.browse_available, |row| {
                row.child(
                    controls::text_button("add-project-browse", "Browse…")
                        .header()
                        .disabled_because(blocked)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.browse_add_project(window, cx);
                        })),
                )
            });
        let footer = div()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(color(colors.border))
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(
                caption(progress.unwrap_or("↑ ↓ Select folder  ·  Enter Open selected folder"))
                    .flex_1(),
            )
            .child(
                controls::text_button("add-project-cancel", "Cancel")
                    .header()
                    .disabled_because(blocked)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.close_add_project(window, cx);
                    })),
            )
            .child(
                controls::text_button("add-project-submit", action)
                    .header()
                    .primary()
                    .disabled_because(blocked.or(unavailable))
                    .loading(matches!(picker.work, Some(Work::Clone | Work::Resolve)))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.submit_add_project(window, cx);
                    })),
            );
        div()
            .id("add-project-backdrop")
            .absolute()
            .inset_0()
            .pt(px(top))
            .flex()
            .justify_center()
            .items_start()
            .occlude()
            .text_color(color(modal_text_color(colors.foreground, &colors)))
            .on_any_mouse_down(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.close_add_project(window, cx);
            }))
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .id("add-project-palette")
                    .role(gpui::Role::Dialog)
                    .aria_label("Add project")
                    .track_focus(&picker.focus)
                    .tab_group()
                    .w(px((width - 32.0).clamp(1.0, 620.0)))
                    .max_h(px((height - top - 12.0).max(1.0)))
                    .flex()
                    .flex_col()
                    .popover_style(cx)
                    .overflow_y_scroll()
                    .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                    .on_key_down(|_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .px_4()
                            .pt_3()
                            .pb_2()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Add project"),
                    )
                    .child(
                        div()
                            .px_4()
                            .pb_3()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(sources)
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap_2()
                                    .child(caption("Environment"))
                                    .child(environment)
                                    .when(!busy && picker.environments.len() > 1, |row| {
                                        row.child(caption(if cfg!(target_os = "macos") {
                                            "⌥E"
                                        } else {
                                            "Alt+E"
                                        }))
                                    })
                                    .when(picker.environments_loading, |row| {
                                        row.child(caption("Finding environments…"))
                                    }),
                            )
                            .when_some(picker.environments_error.clone(), |form, error| {
                                form.child(
                                    div()
                                        .text_color(muted)
                                        .text_size(px(UI_SMALL_TEXT_SIZE))
                                        .child(format!("Could not list environments: {error}")),
                                )
                            })
                            .when(clone, |form| {
                                form.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(caption("Repository URL"))
                                        .child(
                                            Input::new(&picker.url)
                                                .aria_label("Repository URL")
                                                .appearance(true)
                                                .disabled(busy),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(caption(path_caption))
                                    .child(path_field),
                            )
                            .child(self.render_add_project_folders(picker, height, busy, cx))
                            .when(clone, |form| {
                                let preview = destination.unwrap_or_else(|| {
                                    if url.as_ref().is_none_or(|url| url.trim().is_empty()) {
                                        "Enter a repository URL to preview its destination.".into()
                                    } else if invalid_url {
                                        "Enter an HTTPS or SSH repository URL.".into()
                                    } else {
                                        "Choose the parent folder for the clone.".into()
                                    }
                                });
                                let label = if existing.is_some() {
                                    "Existing project"
                                } else {
                                    "Clone into"
                                };
                                form.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(caption(label))
                                        .child(preview),
                                )
                            })
                            .when_some(picker.error.clone(), |form, error| {
                                form.child(div().text_color(color(colors.error)).child(error))
                            }),
                    )
                    .child(footer),
            )
            .into_any_element()
    }

    fn render_add_project_folders(
        &self,
        picker: &AddProject,
        height: f32,
        busy: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let Some(listing) = &picker.listing else {
            return div()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .text_color(muted)
                .child("Enter an absolute folder path to see suggestions.")
                .into_any_element();
        };
        let can_go_up = content_path::resolve(Some(&listing.parent), "..", picker.windows)
            .is_ok_and(|parent| parent != listing.parent);
        let status = if let Some(Err(error)) = &listing.folders {
            Some(error.clone())
        } else if listing.folders.is_none() {
            Some("Loading folders…".into())
        } else if picker.matches.is_empty() {
            Some("No matching folders".into())
        } else {
            None
        };
        let rows = picker.matches.len();
        div()
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(color(colors.border))
            .child(
                div()
                    .px_2()
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .text_color(muted)
                            .child(listing.parent.clone()),
                    )
                    .child(
                        controls::icon_button("add-project-up", IconName::ArrowUp, "Parent folder")
                            .disabled_because((busy || !can_go_up).then_some("No parent folder"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_project_up(window, cx)),
                            ),
                    ),
            )
            .when_some(status, |list, status| {
                list.child(
                    div()
                        .px_2()
                        .pb_2()
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .text_color(muted)
                        .child(status),
                )
            })
            .when(rows != 0, |list| {
                let list_height = ((height - 410.0).clamp(ROW_HEIGHT * 2.0, ROW_HEIGHT * 6.0))
                    .min(rows as f32 * ROW_HEIGHT);
                list.child(
                    uniform_list(
                        "add-project-folders",
                        rows,
                        cx.processor(move |this, range: Range<usize>, _, cx| {
                            let Some(picker) = &this.add_project else {
                                return Vec::new();
                            };
                            let Some(Ok(folders)) = picker
                                .listing
                                .as_ref()
                                .and_then(|listing| listing.folders.as_ref())
                            else {
                                return Vec::new();
                            };
                            range
                                .filter_map(|index| {
                                    let folder = folders.get(*picker.matches.get(index)?)?;
                                    Some(
                                        div()
                                            .id(("add-project-folder", index))
                                            .h(px(ROW_HEIGHT))
                                            .px_2()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .when(picker.highlight == Some(index), |row| {
                                                row.bg(color(colors.surface_hover))
                                            })
                                            .when(!busy, |row| {
                                                row.cursor_pointer()
                                                    .hover(move |row| {
                                                        row.bg(color(colors.surface_hover))
                                                    })
                                                    .on_click(cx.listener(
                                                        move |this, _, window, cx| {
                                                            if let Some(folder) = this
                                                                .add_project
                                                                .as_ref()
                                                                .and_then(|picker| {
                                                                    picker.selected_folder(index)
                                                                })
                                                            {
                                                                this.navigate_add_project_folder(
                                                                    folder, window, cx,
                                                                );
                                                            }
                                                        },
                                                    ))
                                            })
                                            .child(controls::glyph(IconName::Folder, muted))
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .truncate()
                                                    .child(folder.name.clone()),
                                            )
                                            .into_any_element(),
                                    )
                                })
                                .collect()
                        }),
                    )
                    .h(px(list_height))
                    .track_scroll(&picker.scroll),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{clone_name, drive_root};

    #[test]
    fn clone_preview_uses_the_repository_basename() {
        for url in [
            "https://example.com/org/my-repo.git",
            "ssh://git@example.com:2222/org/my-repo.git/",
            "git@example.com:org/my-repo.git",
            "git@[::1]:org/my-repo",
        ] {
            assert_eq!(clone_name(url), Some("my-repo"), "{url}");
        }
        for url in [
            "",
            "https://example.com",
            "https://example.com/org/..",
            "https://example.com/org/my-repo.git?query=1",
            "file:///home/me/my-repo",
            r"C:\repos\my-repo",
            "/home/me/my-repo",
        ] {
            assert_eq!(clone_name(url), None, "{url}");
        }
    }

    #[test]
    fn a_bare_windows_drive_lists_its_root() {
        assert_eq!(drive_root("D:").as_deref(), Some("D:\\"));
        assert_eq!(drive_root("D:subfolder"), None);
        assert_eq!(drive_root("1:"), None);
    }
}
