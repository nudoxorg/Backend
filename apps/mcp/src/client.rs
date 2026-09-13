//! MCP request dispatch and process entrypoint.

use crate::{
    CertifiedCommandTransport, ClientError, Command, CommandDto, CommandTransport, LocalEngine,
    ReplyDto, admit_reply, decode_request, decode_request_with_certificate, frame, unframe,
};
use backend_library::{CommandReply, CoverageCapability, Intent, IntentId, ViewRevision};
use std::io;

/// A small client wrapper for embedded engines. Idempotency is owned by the
/// daemon; this wrapper deliberately does not cache replies.
pub struct Client<E> {
    engine: E,
}

impl<E: LocalEngine> Client<E> {
    /// Creates a client around an embedded engine.
    #[must_use]
    pub fn new(engine: E) -> Self {
        Self { engine }
    }

    /// Sends one command through a trusted embedded owner.
    ///
    /// The legacy intent argument is retained for source compatibility with
    /// embedded callers, but it is not a wire authority claim. Process and
    /// endpoint callers must use [`CommandTransport`], whose versioned DTO
    /// carries the daemon-admitted request identity. If the embedded command
    /// returns an intent result, this wrapper still compares it with the
    /// caller-owned intent before exposing it.
    pub fn call_intent(&mut self, intent: IntentId, request_id: u64, command: Command) -> ReplyDto {
        let expected = match &command {
            Command::Add { package } => Some(Intent::request_package(*package).id()),
            Command::Remove { package } => Some(Intent::remove_package(*package).id()),
            _ => None,
        };
        if expected.is_some_and(|expected| expected != intent) {
            return ReplyDto::error(
                request_id,
                "embedded caller intent does not match the requested command",
            );
        }
        let request = CommandDto::new(request_id, command);
        let mut transport = crate::InProcessTransport::new(&mut self.engine);
        let reply = transport
            .request(request)
            .unwrap_or_else(|error| ReplyDto::error(request_id, error.to_string()));
        if matches!(
            &reply.reply,
            CommandReply::Added(observed) | CommandReply::Removed(observed)
                if *observed != intent
        ) {
            return ReplyDto::error(
                request_id,
                "embedded reply intent does not match the caller-owned intent",
            );
        }
        reply
    }

    /// Returns the wrapped engine.
    #[must_use]
    pub fn into_inner(self) -> E {
        self.engine
    }
}

/// Executes one framed request using an injected transport.
///
/// # Errors
///
/// Returns a [`ClientError`] when request decoding, transport execution, reply
/// admission, serialization, or framing fails.
pub fn dispatch_frame_with_transport(
    transport: &mut impl CommandTransport,
    input: &[u8],
) -> Result<Vec<u8>, ClientError> {
    dispatch_frame_result(input, transport)
}

/// Executes one framed request through a transport that explicitly admits
/// producer certificates and optional complete-view coverage.
///
/// The ordinary MCP process uses [`dispatch_frame_with_transport`] because a
/// standalone process has no trusted source-coverage capability to inject.
/// Embedded composition roots that receive such a capability must use this
/// entry point so a complete snapshot is checked before it reaches the MCP
/// presentation layer.
///
/// # Errors
///
/// Returns a [`ClientError`] when request decoding, transport execution,
/// certificate admission, coverage validation, reply admission, serialization,
/// or framing fails.
pub fn dispatch_frame_with_transport_with_certificate(
    transport: &mut impl CertifiedCommandTransport,
    input: &[u8],
    capability: Option<CoverageCapability>,
) -> Result<Vec<u8>, ClientError> {
    dispatch_frame_result_with_certificate(input, transport, capability)
}

/// Executes one framed request through the compatibility in-process seam.
///
/// # Errors
///
/// Returns an I/O error when request decoding, reply admission, serialization,
/// or framing fails.
pub fn dispatch_frame(engine: &mut impl LocalEngine, input: &[u8]) -> io::Result<Vec<u8>> {
    let request = decode_request(input)?;
    let mut transport = crate::InProcessTransport::new(engine);
    admit_request_basis(&mut transport, &request)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    let raw_reply = transport
        .request(request.clone())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    let reply = admit_reply(&request, raw_reply)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    let body = serde_json::to_vec(&reply)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    frame(&body)
}

/// Executes one correlated in-process request.
#[must_use]
pub fn call(engine: &mut impl LocalEngine, request_id: u64, command: Command) -> ReplyDto {
    let request = CommandDto::new(request_id, command);
    let mut transport = crate::InProcessTransport::new(engine);
    transport
        .request(request)
        .unwrap_or_else(|error| ReplyDto::error(request_id, error.to_string()))
}

/// Produces a framed error reply, including for malformed input where no
/// request identity can be recovered.
#[must_use]
pub fn error_frame(request_id: u64, message: impl Into<String>) -> Vec<u8> {
    let reply = ReplyDto::error(request_id, message);
    let body = match serde_json::to_vec(&reply) {
        Ok(body) => body,
        Err(_) => serde_json::json!({
            "version": backend_library::protocol_version(),
            "request_id": 0,
            "reply": { "kind": "error", "data": { "message": "reply encoding failed" } }
        })
        .to_string()
        .into_bytes(),
    };
    let fallback = serde_json::json!({
        "version": backend_library::protocol_version(),
        "request_id": 0,
        "reply": { "kind": "error", "data": { "message": "frame encoding failed" } }
    });
    if let Ok(framed) = frame(&body) {
        return framed;
    }
    let fallback_body = fallback.to_string().into_bytes();
    if let Ok(framed) = frame(&fallback_body) {
        return framed;
    }
    // The literal fallback is below the frame bound by construction. Run it
    // through the same shared codec as every other process frame.
    let body = br#"{"version":1,"request_id":0,"reply":{"kind":"error","data":{"message":"frame encoding failed"}}}"#;
    frame(body).unwrap_or_default()
}

