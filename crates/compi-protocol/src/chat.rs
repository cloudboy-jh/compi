//! Native agent chat: a content pane talking to an installed agent harness (OMP over the
//! Agent Client Protocol, Claude Code over its headless JSON stream, Codex over its app
//! server's JSON-RPC, OpenCode over its local HTTP server and event stream). The daemon
//! runs the harness, like a shell, and keeps the transcript; windows watch it
//! (`WatchChat`) and send prompts, cancels and answers. The harness owns its sessions,
//! models, tools and credentials.
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
/// Prompts one chat holds for after its running turn.
pub const MAX_CHAT_QUEUED: usize = 32;
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
/// The most of a sub-agent's result kept for display: its start.
pub const MAX_CHAT_AGENT_RESULT_BYTES: usize = 32 * 1024;
/// The most tool calls of a sub-agent kept for display: its latest.
pub const MAX_CHAT_AGENT_STEPS: usize = 200;

/// An installed agent program Compi can chat with. Everything Compi knows of each one is
/// its `AgentEntry`; every list of agents (Settings → Default agent, the palette's New
/// chat with …, `compi chat --agent`, project settings, the model picker's marks, the
/// daemon's lookup and launch) reads `ALL` and the entries. Adding an agent is a variant,
/// its entry in `ENTRIES` and its driver in the daemon (docs/dev/ADDING_AN_AGENT.md).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ChatHarness {
    /// OMP, driven over the Agent Client Protocol (`omp acp`).
    #[default]
    Omp,
    /// Claude Code, driven by its headless JSON stream mode.
    Claude,
    /// OpenAI's Codex, driven through `codex app-server` (JSON-RPC over its standard
    /// input and output).
    Codex,
    /// OpenCode, driven through `opencode serve` (its local HTTP server and event
    /// stream).
    #[serde(rename = "opencode")]
    OpenCode,
}

/// Everything Compi knows of one agent: what it is called, how it looks, how it names
/// Compi's permission levels and where its real program is on each side of a Windows PC.
#[derive(Debug)]
pub struct AgentEntry {
    pub harness: ChatHarness,
    /// The agent's own name.
    pub name: &'static str,
    /// Its command name: `compi chat --agent`, `[chat.<program>]` in the configuration,
    /// and the program Compi looks for.
    pub program: &'static str,
    /// More words the palette finds its New chat with … command by.
    pub keywords: &'static str,
    /// Its mark.
    pub mark: AgentMark,
    /// Model IDs naming no provider are the agent's own (Claude Code's `opus`, Codex's
    /// `gpt-5.5`), listed under its name and drawn with its mark; else they are Other.
    pub own_models: bool,
    /// What the picker calls the agent's own permissions setting (`ChatApproval::Harness`).
    pub own_setting: &'static str,
    /// The approval mode Compi starts it with for `ChatApproval::AlwaysAsk`, `Write` and
    /// `Yolo`, by its own name, as `ChatState::approval_mode` reports it.
    pub approval_modes: [&'static str; 3],
    /// Other modes it can report running with, each with the Compi level it is nearest
    /// to (`Harness` for none) and its name.
    pub other_modes: &'static [(&'static str, ChatApproval, &'static str)],
    /// Folders under the Linux home searched for `program` after the search path, where
    /// its installers put it.
    pub linux_folders: &'static [&'static str],
    /// Where its real Windows program is, in order. Never a script launcher (npm's
    /// `.cmd` and shell shims), whose interpreter can be the wrong side's.
    pub windows: &'static [WindowsProgram],
    /// The variable naming the Bash its Windows program runs commands with (Claude
    /// Code's `CLAUDE_CODE_GIT_BASH_PATH`; OMP's, through the extension Compi loads,
    /// `COMPI_AGENT_SHELL`; OpenCode's `SHELL`): in a WSL chat it names Compi's agent
    /// shell, which runs them in WSL; a Linux program never gets it. `None` for an agent
    /// whose Windows program's commands Compi cannot reach (Codex's always run in
    /// PowerShell).
    pub windows_bash: Option<&'static str>,
    /// The command that installs it, in a terminal in WSL or on Windows (Settings →
    /// Agents shows it for an agent installed nowhere).
    pub install: &'static str,
    /// Its npm package, whose newest published version Settings → Agents compares with
    /// the installed ones.
    pub package: &'static str,
    /// The arguments that update it to the newest version, however it was installed.
    pub update: &'static [&'static str],
}

