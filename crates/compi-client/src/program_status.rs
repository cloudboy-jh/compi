//! Pure presentation of program status records (OSC 7501); never talks to the daemon.
//!
//! Record text is untrusted program output. The daemon refused control characters;
//! [`display_text`] also removes the invisible and direction-changing characters that
//! could make one record's text pose as another's.

use compi_protocol::program_status::{BlockedKind, ProgramState, ProgramStatusRecord};

/// Longest message shown in one line; the specification lets terminals shorten it.
const MAX_MESSAGE_CHARS: usize = 160;

/// What a pane or tab shows: the state that most needs the user, with the progress of
/// the record reporting it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Headline {
    pub state: ProgramState,
    pub progress: Option<u8>,
}

/// Blocked work needs the user now, a failure next; finished results wait.
fn urgency(state: ProgramState) -> u8 {
    match state {
        ProgramState::Blocked => 4,
        ProgramState::Error => 3,
        ProgramState::Working => 2,
        ProgramState::Done => 1,
        ProgramState::Idle => 0,
    }
}

/// The most urgent record; the root record wins a tie.
pub fn headline(records: &[ProgramStatusRecord]) -> Option<Headline> {
    records
        .iter()
        .rev()
        .max_by_key(|record| urgency(record.state))
        .map(|record| Headline {
            state: record.state,
            progress: record.progress,
        })
}

/// The most urgent of several panes' headlines; the first wins a tie.
pub fn combine(headlines: impl IntoIterator<Item = Headline>) -> Option<Headline> {
    headlines
        .into_iter()
        .fold(None, |best: Option<Headline>, next| match best {
            Some(best) if urgency(best.state) >= urgency(next.state) => Some(best),
            _ => Some(next),
        })
}

pub fn state_label(state: ProgramState) -> &'static str {
    match state {
        ProgramState::Idle => "Idle",
        ProgramState::Working => "Working",
        ProgramState::Done => "Done",
        ProgramState::Blocked => "Blocked",
        ProgramState::Error => "Error",
    }
}

/// `Blocked · 40%`, or just the state.
pub fn headline_label(headline: Headline) -> String {
    match headline.progress {
        Some(progress) => format!("{} · {progress}%", state_label(headline.state)),
        None => state_label(headline.state).to_owned(),
    }
}

/// Program text without characters that are invisible or change text direction.
pub fn display_text(text: &str) -> String {
    text.chars()
        .filter(|character| {
            !matches!(
                character,
                '\u{061C}'
                    | '\u{200B}'..='\u{200F}'
                    | '\u{202A}'..='\u{202E}'
                    | '\u{2060}'..='\u{2064}'
                    | '\u{2066}'..='\u{2069}'
                    | '\u{FEFF}'
            )
        })
        .collect()
}

/// The record's `app`, or that of its nearest ancestor that has one (`a/b` → `a` →
/// root); ancestors need not exist.
pub fn inherited_app<'a>(
    record: &'a ProgramStatusRecord,
    records: &'a [ProgramStatusRecord],
) -> Option<&'a str> {
    if let Some(app) = &record.app {
        return Some(app);
    }
    let mut id = record.id.as_deref();
    while let Some(current) = id {
        let parent = current.rsplit_once('/').map(|(parent, _)| parent);
        if let Some(app) = records
            .iter()
            .find(|candidate| candidate.id.as_deref() == parent)
            .and_then(|ancestor| ancestor.app.as_deref())
        {
            return Some(app);
        }
        id = parent;
    }
    None
}

