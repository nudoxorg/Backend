//! The bounded locald wire boundary.
//!
//! locald intentionally uses one outer length-prefixed frame for every local
//! client.  The payload is either a library command DTO (the JSON wire used by
//! CLI and MCP) or an engine envelope.  Replication payloads inside an engine
//! envelope are encoded by `backend-replication` through the re-exports of
//! `backend-engine`; this module does not define a second replication codec.
//!
//! # The lifecycle envelope
//!
//! One operation is not engine state and not a library command: asking a
//! daemon to stop. It is carried by a separate fixed-width `LDL1` envelope
//! defined here rather than as a new `LDC2` control tag, because `LDC2` is the
//! shared replication grammar and a lifecycle verb has no meaning to a peer
//! that replicates. The envelope is
//! `LDL1 | version:u8 | tag:u8 | request_id:u64` — fourteen bytes, no
//! variable-length field, nothing to bound beyond its exact size. Its magic
//! cannot collide with `LDC2` control payloads or with JSON command DTOs.
//!
//! A shutdown request is answered by the listener's own stop capability (see
//! [`crate::listener`]), never by the workspace owner, and is acknowledged
//! with an ordinary [`EngineStatus::Accepted`] response.

use backend_engine::{
    LOCAL_CONTROL_HEADER_BYTES, LocalControlError, LocalControlLimits, LocalControlRequest,
    LocalControlResponse, LocalSubscriptionRequest, LocalSubscriptionResponse, ReplicationError,
    TransportLimits, TransportMessage,
};
use std::fmt;
use std::io::{self, Read, Write};

/// The maximum local command or control frame body.
pub const MAX_FRAME: usize = backend_engine::LOCAL_CONTROL_MAX_FRAME;
/// The maximum cursor bytes carried by a subscription request.
pub const MAX_CURSOR: usize = backend_engine::LOCAL_CONTROL_MAX_CURSOR;
/// The maximum error text returned over a local endpoint.
pub const MAX_ERROR: usize = backend_engine::LOCAL_CONTROL_MAX_ERROR;
/// The local control envelope magic.
pub const CONTROL_MAGIC: [u8; 4] = backend_engine::LOCAL_CONTROL_MAGIC;
/// The current local control envelope version.
pub const CONTROL_VERSION: u8 = backend_engine::LOCAL_CONTROL_VERSION;
/// The locald lifecycle envelope magic.
pub const LIFECYCLE_MAGIC: [u8; 4] = *b"LDL1";
/// The current locald lifecycle envelope version.
pub const LIFECYCLE_VERSION: u8 = 1;
/// Exact byte length of a lifecycle envelope.
pub const LIFECYCLE_BYTES: usize = 14;

const LIFECYCLE_TAG_SHUTDOWN: u8 = 1;

/// Limits applied before a local endpoint allocates or blocks on a payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameLimits {
    /// Maximum payload bytes in one outer frame.
    pub max_frame: usize,
    /// Maximum cursor bytes in one subscription request.
    pub max_cursor: usize,
    /// Maximum number of frames accepted on one connection.
    pub max_frames_per_connection: usize,
    /// Replication limits negotiated by the engine.
    pub transport: TransportLimits,
}

impl Default for FrameLimits {
    fn default() -> Self {
        let mut transport = TransportLimits::default();
        // The replication body is nested inside the local outer frame. Keep
        // its advertised frame/chunk bounds no larger than that envelope.
        transport.max_frame = MAX_FRAME;
        transport.max_chunk = transport.max_chunk.min(MAX_FRAME);
        Self {
            max_frame: MAX_FRAME,
            max_cursor: MAX_CURSOR,
            max_frames_per_connection: 256,
            transport,
        }
    }
}

impl FrameLimits {
    /// Checks that the local limits themselves are usable.
    ///
    /// # Errors
    ///
    /// Returns an error when a configured limit is zero or inconsistent.
    pub fn validate(self) -> Result<(), ProtocolError> {
        if self.max_frame == 0
            || self.max_frame > u32::MAX as usize
            || self.max_cursor > self.max_frame
            || self.max_frames_per_connection == 0
            || self.transport.max_frame > self.max_frame
        {
            return Err(ProtocolError::InvalidLimits);
        }
        LocalControlLimits {
            max_frame: self.max_frame,
            max_cursor: self.max_cursor,
            max_error: diagnostic_limit(self.max_frame),
        }
        .validate()
        .map_err(map_local_error)?;
        self.transport
            .validate()
            .map(|_| ())
            .map_err(ProtocolError::Replication)
    }
}

