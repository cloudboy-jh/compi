//! The runner's terminal output: one line per event, `HH:MM:SS  tag      text`, the tag
//! naming what the event concerns. On a terminal the lines are coloured and work in
//! progress shows on a live line that its result replaces; piped output is plain lines.

use std::io::{IsTerminal, Write};
use std::sync::Mutex;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Dev,
    Client,
    Daemon,
    Preview,
    Change,
}

impl Tag {
    fn label(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Client => "client",
            Self::Daemon => "daemon",
            Self::Preview => "preview",
            Self::Change => "change",
        }
    }

    fn color(self) -> &'static str {
        match self {
            Self::Dev => "90",
            Self::Client => "36",
            Self::Daemon => "35",
            Self::Preview => "32",
            Self::Change => "34",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    /// Drawn dimmed: work in progress and other processes' output.
    Quiet,
    Warn,
    Error,
}

const TAG_WIDTH: usize = 7;
/// `HH:MM:SS` + two spaces + tag column + two spaces.
const INDENT: usize = 8 + 2 + TAG_WIDTH + 2;

struct State {
    color: bool,
    live: bool,
    /// The live line on screen, redrawn after any line printed above it.
    progress: Option<(Tag, String)>,
    /// When the change behind the next preview refresh was saved.
    saved_at: Option<SystemTime>,
}

static STATE: Mutex<State> = Mutex::new(State {
    color: false,
    live: false,
    progress: None,
    saved_at: None,
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Decide colour and the live line once, before anything is printed.
pub fn init() {
    let terminal = std::io::stdout().is_terminal() && enable_escapes();
    let mut state = state();
    state.live = terminal;
    state.color = terminal && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty());
}

/// The runner's first lines: what it runs and the keys it reads.
pub fn header(instance: &str, runtime: &str) {
    let state = state();
    let mut out = std::io::stdout().lock();
    let _ = writeln!(
        out,
        "{} · {instance} · {runtime}",
        paint(&state, "1", "compi dev")
    );
    let _ = writeln!(
        out,
        "{}\n",
        paint(
            &state,
            "90",
            "⏎ reopen preview   r⏎ restart dev daemon   q⏎ quit"
        )
    );
}

/// An event. It replaces the live line: it is that work's result, or what stopped it.
pub fn line(tag: Tag, tone: Tone, text: &str) {
    let mut state = state();
    let mut out = std::io::stdout().lock();
    clear_progress(&state, &mut out);
    state.progress = None;
    let _ = writeln!(out, "{}", format_line(&state, tag, tone, text));
}

/// A line a child process printed, dimmed, kept above the live line.
pub fn output(tag: Tag, text: &str) {
    let state = state();
    let mut out = std::io::stdout().lock();
    clear_progress(&state, &mut out);
    let _ = writeln!(out, "{}", format_line(&state, tag, Tone::Quiet, text));
    redraw_progress(&state, &mut out);
}

/// Work under way, replaced by the next line on a terminal; nothing when piped, where
/// the result line says it all.
pub fn progress(tag: Tag, text: &str) {
    let mut state = state();
    if !state.live {
        return;
    }
    let mut out = std::io::stdout().lock();
    clear_progress(&state, &mut out);
    state.progress = Some((tag, text.to_owned()));
    redraw_progress(&state, &mut out);
}

/// Lines belonging to the line above, such as compiler diagnostics, indented under its
/// text.
pub fn detail(text: &str) {
    let mut state = state();
    let mut out = std::io::stdout().lock();
    clear_progress(&state, &mut out);
    state.progress = None;
    for line in text.lines() {
        let _ = writeln!(out, "{:INDENT$}{line}", "");
    }
}

/// Finish a live line left by an interrupted step.
pub fn finish() {
    let mut state = state();
    if state.progress.take().is_some() {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out);
    }
}

/// Remember when the change now being applied was saved (the earliest of a batch).
pub fn saved(at: Option<SystemTime>) {
    let mut state = state();
    state.saved_at = crate::watch::earliest(state.saved_at, at);
}

/// How long ago the change behind this refresh was saved; forgets it.
pub fn take_save_latency() -> Option<std::time::Duration> {
    state()
        .saved_at
        .take()
        .and_then(|saved| saved.elapsed().ok())
}

pub fn forget_save() {
    state().saved_at = None;
}

fn format_line(state: &State, tag: Tag, tone: Tone, text: &str) -> String {
    let time = paint(state, "90", &clock());
    let label = paint(state, tag.color(), &format!("{:TAG_WIDTH$}", tag.label()));
    let text = match tone {
        Tone::Plain => text.to_owned(),
        Tone::Quiet => paint(state, "90", text),
        Tone::Warn => paint(state, "33", text),
        Tone::Error => paint(state, "31", text),
    };
    format!("{time}  {label}  {text}")
}

fn clear_progress(state: &State, out: &mut impl Write) {
    if state.progress.is_some() {
        let _ = write!(out, "\r\x1b[2K");
    }
}

fn redraw_progress(state: &State, out: &mut impl Write) {
    if let Some((tag, text)) = &state.progress {
        let _ = write!(out, "{}", format_line(state, *tag, Tone::Quiet, text));
        let _ = out.flush();
    }
}

fn paint(state: &State, code: &str, text: &str) -> String {
    if state.color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

#[cfg(windows)]
fn clock() -> String {
    // SAFETY: GetLocalTime only writes the returned struct.
    let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:02}:{:02}:{:02}", now.wHour, now.wMinute, now.wSecond)
}

#[cfg(unix)]
fn clock() -> String {
    let seconds = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as libc::time_t)
        .unwrap_or_default();
    // SAFETY: localtime_r writes only into `local`, which outlives the call.
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&seconds, &mut local) }.is_null() {
        return "--:--:--".into();
    }
    format!(
        "{:02}:{:02}:{:02}",
        local.tm_hour, local.tm_min, local.tm_sec
    )
}

/// Windows consoles draw escape sequences only once asked to; elsewhere they always do.
#[cfg(windows)]
fn enable_escapes() -> bool {
    use windows::Win32::System::Console::{
        CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle,
        STD_OUTPUT_HANDLE, SetConsoleMode,
    };
    // SAFETY: plain console calls on this process's own output handle.
    unsafe {
        let Ok(handle) = GetStdHandle(STD_OUTPUT_HANDLE) else {
            return false;
        };
        let mut mode = CONSOLE_MODE::default();
        if GetConsoleMode(handle, &mut mode).is_err() {
            // Not a console (a pseudo-terminal pipe such as WSL's): it reads escapes.
            return true;
        }
        mode.0 & ENABLE_VIRTUAL_TERMINAL_PROCESSING.0 != 0
            || SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING).is_ok()
    }
}

#[cfg(not(windows))]
fn enable_escapes() -> bool {
    true
}
