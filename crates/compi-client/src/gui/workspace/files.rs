//! Opening files from the focused pane: the Files, Open file… and New editor commands,
//! and file paths Ctrl/Cmd-clicked in a terminal. Each works on the pane's folder in its
//! own environment: a terminal's current directory, or a content pane's root.
use super::*;
use crate::content_path;
use crate::viewport::FileLocation;
use compi_protocol::{ContentEnvironment, SurfaceInfo};

const UNKNOWN_DIRECTORY: &str = "The shell has not reported its current directory. Enable OSC 7 shell integration to open files from this pane.";
const WINDOWS_SEARCH: &str =
    "Open file… searches WSL and Unix folders; use Files to browse this Windows folder";
const NO_SEARCH_TERMINAL: &str =
    "Open file… searches through a terminal of this folder's environment; open one first";

/// The folder file commands work on in the focused pane.
pub(super) struct FileScope {
    pub pane_id: PaneId,
    pub environment: ContentEnvironment,
    /// Absolute, in the environment's syntax.
    pub folder: String,
    /// The pane shows content rather than a terminal.
    pub content: bool,
    /// The terminal surface whose environment the daemon lists and searches `folder`
    /// in, or why it cannot: surface-keyed search reaches only `/` folders.
    pub search: Result<SurfaceId, &'static str>,
}

/// A directory as a shell reports it, in its environment's syntax: OSC 7 reports a
/// Windows folder as `/C:/…`.
fn environment_path(path: &str, windows: bool) -> &str {
    match path.as_bytes() {
        [b'/', drive, b':', ..] if windows && drive.is_ascii_alphabetic() => &path[1..],
        _ => path,
    }
}

/// `path`, printed or linked in a terminal whose current directory is `folder`, as an
/// absolute path in the terminal's environment.
fn link_target(path: &str, folder: Option<&str>, windows: bool) -> Result<String, String> {
    content_path::resolve(folder, environment_path(path, windows), windows)
}

impl CompiApp {
    /// Paths of `environment` use Windows syntax: the host of a local Windows daemon.
    fn windows_paths(&self, environment: &ContentEnvironment) -> bool {
        *environment == ContentEnvironment::Host && cfg!(windows) && !self.target.is_remote()
    }

    /// The directory a terminal last reported, else the one it started in, absolute in
    /// its environment's syntax.
    fn terminal_folder(
        &self,
        view: &SurfaceView,
        surface: &SurfaceInfo,
        windows: bool,
    ) -> Option<String> {
        let reported = view
            .mirror
            .snapshot()
            .and_then(|snapshot| snapshot.current_directory.clone())
            .or_else(|| {
                surface
                    .working_directory
                    .as_ref()
                    .map(|cwd| cwd.resolved_wsl_path.clone())
                    .or_else(|| surface.launch.working_directory.clone())
                    .or_else(|| {
                        surface
                            .launch
                            .profile
                            .as_ref()
                            .and_then(|profile| profile.working_directory.clone())
                    })
            })?;
        link_target(&reported, None, windows).ok()
    }

