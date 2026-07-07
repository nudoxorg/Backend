//! Pipeline part: **the registry store** (`registry::store`).
//!
//! TDD specs for the package-addressed durable store over `object_store`
//! (S3 / local). The system is built around `Package`: you `put`/`get` a
//! package's parsed blob *by the package*, with the object location derived from
//! its coordinates — there is no opaque ref to round-trip.

mod common;

use std::sync::Arc;

use heart::{Connect, ContentHash, Freshness};
use object_store::{ObjectStore, memory::InMemory};
use registry::{Store, error::StoreError, metadata::hash};

/// A live store over a fresh in-memory backend, plus the backend handle so a
/// test can reopen over the same objects.
async fn memory_store() -> (Arc<InMemory>, Store) {
    let backend = Arc::new(InMemory::new());
    let store = Store::new(backend.clone())
        .connect()
        .await
        .expect("an in-memory backend always answers the sentinel probe");
    (backend, store)
}

/// A package's blob round-trips: what you put is what you get back.
///
/// Act: `store.put(&package, &blob)` then `store.get(&package)`.
/// Assert: the retrieved blob equals the stored one.
#[tokio::test]
async fn put_then_get_round_trips() {
    let package = common::rust_package("serde", "1.0.0");
    let (manifest, sections) = common::built_manifest(&package);
    let (_backend, store) = memory_store().await;

    for section in &sections {
        store.put_section(section).await.expect("section write succeeds");
    }
    store.put_manifest(&manifest).await.expect("manifest records");

    let read_back = store.get_manifest(&package.coordinates).await.expect("manifest resolves");
    assert_eq!(read_back, manifest, "the retrieved manifest must equal the stored one");

    // Every referenced section reads back byte-identical (integrity-verified).
    for section in &sections {
        let bytes = store.get_section(section.hash).await.expect("section resolves");
        assert_eq!(bytes, section.bytes, "cas round-trip must be the identity");
    }
}

/// The location is derived deterministically from the package coordinates.
///
/// Assert: two packages with different language/name/version land at different
///   object-store paths; the same package always maps to the same path.
#[tokio::test]
async fn location_is_derived_from_package_coordinates() {
    let serde_one = common::rust_coordinates("serde", "1.0.0");
    let serde_two = common::rust_coordinates("serde", "1.0.1");
    let tokio = common::rust_coordinates("tokio", "1.0.0");
    let python = common::python_package("requests", "2.31.0").coordinates;

    // Deterministic: rebuilding the coordinates yields the same path.
    assert_eq!(
        Store::pointer_path(&serde_one),
        Store::pointer_path(&common::rust_coordinates("serde", "1.0.0")),
        "the same coordinates must always map to the same location"
    );

    // Collision-free: any coordinate difference moves the location.
    let paths = [
        Store::pointer_path(&serde_one),
        Store::pointer_path(&serde_two),
        Store::pointer_path(&tokio),
        Store::pointer_path(&python),
    ];
    for (index, path) in paths.iter().enumerate() {
        for other in &paths[index + 1..] {
            assert_ne!(path, other, "distinct coordinates must map to distinct locations");
        }
        assert!(
            path.as_ref().starts_with("ptr/"),
            "package pointers live under the fixed-shape ptr/ keyspace, got {path}"
        );
    }

    // The leaf is the deterministic package id, never raw name segments.
    assert_eq!(
        Store::pointer_path(&serde_one).as_ref(),
        format!("ptr/{}", serde_one.id().as_uuid()),
    );
}

