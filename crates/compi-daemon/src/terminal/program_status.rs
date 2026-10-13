//! OSC 7501, the Program Status Protocol: parses reports and holds one terminal's records.
//! Specification: <https://www.superlogical.com/rex/docs/build/program-status> (revision 0.4).

use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use compi_protocol::program_status::{BlockedKind, ProgramState, ProgramStatusRecord};
use std::collections::BTreeMap;

/// Longest report, `OSC` through `ST`.
pub(super) const MAX_SEQUENCE_BYTES: usize = 4096;
/// Records one terminal keeps: the specification's minimum, which also bounds what one
/// terminal sends to every watching client.
pub const MAX_RECORDS: usize = 64;
const MAX_KEY_BYTES: usize = 16;
const MAX_MESSAGE_ENCODED_BYTES: usize = 2732;
const MAX_MESSAGE_BYTES: usize = 2048;
const MAX_TITLE_ENCODED_BYTES: usize = 256;
const MAX_TITLE_BYTES: usize = 192;
const MAX_APP_BYTES: usize = 32;
const MAX_ID_BYTES: usize = 128;
const MAX_ID_SEGMENT_BYTES: usize = 32;
const MAX_ID_DEPTH: usize = 8;
/// vte keeps this many `;`-separated OSC parameters and drops the rest, so a sequence
/// that reaches it may have lost bytes and cannot be checked against the limits.
const VTE_MAX_OSC_PARAMS: usize = 16;

const CAPABILITIES: &[u8] =
    b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth";

/// Standard base64; the specification makes padding optional.
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Command {
    /// Feature detection: answer with [`capability_reply`].
    Query,
    Report(Report),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Report {
    Set(ProgramStatusRecord),
    /// Removes the record and every record beneath it; `None` removes all of them.
    Clear {
        id: Option<String>,
    },
}

/// The feature detection answer, ended like the query.
pub(super) fn capability_reply(bell_terminated: bool) -> Vec<u8> {
    let mut reply = CAPABILITIES.to_vec();
    reply.extend_from_slice(if bell_terminated { b"\x07" } else { b"\x1b\\" });
    reply
}

/// Parses vte's `;`-separated OSC parameters, the first being `7501`. `None` ignores the
/// sequence: over a limit, undecodable text, an invalid `id`, or no recognised `state`.
pub(super) fn parse(params: &[&[u8]], bell_terminated: bool) -> Option<Command> {
    if params.len() < 2 || params.len() >= VTE_MAX_OSC_PARAMS {
        return None;
    }
    let terminator = if bell_terminated { 1 } else { 2 };
    let length =
        2 + params.iter().map(|param| param.len()).sum::<usize>() + params.len() - 1 + terminator;
    if length > MAX_SEQUENCE_BYTES {
        return None;
    }
    // Values never contain `;`, so a rejoined `;` only ever makes its pair malformed.
    let body = params[1..].join(&b";"[..]);
    if body.trim_ascii() == b"?" {
        return Some(Command::Query);
    }

    let mut fields = Fields::default();
    for pair in body.split(|byte| *byte == b':') {
        let Some(separator) = pair.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let key = pair[..separator].trim_ascii();
        let value = pair[separator + 1..].trim_ascii();
        if key.len() > MAX_KEY_BYTES {
            return None;
        }
        if key.is_empty()
            || !key.iter().all(u8::is_ascii_lowercase)
            || !value.iter().copied().all(is_value_byte)
        {
            continue;
        }
        let (slot, limit) = match key {
            b"state" => (&mut fields.state, usize::MAX),
            b"id" => (&mut fields.id, MAX_ID_BYTES),
            b"kind" => (&mut fields.kind, usize::MAX),
            b"progress" => (&mut fields.progress, usize::MAX),
            b"app" => (&mut fields.app, MAX_APP_BYTES),
            b"title" => (&mut fields.title, MAX_TITLE_ENCODED_BYTES),
            b"msg" => (&mut fields.message, MAX_MESSAGE_ENCODED_BYTES),
            _ => continue,
        };
        if value.len() > limit {
            return None;
        }
        *slot = Some(value);
    }

    let state = match fields.state? {
        b"idle" => Some(ProgramState::Idle),
        b"working" => Some(ProgramState::Working),
        b"done" => Some(ProgramState::Done),
        b"blocked" => Some(ProgramState::Blocked),
        b"error" => Some(ProgramState::Error),
        b"clear" => None,
        _ => return None,
    };
    let id = match fields.id {
        Some(value) => Some(parse_id(value)?),
        None => None,
    };
    let title = decode_text(fields.title, MAX_TITLE_BYTES)?;
    let message = decode_text(fields.message, MAX_MESSAGE_BYTES)?;
    let Some(state) = state else {
        return Some(Command::Report(Report::Clear { id }));
    };
    let kind = match (state, fields.kind) {
        (ProgramState::Blocked, Some(b"permission")) => Some(BlockedKind::Permission),
        (ProgramState::Blocked, Some(b"question")) => Some(BlockedKind::Question),
        (ProgramState::Blocked, Some(b"auth")) => Some(BlockedKind::Auth),
        _ => None,
    };
    let progress = fields
        .progress
        .filter(|_| matches!(state, ProgramState::Working | ProgramState::Blocked))
        .filter(|value| (1..=3).contains(&value.len()) && value.iter().all(u8::is_ascii_digit))
        .and_then(|value| std::str::from_utf8(value).ok()?.parse::<u8>().ok())
        .filter(|progress| *progress <= 100);
    let app = fields
        .app
        .filter(|value| !value.is_empty() && value.iter().copied().all(is_name_byte))
        .map(ascii_string);
    Some(Command::Report(Report::Set(ProgramStatusRecord {
        id,
        state,
        kind,
        progress,
        app,
        title,
        message,
    })))
}

#[derive(Default)]
struct Fields<'a> {
    state: Option<&'a [u8]>,
    id: Option<&'a [u8]>,
    kind: Option<&'a [u8]>,
    progress: Option<&'a [u8]>,
    app: Option<&'a [u8]>,
    title: Option<&'a [u8]>,
    message: Option<&'a [u8]>,
}

