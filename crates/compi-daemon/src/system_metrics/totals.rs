//! System totals for Monitor's Overview and resource pages. The host's are sampled every
//! 2 s for as long as the daemon runs, keeping the last hour, so every range is filled when
//! a Monitor pane opens; a sample costs a few milliseconds, most of it reading disk
//! counters. A WSL distribution's arrive from its collector while it is watched.
//!
//! The history keeps the totals only; the newest sample's breakdowns (per-core CPU,
//! volumes, interfaces) are kept once, beside it, and attached when it is sent.

mod gpu;

use super::Sampling;
use compi_protocol::system_metrics::{
    CpuTotals, DiskTotals, HostInfo, MemoryKind, MemoryKindName, MemoryTotals, NetworkInterface,
    NetworkTotals, TOTALS_HISTORY, TOTALS_INTERVAL_MS, TotalsDetail, TotalsSample, Volume,
};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sysinfo::{
    CpuRefreshKind, DiskRefreshKind, Disks, MacAddr, MemoryRefreshKind, Networks, System,
};

/// Samples between two reloads of the disk list and its capacities, which costs tens of
/// milliseconds on macOS.
const DISK_LIST_EVERY: u64 = 30;

pub struct SystemTotalsHub {
    state: Mutex<HubState>,
}

struct HubState {
    /// `None` until the source has described itself: a WSL distribution's collector does
    /// once it runs.
    host: Option<HostInfo>,
    history: VecDeque<Arc<TotalsSample>>,
    /// The newest sample's detail, which the history does not keep.
    detail: Option<Box<TotalsDetail>>,
    /// Why the source cannot be sampled, until its collector starts again.
    failure: Option<String>,
    /// Counts a WSL distribution's collectors, so one that has stopped changes nothing.
    #[cfg_attr(not(windows), allow(dead_code))]
    epoch: u64,
    /// The sequence the next sample numbered here is given.
    next_sequence: u64,
}

impl SystemTotalsHub {
    pub fn shared() -> &'static Arc<Self> {
        static HUB: LazyLock<Arc<SystemTotalsHub>> =
            LazyLock::new(|| Arc::new(SystemTotalsHub::new(Some(host_info()))));
        &HUB
    }

    pub(super) fn new(host: Option<HostInfo>) -> Self {
        Self {
            state: Mutex::new(HubState {
                host,
                history: VecDeque::with_capacity(TOTALS_HISTORY),
                detail: None,
                failure: None,
                epoch: 0,
                next_sequence: 0,
            }),
        }
    }

    /// Samples the host until `stopping` is set.
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
        self.lock().push(sample);
    }
}

/// What a WSL distribution's collector reports, which only a Windows daemon runs.
#[cfg_attr(not(windows), allow(dead_code))]
impl SystemTotalsHub {
    /// A new collector starts: what the last one sampled is forgotten, and what it reports
    /// under the returned epoch is kept. Sequences continue, so no watch goes back.
    pub(super) fn begin(&self) -> u64 {
        let mut state = self.lock();
        state.epoch += 1;
        state.host = None;
        state.history.clear();
        state.detail = None;
        state.failure = None;
        state.epoch
    }

    /// The collector of `epoch` describes its source.
    pub(super) fn describe(&self, epoch: u64, host: HostInfo) {
        let mut state = self.lock();
        if state.epoch == epoch {
            state.host = Some(host);
            state.failure = None;
        }
    }

    /// The boot epoch used by the current collector's process identities. WSL's live
    /// `/proc/stat` boot epoch can shift when its clock is synchronized.
    pub(super) fn sampled_boot_time(&self) -> Option<u64> {
        self.lock()
            .host
            .as_ref()?
            .booted_at_ms
            .map(|time| time / 1000)
    }

    /// The collector of `epoch` sampled; the sample is numbered after the last one.
    pub(super) fn publish_numbered(&self, epoch: u64, mut sample: TotalsSample) {
        let mut state = self.lock();
        if state.epoch == epoch {
            sample.sequence = state.next_sequence;
            state.push(sample);
        }
    }

    /// The collector of `epoch` could not sample its source.
    pub(super) fn fail(&self, epoch: u64, reason: String) {
        let mut state = self.lock();
        if state.epoch == epoch && state.host.is_none() {
            state.failure = Some(reason);
        }
    }
}

impl SystemTotalsHub {
    /// A watch of these totals, which keeps `sampling` (a WSL distribution's collector)
    /// running while it lasts.
    pub fn watch(self: &Arc<Self>, sampling: Option<Sampling>) -> SystemTotalsWatch {
        SystemTotalsWatch {
            hub: self.clone(),
            next: None,
            _sampling: sampling,
        }
    }

