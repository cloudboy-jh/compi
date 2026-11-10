//! The transcript's rows: one per entry. Messages are Markdown views; a thought is a dim
//! line that unfolds to its text; a tool call is a card that folds, with its edits as
//! inline diffs, its sub-agents as a line each, and the permission it waits on drawn on
//! it.
use super::*;
use crate::diff::{self, FileDiff, LineKind, Whitespace};
use compi_protocol::chat::{ChatAgent, ChatAgentsFinished, ChatTool, ChatToolStatus};
use gpui_component::text::TextViewStyle;

/// An edit is unfolded at first while it changes at most this many lines.
const SMALL_EDIT_LINES: u32 = 30;
/// Diff lines a card draws at most; Open in diff shows the rest.
const MAX_DIFF_LINES: usize = 400;
/// How tall a tool's input or output gets before it scrolls.
const TOOL_TEXT_MAX_HEIGHT: f32 = 240.0;
/// Gap under a row, and under a tool card followed by another.
const ROW_GAP: f32 = 12.0;
const PACKED_GAP: f32 = 2.0;
/// How long a code block's Copy button shows its check after a copy.
const COPIED_FOR: std::time::Duration = std::time::Duration::from_secs(2);

/// One transcript entry as drawn: cheap to clone into the transcript's row renderer,
/// with what the reader unfolded.
#[derive(Clone)]
pub(super) enum ChatRow {
    User {
        text: Entity<TextViewState>,
        files: Rc<[SharedString]>,
        images: Rc<[SharedString]>,
    },
    Agent(Entity<TextViewState>),
    Thought {
        text: Entity<TextViewState>,
        started_ms: u64,
        ended_ms: Option<u64>,
        open: bool,
    },
    Tool {
        tool: Rc<ToolRow>,
        /// The reader's fold, else the card's own (`ToolRow::opens`).
        open: Option<bool>,
        /// The sub-agents whose result the reader unfolded, by ID.
        open_agents: Vec<String>,
    },
    /// The chat's sub-agents all finished; `tool` is the entry of their call.
    AgentsFinished {
        tool: usize,
        label: SharedString,
    },
    Error(SharedString),
    Cancelled,
}

impl ChatRow {
    /// The Markdown view streamed text goes to.
    pub(super) fn text_view(&self) -> Option<&Entity<TextViewState>> {
        match self {
            Self::User { text, .. } | Self::Agent(text) | Self::Thought { text, .. } => Some(text),
            Self::Tool { .. } | Self::AgentsFinished { .. } | Self::Error(_) | Self::Cancelled => {
                None
            }
        }
    }

    /// Folds or unfolds a tool card or a thought.
    pub(super) fn toggle(&mut self) {
        match self {
            Self::Thought { open, .. } => *open = !*open,
            Self::Tool { tool, open, .. } => *open = Some(!open.unwrap_or_else(|| tool.opens())),
            Self::User { .. }
            | Self::Agent(_)
            | Self::AgentsFinished { .. }
            | Self::Error(_)
            | Self::Cancelled => {}
        }
    }

    /// Unfolds a tool card.
    pub(super) fn unfold(&mut self) {
        if let Self::Tool { open, .. } = self {
            *open = Some(true);
        }
    }

    /// Shows or hides the result of a tool card's sub-agent `agent`.
    pub(super) fn toggle_agent(&mut self, agent: &str) {
        if let Self::Tool { open_agents, .. } = self {
            match open_agents.iter().position(|open| open == agent) {
                Some(index) => {
                    open_agents.remove(index);
                }
                None => open_agents.push(agent.to_owned()),
            }
        }
    }
}

/// A tool call as its card draws it.
pub(super) struct ToolRow {
    tool: ChatTool,
    verb: &'static str,
    icon: IconName,
    /// What it works on: a path (relative to the chat's folder), a command, a query.
    target: Option<SharedString>,
    diffs: Vec<DiffBlock>,
    /// Lines its edits add and remove.
    added: u32,
    removed: u32,
    /// Its input (unless it shows edits or sub-agents) and output as selectable code
    /// blocks.
    input: Option<Entity<TextViewState>>,
    output: Option<Entity<TextViewState>>,
    agents: Vec<AgentRow>,
    /// What its sub-agents cost together, when they reported it.
    cost: Option<f64>,
}

/// A sub-agent as its line draws it, with its result as a Markdown view.
struct AgentRow {
    agent: ChatAgent,
    result: Option<Entity<TextViewState>>,
}

/// One file an edit changed, as the card draws it.
#[derive(Clone)]
struct DiffBlock {
    /// As the harness named it.
    path: String,
    /// Relative to the chat's folder when inside it.
    label: SharedString,
    /// `None` when a side was too large to keep.
    lines: Option<Rc<[DiffText]>>,
    /// Lines past `MAX_DIFF_LINES`.
    more: usize,
    added: u32,
    removed: u32,
}

enum DiffText {
    Line(LineKind, SharedString),
    /// Unchanged lines not shown.
    Fold(u32),
}

impl ToolRow {
    fn new(tool: &ChatTool, folder: &str, before: Option<&ToolRow>, cx: &mut App) -> Self {
        let (verb, icon) = tool_kind(tool);
        // An update with the same edits keeps the diffs already built.
        let diffs = match before.filter(|before| before.tool.diffs == tool.diffs) {
            Some(before) => before.diffs.clone(),
            None => tool
                .diffs
                .iter()
                .map(|edit| DiffBlock::new(edit, folder))
                .collect(),
        };
        let added = diffs.iter().map(|block| block.added).sum();
        let removed = diffs.iter().map(|block| block.removed).sum();
        // A card with edits shows them in place of its input, and in place of its output
        // unless the edit failed or changed nothing, when the output says why. A card with
        // sub-agents shows them in place of both, unless it failed.
        let agents = !tool.agents.is_empty();
        let input = tool.input.as_ref().filter(|_| diffs.is_empty() && !agents);
        let output = tool.output.as_ref().filter(|_| {
            tool.status == ChatToolStatus::Failed
                || !agents && (diffs.is_empty() || added + removed == 0)
        });
        let costs: Vec<f64> = tool.agents.iter().filter_map(|agent| agent.cost).collect();
        Self {
            target: tool_target(tool, folder),
            input: tool_view(
                input,
                before.map(|before| (before.tool.input.as_ref(), before.input.as_ref())),
                tool_markdown,
                cx,
            ),
            output: tool_view(
                output,
                before.map(|before| (before.tool.output.as_ref(), before.output.as_ref())),
                tool_markdown,
                cx,
            ),
            agents: tool
                .agents
                .iter()
                .map(|agent| {
                    let shown = before.and_then(|before| {
                        before.agents.iter().find(|row| row.agent.id == agent.id)
                    });
                    AgentRow {
                        result: tool_view(
                            agent.result.as_ref(),
                            shown.map(|row| (row.agent.result.as_ref(), row.result.as_ref())),
                            str::to_owned,
                            cx,
                        ),
                        agent: agent.clone(),
                    }
                })
                .collect(),
            cost: (!costs.is_empty()).then(|| costs.iter().sum()),
            tool: tool.clone(),
            verb,
            icon,
            diffs,
            added,
            removed,
        }
    }

