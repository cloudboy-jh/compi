//! Signing model providers in and out from a chat, and what the agent says of its
//! sign-ins. All of it runs the agent's own commands where the chat runs, so they reach
//! the same program, credentials and broker as the chat; OpenCode's go through its
//! server instead (`opencode::accounts`).
//!
//! - OMP: `omp login <provider>` signs in (it picks the way itself), `omp usage --json`
//!   lists the accounts and the ones it gave up on (`disabledCredentials`). It has no
//!   sign-out outside its own window.
//! - Claude Code: `claude auth login --claudeai|--console`, `claude auth logout`,
//!   `claude auth status --json`.
//! - Codex: `codex login` (ChatGPT) or `codex login --with-api-key` (the key on its
//!   standard input), `codex logout`, `codex login status`.
//!
//! A login's output is read as it comes (`Output`): it says the page to open, what it
//! waits for and whether it worked; its prompts end a line without ending it, and the
//! answer goes to its standard input.
use super::launch::LAUNCH_MARK;
use compi_protocol::chat::{
    API_KEY_METHOD, ChatAccount, ChatHarness, ChatProvider, ChatSignInMethod, valid_provider,
};
use serde_json::Value;

/// What a login's output said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Signal {
    /// The page to sign in on.
    Url(String),
    /// A line about how it goes ("Waiting for browser authentication...").
    Note(String),
    /// It waits for something typed back (an authorization code).
    Prompt(String),
    /// Signed in, as the account when it says.
    Done(Option<String>),
    Failed(String),
}

/// Reads a login's output as it comes, line by line, and the unfinished line its prompt
/// leaves.
#[derive(Default)]
pub(super) struct Output {
    pending: String,
    /// The previous line said a page follows.
    url_next: bool,
    /// The unfinished line was reported as a prompt.
    prompted: bool,
}

impl Output {
    pub(super) fn feed(&mut self, text: &str) -> Vec<Signal> {
        self.pending.push_str(text);
        let mut signals = Vec::new();
        while let Some(end) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=end).collect();
            self.prompted = false;
            self.line(&plain(&line), &mut signals);
        }
        let tail = plain(&self.pending);
        let tail = tail.trim();
        if !self.prompted && (tail.ends_with(':') || tail.ends_with('>')) {
            self.prompted = true;
            signals.push(Signal::Prompt(
                tail.trim_end_matches([':', '>']).trim().to_owned(),
            ));
        }
        signals
    }

    /// The prompt was answered: the next output starts a line of its own.
    pub(super) fn answered(&mut self) {
        self.pending.clear();
        self.prompted = false;
    }

    fn line(&mut self, line: &str, signals: &mut Vec<Signal>) {
        let line = line.trim();
        if line.is_empty() || line.starts_with(LAUNCH_MARK.trim()) {
            return;
        }
        if std::mem::take(&mut self.url_next) && line.starts_with("http") {
            signals.push(Signal::Url(line.to_owned()));
            return;
        }
        if line.ends_with(':') && (line.contains("URL") || line.contains("link")) {
            // "Open this URL in your browser:", "… navigate to this URL to authenticate:"
            self.url_next = true;
        } else if let Some((_, url)) = line
            .split_once(": http")
            .filter(|(said, _)| said.contains("visit") || said.ends_with("Sign in at"))
        {
            // "If the browser didn't open, visit: https://…"
            signals.push(Signal::Url(format!("http{url}")));
        } else if let Some(rest) = line.strip_prefix("Logged in to ") {
            let account = rest
                .split_once(" as ")
                .map(|(_, account)| account.to_owned());
            signals.push(Signal::Done(account));
        } else if line.starts_with("Login successful")
            || line.starts_with("Successfully logged in")
            || line.starts_with("Successfully logged out")
        {
            signals.push(Signal::Done(None));
        } else if let Some(reason) = ["Login failed: ", "Couldn't log in: ", "Error: "]
            .iter()
            .find_map(|start| line.strip_prefix(start))
        {
            signals.push(Signal::Failed(reason.to_owned()));
        } else if line.starts_with("No credentials were stored") {
            signals.push(Signal::Failed(line.to_owned()));
        } else if !line.starts_with("Local shortcut") && !line.starts_with("Credentials saved to") {
            signals.push(Signal::Note(line.to_owned()));
        }
    }
}

