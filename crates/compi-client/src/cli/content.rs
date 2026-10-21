//! `compi files` and `compi edit`: show a folder or file in the pane they run from. A
//! terminal shows it as its view, in place of its shell, which keeps running underneath;
//! a files content pane shows it itself. `compi diff` places its views the same way.
use super::{CliError, Options, Output, pane_surface, submit, targets};
use crate::{DaemonClient, cli_window::Action, connection::ConnectionTarget};
use compi_protocol::{
    ContentEnvironment, ContentTab, FolderContent, LayoutNode, MutationReceipt, PaneContent,
    PaneId, PaneSide, SurfaceInfo, WorkspaceMutation, WorkspaceSnapshot,
};
use serde_json::{Value, json};

/// What a command shows: a folder in the browser, or a tab (a file or a diff).
pub(super) enum Show {
    Folder(String),
    Tab {
        tab: ContentTab,
        /// The absolute file the tab is about; a view's root that holds it stays.
        file: String,
        /// When set, a view's root must also be this folder or lie below it to stay.
        within: Option<String>,
        /// The root files take when none are shown or a view's root does not stay.
        root: String,
    },
}

/// The pane a command runs from, and where its relative paths start.
pub(super) struct Caller<'a> {
    pub id: PaneId,
    pub environment: ContentEnvironment,
    /// The caller's terminal; `None` when the caller is a files content pane.
    pub surface: Option<&'a SurfaceInfo>,
    /// The files the pane holds now: a files content pane's, or a terminal's files view,
    /// shown or not.
    pub files: Option<FolderContent>,
    /// The caller is a terminal showing its view rather than its shell.
    pub view_shown: bool,
    /// The shell's directory (or a content pane's root), normalised.
    pub directory: Option<String>,
    /// Paths are in Windows syntax, which only a Windows host's are.
    pub windows: bool,
}

/// The pane a command shows something in: always the caller's own.
pub(super) struct Placed {
    pub pane_id: PaneId,
    pub receipt: Option<MutationReceipt>,
}

pub(super) fn run(
    client: &mut DaemonClient,
    state: &WorkspaceSnapshot,
    target: &ConnectionTarget,
    options: &Options,
) -> Result<Output, CliError> {
    let caller = caller(client, state, options)?;
    let windows = caller.windows;
    let directory = caller.directory.as_deref();
    let (show, line) = match options.args.get(1) {
        None => (Show::Folder(caller.directory()?.to_owned()), None),
        Some(folder) if options.args[0] == "files" => (
            Show::Folder(resolve_path(directory, folder, windows)?),
            None,
        ),
        Some(argument) => {
            let (path, line) = location(argument)?;
            let path = resolve_path(directory, path, windows)?;
            // Only a listable folder opens the browser; anything else is opened as a
            // file, and the editor reports a missing or unreadable one.
            if line.is_none()
                && client
                    .list_content_directory(caller.environment.clone(), path.clone())
                    .is_ok()
            {
                (Show::Folder(path), None)
            } else {
                let root = match directory {
                    Some(directory) if contains(directory, &path, windows) => directory.to_owned(),
                    _ => resolve_path(None, &format!("{path}/.."), windows)?,
                };
                let show = Show::Tab {
                    tab: ContentTab::edit(path.clone()),
                    file: path,
                    within: None,
                    root,
                };
                (show, line)
            }
        }
    };
    let placed = place(client, state, &caller, &show)?;
    let (subject, action) = match &show {
        Show::Folder(folder) => (
            folder.clone(),
            Action::FocusPane {
                pane_id: placed.pane_id.clone(),
            },
        ),
        Show::Tab { file, .. } => (
            line.map_or_else(|| file.clone(), |line| format!("{file}:{line}")),
            Action::RevealFile {
                pane_id: placed.pane_id.clone(),
                file: file.clone(),
                line: line.unwrap_or(1),
            },
        ),
    };
    let (text, value) = report(client, target, options, &placed, &subject, action)?;
    Ok(Output::Report { text, value })
}

