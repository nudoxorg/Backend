//! Bounded transport decoding and presentation for thin CLI and MCP consumers.
//!
//! This crate owns frame and JSON shape errors only. It forwards every accepted request unchanged to
//! `wave-application-core`, which remains the sole owner of semantic validation and behavior.

mod cli;
mod command;
mod frame;
mod json;

pub use cli::{
    AdapterError, AdapterErrorCause, AdapterErrorCode, CANONICAL_CONTENT_ID_TEXT_BYTES,
    CLI_COMMAND_SEPARATOR, CanonicalContentId, CanonicalContentIdDecodeError, MAX_CLI_ARGUMENTS,
    collect_cli_arguments, decode_cli,
};
pub use frame::{
    MAX_FRAME_BYTES, MAX_HEADER_LINE_BYTES, MAX_HEADER_LINES, read_frame, write_frame,
};
pub use json::{
    CancellationTarget, McpDecode, McpDecodeError, McpEnvelope, McpError, McpReply, McpRequest,
    McpRequestId, decode_mcp, encode_cli_adapter_error, encode_cli_reply, mcp_error, mcp_reply,
};
