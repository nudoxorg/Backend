//! Shared strict JSON codecs for process-facing DTO bodies.
//!
//! Framing belongs to each transport because CLI and MCP use different error
//! surfaces. The body grammar and certificate dispatch do not belong there:
//! keeping them here guarantees that both clients parse the same versioned
//! envelope and perform the same producer admission before presentation.

use super::ReplyDto;
use crate::{CommandDto, Cursor};
use backend_version::ProducerObservationVerifier;

/// Largest admitted command envelope before JSON parsing or owned string allocation.
pub const MAX_COMMAND_BODY: usize = 256 * 1024;

/// Extracts the correlation ID from a syntactically valid command envelope.
///
/// This is deliberately weaker than command admission: listeners use it only
/// to correlate an error that prevented the full DTO from being admitted. It
/// never constructs an identity-bearing [`CommandDto`].
#[must_use]
pub fn command_request_id(bytes: &[u8]) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value.get("request_id")?.as_u64()
}

/// Decodes one strict command DTO body.
///
/// # Errors
///
/// Returns an error for malformed JSON, unknown fields, unsupported versions,
/// or missing canonical claims for identity-bearing commands.
pub fn decode_command_body(bytes: &[u8]) -> Result<CommandDto, String> {
    if bytes.len() > MAX_COMMAND_BODY {
        return Err(format!("command body exceeds {MAX_COMMAND_BODY} bytes"));
    }
    let request: CommandDto = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    crate::admit_request(&request).map_err(|error| error.to_string())?;
    Ok(request)
}

/// Decodes a command against the durable owner's admitted cursor.
///
/// This scoped boundary permits graph-query continuations to use their
/// constant-size root commitment. Every other command retains the canonical
/// standalone decoder.
///
/// # Errors
///
/// Returns an error when the bounded envelope or any owner-bound identity
/// claim fails admission.
pub fn decode_command_body_for_owner(bytes: &[u8], owner: Cursor) -> Result<CommandDto, String> {
    if bytes.len() > MAX_COMMAND_BODY {
        return Err(format!("command body exceeds {MAX_COMMAND_BODY} bytes"));
    }
    let request = CommandDto::decode_for_owner(bytes, owner)?;
    crate::admit_request(&request).map_err(|error| error.to_string())?;
    Ok(request)
}

/// Encodes one command DTO and verifies that its exact typed value survives a
/// strict round trip.
///
/// # Errors
///
/// Returns an error when serialization fails or the encoded envelope cannot
/// be admitted back to the original command.
pub fn encode_command_body(request: &CommandDto) -> Result<Vec<u8>, String> {
    crate::admit_request(request).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec(request).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_COMMAND_BODY {
        return Err(format!("command body exceeds {MAX_COMMAND_BODY} bytes"));
    }
    // Ordinary requests must survive the producer-certificate decoder before
    // an exact comparison. Comparing an envelope only with itself would let a
    // malformed canonical preimage ride along unchecked. Graph-query resumes
    // are the one deliberate exception: their root claim is an owner-bound
    // commitment and can only be reopened by `decode_command_body_for_owner`.
    let owner_bound_resume = matches!(
        &request.command,
        crate::Command::GraphQuery(query) if query.page().continuation().is_some()
    );
    if !owner_bound_resume {
        let _: CommandDto = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    }
    let admitted = CommandDto::decode_against(&bytes, request)?;
    if admitted != *request {
        return Err("request certificate does not admit the caller-owned command".to_owned());
    }
    Ok(bytes)
}

/// Decodes one strict reply DTO body.
///
/// A certificate-bearing envelope always takes the canonical producer path;
/// identity-free error replies may use the ordinary serde path. The caller
/// still runs [`crate::admit_reply`] against its request after this body
/// decoder returns.
///
/// # Errors
///
/// Returns an error for malformed JSON, unknown fields, unsupported versions,
/// or invalid producer canonical claims.
pub fn decode_reply_body(bytes: &[u8]) -> Result<ReplyDto, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if value
        .get("certificate")
        .is_some_and(|certificate| !certificate.is_null())
    {
        ReplyDto::decode_with_certificate(bytes, None)
    } else {
        serde_json::from_value(value).map_err(|error| error.to_string())
    }
}

/// Decodes a reply body using an authenticated producer observation verifier.
/// Identity-free error envelopes retain the ordinary bounded serde path;
/// complete view envelopes must carry a certificate whose exact observation
/// is admitted by `verifier` before a typed capability is created.
///
/// # Errors
///
/// Returns an error for malformed JSON, unknown fields, unsupported versions,
/// or a producer observation rejected by `verifier`.
pub fn decode_reply_body_with_verifier<V: ProducerObservationVerifier>(
    bytes: &[u8],
    verifier: &V,
) -> Result<ReplyDto, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if value
        .get("certificate")
        .is_some_and(|certificate| !certificate.is_null())
    {
        ReplyDto::decode_with_verifier(bytes, verifier)
    } else {
        serde_json::from_value(value).map_err(|error| error.to_string())
    }
}
