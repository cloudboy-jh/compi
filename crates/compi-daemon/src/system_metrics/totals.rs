//! System totals for Monitor's Overview: sampled every 2 s for as long as the daemon runs,
//! keeping the last hour, so every range is filled when a Monitor pane opens. A sample
//! costs a few milliseconds, most of it reading disk counters.

use compi_protocol::system_metrics::{
    CpuTotals, DiskTotals, HostInfo, MemoryKind, MemoryKindName, MemoryTotals, NetworkTotals,
    TOTALS_HISTORY, TOTALS_INTERVAL_MS, TotalsSample,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sysinfo::{CpuRefreshKind, DiskRefreshKind, Disks, MemoryRefreshKind, Networks, System};

/// Samples between two reloads of the disk list and its capacities, which costs tens of
/// milliseconds on macOS.
const DISK_LIST_EVERY: u64 = 30;

pub struct SystemTotalsHub {
    state: Mutex<HubState>,
}

struct HubState {
    host: HostInfo,
    history: VecDeque<Arc<TotalsSample>>,
}

impl SystemTotalsHub {
    pub fn shared() -> &'static Self {
        static HUB: LazyLock<SystemTotalsHub> = LazyLock::new(|| SystemTotalsHub {
            state: Mutex::new(HubState {
                host: host_info(),
                history: VecDeque::with_capacity(TOTALS_HISTORY),
            }),
        });
        &HUB
    }

    /// Samples until `stopping` is set.
    pub fn start(&'static self, stopping: Arc<AtomicBool>) {
        thread::Builder::new()
            .name("compi-system-totals".into())
            .spawn(move || {
                let mut collector = TotalsCollector::new();
                let interval = Duration::from_millis(TOTALS_INTERVAL_MS);
                let mut next = Instant::now();
                while !stopping.load(Ordering::Acquire) {
                    self.publish(collector.collect());
                    next += interval;
                    // A late sample does not make later ones bunch up.
                    let now = Instant::now();
                    if next < now {
                        next = now + interval;
                    }
                    thread::sleep(next - now);
                }
            })
            .expect("spawn system totals sampler");
    }

    fn lock(&self) -> MutexGuard<'_, HubState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn publish(&self, sample: TotalsSample) {
        let mut state = self.lock();
        if state.history.len() == TOTALS_HISTORY {
            state.history.pop_front();
        }
        state.history.push_back(Arc::new(sample));
    }

    /// The host and every kept sample, oldest first, and a watch that continues after them.
    pub fn watch(&self) -> (HostInfo, Vec<TotalsSample>, SystemTotalsWatch) {
        let state = self.lock();
        let history: Vec<TotalsSample> = state.history.iter().map(|s| (**s).clone()).collect();
        let next = history.last().map_or(0, |sample| sample.sequence + 1);
        (state.host.clone(), history, SystemTotalsWatch { next })
    }

    /// Kept samples from `next` on, oldest first.
    fn since(&self, next: u64) -> Vec<Arc<TotalsSample>> {
        let state = self.lock();
        let newest = state.history.back().map_or(0, |sample| sample.sequence + 1);
        let count = newest.saturating_sub(next).min(state.history.len() as u64) as usize;
        state
            .history
            .iter()
            .skip(state.history.len() - count)
            .cloned()
            .collect()
    }
}

/// Which sample a connection is sent next.
pub struct SystemTotalsWatch {
    next: u64,
}

impl SystemTotalsWatch {
    /// Samples this connection has not been sent, oldest first. A connection more than
    /// an hour behind continues from the oldest kept sample.
    pub fn pending(&self) -> Vec<Arc<TotalsSample>> {
        SystemTotalsHub::shared().since(self.next)
    }

    pub fn delivered(&mut self, sample: &TotalsSample) {
        self.next = sample.sequence + 1;
    }
}

