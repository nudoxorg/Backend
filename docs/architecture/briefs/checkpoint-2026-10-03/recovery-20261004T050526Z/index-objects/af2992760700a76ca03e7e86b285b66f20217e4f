//! Failures from the selected package-index authority.

use std::{fmt, path::PathBuf};

/// Failure to persist, reopen, or advance selected index authority.
#[derive(Debug)]
pub enum AuthorityError {
    /// Turso rejected an operation.
    Database(turso::Error),
    /// A database path cannot be represented by the local Turso runtime.
    NonUtf8Path(PathBuf),
    /// Persisted authority schema is not supported by this binary.
    Schema { found: i64 },
    /// One namespace component is empty, too long, or contains NUL.
    InvalidNamespace,
    /// Observation time/count exceeds the SQL integer domain or has invalid data.
    InvalidObservation,
    /// A checked numeric value cannot fit the SQL integer domain.
    IntegerOverflow,
    /// Observation belongs to another scope or is no longer current.
    StaleObservation,
    /// The attempt was superseded, already selected, or no longer owns its epoch.
    StaleAttempt,
    /// The selected head changed after the attempt was acquired.
    StaleFrontier,
    /// The requested immutable selected generation does not exist in this scope.
    GenerationNotFound,
    /// History pagination must request between one and 512 rows.
    InvalidHistoryPageLimit,
    /// The retained generation differs from the latest source/input frontier; explicit acknowledgement is required.
    HistoricalSelectionRequiresAcknowledgement,
    /// A closure verifier or receipt named a different candidate/closure.
    ClosureMismatch,
    /// Compiler output was not verified against the exact Turso-admitted input attempt.
    AdmittedInputMismatch,
    /// Independent durable closure verification failed.
    ClosureVerification(String),
    /// Persisted authority bytes cannot represent a valid typed answer.
    CorruptRecord(&'static str),
}

impl fmt::Display for AuthorityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => error.fmt(formatter),
            Self::NonUtf8Path(path) => {
                write!(
                    formatter,
                    "Turso authority path is not UTF-8: {}",
                    path.display()
                )
            }
            Self::Schema { found } => {
                write!(formatter, "unsupported Turso authority schema {found}")
            }
            Self::InvalidNamespace => formatter.write_str("invalid Turso authority namespace"),
            Self::InvalidObservation => formatter.write_str("invalid source observation"),
            Self::IntegerOverflow => {
                formatter.write_str("authority value overflows SQLite integer")
            }
            Self::StaleObservation => formatter.write_str("source observation was superseded"),
            Self::StaleAttempt => {
                formatter.write_str("candidate attempt was superseded or already selected")
            }
            Self::StaleFrontier => {
                formatter.write_str("selected package frontier changed during compilation")
            }
            Self::GenerationNotFound => {
                formatter.write_str("selected package generation was not retained")
            }
            Self::InvalidHistoryPageLimit => {
                formatter.write_str("selected-generation history page limit must be 1..=512")
            }
            Self::HistoricalSelectionRequiresAcknowledgement => formatter.write_str(
                "retained generation is historical; explicit historical-selection acknowledgement is required",
            ),
            Self::ClosureMismatch => {
                formatter.write_str("verified closure does not match the candidate")
            }
            Self::AdmittedInputMismatch => {
                formatter.write_str("compiler output does not match its admitted input attempt")
            }
            Self::ClosureVerification(error) => {
                write!(formatter, "durable closure verification failed: {error}")
            }
            Self::CorruptRecord(field) => {
                write!(formatter, "malformed persisted authority field {field}")
            }
        }
    }
}

impl std::error::Error for AuthorityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

impl From<turso::Error> for AuthorityError {
    fn from(error: turso::Error) -> Self {
        Self::Database(error)
    }
}
