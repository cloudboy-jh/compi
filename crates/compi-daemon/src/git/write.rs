//! Explicit Git writes: stage, unstage, hunk staging through the index text, discard and
//! commit. Writes to one repository run one at a time; git's own output is the error.

use super::{Access, Run, Site, check_path, git, records, run};
use crate::content_files::{self, Failure};
use compi_protocol::{
    ContentEnvironment, ErrorCode, FileVersion, GitAction, GitWriteResult, MAX_CONTENT_FILE_BYTES,
};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

/// How long one write may run.
const TIMEOUT: Duration = Duration::from_secs(30);
/// A commit runs hooks.
const COMMIT_TIMEOUT: Duration = Duration::from_secs(120);
/// Standard output kept of a write; the rest is drained.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// Characters of git's output kept in an error; hook output can be long, and its end
/// holds the verdict.
const MAX_ERROR_CHARS: usize = 4000;
/// Pathspec bytes per command line; longer path lists run as several commands.
const MAX_PATHSPEC_BYTES: usize = 16 * 1024;

/// Run `action` in the repository whose top level is `repo`.
pub fn write(
    environment: &ContentEnvironment,
    repo: &str,
    action: GitAction,
) -> Result<GitWriteResult, Failure> {
    let repo = content_files::resolve(environment, repo)?.shell;
    let site = Site::of(environment, &repo);
    let paths: Vec<&str> = match &action {
        GitAction::Stage { paths } | GitAction::Unstage { paths } => {
            paths.iter().map(String::as_str).collect()
        }
        GitAction::SetIndexLines { path, .. } => vec![path.as_str()],
        GitAction::Discard { paths, untracked } => {
            paths.iter().chain(untracked).map(String::as_str).collect()
        }
        GitAction::Commit { .. } | GitAction::LastMessage => Vec::new(),
    };
    for path in &paths {
        check_path(&site.environment, path)?;
    }
    let needs_paths = !matches!(action, GitAction::Commit { .. } | GitAction::LastMessage);
    if needs_paths && paths.is_empty() {
        return Err((ErrorCode::InvalidRequest, "no paths to write".into()));
    }
    let lock = repo_lock(&site);
    let _held = lock.lock();
    let repo = Repo {
        environment: &site.environment,
        folder: &site.folder,
    };
    match action {
        GitAction::Stage { paths } => {
            repo.each(&["add", "-A", "--"], &paths)?;
        }
        GitAction::Unstage { paths } => {
            if repo.unborn()? {
                // Without a commit there is no `HEAD` to restore from: drop the entries.
                // `-f` only skips the check that the index differs from the file, which an
                // unstage discards from the index by definition; the work tree is kept.
                repo.each(&["rm", "--cached", "-f", "-q", "--"], &paths)?;
            } else {
                repo.each(&["restore", "--staged", "--"], &paths)?;
            }
        }
        GitAction::SetIndexLines {
            path,
            expected,
            lines,
            replacement,
        } => return repo.set_index_lines(&path, &expected, lines, &replacement),
        GitAction::Discard { paths, untracked } => {
            repo.each(&["restore", "--worktree", "--"], &paths)?;
            repo.each(&["clean", "-f", "-q", "--"], &untracked)?;
        }
        GitAction::Commit { message, amend } => {
            let mut args = vec!["commit", "-q", "-F", "-"];
            if amend {
                args.push("--amend");
            }
            repo.checked(&args, Some(message.as_bytes()), COMMIT_TIMEOUT)?;
            let head = repo.read(&["log", "-1", "--format=%h%x00%s"], MAX_OUTPUT_BYTES)?;
            let head = String::from_utf8_lossy(&head.stdout);
            let (head, summary) = head
                .trim_end_matches(['\n', '\r'])
                .split_once('\0')
                .unwrap_or((head.trim(), ""));
            return Ok(GitWriteResult::Committed {
                head: head.to_owned(),
                summary: summary.to_owned(),
            });
        }
        GitAction::LastMessage => {
            let message = repo.read(&["log", "-1", "--format=%B"], MAX_CONTENT_FILE_BYTES)?;
            return Ok(GitWriteResult::Message {
                text: String::from_utf8_lossy(&message.stdout)
                    .trim_end()
                    .to_owned(),
            });
        }
    }
    Ok(GitWriteResult::Done)
}

