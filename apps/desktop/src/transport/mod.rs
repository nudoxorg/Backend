//! The certified bounded cursor subscription and the shared command seams.
//! Behaviour here is unchanged from the surface this rewrite replaced; only
//! the module boundaries moved, so the admission proofs stay exactly as tested.
//!
//! The desktop never becomes an authoritative cache. Every event is admitted
//! against the exact prior root, a gap forces an explicit bounded reset, and
//! an acknowledgement is never treated as an empty suffix. The reducer that
//! consumes this lives next door in `reducer`; everything in this module is
//! transport.

pub(crate) mod error;
pub(crate) mod limits;
pub(crate) mod subscription;

#[cfg(any(unix, windows))]
pub(crate) mod diff;
#[cfg(any(unix, windows))]
pub(crate) mod search;
pub(crate) mod unix;
