//! A WSL 2 distribution as a Monitor source. No Linux binary is shipped: a POSIX sh and awk
//! script, `assets/compi-monitor.sh`, runs inside the distribution through `wsl.exe` and
//! streams what it reads from `/proc`. This module parses its output on every platform, so
//! the parsing is tested everywhere; only a Windows daemon runs it (`source`).
// A Unix daemon has no WSL; the tests still run the parsing.
#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod source;

#[cfg(windows)]
pub use source::{hubs, inspect, quit};

use super::process::cut_arguments;
use super::procfs::{self, CpuTicks};
use super::totals::{
    cpu_share, linux_user_volume, network_totals, per_second, under_any, used_cached_free,
    user_volumes, volume_name,
};
use super::{Collected, cut_text};
use compi_protocol::SurfaceId;
use compi_protocol::system_metrics::{
    AppGroup, CpuTotals, DiskTotals, HostInfo, MAX_COMMAND_BYTES, MemoryKindName, MemoryTotals,
    NetworkInterface, Process, ProcessDetails, ProcessInspection, ProcessOwner, ProcessSample,
    QuitOutcome, TotalsDetail, TotalsSample, Volume,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The collector, piped into `/bin/sh -s MODE …` inside the distribution.
pub const SCRIPT: &str = include_str!("../../../../assets/compi-monitor.sh");

/// The usual first person account (`UID_MIN`); below it are system accounts.
const FIRST_PERSON_UID: u32 = 1000;
const NOBODY: u32 = 65534;
/// What WSL mounts itself: the host's drivers and WSLg's or other distributions' disks.
const WSL_MOUNTS: &[&str] = &["/mnt/wsl", "/mnt/wslg", "/usr/lib/wsl"];
/// `/proc/diskstats` counts 512-byte sectors whatever the disk's own.
const SECTOR_BYTES: u64 = 512;

/// What a line of the stream completed.
#[derive(Debug)]
pub enum Event {
    /// The header, first: what the distribution is.
    Described(HostInfo),
    /// A whole sample. The totals' sequence is the hub's to number.
    Sampled {
        totals: Box<TotalsSample>,
        processes: Collected,
    },
}

/// The stream of one run of the script, line by line.
#[derive(Default)]
pub struct Stream {
    header: Option<Header>,
    sample: Option<Pending>,
    last: Option<Counters>,
    /// The volume at `/` and its device, as the last list of volumes had it.
    root: Option<(String, Volume)>,
    volumes: Vec<Volume>,
    /// The details of the last sample's processes.
    details: HashMap<(u32, u64), Arc<ProcessDetails>>,
}

#[derive(Clone, Copy)]
struct Header {
    ticks_per_second: u64,
    page_size: u64,
    /// The distribution's default user, whom the script runs as.
    uid: u32,
    boot_time: u64,
    logical_cpus: u32,
}

/// A sample being read.
#[derive(Default)]
struct Pending {
    uptime: f64,
    cpu: Option<CpuTicks>,
    cores: Vec<CpuTicks>,
    /// `/proc/meminfo` values in KiB.
    memory: HashMap<String, u64>,
    /// Sectors read and written, by device name.
    disks: HashMap<String, (u64, u64)>,
    /// Interfaces with a hardware address: bytes received and sent.
    interfaces: Vec<(String, u64, u64)>,
    /// Every mounted block device as `(device, volume)`, in the samples that list them.
    volumes: Option<Vec<(String, Volume)>>,
    /// Processes first seen, by PID and start in ticks.
    new: HashMap<(u32, u64), NewProcess>,
    rows: Vec<Row>,
}

/// What the previous sample counted, for rates.
struct Counters {
    uptime: f64,
    cpu: Option<CpuTicks>,
    cores: Vec<CpuTicks>,
    disk: Option<(u64, u64)>,
    interfaces: HashMap<String, (u64, u64)>,
    /// CPU ticks of each process, by `(pid, start_time)`.
    processes: HashMap<(u32, u64), u64>,
}

struct NewProcess {
    session: u32,
    uid: Option<u32>,
    user: String,
    name: String,
    executable: String,
    command: String,
}

struct Row {
    pid: u32,
    parent: u32,
    start_ticks: u64,
    cpu_ticks: u64,
    rss_pages: u64,
    threads: u32,
}

impl Stream {
    /// Reads one line, without its line break. `shells` gives the surfaces' shells in the
    /// distribution by PID, asked for only when a sample has processes new to it.
    pub fn line(
        &mut self,
        line: &str,
        shells: impl FnOnce() -> HashMap<u32, SurfaceId>,
    ) -> Option<Event> {
        let mut fields = line.split('\t');
        let tag = fields.next()?;
        if tag == "H" {
            let (header, host) = parse_header(fields)?;
            self.header = Some(header);
            return Some(Event::Described(host));
        }
        self.header.as_ref()?;
        if tag == "S" {
            self.sample = Some(Pending {
                uptime: fields.next()?.parse().ok()?,
                ..Pending::default()
            });
            return None;
        }
        if tag == "E" {
            let sample = self.sample.take()?;
            return Some(self.finish(sample, shells));
        }
        let sample = self.sample.as_mut()?;
        let mut number = || fields.next()?.parse::<u64>().ok();
        match tag {
            "c" => {
                let (label, ticks) = procfs::cpu_line(line.get(2..)?)?;
                if label == "cpu" {
                    sample.cpu = Some(ticks);
                } else {
                    sample.cores.push(ticks);
                }
            }
            "m" => {
                let name = fields.next()?.to_owned();
                sample.memory.insert(name, fields.next()?.parse().ok()?);
            }
            "k" => {
                let device = fields.next()?.to_owned();
                let (read, written) = (fields.next()?.parse().ok()?, fields.next()?.parse().ok()?);
                sample.disks.insert(device, (read, written));
            }
            "n" => {
                let name = fields.next()?.to_owned();
                let address = fields.next()?;
                let (received, sent) = (fields.next()?.parse().ok()?, fields.next()?.parse().ok()?);
                // Loopback and tunnels, which carry the same traffic again, have none.
                if address.chars().any(|c| c.is_ascii_hexdigit() && c != '0') {
                    sample.interfaces.push((name, received, sent));
                }
            }
            "F" => {
                let device = fields.next()?.to_owned();
                let mount_point = fields.next()?.to_owned();
                let file_system = fields.next()?.to_owned();
                let (total, available) = (
                    fields.next()?.parse::<u64>().ok()?,
                    fields.next()?.parse::<u64>().ok()?,
                );
                let volume = Volume {
                    name: volume_name(None, &mount_point),
                    mount_point,
                    file_system: Some(file_system).filter(|name| !name.is_empty()),
                    total_bytes: total * 1024,
                    available_bytes: available * 1024,
                    removable: false,
                };
                sample
                    .volumes
                    .get_or_insert_with(Vec::new)
                    .push((device, volume));
            }
            "d" => {
                let pid = number()? as u32;
                let start_ticks = number()?;
                let session = number()? as u32;
                let uid = fields.next()?.parse().ok();
                let mut text = || fields.next().map(str::to_owned);
                let process = NewProcess {
                    session,
                    uid,
                    user: text()?,
                    name: text()?,
                    executable: text()?,
                    command: text()?,
                };
                sample.new.insert((pid, start_ticks), process);
            }
            "p" => {
                let pid = number()? as u32;
                let parent = number()? as u32;
                fields.next()?;
                let mut number = || fields.next()?.parse::<u64>().ok();
                let start_ticks = number()?;
                let cpu_ticks = number()? + number()?;
                let rss_pages = number()?;
                let threads = number()? as u32;
                sample.rows.push(Row {
                    pid,
                    parent,
                    start_ticks,
                    cpu_ticks,
                    rss_pages,
                    threads,
                });
            }
            _ => {}
        }
        None
    }

    fn finish(
        &mut self,
        sample: Pending,
        shells: impl FnOnce() -> HashMap<u32, SurfaceId>,
    ) -> Event {
        let header = self.header.expect("a sample follows the header");
        let sampled_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as u64);
        let last = self.last.take();
        let elapsed = last
            .as_ref()
            .map(|last| sample.uptime - last.uptime)
            .filter(|seconds| *seconds > 0.0)
            .map(Duration::from_secs_f64);
        let last = last.filter(|_| elapsed.is_some());

        let cpu = last
            .as_ref()
            .and_then(|last| cpu_since(last.cpu?, sample.cpu?));
        let cores = match &last {
            Some(last) if last.cores.len() == sample.cores.len() => last
                .cores
                .iter()
                .zip(&sample.cores)
                .map(|(before, after)| core_share(*before, *after))
                .collect(),
            _ => Vec::new(),
        };

        if let Some(volumes) = sample.volumes {
            self.root = volumes
                .iter()
                .find(|(_, volume)| volume.mount_point == "/")
                .cloned();
            self.volumes = user_volumes(volumes.into_iter(), "/", wsl_user_volume, true);
        }
        let disk_counters = self.root.as_ref().and_then(|(device, _)| {
            let name = device.rsplit('/').next()?;
            sample.disks.get(name).copied()
        });
        let rate = |bytes: u64| elapsed.map(|elapsed| per_second(bytes, elapsed));
        let disk = self.root.as_ref().map(|(device, volume)| {
            let since = last
                .as_ref()
                .and_then(|last| last.disk)
                .zip(disk_counters)
                .map(|((read, written), (now_read, now_written))| {
                    (
                        now_read.wrapping_sub(read) * SECTOR_BYTES,
                        now_written.wrapping_sub(written) * SECTOR_BYTES,
                    )
                });
            DiskTotals {
                name: device.clone(),
                total_bytes: volume.total_bytes,
                available_bytes: volume.available_bytes,
                read_bytes_per_second: since.and_then(|(read, _)| rate(read)),
                written_bytes_per_second: since.and_then(|(_, written)| rate(written)),
                written_bytes: disk_counters.map(|(_, written)| written * SECTOR_BYTES),
            }
        });

        let mut interfaces: Vec<NetworkInterface> = sample
            .interfaces
            .iter()
            .map(|(name, received, sent)| {
                let since = last
                    .as_ref()
                    .and_then(|last| last.interfaces.get(name))
                    .map(|(before_received, before_sent)| {
                        (
                            received.wrapping_sub(*before_received),
                            sent.wrapping_sub(*before_sent),
                        )
                    });
                NetworkInterface {
                    name: name.clone(),
                    received_bytes_per_second: since.and_then(|(received, _)| rate(received)),
                    sent_bytes_per_second: since.and_then(|(_, sent)| rate(sent)),
                    received_bytes: *received,
                    sent_bytes: *sent,
                }
            })
            .collect();
        interfaces.sort_by(|a, b| a.name.cmp(&b.name));

        let mut previous = std::mem::take(&mut self.details);
        let mut new = sample.new;
        let mut shells = Some(shells);
        let mut surfaces: Option<HashMap<u32, SurfaceId>> = None;
        let mut process_ticks = HashMap::with_capacity(sample.rows.len());
        let mut processes = Vec::with_capacity(sample.rows.len());
        for row in &sample.rows {
            let start_time =
                procfs::start_seconds(header.boot_time, row.start_ticks, header.ticks_per_second);
            let key = (row.pid, start_time);
            let details = match previous.remove(&key) {
                Some(details) => details,
                None => {
                    // A process whose details the script could not read is left out.
                    let Some(process) = new.remove(&(row.pid, row.start_ticks)) else {
                        continue;
                    };
                    let surfaces = surfaces
                        .get_or_insert_with(|| shells.take().map_or_else(HashMap::new, |f| f()));
                    Arc::new(process_details(process, header.uid, surfaces))
                }
            };
            let cpu_percent = last
                .as_ref()
                .and_then(|last| last.processes.get(&key))
                .zip(elapsed)
                .map(|(before, elapsed)| {
                    let seconds = row.cpu_ticks.saturating_sub(*before) as f64
                        / header.ticks_per_second as f64;
                    (seconds * 100.0 / elapsed.as_secs_f64()) as f32
                });
            process_ticks.insert(key, row.cpu_ticks);
            self.details.insert(key, details.clone());
            processes.push(Process {
                sample: ProcessSample {
                    pid: row.pid,
                    start_time,
                    parent_pid: Some(row.parent).filter(|&parent| parent > 0),
                    details: None,
                    cpu_percent,
                    memory_bytes: Some(row.rss_pages * header.page_size),
                    threads: Some(row.threads),
                    cpu_time_ms: Some(row.cpu_ticks * 1000 / header.ticks_per_second),
                },
                details,
            });
        }

        let totals = TotalsSample {
            sequence: 0,
            sampled_at_ms,
            cpu,
            memory: memory_totals(&sample.memory),
            disk,
            network: network_totals(&interfaces),
            gpu: None,
            detail: Some(Box::new(TotalsDetail {
                cores,
                volumes: self.volumes.clone(),
                interfaces: interfaces.clone(),
            })),
        };
        self.last = Some(Counters {
            uptime: sample.uptime,
            cpu: sample.cpu,
            cores: sample.cores,
            disk: disk_counters,
            interfaces: interfaces
                .iter()
                .map(|interface| {
                    (
                        interface.name.clone(),
                        (interface.received_bytes, interface.sent_bytes),
                    )
                })
                .collect(),
            processes: process_ticks,
        });
        Event::Sampled {
            totals: Box::new(totals),
            processes: Collected {
                sampled_at_ms,
                logical_cpus: header.logical_cpus,
                apps_known: false,
                processes,
            },
        }
    }
}

