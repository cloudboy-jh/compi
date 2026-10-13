//! Git state of content-pane repositories: uncommitted changes and branches. Reads take no
//! index refresh and no optional locks; the explicit writes of `write` run one at a time per
//! repository.

mod write;

pub use write::write;

use crate::content_files::{self, Failure};
use base64::Engine;
use compi_protocol::{
    ContentEnvironment, ErrorCode, GitBranch, GitBranches, GitChange, GitChangeKind, GitChanges,
    GitFileContent, GitImageFormat, GitRevision, GitScope, MAX_CONTENT_FILE_BYTES,
    MAX_GIT_BRANCHES, MAX_GIT_CHANGES, MAX_GIT_IMAGE_BYTES, is_repo_path,
};
use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// How long one git command may run.
const TIMEOUT: Duration = Duration::from_secs(10);
/// Listing output kept; a longer listing is cut at its last complete record.
const MAX_LISTING_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOP_LEVEL_BYTES: usize = 64 * 1024;
const MAX_STDERR_BYTES: usize = 64 * 1024;

/// Environment for every git read. Smudging LFS pointers could download objects.
const GIT_ENV: [(&str, &str); 4] = [
    ("LC_ALL", "C"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_LFS_SKIP_SMUDGE", "1"),
];
/// Environment for every git write: it takes the index lock, checks files out as the
/// user's git would, and never waits for an editor or a prompt.
const GIT_WRITE_ENV: [(&str, &str); 3] = [
    ("LC_ALL", "C"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_EDITOR", "true"),
];

const STATUS: &[&str] = &[
    "status",
    "--porcelain=v2",
    "-z",
    "--branch",
    "--untracked-files=all",
    "--renames",
];
const UNSTAGED_NUMSTAT: &[&str] = &[
    "diff",
    "--numstat",
    "-z",
    "--no-ext-diff",
    "--no-textconv",
    "-M",
];
const STAGED_NUMSTAT: &[&str] = &[
    "diff",
    "--cached",
    "--numstat",
    "-z",
    "--no-ext-diff",
    "--no-textconv",
    "-M",
];
/// Per branch: `*` when checked out here, the full ref name, its upstream and its commit
/// time, NUL-separated, one branch per line (ref names hold neither).
const BRANCHES: &[&str] = &[
    "for-each-ref",
    "--format=%(HEAD)%00%(refname)%00%(upstream:short)%00%(committerdate:unix)",
    "refs/heads",
    "refs/remotes",
];

/// Errors of `cat-file` for a path that does not exist at the revision.
const MISSING: &[&str] = &[
    "does not exist",
    "exists on disk, but not in",
    "is in the index, but not at stage",
    "invalid object name",
    "Not a valid object name",
];

/// The uncommitted changes of the work tree holding `path`, a folder or a file; `None`
/// when `path` is not inside a Git work tree.
pub fn read_changes(
    environment: &ContentEnvironment,
    path: &str,
) -> Result<Option<GitChanges>, Failure> {
    let root = content_files::resolve(environment, path)?;
    let folder = if fs::metadata(&root.host).is_ok_and(|metadata| metadata.is_file()) {
        Path::new(&root.shell).parent().map_or_else(
            || root.shell.clone(),
            |parent| parent.to_string_lossy().into_owned(),
        )
    } else {
        root.shell.clone()
    };
    let site = Site::of(environment, &folder);
    let (git_environment, git_folder) = (&site.environment, site.folder.as_str());
    let (top, status, unstaged, staged) = thread::scope(|scope| {
        let spawn = |args: &'static [&'static str], limit: usize| {
            scope.spawn(move || git(git_environment, git_folder, args, limit))
        };
        let top = spawn(&["rev-parse", "--show-toplevel"], MAX_TOP_LEVEL_BYTES);
        let status = spawn(STATUS, MAX_LISTING_BYTES);
        let unstaged = spawn(UNSTAGED_NUMSTAT, MAX_LISTING_BYTES);
        let staged = spawn(STAGED_NUMSTAT, MAX_LISTING_BYTES);
        (
            joined(top),
            joined(status),
            joined(unstaged),
            joined(staged),
        )
    });
    let top = top?;
    if !top.success {
        if top.stderr.contains("not a git repository")
            || top.stderr.contains("must be run in a work tree")
        {
            return Ok(None);
        }
        return Err(top.failure("could not find the repository"));
    }
    let top = String::from_utf8_lossy(&top.stdout)
        .trim_end_matches(['\n', '\r'])
        .to_owned();
    // `repo` is the top level in the request's environment, `work_tree` where git ran.
    #[cfg(windows)]
    let (repo, work_tree) = match (&site.environment, environment) {
        (ContentEnvironment::Host, ContentEnvironment::Wsl { .. }) => {
            let work_tree = top.replace('/', "\\");
            let repo = wsl_top_level(&folder, &site.folder, &work_tree).ok_or_else(|| {
                (
                    ErrorCode::Internal,
                    format!("could not find {work_tree} in WSL from {folder}"),
                )
            })?;
            (repo, work_tree)
        }
        (ContentEnvironment::Host, _) => (top.replace('/', "\\"), top.replace('/', "\\")),
        _ => (top.clone(), top),
    };
    #[cfg(unix)]
    let (repo, work_tree) = (top.clone(), top);
    let status = listing(status?, "could not read the status")?;
    let unstaged = parse_numstat(&listing(unstaged?, "could not count unstaged lines")?.stdout);
    let staged = parse_numstat(&listing(staged?, "could not count staged lines")?.stdout);

    let parsed = parse_status(&status.stdout);
    let mut entries = parsed.entries;
    entries.sort_by(|a, b| {
        (a.scope == GitScope::Staged, &a.path).cmp(&(b.scope == GitScope::Staged, &b.path))
    });
    let truncated = status.cut || entries.len() > MAX_GIT_CHANGES;
    entries.truncate(MAX_GIT_CHANGES);
    let changes = entries
        .into_iter()
        .map(|entry| {
            let counts = match (entry.scope, entry.kind) {
                (_, GitChangeKind::Untracked | GitChangeKind::Conflicted) => None,
                (GitScope::Unstaged, _) => unstaged.get(&entry.path),
                (GitScope::Staged, _) => staged.get(&entry.path),
            };
            let (added, removed) = counts.copied().unwrap_or_default();
            let stamp = if entry.in_work_tree {
                format!(
                    "{}:{}",
                    entry.stamp,
                    file_stamp(&site.environment, &work_tree, &entry.path)
                )
            } else {
                entry.stamp
            };
            GitChange {
                scope: entry.scope,
                kind: entry.kind,
                path: entry.path,
                old_path: entry.old_path,
                added,
                removed,
                stamp,
            }
        })
        .collect();
    Ok(Some(GitChanges {
        repo,
        branch: parsed.branch,
        head: parsed.head,
        upstream: parsed.upstream,
        ahead: parsed.ahead,
        behind: parsed.behind,
        changes,
        truncated,
    }))
}

/// The local and remote-tracking branches of the repository whose top level is `repo`.
pub fn read_branches(environment: &ContentEnvironment, repo: &str) -> Result<GitBranches, Failure> {
    let repo = content_files::resolve(environment, repo)?.shell;
    let site = Site::of(environment, &repo);
    let listed = listing(
        git(&site.environment, &site.folder, BRANCHES, MAX_LISTING_BYTES)?,
        "could not list the branches",
    )?;
    let mut branches = parse_branches(&listed.stdout);
    branches.sort_by(|(a, a_time), (b, b_time)| {
        let rank = |branch: &GitBranch| match (branch.current, branch.remote) {
            (true, _) => 0,
            (false, false) => 1,
            (false, true) => 2,
        };
        (rank(a), Reverse(a_time), &a.name).cmp(&(rank(b), Reverse(b_time), &b.name))
    });
    let truncated = listed.cut || branches.len() > MAX_GIT_BRANCHES;
    branches.truncate(MAX_GIT_BRANCHES);
    Ok(GitBranches {
        branches: branches.into_iter().map(|(branch, _)| branch).collect(),
        truncated,
    })
}

/// Branches and their commit times from `BRANCHES` output, without remotes' `HEAD`
/// aliases. Output cut short loses its last, partial line.
fn parse_branches(output: &[u8]) -> Vec<(GitBranch, i64)> {
    let complete = output
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(&output[..0], |end| &output[..end]);
    String::from_utf8_lossy(complete)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let (head, refname, upstream, time) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            let (name, remote) = match refname.strip_prefix("refs/heads/") {
                Some(name) => (name, false),
                None => (refname.strip_prefix("refs/remotes/")?, true),
            };
            if remote && (name == "HEAD" || name.ends_with("/HEAD")) {
                return None;
            }
            let branch = GitBranch {
                name: name.to_owned(),
                current: head == "*",
                upstream: (!upstream.is_empty()).then(|| upstream.to_owned()),
                remote,
            };
            Some((branch, time.trim().parse().unwrap_or(0)))
        })
        .collect()
}

