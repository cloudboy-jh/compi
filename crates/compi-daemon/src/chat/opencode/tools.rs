//! What both of OpenCode's servers report, as the transcript shows it: replies and
//! reasoning streamed into text entries, tool calls as cards (commands, reads,
//! searches, edits with their diffs), the todo list as a card per turn, and sub-agents
//! on the card of the call that started them, their sessions' calls as their steps.
use super::super::codex::items::diff_sides;
use super::super::{Inner, head, new_agent, now_ms, settle, short_line, tail};
use super::AskKind;
use compi_protocol::chat::{
    ChatAgentStep, ChatDiff, ChatEntry, ChatEvent, ChatTool, ChatToolStatus,
    MAX_CHAT_AGENT_RESULT_BYTES, MAX_CHAT_AGENT_STEPS, MAX_CHAT_DIFF_BYTES,
    MAX_CHAT_TOOL_INPUT_BYTES, MAX_CHAT_TOOL_OUTPUT_BYTES,
};
use serde_json::{Value, json};
use std::collections::VecDeque;

/// Tools whose calls show no card: the todo list shows as one of its own.
const HIDDEN: [&str; 2] = ["todowrite", "todoread"];

/// A reply or reasoning being streamed: its entry and its text so far.
pub(super) struct Stream {
    entry: usize,
    text: String,
}

/// A sub-agent's session: the call whose card shows it, its name there, its steps'
/// call IDs in the card's order and its latest reply.
pub(super) struct Child {
    pub(super) call: String,
    agent: String,
    steps: VecDeque<String>,
    reply: Option<String>,
}

/// Whether text is a reply or a thought.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Text {
    Reply,
    Thought,
}

/// Streamed text of `key` (a part, or a message's block).
pub(super) fn delta(inner: &mut Inner, key: &str, kind: Text, delta: &str) {
    if delta.is_empty() {
        return;
    }
    let shown = inner.opencode().streams.get(key).map(|stream| stream.entry);
    match shown {
        Some(entry) => {
            if let Some(stream) = inner.opencode().streams.get_mut(key) {
                stream.text.push_str(delta);
            }
            inner.emit(ChatEvent::Text {
                index: entry,
                text: delta.to_owned(),
            });
        }
        None => {
            let entry = new_text(inner, kind, delta.to_owned(), None);
            inner.opencode().streams.insert(
                key.to_owned(),
                Stream {
                    entry,
                    text: delta.to_owned(),
                },
            );
        }
    }
    if kind == Text::Reply {
        inner.activity_ms = now_ms();
    }
}

/// The text of `key` so far, whole: what was not streamed yet streams.
pub(super) fn so_far(inner: &mut Inner, key: &str, kind: Text, text: &str) {
    let streamed = inner
        .opencode_state()
        .streams
        .get(key)
        .map_or(0, |stream| stream.text.len());
    if let Some(rest) = text.get(streamed..)
        && text.is_char_boundary(streamed)
    {
        delta(inner, key, kind, rest);
    }
}

/// A new reply or thought entry holding `text`; a thought is open unless `ended_ms`.
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

/// Finished text of `key`: it replaces what was streamed when they differ, or shows as
/// an entry of its own; a thought ends.
pub(super) fn done(inner: &mut Inner, key: &str, kind: Text, text: &str) {
    let Some(stream) = inner.opencode().streams.remove(key) else {
        if !text.trim().is_empty() {
            let ended_ms = (kind == Text::Thought).then(now_ms);
            new_text(inner, kind, text.to_owned(), ended_ms);
        }
        return;
    };
    let text = (!text.trim().is_empty() && text != stream.text).then(|| text.to_owned());
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

/// A tool call as a server reports it.
pub(super) struct Report<'a> {
    /// Its call ID.
    pub(super) id: &'a str,
    pub(super) name: &'a str,
    pub(super) input: &'a Value,
    pub(super) metadata: &'a Value,
    pub(super) status: ChatToolStatus,
    /// What it returned, or its error.
    pub(super) output: Option<String>,
    /// A call a resumed session replays.
    pub(super) replay: bool,
}

