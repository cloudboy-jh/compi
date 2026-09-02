use crate::Result;
use crate::frame::{self, Frame};
use crate::identity::PipeSecurity;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::iter::once;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::thread;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE,
    HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_SHARE_MODE,
    FlushFileBuffers, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT, PeekNamedPipe, WaitNamedPipeW,
};
use windows::core::{HRESULT, PCWSTR};

const PIPE_BUFFER: u32 = 64 * 1024;
const PIPE_INSTANCES: u32 = 64;

#[derive(Default)]
pub struct PipeReader {
    buffer: Vec<u8>,
}

impl PipeReader {
    pub fn poll(&mut self, file: &File) -> Result<Option<Frame>> {
        if let Some(frame) = self.take_frame()? {
            return Ok(Some(frame));
        }

        let mut available = 0_u32;
        unsafe {
            PeekNamedPipe(
                HANDLE(file.as_raw_handle()),
                None,
                0,
                None,
                Some(&mut available),
                None,
            )?;
        }
        if available == 0 {
            return Ok(None);
        }

        let mut chunk = vec![0_u8; (available as usize).min(32 * 1024)];
        let mut reader = file;
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "named pipe closed").into());
        }
        self.buffer.extend_from_slice(&chunk[..read]);
        self.take_frame()
    }

    fn take_frame(&mut self) -> Result<Option<Frame>> {
        if self.buffer.len() < 5 {
            return Ok(None);
        }
        let length = u32::from_le_bytes(self.buffer[..4].try_into().unwrap()) as usize;
        if length > frame::MAX_PAYLOAD {
            return Err(format!("frame payload is too large: {length} bytes").into());
        }
        let frame_length = 5 + length;
        if self.buffer.len() < frame_length {
            return Ok(None);
        }

        let kind = self.buffer[4];
        let payload = self.buffer[5..frame_length].to_vec();
        self.buffer.drain(..frame_length);
        Ok(Some(Frame { kind, payload }))
    }
}

pub fn create_server(name: &str, security: &PipeSecurity, first: bool) -> Result<File> {
    let name = wide(name);
    let mut open_mode = PIPE_ACCESS_DUPLEX;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_INSTANCES,
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            Some(security.attributes()),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(windows::core::Error::from_thread().into());
    }
    Ok(unsafe { File::from_raw_handle(handle.0) })
}

pub fn accept(server: &File) -> Result<()> {
    let handle = HANDLE(server.as_raw_handle());
    match unsafe { ConnectNamedPipe(handle, None) } {
        Ok(()) => Ok(()),
        Err(error) if error.code() == HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn connect(name: &str, timeout: Duration) -> Result<File> {
    let name = wide(name);
    let deadline = Instant::now() + timeout;
    loop {
        match open_client(&name) {
            Ok(file) => return Ok(file),
            Err(error) => {
                let code = error
                    .downcast_ref::<windows::core::Error>()
                    .map(windows::core::Error::code);
                let retryable = matches!(
                    code,
                    Some(value)
                        if value == HRESULT::from_win32(ERROR_PIPE_BUSY.0)
                            || value == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0)
                );
                if !retryable || Instant::now() >= deadline {
                    return Err(error);
                }

                let remaining = deadline.saturating_duration_since(Instant::now());
                let wait_ms = remaining.min(Duration::from_millis(100)).as_millis() as u32;
                let _ = unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), wait_ms) };
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

pub fn flush(file: &File) -> Result<()> {
    unsafe { FlushFileBuffers(HANDLE(file.as_raw_handle()))? };
    Ok(())
}

pub fn disconnect(file: &File) {
    let _ = unsafe { DisconnectNamedPipe(HANDLE(file.as_raw_handle())) };
}

fn open_client(name: &[u16]) -> Result<File> {
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_MODE(0),
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )?
    };
    Ok(unsafe { File::from_raw_handle(handle.0) })
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(once(0)).collect()
}