fn joined(handle: thread::ScopedJoinHandle<'_, Result<Run, Failure>>) -> Result<Run, Failure> {
    handle
        .join()
        .unwrap_or_else(|_| Err((ErrorCode::Internal, "git reader failed".into())))
}

/// One side of a changed file: `path` relative to the repository top level `repo`.
pub fn read_file(
    environment: &ContentEnvironment,
    repo: &str,
    path: &str,
    revision: GitRevision,
) -> Result<GitFileContent, Failure> {
    let repo = content_files::resolve(environment, repo)?.shell;
    let site = Site::of(environment, &repo);
    let (environment, repo) = (&site.environment, site.folder.as_str());
    check_path(environment, path)?;
    let limit = read_limit(path);
    // Head and index blobs are filtered as a checkout would write them, so their line
    // endings match the work tree's.
    let object = match revision {
        GitRevision::Head => format!("HEAD:{path}"),
        GitRevision::Index => format!(":0:{path}"),
        GitRevision::WorkTree => return read_work_tree(environment, repo, path, limit),
    };
    let blob = git(
        environment,
        repo,
        &["cat-file", "--filters", object.as_str()],
        limit,
    )?;
    if blob.cut {
        let size = git(environment, repo, &["cat-file", "-s", object.as_str()], 64)?;
        let len = String::from_utf8_lossy(&size.stdout).trim().parse().ok();
        return match len {
            Some(len) if size.success => Ok(GitFileContent::TooLarge { len }),
            _ => Err(size.failure("could not size the file")),
        };
    }
    if !blob.success {
        if MISSING.iter().any(|message| blob.stderr.contains(message)) {
            return Ok(GitFileContent::Missing);
        }
        return Err(blob.failure("could not read the file"));
    }
    Ok(classify(path, blob.stdout))
}

/// Where git runs for a folder of a request's environment. A WSL folder on a Windows drive
/// (`/mnt/c/…`) is a Windows checkout: Git for Windows wrote its work tree with its own
/// settings, typically CRLF line endings, so the distribution's git would list nearly every
/// file as modified. Such folders run Git for Windows on their native path instead.
struct Site {
    environment: ContentEnvironment,
    folder: String,
}

