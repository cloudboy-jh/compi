use super::{CliError, Options};
use crate::arrangement;
use compi_protocol::{PaneId, SurfaceId, WorkspaceSession, WorkspaceSnapshot, WorkspaceTab};

fn unique<T>(mut matches: Vec<T>, kind: &str, selector: Option<&str>) -> Result<T, CliError> {
    match matches.len() {
        1 => Ok(matches.pop().unwrap()),
        0 => Err(CliError::target(format!(
            "{kind} {} was not found",
            selector.unwrap_or("target")
        ))),
        _ => Err(CliError::target(format!(
            "ambiguous {kind} {}; use a stable ID or qualified workspace/tab name",
            selector.unwrap_or("target")
        ))),
    }
}

pub fn workspace<'a>(
    state: &'a WorkspaceSnapshot,
    options: &Options,
) -> Result<&'a WorkspaceSession, CliError> {
    if let Some(selector) = options.workspace.as_deref() {
        return find_workspace(state, selector);
    }
    if let Some(selector) = options.tab.as_deref() {
        return Ok(find_tab(state, selector, None)?.0);
    }
    if let Some(selector) = options.pane.as_deref() {
        return Ok(find_pane(state, Some(selector), None, None)?.0);
    }
    if let Some((session, _, _)) = context(state, options) {
        return Ok(session);
    }
    unique(state.sessions.iter().collect(), "workspace", None)
}

pub fn tab<'a>(
    state: &'a WorkspaceSnapshot,
    options: &Options,
) -> Result<(&'a WorkspaceSession, &'a WorkspaceTab), CliError> {
    let selected_workspace = options
        .workspace
        .as_deref()
        .map(|selector| find_workspace(state, selector))
        .transpose()?;
    if let Some(selector) = options.tab.as_deref() {
        return find_tab(state, selector, selected_workspace);
    }
    if let Some(selector) = options.pane.as_deref() {
        let (session, tab, _, _) = find_pane(state, Some(selector), selected_workspace, None)?;
        return Ok((session, tab));
    }
    if let Some((session, tab, _)) = context(state, options)
        && selected_workspace.is_none_or(|selected| selected.id == session.id)
    {
        return Ok((session, tab));
    }
    unique(
        state
            .sessions
            .iter()
            .filter(|session| selected_workspace.is_none_or(|selected| selected.id == session.id))
            .flat_map(|session| session.tabs.iter().map(move |tab| (session, tab)))
            .collect(),
        "tab",
        None,
    )
}

pub fn pane<'a>(
    state: &'a WorkspaceSnapshot,
    options: &Options,
) -> Result<(&'a WorkspaceSession, &'a WorkspaceTab, PaneId, SurfaceId), CliError> {
    let selected_workspace = options
        .workspace
        .as_deref()
        .map(|selector| find_workspace(state, selector))
        .transpose()?;
    let selected_tab = options
        .tab
        .as_deref()
        .map(|selector| find_tab(state, selector, selected_workspace))
        .transpose()?
        .map(|(_, tab)| tab);
    if options.pane.is_none()
        && let Some((session, tab, surface)) = context(state, options)
        && selected_workspace.is_none_or(|selected| selected.id == session.id)
        && selected_tab.is_none_or(|selected| selected.id == tab.id)
    {
        return find_pane(
            state,
            Some(surface.as_str()),
            selected_workspace,
            selected_tab,
        );
    }
    find_pane(
        state,
        options.pane.as_deref(),
        selected_workspace,
        selected_tab,
    )
}

fn find_workspace<'a>(
    state: &'a WorkspaceSnapshot,
    selector: &str,
) -> Result<&'a WorkspaceSession, CliError> {
    if let Some(session) = state
        .sessions
        .iter()
        .find(|session| session.id.as_str() == selector)
    {
        return Ok(session);
    }
    unique(
        state
            .sessions
            .iter()
            .filter(|session| session.label == selector)
            .collect(),
        "workspace",
        Some(selector),
    )
}

fn find_tab<'a>(
    state: &'a WorkspaceSnapshot,
    selector: &str,
    workspace: Option<&WorkspaceSession>,
) -> Result<(&'a WorkspaceSession, &'a WorkspaceTab), CliError> {
    let candidates = state
        .sessions
        .iter()
        .filter(|session| workspace.is_none_or(|selected| selected.id == session.id))
        .flat_map(|session| session.tabs.iter().map(move |tab| (session, tab)));
    if let Some(found) = candidates
        .clone()
        .find(|(_, tab)| tab.id.as_str() == selector)
    {
        return Ok(found);
    }
    unique(
        candidates
            .filter(|(session, tab)| {
                tab.label == selector || qualified(selector, &[&session.label, &tab.label])
            })
            .collect(),
        "tab",
        Some(selector),
    )
}

fn find_pane<'a>(
    state: &'a WorkspaceSnapshot,
    selector: Option<&str>,
    workspace: Option<&WorkspaceSession>,
    tab: Option<&WorkspaceTab>,
) -> Result<(&'a WorkspaceSession, &'a WorkspaceTab, PaneId, SurfaceId), CliError> {
    let mut candidates = Vec::new();
    for session in &state.sessions {
        if workspace.is_some_and(|selected| selected.id != session.id) {
            continue;
        }
        for current_tab in &session.tabs {
            if tab.is_some_and(|selected| selected.id != current_tab.id) {
                continue;
            }
            for (id, surface) in arrangement::leaves(&current_tab.layout) {
                if let Some(selector) = selector {
                    if id.as_str() == selector || surface.as_str() == selector {
                        return Ok((session, current_tab, id, surface));
                    }
                    if !qualified(selector, &[&session.label, &current_tab.label, id.as_str()]) {
                        continue;
                    }
                }
                candidates.push((session, current_tab, id, surface));
            }
        }
    }
    unique(candidates, "pane", selector)
}