/// `text` without terminal colour codes, links' markup and carriage returns.
pub(super) fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                // An operating system command (a link's `ESC ] 8 ;; url`), ended by BEL
                // or `ESC \`.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' {
                            chars.next_if_eq(&'\\');
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if c != '\r' {
            out.push(c);
        }
    }
    out
}

/// How a sign-in runs.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Way {
    /// The agent's program with `args`. With `key`, the answer is an API key, asked for
    /// at once with that prompt and handed over as the program's whole input.
    Program {
        args: Vec<&'static str>,
        provider_arg: bool,
        key: Option<String>,
    },
    /// OpenCode's server.
    Server,
}

/// Claude Code's one provider, and Codex's.
const ANTHROPIC: &str = "anthropic";
const OPENAI: &str = "openai";
/// Claude Code's ways to sign in.
const CLAUDE_SUBSCRIPTION: &str = "claudeai";
const CLAUDE_CONSOLE: &str = "console";
/// Codex's way to sign in with a ChatGPT account.
const CHATGPT: &str = "chatgpt";

/// How `harness` signs `provider` in the way `method` says, or its own.
pub(super) fn sign_in_way(
    harness: ChatHarness,
    provider: &str,
    method: Option<&str>,
) -> Result<Way, String> {
    if !valid_provider(provider) {
        return Err(format!("{provider} isn't a provider name"));
    }
    let only = |id: &str| {
        if provider == id {
            Ok(())
        } else {
            Err(format!(
                "{} signs in to {} only",
                harness.name(),
                provider_name(id)
            ))
        }
    };
    let program = |args: &[&'static str]| Way::Program {
        args: args.to_vec(),
        provider_arg: false,
        key: None,
    };
    match harness {
        ChatHarness::Omp => Ok(Way::Program {
            args: vec!["login"],
            provider_arg: true,
            key: None,
        }),
        ChatHarness::Claude => {
            only(ANTHROPIC)?;
            Ok(match method {
                Some(CLAUDE_CONSOLE) => program(&["auth", "login", "--console"]),
                _ => program(&["auth", "login", "--claudeai"]),
            })
        }
        ChatHarness::Codex => {
            only(OPENAI)?;
            Ok(match method {
                Some(API_KEY_METHOD) => Way::Program {
                    args: vec!["login", "--with-api-key"],
                    provider_arg: false,
                    key: Some("Paste your OpenAI API key".into()),
                },
                _ => program(&["login"]),
            })
        }
        ChatHarness::OpenCode => Ok(Way::Server),
    }
}

/// The arguments that sign `harness` out, when it signs out with a command.
pub(super) fn sign_out_args(harness: ChatHarness) -> Result<&'static [&'static str], String> {
    match harness {
        ChatHarness::Claude => Ok(&["auth", "logout"]),
        ChatHarness::Codex => Ok(&["logout"]),
        ChatHarness::Omp => Err("OMP signs out only in its own window (/logout there)".into()),
        ChatHarness::OpenCode => unreachable!("OpenCode signs out through its server"),
    }
}

/// The arguments that make `harness` say what it is signed in to.
pub(super) fn status_args(harness: ChatHarness) -> &'static [&'static str] {
    match harness {
        ChatHarness::Omp => &["usage", "--json"],
        ChatHarness::Claude => &["auth", "status", "--json"],
        ChatHarness::Codex => &["login", "status"],
        ChatHarness::OpenCode => unreachable!("OpenCode says it through its server"),
    }
}

/// The providers in what `harness`'s status command (`status_args`) wrote, besides
/// the ones its models come from (`model_providers`); `None` when it said nothing
/// readable.
pub(super) fn providers(
    harness: ChatHarness,
    said: &str,
    model_providers: &[String],
) -> Option<Vec<ChatProvider>> {
    match harness {
        ChatHarness::Omp => Some(omp_providers(&json(said)?, model_providers)),
        ChatHarness::Claude => Some(vec![claude_provider(&json(said)?)]),
        ChatHarness::Codex => codex_provider(said).map(|provider| vec![provider]),
        ChatHarness::OpenCode => None,
    }
}

