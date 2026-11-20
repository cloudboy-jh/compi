//! Claude Code's saved sessions, read from its files for the session list: it keeps a
//! folder's sessions as `<config>/projects/<folder key>/<session>.jsonl`, one JSON
//! entry a line, where the key is the folder with every character but letters and
//! digits as `-`, as the running `claude` names the folder (a Windows `claude` in WSL
//! names it by its `\\wsl.localhost` path).
use compi_protocol::ContentEnvironment;
use compi_protocol::chat::{ChatContent, ChatSessionInfo, MAX_CHAT_SESSION_PREVIEW_CHARS};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// How far into a file the first prompt and the title are looked for.
const HEAD_BYTES: u64 = 1024 * 1024;
/// How much of a file's end is read for the last reply and the latest title.
const TAIL_BYTES: u64 = 512 * 1024;

/// The folder holding the chat's sessions, as this daemon reaches it, for Claude Code
/// configured in `config` (its folder, as the running `claude` names it) and seeing the
/// chat's folder as `cwd`, when it said.
pub(super) fn sessions_folder(
    content: &ChatContent,
    config: &str,
    cwd: Option<&str>,
) -> Option<PathBuf> {
    let windows = is_windows_path(config);
    let projects = reachable(&content.environment, config)?.join("projects");
    let folder = match cwd {
        Some(cwd) => cwd.to_owned(),
        None => claude_folder(&content.environment, &content.folder, windows),
    };
    Some(projects.join(project_key(&folder)))
}

/// `folder` as a `claude` sees it: by its `\\wsl.localhost` path for a Windows one in
/// WSL, else as it is.
fn claude_folder(environment: &ContentEnvironment, folder: &str, windows: bool) -> String {
    match environment {
        ContentEnvironment::Wsl { distribution } if windows => {
            format!(
                r"\\wsl.localhost\{distribution}{}",
                folder.replace('/', r"\")
            )
        }
        _ => folder.to_owned(),
    }
}

/// A Windows path: a drive's or a share's.
fn is_windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with(r"\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'))
}

/// `path`, as the chat's `claude` names it, as this daemon reaches it.
fn reachable(environment: &ContentEnvironment, path: &str) -> Option<PathBuf> {
    if is_windows_path(path) {
        return cfg!(windows).then(|| PathBuf::from(path));
    }
    match environment {
        ContentEnvironment::Host => Some(PathBuf::from(path)),
        #[cfg(windows)]
        ContentEnvironment::Wsl { distribution } => {
            crate::file_tree::resolve_root(path, Some(distribution))
                .ok()
                .map(|root| root.host)
        }
        #[cfg(unix)]
        ContentEnvironment::Wsl { .. } => None,
    }
}

/// Claude Code's key for `folder`: every character but ASCII letters and digits as `-`.
fn project_key(folder: &str) -> String {
    folder
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The sessions saved in `folder`, each with its title, first prompt, last reply, last
/// change and size; sessions without a prompt or reply are left out.
pub(super) fn list(folder: &Path) -> Vec<ChatSessionInfo> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let id = path
                .file_name()?
                .to_str()?
                .strip_suffix(".jsonl")
                .filter(|id| !id.is_empty())?
                .to_owned();
            let metadata = entry.metadata().ok().filter(std::fs::Metadata::is_file)?;
            let saved = read(&path, &metadata);
            if saved.first_prompt.is_none() && saved.last_reply.is_none() {
                return None;
            }
            Some(ChatSessionInfo {
                id,
                title: saved.title,
                updated: metadata.modified().ok().map(rfc3339),
                folder: None,
                size: Some(metadata.len()),
                first_prompt: saved.first_prompt,
                last_reply: saved.last_reply,
            })
        })
        .collect()
}

#[derive(Clone, Default)]
struct Saved {
    title: Option<String>,
    first_prompt: Option<String>,
    last_reply: Option<String>,
}

/// A file's length and last write, to tell whether it changed.
type Stamp = (u64, Option<SystemTime>);

/// What `path` holds, read again only once it changed.
fn read(path: &Path, metadata: &std::fs::Metadata) -> Saved {
    static READ: LazyLock<Mutex<HashMap<PathBuf, (Stamp, Saved)>>> =
        LazyLock::new(Default::default);
    let stamp = (metadata.len(), metadata.modified().ok());
    if let Some((read, saved)) = super::super::lock(&READ).get(path)
        && *read == stamp
    {
        return saved.clone();
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return Saved::default();
    };
    let mut saved = Saved::default();
    let mut head_title = None;
    let mut reader = BufReader::new((&mut file).take(HEAD_BYTES));
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
        if let Ok(entry) = serde_json::from_str::<Value>(&line) {
            if let Some(found) = title(&entry) {
                head_title = Some(found);
            }
            if saved.first_prompt.is_none() {
                saved.first_prompt = prompt(&entry).map(|text| head(&text));
            }
        }
        line.clear();
    }
    let start = stamp.0.saturating_sub(TAIL_BYTES);
    let mut tail = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_ok() {
        let _ = file.take(TAIL_BYTES).read_to_end(&mut tail);
    }
    let tail = String::from_utf8_lossy(&tail);
    // A read from the middle starts inside a line.
    let whole = if start > 0 {
        tail.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &tail
    };
    for entry in whole
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        if saved.title.is_none() {
            saved.title = title(&entry);
        }
        if saved.last_reply.is_none() {
            saved.last_reply = reply(&entry).map(|text| end(&text));
        }
        if saved.title.is_some() && saved.last_reply.is_some() {
            break;
        }
    }
    saved.title = saved.title.or(head_title);
    super::super::lock(&READ).insert(path.to_owned(), (stamp, saved.clone()));
    saved
}

