//! Error types for the C# producer.

use thiserror::Error;

/// A top-level error from the C# producer.
///
/// Unlike the old code which had many granular error types split across
/// `OracleError`, `ExtractionError`, `DotnetError`, etc., we collapse into
/// three buckets here: the oracle subprocess failed, the oracle output was
/// unacceptable, or a symbol construct cannot be represented in the IR.
/// The producer trait contract requires a single [`crate::ProducerError`]
/// type; this crate-local [`Error`] backs the two standalone entry points
/// (`parse_extraction`, `lower`) that run without the trait boundary.
#[derive(Debug, Error)]
pub enum Error {
    /// The oracle subprocess could not be launched or reported failure.
    #[error("oracle subprocess error")]
    Oracle { message: String },

    /// The oracle's JSON could not be deserialized.
    #[error(transparent)]
    Deserialize(#[from] serde_json::Error),

    /// The oracle emitted a format version we do not understand.
    #[error("unsupported oracle format version")]
    UnsupportedFormat { format: u32 },

    /// The oracle ran but produced no types.
    #[error("oracle produced no types")]
    NoTypes,

    /// A C# construct cannot be represented in the current IR.
    ///
    /// The `symbol` field carries the doc-id of the offending symbol
    /// (`T:Ns.Type`, `M:Ns.Type.Method(…)`, etc.) so the caller can report it
    /// precisely.  We return `Unsupported` rather than silently dropping the
    /// symbol so that IR gaps are visible and tracked.
    #[error("unsupported construct")]
    Unsupported { symbol: String, reason: String },

    /// No source files were found under the provided roots.
    #[error("no C# source files found")]
    NoSources,
}

impl Error {
    pub fn oracle(msg: impl Into<String>) -> Self {
        Error::Oracle {
            message: msg.into(),
        }
    }

    pub fn unsupported(symbol: impl Into<String>, reason: impl Into<String>) -> Self {
        Error::Unsupported {
            symbol: symbol.into(),
            reason: reason.into(),
        }
    }
}