    /// A surface the daemon can list and search `folder` of `environment` through.
    fn search_surface(
        &self,
        environment: &ContentEnvironment,
        folder: &str,
        own: Option<&SurfaceId>,
    ) -> Result<SurfaceId, &'static str> {
        if self.windows_paths(environment) || !folder.starts_with('/') {
            return Err(WINDOWS_SEARCH);
        }
        if let Some(own) = own {
            return Ok(own.clone());
        }
        self.workspace
            .as_ref()
            .and_then(|workspace| {
                workspace
                    .surfaces
                    .iter()
                    .find(|surface| ContentEnvironment::of_surface(surface) == *environment)
            })
            .map(|surface| surface.id.clone())
            .ok_or(NO_SEARCH_TERMINAL)
    }

    pub(super) fn file_scope(&self) -> Result<FileScope, &'static str> {
        let pane_id = self.focused_pane().ok_or("Select a pane first")?.clone();
        if let Some(content) = self.focused_content() {
            return Ok(FileScope {
                search: self.search_surface(&content.environment, &content.root, None),
                pane_id,
                environment: content.environment.clone(),
                folder: content.root.clone(),
                content: true,
            });
        }
        if let Some(content) = self.focused_pane_content() {
            return Err(if content.is_monitor() {
                "Monitor has no folder; select a terminal or a files pane"
            } else {
                "Settings has no folder; select a terminal or a files pane"
            });
        }
        let view = self.focused_view().ok_or("Select a pane first")?;
        let surface = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.surface(&view.surface_id))
            .ok_or("Select a pane first")?;
        let environment = ContentEnvironment::of_surface(surface);
        let folder = self
            .terminal_folder(view, surface, self.windows_paths(&environment))
            .ok_or(UNKNOWN_DIRECTORY)?;
        Ok(FileScope {
            search: self.search_surface(&environment, &folder, Some(&view.surface_id)),
            pane_id,
            environment,
            folder,
            content: false,
        })
    }

    /// Files and New editor: the focused terminal's folder in a content pane beside it,
    /// a new one for New editor. Files in a content pane shows its own folder.
    pub(super) fn show_files(
        &mut self,
        new_pane: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scope = match self.file_scope() {
            Ok(scope) => scope,
            Err(reason) => {
                self.global_error = Some(reason.into());
                return;
            }
        };
        if scope.content && !new_pane {
            self.show_content_folder(&scope.pane_id, window, cx);
            return;
        }
        self.open_content_file(
            &scope.pane_id,
            scope.environment,
            Some(scope.folder),
            None,
            None,
            new_pane,
            window,
            cx,
        );
    }

    /// Opens a Ctrl/Cmd-clicked file of `pane_id`'s terminal beside it once the daemon has
    /// read it, at its line; a path that is not a readable file leaves only a note.
    pub(in crate::gui) fn open_terminal_file_link(
        &mut self,
        pane_id: &PaneId,
        location: FileLocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((environment, folder, windows)) = self
            .surface_views
            .iter()
            .find(|view| &view.pane_id == pane_id)
            .and_then(|view| {
                let surface = self.workspace.as_ref()?.surface(&view.surface_id)?;
                let environment = ContentEnvironment::of_surface(surface);
                let windows = self.windows_paths(&environment);
                let folder = self.terminal_folder(view, surface, windows);
                Some((environment, folder, windows))
            })
        else {
            return;
        };
        let path = match link_target(&location.path, folder.as_deref(), windows) {
            Ok(path) => path,
            Err(error) => {
                self.global_error = Some(format!("Cannot open {}: {error}", location.path));
                return;
            }
        };
        let root = folder.filter(|folder| content::root_contains(folder, &path));
        let target = self.target.clone();
        let pane_id = pane_id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let read = {
                let (environment, path) = (environment.clone(), path.clone());
                cx.background_executor()
                    .spawn(async move {
                        (|| -> crate::Result<()> {
                            target.connect()?.read_content_file(environment, path)?;
                            Ok(())
                        })()
                        .map_err(|error| error.to_string())
                    })
                    .await
            };
            this.update_in(cx, |app, window, cx| {
                match read {
                    Ok(()) => app.open_content_file(
                        &pane_id,
                        environment,
                        root,
                        Some(path),
                        location.line,
                        false,
                        window,
                        cx,
                    ),
                    Err(error) => app.global_error = Some(format!("Cannot open {path}: {error}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicked_paths_resolve_in_the_terminal_environment() {
        let wsl = |path| link_target(path, Some("/home/me/proj"), false);
        assert_eq!(
            wsl("src/main.rs").as_deref(),
            Ok("/home/me/proj/src/main.rs")
        );
        assert_eq!(wsl("./a/../b.ts").as_deref(), Ok("/home/me/proj/b.ts"));
        assert_eq!(wsl("/etc/hosts").as_deref(), Ok("/etc/hosts"));
        assert!(link_target("src/main.rs", None, false).is_err());
        let windows = |path| link_target(path, Some(r"C:\work"), true);
        assert_eq!(
            windows("src/main.rs").as_deref(),
            Ok(r"C:\work\src\main.rs")
        );
        assert_eq!(
            windows("/C:/Users/me/a.rs").as_deref(),
            Ok(r"C:\Users\me\a.rs")
        );
        assert_eq!(windows(r"D:\x.rs").as_deref(), Ok(r"D:\x.rs"));
        assert_eq!(
            link_target("/C:/Users/me", None, true).as_deref(),
            Ok(r"C:\Users\me")
        );
        // Outside Windows syntax a leading `/C:` is an ordinary folder name.
        assert_eq!(link_target("/C:/x", None, false).as_deref(), Ok("/C:/x"));
    }
}