    /// The host and every kept sample, oldest first, the newest with its detail, once the
    /// source has described itself.
    fn opened(&self) -> Option<Result<(HostInfo, Vec<TotalsSample>), String>> {
        let state = self.lock();
        if let Some(failure) = &state.failure {
            return Some(Err(failure.clone()));
        }
        let host = state.host.clone()?;
        Some(Ok((host, state.with_detail(state.history.iter()))))
    }

    /// Kept samples from `next` on, oldest first, the newest with its detail.
    fn since(&self, next: u64) -> Vec<TotalsSample> {
        let state = self.lock();
        let newest = state.history.back().map_or(0, |sample| sample.sequence + 1);
        let count = newest.saturating_sub(next).min(state.history.len() as u64) as usize;
        state.with_detail(state.history.iter().skip(state.history.len() - count))
    }
}

impl HubState {
    /// Keeps `sample`, its detail beside the history.
    fn push(&mut self, mut sample: TotalsSample) {
        let detail = sample.detail.take();
        if self.history.len() == TOTALS_HISTORY {
            self.history.pop_front();
        }
        self.next_sequence = sample.sequence + 1;
        self.history.push_back(Arc::new(sample));
        self.detail = detail;
    }

    /// Copies of `samples`, which end with the newest, that one given the detail.
    fn with_detail<'a>(
        &self,
        samples: impl ExactSizeIterator<Item = &'a Arc<TotalsSample>>,
    ) -> Vec<TotalsSample> {
        let mut copies: Vec<TotalsSample> = samples.map(|sample| (**sample).clone()).collect();
        if let Some(newest) = copies.last_mut() {
            newest.detail = self.detail.clone();
        }
        copies
    }
}

/// What one watching connection has been sent.
pub struct SystemTotalsWatch {
    hub: Arc<SystemTotalsHub>,
    /// The sample this connection is sent next, once it has been answered.
    next: Option<u64>,
    _sampling: Option<Sampling>,
}

impl SystemTotalsWatch {
    /// The source's host and kept samples, oldest first, the newest with its detail, once
    /// the source has described itself (at once for the daemon's host); `Err` when it
    /// cannot be sampled. Samples after them are then `pending`.
    pub fn open(&mut self) -> Option<Result<(HostInfo, Vec<TotalsSample>), String>> {
        let opened = self.hub.opened()?;
        if let Ok((_, history)) = &opened {
            self.next = Some(history.last().map_or(0, |sample| sample.sequence + 1));
        }
        Some(opened)
    }

    /// Samples this connection has not been sent since it was opened, oldest first, the
    /// newest with its detail. A connection more than an hour behind continues from the
    /// oldest kept sample.
    pub fn pending(&self) -> Vec<TotalsSample> {
        self.next.map_or_else(Vec::new, |next| self.hub.since(next))
    }

    /// The sample numbered `sequence` was sent.
    pub fn delivered(&mut self, sequence: u64) {
        self.next = Some(sequence + 1);
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
    /// Each logical CPU's ticks at the last sample, and a buffer for the next ones.
    core_ticks: Vec<platform::CoreTicks>,
    next_core_ticks: Vec<platform::CoreTicks>,
    system: System,
    disks: Disks,
    volumes: Vec<Volume>,
    networks: Networks,
    gpu: gpu::GpuReader,
}

impl TotalsCollector {
    fn new() -> Self {
        let disks = Disks::new_with_refreshed_list_specifics(
            DiskRefreshKind::nothing().with_storage().with_io_usage(),
        );
        Self {
            sequence: 0,
            last: None,
            cpu_ticks: None,
            core_ticks: Vec::new(),
            next_core_ticks: Vec::new(),
            system: System::new(),
            volumes: mounted_volumes(&disks),
            disks,
            networks: Networks::new_with_refreshed_list(),
            gpu: gpu::GpuReader::new(),
        }
    }

