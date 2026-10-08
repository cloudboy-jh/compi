//! Bounded, cached observational queries. Never run this from the PTY output worker.
use compi_protocol::metadata::*;
use compi_protocol::{ProcessLifetimeId, SurfaceId};
use parking_lot::Mutex;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const REFRESH: Duration = Duration::from_secs(5);
const BUDGET: Duration = Duration::from_millis(1500);
const MAX_OUTPUT: u64 = 64 * 1024;

#[derive(Clone)]
pub struct MetadataInput {
    pub surface_id: SurfaceId,
    pub process_lifetime_id: ProcessLifetimeId,
    pub title: String,
    /// Only a live OSC 7 report, not the directory supplied at launch.
    pub current_directory: Option<String>,
    pub shell_pid: Option<u32>,
    pub shell_executable: Option<String>,
    /// Native Unix PTY tcgetpgrp fallback for shells without bridge support.
    pub foreground_pgid: Option<u32>,
    pub environment: EnvironmentIdentity,
    pub cols: u16,
    pub rows: u16,
    pub running: bool,
}

#[derive(Default)]
pub struct MetadataCache {
    entry: Mutex<Option<(Instant, PaneMetadata)>>,
    collecting: Mutex<()>,
}

impl MetadataCache {
    /// Call from a request/background thread, after releasing the terminal-state lock.
    pub fn get(&self, input: MetadataInput) -> PaneMetadata {
        let cached = self.entry.lock().clone();
        let mut valid = cached.filter(|(_, value)| {
            value.process_lifetime_id == input.process_lifetime_id
                && value.environment.kind == input.environment.kind
                && value.environment.distribution == input.environment.distribution
        });
        if !input.running {
            let mut value = valid.map_or_else(|| initial(&input), |(_, value)| value);
            value.mark_stale("terminal is not running");
            return value;
        }
        if valid.as_ref().is_some_and(|(at, value)| {
            at.elapsed() < REFRESH && value.directory.value == input.current_directory
        }) && let Some((_, mut value)) = valid.take()
        {
            value.title = input.title;
            value.dimensions = PaneDimensions {
                cols: input.cols,
                rows: input.rows,
            };
            return value;
        }
        let Some(_collection) = self.collecting.try_lock() else {
            let mut value = valid.map_or_else(|| initial(&input), |(_, value)| value);
            value.mark_stale("metadata refresh in progress");
            return value;
        };
        let mut value = initial(&input);
        collect(&input, &mut value, valid.as_ref().map(|(_, value)| value));
        *self.entry.lock() = Some((Instant::now(), value.clone()));
        value
    }
}

/// A persisted pane may outlive its runtime. Inspection must still expose its identity
/// and truthful unavailable fields, without querying another process.
pub(crate) fn unavailable(surface: &compi_protocol::SurfaceInfo) -> PaneMetadata {
    initial(&MetadataInput {
        surface_id: surface.id.clone(),
        process_lifetime_id: surface.process_lifetime_id.clone(),
        title: String::new(),
        current_directory: None,
        shell_pid: None,
        shell_executable: None,
        foreground_pgid: None,
        environment: EnvironmentIdentity {
            kind: if cfg!(windows) {
                EnvironmentKind::Wsl
            } else {
                EnvironmentKind::Unix
            },
            hostname: MetadataField::unavailable("terminal runtime unavailable"),
            distribution: surface
                .working_directory
                .as_ref()
                .map(|directory| directory.distribution.clone()),
        },
        cols: surface.cols as u16,
        rows: surface.rows as u16,
        running: false,
    })
}

fn initial(input: &MetadataInput) -> PaneMetadata {
    PaneMetadata {
        surface_id: input.surface_id.clone(),
        process_lifetime_id: input.process_lifetime_id.clone(),
        state: MetadataState::Unavailable,
        environment: input.environment.clone(),
        collected_at_ms: None,
        title: input.title.clone(),
        shell_executable: input.shell_executable.clone(),
        directory: input.current_directory.clone().map_or_else(
            || MetadataField::unavailable("shell has not reported its current directory"),
            MetadataField::available,
        ),
        process: MetadataField::unavailable("terminal foreground process unavailable"),
        git: MetadataField::unavailable("current directory unavailable"),
        dimensions: PaneDimensions {
            cols: input.cols,
            rows: input.rows,
        },
    }
}

