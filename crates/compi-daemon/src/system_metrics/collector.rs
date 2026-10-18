//! Samples this host's processes: sysinfo for what every platform has, and the platform's
//! own calls for what sysinfo leaves out (thread counts, applications, and on macOS other
//! users' processes, which the kernel withholds from a non-root daemon).

use super::{Collect, Collected};
use compi_protocol::system_metrics::{MAX_COMMAND_BYTES, Process, ProcessDetails, ProcessSample};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind, Users};

/// Least time between two reloads of the user list for an owner it does not know.
const USERS_RELOAD: Duration = Duration::from_secs(60);

pub struct HostCollector {
    system: System,
    names: UserNames,
    own_user: Option<sysinfo::Uid>,
    logical_cpus: u32,
    /// The previous sample's processes. A process found here has a CPU value, because
    /// sysinfo measures CPU between two refreshes.
    details: HashMap<(u32, u64), Arc<ProcessDetails>>,
}

impl HostCollector {
    pub fn new() -> Self {
        let mut system = System::new();
        let own_pid = sysinfo::get_current_pid().ok();
        let own_user = own_pid.and_then(|pid| {
            system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[pid]),
                false,
                ProcessRefreshKind::nothing().with_user(UpdateKind::Always),
            );
            system.process(pid)?.user_id().cloned()
        });
        Self {
            system,
            names: UserNames {
                users: Users::new_with_refreshed_list(),
                loaded: Instant::now(),
            },
            own_user,
            logical_cpus: std::thread::available_parallelism()
                .map_or(1, |count| count.get().min(u32::MAX as usize) as u32),
            details: HashMap::new(),
        }
    }
}

impl Collect for HostCollector {
    fn collect(&mut self) -> Collected {
        let Self {
            system,
            names,
            own_user,
            logical_cpus,
            details: known,
        } = self;
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_user(UpdateKind::OnlyIfNotSet)
                .with_cmd(UpdateKind::OnlyIfNotSet)
                .with_exe(UpdateKind::OnlyIfNotSet),
        );
        let sampled_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as u64);
        let extra = platform::Extra::sample(system.processes().keys().map(|pid| pid.as_u32()));
        let mut previous = std::mem::take(known);
        let mut processes = Vec::with_capacity(system.processes().len());
        for (pid, process) in system.processes() {
            let pid = pid.as_u32();
            let key = (pid, process.start_time());
            let readable = extra.readable(pid);
            // An unreadable process has no start time, so a reused pid is told apart by name.
            let seen = previous
                .remove(&key)
                .filter(|details| key.1 != 0 || *details.name == *process.name().to_string_lossy());
            let measured = seen.is_some();
            let details = seen.unwrap_or_else(|| {
                let user = process.user_id();
                let executable = process
                    .exe()
                    .map(|path| path.to_string_lossy().into_owned());
                Arc::new(ProcessDetails {
                    name: process.name().to_string_lossy().into_owned(),
                    command: command_line(process.cmd()),
                    user: match user {
                        Some(user) => names.name(user),
                        None => extra.user(pid).and_then(platform::user_name),
                    },
                    owner: platform::owner(process, user, own_user.as_ref(), &extra),
                    app: platform::is_app(pid, executable.as_deref(), &extra),
                    executable,
                    restricted: !readable,
                })
            });
            let parent_pid = process.parent().map(|parent| parent.as_u32());
            let sample = if readable {
                ProcessSample {
                    pid,
                    start_time: key.1,
                    parent_pid,
                    details: None,
                    cpu_percent: measured.then(|| process.cpu_usage()),
                    memory_bytes: Some(process.memory()),
                    threads: extra.threads(pid),
                    cpu_time_ms: Some(process.accumulated_cpu_time()),
                }
            } else {
                let withheld = extra.withheld(pid).unwrap_or_default();
                ProcessSample {
                    pid,
                    start_time: key.1,
                    parent_pid,
                    details: None,
                    cpu_percent: withheld.cpu_percent,
                    memory_bytes: withheld.memory_bytes,
                    threads: None,
                    cpu_time_ms: withheld.cpu_time_ms,
                }
            };
            known.insert(key, details.clone());
            processes.push(Process { sample, details });
        }
        Collected {
            sampled_at_ms,
            logical_cpus: *logical_cpus,
            apps_known: platform::APPS_KNOWN,
            processes,
        }
    }
}