/// The lock serializing writes to the repository at `site`.
fn repo_lock(site: &Site) -> Arc<Mutex<()>> {
    type Locks = HashMap<(ContentEnvironment, String), Arc<Mutex<()>>>;
    static LOCKS: LazyLock<Mutex<Locks>> = LazyLock::new(Mutex::default);
    let mut folder = site.folder.trim_end_matches(['/', '\\']).to_owned();
    if cfg!(windows) && site.environment == ContentEnvironment::Host {
        folder.make_ascii_lowercase();
    }
    let mut locks = LOCKS.lock();
    locks.retain(|_, lock| Arc::strong_count(lock) > 1);
    locks
        .entry((site.environment.clone(), folder))
        .or_default()
        .clone()
}

/// Where the writes run.
struct Repo<'a> {
    environment: &'a ContentEnvironment,
    folder: &'a str,
}

impl Repo<'_> {
    /// A read that must succeed in full.
    fn read(&self, args: &[&str], limit: usize) -> Result<Run, Failure> {
        let read = git(self.environment, self.folder, args, limit)?;
        if read.success {
            Ok(read)
        } else if read.cut {
            Err((
                ErrorCode::Internal,
                format!("git {} printed more than {limit} bytes", args[0]),
            ))
        } else {
            Err(failed(&read))
        }
    }

    /// A write that must succeed.
    fn checked(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<Run, Failure> {
        let written = run(
            self.environment,
            self.folder,
            args,
            MAX_OUTPUT_BYTES,
            Access::Write { input, timeout },
        )?;
        if written.success {
            Ok(written)
        } else {
            Err(failed(&written))
        }
    }

    /// `command` followed by `paths` as literal pathspecs from the top level, in as many
    /// commands as the command line needs.
    fn each(&self, command: &[&str], paths: &[String]) -> Result<(), Failure> {
        let specs: Vec<String> = paths
            .iter()
            .map(|path| format!(":(top,literal){path}"))
            .collect();
        let mut rest = specs.as_slice();
        while !rest.is_empty() {
            let mut bytes = 0;
            let count = rest
                .iter()
                .take_while(|spec| {
                    bytes += spec.len() + 1;
                    bytes <= MAX_PATHSPEC_BYTES
                })
                .count()
                .max(1);
            let mut args = command.to_vec();
            args.extend(rest[..count].iter().map(String::as_str));
            self.checked(&args, None, TIMEOUT)?;
            rest = &rest[count..];
        }
        Ok(())
    }

    /// Whether the repository has no commit yet.
    fn unborn(&self) -> Result<bool, Failure> {
        let head = git(
            self.environment,
            self.folder,
            &["rev-parse", "-q", "--verify", "HEAD"],
            MAX_OUTPUT_BYTES,
        )?;
        match (head.success, head.stderr.trim().is_empty()) {
            (true, _) => Ok(false),
            // `-q` is silent about a missing `HEAD` and only that.
            (false, true) => Ok(true),
            (false, false) => Err(failed(&head)),
        }
    }

    /// Replace `lines` of `path`'s filtered index text with `replacement` and store the
    /// result as git would store that text from the work tree.
    fn set_index_lines(
        &self,
        path: &str,
        expected: &FileVersion,
        lines: Range<u32>,
        replacement: &str,
    ) -> Result<GitWriteResult, Failure> {
        let spec = format!(":(top,literal){path}");
        let listed = self.read(&["ls-files", "-s", "-z", "--", &spec], MAX_OUTPUT_BYTES)?;
        let mut mode = None;
        for record in records(&listed.stdout) {
            // `<mode> <object> <stage>\t<path>`
            let Some((info, name)) = record.split_once('\t') else {
                continue;
            };
            if name != path {
                continue;
            }
            let mut fields = info.split(' ');
            let (Some(entry_mode), Some(_), Some(stage)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if stage != "0" {
                return Err((
                    ErrorCode::InvalidRequest,
                    format!("{path} has a merge conflict; stage the whole file to resolve it"),
                ));
            }
            mode = Some(entry_mode.to_owned());
        }
        let Some(mode) = mode else {
            return Err((
                ErrorCode::InvalidRequest,
                format!("{path} is not in the index; stage the whole file"),
            ));
        };
        // The text the client diffed: filtered as a checkout writes it.
        let object = format!(":0:{path}");
        let blob = git(
            self.environment,
            self.folder,
            &["cat-file", "--filters", &object],
            MAX_CONTENT_FILE_BYTES,
        )?;
        if blob.cut {
            return Err((
                ErrorCode::InvalidRequest,
                format!("{path} is too large to stage in parts"),
            ));
        }
        if !blob.success {
            return Err(failed(&blob));
        }
        if content_files::version_of(&blob.stdout) != *expected {
            return Ok(GitWriteResult::Stale);
        }
        let Some(text) = splice(&blob.stdout, lines.clone(), replacement.as_bytes()) else {
            return Err((
                ErrorCode::InvalidRequest,
                format!(
                    "lines {}..{} are outside the index text of {path}",
                    lines.start, lines.end
                ),
            ));
        };
        // `--path` applies the path's clean and end-of-line filters, as `git add` would.
        let at = format!("--path={path}");
        let hashed = self.checked(&["hash-object", "-w", "--stdin", &at], Some(&text), TIMEOUT)?;
        let object = String::from_utf8_lossy(&hashed.stdout).trim().to_owned();
        if object.is_empty() || !object.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err((
                ErrorCode::Internal,
                format!("git hash-object printed {object:?}"),
            ));
        }
        let entry = format!("{mode},{object},{path}");
        self.checked(&["update-index", "--cacheinfo", &entry], None, TIMEOUT)?;
        Ok(GitWriteResult::Done)
    }
}

