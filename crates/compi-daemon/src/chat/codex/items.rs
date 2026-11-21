//! Codex's thread items as the transcript shows them: replies, plans and reasoning as
//! text entries, streamed and then reconciled with the finished item; every other item
//! that does something as a tool card; sub-agents on the card of the call that started
//! them, their threads' items as their steps.
use super::super::{Inner, head, new_agent, now_ms, settle, short_line, tail};
use compi_protocol::chat::{
    ChatAgent, ChatAgentStep, ChatDiff, ChatEntry, ChatEvent, ChatTool, ChatToolStatus,
    MAX_CHAT_AGENT_RESULT_BYTES, MAX_CHAT_AGENT_STEPS, MAX_CHAT_DIFF_BYTES,
    MAX_CHAT_TOOL_INPUT_BYTES, MAX_CHAT_TOOL_OUTPUT_BYTES,
};
use serde_json::{Value, json};
use std::collections::VecDeque;

/// A reply, plan or reasoning item being streamed: its entry and its text so far.
pub(super) struct Stream {
    entry: usize,
    text: String,
    /// The summary or content part the last text was of; a new one begins a paragraph.
    part: i64,
}

/// A sub-agent's thread: the call whose card shows it, its name there, its steps' item
/// IDs in the card's order and its latest reply.
pub(super) struct Child {
    pub(super) call: String,
    agent: String,
    steps: VecDeque<String>,
    reply: Option<String>,
}

/// Whether an entry is a reply or a thought.
#[derive(Clone, Copy, PartialEq)]
enum Text {
    Reply,
    Thought,
}

/// Streamed reply or plan text of item `item`.
pub(super) fn reply_delta(inner: &mut Inner, item: &str, delta: &str) {
    stream(inner, item, Text::Reply, 0, delta);
    inner.activity_ms = now_ms();
}

/// Streamed reasoning of item `item`, of summary or content part `part`.
pub(super) fn thought_delta(inner: &mut Inner, item: &str, part: i64, delta: &str) {
    stream(inner, item, Text::Thought, part, delta);
}

/// `delta` onto the entry of item `item`, else a new entry.
fn stream(inner: &mut Inner, item: &str, kind: Text, part: i64, delta: &str) {
    if delta.is_empty() {
        return;
    }
    let shown = inner
        .codex()
        .streams
        .get(item)
        .map(|stream| (stream.entry, stream.part));
    match shown {
        Some((entry, was)) => {
            let text = if was == part {
                delta.to_owned()
            } else {
                format!("\n\n{delta}")
            };
            if let Some(stream) = inner.codex().streams.get_mut(item) {
                stream.text.push_str(&text);
                stream.part = part;
            }
            inner.emit(ChatEvent::Text { index: entry, text });
        }
        None => {
            let entry = new_text(inner, kind, delta.to_owned(), None);
            inner.codex().streams.insert(
                item.to_owned(),
                Stream {
                    entry,
                    text: delta.to_owned(),
                    part,
                },
            );
        }
    }
}

/// A new reply or thought entry holding `text`; a thought is open unless `ended_ms`.
/// Its index.
fn new_text(inner: &mut Inner, kind: Text, text: String, ended_ms: Option<u64>) -> usize {
    let entry = match kind {
        Text::Reply => {
            inner.end_thought();
            ChatEntry::Agent { text }
        }
        Text::Thought => ChatEntry::Thought {
            text,
            started_ms: now_ms(),
            ended_ms,
        },
    };
    inner.emit(ChatEvent::Entry { entry });
    inner.state.entries.len() - 1
}

/// A finished reply, plan or reasoning item: its whole `text` replaces what was
/// streamed when they differ, or shows as an entry of its own; a thought ends.
fn finish_text(inner: &mut Inner, item: &str, kind: Text, text: &str) {
    let Some(stream) = inner.codex().streams.remove(item) else {
        if !text.trim().is_empty() {
            let ended_ms = (kind == Text::Thought).then(now_ms);
            new_text(inner, kind, text.to_owned(), ended_ms);
        }
        return;
    };
    let text = if text.trim().is_empty() || text == stream.text {
        None
    } else {
        Some(text.to_owned())
    };
    let entry = match inner.state.entries.get(stream.entry) {
        Some(ChatEntry::Agent { .. }) => match text {
            Some(text) => ChatEntry::Agent { text },
            None => return,
        },
        Some(ChatEntry::Thought {
            text: shown,
            started_ms,
            ended_ms,
        }) => ChatEntry::Thought {
            text: text.unwrap_or_else(|| shown.clone()),
            started_ms: *started_ms,
            ended_ms: ended_ms.or_else(|| Some(now_ms())),
        },
        _ => return,
    };
    if inner.state.entries.get(stream.entry) != Some(&entry) {
        inner.emit(ChatEvent::Replace {
            index: stream.entry,
            entry,
        });
    }
}

