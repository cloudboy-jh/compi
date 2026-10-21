//! `compi monitor totals` and `compi monitor list`: Monitor's data for scripts, read
//! straight from the daemon on the command's own connection, with no window. The
//! daemon stops sampling processes once the connection closes with the command.
use super::{CliError, Options, Output};
use crate::DaemonClient;
use crate::monitor::apps::{self, Group, GroupUser, RowKind, View};
use crate::monitor::totals::{self, Span};
use crate::monitor::{
    Filter, Search, Sort, cpu_share, display_line, format_bytes, format_count, format_cpu,
    format_percent, format_rate, format_used_of,
};
use compi_protocol::system_metrics::{HostInfo, Process, ProcessTable, SystemSample, TotalsSample};
use compi_protocol::{ServerEvent, ServerMessage};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// How long a command waits for the daemon's first useful sample.
const WAIT: Duration = Duration::from_secs(5);
/// Process names are cut to this many characters in the table.
const NAME_WIDTH: usize = 40;

/// `--history RANGE`, as Monitor's span buttons name it.
pub(super) fn span(range: &str) -> Result<Span, CliError> {
    Span::ALL
        .into_iter()
        .find(|span| span.label() == range)
        .ok_or_else(|| {
            CliError::usage(format!(
                "--history must be one of {}",
                Span::ALL.map(Span::label).join(", ")
            ))
        })
}

pub(super) fn totals(client: &mut DaemonClient, options: &Options) -> Result<Output, CliError> {
    let span = options
        .values
        .get("--history")
        .map(|range| span(range))
        .transpose()?;
    let (host, mut history) = client.watch_system_totals()?;
    // A daemon that has just started may not have sampled yet.
    if history.is_empty() {
        let sample = wait(client, |message| match message {
            ServerMessage::SystemTotalsSampled { sample } => Some(sample),
            _ => None,
        })?
        .ok_or_else(|| no_sample("totals"))?;
        history.push(sample);
    }
    let samples = recent(history, span);
    Ok(Output::Report {
        text: totals_text(&host, &samples, span),
        value: json!({"host": host, "samples": samples}),
    })
}

/// The newest sample, or with `span` every sample of that span ending at the newest,
/// oldest first. Only the newest carries the breakdowns (`detail`).
fn recent(history: Vec<TotalsSample>, span: Option<Span>) -> Vec<TotalsSample> {
    let Some(latest) = history.last().map(|sample| sample.sequence) else {
        return history;
    };
    let count = span.map_or(1, Span::samples) as u64;
    let first = latest.saturating_sub(count - 1);
    history
        .into_iter()
        .filter(|sample| sample.sequence >= first)
        .collect()
}

