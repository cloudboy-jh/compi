//! One process of this host, named by `(pid, start_time)`: what a sample leaves out, and
//! asking it to quit. Each checks first that the PID still names the process that started
//! then, as the samples report its start, so a reused PID is never touched.

use super::cut_text;
use compi_protocol::system_metrics::{
    MAX_INSPECTED_ARGUMENT_BYTES, ProcessInspection, QuitOutcome,
};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Parents followed at most when looking for the daemon's ancestors.
const MAX_ANCESTORS: usize = 64;

/// The state, working directory and full arguments of `(pid, start_time)`; `None` once no
/// process with that start time has the PID.
pub fn inspect(pid: u32, start_time: u64) -> Option<ProcessInspection> {
    let target = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[target]),
        false,
        ProcessRefreshKind::nothing()
            .with_cwd(UpdateKind::Always)
            .with_cmd(UpdateKind::Always),
    );
    let process = system
        .process(target)
        .filter(|process| process.start_time() == start_time)?;
    let (arguments, arguments_cut) = cut_arguments(
        process
            .cmd()
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned()),
    );
    Some(ProcessInspection {
        state: platform::state(pid),
        working_directory: process
            .cwd()
            .map(|directory| directory.to_string_lossy().into_owned())
            .filter(|directory| !directory.is_empty()),
        arguments,
        arguments_cut,
    })
}

/// Asks `(pid, start_time)` to quit or, with `force`, ends it. Never a process the system
/// or the daemon itself depends on, and never a group: only that one process.
pub fn quit(pid: u32, start_time: u64, force: bool) -> QuitOutcome {
    match refusal(pid) {
        Some(reason) => QuitOutcome::Refused {
            reason: reason.into(),
        },
        None => platform::signal(pid, start_time, force),
    }
}

/// Every argument, the program first, cut once they total more than
/// `MAX_INSPECTED_ARGUMENT_BYTES`, and whether they were; `None` when there are none.
pub(super) fn cut_arguments(
    arguments: impl IntoIterator<Item = String>,
) -> (Option<Vec<String>>, bool) {
    let mut kept = Vec::new();
    let mut room = MAX_INSPECTED_ARGUMENT_BYTES;
    for mut argument in arguments {
        let cut = cut_text(&mut argument, room);
        room -= argument.len();
        if cut {
            if !argument.is_empty() {
                kept.push(argument);
            }
            return (Some(kept), true);
        }
        kept.push(argument);
    }
    ((!kept.is_empty()).then_some(kept), false)
}

/// Why the daemon will not signal `pid`.
fn refusal(pid: u32) -> Option<&'static str> {
    let own = std::process::id();
    if let Some(reason) = system_refusal(pid) {
        Some(reason)
    } else if pid == own {
        Some("Compi does not quit its own daemon")
    } else if ancestors(own).contains(&pid) {
        Some("Compi does not quit a process its daemon runs under")
    } else {
        None
    }
}

/// Why no source's PID 0 (the kernel's) or 1 (the first process) is signalled.
pub(super) fn system_refusal(pid: u32) -> Option<&'static str> {
    (pid <= 1).then_some("Compi does not quit PID 0 or 1, which the system needs")
}

/// The parent of `pid`, its parent, and so on.
fn ancestors(pid: u32) -> Vec<u32> {
    let mut system = System::new();
    let mut chain = Vec::new();
    let mut current = Pid::from_u32(pid);
    while chain.len() < MAX_ANCESTORS {
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[current]),
            false,
            ProcessRefreshKind::nothing(),
        );
        let Some(parent) = system.process(current).and_then(|process| process.parent()) else {
            break;
        };
        if parent.as_u32() == pid || chain.contains(&parent.as_u32()) {
            break;
        }
        chain.push(parent.as_u32());
        current = parent;
    }
    chain
}

/// When `pid` started, as the host's samples report it.
#[cfg(unix)]
fn start_time(pid: u32) -> Option<u64> {
    let target = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[target]),
        false,
        ProcessRefreshKind::nothing(),
    );
    system.process(target).map(|process| process.start_time())
}

