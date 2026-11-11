//! What OMP saved of each session, read from its files: for the session list, the first
//! prompt and the end of the last reply (ACP lists neither); for a chat with sub-agents,
//! the ones that finished and each one's tool calls (ACP reports those only while a turn
//! runs, and the calls only as the latest few). OMP keeps a session as
//! `<agent folder>/sessions/<folder>/<time>_<id>.jsonl`, one JSON entry a line, and its
//! sub-agents' own sessions as `<time>_<id>/<agent>.jsonl` beside it; its agent folder is
//! `$PI_CODING_AGENT_DIR`, else `~/.omp/agent`. A session whose file is not found keeps
//! what the harness reported.
use compi_protocol::ContentEnvironment;
use compi_protocol::chat::{
    ChatAgentStep, ChatContent, ChatSessionInfo, ChatToolStatus, MAX_CHAT_AGENT_STEPS,
    MAX_CHAT_SESSION_PREVIEW_CHARS,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

/// How far into a file the first prompt is looked for; it comes within a few entries.
const HEAD_BYTES: u64 = 1024 * 1024;
/// How much of a file's end is read for the last reply.
const TAIL_BYTES: u64 = 512 * 1024;
/// How much of a file one look for sub-agents' news reads.
const AGENTS_BYTES: u64 = 8 * 1024 * 1024;

/// Fills in each session's first prompt and last reply from its file.
pub(super) fn fill(content: &ChatContent, sessions: &mut [ChatSessionInfo]) {
    if sessions.is_empty() {
        return;
    }
    let Some(root) = sessions_folder(&content.environment) else {
        return;
    };
    let wanted: HashSet<&str> = sessions.iter().map(|session| session.id.as_str()).collect();
    let files = session_files(&root, &wanted);
    for session in sessions {
        if let Some(path) = files.get(&session.id) {
            let saved = read(path);
            session.first_prompt = saved.first_prompt;
            session.last_reply = saved.last_reply;
        }
    }
}

/// The file OMP keeps `session` in, where the chat's harness runs.
pub(super) fn session_file(content: &ChatContent, session: &str) -> Option<PathBuf> {
    let root = sessions_folder(&content.environment)?;
    session_files(&root, &HashSet::from([session])).remove(session)
}

/// The sub-agents OMP noted as finished in the lines of `path` from `offset` on, each as
/// a `wait` call lists a job (`id`, `type`, `status`, `durationMs`, `resultText`). OMP
/// writes an `async-result` entry when a background job ends. `offset` moves past the
/// whole lines read; a line still being written is read next time.
pub(super) fn finished_agents(path: &Path, offset: &mut u64) -> Vec<Value> {
    let mut jobs = Vec::new();
    for line in new_lines(path, offset).lines() {
        if !line.contains(r#""customType":"async-result""#) {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let content = entry["content"].as_str().unwrap_or_default();
        for job in entry["details"]["jobs"].as_array().into_iter().flatten() {
            let id = job["agentId"].as_str().or_else(|| job["jobId"].as_str());
            let (Some(id), "task") = (id, job["type"].as_str().unwrap_or_default()) else {
                continue;
            };
            jobs.push(serde_json::json!({
                "id": id,
                "type": "task",
                "status": job["status"],
                "durationMs": job["durationMs"],
                "resultText": task_result(content, id),
            }));
        }
    }
    jobs
}

/// The whole lines `path` gained from `offset` on, at most `AGENTS_BYTES` a look.
/// `offset` moves past them; a line still being written is read next time.
fn new_lines(path: &Path, offset: &mut u64) -> String {
    let Ok(len) = std::fs::metadata(path).map(|metadata| metadata.len()) else {
        return String::new();
    };
    // A file rewritten shorter is read on from its end.
    if len <= *offset {
        *offset = len;
        return String::new();
    }
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path).and_then(|mut file| {
        file.seek(SeekFrom::Start(*offset))?;
        file.take(AGENTS_BYTES).read_to_end(&mut bytes)
    });
    if read.is_err() {
        return String::new();
    }
    let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        // One line longer than a look reads holds no news.
        if bytes.len() as u64 == AGENTS_BYTES {
            *offset += AGENTS_BYTES;
        }
        return String::new();
    };
    *offset += end as u64 + 1;
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// A sub-agent's tool calls as its own session file tells them, kept from look to look.
#[derive(Default)]
pub(super) struct AgentSteps {
    offset: u64,
    /// The tool call ID of each step kept, in the steps' order.
    calls: VecDeque<String>,
    /// The latest `MAX_CHAT_AGENT_STEPS` calls, oldest first.
    pub(super) steps: VecDeque<ChatAgentStep>,
    /// Every call it made, those no longer kept included.
    pub(super) count: u64,
}

impl AgentSteps {
    /// Reads what `path` (the sub-agent's session file) gained; whether a call began or
    /// ended. OMP writes a `tool_execution_start` entry as a call begins and its
    /// `toolResult` message as it ends.
    pub(super) fn read(&mut self, path: &Path) -> bool {
        let mut changed = false;
        for line in new_lines(path, &mut self.offset).lines() {
            if line.contains(r#""customType":"tool_execution_start""#) {
                let Ok(entry) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                let data = &entry["data"];
                let (Some(call), Some(text)) = (data["toolCallId"].as_str(), step_text(data))
                else {
                    continue;
                };
                if self.steps.len() == MAX_CHAT_AGENT_STEPS {
                    self.steps.pop_front();
                    self.calls.pop_front();
                }
                self.calls.push_back(call.to_owned());
                self.steps.push_back(ChatAgentStep {
                    text,
                    status: ChatToolStatus::Running,
                });
                self.count += 1;
                changed = true;
            } else if line.contains(r#""role":"toolResult""#) {
                let Ok(entry) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                let message = &entry["message"];
                let Some(step) = message["toolCallId"]
                    .as_str()
                    .and_then(|call| self.calls.iter().rposition(|kept| kept == call))
                else {
                    continue;
                };
                self.steps[step].status = if message["isError"].as_bool() == Some(true) {
                    ChatToolStatus::Failed
                } else {
                    ChatToolStatus::Completed
                };
                changed = true;
            }
        }
        changed
    }
}

/// A call's line: the intent the agent stated, else the tool with the argument that
/// says most (a command, a path, what it searched for).
fn step_text(data: &Value) -> Option<String> {
    let stated = data["intent"]
        .as_str()
        .map(str::trim)
        .filter(|intent| !intent.is_empty());
    if let Some(intent) = stated {
        return super::short_line(intent);
    }
    let tool = data["toolName"].as_str()?;
    let argument = ["command", "path", "pattern", "query", "url"]
        .into_iter()
        .find_map(|key| data["args"][key].as_str());
    match argument {
        Some(argument) => super::short_line(&format!("{tool} {argument}")),
        None => super::short_line(tool),
    }
}

/// The `<task-result id="…">…</task-result>` block of the sub-agent `id` in a notice.
fn task_result<'a>(notice: &'a str, id: &str) -> Option<&'a str> {
    let start = notice.find(&format!(r#"<task-result id="{id}""#))?;
    let block = &notice[start..];
    const CLOSE: &str = "</task-result>";
    Some(match block.find(CLOSE) {
        Some(end) => &block[..end + CLOSE.len()],
        None => block,
    })
}

/// Where the harness's environment keeps OMP's sessions, as this daemon reaches it.
fn sessions_folder(environment: &ContentEnvironment) -> Option<PathBuf> {
    match environment {
        ContentEnvironment::Host => {
            let agent = std::env::var_os("PI_CODING_AGENT_DIR")
                .filter(|folder| !folder.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    #[cfg(windows)]
                    let home = std::env::var_os("USERPROFILE");
                    #[cfg(unix)]
                    let home = std::env::var_os("HOME");
                    home.map(|home| PathBuf::from(home).join(".omp").join("agent"))
                })?;
            Some(agent.join("sessions"))
        }
        #[cfg(windows)]
        ContentEnvironment::Wsl { distribution } => wsl_sessions_folder(distribution),
        #[cfg(unix)]
        ContentEnvironment::Wsl { .. } => None,
    }
}

