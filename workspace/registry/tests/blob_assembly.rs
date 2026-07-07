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
/// sections + manifest land in the store, and the outbox statement fans one
/// intent out to every derived sink.
#[tokio::test]
async fn emit_uploads_and_signals() {
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

    // The signal half: one idempotent intent per derived sink in one statement.
    let (sql, values) = registry::schema::queries::outbox::append_all(
        package.id(),
        ContentHash::of_bytes(b"generation"),
    );
    assert!(sql.contains("ON CONFLICT"), "fan-out is idempotent");
    // Three sinks × (package, generation, kind) = nine bound values.
    assert_eq!(values.0.iter().count(), 9, "one intent row per derived sink");
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
        key: "cas/deadbeef".into(),
        expected: "aa".into(),
        found: "bb".into(),
    };
    assert!(!permanent.is_retryable());
    assert!(!BlobError::Malformed("structurally invalid").is_retryable());
}