    /// Unfolded until the reader folds it: a small edit, a call that failed, or one that
    /// started sub-agents.
    fn opens(&self) -> bool {
        self.tool.status == ChatToolStatus::Failed
            || !self.agents.is_empty()
            || (!self.diffs.is_empty() && self.added + self.removed <= SMALL_EDIT_LINES)
    }
}

impl DiffBlock {
    fn new(edit: &compi_protocol::chat::ChatDiff, folder: &str) -> Self {
        let label = relative_to(&edit.path, folder).into();
        let Some(new) = &edit.new_text else {
            return Self {
                path: edit.path.clone(),
                label,
                lines: None,
                more: 0,
                added: 0,
                removed: 0,
            };
        };
        let file = FileDiff::new(
            edit.old_text.as_deref().unwrap_or_default(),
            new,
            Whitespace::Exact,
        );
        let rows = diff::rows(&file, &diff::Context::default(), diff::Layout::Unified);
        let more = rows.len().saturating_sub(MAX_DIFF_LINES);
        let lines = rows
            .iter()
            .take(MAX_DIFF_LINES)
            .filter_map(|row| match row {
                diff::Row::Unified { line, .. } => Some(DiffText::Line(
                    line.kind,
                    file.line(line.side, line.ix).to_owned().into(),
                )),
                diff::Row::Fold { hidden, .. } => Some(DiffText::Fold(*hidden)),
                diff::Row::Split { .. } => None,
            })
            .collect();
        Self {
            path: edit.path.clone(),
            label,
            lines: Some(lines),
            more,
            added: file.added(),
            removed: file.removed(),
        }
    }
}

/// `path` relative to `folder` when it lies inside it.
pub(super) fn relative_to(path: &str, folder: &str) -> String {
    const SEPARATORS: [char; 2] = ['/', '\\'];
    path.strip_prefix(folder.trim_end_matches(SEPARATORS))
        .filter(|rest| rest.starts_with(SEPARATORS))
        .map_or(path, |rest| rest.trim_start_matches(SEPARATORS))
        .to_owned()
}

/// The verb and icon of a tool call, by its ACP kind (or its name, for `other`).
fn tool_kind(tool: &ChatTool) -> (&'static str, IconName) {
    match tool.kind.as_str() {
        "read" => ("Read", IconName::FileText),
        "edit" => ("Edit", IconName::FilePen),
        "delete" => ("Delete", IconName::Delete),
        "move" => ("Move", IconName::MoveRight),
        "search" => ("Search", IconName::FileSearch),
        "execute" => ("Run", IconName::Terminal),
        "fetch" => ("Fetch", IconName::Globe),
        "think" => ("Think", IconName::Brain),
        "switch_mode" => ("Mode", IconName::Settings2),
        _ => match tool.name.as_deref() {
            Some("bash" | "shell") => ("Run", IconName::Terminal),
            Some("read") => ("Read", IconName::FileText),
            Some("edit" | "write") => ("Edit", IconName::FilePen),
            Some("grep" | "glob" | "find" | "search") => ("Search", IconName::FileSearch),
            Some("fetch" | "web_search") => ("Fetch", IconName::Globe),
            Some("task") => ("Agents", IconName::Bot),
            _ => ("Tool", IconName::Wrench),
        },
    }
}

/// What a tool call works on: the file it touched, else the command, path, pattern,
/// query or address in its input.
fn tool_target(tool: &ChatTool, folder: &str) -> Option<SharedString> {
    if let Some(location) = tool
        .locations
        .first()
        .or_else(|| tool.diffs.first().map(|edit| &edit.path))
    {
        return Some(relative_to(location, folder).into());
    }
    let input: serde_json::Value = serde_json::from_str(tool.input.as_deref()?).ok()?;
    [
        "command",
        "cmd",
        "path",
        "file_path",
        "filePath",
        "pattern",
        "query",
        "url",
    ]
    .iter()
    .find_map(|key| input.get(*key)?.as_str())
    .map(|value| relative_to(value, folder).into())
}

/// The view of a tool's input or output, or an agent's result, `text` as `source` makes it
/// Markdown: `before`'s view when it showed the same text, or that view given the new
/// text, else a new one.
fn tool_view(
    text: Option<&String>,
    before: Option<(Option<&String>, Option<&Entity<TextViewState>>)>,
    source: fn(&str) -> String,
    cx: &mut App,
) -> Option<Entity<TextViewState>> {
    let text = text?;
    match before {
        Some((shown, Some(view))) if shown == Some(text) => Some(view.clone()),
        Some((_, Some(view))) => {
            let source = source(text);
            view.update(cx, |view, cx| view.set_text(&source, cx));
            Some(view.clone())
        }
        _ => Some(markdown(&source(text), cx)),
    }
}

/// A tool's input or output as one fenced code block: YAML-like when it is a JSON object
/// or list, else the text as it came.
fn tool_markdown(text: &str) -> String {
    let (language, body) = match json_text(text) {
        Some(body) => ("yaml", body),
        None => ("", text.trim_end().to_owned()),
    };
    let longest = body
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}{language}\n{body}\n{fence}")
}

/// A JSON object or list as `key: value` lines: values unquoted and aligned, lists of
/// plain values joined with commas, nested objects and list items (`- `) indented under
/// their key, text of several lines as a `|` block.
fn json_text(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if !(value.is_object() || value.is_array()) {
        return None;
    }
    let mut out = String::new();
    write_nested(&value, 0, &mut out);
    Some(out.trim_end().to_owned())
}

/// An object's fields, or a list's items, at `indent`.
fn write_nested(value: &serde_json::Value, indent: usize, out: &mut String) {
    match value {
        serde_json::Value::Object(map) => write_fields(map, indent, false, out),
        serde_json::Value::Array(items) => {
            for item in items {
                match item {
                    serde_json::Value::Object(map) if !map.is_empty() => {
                        write_fields(map, indent, true, out)
                    }
                    item => {
                        out.push_str(&" ".repeat(indent));
                        out.push('-');
                        write_value(item, indent + 2, out);
                    }
                }
            }
        }
        _ => {}
    }
}

