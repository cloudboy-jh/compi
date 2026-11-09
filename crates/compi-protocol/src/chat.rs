//! Native agent chat: a content pane talking to an installed agent harness over the
//! Agent Client Protocol (ACP). The daemon runs the harness, like a shell, and keeps the
//! transcript; windows watch it (`WatchChat`) and send prompts, cancels and answers.
//! The harness owns its sessions, models, tools and credentials.
use crate::ContentEnvironment;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The longest chat ID a pane keeps.
pub const MAX_CHAT_ID_BYTES: usize = 64;
/// The longest harness session ID a pane keeps.
pub const MAX_CHAT_SESSION_BYTES: usize = 256;
/// The longest prompt one `SendChat` carries.
pub const MAX_CHAT_PROMPT_BYTES: usize = 256 * 1024;
/// Files one prompt may attach.
pub const MAX_CHAT_ATTACHMENTS: usize = 64;
/// Images one prompt may carry, each at most `MAX_CHAT_IMAGE_BYTES` before encoding, so
/// a prompt stays well inside one control frame.
pub const MAX_CHAT_IMAGES: usize = 4;
pub const MAX_CHAT_IMAGE_BYTES: usize = 1536 * 1024;
/// The most of a tool call's input kept for display.
pub const MAX_CHAT_TOOL_INPUT_BYTES: usize = 16 * 1024;
/// The most of a tool call's output kept for display: its end.
pub const MAX_CHAT_TOOL_OUTPUT_BYTES: usize = 64 * 1024;
/// The largest side of an edit's diff kept; a larger edit keeps only its path.
pub const MAX_CHAT_DIFF_BYTES: usize = 256 * 1024;

/// An installed agent program Compi can chat with.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ChatHarness {
    #[default]
    Omp,
}

impl ChatHarness {
    /// The harness's own name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Omp => "OMP",
        }
    }

    /// The program's command name.
    pub fn program(self) -> &'static str {
        match self {
            Self::Omp => "omp",
        }
    }
}

/// A content pane chatting with `harness` in `folder`. The transcript lives in the
/// daemon under `id`; `session` is the harness's own session, kept so the chat can be
/// resumed once the daemon restarts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatContent {
    pub id: String,
    #[serde(default)]
    pub harness: ChatHarness,
    pub environment: ContentEnvironment,
    /// Absolute folder in `environment` the harness works in.
    pub folder: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

impl ChatContent {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || self.id.len() > MAX_CHAT_ID_BYTES
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(format!(
                "chat id {:?} is not 1–{MAX_CHAT_ID_BYTES} letters, digits or '-'",
                self.id
            ));
        }
        if self.folder.is_empty() || self.folder.len() > crate::MAX_DIRECTORY_PATH_BYTES {
            return Err("chat folder is empty or too long".into());
        }
        validate_session(self.session.as_deref())
    }
}

/// A harness session ID as a pane or a resume names it.
pub fn validate_session(session: Option<&str>) -> Result<(), String> {
    if session.is_some_and(|session| session.is_empty() || session.len() > MAX_CHAT_SESSION_BYTES) {
        return Err("chat session id is empty or too long".into());
    }
    Ok(())
}

/// Everything a window needs to draw a chat: sent whole by `ChatWatching`, then kept
/// current by applying each `ChatChanged` event in order.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatState {
    pub harness: ChatHarness,
    /// The harness's session, once it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The session's title, when the harness names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: ChatStatus,
    pub entries: Vec<ChatEntry>,
    /// The harness's settings for this session (model, thinking, mode, …), in its order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ChatOption>,
    /// Slash commands the harness offers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<ChatCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ChatUsage>,
    /// A permission the harness waits on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<ChatPermission>,
    /// A question the harness waits on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<ChatQuestion>,
}

