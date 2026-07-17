//! Error type for `nudox-ir-vcs`.

use thiserror::Error;

/// Unified error type for all VCS operations.
#[derive(Debug, Error)]
pub enum VcsError {
    /// Catch-all for libpijul errors (apply, record, output, txn).
    ///
    /// libpijul's error types are not easily `std::error::Error + Send + Sync`,
    /// so we convert via `anyhow` before storing here.
    #[error("libpijul error: {0}")]
    Pijul(#[from] anyhow::Error),

    /// Postcard serialization/deserialization failure.
    #[error("serialization error: {0}")]
    Serialize(#[from] postcard::Error),

    /// Archive sealing failure.
    #[error("seal error: {0}")]
    Seal(#[from] nudox_ir_archive::SealError),

    /// A `symbols/*` file contained unexpected or empty content.
    #[error("corrupt symbol file at path '{path}': {reason}")]
    CorruptSymbolFile { path: String, reason: String },
}