fn qualified(mut selector: &str, parts: &[&str]) -> bool {
    for (index, part) in parts.iter().enumerate() {
        let Some(rest) = selector.strip_prefix(*part) else {
            return false;
        };
        selector = rest;
        if index + 1 < parts.len() {
            let Some(rest) = selector.strip_prefix('/') else {
                return false;
            };
            selector = rest;
        }
    }
    selector.is_empty()
}

fn context<'a>(
    state: &'a WorkspaceSnapshot,
    options: &Options,
) -> Option<(&'a WorkspaceSession, &'a WorkspaceTab, &'a SurfaceId)> {
    let surface = options.surface.as_deref()?;
    for session in &state.sessions {
        for tab in &session.tabs {
            fn find<'a>(
                node: &'a compi_protocol::LayoutNode,
                surface: &str,
            ) -> Option<&'a SurfaceId> {
                match node {
                    compi_protocol::LayoutNode::Pane { surface_id, .. }
                        if surface_id.as_str() == surface =>
                    {
                        Some(surface_id)
                    }
                    compi_protocol::LayoutNode::Split { first, second, .. } => {
                        find(first, surface).or_else(|| find(second, surface))
                    }
                    _ => None,
                }
            }
            if let Some(surface) = find(&tab.layout, surface) {
                return Some((session, tab, surface));
            }
        }
    }
    None
}

pub fn validate_context(state: &WorkspaceSnapshot, options: &Options) -> Result<(), CliError> {
    if options.pane.is_some() {
        pane(state, options)?;
    } else if options.tab.is_some() {
        tab(state, options)?;
    } else if options.workspace.is_some() {
        workspace(state, options)?;
    }
    if options.surface.is_some()
        && context(state, options).is_none()
        && options.workspace.is_none()
        && options.tab.is_none()
        && options.pane.is_none()
    {
        return Err(CliError::target(
            "shell context pane no longer exists; specify --pane/--tab/--workspace or an explicit instance",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::{LayoutNode, ServerGeneration, ServerId, SessionId, TabId};

    fn state() -> WorkspaceSnapshot {
        let session =
            |id: &str, label: &str, tab_id: &str, pane: &str, surface: &str| WorkspaceSession {
                id: SessionId::new(id),
                label: label.into(),
                tabs: vec![WorkspaceTab {
                    id: TabId::new(tab_id),
                    label: "build".into(),
                    layout: LayoutNode::Pane {
                        pane_id: PaneId::new(pane),
                        surface_id: SurfaceId::new(surface),
                    },
                    previous_layout: None,
                    merge: None,
                }],
            };
        WorkspaceSnapshot {
            server_id: ServerId::new("server"),
            server_generation: ServerGeneration::new("generation"),
            revision: 1,
            initialized: true,
            sessions: vec![
                session("session-a", "alpha", "tab-a", "pane-a", "surface-a"),
                session("session-b", "beta", "tab-b", "pane-b", "surface-b"),
            ],
            surfaces: vec![],
            recovery_message: None,
        }
    }

    #[test]
    fn duplicate_tab_names_require_an_id_or_qualification() {
        let state = state();
        let options = Options {
            tab: Some("build".into()),
            ..Default::default()
        };
        assert_eq!(tab(&state, &options).unwrap_err().exit, 3);
        let qualified = Options {
            tab: Some("beta/build".into()),
            ..options
        };
        assert_eq!(tab(&state, &qualified).unwrap().1.id.as_str(), "tab-b");
    }

    #[test]
    fn stable_ids_take_precedence_over_colliding_labels() {
        let mut state = state();
        state.sessions[1].tabs[0].label = "tab-a".into();
        let options = Options {
            tab: Some("tab-a".into()),
            ..Default::default()
        };
        assert_eq!(tab(&state, &options).unwrap().1.id.as_str(), "tab-a");
    }

    #[test]
    fn explicit_pane_resolves_its_parents_instead_of_inherited_shell_context() {
        let state = state();
        let options = Options {
            pane: Some("pane-b".into()),
            surface: Some("surface-a".into()),
            ..Default::default()
        };
        assert_eq!(tab(&state, &options).unwrap().1.id.as_str(), "tab-b");
        assert_eq!(
            workspace(&state, &options).unwrap().id.as_str(),
            "session-b"
        );
        assert_eq!(pane(&state, &options).unwrap().2.as_str(), "pane-b");
    }

    #[test]
    fn explicit_tab_replaces_incompatible_inherited_pane_context() {
        let state = state();
        let options = Options {
            tab: Some("tab-b".into()),
            surface: Some("surface-a".into()),
            ..Default::default()
        };
        assert_eq!(pane(&state, &options).unwrap().2.as_str(), "pane-b");
        let contradicted = Options {
            workspace: Some("session-a".into()),
            ..options
        };
        assert_eq!(validate_context(&state, &contradicted).unwrap_err().exit, 3);
    }
}
