use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::mem::size_of;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

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

pub fn log_resource_sample(process_kind: &str, workload: &str, session_count: usize) {
    if !enabled() {
        return;
    }
    let metrics = current_process_metrics();
    let line = format!(
        "timestamp_ms={} sample={} process={} pid={} workload={} sessions={} private_bytes={} working_set_bytes={} handles={}",
        now_ms(),
        sample_id(),
        process_kind,
        std::process::id(),
        workload,
        session_count,
        metrics.private_bytes,
        metrics.working_set_bytes,
        metrics.handles
    );
    append_line(
        &format!("{process_kind}-resource-{}.log", std::process::id()),
        &line,
    );
}

struct ProcessMetrics {
    private_bytes: usize,
    working_set_bytes: usize,
    handles: u32,
}

fn current_process_metrics() -> ProcessMetrics {
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
    let (private_bytes, working_set_bytes) = if memory_result.is_ok() {
        (memory.PrivateUsage, memory.WorkingSetSize)
    } else {
        (0, 0)
    };
    ProcessMetrics {
        private_bytes,
        working_set_bytes,
        handles: if handle_result.is_ok() { handles } else { 0 },
    }
}

fn append_line(file_name: &str, line: &str) {
    let Ok(_guard) = LOG_LOCK.lock() else {
        return;
    };
    let Some(local_app_data) = env::var_os("LOCALAPPDATA") else {
        return;
    };
    let directory = std::path::PathBuf::from(local_app_data).join("Compi");
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
