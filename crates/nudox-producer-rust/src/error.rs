//! Error types for the Rust producer.
//!
//! These are internal; the `Producer` trait boundary converts them into
//! `ProducerError` variants.  Keeping them distinct gives better diagnostics
//! during development.

use std::fmt;

/// Any failure that can occur during workspace load or HIR lowering.
#[derive(Debug)]
pub enum RustProducerError {
    /// `ra_ap_load_cargo` / cargo-metadata / crate-graph construction failed.
    Load(String),

    /// Salsa emitted a `Cancelled` panic during prime or walk — retryable.
    Cancelled,

    /// The proc-macro server was unavailable; macro-generated items may be missing.
    /// This is a soft-degradation warning rather than a hard failure.
    ProcMacroDegraded(String),

    /// A Rust HIR construct that the producer does not yet know how to lower.
    ///
    /// Carries the canonical path of the offending symbol (when known) and a
    /// short description.  The symbol is omitted from the output rather than
    /// aborting the whole package.
    UnsupportedConstruct { path: String, description: String },

    /// An item lowering produced a structural problem (duplicate id, cycle, …).
    ///
    /// This indicates a bug in the producer's `lower` implementation.
    LoweringBug(String),
}

impl fmt::Display for RustProducerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RustProducerError::Load(msg) => write!(f, "workspace load failed: {msg}"),
            RustProducerError::Cancelled => write!(f, "rust-analyzer analysis cancelled (retryable)"),
            RustProducerError::ProcMacroDegraded(msg) => {
                write!(f, "proc-macro server degraded: {msg}")
            }
            RustProducerError::UnsupportedConstruct { path, description } => {
                write!(f, "unsupported construct at `{path}`: {description}")
            }
            RustProducerError::LoweringBug(msg) => write!(f, "lowering bug: {msg}"),
        }
    }
}

impl std::error::Error for RustProducerError {}