/// How an agent's mark is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMark {
    /// Its own mark, in its colours: `assets/brands/<file>.svg`.
    Brand(&'static str),
    /// A model mark, `assets/models/<file>.svg`, drawn in `colour` (`#RRGGBB`).
    Model {
        file: &'static str,
        colour: &'static str,
    },
}

/// One place an agent's Windows program can be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsProgram {
    /// This file name, on the search path.
    Path(&'static str),
    /// This `/`-separated path under the user's home folder.
    Home(&'static str),
    /// This `/`-separated path under npm's global folder: the folder of the agent's npm
    /// launcher (`<program>.cmd`) on the search path, else `%APPDATA%\npm`.
    Npm(&'static str),
}

/// Every agent's entry, in `ChatHarness::ALL` order.
static ENTRIES: [AgentEntry; 4] = [
    AgentEntry {
        harness: ChatHarness::Omp,
        name: "OMP",
        program: "omp",
        keywords: "oh my pi acp",
        mark: AgentMark::Brand("omp"),
        own_models: false,
        own_setting: "OMP's own setting",
        approval_modes: ["always-ask", "write", "yolo"],
        other_modes: &[(
            "always-ask-cached",
            ChatApproval::AlwaysAsk,
            "Ask once per tool",
        )],
        linux_folders: &[".local/bin", ".bun/bin"],
        windows: &[
            WindowsProgram::Path("omp.exe"),
            WindowsProgram::Home(".bun/bin/omp.exe"),
        ],
        windows_bash: Some("COMPI_AGENT_SHELL"),
        install: "bun install -g @oh-my-pi/pi-coding-agent",
        package: "@oh-my-pi/pi-coding-agent",
        update: &["update"],
    },
    AgentEntry {
        harness: ChatHarness::Claude,
        name: "Claude Code",
        program: "claude",
        keywords: "anthropic",
        mark: AgentMark::Model {
            file: "claude",
            colour: "#D97757",
        },
        own_models: true,
        own_setting: "Same as Claude Code",
        approval_modes: ["default", "acceptEdits", "bypassPermissions"],
        other_modes: &[("plan", ChatApproval::Harness, "Plan")],
        linux_folders: &[".local/bin", ".claude/local"],
        windows: &[
            WindowsProgram::Path("claude.exe"),
            WindowsProgram::Home(".local/bin/claude.exe"),
            WindowsProgram::Npm("node_modules/@anthropic-ai/claude-code/bin/claude.exe"),
        ],
        windows_bash: Some("CLAUDE_CODE_GIT_BASH_PATH"),
        install: "npm install -g @anthropic-ai/claude-code",
        package: "@anthropic-ai/claude-code",
        update: &["update"],
    },
    AgentEntry {
        harness: ChatHarness::Codex,
        name: "Codex",
        program: "codex",
        keywords: "openai gpt chatgpt",
        mark: AgentMark::Model {
            file: "openai",
            colour: "#10A37F",
        },
        own_models: true,
        own_setting: "Same as Codex",
        approval_modes: [
            "untrusted/read-only",
            "on-request/workspace-write",
            "never/danger-full-access",
        ],
        other_modes: &[
            ("on-request/read-only", ChatApproval::AlwaysAsk, "Read only"),
            (
                "untrusted/workspace-write",
                ChatApproval::AlwaysAsk,
                "Ask before commands",
            ),
            (
                "on-failure/workspace-write",
                ChatApproval::Write,
                "Edits allowed",
            ),
            (
                "never/workspace-write",
                ChatApproval::Write,
                "Edits allowed, never ask",
            ),
            (
                "on-request/danger-full-access",
                ChatApproval::Yolo,
                "Full access",
            ),
        ],
        linux_folders: &[".local/bin", ".codex/packages/standalone/current/bin"],
        windows: &[
            WindowsProgram::Path("codex.exe"),
            WindowsProgram::Home(".codex/packages/standalone/current/bin/codex.exe"),
            WindowsProgram::Npm(
                "node_modules/@openai/codex/node_modules/@openai/codex-win32-x64/vendor/x86_64-pc-windows-msvc/bin/codex.exe",
            ),
        ],
        windows_bash: None,
        install: "npm install -g @openai/codex",
        package: "@openai/codex",
        update: &["update"],
    },
    AgentEntry {
        harness: ChatHarness::OpenCode,
        name: "OpenCode",
        program: "opencode",
        keywords: "open code sst",
        mark: AgentMark::Brand("opencode"),
        own_models: false,
        own_setting: "Same as OpenCode",
        approval_modes: ["ask", "allow-edits", "allow-all"],
        other_modes: &[],
        linux_folders: &[".opencode/bin", ".local/bin", ".bun/bin"],
        windows: &[
            WindowsProgram::Npm("node_modules/opencode-ai/bin/opencode.exe"),
            WindowsProgram::Path("opencode.exe"),
        ],
        // Its Windows build runs commands with the shell `SHELL` names.
        windows_bash: Some("SHELL"),
        install: "npm install -g opencode-ai",
        package: "opencode-ai",
        update: &["upgrade"],
    },
];

impl ChatHarness {
    pub const ALL: [Self; 4] = [Self::Omp, Self::Claude, Self::Codex, Self::OpenCode];

    /// Everything Compi knows of the agent.
    pub fn entry(self) -> &'static AgentEntry {
        let entry = &ENTRIES[self as usize];
        debug_assert_eq!(entry.harness, self);
        entry
    }

    /// The harness's own name.
    pub fn name(self) -> &'static str {
        self.entry().name
    }

    /// The program's command name.
    pub fn program(self) -> &'static str {
        self.entry().program
    }

    /// The harness named `program` (`ChatHarness::program`).
    pub fn parse(program: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|harness| harness.program() == program)
    }

    /// The approval mode Compi starts the agent with for `approval`, by the agent's own
    /// name; `None` keeps the agent's own setting.
    pub fn approval_mode(self, approval: ChatApproval) -> Option<&'static str> {
        let [always_ask, write, yolo] = self.entry().approval_modes;
        match approval {
            ChatApproval::Harness => None,
            ChatApproval::AlwaysAsk => Some(always_ask),
            ChatApproval::Write => Some(write),
            ChatApproval::Yolo => Some(yolo),
        }
    }

    /// The Compi level and name of approval mode `mode` the agent reports running with;
    /// `None` for a mode its entry does not know.
    pub fn approval_level(self, mode: &str) -> Option<(ChatApproval, Option<&'static str>)> {
        let entry = self.entry();
        let levels = [
            ChatApproval::AlwaysAsk,
            ChatApproval::Write,
            ChatApproval::Yolo,
        ];
        if let Some(index) = entry.approval_modes.iter().position(|known| *known == mode) {
            return Some((levels[index], None));
        }
        entry
            .other_modes
            .iter()
            .find(|(known, ..)| *known == mode)
            .map(|(_, level, name)| (*level, Some(*name)))
    }
}

