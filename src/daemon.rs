use crate::Result;
use crate::frame;
use crate::identity::{self, InstanceNames, PipeSecurity};
use crate::pipe;
use crate::protocol::{
    CONTROL_FRAME, ClientMessage, ErrorCode, PROTOCOL_VERSION, ServerControl, ServerMessage,
    decode_client,
};
use crate::session::{ConnectionSink, Session, SessionError, SessionManager};
use crate::wsl;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::iter::once;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex};
use windows::core::PCWSTR;

pub fn run(instance: Option<&str>) -> Result<()> {
    let names = identity::instance_names(instance)?;
    let _singleton = DaemonSingleton::acquire(&names.mutex)?;
    let manager = Arc::new(SessionManager::persistent(instance)?);
    wsl::ensure_default_wsl2()?;
    let security = PipeSecurity::for_current_user()?;
    let stopping = Arc::new(AtomicBool::new(false));
    if crate::perf::enabled() {
        let sampler_manager = manager.clone();
        let sampler_stopping = stopping.clone();
        thread::spawn(move || {
            while !sampler_stopping.load(Ordering::Acquire) {
                thread::sleep(Duration::from_secs(6));
                if sampler_stopping.load(Ordering::Acquire) {
                    break;
                }
                crate::perf::log_resource_sample(
                    "daemon",
                    "server",
                    sampler_manager.session_count(),
                );
            }
        });
    }
    let connections = Arc::new(Mutex::new(HashMap::<u64, ConnectionSink>::new()));
    let handlers = Arc::new(Mutex::new(Vec::<JoinHandle<()>>::new()));
    let connection_ids = AtomicU64::new(1);

    let result = serve(
        &names,
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
        Ok(()) => "session ended because the daemon stopped intentionally".to_owned(),
        Err(error) => format!("session ended because the daemon failed: {error}"),
    };
    manager.shutdown_all(&shutdown_reason);
    result
}

fn serve(
    names: &InstanceNames,
    security: &PipeSecurity,
    manager: Arc<SessionManager>,
    stopping: Arc<AtomicBool>,
    connections: Arc<Mutex<HashMap<u64, ConnectionSink>>>,
    handlers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    connection_ids: &AtomicU64,
) -> Result<()> {
    let mut server = pipe::create_server(&names.pipe, security, true)?;
    while !stopping.load(Ordering::Acquire) {
        pipe::accept(&server)?;
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
        handlers
            .lock()
            .map_err(|_| "handler registry lock was poisoned")?
            .push(handler);

        if stopping.load(Ordering::Acquire) {
            break;
        }
        server = pipe::create_server(&names.pipe, security, false)?;
    }
    Ok(())
}

