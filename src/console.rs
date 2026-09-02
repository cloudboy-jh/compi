use crate::Result;
use crate::client::DaemonClient;
use crate::frame;
use crate::pipe;
use crate::protocol::{
    CONTROL_FRAME, ClientControl, ClientMessage, SCREEN_FRAME, ServerMessage, decode_server,
    encode_client,
};
use crate::terminal::{
    Color, MirrorApply, ScreenMirror, ScreenSnapshot, TextAttributes, decode_screen,
};
use std::fs::File;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Globalization::CP_UTF8;
use windows::Win32::Storage::FileSystem::{FILE_TYPE_CHAR, GetFileType, ReadFile};
use windows::Win32::System::Console::{
    CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS,
    ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT, ENABLE_QUICK_EDIT_MODE,
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleCP,
    GetConsoleMode, GetConsoleOutputCP, GetConsoleScreenBufferInfo, GetStdHandle, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE, SetConsoleCP, SetConsoleMode, SetConsoleOutputCP,
};

pub fn dimensions() -> (i16, i16) {
    unsafe {
        let Ok(output) = GetStdHandle(STD_OUTPUT_HANDLE) else {
            return (80, 24);
        };
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        if GetConsoleScreenBufferInfo(output, &mut info).is_err() {
            return (80, 24);
        }
        let cols = info.srWindow.Right - info.srWindow.Left + 1;
        let rows = info.srWindow.Bottom - info.srWindow.Top + 1;
        (cols.max(1), rows.max(1))
    }
}

pub fn attach(mut client: DaemonClient, session_id: String) -> Result<()> {
    let _console = ConsoleState::configure()?;
    let initial_size = dimensions();
    match client.request(ClientMessage::Attach {
        session_id,
        cols: initial_size.0,
        rows: initial_size.1,
    })? {
        ServerMessage::Attached { .. } => {}
        message => return Err(format!("unexpected attach response: {message:?}").into()),
    }

    let (connection, next_request_id, mut pending_screen) = client.into_parts();
    let pipe = Arc::new(connection);
    let write_lock = Arc::new(Mutex::new(()));
    let request_ids = Arc::new(AtomicU64::new(next_request_id));
    let running = Arc::new(AtomicBool::new(true));
    spawn_input_pump(
        pipe.clone(),
        write_lock.clone(),
        request_ids.clone(),
        running.clone(),
    );
    spawn_resize_pump(
        pipe.clone(),
        write_lock.clone(),
        request_ids.clone(),
        running.clone(),
        initial_size,
    );

    let mut output = io::stdout().lock();
    let mut mirror = ScreenMirror::default();
    while let Some(message) = pending_screen.pop_front() {
        let _ = mirror.apply(message);
    }
    if let Some(snapshot) = mirror.snapshot() {
        render_snapshot(&mut output, snapshot)?;
    }

    let mut reader = pipe::PipeReader::default();
    loop {
        let message = match reader.poll(&pipe)? {
            Some(message) => message,
            None => {
                thread::sleep(Duration::from_millis(5));
                continue;
            }
        };
        match message.kind {
            SCREEN_FRAME => match mirror.apply(decode_screen(&message.payload)?) {
                MirrorApply::Applied => {
                    if let Some(snapshot) = mirror.snapshot() {
                        render_snapshot(&mut output, snapshot)?;
                    }
                }
                MirrorApply::Gap { .. } => {
                    send(
                        &pipe,
                        &write_lock,
                        &request_ids,
                        ClientMessage::RequestSnapshot,
                    )?;
                }
            },
            CONTROL_FRAME => match decode_server(&message.payload)?.message {
                ServerMessage::Detached { .. } => break,
                ServerMessage::SessionExited { exit_code, .. } => {
                    output.write_all(
                        format!("\r\n[compi: shell exited {exit_code}]\r\n").as_bytes(),
                    )?;
                    output.flush()?;
                    break;
                }
                ServerMessage::Error { code, message } => {
                    return Err(format!("daemon error ({code:?}): {message}").into());
                }
                _ => {}
            },
            kind => return Err(format!("unknown daemon frame type {kind}").into()),
        }
    }

    running.store(false, Ordering::Release);
    Ok(())
}

fn render_snapshot(output: &mut impl Write, snapshot: &ScreenSnapshot) -> Result<()> {
    output.write_all(b"\x1b[?25l\x1b[2J\x1b[H")?;
    let mut active_style = String::new();
    for (row_index, row) in snapshot.cells.iter().enumerate() {
        if row_index != 0 {
            output.write_all(b"\r\n")?;
        }
        for cell in &row.cells {
            if cell.width == 0 {
                continue;
            }
            let style = sgr_style(cell.foreground, cell.background, &cell.attributes);
            if style != active_style {
                output.write_all(style.as_bytes())?;
                active_style = style;
            }
            output.write_all(cell.text.as_bytes())?;
        }
    }
    output.write_all(b"\x1b[0m")?;
    output.write_all(
        format!(
            "\x1b[{};{}H{}",
            snapshot.cursor.row + 1,
            snapshot.cursor.col + 1,
            if snapshot.cursor.visible {
                "\x1b[?25h"
            } else {
                "\x1b[?25l"
            }
        )
        .as_bytes(),
    )?;
    output.flush()?;
    Ok(())
}

fn sgr_style(foreground: Color, background: Color, attributes: &TextAttributes) -> String {
    let mut codes = vec!["0".to_owned()];
    if attributes.bold {
        codes.push("1".into());
    }
    if attributes.dim {
        codes.push("2".into());
    }
    if attributes.italic {
        codes.push("3".into());
    }
    if attributes.underline {
        codes.push("4".into());
    }
    if attributes.blink {
        codes.push("5".into());
    }
    if attributes.inverse {
        codes.push("7".into());
    }
    if attributes.hidden {
        codes.push("8".into());
    }
    if attributes.strike {
        codes.push("9".into());
    }
    codes.push(color_code(foreground, true));
    codes.push(color_code(background, false));
    format!("\x1b[{}m", codes.join(";"))
}

