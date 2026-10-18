//! Monitor's processes by app, kept pure: the Processes table grouped by application
//! (one row per app, its members beneath it when expanded) and Overview's memory by
//! app.
//!
//! Processes of one app share its `AppGroup`; a process without one stands alone under
//! its own name, and so does an app with a single process shown. A group's values are
//! the sums over its members shown (those the filter admits and the search keeps);
//! unavailable values are left out of a sum, and a group whose members all lack a value
//! lacks it too.

use super::{Entry, Filter, ProcessKey, Search, Sort, arrange, arrange_held, compare_entries, key};
use compi_protocol::system_metrics::Process;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

/// Names a table row across samples and re-sorts.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RowKey {
    Process(ProcessKey),
    /// An app group with several processes shown, by its id.
    Group(String),
}

/// What a table row shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// A process, by its index in the processes.
    Process(usize),
    /// An app, by its index in [`Arranged::groups`].
    Group(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableRow {
    pub kind: RowKind,
    /// A process shown under its expanded app.
    pub member: bool,
}

/// Whose an app's processes are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupUser {
    /// Every member shown runs as this user.
    Shared(String),
    /// Members run as different users, or some user is unknown.
    Mixed,
}

/// An app with several processes shown, and their sums.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub id: String,
    pub name: String,
    /// Its processes shown, in table order.
    pub members: Vec<usize>,
    pub cpu_percent: Option<f32>,
    pub memory_bytes: Option<u64>,
    pub threads: Option<u32>,
    pub cpu_time_ms: Option<u64>,
    pub user: GroupUser,
    /// The lowest pid among its members, which PID sorts it by.
    pub pid: u32,
    pub expanded: bool,
}

impl Group {
    fn entry(&self) -> Entry<'_> {
        Entry {
            name: &self.name,
            cpu: self.cpu_percent,
            memory: self.memory_bytes,
            threads: self.threads,
            cpu_time: self.cpu_time_ms,
            pid: self.pid,
            user: match &self.user {
                GroupUser::Shared(user) => Some(user),
                GroupUser::Mixed => None,
            },
            start_time: 0,
        }
    }
}

/// How a pane shows the processes.
#[derive(Clone, Copy, Debug)]
pub struct View<'a> {
    pub filter: Filter,
    pub search: &'a Search,
    pub sort: Sort,
    /// One row per app; else one row per process, as before grouping.
    pub grouped: bool,
    /// The apps whose processes show beneath them, by group id.
    pub expanded: &'a HashSet<String>,
}

/// The table's rows, their keys, and the apps they refer to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arranged {
    pub rows: Vec<TableRow>,
    pub keys: Vec<RowKey>,
    pub groups: Vec<Group>,
}

/// The sum of the values present; `None` when none is.
fn sum<T: std::iter::Sum<T>>(values: impl Iterator<Item = Option<T>>) -> Option<T> {
    let mut present = values.flatten().peekable();
    present.peek()?;
    Some(present.sum())
}

/// An app with the given members (shown, at least two).
fn group(id: &str, name: &str, members: Vec<usize>, processes: &[Process]) -> Group {
    let of = |index: &usize| &processes[*index];
    let mut users = members
        .iter()
        .map(|index| of(index).details.user.as_deref());
    let first = users.next().flatten();
    let user = match first {
        Some(user) if users.all(|other| other == Some(user)) => GroupUser::Shared(user.to_owned()),
        _ => GroupUser::Mixed,
    };
    Group {
        id: id.to_owned(),
        name: name.to_owned(),
        cpu_percent: sum(members.iter().map(|index| of(index).sample.cpu_percent)),
        memory_bytes: sum(members.iter().map(|index| of(index).sample.memory_bytes)),
        threads: sum(members.iter().map(|index| of(index).sample.threads)),
        cpu_time_ms: sum(members.iter().map(|index| of(index).sample.cpu_time_ms)),
        pid: members
            .iter()
            .map(|index| of(index).sample.pid)
            .min()
            .unwrap_or(0),
        user,
        members,
        expanded: false,
    }
}

