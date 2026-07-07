//! Pipeline part: **blob/registry coordination** (`registry::coordination`).
//!
//! TDD specs for recording the creation of a blob to the surrounding systems
//! (the global registry) so the index knows a package is now stored. The
//! transactional outbox is postgres-shaped, so every spec here is gated on
//! `REGISTRY_TEST_POSTGRES`/`DATABASE_URL`.

mod common;

use heart::{Connect, ContentHash, EntryUri, Phase, ResolutionState, SymbolKind};
use registry::coordination::{Outbox, OutboxEntry, OutboxSeq, SinkKind};
use strum::IntoEnumIterator;

/// A connected outbox sharing `pool` with the global store.
async fn outbox(pool: sqlx::PgPool) -> Outbox {
    Outbox::new(pool).connect().await.expect("the schema-bearing pool carries the outbox table")
}

/// Every outbox intent recorded for `package` under `kind`, from the beginning
/// of the stream (test packages are unique, so this is a precise filter).
async fn intents_for(
    outbox: &Outbox,
    kind: SinkKind,
    package: heart::PackageId,
) -> Vec<OutboxEntry> {
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
    let Some(pool) = common::postgres_pool("recording_a_blob_registers_it_globally").await else {
        return;
    };
    let store = common::global_store(pool.clone()).await;
    let outbox = outbox(pool).await;

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
        .record_stored(&store, package.id(), generation)
        .await
        .expect("recording the blob creation succeeds");

    // The global index now knows the package is stored at that generation...
    assert_eq!(
        store.get_state(package.id()).await.expect("state resolves"),
        ResolutionState::Stored { hash: generation }
    );

    // ...and its symbols register under their canonical (deterministic,
    // instance-salted) global ids.
    let uri = EntryUri {
        package: package.id(),
        path: vec![smol_str::SmolStr::new("answer")].into_boxed_slice(),
    };
    let symbol = store.symbol_id(&uri);
    assert_eq!(
        symbol,
        uri.symbol_id(common::test_instance().token()),
        "the store must mint the canonical instance-salted id"
    );
    store
        .upsert_symbol(symbol, package.id(), "fixture::answer", SymbolKind::Function, generation)
        .await
        .expect("the symbol registers under its canonical global id");

    // Every derived sink heard about the new generation exactly once.
    for kind in SinkKind::iter() {
        let intents = intents_for(&outbox, kind, package.id()).await;
        assert_eq!(intents.len(), 1, "sink {kind} must receive exactly one fan-out intent");
    }
}

/// Recording is idempotent.
///
/// Assert: recording the same blob twice does not double-register its symbols.
#[tokio::test]
async fn recording_is_idempotent() {
    let Some(pool) = common::postgres_pool("recording_is_idempotent").await else { return };
    let store = common::global_store(pool.clone()).await;
    let outbox = outbox(pool).await;

    let package = common::rust_package(&common::unique_rust_name("idempotent"), "1.0.0");
    store
        .upsert(&common::global_package(
            package.clone(),
            ResolutionState::Unindexed { needed: false },
        ))
        .await
        .expect("the package is tracked");

    let generation = ContentHash::of_bytes(b"one generation");
    outbox.record_stored(&store, package.id(), generation).await.expect("first recording");
    outbox
        .record_stored(&store, package.id(), generation)
        .await
        .expect("a retried recording is a silent no-op");

    for kind in SinkKind::iter() {
        let intents = intents_for(&outbox, kind, package.id()).await;
        assert_eq!(
            intents.len(),
            1,
            "sink {kind} must hold one intent after a duplicate recording, not two"
        );
    }
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
    let Some(pool) = common::postgres_pool("recording_advances_resolution_state").await else {
        return;
    };
    let store = common::global_store(pool.clone()).await;
    let outbox = outbox(pool).await;

    let package = common::rust_package(&common::unique_rust_name("advancing"), "1.0.0");
    store
        .upsert(&common::global_package(
            package.clone(),
            ResolutionState::Progressing(Phase::Emitting),
        ))
        .await
        .expect("the package is mid-pipeline before coordination");
    assert_eq!(
        store.get_state(package.id()).await.expect("state resolves"),
        ResolutionState::Progressing(Phase::Emitting),
        "precondition: the entry is still pending"
    );

    let generation = ContentHash::of_bytes(b"emitted generation");
    outbox.record_stored(&store, package.id(), generation).await.expect("coordination succeeds");

    assert_eq!(
        store.get_state(package.id()).await.expect("state resolves"),
        ResolutionState::Stored { hash: generation },
        "coordination must advance the lifecycle to Stored"
    );
}