/// An object's `key: value` lines at `indent`; as a list `item`, the first starts `- `.
fn write_fields(
    map: &serde_json::Map<String, serde_json::Value>,
    indent: usize,
    item: bool,
    out: &mut String,
) {
    let width = map
        .iter()
        .filter(|(_, value)| inline_value(value).is_some_and(|text| !text.contains('\n')))
        .map(|(key, _)| key.chars().count())
        .max()
        .unwrap_or(0);
    let body = indent + if item { 2 } else { 0 };
    for (index, (key, value)) in map.iter().enumerate() {
        out.push_str(&" ".repeat(indent));
        if item {
            out.push_str(if index == 0 { "- " } else { "  " });
        }
        out.push_str(key);
        out.push(':');
        if inline_value(value).is_some_and(|text| !text.contains('\n')) {
            out.push_str(&" ".repeat(width - key.chars().count()));
        }
        write_value(value, body + 2, out);
    }
}

/// What follows a key or a list's `-`: an inline value on the line, a `|` block, or the
/// nested fields on the lines under it, at `indent`.
fn write_value(value: &serde_json::Value, indent: usize, out: &mut String) {
    match inline_value(value) {
        Some(text) if text.contains('\n') => {
            out.push_str(" |\n");
            for line in text.lines() {
                out.push_str(&" ".repeat(indent));
                out.push_str(line);
                out.push('\n');
            }
        }
        Some(text) => {
            out.push(' ');
            out.push_str(&text);
            out.push('\n');
        }
        None => {
            out.push('\n');
            write_nested(value, indent, out);
        }
    }
}

/// A value that fits on its key's line: text without quotes, a number, true or false,
/// none, or a list of those (or of one-field objects, by that field) joined with commas.
fn inline_value(value: &serde_json::Value) -> Option<String> {
    use serde_json::Value;
    let scalar = |value: &Value| match value {
        Value::String(text) => Some(text.clone()),
        Value::Bool(_) | Value::Number(_) => Some(value.to_string()),
        Value::Null => Some("none".to_owned()),
        Value::Array(_) | Value::Object(_) => None,
    };
    match value {
        Value::Array(items) if items.is_empty() => Some("none".to_owned()),
        Value::Object(map) if map.is_empty() => Some("none".to_owned()),
        Value::Array(items) => {
            // `[{ "label": "Red" }, { "label": "Blue" }]` reads as `Red, Blue`.
            let field = items.first()?.as_object().and_then(|first| {
                (first.len() == 1)
                    .then(|| first.keys().next().cloned())
                    .flatten()
            });
            items
                .iter()
                .map(|item| match (&field, item) {
                    (Some(key), Value::Object(map)) if map.len() == 1 => {
                        map.get(key).and_then(scalar)
                    }
                    _ => scalar(item),
                })
                .collect::<Option<Vec<_>>>()
                .filter(|items| items.iter().all(|item| !item.contains('\n')))
                .map(|items| items.join(", "))
        }
        _ => scalar(value),
    }
}

/// Whether `now` is `was` as it went on: the same kind, with more text, or the same tool
/// call reported further.
pub(super) fn continues(was: &ChatEntry, now: &ChatEntry) -> bool {
    match (was, now) {
        (ChatEntry::User { text: was, .. }, ChatEntry::User { text: now, .. })
        | (ChatEntry::Agent { text: was }, ChatEntry::Agent { text: now })
        | (ChatEntry::Thought { text: was, .. }, ChatEntry::Thought { text: now, .. }) => {
            now.starts_with(was.as_str())
        }
        (ChatEntry::Tool(was), ChatEntry::Tool(now)) => was.id == now.id,
        (ChatEntry::AgentsFinished(_), ChatEntry::AgentsFinished(_))
        | (ChatEntry::Error { .. }, ChatEntry::Error { .. })
        | (ChatEntry::Cancelled, ChatEntry::Cancelled) => was == now,
        _ => false,
    }
}

fn markdown(text: &str, cx: &mut App) -> Entity<TextViewState> {
    cx.new(|cx| TextViewState::markdown(text, cx))
}

pub(super) fn chat_row(entry: &ChatEntry, folder: &str, cx: &mut App) -> ChatRow {
    match entry {
        ChatEntry::User {
            text,
            files,
            images,
        } => ChatRow::User {
            text: markdown(text, cx),
            files: files
                .iter()
                .map(|file| SharedString::from(relative_to(file, folder)))
                .collect(),
            images: images
                .iter()
                .map(|name| SharedString::from(name.clone()))
                .collect(),
        },
        ChatEntry::Agent { text } => ChatRow::Agent(markdown(text, cx)),
        ChatEntry::Thought {
            text,
            started_ms,
            ended_ms,
        } => ChatRow::Thought {
            text: markdown(text, cx),
            started_ms: *started_ms,
            ended_ms: *ended_ms,
            open: false,
        },
        ChatEntry::Tool(tool) => ChatRow::Tool {
            tool: Rc::new(ToolRow::new(tool, folder, None, cx)),
            open: None,
            open_agents: Vec::new(),
        },
        ChatEntry::AgentsFinished(summary) => ChatRow::AgentsFinished {
            tool: summary.tool,
            label: agents_finished_label(summary),
        },
        ChatEntry::Error { message } => ChatRow::Error(message.clone().into()),
        ChatEntry::Cancelled => ChatRow::Cancelled,
    }
}

/// A summary entry's line: `2 agents finished · 15s · $0.20`.
fn agents_finished_label(summary: &ChatAgentsFinished) -> SharedString {
    let mut label = format!(
        "{} {} finished · {}",
        summary.count,
        if summary.count == 1 {
            "agent"
        } else {
            "agents"
        },
        duration_label(summary.duration_ms)
    );
    if let Some(cost) = summary.cost {
        label.push_str(" · ");
        label.push_str(&cost_label(cost, None));
    }
    label.into()
}

/// The sub-agents in `entries` that have not finished, each with the entry of its call.
pub(super) fn running_agents(entries: &[ChatEntry]) -> Vec<(usize, &ChatAgent)> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| match entry {
            ChatEntry::Tool(tool) => Some((index, &tool.agents)),
            _ => None,
        })
        .flat_map(|(index, agents)| {
            agents
                .iter()
                .filter(|agent| agent.status.is_open())
                .map(move |agent| (index, agent))
        })
        .collect()
}

/// How long a sub-agent has run: counting while it runs, else as it reported.
pub(super) fn agent_time(agent: &ChatAgent) -> Option<String> {
    let ms = match agent.started_ms {
        Some(started) if agent.status.is_open() => now_ms().saturating_sub(started),
        _ => agent.duration_ms,
    };
    (ms > 0).then(|| duration_label(ms))
}

