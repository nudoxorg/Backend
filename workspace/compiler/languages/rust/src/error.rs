//! Error types for the Rust producer.
//!
//! These are internal; the `Producer` trait boundary converts them into
//! `ProducerError` variants.  Keeping them distinct gives better diagnostics
//! during development.

use thiserror::Error;

/// Any failure that can occur during workspace load or HIR lowering.
#[derive(Debug, Error)]
pub enum Error {
    /// `ra_ap_load_cargo` / cargo-metadata / crate-graph construction failed.
    #[error("workspace load failed")]
    Load(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Salsa emitted a `Cancelled` panic during prime or walk — retryable.
    #[error("rust-analyzer analysis cancelled")]
    Cancelled,

    /// The proc-macro server was unavailable; macro-generated items may be missing.
    /// This is a soft-degradation warning rather than a hard failure.
    #[error("proc-macro server degraded")]
    ProcMacroDegraded(String),

    /// A Rust HIR construct that the producer does not yet know how to lower.
    ///
    /// Carries the canonical path of the offending symbol (when known) and a
    /// short description.  The symbol is omitted from the output rather than
    /// aborting the whole package.
    #[error("unsupported construct")]
    UnsupportedConstruct { path: String, description: String },

    /// An item lowering produced a structural problem (duplicate id, cycle, …).
    ///
    /// This indicates a bug in the producer's `lower` implementation.
    #[error("lowering bug")]
    LoweringBug(String),
}

/// Backward-compatible alias.
pub type RustProducerError = Error;
