//! Monitor, kept pure: which processes a pane shows, in what order, and how values
//! read (here), the totals history its graphs draw from ([`totals`]), the newest
//! sample's breakdowns the resource pages list ([`detail`]) and how its pages divide
//! their width ([`layout`]). The pane (`gui::workspace::monitor`) draws them; nothing
//! here talks to the daemon or GPUI.
//!
//! A value the host would not give is `None` and reads as unavailable, never as zero.
//! Sorting puts unavailable values last in either direction, and breaks ties by name,
//! then pid, so the same processes always come in the same order.

pub mod apps;
pub mod detail;
pub mod layout;
pub mod totals;
pub mod treemap;

use compi_protocol::system_metrics::{Process, ProcessOwner};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

/// Names a process across samples: a pid the host reuses has another start time.
pub type ProcessKey = (u32, u64);

pub fn key(process: &Process) -> ProcessKey {
    (process.sample.pid, process.sample.start_time)
}

/// The table's columns, in the order drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Column {
    Name,
    Cpu,
    Memory,
    Threads,
    CpuTime,
    Pid,
    User,
}

impl Column {
    pub const ALL: [Self; 7] = [
        Self::Name,
        Self::Cpu,
        Self::Memory,
        Self::Threads,
        Self::CpuTime,
        Self::Pid,
        Self::User,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
            Self::Threads => "Threads",
            Self::CpuTime => "CPU time",
            Self::Pid => "PID",
            Self::User => "User",
        }
    }

    /// The column's width in pixels; Name takes what the others leave.
    pub const fn width(self) -> f32 {
        match self {
            Self::Name => NAME_MIN_WIDTH,
            Self::Cpu => 64.0,
            Self::Memory => 84.0,
            Self::Threads => 68.0,
            Self::CpuTime => 80.0,
            Self::Pid => 64.0,
            Self::User => 104.0,
        }
    }

    /// Amounts read right-aligned; names and users left.
    pub const fn numeric(self) -> bool {
        !matches!(self, Self::Name | Self::User)
    }

    /// The direction a first click sorts in: the largest amounts first, names A to Z.
    pub const fn first_direction(self) -> Direction {
        match self {
            Self::Name | Self::User | Self::Pid => Direction::Ascending,
            Self::Cpu | Self::Memory | Self::Threads | Self::CpuTime => Direction::Descending,
        }
    }
}

/// The narrowest the Name column gets before other columns make room.
pub const NAME_MIN_WIDTH: f32 = 160.0;

/// What a narrow table gives up first, in order; Name, CPU, Memory and PID stay.
const DROP_ORDER: [Column; 3] = [Column::User, Column::Threads, Column::CpuTime];

/// The columns a table `width` pixels wide shows: every column while Name keeps its
/// minimum, else without User, then Threads, then CPU time.
pub fn visible_columns(width: f32) -> Vec<Column> {
    fit_columns(&Column::ALL, &DROP_ORDER, width, Column::width)
}

/// The columns of `all` a table `width` pixels wide shows, in their order: every one
/// while their widths fit, else without each of `drop_order` in turn until they do (or
/// nothing is left to drop).
pub fn fit_columns<C: Copy + PartialEq>(
    all: &[C],
    drop_order: &[C],
    width: f32,
    column_width: impl Fn(C) -> f32,
) -> Vec<C> {
    let mut columns = all.to_vec();
    for dropped in drop_order {
        let needed: f32 = columns.iter().map(|&column| column_width(column)).sum();
        if needed <= width {
            break;
        }
        columns.retain(|column| column != dropped);
    }
    columns
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Ascending,
    Descending,
}

/// How the table is sorted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub column: Column,
    pub direction: Direction,
}

impl Default for Sort {
    /// The busiest processes first.
    fn default() -> Self {
        Self {
            column: Column::Cpu,
            direction: Direction::Descending,
        }
    }
}