/// Brings `row`, drawn from `was`, up to `now`: views keep their text (grown, or set
/// anew), and what the reader unfolded stays.
pub(super) fn update_row(
    row: &mut ChatRow,
    was: &ChatEntry,
    now: &ChatEntry,
    folder: &str,
    cx: &mut App,
) {
    let text = |entry: &ChatEntry| match entry {
        ChatEntry::User { text, .. }
        | ChatEntry::Agent { text }
        | ChatEntry::Thought { text, .. } => Some(text.clone()),
        _ => None,
    };
    let same_kind = std::mem::discriminant(was) == std::mem::discriminant(now);
    match (&mut *row, now) {
        (ChatRow::Tool { tool, .. }, ChatEntry::Tool(now)) if tool.tool.id == now.id => {
            *tool = Rc::new(ToolRow::new(now, folder, Some(tool), cx));
        }
        (
            ChatRow::Thought {
                started_ms,
                ended_ms,
                ..
            },
            ChatEntry::Thought {
                started_ms: started,
                ended_ms: ended,
                ..
            },
        ) => {
            *started_ms = *started;
            *ended_ms = *ended;
        }
        (
            ChatRow::User { files, images, .. },
            ChatEntry::User {
                files: now,
                images: now_images,
                ..
            },
        ) => {
            *files = now
                .iter()
                .map(|file| SharedString::from(relative_to(file, folder)))
                .collect();
            *images = now_images
                .iter()
                .map(|name| SharedString::from(name.clone()))
                .collect();
        }
        (ChatRow::Agent(_), ChatEntry::Agent { .. }) => {}
        _ => {
            *row = chat_row(now, folder, cx);
            return;
        }
    }
    if let (true, Some(view), Some(shown), Some(full)) =
        (same_kind, row.text_view(), text(was), text(now))
    {
        if let Some(more) = full.strip_prefix(shown.as_str()) {
            if !more.is_empty() {
                view.update(cx, |view, cx| view.push_str(more, cx));
            }
        } else {
            view.update(cx, |view, cx| view.set_text(&full, cx));
        }
    }
}

/// What every row of one draw needs.
pub(super) struct RowFrame {
    pub(super) app: WeakEntity<CompiApp>,
    pub(super) chat: Arc<str>,
    pub(super) colors: ThemeColors,
    /// Message text's size at the chat's text size, which headings step up from.
    pub(super) body_size: Pixels,
    /// The terminal's font, for paths, commands and code.
    pub(super) mono: SharedString,
    /// The harness's mark, which sub-agents' lines lead with.
    pub(super) mark: &'static str,
    pub(super) permission: Option<ChatPermission>,
    /// A prompt can be sent: Retry after a failed turn acts.
    pub(super) retry: bool,
    /// Why Open in diff cannot act.
    pub(super) diff_refusal: Option<&'static str>,
}

impl RowFrame {
    fn muted(&self) -> Hsla {
        color(modal_text_color(self.colors.muted, &self.colors))
    }

    /// A click handler running `act` on the app, for chat `chat`.
    fn act(
        &self,
        act: impl Fn(&mut CompiApp, &str, &mut Window, &mut Context<CompiApp>) + 'static,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        let (app, chat) = (self.app.clone(), self.chat.clone());
        move |_, window, cx| {
            app.update(cx, |this, cx| {
                act(this, &chat, window, cx);
                cx.notify();
            })
            .ok();
        }
    }
}

/// A row the transcript leaves out: a wait on sub-agents only, unless it failed. Their
/// call and the agents bar show the same.
fn hidden(row: &ChatRow) -> bool {
    matches!(row, ChatRow::Tool { tool, .. }
        if tool.tool.waits_on_agents && tool.tool.status != ChatToolStatus::Failed)
}

/// Row `index` of `rows`, with the gap under it: tool cards in a run pack tightly.
pub(super) fn render_row(rows: &[ChatRow], index: usize, frame: &RowFrame) -> AnyElement {
    let Some(row) = rows.get(index) else {
        return div().into_any_element();
    };
    if hidden(row) {
        return div().into_any_element();
    }
    let next = rows[index + 1..].iter().find(|row| !hidden(row));
    let packed = matches!(row, ChatRow::Tool { .. }) && matches!(next, Some(ChatRow::Tool { .. }));
    let last = next.is_none();
    let body = match row {
        ChatRow::User {
            text,
            files,
            images,
        } => render_user(text, files, images, frame),
        ChatRow::Agent(text) => chat_text(text, frame.colors.foreground, frame),
        ChatRow::Thought {
            text,
            started_ms,
            ended_ms,
            open,
        } => render_thought(index, text, *started_ms, *ended_ms, *open, frame),
        ChatRow::Tool {
            tool,
            open,
            open_agents,
        } => render_tool(
            index,
            tool,
            open.unwrap_or_else(|| tool.opens()),
            open_agents,
            frame,
        ),
        ChatRow::AgentsFinished { tool, label } => {
            render_agents_finished(index, *tool, label, rows, frame)
        }
        ChatRow::Error(message) => render_error(message, last && frame.retry, frame),
        ChatRow::Cancelled => div()
            .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
            .text_color(frame.muted())
            .child("Stopped")
            .into_any_element(),
    };
    div()
        .w_full()
        .pb(ui_rems(if packed { PACKED_GAP } else { ROW_GAP }))
        .child(body)
        .into_any_element()
}

/// A user or agent message as Markdown: selectable, links open, code highlighted with a
/// Copy button.
fn chat_text(view: &Entity<TextViewState>, tint: u32, frame: &RowFrame) -> AnyElement {
    TextView::new(view)
        .selectable(true)
        .code_block_actions(copy_code_button)
        .style(message_style(frame))
        .text_size(ui_rems(UI_BODY_TEXT_SIZE))
        // The window's chrome is medium; reading text is regular, so bold and headings
        // stand out from it.
        .font_weight(FontWeight::NORMAL)
        .text_color(color(modal_text_color(tint, &frame.colors)))
        .into_any_element()
}

/// Messages' Markdown: headings a clear step above the body, and code that scales with
/// the chat's text size like the rest.
fn message_style(frame: &RowFrame) -> TextViewStyle {
    TextViewStyle {
        heading_base_font_size: frame.body_size,
        ..TextViewStyle::default()
    }
    .paragraph_gap(gpui::rems(0.75))
    .heading_font_size(|level, body| {
        body * match level {
            1 => 1.75,
            2 => 1.45,
            3 => 1.25,
            4 => 1.1,
            _ => 1.0,
        }
    })
    .code_block(StyleRefinement::default().text_size(ui_rems(UI_SMALL_TEXT_SIZE)))
}

