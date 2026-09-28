//! Compatibility entry point for the shared recorded-callable authority.
use crate::semantics::model::Pipe;

/// A captured Rust declaration projected with the same authority as symbol pages.
#[must_use]
pub fn recorded_pipe(signature: &str) -> Option<Pipe> {
    crate::semantics::recorded::callable(signature, "", crate::semantics::recorded::Language::Rust)
}
