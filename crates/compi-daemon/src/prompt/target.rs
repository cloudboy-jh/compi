//! Runs commands and file operations in the environment Compi launches shells
//! in: the local Unix account, or a WSL2 distribution on Windows. An SSH
//! target is a remote Unix daemon, so it is `Local` there.
use std::io::{Read, Write};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(20);

/// Tests run against a temporary HOME and PATH in the real target.
#[cfg(test)]
pub(super) static TEST_ENV: parking_lot::Mutex<Vec<(String, String)>> =
    parking_lot::Mutex::new(Vec::new());

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Target {
    #[cfg(unix)]
    Local,
    #[cfg(windows)]
    Wsl(String),
}

impl Target {
    pub(super) fn resolve(distribution: Option<&str>) -> Result<Self, String> {
        #[cfg(unix)]
        {
            if distribution.is_some() {
                return Err("WSL distributions are only available on Windows".into());
            }
            Ok(Self::Local)
        }
        #[cfg(windows)]
        {
            compi_protocol::wsl::directory_distribution(distribution)
                .map(Self::Wsl)
                .map_err(|error| error.to_string())
        }
    }

    pub(super) fn distribution(&self) -> Option<String> {
        match self {
            #[cfg(unix)]
            Self::Local => None,
            #[cfg(windows)]
            Self::Wsl(name) => Some(name.clone()),
        }
    }

    fn command(&self, program: &str) -> Command {
        #[cfg(test)]
        let env = TEST_ENV.lock().clone();
        match self {
            #[cfg(unix)]
            Self::Local => {
                #[allow(unused_mut)]
                let mut command = Command::new(program);
                #[cfg(test)]
                command.envs(env);
                command
            }
            #[cfg(windows)]
            Self::Wsl(name) => {
                use std::os::windows::process::CommandExt;
                let mut command = Command::new(r"C:\Windows\System32\wsl.exe");
                command.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
                command.args(["--distribution", name, "--cd", "~", "--exec"]);
                #[cfg(test)]
                if !env.is_empty() {
                    command.arg("/usr/bin/env");
                    command.args(env.iter().map(|(key, value)| format!("{key}={value}")));
                }
                command.arg(program);
                command
            }
        }
    }

    /// Run a POSIX `sh` script with positional arguments. `wsl.exe` rejects an
    /// empty argument, so callers pass a placeholder instead.
    pub(super) fn sh(&self, script: &str, args: &[&str], stdin: &[u8]) -> Result<Output, String> {
        if args.iter().any(|arg| arg.is_empty()) {
            return Err("prompt helper arguments must not be empty".into());
        }
        let mut command = self.command("/bin/sh");
        command.args(["-c", script, "compi-prompt"]).args(args);
        run(command, stdin, TIMEOUT)
    }

    /// Run a script that must succeed, returning its standard output.
    pub(super) fn sh_ok(
        &self,
        context: &str,
        script: &str,
        args: &[&str],
        stdin: &[u8],
    ) -> Result<Vec<u8>, String> {
        let output = self.sh(script, args, stdin)?;
        if !output.status.success() {
            return Err(format!("{context}: {}", error_text(&output)));
        }
        Ok(output.stdout)
    }

    /// Read files; `None` for each one that does not exist.
    pub(super) fn read_files(&self, paths: &[String]) -> Result<Vec<Option<Vec<u8>>>, String> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let args: Vec<&str> = paths.iter().map(String::as_str).collect();
        let output = self.sh_ok("could not read prompt files", READ_FILES, &args, &[])?;
        parse_files(&output, paths.len())
    }

    pub(super) fn apply(&self, operations: &[FileOperation]) -> Result<(), String> {
        let mut input = Vec::new();
        for operation in operations {
            let (code, path, content) = match operation {
                FileOperation::Write { path, content } => ('w', path, content.as_slice()),
                FileOperation::WriteUser { path, content } => ('u', path, content.as_slice()),
                FileOperation::Delete { path } => ('d', path, &[][..]),
                FileOperation::RemoveBackup { path } => ('r', path, &[][..]),
            };
            validate_path(path)?;
            writeln!(input, "{code}\t{}\t{path}", content.len()).map_err(|e| e.to_string())?;
            input.extend_from_slice(content);
        }
        self.sh_ok("could not write prompt files", APPLY_FILES, &[], &input)
            .map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FileOperation {
    /// Private Compi file, replaced atomically with mode 0600.
    Write {
        path: String,
        content: Vec<u8>,
    },
    /// A user's file, rewritten in place so links, owner and mode stay.
    WriteUser {
        path: String,
        content: Vec<u8>,
    },
    Delete {
        path: String,
    },
    RemoveBackup {
        path: String,
    },
}

pub(super) fn validate_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/') || path.contains(['\0', '\n', '\t']) || path.len() > 4096 {
        return Err(format!("invalid prompt file path: {path:?}"));
    }
    Ok(())
}