/// User names by id, reloaded at most every `USERS_RELOAD` for an id it does not know.
struct UserNames {
    users: Users,
    loaded: Instant,
}

impl UserNames {
    fn name(&mut self, user: &sysinfo::Uid) -> Option<String> {
        if self.users.get_user_by_id(user).is_none() && self.loaded.elapsed() > USERS_RELOAD {
            self.users.refresh();
            self.loaded = Instant::now();
        }
        self.users
            .get_user_by_id(user)
            .map(|user| user.name().to_owned())
            .or_else(|| platform::well_known_user(user))
    }
}

/// The command line joined with spaces, cut to `MAX_COMMAND_BYTES` at a character boundary.
fn command_line(arguments: &[std::ffi::OsString]) -> Option<String> {
    if arguments.is_empty() {
        return None;
    }
    let mut command = String::new();
    for argument in arguments {
        if !command.is_empty() {
            command.push(' ');
        }
        command.push_str(&argument.to_string_lossy());
        if command.len() >= MAX_COMMAND_BYTES {
            let mut end = MAX_COMMAND_BYTES;
            while !command.is_char_boundary(end) {
                end -= 1;
            }
            command.truncate(end);
            break;
        }
    }
    Some(command)
}

/// Resource values another source supplied for a process the daemon may not read.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Withheld {
    pub cpu_percent: Option<f32>,
    pub memory_bytes: Option<u64>,
    pub cpu_time_ms: Option<u64>,
}

#[cfg(target_os = "macos")]
mod platform {
    //! macOS gives a non-root process nothing about other users' processes beyond their
    //! name and path. `/bin/ps` is set-user-ID root, so it still reports their owner,
    //! memory and CPU (the kernel's decaying average, not the interval's).

    use super::Withheld;
    use compi_protocol::system_metrics::ProcessOwner;
    use std::collections::HashMap;
    use std::process::{Command, Stdio};

    pub const APPS_KNOWN: bool = true;
    /// Accounts below this are macOS's own.
    const FIRST_PERSON_UID: u32 = 500;

    pub struct Extra {
        threads: HashMap<u32, u32>,
        withheld: HashMap<u32, (u32, Withheld)>,
    }

    impl Extra {
        pub fn sample(pids: impl Iterator<Item = u32>) -> Self {
            let mut threads = HashMap::new();
            let mut restricted = false;
            for pid in pids {
                match task_threads(pid) {
                    Some(count) => {
                        threads.insert(pid, count);
                    }
                    None => restricted = true,
                }
            }
            Self {
                threads,
                withheld: if restricted { ps() } else { HashMap::new() },
            }
        }

        pub fn readable(&self, pid: u32) -> bool {
            self.threads.contains_key(&pid)
        }

        pub fn threads(&self, pid: u32) -> Option<u32> {
            self.threads.get(&pid).copied()
        }

        pub fn user(&self, pid: u32) -> Option<u32> {
            self.withheld.get(&pid).map(|(uid, _)| *uid)
        }

        pub fn withheld(&self, pid: u32) -> Option<Withheld> {
            self.withheld.get(&pid).map(|(_, withheld)| *withheld)
        }
    }

