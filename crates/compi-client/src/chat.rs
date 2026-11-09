//! New native agent chats, as the palette's OMP chat and `compi chat` start them.
use compi_protocol::ContentEnvironment;
use compi_protocol::chat::{ChatContent, ChatHarness};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_CHAT: AtomicU64 = AtomicU64::new(0);

/// A new OMP chat in `folder` of `environment`, under an ID no other chat has: the time,
/// this process and a counter, in hex.
pub fn new_chat(environment: ContentEnvironment, folder: String) -> ChatContent {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let count = NEXT_CHAT.fetch_add(1, Ordering::Relaxed);
    ChatContent {
        id: format!("chat-{nanos:x}-{:x}-{count:x}", std::process::id()),
        harness: ChatHarness::Omp,
        environment,
        folder,
        session: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_chats_are_valid_and_distinct() {
        let first = new_chat(ContentEnvironment::Host, "/work".into());
        let second = new_chat(ContentEnvironment::Host, "/work".into());
        assert!(first.validate().is_ok(), "{}", first.id);
        assert_ne!(first.id, second.id);
        assert_eq!(first.harness, ChatHarness::Omp);
        assert_eq!(first.session, None);
    }
}
