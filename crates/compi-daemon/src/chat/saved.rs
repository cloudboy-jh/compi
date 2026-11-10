//! What OMP saved of each session, read from its files for the session list: the first
//! prompt and the end of the last reply. ACP lists neither. OMP keeps a session as
//! `<agent folder>/sessions/<folder>/<time>_<id>.jsonl`, one JSON entry a line, its
//! agent folder `$PI_CODING_AGENT_DIR`, else `~/.omp/agent`. A session whose file is not
//! found keeps what the harness listed.
use compi_protocol::ContentEnvironment;
use compi_protocol::chat::{ChatContent, ChatSessionInfo, MAX_CHAT_SESSION_PREVIEW_CHARS};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

/// How far into a file the first prompt is looked for; it comes within a few entries.
const HEAD_BYTES: u64 = 1024 * 1024;
/// How much of a file's end is read for the last reply.
const TAIL_BYTES: u64 = 512 * 1024;

/// Fills in each session's first prompt and last reply from its file.
pub(super) fn fill(content: &ChatContent, sessions: &mut [ChatSessionInfo]) {
    if sessions.is_empty() {
        return;
    }
    let Some(root) = sessions_folder(&content.environment) else {
        return;
    };
    let files = session_files(&root, sessions);
    for session in sessions {
        if let Some(path) = files.get(&session.id) {
            let saved = read(path);
            session.first_prompt = saved.first_prompt;
            session.last_reply = saved.last_reply;
        }
    }
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

/// The files of `sessions` under `root`, by session ID.
fn session_files(root: &Path, sessions: &[ChatSessionInfo]) -> HashMap<String, PathBuf> {
    let wanted: HashSet<&str> = sessions.iter().map(|session| session.id.as_str()).collect();
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