/// The selected pane (the calling shell by default), its environment and directory.
/// Settings and Monitor panes have neither, so they cannot be the caller; a terminal
/// showing one as its view can, and its view becomes files.
pub(super) fn caller<'a>(
    client: &mut DaemonClient,
    state: &'a WorkspaceSnapshot,
    options: &Options,
) -> Result<Caller<'a>, CliError> {
    let (_, _, leaf) = targets::pane(state, options)?;
    let id = leaf
        .leaf_pane()
        .expect("targets resolve only leaf nodes")
        .clone();
    let (environment, surface, base, files, view_shown) = match leaf {
        LayoutNode::Pane {
            surface_id, view, ..
        } => {
            let surface = pane_surface(state, &surface_id)?;
            let environment = ContentEnvironment::of_surface(surface);
            let base = shell_directory(client, options, surface, &environment)?;
            let view_shown = view
                .as_ref()
                .is_some_and(|view| view.side == PaneSide::View);
            let files = view.and_then(|view| match view.content {
                PaneContent::Files(folder) => Some(folder),
                PaneContent::Settings(_) | PaneContent::Monitor(_) => None,
            });
            (environment, Some(surface), base, files, view_shown)
        }
        LayoutNode::Content {
            content: PaneContent::Files(folder),
            ..
        } => (
            folder.environment.clone(),
            None,
            Some(folder.root.clone()),
            Some(folder),
            false,
        ),
        LayoutNode::Content {
            content: content @ (PaneContent::Settings(_) | PaneContent::Monitor(_)),
            ..
        } => {
            let command = options.args.first().map_or("this command", String::as_str);
            let shows = if content.is_monitor() {
                "Monitor"
            } else {
                "Settings"
            };
            return Err(CliError::new(
                3,
                "files_pane_required",
                format!(
                    "pane {id} shows {shows}; {command} needs a terminal pane or a content pane showing files"
                ),
            ));
        }
        LayoutNode::Split { .. } => unreachable!("targets resolve only leaf nodes"),
    };
    // Paths are in the environment's syntax, which is Windows only for a Windows host.
    let windows =
        environment == ContentEnvironment::Host && cfg!(windows) && options.connect.is_none();
    Ok(Caller {
        directory: base.and_then(|base| resolve_path(None, &base, windows).ok()),
        id,
        environment,
        surface,
        files,
        view_shown,
        windows,
    })
}

impl Caller<'_> {
    /// The directory relative paths start from, required.
    pub(super) fn directory(&self) -> Result<&str, CliError> {
        self.directory.as_deref().ok_or_else(|| {
            CliError::usage("the pane's current directory is unknown; name an absolute folder")
        })
    }
}

/// Shows `show` in the caller's own pane: a terminal shows it as its view, in place of
/// its shell; a files content pane shows it itself. See `merged` for what changes.
pub(super) fn place(
    client: &mut DaemonClient,
    state: &WorkspaceSnapshot,
    caller: &Caller,
    show: &Show,
) -> Result<Placed, CliError> {
    let current = caller
        .files
        .as_ref()
        .filter(|files| files.environment == caller.environment);
    let content = merged(
        current.cloned(),
        &caller.environment,
        show,
        caller.windows,
        // A terminal's view follows the shell; a content pane keeps its own folder.
        caller.surface.is_some(),
    );
    content.validate().map_err(CliError::usage)?;
    let unchanged = current == Some(&content);
    let mutation = if caller.surface.is_none() {
        (!unchanged).then(|| WorkspaceMutation::UpdateContent {
            pane_id: caller.id.clone(),
            content: PaneContent::Files(content),
        })
    } else {
        (!unchanged || !caller.view_shown).then(|| WorkspaceMutation::ShowView {
            pane_id: caller.id.clone(),
            content: PaneContent::Files(content),
        })
    };
    let receipt = mutation
        .map(|mutation| submit(client, state, mutation, None))
        .transpose()?;
    Ok(Placed {
        pane_id: caller.id.clone(),
        receipt,
    })
}

/// `current` (the pane's files, if it shows any in `environment`) with `show` in it: a
/// folder becomes the root; a tab is added if missing and made active. With `follow`,
/// a root that does not hold the tab's file (or lies outside its `within` folder) moves
/// to the tab's own root. Without `current`, new files show just that.
fn merged(
    current: Option<FolderContent>,
    environment: &ContentEnvironment,
    show: &Show,
    windows: bool,
    follow: bool,
) -> FolderContent {
    let Some(mut content) = current else {
        let environment = environment.clone();
        return match show {
            Show::Folder(folder) => FolderContent {
                environment,
                root: folder.clone(),
                tabs: Vec::new(),
                active: None,
            },
            Show::Tab { tab, root, .. } => FolderContent {
                environment,
                root: root.clone(),
                tabs: vec![tab.clone()],
                active: Some(tab.clone()),
            },
        };
    };
    let current_root = resolve_path(None, &content.root, windows).ok();
    match show {
        Show::Folder(folder) => {
            if current_root.as_ref() != Some(folder) {
                content.root = folder.clone();
            }
        }
        Show::Tab {
            tab,
            file,
            within,
            root,
        } => {
            let holds = current_root.is_some_and(|current| {
                contains(&current, file, windows)
                    && within.as_ref().is_none_or(|within| {
                        *within == current || contains(within, &current, windows)
                    })
            });
            if follow && !holds {
                content.root = root.clone();
            }
            // The file's one tab takes the mode asked for, keeping its place and path.
            let tab = match open_tab(&content, file, windows) {
                Some(index) => {
                    let shown = content.tabs[index].with_diff(tab.diff_scope().cloned());
                    content.tabs[index] = shown.clone();
                    shown
                }
                None => {
                    content.tabs.push(tab.clone());
                    tab.clone()
                }
            };
            content.active = Some(tab);
        }
    }
    content
}

