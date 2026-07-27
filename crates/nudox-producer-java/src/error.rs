//! Error type for the Java producer.
//!
//! This crate does not yet implement the full `nudox-producer` trait contract
//! (that crate does not exist yet). This module provides the error type that
//! `lower.rs` will eventually return, defining the interface boundary for
//! when the trait crate materialises.

use nudox_ir::lower::LoweringError;

use crate::lower::JavaId;

/// Errors that can occur during Java oracle extraction or IR lowering.
#[derive(Debug, thiserror::Error)]
pub enum ProducerError {
    /// The oracle process failed to produce output.
    #[error("oracle invocation failed: {0}")]
    Oracle(String),

    /// The oracle's JSON output could not be parsed.
    #[error("oracle output parse error: {0}")]
    Parse(#[from] serde_json::Error),

    /// IR lowering failed (duplicate ids, undeclared refs, or parent cycles).
    #[error("IR lowering failed: {0}")]
    Lowering(#[from] LoweringError<JavaId>),

    /// A Java construct is not representable in the current IR and is dropped.
    ///
    /// Carry the fully-qualified name of the offending symbol plus a short
    /// description of the unsupported construct.
    #[error("unsupported construct in `{symbol}`: {description}")]
    Unsupported {
        /// Fully-qualified name of the symbol where the gap was found.
        symbol: String,
        /// One-line description of the unsupported construct.
        description: String,
    },

    /// I/O error when reading or writing the oracle JSON file.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl ProducerError {
    /// Convenience constructor for `Unsupported`.
    pub fn unsupported(symbol: impl Into<String>, description: impl Into<String>) -> Self {
        ProducerError::Unsupported {
            symbol: symbol.into(),
            description: description.into(),
        }
    }
}