/// How the harness asks before running tools, as Compi starts it. `Harness` keeps the
/// harness's own setting (OMP's `tools.approvalMode`, which `omp acp` does not read by
/// itself, so the daemon looks it up and passes it on; Claude Code's `permissions`
/// settings, Codex's `approval_policy` and `sandbox_mode` and OpenCode's `permission`
/// rules, which they read themselves). Each agent names the levels its own way
/// (`AgentEntry::approval_modes`).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ChatApproval {
    #[default]
    Harness,
    /// Ask before anything but reading.
    AlwaysAsk,
    /// Run reads and edits; ask before commands.
    Write,
    /// Run everything without asking.
    Yolo,
}

/// The settings a new session of the harness starts on, by `ChatOption::category`: the
/// choice values you last picked for this harness in Compi. A setting the harness does
/// not offer, or a value it does not list, keeps the harness's own. Resumed sessions keep
/// theirs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ChatPicks {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_level: Option<String>,
}

/// One setting's change between two prompts, by its choices' names.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatChange {
    pub from: String,
    pub to: String,
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
    /// The agent's program the chat prefers, on a Windows PC with both: its Linux one in
    /// WSL, which then works in a Windows folder too, or its Windows one, also in a WSL
    /// folder. The other is used when it has no such program. `None` (threads from
    /// before Compi chose) takes the program of the folder's side first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer: Option<AgentSide>,
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
    /// Prompts waiting for the running turn to end, sent in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued: Vec<ChatQueued>,
    /// The model providers signed in with a subscription plan, with what is left of it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plans: Vec<ChatPlan>,
    /// The model providers the agent can sign in to, with the accounts it keeps for
    /// them (`/providers`); `None` until the agent was asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<ChatProvider>>,
    /// A provider being signed in or out from the chat (`SignInChat`, `SignOutChat`),
    /// and how far it got.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<ChatLogin>,
    /// The chat's agent being updated (`UpdateChatAgent`), and how far it got.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<ChatAgentUpdate>,
    /// The daemon keeps the chat as a thread: it was sent its first prompt, or opened
    /// from a saved thread. A chat without one is a draft, which closing leaves nothing
    /// of. A chat gets its thread with its first transcript entry, so watchers learn it
    /// from whole states.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub thread: bool,
    /// How the running harness asks before it runs tools, whether Compi chose it or the
    /// harness's own setting did: OMP's `--approval-mode` (`always-ask`,
    /// `always-ask-cached`, `write`, `yolo`), Claude Code's permission mode (`default`,
    /// `acceptEdits`, `plan`, `bypassPermissions`, `dontAsk`, `auto`), which follows the
    /// session's `mode` setting as it changes, or Codex's approval policy and sandbox
    /// mode as `policy/sandbox` (`untrusted/read-only`, `on-request/workspace-write`,
    /// `never/danger-full-access`, …; a granular policy is `granular`); `None` while
    /// unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_mode: Option<String>,
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
            ChatEvent::ApprovalMode { mode } => self.approval_mode = mode,
            ChatEvent::Commands { commands } => self.commands = commands,
            ChatEvent::Usage { usage } => self.usage = usage,
            ChatEvent::Permission { permission } => self.permission = permission,
            ChatEvent::Question { question } => self.question = question,
            ChatEvent::Queued { queued } => self.queued = queued,
            ChatEvent::Plans { plans } => self.plans = plans,
            ChatEvent::Providers { providers } => self.providers = providers,
            ChatEvent::Login { login } => self.login = login,
            ChatEvent::Update { update } => self.update = update,
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
    /// images it carried; when it was sent and when the turn it went into ended
    /// (milliseconds since the Unix epoch), absent for prompts a resumed session replays.
    User {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sent_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ended_ms: Option<u64>,
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
    /// The last running sub-agent of the chat finished.
    AgentsFinished(ChatAgentsFinished),
    /// A turn failed.
    Error { message: String },
    /// The turn was stopped.
    Cancelled,
    /// Just before a prompt: the model or thinking level changed since the session's
    /// previous prompt (only the settings that did).
    Switch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<ChatChange>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thought_level: Option<ChatChange>,
    },
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
    /// The sub-agents it started (OMP's `task`, Claude Code's `Agent`, Codex's
    /// `spawn_agent`), in its order. The call runs while one of them does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<ChatAgent>,
    /// It only waits for sub-agents (OMP's `wait` on `task` jobs): their call and the
    /// agents bar already show what it shows.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub waits_on_agents: bool,
}

