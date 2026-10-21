//! `compi diff [PATH[:LINE]]` and `compi diff COMMIT [PATH[:LINE]]`: a changed file's tab,
//! in Diff mode, shown in the pane it runs from (a terminal's view, or a files content pane
//! inside its repository), or with no PATH that pane's Changes list. A COMMIT shows what it
//! changed against its first parent, read-only. A lone argument is a PATH when it names a
//! file with uncommitted changes, else a COMMIT when Git knows it as one. Git is only read,
//! never written.
use super::content::{self, Caller, Placed, Show};
use super::{CliError, Options, Output};
use crate::content_path::repo_relative;
use crate::{DaemonClient, cli_window::Action, connection::ConnectionTarget};
use compi_protocol::{
    ContentEnvironment, ContentTab, GitChanges, GitCommit, GitCommitLookup, GitScope,
    WorkspaceSnapshot, is_revision,
};
use serde_json::{Value, json};

pub(super) fn run(
    client: &mut DaemonClient,
    state: &WorkspaceSnapshot,
    target: &ConnectionTarget,
    options: &Options,
) -> Result<Output, CliError> {
    let caller = content::caller(client, state, options)?;
    let mut run = Run {
        client,
        state,
        target,
        options,
        caller,
    };
    let (text, value) = match (options.args.get(1), options.args.get(2)) {
        (None, _) => run.working_changes()?,
        (Some(argument), None) => run.lone(argument)?,
        (Some(revision), Some(argument)) => run.commit_file(revision, argument)?,
    };
    Ok(Output::Report { text, value })
}

/// What a lone argument names before Git is asked for a commit: a file with uncommitted
/// changes (the scope its diff opens in), else maybe a commit (it is a revision), else
/// neither.
#[derive(Debug, PartialEq, Eq)]
enum Lone {
    Changed(GitScope),
    Revision,
    Neither,
}

/// What the lone `argument` names, `relative` being its path in `listing`'s repository
/// (`None` outside it).
fn lone(listing: &GitChanges, relative: Option<&str>, argument: &str) -> Lone {
    match relative.and_then(|relative| unstaged_first(listing, relative)) {
        Some(scope) => Lone::Changed(scope),
        None if is_revision(argument) => Lone::Revision,
        None => Lone::Neither,
    }
}

/// One `compi diff` run: the daemon, the workspace, and the pane it runs from.
struct Run<'a> {
    client: &'a mut DaemonClient,
    state: &'a WorkspaceSnapshot,
    target: &'a ConnectionTarget,
    options: &'a Options,
    caller: Caller<'a>,
}