/// Produces a framed error while retaining a request id when the input DTO is
/// otherwise valid.
#[must_use]
pub fn error_frame_for_input(input: &[u8], message: impl Into<String>) -> Vec<u8> {
    let request_id = unframe(input)
        .ok()
        .and_then(|body| serde_json::from_slice::<serde_json::Value>(body).ok())
        .and_then(|value| value.get("request_id").and_then(serde_json::Value::as_u64))
        .unwrap_or(0);
    error_frame(request_id, message)
}

/// Runs one complete MCP stdio request. It always emits one bounded reply
/// frame, including protocol or endpoint errors.
#[must_use]
pub fn serve_frame(input: &[u8], transport: &mut impl CommandTransport) -> Vec<u8> {
    let request_id = decode_request(input).map_or(0, |request| request.request_id);
    dispatch_frame_result(input, transport)
        .unwrap_or_else(|error| error_frame(request_id, error.to_string()))
}

fn dispatch_frame_result(
    input: &[u8],
    transport: &mut impl CommandTransport,
) -> Result<Vec<u8>, ClientError> {
    let request =
        decode_request(input).map_err(|error| ClientError::Protocol(error.to_string()))?;
    admit_request_basis(transport, &request)?;
    let reply = admit_reply(&request, transport.request(request.clone())?)?;
    let body =
        serde_json::to_vec(&reply).map_err(|error| ClientError::Protocol(error.to_string()))?;
    frame(&body).map_err(|error| ClientError::Protocol(error.to_string()))
}

fn dispatch_frame_result_with_certificate(
    input: &[u8],
    transport: &mut impl CertifiedCommandTransport,
    capability: Option<CoverageCapability>,
) -> Result<Vec<u8>, ClientError> {
    let request = decode_request_with_certificate(input)
        .map_err(|error| ClientError::Protocol(error.to_string()))?;
    admit_request_basis_with_certificate(transport, &request, capability.clone())?;
    let reply = crate::protocol::admit_reply_with_capability(
        &request,
        transport.request_with_certificate(request.clone(), capability.clone())?,
        capability,
    )?;
    let body =
        serde_json::to_vec(&reply).map_err(|error| ClientError::Protocol(error.to_string()))?;
    frame(&body).map_err(|error| ClientError::Protocol(error.to_string()))
}

fn admit_request_basis_with_certificate(
    transport: &mut impl CertifiedCommandTransport,
    request: &CommandDto,
    capability: Option<CoverageCapability>,
) -> Result<(), ClientError> {
    let Some(expected) = request_basis(&request.command) else {
        return Ok(());
    };
    let health_request_id = request
        .request_id
        .checked_add(1)
        .ok_or_else(|| ClientError::Protocol("request id exhausted".to_owned()))?;
    let revision_request = CommandDto::new(health_request_id, Command::Revision);
    let raw_revision =
        transport.request_with_certificate(revision_request.clone(), capability.clone())?;
    let revision =
        crate::protocol::admit_reply_with_capability(&revision_request, raw_revision, capability)?;
    match revision.reply {
        CommandReply::Revision(receipt) => {
            if expected.matches(receipt.root()) {
                Ok(())
            } else {
                Err(ClientError::Protocol(
                    "daemon revision does not match the request".to_owned(),
                ))
            }
        }
        CommandReply::Error(message) => {
            Err(ClientError::Protocol(format!("daemon revision: {message}")))
        }
        CommandReply::Failed(failure) => {
            Err(ClientError::Protocol(format!("daemon revision: {failure}")))
        }
        _ => Err(ClientError::Protocol(
            "daemon revision returned an invalid reply".to_owned(),
        )),
    }
}

pub(crate) fn request_basis(command: &Command) -> Option<ViewRevision> {
    match command {
        Command::Document(query) => Some(query.basis()),
        Command::Outline(query) => Some(query.basis()),
        Command::PackagePage(page)
        | Command::OutlinePage { page, .. }
        | Command::GraphPage { page, .. } => Some(page.basis()),
        Command::Name(query) => Some(query.basis()),
        Command::Search(query) => Some(query.basis()),
        Command::Graph(query) => Some(query.basis()),
        Command::GraphQuery(query) => Some(query.page().basis()),
        _ => None,
    }
}

fn admit_request_basis(
    transport: &mut impl CommandTransport,
    request: &CommandDto,
) -> Result<(), ClientError> {
    let Some(expected) = request_basis(&request.command) else {
        return Ok(());
    };
    let health_request_id = request
        .request_id
        .checked_add(1)
        .ok_or_else(|| ClientError::Protocol("request id exhausted".to_owned()))?;
    let revision_request = CommandDto::new(health_request_id, Command::Revision);
    let raw_revision = transport.request(revision_request.clone())?;
    let revision = admit_reply(&revision_request, raw_revision)?;
    match revision.reply {
        CommandReply::Revision(receipt) => {
            if expected.matches(receipt.root()) {
                Ok(())
            } else {
                Err(ClientError::Protocol(
                    "daemon revision does not match the request".to_owned(),
                ))
            }
        }
        CommandReply::Error(message) => {
            Err(ClientError::Protocol(format!("daemon revision: {message}")))
        }
        CommandReply::Failed(failure) => {
            Err(ClientError::Protocol(format!("daemon revision: {failure}")))
        }
        _ => Err(ClientError::Protocol(
            "daemon revision returned an invalid reply".to_owned(),
        )),
    }
}

/// Compatibility helper retained for callers that only need frame admission.
#[must_use]
pub fn mcp_frame_valid(input: &[u8]) -> bool {
    unframe(input).is_ok()
}
