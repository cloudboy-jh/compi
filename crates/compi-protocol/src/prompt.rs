//! Shell prompt settings. The daemon detects, previews and applies prompt
//! providers inside the terminal environment it launches shells in: the local
//! Unix account, a WSL2 distribution, or the remote account behind SSH.
//!
//! Compi never installs a provider. Every change is planned first, backed up
//! under `~/.compi/prompt/backups`, and applied only with the token of a plan
//! the user confirmed.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PromptProvider {
    OhMyPosh,
    Starship,
}

impl PromptProvider {
    pub const ALL: [Self; 2] = [Self::OhMyPosh, Self::Starship];

    pub const fn label(self) -> &'static str {
        match self {
            Self::OhMyPosh => "Oh My Posh",
            Self::Starship => "Starship",
        }
    }

    pub const fn executable(self) -> &'static str {
        match self {
            Self::OhMyPosh => "oh-my-posh",
            Self::Starship => "starship",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PromptShell {
    Bash,
    Zsh,
}

impl PromptShell {
    pub const ALL: [Self; 2] = [Self::Bash, Self::Zsh];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
        }
    }

    /// Recognize a supported shell from an executable path such as `/usr/bin/zsh`.
    pub fn from_path(path: &str) -> Option<Self> {
        match path.rsplit('/').next()? {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            _ => None,
        }
    }
}

/// Which shells a prompt applies to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PromptScope {
    /// Shells Compi starts with its integration. User startup files are not edited.
    Compi,
    /// The account's login shell everywhere, through a marked block in its startup file.
    Normal,
}

/// The starting configuration. Paths are absolute in the target environment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromptStyle {
    /// The provider's built-in default.
    Default,
    /// An Oh My Posh theme file.
    Theme { path: String },
    /// A Starship preset by name.
    Preset { name: String },
    /// The user's existing configuration. Compi copies it; it never edits it.
    Existing { path: String },
}

/// A style is used exactly as the provider defines it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PromptConfig {
    pub provider: PromptProvider,
    pub style: PromptStyle,
}

/// Most styles one preview request renders; clients page through longer lists.
pub const MAX_PREVIEW_STYLES: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptStyleEntry {
    pub style: PromptStyle,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptProviderInfo {
    pub provider: PromptProvider,
    /// `None` when the provider is not installed in the target environment.
    pub path: Option<String>,
    pub version: Option<String>,
    pub styles: Vec<PromptStyleEntry>,
}

/// A startup-file line that initializes a prompt provider or Compi's managed block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptStartupLine {
    pub path: String,
    pub line: u32,
    pub text: String,
    pub provider: Option<PromptProvider>,
    /// The line belongs to Compi's marked block.
    pub managed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptShellInfo {
    pub path: String,
    /// `None` when Compi cannot manage prompts for this shell.
    pub shell: Option<PromptShell>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptApplied {
    pub config: PromptConfig,
    /// Shell whose startup file holds the managed block (normal scope only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<PromptShell>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_file: Option<String>,
    pub applied_at_ms: u64,
}

/// Saved in the target at `~/.compi/prompt/state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compi: Option<PromptApplied>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal: Option<PromptApplied>,
}

impl PromptState {
    pub fn get(&self, scope: PromptScope) -> Option<&PromptApplied> {
        match scope {
            PromptScope::Compi => self.compi.as_ref(),
            PromptScope::Normal => self.normal.as_ref(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptBackup {
    pub id: String,
    pub created_ms: u64,
    pub description: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptEnvironment {
    /// Human-readable target, such as `WSL Ubuntu` or a host name.
    pub target: String,
    pub distribution: Option<String>,
    /// Installed WSL2 distributions on Windows; empty elsewhere.
    pub distributions: Vec<String>,
    pub home: String,
    /// The shell Compi starts by default in this environment.
    pub compi_shell: PromptShellInfo,
    /// The account's login shell, used outside Compi.
    pub login_shell: PromptShellInfo,
    pub providers: Vec<PromptProviderInfo>,
    pub startup_lines: Vec<PromptStartupLine>,
    pub state: PromptState,
    /// Newest first.
    pub backups: Vec<PromptBackup>,
    /// Explanations for missing or unsupported setups.
    pub notes: Vec<String>,
}

impl PromptEnvironment {
    pub fn provider(&self, provider: PromptProvider) -> Option<&PromptProviderInfo> {
        self.providers.iter().find(|info| info.provider == provider)
    }
}

/// Raw provider output, including ANSI SGR sequences.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptRender {
    pub left: String,
    pub right: String,
}

/// One style's rendering after a fast successful command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptStyleRender {
    pub success: PromptRender,
    /// Why this style could not be rendered.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptPreview {
    /// One entry per requested style, in request order.
    pub renders: Vec<PromptStyleRender>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromptOperation {
    /// Use this style in Compi shells; with `outside`, also in the login shell
    /// through its startup file. Without it, any login-shell prompt is removed.
    Apply {
        config: PromptConfig,
        outside: bool,
    },
    /// Remove every managed prompt.
    Disable,
    Restore {
        backup: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptFileAction {
    Create,
    Replace,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptFileChange {
    pub path: String,
    pub action: PromptFileAction,
    /// A file the user owns, such as `~/.zshrc`, rather than one under `~/.compi`.
    pub user_file: bool,
    /// Line diff for user files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptPlan {
    /// Identifies these exact changes; execution fails if the files changed since.
    pub token: String,
    pub summary: String,
    pub changes: Vec<PromptFileChange>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PromptRequest {
    Detect,
    Preview {
        provider: PromptProvider,
        /// At most [`MAX_PREVIEW_STYLES`].
        styles: Vec<PromptStyle>,
        /// Directory to render in; the target home when absent.
        cwd: Option<String>,
        width: u16,
    },
    /// Plans the change; nothing is written. Running Compi shells pick up any
    /// executed change at their next prompt.
    Plan {
        operation: PromptOperation,
    },
    Execute {
        operation: PromptOperation,
        token: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum PromptResponse {
    Detected { environment: PromptEnvironment },
    Previewed { preview: PromptPreview },
    Planned { plan: PromptPlan },
    Executed { environment: PromptEnvironment },
}