/// The Copy button in a code block's corner; a check for [`COPIED_FOR`] after a copy.
fn copy_code_button(
    block: &gpui_base::text::CodeBlock,
    window: &mut Window,
    cx: &mut App,
) -> controls::Control {
    use std::hash::{Hash as _, Hasher as _};
    let code = block.code();
    // Keyed by the code, so two blocks of one message keep their own state.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    code.hash(&mut hasher);
    let id = ElementId::NamedInteger("copy-code".into(), hasher.finish());
    let copied = window.use_keyed_state(id.clone(), cx, |_, _| false);
    if *copied.read(cx) {
        return icon_button(id, IconName::Check, "Copied");
    }
    icon_button(id, IconName::Copy, "Copy").on_click(move |_, _, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
        copied.update(cx, |copied, cx| {
            *copied = true;
            cx.notify();
        });
        let copied = copied.clone();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            copied.update(cx, |copied, cx| {
                *copied = false;
                cx.notify();
            });
        })
        .detach();
    })
}

/// A file attached to a prompt, or about to be.
pub(super) fn file_chip(label: SharedString, frame_colors: &ThemeColors) -> Div {
    chip(IconName::Paperclip, label, frame_colors)
}

/// An image sent with a prompt, or about to be.
pub(super) fn image_chip(label: SharedString, frame_colors: &ThemeColors) -> Div {
    chip(IconName::Image, label, frame_colors)
}

fn chip(icon: IconName, label: SharedString, frame_colors: &ThemeColors) -> Div {
    div()
        .flex_none()
        .max_w(ui_rems(260.0))
        .h(ui_rems(20.0))
        .px_1p5()
        .flex()
        .items_center()
        .gap_1()
        .rounded(ui_rems(4.0))
        .bg(color(frame_colors.surface_hover))
        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
        .child(controls::glyph(
            icon,
            color(modal_text_color(frame_colors.muted, frame_colors)),
        ))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(label),
        )
}

fn render_user(
    text: &Entity<TextViewState>,
    files: &[SharedString],
    images: &[SharedString],
    frame: &RowFrame,
) -> AnyElement {
    div()
        .w_full()
        .px_3()
        .py_2()
        .flex()
        .flex_col()
        .gap_1p5()
        .rounded(ui_rems(6.0))
        .bg(color(frame.colors.surface))
        .child(chat_text(text, frame.colors.foreground, frame))
        .when(!files.is_empty() || !images.is_empty(), |message| {
            message.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .children(
                        files
                            .iter()
                            .map(|file| file_chip(file.clone(), &frame.colors)),
                    )
                    .children(
                        images
                            .iter()
                            .map(|name| image_chip(name.clone(), &frame.colors)),
                    ),
            )
        })
        .into_any_element()
}

fn render_thought(
    index: usize,
    text: &Entity<TextViewState>,
    started_ms: u64,
    ended_ms: Option<u64>,
    open: bool,
    frame: &RowFrame,
) -> AnyElement {
    let label = match ended_ms.map(|ended| ended.saturating_sub(started_ms)) {
        // Under a second (a replayed session's thoughts all are) says nothing worth a time.
        Some(took) if took < 1000 => "Thought".to_owned(),
        Some(took) => format!("Thought for {}", duration_label(took)),
        None => format!(
            "Thinking… {}",
            duration_label(now_ms().saturating_sub(started_ms))
        ),
    };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .id(("chat-thought", index))
                .flex()
                .items_center()
                .gap_1p5()
                .cursor_pointer()
                .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
                .text_color(frame.muted())
                .child(controls::glyph(
                    if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    },
                    frame.muted(),
                ))
                .child(label)
                .on_click(frame.act(move |this, chat, _, _| this.toggle_chat_row(chat, index))),
        )
        .when(open, |thought| {
            thought.child(
                div()
                    .pl_4()
                    .child(chat_text(text, frame.colors.muted, frame)),
            )
        })
        .into_any_element()
}

fn render_error(message: &SharedString, retry: bool, frame: &RowFrame) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(color(frame.colors.error))
                .child(message.clone()),
        )
        .when(retry, |row| {
            row.child(
                text_button("chat-retry", "Retry").on_click(
                    frame.act(|this, chat, window, cx| this.retry_chat(chat, window, cx)),
                ),
            )
        })
        .into_any_element()
}

/// A tool call's status: a spinner while it runs, then a check, a cross or a stop mark.
fn tool_status(status: ChatToolStatus, frame: &RowFrame) -> AnyElement {
    let muted = frame.muted();
    match status {
        ChatToolStatus::Pending | ChatToolStatus::Running => Spinner::new()
            .with_size(Size::XSmall)
            .color(muted)
            .into_any_element(),
        ChatToolStatus::Completed => controls::glyph(IconName::Check, muted).into_any_element(),
        ChatToolStatus::Failed => {
            controls::glyph(IconName::CircleX, color(frame.colors.error)).into_any_element()
        }
        ChatToolStatus::Stopped => controls::glyph(IconName::Ban, muted).into_any_element(),
    }
}

/// `+N −M` in the diff colours.
fn change_counts(added: u32, removed: u32, frame: &RowFrame) -> Div {
    div()
        .flex_none()
        .flex()
        .gap_1()
        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
        .font_family(frame.mono.clone())
        .child(
            div()
                .text_color(color(frame.colors.added))
                .child(format!("+{added}")),
        )
        .child(
            div()
                .text_color(color(frame.colors.removed))
                .child(format!("−{removed}")),
        )
}

/// `text`'s first non-blank line, with `…` when more follows.
fn first_line(text: &str) -> SharedString {
    let mut lines = text
        .lines()
        .map(str::trim_end)
        .skip_while(|line| line.trim().is_empty());
    let first = lines.next().unwrap_or_default();
    if lines.any(|line| !line.trim().is_empty()) {
        format!("{first} …").into()
    } else {
        first.to_owned().into()
    }
}