/// The helper's error, or `wsl.exe`'s own message, which it writes to stdout.
pub(super) fn error_text(output: &Output) -> String {
    for stream in [&output.stderr, &output.stdout] {
        let text = decode(stream);
        let text = text.trim();
        if !text.is_empty() {
            return text.chars().take(600).collect();
        }
    }
    format!("exited with {}", output.status)
}

/// `wsl.exe` reports its own failures in UTF-16; Linux programs write UTF-8.
fn decode(bytes: &[u8]) -> String {
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

fn run(mut command: Command, stdin: &[u8], timeout: Duration) -> Result<Output, String> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start the prompt helper: {error}"))?;
    let mut input = child.stdin.take().expect("stdin is piped");
    let data = stdin.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = input.write_all(&data);
    });
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let out_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout.read_to_end(&mut buffer);
        buffer
    });
    let err_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stderr.read_to_end(&mut buffer);
        buffer
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "the prompt helper did not finish within {} seconds",
                    timeout.as_secs()
                ));
            }
            Err(error) => return Err(error.to_string()),
        }
    };
    let _ = writer.join();
    Ok(Output {
        status,
        stdout: out_reader.join().unwrap_or_default(),
        stderr: err_reader.join().unwrap_or_default(),
    })
}

/// Each file is `F <size>\n<bytes>` or `M\n` when it does not exist.
const READ_FILES: &str = r#"for path in "$@"; do
    if [ -f "$path" ] && [ -r "$path" ]; then
        printf 'F %s\n' "$(wc -c < "$path" | tr -d ' ')"
        cat -- "$path" || exit 1
    else
        printf 'M\n'
    fi
done"#;

fn parse_files(mut output: &[u8], count: usize) -> Result<Vec<Option<Vec<u8>>>, String> {
    let invalid = || "the prompt helper returned an invalid file listing".to_owned();
    let mut files = Vec::with_capacity(count);
    for _ in 0..count {
        let newline = output
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(invalid)?;
        let header = std::str::from_utf8(&output[..newline]).map_err(|_| invalid())?;
        output = &output[newline + 1..];
        if header == "M" {
            files.push(None);
            continue;
        }
        let size: usize = header
            .strip_prefix("F ")
            .and_then(|size| size.parse().ok())
            .ok_or_else(invalid)?;
        if output.len() < size {
            return Err(invalid());
        }
        files.push(Some(output[..size].to_vec()));
        output = &output[size..];
    }
    Ok(files)
}

/// Input is `<op>\t<size>\t<path>\n<bytes>` records. `read` and `dd bs=1` never
/// consume past a record, which keeps this portable to BSD userlands.
const APPLY_FILES: &str = r#"set -u
umask 077
base=$HOME/.compi/prompt
mkdir -p "$base" || exit 1
if [ -L "$base" ]; then echo 'the Compi prompt directory must not be a symbolic link' >&2; exit 1; fi
chmod 700 "$base" || exit 1
take() {
    tmp=$(mktemp "$1/.compi-prompt.XXXXXX") || exit 1
    dd bs=1 count="$2" of="$tmp" 2>/dev/null || { rm -f "$tmp"; exit 1; }
    [ "$(wc -c < "$tmp" | tr -d ' ')" = "$2" ] || { rm -f "$tmp"; echo 'truncated prompt file input' >&2; exit 1; }
}
while IFS='	' read -r op size path; do
    case $op in
        w)
            dir=${path%/*}
            mkdir -p "$dir" || exit 1
            take "$dir" "$size"
            mv -f "$tmp" "$path" || { rm -f "$tmp"; exit 1; }
            ;;
        u)
            take "$base" "$size"
            cat "$tmp" > "$path" || { rm -f "$tmp"; exit 1; }
            rm -f "$tmp"
            ;;
        d) rm -f -- "$path" || exit 1 ;;
        r)
            case $path in
                "$base"/backups/?*) rm -rf -- "$path" || exit 1 ;;
                *) echo 'refusing to remove a path outside prompt backups' >&2; exit 1 ;;
            esac
            ;;
        *) echo 'invalid prompt file operation' >&2; exit 1 ;;
    esac
done"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_listing_handles_missing_empty_and_binary_files() {
        let output = b"F 3\na\nbM\nF 0\nF 2\n\x00\xff";
        assert_eq!(
            parse_files(output, 4).unwrap(),
            [
                Some(b"a\nb".to_vec()),
                None,
                Some(Vec::new()),
                Some(vec![0, 255])
            ]
        );
        assert!(parse_files(b"F 9\nshort", 1).is_err());
        assert!(parse_files(b"M\n", 2).is_err());
    }

    #[test]
    fn paths_must_be_absolute_single_line_and_tab_free() {
        assert!(validate_path("/home/me/.zshrc").is_ok());
        for path in ["relative", "/a\nb", "/a\tb", "/a\0b"] {
            assert!(validate_path(path).is_err(), "{path:?}");
        }
    }
}
