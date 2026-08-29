//! Non-production representation experiments used by `layout-lab` binaries and tests.
//!
//! None of the types in this crate are exported by a shipping Nudox crate.  The unsafe queue is
//! intentionally a laboratory candidate: its proof and losing/winning evidence live beside it.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod experiments;
