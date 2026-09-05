use crate::terminal::TerminalState;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

const TRACE_MAGIC: &[u8; 8] = b"COMPITR1";
const TRACE_HEADER_LIMIT: usize = 64 * 1024;
pub const MAX_TERMINAL_TRACE_BYTES: u64 = 16 * 1024 * 1024;

const EVENT_INPUT: u8 = 1;
const EVENT_OUTPUT: u8 = 2;
const EVENT_RESIZE: u8 = 3;
const EVENT_TRUNCATED: u8 = u8::MAX;
const EVENT_HEADER_BYTES: u64 = 1 + 8 + 4;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TerminalTraceHeader {
    pub version: u8,
    pub session_id: String,
    pub label: Option<String>,
    pub initial_cols: u16,
    pub initial_rows: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalTraceEvent {
    Input {
        elapsed_us: u64,
        data: Vec<u8>,
    },
    Output {
        elapsed_us: u64,
        data: Vec<u8>,
    },
    Resize {
        elapsed_us: u64,
        cols: u16,
        rows: u16,
    },
    Truncated {
        elapsed_us: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalTrace {
    pub header: TerminalTraceHeader,
    pub events: Vec<TerminalTraceEvent>,
}

impl TerminalTrace {
    pub fn replay(&self) -> TerminalState {
        let mut terminal = TerminalState::new(self.header.initial_cols, self.header.initial_rows);
        for event in &self.events {
            match event {
                TerminalTraceEvent::Output { data, .. } => {
                    terminal.advance(data);
                }
                TerminalTraceEvent::Resize { cols, rows, .. } => {
                    terminal.resize(*cols, *rows);
                }
                TerminalTraceEvent::Input { .. } | TerminalTraceEvent::Truncated { .. } => {}
            }
        }
        terminal
    }
}

pub struct TerminalTraceRecorder {
    path: PathBuf,
    file: File,
    started_at: Instant,
    bytes_written: u64,
    max_bytes: u64,
    stopped: bool,
}

impl TerminalTraceRecorder {
    pub fn from_env(session_id: &str, cols: u16, rows: u16) -> io::Result<Option<Self>> {
        let Some(directory) = env::var_os("COMPI_TERMINAL_TRACE_DIR") else {
            return Ok(None);
        };
        let label = env::var("COMPI_TERMINAL_TRACE_LABEL")
            .ok()
            .map(|value| sanitize_component(&value))
            .filter(|value| !value.is_empty());
        let header = TerminalTraceHeader {
            version: 1,
            session_id: session_id.to_owned(),
            label: label.clone(),
            initial_cols: cols,
            initial_rows: rows,
        };
        fs::create_dir_all(&directory)?;
        let directory = PathBuf::from(directory);
        let session_component = sanitize_component(session_id);
        let stem = label
            .map(|label| format!("{label}-{session_component}"))
            .unwrap_or(session_component);
        let (path, file) = create_unique_trace(&directory, &stem)?;
        let recorder = Self::create(path, file, header, MAX_TERMINAL_TRACE_BYTES)?;
        eprintln!(
            "compi-daemon: terminal trace enabled; input and output may contain sensitive data: {}",
            recorder.path.display()
        );
        Ok(Some(recorder))
    }

    fn create(
        path: PathBuf,
        mut file: File,
        header: TerminalTraceHeader,
        max_bytes: u64,
    ) -> io::Result<Self> {
        let header = serde_json::to_vec(&header)
            .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
        if header.len() > TRACE_HEADER_LIMIT {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "terminal trace header is too large",
            ));
        }
        let header_len = u32::try_from(header.len()).map_err(|_| {
            io::Error::new(
                ErrorKind::InvalidInput,
                "terminal trace header is too large",
            )
        })?;
        let initial_bytes = TRACE_MAGIC.len() as u64 + 4 + u64::from(header_len);
        if initial_bytes > max_bytes {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "terminal trace limit cannot contain its header",
            ));
        }
        file.write_all(TRACE_MAGIC)?;
        file.write_all(&header_len.to_le_bytes())?;
        file.write_all(&header)?;
        file.flush()?;
        Ok(Self {
            path,
            file,
            started_at: Instant::now(),
            bytes_written: initial_bytes,
            max_bytes,
            stopped: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn record_input(&mut self, data: &[u8]) -> io::Result<()> {
        self.record(EVENT_INPUT, data)
    }

    pub fn record_output(&mut self, data: &[u8]) -> io::Result<()> {
        self.record(EVENT_OUTPUT, data)
    }

    pub fn record_resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        let mut payload = [0_u8; 4];
        payload[..2].copy_from_slice(&cols.to_le_bytes());
        payload[2..].copy_from_slice(&rows.to_le_bytes());
        self.record(EVENT_RESIZE, &payload)
    }

    fn record(&mut self, kind: u8, payload: &[u8]) -> io::Result<()> {
        if self.stopped {
            return Ok(());
        }
        let payload_len = u32::try_from(payload.len()).map_err(|_| {
            io::Error::new(ErrorKind::InvalidInput, "terminal trace event is too large")
        })?;
        let event_bytes = EVENT_HEADER_BYTES + u64::from(payload_len);
        if self.bytes_written.saturating_add(event_bytes) > self.max_bytes {
            self.write_truncated_marker()?;
            self.stopped = true;
            return Ok(());
        }
        self.write_event(kind, payload_len, payload)?;
        self.bytes_written += event_bytes;
        Ok(())
    }

    fn write_truncated_marker(&mut self) -> io::Result<()> {
        if self.bytes_written.saturating_add(EVENT_HEADER_BYTES) <= self.max_bytes {
            self.write_event(EVENT_TRUNCATED, 0, &[])?;
            self.bytes_written += EVENT_HEADER_BYTES;
        }
        self.file.flush()
    }

    fn write_event(&mut self, kind: u8, payload_len: u32, payload: &[u8]) -> io::Result<()> {
        let elapsed_us = self
            .started_at
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        self.file.write_all(&[kind])?;
        self.file.write_all(&elapsed_us.to_le_bytes())?;
        self.file.write_all(&payload_len.to_le_bytes())?;
        self.file.write_all(payload)?;
        self.file.flush()
    }
}

pub fn read_terminal_trace(path: impl AsRef<Path>) -> io::Result<TerminalTrace> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() > MAX_TERMINAL_TRACE_BYTES {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "terminal trace exceeds the size limit",
        ));
    }
    let mut magic = [0_u8; TRACE_MAGIC.len()];
    file.read_exact(&mut magic)?;
    if &magic != TRACE_MAGIC {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "invalid terminal trace magic",
        ));
    }
    let header_len = read_u32(&mut file)? as usize;
    if header_len > TRACE_HEADER_LIMIT {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "terminal trace header exceeds the limit",
        ));
    }
    let mut header = vec![0_u8; header_len];
    file.read_exact(&mut header)?;
    let header: TerminalTraceHeader = serde_json::from_slice(&header)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
    if header.version != 1 || header.initial_cols == 0 || header.initial_rows == 0 {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "unsupported or invalid terminal trace header",
        ));
    }

    let mut events = Vec::new();
    loop {
        let mut kind = [0_u8; 1];
        match file.read(&mut kind)? {
            0 => break,
            1 => {}
            _ => unreachable!(),
        }
        let elapsed_us = read_u64(&mut file)?;
        let payload_len = read_u32(&mut file)? as usize;
        if payload_len as u64 > MAX_TERMINAL_TRACE_BYTES {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "terminal trace event exceeds the limit",
            ));
        }
        let mut payload = vec![0_u8; payload_len];
        file.read_exact(&mut payload)?;
        let event = match kind[0] {
            EVENT_INPUT => TerminalTraceEvent::Input {
                elapsed_us,
                data: payload,
            },
            EVENT_OUTPUT => TerminalTraceEvent::Output {
                elapsed_us,
                data: payload,
            },
            EVENT_RESIZE if payload.len() == 4 => TerminalTraceEvent::Resize {
                elapsed_us,
                cols: u16::from_le_bytes([payload[0], payload[1]]),
                rows: u16::from_le_bytes([payload[2], payload[3]]),
            },
            EVENT_TRUNCATED if payload.is_empty() => TerminalTraceEvent::Truncated { elapsed_us },
            _ => {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "invalid terminal trace event",
                ));
            }
        };
        events.push(event);
    }
    Ok(TerminalTrace { header, events })
}

