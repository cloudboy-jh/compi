//! How the transcript folds, after T3 Code's timeline (behaviour only; the rows keep
//! Compi's look). A settled turn's work before its last reply folds into one "Worked
//! for 2m" line; two or more calls in a row fold under one line naming what they did
//! ("Ran 2 commands, edited 3 files"). While a turn runs, its runs of calls show only
//! their latest. A turn's calls that started sub-agents show as one card, unfolded
//! while any agent runs. Nothing that failed, waits on the reader or has sub-agents
//! running folds away. The reader's choices are kept by the entry each line sits on.
use super::*;
use compi_protocol::chat::{ChatTool, ChatToolStatus};
use std::collections::{HashMap, HashSet};

/// How one transcript entry shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Shown {
    /// A folded turn's line, drawn over this entry, the first of the turn's work.
    pub(super) fold: Option<FoldLine>,
    /// A run of calls' line, drawn over this entry, the run's first.
    pub(super) group: Option<GroupLine>,
    /// The card of the turn's sub-agents, drawn in place of this entry, the turn's first
    /// call that started any.
    pub(super) card: Option<AgentCard>,
    pub(super) body: Body,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Body {
    /// As itself.
    #[default]
    Shown,
    /// As itself, inside an unfolded run of calls (or a running run's latest).
    Member,
    /// Folded away.
    Hidden,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct FoldLine {
    pub(super) label: String,
    pub(super) open: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct GroupLine {
    pub(super) label: String,
    /// The verb of what the run did most of, for its icon.
    pub(super) verb: &'static str,
    pub(super) open: bool,
    /// Its latest call failed.
    pub(super) failed: bool,
}

/// A turn's sub-agents as one card: the calls that started them, and whether it shows
/// them.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct AgentCard {
    pub(super) calls: Vec<usize>,
    pub(super) open: bool,
}

/// What the reader unfolded: folded turns and runs, by the entry their line sits on;
/// and sub-agent cards they opened or closed (`true` open), which otherwise show while
/// an agent runs.
#[derive(Clone, Debug, Default)]
pub(super) struct Unfolded {
    pub(super) turns: HashSet<usize>,
    pub(super) runs: HashSet<usize>,
    pub(super) cards: HashMap<usize, bool>,
}

/// Whether `entry` is a call that started sub-agents.
fn starts_agents(entry: &ChatEntry) -> bool {
    matches!(entry, ChatEntry::Tool(tool) if !tool.agents.is_empty())
}

/// The transcript's `entries` as they show: `working` while the last turn runs, with
/// `permission` the call a permission waits on.
pub(super) fn plan(
    entries: &[ChatEntry],
    working: bool,
    permission: Option<&str>,
    unfolded: &Unfolded,
) -> Vec<Shown> {
    let mut shown = vec![Shown::default(); entries.len()];
    let users: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| matches!(entry, ChatEntry::User { .. }))
        .map(|(index, _)| index)
        .collect();
    // Each turn: the entries after a prompt (or before the first) up to the next prompt.
    let mut starts = vec![0];
    starts.extend(users.iter().map(|&user| user + 1));
    for (turn, &start) in starts.iter().enumerate() {
        let end = users.get(turn).copied().unwrap_or(entries.len());
        if start >= end {
            continue;
        }
        let active = working && end == entries.len();
        plan_turn(
            entries,
            start..end,
            active,
            permission,
            unfolded,
            &mut shown,
        );
    }
    shown
}

/// Whether `entry` is a tool call whose sub-agents still run, or which a permission
/// waits on: it stays in view.
fn live(entry: &ChatEntry, permission: Option<&str>) -> bool {
    match entry {
        ChatEntry::Tool(tool) => {
            tool.agents.iter().any(|agent| agent.status.is_open())
                || permission == Some(tool.id.as_str())
        }
        _ => false,
    }
}