/// A tool call: one line (icon, verb, target, changes, duration, cost, status) that
/// unfolds to its edits, its sub-agents, or its input and output; the permission it
/// waits on is drawn on it.
fn render_tool(
    index: usize,
    row: &ToolRow,
    open: bool,
    open_agents: &[String],
    frame: &RowFrame,
) -> AnyElement {
    let tool = &row.tool;
    let muted = frame.muted();
    let failed = tool.status == ChatToolStatus::Failed;
    let duration = tool
        .ended_ms
        .or_else(|| tool.status.is_open().then(now_ms))
        .map(|end| duration_label(end.saturating_sub(tool.started_ms)));
    let permission = frame
        .permission
        .as_ref()
        .filter(|permission| permission.tool.as_deref() == Some(tool.id.as_str()));
    // The head is one line high: a title or target with line breaks (OMP's `eval` title
    // carries its code) shows its first line, or it draws over the rows below.
    let (verb, target) = match &row.target {
        Some(target) => (SharedString::from(row.verb), Some(first_line(target))),
        None => (first_line(&tool.title), None),
    };
    let head = div()
        .id(("chat-tool", index))
        .h(ui_rems(24.0))
        .px_1()
        .flex()
        .items_center()
        .gap_2()
        .rounded(ui_rems(4.0))
        .cursor_pointer()
        .hover(|style| style.bg(color(frame.colors.surface_hover)))
        .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
        .map(|head| {
            if row.agents.is_empty() {
                head.child(controls::glyph(
                    row.icon,
                    if failed {
                        color(frame.colors.error)
                    } else {
                        muted
                    },
                ))
            } else {
                head.child(agents_mark(
                    frame.mark,
                    row.agents.iter().map(|agent| agent.agent.status),
                    &frame.colors,
                ))
            }
        })
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .map(|verb| match target {
                    Some(_) => verb.flex_none(),
                    None => verb.min_w_0().overflow_hidden().text_ellipsis(),
                })
                .whitespace_nowrap()
                .child(verb),
        )
        .when_some(target, |head, target| {
            head.child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_family(frame.mono.clone())
                    .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                    .text_color(muted)
                    .child(target),
            )
        })
        .child(div().flex_1())
        .when(!row.diffs.is_empty(), |head| {
            head.child(change_counts(row.added, row.removed, frame))
        })
        .when_some(duration, |head, duration| {
            head.child(
                div()
                    .flex_none()
                    .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                    .text_color(muted)
                    .child(duration),
            )
        })
        .when_some(row.cost, |head, cost| {
            head.child(
                div()
                    .flex_none()
                    .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                    .text_color(muted)
                    .child(cost_label(cost, None)),
            )
        })
        .child(div().flex_none().child(tool_status(tool.status, frame)))
        .on_click(frame.act(move |this, chat, _, _| this.toggle_chat_row(chat, index)));
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(head)
        .when_some(permission, |card, permission| {
            let subject = row.target.clone();
            card.child(
                div()
                    .ml_6()
                    .child(render_permission_block(subject, permission, frame)),
            )
        })
        .when(open, |card| {
            card.child(render_tool_body(index, row, open_agents, frame))
        })
        .into_any_element()
}

/// A permission the harness waits on: its title, what the call would run or touch, in
/// full, and the harness's answers: allowing ones as buttons, refusing ones quiet.
pub(super) fn render_permission_block(
    subject: Option<SharedString>,
    permission: &ChatPermission,
    frame: &RowFrame,
) -> AnyElement {
    let mut allowing = true;
    let options: Vec<_> = permission
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let id = option.id.clone();
            let button = text_button(("chat-permission", index), option.name.clone()).header();
            let button = if option.kind.starts_with("reject") {
                button.ghost()
            } else if std::mem::take(&mut allowing) {
                button.primary()
            } else {
                button
            };
            button.on_click(frame.act(move |this, chat, window, cx| {
                this.answer_chat_permission(chat, id.clone(), window, cx);
            }))
        })
        .collect();
    div()
        .p_2()
        .flex()
        .flex_col()
        .gap_2()
        .rounded(ui_rems(6.0))
        .bg(color(frame.colors.surface))
        .child(
            div()
                .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
                .child(permission.title.clone()),
        )
        // The subject is the command or path in full; one the title already shows is dropped.
        .when_some(
            subject.filter(|subject| subject.trim() != permission.title.trim()),
            |block, subject| {
                block.child(
                    div()
                        .font_family(frame.mono.clone())
                        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                        .child(subject),
                )
            },
        )
        .child(div().flex().flex_wrap().gap_2().children(options))
        .into_any_element()
}

/// The summary entry: a thin rule across the transcript closing the agents' run, with
/// ✓ (✕ when one failed), how many, how long and what they cost. A click shows their
/// call.
fn render_agents_finished(
    index: usize,
    tool: usize,
    label: &SharedString,
    rows: &[ChatRow],
    frame: &RowFrame,
) -> AnyElement {
    let muted = frame.muted();
    let failed = matches!(rows.get(tool), Some(ChatRow::Tool { tool, .. })
        if tool.agents.iter().any(|agent| agent.agent.status == ChatToolStatus::Failed));
    let rule = || div().flex_1().h(px(1.0)).bg(color(frame.colors.border));
    div()
        .id(("chat-agents-finished", index))
        .h(ui_rems(24.0))
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
        .text_color(muted)
        .hover(|style| style.text_color(color(frame.colors.foreground)))
        .child(rule())
        .child(if failed {
            controls::glyph(IconName::Close, color(frame.colors.error))
        } else {
            controls::glyph(IconName::Check, color(frame.colors.added))
        })
        .child(div().flex_none().whitespace_nowrap().child(label.clone()))
        .child(rule())
        .on_click(frame.act(move |this, chat, _, _| this.reveal_chat_tool(chat, tool)))
        .into_any_element()
}

/// Most dots the agents' mark draws; more agents show as `+N`.
const MAX_AGENT_DOTS: usize = 8;

/// Sub-agents' mark: a ghost of the harness's own mark (they are more of it), then a dot
/// per agent: a ring while it waits (muted) or runs (accent), filled once it ends: green
/// when done, red when it failed, muted when stopped. Static, so it never repaints by
/// itself.
pub(super) fn agents_mark(
    mark: &'static str,
    statuses: impl IntoIterator<Item = ChatToolStatus>,
    colors: &ThemeColors,
) -> Div {
    let statuses: Vec<ChatToolStatus> = statuses.into_iter().collect();
    let more = statuses.len().saturating_sub(MAX_AGENT_DOTS);
    let ghost = color(modal_text_color(colors.muted, colors)).opacity(0.6);
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(ui_rems(3.0))
        .child(
            gpui::svg()
                .path(mark)
                .flex_none()
                .size(ui_rems(12.0))
                .text_color(ghost),
        )
        .children(statuses.into_iter().take(MAX_AGENT_DOTS).map(|status| {
            let (tint, ring) = match status {
                ChatToolStatus::Pending => (colors.muted, true),
                ChatToolStatus::Running => (colors.accent, true),
                ChatToolStatus::Completed => (colors.added, false),
                ChatToolStatus::Failed => (colors.error, false),
                ChatToolStatus::Stopped => (colors.muted, false),
            };
            let dot = div().flex_none().size(ui_rems(6.0)).rounded_full();
            if ring {
                dot.border_1().border_color(color(tint))
            } else {
                dot.bg(color(tint))
            }
        }))
        .when(more > 0, |mark| {
            mark.child(
                div()
                    .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                    .text_color(color(modal_text_color(colors.muted, colors)))
                    .child(format!("+{more}")),
            )
        })
}