impl ChatState {
    /// Applies one event, as the daemon did before sending it.
    pub fn apply(&mut self, event: ChatEvent) {
        match event {
            ChatEvent::Reset { state } => *self = *state,
            ChatEvent::Entry { entry } => self.entries.push(entry),
            ChatEvent::Text { index, text } => {
                if let Some(
                    ChatEntry::User { text: shown, .. }
                    | ChatEntry::Agent { text: shown }
                    | ChatEntry::Thought { text: shown, .. },
                ) = self.entries.get_mut(index)
                {
                    shown.push_str(&text);
                }
            }
            ChatEvent::Replace { index, entry } => {
                if let Some(shown) = self.entries.get_mut(index) {
                    *shown = entry;
                }
            }
            ChatEvent::Status { status } => self.status = status,
            ChatEvent::Session { session } => self.session = session,
            ChatEvent::Title { title } => self.title = title,
            ChatEvent::Options { options } => self.options = options,
            ChatEvent::Commands { commands } => self.commands = commands,
            ChatEvent::Usage { usage } => self.usage = usage,
            ChatEvent::Permission { permission } => self.permission = permission,
            ChatEvent::Question { question } => self.question = question,
        }
    }

    /// Whether a turn is running.
    pub fn working(&self) -> bool {
        matches!(self.status, ChatStatus::Working { .. })
    }

    /// The setting of `category` (`model`, `thought_level`, `mode`), the first of several.
    pub fn option(&self, category: &str) -> Option<&ChatOption> {
        self.options
            .iter()
            .find(|option| option.category.as_deref() == Some(category))
    }
}

/// Where the harness is.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatStatus {
    /// Starting the program and its session.
    #[default]
    Starting,
    /// Waiting for a prompt.
    Ready,
    /// A turn has run since `since_ms` (milliseconds since the Unix epoch).
    Working { since_ms: u64 },
    /// The program could not start or ended; `RestartChat` tries again.
    Failed { message: String },
}

/// One item of the transcript, in order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEntry {
    /// What was sent, as Markdown, with the files it attached and the names of the
    /// images it carried.
    User {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<String>,
    },
    /// The harness's reply, as Markdown.
    Agent { text: String },
    /// The model's reasoning, when the harness shares it; `ended_ms` once the reply or a
    /// tool call followed.
    Thought {
        text: String,
        started_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ended_ms: Option<u64>,
    },
    /// A tool call.
    Tool(Box<ChatTool>),
    /// A turn failed.
    Error { message: String },
    /// The turn was stopped.
    Cancelled,
}

/// A tool call and what it reported.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatTool {
    pub id: String,
    /// The tool's own name (`bash`, `edit`, …), when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub title: String,
    /// ACP's kind: `read`, `edit`, `delete`, `move`, `search`, `execute`, `think`,
    /// `fetch`, `switch_mode` or `other`.
    pub kind: String,
    pub status: ChatToolStatus,
    /// Its input, pretty-printed JSON, at most `MAX_CHAT_TOOL_INPUT_BYTES`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// Its text output, the end of it when longer than `MAX_CHAT_TOOL_OUTPUT_BYTES`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diffs: Vec<ChatDiff>,
    /// Files it touched, as the harness names them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<String>,
    pub started_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
}

/// One file an edit changed. Texts are absent when a side exceeded
/// `MAX_CHAT_DIFF_BYTES` (`old_text` also when the file is new).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatDiff {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_text: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatToolStatus {
    Pending,
    Running,
    Completed,
    Failed,
    /// Its turn was stopped before it reported an end.
    Stopped,
}

impl ChatToolStatus {
    pub fn is_open(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }
}

/// A setting the harness offers for the session (ACP config option of type select).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatOption {
    pub id: String,
    pub name: String,
    /// ACP's category: `model`, `model_config`, `thought_level`, `mode` or another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub current: String,
    pub choices: Vec<ChatChoice>,
}

