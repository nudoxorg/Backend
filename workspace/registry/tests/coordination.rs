//! Pipeline part: **blob/registry coordination** (`registry::coordination`).
//!
//! TDD specs for recording the creation of a blob to the surrounding systems
//! (the global registry) so the index knows a package is now stored. The
//! transactional outbox is catalog-backed; every spec here runs against an
//! in-memory catalog store.

mod common;

use heart::{ContentHash, EntryUri, ResolutionState, SymbolKind};
use registry::coordination::{Outbox, OutboxSeq, SinkKind};
use strum::IntoEnumIterator;

/// Every outbox intent recorded for `package` under `kind`, from the beginning
/// of the stream (test packages are unique, so this is a precise filter).
async fn intents_for(
    outbox: &Outbox<index::engine::memory::MemoryEngine>,
    kind: SinkKind,
    package: heart::PackageId,
) -> Vec<registry::coordination::OutboxEntry> {
    outbox
        .read_since(kind, OutboxSeq(0), 100_000)
        .await
        .expect("reading the outbox succeeds")
        .into_iter()
        .filter(|entry| entry.package == package)
        .collect()
}

/// Recording a blob's creation registers it in the global index.
///
/// Act: `record_blob_creation(coord, blob_ref)`.
/// Assert: the global store now reports the package's symbols as registered,
///   with their canonical global ids.
#[tokio::test]
async fn recording_a_blob_registers_it_globally() {
    let (store, writer) = common::catalog_store("recording_a_blob_registers_it_globally");
    let outbox = Outbox::new(std::sync::Arc::clone(&writer));

    let package = common::rust_package(&common::unique_rust_name("recorded"), "1.0.0");
    store
        .upsert(&common::global_package(
            package.clone(),
            ResolutionState::Unindexed { needed: false },
        ))
        .await
        .expect("the package is tracked before its blob lands");

    // The blob exists — record it to the surrounding systems.
    let generation = ContentHash::of_bytes(b"blob generation");
    outbox
        .record_stored(&store, package.id(), generation, None)
        .await
        .expect("recording the blob creation succeeds");

    // The global index now knows the package is stored at that generation.
    assert_eq!(
        store.get_state(package.id()).await.expect("state resolves"),
        ResolutionState::Stored { hash: generation }
    );

    // A symbol can be registered under its canonical instance-salted global id.
    let uri = EntryUri {
        package: package.id(),
        path: vec![smol_str::SmolStr::new("answer")].into_boxed_slice(),
    };
    let symbol = store.symbol_id(&uri);
    let catalog_instance = registry::index::InstanceToken::new("test/catalog")
        .expect("fixture catalog instance token");
    assert_eq!(
        symbol,
        uri.symbol_id(catalog_instance.token()),
        "the store must mint the canonical instance-salted id"
    );
    store
        .upsert_symbol(symbol, package.id(), "fixture::answer", SymbolKind::Function, generation)
        .await
        .expect("the symbol registers under its canonical global id");

    // Every derived sink heard about the new generation (at least one intent each).
    for kind in SinkKind::iter() {
        let intents = intents_for(&outbox, kind, package.id()).await;
        assert!(
            !intents.is_empty(),
            "sink {kind:?} must receive at least one fan-out intent"
        );
    }
}

/// Recording is idempotent.
///
/// Assert: recording the same blob twice does not produce divergent state.
#[tokio::test]
async fn recording_is_idempotent() {
    let (store, writer) = common::catalog_store("recording_is_idempotent");
    let outbox = Outbox::new(std::sync::Arc::clone(&writer));

    let package = common::rust_package(&common::unique_rust_name("idempotent"), "1.0.0");
    store
        .upsert(&common::global_package(
            package.clone(),
            ResolutionState::Unindexed { needed: false },
        ))
        .await
        .expect("the package is tracked");

    let generation = ContentHash::of_bytes(b"one generation");
    outbox.record_stored(&store, package.id(), generation, None).await.expect("first recording");
    outbox
        .record_stored(&store, package.id(), generation, None)
        .await
        .expect("a retried recording is a silent no-op");

    // State must remain Stored (not diverged or doubled).
    assert_eq!(
        store.get_state(package.id()).await.expect("state resolves"),
        ResolutionState::Stored { hash: generation },
        "the duplicate recording must not disturb the stored state"
    );
}

/// Recording flips the package's resolution state toward `Stored`.
///
/// Assert: after coordination, the index entry reflects "loaded into the runtime
///   stores" rather than still pending.
#[tokio::test]
async fn recording_advances_resolution_state() {
    let (store, writer) = common::catalog_store("recording_advances_resolution_state");
    let outbox = Outbox::new(std::sync::Arc::clone(&writer));

    let package = common::rust_package(&common::unique_rust_name("advancing"), "1.0.0");
    store
        .upsert(&common::global_package(
            package.clone(),
            ResolutionState::Progressing(heart::Phase::Emitting),
        ))
        .await
        .expect("the package is mid-pipeline before coordination");
    assert_eq!(
        store.get_state(package.id()).await.expect("state resolves"),
        ResolutionState::Progressing(heart::Phase::Emitting),
        "precondition: the entry is still pending"
    );

    let generation = ContentHash::of_bytes(b"emitted generation");
    outbox.record_stored(&store, package.id(), generation, None).await.expect("coordination succeeds");

    assert_eq!(
        store.get_state(package.id()).await.expect("state resolves"),
        ResolutionState::Stored { hash: generation },
        "coordination must advance the lifecycle to Stored"
    );
}

/// Set facets alongside record_stored: the lifecycle state is Stored and
/// facets survive in the catalog.
///
/// Assert: after `record_stored` with facets, the state is Stored and the
/// generation is recorded.
#[tokio::test]
async fn set_facets_recorded_with_stored_state() {
    let (store, writer) = common::catalog_store("set_facets_recorded_with_stored_state");
    let outbox = Outbox::new(std::sync::Arc::clone(&writer));

    let package = common::rust_package(&common::unique_rust_name("facets-freshen"), "1.0.0");
    store
        .upsert(&common::global_package(
            package.clone(),
            ResolutionState::Unindexed { needed: false },
        ))
        .await
        .expect("the package is registered");

    let generation = ContentHash::of_bytes(b"facet generation");
    let facets = registry::metadata::SearchFacets {
        keywords: vec!["async".into(), "runtime".into()],
        quality_ppm: 500_000,
        description: Some("async runtime".into()),
        downloads: None,
        ..Default::default()
    };
    outbox
        .record_stored(&store, package.id(), generation, Some(&facets))
        .await
        .expect("record_stored with facets succeeds");

    // The lifecycle must now be Stored with the recorded generation.
    assert_eq!(
        store.get_state(package.id()).await.expect("state resolves"),
        ResolutionState::Stored { hash: generation },
        "record_stored with facets must still land Stored"
    );
    assert_eq!(
        store.generation(package.id()).await.expect("generation resolves"),
        Some(generation),
        "the generation hash must be persisted alongside facets"
    );
}