fn collect(input: &MetadataInput, value: &mut PaneMetadata, previous: Option<&PaneMetadata>) {
    let deadline = Instant::now() + BUDGET;
    if input.environment.kind != EnvironmentKind::Windows {
        value.environment.hostname = match previous
            .map(|previous| &previous.environment.hostname)
            .filter(|hostname| hostname.state == MetadataState::Available)
        {
            Some(hostname) => hostname.clone(),
            None => match run(input, "hostname", &[], deadline) {
                Ok(host) if !host.trim().is_empty() => {
                    MetadataField::available(host.trim().to_owned())
                }
                _ => stale_or_unavailable(
                    previous.map(|previous| &previous.environment.hostname),
                    "environment hostname unavailable",
                ),
            },
        };
        if input.foreground_pgid.is_none() && input.shell_pid.is_none() {
            value.process =
                MetadataField::unavailable("shell/PTY foreground process group unavailable");
        } else {
            let with_tpgid = input.foreground_pgid.is_none();
            let columns = if with_tpgid {
                "pid=,pgid=,tpgid=,comm="
            } else {
                "pid=,pgid=,comm="
            };
            value.process = match run(input, "ps", &["-eo", columns], deadline).and_then(|text| {
                let group = input.foreground_pgid.or_else(|| {
                    text.lines()
                        .filter_map(|line| process_line(line, true))
                        .find(|(pid, _, _, _)| Some(*pid) == input.shell_pid)
                        .and_then(|(_, _, group, _)| group)
                });
                group
                    .and_then(|group| foreground_process(&text, group, with_tpgid))
                    .ok_or_else(|| "terminal foreground process unavailable".into())
            }) {
                Ok(process) => MetadataField::available(process),
                Err(reason) => stale_or_unavailable(previous.map(|value| &value.process), &reason),
            };
        }
    } else {
        // ConPTY has no Unix-style foreground process group. Never label its launcher as active.
        value.process =
            MetadataField::unavailable("native ConPTY does not expose a foreground process group");
    }
    if let Some(cwd) = input.current_directory.as_deref() {
        value.git = match run(
            input,
            "git",
            &[
                "--no-optional-locks",
                "-C",
                cwd,
                "status",
                "--porcelain=v2",
                "--branch",
                "--untracked-files=normal",
            ],
            deadline,
        ) {
            Ok(status) => MetadataField::available(parse_git(&status)),
            Err(reason)
                if reason.contains("not a git repository")
                    || reason.contains("must be run in a work tree") =>
            {
                MetadataField {
                    state: MetadataState::Available,
                    value: None,
                    reason: None,
                }
            }
            Err(reason) => stale_or_unavailable(
                previous
                    .filter(|value| value.directory.value.as_deref() == Some(cwd))
                    .map(|value| &value.git),
                &reason,
            ),
        };
    }
    value.collected_at_ms = Some(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64,
    );
    value.state = MetadataState::Available;
}

fn stale_or_unavailable<T: Clone>(
    previous: Option<&MetadataField<T>>,
    reason: &str,
) -> MetadataField<T> {
    match previous.filter(|field| field.state != MetadataState::Unavailable) {
        Some(field) => {
            let mut field = field.clone();
            field.mark_stale(reason);
            field
        }
        None => MetadataField::unavailable(reason),
    }
}

fn process_line(line: &str, with_tpgid: bool) -> Option<(u32, u32, Option<u32>, &str)> {
    let (pid, rest) = line.trim().split_once(char::is_whitespace)?;
    let (pgid, rest) = rest.trim_start().split_once(char::is_whitespace)?;
    let (tpgid, name) = if with_tpgid {
        let (group, name) = rest.trim_start().split_once(char::is_whitespace)?;
        (
            group.parse::<u32>().ok().filter(|group| *group > 0),
            name.trim(),
        )
    } else {
        (None, rest.trim())
    };
    Some((pid.parse().ok()?, pgid.parse().ok()?, tpgid, name))
}

fn foreground_process(text: &str, group: u32, with_tpgid: bool) -> Option<ActiveProcess> {
    text.lines()
        .filter_map(|line| process_line(line, with_tpgid))
        .filter(|(_, pgid, _, name)| *pgid == group && !name.is_empty())
        .min_by_key(|(pid, _, _, _)| (*pid != group, *pid))
        .map(|(pid, _, _, name)| ActiveProcess {
            pid,
            process_group: group,
            name: name.to_owned(),
        })
}

fn parse_git(text: &str) -> GitMetadata {
    let mut value = GitMetadata {
        branch: None,
        commit: None,
        changed: false,
    };
    for line in text.lines() {
        if let Some(branch) = line.strip_prefix("# branch.head ") {
            if branch != "(detached)" {
                value.branch = Some(branch.to_owned());
            }
        } else if let Some(commit) = line.strip_prefix("# branch.oid ") {
            if commit != "(initial)" {
                value.commit = Some(commit.to_owned());
            }
        } else if !line.starts_with('#') && !line.is_empty() {
            value.changed = true;
        }
    }
    value
}