fn create_unique_trace(directory: &Path, stem: &str) -> io::Result<(PathBuf, File)> {
    for suffix in 0..1_000_u16 {
        let file_name = if suffix == 0 {
            format!("{stem}.compi-trace")
        } else {
            format!("{stem}-{suffix}.compi-trace")
        };
        let path = directory.join(file_name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        ErrorKind::AlreadyExists,
        "could not allocate a unique terminal trace path",
    ))
}

fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .filter_map(|character| {
            character
                .is_ascii_alphanumeric()
                .then_some(character)
                .or_else(|| matches!(character, '-' | '_').then_some(character))
        })
        .take(48)
        .collect()
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn trace_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("compi-{name}-{}-{nonce}.trace", std::process::id()))
    }

    fn recorder(path: &Path, max_bytes: u64) -> TerminalTraceRecorder {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        TerminalTraceRecorder::create(
            path.to_owned(),
            file,
            TerminalTraceHeader {
                version: 1,
                session_id: "trace-test".to_owned(),
                label: Some("bash".to_owned()),
                initial_cols: 8,
                initial_rows: 2,
            },
            max_bytes,
        )
        .unwrap()
    }

    #[test]
    fn trace_round_trips_and_replays_output_and_resize() {
        let path = trace_path("round-trip");
        let mut recorder = recorder(&path, MAX_TERMINAL_TRACE_BYTES);
        recorder.record_input(b"printf hello\\n").unwrap();
        recorder.record_output(b"hello").unwrap();
        recorder.record_resize(12, 3).unwrap();
        drop(recorder);

        let trace = read_terminal_trace(&path).unwrap();
        assert_eq!(trace.header.session_id, "trace-test");
        assert_eq!(trace.events.len(), 3);
        assert!(matches!(
            &trace.events[0],
            TerminalTraceEvent::Input { data, .. } if data == b"printf hello\\n"
        ));
        let snapshot = trace.replay().snapshot();
        assert_eq!((snapshot.cols, snapshot.rows), (12, 3));
        assert_eq!(snapshot.cells[0].cells[0].text, "h");
        assert_eq!(snapshot.cells[0].cells[4].text, "o");

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn trace_stops_at_bound_and_records_truncation() {
        let path = trace_path("bounded");
        let header = TerminalTraceHeader {
            version: 1,
            session_id: "trace-test".to_owned(),
            label: Some("bounded".to_owned()),
            initial_cols: 8,
            initial_rows: 2,
        };
        let header_bytes = serde_json::to_vec(&header).unwrap();
        let initial_bytes = TRACE_MAGIC.len() as u64 + 4 + header_bytes.len() as u64;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let mut recorder = TerminalTraceRecorder::create(
            path.clone(),
            file,
            header,
            initial_bytes + EVENT_HEADER_BYTES,
        )
        .unwrap();
        recorder.record_output(b"too large").unwrap();
        recorder.record_input(b"ignored").unwrap();
        drop(recorder);

        let trace = read_terminal_trace(&path).unwrap();
        assert_eq!(trace.events.len(), 1);
        assert!(matches!(
            trace.events[0],
            TerminalTraceEvent::Truncated { .. }
        ));
        assert!(fs::metadata(&path).unwrap().len() <= initial_bytes + EVENT_HEADER_BYTES);

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn reader_rejects_trace_larger_than_capture_limit() {
        let path = trace_path("oversized");
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.set_len(MAX_TERMINAL_TRACE_BYTES + 1).unwrap();
        drop(file);

        let error = read_terminal_trace(&path).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);

        fs::remove_file(path).unwrap();
    }
}
