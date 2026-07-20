//! # nudox-ir-diff — matcher-free structural delta over IR generations (P3).
//!
//! This crate implements the `PackageDelta` / `IrOp` layer of the Semantic IR
//! VCS plan (§7). Given two materialized [`nudox_ir::apply::PristineIntroTable`]s
//! it computes a typed, deterministic [`PackageDelta`] — a
//! `BTreeMap<IntroId, Vec<IrOp>>` — and provides an `apply_delta` round-trip
//! checker.
//!
//! ## Design contract
//!
//! - **Matcher-free**: `diff_tables(T0, T1)` compares by durable `IntroId` only.
//!   Continuity (rename/move/sig-key) was decided at record time and is
//!   re-derived from payload comparison here, not by re-running the matcher.
//! - **Engine-neutral**: no link to `libpijul`; the delta is a pure value.
//! - **Deterministic**: same T0/T1 always produces byte-identical
//!   `canonical_bytes()` and `delta_digest()`.
//! - **Round-trip law** (§3.4 / §7.5):
//!   `apply_delta_with_t1(T0, diff(T0, T1), Some(T1)) == T1`
//!   tested per fixture pair in the `tests` module.
//!
//! ## Quick usage
//!
//! ```ignore
//! use crate::diff::{diff_tables, PackageDelta, delta_digest};
//! use nudox_ir::change::ChangeSetFingerprint;
//!
//! let delta: PackageDelta = diff_tables(&t0, &t1, from_cset, to_cset, None);
//! let digest = delta.delta_digest();
//! ```
//!
//! ## Crate layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`ir_op`] | `IrOp` enum, `SigKey`, `GenericsDelta`, `WherePred`, `op_sort_key` |
//! | [`delta`] | `PackageDelta`, `PartialDelta`, `delta_digest` |
//! | [`diff`] | `diff_tables`, `diff_tables_partial` |
//! | [`apply`] | `apply_delta`, `apply_delta_with_t1`, `ApplyError` |

pub mod apply;
pub mod delta;
pub mod diff;
pub mod ir_op;

// ---------------------------------------------------------------------------
// Re-exports (the crate's public API surface)
// ---------------------------------------------------------------------------

pub use apply::{ApplyError, apply_delta, apply_delta_with_t1};
pub use delta::{PackageDelta, PartialDelta, delta_digest};
pub use diff::{diff_tables, diff_tables_partial};
pub use ir_op::{GenericsDelta, IrOp, SigKey, WherePred};

#[cfg(test)]
mod tests;