impl Site {
    fn of(environment: &ContentEnvironment, folder: &str) -> Self {
        #[cfg(windows)]
        if let ContentEnvironment::Wsl { distribution } = environment
            && let Some(native) = drive_path(distribution, folder)
        {
            return Self {
                environment: ContentEnvironment::Host,
                folder: native,
            };
        }
        Self {
            environment: environment.clone(),
            folder: folder.to_owned(),
        }
    }
}

/// The native path of a WSL folder that lies on a Windows drive. `wslpath` answers are
/// cached, since a visible Changes pane asks every few seconds; failures are not.
#[cfg(windows)]
fn drive_path(distribution: &str, folder: &str) -> Option<String> {
    use std::sync::{LazyLock, Mutex};
    type Cache = HashMap<(String, String), Option<String>>;
    static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Mutex::default);
    let key = (distribution.to_owned(), folder.to_owned());
    if let Ok(cache) = CACHE.lock()
        && let Some(native) = cache.get(&key)
    {
        return native.clone();
    }
    let native = compi_protocol::wsl::windows_path_for_wsl(folder, distribution)
        .ok()?
        .into_os_string()
        .into_string()
        .ok()
        .filter(|native| is_drive_path(native));
    if let Ok(mut cache) = CACHE.lock() {
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(key, native.clone());
    }
    native
}

/// `C:\…`, as opposed to `\\wsl.localhost\…` or another share.
#[cfg(windows)]
fn is_drive_path(path: &str) -> bool {
    matches!(path.as_bytes(), [letter, b':', rest @ ..]
        if letter.is_ascii_alphabetic() && matches!(rest, [] | [b'\\', ..]))
}

/// The WSL path of the repository top level `native_top`, given a WSL folder `folder`
/// inside it and that folder's native path `native_folder`: `folder` without as many
/// trailing components as `native_folder` has below `native_top`.
#[cfg(windows)]
fn wsl_top_level(folder: &str, native_folder: &str, native_top: &str) -> Option<String> {
    let native_folder = native_folder.trim_end_matches('\\');
    let native_top = native_top.trim_end_matches('\\');
    let below = native_folder
        .get(..native_top.len())
        .filter(|prefix| prefix.eq_ignore_ascii_case(native_top))
        .map(|_| &native_folder[native_top.len()..])
        .filter(|below| below.is_empty() || below.starts_with('\\'))?;
    let mut top = folder.trim_end_matches('/');
    for _ in below.split('\\').filter(|part| !part.is_empty()) {
        top = &top[..top.rfind('/')?];
    }
    Some(if top.is_empty() { "/" } else { top }.to_owned())
}

/// The work-tree file, read like a content file. A symbolic link reads as its target,
/// which is what Git stores for it.
fn read_work_tree(
    environment: &ContentEnvironment,
    repo: &str,
    path: &str,
    limit: usize,
) -> Result<GitFileContent, Failure> {
    let root = content_files::resolve(environment, &work_tree_path(environment, repo, path))?;
    if fs::symlink_metadata(&root.host).is_ok_and(|metadata| metadata.is_symlink())
        && let Ok(target) = fs::read_link(&root.host)
    {
        return Ok(classify(
            path,
            target.to_string_lossy().replace('\\', "/").into_bytes(),
        ));
    }
    let (opened_path, opened) = content_files::open(&root, &content_files::open_options(false));
    let mut file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(GitFileContent::Missing);
        }
        Err(error) => return Err(content_files::open_failure(&root, &opened_path, error)),
    };
    let metadata = content_files::regular_metadata(&file, &root)?;
    if metadata.len() > limit as u64 {
        return Ok(GitFileContent::TooLarge {
            len: metadata.len(),
        });
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    (&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| content_files::read_failure(&root, error))?;
    if bytes.len() > limit {
        return Ok(GitFileContent::TooLarge {
            len: file
                .metadata()
                .map_or(bytes.len() as u64, |metadata| metadata.len()),
        });
    }
    Ok(classify(path, bytes))
}

/// The absolute path of a repository file in the request's environment.
fn work_tree_path(environment: &ContentEnvironment, repo: &str, path: &str) -> String {
    #[cfg(windows)]
    if *environment == ContentEnvironment::Host {
        return format!(
            "{}\\{}",
            repo.trim_end_matches('\\'),
            path.replace('/', "\\")
        );
    }
    let _ = environment;
    format!("{}/{path}", repo.trim_end_matches('/'))
}

/// The work-tree file's length and modification time, `-` when it is gone.
fn file_stamp(environment: &ContentEnvironment, repo: &str, path: &str) -> String {
    let Ok(root) = content_files::resolve(environment, &work_tree_path(environment, repo, path))
    else {
        return "?".into();
    };
    match fs::symlink_metadata(&root.host) {
        Ok(metadata) => {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |time| time.as_nanos());
            format!("{}:{modified}", metadata.len())
        }
        Err(_) => "-".into(),
    }
}

/// Bytes read of one side: images may be larger than text.
fn read_limit(path: &str) -> usize {
    if image_format(path).is_some() {
        MAX_GIT_IMAGE_BYTES
    } else {
        MAX_CONTENT_FILE_BYTES
    }
}

