//! `compi browse [URL|FILE|FOLDER|PORT] [--down]`: a web page in a Browser pane. From a
//! terminal it opens a Browser pane beside it (below with `--down`); from a Browser pane
//! (`--pane`) that pane goes to the page. A file or a folder's `index.html`, where the
//! pane's shell runs, opens as a `file:` page. Without an argument the page is blank, and
//! the window puts the keyboard in its address field, where a link can be pasted without
//! the shell's quoting.
use super::content::{self, Placed};
use super::{CliError, Options, Output, pane_surface, submit, targets};
use crate::{DaemonClient, cli_window::Action, connection::ConnectionTarget};
use compi_protocol::{
    BrowserContent, ContentEnvironment, LayoutNode, PaneContent, SplitAxis, SplitGeometry,
    WorkspaceMutation, WorkspaceSnapshot,
};
use serde_json::json;

/// How an argument reads as a file or folder rather than a web address.
#[derive(Debug, PartialEq)]
enum Named {
    /// `.`, `..`, or a path that says where it starts (`./`, `../`, `/`, `C:\`,
    /// `\\server\`), or a page's bare file name (`index.html`): it must exist.
    Path,
    /// A page's file name below a folder (`docs/guide.html`), which an address can
    /// also end with (`example.com/guide.html`): a file when there is one.
    Page,
}

fn named(typed: &str) -> Option<Named> {
    if typed.contains("://") || typed.starts_with("//") {
        return None;
    }
    let drive = matches!(typed.as_bytes(), [letter, b':', b'\\' | b'/', ..] if letter.is_ascii_alphabetic());
    if matches!(typed, "." | "..")
        || drive
        || ["./", "../", "/", ".\\", "..\\", "\\\\"]
            .iter()
            .any(|start| typed.starts_with(start))
    {
        return Some(Named::Path);
    }
    let lower = typed.to_ascii_lowercase();
    if ![".html", ".htm", ".xhtml", ".svg", ".pdf"]
        .iter()
        .any(|page| lower.ends_with(page))
    {
        return None;
    }
    Some(if typed.contains(['/', '\\']) {
        Named::Page
    } else {
        Named::Path
    })
}

/// The `file:` URL of the file or folder `typed` names where the selected pane's shell
/// runs, as this computer's web view opens it; `None` when `typed` is an address. A
/// folder is its `index.html`.
fn local_page(
    client: &mut DaemonClient,
    state: &WorkspaceSnapshot,
    options: &Options,
    typed: &str,
) -> Result<Option<String>, CliError> {
    let Some(kind) = named(typed) else {
        return Ok(None);
    };
    let caller = content::caller(client, state, options)?;
    let windows = caller.windows;
    let path = content::resolve_path(caller.directory.as_deref(), typed, windows)?;
    let separator = if windows { '\\' } else { '/' };
    let mut holds = |folder: &str, name: &str| {
        client
            .list_content_directory(caller.environment.clone(), folder.to_owned())
            .ok()
            .map(|entries| {
                entries
                    .into_iter()
                    .find(|entry| !entry.is_directory && entry.name.eq_ignore_ascii_case(name))
                    .map(|entry| entry.name)
            })
    };
    let file = match holds(&path, "index.html") {
        Some(Some(index)) => format!("{}{separator}{index}", path.trim_end_matches(separator)),
        Some(None) => {
            return Err(CliError::usage(format!(
                "{path} has no index.html; name a file in it, or start its server and browse the address it prints"
            )));
        }
        None => {
            let parent = content::resolve_path(None, &format!("{path}{separator}.."), windows)?;
            let name = path.rsplit(separator).next().unwrap_or_default();
            match holds(&parent, name) {
                Some(Some(_)) => path,
                _ if kind == Named::Page => return Ok(None),
                _ => return Err(CliError::usage(format!("{path} was not found"))),
            }
        }
    };
    if options.connect.is_some() {
        return Err(CliError::usage(format!(
            "{file} is on another machine, where this window's web view cannot read files; serve its folder there and browse the address"
        )));
    }
    Ok(Some(match &caller.environment {
        ContentEnvironment::Host => crate::content_path::file_url(&file, windows),
        ContentEnvironment::Wsl { distribution } => crate::content_path::file_url(
            &crate::content_path::wsl_windows_path(distribution, &file),
            true,
        ),
    }))
}

