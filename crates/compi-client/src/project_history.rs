//! Per-window shell directory visits, ranked without filesystem discovery.

use serde::{Deserialize, Serialize};

const MAX_PATHS: usize = 256;
const MAX_PATH_BYTES: usize = 1024;
const MAX_VISITS: u32 = 10_000;
const MAX_TICK: u32 = 1_000_000;
const MAX_QUERY_BYTES: usize = 128;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectHistory {
    #[serde(default)]
    entries: Vec<ProjectVisit>,
    #[serde(default)]
    tick: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ProjectVisit {
    path: String,
    visits: u32,
    last_visit: u32,
}

impl ProjectHistory {
    /// Record a shell-reported cwd transition. Relative, native Windows, and
    /// control-character paths are ignored; no filesystem access is performed.
    pub fn record(&mut self, path: &str) -> bool {
        if !valid_path(path) {
            return false;
        }
        let trimmed = path.trim_end_matches('/');
        let path = if trimmed.is_empty() { "/" } else { trimmed };
        if self.tick >= MAX_TICK {
            self.compact_ticks();
        }
        self.tick += 1;
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.path == path) {
            entry.visits = entry.visits.saturating_add(1).min(MAX_VISITS);
            entry.last_visit = self.tick;
        } else {
            if self.entries.len() == MAX_PATHS {
                let oldest = self
                    .entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, entry)| (entry.last_visit, entry.visits, &entry.path))
                    .map(|(index, _)| index)
                    .expect("full project history has an entry");
                self.entries.swap_remove(oldest);
            }
            self.entries.push(ProjectVisit {
                path: path.to_owned(),
                visits: 1,
                last_visit: self.tick,
            });
        }
        true
    }

    /// Rank fuzzy path matches by match quality, then frequency and recency.
    /// An empty query ranks all previously visited directories by frecency.
    pub fn search(&self, query: &str) -> Vec<&str> {
        if query.len() > MAX_QUERY_BYTES {
            return Vec::new();
        }
        let query = query.as_bytes();
        let mut matches: Vec<_> = self
            .entries
            .iter()
            .filter_map(|entry| {
                let quality = if query.is_empty() {
                    0
                } else {
                    let full = fuzzy_score(entry.path.as_bytes(), query);
                    let basename = entry.path.rsplit('/').next().unwrap_or("");
                    full.max(fuzzy_score(basename.as_bytes(), query).map(|score| score + 32))?
                };
                let age = self.tick.saturating_sub(entry.last_visit).min(32);
                let frecency = entry.visits.min(100) * 4 + (32 - age) * 8;
                Some((quality, frecency, entry.last_visit, entry.path.as_str()))
            })
            .collect();
        matches.sort_unstable_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| b.2.cmp(&a.2))
                .then_with(|| a.3.cmp(b.3))
        });
        matches.into_iter().map(|(_, _, _, path)| path).collect()
    }

    /// Correct file-derived values before they become live client state.
    pub(crate) fn sanitize(&mut self) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| valid_path(&entry.path));
        let mut changed = self.entries.len() != before;
        for entry in &mut self.entries {
            let trimmed = entry.path.trim_end_matches('/');
            let len = if trimmed.is_empty() { 1 } else { trimmed.len() };
            changed |= entry.path.len() != len;
            entry.path.truncate(len);
            let visits = entry.visits.clamp(1, MAX_VISITS);
            let last_visit = entry.last_visit.max(1);
            changed |= entry.visits != visits || entry.last_visit != last_visit;
            entry.visits = visits;
            entry.last_visit = last_visit;
        }
        let duplicates = {
            let mut paths = std::collections::HashSet::new();
            self.entries
                .iter()
                .any(|entry| !paths.insert(entry.path.as_str()))
        };
        if duplicates || self.entries.len() > MAX_PATHS {
            self.entries.sort_unstable_by(|a, b| {
                b.last_visit
                    .cmp(&a.last_visit)
                    .then_with(|| b.visits.cmp(&a.visits))
                    .then_with(|| a.path.cmp(&b.path))
            });
            if duplicates {
                let mut seen = std::collections::HashSet::new();
                self.entries.retain(|entry| seen.insert(entry.path.clone()));
            }
            self.entries.truncate(MAX_PATHS);
            changed = true;
        }
        let tick = self.tick.max(
            self.entries
                .iter()
                .map(|entry| entry.last_visit)
                .max()
                .unwrap_or(0),
        );
        changed |= self.tick != tick;
        self.tick = tick;
        if self.tick >= MAX_TICK {
            self.compact_ticks();
            changed = true;
        }
        changed
    }

    fn compact_ticks(&mut self) {
        let mut order: Vec<_> = (0..self.entries.len()).collect();
        order.sort_unstable_by(|&a, &b| {
            self.entries[a]
                .last_visit
                .cmp(&self.entries[b].last_visit)
                .then_with(|| self.entries[a].path.cmp(&self.entries[b].path))
        });
        for (index, entry) in order.into_iter().enumerate() {
            self.entries[entry].last_visit = index as u32 + 1;
        }
        self.tick = self.entries.len() as u32;
    }
}