/// Classify at most `read_limit(path)` bytes of one side.
fn classify(path: &str, bytes: Vec<u8>) -> GitFileContent {
    let len = bytes.len() as u64;
    if let Some(format) = image_format(path).filter(|format| has_signature(*format, &bytes)) {
        return GitFileContent::Image {
            format,
            base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            len,
        };
    }
    if bytes.len() > MAX_CONTENT_FILE_BYTES {
        return GitFileContent::TooLarge { len };
    }
    if bytes[..bytes.len().min(content_files::BINARY_PROBE_BYTES)].contains(&0) {
        return GitFileContent::Binary { len };
    }
    match String::from_utf8(bytes) {
        Ok(text) => GitFileContent::Text { text },
        Err(_) => GitFileContent::Binary { len },
    }
}

fn image_format(path: &str) -> Option<GitImageFormat> {
    let (_, extension) = path.rsplit('/').next()?.rsplit_once('.')?;
    Some(match extension.to_ascii_lowercase().as_str() {
        "png" => GitImageFormat::Png,
        "jpg" | "jpeg" => GitImageFormat::Jpeg,
        "gif" => GitImageFormat::Gif,
        "webp" => GitImageFormat::Webp,
        "bmp" => GitImageFormat::Bmp,
        "ico" => GitImageFormat::Ico,
        "svg" => GitImageFormat::Svg,
        _ => return None,
    })
}

fn has_signature(format: GitImageFormat, bytes: &[u8]) -> bool {
    match format {
        GitImageFormat::Png => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        GitImageFormat::Jpeg => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        GitImageFormat::Gif => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        GitImageFormat::Webp => {
            bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
        }
        GitImageFormat::Bmp => bytes.starts_with(b"BM"),
        GitImageFormat::Ico => bytes.starts_with(&[0, 0, 1, 0]),
        GitImageFormat::Svg => {
            let head = &bytes[..bytes.len().min(content_files::BINARY_PROBE_BYTES)];
            let start = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(head);
            start.trim_ascii_start().starts_with(b"<")
                && !head.contains(&0)
                && head.windows(4).any(|window| window == b"<svg")
        }
    }
}

/// A change from `git status`, before its work-tree file is examined.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    scope: GitScope,
    kind: GitChangeKind,
    path: String,
    old_path: Option<String>,
    stamp: String,
    /// Whether the stamp also needs the work-tree file's length and modification time.
    in_work_tree: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct Status {
    branch: Option<String>,
    head: Option<String>,
    upstream: Option<String>,
    ahead: u32,
    behind: u32,
    entries: Vec<Entry>,
}

/// Parse `git status --porcelain=v2 -z --branch`. Output cut short loses its last,
/// partial record.
fn parse_status(output: &[u8]) -> Status {
    let mut status = Status {
        branch: None,
        head: None,
        upstream: None,
        ahead: 0,
        behind: 0,
        entries: Vec::new(),
    };
    let mut oid = "";
    let mut unborn = false;
    let mut tokens = records(output);
    while let Some(record) = tokens.next() {
        let Some((code, rest)) = record.split_once(' ') else {
            continue;
        };
        match code {
            "#" => {
                if let Some(head) = rest.strip_prefix("branch.oid ") {
                    unborn = head == "(initial)";
                    if !unborn {
                        oid = head;
                        status.head = Some(head.chars().take(7).collect());
                    }
                } else if let Some(name) = rest.strip_prefix("branch.head ") {
                    status.branch = (name != "(detached)").then(|| name.to_owned());
                } else if let Some(upstream) = rest.strip_prefix("branch.upstream ") {
                    status.upstream = Some(upstream.to_owned());
                } else if let Some(counts) = rest.strip_prefix("branch.ab ") {
                    // `+<ahead> -<behind>`; absent when the upstream branch is gone.
                    if let Some((ahead, behind)) = counts.split_once(' ') {
                        status.ahead = ahead.trim_start_matches('+').parse().unwrap_or(0);
                        status.behind = behind.trim_start_matches('-').parse().unwrap_or(0);
                    }
                }
            }
            "1" => {
                let fields: Vec<&str> = rest.splitn(8, ' ').collect();
                if let [xy, _, mh, mi, mw, hh, hi, path] = fields[..] {
                    push_tracked(&mut status.entries, xy, [mh, mi, mw, hh, hi], path, None);
                }
            }
            "2" => {
                let fields: Vec<&str> = rest.splitn(9, ' ').collect();
                if let ([xy, _, mh, mi, mw, hh, hi, _, path], Some(old)) =
                    (&fields[..], tokens.next())
                {
                    push_tracked(
                        &mut status.entries,
                        xy,
                        [mh, mi, mw, hh, hi],
                        path,
                        Some(old),
                    );
                }
            }
            "u" => {
                let fields: Vec<&str> = rest.splitn(10, ' ').collect();
                if let [_, _, _, _, _, mw, h1, h2, h3, path] = fields[..] {
                    status.entries.push(Entry {
                        scope: GitScope::Unstaged,
                        kind: GitChangeKind::Conflicted,
                        path: path.to_owned(),
                        old_path: None,
                        stamp: format!("{oid}:{h1}:{h2}:{h3}:{mw}"),
                        in_work_tree: true,
                    });
                }
            }
            "?" => status.entries.push(Entry {
                scope: GitScope::Unstaged,
                kind: GitChangeKind::Untracked,
                path: rest.to_owned(),
                old_path: None,
                stamp: "?".into(),
                in_work_tree: true,
            }),
            _ => {}
        }
    }
    if unborn {
        status.branch = None;
    }
    status
}