/// Hands `action` to a window showing the pane and reports what was opened: the text
/// `Opened {subject} in pane …` and its JSON value. The workspace change stands without a
/// window; one that shows the tab is told.
pub(super) fn report(
    client: &mut DaemonClient,
    target: &ConnectionTarget,
    options: &Options,
    placed: &Placed,
    subject: &str,
    action: Action,
) -> Result<(String, Value), CliError> {
    let after = client.workspace()?;
    let not_shown = crate::cli_window::execute(target, options.window.as_deref(), action)
        .err()
        .map(|error| error.to_string());
    let Placed { pane_id, receipt } = placed;
    let mut text = format!("Opened {subject} in pane {pane_id}");
    if let Some(reason) = &not_shown {
        text.push_str(&format!(" · not shown: {reason}"));
    }
    text.push('\n');
    Ok((
        text,
        json!({"pane_id":pane_id,"shown":not_shown.is_none(),"not_shown":not_shown,"receipt":receipt,"workspace":after}),
    ))
}

/// The folder relative paths start from: the calling shell's own directory when the
/// command runs in that shell on the daemon's computer, else the terminal's last
/// reported directory.
fn shell_directory(
    client: &mut DaemonClient,
    options: &Options,
    surface: &SurfaceInfo,
    environment: &ContentEnvironment,
) -> Result<Option<String>, CliError> {
    let calling = options.connect.is_none()
        && options.surface.as_deref() == Some(surface.id.as_str())
        && std::env::var("COMPI_SURFACE_ID").is_ok_and(|id| id == surface.id.as_str());
    if calling {
        let directory = match environment {
            // The WSL shell bridge passes the shell's Linux directory to the Windows CLI.
            ContentEnvironment::Wsl { .. } => std::env::var("COMPI_SHELL_CWD").ok(),
            ContentEnvironment::Host => std::env::current_dir()
                .ok()
                .and_then(|directory| directory.into_os_string().into_string().ok()),
        };
        if directory.is_some() {
            return Ok(directory);
        }
    }
    Ok(client.surface_metadata(surface)?.directory.value)
}

/// Where normalised `file` is open among `content`'s tabs, in either mode.
fn open_tab(content: &FolderContent, file: &str, windows: bool) -> Option<usize> {
    content
        .tabs
        .iter()
        .position(|tab| resolve_path(None, tab.path(), windows).ok().as_deref() == Some(file))
}

/// Whether normalised `path` lies below normalised folder `root`.
pub(super) fn contains(root: &str, path: &str, windows: bool) -> bool {
    let separator = if windows { '\\' } else { '/' };
    path.strip_prefix(root).is_some_and(|rest| {
        !rest.is_empty() && (root.ends_with(separator) || rest.starts_with(separator))
    })
}

/// `FILE[:LINE[:COL]]` as the file and its line. The column compilers print is accepted
/// and ignored; the cursor goes to the start of the line.
pub(super) fn location(argument: &str) -> Result<(&str, Option<u32>), CliError> {
    fn number(value: &str) -> Option<(&str, &str)> {
        value.rsplit_once(':').filter(|(rest, digits)| {
            !rest.is_empty() && !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
        })
    }
    let Some((rest, last)) = number(argument) else {
        return Ok((argument, None));
    };
    let (path, line) = number(rest).unwrap_or((rest, last));
    let line = line
        .parse::<u32>()
        .ok()
        .filter(|line| *line > 0)
        .ok_or_else(|| {
            CliError::usage(format!(
                "{argument}: line must be between 1 and {}",
                u32::MAX
            ))
        })?;
    Ok((path, Some(line)))
}

