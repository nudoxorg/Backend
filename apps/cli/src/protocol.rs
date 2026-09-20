//! Command DTO framing and reply admission for the CLI.

use crate::{MAX_FRAME, error::ClientError};
use backend_library::{
    CommandDto, CoverageCapability, ReplyAdmissionError, ReplyDto, RequestAdmissionError,
    ViewProjectionError,
};
use backend_replication::{
    LocalControlError, LocalControlLimits, ReplicationError, frame as frame_local_control,
    unframe as unframe_local_control,
};

/// Encodes one bounded length-prefixed body.
///
/// # Errors
///
/// Returns [`ClientError::Transport`] when the body exceeds the frame bound or
/// its length cannot be represented in the wire prefix.
pub fn frame(body: &[u8]) -> Result<Vec<u8>, ClientError> {
    frame_local_control(body, control_limits()).map_err(map_frame_error)
}

/// Decodes exactly one bounded frame from an in-memory exchange.
///
/// # Errors
///
/// Returns [`ClientError::Protocol`] for a malformed length or trailing bytes,
/// and [`ClientError::Transport`] when the declared size exceeds the bound.
pub fn unframe(input: &[u8]) -> Result<&[u8], ClientError> {
    unframe_local_control(input, control_limits()).map_err(map_frame_error)
}

pub(crate) fn control_limits() -> LocalControlLimits {
    LocalControlLimits {
        max_frame: MAX_FRAME,
        ..LocalControlLimits::default()
    }
}

pub(crate) fn map_frame_error(error: LocalControlError) -> ClientError {
    match error {
        LocalControlError::FrameTooLarge => {
            ClientError::Transport(ReplicationError::MessageTooLarge)
        }
        LocalControlError::Io(kind) => {
            ClientError::Io(format!("local control I/O failed: {kind:?}"))
        }
        LocalControlError::Closed => ClientError::Io("local endpoint is closed".to_owned()),
        other => ClientError::Protocol(other.to_string()),
    }
}

/// Encodes one versioned command DTO as a bounded frame.
///
/// # Errors
///
/// Returns [`ClientError::Protocol`] when serialization or request admission
/// fails, or [`ClientError::Transport`] when the frame is too large.
pub fn encode_request(request: &CommandDto) -> Result<Vec<u8>, ClientError> {
    admit_request(request)?;
    let body = encode_request_body(request)?;
    frame(&body)
}

/// Decodes one versioned command DTO from a bounded frame.
///
/// # Errors
///
/// Returns [`ClientError::Protocol`] when framing or strict DTO decoding fails,
/// or [`ClientError::Transport`] when request bounds are exceeded.
pub fn decode_request(input: &[u8]) -> Result<CommandDto, ClientError> {
    let body = unframe(input)?;
    let request = decode_request_body(body)?;
    admit_request(&request)?;
    Ok(request)
}

/// Decodes a command frame carrying canonical identity preimages.
///
/// # Errors
///
/// Returns [`ClientError::Protocol`] when the strict DTO or one of its
/// producer-certified identities is invalid.
pub fn decode_request_with_certificate(input: &[u8]) -> Result<CommandDto, ClientError> {
    let body = unframe(input)?;
    let request = decode_request_body(body)?;
    admit_request(&request)?;
    Ok(request)
}

fn decode_request_body(body: &[u8]) -> Result<CommandDto, ClientError> {
    backend_library::decode_command_body(body).map_err(ClientError::Protocol)
}

pub(crate) fn encode_request_body(request: &CommandDto) -> Result<Vec<u8>, ClientError> {
    backend_library::encode_command_body(request).map_err(ClientError::Protocol)
}

/// Decodes one versioned reply DTO from a bounded frame.
///
/// # Errors
///
/// Returns [`ClientError::Protocol`] when framing or strict DTO decoding fails.
pub fn decode_reply(input: &[u8]) -> Result<ReplyDto, ClientError> {
    let body = unframe(input)?;
    decode_reply_body(body)
}

/// Decodes a reply body through the producer certificate admission path when
/// the envelope carries one. Identity-free error replies remain valid without
/// a certificate; every successful identity-bearing reply is independently
/// rehashed by `backend-library` before this adapter sees its typed value.
pub(crate) fn decode_reply_body(body: &[u8]) -> Result<ReplyDto, ClientError> {
    backend_library::decode_reply_body(body).map_err(ClientError::Protocol)
}

/// Decodes a reply frame from producer-certified canonical preimages.
///
/// # Errors
///
/// Returns [`ClientError::Protocol`] when any identity, checked transition, or
/// complete-coverage capability is invalid.
pub fn decode_reply_with_certificate(
    input: &[u8],
    capability: Option<CoverageCapability>,
) -> Result<ReplyDto, ClientError> {
    let body = unframe(input)?;
    ReplyDto::decode_with_certificate(body, capability).map_err(ClientError::Protocol)
}

/// Decodes one reply frame against a caller-owned accepted reply.
///
/// The ordinary [`decode_reply`] path admits identity-bearing payloads only
/// when the producer certificate in the shared DTO carries canonical
/// preimages. Callers that already hold the expected typed reply can use this
/// checked comparison as an additional exact comparison without reconstructing
/// any backend ID.
///
/// # Errors
///
/// Returns [`ClientError::Protocol`] when framing, strict DTO decoding, or
/// exact claim comparison fails.
pub fn decode_reply_against(input: &[u8], expected: &ReplyDto) -> Result<ReplyDto, ClientError> {
    let body = unframe(input)?;
    ReplyDto::decode_against(body, expected).map_err(ClientError::Protocol)
}

/// Decodes one command frame against a caller-owned accepted command.
///
/// # Errors
///
/// Returns [`ClientError::Protocol`] when framing, strict DTO decoding, or
/// exact claim comparison fails.
pub fn decode_request_against(
    input: &[u8],
    expected: &CommandDto,
) -> Result<CommandDto, ClientError> {
    let body = unframe(input)?;
    CommandDto::decode_against(body, expected).map_err(ClientError::Protocol)
}

/// Admits reply identity, coherent roots, and query freshness.
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
