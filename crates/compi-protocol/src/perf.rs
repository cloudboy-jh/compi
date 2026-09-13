use crate::ProcessMetrics;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
#[cfg(windows)]
use std::mem::size_of;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
#[cfg(windows)]
use windows::Win32::Foundation::FILETIME;
#[cfg(windows)]
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessHandleCount, GetProcessTimes,
};

static STARTUP_KIND: OnceLock<&'static str> = OnceLock::new();
static NEXT_LATENCY_ID: AtomicU64 = AtomicU64::new(1);
static LOG_LOCK: Mutex<()> = Mutex::new(());

pub fn enabled() -> bool {
    env::var_os("COMPI_PERF_LOG").is_some()
}

pub fn empty_window_enabled() -> bool {
    enabled() && env::var_os("COMPI_PERF_EMPTY_WINDOW").is_some()
}

pub fn ready_probe_enabled() -> bool {
    enabled() && env::var_os("COMPI_PERF_READY_PROBE").is_some()
}

pub fn target_session_count() -> usize {
    if !enabled() || empty_window_enabled() {
        return 0;
    }
    env::var("COMPI_PERF_SESSION_COUNT")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|count| (1..=16).contains(count))
        .unwrap_or(1)
}

pub fn set_startup_kind(kind: &'static str) {
    let _ = STARTUP_KIND.set(kind);
}

fn startup_kind() -> String {
    env::var("COMPI_PERF_STARTUP_KIND")
        .ok()
        .filter(|value| matches!(value.as_str(), "cold" | "warm" | "empty"))
        .or_else(|| STARTUP_KIND.get().map(|kind| (*kind).to_owned()))
        .unwrap_or_else(|| "unknown".to_owned())
}

pub fn log_startup_metric(name: &str, elapsed: Duration) {
    if !enabled() {
        return;
    }
    let line = format!(
        "timestamp_ms={} sample={} startup={} metric={} value_ms={}",
        now_ms(),
        sample_id(),
        startup_kind(),
        name,
        elapsed.as_millis()
    );
    append_line("client-startup.log", &line);
}

pub fn begin_input_latency() -> Option<u64> {
    enabled().then(|| {
        (u64::from(std::process::id()) << 32) | NEXT_LATENCY_ID.fetch_add(1, Ordering::Relaxed)
    })
}

pub fn log_input_latency_stage(id: u64, stage: &str, sequence: Option<u64>) {
    if !enabled() {
        return;
    }
    let line = format!(
        "timestamp_us={} sample={} pid={} input_id={} stage={} sequence={}",
        now_us(),
        sample_id(),
        std::process::id(),
        id,
        stage,
        sequence.map_or_else(|| "-".to_owned(), |value| value.to_string())
    );
    append_line(&format!("latency-{}.log", std::process::id()), &line);
}

#[cfg(windows)]
pub fn log_resource_sample(process_kind: &str, workload: &str, session_count: usize) {
    if !enabled() {
        return;
    }
    let metrics = process_metrics();
    let line = format!(
        "timestamp_ms={} sample={} process={} pid={} workload={} sessions={} private_bytes={} working_set_bytes={} handles={}",
        now_ms(),
        sample_id(),
        process_kind,
        std::process::id(),
        workload,
        session_count,
        metrics.private_bytes.unwrap_or_default(),
        metrics.working_set_bytes.unwrap_or_default(),
        metrics.handles.unwrap_or_default()
    );
    append_line(
        &format!("{process_kind}-resource-{}.log", std::process::id()),
        &line,
    );
}

#[cfg(windows)]
pub fn process_metrics() -> ProcessMetrics {
    let process = unsafe { GetCurrentProcess() };
    let mut memory = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let memory_result = unsafe {
        GetProcessMemoryInfo(
            process,
            &mut memory as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    };
    let mut handles = 0;
    let handle_result = unsafe { GetProcessHandleCount(process, &mut handles) };
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let time_result =
        unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) };
    let ticks =
        |value: FILETIME| (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime);
    ProcessMetrics {
        cpu_time_ns: time_result.is_ok().then(|| {
            ticks(kernel)
                .saturating_add(ticks(user))
                .saturating_mul(100)
        }),
        private_bytes: memory_result.is_ok().then_some(memory.PrivateUsage as u64),
        working_set_bytes: memory_result
            .is_ok()
            .then_some(memory.WorkingSetSize as u64),
        handles: handle_result.is_ok().then_some(u64::from(handles)),
        ..Default::default()
    }
}