/// The JSON object in `said`, after the launch script's mark line.
fn json(said: &str) -> Option<Value> {
    serde_json::from_str(&said[said.find('{')?..]).ok()
}

/// OMP's providers: the accounts `omp usage --json` reports and the ones it disabled
/// (`disabledCredentials`), then the providers of its models no account is for.
fn omp_providers(usage: &Value, model_providers: &[String]) -> Vec<ChatProvider> {
    let mut providers: Vec<ChatProvider> = Vec::new();
    let mut add = |id: &str, account: Option<ChatAccount>| {
        let index = match providers.iter().position(|provider| provider.id == id) {
            Some(index) => index,
            None => {
                providers.push(ChatProvider {
                    id: id.to_owned(),
                    name: None,
                    accounts: Vec::new(),
                    methods: Vec::new(),
                    sign_out: false,
                    adds_accounts: true,
                });
                providers.len() - 1
            }
        };
        providers[index].accounts.extend(account);
    };
    for report in usage["reports"].as_array().into_iter().flatten() {
        let Some(id) = report["provider"].as_str() else {
            continue;
        };
        let metadata = &report["metadata"];
        let plan = metadata["subscriptionPlan"]
            .as_str()
            .or_else(|| metadata["planType"].as_str())
            .map(capitalized);
        add(
            id,
            Some(ChatAccount {
                id: None,
                label: metadata["email"].as_str().map(str::to_owned),
                plan,
                expired: None,
            }),
        );
    }
    for credential in usage["disabledCredentials"].as_array().into_iter().flatten() {
        let Some(id) = credential["provider"].as_str() else {
            continue;
        };
        add(
            id,
            Some(ChatAccount {
                id: None,
                label: credential["email"]
                    .as_str()
                    .or_else(|| credential["accountId"].as_str())
                    .map(str::to_owned),
                plan: None,
                expired: Some(reason(credential["cause"].as_str().unwrap_or("Signed out"))),
            }),
        );
    }
    for id in model_providers {
        add(id, None);
    }
    providers
}

/// The provider's own words in `cause` (an OAuth error's description), else its start.
fn reason(cause: &str) -> String {
    let described = cause
        .split_once("error_description")
        .and_then(|(_, rest)| rest.split('"').map(str::trim).find(|part| part.len() > 1));
    match described {
        Some(description) => description.trim_end_matches('\\').to_owned(),
        None => cause.chars().take(160).collect(),
    }
}

/// Claude Code's provider, from `claude auth status --json`.
fn claude_provider(status: &Value) -> ChatProvider {
    let account = (status["loggedIn"] == true).then(|| ChatAccount {
        id: None,
        label: status["email"].as_str().map(str::to_owned),
        plan: match status["subscriptionType"].as_str() {
            Some(plan) => Some(format!("Claude {}", capitalized(plan))),
            None if status["authMethod"] == "claude.ai" => Some("Claude subscription".into()),
            None if status["apiKeySource"].is_string() || status["authMethod"] == "api_key" => {
                Some("API key".into())
            }
            None => None,
        },
        expired: None,
    });
    ChatProvider {
        id: ANTHROPIC.into(),
        name: Some(provider_name(ANTHROPIC).into()),
        accounts: account.into_iter().collect(),
        methods: vec![
            method(CLAUDE_SUBSCRIPTION, "Claude subscription"),
            method(CLAUDE_CONSOLE, "Anthropic Console (pay per use)"),
        ],
        sign_out: true,
        adds_accounts: false,
    }
}