/// `text` with `lines` (each ending after its `\n`; a last line may lack one) replaced by
/// `replacement`; `None` when the lines are not within the text.
fn splice(text: &[u8], lines: Range<u32>, replacement: &[u8]) -> Option<Vec<u8>> {
    if lines.start > lines.end {
        return None;
    }
    let start = line_start(text, lines.start)?;
    let end = line_start(text, lines.end)?;
    let mut spliced = Vec::with_capacity(text.len() - (end - start) + replacement.len());
    spliced.extend_from_slice(&text[..start]);
    spliced.extend_from_slice(replacement);
    spliced.extend_from_slice(&text[end..]);
    Some(spliced)
}

/// The byte offset where line `line` starts, or the text's length for the line after the
/// last; `None` past that.
fn line_start(text: &[u8], line: u32) -> Option<usize> {
    if line == 0 {
        return Some(0);
    }
    let mut seen = 0;
    for (at, byte) in text.iter().enumerate() {
        if *byte == b'\n' {
            seen += 1;
            if seen == line {
                return Some(at + 1);
            }
        }
    }
    let unterminated = !text.is_empty() && !text.ends_with(b"\n");
    (unterminated && seen + 1 == line).then_some(text.len())
}

/// Git's output as the error: what it printed, the end kept when long.
fn failed(run: &Run) -> Failure {
    let stdout = String::from_utf8_lossy(&run.stdout);
    let parts: Vec<&str> = [stdout.trim(), run.stderr.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect();
    let text = match parts.join("\n") {
        text if text.is_empty() => "git failed".to_owned(),
        text => text,
    };
    let count = text.chars().count();
    let message = if count > MAX_ERROR_CHARS {
        let tail: String = text.chars().skip(count - MAX_ERROR_CHARS).collect();
        format!("…{tail}")
    } else {
        text
    };
    (ErrorCode::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::read_file;
    use compi_protocol::{GitFileContent, GitRevision};
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A fresh repository with a pinned identity in a temporary folder, or `None` without git.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Option<Self> {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "compi-git-write-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).ok()?;
            let repo = Self(path);
            repo.try_git(&["init", "-q"])?;
            for (key, value) in [
                ("core.autocrlf", "false"),
                ("user.name", "Compi"),
                ("user.email", "compi@example.com"),
                ("commit.gpgsign", "false"),
            ] {
                repo.git(&["config", key, value]);
            }
            Some(repo)
        }

        fn try_git(&self, args: &[&str]) -> Option<String> {
            Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(args)
                .env("GIT_AUTHOR_NAME", "Compi")
                .env("GIT_AUTHOR_EMAIL", "compi@example.com")
                .env("GIT_COMMITTER_NAME", "Compi")
                .env("GIT_COMMITTER_EMAIL", "compi@example.com")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8(output.stdout).unwrap())
        }

        fn git(&self, args: &[&str]) -> String {
            self.try_git(args)
                .unwrap_or_else(|| panic!("git {args:?} failed"))
        }

        fn write(&self, name: &str, text: &str) {
            fs::write(self.0.join(name), text).unwrap();
        }

        fn read(&self, name: &str) -> Option<String> {
            fs::read_to_string(self.0.join(name)).ok()
        }

        fn act(&self, action: GitAction) -> Result<GitWriteResult, Failure> {
            write(&ContentEnvironment::Host, self.0.to_str().unwrap(), action)
        }

        fn done(&self, action: GitAction) {
            assert_eq!(self.act(action), Ok(GitWriteResult::Done));
        }

        fn status(&self) -> String {
            self.git(&["status", "--porcelain", "--untracked-files=all"])
        }

        /// The index text as the client reads and diffs it.
        fn index_text(&self, path: &str) -> String {
            match read_file(
                &ContentEnvironment::Host,
                self.0.to_str().unwrap(),
                path,
                GitRevision::Index,
            )
            .unwrap()
            {
                GitFileContent::Text { text } => text,
                other => panic!("index side of {path} is {other:?}"),
            }
        }

        /// Lines added and removed by a diff, without headers.
        fn changed_lines(&self, args: &[&str]) -> Vec<String> {
            self.git(args)
                .lines()
                .filter(|line| {
                    (line.starts_with('+') || line.starts_with('-'))
                        && !line.starts_with("+++")
                        && !line.starts_with("---")
                })
                .map(str::to_owned)
                .collect()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn paths(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    fn version(text: &str) -> FileVersion {
        content_files::version_of(text.as_bytes())
    }

    #[test]
    fn splices_lines_keeping_their_endings() {
        let text = b"a\r\nb\nc";
        let splice = |lines, replacement: &str| {
            splice(text, lines, replacement.as_bytes()).map(|out| String::from_utf8(out).unwrap())
        };
        assert_eq!(splice(1..2, "B\r\n").as_deref(), Some("a\r\nB\r\nc"));
        assert_eq!(splice(0..0, "z\n").as_deref(), Some("z\na\r\nb\nc"));
        assert_eq!(splice(2..3, "C\n").as_deref(), Some("a\r\nb\nC\n"));
        assert_eq!(splice(3..3, "d\n").as_deref(), Some("a\r\nb\ncd\n"));
        assert_eq!(splice(0..3, "").as_deref(), Some(""));
        assert_eq!(splice(3..4, ""), None);
        assert_eq!(splice(Range { start: 2, end: 1 }, ""), None);
        assert_eq!(
            super::splice(b"x\n", 1..1, b"y\n"),
            Some(b"x\ny\n".to_vec())
        );
        assert_eq!(super::splice(b"x\n", 2..2, b""), None);
        assert_eq!(super::splice(b"", 0..0, b"y\n"), Some(b"y\n".to_vec()));
        assert_eq!(super::splice(b"", 0..1, b""), None);
    }

    #[test]
    fn stages_and_unstages_paths_before_and_after_the_first_commit() {
        let Some(repo) = Scratch::new() else {
            return;
        };
        repo.write("a.txt", "a\n");
        repo.write("b.txt", "b\n");
        repo.write("c[1].txt", "bracket\n");
        repo.write("c1.txt", "not matched by a glob\n");
        repo.done(GitAction::Stage {
            paths: paths(&["a.txt", "c[1].txt"]),
        });
        assert_eq!(
            repo.status(),
            "A  a.txt\nA  c[1].txt\n?? b.txt\n?? c1.txt\n"
        );

        // Before the first commit, unstaging drops the entry and keeps the edited file.
        repo.write("a.txt", "a, edited\n");
        repo.done(GitAction::Unstage {
            paths: paths(&["a.txt"]),
        });
        assert_eq!(
            repo.status(),
            "A  c[1].txt\n?? a.txt\n?? b.txt\n?? c1.txt\n"
        );
        assert_eq!(repo.read("a.txt").as_deref(), Some("a, edited\n"));

        repo.done(GitAction::Stage {
            paths: paths(&["a.txt", "b.txt", "c1.txt"]),
        });
        assert!(matches!(
            repo.act(GitAction::Commit {
                message: "first".into(),
                amend: false,
            }),
            Ok(GitWriteResult::Committed { .. })
        ));

        // A deletion stages and unstages like any change.
        fs::remove_file(repo.0.join("b.txt")).unwrap();
        repo.done(GitAction::Stage {
            paths: paths(&["b.txt"]),
        });
        assert_eq!(
            repo.git(&["diff", "--cached", "--name-status"]),
            "D\tb.txt\n"
        );
        repo.done(GitAction::Unstage {
            paths: paths(&["b.txt"]),
        });
        assert_eq!(repo.git(&["diff", "--cached", "--name-status"]), "");
        assert_eq!(repo.status(), " D b.txt\n");

        // Many paths run as several commands.
        let many: Vec<String> = (0..1500)
            .map(|ix| format!("many/file-{ix:04}.txt"))
            .collect();
        fs::create_dir(repo.0.join("many")).unwrap();
        for path in &many {
            repo.write(path, "x\n");
        }
        repo.done(GitAction::Stage {
            paths: many.clone(),
        });
        assert_eq!(
            repo.git(&["diff", "--cached", "--name-only"])
                .lines()
                .count(),
            1500
        );
        repo.done(GitAction::Unstage { paths: many });
        assert_eq!(repo.git(&["diff", "--cached", "--name-only"]), "");

        for bad in [
            Vec::new(),
            paths(&["../outside.txt"]),
            paths(&["/etc/passwd"]),
        ] {
            assert_eq!(
                repo.act(GitAction::Stage { paths: bad }).unwrap_err().0,
                ErrorCode::InvalidRequest
            );
        }
        let missing = repo
            .act(GitAction::Unstage {
                paths: paths(&["never.txt"]),
            })
            .unwrap_err();
        assert!(missing.1.contains("did not match"), "{missing:?}");
    }

    #[test]
    fn stages_and_unstages_one_hunk_of_two() {
        let Some(repo) = Scratch::new() else {
            return;
        };
        let head: String = (1..=10).map(|ix| format!("{ix}\n")).collect();
        repo.write("file.txt", &head);
        repo.git(&["add", "file.txt"]);
        repo.git(&["commit", "-qm", "first"]);
        let work = head.replace("2\n", "two\n").replace("9\n", "nine\n");
        repo.write("file.txt", &work);

        // Stage the first hunk: index line 1 becomes the work tree's line 1.
        let index = repo.index_text("file.txt");
        assert_eq!(index, head);
        repo.done(GitAction::SetIndexLines {
            path: "file.txt".into(),
            expected: version(&index),
            lines: 1..2,
            replacement: "two\n".into(),
        });
        assert_eq!(repo.changed_lines(&["diff", "--cached"]), ["-2", "+two"]);
        assert_eq!(repo.changed_lines(&["diff"]), ["-9", "+nine"]);
        assert_eq!(repo.read("file.txt").as_deref(), Some(work.as_str()));

        // The tab still holds the old index text: refused, nothing written.
        let stale = repo.act(GitAction::SetIndexLines {
            path: "file.txt".into(),
            expected: version(&index),
            lines: 8..9,
            replacement: "nine\n".into(),
        });
        assert_eq!(stale, Ok(GitWriteResult::Stale));
        assert_eq!(repo.changed_lines(&["diff"]), ["-9", "+nine"]);

        // Unstage it: the staged diff's index lines go back to HEAD's.
        let index = repo.index_text("file.txt");
        repo.done(GitAction::SetIndexLines {
            path: "file.txt".into(),
            expected: version(&index),
            lines: 1..2,
            replacement: "2\n".into(),
        });
        assert_eq!(repo.git(&["diff", "--cached"]), "");
        assert_eq!(repo.changed_lines(&["diff"]), ["-2", "+two", "-9", "+nine"]);

        let outside = repo
            .act(GitAction::SetIndexLines {
                path: "file.txt".into(),
                expected: version(&head),
                lines: 10..12,
                replacement: String::new(),
            })
            .unwrap_err();
        assert_eq!(outside.0, ErrorCode::InvalidRequest);
        repo.write("new.txt", "new\n");
        let untracked = repo
            .act(GitAction::SetIndexLines {
                path: "new.txt".into(),
                expected: version(""),
                lines: 0..0,
                replacement: "new\n".into(),
            })
            .unwrap_err();
        assert!(untracked.1.contains("not in the index"), "{untracked:?}");
    }

    #[test]
    fn hunk_staging_keeps_lf_in_the_index_of_a_crlf_checkout() {
        let Some(repo) = Scratch::new() else {
            return;
        };
        repo.write(".gitattributes", "*.txt text eol=crlf\n");
        repo.write("file.txt", "a\nb\nc\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "first"]);
        // A checkout writes CRLF; the stored blob has LF.
        fs::remove_file(repo.0.join("file.txt")).unwrap();
        repo.git(&["checkout", "--", "file.txt"]);
        assert_eq!(repo.read("file.txt").as_deref(), Some("a\r\nb\r\nc\r\n"));
        repo.write("file.txt", "a\r\nB\r\nc\r\nd\r\n");

        // The client diffs the filtered index text, so it sees and sends CRLF.
        let index = repo.index_text("file.txt");
        assert_eq!(index, "a\r\nb\r\nc\r\n");
        repo.done(GitAction::SetIndexLines {
            path: "file.txt".into(),
            expected: version(&index),
            lines: 1..2,
            replacement: "B\r\n".into(),
        });
        assert_eq!(repo.git(&["cat-file", "blob", ":file.txt"]), "a\nB\nc\n");
        assert_eq!(repo.changed_lines(&["diff", "--cached"]), ["-b", "+B"]);
        assert_eq!(repo.changed_lines(&["diff"]), ["+d"]);
        assert_eq!(repo.index_text("file.txt"), "a\r\nB\r\nc\r\n");
    }

    #[test]
    fn hunk_staging_refuses_a_conflicted_path() {
        let Some(repo) = Scratch::new() else {
            return;
        };
        repo.write("file.txt", "base\n");
        repo.git(&["add", "file.txt"]);
        repo.git(&["commit", "-qm", "base"]);
        repo.git(&["checkout", "-qb", "other"]);
        repo.write("file.txt", "other\n");
        repo.git(&["commit", "-qam", "other"]);
        repo.git(&["checkout", "-q", "-"]);
        repo.write("file.txt", "mine\n");
        repo.git(&["commit", "-qam", "mine"]);
        assert!(repo.try_git(&["merge", "-q", "other"]).is_none());
        let conflicted = repo
            .act(GitAction::SetIndexLines {
                path: "file.txt".into(),
                expected: version("mine\n"),
                lines: 0..1,
                replacement: "both\n".into(),
            })
            .unwrap_err();
        assert!(conflicted.1.contains("merge conflict"), "{conflicted:?}");
        // Staging the whole file marks it resolved.
        repo.write("file.txt", "both\n");
        repo.done(GitAction::Stage {
            paths: paths(&["file.txt"]),
        });
        assert_eq!(repo.status(), "M  file.txt\n");
    }

    #[test]
    fn commits_amends_and_reports_git_refusing() {
        let Some(repo) = Scratch::new() else {
            return;
        };
        let nothing = repo
            .act(GitAction::Commit {
                message: "empty".into(),
                amend: false,
            })
            .unwrap_err();
        assert!(nothing.1.contains("nothing to commit"), "{nothing:?}");
        assert_eq!(
            repo.act(GitAction::LastMessage).unwrap_err().0,
            ErrorCode::Internal
        );

        repo.write("file.txt", "one\n");
        repo.done(GitAction::Stage {
            paths: paths(&["file.txt"]),
        });
        let committed = repo
            .act(GitAction::Commit {
                message: "Summary line\n\nBody text.\n".into(),
                amend: false,
            })
            .unwrap();
        let head = repo.git(&["rev-parse", "--short", "HEAD"]);
        assert_eq!(
            committed,
            GitWriteResult::Committed {
                head: head.trim().to_owned(),
                summary: "Summary line".into(),
            }
        );
        assert_eq!(
            repo.act(GitAction::LastMessage),
            Ok(GitWriteResult::Message {
                text: "Summary line\n\nBody text.".into()
            })
        );

        // With nothing staged a commit fails with git's own words; an amend rewords.
        let nothing = repo
            .act(GitAction::Commit {
                message: "again".into(),
                amend: false,
            })
            .unwrap_err();
        assert!(nothing.1.contains("nothing to commit"), "{nothing:?}");
        let amended = repo
            .act(GitAction::Commit {
                message: "Better summary\n".into(),
                amend: true,
            })
            .unwrap();
        assert!(
            matches!(&amended, GitWriteResult::Committed { summary, .. } if summary == "Better summary"),
            "{amended:?}"
        );
        assert_eq!(repo.git(&["rev-list", "--count", "HEAD"]).trim(), "1");
        assert_eq!(
            repo.git(&["log", "-1", "--format=%B"]),
            "Better summary\n\n"
        );
    }

    #[test]
    fn discards_tracked_changes_and_deletes_untracked_files() {
        let Some(repo) = Scratch::new() else {
            return;
        };
        repo.write("edited.txt", "head\n");
        repo.write("gone.txt", "gone\n");
        repo.write("staged.txt", "head\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "first"]);
        repo.write("edited.txt", "edited\n");
        fs::remove_file(repo.0.join("gone.txt")).unwrap();
        repo.write("staged.txt", "staged\n");
        repo.git(&["add", "staged.txt"]);
        repo.write("staged.txt", "work tree\n");
        repo.write("loose.txt", "loose\n");
        repo.write("kept.txt", "kept\n");

        repo.done(GitAction::Discard {
            paths: paths(&["edited.txt", "gone.txt", "staged.txt"]),
            untracked: paths(&["loose.txt"]),
        });
        assert_eq!(repo.read("edited.txt").as_deref(), Some("head\n"));
        assert_eq!(repo.read("gone.txt").as_deref(), Some("gone\n"));
        // Only the unstaged change goes; the staged one stays.
        assert_eq!(repo.read("staged.txt").as_deref(), Some("staged\n"));
        assert_eq!(repo.read("loose.txt"), None);
        assert_eq!(repo.status(), "M  staged.txt\n?? kept.txt\n");
    }
}
