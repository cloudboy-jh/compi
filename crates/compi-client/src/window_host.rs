//! Same-user, same-daemon-instance GUI ownership and bounded window launch delivery.
use crate::config::LoadedConfig;
use compi_protocol::{Result, identity};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const VERSION: u32 = 1;
const MAX_FRAME: usize = 256 * 1024;
const MAX_PENDING: usize = 32;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(10);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchRequest {
    pub initial_working_directory: Option<String>,
    pub config: LoadedConfig,
    #[serde(skip)]
    _permit: Option<QueuePermit>,
}

impl LaunchRequest {
    pub fn new(initial_working_directory: Option<String>, config: LoadedConfig) -> Self {
        Self {
            initial_working_directory,
            config,
            _permit: None,
        }
    }

    fn validate(&self) -> Result<()> {
        if self
            .initial_working_directory
            .as_ref()
            .is_some_and(|path| path.is_empty() || path.len() > 32 * 1024 || path.contains('\0'))
        {
            return Err("invalid launch working directory".into());
        }
        self.config.validate_launch().map_err(Into::into)
    }
}

struct QueuePermit(Arc<AtomicUsize>);

impl Drop for QueuePermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    instance: String,
    launch: LaunchRequest,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", deny_unknown_fields)]
enum Reply {
    Ready { version: u32 },
    Accepted,
    Rejected { message: String },
}

pub enum HostAcquisition {
    Forwarded,
    Host(WindowHost, Box<LaunchRequest>),
}

/// Keep on the acquiring (GUI/main) thread until `gui::run` returns. In particular,
/// the Windows mutex is thread-owned. Drop joins the nonblocking transport worker
/// before releasing ownership, so a successor cannot race endpoint cleanup.
pub struct WindowHost {
    receiver: Option<Receiver<LaunchRequest>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    _lock: platform::HostLock,
}

impl WindowHost {
    pub fn take_receiver(&mut self) -> Receiver<LaunchRequest> {
        self.receiver
            .take()
            .expect("GUI launch receiver was already taken")
    }
}

impl Drop for WindowHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.receiver.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Elect without starting GPUI. A successful forward means the host has accepted
/// this resolved invocation into its bounded queue, not that rendering completed.
/// Once connected, a failed handshake is never retried: delivery may be ambiguous.
pub fn acquire(instance: Option<&str>, request: LaunchRequest) -> Result<HostAcquisition> {
    request.validate()?;
    let names = identity::instance_names(instance)?;
    let endpoint = format!("{}.gui", names.pipe);
    let lock_name = format!("{}.gui", names.mutex);
    let instance = names.pipe;
    let envelope = Envelope {
        version: VERSION,
        instance: instance.clone(),
        launch: request,
    };
    let payload = serde_json::to_vec(&envelope)?;
    if payload.len() > MAX_FRAME {
        return Err("window launch request exceeds 256 KiB".into());
    }
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if let Some(lock) = platform::HostLock::try_acquire(&lock_name)? {
            let listener = platform::Listener::bind(&endpoint)?;
            let (sender, receiver) = mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = stop.clone();
            let worker = thread::Builder::new()
                .name("compi-window-host".into())
                .spawn(move || {
                    serve(listener, sender, instance, &worker_stop);
                })?;
            return Ok(HostAcquisition::Host(
                WindowHost {
                    receiver: Some(receiver),
                    stop,
                    worker: Some(worker),
                    _lock: lock,
                },
                Box::new(envelope.launch),
            ));
        }
        if let Some(mut connection) = platform::connect(&endpoint)? {
            let stop = AtomicBool::new(false);
            let deadline = Instant::now() + REQUEST_TIMEOUT;
            match receive::<Reply>(&mut connection, deadline, &stop)? {
                Reply::Ready { version: VERSION } => {}
                _ => return Err("incompatible GUI host handshake".into()),
            }
            send_bytes(&mut connection, &payload, deadline, &stop)?;
            let reply = receive::<Reply>(&mut connection, deadline, &stop)?;
            // Receipt prevents a Windows disconnect from discarding unread output.
            let _ = write_all(&mut connection, &[1], deadline, &stop);
            match reply {
                Reply::Accepted => return Ok(HostAcquisition::Forwarded),
                Reply::Rejected { message } => {
                    return Err(format!("GUI host rejected launch: {message}").into());
                }
                Reply::Ready { .. } => return Err("unexpected GUI host response".into()),
            }
        }
        if Instant::now() >= deadline {
            return Err(
                "GUI host owns this instance but did not become ready within five seconds".into(),
            );
        }
        thread::sleep(POLL);
    }
}