/// A sub-agent a tool call started, as last reported.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatAgent {
    /// Its name (`TaskA`), unique in the session.
    pub id: String,
    /// Its type (`sonic`, `task`, …).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent: String,
    pub status: ChatToolStatus,
    /// What it does now: its current or latest tool's intent, else that tool and its
    /// arguments; one short line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    #[serde(default)]
    pub tool_count: u64,
    /// When it began running, by the daemon's clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_ms: Option<u64>,
    /// How long it has run, as it reported.
    #[serde(default)]
    pub duration_ms: u64,
    /// In US dollars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Its task, at most `MAX_CHAT_TOOL_INPUT_BYTES`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment: Option<String>,
    /// What it answered once finished, at most `MAX_CHAT_AGENT_RESULT_BYTES`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// The tool calls it made, oldest first, at most its latest `MAX_CHAT_AGENT_STEPS`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<ChatAgentStep>,
}

// Cost is a reported figure, never NaN; equality on the bits is what a change check needs.
impl Eq for ChatAgent {}

/// One tool call of a sub-agent: what it said it was doing, else the tool and its
/// argument, in one short line, and whether it runs, ended or failed. One still running
/// when its agent has ended did not finish.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatAgentStep {
    pub text: String,
    pub status: ChatToolStatus,
}

/// The sub-agents that ran since none did have all finished: how many, their elapsed
/// time from reported starts/finishes, and their summed cost when reported. Late
/// corrections replace the same entry. `tool` is the last call in the wave.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatAgentsFinished {
    pub tool: usize,
    pub count: usize,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