fn valid_path(path: &str) -> bool {
    path.starts_with('/') && path.len() <= MAX_PATH_BYTES && !path.chars().any(char::is_control)
}

fn fuzzy_score(path: &[u8], query: &[u8]) -> Option<i32> {
    let mut matched = 0;
    let mut previous_match = None;
    let mut score = 0;
    for (index, &byte) in path.iter().enumerate() {
        if !byte.eq_ignore_ascii_case(&query[matched]) {
            continue;
        }
        score += 8;
        if index == 0 || matches!(path[index - 1], b'/' | b'-' | b'_' | b'.' | b' ') {
            score += 10;
        }
        if previous_match == Some(index.saturating_sub(1)) {
            score += 12;
        }
        previous_match = Some(index);
        matched += 1;
        if matched == query.len() {
            return Some(score);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_visits_and_recent_transitions_rank_for_empty_query() {
        let mut history = ProjectHistory::default();
        history.record("/work/frequent");
        history.record("/work/recent");
        history.record("/work/frequent");
        history.record("/work/recent");
        history.record("/work/frequent");
        assert_eq!(history.search(""), ["/work/frequent", "/work/recent"]);
        history.record("/work/recent");
        assert_eq!(history.search(""), ["/work/recent", "/work/frequent"]);
        assert_eq!(history.entries.len(), 2);
    }

    #[test]
    fn fuzzy_basename_quality_precedes_frequency() {
        let mut history = ProjectHistory::default();
        for _ in 0..20 {
            history.record("/p/r/o/j");
        }
        history.record("/work/project");
        assert_eq!(history.search("proj"), ["/work/project", "/p/r/o/j"]);
        assert_eq!(history.search("PROJECT"), ["/work/project"]);
        assert_eq!(history.search("not-here"), Vec::<&str>::new());
    }

    #[test]
    fn equal_scores_have_deterministic_path_order() {
        let mut history: ProjectHistory = serde_json::from_value(serde_json::json!({
            "tick": 4,
            "entries": [
                {"path": "/b/project", "visits": 2, "last_visit": 4},
                {"path": "/a/project", "visits": 2, "last_visit": 4}
            ]
        }))
        .unwrap();
        assert!(!history.sanitize());
        assert_eq!(history.search("proj"), ["/a/project", "/b/project"]);
    }

    #[test]
    fn bounded_paths_counters_and_invalid_shell_paths() {
        let mut history = ProjectHistory::default();
        for path in ["relative", "C:\\Windows", "", "/bad\nname"] {
            assert!(!history.record(path));
        }
        assert!(history.record("/mnt/c/code/"));
        assert!(history.record("/mnt/c/code"));
        assert_eq!(history.entries.len(), 1);
        for _ in 0..MAX_VISITS + 1 {
            history.record("/mnt/c/code");
        }
        assert_eq!(history.entries[0].visits, MAX_VISITS);
        for index in 0..MAX_PATHS + 10 {
            history.record(&format!("/projects/{index}"));
        }
        assert_eq!(history.entries.len(), MAX_PATHS);
        assert!(!history.search("").contains(&"/mnt/c/code"));
        assert!(history.tick <= MAX_TICK);
    }

    #[test]
    fn persisted_invalid_entries_are_pruned_and_large_counters_compacted() {
        let mut history: ProjectHistory = serde_json::from_value(serde_json::json!({
            "tick": 4294967295u32,
            "entries": [
                {"path": "relative", "visits": 2, "last_visit": 1},
                {"path": "/work/", "visits": 4294967295u32, "last_visit": 7},
                {"path": "/work", "visits": 3, "last_visit": 6}
            ]
        }))
        .unwrap();
        assert!(history.sanitize());
        assert_eq!(history.search(""), ["/work"]);
        assert_eq!(history.entries[0].visits, MAX_VISITS);
        assert!(history.record("/another"));
        assert_eq!(history.search(""), ["/work", "/another"]);
    }
}