impl Sort {
    /// After a click on `column`'s header: the sorted column flips its direction,
    /// another column starts in its first direction.
    pub fn clicked(self, column: Column) -> Self {
        if self.column == column {
            Self {
                column,
                direction: match self.direction {
                    Direction::Ascending => Direction::Descending,
                    Direction::Descending => Direction::Ascending,
                },
            }
        } else {
            Self {
                column,
                direction: column.first_direction(),
            }
        }
    }
}

/// Which processes the table shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    /// Applications the user launched, where the host can tell them apart.
    Apps,
    /// The daemon user's processes.
    Mine,
    /// The operating system's and service accounts' processes.
    System,
}

impl Filter {
    pub const ALL: [Self; 4] = [Self::All, Self::Apps, Self::Mine, Self::System];

    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Apps => "Apps",
            Self::Mine => "My processes",
            Self::System => "System",
        }
    }

    pub fn admits(self, process: &Process) -> bool {
        let details = &process.details;
        match self {
            Self::All => true,
            Self::Apps => details.app,
            Self::Mine => details.owner == ProcessOwner::CurrentUser,
            Self::System => details.owner == ProcessOwner::System,
        }
    }
}

/// A search of the table: every word must occur, ignoring case, in the process's
/// name, command line, executable path, user or pid.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Search {
    words: Vec<String>,
}

impl Search {
    pub fn new(text: &str) -> Self {
        Self {
            words: text.split_whitespace().map(str::to_lowercase).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn matches(&self, process: &Process) -> bool {
        if self.words.is_empty() {
            return true;
        }
        let details = &process.details;
        let fields: Vec<String> = [
            Some(details.name.as_str()),
            details.command.as_deref(),
            details.executable.as_deref(),
            details.user.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::to_lowercase)
        .chain([process.sample.pid.to_string()])
        .collect();
        self.words
            .iter()
            .all(|word| fields.iter().any(|field| field.contains(word.as_str())))
    }

    /// Whether every word occurs in `name` (an app's), ignoring case. An empty search
    /// names nothing.
    pub fn names(&self, name: &str) -> bool {
        if self.words.is_empty() {
            return false;
        }
        let name = name.to_lowercase();
        self.words.iter().all(|word| name.contains(word.as_str()))
    }
}

fn shown(process: &Process, filter: Filter, search: &Search) -> bool {
    filter.admits(process) && search.matches(process)
}

/// Names ignoring case, then exactly, so differently cased names still order the same
/// way every time.
fn compare_names(a: &str, b: &str) -> Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
        .then_with(|| a.cmp(b))
}

/// Available values in `order`, unavailable ones after them whatever the direction.
fn unavailable_last<T>(
    a: Option<T>,
    b: Option<T>,
    order: impl FnOnce(T, T) -> Ordering,
) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => order(a, b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// What a row is sorted by: a process's values, or an app group's sums.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Entry<'a> {
    pub(crate) name: &'a str,
    pub(crate) cpu: Option<f32>,
    pub(crate) memory: Option<u64>,
    pub(crate) threads: Option<u32>,
    pub(crate) cpu_time: Option<u64>,
    pub(crate) pid: u32,
    pub(crate) user: Option<&'a str>,
    pub(crate) start_time: u64,
}

impl<'a> Entry<'a> {
    pub(crate) fn of(process: &'a Process) -> Self {
        let sample = &process.sample;
        Self {
            name: &process.details.name,
            cpu: sample.cpu_percent,
            memory: sample.memory_bytes,
            threads: sample.threads,
            cpu_time: sample.cpu_time_ms,
            pid: sample.pid,
            user: process.details.user.as_deref(),
            start_time: sample.start_time,
        }
    }
}

/// `a` against `b` under `sort`; ties go by name, then pid, then start time.
pub(crate) fn compare_entries(a: &Entry, b: &Entry, sort: Sort) -> Ordering {
    let directed = |ordering: Ordering| match sort.direction {
        Direction::Ascending => ordering,
        Direction::Descending => ordering.reverse(),
    };
    let primary = match sort.column {
        Column::Name => directed(compare_names(a.name, b.name)),
        Column::Cpu => unavailable_last(a.cpu, b.cpu, |x, y| directed(x.total_cmp(&y))),
        Column::Memory => unavailable_last(a.memory, b.memory, |x, y| directed(x.cmp(&y))),
        Column::Threads => unavailable_last(a.threads, b.threads, |x, y| directed(x.cmp(&y))),
        Column::CpuTime => unavailable_last(a.cpu_time, b.cpu_time, |x, y| directed(x.cmp(&y))),
        Column::Pid => directed(a.pid.cmp(&b.pid)),
        Column::User => unavailable_last(a.user, b.user, |x, y| directed(compare_names(x, y))),
    };
    primary
        .then_with(|| compare_names(a.name, b.name))
        .then_with(|| a.pid.cmp(&b.pid))
        .then_with(|| a.start_time.cmp(&b.start_time))
}

/// `a` against `b` under `sort`; ties go by name, then pid, then start time.
pub fn compare(a: &Process, b: &Process, sort: Sort) -> Ordering {
    compare_entries(&Entry::of(a), &Entry::of(b), sort)
}

/// The indices of `processes` the table shows: those `filter` admits and `search`
/// matches, sorted.
pub fn arrange(processes: &[Process], filter: Filter, search: &Search, sort: Sort) -> Vec<usize> {
    let mut rows: Vec<usize> = (0..processes.len())
        .filter(|&index| shown(&processes[index], filter, search))
        .collect();
    rows.sort_by(|&a, &b| compare(&processes[a], &processes[b], sort));
    rows
}

/// [`arrange`] while the pointer is over the table, so rows do not move under it: the
/// processes of `held` (the rows last shown) keep their order with their new values,
/// those that left go, and new ones follow at the end, sorted among themselves.
pub fn arrange_held(
    processes: &[Process],
    held: &[ProcessKey],
    filter: Filter,
    search: &Search,
    sort: Sort,
) -> Vec<usize> {
    let shown_at: HashMap<ProcessKey, usize> = processes
        .iter()
        .enumerate()
        .filter(|(_, process)| shown(process, filter, search))
        .map(|(index, process)| (key(process), index))
        .collect();
    let mut rows: Vec<usize> = held
        .iter()
        .filter_map(|key| shown_at.get(key).copied())
        .collect();
    let kept: HashSet<&ProcessKey> = held.iter().collect();
    let mut added: Vec<usize> = shown_at
        .iter()
        .filter(|(key, _)| !kept.contains(key))
        .map(|(_, index)| *index)
        .collect();
    added.sort_by(|&a, &b| compare(&processes[a], &processes[b], sort));
    rows.extend(added);
    rows
}

/// A keyboard move of the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Up,
    Down,
    /// A page of this many rows.
    PageUp(usize),
    PageDown(usize),
    Home,
    End,
}

