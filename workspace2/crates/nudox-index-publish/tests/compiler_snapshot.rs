#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! Compiler-to-index public ownership journey.

#[path = "compiler_snapshot/mod.rs"]
mod suite;