/// Orders with `held` first in its order (rows keep their places under the pointer),
/// then the rest sorted.
fn held_order(
    held: &HashMap<&RowKey, usize>,
    a: &RowKey,
    b: &RowKey,
    sorted: impl FnOnce() -> Ordering,
) -> Ordering {
    match (held.get(a), held.get(b)) {
        (Some(a), Some(b)) => a.cmp(b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => sorted(),
    }
}

/// The table's rows. With `held` (the keys of the rows last shown, while the pointer is
/// over the table) rows keep their order, newcomers follow sorted, and leavers go.
pub fn arrange_rows(processes: &[Process], view: View, held: Option<&[RowKey]>) -> Arranged {
    if !view.grouped {
        let indices = match held {
            Some(held) => {
                let keys: Vec<ProcessKey> = held
                    .iter()
                    .filter_map(|key| match key {
                        RowKey::Process(key) => Some(*key),
                        RowKey::Group(_) => None,
                    })
                    .collect();
                arrange_held(processes, &keys, view.filter, view.search, view.sort)
            }
            None => arrange(processes, view.filter, view.search, view.sort),
        };
        return Arranged {
            keys: indices
                .iter()
                .map(|&index| RowKey::Process(key(&processes[index])))
                .collect(),
            rows: indices
                .into_iter()
                .map(|index| TableRow {
                    kind: RowKind::Process(index),
                    member: false,
                })
                .collect(),
            groups: Vec::new(),
        };
    }
    // Each app's processes the filter admits, by group id, in first-seen order.
    let mut by_app: HashMap<&str, (&str, Vec<usize>)> = HashMap::new();
    let mut alone = Vec::new();
    for (index, process) in processes.iter().enumerate() {
        if !view.filter.admits(process) {
            continue;
        }
        match &process.details.app_group {
            Some(app) => by_app
                .entry(app.id.as_str())
                .or_insert_with(|| (app.name.as_str(), Vec::new()))
                .1
                .push(index),
            None => alone.push(index),
        }
    }
    // A search naming the app keeps all its processes; else those it matches.
    let mut groups = Vec::new();
    for (id, (name, admitted)) in by_app {
        let named = view.search.names(name);
        let members: Vec<usize> = admitted
            .into_iter()
            .filter(|&index| named || view.search.matches(&processes[index]))
            .collect();
        match members.len() {
            0 => {}
            1 => alone.push(members[0]),
            _ => groups.push(group(id, name, members, processes)),
        }
    }
    alone.retain(|&index| {
        processes[index].details.app_group.is_some() || view.search.matches(&processes[index])
    });
    let held: HashMap<&RowKey, usize> = held
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(position, key)| (key, position))
        .collect();
    let process_key = |index: usize| RowKey::Process(key(&processes[index]));
    let sort_processes = |indices: &mut Vec<usize>| {
        indices.sort_by(|&a, &b| {
            held_order(&held, &process_key(a), &process_key(b), || {
                compare_entries(
                    &Entry::of(&processes[a]),
                    &Entry::of(&processes[b]),
                    view.sort,
                )
            })
        });
    };
    for group in &mut groups {
        sort_processes(&mut group.members);
        group.expanded = view.expanded.contains(&group.id);
    }
    // The top level: apps and lone processes together.
    let mut top: Vec<RowKind> = groups
        .iter()
        .enumerate()
        .map(|(index, _)| RowKind::Group(index))
        .chain(alone.into_iter().map(RowKind::Process))
        .collect();
    let row_key = |kind: RowKind| match kind {
        RowKind::Process(index) => process_key(index),
        RowKind::Group(index) => RowKey::Group(groups[index].id.clone()),
    };
    let entry = |kind: RowKind| match kind {
        RowKind::Process(index) => Entry::of(&processes[index]),
        RowKind::Group(index) => groups[index].entry(),
    };
    top.sort_by(|&a, &b| {
        held_order(&held, &row_key(a), &row_key(b), || {
            compare_entries(&entry(a), &entry(b), view.sort)
                // Two apps of one name (and lowest pid) still order the same way.
                .then_with(|| row_key(a).cmp_id(&row_key(b)))
        })
    });
    let mut arranged = Arranged::default();
    for kind in top {
        arranged.keys.push(row_key(kind));
        arranged.rows.push(TableRow {
            kind,
            member: false,
        });
        if let RowKind::Group(index) = kind
            && groups[index].expanded
        {
            for &member in &groups[index].members {
                arranged.keys.push(process_key(member));
                arranged.rows.push(TableRow {
                    kind: RowKind::Process(member),
                    member: true,
                });
            }
        }
    }
    arranged.groups = groups;
    arranged
}

