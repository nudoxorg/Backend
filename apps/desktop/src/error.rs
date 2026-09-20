//! Errors reported by the desktop subscription adapter.

use backend_library::ViewStateRoot;
use backend_replication::ReplicationError;
use std::fmt;

/// Errors raised while admitting a desktop subscription result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientError {
    /// A frame or DTO failed strict decoding.
    Protocol(String),
    /// The endpoint or stream failed.
    Io(String),
    /// A transport bound was exceeded.
    Transport(ReplicationError),
    /// A root was not coherent.
    IncoherentRoot,
    /// A root was based on another source basis.
    BasisMismatch {
        /// Basis expected by this desktop model.
        expected: ViewStateRoot,
        /// Basis carried by the root.
        observed: ViewStateRoot,
    },
    /// A cursor did not chain from the prior cursor/root.
    CursorMismatch,
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(message) => write!(formatter, "subscription protocol: {message}"),
            Self::Io(message) => write!(formatter, "subscription endpoint: {message}"),
            Self::Transport(error) => write!(formatter, "subscription transport: {error}"),
            Self::IncoherentRoot => formatter.write_str("subscription root is incoherent"),
            Self::BasisMismatch { .. } => {
                formatter.write_str("subscription root has the wrong source basis")
            }
            Self::CursorMismatch => formatter.write_str("subscription cursor does not chain"),
        }
    }
}

impl std::error::Error for ClientError {}