/// One line per record, root first: who reports it, its state, and its message.
pub fn record_lines(records: &[ProgramStatusRecord]) -> Vec<String> {
    records
        .iter()
        .map(|record| {
            let label = record
                .title
                .as_deref()
                .map(display_text)
                .or_else(|| record.id.clone());
            let name = match (inherited_app(record, records), label) {
                (Some(app), Some(label)) => format!("{label} ({app})"),
                (Some(app), None) => app.to_owned(),
                (None, Some(label)) => label,
                (None, None) => "Program".into(),
            };
            let mut line = format!("{name}: {}", state_label(record.state));
            if let Some(kind) = record.kind {
                line.push_str(match kind {
                    BlockedKind::Permission => ", needs permission",
                    BlockedKind::Question => ", has a question",
                    BlockedKind::Auth => ", needs sign-in",
                });
            }
            if let Some(progress) = record.progress {
                line.push_str(&format!(" · {progress}%"));
            }
            if let Some(message) = &record.message {
                let message = display_text(message);
                line.push_str(" — ");
                if message.chars().count() > MAX_MESSAGE_CHARS {
                    line.extend(message.chars().take(MAX_MESSAGE_CHARS - 1));
                    line.push('…');
                } else {
                    line.push_str(&message);
                }
            }
            line
        })
        .collect()
}

/// Whether the user has unseen results that a keypress in the pane acknowledges.
pub fn has_results(records: &[ProgramStatusRecord]) -> bool {
    records
        .iter()
        .any(|record| matches!(record.state, ProgramState::Done | ProgramState::Error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: Option<&str>, state: ProgramState) -> ProgramStatusRecord {
        ProgramStatusRecord {
            id: id.map(str::to_owned),
            state,
            kind: None,
            progress: None,
            app: None,
            title: None,
            message: None,
        }
    }

    #[test]
    fn headline_prefers_what_needs_the_user() {
        let mut working = record(None, ProgramState::Working);
        working.progress = Some(40);
        let mut blocked = record(Some("eu"), ProgramState::Blocked);
        blocked.progress = Some(70);
        let records = [
            working.clone(),
            record(Some("us"), ProgramState::Done),
            blocked,
        ];
        assert_eq!(
            headline(&records),
            Some(Headline {
                state: ProgramState::Blocked,
                progress: Some(70)
            })
        );
        let tied = [working, record(Some("us"), ProgramState::Working)];
        assert_eq!(
            headline(&tied).unwrap().progress,
            Some(40),
            "root wins a tie"
        );
        assert_eq!(headline(&[]), None);

        let done = Headline {
            state: ProgramState::Done,
            progress: None,
        };
        let error = Headline {
            state: ProgramState::Error,
            progress: None,
        };
        assert_eq!(combine([done, error, done]), Some(error));
        assert_eq!(combine([]), None);
        assert_eq!(headline_label(headline(&records).unwrap()), "Blocked · 70%");
    }

    #[test]
    fn records_inherit_app_from_their_nearest_ancestor() {
        let mut root = record(None, ProgramState::Working);
        root.app = Some("deploy".into());
        let mut region = record(Some("eu"), ProgramState::Working);
        region.app = Some("kubectl".into());
        let records = [
            root,
            region,
            record(Some("eu/canary"), ProgramState::Blocked),
            record(Some("us/canary"), ProgramState::Working),
        ];
        assert_eq!(inherited_app(&records[2], &records), Some("kubectl"));
        // `us` does not exist; the root is the nearest ancestor with an app.
        assert_eq!(inherited_app(&records[3], &records), Some("deploy"));
        assert_eq!(
            inherited_app(&record(Some("us"), ProgramState::Done), &records),
            Some("deploy")
        );
    }

    #[test]
    fn lines_name_the_program_and_disarm_its_text() {
        let mut blocked = record(None, ProgramState::Blocked);
        blocked.app = Some("terraform".into());
        blocked.kind = Some(BlockedKind::Permission);
        blocked.progress = Some(40);
        blocked.message = Some("Apply\u{202E}lacol\u{2066}?".into());
        let mut child = record(Some("eu"), ProgramState::Done);
        child.title = Some("EU\u{200B} West".into());
        child.message = Some("x".repeat(MAX_MESSAGE_CHARS + 1));
        let lines = record_lines(&[blocked, child, record(Some("w"), ProgramState::Idle)]);
        assert_eq!(
            lines[0],
            "terraform: Blocked, needs permission · 40% — Applylacol?"
        );
        assert_eq!(
            lines[1],
            format!(
                "EU West (terraform): Done — {}…",
                "x".repeat(MAX_MESSAGE_CHARS - 1)
            )
        );
        assert_eq!(lines[2], "w (terraform): Idle");
    }
}