impl Arranged {
    /// How many processes the rows stand for, collapsed apps included.
    pub fn process_count(&self) -> usize {
        self.rows
            .iter()
            .map(|row| match row.kind {
                RowKind::Process(_) if row.member => 0,
                RowKind::Process(_) => 1,
                RowKind::Group(index) => self.groups[index].members.len(),
            })
            .sum()
    }
}

impl RowKey {
    /// A last resort to order rows deterministically.
    fn cmp_id(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Group(a), Self::Group(b)) => a.cmp(b),
            (Self::Process(a), Self::Process(b)) => a.cmp(b),
            (Self::Group(_), Self::Process(_)) => Ordering::Less,
            (Self::Process(_), Self::Group(_)) => Ordering::Greater,
        }
    }
}

/// The row showing `key`: its own row, or for an app shown as a single process (or not
/// grouped), the first row of one of its processes.
pub fn find_row(arranged: &Arranged, processes: &[Process], key: &RowKey) -> Option<usize> {
    arranged
        .keys
        .iter()
        .position(|shown| shown == key)
        .or_else(|| {
            let RowKey::Group(id) = key else {
                return None;
            };
            arranged.rows.iter().position(|row| match row.kind {
                RowKind::Process(index) => processes[index]
                    .details
                    .app_group
                    .as_ref()
                    .is_some_and(|app| &app.id == id),
                RowKind::Group(_) => false,
            })
        })
}

/// One app's share of memory: an app group by id, or a process on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppMemory {
    pub key: RowKey,
    pub name: String,
    pub bytes: u64,
}

/// Memory by app: the largest apps, what the rest use together, and everything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryByApp {
    pub apps: Vec<AppMemory>,
    pub other: u64,
    pub total: u64,
}

