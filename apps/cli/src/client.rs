//! Transport-independent command execution for the CLI.
//!
//! Presentation moved out of this module entirely: it now belongs to
//! [`crate::render`], which delegates to the shared model. What is left here
//! is the admitted request/reply round trip every embedder needs, plus the
//! versioned wire encoding used by `--format json`'s escape hatch and by the
//! in-process tests.

use crate::{
    CertifiedCommandTransport, ClientError, Command, CommandDto, CommandTransport, LocalEngine,
    MAX_FRAME, ReplyDto, admit_reply, admit_reply_with_capability, admit_request,
};
use backend_library::CoverageCapability;

/// Executes one command using an injected transport.
///
/// # Errors
///
/// Returns the transport, protocol, admission, or reply validation error from
/// the injected command transport.
pub fn execute_with_transport(
    transport: &mut impl CommandTransport,
    command: Command,
) -> Result<ReplyDto, ClientError> {
    let request = CommandDto::new(1, command);
    execute_dto_with_transport(transport, &request)
}

/// Executes one correlated DTO using an injected transport.
///
/// # Errors
///
/// Returns the transport, protocol, admission, or reply validation error from
/// the injected command transport.
pub fn execute_dto_with_transport(
    transport: &mut impl CommandTransport,
    request: &CommandDto,
) -> Result<ReplyDto, ClientError> {
    admit_request(request)?;
    let reply = transport.request(request.clone())?;
    admit_reply(request, reply)
}

/// Executes one correlated DTO through a transport that explicitly admits
/// producer certificates and optional complete-view coverage.
///
/// # Errors
///
/// Returns the transport, protocol, certificate, coverage, or reply
/// validation error from the transport boundary.
pub fn execute_dto_with_transport_with_certificate(
    transport: &mut impl CertifiedCommandTransport,
    request: &CommandDto,
    capability: Option<CoverageCapability>,
) -> Result<ReplyDto, ClientError> {
    admit_request(request)?;
    let reply = transport.request_with_certificate(request.clone(), capability.clone())?;
    admit_reply_with_capability(request, reply, capability)
}

/// Executes one command through the compatibility in-process seam.
#[must_use]
pub fn execute(engine: &mut impl LocalEngine, command: Command) -> ReplyDto {
    execute_dto(engine, CommandDto::new(1, command))
}

/// Executes one already correlated DTO through the compatibility seam.
#[must_use]
pub fn execute_dto(engine: &mut impl LocalEngine, request: CommandDto) -> ReplyDto {
    let request_id = request.request_id;
    let mut transport = crate::InProcessTransport::new(engine);
    transport
        .request(request)
        .unwrap_or_else(|error| ReplyDto::error(request_id, error.to_string()))
}

/// Serializes one reply DTO using the shared versioned wire schema.
///
/// This is the raw wire projection, not the product one: `--format json`
/// emits the presentation DTOs instead, because a consumer should not bind to
/// certificate claims and cursor internals.
#[must_use]
pub fn run_json(reply: &ReplyDto) -> String {
    if crate::protocol::preflight_reply_memory(reply).is_err() {
        return oversized(reply);
    }
    match serde_json::to_string(reply) {
        Ok(encoded) if encoded.len() <= MAX_FRAME => encoded,
        _ => oversized(reply),
    }
}

fn oversized(reply: &ReplyDto) -> String {
    serde_json::json!({
        "version": backend_library::protocol_version(),
        "request_id": reply.request_id,
        "reply": {
            "kind": "error",
            "data": { "message": "reply encoding failed or exceeded frame bound" }
        }
    })
    .to_string()
}