#[cfg(unix)]
fn unix_signal(pid: u32, start_time: u64, force: bool) -> QuitOutcome {
    // Zero and negative PIDs would signal process groups.
    let Some(target) = libc::pid_t::try_from(pid).ok().filter(|&target| target > 0) else {
        return QuitOutcome::Gone;
    };
    if self::start_time(pid) != Some(start_time) {
        return QuitOutcome::Gone;
    }
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    if unsafe { libc::kill(target, signal) } == 0 {
        return QuitOutcome::Sent;
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EPERM) => QuitOutcome::Denied,
        Some(libc::ESRCH) => QuitOutcome::Gone,
        _ => QuitOutcome::Refused {
            reason: error.to_string(),
        },
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use compi_protocol::system_metrics::{ProcessState, QuitOutcome};

    /// `proc_pidinfo` flavor listing a process's thread handles.
    const PROC_PIDLISTTHREADS: libc::c_int = 6;
    /// Threads listed at most.
    const MAX_THREADS: usize = 1 << 16;

    pub fn signal(pid: u32, start_time: u64, force: bool) -> QuitOutcome {
        super::unix_signal(pid, start_time, force)
    }

    /// A zombie or stopped process says so itself; otherwise, as `ps` does, the state of
    /// its most active thread, since the process's own says "running" for nearly all.
    pub fn state(pid: u32) -> Option<ProcessState> {
        let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdinfo>() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
        let read = unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDTBSDINFO,
                0,
                (&raw mut info).cast(),
                size,
            )
        };
        if read != size {
            return None;
        }
        match info.pbi_status {
            libc::SZOMB => Some(ProcessState::Zombie),
            libc::SSTOP => Some(ProcessState::Stopped),
            libc::SIDL => Some(ProcessState::Other),
            _ => thread_state(pid),
        }
    }

    fn thread_state(pid: u32) -> Option<ProcessState> {
        let mut threads = vec![0u64; 64];
        let count = loop {
            let room = std::mem::size_of_val(threads.as_slice()) as i32;
            let read = unsafe {
                libc::proc_pidinfo(
                    pid as i32,
                    PROC_PIDLISTTHREADS,
                    0,
                    threads.as_mut_ptr().cast(),
                    room,
                )
            };
            if read <= 0 {
                return None;
            }
            let count = read as usize / std::mem::size_of::<u64>();
            if count < threads.len() || threads.len() >= MAX_THREADS {
                break count.min(threads.len());
            }
            threads.resize(threads.len() * 4, 0);
        };
        threads[..count]
            .iter()
            .filter_map(|&thread| {
                let mut info = unsafe { std::mem::zeroed::<libc::proc_threadinfo>() };
                let size = std::mem::size_of::<libc::proc_threadinfo>() as i32;
                let read = unsafe {
                    libc::proc_pidinfo(
                        pid as i32,
                        libc::PROC_PIDTHREADINFO,
                        thread,
                        (&raw mut info).cast(),
                        size,
                    )
                };
                (read == size).then_some(info.pth_run_state)
            })
            .filter_map(|run_state| match run_state {
                libc::TH_STATE_RUNNING => Some((0, ProcessState::Running)),
                libc::TH_STATE_UNINTERRUPTIBLE => Some((1, ProcessState::Waiting)),
                libc::TH_STATE_WAITING => Some((2, ProcessState::Sleeping)),
                libc::TH_STATE_STOPPED => Some((3, ProcessState::Stopped)),
                libc::TH_STATE_HALTED => Some((4, ProcessState::Other)),
                _ => None,
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, state)| state)
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::super::procfs;
    use compi_protocol::system_metrics::{ProcessState, QuitOutcome};

    pub fn signal(pid: u32, start_time: u64, force: bool) -> QuitOutcome {
        super::unix_signal(pid, start_time, force)
    }

    /// The state letter of `/proc/<pid>/stat`.
    pub fn state(pid: u32) -> Option<ProcessState> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        Some(procfs::process_state(procfs::stat_fields(&stat)?[0]))
    }
}

#[cfg(windows)]
mod platform {
    //! Quit closes the process's windows, as Task Manager's End task first does; Force quit
    //! terminates it. Both act on one handle opened before the start time is checked, so
    //! the PID cannot name another process in between.

