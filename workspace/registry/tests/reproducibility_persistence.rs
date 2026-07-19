//! Diagram: **REPRODUCIBILITY** — registry persists every store so the system is
//! rebuildable: qdrant persistence, tantivy persistence, terminus persistence,
//! and the blobs (parsed code) that everything else derives from.
//!
//! TDD specs for reproducibility. The registry crate owns the blob root, the
//! fan-out intents the derived stores rebuild from, and the tantivy replica;
//! the qdrant/terminus *engines* live outside this crate (their rebuild inputs
//! — the signal and the sections — are what is asserted here).

mod common;

use std::sync::Arc;

use heart::{Connect, ContentHash, ResolutionState};
use object_store::{ObjectStore, memory::InMemory};
use registry::{
    BlobManifest, GlobalPackage, Store,
    blob::ReferenceSet,
    coordination::SinkKind,
    search::tantivy::PackageIndex,
};

// `metadata::hash::package_generation` was removed; the canonical generation
// stamp is now derived directly from `BlobManifest::identity_bytes`.

/// A live store over a fresh in-memory backend, plus the backend handle.
async fn memory_store() -> (Arc<InMemory>, Store) {
    let backend = Arc::new(InMemory::new());
    let store = Store::new(backend.clone())
        .connect()
        .await
        .expect("an in-memory backend always connects");
    (backend, store)
}

/// Persist a package's full blob (sections + manifest) into `store`.
async fn persist_blob(store: &Store, package: &registry::Package) -> BlobManifest {
    let (manifest, sections) = common::built_manifest(package);
    for section in &sections {
        store.put_section(section).await.expect("section write succeeds");
    }
    store.put_manifest(&manifest).await.expect("manifest records");
    manifest
}

/// The derived search record a blob manifest reconstructs to — the rebuild
/// unit every read-plane store replays.
fn record_from_blob(package: &registry::Package, manifest: &BlobManifest) -> GlobalPackage {
    let generation = ContentHash::of_bytes(&manifest.identity_bytes());
    common::global_package(package.clone(), ResolutionState::Stored { hash: generation })
}

/// Assert every ref a manifest declares resolves from `store`, byte-verified.
async fn assert_blob_complete(store: &Store, manifest: &BlobManifest) {
    for entry in manifest.files.iter() {
        let bytes = store.get_section(entry.hash).await.expect("file section resolves");
        assert_eq!(ContentHash::of_bytes(&bytes), entry.hash);
        assert_eq!(bytes.len() as u64, entry.size);
    }
    store.get_section(manifest.ir_ref).await.expect("ir section resolves");
    store.get_section(manifest.references_ref).await.expect("references section resolves");
}

/// Blobs are the root of reproducibility.
///
/// Assert: persisted blobs survive a full wipe + restore of every other store.
#[tokio::test]
async fn blobs_are_the_durable_root() {
    let package = common::rust_package("serde", "1.0.0");
    let (_backend, store) = memory_store().await;
    let manifest = persist_blob(&store, &package).await;

    // Build a derived store from the blobs, then wipe it entirely.
    let replica_directory = common::TempDir::new("wipeable-replica");
    {
        let mut replica = PackageIndex::open(replica_directory.path()).expect("replica opens");
        replica
            .absorb([&record_from_blob(&package, &manifest)], 1)
            .expect("the derived store materializes from the blob");
    }
    std::fs::remove_dir_all(replica_directory.path()).expect("the derived store is wiped");

    // The root is untouched: the manifest still resolves and every declared
    // ref is present and integrity-verified — enough to rebuild everything.
    let read_back = store.get_manifest(&package.coordinates).await.expect("the root survives");
    assert_eq!(read_back, manifest);
    assert_blob_complete(&store, &manifest).await;

    // And the rebuild actually works: a fresh derived store from the same root.
    let fresh_directory = common::TempDir::new("rebuilt-replica");
    let mut rebuilt = PackageIndex::open(fresh_directory.path()).expect("fresh replica opens");
    rebuilt
        .absorb([&record_from_blob(&package, &read_back)], 1)
        .expect("the derived store rebuilds from the durable root");
    assert!(!rebuilt.query("serde", 10).expect("query executes").is_empty());
}

/// The qdrant collection can be rebuilt from the blobs.
///
/// The qdrant engine lives in the runtime crate (a gap against this spec at
/// the registry layer); what the registry owns of the rebuild is asserted
/// here: the vector sink's idempotent fan-out intent, and the IR section —
/// the embedding source — round-tripping byte-identically from the blob root.
#[tokio::test]
async fn qdrant_rebuilds_from_blobs() {
    let package = common::rust_package("serde", "1.0.0");
    let (_backend, store) = memory_store().await;
    let manifest = persist_blob(&store, &package).await;

    // The rebuild signal: a re-emit toward the vector sink is idempotent, so
    // replaying blob history can never double-materialize a generation.
    let generation = ContentHash::of_bytes(b"generation");
    let (sql, values) =
        registry::schema::queries::outbox::append_one(package.id(), generation, SinkKind::Vector);
    assert!(sql.contains("ON CONFLICT"), "the vector rebuild intent must be idempotent");
    assert_eq!(values.0.iter().count(), 4, "one (package, generation, kind, op) intent row");

    // The rebuild input: the IR section the embedder consumes, exactly as
    // emitted.
    let ir = store.get_section(manifest.ir_ref).await.expect("ir section resolves");
    assert_eq!(&ir[..], b"ir-section-bytes", "the embedding source must survive verbatim");
}

