//! Setup shows one short sentence; the full detail goes to a log file.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_SHOWN: usize = 160;

/// Known failures, as the one sentence (plus what to do) a person should read.
const KNOWN: &[(&str, &str)] = &[
    (
        "Another install/update/repair/removal owns this installation",
        "Another Compi update or repair is running. Wait for it to finish, then try again.",
    ),
    (
        "Windows Installer still owns an active transaction",
        "Windows is installing something else. Wait for it to finish, then try again.",
    ),
    (
        "_MSIExecute",
        "Windows is installing something else. Wait for it to finish, then try again.",
    ),
    (
        "belongs to another Windows account",
        "Compi's background task belongs to another Windows account. Sign in as that account to change it.",
    ),
    (
        "belongs to another installation",
        "Compi's background task belongs to another copy of Compi. Remove that copy, then try again.",
    ),
    (
        "supervisor startup/recovery",
        "Compi's background service is starting. Wait a moment, then try again.",
    ),
    (
        "recovery backoff",
        "Compi's background service is restarting. Wait a moment, then try again.",
    ),
    (
        "processes changed during inspection",
        "Compi started or stopped while Setup was checking. Try again.",
    ),
    (
        "review shutdown consent again",
        "Your shells changed while Setup was open. Check again.",
    ),
    (
        "RevisionConflict",
        "Your shells changed while Setup was open. Check again.",
    ),
    (
        "Access is denied",
        "Windows denied access. Close Compi, then try again.",
    ),
    (
        "There is not enough space on the disk",
        "The disk is full. Free some space, then try again.",
    ),
];

/// One readable sentence for the window. Strips PowerShell error records ("At line:1
/// char:408 … CategoryInfo …"), installer jargon and anything past the first sentences.
pub(crate) fn short(raw: &str) -> String {
    if let Some((_, message)) = KNOWN.iter().find(|(needle, _)| raw.contains(needle)) {
        return (*message).to_owned();
    }
    let mut text = raw;
    for marker in [
        "At line:",
        "\nAt ",
        "+ CategoryInfo",
        "CategoryInfo",
        "+ FullyQualifiedErrorId",
        "FullyQualifiedErrorId",
    ] {
        if let Some(index) = text.find(marker) {
            text = &text[..index];
        }
    }
    let mut text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    // `Exception calling "Translate" with "1" argument(s): "..."` → the quoted reason.
    if let Some(rest) = text.strip_prefix("Exception calling ")
        && let Some(index) = rest.find("): ")
    {
        text = rest[index + 3..].trim_matches('"').to_owned();
    }
    let text = text.trim().trim_end_matches(':').trim();
    if text.is_empty() {
        return "Something went wrong. Open the log for details.".into();
    }
    let mut shown = String::new();
    for sentence in text.split_inclusive(". ") {
        if !shown.is_empty() && shown.len() + sentence.len() > MAX_SHOWN {
            break;
        }
        shown.push_str(sentence);
    }
    let shown = shown.trim();
    if shown.chars().count() <= MAX_SHOWN {
        return capitalized(shown);
    }
    let cut: String = shown.chars().take(MAX_SHOWN - 1).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    capitalized(&format!("{cut}…"))
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The message shown when Windows Installer returns a failure code.
pub(crate) fn installer_code(code: u32) -> &'static str {
    match code {
        1602 => "Cancelled. Nothing was changed.",
        1618 => "Windows is installing something else. Wait for it to finish, then try again.",
        1603 | 1601 | 1620 => "Windows Installer couldn't finish. Nothing was changed. Try again.",
        _ => "Windows Installer couldn't finish. Try again, or open the log.",
    }
}

/// Appends `text` to a readable log, one timestamped line per entry.
pub(crate) fn log(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let _ = writeln!(file, "{} {}", timestamp(), line.trim_end());
        }
    }
}

/// UTC "YYYY-MM-DD HH:MM:SS" without a calendar dependency.
fn timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (days, time) = (seconds / 86_400, seconds % 86_400);
    // Howard Hinnant's civil-from-days.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        time / 3600,
        time / 60 % 60,
        time % 60
    )
}

/// Megabytes with one decimal below 10 MB, whole numbers above.
pub(crate) fn megabytes(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb < 10.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{} MB", mb.round() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_error_records_become_their_message() {
        let record = "The daemon task belongs to a stale copy. Its registration was not changed.\r\nAt line:1 char:408\r\n+ ... ionComparison]::OrdinalIgnoreCase)) { throw 'The daemon task belongs  ...\r\n+                                                             ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~\r\n    + CategoryInfo          : OperationStopped: (The daemon task...) [], RuntimeException\r\n    + FullyQualifiedErrorId : The daemon task belongs to a stale copy.\r\n";
        assert_eq!(
            short(record),
            "The daemon task belongs to a stale copy. Its registration was not changed."
        );
    }

    #[test]
    fn known_failures_map_to_one_sentence_with_what_to_do() {
        assert_eq!(
            short(
                "Cannot safely change daemon task registration: The task Compi Daemon-S-1-5-21-1 belongs to another Windows account: johns. Ask its owner\nAt line:1 char:9\n    + CategoryInfo : x"
            ),
            "Compi's background task belongs to another Windows account. Sign in as that account to change it."
        );
        assert_eq!(
            short(
                "Another install/update/repair/removal owns this installation: The process cannot access the file"
            ),
            "Another Compi update or repair is running. Wait for it to finish, then try again."
        );
    }

    #[test]
    fn long_detail_is_cut_at_a_sentence_or_word() {
        let long = format!("First problem happened. {}", "word ".repeat(80));
        assert_eq!(short(&long), "First problem happened.");
        let single = "x".repeat(10) + &" word".repeat(60);
        let shown = short(&single);
        assert!(shown.chars().count() <= MAX_SHOWN && shown.ends_with('…'));
    }

    #[test]
    fn exception_wrappers_and_empty_errors_stay_readable() {
        assert_eq!(
            short(
                "Exception calling \"Translate\" with \"1\" argument(s): \"Some or all identity references could not be translated.\"\nAt line:1 char:3"
            ),
            "Some or all identity references could not be translated."
        );
        assert_eq!(
            short("\n   + CategoryInfo : x"),
            "Something went wrong. Open the log for details."
        );
    }

    #[test]
    fn installer_codes_never_show_numbers() {
        for code in [1602, 1603, 1618, 1638, 9999] {
            assert!(!installer_code(code).chars().any(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn sizes_read_in_megabytes() {
        assert_eq!(megabytes(5 * 1024 * 1024 + 300 * 1024), "5.3 MB");
        assert_eq!(megabytes(42 * 1024 * 1024 + 700 * 1024), "43 MB");
    }
}