/// The session's title an entry gives: the user's, else Claude Code's.
fn title(entry: &Value) -> Option<String> {
    let title = match entry["type"].as_str()? {
        "custom-title" => entry["customTitle"].as_str(),
        "ai-title" => entry["aiTitle"].as_str(),
        "summary" => entry["summary"].as_str(),
        _ => None,
    }?;
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_owned())
}

/// What the person typed, when the entry is a prompt of theirs: not a tool result, not
/// a command's output or Claude Code's own note, not a sub-agent's.
fn prompt(entry: &Value) -> Option<String> {
    if entry["type"] != "user"
        || entry["isMeta"].as_bool() == Some(true)
        || entry["isSidechain"].as_bool() == Some(true)
    {
        return None;
    }
    let text = text(&entry["message"]["content"])?;
    (!text.starts_with('<') && !text.starts_with("[Request interrupted")).then_some(text)
}

/// A reply's text, when the entry is one of the session's own replies.
fn reply(entry: &Value) -> Option<String> {
    if entry["type"] != "assistant" || entry["isSidechain"].as_bool() == Some(true) {
        return None;
    }
    text(&entry["message"]["content"])
}

/// A message's text blocks, joined and trimmed; none when blank.
fn text(content: &Value) -> Option<String> {
    let text = match content {
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

/// `time` in RFC 3339, in UTC, to the second.
fn rfc3339(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Howard Hinnant's days-to-civil, for days since 1970-01-01.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_folder_s_key_is_its_name_with_every_other_character_a_dash() {
        assert_eq!(project_key(r"C:\Users\johns\compi"), "C--Users-johns-compi");
        assert_eq!(
            project_key(r"\\wsl.localhost\Ubuntu\tmp\cprobe"),
            "--wsl-localhost-Ubuntu-tmp-cprobe"
        );
        assert_eq!(project_key("/home/me/my proj"), "-home-me-my-proj");
    }

    #[test]
    fn a_windows_claude_in_wsl_names_the_folder_by_its_share() {
        let wsl = ContentEnvironment::Wsl {
            distribution: "Ubuntu".into(),
        };
        assert_eq!(
            claude_folder(&wsl, "/home/me/work", true),
            r"\\wsl.localhost\Ubuntu\home\me\work"
        );
        assert_eq!(claude_folder(&wsl, "/home/me/work", false), "/home/me/work");
        assert!(is_windows_path(r"C:\Users\me\.claude"));
        assert!(is_windows_path(r"\\wsl.localhost\Ubuntu"));
        assert!(!is_windows_path("/home/me/.claude"));
    }

    #[test]
    fn times_are_rfc3339_in_utc() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let time = UNIX_EPOCH + Duration::from_secs(1_795_209_588);
        assert_eq!(rfc3339(time), "2026-11-20T21:19:48Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400);
        assert_eq!(rfc3339(leap), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn a_session_lists_with_its_title_first_prompt_and_last_reply() {
        let folder =
            std::env::temp_dir().join(format!("compi-claude-store-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let lines = [
            r#"{"type":"queue-operation","operation":"enqueue"}"#,
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"<local-command-caveat>x</local-command-caveat>"}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"fix the picker"}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":""},{"type":"text","text":"first answer"}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"a sub-agent's"}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
            r#"{"type":"custom-title","customTitle":"Picker fix"}"#,
        ];
        std::fs::write(folder.join("s1.jsonl"), lines.join("\n") + "\n").unwrap();
        // Nothing said yet: left out.
        std::fs::write(folder.join("s2.jsonl"), lines[0].to_owned() + "\n").unwrap();
        std::fs::create_dir_all(folder.join("memory")).unwrap();
        let sessions = list(&folder);
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.id, "s1");
        assert_eq!(session.title.as_deref(), Some("Picker fix"));
        assert_eq!(session.first_prompt.as_deref(), Some("fix the picker"));
        assert_eq!(session.last_reply.as_deref(), Some("done"));
        assert!(
            session
                .updated
                .as_deref()
                .is_some_and(|time| time.ends_with('Z'))
        );
        assert!(session.size.is_some_and(|size| size > 0));
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
