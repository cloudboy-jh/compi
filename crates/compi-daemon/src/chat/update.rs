//! Updating an agent's program from Compi: from a chat (`/update`), the program the
//! chat runs it with, or from Settings → Agents, its Windows or Linux program. The
//! agent's own updater runs (`AgentEntry::update`: `omp update`, `claude update`,
//! `codex update`, `opencode upgrade`), so it updates the way it was installed.
//!
//! Every chat running that program stops first, since Windows can't replace a running
//! program, and starts again on its session afterwards, whether the update worked or
//! not; none may be running a turn (`ChatHub::running`).
use super::launch::{self, LAUNCH_MARK};
use super::{Chat, lock, login};
use compi_protocol::ContentEnvironment;
use compi_protocol::chat::{
    AgentSide, AgentUpdated, ChatAgentUpdate, ChatEvent, ChatHarness, ChatStatus, ChatUpdateStep,
};
use std::collections::HashSet;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex, mpsc};
use std::thread;

/// One installed program of an agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Program {
    pub(super) harness: ChatHarness,
    /// Its Windows program; else its Linux one, in `distribution` on Windows.
    windows: bool,
    distribution: Option<String>,
}

impl Program {
    /// The program `chat` runs its agent with, once it started it.
    pub(super) fn of(chat: &Chat) -> Result<Self, String> {
        let harness = chat.content.harness;
        let windows = lock(&chat.inner)
            .started
            .as_ref()
            .map(|started| started.windows)
            .ok_or_else(|| format!("{} hasn't started yet.", harness.name()))?;
        let distribution = match (&chat.content.environment, windows) {
            (_, true) => None,
            (ContentEnvironment::Wsl { distribution }, false) => Some(distribution.clone()),
            (ContentEnvironment::Host, false) => launch::default_distribution(),
        };
        Ok(Self {
            harness,
            windows,
            distribution,
        })
    }

    /// `harness`'s program on `side`.
    pub(super) fn on(harness: ChatHarness, side: AgentSide) -> Self {
        let windows = side == AgentSide::Windows;
        Self {
            harness,
            windows,
            distribution: if windows {
                None
            } else {
                launch::default_distribution()
            },
        }
    }

    fn command(&self, args: &[&str]) -> Result<Command, String> {
        launch::side_command(
            self.harness,
            self.windows,
            self.distribution.as_deref(),
            args,
        )
    }

    /// Its version, as `--version` says.
    fn version(&self) -> Option<String> {
        let output = self
            .command(&["--version"])
            .ok()?
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let said = String::from_utf8_lossy(&output.stdout);
        launch::version(&without_marks(&said))
    }
}

/// The programs being updated.
static UPDATING: LazyLock<Mutex<HashSet<Program>>> = LazyLock::new(Mutex::default);

/// Updates `program` on a thread of its own: `chats`, which run it, stop meanwhile and
/// start again after; `asked`, the chat the update was asked from, shows how it goes.
/// `done` gets the versions before and after, or what went wrong. What keeps it from
/// starting is returned; `done` gets it too, unless no thread could start.
pub(super) fn start(
    program: Program,
    chats: Vec<Arc<Chat>>,
    asked: Option<Arc<Chat>>,
    done: impl FnOnce(Result<AgentUpdated, String>) + Send + 'static,
) -> Result<(), String> {
    if !lock(&UPDATING).insert(program.clone()) {
        let error = format!("{} is already being updated.", program.harness.name());
        done(Err(error.clone()));
        return Err(error);
    }
    let show = move |update: ChatAgentUpdate| {
        if let Some(chat) = &asked {
            let mut inner = chat.lock();
            if !inner.closed {
                inner.emit(ChatEvent::Update {
                    update: Some(update),
                });
            }
        }
    };
    show(ChatAgentUpdate {
        from: None,
        to: None,
        step: ChatUpdateStep::Running { note: None },
    });
    for chat in &chats {
        let mut inner = chat.lock();
        inner.stop();
        inner.emit(ChatEvent::Status {
            status: ChatStatus::Starting,
        });
    }
    let updating = program.clone();
    let thread = thread::Builder::new()
        .name("compi-agent-update".into())
        .spawn(move || {
            let result = run(&program, |note| {
                show(ChatAgentUpdate {
                    from: None,
                    to: None,
                    step: ChatUpdateStep::Running { note: Some(note) },
                });
            });
            lock(&UPDATING).remove(&program);
            for chat in &chats {
                chat.restart();
            }
            show(match &result {
                Ok(updated) => ChatAgentUpdate {
                    from: updated.from.clone(),
                    to: updated.to.clone(),
                    step: ChatUpdateStep::Done,
                },
                Err(message) => ChatAgentUpdate {
                    from: None,
                    to: None,
                    step: ChatUpdateStep::Failed {
                        message: message.clone(),
                    },
                },
            });
            done(result);
        });
    thread.map(drop).map_err(|error| {
        lock(&UPDATING).remove(&updating);
        format!("Couldn't start the update: {error}")
    })
}