impl Run<'_> {
    /// `compi diff`: the Changes list of the repository holding the pane's folder.
    fn working_changes(&mut self) -> Result<(String, Value), CliError> {
        let folder = self.caller.directory()?.to_owned();
        let listing = read(self.client, &self.caller.environment, &folder)?;
        let subject = format!(
            "the changes of {}",
            content::resolve_path(None, &listing.repo, self.caller.windows)?
        );
        let (text, mut value) = self.show_changes(&listing.repo, None, &subject)?;
        value["repo"] = json!(listing.repo);
        value["branch"] = json!(listing.branch);
        value["changes"] = json!(listing.changes.len());
        Ok((text, value))
    }

    /// `compi diff ARGUMENT`: the diff of a file with uncommitted changes, else the changes
    /// of a commit.
    fn lone(&mut self, argument: &str) -> Result<(String, Value), CliError> {
        let windows = self.caller.windows;
        let (path, line) = content::location(argument)?;
        let path = content::resolve_path(self.caller.directory.as_deref(), path, windows)?;
        // The folder holds the file even when the change deleted it.
        let folder = content::resolve_path(None, &format!("{path}/.."), windows)?;
        let listing = match read(self.client, &self.caller.environment, &folder) {
            Ok(listing) => listing,
            // A revision such as `origin/main` names no folder; the pane's folder may know it.
            Err(error) => {
                let commit = match self.caller.directory.clone() {
                    Some(directory) if is_revision(argument) => {
                        self.find_commit(&directory, argument)?
                    }
                    _ => None,
                };
                return match commit {
                    Some(commit) => self.commit_changes(commit),
                    None => Err(error),
                };
            }
        };
        let relative = repo_relative(&listing.repo, &path);
        let verdict = lone(&listing, relative.as_deref(), argument);
        if let (Lone::Changed(scope), Some(relative)) = (&verdict, &relative) {
            return self.working_file(&listing, path, relative, scope.clone(), line);
        }
        if verdict == Lone::Revision
            && let Some(commit) = self.find_commit(&listing.repo, argument)?
        {
            return self.commit_changes(commit);
        }
        let Some(relative) = relative else {
            return Err(CliError::usage(format!(
                "{path} is not a file in the repository at {}",
                listing.repo
            )));
        };
        let mut message = format!("{relative} has no uncommitted changes");
        if listing.truncated {
            message.push_str(&format!(
                " among the first {} changes listed",
                listing.changes.len()
            ));
        }
        if verdict == Lone::Revision {
            message.push_str(" and names no commit");
        }
        Err(CliError::new(1, "no_changes", message))
    }

    /// The diff of the working file at absolute `path` (`relative` in `listing`'s
    /// repository) in `scope`.
    fn working_file(
        &mut self,
        listing: &GitChanges,
        path: String,
        relative: &str,
        scope: GitScope,
        line: Option<u32>,
    ) -> Result<(String, Value), CliError> {
        let subject = format!(
            "the {}diff of {}",
            if scope == GitScope::Staged {
                "staged "
            } else {
                ""
            },
            located(relative, line)
        );
        let (text, mut value) =
            self.show_diff(&listing.repo, path, relative, scope.clone(), line, &subject)?;
        value["scope"] = json!(scope);
        Ok((text, value))
    }

    /// `compi diff COMMIT PATH`: the diff of PATH in the commit.
    fn commit_file(&mut self, revision: &str, argument: &str) -> Result<(String, Value), CliError> {
        if !is_revision(revision) {
            return Err(CliError::usage(format!("{revision} is not a commit")));
        }
        let windows = self.caller.windows;
        let (path, line) = content::location(argument)?;
        let path = content::resolve_path(self.caller.directory.as_deref(), path, windows)?;
        // The pane's folder, as a file the commit changed may since have left the work tree.
        let folder = match self.caller.directory.clone() {
            Some(directory) => directory,
            None => content::resolve_path(None, &format!("{path}/.."), windows)?,
        };
        let commit = match self.client.read_git_commit(
            self.caller.environment.clone(),
            folder.clone(),
            revision.to_owned(),
        )? {
            GitCommitLookup::Found { commit } => *commit,
            GitCommitLookup::Unknown => {
                return Err(CliError::new(
                    1,
                    "unknown_commit",
                    format!("{revision} names no commit in the repository holding {folder}"),
                ));
            }
            GitCommitLookup::NotRepository => return Err(not_a_repository(&folder)),
        };
        let relative = repo_relative(&commit.repo, &path).ok_or_else(|| {
            CliError::usage(format!(
                "{path} is not a file in the repository at {}",
                commit.repo
            ))
        })?;
        if !commit.changes.iter().any(|change| change.path == relative) {
            let mut message = format!("{relative} is not changed in {}", commit.short);
            if commit.truncated {
                message.push_str(&format!(
                    " among the first {} changes listed",
                    commit.changes.len()
                ));
            }
            return Err(CliError::new(1, "no_changes", message));
        }
        let subject = format!("the {} diff of {}", commit.short, located(&relative, line));
        let scope = GitScope::Commit(commit.id.clone());
        let (text, mut value) =
            self.show_diff(&commit.repo, path, &relative, scope, line, &subject)?;
        add_commit(&mut value, &commit);
        Ok((text, value))
    }

    /// The changes of `commit` in the Changes list.
    fn commit_changes(&mut self, commit: GitCommit) -> Result<(String, Value), CliError> {
        let subject = format!("the changes of {} ({})", commit.short, commit.summary);
        let (text, mut value) =
            self.show_changes(&commit.repo, Some(commit.id.clone()), &subject)?;
        value["repo"] = json!(commit.repo);
        add_commit(&mut value, &commit);
        value["changes"] = json!(commit.changes.len());
        Ok((text, value))
    }

    /// `revision`'s commit in the repository holding `folder`; `None` when Git knows no
    /// such commit there, or `folder` is in no repository.
    fn find_commit(&mut self, folder: &str, revision: &str) -> Result<Option<GitCommit>, CliError> {
        Ok(
            match self.client.read_git_commit(
                self.caller.environment.clone(),
                folder.to_owned(),
                revision.to_owned(),
            )? {
                GitCommitLookup::Found { commit } => Some(*commit),
                GitCommitLookup::Unknown | GitCommitLookup::NotRepository => None,
            },
        )
    }

    /// The Changes list of the repository at `repo`, with `commit`'s section when given,
    /// in the caller's pane: a content pane keeps its folder, a terminal's view shows the
    /// repository.
    fn show_changes(
        &mut self,
        repo: &str,
        commit: Option<String>,
        subject: &str,
    ) -> Result<(String, Value), CliError> {
        let placed = if self.caller.surface.is_none() {
            Placed {
                pane_id: self.caller.id.clone(),
                receipt: None,
            }
        } else {
            let repo = content::resolve_path(None, repo, self.caller.windows)?;
            content::place(self.client, self.state, &self.caller, &Show::Folder(repo))?
        };
        let action = Action::ShowChanges {
            pane_id: placed.pane_id.clone(),
            commit,
        };
        content::report(
            self.client,
            self.target,
            self.options,
            &placed,
            subject,
            action,
        )
    }

    /// The tab of absolute `path` (`relative` in the repository at `repo`) in Diff mode at
    /// `scope`, scrolled to the new side's `line`.
    fn show_diff(
        &mut self,
        repo: &str,
        path: String,
        relative: &str,
        scope: GitScope,
        line: Option<u32>,
        subject: &str,
    ) -> Result<(String, Value), CliError> {
        let windows = self.caller.windows;
        let within = content::resolve_path(None, repo, windows)?;
        if self.caller.surface.is_none()
            && self
                .caller
                .directory
                .as_deref()
                .is_some_and(|root| root != within && !content::contains(&within, root, windows))
        {
            return Err(CliError::usage(format!(
                "pane {} shows a folder outside the repository at {repo}; run diff from a terminal or name a content pane inside it",
                self.caller.id
            )));
        }
        let show = Show::Tab {
            tab: ContentTab::diff(path.clone(), scope.clone()),
            file: path,
            within: Some(within),
            root: repo.to_owned(),
        };
        let placed = content::place(self.client, self.state, &self.caller, &show)?;
        let action = Action::RevealDiff {
            pane_id: placed.pane_id.clone(),
            path: relative.to_owned(),
            scope,
            line,
        };
        let (text, mut value) = content::report(
            self.client,
            self.target,
            self.options,
            &placed,
            subject,
            action,
        )?;
        value["repo"] = json!(repo);
        value["path"] = json!(relative);
        value["line"] = json!(line);
        Ok((text, value))
    }
}