/// Codex's provider, from `codex login status`: `Logged in using ChatGPT`, `Logged in
/// using an API key - sk-…`, `Not logged in`.
fn codex_provider(said: &str) -> Option<ChatProvider> {
    let account = said.lines().map(str::trim).find_map(|line| {
        if line.starts_with("Not logged in") {
            return Some(None);
        }
        let how = line.strip_prefix("Logged in using ")?;
        let plan = if how.starts_with("ChatGPT") {
            "ChatGPT".to_owned()
        } else if how.contains("API key") {
            "API key".to_owned()
        } else {
            capitalized(how)
        };
        Some(Some(ChatAccount {
            plan: Some(plan),
            ..ChatAccount::default()
        }))
    })?;
    Some(ChatProvider {
        id: OPENAI.into(),
        name: Some(provider_name(OPENAI).into()),
        accounts: account.into_iter().collect(),
        methods: vec![
            method(CHATGPT, "ChatGPT account"),
            method(API_KEY_METHOD, "API key"),
        ],
        sign_out: true,
        adds_accounts: false,
    })
}

pub(super) fn method(id: &str, label: &str) -> ChatSignInMethod {
    ChatSignInMethod {
        id: id.to_owned(),
        label: label.to_owned(),
    }
}

fn provider_name(id: &str) -> &str {
    match id {
        ANTHROPIC => "Anthropic",
        OPENAI => "OpenAI",
        _ => id,
    }
}

