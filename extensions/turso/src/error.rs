//! Errors crossing the Turso projection boundary.

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
        }
    }
}

impl std::error::Error for ProjectionError {}

impl From<turso::Error> for ProjectionError {
    fn from(error: turso::Error) -> Self {
        Self::Database(error)
    }
}
