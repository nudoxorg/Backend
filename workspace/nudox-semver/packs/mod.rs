//! Law pack modules.
//!
//! Each pack is a collection of pure lint functions that together implement one
//! layer of the semver classification spec (§9.4–§9.6). The packs are language-
//! agnostic in their interfaces; Rust-specific semantics live in their
//! `doc`-comments and precondition checks.
//!
//! Current packs:
//! - [`a`] — Pack A: CSC parity (A-1 … A-19, §9.4)
//!
//! Planned but not yet drafted:
//! - Pack B — type / generics lints (§9.5)
//! - Pack C — cross-crate re-exports (§9.6)
//! - Pack D — feature / target lattice (§9.7)

pub mod a;