/// `CLK_TCK PAGE_SIZE UID BOOT_TIME LOGICAL_CPUS HOST_NAME OS CPU_MODEL`.
fn parse_header<'a>(mut fields: impl Iterator<Item = &'a str>) -> Option<(Header, HostInfo)> {
    let mut number = || fields.next()?.parse::<u64>().ok();
    let header = Header {
        ticks_per_second: number().filter(|&ticks| ticks > 0)?,
        page_size: number()?,
        uid: number()? as u32,
        boot_time: number()?,
        logical_cpus: number()?.max(1) as u32,
    };
    let mut text = || {
        fields
            .next()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let host = HostInfo {
        host_name: text(),
        os: text(),
        cpu: text(),
        logical_cpus: header.logical_cpus,
        booted_at_ms: Some(header.boot_time * 1000).filter(|&booted| booted > 0),
    };
    Some((header, host))
}

fn cpu_since(before: CpuTicks, after: CpuTicks) -> Option<CpuTotals> {
    cpu_share(
        after.user.wrapping_sub(before.user),
        after.system.wrapping_sub(before.system),
        after.idle.wrapping_sub(before.idle),
    )
}

/// Busy share of one logical CPU between two readings, 0 to 100.
fn core_share(before: CpuTicks, after: CpuTicks) -> f32 {
    let busy = (after.user + after.system).wrapping_sub(before.user + before.system);
    let total = busy + after.idle.wrapping_sub(before.idle);
    if total == 0 {
        0.0
    } else {
        (busy as f64 * 100.0 / total as f64) as f32
    }
}

/// From `/proc/meminfo` in KiB: cache is `Cached`, `Buffers` and `SReclaimable`.
fn memory_totals(memory: &HashMap<String, u64>) -> Option<MemoryTotals> {
    let bytes = |name: &str| memory.get(name).map(|kib| kib * 1024);
    let total = bytes("MemTotal").filter(|&total| total > 0)?;
    let cache = ["Cached", "Buffers", "SReclaimable"]
        .into_iter()
        .filter_map(bytes)
        .sum();
    let kinds = used_cached_free(total, bytes("MemAvailable")?, cache);
    let available: u64 = kinds
        .iter()
        .filter(|kind| matches!(kind.kind, MemoryKindName::Cached | MemoryKindName::Free))
        .map(|kind| kind.bytes)
        .sum();
    let swap_total = bytes("SwapTotal");
    Some(MemoryTotals {
        total_bytes: total,
        used_bytes: total - available,
        available_bytes: Some(available),
        kinds,
        swap_used_bytes: swap_total
            .zip(bytes("SwapFree"))
            .map(|(total, free)| total.saturating_sub(free)),
        swap_total_bytes: swap_total,
        pressure: None,
    })
}

/// Linux's rule, less what WSL mounts itself.
fn wsl_user_volume(volume: &Volume) -> bool {
    linux_user_volume(volume) && !under_any(&volume.mount_point, WSL_MOUNTS)
}

fn process_details(
    process: NewProcess,
    own_uid: u32,
    shells: &HashMap<u32, SurfaceId>,
) -> ProcessDetails {
    let mut command = process.command.trim_end().to_owned();
    cut_text(&mut command, MAX_COMMAND_BYTES);
    let executable = Some(process.executable).filter(|path| !path.is_empty());
    ProcessDetails {
        name: process.name,
        command: Some(command).filter(|command| !command.is_empty()),
        app_group: executable.as_ref().and_then(|path| {
            let name = path.rsplit('/').next().filter(|name| !name.is_empty())?;
            Some(AppGroup {
                id: path.clone(),
                name: name.to_owned(),
            })
        }),
        executable,
        user: Some(process.user).filter(|user| !user.is_empty()),
        owner: match process.uid {
            Some(uid) if uid == own_uid => ProcessOwner::CurrentUser,
            Some(uid) if uid < FIRST_PERSON_UID || uid == NOBODY => ProcessOwner::System,
            _ => ProcessOwner::Other,
        },
        app: false,
        restricted: false,
        surface: (process.session > 0)
            .then(|| shells.get(&process.session).cloned())
            .flatten(),
    }
}

/// The answer of `inspect`: NUL-terminated `gone`, or `ok`, the state letter, the working
/// directory and each argument.
pub fn parse_inspection(output: &[u8]) -> Result<Option<ProcessInspection>, String> {
    let mut fields = output.split(|&byte| byte == 0);
    match fields.next() {
        Some(b"gone") => return Ok(None),
        Some(b"ok") => {}
        _ => return Err(unexpected(output)),
    }
    let mut text = || {
        fields
            .next()
            .map(|field| String::from_utf8_lossy(field).into_owned())
    };
    let state = text().ok_or_else(|| unexpected(output))?;
    let working_directory = text().filter(|directory| !directory.is_empty());
    let mut arguments: Vec<String> = std::iter::from_fn(text).collect();
    // The command line ends with a NUL, which leaves an empty last field.
    if arguments.last().is_some_and(String::is_empty) {
        arguments.pop();
    }
    let (arguments, arguments_cut) = cut_arguments(arguments);
    Ok(Some(ProcessInspection {
        state: Some(procfs::process_state(&state)),
        working_directory,
        arguments,
        arguments_cut,
    }))
}

/// The answer of `quit`: `sent`, `gone` or `denied`.
pub fn parse_quit(output: &[u8]) -> Result<QuitOutcome, String> {
    match String::from_utf8_lossy(output).trim() {
        "sent" => Ok(QuitOutcome::Sent),
        "gone" => Ok(QuitOutcome::Gone),
        "denied" => Ok(QuitOutcome::Denied),
        _ => Err(unexpected(output)),
    }
}

fn unexpected(output: &[u8]) -> String {
    let message = decode(output);
    let message = message.trim();
    if message.is_empty() {
        "WSL gave Monitor no answer".into()
    } else {
        format!(
            "WSL answered Monitor with: {}",
            message.chars().take(300).collect::<String>()
        )
    }
}

/// `wsl.exe` reports its own failures in UTF-16; Linux programs write UTF-8.
pub fn decode(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes.len().is_multiple_of(2) && bytes.chunks(2).any(|pair| pair[1] == 0)
    {
        let words: Vec<u16> = bytes
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16_lossy(&words).replace('\0', "");
    }
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::system_metrics::{MAX_INSPECTED_ARGUMENT_BYTES, ProcessState};

    /// Two samples as the script writes them, two seconds apart.
    const STREAM: &str = "\
H\t100\t4096\t1000\t1760000000\t2\tdesk\tUbuntu 24.04.1 LTS\tAMD Ryzen 9 7950X 16-Core Processor
S\t100.00
c\tcpu  100 5 50 800 20 3 2 1 0 0
c\tcpu0 60 5 30 400 10 3 2 1 0 0
c\tcpu1 40 0 20 400 10 0 0 0 0 0
m\tMemTotal\t16000000
m\tMemAvailable\t12000000
m\tBuffers\t100000
m\tCached\t2000000
m\tSReclaimable\t200000
m\tSwapTotal\t4194304
m\tSwapFree\t4000000
k\tsda\t1000\t2000
k\tsdc\t400000\t800000
n\teth0\t00:15:5d:01:02:03\t5000000\t300000
n\ttunl0\t00:00:00:00\t70\t70
F\t/dev/sdc\t/\text4\t1000000\t600000
F\t/dev/sdc\t/mnt/wslg/distro\text4\t1000000\t600000
F\t/dev/sdd\t/mnt/wsl/docker-desktop-data/isocache\text4\t5000\t1000
F\t/dev/sde\t/home/me/data\text4\t2000\t1000
F\t/dev/sda1\t/boot\text4\t300\t200
d\t1\t5\t0\t0\troot\tinit\t\t
p\t1\t0\tS\t5\t10\t20\t300\t2
d\t4242\t123456\t4200\t1000\tme\tpython3\t/usr/bin/python3.12\tpython3 -m http.server 
p\t4242\t4200\tR\t123456\t150\t50\t2560\t3
d\t4300\t130000\t4300\t1001\tother\tbash\t\t-bash 
p\t4300\t1\tS\t130000\t5\t5\t1000\t1
p\t4400\t1\tS\t140000\t1\t1\t10\t1
E
S\t102.00
c\tcpu  200 5 100 900 20 3 2 1 0 0
c\tcpu0 135 5 55 400 10 3 2 1 0 0
c\tcpu1 65 0 45 500 10 0 0 0 0 0
m\tMemTotal\t16000000
m\tMemAvailable\t12000000
k\tsdc\t404000\t808000
n\teth0\t00:15:5d:01:02:03\t5002000\t301000
p\t1\t0\tS\t5\t10\t20\t300\t2
p\t4242\t4200\tR\t123456\t250\t50\t2560\t3
E
";

    fn read(stream: &mut Stream, text: &str) -> Vec<Event> {
        let shells = || HashMap::from([(4200, SurfaceId::new("surface-1"))]);
        text.lines()
            .filter_map(|line| stream.line(line, shells))
            .collect()
    }

    #[test]
    fn the_header_describes_the_distribution() {
        let events = read(&mut Stream::default(), STREAM.lines().next().unwrap());
        let [Event::Described(host)] = events.as_slice() else {
            panic!("{events:?}");
        };
        assert_eq!(
            host,
            &HostInfo {
                host_name: Some("desk".into()),
                os: Some("Ubuntu 24.04.1 LTS".into()),
                cpu: Some("AMD Ryzen 9 7950X 16-Core Processor".into()),
                logical_cpus: 2,
                booted_at_ms: Some(1_760_000_000_000),
            }
        );
    }

    #[test]
    fn nothing_is_read_before_the_header() {
        let mut stream = Stream::default();
        let body = STREAM.split_once('\n').unwrap().1;
        assert!(read(&mut stream, body).is_empty());
    }

    #[test]
    fn samples_carry_totals_and_processes_with_rates_from_the_second() {
        let events = read(&mut Stream::default(), STREAM);
        let [
            Event::Described(_),
            Event::Sampled {
                totals: first,
                processes: first_processes,
            },
            Event::Sampled {
                totals: second,
                processes: second_processes,
            },
        ] = events.as_slice()
        else {
            panic!("{events:?}");
        };

        // The first sample has no rates.
        assert_eq!(first.cpu, None);
        assert_eq!(first.gpu, None);
        let detail = first.detail.as_ref().unwrap();
        assert!(detail.cores.is_empty());
        let mounts: Vec<&str> = detail
            .volumes
            .iter()
            .map(|volume| volume.mount_point.as_str())
            .collect();
        assert_eq!(
            mounts,
            ["/", "/home/me/data"],
            "the system disk first, once"
        );
        assert_eq!(
            detail.interfaces.len(),
            1,
            "a tunnel has no hardware address"
        );
        let memory = first.memory.as_ref().unwrap();
        assert_eq!(memory.total_bytes, 16_000_000 * 1024);
        assert_eq!(memory.available_bytes, Some(12_000_000 * 1024));
        assert_eq!(memory.used_bytes, 4_000_000 * 1024);
        assert_eq!(memory.swap_used_bytes, Some(194_304 * 1024));
        let disk = first.disk.as_ref().unwrap();
        assert_eq!(disk.name, "/dev/sdc");
        assert_eq!(
            (disk.total_bytes, disk.available_bytes),
            (1_000_000 * 1024, 600_000 * 1024)
        );
        assert_eq!(disk.read_bytes_per_second, None);
        assert_eq!(disk.written_bytes, Some(800_000 * 512));

        let pids: Vec<u32> = first_processes
            .processes
            .iter()
            .map(|process| process.sample.pid)
            .collect();
        assert_eq!(pids, [1, 4242, 4300], "one without details is left out");
        assert_eq!(first_processes.logical_cpus, 2);
        assert!(!first_processes.apps_known);
        let init = &first_processes.processes[0];
        assert_eq!(init.sample.parent_pid, None);
        assert_eq!(init.details.owner, ProcessOwner::System);
        assert_eq!(
            (
                init.details.command.as_deref(),
                init.details.executable.as_deref()
            ),
            (None, None)
        );
        let python = &first_processes.processes[1];
        assert_eq!(python.sample.start_time, 1_760_001_234);
        assert_eq!(python.sample.parent_pid, Some(4200));
        assert_eq!(python.sample.cpu_percent, None);
        assert_eq!(python.sample.memory_bytes, Some(2560 * 4096));
        assert_eq!(python.sample.threads, Some(3));
        assert_eq!(python.sample.cpu_time_ms, Some(2000));
        let details = &python.details;
        assert_eq!(details.name, "python3");
        assert_eq!(details.command.as_deref(), Some("python3 -m http.server"));
        assert_eq!(details.executable.as_deref(), Some("/usr/bin/python3.12"));
        assert_eq!(details.user.as_deref(), Some("me"));
        assert_eq!(details.owner, ProcessOwner::CurrentUser);
        assert_eq!(
            details.app_group.as_ref().map(|group| group.name.as_str()),
            Some("python3.12")
        );
        assert_eq!(details.surface, Some(SurfaceId::new("surface-1")));
        let bash = &first_processes.processes[2];
        assert_eq!(bash.details.owner, ProcessOwner::Other);
        assert_eq!(bash.details.surface, None, "it leads a session of its own");

        // The second measures over the two seconds between them.
        let cpu = second.cpu.unwrap();
        assert!((cpu.user - 40.0).abs() < 0.01 && (cpu.system - 20.0).abs() < 0.01);
        let cores = &second.detail.as_ref().unwrap().cores;
        assert!(cores.len() == 2 && cores[0] == 100.0 && (cores[1] - 100.0 / 3.0).abs() < 0.01);
        assert_eq!(
            second.detail.as_ref().unwrap().volumes.len(),
            2,
            "volumes are kept between their lists"
        );
        let disk = second.disk.as_ref().unwrap();
        assert_eq!(disk.read_bytes_per_second, Some(4000 * 512 / 2));
        assert_eq!(disk.written_bytes_per_second, Some(8000 * 512 / 2));
        let network = second.network.unwrap();
        assert_eq!(
            (
                network.received_bytes_per_second,
                network.sent_bytes_per_second
            ),
            (Some(1000), Some(500))
        );
        assert_eq!(second.memory.as_ref().unwrap().swap_total_bytes, None);
        let pids: Vec<u32> = second_processes
            .processes
            .iter()
            .map(|process| process.sample.pid)
            .collect();
        assert_eq!(pids, [1, 4242]);
        let python = &second_processes.processes[1];
        assert_eq!(python.sample.cpu_percent, Some(50.0));
        assert!(Arc::ptr_eq(
            &python.details,
            &first_processes.processes[1].details
        ));
    }

    #[test]
    fn inspections_answer_state_directory_and_arguments() {
        assert_eq!(parse_inspection(b"gone\0"), Ok(None));
        let inspection = parse_inspection(b"ok\0S\0/home/me\0python3\0-m\0http.server\0")
            .unwrap()
            .unwrap();
        assert_eq!(inspection.state, Some(ProcessState::Sleeping));
        assert_eq!(inspection.working_directory.as_deref(), Some("/home/me"));
        assert_eq!(
            inspection.arguments,
            Some(vec!["python3".into(), "-m".into(), "http.server".into()])
        );
        let kernel = parse_inspection(b"ok\0I\0\0").unwrap().unwrap();
        assert_eq!(kernel.state, Some(ProcessState::Idle));
        assert_eq!((kernel.working_directory, kernel.arguments), (None, None));
        let mut long = b"ok\0R\0/\0".to_vec();
        long.extend(vec![b'x'; MAX_INSPECTED_ARGUMENT_BYTES + 1]);
        assert!(parse_inspection(&long).unwrap().unwrap().arguments_cut);
        assert!(parse_inspection(b"").is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inspection_uses_the_sampled_epoch_without_relaxing_lifetime_checks() {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
        let ticks: u64 = procfs::stat_fields(&stat).unwrap()[19].parse().unwrap();
        let clock = Command::new("getconf").arg("CLK_TCK").output().unwrap();
        assert!(clock.status.success());
        let hz = String::from_utf8(clock.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // Deliberately unrelated to today's /proc/stat btime, as after WSL clock sync.
        let sampled_epoch = 1_760_000_000;
        let started = procfs::start_seconds(sampled_epoch, ticks, hz);
        let inspect = |start_time: u64| {
            let mut child = Command::new("/bin/sh")
                .args([
                    "-s",
                    "inspect",
                    &std::process::id().to_string(),
                    &start_time.to_string(),
                    &sampled_epoch.to_string(),
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(SCRIPT.as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success());
            parse_inspection(&output.stdout).unwrap()
        };
        assert!(inspect(started).is_some());
        assert!(inspect(started + 1).is_none());
    }

    #[test]
    fn quit_answers_and_wsl_failures_are_read() {
        assert_eq!(parse_quit(b"sent\n"), Ok(QuitOutcome::Sent));
        assert_eq!(parse_quit(b"gone\n"), Ok(QuitOutcome::Gone));
        assert_eq!(parse_quit(b"denied\n"), Ok(QuitOutcome::Denied));
        let utf16: Vec<u8> = "No distribution\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(
            parse_quit(&utf16),
            Err("WSL answered Monitor with: No distribution".into())
        );
        assert_eq!(parse_quit(b""), Err("WSL gave Monitor no answer".into()));
    }

    #[cfg(unix)]
    #[test]
    fn the_script_is_posix_sh() {
        let status = std::process::Command::new("sh")
            .args(["-n", "-c", SCRIPT])
            .status()
            .expect("run sh");
        assert!(status.success(), "sh -n rejects the collector script");
        for bashism in ["[[", "local ", "$'", "<<<", "${!", "pipefail"] {
            assert!(!SCRIPT.contains(bashism), "the script uses {bashism}");
        }
    }
}