fn host_info() -> HostInfo {
    let system = System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing()),
    );
    HostInfo {
        host_name: System::host_name(),
        os: System::long_os_version(),
        cpu: system
            .cpus()
            .first()
            .map(|cpu| cpu.brand().trim().to_owned())
            .filter(|brand| !brand.is_empty()),
        logical_cpus: std::thread::available_parallelism()
            .map_or(1, |count| count.get().min(u32::MAX as usize) as u32),
        booted_at_ms: Some(System::boot_time() * 1000).filter(|&booted| booted > 0),
    }
}

struct TotalsCollector {
    sequence: u64,
    last: Option<Instant>,
    cpu_ticks: Option<platform::CpuTicks>,
    system: System,
    disks: Disks,
    networks: Networks,
}

impl TotalsCollector {
    fn new() -> Self {
        Self {
            sequence: 0,
            last: None,
            cpu_ticks: None,
            system: System::new(),
            disks: Disks::new_with_refreshed_list_specifics(
                DiskRefreshKind::nothing().with_storage().with_io_usage(),
            ),
            networks: Networks::new_with_refreshed_list(),
        }
    }

    fn collect(&mut self) -> TotalsSample {
        let now = Instant::now();
        let elapsed = self.last.replace(now).map(|last| now - last);
        if self.sequence > 0 && self.sequence.is_multiple_of(DISK_LIST_EVERY) {
            self.disks
                .refresh_specifics(true, DiskRefreshKind::nothing().with_storage());
        }
        self.disks
            .refresh_specifics(false, DiskRefreshKind::nothing().with_io_usage());
        self.networks.refresh(true);
        self.system
            .refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram().with_swap());
        let ticks = platform::cpu_ticks();
        let cpu = match (self.cpu_ticks.take(), ticks) {
            (Some(before), Some(after)) => after.since(&before),
            _ => None,
        };
        self.cpu_ticks = ticks;
        let sample = TotalsSample {
            sequence: self.sequence,
            sampled_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis() as u64),
            cpu,
            memory: self.memory(),
            disk: self.disk(elapsed),
            network: self.network(elapsed),
        };
        self.sequence += 1;
        sample
    }

    fn memory(&self) -> Option<MemoryTotals> {
        let total = self.system.total_memory();
        if total == 0 {
            return None;
        }
        let (kinds, pressure) = platform::memory_kinds(total, self.system.available_memory())?;
        let used = kinds
            .iter()
            .filter(|kind| !matches!(kind.kind, MemoryKindName::Cached | MemoryKindName::Free))
            .map(|kind| kind.bytes)
            .sum();
        let swap_total = self.system.total_swap();
        Some(MemoryTotals {
            total_bytes: total,
            used_bytes: used,
            kinds,
            swap_total_bytes: Some(swap_total),
            swap_used_bytes: Some(self.system.used_swap()),
            pressure,
        })
    }

    fn disk(&self, elapsed: Option<Duration>) -> Option<DiskTotals> {
        let system = platform::system_mount();
        let disk = self
            .disks
            .list()
            .iter()
            .find(|disk| disk.mount_point() == std::path::Path::new(&system))?;
        let usage = disk.usage();
        let rate = |bytes: u64| elapsed.map(|elapsed| per_second(bytes, elapsed));
        Some(DiskTotals {
            name: disk.name().to_string_lossy().into_owned(),
            total_bytes: disk.total_space(),
            available_bytes: disk.available_space(),
            read_bytes_per_second: rate(usage.read_bytes),
            written_bytes_per_second: rate(usage.written_bytes),
            written_bytes: Some(usage.total_written_bytes).filter(|&written| written > 0),
        })
    }

    fn network(&self, elapsed: Option<Duration>) -> Option<NetworkTotals> {
        let mut totals = NetworkTotals::default();
        let (mut received, mut sent) = (0, 0);
        let mut any = false;
        for (_, data) in self
            .networks
            .iter()
            .filter(|(_, data)| data.mac_address() != sysinfo::MacAddr::UNSPECIFIED)
        {
            any = true;
            received += data.received();
            sent += data.transmitted();
            totals.received_bytes += data.total_received();
            totals.sent_bytes += data.total_transmitted();
        }
        if !any {
            return None;
        }
        totals.received_bytes_per_second = elapsed.map(|elapsed| per_second(received, elapsed));
        totals.sent_bytes_per_second = elapsed.map(|elapsed| per_second(sent, elapsed));
        Some(totals)
    }
}