fn handle_connection(
    connection: Arc<File>,
    sink: ConnectionSink,
    manager: Arc<SessionManager>,
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

    let mut attached: Option<Arc<Session>> = None;
    while !stopping.load(Ordering::Acquire) && sink.is_alive() {
        let Some(incoming) = read_next(&mut reader, &connection, &stopping)? else {
            break;
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

        match request.message {
            ClientMessage::ListSessions => {
                sink.send_control(&ServerControl {
                    request_id: Some(request.request_id),
                    message: ServerMessage::Sessions {
                        sessions: manager.list(),
                    },
                })?;
            }
            ClientMessage::CreateSession { cols, rows } => match manager.create(cols, rows) {
                Ok(session) => sink.send_control(&ServerControl {
                    request_id: Some(request.request_id),
                    message: ServerMessage::SessionCreated {
                        session: session.info(),
                    },
                })?,
                Err(error) => send_error(
                    &sink,
                    Some(request.request_id),
                    ErrorCode::Internal,
                    &error.to_string(),
                ),
            },
            ClientMessage::Attach {
                session_id,
                cols,
                rows,
            } => {
                if attached.is_some() {
                    send_error(
                        &sink,
                        Some(request.request_id),
                        ErrorCode::AlreadyAttached,
                        "connection is already attached to a session",
                    );
                    continue;
                }
                let Some(session) = manager.get(&session_id) else {
                    send_unavailable_session(&sink, request.request_id, &manager, &session_id);
                    continue;
                };
                match session.attach(sink.clone(), request.request_id, cols, rows) {
                    Ok(()) => attached = Some(session),
                    Err(error) => send_session_error(&sink, request.request_id, &error),
                }
            }
            ClientMessage::Detach => {
                let Some(session) = attached.as_ref() else {
                    send_error(
                        &sink,
                        Some(request.request_id),
                        ErrorCode::NotAttached,
                        "connection is not attached",
                    );
                    continue;
                };
                match session.detach(sink.id(), request.request_id) {
                    Ok(()) => attached = None,
                    Err(error) => send_session_error(&sink, request.request_id, &error),
                }
            }
            ClientMessage::Input { data } => {
                let Some(session) = attached.as_ref() else {
                    send_error(
                        &sink,
                        Some(request.request_id),
                        ErrorCode::NotAttached,
                        "connection is not attached",
                    );
                    continue;
                };
                match session.write_input(sink.id(), &data) {
                    Ok(()) => sink.send_control(&ServerControl {
                        request_id: Some(request.request_id),
                        message: ServerMessage::InputAccepted,
                    })?,
                    Err(error) => send_session_error(&sink, request.request_id, &error),
                }
            }
            ClientMessage::Resize { cols, rows } => {
                let Some(session) = attached.as_ref() else {
                    send_error(
                        &sink,
                        Some(request.request_id),
                        ErrorCode::NotAttached,
                        "connection is not attached",
                    );
                    continue;
                };
                match session.resize(sink.id(), cols, rows) {
                    Ok(()) => sink.send_control(&ServerControl {
                        request_id: Some(request.request_id),
                        message: ServerMessage::Resized { cols, rows },
                    })?,
                    Err(error) => send_session_error(&sink, request.request_id, &error),
                }
            }
            ClientMessage::RequestSnapshot => {
                let Some(session) = attached.as_ref() else {
                    send_error(
                        &sink,
                        Some(request.request_id),
                        ErrorCode::NotAttached,
                        "connection is not attached",
                    );
                    continue;
                };
                if let Err(error) = session.request_snapshot(sink.id(), request.request_id) {
                    send_session_error(&sink, request.request_id, &error);
                }
            }
            ClientMessage::Kill { session_id } => {
                let Some(session) = manager.get(&session_id) else {
                    send_unavailable_session(&sink, request.request_id, &manager, &session_id);
                    continue;
                };
                match session.kill() {
                    Ok(()) => sink.send_control(&ServerControl {
                        request_id: Some(request.request_id),
                        message: ServerMessage::KillRequested { session_id },
                    })?,
                    Err(error) => send_session_error(&sink, request.request_id, &error),
                }
            }
            ClientMessage::ShutdownDaemon => {
                sink.send_control_sync(&ServerControl {
                    request_id: Some(request.request_id),
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

    if let Some(session) = attached {
        session.detach_connection(sink.id());
    }
    Ok(())
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

fn send_unavailable_session(
    sink: &ConnectionSink,
    request_id: u64,
    manager: &SessionManager,
    session_id: &str,
) {
    if let Some(session) = manager.get_info(session_id) {
        let message = session
            .error
            .unwrap_or_else(|| format!("session is {:?}", session.status).to_lowercase());
        send_error(sink, Some(request_id), ErrorCode::SessionExited, &message);
    } else {
        send_error(
            sink,
            Some(request_id),
            ErrorCode::SessionNotFound,
            "session was not found",
        );
    }
}

fn send_session_error(sink: &ConnectionSink, request_id: u64, error: &SessionError) {
    send_error(sink, Some(request_id), error.code(), &error.to_string());
}

fn send_error_sync(sink: &ConnectionSink, request_id: Option<u64>, code: ErrorCode, message: &str) {
    let _ = sink.send_control_sync(&ServerControl {
        request_id,
        message: ServerMessage::Error {
            code,
            message: message.into(),
        },
    });
}

fn send_error(sink: &ConnectionSink, request_id: Option<u64>, code: ErrorCode, message: &str) {
    let _ = sink.send_control(&ServerControl {
        request_id,
        message: ServerMessage::Error {
            code,
            message: message.into(),
        },
    });
}

struct DaemonSingleton {
    handle: OwnedHandle,
}

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

impl Drop for DaemonSingleton {
    fn drop(&mut self) {
        let _ = unsafe { ReleaseMutex(HANDLE(self.handle.as_raw_handle())) };
    }
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(once(0)).collect()
}

use std::os::windows::io::AsRawHandle;
