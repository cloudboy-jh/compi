//! The transcript's rows: one per entry. Messages are Markdown views; a thought is a dim
//! line that unfolds to its text; a tool call is a card that folds, with its edits as
//! inline diffs, and the permission it waits on drawn on it.
use super::*;
use crate::diff::{self, FileDiff, LineKind, Whitespace};
use compi_protocol::chat::{ChatTool, ChatToolStatus};

/// An edit is unfolded at first while it changes at most this many lines.
const SMALL_EDIT_LINES: u32 = 30;
/// Diff lines a card draws at most; Open in diff shows the rest.
const MAX_DIFF_LINES: usize = 400;
/// How tall a tool's input or output gets before it scrolls.
const TOOL_TEXT_MAX_HEIGHT: f32 = 240.0;
/// Gap under a row, and under a tool card followed by another.
const ROW_GAP: f32 = 12.0;
const PACKED_GAP: f32 = 2.0;

/// One transcript entry as drawn: cheap to clone into the transcript's row renderer,
/// with what the reader unfolded.
#[derive(Clone)]
pub(super) enum ChatRow {
    User {
        text: Entity<TextViewState>,
        files: Rc<[SharedString]>,
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
    },
    Error(SharedString),
    Cancelled,
}

impl ChatRow {
    /// The Markdown view streamed text goes to.
    pub(super) fn text_view(&self) -> Option<&Entity<TextViewState>> {
        match self {
            Self::User { text, .. } | Self::Agent(text) | Self::Thought { text, .. } => Some(text),
            Self::Tool { .. } | Self::Error(_) | Self::Cancelled => None,
        }
    }