/// An item of the chat's thread began.
pub(super) fn started(inner: &mut Inner, item: &Value) {
    match item["type"].as_str().unwrap_or_default() {
        "agentMessage" | "plan" | "reasoning" | "userMessage" => {}
        "collabAgentToolCall" => collab(inner, item, false, false),
        "subAgentActivity" => sub_agent_activity(inner, item),
        _ => {
            tool_item(inner, item, false, false);
        }
    }
}

/// An item of the chat's thread finished, or one a resumed thread replays (`replay`).
pub(super) fn completed(inner: &mut Inner, item: &Value, replay: bool) {
    let id = item["id"].as_str().unwrap_or_default();
    match item["type"].as_str().unwrap_or_default() {
        "agentMessage" | "plan" => {
            finish_text(
                inner,
                id,
                Text::Reply,
                item["text"].as_str().unwrap_or_default(),
            );
            inner.activity_ms = now_ms();
        }
        "reasoning" => {
            let parts = |key: &str| -> String {
                item[key]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter(|part| !part.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            };
            let summary = parts("summary");
            let text = if summary.is_empty() {
                parts("content")
            } else {
                summary
            };
            finish_text(inner, id, Text::Thought, &text);
        }
        "userMessage" if replay => replay_prompt(inner, &item["content"]),
        "userMessage" => {}
        "collabAgentToolCall" => collab(inner, item, true, replay),
        "subAgentActivity" => sub_agent_activity(inner, item),
        _ => {
            tool_item(inner, item, true, replay);
        }
    }
}

/// A resumed thread's turns, as its items finished: its prompts, replies, reasoning
/// and calls, and how each turn ended when it did not complete.
pub(super) fn replay(inner: &mut Inner, turns: &Value) {
    for turn in turns.as_array().into_iter().flatten() {
        for item in turn["items"].as_array().into_iter().flatten() {
            completed(inner, item, true);
        }
        inner.end_thought();
        match turn["status"].as_str() {
            Some("interrupted") => inner.emit(ChatEvent::Entry {
                entry: ChatEntry::Cancelled,
            }),
            Some("failed") => inner.emit(ChatEvent::Entry {
                entry: ChatEntry::Error {
                    message: super::turn_error(&turn["error"]),
                },
            }),
            _ => {}
        }
    }
    inner.codex().streams.clear();
}

/// A prompt a resumed thread replays: its text, its images by name.
fn replay_prompt(inner: &mut Inner, content: &Value) {
    let mut texts = Vec::new();
    let mut images = Vec::new();
    for input in content.as_array().into_iter().flatten() {
        match input["type"].as_str() {
            Some("text") => texts.extend(input["text"].as_str()),
            Some("image") => images.push("image".to_owned()),
            Some("localImage") => images.extend(
                input["path"]
                    .as_str()
                    .map(|path| path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned()),
            ),
            _ => {}
        }
    }
    let text = texts.join("\n");
    if text.trim().is_empty() && images.is_empty() {
        return;
    }
    inner.end_thought();
    inner.emit(ChatEvent::Entry {
        entry: ChatEntry::User {
            text,
            files: Vec::new(),
            images,
            sent_ms: None,
            ended_ms: None,
        },
    });
}

/// How an item's status shows: a call stopped by the user's stop is stopped, a declined
/// one failed.
fn item_status(item: &Value, interrupting: bool) -> ChatToolStatus {
    match item["status"].as_str() {
        Some("completed") => {
            if item["exitCode"].as_i64().is_some_and(|code| code != 0)
                || item["success"].as_bool() == Some(false)
            {
                ChatToolStatus::Failed
            } else {
                ChatToolStatus::Completed
            }
        }
        Some("failed" | "declined" | "errored") if interrupting => ChatToolStatus::Stopped,
        Some("failed" | "declined" | "errored") => ChatToolStatus::Failed,
        Some("interrupted") => ChatToolStatus::Stopped,
        _ => ChatToolStatus::Running,
    }
}

/// A tool-like item's card, begun or finished; false for an item that is none.
fn tool_item(inner: &mut Inner, item: &Value, ended: bool, replay: bool) -> bool {
    let Some(id) = item["id"].as_str() else {
        return false;
    };
    let mut tool = inner.tool_call(id);
    if !describe(&mut tool, item) {
        return false;
    }
    let was = tool.status;
    let codex = inner.codex_state();
    let mut status = item_status(item, codex.interrupting);
    if status.is_open() {
        let asked = codex.asks.iter().any(|ask| match &ask.kind {
            super::AskKind::Permission { shown, .. } => shown.tool.as_deref() == Some(id),
            super::AskKind::Question { .. } => false,
        });
        status = match (ended, replay) {
            (false, _) if asked => ChatToolStatus::Pending,
            (false, _) => ChatToolStatus::Running,
            (true, false) => ChatToolStatus::Completed,
            (true, true) => ChatToolStatus::Stopped,
        };
    }
    if let Some(output) = output(item) {
        tool.output = Some(tail(output, MAX_CHAT_TOOL_OUTPUT_BYTES));
    }
    tool.status = status;
    let now = now_ms();
    if !status.is_open() {
        tool.ended_ms.get_or_insert(now);
    }
    settle(&mut tool, was, now);
    inner.end_thought();
    inner.put_tool(tool);
    inner.activity_ms = now;
    true
}

/// `value` as a card's input: pretty JSON, its start when long.
fn input_text(value: &Value) -> String {
    head(
        serde_json::to_string_pretty(value).unwrap_or_default(),
        MAX_CHAT_TOOL_INPUT_BYTES,
    )
}

/// Fills in a card from a tool-like item: its name, kind, title, input, files and
/// changes; false for an item that is no tool call.
fn describe(tool: &mut ChatTool, item: &Value) -> bool {
    let text = |key: &str| {
        item[key]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    match item["type"].as_str().unwrap_or_default() {
        "commandExecution" => describe_command(
            tool,
            item["command"].as_str().unwrap_or_default(),
            &item["commandActions"],
            &item["cwd"],
        ),
        "fileChange" => describe_changes(tool, &item["changes"]),
        "mcpToolCall" => {
            let (server, name) = (
                text("server").unwrap_or("mcp"),
                text("tool").unwrap_or("tool"),
            );
            tool.name = Some(format!("mcp__{server}__{name}"));
            tool.kind = "other".into();
            tool.title = format!("{server}: {name}");
            tool.input = (!item["arguments"].is_null()).then(|| input_text(&item["arguments"]));
        }
        "dynamicToolCall" => {
            let name = text("tool").unwrap_or("tool");
            tool.name = Some(name.to_owned());
            tool.kind = "other".into();
            tool.title = match text("namespace") {
                Some(namespace) => format!("{namespace}: {name}"),
                None => name.to_owned(),
            };
            tool.input = (!item["arguments"].is_null()).then(|| input_text(&item["arguments"]));
        }
        "webSearch" => {
            let action = &item["action"];
            let field = |key: &str| {
                action[key]
                    .as_str()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
            };
            tool.name = Some("web_search".into());
            tool.kind = "fetch".into();
            let (title, input) = match action["type"].as_str() {
                Some("openPage") => {
                    let url = field("url").unwrap_or_default();
                    (url.to_owned(), json!({ "url": url }))
                }
                Some("findInPage") => {
                    let pattern = field("pattern").unwrap_or_default();
                    let url = field("url").unwrap_or_default();
                    (
                        format!("{pattern} in {url}"),
                        json!({ "pattern": pattern, "url": url }),
                    )
                }
                _ => {
                    let query = field("query").or_else(|| text("query")).unwrap_or_default();
                    (query.to_owned(), json!({ "query": query }))
                }
            };
            tool.title = if title.trim().is_empty() {
                "Search the web".to_owned()
            } else {
                title
            };
            tool.input = Some(input_text(&input));
        }
        "imageView" => {
            let path = text("path").unwrap_or_default();
            tool.name = Some("view_image".into());
            tool.kind = "read".into();
            tool.title = format!("View {path}");
            tool.locations = vec![path.to_owned()];
        }
        "imageGeneration" => {
            tool.name = Some("image_generation".into());
            tool.kind = "other".into();
            tool.title = "Generate an image".into();
            tool.input = text("revisedPrompt").map(str::to_owned);
            tool.locations = text("savedPath").map(str::to_owned).into_iter().collect();
        }
        "contextCompaction" => {
            tool.name = Some("compact".into());
            tool.kind = "other".into();
            tool.title = "Compact the conversation".into();
        }
        _ => return false,
    }
    true
}

/// What a finished or running item reported, when it says.
fn output(item: &Value) -> Option<String> {
    match item["type"].as_str()? {
        "commandExecution" => item["aggregatedOutput"].as_str().map(str::to_owned),
        "mcpToolCall" => item["error"]["message"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| {
                let texts: Vec<&str> = item["result"]["content"]
                    .as_array()?
                    .iter()
                    .filter_map(|block| match block["type"].as_str() {
                        Some("text") => block["text"].as_str(),
                        Some("image") => Some("[image]"),
                        _ => None,
                    })
                    .collect();
                (!texts.is_empty()).then(|| texts.join("\n"))
            }),
        "dynamicToolCall" => {
            let texts: Vec<&str> = item["contentItems"]
                .as_array()?
                .iter()
                .filter_map(|content| match content["type"].as_str() {
                    Some("inputText") => content["text"].as_str(),
                    Some("inputImage") => Some("[image]"),
                    _ => None,
                })
                .collect();
            (!texts.is_empty()).then(|| texts.join("\n"))
        }
        "imageGeneration" => item["failure"]["message"]
            .as_str()
            .or_else(|| item["savedPath"].as_str())
            .map(str::to_owned),
        _ => None,
    }
}

/// A command's card: the command as Codex parsed it, run, or read or searched when that
/// is all it does.
pub(super) fn describe_command(tool: &mut ChatTool, command: &str, actions: &Value, cwd: &Value) {
    let line = command_line(command, actions);
    let actions: Vec<&Value> = actions.as_array().into_iter().flatten().collect();
    let all = |kinds: &[&str]| {
        !actions.is_empty()
            && actions.iter().all(|action| {
                action["type"]
                    .as_str()
                    .is_some_and(|kind| kinds.contains(&kind))
            })
    };
    tool.name = Some("shell".into());
    tool.kind = if all(&["read"]) {
        "read"
    } else if all(&["read", "listFiles", "search"]) {
        "search"
    } else {
        "execute"
    }
    .to_owned();
    tool.locations = actions
        .iter()
        .filter(|action| action["type"] == "read")
        .filter_map(|action| action["path"].as_str().map(str::to_owned))
        .collect();
    let mut input = json!({ "command": line });
    if let Some(cwd) = cwd.as_str() {
        input["cwd"] = cwd.into();
    }
    tool.input = Some(input_text(&input));
    tool.title = line;
}

/// A command as people read it: the commands Codex parsed out of it, else the command
/// without the shell it runs in.
pub(super) fn command_line(command: &str, actions: &Value) -> String {
    let parsed: Vec<&str> = actions
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|action| action["command"].as_str())
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .collect();
    if !parsed.is_empty() {
        return parsed.join(" && ");
    }
    unwrap_shell(command).trim().to_owned()
}

/// `bash -lc 'ls'` (or zsh, sh, PowerShell's `-Command`) as `ls`; any other command as
/// it is.
fn unwrap_shell(command: &str) -> &str {
    for flag in [" -lc ", " -c ", " -Command "] {
        let Some((shell, rest)) = command.split_once(flag) else {
            continue;
        };
        let program = shell
            .trim_matches('"')
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .trim_end_matches(".exe")
            .to_ascii_lowercase();
        if !matches!(
            program.as_str(),
            "bash" | "zsh" | "sh" | "pwsh" | "powershell"
        ) {
            continue;
        }
        let rest = rest.trim();
        for quote in ['\'', '"'] {
            if let Some(inner) = rest
                .strip_prefix(quote)
                .and_then(|rest| rest.strip_suffix(quote))
            {
                return inner;
            }
        }
        return rest;
    }
    command
}

/// A file change's card: an edit of its files, each change a diff.
fn describe_changes(tool: &mut ChatTool, changes: &Value) {
    let changes: Vec<&Value> = changes.as_array().into_iter().flatten().collect();
    tool.name = Some("apply_patch".into());
    let deletes = !changes.is_empty()
        && changes
            .iter()
            .all(|change| change["kind"]["type"] == "delete");
    tool.kind = if deletes { "delete" } else { "edit" }.to_owned();
    tool.diffs = changes
        .iter()
        .filter_map(|change| change_diff(change))
        .collect();
    tool.locations = tool.diffs.iter().map(|diff| diff.path.clone()).collect();
    tool.title = match tool.locations.as_slice() {
        [path] => format!("Edit {path}"),
        [] => "Change files".to_owned(),
        paths => format!("Edit {} files", paths.len()),
    };
}

/// One file's change as a diff: an added file's text, a deleted file's, or an update's
/// unified diff split into its sides, its hunks one after another. A moved file shows
/// under its new path.
fn change_diff(change: &Value) -> Option<ChatDiff> {
    let path = change["path"].as_str()?;
    let diff = change["diff"].as_str().unwrap_or_default();
    let side = |text: String| (text.len() <= MAX_CHAT_DIFF_BYTES).then_some(text);
    let kind = &change["kind"];
    Some(match kind["type"].as_str() {
        Some("add") => ChatDiff {
            path: path.to_owned(),
            old_text: None,
            new_text: side(diff.to_owned()),
        },
        Some("delete") => ChatDiff {
            path: path.to_owned(),
            old_text: side(diff.to_owned()),
            new_text: Some(String::new()),
        },
        _ => {
            let (old_text, new_text) = match diff_sides(diff) {
                Some((old, new)) => (side(old), side(new)),
                None => (None, None),
            };
            ChatDiff {
                path: kind["move_path"].as_str().unwrap_or(path).to_owned(),
                old_text,
                new_text,
            }
        }
    })
}

/// The old and new sides of a unified diff's hunks, a `⋯` line between hunks; `None`
/// when it has none.
fn diff_sides(diff: &str) -> Option<(String, String)> {
    let mut old = String::new();
    let mut new = String::new();
    let mut hunks = 0;
    for line in diff.split_inclusive('\n') {
        if line.starts_with("@@") {
            if hunks > 0 {
                old.push_str("⋯\n");
                new.push_str("⋯\n");
            }
            hunks += 1;
            continue;
        }
        if hunks == 0 {
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => new.push_str(&line[1..]),
            Some(b'-') => old.push_str(&line[1..]),
            Some(b' ') => {
                old.push_str(&line[1..]);
                new.push_str(&line[1..]);
            }
            // `\ No newline at end of file`.
            Some(b'\\') => {}
            _ => {
                old.push_str(line);
                new.push_str(line);
            }
        }
    }
    (hunks > 0).then_some((old, new))
}

/// More output of a command or file change.
pub(super) fn output_delta(inner: &mut Inner, item: &str, delta: &str) {
    if delta.is_empty() || !inner.tools.contains_key(item) {
        return;
    }
    let mut tool = inner.tool_call(item);
    let mut output = tool.output.take().unwrap_or_default();
    output.push_str(delta);
    tool.output = Some(tail(output, MAX_CHAT_TOOL_OUTPUT_BYTES));
    inner.put_tool(tool);
    inner.activity_ms = now_ms();
}

/// A file change's changes as Codex has them now.
pub(super) fn patch_updated(inner: &mut Inner, item: &str, changes: &Value) {
    if !inner.tools.contains_key(item) {
        return;
    }
    let mut tool = inner.tool_call(item);
    describe_changes(&mut tool, changes);
    inner.put_tool(tool);
}

/// An MCP call's progress: its message shows until it reports.
pub(super) fn progress(inner: &mut Inner, item: &str, message: &str) {
    if message.trim().is_empty() || !inner.tools.contains_key(item) {
        return;
    }
    let mut tool = inner.tool_call(item);
    if tool.status.is_open() {
        tool.output = Some(tail(message.to_owned(), MAX_CHAT_TOOL_OUTPUT_BYTES));
        inner.put_tool(tool);
    }
}

/// The turn's plan, one card per turn, updated in place: its steps checked off as they
/// are done.
pub(super) fn plan_updated(inner: &mut Inner, params: &Value) {
    let turn = params["turnId"].as_str().unwrap_or_default();
    let steps: Vec<(&str, &str)> = params["plan"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|step| {
            let status = step["status"].as_str().unwrap_or("pending");
            Some((step["step"].as_str()?, status))
        })
        .collect();
    let mut tool = inner.tool_call(&format!("plan-{turn}"));
    let done = steps
        .iter()
        .filter(|(_, status)| *status == "completed")
        .count();
    tool.name = Some("update_plan".into());
    tool.kind = "think".into();
    tool.title = format!("Plan · {done} of {} done", steps.len());
    let mut input = json!({ "plan": params["plan"] });
    if let Some(explanation) = params["explanation"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
    {
        input["explanation"] = explanation.into();
    }
    tool.input = Some(input_text(&input));
    tool.output = Some(
        steps
            .iter()
            .map(|(step, status)| {
                let mark = match *status {
                    "completed" => "[x]",
                    "inProgress" => "[>]",
                    _ => "[ ]",
                };
                format!("{mark} {step}")
            })
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let now = now_ms();
    tool.status = ChatToolStatus::Completed;
    tool.ended_ms = Some(now);
    inner.end_thought();
    inner.put_tool(tool);
    inner.activity_ms = now;
}

/// A thread Codex started: a sub-agent's, its nickname and role kept for its card.
pub(super) fn thread_started(inner: &mut Inner, thread: &Value) {
    let (Some(id), Some(_)) = (thread["id"].as_str(), thread["parentThreadId"].as_str()) else {
        return;
    };
    let text = |key: &str| {
        thread[key]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let (nickname, role) = (text("agentNickname"), text("agentRole"));
    if let Some(role) = role.clone() {
        update_agent(inner, id, |agent, _| {
            if agent.agent.is_empty() {
                agent.agent = role;
            }
        });
    }
    inner
        .codex()
        .nicknames
        .insert(id.to_owned(), (nickname, role));
}

/// How a sub-agent's status shows.
fn agent_status(status: &str) -> Option<ChatToolStatus> {
    Some(match status {
        "pendingInit" => ChatToolStatus::Pending,
        "running" => ChatToolStatus::Running,
        "completed" => ChatToolStatus::Completed,
        "errored" | "notFound" => ChatToolStatus::Failed,
        "interrupted" | "shutdown" => ChatToolStatus::Stopped,
        _ => return None,
    })
}

/// Sets a sub-agent's status while it is open; one that ends has run since it began.
fn set_status(agent: &mut ChatAgent, status: ChatToolStatus, now: u64) {
    if !agent.status.is_open() || agent.status == status {
        return;
    }
    agent.status = status;
    if !status.is_open()
        && agent.duration_ms == 0
        && let Some(started) = agent.started_ms
    {
        agent.duration_ms = now.saturating_sub(started);
    }
}

/// Changes the sub-agent of thread `thread` on its call's card.
fn update_agent(inner: &mut Inner, thread: &str, change: impl FnOnce(&mut ChatAgent, &mut Child)) {
    let Some((call, name)) = inner
        .codex_state()
        .children
        .get(thread)
        .map(|child| (child.call.clone(), child.agent.clone()))
    else {
        return;
    };
    if !inner.tools.contains_key(&call) {
        return;
    }
    let now = now_ms();
    let mut tool = inner.tool_call(&call);
    let was = tool.status;
    let Some(agent) = tool.agents.iter_mut().find(|agent| agent.id == name) else {
        return;
    };
    let Some(child) = inner.codex().children.get_mut(thread) else {
        return;
    };
    change(agent, child);
    settle(&mut tool, was, now);
    inner.put_tool(tool);
}

/// `name`, else with a number, so no other sub-agent in the transcript or on `tool` has
/// it.
fn agent_name(inner: &Inner, tool: &ChatTool, name: &str) -> String {
    let taken = |candidate: &str| {
        tool.agents.iter().any(|agent| agent.id == candidate)
            || inner.state.entries.iter().any(|entry| match entry {
                ChatEntry::Tool(shown) => shown.agents.iter().any(|agent| agent.id == candidate),
                _ => false,
            })
    };
    let name = short_line(name).unwrap_or_else(|| "Agent".to_owned());
    if !taken(&name) {
        return name;
    }
    (2..)
        .map(|number| format!("{name} ({number})"))
        .find(|candidate| !taken(candidate))
        .expect("some number is free")
}

/// Adds the sub-agent of thread `thread` to `tool`, unless it is on a card already.
fn add_agent(
    inner: &mut Inner,
    tool: &mut ChatTool,
    thread: &str,
    fallback: &str,
    prompt: Option<&str>,
    model: Option<&str>,
) {
    if inner.codex_state().children.contains_key(thread) {
        return;
    }
    let (nickname, role) = inner
        .codex_state()
        .nicknames
        .get(thread)
        .cloned()
        .unwrap_or_default();
    let name = agent_name(inner, tool, nickname.as_deref().unwrap_or(fallback));
    let mut agent = new_agent(&name);
    agent.agent = role.unwrap_or_default();
    agent.status = ChatToolStatus::Running;
    agent.started_ms = Some(now_ms());
    agent.activity = prompt.and_then(short_line);
    agent.assignment = prompt.map(|prompt| head(prompt.to_owned(), MAX_CHAT_TOOL_INPUT_BYTES));
    agent.model = model.map(str::to_owned);
    tool.agents.push(agent);
    inner.codex().children.insert(
        thread.to_owned(),
        Child {
            call: tool.id.clone(),
            agent: name,
            steps: VecDeque::new(),
            reply: None,
        },
    );
}

/// A call of Codex's sub-agent tools: `spawnAgent` gets a card with the agents it
/// started; the others (messages, waits, closes) a card of their own. Each reports
/// the states of the agents it names.
fn collab(inner: &mut Inner, item: &Value, ended: bool, replay: bool) {
    let Some(id) = item["id"].as_str() else {
        return;
    };
    let kind = item["tool"].as_str().unwrap_or_default();
    let prompt = item["prompt"]
        .as_str()
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty());
    let receivers: Vec<&str> = item["receiverThreadIds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut tool = inner.tool_call(id);
    let was = tool.status;
    tool.kind = "other".into();
    tool.input = prompt.map(|prompt| head(prompt.to_owned(), MAX_CHAT_TOOL_INPUT_BYTES));
    if kind == "spawnAgent" {
        tool.name = Some("task".into());
        tool.title = prompt
            .and_then(short_line)
            .unwrap_or_else(|| "Start a sub-agent".to_owned());
        for receiver in &receivers {
            add_agent(
                inner,
                &mut tool,
                receiver,
                "Agent",
                prompt,
                item["model"].as_str(),
            );
        }
    } else {
        let codex = inner.codex_state();
        let names: Vec<&str> = receivers
            .iter()
            .filter_map(|receiver| codex.children.get(*receiver))
            .map(|child| child.agent.as_str())
            .collect();
        let names = if names.is_empty() {
            "a sub-agent".to_owned()
        } else {
            names.join(", ")
        };
        tool.name = Some(format!("agent_{kind}"));
        tool.title = match kind {
            "wait" => format!("Wait for {names}"),
            "sendInput" | "sendMessage" | "followupTask" => format!("Message {names}"),
            "resumeAgent" => format!("Resume {names}"),
            "closeAgent" => format!("Close {names}"),
            "interruptAgent" => format!("Stop {names}"),
            "listAgents" => "List sub-agents".to_owned(),
            other => other.to_owned(),
        };
        tool.waits_on_agents = kind == "wait";
    }
    let mut status = item_status(item, inner.codex_state().interrupting);
    if status.is_open() && ended {
        status = if replay {
            ChatToolStatus::Stopped
        } else {
            ChatToolStatus::Completed
        };
    }
    tool.status = status;
    let now = now_ms();
    if !status.is_open() {
        tool.ended_ms.get_or_insert(now);
    }
    settle(&mut tool, was, now);
    inner.end_thought();
    inner.put_tool(tool);
    inner.activity_ms = now;
    agent_states(inner, &item["agentsStates"]);
}

/// The sub-agents' states a collab call reports, by thread: status, and the answer of
/// one that ended.
fn agent_states(inner: &mut Inner, states: &Value) {
    let Some(states) = states.as_object() else {
        return;
    };
    for (thread, state) in states {
        let Some(status) = state["status"].as_str().and_then(agent_status) else {
            continue;
        };
        let message = state["message"]
            .as_str()
            .map(str::trim)
            .filter(|message| !message.is_empty());
        let now = now_ms();
        update_agent(inner, thread, |agent, _| {
            set_status(agent, status, now);
            if !status.is_open()
                && let Some(message) = message
            {
                agent.result = Some(head(message.to_owned(), MAX_CHAT_AGENT_RESULT_BYTES));
            }
        });
    }
}

/// A sub-agent began or ended, as the chat's thread reports it: one no call started
/// gets a card of its own.
fn sub_agent_activity(inner: &mut Inner, item: &Value) {
    let (Some(id), Some(thread)) = (item["id"].as_str(), item["agentThreadId"].as_str()) else {
        return;
    };
    let now = now_ms();
    match item["kind"].as_str() {
        Some("started") if !inner.codex_state().children.contains_key(thread) => {
            let path = item["agentPath"].as_str().unwrap_or_default();
            let fallback = path.rsplit(['/', '\\']).next().unwrap_or(path);
            let mut tool = inner.tool_call(id);
            let was = tool.status;
            tool.name = Some("task".into());
            tool.kind = "other".into();
            tool.title = short_line(path).unwrap_or_else(|| "Sub-agent".to_owned());
            add_agent(
                inner,
                &mut tool,
                thread,
                fallback,
                None,
                item["model"].as_str(),
            );
            tool.status = ChatToolStatus::Completed;
            settle(&mut tool, was, now);
            inner.end_thought();
            inner.put_tool(tool);
        }
        Some("interrupted") => {
            update_agent(inner, thread, |agent, _| {
                set_status(agent, ChatToolStatus::Stopped, now);
            });
        }
        Some("completed") => update_agent(inner, thread, |agent, child| {
            set_status(agent, ChatToolStatus::Completed, now);
            if agent.result.is_none() {
                agent.result = child.reply.clone();
            }
        }),
        _ => {}
    }
}

/// A sub-agent's turn began, or ended (`ended`): how it ended, with its last reply.
pub(super) fn child_turn(inner: &mut Inner, thread: &str, turn: &Value, ended: bool) {
    let now = now_ms();
    let status = if !ended {
        ChatToolStatus::Running
    } else {
        match turn["status"].as_str() {
            Some("failed") => ChatToolStatus::Failed,
            Some("interrupted") => ChatToolStatus::Stopped,
            _ => ChatToolStatus::Completed,
        }
    };
    update_agent(inner, thread, |agent, child| {
        if !ended {
            // A sub-agent sent more work runs again.
            if !agent.status.is_open() {
                agent.status = ChatToolStatus::Running;
            }
            set_status(agent, status, now);
            return;
        }
        set_status(agent, status, now);
        if agent.result.is_none() {
            agent.result = child.reply.clone();
        }
    });
}

/// A sub-agent's item began or finished: a call is one of its steps, its replies
/// what it answers.
pub(super) fn child_item(inner: &mut Inner, thread: &str, item: &Value, ended: bool) {
    let Some(id) = item["id"].as_str() else {
        return;
    };
    if item["type"] == "agentMessage" {
        let Some(text) = item["text"]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        else {
            return;
        };
        if ended {
            update_agent(inner, thread, |agent, child| {
                child.reply = Some(head(text.to_owned(), MAX_CHAT_AGENT_RESULT_BYTES));
                if agent.status.is_open() {
                    agent.activity = short_line(text);
                }
            });
        }
        return;
    }
    let Some(text) = step_text(item) else {
        return;
    };
    let status = match item_status(item, false) {
        status if status.is_open() && ended => ChatToolStatus::Completed,
        status => status,
    };
    update_agent(inner, thread, |agent, child| {
        if let Some(step) = child
            .steps
            .iter()
            .position(|step| step == id)
            .and_then(|step| agent.steps.get_mut(step))
        {
            step.status = status;
            return;
        }
        if agent.steps.len() >= MAX_CHAT_AGENT_STEPS {
            agent.steps.remove(0);
            child.steps.pop_front();
        }
        child.steps.push_back(id.to_owned());
        agent.steps.push(ChatAgentStep {
            text: text.clone(),
            status,
        });
        agent.tool_count += 1;
        if agent.status.is_open() {
            agent.activity = Some(text);
        }
    });
}

/// A sub-agent's call in one short line; `None` for an item that is no call.
fn step_text(item: &Value) -> Option<String> {
    let mut tool = ChatTool {
        id: String::new(),
        name: None,
        title: String::new(),
        kind: String::new(),
        status: ChatToolStatus::Running,
        input: None,
        output: None,
        diffs: Vec::new(),
        locations: Vec::new(),
        started_ms: 0,
        ended_ms: None,
        agents: Vec::new(),
        waits_on_agents: false,
    };
    if !describe(&mut tool, item) {
        return None;
    }
    short_line(&tool.title)
}