fn plan_turn(
    entries: &[ChatEntry],
    turn: Range<usize>,
    active: bool,
    permission: Option<&str>,
    unfolded: &Unfolded,
    shown: &mut [Shown],
) {
    let reply = turn
        .clone()
        .rev()
        .find(|&index| matches!(entries[index], ChatEntry::Agent { .. }));
    let failed = entries[turn.clone()]
        .iter()
        .any(|entry| matches!(entry, ChatEntry::Error { .. }));
    let held = entries[turn.clone()]
        .iter()
        .any(|entry| live(entry, permission));
    if let Some(reply) = reply
        && !active
        && !failed
        && !held
    {
        let work = turn.start..reply;
        let worked = entries[work.clone()]
            .iter()
            .any(|entry| matches!(entry, ChatEntry::Tool(_) | ChatEntry::Thought { .. }));
        if worked {
            let open = unfolded.turns.contains(&work.start);
            let stopped = entries[turn.clone()]
                .iter()
                .any(|entry| matches!(entry, ChatEntry::Cancelled));
            shown[work.start].fold = Some(FoldLine {
                label: fold_label(stopped, prompt_span(entries, &turn)),
                open,
            });
            if !open {
                for index in work {
                    shown[index].body = Body::Hidden;
                }
                return;
            }
        }
    }
    let cards: Vec<usize> = turn
        .clone()
        .filter(|&index| starts_agents(&entries[index]))
        .collect();
    if let Some(&first) = cards.first() {
        let running = cards.iter().any(|&index| live(&entries[index], None));
        for &index in &cards[1..] {
            shown[index].body = Body::Hidden;
        }
        shown[first].card = Some(AgentCard {
            open: unfolded.cards.get(&first).copied().unwrap_or(running),
            calls: cards,
        });
    }
    // A run is calls (but those that started sub-agents, which are the card) and the
    // thoughts between them.
    let member = |entry: &ChatEntry| {
        matches!(entry, ChatEntry::Tool(_) | ChatEntry::Thought { .. }) && !starts_agents(entry)
    };
    let mut index = turn.start;
    while index < turn.end {
        if !member(&entries[index]) {
            index += 1;
            continue;
        }
        let run_start = index;
        while index < turn.end && member(&entries[index]) {
            index += 1;
        }
        let run = run_start..index;
        let calls: Vec<&ChatTool> = entries[run.clone()]
            .iter()
            .filter_map(|entry| match entry {
                ChatEntry::Tool(tool) if !tool.waits_on_agents => Some(tool.as_ref()),
                _ => None,
            })
            .collect();
        if calls.len() < 2 {
            continue;
        }
        let open = unfolded.runs.contains(&run_start);
        let (label, verb) = run_label(&calls);
        shown[run_start].group = Some(GroupLine {
            label,
            verb,
            open,
            failed: calls
                .last()
                .is_some_and(|tool| tool.status == ChatToolStatus::Failed),
        });
        // A running turn shows its run's latest call under the line.
        let latest = run
            .clone()
            .rev()
            .find(|&at| matches!(&entries[at], ChatEntry::Tool(tool) if !tool.waits_on_agents));
        for at in run {
            let keep = open
                || live(&entries[at], permission)
                || (active && Some(at) == latest && index == turn.end);
            shown[at].body = if keep { Body::Member } else { Body::Hidden };
        }
    }
}

/// When the prompt that began `turn` was sent and when its turn ended, when the daemon
/// timed it (prompts a resumed session replays are not).
fn prompt_span(entries: &[ChatEntry], turn: &Range<usize>) -> Option<(u64, u64)> {
    match entries.get(turn.start.checked_sub(1)?)? {
        ChatEntry::User {
            sent_ms: Some(sent),
            ended_ms: Some(ended),
            ..
        } => Some((*sent, *ended)),
        _ => None,
    }
}

/// "Worked for 2m 10s", or "Stopped after 40s" when the reader stopped the turn: from
/// its prompt to its end. A turn a resumed session replays has no times, so it reads
/// "Worked" (or "Stopped").
fn fold_label(stopped: bool, prompt: Option<(u64, u64)>) -> String {
    match (
        prompt.map(|(start, end)| end.saturating_sub(start)),
        stopped,
    ) {
        (Some(ms), true) => format!("Stopped after {}", duration_label(ms)),
        (Some(ms), false) => format!("Worked for {}", duration_label(ms)),
        (None, true) => "Stopped".to_owned(),
        (None, false) => "Worked".to_owned(),
    }
}

