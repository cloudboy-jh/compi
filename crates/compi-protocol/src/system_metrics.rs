//! Monitor's samples of a source: the daemon's host or, on Windows, a WSL distribution.
//!
//! The host's totals (CPU, memory, disk, network) are sampled all the time into an hour of
//! history and sent to connections that `WatchSystemTotals`; a WSL distribution's only
//! while watched. Processes are sampled only while a connection watches them
//! (`WatchSystemMetrics`). Totals and process IDs of two sources are never mixed.
//!
//! A value the source would not give the daemon is `None`, never zero: a zero is a real
//! reading. Names, paths and command lines are whatever the processes chose and are shown
//! as plain text.

use crate::SurfaceId;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Time between two totals samples.
pub const TOTALS_INTERVAL_MS: u64 = 2000;
/// Totals samples kept: one hour.
pub const TOTALS_HISTORY: usize = 1800;

/// What Monitor watches: the daemon's host, or a WSL 2 distribution of a Windows daemon.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MonitorSource {
    #[default]
    Host,
    Wsl {
        distribution: String,
    },
}

impl MonitorSource {
    pub fn is_host(&self) -> bool {
        matches!(self, Self::Host)
    }

    /// Shape checks every daemon applies: a WSL distribution name is non-empty, at most 256
    /// bytes, and free of NUL, `/` and `\`.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Host => Ok(()),
            Self::Wsl { distribution } => {
                if distribution.is_empty()
                    || distribution.len() > 256
                    || distribution.contains(['\0', '/', '\\'])
                {
                    Err("Monitor source names an invalid WSL distribution".into())
                } else {
                    Ok(())
                }
            }
        }
    }
}

/// The full argument list `InspectProcess` returns is cut at this many bytes in all.
pub const MAX_INSPECTED_ARGUMENT_BYTES: usize = 64 * 1024;

/// What a process sample leaves out, read once when the inspector asks.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessInspection {
    pub state: Option<ProcessState>,
    pub working_directory: Option<String>,
    /// Every argument, the program first, cut once the total passes
    /// `MAX_INSPECTED_ARGUMENT_BYTES` (then `arguments_cut`).
    pub arguments: Option<Vec<String>>,
    pub arguments_cut: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Running,
    Sleeping,
    /// Uninterruptible wait, usually on disk.
    Waiting,
    Stopped,
    Zombie,
    Idle,
    Other,
}

/// What became of a `QuitProcess`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QuitOutcome {
    /// The process was asked to quit (or, forced, was ended).
    Sent,
    /// No process has that PID any more, or the PID now names a process with another start
    /// time; nothing was signalled.
    Gone,
    /// The host does not let the daemon signal it.
    Denied,
    /// Windows: the process has no window to close; only Force quit can end it.
    NoWindow,
    /// The daemon will not signal it: itself, PID 0 or 1, or a process the host keeps.
    Refused { reason: String },
}

/// What the host is, sent once when a connection starts watching totals.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostInfo {
    pub host_name: Option<String>,
    /// The operating system and its version, such as "macOS 26.6 Tahoe".
    pub os: Option<String>,
    /// The processor's name, such as "Apple M3 Pro".
    pub cpu: Option<String>,
    pub logical_cpus: u32,
    /// Milliseconds since the Unix epoch.
    pub booted_at_ms: Option<u64>,
}

/// One totals sample. Rates cover the interval since the previous sample, so the first
/// sample after the daemon starts has none.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TotalsSample {
    /// Counts samples since the daemon started; consecutive samples differ by one.
    pub sequence: u64,
    /// Milliseconds since the Unix epoch.
    pub sampled_at_ms: u64,
    pub cpu: Option<CpuTotals>,
    pub memory: Option<MemoryTotals>,
    /// The disk holding the system: the volume at `/`, or the Windows system drive.
    pub disk: Option<DiskTotals>,
    pub network: Option<NetworkTotals>,
    /// The host's GPU; `None` where the host has none the daemon can read (Linux).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<GpuTotals>,
    /// What only the newest sample carries: the daemon keeps it out of the history, so
    /// the last sample of `SystemTotalsWatching` and each `SystemTotalsSampled` have it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Box<TotalsDetail>>,
}