fn append_line(file_name: &str, line: &str) {
    let Ok(_guard) = LOG_LOCK.lock() else {
        return;
    };
    let Ok(directory) = crate::paths::data_dir() else {
        return;
    };
    if fs::create_dir_all(&directory).is_err() {
        return;
    }
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(file_name))
    {
        let _ = writeln!(file, "{line}");
    }
}

fn sample_id() -> String {
    env::var("COMPI_PERF_SAMPLE")
        .unwrap_or_else(|_| "unspecified".to_owned())
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn now_us() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
}

#[cfg(unix)]
pub fn log_resource_sample(process_kind: &str, workload: &str, session_count: usize) {
    if !enabled() {
        return;
    }
    let metrics = process_metrics();
    let line = format!(
        "timestamp_ms={} sample={} process={} pid={} workload={} sessions={} resident_bytes={} virtual_bytes={} threads={} fds={}",
        now_ms(),
        sample_id(),
        process_kind,
        std::process::id(),
        workload,
        session_count,
        metrics.resident_bytes.unwrap_or_default(),
        metrics.virtual_bytes.unwrap_or_default(),
        metrics.threads.unwrap_or_default(),
        metrics.file_descriptors.unwrap_or_default()
    );
    append_line(
        &format!("{process_kind}-resource-{}.log", std::process::id()),
        &line,
    );
}

#[cfg(target_os = "macos")]
pub fn process_metrics() -> ProcessMetrics {
    let mut task = std::mem::MaybeUninit::<libc::proc_taskinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
    let read = unsafe {
        libc::proc_pidinfo(
            std::process::id() as i32,
            libc::PROC_PIDTASKINFO,
            0,
            task.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return ProcessMetrics::default();
    }
    let task = unsafe { task.assume_init() };
    let fd_bytes = unsafe {
        libc::proc_pidinfo(
            std::process::id() as i32,
            libc::PROC_PIDLISTFDS,
            0,
            std::ptr::null_mut(),
            0,
        )
    };
    ProcessMetrics {
        cpu_time_ns: Some(task.pti_total_user.saturating_add(task.pti_total_system)),
        resident_bytes: Some(task.pti_resident_size),
        virtual_bytes: Some(task.pti_virtual_size),
        threads: Some(u64::from(task.pti_threadnum)),
        file_descriptors: (fd_bytes >= 0).then_some((fd_bytes / libc::PROC_PIDLISTFD_SIZE) as u64),
        ..Default::default()
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn process_metrics() -> ProcessMetrics {
    let statm = fs::read_to_string("/proc/self/statm").ok();
    let (virtual_bytes, resident_bytes) = statm
        .as_deref()
        .and_then(|statm| {
            let mut fields = statm.split_whitespace();
            Some((
                fields.next()?.parse::<u64>().ok()?,
                fields.next()?.parse::<u64>().ok()?,
            ))
        })
        .and_then(|(virtual_pages, resident_pages)| {
            let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            (page_size > 0).then_some((
                virtual_pages.saturating_mul(page_size as u64),
                resident_pages.saturating_mul(page_size as u64),
            ))
        })
        .map_or((None, None), |(virtual_bytes, resident_bytes)| {
            (Some(virtual_bytes), Some(resident_bytes))
        });
    let stat = fs::read_to_string("/proc/self/stat").ok();
    let (cpu_time_ns, threads) = stat
        .as_deref()
        .and_then(|stat| stat.rsplit_once(')'))
        .map(|(_, fields)| fields.split_whitespace().collect::<Vec<_>>())
        .map_or((None, None), |fields| {
            let ticks = fields
                .get(11)
                .and_then(|value| value.parse::<u64>().ok())
                .zip(fields.get(12).and_then(|value| value.parse::<u64>().ok()))
                .map(|(user, system)| user.saturating_add(system));
            let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            let cpu_time_ns = ticks
                .filter(|_| ticks_per_second > 0)
                .map(|ticks| ticks.saturating_mul(1_000_000_000) / ticks_per_second as u64);
            let threads = fields.get(17).and_then(|value| value.parse::<u64>().ok());
            (cpu_time_ns, threads)
        });
    let file_descriptors = fs::read_dir("/proc/self/fd")
        .ok()
        .map(|entries| entries.count().saturating_sub(1) as u64);
    ProcessMetrics {
        cpu_time_ns,
        resident_bytes,
        virtual_bytes,
        file_descriptors,
        threads,
        ..Default::default()
    }
}
