//! Diagram: **postgres handles all metadata associated with a package, including
//! its canonical GUID and the hash of its code/treesitter representation.**
//!
//! TDD specs (`todo!()`) for `registry::metadata`.

/// A package is assigned a canonical, version-agnostic GUID.
///
/// Assert: registering a package yields a stable GUID recorded in postgres.
#[tokio::test]
async fn package_gets_a_canonical_guid() {
    todo!("assert a canonical GUID is minted + persisted");
}

/// The same package re-registered keeps the same GUID.
///
/// Assert: GUID assignment is idempotent (deterministic identity).
#[tokio::test]
async fn guid_is_stable_across_reregistration() {
    todo!("assert GUID stability across re-registration");
}

/// The code/treesitter hash is recorded alongside the GUID.
///
/// Assert: metadata stores a content hash of the package's code/treesitter
///   representation.
#[tokio::test]
async fn code_treesitter_hash_is_recorded() {
    todo!("assert the code/treesitter hash is persisted");
}

/// A changed representation produces a different hash (freshness signal).
///
/// Assert: re-parsing changed source yields a different hash, which is what
///   drives re-indexing decisions.
#[tokio::test]
async fn changed_representation_changes_the_hash() {
    todo!("assert hash changes when the representation changes");
}

/// Metadata links the GUID to every cross-store identity.
///
/// Assert: from the GUID you can reach the blob ref, graph URI, and vector ids.
#[tokio::test]
async fn metadata_links_guid_to_cross_store_ids() {
    todo!("assert GUID -> cross-store id links");
}