/// A request sent to the owner loop after framing admission.
#[derive(Clone, Debug)]
pub enum EngineRequest {
    /// A canonical replication/control message.
    Replicate(Box<TransportMessage>),
    /// An untrusted completion claim. The owner adapter must compare every
    /// field against its retained scheduler ticket before constructing an
    /// engine completion capability. A wire claim never becomes a
    /// `CompletionNotice` by itself.
    Complete(CompletionClaim),
    /// A bounded cursor subscription.
    Subscribe {
        /// Cursor bytes retained by the client.
        cursor: Box<[u8]>,
        /// Event credit reserved by the client.
        credit: usize,
    },
    /// A leased subscription lifecycle operation.
    Subscription(LocalSubscriptionRequest),
    /// A request that the daemon retire itself.
    ///
    /// This is a listener lifecycle operation: it stops accepting clients,
    /// drains the ones already connected, closes the workspace owner, and
    /// unlinks the endpoint. It never reaches the owner adapter and never
    /// mutates durable state.
    Shutdown,
}

/// Untrusted fields echoed by a completion sender.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompletionClaim {
    /// Work identity claimed by the sender.
    pub work_key: [u8; 32],
    /// Output identity claimed by the sender.
    pub output: [u8; 32],
    /// Winning attempt ordinal claimed by the sender.
    pub ordinal: u32,
    /// Scheduler fence claimed by the sender.
    pub fence: [u8; 32],
}

/// A framed local request. Command bytes are decoded only after the owner has
/// supplied the expected typed command; digest-only claims are never turned
/// into trusted identities by this module.
#[derive(Clone, Debug)]
pub enum RequestFrame {
    /// CLI/MCP/library command DTO bytes.
    Command(Box<[u8]>),
    /// Engine operation with an explicit request correlation identity.
    Engine {
        /// Correlation identity for the one-shot reply.
        request_id: u64,
        /// Owner-loop operation.
        request: Box<EngineRequest>,
    },
}

/// The operation class of a local request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    /// Library command DTO.
    Command,
    /// Replication/control message.
    Replicate,
    /// Completion admission.
    Complete,
    /// Cursor subscription.
    Subscribe,
    /// Daemon lifecycle control.
    Shutdown,
}

impl RequestFrame {
    /// Returns the operation class used for backpressure accounting.
    #[must_use]
    pub const fn operation(&self) -> Operation {
        match self {
            Self::Command(_) => Operation::Command,
            Self::Engine { request, .. } => match &**request {
                EngineRequest::Replicate(_) => Operation::Replicate,
                EngineRequest::Complete(_) => Operation::Complete,
                EngineRequest::Subscribe { .. } | EngineRequest::Subscription(_) => {
                    Operation::Subscribe
                }
                EngineRequest::Shutdown => Operation::Shutdown,
            },
        }
    }

    /// Returns the request id, if this frame carries one in the local
    /// envelope. Command IDs remain inside the library DTO bytes.
    #[must_use]
    pub fn request_id(&self) -> Option<u64> {
        match self {
            Self::Command(_) => None,
            Self::Engine { request_id, .. } => Some(*request_id),
        }
    }
}

/// The result class emitted by an owner-loop adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineStatus {
    /// The operation was admitted and processed by the owner.
    Accepted,
    /// The operation was admitted and returned a bounded typed payload.
    ///
    /// Subscription acknowledgements use this variant even when the event
    /// suffix is empty.  An empty status body cannot distinguish an accepted
    /// empty suffix from a dropped cursor or reset, so successful subscription
    /// responses always carry their versioned payload here.
    AcceptedPayload(Box<[u8]>),
    /// The owner admitted the operation to its bounded internal lane. The
    /// byte count lets clients account for transfer work that is still being
    /// consumed by the owner loop instead of mistaking admission for
    /// completion.
    Queued {
        /// Estimated bytes retained by the queued operation.
        bytes: usize,
    },
    /// The owner rejected the operation with a bounded diagnostic.
    Rejected(String),
    /// A durable subscription response with an exact lease/cursor binding.
    Subscription(LocalSubscriptionResponse),
}

