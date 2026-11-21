//! New native agent chats, as the palette's New chat and `compi chat` start them, and
//! how the palette's Threads and `compi threads` describe a thread.
use compi_protocol::ContentEnvironment;
use compi_protocol::chat::{AgentEntry, ChatContent, ChatHarness, ThreadActivity, ThreadInfo};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_CHAT: AtomicU64 = AtomicU64::new(0);

/// A new chat with `harness` in `folder` of `environment`, under an ID no other chat has:
/// the time, this process and a counter, in hex.
pub fn new_chat(
    harness: ChatHarness,
    environment: ContentEnvironment,
    folder: String,
) -> ChatContent {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let count = NEXT_CHAT.fetch_add(1, Ordering::Relaxed);
    ChatContent {
        id: format!("chat-{nanos:x}-{:x}-{count:x}", std::process::id()),
        harness,
        environment,
        folder,
        session: None,
    }
}

/// Every agent, as a sentence lists them, each as `show` writes it: `omp, claude or
/// codex`.
pub fn agent_list(show: impl Fn(&AgentEntry) -> String) -> String {
    let mut agents: Vec<String> = ChatHarness::ALL
        .into_iter()
        .map(|agent| show(agent.entry()))
        .collect();
    let last = agents.pop().unwrap_or_default();
    if agents.is_empty() {
        last
    } else {
        format!("{} or {last}", agents.join(", "))
    }
}

/// A thread's title, else "New thread".
pub fn thread_title(thread: &ThreadInfo) -> &str {
    thread
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("New thread")
}

/// What a thread is doing: Ended once no pane holds it; otherwise Working or Waiting
/// while a turn runs or asks, else Open.
pub fn thread_status(thread: &ThreadInfo) -> &'static str {
    if !thread.open {
        return "Ended";
    }
    match thread.activity {
        ThreadActivity::Working => "Working",
        ThreadActivity::Waiting => "Waiting",
        ThreadActivity::Idle => "Open",
    }
}

/// How long before `now_ms` the time `then_ms` was, both in milliseconds since the
/// epoch: "now" within a minute, then "5m", "2h", "3d".
pub fn age_label(then_ms: u64, now_ms: u64) -> String {
    let minutes = now_ms.saturating_sub(then_ms) / 60_000;
    match minutes {
        0 => "now".to_owned(),
        1..60 => format!("{minutes}m"),
        60..1440 => format!("{}h", minutes / 60),
        _ => format!("{}d", minutes / 1440),
    }
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_chats_are_valid_and_distinct() {
        let first = new_chat(ChatHarness::Omp, ContentEnvironment::Host, "/work".into());
        let second = new_chat(
            ChatHarness::Claude,
            ContentEnvironment::Host,
            "/work".into(),
        );
        assert!(first.validate().is_ok(), "{}", first.id);
        assert_ne!(first.id, second.id);
        assert_eq!(first.harness, ChatHarness::Omp);
        assert_eq!(second.harness, ChatHarness::Claude);
        assert_eq!(first.session, None);
    }

    #[test]
    fn agent_lists_name_every_agent() {
        assert_eq!(
            agent_list(|agent| agent.program.to_owned()),
            "omp, claude, codex or opencode"
        );
        assert_eq!(
            agent_list(|agent| format!("\"{}\"", agent.program)),
            "\"omp\", \"claude\", \"codex\" or \"opencode\""
        );
    }

    #[test]
    fn ages_read_in_the_largest_whole_unit() {
        let now = 10 * 86_400_000;
        assert_eq!(age_label(now, now), "now");
        assert_eq!(age_label(now - 59_999, now), "now");
        assert_eq!(age_label(now - 5 * 60_000, now), "5m");
        assert_eq!(age_label(now - 59 * 60_000, now), "59m");
        assert_eq!(age_label(now - 2 * 3_600_000 - 1, now), "2h");
        assert_eq!(age_label(now - 86_400_000 + 1, now), "23h");
        assert_eq!(age_label(now - 3 * 86_400_000, now), "3d");
        // A clock behind the thread's time reads as now.
        assert_eq!(age_label(now + 5_000, now), "now");
    }

    #[test]
    fn threads_read_by_title_and_status() {
        let mut thread = ThreadInfo {
            chat: new_chat(ChatHarness::Omp, ContentEnvironment::Host, "/work".into()),
            title: Some("  ".into()),
            created_ms: 0,
            updated_ms: 0,
            activity: ThreadActivity::Idle,
            project: None,
            open: false,
        };
        assert_eq!(thread_title(&thread), "New thread");
        assert_eq!(thread_status(&thread), "Ended");
        thread.title = Some("Fix the build".into());
        thread.open = true;
        assert_eq!(thread_title(&thread), "Fix the build");
        assert_eq!(thread_status(&thread), "Open");
        thread.activity = ThreadActivity::Waiting;
        assert_eq!(thread_status(&thread), "Waiting");
        thread.activity = ThreadActivity::Working;
        assert_eq!(thread_status(&thread), "Working");
    }
}
