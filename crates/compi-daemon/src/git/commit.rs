//! One commit of a work tree's repository and what it changed against its first parent.

use super::{
    MAX_LISTING_BYTES, MAX_TOP_LEVEL_BYTES, Site, TOP_LEVEL, change_kind, git, joined, listing,
    parse_numstat, records, top_level, work_folder,
};
use crate::content_files::Failure;
use compi_protocol::{
    ContentEnvironment, ErrorCode, GitChange, GitChangeKind, GitCommit, GitCommitLookup, GitScope,
    MAX_GIT_CHANGES, is_revision,
};
use std::thread;

/// The commit fields `parse_commit` reads, NUL-separated (none can hold a NUL): full id,
/// abbreviated id, parent ids, author time, author name, subject.
const COMMIT_FORMAT: &str = "--format=%H%x00%h%x00%P%x00%at%x00%an%x00%s";
/// A tree diff between two commits, or of a root commit against nothing, followed by
/// `--name-status` or `--numstat`. Plumbing: no rename, external diff or textconv settings
/// apply.
const DIFF_TREE: &[&str] = &[
    "diff-tree",
    "-r",
    "-z",
    "-M",
    "--no-commit-id",
    "--no-ext-diff",
    "--no-textconv",
];

/// The commit `revision` names in the repository of the work tree holding `path`, a folder
/// or a file, with its changes against its first parent (a root commit's against nothing).
pub fn read_commit(
    environment: &ContentEnvironment,
    path: &str,
    revision: &str,
) -> Result<GitCommitLookup, Failure> {
    if !is_revision(revision) {
        return Err((
            ErrorCode::InvalidRequest,
            format!("{revision:?} is not a revision"),
        ));
    }
    let folder = work_folder(environment, path)?;
    let site = Site::of(environment, &folder);
    let (git_environment, git_folder) = (&site.environment, site.folder.as_str());
    let peeled = format!("{revision}^{{commit}}");
    let resolve = [
        "rev-parse",
        "--verify",
        "--quiet",
        "--end-of-options",
        peeled.as_str(),
    ];
    let (top, resolved) = thread::scope(|scope| {
        let top = scope.spawn(|| git(git_environment, git_folder, TOP_LEVEL, MAX_TOP_LEVEL_BYTES));
        let resolved =
            scope.spawn(|| git(git_environment, git_folder, &resolve, MAX_TOP_LEVEL_BYTES));
        (joined(top), joined(resolved))
    });
    let Some((repo, _)) = top_level(top?, environment, &site, &folder)? else {
        return Ok(GitCommitLookup::NotRepository);
    };
    let resolved = resolved?;
    let id = String::from_utf8_lossy(&resolved.stdout).trim().to_owned();
    if !resolved.success || id.is_empty() {
        return Ok(GitCommitLookup::Unknown);
    }

    let shown = listing(
        git(
            git_environment,
            git_folder,
            &[
                "show",
                "-s",
                "--no-show-signature",
                COMMIT_FORMAT,
                &id,
                "--",
            ],
            MAX_TOP_LEVEL_BYTES,
        )?,
        "could not read the commit",
    )?;
    let mut commit =
        parse_commit(&shown.stdout).ok_or_else(|| shown.failure("could not read the commit"))?;
    // A root commit lists every file it holds as added.
    let sides = match &commit.parent {
        Some(parent) => [parent.as_str(), id.as_str()],
        None => ["--root", id.as_str()],
    };
    let (statuses, counts) = thread::scope(|scope| {
        let spawn = |format: &'static str| {
            let args: Vec<&str> = DIFF_TREE
                .iter()
                .copied()
                .chain([format])
                .chain(sides)
                .chain(["--"])
                .collect();
            scope.spawn(move || git(git_environment, git_folder, &args, MAX_LISTING_BYTES))
        };
        let statuses = spawn("--name-status");
        let counts = spawn("--numstat");
        (joined(statuses), joined(counts))
    });
    let statuses = listing(statuses?, "could not list the commit's changes")?;
    let counts = parse_numstat(&listing(counts?, "could not count the commit's lines")?.stdout);

    let mut entries = parse_name_status(&statuses.stdout);
    entries.sort_by(|a, b| a.1.cmp(&b.1));
    commit.truncated = statuses.cut || entries.len() > MAX_GIT_CHANGES;
    entries.truncate(MAX_GIT_CHANGES);
    commit.changes = entries
        .into_iter()
        .map(|(kind, path, old_path)| {
            let (added, removed) = counts.get(&path).copied().unwrap_or_default();
            GitChange {
                scope: GitScope::Commit(id.clone()),
                kind,
                path,
                old_path,
                added,
                removed,
                stamp: id.clone(),
            }
        })
        .collect();
    commit.repo = repo;
    Ok(GitCommitLookup::Found {
        commit: Box::new(commit),
    })
}

/// A commit's fields from `COMMIT_FORMAT` output, without its repository and changes.
fn parse_commit(output: &[u8]) -> Option<GitCommit> {
    let output = String::from_utf8_lossy(output);
    let mut fields = output.trim_end_matches('\n').splitn(6, '\0');
    let (id, short, parents, time, author, summary) = (
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    );
    let mut parents = parents.split_whitespace();
    Some(GitCommit {
        id: id.to_owned(),
        short: short.to_owned(),
        parent: parents.next().map(str::to_owned),
        merge: parents.next().is_some(),
        summary: summary.to_owned(),
        author: author.to_owned(),
        time: time.parse().unwrap_or(0),
        ..GitCommit::default()
    })
}

/// Changes from `git diff-tree -z --name-status`: a status letter (with a score for a rename
/// or copy), then the path, or a rename's or copy's old and new paths. Output cut short loses
/// its last, partial record.
fn parse_name_status(output: &[u8]) -> Vec<(GitChangeKind, String, Option<String>)> {
    let mut changes = Vec::new();
    let mut tokens = records(output);
    while let Some(status) = tokens.next() {
        let Some(kind) = status.chars().next().and_then(change_kind) else {
            // An unknown status still names one path, which is skipped with it.
            tokens.next();
            continue;
        };
        let Some(path) = tokens.next() else {
            break;
        };
        if matches!(kind, GitChangeKind::Renamed | GitChangeKind::Copied) {
            let Some(new) = tokens.next() else {
                break;
            };
            changes.push((kind, new.to_owned(), Some(path.to_owned())));
        } else {
            changes.push((kind, path.to_owned(), None));
        }
    }
    changes
}
