//! `compi diff [PATH[:LINE]]`: a changed file's diff as a tab of the content pane of its
//! repository, or with no PATH that pane's Changes list. Panes are found or opened as
//! `compi edit` does; Git is only read, never written.
use super::content::{self, Placed, Show};
use super::{CliError, Options, Output};
use crate::content_path::repo_relative;
use crate::{DaemonClient, cli_window::Action, connection::ConnectionTarget};
use compi_protocol::{ContentTab, GitChanges, GitScope, WorkspaceSnapshot};
use serde_json::json;

pub(super) fn run(
    client: &mut DaemonClient,
    state: &WorkspaceSnapshot,
    target: &ConnectionTarget,
    options: &Options,
) -> Result<Output, CliError> {
    let caller = content::caller(client, state, options)?;
    let windows = caller.windows;
    let directory = caller.directory.as_deref();
    let Some(argument) = options.args.get(1) else {
        let folder = caller.directory()?.to_owned();
        let listing = read(client, &caller.environment, &folder)?;
        let repo = content::resolve_path(None, &listing.repo, windows)?;
        // A content pane keeps its folder and lists the changes in its sidebar.
        let placed = if caller.surface.is_none() {
            Placed {
                pane_id: caller.id.clone(),
                receipt: None,
                opened: false,
            }
        } else {
            content::place(client, state, &caller, &Show::Folder(repo.clone()))?
        };
        let action = Action::ShowChanges {
            pane_id: placed.pane_id.clone(),
        };
        let subject = format!("the changes of {repo}");
        let (text, mut value) =
            content::report(client, target, options, &placed, &subject, action)?;
        value["repo"] = json!(listing.repo);
        value["branch"] = json!(listing.branch);
        value["changes"] = json!(listing.changes.len());
        return Ok(Output::Report { text, value });
    };
    let (path, line) = content::location(argument)?;
    let path = content::resolve_path(directory, path, windows)?;
    // The folder holds the file even when the change deleted it.
    let folder = content::resolve_path(None, &format!("{path}/.."), windows)?;
    let listing = read(client, &caller.environment, &folder)?;
    let relative = repo_relative(&listing.repo, &path).ok_or_else(|| {
        CliError::usage(format!(
            "{path} is not a file in the repository at {}",
            listing.repo
        ))
    })?;
    let scope = unstaged_first(&listing, &relative).ok_or_else(|| {
        let mut message = format!("{relative} has no uncommitted changes");
        if listing.truncated {
            message.push_str(&format!(
                " among the first {} changes listed",
                listing.changes.len()
            ));
        }
        CliError::new(1, "no_changes", message)
    })?;
    let within = content::resolve_path(None, &listing.repo, windows)?;
    if caller.surface.is_none()
        && caller
            .directory
            .as_deref()
            .is_some_and(|root| root != within && !content::contains(&within, root, windows))
    {
        return Err(CliError::usage(format!(
            "pane {} shows a folder outside the repository at {}; run diff from a terminal or name a content pane inside it",
            caller.id, listing.repo
        )));
    }
    let show = Show::Tab {
        tab: ContentTab::Diff {
            repo: listing.repo.clone(),
            path: relative.clone(),
            scope,
        },
        file: path,
        within: Some(within),
        root: listing.repo.clone(),
    };
    let placed = content::place(client, state, &caller, &show)?;
    let action = Action::RevealDiff {
        pane_id: placed.pane_id.clone(),
        path: relative.clone(),
        scope,
        line,
    };
    let subject = format!(
        "the {}diff of {}",
        if scope == GitScope::Staged {
            "staged "
        } else {
            ""
        },
        line.map_or_else(|| relative.clone(), |line| format!("{relative}:{line}"))
    );
    let (text, mut value) = content::report(client, target, options, &placed, &subject, action)?;
    value["repo"] = json!(listing.repo);
    value["path"] = json!(relative);
    value["scope"] = json!(scope);
    value["line"] = json!(line);
    Ok(Output::Report { text, value })
}

/// The Git changes of the work tree holding `folder`; outside one, an error.
fn read(
    client: &mut DaemonClient,
    environment: &compi_protocol::ContentEnvironment,
    folder: &str,
) -> Result<GitChanges, CliError> {
    client
        .read_git_changes(environment.clone(), folder.to_owned())?
        .ok_or_else(|| {
            CliError::new(
                1,
                "not_a_repository",
                format!("{folder} is not in a Git repository"),
            )
        })
}

/// The scope a diff of `path` opens in: its unstaged change, else its staged one.
fn unstaged_first(listing: &GitChanges, path: &str) -> Option<GitScope> {
    [GitScope::Unstaged, GitScope::Staged]
        .into_iter()
        .find(|scope| {
            listing
                .changes
                .iter()
                .any(|change| change.scope == *scope && change.path == path)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::{GitChange, GitChangeKind};

    #[test]
    fn unstaged_changes_win_over_staged_ones() {
        let change = |scope, path: &str| GitChange {
            scope,
            kind: GitChangeKind::Modified,
            path: path.into(),
            old_path: None,
            added: Some(1),
            removed: Some(1),
            stamp: String::new(),
        };
        let listing = GitChanges {
            repo: "/home/me/proj".into(),
            branch: Some("main".into()),
            head: None,
            changes: vec![
                change(GitScope::Unstaged, "both.rs"),
                change(GitScope::Staged, "both.rs"),
                change(GitScope::Staged, "staged.rs"),
            ],
            truncated: false,
        };
        assert_eq!(
            unstaged_first(&listing, "both.rs"),
            Some(GitScope::Unstaged)
        );
        assert_eq!(
            unstaged_first(&listing, "staged.rs"),
            Some(GitScope::Staged)
        );
        assert_eq!(unstaged_first(&listing, "clean.rs"), None);
    }
}