/// The distribution's sessions folder as a `\\wsl.localhost` path, asked of WSL once per
/// distribution (found ones only).
#[cfg(windows)]
fn wsl_sessions_folder(distribution: &str) -> Option<PathBuf> {
    use std::os::windows::process::CommandExt as _;
    static FOUND: LazyLock<Mutex<HashMap<String, PathBuf>>> = LazyLock::new(Default::default);
    if let Some(folder) = super::lock(&FOUND).get(distribution) {
        return Some(folder.clone());
    }
    let mut command = std::process::Command::new(super::WSL_EXE);
    command.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
    if let Some(path) = compi_protocol::wsl::launch_path() {
        command.env("PATH", path);
    }
    let output = command
        .args(["--distribution", distribution, "--exec", "/bin/sh", "-c"])
        .arg(r#"printf '%s' "${PI_CODING_AGENT_DIR:-$HOME/.omp/agent}/sessions""#)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let linux = String::from_utf8(output.stdout).ok()?;
    let folder = crate::file_tree::resolve_root(linux.trim(), Some(distribution))
        .ok()?
        .host;
    super::lock(&FOUND).insert(distribution.to_owned(), folder.clone());
    Some(folder)
}

/// The session files under `root` of the session IDs `wanted`, by ID.
fn session_files(root: &Path, wanted: &HashSet<&str>) -> HashMap<String, PathBuf> {
    let mut files = HashMap::new();
    let Ok(folders) = std::fs::read_dir(root) else {
        return files;
    };
    for folder in folders.flatten() {
        let Ok(entries) = std::fs::read_dir(folder.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(session_id) else {
                continue;
            };
            if wanted.contains(id) {
                files.insert(id.to_owned(), entry.path());
            }
        }
    }
    files
}

/// The session ID a file name `<time>_<id>.jsonl` carries.
fn session_id(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".jsonl")?;
    let (_, id) = stem.rsplit_once('_')?;
    (!id.is_empty()).then_some(id)
}