fn per_second(bytes: u64, elapsed: Duration) -> u64 {
    (bytes as f64 / elapsed.as_secs_f64().max(0.001)).round() as u64
}

fn kind(kind: MemoryKindName, bytes: u64) -> MemoryKind {
    MemoryKind { kind, bytes }
}

/// Used, Cached and Free from what is available and how much of it is cache.
#[cfg(not(target_os = "macos"))]
fn used_cached_free(total: u64, available: u64, cache: u64) -> Vec<MemoryKind> {
    let available = available.min(total);
    let cached = cache.min(available);
    vec![
        kind(MemoryKindName::Used, total - available),
        kind(MemoryKindName::Cached, cached),
        kind(MemoryKindName::Free, available - cached),
    ]
}

#[cfg(target_os = "macos")]
mod platform {
    //! Activity Monitor's terms: App is anonymous memory less what may be purged, Cached is
    //! file-backed and purgeable memory, and pressure is the kernel's own level.

    use super::kind;
    use compi_protocol::system_metrics::{CpuTotals, MemoryKind, MemoryKindName, MemoryPressure};
    use std::sync::LazyLock;

    /// The host port, taken once: each `mach_host_self` call adds a send right. libc marks
    /// it deprecated in favour of the `mach2` crate, which this one call does not warrant.
    #[allow(deprecated)]
    static HOST: LazyLock<libc::mach_port_t> = LazyLock::new(|| unsafe { libc::mach_host_self() });

    #[derive(Clone, Copy)]
    pub struct CpuTicks([u64; 4]);

    impl CpuTicks {
        /// `cpu_ticks` is user, system, idle, nice.
        pub fn since(&self, before: &Self) -> Option<CpuTotals> {
            let delta: Vec<u64> = (0..4)
                .map(|state| self.0[state].wrapping_sub(before.0[state]))
                .collect();
            super::cpu_share(delta[0] + delta[3], delta[1], delta[2])
        }
    }

    pub fn cpu_ticks() -> Option<CpuTicks> {
        let mut info = unsafe { std::mem::zeroed::<libc::host_cpu_load_info>() };
        let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
        let status = unsafe {
            libc::host_statistics(
                *HOST,
                libc::HOST_CPU_LOAD_INFO,
                (&raw mut info).cast(),
                &mut count,
            )
        };
        (status == libc::KERN_SUCCESS).then(|| CpuTicks(info.cpu_ticks.map(u64::from)))
    }

    pub fn memory_kinds(
        total: u64,
        _available: u64,
    ) -> Option<(Vec<MemoryKind>, Option<MemoryPressure>)> {
        let mut stats = unsafe { std::mem::zeroed::<libc::vm_statistics64>() };
        let mut count = libc::HOST_VM_INFO64_COUNT;
        let status = unsafe {
            libc::host_statistics64(
                *HOST,
                libc::HOST_VM_INFO64,
                (&raw mut stats).cast(),
                &mut count,
            )
        };
        if status != libc::KERN_SUCCESS {
            return None;
        }
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64;
        let pages = |count: u32| u64::from(count) * page;
        let app = pages(
            stats
                .internal_page_count
                .saturating_sub(stats.purgeable_count),
        );
        let wired = pages(stats.wire_count);
        let compressed = pages(stats.compressor_page_count);
        let cached = pages(stats.external_page_count) + pages(stats.purgeable_count);
        let free = total.saturating_sub(app + wired + compressed + cached);
        Some((
            vec![
                kind(MemoryKindName::App, app),
                kind(MemoryKindName::Wired, wired),
                kind(MemoryKindName::Compressed, compressed),
                kind(MemoryKindName::Cached, cached),
                kind(MemoryKindName::Free, free),
            ],
            pressure(),
        ))
    }

