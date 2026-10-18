//! Monitor's samples of the daemon's host, collected only while a connection
//! watches them (`WatchSystemMetrics`).
//!
//! A value the host would not give the daemon is `None`, never zero: a zero is a real
//! reading. Names, paths and command lines are whatever the processes chose and are shown
//! as plain text.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// One sample of the host's processes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SystemSample {
    /// Milliseconds since the Unix epoch.
    pub sampled_at_ms: u64,
    /// Logical CPUs on the host. `ProcessSample::cpu_percent` is a percentage of one.
    pub logical_cpus: u32,
    /// Whether `ProcessDetails::app` is known on this host. Where it is not, every
    /// process has `app == false` and an Apps filter would show nothing meaningful.
    pub apps_known: bool,
    pub processes: Vec<ProcessSample>,
}

/// One process in one sample. `(pid, start_time)` names a process: a pid the host reuses
/// later has another start time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessSample {
    pub pid: u32,
    /// Seconds since the Unix epoch.
    pub start_time: u64,
    pub parent_pid: Option<u32>,
    /// What does not change while the process runs. Sent with the first sample that
    /// carries the process on a connection; the client keeps it until the process
    /// leaves the samples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Box<ProcessDetails>>,
    /// Percent of one logical CPU since the previous sample (above 100 for several
    /// busy threads). `None` in a process's first sample or when not readable.
    pub cpu_percent: Option<f32>,
    /// Resident memory (working set on Windows).
    pub memory_bytes: Option<u64>,
    pub threads: Option<u32>,
    /// CPU time used since the process started.
    pub cpu_time_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessDetails {
    pub name: String,
    /// The command line, joined with spaces and cut to `MAX_COMMAND_BYTES`.
    pub command: Option<String>,
    pub executable: Option<String>,
    pub user: Option<String>,
    pub owner: ProcessOwner,
    /// An application the user launched and sees, as the host defines it.
    pub app: bool,
    /// The host does not let the daemon read this process's resources, so its missing
    /// values are withheld rather than unsupported (macOS: other users' processes).
    pub restricted: bool,
}

/// Whose process it is, relative to the user the daemon runs as.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProcessOwner {
    /// The daemon's own user.
    CurrentUser,
    /// The operating system or a service account.
    System,
    /// Another user, or an owner the host would not tell.
    Other,
}

/// Longest command line sent, in bytes; longer ones are cut at a character boundary.
pub const MAX_COMMAND_BYTES: usize = 2048;

/// One process as a client shows it: its latest sample and the details sent earlier.
#[derive(Debug, Clone, PartialEq)]
pub struct Process {
    pub sample: ProcessSample,
    pub details: Arc<ProcessDetails>,
}

/// What a watching client keeps between samples: each process's details, sent once.
#[derive(Debug, Default)]
pub struct ProcessTable {
    details: HashMap<(u32, u64), Arc<ProcessDetails>>,
}

impl ProcessTable {
    /// The sample's processes with their details. Details of processes that have left
    /// the samples are dropped; a process whose details never arrived is skipped.
    pub fn apply(&mut self, processes: Vec<ProcessSample>) -> Vec<Process> {
        let mut details = HashMap::with_capacity(processes.len());
        let processes = processes
            .into_iter()
            .filter_map(|mut sample| {
                let key = (sample.pid, sample.start_time);
                let known = match sample.details.take() {
                    Some(sent) => Arc::from(sent),
                    None => self.details.remove(&key)?,
                };
                details.insert(key, known.clone());
                Some(Process {
                    sample,
                    details: known,
                })
            })
            .collect();
        self.details = details;
        processes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(pid: u32, start_time: u64, name: Option<&str>) -> ProcessSample {
        ProcessSample {
            pid,
            start_time,
            parent_pid: None,
            details: name.map(|name| {
                Box::new(ProcessDetails {
                    name: name.into(),
                    command: None,
                    executable: None,
                    user: None,
                    owner: ProcessOwner::CurrentUser,
                    app: false,
                    restricted: false,
                })
            }),
            cpu_percent: None,
            memory_bytes: None,
            threads: None,
            cpu_time_ms: None,
        }
    }

    fn names(processes: &[Process]) -> Vec<(u32, &str)> {
        processes
            .iter()
            .map(|process| (process.sample.pid, process.details.name.as_str()))
            .collect()
    }

    #[test]
    fn details_are_kept_per_process_until_it_leaves() {
        let mut table = ProcessTable::default();
        let first = table.apply(vec![sample(1, 10, Some("a")), sample(2, 20, Some("b"))]);
        assert_eq!(names(&first), [(1, "a"), (2, "b")]);
        // Later samples carry no details for known processes; a new one brings its own.
        let second = table.apply(vec![sample(1, 10, None), sample(3, 30, Some("c"))]);
        assert_eq!(names(&second), [(1, "a"), (3, "c")]);
        // Pid 2 left, so a reused pid 2 with another start is a different process.
        let third = table.apply(vec![sample(2, 40, None), sample(2, 50, Some("d"))]);
        assert_eq!(names(&third), [(2, "d")]);
    }
}