    use compi_protocol::system_metrics::{ProcessState, QuitOutcome};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::Win32::Foundation::{
        ERROR_ACCESS_DENIED, FILETIME, HANDLE, HWND, LPARAM, STILL_ACTIVE, WPARAM,
    };
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_TERMINATE, TerminateProcess,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GetWindow, GetWindowThreadProcessId, IsWindowVisible, PostMessageW,
        WM_CLOSE,
    };
    use windows::core::BOOL;

    /// Seconds from 1601, where Windows counts from, to 1970.
    const UNIX_EPOCH_SECONDS: u64 = 11_644_473_600;

    /// Windows does not tell a process's state.
    pub fn state(_pid: u32) -> Option<ProcessState> {
        None
    }

    pub fn signal(pid: u32, start_time: u64, force: bool) -> QuitOutcome {
        let access = if force {
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION
        } else {
            PROCESS_QUERY_LIMITED_INFORMATION
        };
        let process = match unsafe { OpenProcess(access, false, pid) } {
            Ok(handle) => unsafe { OwnedHandle::from_raw_handle(handle.0) },
            Err(error) if error.code() == ERROR_ACCESS_DENIED.to_hresult() => {
                return QuitOutcome::Denied;
            }
            Err(_) => return QuitOutcome::Gone,
        };
        let process = HANDLE(process.as_raw_handle());
        if !running_since(process, start_time) {
            return QuitOutcome::Gone;
        }
        if force {
            return match unsafe { TerminateProcess(process, 1) } {
                Ok(()) => QuitOutcome::Sent,
                Err(error) if error.code() == ERROR_ACCESS_DENIED.to_hresult() => {
                    QuitOutcome::Denied
                }
                Err(error) => QuitOutcome::Refused {
                    reason: error.message(),
                },
            };
        }
        let windows = main_windows(pid);
        if windows.is_empty() {
            return QuitOutcome::NoWindow;
        }
        let posted = windows
            .into_iter()
            .filter(|&window| {
                unsafe { PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_ok()
            })
            .count();
        // Windows refuses messages to a process running at a higher integrity level.
        if posted > 0 {
            QuitOutcome::Sent
        } else {
            QuitOutcome::Denied
        }
    }

    /// Whether `process` is still running and started at `start_time`, in Unix seconds as
    /// sysinfo reports it.
    fn running_since(process: HANDLE, start_time: u64) -> bool {
        let mut exit_code = 0;
        if unsafe { GetExitCodeProcess(process, &mut exit_code) }.is_err()
            || exit_code != STILL_ACTIVE.0 as u32
        {
            return false;
        }
        let (mut creation, mut exit, mut kernel, mut user) = Default::default();
        if unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) }
            .is_err()
        {
            return false;
        }
        unix_seconds(creation) == start_time
    }

    fn unix_seconds(time: FILETIME) -> u64 {
        let intervals = (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
        (intervals / 10_000_000).saturating_sub(UNIX_EPOCH_SECONDS)
    }

    /// The process's visible top-level windows that no other window owns.
    fn main_windows(pid: u32) -> Vec<HWND> {
        struct Search {
            pid: u32,
            windows: Vec<HWND>,
        }
        unsafe extern "system" fn visit(window: HWND, search: LPARAM) -> BOOL {
            let search = unsafe { &mut *(search.0 as *mut Search) };
            let mut owner = 0;
            unsafe { GetWindowThreadProcessId(window, Some(&mut owner)) };
            let main = owner == search.pid
                && unsafe {
                    IsWindowVisible(window).as_bool() && GetWindow(window, GW_OWNER).is_err()
                };
            if main {
                search.windows.push(window);
            }
            true.into()
        }
        let mut search = Search {
            pid,
            windows: Vec::new(),
        };
        let _ = unsafe { EnumWindows(Some(visit), LPARAM(&raw mut search as isize)) };
        search.windows
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use compi_protocol::system_metrics::ProcessState;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    fn sleeper(directory: Option<&std::path::Path>) -> (Child, u64) {
        let mut command = Command::new("sleep");
        command.arg("30");
        if let Some(directory) = directory {
            command.current_dir(directory);
        }
        let child = command.spawn().expect("spawn sleep");
        let started = start_time(child.id()).expect("the child is sampled");
        (child, started)
    }

    #[test]
    fn quitting_refuses_the_system_and_the_daemon_and_its_ancestors() {
        let refused = |pid| matches!(quit(pid, 0, true), QuitOutcome::Refused { .. });
        assert!(refused(0));
        assert!(refused(1));
        assert!(refused(std::process::id()));
        assert!(refused(std::os::unix::process::parent_id()));
    }

    #[test]
    fn quitting_signals_only_the_process_that_started_then() {
        let (mut child, started) = sleeper(None);
        assert_eq!(quit(child.id(), started + 1, false), QuitOutcome::Gone);
        assert!(
            child.try_wait().unwrap().is_none(),
            "another start time is left alone"
        );

        assert_eq!(quit(child.id(), started, false), QuitOutcome::Sent);
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGTERM));

        let (mut child, started) = sleeper(None);
        assert_eq!(quit(child.id(), started, true), QuitOutcome::Sent);
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGKILL));
        assert_eq!(quit(child.id(), started, true), QuitOutcome::Gone);
    }

    #[test]
    fn inspecting_reads_the_directory_arguments_and_state() {
        let directory = std::env::temp_dir().canonicalize().unwrap();
        let (mut child, started) = sleeper(Some(&directory));
        assert_eq!(inspect(child.id(), started + 1), None);
        let deadline = Instant::now() + Duration::from_secs(5);
        let inspection = loop {
            let inspection = inspect(child.id(), started).expect("the child is running");
            if inspection.state == Some(ProcessState::Sleeping) || Instant::now() > deadline {
                break inspection;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(inspection.state, Some(ProcessState::Sleeping));
        assert_eq!(
            inspection.working_directory.as_deref(),
            directory.to_str(),
            "the child's directory"
        );
        assert_eq!(
            inspection.arguments,
            Some(vec!["sleep".to_owned(), "30".to_owned()])
        );
        assert!(!inspection.arguments_cut);
        assert_eq!(inspect(child.id(), started), None, "gone once reaped");
    }

    #[test]
    fn arguments_are_cut_past_their_limit() {
        assert_eq!(cut_arguments(Vec::new()), (None, false));
        let long = "x".repeat(MAX_INSPECTED_ARGUMENT_BYTES - 2);
        let (kept, cut) = cut_arguments(["ab".into(), long.clone(), "cd".into(), "ef".into()]);
        assert!(cut);
        assert_eq!(kept, Some(vec!["ab".to_owned(), long]));
        let (kept, cut) = cut_arguments(["ab".into(), "x".repeat(MAX_INSPECTED_ARGUMENT_BYTES)]);
        assert!(cut);
        assert_eq!(kept.unwrap()[1].len(), MAX_INSPECTED_ARGUMENT_BYTES - 2);
    }
}
