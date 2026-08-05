//! [`PackageDelta`] and [`PartialDelta`] — the top-level structural delta (§7.2).
//!
//! A `PackageDelta` is a deterministic, content-addressed record of every
//! [`IrOp`] emitted for each [`IntroId`] when comparing two
//! [`PristineIntroTable`] generations. The companion [`PartialDelta`] marks
//! whether the delta was computed from a checkpoint (provisional) tip or from a
//! `finish()` tip.
//!
//! # Canonical bytes and digest
//!
//! [`PackageDelta::canonical_bytes`] serializes the delta in a deterministic
//! order (by `IntroId` bytes, then by op sort key within each id) and
//! [`delta_digest`] computes `blake3("nudox.delta.v1" || canonical_bytes)`.
//! This digest is stored in [`crate::diff::nudox_ir_vcs::GenerationMeta::delta_digest`]
//! and verified by the `verify` mode.

use std::collections::BTreeMap;

use crate::vcs_types::ChangeSetFingerprint;
use ir::change::{ContentBlake3, IntroId};
use serde::{Deserialize, Serialize};

use crate::diff::ir_op::{IrOp, op_sort_key};

// ---------------------------------------------------------------------------
// PartialDelta
// ---------------------------------------------------------------------------

/// Annotation attached to a [`PackageDelta`] computed from a checkpoint tip
/// (§5.6) rather than from a `finish()` tip.
///
/// - `deletions_valid = false`: the tip was a checkpoint, so deleted entries
///   may not yet have been processed; any `Deleted` ops in this delta are
///   provisional.
/// - `identity_final = false`: the σ substitution (§5.1 Phase B) has not been
///   applied; continuity ops (Renamed/Moved/SignatureEvolved) reflect wire ids,
///   not yet durable ids.
///
/// Publishing an `ApiReport` from a `PartialDelta` is forbidden (acceptance V-4).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PartialDelta {
    /// False when the tip is a checkpoint: deletions may not yet be final.
    pub deletions_valid: bool,
    /// False when σ has not been applied: ids in continuity ops may be wire ids.
    pub identity_final: bool,
}

// ---------------------------------------------------------------------------
// PackageDelta
// ---------------------------------------------------------------------------

/// The full structural delta between two package IR generations (§7.2).
///
/// Produced by [`crate::diff::diff::diff_tables`] and consumed by the semver
/// classifier (`nudox-semver`), the archive, and the GUI. The `ops` map is
/// keyed by `IntroId`; within each entry's `Vec<IrOp>` the ops are sorted in
/// canonical order (lifecycle < continuity < meta < kind-specific < links;
/// §7.3).
///
/// # Determinism
///
/// The `BTreeMap` key order and the per-id op order are both canonical and
/// deterministic. Two calls to `diff_tables(T0, T1)` with the same inputs
/// must produce byte-identical `canonical_bytes()`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PackageDelta {
    /// The tip [`ChangeSetFingerprint`] of the baseline generation (T0).
    pub from: ChangeSetFingerprint,
    /// The tip [`ChangeSetFingerprint`] of the new generation (T1).
    pub to: ChangeSetFingerprint,
    /// Per-intro op lists, sorted by `IntroId` bytes (BTreeMap is key-sorted).
    /// Entries with no ops are not stored.
    pub ops: BTreeMap<IntroId, Vec<IrOp>>,
    /// `None` for finish-to-finish deltas; `Some` for checkpoint deltas (§5.6).
    pub partial: Option<PartialDelta>,
}

impl PackageDelta {
    /// Domain tag for the delta digest (frozen).
    pub const DELTA_DOMAIN: &'static str = "nudox.delta.v1";

    /// Serialize the delta into a deterministic, canonical byte sequence.
    ///
    /// Layout:
    /// ```text
    /// from_bytes(32)
    /// to_bytes(32)
    /// partial_flag(u8): 0=None, 1=deletions_valid+identity_final bits
    /// if partial: deletions_valid(u8) || identity_final(u8)
    /// entry_count(u64le)
    /// for each (IntroId, ops) in BTreeMap order:
    ///   intro_id_bytes(32)
    ///   op_count(u32le)
    ///   for each op (in op_sort_key order, then insertion order for ties):
    ///     postcard_op(varlen) prefixed by u32le length
    /// ```
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();

        // from / to fingerprints
        out.extend_from_slice(self.from.as_bytes());
        out.extend_from_slice(self.to.as_bytes());

        // partial delta annotation
        match &self.partial {
            None => out.push(0u8),
            Some(p) => {
                out.push(1u8);
                out.push(p.deletions_valid as u8);
                out.push(p.identity_final as u8);
            }
        }

        // entry count
        let entry_count = self.ops.len() as u64;
        out.extend_from_slice(&entry_count.to_le_bytes());

        // entries — BTreeMap guarantees IntroId byte order
        for (intro, ops) in &self.ops {
            out.extend_from_slice(intro.as_bytes());

            // Sort ops by sort key; stable sort preserves insertion order for
            // ties (added/removed lists within one variant are already ordered
            // by diff_tables).
            let mut sorted = ops.clone();
            sorted.sort_by_key(op_sort_key);

            out.extend_from_slice(&(sorted.len() as u32).to_le_bytes());
            for op in &sorted {
                let op_bytes = postcard::to_allocvec(op)
                    .expect("IrOp postcard serialization is infallible for in-memory data");
                out.extend_from_slice(&(op_bytes.len() as u32).to_le_bytes());
                out.extend_from_slice(&op_bytes);
            }
        }

        out
    }

    /// Compute the delta digest: `blake3("nudox.delta.v1" || canonical_bytes())`.
    ///
    /// Stored in `GenerationMeta::delta_digest` (§7.6). Advisory: the table
    /// wins if recomputation disagrees (§3.4).
    pub fn delta_digest(&self) -> ContentBlake3 {
        delta_digest(self)
    }
}

// ---------------------------------------------------------------------------
// Free function form (also exported from crate root)
// ---------------------------------------------------------------------------

/// Compute `blake3("nudox.delta.v1" || canonical_bytes)` for a delta.
///
/// Equivalent to [`PackageDelta::delta_digest`]; the free-function form is
/// convenient for `verify` pipelines that compare a recomputed digest to one
/// stored in change metadata.
pub fn delta_digest(delta: &PackageDelta) -> ContentBlake3 {
    let canonical = delta.canonical_bytes();
    ContentBlake3::from_domain(PackageDelta::DELTA_DOMAIN, &canonical)
}