/// Re-putting the same package overwrites in place (no duplicate objects).
///
/// Assert: putting a package twice leaves a single object at its location.
#[tokio::test]
async fn reput_overwrites_in_place() {
    use futures::TryStreamExt;

    let package = common::rust_package("serde", "1.0.0");
    let (manifest, sections) = common::built_manifest(&package);
    let (backend, store) = memory_store().await;

    for section in &sections {
        assert!(store.put_section(section).await.expect("first write"), "first put writes");
    }
    let first = store.put_manifest(&manifest).await.expect("first manifest put");

    // Second pass: every cas put dedupes, the pointer put replaces in place.
    for section in &sections {
        assert!(
            !store.put_section(section).await.expect("second write"),
            "re-putting an existing section must be an idempotent no-op"
        );
    }
    let second = store.put_manifest(&manifest).await.expect("second manifest put");
    assert_eq!(first, second, "an identical manifest re-put must land on the same hash");

    let pointers: Vec<object_store::ObjectMeta> = backend
        .list(Some(&object_store::path::Path::from("ptr")))
        .try_collect()
        .await
        .expect("listing the pointer keyspace succeeds");
    assert_eq!(pointers.len(), 1, "the package must own exactly one pointer object");
    assert_eq!(pointers[0].location, Store::pointer_path(&package.coordinates));
}

/// Getting a package that was never stored reports a backend not-found.
#[tokio::test]
async fn get_unknown_package_errors() {
    let (_backend, store) = memory_store().await;
    let unstored = common::rust_coordinates("never-published", "0.1.0");

    let error = store
        .get_manifest(&unstored)
        .await
        .expect_err("an unstored package must not resolve to a manifest");
    // The backend's NotFound is folded into StoreError::NotFound with the
    // derived key preserved (the spec's `Backend(NotFound)`, post-mapping).
    match error {
        StoreError::NotFound(key) => assert_eq!(
            key,
            Store::pointer_path(&unstored).to_string(),
            "the error must name the exact derived location that missed"
        ),
        other => panic!("expected StoreError::NotFound, got {other:?}"),
    }
}

/// A re-parse with a changed representation is detected via the freshness hash.
///
/// Assert: `metadata::hash::freshness(recorded, recomputed)` is `Stale` when the
///   code/treesitter hash changed, `Fresh` when it didn't — the signal that
///   gates pulling a library down again.
#[tokio::test]
async fn changed_representation_is_stale() {
    let package = common::rust_package("serde", "1.0.0");
    let (original, _) = common::built_manifest(&package);
    let (unchanged, _) = common::built_manifest(&package);
    let (changed, _) =
        common::built_manifest_with(&package, b"pub fn answer() -> u32 { 43 } // re-parse");

    let generation = |manifest: &registry::BlobManifest| {
        let files: Vec<registry::FileEntry> = manifest.files.iter().cloned().collect();
        hash::package_generation(&files)
    };

    let recorded = generation(&original);
    assert_eq!(
        hash::freshness(recorded, generation(&unchanged)),
        Freshness::Fresh,
        "an identical re-parse must read Fresh"
    );
    assert_eq!(
        hash::freshness(recorded, generation(&changed)),
        Freshness::Stale,
        "a changed representation must read Stale"
    );
}

/// Packages persist across a store reopen (S3/local durability).
///
/// Assert: after reopening the store over the same backend, a previously-put
///   package's blob is still retrievable.
#[tokio::test]
async fn packages_persist_across_reopen() {
    let package = common::rust_package("serde", "1.0.0");
    let (manifest, sections) = common::built_manifest(&package);
    let (backend, store) = memory_store().await;

    for section in &sections {
        store.put_section(section).await.expect("section write succeeds");
    }
    store.put_manifest(&manifest).await.expect("manifest records");
    drop(store);

    // "Restart": a brand new store handle over the same durable backend.
    let reopened = Store::new(backend)
        .connect()
        .await
        .expect("reopening over a populated backend connects");
    assert!(reopened.exists(&package.coordinates).await.expect("existence probe succeeds"));
    let read_back =
        reopened.get_manifest(&package.coordinates).await.expect("manifest survives reopen");
    assert_eq!(read_back, manifest);
    let ir = reopened.get_section(manifest.ir_ref).await.expect("ir section survives reopen");
    assert_eq!(ContentHash::of_bytes(&ir), manifest.ir_ref);
}