/// What a run of calls did, by kind, the weightiest first, at most two kinds named:
/// "Ran 2 commands, edited 3 files and 2 more"; with the verb of the first named.
fn run_label(calls: &[&ChatTool]) -> (String, &'static str) {
    // Kinds in the order they are named, with how a count of them reads.
    const KINDS: [(&str, &str, &str); 8] = [
        ("Run", "ran", "command"),
        ("Edit", "edited", "file"),
        ("Delete", "deleted", "file"),
        ("Move", "moved", "file"),
        ("Agents", "ran", "sub-agent"),
        ("Fetch", "fetched", "page"),
        ("Search", "ran", "search"),
        ("Read", "read", "file"),
    ];
    let mut parts: Vec<(&'static str, String)> = Vec::new();
    for (verb, done, noun) in KINDS {
        let of_kind: Vec<&&ChatTool> = calls
            .iter()
            .filter(|tool| rows::tool_kind(tool).0 == verb)
            .collect();
        if of_kind.is_empty() {
            continue;
        }
        let count = match verb {
            // Files count once however often they were touched.
            "Edit" | "Read" | "Delete" | "Move" => {
                let mut paths: Vec<Option<String>> = Vec::new();
                for tool in &of_kind {
                    let path = tool_path(tool);
                    if path.is_none() || !paths.contains(&path) {
                        paths.push(path);
                    }
                }
                paths.len()
            }
            "Agents" => of_kind.iter().map(|tool| tool.agents.len().max(1)).sum(),
            _ => of_kind.len(),
        };
        let noun = match (noun, count) {
            (noun, 1) => noun.to_owned(),
            ("search", _) => "searches".to_owned(),
            (noun, _) => format!("{noun}s"),
        };
        parts.push((verb, format!("{done} {count} {noun}")));
    }
    let others = calls
        .iter()
        .filter(|tool| {
            !KINDS
                .iter()
                .any(|(verb, ..)| rows::tool_kind(tool).0 == *verb)
        })
        .count();
    if others > 0 {
        parts.push((
            "Tool",
            format!(
                "used {others} {}",
                if others == 1 { "tool" } else { "tools" }
            ),
        ));
    }
    let verb = parts.first().map_or("Tool", |(verb, _)| *verb);
    let mut label = parts
        .iter()
        .take(2)
        .map(|(_, part)| part.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if parts.len() > 2 {
        label.push_str(&format!(" and {} more", parts.len() - 2));
    }
    let mut letters = label.chars();
    let label = letters
        .next()
        .map(|first| first.to_uppercase().chain(letters).collect())
        .unwrap_or_default();
    (label, verb)
}

/// The file a call works on: where it reported, else its input's path.
fn tool_path(tool: &ChatTool) -> Option<String> {
    if let Some(path) = tool
        .locations
        .first()
        .or_else(|| tool.diffs.first().map(|edit| &edit.path))
    {
        return Some(path.clone());
    }
    let input: serde_json::Value = serde_json::from_str(tool.input.as_deref()?).ok()?;
    ["path", "file_path", "filePath"]
        .iter()
        .find_map(|key| input.get(*key)?.as_str())
        .map(str::to_owned)
}

/// What holds entry `index` out of view, by the entry it sits on.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Holders {
    pub(super) turn: Option<usize>,
    pub(super) run: Option<usize>,
    pub(super) card: Option<usize>,
}

/// The lines that fold entry `index` away: its turn's, its run's, and the sub-agent card
/// it is part of.
pub(super) fn holders(plan: &[Shown], index: usize) -> Holders {
    let mut holders = Holders::default();
    for at in (0..=index.min(plan.len().saturating_sub(1))).rev() {
        if holders.run.is_none() && plan[at].group.is_some() {
            holders.run = Some(at);
        }
        if holders.card.is_none()
            && plan[at]
                .card
                .as_ref()
                .is_some_and(|card| card.calls.contains(&index))
        {
            holders.card = Some(at);
        }
        if plan[at].fold.is_some() {
            holders.turn = Some(at);
            break;
        }
    }
    holders
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::chat::ChatAgent;

    fn user() -> ChatEntry {
        ChatEntry::User {
            text: "go".into(),
            files: Vec::new(),
            images: Vec::new(),
            sent_ms: None,
            ended_ms: None,
        }
    }

    fn reply() -> ChatEntry {
        ChatEntry::Agent {
            text: "done".into(),
        }
    }

    fn call(kind: &str, path: &str, started: u64, ended: u64) -> ChatEntry {
        ChatEntry::Tool(Box::new(ChatTool {
            id: format!("{kind}-{path}-{started}"),
            name: None,
            title: kind.into(),
            kind: kind.into(),
            status: ChatToolStatus::Completed,
            input: Some(format!(r#"{{ "path": "{path}" }}"#)),
            output: None,
            diffs: Vec::new(),
            locations: Vec::new(),
            started_ms: started,
            ended_ms: Some(ended),
            agents: Vec::new(),
            waits_on_agents: false,
        }))
    }

    fn bodies(plan: &[Shown]) -> Vec<char> {
        plan.iter()
            .map(|shown| match shown.body {
                Body::Shown => 's',
                Body::Member => 'm',
                Body::Hidden => '-',
            })
            .collect()
    }

    #[test]
    fn a_settled_turn_folds_its_work_before_the_last_reply() {
        let entries = [
            user(),
            call("execute", "", 1_000, 2_000),
            reply(),
            call("edit", "a.rs", 2_000, 3_000),
            call("edit", "a.rs", 3_000, 61_000),
            reply(),
        ];
        let plan = plan(&entries, false, None, &Unfolded::default());
        assert_eq!(bodies(&plan), ['s', '-', '-', '-', '-', 's']);
        assert_eq!(
            plan[1].fold,
            Some(FoldLine {
                label: "Worked".into(),
                open: false
            })
        );
        // Unfolded, the edits in a row fold under their own line.
        let mut unfolded = Unfolded::default();
        unfolded.turns.insert(1);
        let plan = super::plan(&entries, false, None, &unfolded);
        assert_eq!(bodies(&plan), ['s', 's', 's', '-', '-', 's']);
        assert_eq!(
            plan[3].group.as_ref().map(|group| group.label.as_str()),
            Some("Edited 1 file")
        );
    }

    #[test]
    fn a_timed_prompt_counts_the_whole_turn() {
        let timed = ChatEntry::User {
            text: "go".into(),
            files: Vec::new(),
            images: Vec::new(),
            sent_ms: Some(10_000),
            ended_ms: Some(25_000),
        };
        let entries = [timed, call("read", "a.rs", 12_000, 12_100), reply()];
        let plan = plan(&entries, false, None, &Unfolded::default());
        assert_eq!(
            plan[1].fold.as_ref().map(|fold| fold.label.as_str()),
            Some("Worked for 15s")
        );
    }

    #[test]
    fn a_running_turn_shows_its_runs_latest_call_and_never_folds() {
        let entries = [
            user(),
            call("read", "a.rs", 0, 1),
            call("read", "b.rs", 1, 2),
            call("execute", "", 2, 3),
        ];
        let plan = plan(&entries, true, None, &Unfolded::default());
        assert_eq!(bodies(&plan), ['s', '-', '-', 'm']);
        assert!(plan[1].fold.is_none());
        assert_eq!(
            plan[1].group.as_ref().map(|group| group.label.as_str()),
            Some("Ran 1 command, read 2 files")
        );
    }

    #[test]
    fn failed_turns_waiting_calls_and_running_agents_stay_in_view() {
        let failed = [
            user(),
            call("execute", "", 0, 1),
            ChatEntry::Error {
                message: "boom".into(),
            },
            reply(),
        ];
        assert!(
            plan(&failed, false, None, &Unfolded::default())[1]
                .fold
                .is_none()
        );
        let task = |status: ChatToolStatus| {
            let mut task = call("other", "", 0, 1);
            if let ChatEntry::Tool(tool) = &mut task {
                tool.id = format!("task-{status:?}");
                tool.agents.push(ChatAgent {
                    id: "A".into(),
                    agent: "task".into(),
                    status,
                    activity: None,
                    tool_count: 0,
                    started_ms: None,
                    duration_ms: 0,
                    cost: None,
                    model: None,
                    assignment: None,
                    result: None,
                    steps: Vec::new(),
                });
            }
            task
        };
        let mut entries = vec![
            user(),
            call("read", "a.rs", 0, 1),
            task(ChatToolStatus::Running),
            call("read", "b", 1, 2),
            task(ChatToolStatus::Completed),
            reply(),
        ];
        // The turn's sub-agents are one card, on its first call that started any, open
        // while one runs; the turn does not fold, and the card splits the reads' run.
        let shown = plan(&entries, false, None, &Unfolded::default());
        assert!(shown[1].fold.is_none());
        assert_eq!(bodies(&shown), ['s', 's', 's', 's', '-', 's']);
        assert_eq!(
            shown[2].card,
            Some(AgentCard {
                calls: vec![2, 4],
                open: true
            })
        );
        assert_eq!(holders(&shown, 4).card, Some(2));
        // Once all finished, the turn folds; unfolded, the card is closed until opened.
        entries[2] = task(ChatToolStatus::Completed);
        let mut unfolded = Unfolded::default();
        unfolded.turns.insert(1);
        let shown = plan(&entries, false, None, &unfolded);
        assert_eq!(shown[2].card.as_ref().map(|card| card.open), Some(false));
        unfolded.cards.insert(2, true);
        let shown = plan(&entries, false, None, &unfolded);
        assert_eq!(shown[2].card.as_ref().map(|card| card.open), Some(true));
    }

    #[test]
    fn a_run_names_its_weightiest_kinds_first() {
        let calls = [
            call("read", "a", 0, 1),
            call("read", "b", 0, 1),
            call("search", "", 0, 1),
            call("edit", "a", 0, 1),
            call("execute", "", 0, 1),
        ];
        let calls: Vec<&ChatTool> = calls
            .iter()
            .filter_map(|entry| match entry {
                ChatEntry::Tool(tool) => Some(tool.as_ref()),
                _ => None,
            })
            .collect();
        assert_eq!(
            run_label(&calls),
            ("Ran 1 command, edited 1 file and 2 more".to_owned(), "Run")
        );
    }
}