fn run(
    input: &MetadataInput,
    program: &str,
    args: &[&str],
    deadline: Instant,
) -> Result<String, String> {
    if Instant::now() >= deadline {
        return Err("metadata refresh timed out".into());
    }
    #[cfg(windows)]
    let mut command = if input.environment.kind == EnvironmentKind::Wsl {
        use std::os::windows::process::CommandExt;
        let distribution = input
            .environment
            .distribution
            .as_deref()
            .ok_or("WSL distribution unavailable")?;
        let mut command = Command::new(r"C:\Windows\System32\wsl.exe");
        command.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
        command.args([
            "--distribution",
            distribution,
            "--exec",
            "timeout",
            "1s",
            program,
        ]);
        command
    } else {
        Command::new(program)
    };
    #[cfg(unix)]
    let mut command = {
        let _ = input;
        Command::new(program)
    };
    command
        .args(args)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
    }
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let stdout = child.stdout.take().ok_or("metadata stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("metadata stderr unavailable")?;
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.take(MAX_OUTPUT + 1).read_to_end(&mut bytes);
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.take(MAX_OUTPUT + 1).read_to_end(&mut bytes);
        bytes
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(result.err().map_or_else(
                    || "metadata refresh timed out".into(),
                    |error| error.to_string(),
                ));
            }
        }
    };
    let stdout = out.join().map_err(|_| "metadata reader failed")?;
    let stderr = err.join().map_err(|_| "metadata reader failed")?;
    if stdout.len() as u64 > MAX_OUTPUT || stderr.len() as u64 > MAX_OUTPUT {
        return Err("metadata output exceeded limit".into());
    }
    if !status.success() {
        return Err(String::from_utf8_lossy(&stderr)
            .trim()
            .chars()
            .take(300)
            .collect());
    }
    String::from_utf8(stdout).map_err(|_| "metadata output was not UTF-8".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn foreground_group_excludes_launcher_and_selects_group_leader() {
        let process = foreground_process(
            "10 10 bash\n21 20 worker\n20 20 editor\n9 9 daemon",
            20,
            false,
        )
        .unwrap();
        assert_eq!(process.name, "editor");
        assert_eq!(process.pid, 20);
    }
    #[test]
    fn detached_dirty_and_initial_worktrees_have_distinct_metadata() {
        assert_eq!(
            parse_git("# branch.head (detached)\n# branch.oid abc\n? new.txt").branch,
            None
        );
        assert!(parse_git("# branch.head (detached)\n? new.txt").changed);
        assert_eq!(
            parse_git("# branch.head main\n# branch.oid (initial)").commit,
            None
        );
        assert!(!parse_git("# branch.head main\n# branch.oid abc").changed);
    }
    #[test]
    fn stopped_cache_never_reuses_another_lifetime_or_distribution() {
        let mut input = MetadataInput {
            surface_id: SurfaceId::from("pane"),
            process_lifetime_id: ProcessLifetimeId::from("first"),
            title: String::new(),
            current_directory: Some("/project".into()),
            shell_pid: None,
            shell_executable: Some("/bin/bash".into()),
            foreground_pgid: None,
            environment: EnvironmentIdentity {
                kind: EnvironmentKind::Wsl,
                hostname: MetadataField::available("linux".into()),
                distribution: Some("Ubuntu".into()),
            },
            cols: 80,
            rows: 24,
            running: false,
        };
        let cache = MetadataCache::default();
        let mut old = initial(&input);
        old.collected_at_ms = Some(1);
        old.state = MetadataState::Available;
        old.process = MetadataField::available(ActiveProcess {
            pid: 5,
            process_group: 5,
            name: "editor".into(),
        });
        *cache.entry.lock() = Some((Instant::now(), old));
        let stopped = cache.get(input.clone());
        assert_eq!(stopped.process.state, MetadataState::Stale);
        assert_eq!(stopped.state, MetadataState::Stale);
        assert_eq!(stopped.process.value.unwrap().name, "editor");
        input.environment.distribution = Some("Debian".into());
        assert_eq!(
            cache.get(input.clone()).process.state,
            MetadataState::Unavailable
        );
        input.environment.distribution = Some("Ubuntu".into());
        input.process_lifetime_id = ProcessLifetimeId::from("second");
        assert!(cache.get(input).process.value.is_none());
    }
}