fn is_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"_.,+/=-".contains(&byte)
}

/// Bytes of `app` and of each `id` segment.
fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"_.+-".contains(&byte)
}

fn ascii_string(value: &[u8]) -> String {
    value.iter().copied().map(char::from).collect()
}

fn parse_id(value: &[u8]) -> Option<String> {
    let mut depth = 0;
    for segment in value.split(|byte| *byte == b'/') {
        depth += 1;
        if depth > MAX_ID_DEPTH
            || segment.is_empty()
            || segment.len() > MAX_ID_SEGMENT_BYTES
            || !segment.iter().copied().all(is_name_byte)
        {
            return None;
        }
    }
    Some(ascii_string(value))
}

/// `None` discards the report; `Some(None)` is an absent or empty value.
fn decode_text(value: Option<&[u8]>, max_bytes: usize) -> Option<Option<String>> {
    let Some(value) = value else {
        return Some(None);
    };
    let bytes = BASE64.decode(value).ok()?;
    if bytes.len() > max_bytes {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    if text.chars().any(char::is_control) {
        return None;
    }
    Some((!text.is_empty()).then_some(text))
}

/// One terminal's records. Records belong to the terminal, not a screen, and only a full
/// reset, `clear`, a lifecycle event or the user's acknowledgement removes them.
#[derive(Default)]
pub struct ProgramStatusSet {
    /// Keyed by `id`; the root record's key is empty, which no valid `id` is.
    records: BTreeMap<String, Entry>,
    clock: u64,
    generation: u64,
}

struct Entry {
    record: ProgramStatusRecord,
    updated: u64,
}

impl ProgramStatusSet {
    /// Changes whenever the records do.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Ordered by `id`, the root record first.
    pub fn records(&self) -> Vec<ProgramStatusRecord> {
        self.records
            .values()
            .map(|entry| entry.record.clone())
            .collect()
    }

    pub(super) fn apply(&mut self, report: Report) {
        match report {
            Report::Set(record) => {
                self.clock += 1;
                let key = record.id.clone().unwrap_or_default();
                if let Some(entry) = self.records.get_mut(&key) {
                    entry.updated = self.clock;
                    if entry.record != record {
                        entry.record = record;
                        self.changed();
                    }
                    return;
                }
                if self.records.len() >= MAX_RECORDS
                    && let Some(oldest) = self
                        .records
                        .iter()
                        .min_by_key(|(_, entry)| entry.updated)
                        .map(|(key, _)| key.clone())
                {
                    self.records.remove(&oldest);
                }
                self.records.insert(
                    key,
                    Entry {
                        record,
                        updated: self.clock,
                    },
                );
                self.changed();
            }
            Report::Clear { id: None } => self.clear(),
            Report::Clear { id: Some(id) } => {
                let prefix = format!("{id}/");
                self.retain(|key, _| key != id && !key.starts_with(&prefix));
            }
        }
    }

    /// A new shell prompt began or the terminal's process exited: work in progress and
    /// idle programs are gone, finished results stay for the user to find.
    pub fn end_activity(&mut self) {
        self.retain(|_, state| matches!(state, ProgramState::Done | ProgramState::Error));
    }

    /// The user has seen the finished results. Returns whether any were removed.
    pub fn acknowledge(&mut self) -> bool {
        let generation = self.generation;
        self.retain(|_, state| !matches!(state, ProgramState::Done | ProgramState::Error));
        generation != self.generation
    }

    pub fn clear(&mut self) {
        if !self.records.is_empty() {
            self.records.clear();
            self.changed();
        }
    }

    fn retain(&mut self, mut keep: impl FnMut(&str, ProgramState) -> bool) {
        let before = self.records.len();
        self.records
            .retain(|key, entry| keep(key, entry.record.state));
        if self.records.len() != before {
            self.changed();
        }
    }

    fn changed(&mut self) {
        self.generation += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    fn parse_body(body: &str) -> Option<Command> {
        let params: Vec<&[u8]> = std::iter::once(&b"7501"[..])
            .chain(body.split(';').map(str::as_bytes))
            .collect();
        parse(&params, false)
    }

    fn record(body: &str) -> ProgramStatusRecord {
        match parse_body(body) {
            Some(Command::Report(Report::Set(record))) => record,
            other => panic!("{body:?} parsed as {other:?}"),
        }
    }

    fn apply(set: &mut ProgramStatusSet, body: &str) {
        match parse_body(body) {
            Some(Command::Report(report)) => set.apply(report),
            other => panic!("{body:?} parsed as {other:?}"),
        }
    }

    fn ids(set: &ProgramStatusSet) -> Vec<Option<String>> {
        set.records().into_iter().map(|record| record.id).collect()
    }

    #[test]
    fn parses_specification_report_with_every_key() {
        let message = "Apply 3 to add, 1 to change, 0 to destroy?";
        let parsed = record(&format!(
            " state = blocked :kind=permission:app=terraform:id=plan/eu-west:progress=40:\
             title={}:msg={}",
            encode("EU West"),
            encode(message)
        ));
        assert_eq!(
            parsed,
            ProgramStatusRecord {
                id: Some("plan/eu-west".into()),
                state: ProgramState::Blocked,
                kind: Some(BlockedKind::Permission),
                progress: Some(40),
                app: Some("terraform".into()),
                title: Some("EU West".into()),
                message: Some(message.into()),
            }
        );
    }

    #[test]
    fn answers_feature_detection_with_the_query_terminator() {
        assert_eq!(parse_body("?"), Some(Command::Query));
        assert_eq!(
            capability_reply(true),
            b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x07"
        );
        assert!(capability_reply(false).ends_with(b"auth\x1b\\"));
    }

    #[test]
    fn skips_malformed_pairs_and_unknown_keys_and_keeps_the_last_repeat() {
        let parsed = record(
            "state=idle:noequals:=empty:Upper=x:app=bad value:future=1:app=first:state=working:app=cargo",
        );
        assert_eq!(parsed.state, ProgramState::Working);
        assert_eq!(parsed.app.as_deref(), Some("cargo"));
        // A `;` inside the body only spoils its own pair.
        let parsed = record("state=done:msg=a;b:app=cargo");
        assert_eq!(
            (parsed.message, parsed.app.as_deref()),
            (None, Some("cargo"))
        );
    }

    #[test]
    fn ignores_reports_without_a_recognised_state_or_with_an_invalid_id() {
        for body in [
            "app=cargo",
            "state=paused",
            "state=",
            "state=working:id=",
            "state=working:id=a//b",
            "state=working:id=/a",
            "state=working:id=a,b",
            "state=working:id=1/2/3/4/5/6/7/8/9",
        ] {
            assert_eq!(parse_body(body), None, "{body}");
        }
        assert_eq!(
            record("state=working:id=1/2/3/4/5/6/7/8").id.as_deref(),
            Some("1/2/3/4/5/6/7/8")
        );
    }

    #[test]
    fn discards_the_whole_report_for_bad_text() {
        for body in [
            "state=done:msg=QQ=QQ".to_owned(),
            "state=done:msg=A".to_owned(),
            "state=done:msg=-_".to_owned(),
            format!("state=done:msg={}", encode("two\nlines")),
            format!("state=done:title={}", encode("tab\there")),
            format!("state=done:msg={}", encode("c1 \u{85} control")),
            format!(
                "state=done:msg={}",
                base64::prelude::BASE64_STANDARD.encode([0xff, 0xfe])
            ),
            format!("state=clear:msg={}", encode("bell\u{7}")),
        ] {
            assert_eq!(parse_body(&body), None, "{body}");
        }
        assert_eq!(
            record("state=done:msg=SGk").message.as_deref(),
            Some("Hi"),
            "padding is optional"
        );
        assert_eq!(record("state=done:msg=").message, None);
    }

    #[test]
    fn enforces_every_limit_at_its_boundary() {
        let at = |key: &str, value: &str| parse_body(&format!("state=done:{key}={value}"));
        let at_limit = [
            ("msg", encode(&"a".repeat(MAX_MESSAGE_BYTES))),
            ("title", encode(&"a".repeat(MAX_TITLE_BYTES))),
            ("app", "a".repeat(MAX_APP_BYTES)),
            ("id", ["abcdefghijklmno"; 8].join("/") + "x"),
            ("id", format!("{}/{}", "a".repeat(32), "b".repeat(32))),
        ];
        let over_limit = [
            // 2049 bytes still encode to 2732 characters: the decoded size is checked too.
            ("msg", encode(&"a".repeat(MAX_MESSAGE_BYTES + 1))),
            ("msg", "A".repeat(MAX_MESSAGE_ENCODED_BYTES + 1)),
            ("title", encode(&"a".repeat(MAX_TITLE_BYTES + 1))),
            ("title", "A".repeat(MAX_TITLE_ENCODED_BYTES + 1)),
            ("app", "a".repeat(MAX_APP_BYTES + 1)),
            ("id", ["abcdefghijklmno"; 8].join("/") + "xy"),
            ("id", format!("{}/{}", "a".repeat(32), "b".repeat(33))),
        ];
        for (key, value) in &at_limit {
            assert!(at(key, value).is_some(), "{key} of {} bytes", value.len());
        }
        for (key, value) in &over_limit {
            assert_eq!(at(key, value), None, "{key} of {} bytes", value.len());
        }
        assert_eq!(at_limit[3].1.len(), MAX_ID_BYTES);
        assert!(at(&"k".repeat(MAX_KEY_BYTES), "1").is_some());
        assert_eq!(at(&"k".repeat(MAX_KEY_BYTES + 1), "1"), None);
    }

    #[test]
    fn whole_sequence_limit_counts_introducer_and_terminator() {
        // ESC ] 7501 ; body ESC \ is the body plus nine bytes.
        let fits = format!("state=done:pad={}", "a".repeat(MAX_SEQUENCE_BYTES - 9 - 15));
        assert_eq!(fits.len() + 9, MAX_SEQUENCE_BYTES);
        assert!(parse_body(&fits).is_some());
        assert_eq!(parse_body(&format!("{fits}a")), None);
        let params: [&[u8]; 2] = [b"7501", fits.as_bytes()];
        assert!(parse(&params, true).is_some());
        let longer = format!("{fits}a");
        let params: [&[u8]; 2] = [b"7501", longer.as_bytes()];
        assert!(
            parse(&params, true).is_some(),
            "BEL is one byte shorter than ST"
        );
        let truncated = vec![&b"state=done"[..]; VTE_MAX_OSC_PARAMS - 1];
        let params: Vec<&[u8]> = std::iter::once(&b"7501"[..]).chain(truncated).collect();
        assert_eq!(parse(&params, true), None);
    }

    #[test]
    fn kind_and_progress_apply_only_to_their_states() {
        let blocked = record("state=blocked:kind=auth:progress=100");
        assert_eq!(
            (blocked.kind, blocked.progress),
            (Some(BlockedKind::Auth), Some(100))
        );
        let working = record("state=working:kind=auth:progress=0");
        assert_eq!((working.kind, working.progress), (None, Some(0)));
        let done = record("state=done:kind=auth:progress=50");
        assert_eq!((done.kind, done.progress), (None, None));
        for progress in ["101", "-1", "4.5", "1000", "", "+5"] {
            assert_eq!(
                record(&format!("state=working:progress={progress}")).progress,
                None
            );
        }
        assert_eq!(record("state=blocked:kind=other").kind, None);
        assert_eq!(record("state=done:app=a/b").app, None);
    }

    #[test]
    fn each_report_replaces_its_record_and_clear_removes_subtrees() {
        let mut set = ProgramStatusSet::default();
        apply(&mut set, "state=working:app=deploy");
        apply(&mut set, "state=working:id=us-east:progress=40");
        apply(&mut set, "state=blocked:id=us-east/canary");
        apply(&mut set, "state=working:id=us-east-2");
        assert_eq!(set.records()[0].id, None, "root comes first");
        apply(&mut set, "state=done:id=us-east");
        assert_eq!(
            set.records()[1].progress,
            None,
            "keys missing from a report are dropped"
        );

        apply(&mut set, "state=clear:id=us-east");
        assert_eq!(ids(&set), [None, Some("us-east-2".into())]);
        let generation = set.generation();
        apply(&mut set, "state=clear:id=missing");
        assert_eq!(set.generation(), generation, "no change, no new generation");
        apply(&mut set, "state=working:id=us-east-2");
        assert_eq!(
            set.generation(),
            generation,
            "an identical report changes nothing"
        );
        apply(&mut set, "state=clear");
        assert!(set.records().is_empty());
        assert!(set.generation() > generation);
    }

    #[test]
    fn evicts_the_least_recently_updated_record_at_the_cap() {
        let mut set = ProgramStatusSet::default();
        for index in 0..MAX_RECORDS {
            apply(&mut set, &format!("state=working:id=r{index}"));
        }
        // Repeating a report counts as an update even when nothing changed.
        apply(&mut set, "state=working:id=r0");
        apply(&mut set, "state=working:id=new");
        let ids = ids(&set);
        assert_eq!(ids.len(), MAX_RECORDS);
        assert!(ids.contains(&Some("r0".into())));
        assert!(!ids.contains(&Some("r1".into())));
        assert!(ids.contains(&Some("new".into())));
    }

    #[test]
    fn lifecycle_keeps_finished_results_until_acknowledged() {
        let mut set = ProgramStatusSet::default();
        for state in ["idle", "working", "blocked", "done", "error"] {
            apply(&mut set, &format!("state={state}:id={state}"));
        }
        set.end_activity();
        assert_eq!(ids(&set), [Some("done".into()), Some("error".into())]);
        apply(&mut set, "state=working:id=next");
        assert!(set.acknowledge());
        assert_eq!(ids(&set), [Some("next".into())]);
        assert!(!set.acknowledge());
    }
}