/// Percentages of all logical CPUs together, 0 to 100.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct CpuTotals {
    /// Programs, including niced ones.
    pub user: f32,
    /// The kernel on their behalf.
    pub system: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MemoryTotals {
    pub total_bytes: u64,
    /// Memory in use: what is not free or reclaimable cache.
    pub used_bytes: u64,
    /// What programs could still get without swapping: free plus reclaimable cache, as
    /// the host counts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_bytes: Option<u64>,
    /// How the memory divides, in the host's own terms, adding up to `total_bytes`.
    pub kinds: Vec<MemoryKind>,
    pub swap_total_bytes: Option<u64>,
    pub swap_used_bytes: Option<u64>,
    /// Where the host reports it (macOS).
    pub pressure: Option<MemoryPressure>,
}

/// The host's GPU. With several, the one with the most memory (Windows sums nothing
/// across adapters). Unified memory (Apple silicon) reports what the GPU has in use of
/// the shared memory.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GpuTotals {
    /// Such as "Apple M3 Pro" or "NVIDIA GeForce RTX 4070".
    pub name: Option<String>,
    /// Busy share of the GPU over the interval, 0 to 100.
    pub utilization: Option<f32>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
}

/// The newest sample's breakdowns, shown on the resource pages as they are now.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TotalsDetail {
    /// Busy share of each logical CPU over the interval, 0 to 100, in the host's order;
    /// empty when the host does not give it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cores: Vec<f32>,
    /// Mounted volumes a user stores files on (pseudo and system-internal file systems
    /// left out), the system volume first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<Volume>,
    /// Interfaces with a hardware address, as `NetworkTotals` counts them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<NetworkInterface>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Volume {
    /// The volume's label, else its mount point's last part.
    pub name: String,
    pub mount_point: String,
    pub file_system: Option<String>,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub removable: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkInterface {
    /// The host's name for it, such as `en0` or `Ethernet`.
    pub name: String,
    pub received_bytes_per_second: Option<u64>,
    pub sent_bytes_per_second: Option<u64>,
    /// Since the interface's counters started (usually boot).
    pub received_bytes: u64,
    pub sent_bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryKind {
    pub kind: MemoryKindName,
    pub bytes: u64,
}

/// macOS divides memory into App, Wired, Compressed, Cached and Free; Windows and Linux
/// into Used, Cached and Free.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKindName {
    App,
    Wired,
    Compressed,
    Used,
    Cached,
    Free,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPressure {
    Normal,
    Elevated,
    Critical,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiskTotals {
    pub name: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub read_bytes_per_second: Option<u64>,
    pub written_bytes_per_second: Option<u64>,
    /// Written since the disk's counters started, where the host keeps them.
    pub written_bytes: Option<u64>,
}

/// Every network interface with a hardware address together, so loopback and tunnels
/// that carry the same traffic again are left out.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkTotals {
    pub received_bytes_per_second: Option<u64>,
    pub sent_bytes_per_second: Option<u64>,
    /// Since the interfaces' counters started (usually boot).
    pub received_bytes: u64,
    pub sent_bytes: u64,
}

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
    /// The application the process belongs to, shared by all its processes: on macOS
    /// its outermost `.app` bundle (so an app's helpers join it), elsewhere its
    /// executable. `None` when the executable is not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_group: Option<AppGroup>,
    /// The Compi terminal surface the process runs in, when it belongs to one's session
    /// (Unix), job (Windows) or shell session (WSL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<SurfaceId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct AppGroup {
    /// The bundle or executable path; also what `GetAppIcon` asks for.
    pub id: String,
    /// The bundle's name without `.app`, or the executable's file name (without `.exe`).
    pub name: String,
}

/// Largest icon sent, in bytes of PNG.
pub const MAX_APP_ICON_BYTES: usize = 256 * 1024;

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
                    app_group: None,
                    surface: None,
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