fn serve(
    listener: platform::Listener,
    sender: Sender<LaunchRequest>,
    instance: String,
    stop: &AtomicBool,
) {
    let pending = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok(Some(mut connection)) => {
                let deadline = Instant::now() + REQUEST_TIMEOUT;
                let result = (|| -> Result<()> {
                    send(
                        &mut connection,
                        &Reply::Ready { version: VERSION },
                        deadline,
                        stop,
                    )?;
                    let mut envelope: Envelope = receive(&mut connection, deadline, stop)?;
                    if envelope.version != VERSION || envelope.instance != instance {
                        return Err("launch protocol or instance does not match this host".into());
                    }
                    envelope.launch.validate()?;
                    if pending.load(Ordering::Acquire) >= MAX_PENDING {
                        return Err("window launch queue is full".into());
                    }
                    pending.fetch_add(1, Ordering::AcqRel);
                    envelope.launch._permit = Some(QueuePermit(pending.clone()));
                    sender
                        .send(envelope.launch)
                        .map_err(|_| "GUI host is closing")?;
                    send(&mut connection, &Reply::Accepted, deadline, stop)?;
                    let mut receipt = [0];
                    let _ = read_exact(&mut connection, &mut receipt, deadline, stop);
                    Ok(())
                })();
                if let Err(error) = result
                    && send(
                        &mut connection,
                        &Reply::Rejected {
                            message: error.to_string(),
                        },
                        deadline,
                        stop,
                    )
                    .is_ok()
                {
                    let mut receipt = [0];
                    let _ = read_exact(&mut connection, &mut receipt, deadline, stop);
                }
                listener.disconnect(&connection);
            }
            Ok(None) => thread::sleep(POLL),
            Err(error) => {
                // An untrusted peer or a disconnected client must not terminate the
                // host. No client data or environment values are logged here.
                eprintln!("Compi GUI host transport: {error}");
                thread::sleep(POLL);
            }
        }
    }
}

fn send<T: Serialize>(
    file: &mut File,
    value: &T,
    deadline: Instant,
    stop: &AtomicBool,
) -> Result<()> {
    send_bytes(file, &serde_json::to_vec(value)?, deadline, stop)
}

fn send_bytes(file: &mut File, bytes: &[u8], deadline: Instant, stop: &AtomicBool) -> Result<()> {
    if bytes.len() > MAX_FRAME {
        return Err("GUI host frame exceeds 256 KiB".into());
    }
    write_all(file, &(bytes.len() as u32).to_le_bytes(), deadline, stop)?;
    write_all(file, bytes, deadline, stop)
}

fn receive<T: serde::de::DeserializeOwned>(
    file: &mut File,
    deadline: Instant,
    stop: &AtomicBool,
) -> Result<T> {
    let mut prefix = [0; 4];
    read_exact(file, &mut prefix, deadline, stop)?;
    let length = u32::from_le_bytes(prefix) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err("invalid GUI host frame length".into());
    }
    let mut bytes = vec![0; length];
    read_exact(file, &mut bytes, deadline, stop)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn check_deadline(deadline: Instant, stop: &AtomicBool) -> Result<()> {
    if stop.load(Ordering::Acquire) {
        return Err("GUI host is closing".into());
    }
    if Instant::now() >= deadline {
        return Err("GUI host handshake timed out; delivery may be unknown".into());
    }
    Ok(())
}

