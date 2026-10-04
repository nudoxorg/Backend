//! Errors crossing the Turso projection boundary.

use crate::sharing::SharingRefusal;
use std::fmt;
use std::path::PathBuf;

/// Failure to open, align, or query the derived projection.
#[derive(Debug)]
pub enum ProjectionError {
    /// A database path was not representable as UTF-8.
    NonUtf8Path(PathBuf),
    /// A database operation failed.
    Database(turso::Error),
    /// Persisted projection metadata has an unknown schema.
    Schema {
        /// Schema number read from the projection metadata row.
        found: i64,
    },
    /// A hot transition was offered against a different cached root.
    StaleTransition,
    /// A view size exceeded the SQL integer domain.
    RowCountOverflow,
    /// A package graph exceeded the bounded SQL graph projection size.
    GraphRowCountOverflow,
    /// Persisted projection metadata is structurally invalid.
    CorruptMetadata {
        /// Name of the malformed metadata field.
        field: &'static str,
    },
    /// A projection file from an older schema could not be discarded.
    Discard(std::io::Error),
    /// The database cannot be opened in the requested process-sharing mode.
    Sharing(SharingRefusal),
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonUtf8Path(path) => {
                write!(
                    formatter,
                    "Turso projection path is not UTF-8: {}",
                    path.display()
                )
            }
            Self::Database(error) => error.fmt(formatter),
            Self::Schema { found } => {
                write!(formatter, "unsupported Turso projection schema {found}")
            }
            Self::StaleTransition => {
                formatter.write_str("Turso projection transition has the wrong base root")
            }
            Self::RowCountOverflow => formatter.write_str("projection row count overflows i64"),
            Self::GraphRowCountOverflow => {
                formatter.write_str("package graph row count overflows i64")
            }
            Self::CorruptMetadata { field } => {
                write!(formatter, "projection metadata field {field} is invalid")
            }
            Self::Discard(error) => {
                write!(formatter, "discard outdated Turso projection: {error}")
            }
            Self::Sharing(refusal) => refusal.fmt(formatter),
        }
    }
}

impl std::error::Error for ProjectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Discard(error) => Some(error),
            Self::Sharing(refusal) => Some(refusal),
            _ => None,
        }
    }
}

impl From<turso::Error> for ProjectionError {
    fn from(error: turso::Error) -> Self {
        Self::Database(error)
    }
}

impl From<SharingRefusal> for ProjectionError {
    fn from(refusal: SharingRefusal) -> Self {
        Self::Sharing(refusal)
    }
}

impl From<crate::sharing::OpenFailure> for ProjectionError {
    fn from(failure: crate::sharing::OpenFailure) -> Self {
        match failure {
            crate::sharing::OpenFailure::Sharing(refusal) => Self::Sharing(refusal),
            crate::sharing::OpenFailure::Database(error) => Self::Database(error),
        }
    }
}
