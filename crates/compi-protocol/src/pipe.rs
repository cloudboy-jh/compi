use crate::Result;
use crate::frame::{self, Frame};
#[cfg(windows)]
use crate::identity::PipeSecurity;
#[cfg(windows)]
use std::ffi::OsStr;
use std::fs::File;
use std::io;
#[cfg(windows)]
use std::io::Read;
#[cfg(windows)]
use std::iter::once;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::thread;
use std::time::{Duration, Instant};
#[cfg(windows)]
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE,
    HANDLE, INVALID_HANDLE_VALUE,
};
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_SHARE_MODE,
    FlushFileBuffers, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
#[cfg(windows)]
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT, PeekNamedPipe, WaitNamedPipeW,
};
#[cfg(windows)]
use windows::core::{HRESULT, PCWSTR};

#[cfg(windows)]
const PIPE_BUFFER: u32 = 64 * 1024;
#[cfg(windows)]
const PIPE_INSTANCES: u32 = 64;

#[derive(Default)]
pub struct PipeReader {
    buffer: Vec<u8>,
}

impl PipeReader {
    #[cfg(windows)]
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

    #[cfg(unix)]
    pub fn poll(&mut self, file: &File) -> Result<Option<Frame>> {
        use std::os::fd::AsRawFd;
        if let Some(frame) = self.take_frame()? {
            return Ok(Some(frame));
        }
        let mut chunk = [0_u8; 32 * 1024];
        let count = unsafe {
            libc::recv(
                file.as_raw_fd(),
                chunk.as_mut_ptr().cast(),
                chunk.len(),
                libc::MSG_DONTWAIT,
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Ok(None);
            }
            return Err(error.into());
        }
        if count == 0 {
            return Err(
                io::Error::new(io::ErrorKind::UnexpectedEof, "local connection closed").into(),
            );
        }
        self.buffer.extend_from_slice(&chunk[..count as usize]);
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

#[cfg(windows)]
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

#[cfg(windows)]
pub fn accept(server: &File) -> Result<()> {
    let handle = HANDLE(server.as_raw_handle());
    match unsafe { ConnectNamedPipe(handle, None) } {
        Ok(()) => Ok(()),
        Err(error) if error.code() == HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
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

#[cfg(windows)]
pub fn flush(file: &File) -> Result<()> {
    unsafe { FlushFileBuffers(HANDLE(file.as_raw_handle()))? };
    Ok(())
}

#[cfg(windows)]
pub fn disconnect(file: &File) {
    let _ = unsafe { DisconnectNamedPipe(HANDLE(file.as_raw_handle())) };
}

#[cfg(windows)]
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

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(once(0)).collect()
}

#[cfg(unix)]
pub struct Listener {
    listener: std::os::unix::net::UnixListener,
    path: std::path::PathBuf,
}

#[cfg(unix)]
impl Listener {
    /// Caller holds the instance lock before recovering a stale endpoint.
    pub fn bind(name: &str) -> Result<Self> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
        let path = std::path::PathBuf::from(name);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() }
                {
                    return Err("refusing to replace an unowned or non-socket endpoint".into());
                }
                match std::os::unix::net::UnixStream::connect(&path) {
                    Ok(_) => return Err("a live server already owns this endpoint".into()),
                    Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {}
                    Err(error) => return Err(error.into()),
                }
                std::fs::remove_file(&path)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        let owner = Self { listener, path };
        std::fs::set_permissions(&owner.path, std::fs::Permissions::from_mode(0o600))?;
        Ok(owner)
    }

    pub fn accept(&self) -> Result<File> {
        let (stream, _) = self.listener.accept()?;
        check_peer(&stream)?;
        Ok(File::from(std::os::fd::OwnedFd::from(stream)))
    }
}

#[cfg(unix)]
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
pub fn connect(name: &str, timeout: Duration) -> Result<File> {
    let deadline = Instant::now() + timeout;
    loop {
        match std::os::unix::net::UnixStream::connect(name) {
            Ok(stream) => {
                check_peer(&stream)?;
                return Ok(File::from(std::os::fd::OwnedFd::from(stream)));
            }
            Err(error) => {
                if !matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) || Instant::now() >= deadline
                {
                    return Err(error.into());
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[cfg(unix)]
fn check_peer(stream: &std::os::unix::net::UnixStream) -> Result<()> {
    use std::os::fd::AsRawFd;
    #[cfg(target_os = "macos")]
    let uid = {
        let mut uid = 0;
        let mut gid = 0;
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        uid
    };
    #[cfg(not(target_os = "macos"))]
    let uid = {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of_val(&credentials) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut len,
            )
        } != 0
        {
            return Err(io::Error::last_os_error().into());
        }
        credentials.uid
    };
    if uid != unsafe { libc::geteuid() } {
        return Err("local connection peer is not the current user".into());
    }
    Ok(())
}

#[cfg(unix)]
pub fn flush(file: &File) -> Result<()> {
    use std::io::Write;
    let mut writer = file;
    writer.flush()?;
    Ok(())
}

#[cfg(unix)]
pub fn disconnect(file: &File) {
    use std::os::fd::AsRawFd;
    unsafe {
        libc::shutdown(file.as_raw_fd(), libc::SHUT_RDWR);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn fragmented_frames_remain_ordered_and_peer_close_is_reported() {
        let (receiver, mut sender) = UnixStream::pair().unwrap();
        let receiver = File::from(OwnedFd::from(receiver));
        let mut reader = PipeReader::default();
        sender.write_all(&[3, 0]).unwrap();
        assert!(reader.poll(&receiver).unwrap().is_none());
        sender.write_all(&[0, 0, 1, b'a']).unwrap();
        assert!(reader.poll(&receiver).unwrap().is_none());
        sender
            .write_all(&[b'b', b'c', 1, 0, 0, 0, 2, b'd'])
            .unwrap();
        let first = reader.poll(&receiver).unwrap().unwrap();
        assert_eq!((first.kind, first.payload), (1, b"abc".to_vec()));
        let second = reader.poll(&receiver).unwrap().unwrap();
        assert_eq!((second.kind, second.payload), (2, b"d".to_vec()));
        drop(sender);
        let error = reader.poll(&receiver).unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
