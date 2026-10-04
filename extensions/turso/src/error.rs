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
    /// A mutation was offered against a different selected root or witness.
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
    /// The database cannot be opened in the requested process-sharing mode.
    Sharing(SharingRefusal),
    /// No seeded generation has been selected for this database stem yet.
    NeedsSeed,
    /// A bounded projection namespace record or inventory is malformed.
    CorruptNamespace {
        /// Name of the malformed field or entry class.
        field: &'static str,
    },
    /// A pinned namespace object was replaced or no longer has its admitted identity.
    NamespaceIdentity,
    /// The namespace contains the maximum retained generations. No generation
    /// is removed without an exact retired-child cleanup receipt.
    GenerationLimit {
        /// Maximum retained generations.
        maximum: usize,
    },
    /// The monotonic generation sequence reached its integer limit.
    GenerationIdExhausted,
    /// A cross-process namespace gate remained busy past its bounded wait.
    NamespaceBusy,
    /// Atomic selector publication happened, but directory durability could
    /// not be confirmed. The new generation is retained for reopen recovery.
    PublicationIndeterminate,
    /// Another process replaced the selected generation before admission.
    SupersededGeneration,
    /// Exact stage cleanup failed after a seed or publication error.
    StageCleanup {
        /// Original stage/publication failure description.
        cause: String,
        /// Cleanup failure from the exact creation receipt.
        cleanup: std::io::Error,
    },
    /// A local filesystem operation failed while managing the namespace.
    Filesystem(std::io::Error),
    /// A bounded namespace allocation failed.
    Allocation,
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
            Self::StaleTransition => formatter.write_str(
                "Turso projection operation has a stale selected root or source revision",
            ),
            Self::RowCountOverflow => formatter.write_str("projection row count overflows i64"),
            Self::GraphRowCountOverflow => {
                formatter.write_str("package graph row count overflows i64")
            }
            Self::CorruptMetadata { field } => {
                write!(formatter, "projection metadata field {field} is invalid")
            }
            Self::Sharing(refusal) => refusal.fmt(formatter),
            Self::NeedsSeed => formatter.write_str(
                "Turso projection has no selected generation; open it with a complete seed",
            ),
            Self::CorruptNamespace { field } => {
                write!(formatter, "Turso projection namespace field {field} is invalid")
            }
            Self::NamespaceIdentity => formatter.write_str(
                "Turso projection namespace object no longer has its admitted identity",
            ),
            Self::GenerationLimit { maximum } => write!(
                formatter,
                "Turso projection retained-generation limit reached ({maximum})"
            ),
            Self::GenerationIdExhausted => {
                formatter.write_str("Turso projection generation identifiers are exhausted")
            }
            Self::NamespaceBusy => {
                formatter.write_str("Turso projection generation handoff is busy")
            }
            Self::PublicationIndeterminate => formatter.write_str(
                "Turso projection selector changed but its durability is indeterminate",
            ),
            Self::SupersededGeneration => formatter.write_str(
                "Turso projection handle belongs to a superseded generation",
            ),
            Self::StageCleanup { cause, cleanup } => write!(
                formatter,
                "Turso projection stage failed ({cause}) and exact cleanup failed ({cleanup})"
            ),
            Self::Filesystem(error) => error.fmt(formatter),
            Self::Allocation => formatter.write_str("Turso projection namespace allocation failed"),
        }
    }
}

impl std::error::Error for ProjectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Sharing(refusal) => Some(refusal),
            Self::Filesystem(error) => Some(error),
            Self::StageCleanup { cleanup, .. } => Some(cleanup),
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