    fn task_threads(pid: u32) -> Option<u32> {
        let mut info = unsafe { std::mem::zeroed::<libc::proc_taskinfo>() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        let read = unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDTASKINFO,
                0,
                (&raw mut info).cast(),
                size,
            )
        };
        (read == size).then(|| info.pti_threadnum.max(0) as u32)
    }

    /// Every process `ps` reports, by pid: owner, resident memory, CPU and CPU time.
    fn ps() -> HashMap<u32, (u32, Withheld)> {
        let Ok(output) = Command::new("/bin/ps")
            .args(["-axo", "pid=,uid=,rss=,%cpu=,time="])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        else {
            return HashMap::new();
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_ps_line)
            .collect()
    }

    pub(super) fn parse_ps_line(line: &str) -> Option<(u32, (u32, Withheld))> {
        let mut fields = line.split_whitespace();
        let pid = fields.next()?.parse().ok()?;
        let uid = fields.next()?.parse().ok()?;
        let rss_kib: u64 = fields.next()?.parse().ok()?;
        let cpu: f32 = fields.next()?.parse().ok()?;
        let time = fields.next().and_then(parse_cpu_time);
        Some((
            pid,
            (
                uid,
                Withheld {
                    cpu_percent: Some(cpu),
                    memory_bytes: Some(rss_kib * 1024),
                    cpu_time_ms: time,
                },
            ),
        ))
    }

    /// `[[dd-]hh:]mm:ss[.cc]` as milliseconds.
    pub(super) fn parse_cpu_time(text: &str) -> Option<u64> {
        let (days, clock) = match text.split_once('-') {
            Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
            None => (0, text),
        };
        let mut parts = clock.rsplit(':');
        let seconds: f64 = parts.next()?.parse().ok()?;
        let minutes: u64 = parts.next()?.parse().ok()?;
        let hours: u64 = match parts.next() {
            Some(hours) => hours.parse().ok()?,
            None => 0,
        };
        if parts.next().is_some() {
            return None;
        }
        let whole = ((days * 24 + hours) * 60 + minutes) * 60;
        Some(whole * 1000 + (seconds * 1000.0).round() as u64)
    }

    pub fn user_name(uid: u32) -> Option<String> {
        let mut buffer = vec![0 as libc::c_char; 1024];
        let mut entry = unsafe { std::mem::zeroed::<libc::passwd>() };
        let mut found = std::ptr::null_mut();
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                &mut entry,
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut found,
            )
        };
        if status != 0 || found.is_null() {
            return None;
        }
        let name = unsafe { std::ffi::CStr::from_ptr(entry.pw_name) };
        Some(name.to_string_lossy().into_owned())
    }

    pub fn well_known_user(_user: &sysinfo::Uid) -> Option<String> {
        None
    }

    pub fn owner(
        process: &sysinfo::Process,
        user: Option<&sysinfo::Uid>,
        own: Option<&sysinfo::Uid>,
        extra: &Extra,
    ) -> ProcessOwner {
        let uid = user
            .map(|user| **user)
            .or_else(|| extra.user(process.pid().as_u32()));
        super::unix_owner(uid, own.map(|own| **own), FIRST_PERSON_UID)
    }

    /// The main executable of an application bundle that is not inside another bundle,
    /// so helpers nested in an app's frameworks are not apps. macOS's own bundles under
    /// `/System/Library` (Dock, Control Center, agents) are not either, except Finder;
    /// its applications in `/System/Applications` are.
    pub fn is_app(_pid: u32, executable: Option<&str>, _extra: &Extra) -> bool {
        const FINDER: &str = "/System/Library/CoreServices/Finder.app/Contents/MacOS/Finder";
        executable.is_some_and(|path| {
            (path == FINDER || !path.starts_with("/System/Library/"))
                && path.split_once(".app/").is_some_and(|(_, inside)| {
                    inside
                        .strip_prefix("Contents/MacOS/")
                        .is_some_and(|name| !name.is_empty() && !name.contains('/'))
                })
        })
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::Withheld;
    use compi_protocol::system_metrics::ProcessOwner;

    pub const APPS_KNOWN: bool = false;
    /// The usual first person account (`UID_MIN`); below it are system accounts.
    const FIRST_PERSON_UID: u32 = 1000;
    const NOBODY: u32 = 65534;

    pub struct Extra;

    impl Extra {
        pub fn sample(_pids: impl Iterator<Item = u32>) -> Self {
            Self
        }

        pub fn readable(&self, _pid: u32) -> bool {
            true
        }

        /// Field 20 of `/proc/<pid>/stat`, counted after the parenthesized name.
        pub fn threads(&self, pid: u32) -> Option<u32> {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let (_, fields) = stat.rsplit_once(')')?;
            fields.split_whitespace().nth(17)?.parse().ok()
        }

        pub fn user(&self, _pid: u32) -> Option<u32> {
            None
        }

        pub fn withheld(&self, _pid: u32) -> Option<Withheld> {
            None
        }
    }

    pub fn user_name(_uid: u32) -> Option<String> {
        None
    }

    pub fn well_known_user(_user: &sysinfo::Uid) -> Option<String> {
        None
    }

    pub fn owner(
        _process: &sysinfo::Process,
        user: Option<&sysinfo::Uid>,
        own: Option<&sysinfo::Uid>,
        _extra: &Extra,
    ) -> ProcessOwner {
        let uid = user.map(|user| **user);
        if uid == Some(NOBODY) && own.map(|own| **own) != Some(NOBODY) {
            return ProcessOwner::System;
        }
        super::unix_owner(uid, own.map(|own| **own), FIRST_PERSON_UID)
    }

    pub fn is_app(_pid: u32, _executable: Option<&str>, _extra: &Extra) -> bool {
        false
    }
}

