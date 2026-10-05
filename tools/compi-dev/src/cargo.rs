//! Incremental builds into the workspace's normal Cargo target directory.

use crate::controller::{BuildOutcome, Target};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

pub struct Built {
    pub executable: PathBuf,
}

/// Build one binary. Diagnostics are captured and only shown on failure, so a
/// successful rebuild stays a single line.
pub fn build(
    workspace: &Path,
    target: Target,
    interrupted: impl Fn() -> bool,
) -> (BuildOutcome, Option<Built>) {
    let (package, binary) = match target {
        Target::Client => ("compi-client", "compi"),
        Target::Daemon => ("compi-daemon", "compi-daemon"),
    };
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let color = if std::io::stderr().is_terminal() {
        "always"
    } else {
        "never"
    };
    let started = Instant::now();
    let output = Command::new(cargo)
        .current_dir(workspace)
        .args(["build", "--locked", "-p", package, "--bin", binary])
        .args([
            "--message-format",
            "json-render-diagnostics",
            "--color",
            color,
        ])
        .stdin(Stdio::null())
        .output();
    let elapsed = started.elapsed();
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            return (
                BuildOutcome::Failed(format!("cannot run cargo: {error}")),
                None,
            );
        }
    };
    if interrupted() {
        return (BuildOutcome::Interrupted, None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return (BuildOutcome::Failed(explain(&stderr)), None);
    }
    let Some(executable) = artifact(&stdout, binary) else {
        return (
            BuildOutcome::Failed(format!("cargo reported no `{binary}` executable\n{stderr}")),
            None,
        );
    };
    let warnings = warning_count(&stderr);
    (
        BuildOutcome::Built { elapsed, warnings },
        Some(Built { executable }),
    )
}

/// Executable path of `binary` from Cargo's JSON message stream.
fn artifact(messages: &str, binary: &str) -> Option<PathBuf> {
    messages
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| message["reason"] == "compiler-artifact")
        .filter(|message| message["target"]["name"] == binary)
        .find_map(|message| message["executable"].as_str().map(PathBuf::from))
}

/// Sum of Cargo's per-crate "generated N warnings" summaries.
fn warning_count(stderr: &str) -> usize {
    stderr
        .lines()
        .filter_map(|line| {
            let tail = line.split(" generated ").nth(1)?;
            let count = tail.split_whitespace().next()?;
            tail.contains("warning")
                .then(|| count.parse::<usize>().ok())?
        })
        .sum()
}

/// Keep Cargo's diagnostics without its progress lines, and explain the one failure
/// Windows users hit from older manual previews: a process running straight out of
/// `target` locks its outputs.
fn explain(stderr: &str) -> String {
    let diagnostics: String = stderr
        .lines()
        .filter(|line| {
            let line = strip_ansi(line);
            let line = line.trim_start();
            !["Compiling ", "Checking ", "Building ", "Blocking "]
                .iter()
                .any(|progress| line.starts_with(progress))
        })
        .map(|line| format!("{line}\n"))
        .collect();
    let locked = [
        "Access is denied",
        "os error 5)",
        "os error 32)",
        "being used by another process",
    ]
    .iter()
    .any(|needle| stderr.contains(needle));
    if cfg!(windows) && locked {
        format!(
            "{diagnostics}\nA process running directly from the Cargo target directory (for \
             example an older manual preview daemon) is locking build output. `cargo dev` never \
             runs from there; stop that process if its shells are disposable, then save again."
        )
    } else {
        diagnostics
    }
}

/// Remove SGR color codes so colored output can be matched.
fn strip_ansi(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(char) = chars.next() {
        if char == '\u{1b}' {
            for char in chars.by_ref() {
                if char.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(char);
        }
    }
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_path_comes_from_the_named_binary() {
        let messages = r#"{"reason":"compiler-artifact","target":{"name":"compi_protocol"},"executable":null}
{"reason":"build-script-executed","package_id":"x"}
{"reason":"compiler-artifact","target":{"name":"compi"},"executable":"C:\\repo\\target\\debug\\compi.exe"}
{"reason":"build-finished","success":true}"#;
        assert_eq!(
            artifact(messages, "compi"),
            Some(PathBuf::from(r"C:\repo\target\debug\compi.exe"))
        );
        assert_eq!(artifact(messages, "compi-daemon"), None);
    }

    #[test]
    fn warning_summaries_are_summed() {
        let stderr = "warning: unused variable: `x`\n\
            warning: `compi-protocol` (lib) generated 1 warning\n\
            warning: `compi-client` (bin \"compi\") generated 3 warnings (run `cargo fix`)\n";
        assert_eq!(warning_count(stderr), 4);
        assert_eq!(warning_count("    Finished `dev` profile"), 0);
    }

    #[test]
    fn failure_output_keeps_diagnostics_but_drops_progress() {
        let stderr = "\u{1b}[1m\u{1b}[32m   Compiling\u{1b}[0m compi-client v0.1.3\n\
            error[E0425]: cannot find value `x` in this scope\n\
            \x20  --> crates/compi-client/src/gui.rs:1:1\n";
        let shown = explain(stderr);
        assert!(!shown.contains("Compiling"), "{shown}");
        assert!(shown.contains("error[E0425]"));
        assert!(shown.contains("--> crates/compi-client/src/gui.rs:1:1"));
    }
}