    /// Folds or unfolds a tool card or a thought.
    pub(super) fn toggle(&mut self) {
        match self {
            Self::Thought { open, .. } => *open = !*open,
            Self::Tool { tool, open } => *open = Some(!open.unwrap_or_else(|| tool.opens())),
            Self::User { .. } | Self::Agent(_) | Self::Error(_) | Self::Cancelled => {}
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
    fn new(tool: &ChatTool, folder: &str, before: Option<&ToolRow>) -> Self {
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
        Self {
            target: tool_target(tool, folder),
            tool: tool.clone(),
            verb,
            icon,
            diffs,
            added,
            removed,
        }
    }

    /// Unfolded until the reader folds it: a small edit, or a call that failed.
    fn opens(&self) -> bool {
        self.tool.status == ChatToolStatus::Failed
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
        (ChatEntry::Error { .. }, ChatEntry::Error { .. })
        | (ChatEntry::Cancelled, ChatEntry::Cancelled) => was == now,
        _ => false,
    }
}

fn markdown(text: &str, cx: &mut App) -> Entity<TextViewState> {
    cx.new(|cx| TextViewState::markdown(text, cx))
}

pub(super) fn chat_row(entry: &ChatEntry, folder: &str, cx: &mut App) -> ChatRow {
    match entry {
        ChatEntry::User { text, files } => ChatRow::User {
            text: markdown(text, cx),
            files: files
                .iter()
                .map(|file| SharedString::from(relative_to(file, folder)))
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
            tool: Rc::new(ToolRow::new(tool, folder, None)),
            open: None,
        },
        ChatEntry::Error { message } => ChatRow::Error(message.clone().into()),
        ChatEntry::Cancelled => ChatRow::Cancelled,
    }
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
            *tool = Rc::new(ToolRow::new(now, folder, Some(tool)));
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
        (ChatRow::User { files, .. }, ChatEntry::User { files: now, .. }) => {
            *files = now
                .iter()
                .map(|file| SharedString::from(relative_to(file, folder)))
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
    /// The terminal's font, for paths, commands and code.
    pub(super) mono: SharedString,
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

/// Row `index` of `rows`, with the gap under it: tool cards in a run pack tightly.
pub(super) fn render_row(rows: &[ChatRow], index: usize, frame: &RowFrame) -> AnyElement {
    let Some(row) = rows.get(index) else {
        return div().into_any_element();
    };
    let packed = matches!(row, ChatRow::Tool { .. })
        && matches!(rows.get(index + 1), Some(ChatRow::Tool { .. }));
    let last = index + 1 == rows.len();
    let body = match row {
        ChatRow::User { text, files } => render_user(text, files, frame),
        ChatRow::Agent(text) => chat_text(text, frame.colors.foreground, frame),
        ChatRow::Thought {
            text,
            started_ms,
            ended_ms,
            open,
        } => render_thought(index, text, *started_ms, *ended_ms, *open, frame),
        ChatRow::Tool { tool, open } => {
            render_tool(index, tool, open.unwrap_or_else(|| tool.opens()), frame)
        }
        ChatRow::Error(message) => render_error(message, last && frame.retry, frame),
        ChatRow::Cancelled => div()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .text_color(frame.muted())
            .child("Stopped")
            .into_any_element(),
    };
    div()
        .w_full()
        .pb(px(if packed { PACKED_GAP } else { ROW_GAP }))
        .child(body)
        .into_any_element()
}

/// A user or agent message as Markdown: selectable, links open, code highlighted with a
/// Copy button.
fn chat_text(view: &Entity<TextViewState>, tint: u32, frame: &RowFrame) -> AnyElement {
    TextView::new(view)
        .selectable(true)
        .code_block_actions(|block, _, _| {
            let code = block.code();
            icon_button("copy-code", IconName::Copy, "Copy").on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
            })
        })
        .text_size(px(UI_BODY_TEXT_SIZE))
        .text_color(color(modal_text_color(tint, &frame.colors)))
        .into_any_element()
}

/// A file attached to a prompt, or about to be.
pub(super) fn file_chip(label: SharedString, frame_colors: &ThemeColors) -> Div {
    div()
        .flex_none()
        .max_w(px(260.0))
        .h(px(20.0))
        .px_1p5()
        .flex()
        .items_center()
        .gap_1()
        .rounded(px(4.0))
        .bg(color(frame_colors.surface_hover))
        .text_size(px(UI_MICRO_TEXT_SIZE))
        .child(controls::glyph(
            IconName::Paperclip,
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
    frame: &RowFrame,
) -> AnyElement {
    div()
        .w_full()
        .px_3()
        .py_2()
        .flex()
        .flex_col()
        .gap_1p5()
        .rounded(px(6.0))
        .bg(color(frame.colors.surface))
        .child(chat_text(text, frame.colors.foreground, frame))
        .when(!files.is_empty(), |message| {
            message.child(
                div().flex().flex_wrap().gap_1().children(
                    files
                        .iter()
                        .map(|file| file_chip(file.clone(), &frame.colors)),
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
                .text_size(px(UI_SMALL_TEXT_SIZE))
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
                .on_click(
                    frame.act(move |this, chat, _, cx| this.toggle_chat_row(chat, index, cx)),
                ),
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
        .text_size(px(UI_SMALL_TEXT_SIZE))
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
        .text_size(px(UI_MICRO_TEXT_SIZE))
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

/// A tool call: one line (icon, verb, target, changes, duration, status) that unfolds to
/// its edits, or its input and output; the permission it waits on is drawn on it.
fn render_tool(index: usize, row: &ToolRow, open: bool, frame: &RowFrame) -> AnyElement {
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
    let (verb, target) = match &row.target {
        Some(target) => (SharedString::from(row.verb), Some(target.clone())),
        None => (SharedString::from(tool.title.clone()), None),
    };
    let head = div()
        .id(("chat-tool", index))
        .h(px(24.0))
        .px_1()
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(4.0))
        .cursor_pointer()
        .hover(|style| style.bg(color(frame.colors.surface_hover)))
        .text_size(px(UI_SMALL_TEXT_SIZE))
        .child(controls::glyph(
            row.icon,
            if failed {
                color(frame.colors.error)
            } else {
                muted
            },
        ))
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
                    .text_size(px(UI_MICRO_TEXT_SIZE))
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
                    .text_size(px(UI_MICRO_TEXT_SIZE))
                    .text_color(muted)
                    .child(duration),
            )
        })
        .child(div().flex_none().child(tool_status(tool.status, frame)))
        .on_click(frame.act(move |this, chat, _, cx| this.toggle_chat_row(chat, index, cx)));
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
        .when(open, |card| card.child(render_tool_body(index, row, frame)))
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
        .rounded(px(6.0))
        .bg(color(frame.colors.surface))
        .child(
            div()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .child(permission.title.clone()),
        )
        // The subject is the command or path in full; one the title already shows is dropped.
        .when_some(
            subject.filter(|subject| subject.trim() != permission.title.trim()),
            |block, subject| {
                block.child(
                    div()
                        .font_family(frame.mono.clone())
                        .text_size(px(UI_MICRO_TEXT_SIZE))
                        .child(subject),
                )
            },
        )
        .child(div().flex().flex_wrap().gap_2().children(options))
        .into_any_element()
}
/// An unfolded tool call: its edits as diffs, else its input; then its output.
fn render_tool_body(index: usize, row: &ToolRow, frame: &RowFrame) -> AnyElement {
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
        .when_some(
            tool.input.clone().filter(|_| row.diffs.is_empty()),
            |body, input| {
                body.child(tool_text(
                    "Input",
                    ("chat-tool-input", index),
                    input,
                    false,
                    frame,
                ))
            },
        )
        .when_some(tool.output.clone(), |body, output| {
            body.child(tool_text(
                "Output",
                ("chat-tool-output", index),
                output,
                tool.status == ChatToolStatus::Failed,
                frame,
            ))
        })
        .into_any_element()
}

/// A tool's input or output: a labelled block of monospaced text that scrolls once tall.
fn tool_text(
    label: &'static str,
    id: (&'static str, usize),
    text: String,
    error: bool,
    frame: &RowFrame,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .text_size(px(UI_MICRO_TEXT_SIZE))
                .text_color(frame.muted())
                .child(label),
        )
        .child(
            div()
                .id(id)
                .max_h(px(TOOL_TEXT_MAX_HEIGHT))
                .overflow_y_scroll()
                .p_2()
                .rounded(px(4.0))
                .bg(color(frame.colors.surface))
                .font_family(frame.mono.clone())
                .text_size(px(UI_MICRO_TEXT_SIZE))
                .when(error, |block| block.text_color(color(frame.colors.error)))
                .child(text),
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
                            .w(px(14.0))
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
        .rounded(px(4.0))
        .border_1()
        .border_color(color(frame.colors.border))
        .overflow_hidden()
        .child(
            div()
                .h(px(28.0))
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
                        .text_size(px(UI_MICRO_TEXT_SIZE))
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
                .text_size(px(UI_MICRO_TEXT_SIZE))
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
        }
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
                files: Vec::new()
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
}