/// Entries of an ordinary or renamed record: X is the staged change, Y the unstaged one.
fn push_tracked(
    entries: &mut Vec<Entry>,
    xy: &str,
    [mh, mi, mw, hh, hi]: [&str; 5],
    path: &str,
    old: Option<&str>,
) {
    let mut codes = xy.chars();
    let renamed = |kind| matches!(kind, GitChangeKind::Renamed | GitChangeKind::Copied);
    let old_suffix = old.map_or_else(String::new, |old| format!(":{old}"));
    if let Some(kind) = codes.next().and_then(change_kind) {
        entries.push(Entry {
            scope: GitScope::Staged,
            kind,
            path: path.to_owned(),
            old_path: old.filter(|_| renamed(kind)).map(str::to_owned),
            stamp: format!("{mh}:{hh}:{mi}:{hi}{old_suffix}"),
            in_work_tree: false,
        });
    }
    if let Some(kind) = codes.next().and_then(change_kind) {
        entries.push(Entry {
            scope: GitScope::Unstaged,
            kind,
            path: path.to_owned(),
            old_path: old.filter(|_| renamed(kind)).map(str::to_owned),
            stamp: format!("{mi}:{hi}:{mw}{old_suffix}"),
            in_work_tree: true,
        });
    }
}

fn change_kind(code: char) -> Option<GitChangeKind> {
    Some(match code {
        'M' => GitChangeKind::Modified,
        'T' => GitChangeKind::TypeChanged,
        'A' => GitChangeKind::Added,
        'D' => GitChangeKind::Deleted,
        'R' => GitChangeKind::Renamed,
        'C' => GitChangeKind::Copied,
        _ => return None,
    })
}

/// Lines added and removed per path from `git diff --numstat -z`; binary files count `-`.
fn parse_numstat(output: &[u8]) -> HashMap<String, (Option<u32>, Option<u32>)> {
    let mut counts = HashMap::new();
    let mut tokens = records(output);
    while let Some(record) = tokens.next() {
        let mut fields = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        // A rename leaves the path empty and follows with the old and new paths.
        let path = if path.is_empty() {
            match (tokens.next(), tokens.next()) {
                (Some(_), Some(new)) => new,
                _ => break,
            }
        } else {
            path
        };
        counts.insert(path.to_owned(), (added.parse().ok(), removed.parse().ok()));
    }
    counts
}

/// NUL-terminated records, without the unterminated remainder of output cut short.
fn records(output: &[u8]) -> impl Iterator<Item = &str> {
    let complete = output
        .iter()
        .rposition(|byte| *byte == 0)
        .map_or(&output[..0], |end| &output[..end]);
    complete
        .split(|byte| *byte == 0)
        .filter(|_| !complete.is_empty())
        .map(|record| std::str::from_utf8(record).unwrap_or(""))
}

/// What a git command printed.
struct Run {
    success: bool,
    stdout: Vec<u8>,
    /// Standard output exceeded the limit; the command was stopped.
    cut: bool,
    stderr: String,
}

impl Run {
    fn failure(&self, context: &str) -> Failure {
        let detail = match self.stderr.trim() {
            "" => "git failed",
            detail => detail,
        };
        (
            ErrorCode::Internal,
            format!(
                "{context}: {}",
                detail.chars().take(600).collect::<String>()
            ),
        )
    }
}

/// A listing that succeeded, or was cut short.
fn listing(run: Run, context: &str) -> Result<Run, Failure> {
    if run.success || run.cut {
        Ok(run)
    } else {
        Err(run.failure(context))
    }
}

/// How a git command runs.
#[derive(Clone, Copy)]
enum Access<'a> {
    /// No optional locks, stdin closed, `TIMEOUT`; output past the limit stops it.
    Read,
    /// Locks allowed, `input` on stdin; output past the limit is drained and dropped.
    Write {
        input: Option<&'a [u8]>,
        timeout: Duration,
    },
}

/// Run git in `folder` with at most `limit` bytes of standard output; more stops it.
fn git(
    environment: &ContentEnvironment,
    folder: &str,
    args: &[&str],
    limit: usize,
) -> Result<Run, Failure> {
    run(environment, folder, args, limit, Access::Read)
}

fn run(
    environment: &ContentEnvironment,
    folder: &str,
    args: &[&str],
    limit: usize,
    access: Access<'_>,
) -> Result<Run, Failure> {
    let (input, timeout) = match access {
        Access::Read => (None, TIMEOUT),
        Access::Write { input, timeout } => (input, timeout),
    };
    let reading = matches!(access, Access::Read);
    let mut command = git_command(environment, reading)?;
    if reading {
        command.arg("--no-optional-locks");
    }
    command
        .args(["-c", "core.quotepath=off", "-C", folder])
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| (ErrorCode::Internal, format!("could not run git: {error}")))?;
    let stdin = child.stdin.take();
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let mut reader = Some(thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = (&mut stdout).take(limit as u64 + 1).read_to_end(&mut bytes);
        if !reading && bytes.len() > limit {
            bytes.truncate(limit);
            let _ = io::copy(&mut stdout, &mut io::sink());
        }
        bytes
    }));
    let errors = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = (&mut stderr)
            .take(MAX_STDERR_BYTES as u64)
            .read_to_end(&mut bytes);
        let _ = io::copy(&mut stderr, &mut io::sink());
        bytes
    });
    let deadline = Instant::now() + timeout;
    let mut output = None;
    let status = thread::scope(|scope| {
        // Writing on its own thread keeps the deadline even if git stops reading; killing
        // git breaks the pipe and ends the writer.
        if let (Some(mut stdin), Some(input)) = (stdin, input) {
            scope.spawn(move || {
                use std::io::Write;
                let _ = stdin.write_all(input);
            });
        }
        loop {
            if let Some(finished) = reader.take_if(|reader| reader.is_finished()) {
                let bytes = finished.join().unwrap_or_default();
                if bytes.len() > limit {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(Err(bytes));
                }
                output = Some(bytes);
            }
            match child.try_wait() {
                Ok(Some(status)) => return Ok(Ok(status)),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                result => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err((
                        ErrorCode::Internal,
                        result.err().map_or_else(
                            || format!("git did not finish within {} seconds", timeout.as_secs()),
                            |error| error.to_string(),
                        ),
                    ));
                }
            }
        }
    })?;
    let status = match status {
        Ok(status) => status,
        Err(bytes) => {
            return Ok(Run {
                success: false,
                stdout: bytes,
                cut: true,
                stderr: String::new(),
            });
        }
    };
    let stdout = match (output, reader) {
        (Some(bytes), _) => bytes,
        (None, Some(reader)) => collected(reader),
        (None, None) => Vec::new(),
    };
    let stderr = crate::prompt::target::decode(&collected(errors));
    let cut = stdout.len() > limit;
    Ok(Run {
        success: status.success() && !cut,
        stdout,
        cut,
        stderr,
    })
}

