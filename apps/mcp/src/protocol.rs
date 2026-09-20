//! MCP DTO framing and reply admission.

use crate::{ClientError, CommandDto, MAX_FRAME, ReplyDto};
use backend_library::{
    CoverageCapability, ReplyAdmissionError, RequestAdmissionError, ViewProjectionError,
};
use backend_replication::ReplicationError;
use backend_replication::{
    LocalControlError, LocalControlLimits, frame as frame_local_control,
    unframe as unframe_local_control,
};
use std::io;

/// Encodes one bounded length-prefixed request or reply body.
///
/// # Errors
///
/// Returns an I/O error when the body exceeds the MCP frame bound or its
/// length cannot be represented by the wire prefix.
pub fn frame(body: &[u8]) -> io::Result<Vec<u8>> {
    frame_local_control(body, control_limits()).map_err(map_frame_error)
}

/// Decodes exactly one bounded frame.
///
/// # Errors
///
/// Returns an I/O error for a truncated, oversized, or trailing frame.
pub fn unframe(input: &[u8]) -> io::Result<&[u8]> {
    unframe_local_control(input, control_limits()).map_err(map_frame_error)
}

pub(crate) fn control_limits() -> LocalControlLimits {
    LocalControlLimits {
        max_frame: MAX_FRAME,
        ..LocalControlLimits::default()
    }
}

fn map_frame_error(error: LocalControlError) -> io::Error {
    let kind = match error {
        LocalControlError::Truncated | LocalControlError::Closed => io::ErrorKind::UnexpectedEof,
        LocalControlError::Io(kind) => kind,
        LocalControlError::FrameTooLarge => io::ErrorKind::InvalidInput,
        LocalControlError::InvalidLimits
        | LocalControlError::PendingLimit
        | LocalControlError::Trailing
        | LocalControlError::Invalid(_)
        | LocalControlError::InvalidUtf8 => io::ErrorKind::InvalidData,
    };
    io::Error::new(kind, error.to_string())
}

/// Encodes a command DTO as one bounded MCP frame.
///
/// # Errors
///
/// Returns an I/O error when request admission, serialization, or frame bounds
/// fail.
pub fn encode_request(request: &CommandDto) -> io::Result<Vec<u8>> {
    admit_request(request)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    let body = encode_request_body(request)?;
    frame(&body)
}

/// Decodes one bounded MCP frame as a command DTO.
///
/// # Errors
///
/// Returns an I/O error when framing, strict DTO decoding, or request
/// admission fails.
pub fn decode_request(input: &[u8]) -> io::Result<CommandDto> {
    let body = unframe(input)?;
    let request = decode_request_body(body)?;
    admit_request(&request)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    Ok(request)
}

/// Decodes a command frame carrying canonical identity preimages.
///
/// # Errors
///
/// Returns an I/O error when strict DTO or producer-certificate admission
/// fails.
pub fn decode_request_with_certificate(input: &[u8]) -> io::Result<CommandDto> {
    let body = unframe(input)?;
    let request = decode_request_body(body)?;
    admit_request(&request)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    Ok(request)
}

/// Decodes one versioned reply DTO from a bounded MCP frame.
///
/// # Errors
///
/// Returns an I/O error when framing or strict DTO decoding fails. Identity
/// bearing replies are admitted from their producer certificate; callers with
/// an accepted typed reply can use [`decode_reply_against`] for an additional
/// exact comparison.
pub fn decode_reply(input: &[u8]) -> io::Result<ReplyDto> {
    let body = unframe(input)?;
    decode_reply_body(body)
}

