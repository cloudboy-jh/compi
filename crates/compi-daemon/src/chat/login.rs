//! Signing model providers in from a chat, and finding sign-ins the agent can no longer
//! use. Both run the agent's own commands where the chat runs, so they reach the same
//! program, credentials and broker as the chat: `omp login <provider>`, whose output
//! says the page to open, what it waits for and whether it worked, and `omp usage
//! --json`, whose `disabledCredentials` lists the sign-ins it gave up on.
//!
//! The login's output is read as it comes (`Output`): its prompts end a line without
//! ending it, and the answer goes to its standard input.
use super::launch::LAUNCH_MARK;
use compi_protocol::chat::ChatExpiredLogin;
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
        if !self.prompted && tail.ends_with(':') {
            self.prompted = true;
            signals.push(Signal::Prompt(tail.trim_end_matches(':').trim().to_owned()));
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
        if line == "Open this URL in your browser:" {
            self.url_next = true;
        } else if let Some(rest) = line.strip_prefix("Logged in to ") {
            let account = rest
                .split_once(" as ")
                .map(|(_, account)| account.to_owned());
            signals.push(Signal::Done(account));
        } else if let Some(reason) = line.strip_prefix("Login failed: ") {
            signals.push(Signal::Failed(reason.to_owned()));
        } else if line.starts_with("No credentials were stored") {
            signals.push(Signal::Failed(line.to_owned()));
        } else if !line.starts_with("Local shortcut") && !line.starts_with("Credentials saved to") {
            signals.push(Signal::Note(line.to_owned()));
        }
    }
}

/// `text` without terminal colour codes and the launch mark's separator.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if c != '\r' {
            out.push(c);
        }
    }
    out
}

/// The sign-ins `omp usage --json` says it disabled, with why.
pub(super) fn expired_logins(usage: &Value) -> Vec<ChatExpiredLogin> {
    usage["disabledCredentials"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|credential| {
            Some(ChatExpiredLogin {
                provider: credential["provider"].as_str()?.to_owned(),
                account: credential["email"]
                    .as_str()
                    .or_else(|| credential["accountId"].as_str())
                    .map(str::to_owned),
                reason: reason(credential["cause"].as_str().unwrap_or("Signed out")),
            })
        })
        .collect()
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
    }

    #[test]
    fn disabled_credentials_are_expired_logins_with_the_providers_reason() {
        let usage = serde_json::json!({
            "reports": [],
            "disabledCredentials": [{
                "id": 4,
                "provider": "anthropic",
                "type": "oauth",
                "cause": "oauth refresh failed: OAuthError: anthropic token refresh failed: 400 {\"error\": \"invalid_grant\", \"error_description\": \"Refresh token expired\"}",
                "email": "me@example.com"
            }, {
                "provider": "openai-codex",
                "cause": "revoked"
            }]
        });
        assert_eq!(
            expired_logins(&usage),
            [
                ChatExpiredLogin {
                    provider: "anthropic".into(),
                    account: Some("me@example.com".into()),
                    reason: "Refresh token expired".into(),
                },
                ChatExpiredLogin {
                    provider: "openai-codex".into(),
                    account: None,
                    reason: "revoked".into(),
                },
            ]
        );
        assert_eq!(expired_logins(&serde_json::json!({ "reports": [] })), []);
    }
}
