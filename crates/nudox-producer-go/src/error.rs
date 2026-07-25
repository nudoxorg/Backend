//! Error types for the Go producer.

use thiserror::Error;

/// All errors the Go producer can surface.
#[derive(Debug, Error)]
pub enum GoError {
    /// The oracle subprocess failed or produced invalid JSON.
    #[error("oracle error: {detail}")]
    Oracle { detail: String },

    /// The oracle output failed JSON deserialization.
    #[error("oracle JSON deserialization failed: {0}")]
    Json(#[from] serde_json::Error),

    /// The lowering step encountered an error (cycle, undeclared, duplicate).
    #[error("lowering error: {detail}")]
    Lowering { detail: String },

    /// A Go construct that the IR cannot yet represent.
    ///
    /// Callers should log this and continue; the symbol is omitted from
    /// the output rather than silently dropped.
    #[error("unsupported Go construct `{symbol}`: {reason}")]
    Unsupported { symbol: String, reason: &'static str },
}

pub type Result<T> = std::result::Result<T, GoError>;
