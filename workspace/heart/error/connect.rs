use std::io;

use super::{BackendKind, Retryable};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConnectFailure {
    /// The endpoint could not be reached (DNS, refused, network).
    #[error("endpoint unreachable")]
    Unreachable,

    /// Authentication or authorization was rejected.
    #[error("authentication rejected")]
    Auth,

    /// The connection came up but the schema/migration state is incompatible.
    #[error("schema not migrated or incompatible")]
    SchemaMismatch,

    /// A vector collection's dimension disagrees with the compiled-in `DIM`.
    #[error("vector dimension mismatch")]
    DimensionMismatch { expected: usize, found: usize },

    /// The handshake timed out.
    #[error("connection timed out")]
    Timeout,

    /// A host I/O error during connect (mkdir, file open, etc.).
    #[error("i/o error during connect")]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
#[error("failed to connect")]
pub struct ConnectError {
    /// Which backend failed to connect.
    pub backend: BackendKind,
    /// How it failed.
    #[source]
    pub kind: ConnectFailure,
}

impl ConnectError {
    /// Construct a connection error for a backend and failure mode.
    pub fn new(backend: BackendKind, kind: ConnectFailure) -> Self {
        Self { backend, kind }
    }
}

impl Retryable for ConnectError {
    fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            ConnectFailure::Unreachable | ConnectFailure::Timeout
        )
    }
}