/// An unfolded tool call: its edits as diffs, its sub-agents, else its input; then its
/// output.
fn render_tool_body(
    index: usize,
    row: &ToolRow,
    open_agents: &[String],
    frame: &RowFrame,
) -> AnyElement {
    let tool = &row.tool;
    div()
        .ml_6()
        .flex()
        .flex_col()
        .gap_2()
        .children(
            row.diffs
                .iter()
                .enumerate()
                .map(|(block, diff)| render_diff(index, block, diff, frame)),
        )
        .when(!row.agents.is_empty(), |body| {
            body.child(
                div()
                    .flex()
                    .flex_col()
                    .children(row.agents.iter().enumerate().map(|(position, agent)| {
                        let open = open_agents.contains(&agent.agent.id);
                        render_agent(index, position, agent, open, frame)
                    })),
            )
        })
        .when_some(row.input.as_ref(), |body, input| {
            body.child(tool_text("Input", input, false, frame))
        })
        .when_some(row.output.as_ref(), |body, output| {
            body.child(tool_text(
                "Output",
                output,
                tool.status == ChatToolStatus::Failed,
                frame,
            ))
        })
        .into_any_element()
}

/// A sub-agent: one line (status, name, type, what it does or answered, time, cost) that
/// unfolds, once it answered, to its answer.
fn render_agent(
    index: usize,
    position: usize,
    row: &AgentRow,
    open: bool,
    frame: &RowFrame,
) -> AnyElement {
    let agent = &row.agent;
    let muted = frame.muted();
    let doing = match (&agent.result, agent.status.is_open()) {
        (Some(result), false) => Some(first_line(result)),
        _ => agent.activity.as_deref().map(SharedString::from),
    };
    let micro = |text: SharedString| {
        div()
            .flex_none()
            .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
            .text_color(muted)
            .child(text)
    };
    let id = agent.id.clone();
    let head = div()
        .id(("chat-agent", index * 1000 + position))
        .h(ui_rems(22.0))
        .px_1()
        .flex()
        .items_center()
        .gap_2()
        .rounded(ui_rems(4.0))
        .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
        .child(div().flex_none().child(tool_status(agent.status, frame)))
        .child(
            div()
                .flex_none()
                .font_weight(FontWeight::MEDIUM)
                .whitespace_nowrap()
                .child(agent.id.clone()),
        )
        .when(!agent.agent.is_empty(), |head| {
            head.child(micro(agent.agent.clone().into()))
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                .text_color(muted)
                .children(doing),
        )
        .when_some(agent_time(agent), |head, time| {
            head.child(micro(time.into()))
        })
        .when_some(agent.cost, |head, cost| {
            head.child(micro(cost_label(cost, None).into()))
        })
        .when(row.result.is_some(), |head| {
            head.cursor_pointer()
                .hover(|style| style.bg(color(frame.colors.surface_hover)))
                .on_click(frame.act(move |this, chat, _, _| {
                    this.toggle_chat_agent(chat, index, &id);
                }))
        });
    div()
        .flex()
        .flex_col()
        .child(head)
        .when_some(row.result.as_ref().filter(|_| open), |agent_row, result| {
            agent_row.child(
                div()
                    .ml_6()
                    .pt_0p5()
                    .pb_2()
                    .child(agent_result(agent, result, frame)),
            )
        })
        .into_any_element()
}

/// What a sub-agent answered: a label naming its model and tool calls over the Markdown,
/// selectable, links open.
fn agent_result(agent: &ChatAgent, view: &Entity<TextViewState>, frame: &RowFrame) -> AnyElement {
    let mut label = String::from("Result");
    if let Some(model) = &agent.model {
        label.push_str(" · ");
        label.push_str(model);
    }
    match agent.tool_count {
        0 => {}
        1 => label.push_str(" · 1 tool call"),
        count => label.push_str(&format!(" · {count} tool calls")),
    }
    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                .text_color(frame.muted())
                .child(label),
        )
        .child(
            TextView::new(view)
                .selectable(true)
                .code_block_actions(copy_code_button)
                .style(message_style(frame))
                .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
                .font_weight(FontWeight::NORMAL)
                .text_color(color(modal_text_color(
                    if agent.status == ChatToolStatus::Failed {
                        frame.colors.error
                    } else {
                        frame.colors.foreground
                    },
                    &frame.colors,
                ))),
        )
        .into_any_element()
}

/// A tool's input or output: a label over its code block, selectable with Copy, which
/// scrolls once tall.
fn tool_text(
    label: &'static str,
    view: &Entity<TextViewState>,
    error: bool,
    frame: &RowFrame,
) -> AnyElement {
    let mut code_block = StyleRefinement::default()
        .max_h(ui_rems(TOOL_TEXT_MAX_HEIGHT))
        .text_size(ui_rems(UI_MICRO_TEXT_SIZE));
    code_block.overflow.y = Some(gpui::Overflow::Scroll);
    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                .text_color(frame.muted())
                .child(label),
        )
        .child(
            TextView::new(view)
                .selectable(true)
                .code_block_actions(copy_code_button)
                .style(TextViewStyle::default().code_block(code_block))
                .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                .when(error, |text| text.text_color(color(frame.colors.error))),
        )
        .into_any_element()
}