/// The tantivy indexes can be rebuilt from the blobs.
///
/// Assert: wiping tantivy then rebuilding restores precise search identically.
#[tokio::test]
async fn tantivy_rebuilds_from_blobs() {
    let (_backend, store) = memory_store().await;
    let packages =
        [common::rust_package("serde", "1.0.0"), common::rust_package("tokio", "1.0.0")];
    let mut records = Vec::new();
    for package in &packages {
        let manifest = persist_blob(&store, package).await;
        records.push(record_from_blob(package, &manifest));
    }

    let results_of = |directory: &common::TempDir| {
        let mut index = PackageIndex::open(directory.path()).expect("replica opens");
        index.absorb(records.iter(), 1).expect("records fold in");
        index.query("serde", 10).expect("query executes")
    };

    // The original index, then a full wipe...
    let original_directory = common::TempDir::new("tantivy-original");
    let original = results_of(&original_directory);
    assert!(!original.is_empty(), "precondition: the corpus is searchable");
    std::fs::remove_dir_all(original_directory.path()).expect("tantivy is wiped");

    // ...and the rebuild from blob-derived records restores search identically.
    let rebuilt_directory = common::TempDir::new("tantivy-rebuilt");
    let rebuilt = results_of(&rebuilt_directory);
    assert_eq!(rebuilt, original, "the rebuilt index must answer identically");
}

/// The terminus graph can be rebuilt from the blobs.
///
/// The terminus engine lives outside this crate (a gap against this spec at
/// the registry layer); what the registry owns of the rebuild is asserted
/// here: the graph sink's idempotent intent, and the CST-free reference set —
/// the graph's edge source — decoding identically from the blob root.
#[tokio::test]
async fn terminus_rebuilds_from_blobs() {
    let package = common::rust_package("serde", "1.0.0");
    let (_backend, store) = memory_store().await;
    let manifest = persist_blob(&store, &package).await;

    // The rebuild signal for the graph sink is idempotent.
    let generation = ContentHash::of_bytes(b"generation");
    let (sql, _values) =
        registry::schema::queries::outbox::append_one(package.id(), generation, SinkKind::Graph);
    assert!(sql.contains("ON CONFLICT"), "the graph rebuild intent must be idempotent");

    // The rebuild input: the reference section decodes from the root exactly
    // as it was encoded (the codec is the identity through the CAS).
    let encoded = ReferenceSet { by_file: Vec::new() }.encode().expect("references encode");
    let stored = store.get_section(manifest.references_ref).await.expect("section resolves");
    assert_eq!(&stored[..], &encoded[..], "the reference bytes must survive verbatim");
    let decoded = ReferenceSet::decode(&stored).expect("the graph edge source decodes");
    assert!(decoded.by_file.is_empty(), "decode must reproduce the emitted reference set");
}

/// A full snapshot + restore round-trips every store.
///
/// Assert: snapshot the blob root, restore into a fresh deployment, and every
///   search surface answers identically (the derived stores rebuild from the
///   restored root — blobs are the one store that must round-trip bit-exact).
#[tokio::test]
async fn full_snapshot_restore_round_trips() {
    use futures::TryStreamExt;

    let package = common::rust_package("serde", "1.0.0");
    let (backend, store) = memory_store().await;
    let manifest = persist_blob(&store, &package).await;

    // Snapshot: every object in the deployment, bit-exact.
    let objects: Vec<object_store::ObjectMeta> =
        backend.list(None).try_collect().await.expect("the snapshot enumerates");
    assert!(!objects.is_empty());

    // Restore into a fresh deployment.
    let restored_backend = Arc::new(InMemory::new());
    for meta in &objects {
        let bytes = backend
            .get(&meta.location)
            .await
            .expect("snapshot read succeeds")
            .bytes()
            .await
            .expect("snapshot bytes materialize");
        restored_backend
            .put(&meta.location, bytes.into())
            .await
            .expect("restore write succeeds");
    }
    let restored_store = Store::new(restored_backend)
        .connect()
        .await
        .expect("the restored deployment connects");

    // The root round-trips...
    let restored_manifest = restored_store
        .get_manifest(&package.coordinates)
        .await
        .expect("the restored root resolves");
    assert_eq!(restored_manifest, manifest);
    assert_blob_complete(&restored_store, &restored_manifest).await;

    // ...and each search surface, rebuilt on either side, answers identically.
    let original_directory = common::TempDir::new("snapshot-original");
    let restored_directory = common::TempDir::new("snapshot-restored");
    let results_of = |directory: &common::TempDir, manifest: &BlobManifest| {
        let mut index = PackageIndex::open(directory.path()).expect("replica opens");
        index.absorb([&record_from_blob(&package, manifest)], 1).expect("record folds in");
        index.query("serde", 10).expect("query executes")
    };
    assert_eq!(
        results_of(&restored_directory, &restored_manifest),
        results_of(&original_directory, &manifest),
        "the restored deployment must answer every search identically"
    );
}