/// `text` with its first letter upper case (`max` → `Max`).
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_gives_its_page_then_waits_for_a_code_then_signs_in() {
        let mut output = Output::default();
        assert_eq!(
            output
                .feed("\u{1e}compi-agent linux /home/me/compi\n\nOpen this URL in your browser:\n"),
            []
        );
        assert_eq!(
            output.feed("https://claude.ai/oauth/authorize?code=true\n\n"),
            [Signal::Url(
                "https://claude.ai/oauth/authorize?code=true".into()
            )]
        );
        assert_eq!(
            output.feed("Waiting for browser authentication...\n"),
            [Signal::Note("Waiting for browser authentication...".into())]
        );
        // The prompt comes without a line end, maybe in pieces; it is reported once.
        assert_eq!(output.feed("Paste the authorization code (or full"), []);
        assert_eq!(
            output.feed(" redirect URL): "),
            [Signal::Prompt(
                "Paste the authorization code (or full redirect URL)".into()
            )]
        );
        assert_eq!(output.feed(""), []);
        output.answered();
        assert_eq!(
            output.feed("\u{1b}[32m\nLogged in to Anthropic as me@example.com\n\u{1b}[39m"),
            [Signal::Done(Some("me@example.com".into()))]
        );
        assert_eq!(
            output.feed("Credentials saved to /home/me/.omp/agent.db\n"),
            []
        );
    }

    #[test]
    fn claude_code_s_login_links_its_page_and_asks_for_a_code_if_needed() {
        let mut output = Output::default();
        assert_eq!(
            output.feed(
                "Opening browser to sign in…\nIf the browser didn't open, visit: \u{1b}]8;;https://claude.com/cai/oauth/authorize?x=1\u{7}https://claude.com/cai/oauth/authorize?x=1\u{1b}]8;;\u{7}\nPaste code here if prompted > "
            ),
            [
                Signal::Note("Opening browser to sign in…".into()),
                Signal::Url("https://claude.com/cai/oauth/authorize?x=1".into()),
                Signal::Prompt("Paste code here if prompted".into()),
            ]
        );
        output.answered();
        assert_eq!(
            output.feed("Login successful.\n"),
            [Signal::Done(None)]
        );
    }

    #[test]
    fn codex_s_login_names_its_page_after_a_blank_line() {
        let mut output = Output::default();
        let signals = output.feed(
            "Starting local login server on http://localhost:1455.\nIf your browser did not open, navigate to this URL to authenticate:\n\nhttps://auth.openai.com/oauth/authorize?a=b\n\nOn a remote or headless machine? Use `codex login --device-auth` instead.\nSuccessfully logged in\n",
        );
        assert_eq!(
            signals,
            [
                Signal::Note("Starting local login server on http://localhost:1455.".into()),
                Signal::Url("https://auth.openai.com/oauth/authorize?a=b".into()),
                Signal::Note(
                    "On a remote or headless machine? Use `codex login --device-auth` instead."
                        .into()
                ),
                Signal::Done(None),
            ]
        );
    }

    #[test]
    fn a_failed_login_says_why() {
        let mut output = Output::default();
        assert_eq!(
            output.feed("Login failed: token exchange failed\r\n"),
            [Signal::Failed("token exchange failed".into())]
        );
        assert_eq!(
            output.feed("No credentials were stored for Anthropic.\n"),
            [Signal::Failed(
                "No credentials were stored for Anthropic.".into()
            )]
        );
        assert_eq!(
            output.feed("Couldn't log in: the code expired\n"),
            [Signal::Failed("the code expired".into())]
        );
    }

    #[test]
    fn omp_lists_its_accounts_the_ones_it_disabled_and_its_models_providers() {
        let usage = serde_json::json!({
            "reports": [{
                "provider": "anthropic",
                "metadata": { "email": "me@example.com", "subscriptionPlan": "Max 5x" }
            }],
            "disabledCredentials": [{
                "id": 4,
                "provider": "anthropic",
                "type": "oauth",
                "cause": "oauth refresh failed: OAuthError: anthropic token refresh failed: 400 {\"error\": \"invalid_grant\", \"error_description\": \"Refresh token expired\"}",
                "email": "old@example.com"
            }, {
                "provider": "openai-codex",
                "cause": "revoked"
            }]
        });
        let said = format!("\u{1e}compi-agent linux /home/me\n{usage}");
        let providers = providers(
            ChatHarness::Omp,
            &said,
            &["anthropic".into(), "moonshot".into()],
        )
        .unwrap();
        let summary: Vec<_> = providers
            .iter()
            .map(|provider| {
                let accounts: Vec<_> = provider
                    .accounts
                    .iter()
                    .map(|account| {
                        (
                            account.label.clone(),
                            account.plan.clone(),
                            account.expired.clone(),
                        )
                    })
                    .collect();
                (provider.id.as_str(), accounts)
            })
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "anthropic",
                    vec![
                        (
                            Some("me@example.com".into()),
                            Some("Max 5x".into()),
                            None
                        ),
                        (
                            Some("old@example.com".into()),
                            None,
                            Some("Refresh token expired".into())
                        ),
                    ]
                ),
                ("openai-codex", vec![(None, None, Some("revoked".into()))]),
                ("moonshot", vec![]),
            ]
        );
        assert!(providers.iter().all(|provider| provider.adds_accounts && !provider.sign_out));
    }

    #[test]
    fn claude_code_and_codex_say_how_they_are_signed_in() {
        let claude = providers(
            ChatHarness::Claude,
            r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "me@example.com", "subscriptionType": "max"}"#,
            &[],
        )
        .unwrap();
        assert_eq!(claude[0].accounts[0].label.as_deref(), Some("me@example.com"));
        assert_eq!(claude[0].accounts[0].plan.as_deref(), Some("Claude Max"));
        let signed_out =
            providers(ChatHarness::Claude, r#"{"loggedIn": false}"#, &[]).unwrap();
        assert!(signed_out[0].accounts.is_empty());

        let codex = providers(ChatHarness::Codex, "Logged in using ChatGPT\n", &[]).unwrap();
        assert_eq!(codex[0].accounts[0].plan.as_deref(), Some("ChatGPT"));
        let key = providers(
            ChatHarness::Codex,
            "Logged in using an API key - sk-proj-***ABCD\n",
            &[],
        )
        .unwrap();
        assert_eq!(key[0].accounts[0].plan.as_deref(), Some("API key"));
        let none = providers(ChatHarness::Codex, "Not logged in\n", &[]).unwrap();
        assert!(none[0].accounts.is_empty());
        assert_eq!(providers(ChatHarness::Codex, "", &[]), None);
    }

    #[test]
    fn each_agent_signs_in_its_own_way() {
        assert_eq!(
            sign_in_way(ChatHarness::Claude, "anthropic", Some("console")),
            Ok(Way::Program {
                args: vec!["auth", "login", "--console"],
                provider_arg: false,
                key: None
            })
        );
        assert!(sign_in_way(ChatHarness::Claude, "openai", None).is_err());
        assert!(matches!(
            sign_in_way(ChatHarness::Codex, "openai", Some(API_KEY_METHOD)),
            Ok(Way::Program { key: Some(_), .. })
        ));
        assert!(sign_in_way(ChatHarness::Omp, "bad provider", None).is_err());
    }
}