/// `relative`, with `:line` when given.
fn located(relative: &str, line: Option<u32>) -> String {
    line.map_or_else(|| relative.to_owned(), |line| format!("{relative}:{line}"))
}

/// The JSON report's `commit` (full id), `short` and `summary`.
fn add_commit(value: &mut Value, commit: &GitCommit) {
    value["commit"] = json!(commit.id);
    value["short"] = json!(commit.short);
    value["summary"] = json!(commit.summary);
}

fn not_a_repository(folder: &str) -> CliError {
    CliError::new(
        1,
        "not_a_repository",
        format!("{folder} is not in a Git repository"),
    )
}

/// The Git changes of the work tree holding `folder`; outside one, an error.
fn read(
    client: &mut DaemonClient,
    environment: &ContentEnvironment,
    folder: &str,
) -> Result<GitChanges, CliError> {
    client
        .read_git_changes(environment.clone(), folder.to_owned())?
        .ok_or_else(|| not_a_repository(folder))
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

    fn listing() -> GitChanges {
        let change = |scope, path: &str| GitChange {
            scope,
            kind: GitChangeKind::Modified,
            path: path.into(),
            old_path: None,
            added: Some(1),
            removed: Some(1),
            stamp: String::new(),
        };
        GitChanges {
            repo: "/home/me/proj".into(),
            branch: Some("main".into()),
            head: None,
            changes: vec![
                change(GitScope::Unstaged, "both.rs"),
                change(GitScope::Staged, "both.rs"),
                change(GitScope::Staged, "staged.rs"),
                change(GitScope::Unstaged, "HEAD"),
            ],
            truncated: false,
            ..Default::default()
        }
    }

    #[test]
    fn unstaged_changes_win_over_staged_ones() {
        let listing = listing();
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

    #[test]
    fn a_lone_argument_is_a_changed_file_before_a_commit() {
        let listing = listing();
        assert_eq!(
            lone(&listing, Some("staged.rs"), "staged.rs"),
            Lone::Changed(GitScope::Staged)
        );
        // A changed file named like a revision is the file.
        assert_eq!(
            lone(&listing, Some("HEAD"), "HEAD"),
            Lone::Changed(GitScope::Unstaged)
        );
        assert_eq!(lone(&listing, Some("HEAD~1"), "HEAD~1"), Lone::Revision);
        assert_eq!(lone(&listing, None, "origin/main"), Lone::Revision);
        // `:` names a line, never a commit; nor does an option.
        assert_eq!(
            lone(&listing, Some("clean.rs"), "clean.rs:4"),
            Lone::Neither
        );
        assert_eq!(lone(&listing, Some("-x"), "-x"), Lone::Neither);
    }
}