/// A local response. Command replies remain the library JSON DTO bytes so CLI
/// and MCP can use their existing strict reply admission paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResponseFrame {
    /// One serialized library reply DTO.
    Command(Box<[u8]>),
    /// One owner-loop operation status.
    Engine {
        /// Correlation identity copied from the request envelope.
        request_id: u64,
        /// Bounded operation result.
        status: EngineStatus,
    },
}

/// Failure at the local endpoint boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    /// The configured limits are inconsistent or zero.
    InvalidLimits,
    /// The peer closed the stream before a complete frame arrived.
    Truncated,
    /// A frame exceeds the configured body bound.
    FrameTooLarge,
    /// An engine payload exceeds its negotiated replication bound.
    Replication(ReplicationError),
    /// A control envelope is malformed.
    InvalidControl(&'static str),
    /// A decoded command or reply is malformed.
    InvalidCommand(String),
    /// The owner could not execute an admitted command.
    ///
    /// This is deliberately separate from [`Self::InvalidCommand`]: framing
    /// and DTO admission succeeded, so reporting an owner failure as an
    /// "invalid command frame" sends callers debugging in the wrong layer.
    CommandExecution(String),
    /// A stream read or write failed.
    Io(io::ErrorKind),
    /// The endpoint timed out while the peer was idle.
    Timeout,
    /// The endpoint was closed while work was pending.
    Closed,
    /// The local queue could not admit another request.
    Backpressure,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => formatter.write_str("invalid local transport limits"),
            Self::Truncated => formatter.write_str("truncated local frame"),
            Self::FrameTooLarge => formatter.write_str("local frame exceeds its bound"),
            Self::Replication(error) => write!(formatter, "{error}"),
            Self::InvalidControl(message) => {
                write!(formatter, "invalid local control envelope: {message}")
            }
            Self::InvalidCommand(message) => write!(formatter, "invalid command frame: {message}"),
            Self::CommandExecution(message) => {
                write!(formatter, "command execution failed: {message}")
            }
            Self::Io(kind) => write!(formatter, "local endpoint I/O failed: {kind:?}"),
            Self::Timeout => formatter.write_str("local endpoint timed out"),
            Self::Closed => formatter.write_str("local endpoint is closed"),
            Self::Backpressure => formatter.write_str("local endpoint backpressure"),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// Converts locald's richer settings into the one shared byte-codec limit.
fn local_control_limits(limits: FrameLimits) -> LocalControlLimits {
    LocalControlLimits {
        max_frame: limits.max_frame,
        max_cursor: limits.max_cursor,
        // A custom local frame may be smaller than the default diagnostic
        // budget. Clamping here keeps rejection responses admissible while
        // preserving the caller's frame budget.
        max_error: diagnostic_limit(limits.max_frame),
    }
}

fn map_local_error(error: LocalControlError) -> ProtocolError {
    match error {
        LocalControlError::Closed => ProtocolError::Closed,
        LocalControlError::InvalidLimits => ProtocolError::InvalidLimits,
        LocalControlError::FrameTooLarge => ProtocolError::FrameTooLarge,
        LocalControlError::Truncated => ProtocolError::Truncated,
        LocalControlError::Trailing => ProtocolError::InvalidControl("trailing bytes after frame"),
        LocalControlError::Invalid(field) => ProtocolError::InvalidControl(field),
        LocalControlError::InvalidUtf8 => ProtocolError::InvalidControl("diagnostic encoding"),
        LocalControlError::PendingLimit => ProtocolError::Backpressure,
        LocalControlError::Io(kind) => match kind {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ProtocolError::Timeout,
            io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected => ProtocolError::Closed,
            io::ErrorKind::UnexpectedEof => ProtocolError::Truncated,
            kind => ProtocolError::Io(kind),
        },
    }
}

/// Reads exactly one bounded outer frame using the shared local codec.
///
/// # Errors
///
/// Returns [`ProtocolError`] when the peer closes, framing is malformed, a
/// declared length exceeds the configured bound, or stream I/O fails.
pub fn read_frame(reader: &mut impl Read, limits: FrameLimits) -> Result<Vec<u8>, ProtocolError> {
    limits.validate()?;
    backend_engine::read_local_frame(reader, local_control_limits(limits)).map_err(map_local_error)
}

/// Writes one bounded outer frame and flushes it before returning.
///
/// # Errors
///
/// Returns [`ProtocolError`] when the body is too large or the stream rejects
/// the write.
pub fn write_frame(
    writer: &mut impl Write,
    body: &[u8],
    limits: FrameLimits,
) -> Result<(), ProtocolError> {
    limits.validate()?;
    backend_engine::write_local_frame(writer, body, local_control_limits(limits))
        .map_err(map_local_error)
}

/// Encodes one bounded outer frame into memory.
///
/// # Errors
///
/// Returns [`ProtocolError::FrameTooLarge`] when the body exceeds its bound.
pub fn frame(body: &[u8], limits: FrameLimits) -> Result<Vec<u8>, ProtocolError> {
    limits.validate()?;
    backend_engine::frame_local_control(body, local_control_limits(limits)).map_err(map_local_error)
}

/// Decodes one complete in-memory outer frame.
///
/// # Errors
///
/// Returns [`ProtocolError`] when the length prefix is truncated, oversized, or
/// followed by trailing bytes.
pub fn unframe(input: &[u8], limits: FrameLimits) -> Result<&[u8], ProtocolError> {
    limits.validate()?;
    backend_engine::unframe_local(input, local_control_limits(limits)).map_err(map_local_error)
}

/// Classifies one extracted payload. JSON command bytes stay opaque until the
/// owner adapter supplies the typed command; an `LDC2` prefix selects the
/// shared versioned control grammar.
///
/// # Errors
///
/// Returns an error when the payload is malformed, oversized, or unknown.
pub fn decode_request(payload: &[u8], limits: FrameLimits) -> Result<RequestFrame, ProtocolError> {
    limits.validate()?;
    if payload.is_empty() {
        return Err(ProtocolError::InvalidCommand(
            "empty command frame".to_owned(),
        ));
    }
    if is_lifecycle(payload) {
        let (request_id, request) = decode_lifecycle_request(payload)?;
        return Ok(RequestFrame::Engine {
            request_id,
            request: Box::new(request),
        });
    }
    if backend_engine::is_local_control(payload) {
        let (request_id, request) = decode_engine_request(payload, limits)?;
        return Ok(RequestFrame::Engine {
            request_id,
            request: Box::new(request),
        });
    }
    Ok(RequestFrame::Command(payload.to_vec().into_boxed_slice()))
}

/// Reports whether a payload carries the locald lifecycle envelope.
#[must_use]
pub fn is_lifecycle(payload: &[u8]) -> bool {
    payload.get(..LIFECYCLE_MAGIC.len()) == Some(LIFECYCLE_MAGIC.as_slice())
}

/// Encodes one lifecycle envelope body. The result is the body of the outer
/// frame; call [`frame`] before writing it to a stream.
fn encode_lifecycle(request_id: u64, tag: u8) -> Vec<u8> {
    let mut output = Vec::with_capacity(LIFECYCLE_BYTES);
    output.extend_from_slice(&LIFECYCLE_MAGIC);
    output.push(LIFECYCLE_VERSION);
    output.push(tag);
    output.extend_from_slice(&request_id.to_be_bytes());
    output
}

/// Decodes one lifecycle envelope after an exact-size and version check.
fn decode_lifecycle_request(payload: &[u8]) -> Result<(u64, EngineRequest), ProtocolError> {
    if payload.len() != LIFECYCLE_BYTES {
        return Err(ProtocolError::InvalidControl("lifecycle envelope size"));
    }
    if payload.get(4) != Some(&LIFECYCLE_VERSION) {
        return Err(ProtocolError::InvalidControl("lifecycle envelope version"));
    }
    let request_id = payload
        .get(6..LIFECYCLE_BYTES)
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
        .map(u64::from_be_bytes)
        .ok_or(ProtocolError::Truncated)?;
    match payload.get(5) {
        Some(&LIFECYCLE_TAG_SHUTDOWN) => Ok((request_id, EngineRequest::Shutdown)),
        _ => Err(ProtocolError::InvalidControl("lifecycle operation tag")),
    }
}

/// Encodes one local engine operation payload. The result is the body of the
/// outer frame; call [`frame`] before writing to a stream.
///
/// # Errors
///
/// Returns [`ProtocolError`] when nested replication encoding, bounds, or
/// control admission fails.
pub fn encode_engine_request(
    request_id: u64,
    request: &EngineRequest,
    limits: FrameLimits,
) -> Result<Vec<u8>, ProtocolError> {
    limits.validate()?;
    if matches!(request, EngineRequest::Shutdown) {
        return Ok(encode_lifecycle(request_id, LIFECYCLE_TAG_SHUTDOWN));
    }
    let raw = match request {
        EngineRequest::Shutdown => {
            return Err(ProtocolError::InvalidControl("lifecycle operation tag"));
        }
        EngineRequest::Replicate(message) => LocalControlRequest::Replicate {
            request_id,
            payload: message
                .encode(limits.transport)
                .map_err(ProtocolError::Replication)?
                .into_boxed_slice(),
        },
        EngineRequest::Complete(claim) => LocalControlRequest::Complete {
            request_id,
            work_key: claim.work_key,
            output: claim.output,
            ordinal: claim.ordinal,
            fence: claim.fence,
        },
        EngineRequest::Subscribe { cursor, credit } => LocalControlRequest::Subscribe {
            request_id,
            cursor: cursor.clone(),
            credit: *credit,
        },
        EngineRequest::Subscription(subscription) => {
            if subscription.request_id() != request_id {
                return Err(ProtocolError::InvalidControl(
                    "subscription request correlation mismatch",
                ));
            }
            LocalControlRequest::Subscription(subscription.clone())
        }
    };
    backend_engine::encode_local_control_request(&raw, local_control_limits(limits))
        .map_err(map_local_error)
}

/// Decodes one local engine operation payload through the shared control codec.
///
/// # Errors
///
/// Returns [`ProtocolError`] for malformed control bytes or invalid nested
/// replication payloads.
pub fn decode_engine_request(
    payload: &[u8],
    limits: FrameLimits,
) -> Result<(u64, EngineRequest), ProtocolError> {
    limits.validate()?;
    let (request_id, request) =
        match backend_engine::decode_local_control_request(payload, local_control_limits(limits))
            .map_err(map_local_error)?
        {
            LocalControlRequest::Replicate {
                request_id,
                payload,
            } => (
                request_id,
                EngineRequest::Replicate(Box::new(
                    TransportMessage::decode(&payload, limits.transport)
                        .map_err(ProtocolError::Replication)?,
                )),
            ),
            LocalControlRequest::Complete {
                request_id,
                work_key,
                output,
                ordinal,
                fence,
            } => (
                request_id,
                EngineRequest::Complete(CompletionClaim {
                    work_key,
                    output,
                    ordinal,
                    fence,
                }),
            ),
            LocalControlRequest::Subscribe {
                request_id,
                cursor,
                credit,
            } => (request_id, EngineRequest::Subscribe { cursor, credit }),
            LocalControlRequest::Subscription(subscription) => (
                subscription.request_id(),
                EngineRequest::Subscription(subscription),
            ),
        };
    Ok((request_id, request))
}

/// Encodes one response payload. Command bytes remain opaque and bypass the
/// control envelope; owner operation statuses use the shared LDC2 codec.
///
/// # Errors
///
/// Returns [`ProtocolError`] when response bounds or control admission fail.
pub fn encode_response(
    response: &ResponseFrame,
    limits: FrameLimits,
) -> Result<Vec<u8>, ProtocolError> {
    limits.validate()?;
    match response {
        ResponseFrame::Command(bytes) => {
            if bytes.len() > limits.max_frame {
                return Err(ProtocolError::FrameTooLarge);
            }
            Ok(bytes.to_vec())
        }
        ResponseFrame::Engine { request_id, status } => {
            let raw = match status {
                EngineStatus::Accepted => LocalControlResponse::Accepted {
                    request_id: *request_id,
                },
                EngineStatus::AcceptedPayload(payload) => LocalControlResponse::AcceptedPayload {
                    request_id: *request_id,
                    payload: payload.clone(),
                },
                EngineStatus::Queued { bytes } => LocalControlResponse::Queued {
                    request_id: *request_id,
                    bytes: *bytes,
                },
                EngineStatus::Rejected(message) => LocalControlResponse::Rejected {
                    request_id: *request_id,
                    message: bounded_error(message, diagnostic_limit(limits.max_frame)).to_owned(),
                },
                EngineStatus::Subscription(subscription) => {
                    if subscription.request_id() != *request_id {
                        return Err(ProtocolError::InvalidControl(
                            "subscription response correlation mismatch",
                        ));
                    }
                    LocalControlResponse::Subscription(subscription.clone())
                }
            };
            backend_engine::encode_local_control_response(&raw, local_control_limits(limits))
                .map_err(map_local_error)
        }
    }
}

/// Decodes one response payload. Bodies without the control magic remain
/// opaque command replies; an `LDC2` prefix must pass the shared strict grammar.
///
/// # Errors
///
/// Returns [`ProtocolError`] when a control response is malformed or exceeds
/// configured bounds.
pub fn decode_response(
    payload: &[u8],
    limits: FrameLimits,
) -> Result<ResponseFrame, ProtocolError> {
    limits.validate()?;
    if !backend_engine::is_local_control(payload) {
        return Ok(ResponseFrame::Command(payload.to_vec().into_boxed_slice()));
    }
    let (request_id, status) =
        match backend_engine::decode_local_control_response(payload, local_control_limits(limits))
            .map_err(map_local_error)?
        {
            LocalControlResponse::Accepted { request_id } => (request_id, EngineStatus::Accepted),
            LocalControlResponse::AcceptedPayload {
                request_id,
                payload,
            } => (request_id, EngineStatus::AcceptedPayload(payload)),
            LocalControlResponse::Queued { request_id, bytes } => {
                (request_id, EngineStatus::Queued { bytes })
            }
            LocalControlResponse::Rejected {
                request_id,
                message,
            } => (request_id, EngineStatus::Rejected(message)),
            LocalControlResponse::Subscription(subscription) => (
                subscription.request_id(),
                EngineStatus::Subscription(subscription),
            ),
        };
    Ok(ResponseFrame::Engine { request_id, status })
}

fn diagnostic_limit(max_frame: usize) -> usize {
    MAX_ERROR.min(max_frame.saturating_sub(LOCAL_CONTROL_HEADER_BYTES + 4))
}

fn bounded_error(message: &str, max_error: usize) -> &str {
    if message.len() <= max_error {
        message
    } else {
        // Diagnostics are UTF-8. Keep a valid boundary when truncating.
        let mut end = max_error;
        while end > 0 && !message.is_char_boundary(end) {
            end -= 1;
        }
        &message[..end]
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn limits() -> FrameLimits {
        FrameLimits {
            max_frame: 4096,
            max_cursor: 512,
            max_frames_per_connection: 8,
            transport: TransportLimits {
                max_frame: 4096,
                max_chunk: 4096,
                ..TransportLimits::default()
            },
        }
    }

    #[test]
    fn frame_reader_handles_partial_reads_and_truncation() {
        let limits = limits();
        let bytes = frame(b"hello", limits).expect("frame");
        let mut reader = Cursor::new(bytes);
        assert_eq!(read_frame(&mut reader, limits).expect("read"), b"hello");
        let mut truncated = Cursor::new([0_u8, 0, 0, 5, b'h']);
        assert_eq!(
            read_frame(&mut truncated, limits),
            Err(ProtocolError::Truncated)
        );
        let mut closed = Cursor::new(Vec::<u8>::new());
        assert_eq!(read_frame(&mut closed, limits), Err(ProtocolError::Closed));
    }

    #[test]
    fn locald_outer_frame_matches_the_shared_codec() {
        let limits = limits();
        let shared_limits = LocalControlLimits {
            max_frame: limits.max_frame,
            max_cursor: limits.max_cursor,
            max_error: diagnostic_limit(limits.max_frame),
        };
        assert_eq!(
            frame(b"locald-wire", limits).expect("locald frame"),
            backend_engine::frame_local_control(b"locald-wire", shared_limits)
                .expect("shared frame")
        );
    }

    #[test]
    fn oversized_and_trailing_frames_are_rejected_before_allocation() {
        let limits = limits();
        assert_eq!(
            unframe(&[0, 0, 0, 5, b'h'], limits),
            Err(ProtocolError::Truncated)
        );
        assert_eq!(
            unframe(&[0, 0, 0, 1, b'h', b'x'], limits),
            Err(ProtocolError::InvalidControl("trailing bytes after frame"))
        );
        let too_large = [0xff_u8; 4];
        assert_eq!(
            unframe(&too_large, limits),
            Err(ProtocolError::FrameTooLarge)
        );
    }

    #[test]
    fn owner_execution_failures_are_not_reported_as_malformed_frames() {
        assert_eq!(
            ProtocolError::CommandExecution("index failed".to_owned()).to_string(),
            "command execution failed: index failed"
        );
        assert_ne!(
            ProtocolError::CommandExecution("index failed".to_owned()),
            ProtocolError::InvalidCommand("index failed".to_owned())
        );
    }

    #[test]
    fn subscription_control_round_trips_without_unbounded_lengths() {
        let limits = limits();
        let encoded = encode_engine_request(
            7,
            &EngineRequest::Subscribe {
                cursor: Box::from(*b"cursor"),
                credit: 3,
            },
            limits,
        )
        .expect("encode subscription");
        let (request_id, decoded) = decode_engine_request(&encoded, limits).expect("decode");
        assert_eq!(request_id, 7);
        assert!(matches!(
            decoded,
            EngineRequest::Subscribe { credit: 3, .. }
        ));
    }

    #[test]
    fn the_lifecycle_envelope_round_trips_and_never_shadows_another_grammar() {
        let limits = limits();
        let encoded =
            encode_engine_request(9, &EngineRequest::Shutdown, limits).expect("encode shutdown");
        assert_eq!(encoded.len(), LIFECYCLE_BYTES);
        assert!(is_lifecycle(&encoded));
        assert!(
            !backend_engine::is_local_control(&encoded),
            "the lifecycle magic must not be admitted by the replication grammar"
        );
        match decode_request(&encoded, limits).expect("decode shutdown") {
            RequestFrame::Engine {
                request_id,
                request,
            } => {
                assert_eq!(request_id, 9);
                assert!(matches!(*request, EngineRequest::Shutdown));
            }
            other => panic!("lifecycle frame decoded as {other:?}"),
        }
        assert_eq!(
            RequestFrame::Engine {
                request_id: 9,
                request: Box::new(EngineRequest::Shutdown),
            }
            .operation(),
            Operation::Shutdown
        );
    }

    #[test]
    fn a_malformed_lifecycle_envelope_is_rejected_before_it_reaches_an_owner() {
        let limits = limits();
        let mut truncated =
            encode_engine_request(1, &EngineRequest::Shutdown, limits).expect("encode shutdown");
        truncated.pop();
        assert_eq!(
            decode_request(&truncated, limits).map(|_| ()),
            Err(ProtocolError::InvalidControl("lifecycle envelope size"))
        );

        let mut wrong_version =
            encode_engine_request(1, &EngineRequest::Shutdown, limits).expect("encode shutdown");
        wrong_version[4] = LIFECYCLE_VERSION.wrapping_add(1);
        assert_eq!(
            decode_request(&wrong_version, limits).map(|_| ()),
            Err(ProtocolError::InvalidControl("lifecycle envelope version"))
        );

        let mut unknown_tag =
            encode_engine_request(1, &EngineRequest::Shutdown, limits).expect("encode shutdown");
        unknown_tag[5] = 0xfe;
        assert_eq!(
            decode_request(&unknown_tag, limits).map(|_| ()),
            Err(ProtocolError::InvalidControl("lifecycle operation tag"))
        );
    }

    #[test]
    fn queued_status_round_trips_with_explicit_retained_bytes() {
        let limits = limits();
        let encoded = encode_response(
            &ResponseFrame::Engine {
                request_id: 42,
                status: EngineStatus::Queued { bytes: 1536 },
            },
            limits,
        )
        .expect("encode queued response");
        assert_eq!(
            decode_response(&encoded, limits).expect("decode queued response"),
            ResponseFrame::Engine {
                request_id: 42,
                status: EngineStatus::Queued { bytes: 1536 },
            }
        );
    }
}