impl Eq for ChatAgentsFinished {}

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

/// How `SendChat` hands its prompt to the harness.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatDelivery {
    /// As the next turn; refused while one runs.
    #[default]
    Prompt,
    /// Into the running turn, which takes it at its next step; as the next turn when
    /// none runs. Queued when the harness cannot steer or the chat is still starting.
    Steer,
    /// After the running turn and the prompts queued before it; at once when the chat
    /// waits for a prompt.
    Queue,
}

/// A prompt waiting for the running turn to end: its text, attached files and the names
/// of its images.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatQueued {
    pub id: u64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

/// A model provider used through a subscription plan rather than paid per use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatPlan {
    /// As model values name it before their `/` (`anthropic`, `openai-codex`).
    pub provider: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<ChatPlanLimit>,
}

/// One usage window of a plan (`5 Hour`, `7 days`) and how much of it is left.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatPlanLimit {
    pub label: String,
    pub left_percent: u8,
}

/// A model provider the chat's agent can sign in to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatProvider {
    /// What `SignInChat` names: as model values name it before their `/` (OMP's
    /// `anthropic`, OpenCode's `openai`), or the agent's one provider (Claude Code's
    /// `anthropic`, Codex's `openai`).
    pub id: String,
    /// Its name, when the agent gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The accounts signed in, usable or not.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<ChatAccount>,
    /// The ways to sign in, in the agent's order; none when the agent picks one itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<ChatSignInMethod>,
    /// The agent can sign it out from the chat (`SignOutChat`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sign_out: bool,
    /// Signing in again adds an account beside the ones there; else it replaces them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub adds_accounts: bool,
}

/// One account an agent keeps for a provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatAccount {
    /// What `SignOutChat` names to sign out this one alone, when the agent tells them
    /// apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Who it is (an email address), when the agent says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// How it pays (`Max 5x`, `ChatGPT`, `API key`), when the agent says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Why the agent can no longer use it, in the provider's words (`Refresh token
    /// expired`); `None` while it works.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expired: Option<String>,
}

/// One way to sign a provider in (`Claude subscription`, `API key`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatSignInMethod {
    pub id: String,
    pub label: String,
}

/// The sign-in method (`ChatSignInMethod::id`) that takes an API key pasted in the chat.
pub const API_KEY_METHOD: &str = "api-key";

/// An agent being updated from a chat, with its version before and after.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatAgentUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub step: ChatUpdateStep,
}

/// How far an update got.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatUpdateStep {
    /// Running; what the updater said last.
    Running {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Finished: `to` is the version now (the same as `from` when it was up to date).
    Done,
    Failed {
        message: String,
    },
}

/// A provider being signed in or out from the chat, through the agent's own login,
/// where the chat runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatLogin {
    /// As `ChatProvider::id` names it.
    pub provider: String,
    /// It signs the provider out rather than in.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sign_out: bool,
    /// The page to sign in on, once the login gives it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub step: ChatLoginStep,
}

/// How far a sign-in got.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatLoginStep {
    /// Starting, or waiting for the browser; what the login said last.
    Waiting {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// The login asks for something pasted back: an authorization code, or the page the
    /// provider redirected to.
    Code {
        prompt: String,
    },
    /// Signed in, as `account` when the login says.
    Done {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
    },
    Failed {
        message: String,
    },
}

/// The provider a model value (`anthropic/claude-opus-5-5`) belongs to: what comes
/// before its `/`.
pub fn model_provider(model: &str) -> Option<&str> {
    model.split_once('/').map(|(provider, _)| provider)
}

/// Whether `provider` can name a provider to sign in: letters, digits, `-`, `_` and `.`.
pub fn valid_provider(provider: &str) -> bool {
    !provider.is_empty()
        && provider.len() <= 64
        && provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

/// A session of the harness in the chat's folder, for resuming. Sessions without a
/// message are left out.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatSessionInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// When it last changed, as the harness reports it (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    /// The folder the harness saved it under, which can be another name of the chat's
    /// folder (a link's target).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// The saved session's size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// The first prompt, at most [`MAX_CHAT_SESSION_PREVIEW_CHARS`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    /// The end of the last reply, at most [`MAX_CHAT_SESSION_PREVIEW_CHARS`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reply: Option<String>,
}

/// What a thread is doing, for the threads list.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadActivity {
    /// A turn runs.
    Working,
    /// A permission or question waits on the user.
    Waiting,
    Idle,
}