#[derive(Clone, Default)]
struct Saved {
    first_prompt: Option<String>,
    last_reply: Option<String>,
}

/// A file's length and last write, to tell whether it changed.
type Stamp = (u64, Option<SystemTime>);

/// What `path` holds, read again only once it changed.
fn read(path: &Path) -> Saved {
    static READ: LazyLock<Mutex<HashMap<PathBuf, (Stamp, Saved)>>> =
        LazyLock::new(Default::default);
    let Ok(metadata) = std::fs::metadata(path) else {
        return Saved::default();
    };
    let stamp = (metadata.len(), metadata.modified().ok());
    if let Some((read, saved)) = super::lock(&READ).get(path)
        && *read == stamp
    {
        return saved.clone();
    }
    let Ok(file) = std::fs::File::open(path) else {
        return Saved::default();
    };
    let saved = Saved {
        first_prompt: first_prompt(&file),
        last_reply: last_reply(&file, stamp.0),
    };
    super::lock(&READ).insert(path.to_owned(), (stamp, saved.clone()));
    saved
}

/// The first prompt a person typed, from the file's start.
fn first_prompt(file: &std::fs::File) -> Option<String> {
    let mut reader = BufReader::new(file.take(HEAD_BYTES));
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if !line.contains(r#""role":"user""#) {
            continue;
        }
        if let Some(text) = message_text(&line, "user") {
            return Some(head(&text));
        }
    }
}

