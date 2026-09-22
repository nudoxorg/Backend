//! Bounded MCP command adapter for the versioned local daemon protocol.
//!
//! MCP and CLI use the same backend-library [`CommandDto`] and [`ReplyDto`] values.
//! This module owns framing and endpoint I/O; it does not maintain an
//! authoritative cache or interpret semantic records.

/// Maximum frame body accepted by MCP.
pub const MAX_FRAME: usize = backend_replication::LOCAL_CONTROL_MAX_FRAME;
/// Maximum newline-delimited or HTTP JSON-RPC request body.
pub const MAX_MCP_REQUEST_FRAME: usize = backend_library::protocol::mcp::MAX_REQUEST_FRAME_BYTES;
/// Maximum emitted JSON-RPC response body.
pub const MAX_MCP_RESPONSE_FRAME: usize = backend_library::protocol::mcp::MAX_RESPONSE_FRAME_BYTES;
/// Maximum bytes in one free-form command text.
pub const MAX_TEXT: usize = backend_library::MAX_COMMAND_TEXT;
/// Maximum path length accepted for a local Unix endpoint.
///
/// This matches the local daemon's configured path budget and keeps an
/// invalid path from reaching the platform socket API.
pub const MAX_ENDPOINT_PATH: usize = backend_replication::MAX_UNIX_ENDPOINT_PATH_BYTES;
/// Environment variable naming the local daemon Unix endpoint.
pub const ENDPOINT_ENV: &str = "BACKEND_LOCALD_ENDPOINT";

mod client;
mod error;
#[cfg(any(unix, windows))]
mod http;
mod jsonrpc;
mod process;
mod protocol;
mod transport;

#[cfg(test)]
mod tests;

#[cfg(any(unix, windows))]
pub use backend_client::Session;
pub use backend_library::{
    Command, CommandDto, CoverageCapability, ReplyDto, WireCertificate, WireClaim, WireSchema,
};
pub use client::{
    Client, call, dispatch_frame, dispatch_frame_with_transport,
    dispatch_frame_with_transport_with_certificate, error_frame, error_frame_for_input,
    mcp_frame_valid, serve_frame,
};
pub use error::ClientError;
pub use process::main_entry;
pub use protocol::{
    admit_reply, admit_reply_with_capability, decode_reply, decode_reply_against,
    decode_reply_with_certificate, decode_request, decode_request_against,
    decode_request_with_certificate, encode_request, frame, unframe,
};
#[cfg(any(unix, windows))]
pub use transport::UnixCommandTransport;
pub use transport::{CertifiedCommandTransport, CommandTransport, InProcessTransport, LocalEngine};

/// Exposes the canonical tools/list projection to the dev-only payload
/// budget generator without putting tokenizer code in MCP request handling.
#[cfg(feature = "token-budget")]
#[doc(hidden)]
pub fn token_budget_tools() -> serde_json::Value {
    jsonrpc::token_budget_tools_projection()
}

/// Exposes the production-bounded JSON-RPC response serializer to the
/// dev-only budget generator. The generator supplies canonical typed fixture
/// values; this function owns the same `tools/call` result and whole-reply
/// admission gates used by a live MCP session.
#[cfg(feature = "token-budget")]
#[doc(hidden)]
pub fn token_budget_rpc_response(
    id: serde_json::Value,
    text: &str,
    structured: serde_json::Value,
    is_error: bool,
) -> serde_json::Value {
    jsonrpc::token_budget_rpc_response(id, text, structured, is_error)
}

/// Returns the bounded response together with the candidate size measured by
/// the production whole-reply admission gate.
#[cfg(feature = "token-budget")]
#[doc(hidden)]
pub fn token_budget_rpc_response_with_observed(
    id: serde_json::Value,
    text: &str,
    structured: serde_json::Value,
    is_error: bool,
) -> (serde_json::Value, usize) {
    jsonrpc::token_budget_rpc_response_with_observed(id, text, structured, is_error)
}