    fn collect(&mut self) -> TotalsSample {
        let now = Instant::now();
        let elapsed = self.last.replace(now).map(|last| now - last);
        if self.sequence > 0 && self.sequence.is_multiple_of(DISK_LIST_EVERY) {
            self.disks
                .refresh_specifics(true, DiskRefreshKind::nothing().with_storage());
            self.volumes = mounted_volumes(&self.disks);
        }
        self.disks
            .refresh_specifics(false, DiskRefreshKind::nothing().with_io_usage());
        self.networks.refresh(true);
        self.system
            .refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram().with_swap());
        let ticks = platform::cpu_ticks(&mut self.next_core_ticks);
        let cpu = match (self.cpu_ticks.take(), ticks) {
            (Some(before), Some(after)) => after.since(&before),
            _ => None,
        };
        self.cpu_ticks = ticks;
        let cores = core_shares(&self.core_ticks, &self.next_core_ticks);
        std::mem::swap(&mut self.core_ticks, &mut self.next_core_ticks);
        let interfaces = self.interfaces(elapsed);
        let sample = TotalsSample {
            sequence: self.sequence,
            sampled_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis() as u64),
            cpu,
            memory: self.memory(),
            disk: self.disk(elapsed),
            network: network_totals(&interfaces),
            gpu: self.gpu.read(),
            detail: Some(Box::new(TotalsDetail {
                cores,
                volumes: self.volumes.clone(),
                interfaces,
            })),
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
        let available: u64 = kinds
            .iter()
            .filter(|kind| matches!(kind.kind, MemoryKindName::Cached | MemoryKindName::Free))
            .map(|kind| kind.bytes)
            .sum();
        let swap_total = self.system.total_swap();
        Some(MemoryTotals {
            total_bytes: total,
            used_bytes: total.saturating_sub(available),
            available_bytes: Some(available),
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
            .find(|disk| disk.mount_point() == Path::new(&system))?;
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

    fn interfaces(&self, elapsed: Option<Duration>) -> Vec<NetworkInterface> {
        hardware_interfaces(
            self.networks.iter().map(|(name, data)| {
                (
                    name.as_str(),
                    InterfaceCounters {
                        mac: data.mac_address(),
                        received: data.received(),
                        sent: data.transmitted(),
                        total_received: data.total_received(),
                        total_sent: data.total_transmitted(),
                    },
                )
            }),
            elapsed,
        )
    }
}

/// One interface's byte counts: since the last refresh, and since its counters started.
struct InterfaceCounters {
    mac: MacAddr,
    received: u64,
    sent: u64,
    total_received: u64,
    total_sent: u64,
}

/// Interfaces with a hardware address, so loopback and tunnels that carry the same traffic
/// again are left out, by name.
fn hardware_interfaces<'a>(
    interfaces: impl Iterator<Item = (&'a str, InterfaceCounters)>,
    elapsed: Option<Duration>,
) -> Vec<NetworkInterface> {
    let rate = |bytes: u64| elapsed.map(|elapsed| per_second(bytes, elapsed));
    let mut list: Vec<NetworkInterface> = interfaces
        .filter(|(_, counters)| counters.mac != MacAddr::UNSPECIFIED)
        .map(|(name, counters)| NetworkInterface {
            name: name.to_owned(),
            received_bytes_per_second: rate(counters.received),
            sent_bytes_per_second: rate(counters.sent),
            received_bytes: counters.total_received,
            sent_bytes: counters.total_sent,
        })
        .collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

pub(super) fn network_totals(interfaces: &[NetworkInterface]) -> Option<NetworkTotals> {
    if interfaces.is_empty() {
        return None;
    }
    let sum = |rate: fn(&NetworkInterface) -> Option<u64>| {
        interfaces.iter().map(rate).sum::<Option<u64>>()
    };
    Some(NetworkTotals {
        received_bytes_per_second: sum(|interface| interface.received_bytes_per_second),
        sent_bytes_per_second: sum(|interface| interface.sent_bytes_per_second),
        received_bytes: interfaces.iter().map(|i| i.received_bytes).sum(),
        sent_bytes: interfaces.iter().map(|i| i.sent_bytes).sum(),
    })
}

/// The volume's label where the host has one, else its mount point's last part, else
/// the mount point (`/`, `C:\`).
pub(super) fn volume_name(label: Option<&str>, mount_point: &str) -> String {
    label
        .filter(|label| !label.is_empty())
        .or_else(|| Path::new(mount_point).file_name()?.to_str())
        .unwrap_or(mount_point)
        .to_owned()
}

/// The volumes a user stores files on, as of the last reload of the disk list.
fn mounted_volumes(disks: &Disks) -> Vec<Volume> {
    let mounted = disks.list().iter().map(|disk| {
        let mount_point = disk.mount_point().to_string_lossy().into_owned();
        let device = disk.name().to_string_lossy();
        let file_system = disk.file_system().to_string_lossy();
        let volume = Volume {
            name: volume_name(platform::VOLUME_LABELS.then_some(&*device), &mount_point),
            mount_point,
            file_system: Some(file_system.into_owned()).filter(|name| !name.is_empty()),
            total_bytes: disk.total_space(),
            available_bytes: disk.available_space(),
            removable: disk.is_removable(),
        };
        (device.into_owned(), volume)
    });
    user_volumes(
        mounted,
        &platform::system_mount(),
        platform::user_volume,
        platform::MOUNTS_SHARE_DEVICES,
    )
}

/// The volumes a user stores files on from what is mounted, each `(device, volume)`: the
/// one at `system` first and the rest by mount point. Where `share_devices`, a device
/// mounted again (a bind mount, another subvolume) is shown once, at its first mount point
/// in that order.
pub(super) fn user_volumes(
    mounted: impl Iterator<Item = (String, Volume)>,
    system: &str,
    user_volume: fn(&Volume) -> bool,
    share_devices: bool,
) -> Vec<Volume> {
    let mut mounted: Vec<(String, Volume)> = mounted
        .filter(|(_, volume)| volume.total_bytes > 0 && user_volume(volume))
        .collect();
    let order = |a: &Volume, b: &Volume| {
        (a.mount_point != system)
            .cmp(&(b.mount_point != system))
            .then_with(|| a.mount_point.cmp(&b.mount_point))
    };
    if share_devices {
        mounted.sort_by(|(a_device, a), (b_device, b)| a_device.cmp(b_device).then(order(a, b)));
        mounted.dedup_by(|(later, _), (first, _)| later == first);
    }
    mounted.sort_by(|(_, a), (_, b)| order(a, b));
    mounted.into_iter().map(|(_, volume)| volume).collect()
}

/// Linux: real file systems outside the system's own mounts; removable media under
/// `/run/media` count.
pub(super) fn linux_user_volume(volume: &Volume) -> bool {
    /// Kernel and memory file systems, and those that only repackage others.
    const PSEUDO_FILE_SYSTEMS: &[&str] = &[
        "autofs",
        "binfmt_misc",
        "bpf",
        "cgroup",
        "cgroup2",
        "configfs",
        "debugfs",
        "devpts",
        "devtmpfs",
        "efivarfs",
        "fuse.gvfsd-fuse",
        "fuse.portal",
        "fuse.snapfuse",
        "fusectl",
        "hugetlbfs",
        "mqueue",
        "nsfs",
        "overlay",
        "proc",
        "pstore",
        "ramfs",
        "rpc_pipefs",
        "securityfs",
        "squashfs",
        "sysfs",
        "tmpfs",
        "tracefs",
    ];
    /// Where the system mounts what is not the user's files.
    const SYSTEM_MOUNTS: &[&str] = &[
        "/boot",
        "/dev",
        "/proc",
        "/run",
        "/snap",
        "/sys",
        "/var/lib/docker",
        "/var/snap",
    ];
    let file_system = volume.file_system.as_deref().unwrap_or_default();
    if PSEUDO_FILE_SYSTEMS.contains(&file_system) {
        return false;
    }
    let mount = volume.mount_point.as_str();
    mount.starts_with("/run/media/") || !under_any(mount, SYSTEM_MOUNTS)
}

/// Whether `path` is one of `roots` or inside one.
pub(super) fn under_any(path: &str, roots: &[&str]) -> bool {
    roots.iter().any(|root| {
        path.strip_prefix(root)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// Busy share of each logical CPU between two readings, 0 to 100; none when the CPUs
/// changed between them.
fn core_shares(before: &[platform::CoreTicks], after: &[platform::CoreTicks]) -> Vec<f32> {
    if before.len() != after.len() {
        return Vec::new();
    }
    before
        .iter()
        .zip(after)
        .map(|(before, after)| {
            let (busy, idle) = after.since(before);
            let total = busy + idle;
            if total == 0 {
                0.0
            } else {
                (busy as f64 * 100.0 / total as f64) as f32
            }
        })
        .collect()
}

pub(super) fn per_second(bytes: u64, elapsed: Duration) -> u64 {
    (bytes as f64 / elapsed.as_secs_f64().max(0.001)).round() as u64
}

fn kind(kind: MemoryKindName, bytes: u64) -> MemoryKind {
    MemoryKind { kind, bytes }
}

/// Used, Cached and Free from what is available and how much of it is cache.
pub(super) fn used_cached_free(total: u64, available: u64, cache: u64) -> Vec<MemoryKind> {
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
    use compi_protocol::system_metrics::{
        CpuTotals, MemoryKind, MemoryKindName, MemoryPressure, Volume,
    };
    use std::sync::LazyLock;

    /// Disk names are volume labels, such as "Macintosh HD".
    pub const VOLUME_LABELS: bool = true;
    pub const MOUNTS_SHARE_DEVICES: bool = false;

    /// What Finder shows: the system volume and what is mounted under `/Volumes`. The
    /// system's own volumes under `/System/Volumes` (Data shares the system volume's
    /// container and space) and disk images mounted elsewhere are left out.
    pub fn user_volume(volume: &Volume) -> bool {
        volume.mount_point == "/" || volume.mount_point.starts_with("/Volumes/")
    }

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

    /// One logical CPU's `cpu_ticks`: user, system, idle, nice, each wrapping at 32 bits.
    #[derive(Clone, Copy)]
    pub struct CoreTicks(pub [u32; 4]);

    impl CoreTicks {
        /// Busy and idle ticks since `before`.
        pub fn since(&self, before: &Self) -> (u64, u64) {
            let delta = |state: usize| u64::from(self.0[state].wrapping_sub(before.0[state]));
            (delta(0) + delta(1) + delta(3), delta(2))
        }
    }

    /// The host's ticks, with each logical CPU's put in `cores`.
    pub fn cpu_ticks(cores: &mut Vec<CoreTicks>) -> Option<CpuTicks> {
        core_ticks(cores);
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

    /// `host_processor_info` hands over an array the caller deallocates; libc marks the
    /// task port deprecated as it does the host port.
    #[allow(deprecated)]
    fn core_ticks(cores: &mut Vec<CoreTicks>) {
        cores.clear();
        let mut count: libc::natural_t = 0;
        let mut info: libc::processor_info_array_t = std::ptr::null_mut();
        let mut info_count: libc::mach_msg_type_number_t = 0;
        let status = unsafe {
            libc::host_processor_info(
                *HOST,
                libc::PROCESSOR_CPU_LOAD_INFO,
                &mut count,
                &mut info,
                &mut info_count,
            )
        };
        if status != libc::KERN_SUCCESS || info.is_null() {
            return;
        }
        let loads = unsafe {
            std::slice::from_raw_parts(info.cast::<libc::processor_cpu_load_info>(), count as usize)
        };
        cores.extend(loads.iter().map(|load| CoreTicks(load.cpu_ticks)));
        unsafe {
            libc::vm_deallocate(
                libc::mach_task_self(),
                info as libc::vm_address_t,
                info_count as usize * std::mem::size_of::<libc::integer_t>(),
            );
        }
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

    /// Disk names are devices, such as `/dev/nvme0n1p2`, not labels.
    pub const VOLUME_LABELS: bool = false;
    pub const MOUNTS_SHARE_DEVICES: bool = true;

    pub(super) use super::linux_user_volume as user_volume;

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

    /// One logical CPU's ticks.
    #[derive(Clone, Copy)]
    pub struct CoreTicks(CpuTicks);

    impl CoreTicks {
        /// Busy and idle ticks since `before`.
        pub fn since(&self, before: &Self) -> (u64, u64) {
            let (now, before) = (self.0, before.0);
            (
                (now.user + now.system).wrapping_sub(before.user + before.system),
                now.idle.wrapping_sub(before.idle),
            )
        }
    }

    /// The host's ticks from `/proc/stat`, with each logical CPU's put in `cores`.
    pub fn cpu_ticks(cores: &mut Vec<CoreTicks>) -> Option<CpuTicks> {
        cores.clear();
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        parse_stat(&stat, cores)
    }

    /// The `cpu` line for the host and the `cpuN` lines that follow for each logical CPU.
    pub(super) fn parse_stat(stat: &str, cores: &mut Vec<CoreTicks>) -> Option<CpuTicks> {
        let mut lines = stat.lines();
        let (label, host) = parse_cpu_line(lines.next()?)?;
        if label != "cpu" {
            return None;
        }
        cores.extend(
            lines
                .map_while(parse_cpu_line)
                .map(|(_, ticks)| CoreTicks(ticks)),
        );
        Some(host)
    }

    fn parse_cpu_line(line: &str) -> Option<(&str, CpuTicks)> {
        let (label, ticks) = super::super::procfs::cpu_line(line)?;
        Some((
            label,
            CpuTicks {
                user: ticks.user,
                system: ticks.system,
                idle: ticks.idle,
            },
        ))
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
    use compi_protocol::system_metrics::{CpuTotals, MemoryKind, MemoryPressure, Volume};
    use windows::Wdk::System::SystemInformation::{
        NtQuerySystemInformation, SystemProcessorPerformanceInformation,
    };
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
    use windows::Win32::System::Threading::GetSystemTimes;
    use windows::Win32::System::WindowsProgramming::SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION;

    /// Disk names are volume labels, such as "Windows"; unlabelled drives have none.
    pub const VOLUME_LABELS: bool = true;
    pub const MOUNTS_SHARE_DEVICES: bool = false;

    /// Every drive with a medium in it.
    pub fn user_volume(_volume: &Volume) -> bool {
        true
    }

    /// Logical CPUs in one processor group, the most the kernel reports to a thread.
    const MAX_GROUP_CPUS: usize = 64;

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

    /// One logical CPU's idle, kernel (including idle) and user time.
    #[derive(Clone, Copy)]
    pub struct CoreTicks(CpuTicks);

    impl CoreTicks {
        /// Busy and idle time since `before`.
        pub fn since(&self, before: &Self) -> (u64, u64) {
            let (now, before) = (self.0, before.0);
            let idle = now.idle.wrapping_sub(before.idle);
            let kernel = now.kernel.wrapping_sub(before.kernel);
            (
                now.user.wrapping_sub(before.user) + kernel.saturating_sub(idle),
                idle,
            )
        }
    }

    /// The host's times, with each logical CPU's (in the sampler's processor group) put
    /// in `cores`.
    pub fn cpu_ticks(cores: &mut Vec<CoreTicks>) -> Option<CpuTicks> {
        core_ticks(cores);
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

    fn core_ticks(cores: &mut Vec<CoreTicks>) {
        cores.clear();
        let mut info = [SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION::default(); MAX_GROUP_CPUS];
        let mut written = 0u32;
        let status = unsafe {
            NtQuerySystemInformation(
                SystemProcessorPerformanceInformation,
                info.as_mut_ptr().cast(),
                std::mem::size_of_val(&info) as u32,
                &mut written,
            )
        };
        if status.is_err() {
            return;
        }
        let count =
            written as usize / std::mem::size_of::<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION>();
        cores.extend(info[..count.min(MAX_GROUP_CPUS)].iter().map(|cpu| {
            CoreTicks(CpuTicks {
                idle: cpu.IdleTime as u64,
                kernel: cpu.KernelTime as u64,
                user: cpu.UserTime as u64,
            })
        }));
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
pub(super) fn cpu_share(user: u64, system: u64, idle: u64) -> Option<CpuTotals> {
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

    #[test]
    fn cache_is_bounded_by_what_is_available() {
        let kinds = used_cached_free(100, 40, 60);
        let bytes: Vec<u64> = kinds.iter().map(|kind| kind.bytes).collect();
        assert_eq!(bytes, [60, 40, 0]);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn proc_stat_cpu_lines_parse() {
        let mut cores = Vec::new();
        let stat = "cpu  100 5 50 800 20 3 2 1 0 0\n\
                    cpu0 60 5 30 400 10 3 2 1 0 0\n\
                    cpu1 40 0 20 400 10 0 0 0 0 0\n\
                    intr 1 2 3\n";
        let ticks = platform::parse_stat(stat, &mut cores).unwrap();
        assert_eq!(cores.len(), 2);
        let mut later_cores = Vec::new();
        let later = platform::parse_stat(
            "cpu  200 5 100 900 20 3 2 1 0 0\n\
             cpu0 135 5 55 400 10 3 2 1 0 0\n\
             cpu1 65 0 45 500 10 0 0 0 0 0\n",
            &mut later_cores,
        )
        .unwrap();
        let share = later.since(&ticks).unwrap();
        assert!((share.user - 40.0).abs() < 0.01 && (share.system - 20.0).abs() < 0.01);
        let shares = core_shares(&cores, &later_cores);
        assert!(shares.len() == 2 && shares[0] == 100.0 && (shares[1] - 100.0 / 3.0).abs() < 0.01);
        assert!(platform::parse_stat("intr 1 2 3", &mut cores).is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn each_core_is_busy_for_its_share_of_its_own_ticks() {
        use platform::CoreTicks;
        // user, system, idle, nice
        let before = [
            CoreTicks([100, 50, 800, 0]),
            CoreTicks([u32::MAX - 9, 0, 0, 0]),
        ];
        let after = [CoreTicks([130, 60, 850, 10]), CoreTicks([20, 0, 90, 0])];
        assert_eq!(core_shares(&before, &after), [50.0, 25.0]);
        assert_eq!(
            core_shares(&before, &before),
            [0.0, 0.0],
            "no ticks is idle"
        );
        assert!(
            core_shares(&before, &after[..1]).is_empty(),
            "CPUs that changed between readings give no shares"
        );
    }

    #[cfg(unix)]
    fn volume(mount_point: &str, file_system: &str, total_bytes: u64) -> Volume {
        Volume {
            name: volume_name(None, mount_point),
            mount_point: mount_point.into(),
            file_system: Some(file_system.into()),
            total_bytes,
            available_bytes: total_bytes / 2,
            removable: false,
        }
    }

    #[test]
    fn volumes_are_named_by_label_else_their_mount_point() {
        assert_eq!(volume_name(Some("Macintosh HD"), "/"), "Macintosh HD");
        assert_eq!(volume_name(Some(""), "/Volumes/Backup"), "Backup");
        assert_eq!(volume_name(None, "/home"), "home");
        assert_eq!(volume_name(None, "/"), "/");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_shows_the_volumes_finder_shows_the_system_first() {
        let mounted = [
            volume("/Volumes/Backup", "apfs", 4 << 40),
            volume("/System/Volumes/Data", "apfs", 1 << 40),
            volume("/System/Volumes/Preboot", "apfs", 1 << 40),
            volume("/dev", "devfs", 1 << 20),
            volume("/", "apfs", 1 << 40),
            volume("/Volumes/Card", "exfat", 0),
            volume(
                "/Library/Developer/CoreSimulator/Volumes/iOS",
                "apfs",
                1 << 34,
            ),
            volume("/Volumes/Archive", "smbfs", 8 << 40),
        ];
        let shown = user_volumes(
            mounted
                .into_iter()
                .map(|volume| (volume.mount_point.clone(), volume)),
            "/",
            platform::user_volume,
            platform::MOUNTS_SHARE_DEVICES,
        );
        let mounts: Vec<&str> = shown.iter().map(|v| v.mount_point.as_str()).collect();
        assert_eq!(mounts, ["/", "/Volumes/Archive", "/Volumes/Backup"]);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_shows_real_file_systems_once_each() {
        let mounted = [
            ("/dev/sda2", volume("/home", "btrfs", 1 << 40)),
            ("/dev/sda2", volume("/", "btrfs", 1 << 40)),
            ("tmpfs", volume("/tmp", "tmpfs", 1 << 30)),
            ("/dev/sda1", volume("/boot/efi", "vfat", 1 << 29)),
            ("/dev/loop0", volume("/snap/core/1", "squashfs", 1 << 26)),
            ("/dev/sdb1", volume("/run/media/me/USB", "exfat", 1 << 35)),
            ("/dev/sdc1", volume("/run/user/1000/doc", "ext4", 1 << 35)),
            ("/dev/sdd1", volume("/data", "ext4", 1 << 41)),
        ];
        let shown = user_volumes(
            mounted
                .into_iter()
                .map(|(device, volume)| (device.to_owned(), volume)),
            "/",
            platform::user_volume,
            platform::MOUNTS_SHARE_DEVICES,
        );
        let mounts: Vec<&str> = shown.iter().map(|v| v.mount_point.as_str()).collect();
        assert_eq!(mounts, ["/", "/data", "/run/media/me/USB"]);
    }

    #[test]
    fn interfaces_with_a_hardware_address_make_the_network_totals() {
        let counters = |mac: [u8; 6], received: u64| InterfaceCounters {
            mac: MacAddr(mac),
            received,
            sent: received / 2,
            total_received: received * 100,
            total_sent: received * 50,
        };
        let hardware = [0x02, 0, 0, 0, 0, 1];
        let interfaces = hardware_interfaces(
            [
                ("en1", counters(hardware, 2000)),
                ("lo0", counters([0; 6], 9000)),
                ("utun3", counters([0; 6], 4000)),
                ("en0", counters(hardware, 4000)),
            ]
            .into_iter(),
            Some(Duration::from_secs(2)),
        );
        let names: Vec<&str> = interfaces.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["en0", "en1"]);
        assert_eq!(interfaces[0].received_bytes_per_second, Some(2000));
        assert_eq!(
            network_totals(&interfaces),
            Some(NetworkTotals {
                received_bytes_per_second: Some(3000),
                sent_bytes_per_second: Some(1500),
                received_bytes: 600_000,
                sent_bytes: 300_000,
            })
        );
        let first = hardware_interfaces([("en0", counters(hardware, 1))].into_iter(), None);
        assert_eq!(first[0].received_bytes_per_second, None);
        assert_eq!(network_totals(&first).unwrap().sent_bytes_per_second, None);
        assert_eq!(network_totals(&[]), None);
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
        assert_eq!(
            memory.available_bytes,
            Some(memory.total_bytes - memory.used_bytes)
        );
        let disk = first.disk.expect("the system disk");
        assert!(disk.total_bytes >= disk.available_bytes && disk.total_bytes > 0);
        assert_eq!(
            disk.read_bytes_per_second, None,
            "no rates before a second sample"
        );
        let detail = first.detail.expect("the newest sample's detail");
        assert!(
            detail.cores.is_empty(),
            "no core shares before a second sample"
        );
        assert_eq!(
            detail.volumes.first().map(|volume| &volume.mount_point),
            Some(&platform::system_mount()),
            "the system volume first"
        );
        thread::sleep(Duration::from_millis(200));
        let second = collector.collect();
        assert_eq!(second.sequence, 1);
        let cpu = second.cpu.expect("CPU share");
        assert!((0.0..=100.0).contains(&(cpu.user + cpu.system)));
        assert!(second.disk.unwrap().read_bytes_per_second.is_some());
        let detail = second.detail.expect("the newest sample's detail");
        assert_eq!(detail.cores.len(), host_info().logical_cpus as usize);
        assert!(detail.cores.iter().all(|core| (0.0..=100.0).contains(core)));
        let network = network_totals(&detail.interfaces);
        assert_eq!(
            network.map(|network| network.received_bytes),
            second.network.map(|network| network.received_bytes)
        );
    }

    #[test]
    fn only_the_newest_sample_carries_its_detail() {
        let hub = Arc::new(SystemTotalsHub::new(Some(HostInfo::default())));
        let sample = |sequence: u64, core: f32| TotalsSample {
            sequence,
            detail: Some(Box::new(TotalsDetail {
                cores: vec![core],
                ..TotalsDetail::default()
            })),
            ..TotalsSample::default()
        };
        for sequence in 0..3 {
            hub.publish(sample(sequence, sequence as f32));
        }
        assert!(
            hub.lock().history.iter().all(|kept| kept.detail.is_none()),
            "the history keeps no detail"
        );
        let (_, history) = hub.watch(None).open().unwrap().unwrap();
        let cores: Vec<Option<Vec<f32>>> = history
            .iter()
            .map(|s| s.detail.as_ref().map(|detail| detail.cores.clone()))
            .collect();
        assert_eq!(cores, [None, None, Some(vec![2.0])]);
        hub.publish(sample(3, 3.0));
        let pending = hub.since(2);
        assert_eq!(pending[0].detail, None);
        assert_eq!(pending[1].detail.as_ref().unwrap().cores, [3.0]);
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
        let mut watch = hub.watch(None);
        let (_, history) = watch.open().unwrap().unwrap();
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
        watch.delivered(pending[0].sequence);
        assert!(watch.pending().is_empty());
    }

    #[test]
    fn a_collectors_totals_open_once_it_describes_its_source() {
        let hub = Arc::new(SystemTotalsHub::new(None));
        let epoch = hub.begin();
        let mut watch = hub.watch(None);
        assert_eq!(watch.open(), None, "not described yet");
        assert_eq!(hub.sampled_boot_time(), None);
        assert!(watch.pending().is_empty(), "nothing before it opens");
        hub.publish_numbered(epoch, TotalsSample::default());
        let host = HostInfo {
            host_name: Some("wsl".into()),
            booted_at_ms: Some(1_792_553_694_000),
            ..HostInfo::default()
        };
        hub.describe(epoch, host.clone());
        assert_eq!(hub.sampled_boot_time(), Some(1_792_553_694));
        let (described, history) = watch.open().unwrap().unwrap();
        assert_eq!(described, host);
        assert_eq!(history.iter().map(|s| s.sequence).collect::<Vec<_>>(), [0]);
        hub.publish_numbered(epoch, TotalsSample::default());
        assert_eq!(watch.pending()[0].sequence, 1);

        // A later collector starts over without going back; a stopped one changes nothing.
        let later = hub.begin();
        assert_eq!(
            hub.sampled_boot_time(),
            None,
            "a new collector forgets the old epoch"
        );
        hub.publish_numbered(epoch, TotalsSample::default());
        hub.fail(epoch, "stopped".into());
        assert!(watch.pending().is_empty());
        let mut fresh = hub.watch(None);
        assert_eq!(fresh.open(), None);
        hub.fail(later, "WSL is not running".into());
        assert_eq!(fresh.open(), Some(Err("WSL is not running".into())));
        hub.describe(later, host);
        hub.publish_numbered(later, TotalsSample::default());
        assert_eq!(watch.pending()[0].sequence, 2);
    }
}
