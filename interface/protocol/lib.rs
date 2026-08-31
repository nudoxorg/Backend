//! The `interface-protocol` crate exists to decode and project the shared application vocabulary for external transports.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Bounded transport decoding and presentation for thin CLI and MCP consumers.
//!
//! This crate owns frame and JSON shape errors only. It forwards every accepted request unchanged to
//! `interface-core`, which remains the sole owner of semantic validation and behavior.

mod cli;
mod command;
mod field;
mod frame;
mod json;
mod source;

pub use cli::{
    AdapterError, AdapterErrorCause, AdapterErrorCode, CANONICAL_CONTENT_ID_TEXT_BYTES,
    CLI_COMMAND_SEPARATOR, CanonicalContentId, CanonicalContentIdDecodeError, MAX_CLI_ARGUMENTS,
    collect_cli_arguments, decode_cli, decode_cli_command, source_input,
};
pub use field::AdapterField;
pub use frame::{
    MAX_FRAME_BYTES, MAX_HEADER_LINE_BYTES, MAX_HEADER_LINES, read_frame, write_frame,
};
pub use json::{
    CancellationTarget, McpDecode, McpDecodeError, McpEnvelope, McpError, McpReply, McpRequest,
    McpRequestId, decode_mcp, encode_cli_adapter_error, encode_cli_reply, mcp_error, mcp_reply,
};
pub use source::{
    CliCommand, SourceEncodingError, SourceIngressPhase, SourceIngressRole, SourceIoFact,
};
