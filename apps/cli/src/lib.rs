//! The command line rendering of the shared presentation model.
//!
//! The CLI owns three things and no more: the argument grammar
//! ([`invoke`]), the request sequence one command needs ([`run`]), and the
//! choice of rendering ([`render`]). What an answer *means* lives in
//! `backend-present`, and what a command *is* lives in
//! [`backend_library::COMMANDS`]. That split is what lets the CLI reach all
//! thirty-five registry rows without thirty-five hand-written parsers, and
//! what makes `--format markdown` byte-identical to the MCP text block rather
//! than merely similar.
//!
//! Identity admission, view roots, and freshness stay in `backend-library`
//! and `backend-client`; nothing here mints a key or judges a proof.

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
mod error;
pub mod invoke;
pub mod options;
mod process;
mod protocol;
pub mod render;
pub mod run;
mod transport;

#[cfg(test)]
mod tests;

pub use client::{
    execute, execute_dto, execute_dto_with_transport, execute_dto_with_transport_with_certificate,
    execute_with_transport, run_json,
};
pub use error::ClientError;
pub use backend_present::{Request, lower, lower_surface_json};
pub use options::{Format, Options};
pub use process::{answer_with_session, main_entry};
pub use protocol::{
    admit_reply, admit_reply_with_capability, decode_reply, decode_reply_against,
    decode_reply_with_certificate, decode_request, decode_request_against,
    decode_request_with_certificate, encode_request, frame, unframe,
};
pub use render::{EXIT_IO, EXIT_OK, EXIT_REFUSED, EXIT_USAGE};
pub use run::Answer;
#[cfg(any(unix, windows))]
pub use transport::UnixCommandTransport;
pub use transport::{CertifiedCommandTransport, CommandTransport, InProcessTransport, LocalEngine};

pub use backend_library::{
    Command, CommandDto, CoverageCapability, ReplyDto, WireCertificate, WireClaim, WireSchema,
};
pub use backend_present::{Fault, Theme, Width};
pub(crate) use protocol::admit_request;
