use crate::Result;
use crate::launch;
use crate::surface::{ConnectionSink, Surface, SurfaceError, SurfaceManager};
use compi_protocol::frame;
#[cfg(windows)]
use compi_protocol::identity::PipeSecurity;
use compi_protocol::identity::{self, InstanceNames};
use compi_protocol::pipe;
use compi_protocol::{
    CONTROL_FRAME, ClientMessage, ErrorCode, PROTOCOL_VERSION, ServerControl, ServerMessage,
    SurfaceId, TerminalTarget, decode_client,
};
use std::collections::HashMap;
#[cfg(windows)]
use std::ffi::OsStr;
use std::fs::File;
#[cfg(windows)]
use std::iter::once;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
#[cfg(windows)]
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
#[cfg(windows)]
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex};
#[cfg(windows)]
use windows::core::PCWSTR;

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
