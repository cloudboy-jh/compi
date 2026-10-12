//! `compi files` and `compi edit`: show a folder or file in a content pane beside the
//! selected terminal, reusing a content pane its tab already has.
use super::{CliError, Options, Output, pane_surface, submit, targets};
use crate::{DaemonClient, arrangement, cli_window::Action, connection::ConnectionTarget};
use compi_protocol::{
    ContentEnvironment, LayoutNode, PaneContent, PaneId, SplitAxis, SplitGeometry, SurfaceInfo,
    WorkspaceMutation, WorkspaceSnapshot,
};
use serde_json::json;

/// What the command shows: a folder in the browser, or a file at a 1-based line.
enum Show {
    Folder(String),
    File { path: String, line: Option<u32> },
}

/// Terminal grids measured in nominal 8x16-pixel cells, the scale content minimums are
/// in; the GUI lays the split out with real metrics when it applies it.
const CELL_METRICS: crate::layout::LayoutMetrics = crate::layout::LayoutMetrics {
    cell_width: 8.0,
    line_height: 16.0,
    padding_x: 0.0,
    padding_y: 0.0,
    pane_chrome_height: 0.0,
    divider_thickness: 1.0,
    margin: 0.0,
    scale_factor: 1.0,
};

pub(super) fn run(
    client: &mut DaemonClient,
    state: &WorkspaceSnapshot,
    target: &ConnectionTarget,
    options: &Options,
) -> Result<Output, CliError> {
    let (_, tab, caller) = targets::pane(state, options)?;
    let caller_id = caller
        .leaf_pane()
        .expect("targets resolve only leaf nodes")
        .clone();
    let (environment, surface, base) = match &caller {
        LayoutNode::Pane { surface_id, .. } => {
            let surface = pane_surface(state, surface_id)?;
            let environment = ContentEnvironment::of_surface(surface);
            let base = shell_directory(client, options, surface, &environment)?;
            (environment, Some(surface), base)
        }
        LayoutNode::Content { content, .. } => (
            content.environment.clone(),
            None,
            Some(content.root.clone()),
        ),
        LayoutNode::Split { .. } => unreachable!("targets resolve only leaf nodes"),
    };
    // Paths are in the environment's syntax, which is Windows only for a Windows host.
    let windows =
        environment == ContentEnvironment::Host && cfg!(windows) && options.connect.is_none();
    let directory = base.and_then(|base| resolve_path(None, &base, windows).ok());
    let show = match options.args.get(1) {
        None => Show::Folder(directory.clone().ok_or_else(|| {
            CliError::usage("the pane's current directory is unknown; name an absolute folder")
        })?),
        Some(folder) if options.args[0] == "files" => {
            Show::Folder(resolve_path(directory.as_deref(), folder, windows)?)
        }
        Some(argument) => {
            let (path, line) = location(argument)?;
            let path = resolve_path(directory.as_deref(), path, windows)?;
            // Only a listable folder opens the browser; anything else is opened as a
            // file, and the editor reports a missing or unreadable one.
            if line.is_none()
                && client
                    .list_content_directory(environment.clone(), path.clone())
                    .is_ok()
            {
                Show::Folder(path)
            } else {
                Show::File { path, line }
            }
        }
    };
    let (pane_id, receipt, opened) =
        match reusable(&tab.layout, &caller_id, &environment, &show, windows) {
            Some((pane_id, current)) => {
                let mut content = current.clone();
                match &show {
                    Show::Folder(folder) => {
                        if resolve_path(None, &content.root, windows).ok().as_ref() != Some(folder)
                        {
                            content.root = folder.clone();
                        }
                    }
                    Show::File { path, .. } => {
                        if !content.files.contains(path) {
                            content.files.push(path.clone());
                        }
                        content.active_file = Some(path.clone());
                    }
                }
                content.validate().map_err(CliError::usage)?;
                let receipt = if content == current {
                    None
                } else {
                    Some(submit(
                        client,
                        state,
                        WorkspaceMutation::UpdateContent {
                            pane_id: pane_id.clone(),
                            content,
                        },
                        None,
                    )?)
                };
                (pane_id, receipt, false)
            }
            None => {
                let surface = surface.expect("a content pane caller is always reused, never split");
                let content = match &show {
                    Show::Folder(folder) => PaneContent {
                        environment,
                        root: folder.clone(),
                        files: Vec::new(),
                        active_file: None,
                    },
                    Show::File { path, .. } => PaneContent {
                        root: match directory {
                            Some(directory) if contains(&directory, path, windows) => directory,
                            _ => resolve_path(None, &format!("{path}/.."), windows)?,
                        },
                        files: vec![path.clone()],
                        active_file: Some(path.clone()),
                        environment,
                    },
                };
                content.validate().map_err(CliError::usage)?;
                let receipt = submit(
                    client,
                    state,
                    WorkspaceMutation::OpenContent {
                        pane_id: caller_id.clone(),
                        axis: SplitAxis::Horizontal,
                        geometry: geometry(surface)?,
                        content,
                    },
                    None,
                )?;
                let pane_id = receipt
                    .affected_panes
                    .iter()
                    .find(|pane| **pane != caller_id)
                    .cloned()
                    .ok_or_else(|| {
                        CliError::new(
                            1,
                            "operation_failed",
                            "the daemon did not report the new content pane",
                        )
                    })?;
                (pane_id, Some(receipt), true)
            }
        };
    let after = client.workspace()?;
    let (subject, action) = match &show {
        Show::Folder(folder) => (
            folder.clone(),
            Action::FocusPane {
                pane_id: pane_id.clone(),
            },
        ),
        Show::File { path, line } => (
            line.map_or_else(|| path.clone(), |line| format!("{path}:{line}")),
            Action::RevealFile {
                pane_id: pane_id.clone(),
                file: path.clone(),
                line: line.unwrap_or(1),
            },
        ),
    };
    // The workspace change stands without a window; one that shows the tab is told.
    let not_shown = crate::cli_window::execute(target, options.window.as_deref(), action)
        .err()
        .map(|error| error.to_string());
    let mut text = format!(
        "Opened {subject} in {}pane {pane_id}",
        if opened { "new " } else { "" }
    );
    if let Some(reason) = &not_shown {
        text.push_str(&format!(" · not shown: {reason}"));
    }
    text.push('\n');
    Ok(Output::Report {
        text,
        value: json!({"pane_id":pane_id,"opened":opened,"shown":not_shown.is_none(),"not_shown":not_shown,"receipt":receipt,"workspace":after}),
    })
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

/// Split geometry for a content pane beside `surface`'s terminal; rejects a terminal too
/// small for both, as the daemon would.
fn geometry(surface: &SurfaceInfo) -> Result<SplitGeometry, CliError> {
    use crate::layout::PaneKind;
    let terminal = CELL_METRICS.minimum(PaneKind::Terminal);
    let content = CELL_METRICS.minimum(PaneKind::Content);
    let geometry = SplitGeometry {
        width: f32::from(surface.cols) * CELL_METRICS.cell_width,
        height: f32::from(surface.rows) * CELL_METRICS.line_height,
        min_width: terminal.width.max(content.width),
        min_height: terminal.height.max(content.height),
        divider: CELL_METRICS.divider_size(),
    };
    if geometry.width < 2.0 * geometry.min_width + geometry.divider
        || geometry.height < geometry.min_height
    {
        return Err(CliError::usage(
            "pane is too small to open files beside it; enlarge the pane first",
        ));
    }
    Ok(geometry)
}

/// The content pane in `layout` to show `show` in: `caller` itself when it is a content
/// pane; else one in `environment` whose root is the folder, or for a file the one whose
/// root holds it, preferring one with the file open, then the deepest root.
fn reusable(
    layout: &LayoutNode,
    caller: &PaneId,
    environment: &ContentEnvironment,
    show: &Show,
    windows: bool,
) -> Option<(PaneId, PaneContent)> {
    let mut best: Option<((bool, usize), PaneId, PaneContent)> = None;
    for leaf in arrangement::leaves(layout) {
        let LayoutNode::Content { pane_id, content } = leaf else {
            continue;
        };
        if &pane_id == caller {
            return Some((pane_id, content));
        }
        if &content.environment != environment {
            continue;
        }
        let root = resolve_path(None, &content.root, windows).ok();
        let rank = match show {
            Show::Folder(folder) => (root.as_ref() == Some(folder)).then_some((true, 0)),
            Show::File { path, .. } => root
                .filter(|root| contains(root, path, windows))
                .map(|root| (content.files.contains(path), root.len())),
        };
        if let Some(rank) = rank
            && best.as_ref().is_none_or(|(current, ..)| rank > *current)
        {
            best = Some((rank, pane_id, content));
        }
    }
    best.map(|(_, pane_id, content)| (pane_id, content))
}

/// Whether normalised `path` lies below normalised folder `root`.
fn contains(root: &str, path: &str, windows: bool) -> bool {
    let separator = if windows { '\\' } else { '/' };
    path.strip_prefix(root).is_some_and(|rest| {
        !rest.is_empty() && (root.ends_with(separator) || rest.starts_with(separator))
    })
}

/// `FILE[:LINE[:COL]]` as the file and its line. The column compilers print is accepted
/// and ignored; the cursor goes to the start of the line.
fn location(argument: &str) -> Result<(&str, Option<u32>), CliError> {
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
fn resolve_path(base: Option<&str>, path: &str, windows: bool) -> Result<String, CliError> {
    crate::content_path::resolve(base, path, windows).map_err(CliError::usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::SurfaceId;

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
    fn content_panes_are_reused_by_environment_and_root() {
        let content = |pane: &str, root: &str, files: &[&str], distribution: Option<&str>| {
            LayoutNode::Content {
                pane_id: PaneId::new(pane),
                content: PaneContent {
                    environment: distribution.map_or(ContentEnvironment::Host, |name| {
                        ContentEnvironment::Wsl {
                            distribution: name.into(),
                        }
                    }),
                    root: root.into(),
                    files: files.iter().map(|file| (*file).to_owned()).collect(),
                    active_file: None,
                },
            }
        };
        let split = |first, second| LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            ratio: 0.5,
            first: Box::new(first),
            second: Box::new(second),
        };
        let layout = split(
            split(
                LayoutNode::Pane {
                    pane_id: PaneId::new("terminal"),
                    surface_id: SurfaceId::new("surface"),
                },
                content("wsl", "/home/me/proj", &[], Some("Ubuntu")),
            ),
            split(
                content("proj", "/home/me/proj/", &[], None),
                split(
                    content("src", "/home/me/proj/src", &[], None),
                    content("open", "/home/me", &["/home/me/proj/src/lib.rs"], None),
                ),
            ),
        );
        let pick = |caller: &str, show: Show| {
            reusable(
                &layout,
                &PaneId::new(caller),
                &ContentEnvironment::Host,
                &show,
                false,
            )
            .map(|(pane, _)| pane.as_str().to_owned())
        };
        let file = |path: &str| Show::File {
            path: path.into(),
            line: None,
        };
        assert_eq!(
            pick("terminal", Show::Folder("/home/me/proj".into())).as_deref(),
            Some("proj")
        );
        assert_eq!(pick("terminal", Show::Folder("/home".into())), None);
        assert_eq!(
            pick("terminal", file("/home/me/proj/src/main.rs")).as_deref(),
            Some("src")
        );
        assert_eq!(
            pick("terminal", file("/home/me/proj/src/lib.rs")).as_deref(),
            Some("open")
        );
        assert_eq!(
            pick("terminal", file("/home/me/proj/README.md")).as_deref(),
            Some("proj")
        );
        assert_eq!(
            pick("terminal", file("/home/me/proj")).as_deref(),
            Some("open")
        );
        assert_eq!(pick("terminal", file("/srv/x")), None);
        assert_eq!(pick("src", file("/srv/x")).as_deref(), Some("src"));
    }
}