/// The row selected after `step` from `current` in a table of `len` rows. Without a
/// selection every move but End starts at the first row.
pub fn step_selection(current: Option<usize>, len: usize, step: Step) -> Option<usize> {
    let last = len.checked_sub(1)?;
    let Some(current) = current.map(|current| current.min(last)) else {
        return Some(if step == Step::End { last } else { 0 });
    };
    Some(match step {
        Step::Up => current.saturating_sub(1),
        Step::Down => (current + 1).min(last),
        Step::PageUp(rows) => current.saturating_sub(rows.max(1)),
        Step::PageDown(rows) => (current + rows.max(1)).min(last),
        Step::Home => 0,
        Step::End => last,
    })
}

/// The process's CPU as a share of the whole machine: `cpu_percent` is of one logical
/// CPU.
pub fn cpu_share(cpu_percent: Option<f32>, logical_cpus: u32) -> Option<f32> {
    cpu_percent.map(|percent| percent / logical_cpus.max(1) as f32)
}

pub fn format_cpu(share: f32) -> String {
    format!("{share:.1}%")
}

const KB: u64 = 1024;
const MB: u64 = KB * KB;
const GB: u64 = MB * KB;
const TB: u64 = GB * KB;

/// A size as a number and its unit: KB below a megabyte (rounded up, so a few bytes
/// are not "0 KB"), else MB, GB or TB with one decimal. Binary units.
pub fn bytes_parts(bytes: u64) -> (String, &'static str) {
    let scaled = |unit: u64| format!("{:.1}", bytes as f64 / unit as f64);
    if bytes >= TB {
        (scaled(TB), "TB")
    } else if bytes >= GB {
        (scaled(GB), "GB")
    } else if bytes >= MB {
        (scaled(MB), "MB")
    } else {
        (bytes.div_ceil(KB).to_string(), "KB")
    }
}