/// Memory summed per app over every process, the `top` largest apps (ties by name, then
/// key) and the rest as Other. Processes without a known memory count for nothing.
pub fn memory_by_app(processes: &[Process], top: usize) -> MemoryByApp {
    let mut apps: HashMap<RowKey, AppMemory> = HashMap::new();
    for process in processes {
        let Some(bytes) = process.sample.memory_bytes else {
            continue;
        };
        let (key, name) = match &process.details.app_group {
            Some(app) => (RowKey::Group(app.id.clone()), app.name.as_str()),
            None => (RowKey::Process(key(process)), process.details.name.as_str()),
        };
        apps.entry(key.clone())
            .or_insert_with(|| AppMemory {
                key,
                name: name.to_owned(),
                bytes: 0,
            })
            .bytes += bytes;
    }
    let mut apps: Vec<AppMemory> = apps.into_values().filter(|app| app.bytes > 0).collect();
    apps.sort_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            .then_with(|| super::compare_names(&a.name, &b.name))
            .then_with(|| a.key.cmp_id(&b.key))
    });
    let total = apps.iter().map(|app| app.bytes).sum();
    let other = apps.iter().skip(top).map(|app| app.bytes).sum();
    apps.truncate(top);
    MemoryByApp { apps, other, total }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::{Column, Direction};
    use compi_protocol::system_metrics::{AppGroup, ProcessDetails, ProcessOwner, ProcessSample};
    use std::sync::Arc;

    fn process(pid: u32, name: &str, app: Option<&str>, memory: Option<u64>) -> Process {
        Process {
            sample: ProcessSample {
                pid,
                start_time: 100 + u64::from(pid),
                parent_pid: None,
                details: None,
                cpu_percent: memory.map(|memory| memory as f32 / 10.0),
                memory_bytes: memory,
                threads: Some(2),
                cpu_time_ms: None,
            },
            details: Arc::new(ProcessDetails {
                name: name.into(),
                command: None,
                executable: None,
                user: Some("me".into()),
                owner: ProcessOwner::CurrentUser,
                app: app.is_some(),
                restricted: false,
                app_group: app.map(|app| AppGroup {
                    id: format!("/Applications/{app}.app"),
                    name: app.into(),
                }),
            }),
        }
    }

    fn id(app: &str) -> String {
        format!("/Applications/{app}.app")
    }

    /// Safari (two processes), Mail (one), and a lone process.
    fn fixture() -> Vec<Process> {
        vec![
            process(10, "Safari", Some("Safari"), Some(300)),
            process(11, "Safari Web Content", Some("Safari"), Some(200)),
            process(20, "Mail", Some("Mail"), Some(400)),
            process(30, "zsh", None, Some(50)),
        ]
    }

    fn by_memory() -> Sort {
        Sort {
            column: Column::Memory,
            direction: Direction::Descending,
        }
    }

    fn view<'a>(search: &'a Search, expanded: &'a HashSet<String>, grouped: bool) -> View<'a> {
        View {
            filter: Filter::All,
            search,
            sort: by_memory(),
            grouped,
            expanded,
        }
    }

    /// Rows as text: an app as "app:name", a process as its pid, a member indented.
    fn shown(processes: &[Process], arranged: &Arranged) -> Vec<String> {
        arranged
            .rows
            .iter()
            .map(|row| match row.kind {
                RowKind::Group(index) => format!("app:{}", arranged.groups[index].name),
                RowKind::Process(index) => format!(
                    "{}{}",
                    if row.member { "  " } else { "" },
                    processes[index].sample.pid
                ),
            })
            .collect()
    }

    #[test]
    fn apps_group_their_processes_and_single_ones_stand_alone() {
        let processes = fixture();
        let search = Search::default();
        let none = HashSet::new();
        let arranged = arrange_rows(&processes, view(&search, &none, true), None);
        assert_eq!(shown(&processes, &arranged), ["app:Safari", "20", "30"]);
        assert_eq!(
            arranged.keys,
            [
                RowKey::Group(id("Safari")),
                RowKey::Process((20, 120)),
                RowKey::Process((30, 130)),
            ]
        );
        let safari = &arranged.groups[0];
        assert_eq!(safari.memory_bytes, Some(500));
        assert_eq!(safari.threads, Some(4));
        assert_eq!(safari.cpu_time_ms, None);
        assert_eq!(safari.cpu_percent, Some(50.0));
        assert_eq!(safari.pid, 10);
        assert_eq!(safari.user, GroupUser::Shared("me".into()));
        let expanded = HashSet::from([id("Safari")]);
        let arranged = arrange_rows(&processes, view(&search, &expanded, true), None);
        assert_eq!(
            shown(&processes, &arranged),
            ["app:Safari", "  10", "  11", "20", "30"]
        );
        // Flat, the table is as it always was.
        let flat = arrange_rows(&processes, view(&search, &expanded, false), None);
        assert_eq!(shown(&processes, &flat), ["20", "10", "11", "30"]);
        assert_eq!(
            flat.rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
            crate::monitor::arrange(&processes, Filter::All, &search, by_memory())
                .into_iter()
                .map(RowKind::Process)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn sums_leave_out_unavailable_values() {
        let mut processes = fixture();
        processes[1].sample.memory_bytes = None;
        let mut other = (*processes[1].details).clone();
        other.user = Some("root".into());
        processes[1].details = Arc::new(other);
        let search = Search::default();
        let none = HashSet::new();
        let arranged = arrange_rows(&processes, view(&search, &none, true), None);
        let safari = &arranged.groups[0];
        assert_eq!(safari.memory_bytes, Some(300));
        assert_eq!(safari.user, GroupUser::Mixed);
        processes[0].sample.memory_bytes = None;
        let arranged = arrange_rows(&processes, view(&search, &none, true), None);
        assert_eq!(arranged.groups[0].memory_bytes, None);
        // Unavailable sorts last.
        assert_eq!(shown(&processes, &arranged), ["20", "30", "app:Safari"]);
    }

    #[test]
    fn search_keeps_named_apps_whole_and_filters_apply_per_process() {
        let processes = fixture();
        let none = HashSet::new();
        let expanded = HashSet::from([id("Safari")]);
        let search = Search::new("safari");
        let arranged = arrange_rows(&processes, view(&search, &expanded, true), None);
        assert_eq!(shown(&processes, &arranged), ["app:Safari", "  10", "  11"]);
        // A member's own match keeps only it, shown as a plain row.
        let search = Search::new("web content");
        let arranged = arrange_rows(&processes, view(&search, &none, true), None);
        assert_eq!(shown(&processes, &arranged), ["11"]);
        assert_eq!(arranged.groups, []);
        // The sums cover only the members shown.
        let search = Search::default();
        let mut filtered = fixture();
        let mut system = (*filtered[1].details).clone();
        system.owner = ProcessOwner::System;
        filtered[1].details = Arc::new(system);
        let mine = View {
            filter: Filter::Mine,
            ..view(&search, &none, true)
        };
        let arranged = arrange_rows(&filtered, mine, None);
        assert_eq!(shown(&filtered, &arranged), ["20", "10", "30"]);
        let system_only = View {
            filter: Filter::System,
            ..view(&search, &none, true)
        };
        assert_eq!(
            shown(&filtered, &arrange_rows(&filtered, system_only, None)),
            ["11"]
        );
    }

    #[test]
    fn apps_sort_by_their_sums_and_members_within() {
        let mut processes = fixture();
        processes.push(process(12, "Safari Networking", Some("Safari"), Some(600)));
        let search = Search::default();
        let expanded = HashSet::from([id("Safari")]);
        let arranged = arrange_rows(&processes, view(&search, &expanded, true), None);
        assert_eq!(
            shown(&processes, &arranged),
            ["app:Safari", "  12", "  10", "  11", "20", "30"]
        );
        let ascending = View {
            sort: Sort {
                column: Column::Memory,
                direction: Direction::Ascending,
            },
            ..view(&search, &expanded, true)
        };
        let arranged = arrange_rows(&processes, ascending, None);
        assert_eq!(
            shown(&processes, &arranged),
            ["30", "20", "app:Safari", "  11", "  10", "  12"]
        );
        let by_pid = View {
            sort: Sort {
                column: Column::Pid,
                direction: Direction::Ascending,
            },
            ..view(&search, &expanded, true)
        };
        assert_eq!(
            shown(&processes, &arrange_rows(&processes, by_pid, None))[0],
            "app:Safari"
        );
    }

    #[test]
    fn held_rows_keep_their_order_across_apps_and_members() {
        let processes = fixture();
        let search = Search::default();
        let expanded = HashSet::from([id("Safari")]);
        let before = arrange_rows(&processes, view(&search, &expanded, true), None);
        assert_eq!(
            shown(&processes, &before),
            ["app:Safari", "  10", "  11", "20", "30"]
        );
        // Mail grows past Safari, Safari's helper outgrows its main process, zsh leaves
        // and a lone process arrives.
        let mut after = fixture();
        after[2].sample.memory_bytes = Some(5000);
        after[1].sample.memory_bytes = Some(900);
        after.remove(3);
        after.push(process(40, "top", None, Some(1)));
        let held = arrange_rows(&after, view(&search, &expanded, true), Some(&before.keys));
        assert_eq!(
            shown(&after, &held),
            ["app:Safari", "  10", "  11", "20", "40"]
        );
        let released = arrange_rows(&after, view(&search, &expanded, true), None);
        assert_eq!(
            shown(&after, &released),
            ["20", "app:Safari", "  11", "  10", "40"]
        );
    }

    #[test]
    fn expanded_apps_stay_open_while_shown_and_rows_are_found_by_app() {
        let processes = fixture();
        let search = Search::default();
        let expanded = HashSet::from([id("Safari"), id("Gone")]);
        let arranged = arrange_rows(&processes, view(&search, &expanded, true), None);
        assert!(arranged.groups[0].expanded);
        assert_eq!(
            find_row(&arranged, &processes, &RowKey::Group(id("Safari"))),
            Some(0)
        );
        // Mail shows as its single process; its app still finds that row.
        assert_eq!(
            find_row(&arranged, &processes, &RowKey::Group(id("Mail"))),
            Some(3)
        );
        assert_eq!(
            find_row(&arranged, &processes, &RowKey::Group(id("Gone"))),
            None
        );
        let flat = arrange_rows(&processes, view(&search, &expanded, false), None);
        assert_eq!(
            find_row(&flat, &processes, &RowKey::Group(id("Safari"))),
            Some(1)
        );
    }

    #[test]
    fn memory_by_app_takes_the_largest_apps_and_sums_the_rest() {
        let mut processes = fixture();
        processes.push(process(31, "zsh", None, Some(25)));
        processes.push(process(50, "launchd", None, None));
        let memory = memory_by_app(&processes, 2);
        assert_eq!(memory.total, 975);
        assert_eq!(
            memory
                .apps
                .iter()
                .map(|app| (app.name.as_str(), app.bytes))
                .collect::<Vec<_>>(),
            [("Safari", 500), ("Mail", 400)]
        );
        assert_eq!(memory.apps[0].key, RowKey::Group(id("Safari")));
        // Two lone processes of one name stay apart.
        assert_eq!(memory.other, 75);
        assert_eq!(memory_by_app(&processes, 10).apps.len(), 4);
        assert_eq!(memory_by_app(&processes, 10).other, 0);
        assert_eq!(memory_by_app(&[], 7), MemoryByApp::default());
    }
}