/// What a pipe reader read once git exited. A process git started (a hook's background
/// job) can hold the pipe open; after a short grace its output is given up rather than
/// waited for.
fn collected(reader: thread::JoinHandle<Vec<u8>>) -> Vec<u8> {
    let grace = Instant::now() + Duration::from_secs(2);
    while !reader.is_finished() && Instant::now() < grace {
        thread::sleep(Duration::from_millis(5));
    }
    if reader.is_finished() {
        reader.join().unwrap_or_default()
    } else {
        Vec::new()
    }
}

/// Rejects `path` unless it names a file relative to a repository top level. Windows
/// paths would also split at `\` and name alternate streams with `:`.
fn check_path(environment: &ContentEnvironment, path: &str) -> Result<(), Failure> {
    let windows_host = cfg!(windows) && *environment == ContentEnvironment::Host;
    if !is_repo_path(path) || (windows_host && path.contains(['\\', ':'])) {
        return Err((
            ErrorCode::InvalidRequest,
            format!("{path:?} is not a path inside the repository"),
        ));
    }
    Ok(())
}

/// Git on the daemon host, or inside a WSL distribution of a Windows daemon, with the read
/// or the write environment.
fn git_command(environment: &ContentEnvironment, reading: bool) -> Result<Command, Failure> {
    let env: &[(&str, &str)] = if reading { &GIT_ENV } else { &GIT_WRITE_ENV };
    match environment {
        ContentEnvironment::Host => {
            let mut command = Command::new("git");
            command.envs(env.iter().copied());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
            }
            Ok(command)
        }
        #[cfg(windows)]
        ContentEnvironment::Wsl { distribution } => {
            use std::os::windows::process::CommandExt;
            let mut command = Command::new(r"C:\Windows\System32\wsl.exe");
            command.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
            command
                .args(["--distribution", distribution, "--exec", "/usr/bin/env"])
                .args(env.iter().map(|(key, value)| format!("{key}={value}")))
                .arg("git");
            Ok(command)
        }
        #[cfg(unix)]
        ContentEnvironment::Wsl { .. } => Err((
            ErrorCode::InvalidRequest,
            "WSL repositories are only available on a Windows host".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[cfg(windows)]
    #[test]
    fn drive_checkouts_map_their_top_level_back_to_wsl() {
        let top = |folder, native| wsl_top_level(folder, native, r"C:\Users\me\proj");
        assert_eq!(
            top("/mnt/c/Users/me/proj/src/gui", r"C:\Users\me\proj\src\gui").as_deref(),
            Some("/mnt/c/Users/me/proj")
        );
        // Case differs between git's and wslpath's answers; links rename the WSL side.
        assert_eq!(
            top("/home/me/link/src", r"c:\users\ME\proj\src").as_deref(),
            Some("/home/me/link")
        );
        assert_eq!(
            top("/mnt/c/Users/me/proj/", r"C:\Users\me\proj\").as_deref(),
            Some("/mnt/c/Users/me/proj")
        );
        // A sibling that merely shares the prefix is not inside the top level.
        assert_eq!(top("/mnt/c/Users/me/project", r"C:\Users\me\project"), None);
        assert!(is_drive_path(r"C:\Users") && is_drive_path("d:"));
        assert!(!is_drive_path(r"\\wsl.localhost\Ubuntu\home") && !is_drive_path("C:x"));
    }

    #[test]
    fn status_records_become_changes_per_scope() {
        let output = b"# branch.oid 0123456789abcdef\0# branch.head main\0\
            1 MM N... 100644 100644 100644 aaaa bbbb both.rs\0\
            2 R. N... 100644 100644 100644 cccc cccc R100 new name.rs\0old name.rs\0\
            u UU N... 100644 100644 100644 100644 h1 h2 h3 clash.rs\0\
            ? notes/todo.md\0! target/out\0? cut";
        let status = parse_status(output);
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.head.as_deref(), Some("0123456"));
        let summary: Vec<_> = status
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.scope,
                    entry.kind,
                    entry.path.as_str(),
                    entry.old_path.as_deref(),
                    entry.in_work_tree,
                )
            })
            .collect();
        use GitChangeKind::*;
        use GitScope::*;
        assert_eq!(
            summary,
            [
                (Staged, Modified, "both.rs", None, false),
                (Unstaged, Modified, "both.rs", None, true),
                (Staged, Renamed, "new name.rs", Some("old name.rs"), false),
                (Unstaged, Conflicted, "clash.rs", None, true),
                (Unstaged, Untracked, "notes/todo.md", None, true),
            ]
        );
        assert_eq!(status.entries[0].stamp, "100644:aaaa:100644:bbbb");
        assert_eq!(status.entries[3].stamp, "0123456789abcdef:h1:h2:h3:100644");

        let unborn = parse_status(b"# branch.oid (initial)\0# branch.head main\0");
        assert_eq!((unborn.branch, unborn.head), (None, None));
        assert_eq!((unborn.upstream, unborn.ahead, unborn.behind), (None, 0, 0));

        let tracking = parse_status(
            b"# branch.oid 0123\0# branch.head main\0# branch.upstream origin/main\0\
            # branch.ab +3 -1\0",
        );
        assert_eq!(
            (
                tracking.upstream.as_deref(),
                tracking.ahead,
                tracking.behind
            ),
            (Some("origin/main"), 3, 1)
        );
        // An upstream whose branch is gone has no counts.
        let gone =
            parse_status(b"# branch.oid 0123\0# branch.head main\0# branch.upstream origin/old\0");
        assert_eq!(
            (gone.upstream.as_deref(), gone.ahead, gone.behind),
            (Some("origin/old"), 0, 0)
        );

        let counts = parse_numstat(b"1\t2\ta.rs\0-\t-\timage.png\x003\t0\t\0old.rs\0new.rs\0");
        assert_eq!(counts["a.rs"], (Some(1), Some(2)));
        assert_eq!(counts["image.png"], (None, None));
        assert_eq!(counts["new.rs"], (Some(3), Some(0)));
    }

    #[test]
    fn branch_records_skip_remote_heads_and_a_cut_line() {
        let output = b"*\0refs/heads/main\0origin/main\x00200\n\
             \0refs/heads/topic\0\x00100\n\
             \0refs/remotes/origin/HEAD\0\x00200\n\
             \0refs/remotes/origin/main\0\x00200\n\
             \0refs/heads/cu";
        let branches: Vec<_> = parse_branches(output)
            .into_iter()
            .map(|(branch, time)| {
                (
                    branch.name,
                    branch.current,
                    branch.upstream,
                    branch.remote,
                    time,
                )
            })
            .collect();
        assert_eq!(
            branches,
            [
                ("main".into(), true, Some("origin/main".into()), false, 200),
                ("topic".into(), false, None, false, 100),
                ("origin/main".into(), false, None, true, 200),
            ]
        );
    }

    /// A fresh repository in a temporary folder, or `None` without git.
    struct Repo(PathBuf);

    impl Repo {
        fn new() -> Option<Self> {
            Self::init(&["init", "-q"])
        }

        /// A repository made by `args` (`init`, `clone … .`) in a fresh temporary folder.
        fn init(args: &[&str]) -> Option<Self> {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "compi-git-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).ok()?;
            let repo = Self(path);
            repo.git(args)?;
            repo.git(&["config", "core.autocrlf", "false"])?;
            Some(repo)
        }

        /// An empty commit made at `time` (seconds since the epoch).
        fn commit_at(&self, message: &str, time: u64) {
            let date = format!("{time} +0000");
            let output = self
                .command(&[
                    "commit",
                    "-q",
                    "--allow-empty",
                    "--no-gpg-sign",
                    "-m",
                    message,
                ])
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        }

        fn command(&self, args: &[&str]) -> Command {
            let mut command = Command::new("git");
            command
                .arg("-C")
                .arg(&self.0)
                .args(args)
                .env("GIT_AUTHOR_NAME", "Compi")
                .env("GIT_AUTHOR_EMAIL", "compi@example.com")
                .env("GIT_COMMITTER_NAME", "Compi")
                .env("GIT_COMMITTER_EMAIL", "compi@example.com");
            command
        }

        fn git(&self, args: &[&str]) -> Option<()> {
            self.command(args)
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|_| ())
        }

        fn write(&self, name: &str, text: &str) {
            fs::write(self.0.join(name), text).unwrap();
        }

        fn root(&self) -> String {
            self.0.to_str().unwrap().to_owned()
        }

        fn changes(&self) -> GitChanges {
            read_changes(&ContentEnvironment::Host, &self.root())
                .unwrap()
                .unwrap()
        }

        fn read(&self, path: &str, revision: GitRevision) -> GitFileContent {
            read_file(&ContentEnvironment::Host, &self.root(), path, revision).unwrap()
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn text(value: &str) -> GitFileContent {
        GitFileContent::Text { text: value.into() }
    }

    #[test]
    fn unborn_repository_lists_staged_and_untracked_files() {
        let Some(repo) = Repo::new() else {
            return;
        };
        repo.write("added.txt", "one\ntwo\n");
        repo.write("loose.txt", "x\n");
        repo.git(&["add", "added.txt"]).unwrap();
        let changes = repo.changes();
        assert_eq!((changes.branch, changes.head), (None, None));
        let summary: Vec<_> = changes
            .changes
            .iter()
            .map(|change| {
                (
                    change.scope,
                    change.kind,
                    change.path.as_str(),
                    change.added,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (
                    GitScope::Unstaged,
                    GitChangeKind::Untracked,
                    "loose.txt",
                    None
                ),
                (GitScope::Staged, GitChangeKind::Added, "added.txt", Some(2)),
            ]
        );
        assert_eq!(
            repo.read("added.txt", GitRevision::Head),
            GitFileContent::Missing
        );
        assert_eq!(
            repo.read("added.txt", GitRevision::Index),
            text("one\ntwo\n")
        );
        // A file inside the repository finds it too; a folder outside any does not.
        let inside = repo.0.join("added.txt");
        assert!(
            read_changes(&ContentEnvironment::Host, inside.to_str().unwrap())
                .unwrap()
                .is_some()
        );
        assert_eq!(
            read_changes(
                &ContentEnvironment::Host,
                repo.0.join(".git").to_str().unwrap()
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn sides_and_stamps_follow_each_edit() {
        let Some(repo) = Repo::new() else {
            return;
        };
        repo.write("file.txt", "head\n");
        repo.git(&["add", "file.txt"]).unwrap();
        repo.git(&["commit", "-qm", "first"]).unwrap();
        repo.write("file.txt", "index\n");
        repo.git(&["add", "file.txt"]).unwrap();
        repo.write("file.txt", "work tree\n");

        let first = repo.changes();
        assert!(first.head.is_some());
        let scopes: Vec<_> = first.changes.iter().map(|change| change.scope).collect();
        assert_eq!(scopes, [GitScope::Unstaged, GitScope::Staged]);
        assert_eq!(repo.read("file.txt", GitRevision::Head), text("head\n"));
        assert_eq!(repo.read("file.txt", GitRevision::Index), text("index\n"));
        assert_eq!(
            repo.read("file.txt", GitRevision::WorkTree),
            text("work tree\n")
        );
        assert_eq!(
            repo.read("gone.txt", GitRevision::WorkTree),
            GitFileContent::Missing
        );
        assert_eq!(
            repo.read("gone.txt", GitRevision::Index),
            GitFileContent::Missing
        );
        for path in ["../outside.txt", "/etc/passwd", ""] {
            assert!(
                read_file(
                    &ContentEnvironment::Host,
                    &repo.root(),
                    path,
                    GitRevision::Head
                )
                .is_err(),
                "accepted {path:?}"
            );
        }

        // A second edit of an already modified file changes only the unstaged stamp.
        repo.write("file.txt", "work tree, edited again\n");
        let second = repo.changes();
        assert_ne!(first.changes[0].stamp, second.changes[0].stamp);
        assert_eq!(first.changes[1].stamp, second.changes[1].stamp);
    }

    #[test]
    fn branches_list_the_current_first_and_count_ahead_and_behind() {
        let Some(work) = Repo::new() else {
            return;
        };
        let origin = Repo::init(&["init", "-q", "--bare"]).unwrap();
        origin
            .git(&["symbolic-ref", "HEAD", "refs/heads/main"])
            .unwrap();
        work.git(&["symbolic-ref", "HEAD", "refs/heads/main"])
            .unwrap();
        work.commit_at("one", 1_000_000_000);
        work.git(&["remote", "add", "origin", &origin.root()])
            .unwrap();
        work.git(&["push", "-q", "origin", "main"]).unwrap();
        work.git(&["switch", "-q", "-c", "feature"]).unwrap();
        work.commit_at("feature", 1_000_000_100);
        work.git(&["push", "-q", "origin", "feature"]).unwrap();
        work.git(&["switch", "-q", "main"]).unwrap();

        let clone = Repo::init(&["clone", "-q", &origin.root(), "."]).unwrap();
        clone
            .git(&["branch", "-q", "--no-track", "recent", "origin/feature"])
            .unwrap();
        clone.git(&["branch", "-q", "old"]).unwrap();
        // The clone's `origin/HEAD` alias exists, and is not listed.
        clone
            .git(&["show-ref", "--verify", "-q", "refs/remotes/origin/HEAD"])
            .unwrap();
        let listing = read_branches(&ContentEnvironment::Host, &clone.root()).unwrap();
        let summary: Vec<_> = listing
            .branches
            .iter()
            .map(|branch| {
                (
                    branch.name.as_str(),
                    branch.current,
                    branch.upstream.as_deref(),
                    branch.remote,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("main", true, Some("origin/main"), false),
                ("recent", false, None, false),
                ("old", false, None, false),
                ("origin/feature", false, None, true),
                ("origin/main", false, None, true),
            ]
        );
        assert!(!listing.truncated);

        let counts = |repo: &Repo| {
            let changes = repo.changes();
            (changes.upstream, changes.ahead, changes.behind)
        };
        let upstream = Some("origin/main".to_owned());
        assert_eq!(counts(&clone), (upstream.clone(), 0, 0));
        clone.commit_at("mine", 1_000_000_200);
        work.commit_at("theirs", 1_000_000_300);
        work.git(&["push", "-q", "origin", "main"]).unwrap();
        // The upstream's new commit counts once fetched.
        assert_eq!(counts(&clone), (upstream.clone(), 1, 0));
        clone.git(&["fetch", "-q"]).unwrap();
        assert_eq!(counts(&clone), (upstream, 1, 1));

        // A branch without an upstream, and a detached `HEAD`, have no counts.
        clone.git(&["switch", "-q", "old"]).unwrap();
        assert_eq!(counts(&clone), (None, 0, 0));
        clone.git(&["switch", "-q", "--detach", "main"]).unwrap();
        let detached = clone.changes();
        assert_eq!(
            (detached.branch, detached.upstream, detached.ahead),
            (None, None, 0)
        );
        let listing = read_branches(&ContentEnvironment::Host, &clone.root()).unwrap();
        assert!(listing.branches.iter().all(|branch| !branch.current));
    }
}