pub fn format_bytes(bytes: u64) -> String {
    let (value, unit) = bytes_parts(bytes);
    format!("{value} {unit}")
}

/// An amount in use of a whole, as "8.0 GB of 16.0 GB"; the amount alone where the
/// whole is unknown.
pub fn format_used_of(used: Option<u64>, total: Option<u64>) -> Option<String> {
    let used = format_bytes(used?);
    Some(match total {
        Some(total) => format!("{used} of {}", format_bytes(total)),
        None => used,
    })
}

/// A transfer rate as a number and its unit: whole B/s below a kilobyte a second, else
/// KB/s, MB/s or GB/s with one decimal. Binary units.
pub fn rate_parts(bytes_per_second: u64) -> (String, &'static str) {
    let scaled = |unit: u64| format!("{:.1}", bytes_per_second as f64 / unit as f64);
    if bytes_per_second >= GB {
        (scaled(GB), "GB/s")
    } else if bytes_per_second >= MB {
        (scaled(MB), "MB/s")
    } else if bytes_per_second >= KB {
        (scaled(KB), "KB/s")
    } else {
        (bytes_per_second.to_string(), "B/s")
    }
}

pub fn format_rate(bytes_per_second: u64) -> String {
    let (value, unit) = rate_parts(bytes_per_second);
    format!("{value} {unit}")
}

/// A percentage with one decimal and its unit.
pub fn percent_parts(percent: f32) -> (String, &'static str) {
    (format!("{:.1}", percent.clamp(0.0, 100.0)), "%")
}

pub fn format_percent(percent: f32) -> String {
    let (value, unit) = percent_parts(percent);
    format!("{value}{unit}")
}

/// A whole percentage, for tight places such as Monitor's nav.
pub fn format_whole_percent(percent: f32) -> String {
    format!("{:.0}%", percent.clamp(0.0, 100.0))
}

/// How long the host has been up: "Xd Yh" from a day, else "Xh Ym".
pub fn format_uptime(seconds: u64) -> String {
    let (days, hours, minutes) = (
        seconds / 86_400,
        seconds % 86_400 / 3600,
        seconds % 3600 / 60,
    );
    if days > 0 {
        format!("{days}d {hours}h")
    } else {
        format!("{hours}h {minutes}m")
    }
}