#[cfg(unix)]
fn unix_owner(
    uid: Option<u32>,
    own: Option<u32>,
    first_person: u32,
) -> compi_protocol::system_metrics::ProcessOwner {
    use compi_protocol::system_metrics::ProcessOwner;
    match uid {
        Some(uid) if Some(uid) == own => ProcessOwner::CurrentUser,
        Some(uid) if uid < first_person => ProcessOwner::System,
        _ => ProcessOwner::Other,
    }
}

#[cfg(windows)]
mod platform {
    //! Thread counts come from one ToolHelp snapshot. An application is a process with a
    //! visible top-level window that has a title and no owner, as Task Manager counts them.

    use super::Withheld;
    use compi_protocol::system_metrics::ProcessOwner;
    use std::collections::{HashMap, HashSet};
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GetWindow, GetWindowTextLengthW, GetWindowThreadProcessId,
        IsWindowVisible,
    };
    use windows::core::BOOL;

    pub const APPS_KNOWN: bool = true;

    /// LocalSystem, LocalService and NetworkService.
    const SERVICE_ACCOUNTS: [(&str, &str); 3] = [
        ("S-1-5-18", "SYSTEM"),
        ("S-1-5-19", "LOCAL SERVICE"),
        ("S-1-5-20", "NETWORK SERVICE"),
    ];

    pub struct Extra {
        threads: HashMap<u32, u32>,
        apps: HashSet<u32>,
    }

    impl Extra {
        pub fn sample(_pids: impl Iterator<Item = u32>) -> Self {
            Self {
                threads: thread_counts(),
                apps: window_owners(),
            }
        }

        pub fn readable(&self, _pid: u32) -> bool {
            true
        }

        pub fn threads(&self, pid: u32) -> Option<u32> {
            self.threads.get(&pid).copied()
        }

        pub fn user(&self, _pid: u32) -> Option<u32> {
            None
        }

        pub fn withheld(&self, _pid: u32) -> Option<Withheld> {
            None
        }
    }

    fn thread_counts() -> HashMap<u32, u32> {
        let mut counts = HashMap::new();
        let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
            return counts;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
        while more {
            counts.insert(entry.th32ProcessID, entry.cntThreads);
            more = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
        }
        let _ = unsafe { CloseHandle(snapshot) };
        counts
    }

    fn window_owners() -> HashSet<u32> {
        unsafe extern "system" fn visit(window: HWND, owners: LPARAM) -> BOOL {
            let owners = unsafe { &mut *(owners.0 as *mut HashSet<u32>) };
            let shown = unsafe {
                IsWindowVisible(window).as_bool()
                    && GetWindow(window, GW_OWNER).is_err()
                    && GetWindowTextLengthW(window) > 0
            };
            if shown {
                let mut pid = 0;
                unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
                if pid != 0 {
                    owners.insert(pid);
                }
            }
            true.into()
        }
        let mut owners = HashSet::new();
        let _ = unsafe { EnumWindows(Some(visit), LPARAM(&raw mut owners as isize)) };
        owners
    }

    pub fn user_name(_uid: u32) -> Option<String> {
        None
    }

    pub fn well_known_user(user: &sysinfo::Uid) -> Option<String> {
        let sid = (**user).to_string();
        SERVICE_ACCOUNTS
            .iter()
            .find(|(known, _)| *known == sid)
            .map(|(_, name)| (*name).to_owned())
    }

    pub fn owner(
        process: &sysinfo::Process,
        user: Option<&sysinfo::Uid>,
        own: Option<&sysinfo::Uid>,
        _extra: &Extra,
    ) -> ProcessOwner {
        if user.is_some() && user == own {
            return ProcessOwner::CurrentUser;
        }
        let service = user.is_some_and(|user| {
            let sid = (**user).to_string();
            SERVICE_ACCOUNTS.iter().any(|(known, _)| *known == sid)
        });
        // Session 0 holds services; a process whose owner is hidden is a protected one.
        if service || user.is_none() || process.session_id().map(|id| id.as_u32()) == Some(0) {
            ProcessOwner::System
        } else {
            ProcessOwner::Other
        }
    }

    pub fn is_app(pid: u32, _executable: Option<&str>, extra: &Extra) -> bool {
        extra.apps.contains(&pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::system_metrics::ProcessOwner;

    #[test]
    fn command_lines_are_joined_and_cut_at_a_character_boundary() {
        assert_eq!(command_line(&[]), None);
        assert_eq!(
            command_line(&["ls".into(), "-la".into()]).as_deref(),
            Some("ls -la")
        );
        let long = "é".repeat(MAX_COMMAND_BYTES);
        let cut = command_line(&["x".into(), long.into()]).unwrap();
        assert!(cut.len() <= MAX_COMMAND_BYTES && cut.len() > MAX_COMMAND_BYTES - 4);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ps_lines_and_cpu_times_parse() {
        assert_eq!(platform::parse_cpu_time("00:14"), Some(14_000));
        assert_eq!(platform::parse_cpu_time("1:02.50"), Some(62_500));
        assert_eq!(platform::parse_cpu_time("2:00:01"), Some(7_201_000));
        assert_eq!(platform::parse_cpu_time("1-00:00:00"), Some(86_400_000));
        assert_eq!(platform::parse_cpu_time("1:2:3:4"), None);
        let (pid, (uid, withheld)) = platform::parse_ps_line("  362  0  6432  1.5 00:02").unwrap();
        assert_eq!((pid, uid), (362, 0));
        assert_eq!(withheld.memory_bytes, Some(6432 * 1024));
        assert_eq!(withheld.cpu_percent, Some(1.5));
        assert_eq!(withheld.cpu_time_ms, Some(2_000));
        assert_eq!(platform::parse_ps_line("garbage"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn only_outermost_bundle_executables_are_apps() {
        let extra = platform::Extra::sample(std::iter::empty());
        let app = |path| platform::is_app(1, Some(path), &extra);
        assert!(app("/Applications/Compi.app/Contents/MacOS/compi"));
        assert!(!app(
            "/Applications/Brave Browser.app/Contents/Frameworks/B.framework/Helpers/Helper (Renderer).app/Contents/MacOS/Helper (Renderer)"
        ));
        assert!(!app("/usr/libexec/secinitd"));
        assert!(!app(
            "/System/Library/CoreServices/Dock.app/Contents/MacOS/Dock"
        ));
        assert!(app(
            "/System/Library/CoreServices/Finder.app/Contents/MacOS/Finder"
        ));
        assert!(app(
            "/System/Applications/Calculator.app/Contents/MacOS/Calculator"
        ));
        assert!(!platform::is_app(1, None, &extra));
    }

    #[test]
    fn the_host_collector_sees_this_process() {
        let mut collector = HostCollector::new();
        let first = collector.collect();
        let own = std::process::id();
        let mine = first
            .processes
            .iter()
            .find(|process| process.sample.pid == own)
            .expect("this process is sampled");
        assert_eq!(mine.details.owner, ProcessOwner::CurrentUser);
        assert!(!mine.details.restricted);
        assert_eq!(
            mine.sample.cpu_percent, None,
            "no CPU before a second sample"
        );
        assert!(mine.sample.memory_bytes.is_some_and(|bytes| bytes > 0));
        assert!(mine.sample.threads.is_some_and(|threads| threads >= 1));
        std::thread::sleep(Duration::from_millis(200));
        let second = collector.collect();
        let mine = second
            .processes
            .iter()
            .find(|process| process.sample.pid == own)
            .unwrap();
        assert!(mine.sample.cpu_percent.is_some());
    }
}
