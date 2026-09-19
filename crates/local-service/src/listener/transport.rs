//! Bounded client worker and Unix endpoint helpers.

use super::{Inbound, ListenerError};
use crate::protocol::{FrameLimits, ProtocolError, read_frame, write_frame};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

pub(super) struct ConnectionContext {
    pub(super) sender: SyncSender<Inbound>,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) active: Arc<AtomicUsize>,
    pub(super) streams:
        Arc<Mutex<std::collections::BTreeMap<usize, backend_engine::LocalStream>>>,
    pub(super) connection_id: usize,
    pub(super) limits: FrameLimits,
    pub(super) timeout: Duration,
}

pub(super) fn connection_worker(
    mut stream: backend_engine::LocalStream,
    context: ConnectionContext,
) {
    let ConnectionContext {
        sender,
        stop,
        active,
        streams,
        connection_id,
        limits,
        timeout,
    } = context;
    let mut frames = 0usize;
    while !stop.load(Ordering::Acquire) && frames < limits.max_frames_per_connection {
        let Ok(payload) = read_frame(&mut stream, limits) else {
            break;
        };
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        let correlation = crate::service::RequestCorrelation::from_payload(&payload);
        let mut inbound = Inbound {
            payload,
            reply: reply_sender,
        };
        // A client must never be able to park a thread forever while the
        // owner loop is stopped or saturated. The listener normally drains
        // this channel promptly; the deadline turns saturation into an
        // explicit backpressure response.
        let handoff_deadline = Instant::now() + timeout;
        let handed_off = loop {
            match sender.try_send(inbound) {
                Ok(()) => break true,
                Err(TrySendError::Disconnected(_)) => break false,
                Err(TrySendError::Full(returned)) => {
                    inbound = returned;
                    if Instant::now() >= handoff_deadline {
                        let _ = write_frame(
                            &mut stream,
                            &crate::service::error_payload(
                                correlation,
                                &ProtocolError::Backpressure,
                                limits,
                            ),
                            limits,
                        );
                        break false;
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            }
        };
        if !handed_off {
            break;
        }
        let response = match reply_receiver.recv_timeout(timeout) {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                // The owner loop has already validated and, when possible,
                // emitted a correlated response. On a hard protocol fault we
                // close this connection to avoid desynchronizing later frames.
                let _ = write_frame(
                    &mut stream,
                    &crate::service::error_payload(correlation, &error, limits),
                    limits,
                );
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = write_frame(
                    &mut stream,
                    &crate::service::error_payload(correlation, &ProtocolError::Timeout, limits),
                    limits,
                );
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if write_frame(&mut stream, &response, limits).is_err() {
            break;
        }
        frames = frames.saturating_add(1);
    }
    if let Ok(mut active_streams) = streams.lock() {
        active_streams.remove(&connection_id);
    }
    active.fetch_sub(1, Ordering::AcqRel);
}

pub(super) fn configure_stream(
    stream: &backend_engine::LocalStream,
    timeout: Duration,
) -> Result<(), ListenerError> {
    stream
        // Accepted streams can inherit the listener's nonblocking status on
        // supported Unix hosts. The per-connection worker uses bounded
        // blocking I/O, so restore that contract before installing deadlines.
        .set_nonblocking(false)
        .and_then(|()| stream.set_read_timeout(Some(timeout)))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| ListenerError::Io(error.kind()))
}

pub(super) fn prepare_socket_path(path: &Path) -> Result<(), ListenerError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|error| ListenerError::Io(error.kind()))?;
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(ListenerError::Io(error.kind())),
    };
    if !is_endpoint_file(&metadata) {
        return Err(ListenerError::EndpointOccupied);
    }
    match backend_engine::LocalStream::connect(path) {
        Ok(_) => Err(ListenerError::AlreadyRunning),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::NotFound
                    | io::ErrorKind::ConnectionReset
            ) =>
        {
            std::fs::remove_file(path).map_err(|remove| ListenerError::Io(remove.kind()))
        }
        Err(error) => Err(ListenerError::Io(error.kind())),
    }
}

#[cfg(unix)]
pub(super) fn set_private_socket_permissions(path: &Path) -> Result<(), ListenerError> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .map_err(|error| ListenerError::Io(error.kind()))?
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions).map_err(|error| ListenerError::Io(error.kind()))
}

/// Restricts the endpoint to its owner with a protected DACL, the Windows
/// counterpart of mode `0600`.
#[cfg(windows)]
pub(super) fn set_private_socket_permissions(path: &Path) -> Result<(), ListenerError> {
    backend_platform::win32::security::restrict_to_current_user(path)
        .map_err(|error| ListenerError::Io(error.kind()))
}

#[cfg(unix)]
fn is_endpoint_file(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    metadata.file_type().is_socket()
}

#[cfg(windows)]
fn is_endpoint_file(metadata: &std::fs::Metadata) -> bool {
    backend_platform::win32::security::is_endpoint_metadata(metadata)
}
