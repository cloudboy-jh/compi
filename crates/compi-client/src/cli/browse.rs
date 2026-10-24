//! `compi browse URL [--down]`: a web page in a Browser pane. From a terminal it opens a
//! Browser pane beside it (below with `--down`); from a Browser pane (`--pane`) that pane
//! goes to the URL.
use super::content::{self, Placed};
use super::{CliError, Options, Output, pane_surface, submit, targets};
use crate::{DaemonClient, cli_window::Action, connection::ConnectionTarget};
use compi_protocol::{
    BrowserContent, LayoutNode, PaneContent, SplitAxis, SplitGeometry, WorkspaceMutation,
    WorkspaceSnapshot,
};
use serde_json::json;

pub(super) fn run(
    client: &mut DaemonClient,
    state: &WorkspaceSnapshot,
    target: &ConnectionTarget,
    options: &Options,
) -> Result<Output, CliError> {
    let url = BrowserContent::from_typed(&options.args[1])
        .map_err(CliError::usage)?
        .url;

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
