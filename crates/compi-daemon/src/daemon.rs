use crate::Result;
use crate::launch;
use crate::surface::{ConnectionSink, Surface, SurfaceError, SurfaceManager};
use compi_protocol::frame;
#[cfg(windows)]
use compi_protocol::identity::PipeSecurity;
use compi_protocol::identity::{self, InstanceNames};
use compi_protocol::pipe;
use compi_protocol::{
    CONTROL_FRAME, ClientMessage, ErrorCode, IMAGE_UPLOAD_CHUNK_BYTES, MAX_IMAGE_UPLOAD_BYTES,
    PROTOCOL_VERSION, RuntimeMetrics, ServerControl, ServerMessage, SurfaceId, SurfaceStatus,
    TerminalTarget, UploadId, decode_client,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
#[cfg(windows)]
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::iter::once;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::io::{FromRawHandle, OwnedHandle};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
#[cfg(windows)]
use windows::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS,
    SetHandleInformation,
};
#[cfg(windows)]
use windows::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CreateMutexW, DETACHED_PROCESS, ReleaseMutex,
};
#[cfg(windows)]
use windows::core::PCWSTR;

const MAX_CONNECTION_UPLOADS: usize = 4;
const REMOTE_IMAGE_STORE_BYTES: u64 = 512 * 1024 * 1024;
const REMOTE_IMAGE_STORE_FILES: usize = 4096;
static NEXT_UPLOAD: AtomicU64 = AtomicU64::new(1);
static UPLOAD_STORE_LOCK: Mutex<()> = Mutex::new(());

struct ImageUpload {
    temporary: PathBuf,
    file: Option<File>,
    extension: String,
    expected_bytes: u64,
    expected_sha256: [u8; 32],
    written: u64,
    hasher: Sha256,
}

impl Drop for ImageUpload {
    fn drop(&mut self) {
        let _ = self.file.take();
        let _ = std::fs::remove_file(&self.temporary);
    }
}

pub fn relay_stdio(instance: Option<&str>) -> Result<()> {
    #[cfg(windows)]
    prevent_stdio_inheritance()?;
    ensure_daemon(instance)?;
    let names = identity::instance_names(instance)?;
    let connection = Arc::new(pipe::connect(&names.pipe, Duration::from_secs(2))?);
    let writer = connection.clone();
    let (finished_tx, finished_rx) = mpsc::channel();

    let input_finished = finished_tx.clone();
    thread::spawn(move || {
        let result = io::copy(&mut io::stdin().lock(), &mut &*writer).map(|_| ());
        let _ = input_finished.send(result);
    });
    thread::spawn(move || {
        let mut output = io::stdout().lock();
        let mut buffer = [0_u8; 32 * 1024];
        let result = loop {
            match pipe::read_available(&connection, &mut buffer) {
                Ok(Some(count)) => {
                    if let Err(error) = output
                        .write_all(&buffer[..count])
                        .and_then(|()| output.flush())
                    {
                        break Err(error);
                    }
                }
                Ok(None) => thread::sleep(Duration::from_millis(2)),
                Err(error) => {
                    let kind = error
                        .downcast_ref::<io::Error>()
                        .map_or(io::ErrorKind::Other, io::Error::kind);
                    break Err(io::Error::new(kind, error.to_string()));
                }
            }
        };
        let _ = finished_tx.send(result);
    });

    match finished_rx.recv()? {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::UnexpectedEof
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn prevent_stdio_inheritance() -> Result<()> {
    for stream in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        let handle = unsafe { GetStdHandle(stream)? };
        unsafe {
            SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))?;
        }
    }
    Ok(())
}

