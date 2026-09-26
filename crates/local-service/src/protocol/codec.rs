//! Outer-frame and engine-envelope codecs for the local endpoint.
//!
//! Length prefixes, lifecycle envelopes, and shared `LDC2` control payloads
//! are encoded here. Request and response types stay with the parent module.

use super::{
    CompletionClaim, EngineRequest, EngineStatus, FrameLimits, LIFECYCLE_BYTES, LIFECYCLE_MAGIC,
    LIFECYCLE_TAG_SHUTDOWN, LIFECYCLE_VERSION, MAX_ERROR, ProtocolError, RequestFrame,
    ResponseFrame,
};
use backend_engine::{
    LOCAL_CONTROL_HEADER_BYTES, LocalControlError, LocalControlLimits, LocalControlRequest,
    LocalControlResponse, TransportMessage,
};
use std::io::{self, Read, Write};

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

pub(super) fn map_local_error(error: LocalControlError) -> ProtocolError {
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

pub(super) fn diagnostic_limit(max_frame: usize) -> usize {
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