/// The last reply's text, from the file's end.
fn last_reply(mut file: &std::fs::File, len: u64) -> Option<String> {
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail);
    // A read from the middle starts inside a line.
    let whole = if start > 0 {
        tail.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &tail
    };
    whole
        .lines()
        .rev()
        .filter(|line| line.contains(r#""role":"assistant""#))
        .find_map(|line| message_text(line, "assistant"))
        .map(|text| end(&text))
}

/// The text of a `message` entry by `role`, if it has any. A user message attributed to
/// anyone but the person (OMP's own follow-ups) has none.
fn message_text(line: &str, role: &str) -> Option<String> {
    let entry: Value = serde_json::from_str(line).ok()?;
    let message = &entry["message"];
    if entry["type"] != "message" || message["role"] != role {
        return None;
    }
    if role == "user"
        && message["attribution"]
            .as_str()
            .is_some_and(|by| by != "user")
    {
        return None;
    }
    let text = match &message["content"] {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// `text`'s start, cut to the preview's length.
fn head(text: &str) -> String {
    match text.char_indices().nth(MAX_CHAT_SESSION_PREVIEW_CHARS) {
        Some((cut, _)) => format!("{}…", text[..cut].trim_end()),
        None => text.to_owned(),
    }
}

/// `text`'s end, cut to the preview's length.
fn end(text: &str) -> String {
    let count = text.chars().count();
    match count.checked_sub(MAX_CHAT_SESSION_PREVIEW_CHARS) {
        Some(skip) if skip > 0 => {
            let (cut, _) = text.char_indices().nth(skip).expect("within the text");
            format!("…{}", text[cut..].trim_start())
        }
        _ => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(role: &str, attribution: Option<&str>, text: &str) -> String {
        let mut message = serde_json::json!({
            "role": role,
            "content": [{ "type": "thinking", "thinking": "hm" }, { "type": "text", "text": text }],
        });
        if let Some(by) = attribution {
            message["attribution"] = by.into();
        }
        serde_json::json!({ "type": "message", "message": message }).to_string()
    }

    #[test]
    fn reads_the_first_prompt_and_the_last_reply_text() {
        let folder = std::env::temp_dir().join(format!("compi-saved-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("2026-11-10T03-05-40-956Z_01a1.jsonl");
        let big = "x".repeat(TAIL_BYTES as usize);
        let lines = [
            r#"{"type":"session","id":"01a1","cwd":"/w"}"#.to_owned(),
            line("user", Some("agent"), "a follow-up OMP sent"),
            line("user", Some("user"), "fix the picker"),
            line("assistant", None, "first answer"),
            line("user", Some("user"), "and the scroll"),
            line("assistant", None, "done"),
            // A tool result longer than the tail read hides the reply before it.
            format!(r#"{{"type":"message","message":{{"role":"toolResult","content":"{big}"}}}}"#),
        ];
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let saved = read(&path);
        assert_eq!(saved.first_prompt.as_deref(), Some("fix the picker"));
        assert_eq!(saved.last_reply, None);
        std::fs::write(&path, lines[..6].join("\n") + "\n").unwrap();
        let saved = read(&path);
        assert_eq!(saved.last_reply.as_deref(), Some("done"));
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn finished_sub_agents_are_read_once_each_from_whole_lines() {
        let folder = std::env::temp_dir().join(format!("compi-agents-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("2026-11-10T03-05-40-956Z_01a1.jsonl");
        let notice = |id: &str, status: &str, output: &str| {
            serde_json::json!({
                "type": "custom_message", "customType": "async-result",
                "content": format!("<system-notice>\nBackground job {id} has {status}.\n<task-result id=\"{id}\" agent=\"sonic\" status=\"{status}\" duration=\"9s\">\n\n<output>\n{output}\n</output>\n</task-result>\n\n{id} is now idle\n</system-notice>"),
                "details": { "jobs": [
                    { "jobId": id, "type": "task", "status": status, "agentId": id, "durationMs": 9578 },
                    { "jobId": "bg_1", "type": "bash", "status": "completed" },
                ] },
            })
            .to_string()
        };
        let earlier = notice("Old", "completed", "before the agents started") + "\n";
        std::fs::write(&path, &earlier).unwrap();
        let mut offset = earlier.len() as u64;
        assert!(finished_agents(&path, &mut offset).is_empty());

        let first = line("assistant", None, "spawned")
            + "\n"
            + &notice("A", "completed", "A is done")
            + "\n";
        let second = notice("B", "failed", "B broke");
        // The second entry is still being written.
        let cut = second.len() / 2;
        std::fs::write(&path, format!("{earlier}{first}{}", &second[..cut])).unwrap();
        let jobs = finished_agents(&path, &mut offset);
        assert_eq!(jobs.len(), 1, "{jobs:?}");
        assert_eq!(
            (
                jobs[0]["id"].as_str(),
                jobs[0]["status"].as_str(),
                jobs[0]["durationMs"].as_u64()
            ),
            (Some("A"), Some("completed"), Some(9578))
        );
        let text = jobs[0]["resultText"].as_str().unwrap();
        assert!(text.starts_with("<task-result id=\"A\"") && text.ends_with("</task-result>"));
        assert!(text.contains("A is done"));
        assert!(finished_agents(&path, &mut offset).is_empty());

        std::fs::write(&path, format!("{earlier}{first}{second}\n")).unwrap();
        let jobs = finished_agents(&path, &mut offset);
        assert_eq!(jobs.len(), 1, "{jobs:?}");
        assert_eq!(
            (jobs[0]["id"].as_str(), jobs[0]["status"].as_str()),
            (Some("B"), Some("failed"))
        );
        assert!(finished_agents(&path, &mut offset).is_empty());
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_sub_agents_calls_are_kept_in_order_with_how_each_ended() {
        let folder = std::env::temp_dir().join(format!("compi-steps-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("AgentA.jsonl");
        let start = |call: &str, data: Value| {
            let mut data = data;
            data["toolCallId"] = call.into();
            serde_json::json!({ "type": "custom", "customType": "tool_execution_start", "data": data })
                .to_string()
        };
        let result = |call: &str, failed: bool| {
            serde_json::json!({ "type": "message", "message": {
                "role": "toolResult", "toolCallId": call, "content": [], "isError": failed,
            } })
            .to_string()
        };
        let first = [
            start(
                "c1",
                serde_json::json!({ "toolName": "read", "args": { "path": "a.rs" }, "intent": "Reading the\n  parser" }),
            ),
            result("c1", false),
            start(
                "c2",
                serde_json::json!({ "toolName": "bash", "args": { "command": "cargo test", "timeout": 5 } }),
            ),
        ];
        std::fs::write(&path, first.join("\n") + "\n").unwrap();
        let mut steps = AgentSteps::default();
        assert!(steps.read(&path));
        let shown = |steps: &AgentSteps| -> Vec<(String, ChatToolStatus)> {
            steps
                .steps
                .iter()
                .map(|step| (step.text.clone(), step.status))
                .collect()
        };
        assert_eq!(
            shown(&steps),
            [
                ("Reading the parser".to_owned(), ChatToolStatus::Completed),
                ("bash cargo test".to_owned(), ChatToolStatus::Running),
            ]
        );
        assert!(!steps.read(&path));

        // More calls than are kept: the oldest go, and a result still finds its call.
        let mut more = vec![result("c2", true)];
        for index in 0..MAX_CHAT_AGENT_STEPS {
            more.push(start(
                &format!("n{index}"),
                serde_json::json!({ "toolName": "grep", "args": {} }),
            ));
        }
        more.push(result(&format!("n{}", MAX_CHAT_AGENT_STEPS - 1), false));
        let all = first.iter().chain(&more).cloned().collect::<Vec<_>>();
        std::fs::write(&path, all.join("\n") + "\n").unwrap();
        assert!(steps.read(&path));
        assert_eq!(steps.count, MAX_CHAT_AGENT_STEPS as u64 + 2);
        assert_eq!(steps.steps.len(), MAX_CHAT_AGENT_STEPS);
        assert_eq!(
            (steps.steps[0].text.as_str(), steps.steps[0].status),
            ("grep", ChatToolStatus::Running)
        );
        assert_eq!(
            steps.steps.back().map(|step| step.status),
            Some(ChatToolStatus::Completed)
        );
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn names_carry_the_session_id_after_the_last_underscore() {
        assert_eq!(
            session_id("2026-11-10T03-05-40-956Z_01a1c36b-235c.jsonl"),
            Some("01a1c36b-235c")
        );
        assert_eq!(session_id("notes.md"), None);
        assert_eq!(session_id("2026_.jsonl"), None);
    }

    #[test]
    fn previews_cut_long_text_at_its_start_or_end() {
        let long: String = "é".repeat(MAX_CHAT_SESSION_PREVIEW_CHARS + 5);
        assert_eq!(
            head(&long).chars().count(),
            MAX_CHAT_SESSION_PREVIEW_CHARS + 1
        );
        assert!(head(&long).ends_with('…'));
        assert!(end(&long).starts_with('…'));
        assert_eq!(
            end(&long).chars().count(),
            MAX_CHAT_SESSION_PREVIEW_CHARS + 1
        );
        assert_eq!(head("short"), "short");
        assert_eq!(end("short"), "short");
    }
}
