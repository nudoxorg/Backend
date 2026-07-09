//! Diagram (indexing): **runs computer · generate blob information.**
//!
//! TDD specs (`todo!()`) for `compiler::generate` — producing the three
//! resolutions and the canonical code/treesitter hash that becomes a package's
//! postgres identity.

/// Generation produces all three resolutions for a package.
///
/// Assert: the CST (`generate::cst`), API surface (`generate::surface`), and
///   tarred source (`generate::source_archive`) are all produced.
#[ignore = "TDD stub — not yet implemented"]
#[test]
fn generates_cst_surface_and_archive() {
    todo!("assert all three resolutions are generated");
}

/// Generation emits linked data for the graph store.
///
/// Assert: `generate::linked_data` produces the Entry + Kind documents for
///   terminus.
#[ignore = "TDD stub — not yet implemented"]
#[test]
fn generates_linked_data_documents() {
    todo!("assert linked-data documents are generated");
}

/// Blob info carries the canonical code/treesitter hash.
///
/// Assert: `generate::blob_info` computes a stable hash of the
///   code/treesitter representation (the value postgres records as identity).
#[ignore = "TDD stub — not yet implemented"]
#[test]
fn blob_info_carries_canonical_hash() {
    todo!("assert blob info includes the canonical hash");
}

/// Identical input yields an identical hash (reproducibility).
///
/// Assert: regenerating from the same source produces the same hash.
#[ignore = "TDD stub — not yet implemented"]
#[test]
fn hash_is_reproducible() {
    todo!("assert reproducible hashing");
}
