//! Diagram: **postgres handles all metadata associated with a package, including
//! its canonical GUID and the hash of its code/treesitter representation.**
//!
//! TDD specs for `registry::metadata`. GUID assignment is deterministic (not
//! allocated), so identity facts run pure; the postgres persistence halves are
//! gated on `REGISTRY_TEST_POSTGRES`/`DATABASE_URL`.

mod common;

use heart::ResolutionState;
use registry::{
    Store,
    identity::Minter,
    metadata::{PackageMetadata, StoreLinks, hash},
    schema::codec,
};

/// A package is assigned a canonical, version-agnostic GUID.
///
/// Assert: registering a package yields a stable GUID recorded in postgres.
#[tokio::test]
async fn package_gets_a_canonical_guid() {
    // Pure: the GUID is minted deterministically from the coordinates — the
    // Minter and the coordinates agree, and re-minting is the identity.
    let package = common::rust_package("serde", "1.0.0");
    let minter = Minter::new(common::test_instance());
    let guid = minter.package_id(&package.coordinates);
    assert_eq!(guid, package.id(), "the minter must delegate to the coordinate fingerprint");
    assert_eq!(guid, minter.package_id(&common::rust_coordinates("serde", "1.0.0")));
    assert_eq!(guid.as_uuid().get_version_num(), 5, "package GUIDs are UUIDv5 fingerprints");

    // Gated: registering persists the identity row under that GUID.
    let Some(pool) = common::postgres_pool("package_gets_a_canonical_guid").await else { return };
    let store = common::global_store(pool).await;
    let registered = common::rust_package(&common::unique_rust_name("guid"), "1.0.0");
    store
        .upsert(&common::global_package(
            registered.clone(),
            ResolutionState::Unindexed { needed: false },
        ))
        .await
        .expect("registration succeeds");
    let fetched = store.get(registered.id()).await.expect("the registered package resolves");
    assert_eq!(fetched.id, registered.id(), "the persisted record must carry the minted GUID");
    assert_eq!(fetched.package.coordinates, registered.coordinates);
}

/// The same package re-registered keeps the same GUID.
///
/// Assert: GUID assignment is idempotent (deterministic identity).
#[tokio::test]
async fn guid_is_stable_across_reregistration() {
    // Pure: identity is a recomputation, so two independent registrations of
    // the same coordinates cannot disagree — including across the crates.io
    // `_`/`-` naming equivalence.
    assert_eq!(
        common::rust_coordinates("serde-json", "1.0.0").id(),
        common::rust_coordinates("serde_json", "1.0.0").id(),
        "name normalization must fold into one GUID"
    );

    // Gated: re-registering upserts the same row rather than forking identity.
    let Some(pool) = common::postgres_pool("guid_is_stable_across_reregistration").await else {
        return;
    };
    let store = common::global_store(pool).await;
    let package = common::rust_package(&common::unique_rust_name("stable"), "1.0.0");
    let record =
        common::global_package(package.clone(), ResolutionState::Unindexed { needed: false });
    store.upsert(&record).await.expect("first registration succeeds");
    store.upsert(&record).await.expect("re-registration is an idempotent upsert");
    let fetched = store.get(package.id()).await.expect("one coherent record resolves");
    assert_eq!(fetched.id, package.id(), "re-registration must return the existing GUID");
}

