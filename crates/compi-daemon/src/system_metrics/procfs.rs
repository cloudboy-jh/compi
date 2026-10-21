//! Readers of Linux's `/proc` text, shared by a Linux host and the WSL collector's stream.
// A macOS daemon reads no `/proc`; the tests still run these.
#![cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]

use compi_protocol::system_metrics::ProcessState;

/// CPU ticks of a `cpu` or `cpuN` line of `/proc/stat`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTicks {
    /// User and nice.
    pub user: u64,
    /// System, irq, softirq and steal.
    pub system: u64,
    /// Idle and iowait.
    pub idle: u64,
}

/// A `cpu` or `cpuN` line: its label and ticks (user nice system idle iowait irq softirq
/// steal; later columns are counted in user already).
pub fn cpu_line(line: &str) -> Option<(&str, CpuTicks)> {
    let mut fields = line.split_whitespace();
    let label = fields.next().filter(|label| label.starts_with("cpu"))?;
    let mut values = [0u64; 8];
    for (value, field) in values.iter_mut().zip(fields) {
        *value = field.parse().ok()?;
    }
    Some((
        label,
        CpuTicks {
            user: values[0] + values[1],
            system: values[2] + values[5] + values[6] + values[7],
            idle: values[3] + values[4],
        },
    ))
}

/// The fields of a `/proc/<pid>/stat` line after the parenthesized name, which may itself
/// hold spaces and parentheses: the first is field 3, the state.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn stat_fields(stat: &str) -> Option<Vec<&str>> {
    let (_, fields) = stat.rsplit_once(')')?;
    let fields: Vec<&str> = fields.split_whitespace().collect();
    (!fields.is_empty()).then_some(fields)
}

/// The state letter of `/proc/<pid>/stat` (field 3).
pub fn process_state(letter: &str) -> ProcessState {
    match letter {
        "R" => ProcessState::Running,
        "S" => ProcessState::Sleeping,
        "D" => ProcessState::Waiting,
        "T" | "t" => ProcessState::Stopped,
        "Z" | "X" | "x" => ProcessState::Zombie,
        "I" => ProcessState::Idle,
        _ => ProcessState::Other,
    }
}

/// A process's start in Unix seconds from the boot time (`btime` of `/proc/stat`) and its
/// start in clock ticks since boot (field 22), as sysinfo reports it for a Linux host.
pub fn start_seconds(boot_time: u64, start_ticks: u64, ticks_per_second: u64) -> u64 {
    boot_time + start_ticks / ticks_per_second.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_lines_sum_their_columns() {
        let (label, ticks) = cpu_line("cpu  100 5 50 800 20 3 2 1 0 0").unwrap();
        assert_eq!(label, "cpu");
        assert_eq!(
            ticks,
            CpuTicks {
                user: 105,
                system: 56,
                idle: 820
            }
        );
        assert_eq!(cpu_line("cpu3 1 2 3 4").unwrap().1.idle, 4);
        assert_eq!(cpu_line("intr 1 2 3"), None);
        assert_eq!(cpu_line("cpu x"), None);
    }

    #[test]
    fn stat_fields_follow_a_name_with_spaces_and_parentheses() {
        let stat = "4242 (my (odd) prog) R 1 4242 4242 34816 4242 4194304 500 0 0 0 150 50 0 0 20 0 3 0 123456 2000000 2560";
        let fields = stat_fields(stat).unwrap();
        assert_eq!(fields[0], "R");
        // Field n is fields[n - 3]: parent 4, session 6, threads 20, start 22, rss 24.
        assert_eq!(
            (fields[1], fields[3], fields[17], fields[19], fields[21]),
            ("1", "4242", "3", "123456", "2560")
        );
        assert_eq!(stat_fields("4242 (cut"), None);
        assert_eq!(stat_fields("1 (x)"), None);
    }

    #[test]
    fn state_letters_name_states() {
        let states: Vec<ProcessState> = ["R", "S", "D", "T", "t", "Z", "X", "I", "W", ""]
            .into_iter()
            .map(process_state)
            .collect();
        use ProcessState::*;
        assert_eq!(
            states,
            [
                Running, Sleeping, Waiting, Stopped, Stopped, Zombie, Zombie, Idle, Other, Other
            ]
        );
    }

    #[test]
    fn start_ticks_become_unix_seconds_rounded_down() {
        assert_eq!(start_seconds(1_760_000_000, 123_456, 100), 1_760_001_234);
        assert_eq!(start_seconds(1_760_000_000, 99, 100), 1_760_000_000);
        assert_eq!(
            start_seconds(10, 5, 0),
            15,
            "a zero tick rate is not divided by"
        );
    }
}
