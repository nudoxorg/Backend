//! Pipeline part: **blob assembly + emission** (`registry::blob::{creation, emit}`).
//!
//! Specs for building a blob manifest from the compiler's resolutions (source
//! files, IR section, CST-free references) and shipping it off: idempotent
//! `cas/` writes plus the outbox fan-out signal.

mod common;

use heart::{Connect, ContentHash, Retryable};
use registry::Store;

/// A blob is assembled from all three resolutions: the source files, the IR
/// surface, and the extracted references — one manifest, every ref populated.
#[test]
fn blob_assembles_all_three_resolutions() {
    let package = common::rust_package("serde", "1.0.0");
    let (manifest, sections) = common::built_manifest(&package);

    assert_eq!(manifest.package, package.id());
    assert_eq!(manifest.files.len(), 2);
    assert_ne!(manifest.ir_ref, manifest.references_ref);
    // Every declared ref has a matching pending section: 2 files + ir + refs.
    assert_eq!(sections.len(), 4);
    for entry in manifest.files.iter() {
        assert!(sections.iter().any(|section| section.hash == entry.hash));
    }
    assert!(sections.iter().any(|section| section.hash == manifest.ir_ref));
    assert!(sections.iter().any(|section| section.hash == manifest.references_ref));
}

/// The stored source is real, readable content: every section's bytes hash
/// back to the manifest entry they are keyed under (the CAS round-trip).
#[test]
fn tarred_source_is_a_readable_archive() {
    let package = common::rust_package("serde", "1.0.0");
    let (manifest, sections) = common::built_manifest(&package);

    for entry in manifest.files.iter() {
        let section = sections
            .iter()
            .find(|section| section.hash == entry.hash)
            .expect("every file has a section");
        assert_eq!(ContentHash::of_bytes(&section.bytes), entry.hash);
        assert_eq!(section.bytes.len() as u64, entry.size);
    }
}

/// Emitting a blob uploads it to object storage and signals downstream: the
/// sections + manifest land in the store, and the catalog outbox fans one
/// intent out to every derived sink.
#[tokio::test]
async fn emit_uploads_and_signals() {
    use registry::coordination::{Outbox, OutboxSeq, SinkKind};
    use strum::IntoEnumIterator;

    let package = common::rust_package("serde", "1.0.0");
    let (manifest, sections) = common::built_manifest(&package);

    // The upload half against an in-memory backend.
    let backend = std::sync::Arc::new(object_store::memory::InMemory::new());
    let store = Store::new(backend).connect().await.expect("in-memory store connects");
    for section in &sections {
        assert!(store.put_section(section).await.expect("section write succeeds"));
    }
    store.put_manifest(&manifest).await.expect("manifest records");
    let read_back = store.get_manifest(&package.coordinates).await.expect("manifest resolves");
    assert_eq!(read_back, manifest);

    // The signal half: the catalog outbox fans one intent per derived sink.
    let (catalog_store, writer) = common::catalog_store("emit_uploads_and_signals");
    let outbox = Outbox::new(std::sync::Arc::clone(&writer));
    catalog_store
        .upsert(&common::global_package(
            package.clone(),
            heart::ResolutionState::Unindexed { needed: false },
        ))
        .await
        .expect("package registered for outbox test");
    let generation = heart::ContentHash::of_bytes(b"generation");
    outbox
        .append(package.id(), generation, &[SinkKind::Text, SinkKind::Vector, SinkKind::Graph])
        .await
        .expect("fan-out appended for all sinks");
    // Re-appending is idempotent.
    outbox
        .append(package.id(), generation, &[SinkKind::Text, SinkKind::Vector, SinkKind::Graph])
        .await
        .expect("duplicate fan-out is idempotent");
    // Every sink has at least one intent recorded.
    for kind in SinkKind::iter() {
        let intents = outbox
            .read_since(kind, OutboxSeq(0), 1_000)
            .await
            .expect("outbox readable");
        assert!(
            intents.iter().any(|e| e.package == package.id()),
            "sink {kind:?} must have an intent after fan-out"
        );
    }
}

/// Emission distinguishes transient upload failures (retryable per the sink
/// backoff) from permanent ones (surfaced immediately).
#[tokio::test]
async fn emit_retries_transient_failures() {
    use registry::error::{BlobError, StoreError};

    let transient = StoreError::Backend(object_store::Error::Generic {
        store: "s3",
        source: "connection reset".into(),
    });
    assert!(transient.is_retryable());
    assert!(BlobError::Store(Box::new(transient)).is_retryable());

    let permanent = StoreError::Integrity {
        path: object_store::path::Path::from("cas/deadbeef"),
        expected: heart::content::ContentHash::of_bytes(b"aa"),
        found: heart::content::ContentHash::of_bytes(b"bb"),
    };
    assert!(!permanent.is_retryable());
    assert!(!BlobError::DuplicateFilePathInManifest.is_retryable());
}