/// The code/treesitter hash is recorded alongside the GUID.
///
/// Assert: metadata stores a content hash of the package's code/treesitter
///   representation.
#[tokio::test]
async fn code_treesitter_hash_is_recorded() {
    let package = common::rust_package("serde", "1.0.0");
    let (manifest, _sections) = common::built_manifest(&package);
    let files: Vec<registry::FileEntry> = manifest.files.iter().cloned().collect();
    let representation_hash = hash::package_generation(&files);

    // Pure: the persisted column projection of `Stored { hash }` carries the
    // hash losslessly — exactly 32 bytes in, the same hash back out.
    let stored = ResolutionState::Stored { hash: representation_hash };
    let columns = codec::state_to_columns(&stored).expect("Stored projects onto columns");
    let recorded = columns.content_hash.expect("Stored must persist its content hash");
    assert_eq!(recorded.len(), 32, "the hash column is a 32-byte blake3 digest");
    assert_eq!(
        codec::state_from_columns("stored", None, Some(&recorded), false, None)
            .expect("the stored row decodes"),
        stored,
        "the recorded hash must decode back to the same representation hash"
    );

    // Gated: postgres serves the recorded hash back for freshness checks.
    let Some(pool) = common::postgres_pool("code_treesitter_hash_is_recorded").await else {
        return;
    };
    let store = common::global_store(pool).await;
    let registered = common::rust_package(&common::unique_rust_name("hashed"), "1.0.0");
    store
        .upsert(&common::global_package(
            registered.clone(),
            ResolutionState::Stored { hash: representation_hash },
        ))
        .await
        .expect("publishing succeeds");
    assert_eq!(
        store.generation(registered.id()).await.expect("generation resolves"),
        Some(representation_hash),
        "the code/treesitter hash must be persisted next to the GUID"
    );
}

/// A changed representation produces a different hash (freshness signal).
///
/// Assert: re-parsing changed source yields a different hash, which is what
///   drives re-indexing decisions.
#[tokio::test]
async fn changed_representation_changes_the_hash() {
    let package = common::rust_package("serde", "1.0.0");
    let generation = |bytes: &'static [u8]| {
        let (manifest, _) = common::built_manifest_with(&package, bytes);
        let files: Vec<registry::FileEntry> = manifest.files.iter().cloned().collect();
        hash::package_generation(&files)
    };

    let original = generation(b"pub fn answer() -> u32 { 42 }");
    let reparsed_identical = generation(b"pub fn answer() -> u32 { 42 }");
    let reparsed_changed = generation(b"pub fn answer() -> u32 { 43 }");

    assert_eq!(original, reparsed_identical, "an identical re-parse must hash identically");
    assert_ne!(original, reparsed_changed, "a changed representation must move the hash");
    assert_eq!(
        hash::freshness(original, reparsed_changed),
        heart::Freshness::Stale,
        "the moved hash is exactly what flags re-indexing"
    );
}

/// Metadata links the GUID to every cross-store identity.
///
/// Assert: from the GUID you can reach the blob ref, graph URI, and vector ids.
#[tokio::test]
async fn metadata_links_guid_to_cross_store_ids() {
    let package = common::rust_package("serde", "1.0.0");
    let guid = package.id();

    // The blob ref: the object-store pointer location is derived from the GUID
    // alone, so holding the GUID is holding the blob address.
    assert_eq!(
        Store::pointer_path(&package.coordinates).to_string(),
        format!("ptr/{}", guid.as_uuid()),
        "the blob pointer must be reachable from the GUID"
    );

    // The graph URI + vector ids: every symbol id under this package derives
    // from an EntryUri rooted at the GUID, salted by the instance.
    let uri = heart::EntryUri {
        package: guid,
        path: vec![smol_str::SmolStr::new("Serialize")].into_boxed_slice(),
    };
    assert!(
        uri.canonical().starts_with(&guid.to_string()),
        "the graph URI is rooted at the GUID"
    );
    let minter = Minter::new(common::test_instance());
    assert_eq!(
        minter.symbol_id(&uri),
        uri.symbol_id(common::test_instance().token()),
        "vector/graph symbol ids derive from GUID-rooted URIs"
    );

    // The metadata row itself pairs the GUID with the per-store link bitmap —
    // the join the read plane uses to know where this generation landed.
    let row = PackageMetadata { id: guid, links: StoreLinks { vector: true, graph: true, text: false } };
    assert_eq!(row.id, guid);
    assert!(!row.links.fully_linked(), "the bitmap must expose the not-yet-linked text store");
}
