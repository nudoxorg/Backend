//! Errors returned by the versioned library façade.

use crate::{ViewError, ViewRevision, ViewStateRoot};

/// Failure returned by the portable library query/transition layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LibraryError {
    /// Requested record does not exist in the pinned source scope.
    NotFound,
    /// Query revision differs from the current coherent view revision.
    WrongBasis {
        /// Root expected by this library instance.
        expected: ViewStateRoot,
        /// Root supplied by the caller.
        observed: ViewRevision,
    },
    /// A checked view transition failed.
    View(ViewError),
    /// Query text or limit failed validation.
    InvalidQuery(String),
    /// A durable event sequence could not advance.
    SequenceOverflow,
    /// A subscription cursor belongs to another recipe or source stream.
    CursorMismatch,
}

impl core::fmt::Display for LibraryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotFound => f.write_str("library record not found"),
            Self::WrongBasis { .. } => f.write_str("query basis does not match the view revision"),
            Self::View(error) => write!(f, "view transition failed: {error:?}"),
            Self::InvalidQuery(error) => write!(f, "invalid query: {error}"),
            Self::SequenceOverflow => f.write_str("library event sequence overflowed"),
            Self::CursorMismatch => f.write_str("subscription cursor does not match the view"),
        }
    }
}

impl std::error::Error for LibraryError {}

impl From<ViewError> for LibraryError {
    fn from(error: ViewError) -> Self {
        Self::View(error)
    }
}