    fn pressure() -> Option<MemoryPressure> {
        let mut level: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        let status = unsafe {
            libc::sysctlbyname(
                c"kern.memorystatus_vm_pressure_level".as_ptr(),
                (&raw mut level).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 {
            return None;
        }
        match level {
            1 => Some(MemoryPressure::Normal),
            2 => Some(MemoryPressure::Elevated),
            4 => Some(MemoryPressure::Critical),
            _ => None,
        }
    }

    pub fn system_mount() -> String {
        "/".into()
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use compi_protocol::system_metrics::{CpuTotals, MemoryKind, MemoryPressure};

    #[derive(Clone, Copy)]
    pub struct CpuTicks {
        user: u64,
        system: u64,
        idle: u64,
    }

    impl CpuTicks {
        pub fn since(&self, before: &Self) -> Option<CpuTotals> {
            super::cpu_share(
                self.user.wrapping_sub(before.user),
                self.system.wrapping_sub(before.system),
                self.idle.wrapping_sub(before.idle),
            )
        }
    }

    /// The `cpu` line of `/proc/stat`: user nice system idle iowait irq softirq steal.
    pub fn cpu_ticks() -> Option<CpuTicks> {
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        parse_cpu_line(stat.lines().next()?)
    }

    pub(super) fn parse_cpu_line(line: &str) -> Option<CpuTicks> {
        let mut fields = line.split_whitespace();
        if fields.next()? != "cpu" {
            return None;
        }
        let values: Vec<u64> = fields
            .take(8)
            .map(|field| field.parse().ok())
            .collect::<Option<_>>()?;
        let value = |index: usize| values.get(index).copied().unwrap_or(0);
        Some(CpuTicks {
            user: value(0) + value(1),
            system: value(2) + value(5) + value(6) + value(7),
            idle: value(3) + value(4),
        })
    }

    /// Cache is `Cached`, `Buffers` and `SReclaimable` from `/proc/meminfo`.
    pub fn memory_kinds(
        total: u64,
        available: u64,
    ) -> Option<(Vec<MemoryKind>, Option<MemoryPressure>)> {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        let cache = meminfo
            .lines()
            .filter_map(|line| {
                let (name, rest) = line.split_once(':')?;
                matches!(name, "Cached" | "Buffers" | "SReclaimable")
                    .then(|| rest.split_whitespace().next()?.parse::<u64>().ok())?
            })
            .sum::<u64>()
            * 1024;
        Some((super::used_cached_free(total, available, cache), None))
    }

    pub fn system_mount() -> String {
        "/".into()
    }
}

#[cfg(windows)]
mod platform {
    use compi_protocol::system_metrics::{CpuTotals, MemoryKind, MemoryPressure};
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
    use windows::Win32::System::Threading::GetSystemTimes;

    #[derive(Clone, Copy)]
    pub struct CpuTicks {
        idle: u64,
        kernel: u64,
        user: u64,
    }

    impl CpuTicks {
        /// Kernel time includes idle time.
        pub fn since(&self, before: &Self) -> Option<CpuTotals> {
            let idle = self.idle.wrapping_sub(before.idle);
            let kernel = self.kernel.wrapping_sub(before.kernel);
            super::cpu_share(
                self.user.wrapping_sub(before.user),
                kernel.saturating_sub(idle),
                idle,
            )
        }
    }

    pub fn cpu_ticks() -> Option<CpuTicks> {
        let (mut idle, mut kernel, mut user) = Default::default();
        unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
        let ticks =
            |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
        Some(CpuTicks {
            idle: ticks(idle),
            kernel: ticks(kernel),
            user: ticks(user),
        })
    }

    /// Cache is the system file cache.
    pub fn memory_kinds(
        total: u64,
        available: u64,
    ) -> Option<(Vec<MemoryKind>, Option<MemoryPressure>)> {
        let mut info = PERFORMANCE_INFORMATION {
            cb: std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32,
            ..Default::default()
        };
        let cache = unsafe { GetPerformanceInfo(&mut info, info.cb) }
            .ok()
            .map_or(0, |()| (info.SystemCache * info.PageSize) as u64);
        Some((super::used_cached_free(total, available, cache), None))
    }

    /// The system drive's root, such as `C:\`.
    pub fn system_mount() -> String {
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        format!("{drive}\\")
    }
}

/// Shares of all CPU time spent by programs and by the kernel, as percentages.
fn cpu_share(user: u64, system: u64, idle: u64) -> Option<CpuTotals> {
    let total = user + system + idle;
    (total > 0).then(|| CpuTotals {
        user: (user as f64 * 100.0 / total as f64) as f32,
        system: (system as f64 * 100.0 / total as f64) as f32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_shares_are_percentages_of_all_time() {
        let share = cpu_share(30, 10, 60).unwrap();
        assert_eq!((share.user, share.system), (30.0, 10.0));
        assert_eq!(cpu_share(0, 0, 0), None);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn cache_is_bounded_by_what_is_available() {
        let kinds = used_cached_free(100, 40, 60);
        let bytes: Vec<u64> = kinds.iter().map(|kind| kind.bytes).collect();
        assert_eq!(bytes, [60, 40, 0]);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn proc_stat_cpu_lines_parse() {
        let ticks = platform::parse_cpu_line("cpu  100 5 50 800 20 3 2 1 0 0").unwrap();
        let later = platform::parse_cpu_line("cpu  200 5 100 900 20 3 2 1 0 0").unwrap();
        let share = later.since(&ticks).unwrap();
        assert!((share.user - 40.0).abs() < 0.01 && (share.system - 20.0).abs() < 0.01);
        assert!(platform::parse_cpu_line("intr 1 2 3").is_none());
    }

    #[test]
    fn the_collector_reads_this_host() {
        let mut collector = TotalsCollector::new();
        let first = collector.collect();
        assert_eq!(first.sequence, 0);
        assert_eq!(first.cpu, None, "no CPU share before a second sample");
        let memory = first.memory.expect("memory totals");
        assert_eq!(
            memory.kinds.iter().map(|kind| kind.bytes).sum::<u64>(),
            memory.total_bytes
        );
        assert!(memory.used_bytes > 0 && memory.used_bytes <= memory.total_bytes);
        let disk = first.disk.expect("the system disk");
        assert!(disk.total_bytes >= disk.available_bytes && disk.total_bytes > 0);
        assert_eq!(
            disk.read_bytes_per_second, None,
            "no rates before a second sample"
        );
        thread::sleep(Duration::from_millis(200));
        let second = collector.collect();
        assert_eq!(second.sequence, 1);
        let cpu = second.cpu.expect("CPU share");
        assert!((0.0..=100.0).contains(&(cpu.user + cpu.system)));
        assert!(second.disk.unwrap().read_bytes_per_second.is_some());
    }

    #[test]
    fn watches_continue_after_what_they_were_given() {
        let hub = SystemTotalsHub::shared();
        let base = hub
            .lock()
            .history
            .back()
            .map_or(1_000_000, |s| s.sequence + 1);
        for sequence in base..base + 3 {
            hub.publish(TotalsSample {
                sequence,
                ..TotalsSample::default()
            });
        }
        let (_, history, mut watch) = hub.watch();
        assert_eq!(history.last().unwrap().sequence, base + 2);
        assert!(watch.pending().is_empty());
        hub.publish(TotalsSample {
            sequence: base + 3,
            ..TotalsSample::default()
        });
        let pending = watch.pending();
        assert_eq!(
            pending.iter().map(|s| s.sequence).collect::<Vec<_>>(),
            [base + 3]
        );
        watch.delivered(&pending[0]);
        assert!(watch.pending().is_empty());
    }
}
