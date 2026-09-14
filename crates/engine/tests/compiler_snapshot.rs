//! Exercises the `backend-engine index_publish` tests compiler-snapshot contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! Compiler-to-index public ownership journey.

#[path = "compiler_snapshot/mod.rs"]
mod suite;