fn read_exact(
    file: &mut File,
    mut bytes: &mut [u8],
    deadline: Instant,
    stop: &AtomicBool,
) -> Result<()> {
    while !bytes.is_empty() {
        check_deadline(deadline, stop)?;
        match platform::read(file, bytes) {
            Ok(0) => return Err("GUI host connection closed".into()),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if platform::would_block(&error) => thread::sleep(POLL),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn write_all(
    file: &mut File,
    mut bytes: &[u8],
    deadline: Instant,
    stop: &AtomicBool,
) -> Result<()> {
    while !bytes.is_empty() {
        check_deadline(deadline, stop)?;
        match file.write(bytes) {
            Ok(0) => thread::sleep(POLL),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if platform::would_block(&error) => thread::sleep(POLL),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::io::Read;
    use std::os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    };

    pub struct HostLock {
        _file: File,
    }

    impl HostLock {
        pub fn try_acquire(name: &str) -> Result<Option<Self>> {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(name)?;
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o077 != 0
            {
                return Err("unsafe GUI host lock ownership or permissions".into());
            }
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    return Ok(None);
                }
                return Err(error.into());
            }
            // Retain this inode even after exit: unlinking permits two owners.
            Ok(Some(Self { _file: file }))
        }
    }

    pub struct Listener(compi_protocol::pipe::Listener);

    impl Listener {
        pub fn bind(name: &str) -> Result<Self> {
            let listener = compi_protocol::pipe::Listener::bind(name)?;
            listener.set_nonblocking(true)?;
            Ok(Self(listener))
        }

        pub fn accept(&self) -> Result<Option<File>> {
            match self.0.accept() {
                Ok(file) => {
                    nonblocking(&file)?;
                    Ok(Some(file))
                }
                Err(error) if error.downcast_ref::<io::Error>().is_some_and(would_block) => {
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        }

        pub fn disconnect(&self, file: &File) {
            compi_protocol::pipe::disconnect(file);
        }
    }

    pub fn connect(name: &str) -> Result<Option<File>> {
        match compi_protocol::pipe::connect(name, Duration::from_millis(50)) {
            Ok(file) => {
                nonblocking(&file)?;
                Ok(Some(file))
            }
            Err(error)
                if error.downcast_ref::<io::Error>().is_some_and(|error| {
                    matches!(
                        error.kind(),
                        io::ErrorKind::NotFound
                            | io::ErrorKind::ConnectionRefused
                            | io::ErrorKind::WouldBlock
                    ) || error.raw_os_error() == Some(libc::EINPROGRESS)
                }) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn nonblocking(file: &File) -> Result<()> {
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
        {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }

    pub fn read(file: &mut File, bytes: &mut [u8]) -> io::Result<usize> {
        file.read(bytes)
    }

    pub fn would_block(error: &io::Error) -> bool {
        error.kind() == io::ErrorKind::WouldBlock
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{
        ffi::OsStr,
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
    };
    use windows::{
        Win32::{
            Foundation::{
                ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_NO_DATA, ERROR_PIPE_BUSY,
                ERROR_PIPE_CONNECTED, ERROR_PIPE_LISTENING, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ,
                GENERIC_WRITE, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree, WAIT_ABANDONED,
                WAIT_OBJECT_0, WAIT_TIMEOUT,
            },
            Security::{
                Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY,
                TOKEN_USER, TokenUser,
            },
            Storage::FileSystem::{
                CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_SHARE_MODE, OPEN_EXISTING,
                PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
            },
            System::{
                Pipes::{
                    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe,
                    GetNamedPipeClientProcessId, GetNamedPipeServerProcessId, PIPE_NOWAIT,
                    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
                    SetNamedPipeHandleState,
                },
                Threading::{
                    CreateMutexW, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
                    ReleaseMutex, WaitForSingleObject,
                },
            },
        },
        core::{HRESULT, PCWSTR, PWSTR},
    };

    pub struct HostLock {
        handle: OwnedHandle,
        // Mutex ownership is thread-affine even though OwnedHandle itself is Send.
        _thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }

    impl HostLock {
        pub fn try_acquire(name: &str) -> Result<Option<Self>> {
            let security = identity::PipeSecurity::for_current_user()?;
            let name = wide(name);
            let handle =
                unsafe { CreateMutexW(Some(security.attributes()), false, PCWSTR(name.as_ptr()))? };
            let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
            match unsafe { WaitForSingleObject(HANDLE(handle.as_raw_handle()), 0) } {
                WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Some(Self {
                    handle,
                    _thread: std::marker::PhantomData,
                })),
                WAIT_TIMEOUT => Ok(None),
                _ => Err(windows::core::Error::from_thread().into()),
            }
        }
    }

    impl Drop for HostLock {
        fn drop(&mut self) {
            let _ = unsafe { ReleaseMutex(HANDLE(self.handle.as_raw_handle())) };
        }
    }

    pub struct Listener {
        file: File,
        sid: String,
    }

    impl Listener {
        pub fn bind(name: &str) -> Result<Self> {
            let security = identity::PipeSecurity::for_current_user()?;
            let name = wide(name);
            let handle = unsafe {
                CreateNamedPipeW(
                    PCWSTR(name.as_ptr()),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    MAX_FRAME as u32 + 4,
                    MAX_FRAME as u32 + 4,
                    0,
                    Some(security.attributes()),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(windows::core::Error::from_thread().into());
            }
            let file = unsafe { File::from_raw_handle(handle.0) };
            Ok(Self {
                file,
                sid: identity::current_user_sid_string()?,
            })
        }
        pub fn accept(&self) -> Result<Option<File>> {
            let handle = HANDLE(self.file.as_raw_handle());
            match unsafe { ConnectNamedPipe(handle, None) } {
                // In NOWAIT mode success means the disconnected instance became
                // available, not that a client connected. The next poll reports
                // ERROR_PIPE_CONNECTED once a client has actually arrived.
                Ok(()) => return Ok(None),
                Err(error) if error.code() == HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) => {}
                Err(error) if error.code() == HRESULT::from_win32(ERROR_PIPE_LISTENING.0) => {
                    return Ok(None);
                }
                Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_DATA.0) => {
                    let _ = unsafe { DisconnectNamedPipe(handle) };
                    return Ok(None);
                }
                Err(error) => return Err(error.into()),
            }
            if let Err(error) = check_peer(&self.file, false, &self.sid) {
                self.disconnect(&self.file);
                return Err(error);
            }
            Ok(Some(self.file.try_clone()?))
        }
        pub fn disconnect(&self, file: &File) {
            let _ = unsafe { DisconnectNamedPipe(HANDLE(file.as_raw_handle())) };
        }
    }

    pub fn connect(name: &str) -> Result<Option<File>> {
        let name = wide(name);
        let handle = match unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                None,
            )
        } {
            Ok(handle) => handle,
            Err(error)
                if [ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY]
                    .iter()
                    .any(|code| error.code() == HRESULT::from_win32(code.0)) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let file = unsafe { File::from_raw_handle(handle.0) };
        check_peer(&file, true, &identity::current_user_sid_string()?)?;
        let mode = PIPE_NOWAIT | PIPE_READMODE_BYTE;
        unsafe {
            SetNamedPipeHandleState(handle, Some(&mode), None, None)?;
        }
        Ok(Some(file))
    }

    pub fn read(file: &mut File, bytes: &mut [u8]) -> io::Result<usize> {
        let mut count = 0;
        // File::read normalizes ERROR_NO_DATA to EOF on Windows. A connected
        // PIPE_NOWAIT pipe reports that status whenever its peer has not written yet.
        match unsafe {
            ReadFile(
                HANDLE(file.as_raw_handle()),
                Some(bytes),
                Some(&mut count),
                None,
            )
        } {
            Ok(()) => Ok(count as usize),
            Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_DATA.0) => {
                Err(io::ErrorKind::WouldBlock.into())
            }
            Err(error)
                if [ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED]
                    .iter()
                    .any(|code| error.code() == HRESULT::from_win32(code.0)) =>
            {
                Ok(0)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn check_peer(file: &File, server: bool, sid: &str) -> Result<()> {
        unsafe {
            let mut pid = 0;
            if server {
                GetNamedPipeServerProcessId(HANDLE(file.as_raw_handle()), &mut pid)?;
            } else {
                GetNamedPipeClientProcessId(HANDLE(file.as_raw_handle()), &mut pid)?;
            }
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)?;
            let process = OwnedHandle::from_raw_handle(process.0);
            let mut token = HANDLE::default();
            OpenProcessToken(HANDLE(process.as_raw_handle()), TOKEN_QUERY, &mut token)?;
            let token = OwnedHandle::from_raw_handle(token.0);
            let mut required = 0;
            let _ = GetTokenInformation(
                HANDLE(token.as_raw_handle()),
                TokenUser,
                None,
                0,
                &mut required,
            );
            if required == 0 {
                return Err(windows::core::Error::from_thread().into());
            }
            let mut storage =
                vec![0_usize; (required as usize).div_ceil(std::mem::size_of::<usize>())];
            GetTokenInformation(
                HANDLE(token.as_raw_handle()),
                TokenUser,
                Some(storage.as_mut_ptr().cast()),
                required,
                &mut required,
            )?;
            let user = &*storage.as_ptr().cast::<TOKEN_USER>();
            let mut string_sid = PWSTR::null();
            ConvertSidToStringSidW(user.User.Sid, &mut string_sid)?;
            let peer = string_sid.to_string();
            let _ = LocalFree(Some(HLOCAL(string_sid.0.cast())));
            if peer? != sid {
                return Err("GUI host connection peer is not the current user".into());
            }
        }
        Ok(())
    }

    pub fn would_block(error: &io::Error) -> bool {
        error.kind() == io::ErrorKind::WouldBlock
            || error.raw_os_error() == Some(ERROR_NO_DATA.0 as i32)
    }

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain(Some(0)).collect()
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn instance() -> String {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        format!(
            "host-read-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn delayed_named_pipe_handshake_completes() {
        let instance = instance();
        let names = identity::instance_names(Some(&instance)).unwrap();
        // Own the mutex on this thread so the other invocation must forward.
        let _lock = platform::HostLock::try_acquire(&format!("{}.gui", names.mutex))
            .unwrap()
            .unwrap();
        let listener = platform::Listener::bind(&format!("{}.gui", names.pipe)).unwrap();
        let (finished_sender, finished_receiver) = mpsc::channel();
        let client = thread::spawn(move || {
            let result = acquire(
                Some(&instance),
                LaunchRequest::new(None, LoadedConfig::default()),
            )
            .map(|acquisition| matches!(acquisition, HostAcquisition::Forwarded))
            .map_err(|error| error.to_string());
            finished_sender.send(result).unwrap();
        });

        // The endpoint exists, but no host worker can send Ready yet. An idle
        // NOWAIT read must keep the invocation pending rather than report EOF.
        assert!(matches!(
            finished_receiver.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (sender, _receiver) = mpsc::channel();
        let server = thread::spawn(move || serve(listener, sender, names.pipe, &worker_stop));
        let result = finished_receiver.recv_timeout(STARTUP_TIMEOUT);
        stop.store(true, Ordering::Release);
        server.join().unwrap();
        client.join().unwrap();

        // Exercise the real acquire/serve handshake, validation, acceptance, and
        // receipt over the secured named pipe, without starting GPUI.
        assert!(result.unwrap().unwrap());
    }

    #[test]
    fn named_pipe_read_distinguishes_idle_from_disconnected() {
        let names = identity::instance_names(Some(&instance())).unwrap();
        let endpoint = format!("{}.gui", names.pipe);
        let listener = platform::Listener::bind(&endpoint).unwrap();
        let mut client = platform::connect(&endpoint).unwrap().unwrap();
        let mut byte = [0];
        assert_eq!(
            platform::read(&mut client, &mut byte).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );

        drop(listener);
        assert_eq!(platform::read(&mut client, &mut byte).unwrap(), 0);
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        assert!(read_exact(&mut client, &mut byte, deadline, &AtomicBool::new(false),).is_err());
        assert!(Instant::now() < deadline, "EOF waited for the deadline");
    }

    #[test]
    fn idle_named_pipe_read_obeys_deadline_and_stop() {
        let names = identity::instance_names(Some(&instance())).unwrap();
        let endpoint = format!("{}.gui", names.pipe);
        let _listener = platform::Listener::bind(&endpoint).unwrap();
        let mut client = platform::connect(&endpoint).unwrap().unwrap();
        let mut byte = [0];
        let stop = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_millis(50);
        assert!(read_exact(&mut client, &mut byte, deadline, &stop).is_err());
        assert!(Instant::now() >= deadline, "idle pipe was mistaken for EOF");

        stop.store(true, Ordering::Release);
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        assert!(read_exact(&mut client, &mut byte, deadline, &stop).is_err());
        assert!(Instant::now() < deadline, "stop waited for the deadline");
    }
}