/// A call of the chat's session: its card, begun, updated or finished.
pub(super) fn call(inner: &mut Inner, report: &Report<'_>) {
    if HIDDEN.contains(&report.name) {
        return;
    }
    let mut tool = inner.tool_call(report.id);
    let was = tool.status;
    describe(&mut tool, report.name, report.input, report.metadata);
    let mut status = report.status;
    if status.is_open() {
        let asked = inner
            .opencode_state()
            .asks
            .iter()
            .any(|ask| match &ask.kind {
                AskKind::Permission(shown) => shown.tool.as_deref() == Some(report.id),
                AskKind::Question(..) => false,
            });
        status = if report.replay {
            ChatToolStatus::Stopped
        } else if asked {
            ChatToolStatus::Pending
        } else {
            ChatToolStatus::Running
        };
    }
    // A call stopped with its turn stays stopped, whatever it reports after.
    if was == ChatToolStatus::Stopped {
        status = ChatToolStatus::Stopped;
    }
    if let Some(output) = &report.output
        && !output.is_empty()
    {
        tool.output = Some(tail(output.clone(), MAX_CHAT_TOOL_OUTPUT_BYTES));
    }
    tool.status = status;
    let now = now_ms();
    if !status.is_open() {
        tool.ended_ms.get_or_insert(now);
    }
    if is_agent_call(report.name) {
        agents(inner, &mut tool, report);
    }
    settle(&mut tool, was, now);
    inner.end_thought();
    inner.put_tool(tool);
    inner.activity_ms = now;
}

fn is_agent_call(name: &str) -> bool {
    matches!(name, "task" | "subagent")
}

/// `value` as a card's input: pretty JSON, its start when long.
fn input_text(value: &Value) -> String {
    head(
        serde_json::to_string_pretty(value).unwrap_or_default(),
        MAX_CHAT_TOOL_INPUT_BYTES,
    )
}

