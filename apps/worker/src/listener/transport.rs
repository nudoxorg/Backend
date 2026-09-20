//! Socket and endpoint helpers for worker listeners.

use super::WorkerListenerError;
use std::io;
use std::net::TcpStream;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

#[cfg(any(unix, windows))]
pub(super) fn configure_stream(
    stream: &backend_platform::LocalStream,
    timeout: Duration,
) -> Result<(), WorkerListenerError> {
    stream
        // The listener itself is nonblocking so accept can be polled, but an
        // accepted stream must wait for the peer's next bounded frame. Keep
        // the stream blocking with explicit read/write deadlines; otherwise
        // the first read can observe `WouldBlock` during a normal connect
        // race and incorrectly abort a healthy worker session.
        .set_nonblocking(false)
        .and_then(|()| stream.set_read_timeout(Some(timeout)))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| WorkerListenerError::Io(error.kind()))
}

pub(super) fn configure_tcp_stream(
    stream: &TcpStream,
    timeout: Duration,
) -> Result<(), WorkerListenerError> {
    stream
        .set_nonblocking(false)
        .and_then(|()| stream.set_read_timeout(Some(timeout)))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| WorkerListenerError::Io(error.kind()))
}

pub(super) fn set_active_tcp_stream(active_stream: &Mutex<Option<TcpStream>>, stream: &TcpStream) {
    if let Ok(clone) = stream.try_clone()
        && let Ok(mut active) = active_stream.lock()
    {
        *active = Some(clone);
    }
}

pub(super) fn clear_active_tcp_stream(active_stream: &Mutex<Option<TcpStream>>) {
    if let Ok(mut active) = active_stream.lock() {
        *active = None;
    }
}

#[cfg(any(unix, windows))]
pub(super) fn set_active_stream(
    active_stream: &Mutex<Option<backend_platform::LocalStream>>,
    stream: &backend_platform::LocalStream,
) {
    if let Ok(clone) = stream.try_clone()
        && let Ok(mut active) = active_stream.lock()
    {
        *active = Some(clone);
    }
}

#[cfg(any(unix, windows))]
pub(super) fn clear_active_stream(active_stream: &Mutex<Option<backend_platform::LocalStream>>) {
    if let Ok(mut active) = active_stream.lock() {
        *active = None;
    }
}

#[cfg(any(unix, windows))]
pub(super) fn prepare_socket_path(path: &Path) -> Result<(), WorkerListenerError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|error| WorkerListenerError::Io(error.kind()))?;
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(WorkerListenerError::Io(error.kind())),
    };
    if !is_endpoint_file(&metadata) {
        return Err(WorkerListenerError::EndpointOccupied);
    }
    match backend_platform::LocalStream::connect(path) {
        Ok(_) => Err(WorkerListenerError::AlreadyRunning),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::NotFound
                    | io::ErrorKind::ConnectionReset
            ) =>
        {
            std::fs::remove_file(path).map_err(|remove| WorkerListenerError::Io(remove.kind()))
        }
        Err(error) => Err(WorkerListenerError::Io(error.kind())),
    }
}

#[cfg(unix)]
pub(super) fn set_private_socket_permissions(path: &Path) -> Result<(), WorkerListenerError> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .map_err(|error| WorkerListenerError::Io(error.kind()))?
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions)
        .map_err(|error| WorkerListenerError::Io(error.kind()))
}

/// Restricts the endpoint to its owner with a protected DACL, the Windows
/// counterpart of mode `0600`.
#[cfg(windows)]
pub(super) fn set_private_socket_permissions(path: &Path) -> Result<(), WorkerListenerError> {
    backend_platform::win32::security::restrict_to_current_user(path)
        .map_err(|error| WorkerListenerError::Io(error.kind()))
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