/// A thread the daemon keeps: a chat, its agent session and its transcript, kept after
/// its pane closes until it is deleted. Opening it again starts its agent on the same
/// session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadInfo {
    /// The chat as a pane holds it: id, agent, environment, folder and session.
    pub chat: ChatContent,
    /// The session's title, else its first prompt's first line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The selected model, when the harness has reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ChatChoice>,
    pub created_ms: u64,
    /// When its transcript last changed.
    pub updated_ms: u64,
    pub activity: ThreadActivity,
    /// A pane or view holds it now; a thread none holds has ended.
    pub open: bool,
    /// The project its folder belongs to: the Git top level above it, else the folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<crate::Project>,
}

/// How much of a session's first prompt and last reply its listing carries.
pub const MAX_CHAT_SESSION_PREVIEW_CHARS: usize = 1200;

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
    /// The harness started with approval mode `mode` (see `ChatState::approval_mode`).
    ApprovalMode {
        mode: Option<String>,
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
    /// The queued prompts, all of them.
    Queued {
        queued: Vec<ChatQueued>,
    },
    Plans {
        plans: Vec<ChatPlan>,
    },
    Providers {
        providers: Option<Vec<ChatProvider>>,
    },
    Login {
        login: Option<ChatLogin>,
    },
    Update {
        update: Option<ChatAgentUpdate>,
    },
}

/// Where every agent is installed on this PC, for Settings → Agents (`AgentInstalls`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentInstalls {
    /// The WSL distribution the Linux programs were looked for in, the default one; `None`
    /// on a PC without WSL, and on a Linux or macOS host, whose own programs they are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution: Option<String>,
    /// One per agent, in `ChatHarness::ALL` order.
    #[serde(default)]
    pub agents: Vec<AgentInstall>,
}

/// Where one agent is installed: its Windows program and its Linux program, each as its
/// launch finds it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentInstall {
    pub harness: ChatHarness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<AgentProgram>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux: Option<AgentProgram>,
    /// The newest version published (`AgentEntry::package`), when the registry said in
    /// time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
}

/// Which of an agent's programs `UpdateAgent` updates.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentSide {
    /// Its Windows program.
    Windows,
    /// Its Linux program: in the default WSL distribution on Windows, the host's own
    /// elsewhere.
    Linux,
}

/// An agent's version before and after an update.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentUpdated {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// Whether version `newer` comes after `older`, comparing their numbers in order
/// (`1.19.6` after `1.19.0`, `1.19.6` not after `2.0.27`); a part that is no number
/// counts as 0.
pub fn newer_version(newer: &str, older: &str) -> bool {
    let numbers = |version: &str| -> Vec<u64> {
        version
            .split(['-', '+'])
            .next()
            .unwrap_or_default()
            .split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };
    let (newer, older) = (numbers(newer), numbers(older));
    for index in 0..newer.len().max(older.len()) {
        let (a, b) = (
            newer.get(index).copied().unwrap_or(0),
            older.get(index).copied().unwrap_or(0),
        );
        if a != b {
            return a > b;
        }
    }
    false
}

/// One installed program of an agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProgram {
    /// Its version, as `<program> --version` reports it; `None` when it reports none in
    /// time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
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
            agents: Vec::new(),
            waits_on_agents: false,
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
    fn a_summary_entry_is_tagged_like_the_others() {
        let entry = ChatEntry::AgentsFinished(ChatAgentsFinished {
            tool: 3,
            count: 2,
            duration_ms: 15_000,
            cost: Some(0.2),
        });
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "agents_finished", "tool": 3, "count": 2, "duration_ms": 15000, "cost": 0.2,
            })
        );
        assert_eq!(serde_json::from_value::<ChatEntry>(json).unwrap(), entry);
    }

    #[test]
    fn chat_ids_are_short_plain_names() {
        let chat = |id: &str| ChatContent {
            id: id.into(),
            harness: ChatHarness::Omp,
            environment: ContentEnvironment::Host,
            folder: "/work".into(),
            prefer: None,
            session: None,
        };
        assert!(chat("chat-01ab").validate().is_ok());
        assert!(chat("").validate().is_err());
        assert!(chat("../x").validate().is_err());
        assert!(chat(&"a".repeat(MAX_CHAT_ID_BYTES + 1)).validate().is_err());
    }
}
