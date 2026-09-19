use crate::protocol::{EngineStatus, FrameLimits, ProtocolError};
use backend_engine::{DaemonReply, QueueError};
use std::fmt;
use std::sync::mpsc::{Receiver, RecvError, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

const OWNER_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) fn daemon_replicate<M, V, A>(
    daemon: &mut crate::Locald<M, V, A>,
    request_id: u64,
    message: backend_engine::TransportMessage,
) -> Result<EngineStatus, ProtocolError>
where
    M: backend_engine::WorkspaceModel,
    M::Intent: backend_engine::QueueSized,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    let receiver = daemon
        .client()
        .request(request_id, crate::Request::Replicate(Box::new(message)))
        .map_err(|error| map_queue_error(&error))?;
    if !daemon.serve_one() {
        return Err(ProtocolError::Closed);
    }
    match wait_for_daemon_reply(daemon, &receiver)? {
        DaemonReply::Replicated(Ok(backend_engine::ReplicationReply::Queued { bytes })) => {
            Ok(EngineStatus::Queued { bytes })
        }
        DaemonReply::Replicated(Ok(_)) => Ok(EngineStatus::Accepted),
        DaemonReply::Replicated(Err(error)) => Ok(EngineStatus::Rejected(error.to_string())),
        _ => Err(ProtocolError::Closed),
    }
}

pub(crate) fn wait_for_daemon_reply<M, V, A>(
    daemon: &mut crate::Locald<M, V, A>,
    receiver: &Receiver<DaemonReply>,
) -> Result<DaemonReply, ProtocolError>
where
    M: backend_engine::WorkspaceModel,
    M::Intent: backend_engine::QueueSized,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    let deadline = Instant::now() + OWNER_REPLY_TIMEOUT;
    loop {
        match receiver.try_recv() {
            Ok(reply) => return Ok(reply),
            Err(TryRecvError::Disconnected) => return Err(ProtocolError::Closed),
            Err(TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            return Err(ProtocolError::Timeout);
        }
        if !daemon.serve_one() {
            thread::sleep(Duration::from_millis(1));
        }
    }
}

/// Converts the daemon's typed subscription result into the versioned local
/// control payload.  A successful empty suffix is still represented as an
/// explicit `events` document with its current cursor; returning bare
/// `Accepted` would make a standalone desktop client unable to tell an empty
/// suffix from a dropped event or reset.
pub(crate) fn map_queue_error(error: &crate::LocaldError) -> ProtocolError {
    match error {
        crate::LocaldError::Queue(QueueError::Count | QueueError::Bytes) => {
            ProtocolError::Backpressure
        }
        crate::LocaldError::Queue(QueueError::Closed)
        | crate::LocaldError::Workspace(_)
        | crate::LocaldError::Daemon(_) => ProtocolError::Closed,
        crate::LocaldError::Queue(QueueError::Accounting) => ProtocolError::InvalidLimits,
    }
}

/// The command error shape shared by the CLI and MCP adapters. This fallback
/// is emitted only when no request DTO can safely be recovered.
pub(crate) fn command_error_payload(
    request_id: u64,
    message: &str,
    limits: FrameLimits,
) -> Vec<u8> {
    let encode = |message: &str| {
        backend_engine::encode_reply_dto(&backend_engine::ReplyDto::error(request_id, message))
            .ok()
            .filter(|encoded| encoded.len() <= limits.max_frame)
    };
    if let Some(output) = encode(message) {
        return output;
    }
    // A diagnostic is optional. Preserve the one shared ReplyDto grammar
    // even when the configured frame cannot retain the original message.
    encode("").unwrap_or_default()
}

/// Protocol family and correlation identity recovered before full admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RequestCorrelation {
    Command(u64),
    Engine(u64),
    Unknown,
}

impl RequestCorrelation {
    pub(crate) fn from_payload(payload: &[u8]) -> Self {
        if backend_engine::is_local_control(payload) {
            backend_engine::local_control_request_id(payload).map_or(Self::Unknown, Self::Engine)
        } else {
            backend_engine::command_request_id(payload).map_or(Self::Unknown, Self::Command)
        }
    }

    pub(crate) const fn request_id(self) -> Option<u64> {
        match self {
            Self::Command(request_id) | Self::Engine(request_id) => Some(request_id),
            Self::Unknown => None,
        }
    }
}

pub(crate) fn error_payload(
    correlation: RequestCorrelation,
    error: &ProtocolError,
    limits: FrameLimits,
) -> Vec<u8> {
    match correlation {
        RequestCorrelation::Engine(request_id) if request_id != 0 => {
            crate::protocol::encode_response(
                &crate::protocol::ResponseFrame::Engine {
                    request_id,
                    status: EngineStatus::Rejected(error.to_string()),
                },
                limits,
            )
            .unwrap_or_default()
        }
        RequestCorrelation::Command(request_id) => {
            command_error_payload(request_id, &error.to_string(), limits)
        }
        RequestCorrelation::Engine(_) | RequestCorrelation::Unknown => {
            command_error_payload(0, &error.to_string(), limits)
        }
    }
}

/// Errors returned by a stream service operation.
#[derive(Debug)]
pub enum ServiceError {
    /// Local frame, owner, or transport protocol failure.
    Protocol(ProtocolError),
    /// The owner service itself failed while producing a response.
    Owner(String),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(error) => error.fmt(formatter),
            Self::Owner(error) => write!(formatter, "owner service failed: {error}"),
        }
    }
}

impl std::error::Error for ServiceError {}

impl From<RecvError> for ServiceError {
    fn from(_: RecvError) -> Self {
        Self::Protocol(ProtocolError::Closed)
    }
}
