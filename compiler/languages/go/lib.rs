//! Typed adapter for the vendored Go semantic oracle.
//! It owns protocol decoding and child-process resource bounds.
//! Semantic lowering remains outside this adapter.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(
    missing_docs,
    reason = "the vendored oracle protocol mirrors external JSON fields"
)]

pub mod oracle;

pub use oracle::{GoOracle, OracleError, Output};
