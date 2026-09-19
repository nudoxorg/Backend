//! Defines json behavior for `backend-library`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Lossless JSON presentation of one application reply or transport diagnostic for the CLI.
//!
//! The MCP transport moved to [`crate::protocol::mcp`], which decodes methods without knowing what a tool
//! means. What remains here is the one thing both surfaces still share: projecting an accepted
//! application reply onto stable JSON.

use std::io;

use crate::interface::ApplicationReply;

use crate::protocol::AdapterError;

mod wire;

/// Encodes one CLI semantic result without giving the CLI a JSON dependency edge.
///
/// # Errors
///
/// Returns the serializer's I/O-compatible error if bounded reply presentation cannot serialize.
pub fn encode_cli_reply(reply: ApplicationReply) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&wire::ApplicationReplyWire::from(reply)).map_err(io::Error::other)
}

/// Encodes one CLI transport diagnostic without giving the CLI a JSON dependency edge.
///
/// # Errors
///
/// Returns the serializer's I/O-compatible error if the diagnostic cannot serialize.
pub fn encode_cli_adapter_error(error: &AdapterError) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&wire::AdapterErrorEnvelope::from(error)).map_err(io::Error::other)
}