/// The newest sample in a short block; with a span, how many samples it covers.
fn totals_text(host: &HostInfo, samples: &[TotalsSample], span: Option<Span>) -> String {
    const UNAVAILABLE: &str = "unavailable";
    let sample = samples.last().expect("totals always have a sample");
    let mut lines = Vec::new();
    let host_parts: Vec<String> = [&host.host_name, &host.os, &host.cpu]
        .into_iter()
        .flatten()
        .map(|part| display_line(part))
        .chain((host.logical_cpus > 0).then(|| format!("{} logical CPUs", host.logical_cpus)))
        .collect();
    if !host_parts.is_empty() {
        lines.push(("Host", host_parts.join(" · ")));
    }
    lines.push((
        "CPU",
        match (totals::cpu_used(sample), sample.cpu) {
            (Some(used), Some(cpu)) => format!(
                "{} (user {}, system {})",
                format_percent(used as f32),
                format_percent(cpu.user),
                format_percent(cpu.system)
            ),
            _ => UNAVAILABLE.into(),
        },
    ));
    let memory = sample.memory.as_ref();
    lines.push((
        "Memory",
        memory.map_or(UNAVAILABLE.into(), |memory| {
            let mut text = format!(
                "{} used",
                format_used_of(Some(memory.used_bytes), Some(memory.total_bytes))
                    .unwrap_or_default()
            );
            if let Some(available) = memory.available_bytes {
                text.push_str(&format!(" · {} available", format_bytes(available)));
            }
            text
        }),
    ));
    lines.push((
        "Swap",
        memory
            .and_then(|memory| format_used_of(memory.swap_used_bytes, memory.swap_total_bytes))
            .map_or(UNAVAILABLE.into(), |swap| format!("{swap} used")),
    ));
    lines.push((
        "GPU",
        sample.gpu.as_ref().map_or(UNAVAILABLE.into(), |gpu| {
            [
                gpu.utilization.map(format_percent),
                format_used_of(gpu.memory_used_bytes, gpu.memory_total_bytes)
                    .map(|memory| format!("{memory} memory")),
                gpu.name.as_deref().map(display_line),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
        }),
    ));
    let rate = |rate: Option<u64>| rate.map_or(UNAVAILABLE.into(), format_rate);
    lines.push((
        "Disk",
        sample.disk.as_ref().map_or(UNAVAILABLE.into(), |disk| {
            format!(
                "read {} · write {}",
                rate(disk.read_bytes_per_second),
                rate(disk.written_bytes_per_second)
            )
        }),
    ));
    lines.push((
        "Network",
        sample.network.map_or(UNAVAILABLE.into(), |network| {
            format!(
                "down {} · up {}",
                rate(network.received_bytes_per_second),
                rate(network.sent_bytes_per_second)
            )
        }),
    ));
    if let Some(span) = span {
        let noun = if samples.len() == 1 {
            "sample"
        } else {
            "samples"
        };
        lines.push((
            "History",
            format!("{} {noun} over {}", samples.len(), span.label()),
        ));
    }
    lines
        .into_iter()
        .map(|(label, value)| format!("{label:<9}{value}\n"))
        .collect()
}

/// One sample of the host's processes, with every process's details.
struct Listing {
    sampled_at_ms: u64,
    logical_cpus: u32,
    apps_known: bool,
    processes: Vec<Process>,
}

pub(super) fn list(client: &mut DaemonClient, options: &Options) -> Result<Output, CliError> {
    client.watch_system_metrics()?;
    // The first sample has no CPU values yet; the next follows shortly. Details come
    // once per process, so every sample goes through the table.
    let mut table = ProcessTable::default();
    let listing = wait(client, |message| {
        let ServerMessage::SystemSampled { sample } = message else {
            return None;
        };
        let SystemSample {
            sampled_at_ms,
            logical_cpus,
            apps_known,
            processes,
        } = sample;
        let processes = table.apply(processes);
        processes
            .iter()
            .any(|process| process.sample.cpu_percent.is_some())
            .then_some(Listing {
                sampled_at_ms,
                logical_cpus,
                apps_known,
                processes,
            })
    })?
    .ok_or_else(|| no_sample("process"))?;
    let search = Search::new(options.values.get("--search").map_or("", String::as_str));
    let (text, value) = report(&listing, &search, options.switches.contains("--group"));
    Ok(Output::Report { text, value })
}

/// The processes `search` keeps, busiest first, as Monitor's Processes table sorts them;
/// `grouped`, one row per app with its processes' sums, as the table's grouping does.
fn report(listing: &Listing, search: &Search, grouped: bool) -> (String, Value) {
    let expanded = HashSet::new();
    let arranged = apps::arrange_rows(
        &listing.processes,
        View {
            filter: Filter::All,
            search,
            sort: Sort::default(),
            grouped,
            expanded: &expanded,
        },
        None,
    );
    let cpus = listing.logical_cpus;
    let unavailable = || "-".to_owned();
    let cpu = |percent: Option<f32>| cpu_share(percent, cpus).map_or_else(unavailable, format_cpu);
    let memory = |bytes: Option<u64>| bytes.map_or_else(unavailable, format_bytes);
    let mut processes = Vec::new();
    let mut groups = Vec::new();
    let mut rows = Vec::new();
    for row in &arranged.rows {
        match row.kind {
            RowKind::Process(index) => {
                let process = &listing.processes[index];
                processes.push(process_value(process, cpus));
                rows.push([
                    process.sample.pid.to_string(),
                    display_line(&process.details.name),
                    cpu(process.sample.cpu_percent),
                    memory(process.sample.memory_bytes),
                    process
                        .details
                        .user
                        .as_deref()
                        .map_or_else(unavailable, display_line),
                ]);
            }
            RowKind::Group(index) => {
                let group = &arranged.groups[index];
                groups.push(group_value(group, &listing.processes, cpus));
                processes.extend(
                    group
                        .members
                        .iter()
                        .map(|&member| process_value(&listing.processes[member], cpus)),
                );
                rows.push([
                    String::new(),
                    format!(
                        "{} ({} processes)",
                        display_line(&group.name),
                        group.members.len()
                    ),
                    cpu(group.cpu_percent),
                    memory(group.memory_bytes),
                    match &group.user {
                        GroupUser::Shared(user) => display_line(user),
                        GroupUser::Mixed => unavailable(),
                    },
                ]);
            }
        }
    }
    let mut text = process_table(&rows);
    text.push_str(&format_count(processes.len(), listing.processes.len()));
    text.push('\n');
    let mut value = json!({
        "sampled_at_ms": listing.sampled_at_ms,
        "logical_cpus": listing.logical_cpus,
        "apps_known": listing.apps_known,
        "processes": processes,
    });
    if grouped {
        value["groups"] = Value::Array(groups);
    }
    (text, value)
}

/// A process as the protocol samples it, with its details and its CPU as a share of the
/// whole machine (`cpu_percent` is of one logical CPU).
fn process_value(process: &Process, logical_cpus: u32) -> Value {
    let mut value = serde_json::to_value(&process.sample).expect("samples serialize");
    value["details"] = json!(&*process.details);
    value["cpu_share"] = json!(cpu_share(process.sample.cpu_percent, logical_cpus));
    value
}

/// An app with several processes shown: its members' pids, in table order, and sums.
fn group_value(group: &Group, processes: &[Process], logical_cpus: u32) -> Value {
    json!({
        "id": group.id,
        "name": group.name,
        "pids": group.members.iter().map(|&member| processes[member].sample.pid).collect::<Vec<_>>(),
        "process_count": group.members.len(),
        "cpu_percent": group.cpu_percent,
        "cpu_share": cpu_share(group.cpu_percent, logical_cpus),
        "memory_bytes": group.memory_bytes,
        "threads": group.threads,
        "cpu_time_ms": group.cpu_time_ms,
        "user": match &group.user {
            GroupUser::Shared(user) => Some(user),
            GroupUser::Mixed => None,
        },
    })
}

/// PID, name (cut to [`NAME_WIDTH`]), CPU, memory and user, numbers right-aligned.
fn process_table(rows: &[[String; 5]]) -> String {
    let name = |text: &str| -> String {
        match text.char_indices().nth(NAME_WIDTH - 1) {
            Some((cut, _)) if text.chars().count() > NAME_WIDTH => format!("{}…", &text[..cut]),
            _ => text.to_owned(),
        }
    };
    let header = ["PID", "NAME", "CPU", "MEMORY", "USER"].map(str::to_owned);
    let rows: Vec<[String; 5]> = std::iter::once(header)
        .chain(rows.iter().cloned())
        .map(|[pid, process, cpu, memory, user]| [pid, name(&process), cpu, memory, user])
        .collect();
    let mut widths = [0; 5];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut text = String::new();
    for [pid, process, cpu, memory, user] in &rows {
        let line = format!(
            "{pid:>w0$}  {process:<w1$}  {cpu:>w2$}  {memory:>w3$}  {user}",
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3],
        );
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text
}

/// Reads the connection's events until `pick` takes one, or [`WAIT`] passes (`None`).
fn wait<T>(
    client: &mut DaemonClient,
    mut pick: impl FnMut(ServerMessage) -> Option<T>,
) -> Result<Option<T>, CliError> {
    let deadline = Instant::now() + WAIT;
    loop {
        match client.poll_event()? {
            Some(ServerEvent::Control { message, .. }) => {
                if let Some(found) = pick(message) {
                    return Ok(Some(found));
                }
            }
            Some(_) => {}
            None if Instant::now() >= deadline => return Ok(None),
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn no_sample(kind: &str) -> CliError {
    CliError::new(
        1,
        "no_sample",
        format!(
            "the daemon sent no {kind} sample within {} seconds",
            WAIT.as_secs()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::system_metrics::{
        AppGroup, CpuTotals, DiskTotals, MemoryTotals, NetworkTotals, ProcessDetails, ProcessOwner,
        ProcessSample, TotalsDetail,
    };
    use std::sync::Arc;

    fn sample(sequence: u64) -> TotalsSample {
        TotalsSample {
            sequence,
            sampled_at_ms: sequence * 2000,
            cpu: Some(CpuTotals {
                user: 10.0,
                system: 5.0,
            }),
            ..TotalsSample::default()
        }
    }

    #[test]
    fn history_ranges_keep_the_samples_of_their_span() {
        let history = |count: u64| -> Vec<TotalsSample> {
            let mut history: Vec<_> = (1..=count).map(sample).collect();
            history.last_mut().unwrap().detail = Some(Box::new(TotalsDetail::default()));
            history
        };
        let sequences = |samples: &[TotalsSample]| -> Vec<u64> {
            samples.iter().map(|sample| sample.sequence).collect()
        };
        // Without a range only the newest sample, with its breakdowns.
        let newest = recent(history(100), None);
        assert_eq!(sequences(&newest), [100]);
        assert!(newest[0].detail.is_some());
        // A minute is 30 samples at 2 s, ending at the newest.
        let minute = recent(history(100), Some(Span::Minute));
        assert_eq!(minute.len(), 30);
        assert_eq!(minute.first().unwrap().sequence, 71);
        assert!(minute.last().unwrap().detail.is_some());
        // A range longer than the history keeps all of it.
        assert_eq!(recent(history(100), Some(Span::Hour)).len(), 100);
        assert_eq!(recent(history(2000), Some(Span::Hour)).len(), 1800);
        assert_eq!(recent(history(1000), Some(Span::FifteenMinutes)).len(), 450);
        assert_eq!(recent(history(1000), Some(Span::FiveMinutes)).len(), 150);
        // Sequences the connection never received leave gaps, not extra samples.
        let gapped: Vec<_> = [1, 2, 50, 80, 100].into_iter().map(sample).collect();
        assert_eq!(sequences(&recent(gapped, Some(Span::Minute))), [80, 100]);
        assert!(recent(Vec::new(), Some(Span::Minute)).is_empty());
    }

    #[test]
    fn ranges_are_monitors_span_labels() {
        assert_eq!(span("1m").unwrap(), Span::Minute);
        assert_eq!(span("5m").unwrap(), Span::FiveMinutes);
        assert_eq!(span("15m").unwrap(), Span::FifteenMinutes);
        assert_eq!(span("1h").unwrap(), Span::Hour);
        let error = span("2h").unwrap_err();
        assert_eq!(error.exit, 2);
        assert_eq!(error.message, "--history must be one of 1m, 5m, 15m, 1h");
    }

    #[test]
    fn totals_text_reads_the_newest_sample() {
        const GB: u64 = 1024 * 1024 * 1024;
        let host = HostInfo {
            host_name: Some("studio".into()),
            os: Some("macOS 26.6".into()),
            cpu: None,
            logical_cpus: 8,
            booted_at_ms: None,
        };
        let mut newest = sample(2);
        newest.memory = Some(MemoryTotals {
            total_bytes: 16 * GB,
            used_bytes: 8 * GB,
            available_bytes: Some(6 * GB),
            kinds: Vec::new(),
            swap_total_bytes: Some(2 * GB),
            swap_used_bytes: Some(GB),
            pressure: None,
        });
        newest.disk = Some(DiskTotals {
            read_bytes_per_second: Some(2048),
            written_bytes_per_second: None,
            ..DiskTotals::default()
        });
        newest.network = Some(NetworkTotals {
            received_bytes_per_second: Some(512),
            sent_bytes_per_second: Some(3 * 1024 * 1024),
            received_bytes: 0,
            sent_bytes: 0,
        });
        assert_eq!(
            totals_text(&host, &[sample(1), newest], Some(Span::Minute)),
            "Host     studio · macOS 26.6 · 8 logical CPUs\n\
             CPU      15.0% (user 10.0%, system 5.0%)\n\
             Memory   8.0 GB of 16.0 GB used · 6.0 GB available\n\
             Swap     1.0 GB of 2.0 GB used\n\
             GPU      unavailable\n\
             Disk     read 2.0 KB/s · write unavailable\n\
             Network  down 512 B/s · up 3.0 MB/s\n\
             History  2 samples over 1m\n"
        );
    }

    fn process(
        pid: u32,
        name: &str,
        app: Option<&str>,
        cpu: Option<f32>,
        memory: u64,
        user: &str,
    ) -> Process {
        Process {
            sample: ProcessSample {
                pid,
                start_time: 100,
                parent_pid: Some(1),
                details: None,
                cpu_percent: cpu,
                memory_bytes: Some(memory),
                threads: Some(2),
                cpu_time_ms: Some(1000),
            },
            details: Arc::new(ProcessDetails {
                name: name.into(),
                command: Some(format!("/usr/bin/{name} --serve")),
                executable: Some(format!("/usr/bin/{name}")),
                user: Some(user.into()),
                owner: ProcessOwner::CurrentUser,
                app: app.is_some(),
                restricted: false,
                app_group: app.map(|app| AppGroup {
                    id: format!("/Applications/{app}.app"),
                    name: app.into(),
                }),
            }),
        }
    }

    fn listing() -> Listing {
        const MB: u64 = 1024 * 1024;
        Listing {
            sampled_at_ms: 1_000,
            logical_cpus: 4,
            apps_known: true,
            processes: vec![
                process(10, "Safari", Some("Safari"), Some(40.0), 300 * MB, "me"),
                process(
                    11,
                    "Safari Web Content",
                    Some("Safari"),
                    Some(20.0),
                    200 * MB,
                    "me",
                ),
                process(20, "cargo", None, Some(80.0), 100 * MB, "me"),
                process(30, "launchd", None, None, 10 * MB, "root"),
                process(40, "Notes", Some("Notes"), Some(4.0), 50 * MB, "me"),
            ],
        }
    }

    #[test]
    fn process_lists_sort_by_cpu_and_search() {
        let (text, value) = report(&listing(), &Search::default(), false);
        let pids: Vec<u64> = value["processes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|process| process["pid"].as_u64().unwrap())
            .collect();
        // Busiest first; a process without a CPU value last.
        assert_eq!(pids, [20, 10, 11, 40, 30]);
        assert!(value.get("groups").is_none());
        assert_eq!(value["logical_cpus"], 4);
        assert_eq!(value["apps_known"], true);
        let cargo = &value["processes"][0];
        assert_eq!(cargo["cpu_percent"], 80.0);
        assert_eq!(cargo["cpu_share"], 20.0);
        assert_eq!(cargo["details"]["name"], "cargo");
        assert_eq!(cargo["details"]["owner"], "current_user");
        assert_eq!(cargo["memory_bytes"], 100 * 1024 * 1024);
        assert_eq!(value["processes"][4]["cpu_share"], Value::Null);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0].split_whitespace().collect::<Vec<_>>(),
            ["PID", "NAME", "CPU", "MEMORY", "USER"]
        );
        assert_eq!(
            lines[1].split_whitespace().collect::<Vec<_>>(),
            ["20", "cargo", "20.0%", "100.0", "MB", "me"]
        );
        assert_eq!(
            lines[5].split_whitespace().collect::<Vec<_>>(),
            ["30", "launchd", "-", "10.0", "MB", "root"]
        );
        assert_eq!(lines[6], "5 processes");

        // Every word must match name, command line, executable, user or pid.
        let (text, value) = report(&listing(), &Search::new("safari web"), false);
        assert_eq!(value["processes"].as_array().unwrap().len(), 1);
        assert_eq!(value["processes"][0]["pid"], 11);
        assert_eq!(text.lines().last(), Some("1 of 5 processes"));
        let (_, value) = report(&listing(), &Search::new("root"), false);
        assert_eq!(value["processes"][0]["pid"], 30);
    }

    #[test]
    fn grouped_lists_sum_each_apps_processes() {
        let (text, value) = report(&listing(), &Search::default(), true);
        let groups = value["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "Notes has one process and stands alone");
        let safari = &groups[0];
        assert_eq!(safari["id"], "/Applications/Safari.app");
        assert_eq!(safari["name"], "Safari");
        assert_eq!(safari["pids"], json!([10, 11]));
        assert_eq!(safari["process_count"], 2);
        assert_eq!(safari["cpu_percent"], 60.0);
        assert_eq!(safari["cpu_share"], 15.0);
        assert_eq!(safari["memory_bytes"], 500 * 1024 * 1024);
        assert_eq!(safari["threads"], 4);
        assert_eq!(safari["cpu_time_ms"], 2000);
        assert_eq!(safari["user"], "me");
        // Processes in table order, an app's members where the app stands.
        let pids: Vec<u64> = value["processes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|process| process["pid"].as_u64().unwrap())
            .collect();
        assert_eq!(pids, [20, 10, 11, 40, 30]);
        let lines: Vec<Vec<&str>> = text
            .lines()
            .map(|line| line.split_whitespace().collect())
            .collect();
        assert_eq!(lines[1], ["20", "cargo", "20.0%", "100.0", "MB", "me"]);
        assert_eq!(
            lines[2],
            ["Safari", "(2", "processes)", "15.0%", "500.0", "MB", "me"]
        );
        assert_eq!(lines[3], ["40", "Notes", "1.0%", "50.0", "MB", "me"]);
        assert_eq!(lines.len(), 6);
        assert_eq!(text.lines().last(), Some("5 processes"));

        // A search naming the app keeps all its processes.
        let (_, value) = report(&listing(), &Search::new("safari"), true);
        assert_eq!(value["groups"][0]["pids"], json!([10, 11]));
        assert_eq!(value["processes"].as_array().unwrap().len(), 2);
        // One that matches a single member leaves it on its own.
        let (_, value) = report(&listing(), &Search::new("content"), true);
        assert_eq!(value["groups"], json!([]));
        assert_eq!(value["processes"][0]["pid"], 11);
    }

    #[test]
    fn long_names_are_cut() {
        let rows = [[
            "1".to_owned(),
            "x".repeat(50),
            "1.0%".to_owned(),
            "1 KB".to_owned(),
            "me".to_owned(),
        ]];
        let text = process_table(&rows);
        let row = text.lines().nth(1).unwrap();
        assert!(row.contains(&format!("{}…", "x".repeat(NAME_WIDTH - 1))));
        assert!(!row.contains(&"x".repeat(NAME_WIDTH)));
    }
}