pub(super) fn run(
    client: &mut DaemonClient,
    state: &WorkspaceSnapshot,
    target: &ConnectionTarget,
    options: &Options,
) -> Result<Output, CliError> {
    let url = match options.args.get(1) {
        Some(typed) => match local_page(client, state, options, typed)? {
            Some(url) => url,
            None => {
                BrowserContent::from_typed(typed)
                    .map_err(CliError::usage)?
                    .url
            }
        },
        None => BrowserContent::BLANK.to_owned(),
    };

    let (_, _, leaf) = targets::pane(state, options)?;
    let content = PaneContent::Browser(BrowserContent { url: url.clone() });
    let placed = match &leaf {
        LayoutNode::Content {
            pane_id,
            content: PaneContent::Browser(shown),
        } => Placed {
            pane_id: pane_id.clone(),
            receipt: (shown.url != url)
                .then(|| {
                    submit(
                        client,
                        state,
                        WorkspaceMutation::UpdateContent {
                            pane_id: pane_id.clone(),
                            content,
                        },
                        None,
                    )
                })
                .transpose()?,
        },
        LayoutNode::Pane {
            pane_id,
            surface_id,
            ..
        } => {
            let surface = pane_surface(state, surface_id)?;
            let axis = if options.switches.contains("--down") {
                SplitAxis::Vertical
            } else {
                SplitAxis::Horizontal
            };
            let receipt = submit(
                client,
                state,
                WorkspaceMutation::OpenContent {
                    pane_id: pane_id.clone(),
                    axis,
                    // In the terminal's cells; the window lays the panes out in pixels.
                    geometry: SplitGeometry {
                        width: f32::from(surface.cols),
                        height: f32::from(surface.rows),
                        min_width: 20.0,
                        min_height: 4.0,
                        divider: 1.0,
                    },
                    content,
                },
                None,
            )?;
            let opened = receipt
                .affected_panes
                .iter()
                .find(|pane| *pane != pane_id)
                .cloned()
                .ok_or_else(|| {
                    CliError::new(1, "pane_missing", "the new Browser pane was not reported")
                })?;
            Placed {
                pane_id: opened,
                receipt: Some(receipt),
            }
        }
        LayoutNode::Content { pane_id, content } => {
            return Err(CliError::new(
                3,
                "terminal_or_browser_pane_required",
                format!(
                    "pane {pane_id} shows {}; browse needs a terminal pane or a Browser pane",
                    content.label()
                ),
            ));
        }
        LayoutNode::Split { .. } => unreachable!("targets resolve only leaf nodes"),
    };
    let pane_id = placed.pane_id.clone();
    let (text, mut value) = content::report(
        client,
        target,
        options,
        &placed,
        &format!("Browser · {url}"),
        Action::FocusPane { pane_id },
    )?;
    value["url"] = json!(url);
    Ok(Output::Report { text, value })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_and_folders_are_told_from_addresses() {
        for path in [
            ".",
            "..",
            "./site",
            "../x/y.html",
            "/home/me/app",
            r"C:\site",
            "C:/site",
            r"\\wsl.localhost\Ubuntu\home",
            "index.html",
            "Page.HTM",
            "logo.svg",
        ] {
            assert_eq!(named(path), Some(Named::Path), "{path}");
        }
        // An address can end like a page too; a file wins only when it is there.
        assert_eq!(named("docs/guide.html"), Some(Named::Page));
        assert_eq!(named("example.com/guide.html"), Some(Named::Page));
        for address in [
            "localhost:3000",
            "5173",
            "example.com",
            "docs",
            "https://example.com/a.html",
            "file:///tmp/a.html",
            "//example.com/a.html",
            "about:blank",
        ] {
            assert_eq!(named(address), None, "{address}");
        }
    }
}