/// CPU time as h:mm:ss, or m:ss under an hour.
pub fn format_cpu_time(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// The header's count: every process, or how many of them the search and filter show.
pub fn format_count(shown: usize, total: usize) -> String {
    let noun = |count: usize| if count == 1 { "process" } else { "processes" };
    if shown == total {
        format!("{total} {}", noun(total))
    } else {
        format!("{shown} of {total} {}", noun(total))
    }
}

/// Text a process chose, as one line: control characters become spaces and invisible
/// or direction-changing characters go, so one process cannot pose as another.
pub fn display_line(text: &str) -> String {
    crate::program_status::display_text(text)
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::system_metrics::{ProcessDetails, ProcessSample};
    use std::sync::Arc;

    fn process(pid: u32, name: &str, cpu: Option<f32>, memory: Option<u64>) -> Process {
        Process {
            sample: ProcessSample {
                pid,
                start_time: 1_000 + u64::from(pid),
                parent_pid: Some(1),
                details: None,
                cpu_percent: cpu,
                memory_bytes: memory,
                threads: Some(4),
                cpu_time_ms: Some(1_000),
            },
            details: Arc::new(ProcessDetails {
                name: name.into(),
                command: Some(format!("/usr/bin/{name} --serve")),
                executable: Some(format!("/usr/bin/{name}")),
                user: Some("me".into()),
                owner: ProcessOwner::CurrentUser,
                app: false,
                restricted: false,
                app_group: None,
            }),
        }
    }

    fn with_details(mut process: Process, change: impl FnOnce(&mut ProcessDetails)) -> Process {
        let mut details = (*process.details).clone();
        change(&mut details);
        process.details = Arc::new(details);
        process
    }

    fn pids(processes: &[Process], rows: &[usize]) -> Vec<u32> {
        rows.iter().map(|&row| processes[row].sample.pid).collect()
    }

    fn sorted(processes: &[Process], column: Column, direction: Direction) -> Vec<u32> {
        let sort = Sort { column, direction };
        pids(
            processes,
            &arrange(processes, Filter::All, &Search::default(), sort),
        )
    }

    #[test]
    fn unavailable_values_sort_last_in_both_directions() {
        let processes = vec![
            process(1, "a", None, Some(10)),
            process(2, "b", Some(50.0), None),
            process(3, "c", Some(5.0), Some(30)),
            process(4, "d", Some(0.0), Some(20)),
        ];
        assert_eq!(
            sorted(&processes, Column::Cpu, Direction::Descending),
            [2, 3, 4, 1]
        );
        assert_eq!(
            sorted(&processes, Column::Cpu, Direction::Ascending),
            [4, 3, 2, 1]
        );
        assert_eq!(
            sorted(&processes, Column::Memory, Direction::Descending),
            [3, 4, 1, 2]
        );
        assert_eq!(
            sorted(&processes, Column::Memory, Direction::Ascending),
            [1, 4, 3, 2]
        );
        let users = vec![
            with_details(process(1, "a", None, None), |details| details.user = None),
            with_details(process(2, "b", None, None), |details| {
                details.user = Some("root".into())
            }),
            process(3, "c", None, None),
        ];
        assert_eq!(
            sorted(&users, Column::User, Direction::Ascending),
            [3, 2, 1]
        );
        assert_eq!(
            sorted(&users, Column::User, Direction::Descending),
            [2, 3, 1]
        );
    }

    #[test]
    fn ties_break_by_name_then_pid_whatever_the_input_order() {
        let processes = vec![
            process(30, "zsh", Some(1.0), None),
            process(20, "Bash", Some(1.0), None),
            process(10, "zsh", Some(1.0), None),
            process(40, "bash", Some(1.0), None),
        ];
        let mut reversed = processes.clone();
        reversed.reverse();
        for direction in [Direction::Ascending, Direction::Descending] {
            assert_eq!(sorted(&processes, Column::Cpu, direction), [20, 40, 10, 30]);
            assert_eq!(sorted(&reversed, Column::Cpu, direction), [20, 40, 10, 30]);
        }
        assert_eq!(
            sorted(&processes, Column::Name, Direction::Descending),
            [10, 30, 40, 20]
        );
        assert_eq!(
            sorted(&processes, Column::Pid, Direction::Descending),
            [40, 30, 20, 10]
        );
    }

    #[test]
    fn a_held_order_keeps_its_rows_drops_leavers_and_appends_newcomers() {
        let before = vec![
            process(1, "a", Some(10.0), None),
            process(2, "b", Some(20.0), None),
            process(3, "c", Some(30.0), None),
        ];
        let sort = Sort::default();
        let search = Search::default();
        let rows = arrange(&before, Filter::All, &search, sort);
        assert_eq!(pids(&before, &rows), [3, 2, 1]);
        let held: Vec<ProcessKey> = rows.iter().map(|&row| key(&before[row])).collect();
        // 1 is now the busiest and 2 left; 5 and 4 arrive.
        let after = vec![
            process(5, "e", Some(1.0), None),
            process(1, "a", Some(90.0), None),
            process(3, "c", Some(0.0), None),
            process(4, "d", Some(50.0), None),
        ];
        let rows = arrange_held(&after, &held, Filter::All, &search, sort);
        assert_eq!(pids(&after, &rows), [3, 1, 4, 5]);
        assert_eq!(after[rows[1]].sample.cpu_percent, Some(90.0));
        // Released, the table sorts again.
        assert_eq!(
            pids(&after, &arrange(&after, Filter::All, &search, sort)),
            [1, 4, 5, 3]
        );
        // A pid reused by a new process is a newcomer, not the old row.
        let mut reused = process(3, "c", Some(5.0), None);
        reused.sample.start_time += 1;
        let after = vec![reused, process(1, "a", Some(1.0), None)];
        let rows = arrange_held(&after, &held, Filter::All, &search, sort);
        assert_eq!(pids(&after, &rows), [1, 3]);
    }

    #[test]
    fn search_needs_every_word_in_some_field_ignoring_case() {
        let server = with_details(process(4242, "Node", None, None), |details| {
            details.command = Some("node server.js --port 8080".into());
            details.executable = Some("/opt/homebrew/bin/node".into());
            details.user = Some("Alice".into());
        });
        let matches = |text: &str| Search::new(text).matches(&server);
        assert!(matches(""));
        assert!(matches("   "));
        assert!(matches("node"));
        assert!(matches("NODE server"));
        assert!(matches("homebrew"));
        assert!(matches("alice 8080"));
        assert!(matches("4242"));
        assert!(matches("424"));
        assert!(!matches("node python"));
        assert!(!matches("4243"));
        let bare = with_details(process(7, "kernel_task", None, None), |details| {
            details.command = None;
            details.executable = None;
            details.user = None;
        });
        assert!(Search::new("KERNEL").matches(&bare));
        assert!(!Search::new("usr").matches(&bare));
    }

    #[test]
    fn filters_pick_apps_the_users_processes_or_the_systems() {
        let processes = vec![
            with_details(process(1, "launchd", None, None), |details| {
                details.owner = ProcessOwner::System;
                details.user = Some("root".into());
            }),
            with_details(process(2, "Safari", None, None), |details| {
                details.app = true
            }),
            process(3, "zsh", None, None),
            with_details(process(4, "other", None, None), |details| {
                details.owner = ProcessOwner::Other;
                details.restricted = true;
            }),
        ];
        let shown = |filter: Filter, text: &str| {
            let mut pids = pids(
                &processes,
                &arrange(&processes, filter, &Search::new(text), Sort::default()),
            );
            pids.sort_unstable();
            pids
        };
        assert_eq!(shown(Filter::All, ""), [1, 2, 3, 4]);
        assert_eq!(shown(Filter::Apps, ""), [2]);
        assert_eq!(shown(Filter::Mine, ""), [2, 3]);
        assert_eq!(shown(Filter::System, ""), [1]);
        assert_eq!(shown(Filter::Mine, "zsh"), [3]);
        assert!(shown(Filter::System, "zsh").is_empty());
        let held = [key(&processes[0]), key(&processes[2])];
        let rows = arrange_held(
            &processes,
            &held,
            Filter::Mine,
            &Search::default(),
            Sort::default(),
        );
        assert_eq!(pids(&processes, &rows), [3, 2]);
    }

    #[test]
    fn header_clicks_flip_the_sorted_column_and_start_others_in_their_direction() {
        let sort = Sort::default();
        assert_eq!(sort.column, Column::Cpu);
        assert_eq!(sort.direction, Direction::Descending);
        let flipped = sort.clicked(Column::Cpu);
        assert_eq!(flipped.direction, Direction::Ascending);
        assert_eq!(flipped.clicked(Column::Cpu), sort);
        assert_eq!(
            flipped.clicked(Column::Name),
            Sort {
                column: Column::Name,
                direction: Direction::Ascending
            }
        );
        assert_eq!(
            sort.clicked(Column::Memory).direction,
            Direction::Descending
        );
    }

    #[test]
    fn narrow_tables_drop_user_then_threads_then_cpu_time() {
        let all: f32 = Column::ALL.iter().map(|column| column.width()).sum();
        assert_eq!(visible_columns(all), Column::ALL);
        let without = |dropped: &[Column]| -> Vec<Column> {
            Column::ALL
                .into_iter()
                .filter(|column| !dropped.contains(column))
                .collect()
        };
        assert_eq!(visible_columns(all - 1.0), without(&[Column::User]));
        let no_user = all - Column::User.width();
        assert_eq!(visible_columns(no_user), without(&[Column::User]));
        assert_eq!(
            visible_columns(no_user - 1.0),
            without(&[Column::User, Column::Threads])
        );
        assert_eq!(
            visible_columns(0.0),
            [Column::Name, Column::Cpu, Column::Memory, Column::Pid]
        );
    }

    #[test]
    fn keyboard_steps_stay_inside_the_table() {
        assert_eq!(step_selection(None, 0, Step::Down), None);
        assert_eq!(step_selection(None, 5, Step::Down), Some(0));
        assert_eq!(step_selection(None, 5, Step::Up), Some(0));
        assert_eq!(step_selection(None, 5, Step::End), Some(4));
        assert_eq!(step_selection(Some(0), 5, Step::Up), Some(0));
        assert_eq!(step_selection(Some(4), 5, Step::Down), Some(4));
        assert_eq!(step_selection(Some(2), 5, Step::Down), Some(3));
        assert_eq!(step_selection(Some(1), 5, Step::PageDown(10)), Some(4));
        assert_eq!(step_selection(Some(3), 5, Step::PageUp(2)), Some(1));
        assert_eq!(step_selection(Some(3), 5, Step::Home), Some(0));
        assert_eq!(step_selection(Some(9), 5, Step::Up), Some(3));
    }

    #[test]
    fn values_read_as_shares_sizes_and_durations() {
        assert_eq!(cpu_share(Some(200.0), 8), Some(25.0));
        assert_eq!(cpu_share(Some(50.0), 0), Some(50.0));
        assert_eq!(cpu_share(None, 8), None);
        assert_eq!(format_cpu(12.345), "12.3%");
        assert_eq!(format_cpu(0.0), "0.0%");
        assert_eq!(format_bytes(0), "0 KB");
        assert_eq!(format_bytes(1), "1 KB");
        assert_eq!(format_bytes(512 * 1024), "512 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(150 * 1024 * 1024 + 300 * 1024), "150.3 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
        assert_eq!(
            bytes_parts(2 * 1024 * 1024 * 1024 * 1024),
            ("2.0".into(), "TB")
        );
        assert_eq!(format_rate(0), "0 B/s");
        assert_eq!(format_rate(1023), "1023 B/s");
        assert_eq!(format_rate(1536), "1.5 KB/s");
        assert_eq!(format_rate(5 * 1024 * 1024), "5.0 MB/s");
        assert_eq!(rate_parts(3 * 1024 * 1024 * 1024), ("3.0".into(), "GB/s"));
        assert_eq!(format_percent(12.345), "12.3%");
        assert_eq!(format_percent(140.0), "100.0%");
        assert_eq!(percent_parts(7.0), ("7.0".into(), "%"));
        assert_eq!(format_whole_percent(62.6), "63%");
        assert_eq!(
            format_used_of(Some(2 * 1024 * 1024 * 1024), Some(16 * 1024 * 1024 * 1024)).as_deref(),
            Some("2.0 GB of 16.0 GB")
        );
        assert_eq!(format_used_of(Some(1024), None).as_deref(), Some("1 KB"));
        assert_eq!(format_used_of(None, Some(1024)), None);
        assert_eq!(format_uptime(59), "0h 0m");
        assert_eq!(format_uptime(3 * 3600 + 25 * 60), "3h 25m");
        assert_eq!(format_uptime(24 * 86_400 + 14 * 3600 + 59), "24d 14h");
        assert_eq!(format_cpu_time(0), "0:00");
        assert_eq!(format_cpu_time(65_999), "1:05");
        assert_eq!(format_cpu_time(3_600_000), "1:00:00");
        assert_eq!(format_cpu_time(37_230_000), "10:20:30");
        assert_eq!(format_count(1, 1), "1 process");
        assert_eq!(format_count(300, 300), "300 processes");
        assert_eq!(format_count(3, 300), "3 of 300 processes");
        assert_eq!(display_line("evil\nname\u{202e}txt"), "evil nametxt");
    }
}