impl ChatOption {
    /// The current choice's name.
    pub fn current_name(&self) -> &str {
        self.choices
            .iter()
            .find(|choice| choice.value == self.current)
            .map_or(self.current.as_str(), |choice| choice.name.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatChoice {
    pub value: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The group the harness lists it under, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// A slash command: sent as `/name arguments`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatCommand {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// What its argument is, when it takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Context use, and cost when the harness reports it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatUsage {
    /// Tokens in the context now.
    pub used: u64,
    /// The context window.
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
}

// Cost is a reported figure, never NaN; equality on the bits is what a change check needs.
impl Eq for ChatUsage {}

/// The harness asks before running a tool call; one of `options` answers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatPermission {
    pub id: u64,
    /// The tool call it asks about, which is in the transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub title: String,
    pub options: Vec<ChatPermissionOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatPermissionOption {
    pub id: String,
    pub name: String,
    /// ACP's kind: `allow_once`, `allow_always`, `reject_once` or `reject_always`.
    pub kind: String,
}

/// The harness asks the user something (an ACP form): each field takes choices, text or
/// both. Answered by `AnswerChatQuestion`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatQuestion {
    pub id: u64,
    pub message: String,
    pub fields: Vec<ChatField>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatField {
    pub key: String,
    pub title: String,
    /// Empty for a text field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<ChatChoice>,
    /// Several choices may be picked.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multi: bool,
    /// A choice field may also be answered in words, which replaces the choice.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub other: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
}

/// One field's answer: the picked choice values, or typed text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatAnswer {
    Choices { values: Vec<String> },
    Text { text: String },
}

/// Answers by field key.
pub type ChatAnswers = BTreeMap<String, ChatAnswer>;

/// An image sent with a prompt, its bytes carried along: the window reads it where it
/// was picked, so it needs no path the harness can reach.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatImage {
    /// Its file name, as the transcript shows it.
    pub name: String,
    /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
    pub mime: String,
    /// Standard padded base64.
    pub data: String,
}

impl ChatImage {
    /// The MIME type of an image file the harnesses take, by its extension.
    pub fn mime_for(name: &str) -> Option<&'static str> {
        let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
        Some(match extension.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            _ => return None,
        })
    }
}

/// A session of the harness in the chat's folder, for resuming.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatSessionInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// When it last changed, as the harness reports it (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<u64>,
}

/// A change to a [`ChatState`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    /// Everything changed: a restart or a resumed session.
    Reset {
        state: Box<ChatState>,
    },
    /// A new last entry.
    Entry {
        entry: ChatEntry,
    },
    /// More text for the user, agent or thought entry at `index`.
    Text {
        index: usize,
        text: String,
    },
    /// The entry at `index` changed (a tool call reported, a thought ended).
    Replace {
        index: usize,
        entry: ChatEntry,
    },
    Status {
        status: ChatStatus,
    },
    Session {
        session: Option<String>,
    },
    Title {
        title: Option<String>,
    },
    Options {
        options: Vec<ChatOption>,
    },
    Commands {
        commands: Vec<ChatCommand>,
    },
    Usage {
        usage: Option<ChatUsage>,
    },
    Permission {
        permission: Option<ChatPermission>,
    },
    Question {
        question: Option<ChatQuestion>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(status: ChatToolStatus) -> ChatEntry {
        ChatEntry::Tool(Box::new(ChatTool {
            id: "t".into(),
            name: Some("bash".into()),
            title: "$ ls".into(),
            kind: "execute".into(),
            status,
            input: None,
            output: None,
            diffs: Vec::new(),
            locations: Vec::new(),
            started_ms: 1,
            ended_ms: None,
        }))
    }

    #[test]
    fn streamed_text_joins_its_entry_and_replacements_land_in_place() {
        let mut state = ChatState::default();
        state.apply(ChatEvent::Entry {
            entry: ChatEntry::Agent { text: "Hel".into() },
        });
        state.apply(ChatEvent::Text {
            index: 0,
            text: "lo".into(),
        });
        state.apply(ChatEvent::Entry {
            entry: tool(ChatToolStatus::Pending),
        });
        state.apply(ChatEvent::Replace {
            index: 1,
            entry: tool(ChatToolStatus::Completed),
        });
        // Text aimed at a tool entry is ignored, not appended somewhere else.
        state.apply(ChatEvent::Text {
            index: 1,
            text: "x".into(),
        });
        assert_eq!(
            state.entries,
            [
                ChatEntry::Agent {
                    text: "Hello".into()
                },
                tool(ChatToolStatus::Completed),
            ]
        );
    }

    #[test]
    fn chat_ids_are_short_plain_names() {
        let chat = |id: &str| ChatContent {
            id: id.into(),
            harness: ChatHarness::Omp,
            environment: ContentEnvironment::Host,
            folder: "/work".into(),
            session: None,
        };
        assert!(chat("chat-01ab").validate().is_ok());
        assert!(chat("").validate().is_err());
        assert!(chat("../x").validate().is_err());
        assert!(chat(&"a".repeat(MAX_CHAT_ID_BYTES + 1)).validate().is_err());
    }
}