/// `path` made absolute against `base` in the environment's syntax; see `content_path`.
pub(super) fn resolve_path(
    base: Option<&str>,
    path: &str,
    windows: bool,
) -> Result<String, CliError> {
    crate::content_path::resolve(base, path, windows).map_err(CliError::usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::GitScope;

    #[test]
    fn locations_strip_a_trailing_line_and_column() {
        assert_eq!(location("src/main.rs").unwrap(), ("src/main.rs", None));
        assert_eq!(
            location("src/main.rs:12").unwrap(),
            ("src/main.rs", Some(12))
        );
        assert_eq!(
            location("src/main.rs:12:5").unwrap(),
            ("src/main.rs", Some(12))
        );
        assert_eq!(
            location(r"C:\src\a.rs:7").unwrap(),
            (r"C:\src\a.rs", Some(7))
        );
        assert_eq!(location("notes:draft").unwrap(), ("notes:draft", None));
        assert_eq!(location(":12").unwrap(), (":12", None));
        assert_eq!(location("a.rs:0").unwrap_err().exit, 2);
        assert_eq!(location("a.rs:99999999999").unwrap_err().exit, 2);
    }

    #[test]
    fn paths_resolve_against_the_directory_lexically() {
        let unix = |path| resolve_path(Some("/home/me/proj/"), path, false).unwrap();
        assert_eq!(unix("src/../Cargo.toml"), "/home/me/proj/Cargo.toml");
        assert_eq!(unix("./src//main.rs"), "/home/me/proj/src/main.rs");
        assert_eq!(unix("../other"), "/home/me/other");
        assert_eq!(unix("."), "/home/me/proj");
        assert_eq!(unix("/etc/./hosts"), "/etc/hosts");
        assert_eq!(unix("/../.."), "/");
        assert_eq!(unix(r"a\b"), r"/home/me/proj/a\b");
        assert_eq!(resolve_path(None, "src", false).unwrap_err().exit, 2);
        assert_eq!(
            resolve_path(Some("proj"), "src", false).unwrap_err().exit,
            2
        );
        let windows = |path| resolve_path(Some(r"C:\Users\me"), path, true).unwrap();
        assert_eq!(windows("src/a.rs"), r"C:\Users\me\src\a.rs");
        assert_eq!(windows(r"..\..\.."), r"C:\");
        assert_eq!(windows(r"\temp"), r"C:\temp");
        assert_eq!(windows("D:/x/../y"), r"D:\y");
        assert_eq!(
            windows(r"\\wsl.localhost\Ubuntu\home\..\.."),
            r"\\wsl.localhost\Ubuntu\"
        );
    }

    #[test]
    fn files_in_place_keep_their_tabs_and_follow_the_shell_only_as_a_view() {
        let host = ContentEnvironment::Host;
        let file = |path: &str| Show::Tab {
            tab: ContentTab::edit(path),
            file: path.into(),
            within: None,
            root: "/home/me/other".into(),
        };
        let current = |root: &str| FolderContent {
            environment: host.clone(),
            root: root.into(),
            tabs: vec![ContentTab::edit("/home/me/proj/a.rs")],
            active: None,
        };
        // Nothing shown yet: just what was asked for.
        let fresh = merged(None, &host, &file("/home/me/other/b.rs"), false, true);
        assert_eq!(fresh.root, "/home/me/other");
        assert_eq!(fresh.tabs, [ContentTab::edit("/home/me/other/b.rs")]);
        assert_eq!(fresh.active, Some(ContentTab::edit("/home/me/other/b.rs")));
        // A file under the root joins the open tabs and the root stays.
        let proj = current("/home/me/proj/");
        let inside = merged(
            Some(proj.clone()),
            &host,
            &file("/home/me/proj/src/c.rs"),
            false,
            true,
        );
        assert_eq!(inside.root, "/home/me/proj/");
        assert_eq!(inside.tabs.len(), 2);
        assert_eq!(
            inside.active,
            Some(ContentTab::edit("/home/me/proj/src/c.rs"))
        );
        // A terminal's view follows the shell to a file elsewhere, keeping its tabs; a
        // content pane keeps its own folder.
        let elsewhere = file("/home/me/other/b.rs");
        let view = merged(Some(proj.clone()), &host, &elsewhere, false, true);
        assert_eq!(view.root, "/home/me/other");
        assert_eq!(view.tabs.len(), 2);
        let pane = merged(Some(proj.clone()), &host, &elsewhere, false, false);
        assert_eq!(pane.root, "/home/me/proj/");
        // A diff moves a root holding the file but outside its repository to the
        // repository, and the file's one tab switches to Diff mode.
        let diff = Show::Tab {
            tab: ContentTab::diff("/home/me/proj/a.rs", GitScope::Unstaged),
            file: "/home/me/proj/a.rs".into(),
            within: Some("/home/me/proj".into()),
            root: "/home/me/proj".into(),
        };
        let reviewed = merged(Some(current("/home/me")), &host, &diff, false, true);
        assert_eq!(reviewed.root, "/home/me/proj");
        assert_eq!(
            reviewed.tabs,
            [ContentTab::diff("/home/me/proj/a.rs", GitScope::Unstaged)]
        );
        // Inside the repository the root stays.
        let kept = merged(Some(proj.clone()), &host, &diff, false, true);
        assert_eq!(kept.root, "/home/me/proj/");
        // A folder becomes the root.
        let folder = merged(Some(proj), &host, &Show::Folder("/srv".into()), false, true);
        assert_eq!(folder.root, "/srv");
        assert_eq!(folder.tabs.len(), 1);
    }
}