/// Fills in a card from a call: its name, kind, title, input, files and changes.
pub(super) fn describe(tool: &mut ChatTool, name: &str, input: &Value, metadata: &Value) {
    let text = |key: &str| {
        input[key]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    let path = text("filePath").or_else(|| text("path"));
    tool.name = Some(name.to_owned());
    if !input.is_null() && input.as_object().is_none_or(|input| !input.is_empty()) {
        tool.input = Some(input_text(input));
    }
    match name {
        "bash" | "shell" => {
            tool.kind = "execute".into();
            if let Some(command) = text("command") {
                tool.title = command.to_owned();
            } else if tool.title.is_empty() {
                tool.title = "Run a command".into();
            }
        }
        "read" => {
            tool.kind = "read".into();
            if let Some(path) = path {
                tool.title = format!("Read {path}");
                tool.locations = vec![path.to_owned()];
            }
        }
        "write" | "edit" | "multiedit" | "patch" | "apply_patch" => {
            tool.kind = "edit".into();
            let diffs = edit_diffs(name, input, metadata, path);
            if !diffs.is_empty() {
                tool.diffs = diffs;
            }
            if let Some(path) = path {
                tool.locations = vec![path.to_owned()];
            } else if !tool.diffs.is_empty() {
                tool.locations = tool.diffs.iter().map(|diff| diff.path.clone()).collect();
            }
            tool.title = match tool.locations.as_slice() {
                [path] if name == "write" => format!("Write {path}"),
                [path] => format!("Edit {path}"),
                [] => "Change files".to_owned(),
                paths => format!("Edit {} files", paths.len()),
            };
        }
        "glob" | "grep" | "list" | "codesearch" | "search" => {
            tool.kind = "search".into();
            let what = text("pattern")
                .or_else(|| text("query"))
                .or(path)
                .unwrap_or(name);
            tool.title = match text("path").filter(|_| text("pattern").is_some()) {
                Some(path) => format!("{what} in {path}"),
                None => what.to_owned(),
            };
        }
        "webfetch" => {
            tool.kind = "fetch".into();
            tool.title = text("url").unwrap_or("Fetch a page").to_owned();
        }
        "websearch" => {
            tool.kind = "fetch".into();
            tool.title = text("query").unwrap_or("Search the web").to_owned();
        }
        "task" | "subagent" => {
            tool.kind = "other".into();
            tool.title = text("description")
                .or_else(|| text("prompt"))
                .and_then(short_line)
                .unwrap_or_else(|| "Start a sub-agent".to_owned());
        }
        "question" => {
            tool.kind = "other".into();
            let asked: Vec<&str> = input["questions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|question| question["question"].as_str())
                .collect();
            tool.title = match asked.as_slice() {
                [] => "Ask you".to_owned(),
                asked => format!("Ask: {}", asked.join(" · ")),
            };
        }
        "execute" => {
            tool.kind = "execute".into();
            tool.title = "Run code".into();
        }
        _ => {
            tool.kind = "other".into();
            tool.title = name.to_owned();
        }
    }
}

/// An edit's changes: OpenCode's unified diffs (1.x `metadata.diff`, 2.x
/// `metadata.files`), else the call's own texts.
fn edit_diffs(name: &str, input: &Value, metadata: &Value, path: Option<&str>) -> Vec<ChatDiff> {
    let files: Vec<ChatDiff> = metadata["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|file| {
            let patch = file["patch"].as_str()?;
            let shown = file["file"].as_str()?;
            // A single file's own path is the one the call names.
            let path = path.unwrap_or(shown);
            patch_diff(path, patch, file["status"] == "added")
        })
        .collect();
    if !files.is_empty() {
        return files;
    }
    let Some(path) = path else {
        return Vec::new();
    };
    if let Some(patch) = metadata["diff"]
        .as_str()
        .or_else(|| metadata["filediff"]["patch"].as_str())
        && let Some(diff) = patch_diff(path, patch, false)
    {
        return vec![diff];
    }
    let side = |text: &str| (text.len() <= MAX_CHAT_DIFF_BYTES).then(|| text.to_owned());
    match (
        name,
        input["content"].as_str(),
        input["oldString"].as_str(),
        input["newString"].as_str(),
    ) {
        ("write", Some(content), ..) => vec![ChatDiff {
            path: path.to_owned(),
            old_text: None,
            new_text: side(content),
        }],
        (_, _, Some(old), Some(new)) => vec![ChatDiff {
            path: path.to_owned(),
            old_text: side(old),
            new_text: side(new),
        }],
        _ => Vec::new(),
    }
}

/// One file's unified diff as its two sides; an added file has no old side.
pub(super) fn patch_diff(path: &str, patch: &str, added: bool) -> Option<ChatDiff> {
    let side = |text: String| (text.len() <= MAX_CHAT_DIFF_BYTES).then_some(text);
    let (old, new) = diff_sides(patch)?;
    let added = added || patch.contains("@@ -0,0 ");
    Some(ChatDiff {
        path: path.to_owned(),
        old_text: if added { None } else { side(old) },
        new_text: side(new),
    })
}

/// The sub-agent a `task` (1.x) or `subagent` (2.x) call started, once its session is
/// known, and how it ended once the call did.
fn agents(inner: &mut Inner, tool: &mut ChatTool, report: &Report<'_>) {
    let session = report.metadata["sessionId"]
        .as_str()
        .or_else(|| report.metadata["sessionID"].as_str());
    let text = |key: &str| {
        report.input[key]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    if let Some(session) = session
        && !inner.opencode_state().children.contains_key(session)
        && tool.agents.is_empty()
    {
        let kind = text("subagent_type")
            .or_else(|| text("agent"))
            .unwrap_or("task");
        let name = agent_name(inner, tool, text("description").unwrap_or(kind));
        let mut agent = new_agent(&name);
        agent.agent = kind.to_owned();
        agent.status = if report.replay {
            ChatToolStatus::Stopped
        } else {
            ChatToolStatus::Running
        };
        agent.started_ms = Some(now_ms());
        agent.assignment =
            text("prompt").map(|prompt| head(prompt.to_owned(), MAX_CHAT_TOOL_INPUT_BYTES));
        agent.activity = text("prompt").and_then(short_line);
        let model = &report.metadata["model"];
        agent.model = model["modelID"]
            .as_str()
            .or_else(|| model["id"].as_str())
            .map(str::to_owned);
        tool.agents.push(agent);
        inner.opencode().children.insert(
            session.to_owned(),
            Child {
                call: tool.id.clone(),
                agent: name,
                steps: VecDeque::new(),
                reply: None,
            },
        );
    }
    if report.status.is_open() && !report.replay {
        return;
    }
    // The call ended: so did its sub-agent, with its last reply, else the call's answer.
    let reply = session.and_then(|session| {
        inner
            .opencode_state()
            .children
            .get(session)
            .and_then(|child| child.reply.clone())
    });
    let answer = reply.or_else(|| report.output.as_deref().and_then(task_result));
    let status = match report.status {
        ChatToolStatus::Failed => ChatToolStatus::Failed,
        ChatToolStatus::Completed => ChatToolStatus::Completed,
        _ => ChatToolStatus::Stopped,
    };
    let now = now_ms();
    for agent in tool
        .agents
        .iter_mut()
        .filter(|agent| agent.status.is_open())
    {
        end_agent(agent, status, now);
        if agent.result.is_none() {
            agent.result.clone_from(&answer);
        }
    }
}

/// What a sub-agent's call answered, without the tags it comes in
/// (`<task_result>…</task_result>`, `<subagent …>…</subagent>`).
fn task_result(output: &str) -> Option<String> {
    let inner = match output.split_once("<task_result>") {
        Some((_, rest)) => rest.split("</task_result>").next().unwrap_or(rest),
        None => {
            let rest = output
                .trim()
                .strip_prefix("<subagent")
                .and_then(|rest| rest.split_once('>'))
                .map_or(output, |(_, rest)| rest);
            rest.split("</subagent>").next().unwrap_or(rest)
        }
    };
    let text = inner.trim();
    (!text.is_empty()).then(|| head(text.to_owned(), MAX_CHAT_AGENT_RESULT_BYTES))
}

/// Ends a sub-agent that is open: it ran since it began.
fn end_agent(agent: &mut compi_protocol::chat::ChatAgent, status: ChatToolStatus, now: u64) {
    if !agent.status.is_open() {
        return;
    }
    agent.status = status;
    if agent.duration_ms == 0
        && let Some(started) = agent.started_ms
    {
        agent.duration_ms = now.saturating_sub(started);
    }
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

/// A session a sub-agent's session started: its calls show on the same card.
pub(super) fn adopt(inner: &mut Inner, session: &str, parent: &str) {
    let opencode = inner.opencode();
    let Some(child) = opencode.children.get(parent) else {
        return;
    };
    let adopted = Child {
        call: child.call.clone(),
        agent: child.agent.clone(),
        steps: VecDeque::new(),
        reply: None,
    };
    opencode
        .children
        .entry(session.to_owned())
        .or_insert(adopted);
}

/// Changes the sub-agent of session `session` on its call's card.
fn update_agent(
    inner: &mut Inner,
    session: &str,
    change: impl FnOnce(&mut compi_protocol::chat::ChatAgent, &mut Child),
) {
    let Some((call, name)) = inner
        .opencode_state()
        .children
        .get(session)
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
    let Some(child) = inner.opencode().children.get_mut(session) else {
        return;
    };
    change(agent, child);
    settle(&mut tool, was, now);
    inner.put_tool(tool);
}

/// A call of a sub-agent's session: one of its steps.
pub(super) fn child_call(inner: &mut Inner, session: &str, report: &Report<'_>) {
    if HIDDEN.contains(&report.name) {
        return;
    }
    let mut shown = inner.tool_call(report.id);
    describe(&mut shown, report.name, report.input, report.metadata);
    let Some(text) = short_line(&shown.title) else {
        return;
    };
    let status = report.status;
    let id = report.id.to_owned();
    update_agent(inner, session, |agent, child| {
        if let Some(step) = child
            .steps
            .iter()
            .position(|step| *step == id)
            .and_then(|step| agent.steps.get_mut(step))
        {
            step.status = status;
            step.text = text;
            return;
        }
        if agent.steps.len() >= MAX_CHAT_AGENT_STEPS {
            agent.steps.remove(0);
            child.steps.pop_front();
        }
        child.steps.push_back(id);
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

/// A sub-agent's finished reply: what it answers, unless it says more later.
pub(super) fn child_reply(inner: &mut Inner, session: &str, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    update_agent(inner, session, |agent, child| {
        child.reply = Some(head(text.to_owned(), MAX_CHAT_AGENT_RESULT_BYTES));
        if agent.status.is_open() {
            agent.activity = short_line(text);
        }
    });
}

/// A sub-agent's session ended its turn: `status` says how.
pub(super) fn child_ended(inner: &mut Inner, session: &str, status: ChatToolStatus) {
    let now = now_ms();
    update_agent(inner, session, |agent, child| {
        end_agent(agent, status, now);
        if agent.result.is_none() {
            agent.result.clone_from(&child.reply);
        }
    });
}

/// The session's todo list: one card per turn, updated in place, its items checked off
/// as they are done.
pub(super) fn todos(inner: &mut Inner, todos: &Value) {
    let items: Vec<(&str, &str)> = todos
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|todo| {
            Some((
                todo["content"].as_str()?,
                todo["status"].as_str().unwrap_or("pending"),
            ))
        })
        .collect();
    let opencode = inner.opencode();
    let card = match &opencode.todo_card {
        Some(card) => card.clone(),
        None if items.is_empty() => return,
        None => {
            opencode.todo_cards += 1;
            let card = format!("todo-{}", opencode.todo_cards);
            opencode.todo_card = Some(card.clone());
            card
        }
    };
    let done = items
        .iter()
        .filter(|(_, status)| *status == "completed")
        .count();
    let mut tool = inner.tool_call(&card);
    tool.name = Some("todowrite".into());
    tool.kind = "think".into();
    tool.title = format!("Todos · {done} of {} done", items.len());
    tool.input = Some(input_text(&json!({ "todos": todos })));
    tool.output = Some(
        items
            .iter()
            .map(|(item, status)| {
                let mark = match *status {
                    "completed" => "[x]",
                    "in_progress" => "[>]",
                    "cancelled" => "[-]",
                    _ => "[ ]",
                };
                format!("{mark} {item}")
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

/// A prompt a resumed session replays, its images by name.
pub(super) fn replay_prompt(inner: &mut Inner, text: String, images: Vec<String>) {
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
