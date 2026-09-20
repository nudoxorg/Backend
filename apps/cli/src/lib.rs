//! Bounded command line adapter for the versioned local daemon protocol.
//!
//! The CLI owns argument parsing and presentation only. Command meaning,
//! identity admission, view roots, and freshness stay in backend-library.
//! A process invocation talks to a configured Unix endpoint; tests and
//! embedders can inject [`CommandTransport`] directly.

/// Maximum bytes in one CLI request or reply frame.
pub const MAX_FRAME: usize = backend_replication::LOCAL_CONTROL_MAX_FRAME;
/// Maximum bytes in one text argument.
pub const MAX_TEXT: usize = backend_library::MAX_COMMAND_TEXT;
/// Maximum path length accepted for a local Unix endpoint.
///
/// This matches the local daemon's configured path budget and keeps an
/// invalid path from reaching the platform socket API.
pub const MAX_ENDPOINT_PATH: usize = backend_replication::MAX_UNIX_ENDPOINT_PATH_BYTES;
/// Environment variable naming the local daemon Unix endpoint.
pub const ENDPOINT_ENV: &str = "BACKEND_LOCALD_ENDPOINT";

mod client;
mod command;
mod error;
mod process;
mod protocol;
mod transport;

#[cfg(test)]
mod tests;

pub use client::{
    execute, execute_dto, execute_dto_with_transport, execute_dto_with_transport_with_certificate,
    execute_with_transport, format_human, run, run_json, run_with_transport,
};
pub use command::{parse, parse_with_basis};
pub use error::ClientError;
pub use process::main_entry;
pub use protocol::{
    admit_reply, admit_reply_with_capability, decode_reply, decode_reply_against,
    decode_reply_with_certificate, decode_request, decode_request_against,
    decode_request_with_certificate, encode_request, frame, unframe,
};
#[cfg(unix)]
pub use transport::UnixCommandTransport;
pub use transport::{CertifiedCommandTransport, CommandTransport, InProcessTransport, LocalEngine};

pub use backend_library::{
    Command, CommandDto, CoverageCapability, ReplyDto, WireCertificate, WireClaim, WireSchema,
};
pub(crate) use protocol::admit_request;
