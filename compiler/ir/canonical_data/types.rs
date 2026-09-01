//! Caller-owned semantic lanes and canonicalization resource facts.

use crate::{
    AtomId, ListSpan, ProductChildren, ProductId, SemanticAtom, SemanticProduct,
    SemanticProductChild, SemanticProductConstructor,
};

/// Widens one validated dense coordinate to its native lane position.
///
/// The crate gate rejects address widths below 32 bits, so every `u32`
/// coordinate is representable and validation proves each coordinate is
/// inside its lane before this projection runs.
#[allow(
    clippy::as_conversions,
    reason = "u32 coordinates are admitted by the 32-bit-address-space crate gate and validated against lane lengths"
)]
pub(super) const fn lane_index(raw: u32) -> usize {
    raw as usize
}

/// Borrowed semantic facts supplied to the canonicalizer.
///
/// The four lanes remain owned by the caller. The canonicalizer reads them,
/// validates every coordinate, and writes only to the caller-provided output
/// lanes after all admission checks have passed.
#[derive(Clone, Copy)]
pub struct DataFacts<'facts, 'bytes> {
    /// Atoms referenced by product heads.
    pub atoms: &'facts [SemanticAtom<'bytes>],
    /// Recursive product facts.
    pub products: &'facts [SemanticProduct],
    /// Product-aligned closed constructor payloads.
    pub constructors: &'facts [SemanticProductConstructor],
    /// Product-list spans into [`Self::children`].
    pub lists: &'facts [ListSpan<ProductChildren>],
    /// One pooled, ordered, role-bearing child lane shared by all product lists.
    pub children: &'facts [SemanticProductChild],
}

/// Caller-owned scratch lanes used by canonical preparation.
///
/// No lane is allocated by the canonicalizer. `intern_slots` is an open
/// addressing table whose entries are source product ordinals plus one; zero
/// is the empty marker. The caller chooses all capacities and can reuse the
/// same region across fragments. Resource, capacity, and validation failures
/// are admitted before any lane is written, so a failed call leaves every
/// scratch and output cell byte-for-byte unchanged. A successful call may
/// overwrite only the prefixes needed by the canonical result; suffixes stay
/// untouched.
pub struct DataScratch<'scratch> {
    /// Source atom order, sorted by atom bytes.
    pub atom_order: &'scratch mut [AtomId],
    /// Source atom to canonical atom mapping.
    pub atom_to_canonical: &'scratch mut [u32],
    /// Source product order, sorted by refined structural key.
    pub product_order: &'scratch mut [ProductId],
    /// Source product to canonical product mapping.
    pub product_to_canonical: &'scratch mut [u32],
    /// Current recursive equivalence colors.
    pub colors: &'scratch mut [u32],
    /// Next recursive equivalence colors.
    pub next_colors: &'scratch mut [u32],
    /// Current structural hashes.
    pub hashes: &'scratch mut [u64],
    /// Next structural hashes.
    pub next_hashes: &'scratch mut [u64],
    /// Canonical representative for each interned product.
    pub product_representatives: &'scratch mut [ProductId],
    /// Caller-sized open-addressing hash-cons table. The canonicalizer admits
    /// only the prefix through `min(self.len(), source_product_count)`; any
    /// suffix remains untouched and is excluded from measured work.
    pub intern_slots: &'scratch mut [u64],
}

/// Caller-owned output lanes. The canonicalizer borrows the written prefixes;
/// unused suffixes are never modified.
pub struct DataOutput<'output, 'bytes> {
    /// Canonical atom lane.
    pub atoms: &'output mut [SemanticAtom<'bytes>],
    /// Canonical product lane.
    pub products: &'output mut [SemanticProduct],
    /// Canonical product-aligned constructor lane.
    pub constructors: &'output mut [SemanticProductConstructor],
    /// Canonical pooled-list table.
    pub lists: &'output mut [ListSpan<ProductChildren>],
    /// Canonical pooled, ordered, role-bearing product-child lane.
    pub children: &'output mut [SemanticProductChild],
}

/// Resource class controlled by [`DataResourceBudget`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataResource {
    /// Refinement iterations over the recursive product lane.
    RefinementRounds,
    /// Comparisons performed by atom/product sorting and structural keys.
    SortComparisons,
    /// Structural hash evaluations.
    HashEvaluations,
    /// Open-addressing probes in the caller's intern table.
    InternProbes,
    /// Aggregate measured logical work.
    Work,
}

/// Explicit caller-selected limits for one canonicalization operation.
///
/// The canonicalizer reports actual counters in [`DataCanonicalization`]. A
/// caller may derive these finite limits from a measured corpus and reject a
/// larger graph before it mutates output. The budget is mandatory so callers
/// choose the resource envelope explicitly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DataResourceBudget {
    /// Maximum refinement rounds.
    pub max_refinement_rounds: u32,
    /// Maximum sort/structural comparisons.
    pub max_sort_comparisons: u64,
    /// Maximum hash evaluations.
    pub max_hash_evaluations: u64,
    /// Maximum intern-table probes.
    pub max_intern_probes: u64,
    /// Maximum aggregate logical work.
    pub max_work: u64,
}

/// Measured canonicalization counters and output cardinalities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DataCanonicalization {
    /// Number of input atoms.
    pub source_atom_count: u32,
    /// Number of input products.
    pub source_product_count: u32,
    /// Number of input product constructors.
    pub source_constructor_count: u32,
    /// Number of input list facts.
    pub source_list_count: u32,
    /// Number of input pooled children.
    pub source_child_count: u32,
    /// Number of canonical atoms.
    pub canonical_atom_count: u32,
    /// Number of canonical products.
    pub canonical_product_count: u32,
    /// Number of canonical product constructors.
    pub canonical_constructor_count: u32,
    /// Number of canonical pooled lists.
    pub canonical_list_count: u32,
    /// Number of canonical pooled children.
    pub canonical_child_count: u32,
    /// Fixed-point refinement rounds performed.
    pub refinement_rounds: u32,
    /// Atom/product structural sort comparisons.
    pub sort_comparisons: u64,
    /// Structural hash evaluations.
    pub hash_evaluations: u64,
    /// Open-addressing probes.
    pub intern_probes: u64,
    /// Aggregate logical work (`hash + compare + probe + lane visits`).
    pub work: u64,
}
