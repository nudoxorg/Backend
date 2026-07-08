//! Shared rustdoc-JSON → IR lowering logic.
//!
//! Used by both `compiler` (via `compile::rust::*` re-exports) and the
//! `rustdoc-driver` binary which runs the doc pass in-process.

pub mod context;
pub mod error;
pub mod function;
pub mod generics;
pub mod item;
pub mod types;

pub type Result<T> = std::result::Result<T, error::Parse>;

pub(crate) fn empty_to_none<T>(v: Vec<T>) -> Option<Vec<T>> {
    if v.is_empty() { None } else { Some(v) }
}
