//! Bounded client worker and Unix endpoint helpers.

use super::{Inbound, ListenerError};
use crate::protocol::{FrameLimits, ProtocolError, read_frame, write_frame};
use std::io::{self, Read as _};
use std::os::unix::fs::FileTypeExt;
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
    pub(super) inflight: Arc<AtomicUsize>,
    pub(super) streams:
        Arc<Mutex<std::collections::BTreeMap<usize, std::os::unix::net::UnixStream>>>,
    pub(super) connection_id: usize,
    pub(super) limits: FrameLimits,
    pub(super) timeout: Duration,
    pub(super) request_idle: Duration,
    pub(super) owner_reply: Duration,
}

pub(super) fn connection_worker(
    mut stream: std::os::unix::net::UnixStream,
    context: ConnectionContext,
) {
    let ConnectionContext {
        sender,
        stop,
        active,
        inflight,
        streams,
        connection_id,
        limits,
        timeout,
        request_idle,
        owner_reply,
    } = context;
    let mut frames = 0usize;
    while !stop.load(Ordering::Acquire) && frames < limits.max_frames_per_connection {
        // Waiting for the next request is not the same as reading a frame
        // slowly. Charging the per-frame deadline for the wait closed every
        // connection that went one `timeout` without a request, which is
        // what a thinking agent does between tool calls; the client then saw
        // a reset on its next call and had no way back.
        let Some(first) = await_frame_start(&mut stream, request_idle, timeout) else {
            break;
        };
        // The byte that proved the frame started is the frame's first byte,
        // so hand it back to the reader rather than losing it.
        let Ok(payload) = read_frame(&mut io::Cursor::new([first]).chain(&mut stream), limits)
        else {
            break;
        };
        inflight.fetch_add(1, Ordering::AcqRel);
        let Some(response) = serve_one_frame(
            &mut stream,
            payload,
            &sender,
            ServeWindows {
                handoff: timeout,
                owner_reply,
            },
            limits,
        ) else {
            inflight.fetch_sub(1, Ordering::AcqRel);
            break;
        };
        let write_result = write_frame(&mut stream, &response, limits);
        inflight.fetch_sub(1, Ordering::AcqRel);
        if write_result.is_err() {
            break;
        }
        frames = frames.saturating_add(1);
    }
    if let Ok(mut active_streams) = streams.lock() {
        active_streams.remove(&connection_id);
    }
    active.fetch_sub(1, Ordering::AcqRel);
}

/// The two windows one served frame is charged against.
#[derive(Clone, Copy)]
struct ServeWindows {
    /// How long the owner channel may stay saturated before the client is
    /// told so.
    handoff: Duration,
    /// How long the owner itself may take to answer.
    owner_reply: Duration,
}

/// Hands one payload to the owner loop and returns its response.
///
/// `None` means this connection is finished: either the client was told why
/// (backpressure, a protocol fault, or an owner that ran past its window) or
/// the owner is gone. Every refusal is written before returning.
fn serve_one_frame(
    stream: &mut std::os::unix::net::UnixStream,
    payload: Vec<u8>,
    sender: &SyncSender<Inbound>,
    windows: ServeWindows,
    limits: FrameLimits,
) -> Option<Vec<u8>> {
    let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
    let correlation = crate::service::RequestCorrelation::from_payload(&payload);
    let inbound = Inbound {
        payload,
        reply: reply_sender,
    };
    let refuse = |stream: &mut std::os::unix::net::UnixStream, error: &ProtocolError| {
        let _ = write_frame(
            stream,
            &crate::service::error_payload(correlation, error, limits),
            limits,
        );
        None
    };
    if !hand_off(stream, inbound, sender, windows.handoff, limits) {
        return None;
    }
    // The owner has the request now. Waiting for it to finish indexing a
    // project is not an I/O deadline, so it gets its own window: the
    // per-frame one is far shorter than a real index and made the surface
    // report a timeout for work the owner then completed anyway.
    match reply_receiver.recv_timeout(windows.owner_reply) {
        Ok(Ok(response)) => Some(response),
        // The owner loop has already validated and, when possible, emitted a
        // correlated response. On a hard protocol fault this connection
        // closes rather than desynchronizing later frames.
        Ok(Err(error)) => refuse(stream, &error),
        Err(mpsc::RecvTimeoutError::Timeout) => refuse(stream, &ProtocolError::Timeout),
        Err(mpsc::RecvTimeoutError::Disconnected) => None,
    }
}

/// Offers one request to the single owner loop, returning whether it landed.
///
/// A client must never be able to park a thread forever while the owner loop
/// is stopped or saturated. The listener normally drains this channel
/// promptly; the deadline turns saturation into an explicit backpressure
/// response rather than a silent wait.
fn hand_off(
    stream: &mut std::os::unix::net::UnixStream,
    mut inbound: Inbound,
    sender: &SyncSender<Inbound>,
    handoff: Duration,
    limits: FrameLimits,
) -> bool {
    let correlation = crate::service::RequestCorrelation::from_payload(&inbound.payload);
    let deadline = Instant::now() + handoff;
    loop {
        match sender.try_send(inbound) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(returned)) => {
                inbound = returned;
                if Instant::now() >= deadline {
                    let _ = write_frame(
                        stream,
                        &crate::service::error_payload(
                            correlation,
                            &ProtocolError::Backpressure,
                            limits,
                        ),
                        limits,
                    );
                    return false;
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

/// Waits for a client to begin its next frame, then arms the frame deadline.
///
/// Returns the frame's first byte, which the caller must feed back to the
/// frame reader.  The wait is bounded by `request_idle`, so an abandoned
/// connection still releases its client slot; once the first byte has arrived
/// the rest of the frame must land inside `frame_timeout`, which is what stops
/// a peer trickling one frame forever.
fn await_frame_start(
    stream: &mut std::os::unix::net::UnixStream,
    request_idle: Duration,
    frame_timeout: Duration,
) -> Option<u8> {
    stream.set_read_timeout(Some(request_idle)).ok()?;
    let mut first = [0_u8; 1];
    let started = stream.read_exact(&mut first).is_ok();
    // Re-arm the frame deadline before reading the rest of the frame: the
    // remainder of a started frame is charged the per-frame window, never the
    // idle one.
    stream.set_read_timeout(Some(frame_timeout)).ok()?;
    started
        .then_some(first)
        .and_then(|bytes| bytes.first().copied())
}

pub(super) fn configure_stream(
    stream: &std::os::unix::net::UnixStream,
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
    if !metadata.file_type().is_socket() {
        return Err(ListenerError::EndpointOccupied);
    }
    match std::os::unix::net::UnixStream::connect(path) {
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

pub(super) fn set_private_socket_permissions(path: &Path) -> Result<(), ListenerError> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .map_err(|error| ListenerError::Io(error.kind()))?
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions).map_err(|error| ListenerError::Io(error.kind()))
}