/// One edited file: its path and changes with Open in diff, then its lines.
fn render_diff(index: usize, block: usize, diff: &DiffBlock, frame: &RowFrame) -> AnyElement {
    let path = diff.path.clone();
    let open = text_button(("chat-open-diff", index * 1000 + block), "Open in diff")
        .ghost()
        .disabled_because(frame.diff_refusal)
        .on_click(frame.act(move |this, chat, window, cx| {
            this.open_chat_diff(chat, &path, window, cx);
        }));
    let added_row = color(frame.colors.added).opacity(0.12);
    let removed_row = color(frame.colors.removed).opacity(0.12);
    let lines = diff.lines.as_ref().map(|lines| {
        lines.iter().map(|line| match line {
            DiffText::Line(kind, text) => {
                let (marker, background) = match kind {
                    LineKind::Added => ("+", Some(added_row)),
                    LineKind::Removed => ("−", Some(removed_row)),
                    LineKind::Context | LineKind::Formatting => (" ", None),
                };
                div()
                    .flex()
                    .px_1()
                    .when_some(background, |line, background| line.bg(background))
                    .child(
                        div()
                            .flex_none()
                            .w(ui_rems(14.0))
                            .text_color(frame.muted())
                            .child(marker),
                    )
                    .child(div().min_w_0().child(text.clone()))
                    .into_any_element()
            }
            DiffText::Fold(hidden) => div()
                .px_1()
                .text_color(frame.muted())
                .child(format!("⋯ {hidden} unchanged lines"))
                .into_any_element(),
        })
    });
    div()
        .flex()
        .flex_col()
        .rounded(ui_rems(4.0))
        .border_1()
        .border_color(color(frame.colors.border))
        .overflow_hidden()
        .child(
            div()
                .h(ui_rems(28.0))
                .pl_2()
                .pr_1()
                .flex()
                .items_center()
                .gap_2()
                .bg(color(frame.colors.surface))
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_family(frame.mono.clone())
                        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                        .child(diff.label.clone()),
                )
                .child(change_counts(diff.added, diff.removed, frame))
                .child(div().flex_1())
                .child(open),
        )
        .child(
            div()
                .py_1()
                .flex()
                .flex_col()
                .font_family(frame.mono.clone())
                .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                .map(|body| match lines {
                    Some(lines) => body.children(lines),
                    None => body.child(
                        div()
                            .px_2()
                            .text_color(frame.muted())
                            .child("Too large to show here"),
                    ),
                })
                .when(diff.more > 0, |body| {
                    body.child(
                        div()
                            .px_2()
                            .text_color(frame.muted())
                            .child(format!("{} more lines in the diff view", diff.more)),
                    )
                }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_head_shows_the_first_line_of_a_title_with_line_breaks() {
        assert_eq!(
            first_line("[py] Listing commands\nimport json\n"),
            "[py] Listing commands …"
        );
        assert_eq!(first_line("\n  \ncargo test\n\n"), "cargo test");
        assert_eq!(first_line("ls -la"), "ls -la");
    }

    fn tool(id: &str, kind: &str, input: Option<&str>) -> ChatTool {
        ChatTool {
            id: id.into(),
            name: None,
            title: "Tool".into(),
            kind: kind.into(),
            status: ChatToolStatus::Running,
            input: input.map(str::to_owned),
            output: None,
            diffs: Vec::new(),
            locations: Vec::new(),
            started_ms: 1,
            ended_ms: None,
            agents: Vec::new(),
            waits_on_agents: false,
        }
    }

    #[test]
    fn running_agents_and_their_summary_read_short() {
        let agent = |id: &str, status: ChatToolStatus| ChatAgent {
            id: id.into(),
            agent: "sonic".into(),
            status,
            activity: None,
            tool_count: 0,
            started_ms: None,
            duration_ms: 12_000,
            cost: None,
            model: None,
            assignment: None,
            result: None,
        };
        let mut task = tool("t", "other", None);
        task.agents = vec![
            agent("TaskA", ChatToolStatus::Completed),
            agent("TaskB", ChatToolStatus::Running),
        ];
        let entries = [
            ChatEntry::Cancelled,
            ChatEntry::Tool(Box::new(task.clone())),
        ];
        let running = running_agents(&entries);
        assert_eq!(running.len(), 1);
        assert_eq!((running[0].0, running[0].1.id.as_str()), (1, "TaskB"));
        assert_eq!(agent_time(&task.agents[0]).as_deref(), Some("12s"));
        let summary = |count, cost| ChatAgentsFinished {
            tool: 1,
            count,
            duration_ms: 15_000,
            cost,
        };
        assert_eq!(
            agents_finished_label(&summary(2, Some(0.2))),
            "2 agents finished · 15s · $0.20"
        );
        assert_eq!(
            agents_finished_label(&summary(1, None)),
            "1 agent finished · 15s"
        );
        let finished = |count| ChatEntry::AgentsFinished(summary(count, None));
        assert!(continues(&finished(2), &finished(2)));
        assert!(!continues(&finished(2), &finished(3)));
    }

    #[test]
    fn entries_that_went_on_keep_their_rows() {
        let agent = |text: &str| ChatEntry::Agent { text: text.into() };
        let call = |id: &str| ChatEntry::Tool(Box::new(tool(id, "read", None)));
        assert!(continues(&agent("Hel"), &agent("Hello")));
        assert!(!continues(&agent("Hello"), &agent("Help")));
        assert!(!continues(
            &agent("Hi"),
            &ChatEntry::User {
                text: "Hi".into(),
                files: Vec::new(),
                images: Vec::new(),
            }
        ));
        assert!(continues(&call("1"), &call("1")));
        assert!(!continues(&call("1"), &call("2")));
        assert!(continues(&ChatEntry::Cancelled, &ChatEntry::Cancelled));
    }

    #[test]
    fn tool_targets_are_paths_in_the_folder_or_the_input() {
        let mut read = tool("1", "read", None);
        read.locations = vec!["/work/src/main.rs".into()];
        assert_eq!(tool_target(&read, "/work").as_deref(), Some("src/main.rs"));
        let run = tool("2", "execute", Some(r#"{ "command": "cargo test" }"#));
        assert_eq!(tool_target(&run, "/work").as_deref(), Some("cargo test"));
        assert_eq!(tool_kind(&run).0, "Run");
        assert_eq!(tool_target(&tool("3", "other", None), "/work"), None);
        assert_eq!(relative_to(r"C:\work\a.rs", r"C:\work\"), "a.rs");
        assert_eq!(relative_to("/elsewhere/a.rs", "/work"), "/elsewhere/a.rs");
    }

    #[test]
    fn json_tool_text_reads_as_aligned_fields() {
        let text = json_text(
            r#"{
                "i": "Asking color preference",
                "questions": [{
                    "id": "color",
                    "question": "Red or blue?",
                    "options": [{ "label": "Red" }, { "label": "Blue" }],
                    "multi": false
                }],
                "command": "ls -la\npwd",
                "env": { "CI": true, "skip": null },
                "tags": []
            }"#,
        );
        // In the harness's order.
        assert_eq!(
            text.as_deref(),
            Some(
                "i:    Asking color preference\n\
                 questions:\n  \
                   - id:       color\n    \
                     question: Red or blue?\n    \
                     options:  Red, Blue\n    \
                     multi:    false\n\
                 command: |\n  \
                   ls -la\n  \
                   pwd\n\
                 env:\n  \
                   CI:   true\n  \
                   skip: none\n\
                 tags: none"
            )
        );
        // Plain text, a lone JSON number and cut-off JSON stay as they are.
        assert_eq!(json_text("total 8"), None);
        assert_eq!(json_text("42"), None);
        assert_eq!(json_text(r#"{"command": "ls"#), None);
        // A fence outlasts any backticks in the text.
        assert_eq!(tool_markdown("a ``` b"), "````\na ``` b\n````");
    }
}