fn ensure_daemon(instance: Option<&str>) -> Result<()> {
    if compi_protocol::DaemonClient::connect(instance, Duration::from_millis(100)).is_ok() {
        return Ok(());
    }

    let directory = compi_protocol::paths::data_dir()?;
    std::fs::create_dir_all(&directory)?;
    let suffix = instance.map(|name| format!("-{name}")).unwrap_or_default();
    let log_path = directory.join(format!("daemon{suffix}.log"));
    let log = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&log_path)?;
    let log_error = log.try_clone()?;
    let mut command = Command::new(std::env::current_exe()?);
    if let Some(instance) = instance {
        command.arg("--instance").arg(instance);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_error));
    #[cfg(windows)]
    command.creation_flags(DETACHED_PROCESS.0 | CREATE_NEW_PROCESS_GROUP.0);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if compi_protocol::DaemonClient::connect(instance, Duration::from_millis(100)).is_ok() {
            #[cfg(unix)]
            thread::spawn(move || {
                let _ = child.wait();
            });
            #[cfg(windows)]
            drop(child);
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(format!(
                "daemon exited with {status}; inspect {}",
                log_path.display()
            )
            .into());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("daemon did not start; inspect {}", log_path.display()).into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn begin_image_upload(
    name: &str,
    expected_bytes: u64,
    expected_sha256: [u8; 32],
) -> Result<(UploadId, ImageUpload)> {
    if expected_bytes == 0 || expected_bytes > MAX_IMAGE_UPLOAD_BYTES as u64 {
        return Err(
            format!("image upload must contain 1 through {MAX_IMAGE_UPLOAD_BYTES} bytes").into(),
        );
    }
    let extension = upload_extension(name)?;
    let root = upload_directory()?;
    for _ in 0..16 {
        let nonce = NEXT_UPLOAD.fetch_add(1, Ordering::Relaxed);
        let upload_id = UploadId::new(format!("upload-{:x}-{nonce:x}", std::process::id()));
        let temporary = root.join(format!(".{}.part", upload_id.as_str()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temporary) {
            Ok(file) => {
                return Ok((
                    upload_id,
                    ImageUpload {
                        temporary,
                        file: Some(file),
                        extension,
                        expected_bytes,
                        expected_sha256,
                        written: 0,
                        hasher: Sha256::new(),
                    },
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err("could not allocate a unique image upload".into())
}

fn append_image_upload(upload: &mut ImageUpload, offset: u64, data: &[u8]) -> Result<u64> {
    if data.is_empty() || data.len() > IMAGE_UPLOAD_CHUNK_BYTES {
        return Err(format!(
            "image upload chunks must contain 1 through {IMAGE_UPLOAD_CHUNK_BYTES} bytes"
        )
        .into());
    }
    if offset != upload.written {
        return Err(format!(
            "image upload offset mismatch: expected {}, got {offset}",
            upload.written
        )
        .into());
    }
    let next = upload
        .written
        .checked_add(u64::try_from(data.len())?)
        .filter(|next| *next <= upload.expected_bytes)
        .ok_or("image upload exceeds its declared length")?;
    upload
        .file
        .as_mut()
        .ok_or("image upload is already finished")?
        .write_all(data)?;
    upload.hasher.update(data);
    upload.written = next;
    Ok(next)
}

fn finish_image_upload(mut upload: ImageUpload) -> Result<String> {
    if upload.written != upload.expected_bytes {
        return Err(format!(
            "image upload is incomplete: expected {} bytes, received {}",
            upload.expected_bytes, upload.written
        )
        .into());
    }
    upload
        .file
        .as_mut()
        .ok_or("image upload is already finished")?
        .sync_all()?;
    upload.file.take();
    let actual: [u8; 32] = std::mem::take(&mut upload.hasher).finalize().into();
    if actual != upload.expected_sha256 {
        return Err("image upload checksum does not match".into());
    }
    let root = upload
        .temporary
        .parent()
        .ok_or("image upload has no storage directory")?;
    let destination = root.join(format!("{}.{}", digest_hex(&actual), upload.extension));
    let _store = UPLOAD_STORE_LOCK
        .lock()
        .map_err(|_| "remote image storage lock was poisoned")?;
    match std::fs::symlink_metadata(&destination) {
        Ok(_) => verify_image_file(&destination, upload.expected_bytes, &actual)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ensure_upload_capacity(root, upload.expected_bytes)?;
            match std::fs::hard_link(&upload.temporary, &destination) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    verify_image_file(&destination, upload.expected_bytes, &actual)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    File::open(root)?.sync_all()?;
    destination
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| "remote image path is not valid UTF-8".into())
}

fn upload_directory() -> Result<PathBuf> {
    let data = compi_protocol::paths::data_dir()?;
    std::fs::create_dir_all(&data)?;
    let root = data.join("uploaded-images");
    match std::fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err("remote image storage is not a directory".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir(&root)?;
            #[cfg(unix)]
            std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        }
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(&root)?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(
                "remote image storage must be owned by the daemon user with mode 0700".into(),
            );
        }
    }
    Ok(root)
}

fn ensure_upload_capacity(root: &Path, incoming: u64) -> Result<()> {
    let mut used = 0_u64;
    let mut files = 0_usize;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if !metadata.is_file() {
            return Err("remote image storage contains an unexpected entry".into());
        }
        files = files.saturating_add(1);
        used = used.saturating_add(metadata.len());
    }
    if files >= REMOTE_IMAGE_STORE_FILES || used.saturating_add(incoming) > REMOTE_IMAGE_STORE_BYTES
    {
        return Err(format!(
            "remote image storage is full ({REMOTE_IMAGE_STORE_BYTES} bytes or {REMOTE_IMAGE_STORE_FILES} files)"
        )
        .into());
    }
    Ok(())
}

fn upload_extension(name: &str) -> Result<String> {
    if name.is_empty()
        || name.len() > 255
        || name.chars().any(char::is_control)
        || name.contains(['/', '\\'])
    {
        return Err("image upload name must be a plain file name of at most 255 bytes".into());
    }
    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or("image upload name requires a supported extension")?;
    if !matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp"
    ) {
        return Err("supported image upload formats are PNG, JPEG, WebP, GIF and BMP".into());
    }
    Ok(extension)
}

fn verify_image_file(path: &Path, expected_bytes: u64, expected_sha256: &[u8; 32]) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() != expected_bytes {
        return Err("existing remote image does not match the uploaded content".into());
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual: [u8; 32] = hasher.finalize().into();
    if &actual != expected_sha256 {
        return Err("existing remote image checksum does not match".into());
    }
    Ok(())
}

fn digest_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

pub fn run(instance: Option<&str>) -> Result<()> {
    let started_at = Instant::now();
    let names = identity::instance_names(instance)?;
    let _singleton = DaemonSingleton::acquire(&names.mutex)?;
    compi_protocol::perf::log_startup_metric("daemon_singleton_ready_ms", started_at.elapsed());
    let manager = Arc::new(SurfaceManager::persistent(instance)?);
    compi_protocol::perf::log_startup_metric("daemon_store_ready_ms", started_at.elapsed());
    launch::check_system()?;
    compi_protocol::perf::log_startup_metric("daemon_host_ready_ms", started_at.elapsed());
    #[cfg(windows)]
    let security = PipeSecurity::for_current_user()?;
    compi_protocol::perf::log_startup_metric("daemon_security_ready_ms", started_at.elapsed());
    let stopping = Arc::new(AtomicBool::new(false));
    if compi_protocol::perf::enabled() {
        let sampler_manager = manager.clone();
        let sampler_stopping = stopping.clone();
        thread::spawn(move || {
            while !sampler_stopping.load(Ordering::Acquire) {
                thread::sleep(Duration::from_secs(6));
                if sampler_stopping.load(Ordering::Acquire) {
                    break;
                }
                compi_protocol::perf::log_resource_sample(
                    "daemon",
                    "server",
                    sampler_manager.surface_count(),
                );
            }
        });
    }
    let connections = Arc::new(Mutex::new(HashMap::<u64, ConnectionSink>::new()));
    let handlers = Arc::new(Mutex::new(Vec::<JoinHandle<()>>::new()));
    compi_protocol::perf::log_startup_metric("daemon_ready_ms", started_at.elapsed());
    let connection_ids = AtomicU64::new(1);

    let result = serve(
        &names,
        #[cfg(windows)]
        &security,
        manager.clone(),
        stopping.clone(),
        connections.clone(),
        handlers.clone(),
        &connection_ids,
    );

    stopping.store(true, Ordering::Release);
    if let Ok(connections) = connections.lock() {
        for connection in connections.values() {
            connection.disconnect();
        }
    }
    if let Ok(mut handlers) = handlers.lock() {
        for handler in handlers.drain(..) {
            let _ = handler.join();
        }
    }
    let shutdown_reason = match &result {
        Ok(()) => "surface ended because the daemon stopped intentionally".to_owned(),
        Err(error) => format!("surface ended because the daemon failed: {error}"),
    };
    manager.shutdown_all(&shutdown_reason);
    result
}

fn serve(
    names: &InstanceNames,
    #[cfg(windows)] security: &PipeSecurity,
    manager: Arc<SurfaceManager>,
    stopping: Arc<AtomicBool>,
    connections: Arc<Mutex<HashMap<u64, ConnectionSink>>>,
    handlers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    connection_ids: &AtomicU64,
) -> Result<()> {
    #[cfg(windows)]
    let mut server = pipe::create_server(&names.pipe, security, true)?;
    #[cfg(unix)]
    let listener = pipe::Listener::bind(&names.pipe)?;
    while !stopping.load(Ordering::Acquire) {
        #[cfg(windows)]
        pipe::accept(&server)?;
        #[cfg(unix)]
        let server = listener.accept()?;
        if stopping.load(Ordering::Acquire) {
            break;
        }

        let connection = Arc::new(server);
        let connection_id = connection_ids.fetch_add(1, Ordering::Relaxed);
        let sink = ConnectionSink::new(connection_id, connection.clone())?;
        connections
            .lock()
            .map_err(|_| "connection registry lock was poisoned")?
            .insert(connection_id, sink.clone());

        let handler_manager = manager.clone();
        let handler_stopping = stopping.clone();
        let handler_connections = connections.clone();
        let wake_pipe = names.pipe.clone();
        let handler = thread::spawn(move || {
            if let Err(error) = handle_connection(
                connection,
                sink.clone(),
                handler_manager,
                handler_stopping,
                &wake_pipe,
            ) {
                eprintln!("compi-daemon: connection {connection_id}: {error}");
            }
            sink.disconnect();
            if let Ok(mut connections) = handler_connections.lock() {
                connections.remove(&connection_id);
            }
        });
        let mut active_handlers = handlers
            .lock()
            .map_err(|_| "handler registry lock was poisoned")?;
        reap_finished_handlers(&mut active_handlers);
        active_handlers.push(handler);

        if stopping.load(Ordering::Acquire) {
            break;
        }
        #[cfg(windows)]
        {
            server = pipe::create_server(&names.pipe, security, false)?;
        }
    }
    Ok(())
}
fn reap_finished_handlers(handlers: &mut Vec<JoinHandle<()>>) {
    let mut index = 0;
    while index < handlers.len() {
        if handlers[index].is_finished() {
            let handler = handlers.swap_remove(index);
            let _ = handler.join();
        } else {
            index += 1;
        }
    }
}

fn handle_connection(
    connection: Arc<File>,
    sink: ConnectionSink,
    manager: Arc<SurfaceManager>,
    stopping: Arc<AtomicBool>,
    wake_pipe: &str,
) -> Result<()> {
    let mut reader = pipe::PipeReader::default();
    let Some(first) = read_next(&mut reader, &connection, &stopping)? else {
        return Ok(());
    };
    if first.kind != CONTROL_FRAME {
        send_error_sync(
            &sink,
            None,
            ErrorCode::InvalidRequest,
            "first frame must be a hello control message",
        );
        return Ok(());
    }
    let hello = match decode_client(&first.payload) {
        Ok(hello) => hello,
        Err(error) => {
            send_error_sync(
                &sink,
                None,
                ErrorCode::InvalidRequest,
                &format!("invalid hello payload: {error}"),
            );
            return Ok(());
        }
    };
    let ClientMessage::Hello { protocol_version } = hello.message else {
        send_error_sync(
            &sink,
            Some(hello.request_id),
            ErrorCode::InvalidRequest,
            "first control message must be hello",
        );
        return Ok(());
    };
    if protocol_version != PROTOCOL_VERSION {
        send_error_sync(
            &sink,
            Some(hello.request_id),
            ErrorCode::IncompatibleProtocol,
            &format!(
                "client protocol {protocol_version} is incompatible with daemon protocol {PROTOCOL_VERSION}"
            ),
        );
        return Ok(());
    }
    sink.send_control(&ServerControl {
        request_id: Some(hello.request_id),
        message: ServerMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    })?;

    let mut attached: Option<Arc<Surface>> = None;
    let mut workspace_events = manager.subscribe();
    let mut uploads = HashMap::<UploadId, ImageUpload>::new();
    let result = (|| -> Result<()> {
        while !stopping.load(Ordering::Acquire) && sink.is_alive() {
            send_workspace_events(&sink, &mut workspace_events, &manager)?;
            let Some(incoming) = reader.poll(&connection)? else {
                thread::sleep(Duration::from_millis(5));
                continue;
            };
            if incoming.kind != CONTROL_FRAME {
                send_error(
                    &sink,
                    None,
                    ErrorCode::InvalidRequest,
                    "clients may only send control frames",
                );
                continue;
            }
            let request = match decode_client(&incoming.payload) {
                Ok(request) => request,
                Err(error) => {
                    send_error(
                        &sink,
                        None,
                        ErrorCode::InvalidRequest,
                        &format!("invalid control payload: {error}"),
                    );
                    continue;
                }
            };
            if matches!(request.message, ClientMessage::Hello { .. }) {
                send_error(
                    &sink,
                    Some(request.request_id),
                    ErrorCode::InvalidRequest,
                    "hello may only be sent once",
                );
                continue;
            }

            let request_id = request.request_id;
            let target = request.target;
            match request.message {
                ClientMessage::GetWorkspace => match manager.snapshot() {
                    Ok(workspace) => sink.send_control(&ServerControl {
                        request_id: Some(request_id),
                        message: ServerMessage::Workspace { workspace },
                    })?,
                    Err(error) => send_actor_error(&sink, request_id, &error),
                },
                ClientMessage::GetRuntimeMetrics => match manager.snapshot() {
                    Ok(workspace) => {
                        let live_surfaces = workspace
                            .surfaces
                            .iter()
                            .filter(|surface| {
                                matches!(
                                    surface.status,
                                    SurfaceStatus::Starting
                                        | SurfaceStatus::Running
                                        | SurfaceStatus::Ending
                                )
                            })
                            .count();
                        let attached_surfaces = workspace
                            .surfaces
                            .iter()
                            .filter(|surface| surface.attached)
                            .count();
                        sink.send_control(&ServerControl {
                            request_id: Some(request_id),
                            message: ServerMessage::RuntimeMetrics {
                                metrics: RuntimeMetrics {
                                    process: compi_protocol::perf::process_metrics(),
                                    surfaces: workspace.surfaces.len().min(u32::MAX as usize)
                                        as u32,
                                    live_surfaces: live_surfaces.min(u32::MAX as usize) as u32,
                                    attached_surfaces: attached_surfaces.min(u32::MAX as usize)
                                        as u32,
                                },
                            },
                        })?;
                    }
                    Err(error) => send_actor_error(&sink, request_id, &error),
                },
                ClientMessage::Mutate { mutation } => match manager.mutate(mutation) {
                    Ok(receipt) => {
                        send_workspace_events(&sink, &mut workspace_events, &manager)?;
                        sink.send_control(&ServerControl {
                            request_id: Some(request_id),
                            message: ServerMessage::MutationCommitted { receipt },
                        })?;
                    }
                    Err(error) => send_actor_error(&sink, request_id, &error),
                },
                ClientMessage::MutationOutcome { mutation_id } => {
                    match manager.outcome(mutation_id) {
                        Ok(receipt) => sink.send_control(&ServerControl {
                            request_id: Some(request_id),
                            message: ServerMessage::MutationOutcome { receipt },
                        })?,
                        Err(error) => send_actor_error(&sink, request_id, &error),
                    }
                }
                ClientMessage::Attach {
                    surface_id,
                    expected_lifetime,
                    cols,
                    rows,
                } => {
                    if attached.is_some() {
                        send_error(
                            &sink,
                            Some(request_id),
                            ErrorCode::AlreadyAttached,
                            "connection is already attached to a surface",
                        );
                        continue;
                    }
                    let Some(surface) = manager.get(&surface_id) else {
                        send_unavailable_surface(&sink, request_id, &manager, &surface_id);
                        continue;
                    };
                    match surface.attach(sink.clone(), request_id, &expected_lifetime, cols, rows) {
                        Ok(()) => attached = Some(surface),
                        Err(error) => send_surface_error(&sink, request_id, &error),
                    }
                }
                ClientMessage::Detach => {
                    let Some((surface, target)) =
                        attached_target(&attached, target.as_ref(), &sink, request_id)
                    else {
                        continue;
                    };
                    match surface.detach(target, request_id) {
                        Ok(()) => attached = None,
                        Err(error) => send_surface_error(&sink, request_id, &error),
                    }
                }
                ClientMessage::Input { data, latency_id } => {
                    let Some((surface, target)) =
                        attached_target(&attached, target.as_ref(), &sink, request_id)
                    else {
                        continue;
                    };
                    match surface.write_input(target, &data, latency_id) {
                        Ok(()) => sink.send_control(&ServerControl {
                            request_id: Some(request_id),
                            message: ServerMessage::InputAccepted,
                        })?,
                        Err(error) => send_surface_error(&sink, request_id, &error),
                    }
                }
                ClientMessage::Resize { cols, rows } => {
                    let Some((surface, target)) =
                        attached_target(&attached, target.as_ref(), &sink, request_id)
                    else {
                        continue;
                    };
                    match surface.resize(target, cols, rows) {
                        Ok(()) => sink.send_control(&ServerControl {
                            request_id: Some(request_id),
                            message: ServerMessage::Resized { cols, rows },
                        })?,
                        Err(error) => send_surface_error(&sink, request_id, &error),
                    }
                }
                ClientMessage::RequestSnapshot => {
                    let Some((surface, target)) =
                        attached_target(&attached, target.as_ref(), &sink, request_id)
                    else {
                        continue;
                    };
                    if let Err(error) = surface.request_snapshot(target, request_id) {
                        send_surface_error(&sink, request_id, &error);
                    }
                }
                ClientMessage::ClearScrollback => {
                    let Some((surface, target)) =
                        attached_target(&attached, target.as_ref(), &sink, request_id)
                    else {
                        continue;
                    };
                    if let Err(error) = surface.clear_scrollback(target, request_id) {
                        send_surface_error(&sink, request_id, &error);
                    }
                }
                ClientMessage::BeginImageUpload {
                    name,
                    byte_len,
                    sha256,
                } => {
                    if uploads.len() >= MAX_CONNECTION_UPLOADS {
                        send_error(
                            &sink,
                            Some(request_id),
                            ErrorCode::Busy,
                            "too many image uploads are active on this connection",
                        );
                        continue;
                    }
                    match begin_image_upload(&name, byte_len, sha256) {
                        Ok((upload_id, upload)) => {
                            uploads.insert(upload_id.clone(), upload);
                            sink.send_control(&ServerControl {
                                request_id: Some(request_id),
                                message: ServerMessage::ImageUploadStarted { upload_id },
                            })?;
                        }
                        Err(error) => send_error(
                            &sink,
                            Some(request_id),
                            ErrorCode::InvalidRequest,
                            &error.to_string(),
                        ),
                    }
                }
                ClientMessage::UploadImageChunk {
                    upload_id,
                    offset,
                    data,
                } => {
                    let result = uploads
                        .get_mut(&upload_id)
                        .ok_or_else(|| "image upload does not exist".into())
                        .and_then(|upload| append_image_upload(upload, offset, &data));
                    match result {
                        Ok(next_offset) => sink.send_control(&ServerControl {
                            request_id: Some(request_id),
                            message: ServerMessage::ImageUploadProgress {
                                upload_id,
                                next_offset,
                            },
                        })?,
                        Err(error) => {
                            uploads.remove(&upload_id);
                            send_error(
                                &sink,
                                Some(request_id),
                                ErrorCode::InvalidRequest,
                                &error.to_string(),
                            );
                        }
                    }
                }
                ClientMessage::FinishImageUpload { upload_id } => {
                    let result = uploads
                        .remove(&upload_id)
                        .ok_or_else(|| "image upload does not exist".into())
                        .and_then(finish_image_upload);
                    match result {
                        Ok(path) => sink.send_control(&ServerControl {
                            request_id: Some(request_id),
                            message: ServerMessage::ImageUploaded { path },
                        })?,
                        Err(error) => send_error(
                            &sink,
                            Some(request_id),
                            ErrorCode::InvalidRequest,
                            &error.to_string(),
                        ),
                    }
                }
                ClientMessage::ShutdownDaemon => {
                    sink.send_control_sync(&ServerControl {
                        request_id: Some(request_id),
                        message: ServerMessage::DaemonStopping,
                    })?;
                    thread::sleep(Duration::from_millis(25));
                    stopping.store(true, Ordering::Release);
                    let _ = pipe::connect(wake_pipe, Duration::from_millis(250));
                    break;
                }
                ClientMessage::Hello { .. } => unreachable!(),
            }
        }
        Ok(())
    })();

    if let Some(session) = attached {
        session.detach_connection(sink.id());
    }
    result
}

fn read_next(
    reader: &mut pipe::PipeReader,
    connection: &File,
    stopping: &AtomicBool,
) -> Result<Option<frame::Frame>> {
    while !stopping.load(Ordering::Acquire) {
        match reader.poll(connection)? {
            Some(message) => return Ok(Some(message)),
            None => thread::sleep(Duration::from_millis(5)),
        }
    }
    Ok(None)
}

fn send_workspace_events(
    sink: &ConnectionSink,
    events: &mut std::sync::mpsc::Receiver<crate::workspace::WorkspaceEvent>,
    manager: &SurfaceManager,
) -> Result<()> {
    loop {
        match events.try_recv() {
            Ok(crate::workspace::WorkspaceEvent::Revision(revision)) => {
                sink.send_control(&ServerControl {
                    request_id: None,
                    message: ServerMessage::WorkspaceChanged { revision },
                })?;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(()),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                let revision = manager.snapshot()?.revision;
                sink.send_control(&ServerControl {
                    request_id: None,
                    message: ServerMessage::WorkspaceChanged { revision },
                })?;
                *events = manager.subscribe();
                return Ok(());
            }
        }
    }
}

fn attached_target<'a>(
    attached: &'a Option<Arc<Surface>>,
    target: Option<&'a TerminalTarget>,
    sink: &ConnectionSink,
    request_id: u64,
) -> Option<(&'a Arc<Surface>, &'a TerminalTarget)> {
    let Some(surface) = attached.as_ref() else {
        send_error(
            sink,
            Some(request_id),
            ErrorCode::NotAttached,
            "connection is not attached",
        );
        return None;
    };
    let Some(target) = target else {
        send_error(
            sink,
            Some(request_id),
            ErrorCode::InvalidRequest,
            "terminal operation requires an attachment and process-lifetime target",
        );
        return None;
    };
    Some((surface, target))
}

fn send_unavailable_surface(
    sink: &ConnectionSink,
    request_id: u64,
    manager: &SurfaceManager,
    surface_id: &SurfaceId,
) {
    if let Some(surface) = manager.get_info(surface_id) {
        let message = surface
            .error
            .unwrap_or_else(|| format!("surface is {:?}", surface.status).to_lowercase());
        send_error(
            sink,
            Some(request_id),
            ErrorCode::SurfaceUnavailable,
            &message,
        );
    } else {
        send_error(
            sink,
            Some(request_id),
            ErrorCode::SurfaceNotFound,
            "surface was not found",
        );
    }
}

fn send_surface_error(sink: &ConnectionSink, request_id: u64, error: &SurfaceError) {
    send_error(sink, Some(request_id), error.code(), &error.to_string());
}

fn send_actor_error(sink: &ConnectionSink, request_id: u64, error: &crate::workspace::ActorError) {
    let _ = sink.send_control(&ServerControl {
        request_id: Some(request_id),
        message: ServerMessage::Error {
            code: error.code,
            message: error.message.clone(),
            current_revision: error.current_revision,
        },
    });
}

fn send_error_sync(sink: &ConnectionSink, request_id: Option<u64>, code: ErrorCode, message: &str) {
    let _ = sink.send_control_sync(&ServerControl {
        request_id,
        message: ServerMessage::Error {
            code,
            message: message.into(),
            current_revision: None,
        },
    });
}

fn send_error(sink: &ConnectionSink, request_id: Option<u64>, code: ErrorCode, message: &str) {
    let _ = sink.send_control(&ServerControl {
        request_id,
        message: ServerMessage::Error {
            code,
            message: message.into(),
            current_revision: None,
        },
    });
}

#[cfg(windows)]
struct DaemonSingleton {
    handle: OwnedHandle,
}

#[cfg(windows)]
impl DaemonSingleton {
    fn acquire(name: &str) -> Result<Self> {
        let name = wide(name);
        let handle = unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr()))? };
        let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            return Err("a Compi daemon instance is already running".into());
        }
        Ok(Self { handle })
    }
}

#[cfg(windows)]
impl Drop for DaemonSingleton {
    fn drop(&mut self) {
        let _ = unsafe { ReleaseMutex(HANDLE(self.handle.as_raw_handle())) };
    }
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(once(0)).collect()
}

#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

#[cfg(unix)]
struct DaemonSingleton {
    _file: File,
}

#[cfg(unix)]
impl DaemonSingleton {
    fn acquire(name: &str) -> Result<Self> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
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
            return Err("unsafe daemon lock file ownership or permissions".into());
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(format!(
                "cannot acquire daemon instance lock: {}",
                std::io::Error::last_os_error()
            )
            .into());
        }
        // Do not unlink: contenders must always lock the same inode.
        Ok(Self { _file: file })
    }
}