fn decode_request_body(body: &[u8]) -> io::Result<CommandDto> {
    backend_library::decode_command_body(body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub(crate) fn encode_request_body(request: &CommandDto) -> io::Result<Vec<u8>> {
    backend_library::encode_command_body(request)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

/// Decodes a reply body through producer certificate admission whenever the
/// envelope carries one. Identity-free error replies remain valid without a
/// certificate; successful identity-bearing replies are independently
/// rehashed by `backend-library` before they enter MCP.
pub(crate) fn decode_reply_body(body: &[u8]) -> io::Result<ReplyDto> {
    backend_library::decode_reply_body(body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Decodes a reply frame from producer-certified canonical preimages.
///
/// # Errors
///
/// Returns an I/O error when any identity, checked transition, or complete
/// coverage capability is invalid.
pub fn decode_reply_with_certificate(
    input: &[u8],
    capability: Option<CoverageCapability>,
) -> io::Result<ReplyDto> {
    let body = unframe(input)?;
    ReplyDto::decode_with_certificate(body, capability)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Decodes a bounded reply frame against a caller-owned accepted reply.
///
/// Identity-bearing replies are not admitted from digest-only claims. This
/// helper compares the strict wire envelope with the exact typed value held by
/// the caller and returns that accepted value.
///
/// # Errors
///
/// Returns an I/O error when framing, strict DTO decoding, or exact claim
/// comparison fails.
pub fn decode_reply_against(input: &[u8], expected: &ReplyDto) -> io::Result<ReplyDto> {
    let body = unframe(input)?;
    ReplyDto::decode_against(body, expected)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Decodes one command frame against a caller-owned accepted command.
///
/// # Errors
///
/// Returns an I/O error when framing, strict DTO decoding, or exact claim
/// comparison fails.
pub fn decode_request_against(input: &[u8], expected: &CommandDto) -> io::Result<CommandDto> {
    let body = unframe(input)?;
    CommandDto::decode_against(body, expected)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Admits request correlation, coherent view roots, and freshness.
/// Health replies additionally cross the shared certificate boundary and are
/// rejected unless they produce a complete [`backend_library::CompleteViewProjection`].
///
/// # Errors
///
/// Returns a [`ClientError`] when request correlation, root coherence,
/// freshness, cursor binding, serialization, or frame bounds fail.
pub fn admit_reply(request: &CommandDto, reply: ReplyDto) -> Result<ReplyDto, ClientError> {
    admit_reply_with_capability(request, reply, None)
}

/// Admits a reply with an externally authenticated complete-view capability.
///
/// # Errors
///
/// Returns an error when reply correlation, identity, freshness, or complete
/// coverage admission fails.
pub fn admit_reply_with_capability(
    request: &CommandDto,
    reply: ReplyDto,
    capability: Option<CoverageCapability>,
) -> Result<ReplyDto, ClientError> {
    backend_library::admit_reply_with_capability(request, &reply, capability)
        .map_err(map_reply_admission_error)?;
    preflight_reply_memory(&reply)?;
    let encoded =
        serde_json::to_vec(&reply).map_err(|error| ClientError::Protocol(error.to_string()))?;
    if encoded.len() > MAX_FRAME {
        return Err(ClientError::Transport(ReplicationError::MessageTooLarge));
    }
    Ok(reply)
}

/// Rejects a hostile in-process result before the wire serializer walks and
/// materializes its complete DTO representation.
pub(crate) fn preflight_reply_memory(reply: &ReplyDto) -> Result<(), ClientError> {
    if backend_library::reply_memory_bound(reply) > MAX_FRAME {
        return Err(ClientError::Transport(ReplicationError::MessageTooLarge));
    }
    Ok(())
}

pub(crate) fn admit_request(request: &CommandDto) -> Result<(), ClientError> {
    backend_library::admit_request(request).map_err(|error| match error {
        RequestAdmissionError::EmptyText => ClientError::Protocol(error.to_string()),
        RequestAdmissionError::TextTooLarge => {
            ClientError::Transport(ReplicationError::MessageTooLarge)
        }
    })
}

fn map_reply_admission_error(error: ReplyAdmissionError) -> ClientError {
    match error {
        ReplyAdmissionError::RequestMismatch { expected, observed } => {
            ClientError::RequestMismatch { expected, observed }
        }
        ReplyAdmissionError::Protocol(message) => ClientError::Protocol(message),
        ReplyAdmissionError::Projection(error) => map_projection_error(error),
    }
}

fn map_projection_error(error: ViewProjectionError) -> ClientError {
    match error {
        ViewProjectionError::Incoherent => ClientError::IncoherentView,
        ViewProjectionError::UnsupportedSchema => {
            ClientError::Protocol("unsupported view schema".to_owned())
        }
        ViewProjectionError::CursorMismatch => ClientError::CursorMismatch,
        ViewProjectionError::FreshnessMismatch => ClientError::FreshnessMismatch,
        ViewProjectionError::BasisMismatch { expected, observed } => {
            ClientError::BasisMismatch { expected, observed }
        }
        ViewProjectionError::MissingCoverage => ClientError::Protocol(
            "view projection requires producer-admitted complete coverage".to_owned(),
        ),
    }
}