fn color_code(color: Color, foreground: bool) -> String {
    match color {
        Color::Default => {
            if foreground {
                "39".into()
            } else {
                "49".into()
            }
        }
        Color::Indexed(index @ 0..=7) => {
            (if foreground { 30 + index } else { 40 + index }).to_string()
        }
        Color::Indexed(index @ 8..=15) => (if foreground {
            90 + index - 8
        } else {
            100 + index - 8
        })
        .to_string(),
        Color::Indexed(index) => format!("{};5;{index}", if foreground { 38 } else { 48 }),
        Color::Rgb(red, green, blue) => format!(
            "{};2;{red};{green};{blue}",
            if foreground { 38 } else { 48 }
        ),
    }
}

fn spawn_input_pump(
    pipe: Arc<File>,
    write_lock: Arc<Mutex<()>>,
    request_ids: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let Ok(input) = (unsafe { GetStdHandle(STD_INPUT_HANDLE) }) else {
            return;
        };
        let mut buffer = [0_u8; 4096];
        while running.load(Ordering::Acquire) {
            let mut read = 0_u32;
            if unsafe { ReadFile(input, Some(&mut buffer), Some(&mut read), None) }.is_err() {
                break;
            }
            if read == 0 {
                break;
            }

            let bytes = &buffer[..read as usize];
            if let Some(detach_at) = bytes.iter().position(|byte| *byte == 0x1d) {
                if detach_at > 0
                    && send(
                        &pipe,
                        &write_lock,
                        &request_ids,
                        ClientMessage::Input {
                            data: bytes[..detach_at].to_vec(),
                        },
                    )
                    .is_err()
                {
                    break;
                }
                let _ = send(&pipe, &write_lock, &request_ids, ClientMessage::Detach);
                running.store(false, Ordering::Release);
                break;
            }

            if send(
                &pipe,
                &write_lock,
                &request_ids,
                ClientMessage::Input {
                    data: bytes.to_vec(),
                },
            )
            .is_err()
            {
                break;
            }
        }
    });
}

fn spawn_resize_pump(
    pipe: Arc<File>,
    write_lock: Arc<Mutex<()>>,
    request_ids: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
    mut previous: (i16, i16),
) {
    thread::spawn(move || {
        while running.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(100));
            let current = dimensions();
            if current != previous {
                if send(
                    &pipe,
                    &write_lock,
                    &request_ids,
                    ClientMessage::Resize {
                        cols: current.0,
                        rows: current.1,
                    },
                )
                .is_err()
                {
                    break;
                }
                previous = current;
            }
        }
    });
}

fn send(
    pipe: &File,
    lock: &Mutex<()>,
    request_ids: &AtomicU64,
    message: ClientMessage,
) -> Result<u64> {
    let request_id = request_ids.fetch_add(1, Ordering::Relaxed);
    let payload = encode_client(&ClientControl {
        request_id,
        message,
    })?;
    let _guard = lock.lock().map_err(|_| "pipe writer lock was poisoned")?;
    frame::write(&mut &*pipe, CONTROL_FRAME, &payload)?;
    Ok(request_id)
}

struct ConsoleState {
    input: Option<(HANDLE, CONSOLE_MODE)>,
    output: Option<(HANDLE, CONSOLE_MODE)>,
    input_code_page: u32,
    output_code_page: u32,
}

impl ConsoleState {
    fn configure() -> Result<Self> {
        unsafe {
            let input_code_page = GetConsoleCP();
            let output_code_page = GetConsoleOutputCP();
            let input = console_mode(STD_INPUT_HANDLE)?;
            let output = console_mode(STD_OUTPUT_HANDLE)?;
            let state = Self {
                input,
                output,
                input_code_page,
                output_code_page,
            };

            if let Some((handle, original)) = state.input {
                let mut mode = original;
                mode |= ENABLE_EXTENDED_FLAGS | ENABLE_VIRTUAL_TERMINAL_INPUT;
                mode &= !(ENABLE_ECHO_INPUT
                    | ENABLE_LINE_INPUT
                    | ENABLE_PROCESSED_INPUT
                    | ENABLE_QUICK_EDIT_MODE);
                SetConsoleMode(handle, mode)?;
                SetConsoleCP(CP_UTF8)?;
            }
            if let Some((handle, original)) = state.output {
                let mode = original | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
                SetConsoleMode(handle, mode)?;
                SetConsoleOutputCP(CP_UTF8)?;
            }
            Ok(state)
        }
    }
}

impl Drop for ConsoleState {
    fn drop(&mut self) {
        unsafe {
            if let Some((handle, mode)) = self.input {
                let _ = SetConsoleMode(handle, mode);
            }
            if let Some((handle, mode)) = self.output {
                let _ = SetConsoleMode(handle, mode);
            }
            if self.input.is_some() {
                let _ = SetConsoleCP(self.input_code_page);
            }
            if self.output.is_some() {
                let _ = SetConsoleOutputCP(self.output_code_page);
            }
        }
    }
}

unsafe fn console_mode(
    handle_kind: windows::Win32::System::Console::STD_HANDLE,
) -> Result<Option<(HANDLE, CONSOLE_MODE)>> {
    let handle = unsafe { GetStdHandle(handle_kind)? };
    if unsafe { GetFileType(handle) } != FILE_TYPE_CHAR {
        return Ok(None);
    }
    let mut original = CONSOLE_MODE::default();
    unsafe { GetConsoleMode(handle, &mut original)? };
    Ok(Some((handle, original)))
}