/// Runs `program`'s updater to its end, each line it writes to `note`.
fn run(program: &Program, mut note: impl FnMut(String)) -> Result<AgentUpdated, String> {
    let name = program.harness.name();
    let from = program.version();
    let mut child = program
        .command(program.harness.entry().update)?
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Couldn't run {name}'s updater: {error}"))?;
    let (lines, said) = mpsc::channel::<String>();
    let readers: Vec<_> = [
        child.stdout.take().map(|output| Box::new(output) as Box<dyn Read + Send>),
        child.stderr.take().map(|output| Box::new(output) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|output| {
        let lines = lines.clone();
        thread::spawn(move || read_lines(output, &lines))
    })
    .collect();
    drop(lines);
    let mut last = None;
    for line in said {
        last = Some(line.clone());
        note(line);
    }
    for reader in readers {
        let _ = reader.join();
    }
    let status = child
        .wait()
        .map_err(|error| format!("{name}'s updater didn't finish: {error}"))?;
    if !status.success() {
        return Err(last.unwrap_or_else(|| match status.code() {
            Some(127) => format!("{name} isn't installed there."),
            Some(code) => format!("{name}'s updater failed (exit code {code})."),
            None => format!("{name}'s updater stopped."),
        }));
    }
    Ok(AgentUpdated {
        from,
        to: program.version(),
    })
}

/// Sends each line `output` writes, a carriage return ending one too (progress bars),
/// without colour codes or the launch script's mark.
fn read_lines(mut output: impl Read, lines: &mpsc::Sender<String>) {
    let mut pending = String::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = match output.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        pending.push_str(&String::from_utf8_lossy(&buffer[..read]));
        while let Some(end) = pending.find(['\n', '\r']) {
            let line: String = pending.drain(..=end).collect();
            send_line(&line, lines);
        }
    }
    send_line(&pending, lines);
}

fn send_line(line: &str, lines: &mpsc::Sender<String>) {
    let line = login::plain(line);
    let line = line.trim();
    if !line.is_empty() && !line.starts_with(LAUNCH_MARK.trim()) {
        let _ = lines.send(line.to_owned());
    }
}

/// `said` without the launch script's mark lines.
fn without_marks(said: &str) -> String {
    said.lines()
        .filter(|line| !line.starts_with(LAUNCH_MARK))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updater_lines_leave_out_colours_progress_redraws_and_the_launch_mark() {
        let (sender, receiver) = mpsc::channel();
        read_lines(
            "\u{1e}compi-agent linux /home/me\nDownloading 10%\rDownloading 100%\r\n\u{1b}[32mUpdated to 19.2.0\u{1b}[0m"
                .as_bytes(),
            &sender,
        );
        drop(sender);
        assert_eq!(
            receiver.iter().collect::<Vec<_>>(),
            ["Downloading 10%", "Downloading 100%", "Updated to 19.2.0"]
        );
        assert_eq!(
            without_marks("\u{1e}compi-agent linux /home/v1.2\nomp/19.1.0\n"),
            "omp/19.1.0"
        );
    }
}
