//! Diagram flow: **indexing** (`driver::coordination::indexing`).
//!
//! "runs computer · update catalog (tantivy polls) · generate blob information".
//!
//! These drive one real indexing job end to end (acquire → extract → compile →
//! emit), so they need the live stack *and* network access to the package
//! origin — opt-in via `SERVER_TEST_BACKENDS`.

mod common;

use std::sync::Arc;

use heart::ResolutionState;
use index::ecosystem::PackageNameExt as _;
use driver::registry::coordination::{OutboxSeq, SinkKind};
use driver::coordination::indexing::Indexer;

/// The tiny, dependency-free fixture crate one job indexes.
const FIXTURE_NAME: &str = "either";
const FIXTURE_VERSION: &str = "1.15.0";

/// Indexing runs the compiler/generation over the materialized package.
///
/// Assert: the job completes with a snapshot hash and the lifecycle lands on
///   `Stored { hash }` — the compile phase's terminal proof.
#[tokio::test]
async fn indexing_runs_the_compiler() {
    let Some((server, _data)) = common::assembled_server("indexing_runs_the_compiler").await
    else {
        return;
    };
    let (package, indexer) = ensured_job(&server).await;

    let snapshot = indexer.run_indexing_job(package).await.expect("the pipeline completes");

    let state = server
        .parse_status(package)
        .await
        .expect("status answers")
        .expect("the indexed package has a row");
    assert_eq!(
        state,
        ResolutionState::Stored { hash: snapshot },
        "the compile phase's terminal transition records the snapshot hash"
    );
}

/// Indexing generates blob information.
///
/// Assert: per-package blob info (the manifest and its content-addressed
///   sections) is produced and persisted to the object store.
#[tokio::test]
async fn indexing_generates_blob_information() {
    let Some((server, _data)) =
        common::assembled_server("indexing_generates_blob_information").await
    else {
        return;
    };
    let (package, indexer) = ensured_job(&server).await;
    let snapshot = indexer.run_indexing_job(package).await.expect("the pipeline completes");

    // The recomputable content hash *is* the blob information: recomputing it
    // from the persisted manifest reproduces the recorded snapshot exactly.
    let recomputed = indexer.content_hash(package).await.expect("the stored manifest resolves");
    assert_eq!(recomputed, snapshot, "blob info persisted content-addressed and reproducible");
}

/// Indexing updates catalog status; tantivy picks it up by polling.
///
/// Assert: indexing writes status to the catalog (push) and appends a text-sink
///   intent to the outbox — the tantivy replica *pulls* from that watermark,
///   never receives a direct push.
#[tokio::test]
async fn indexing_updates_catalog_and_tantivy_polls() {
    let Some((server, _data)) =
        common::assembled_server("indexing_updates_catalog_and_tantivy_polls").await
    else {
        return;
    };
    let (package, indexer) = ensured_job(&server).await;
    indexer.run_indexing_job(package).await.expect("the pipeline completes");

    // Push half: catalog holds the terminal status.
    let state = server.parse_status(package).await.expect("status answers");
    assert!(matches!(state, Some(ResolutionState::Stored { .. })));

    // Pull half: the text sink's fan-out intent waits on the outbox for the
    // replica-local poller — nothing wrote to tantivy directly.
    let intents = server
        .outbox()
        .read_since(SinkKind::Text, OutboxSeq(0), 1024)
        .await
        .expect("the outbox answers");
    assert!(
        intents.iter().any(|entry| entry.package == package),
        "the emit phase left a text-sink intent for the poller to consume"
    );
}

/// One parse fans out to every store (text, vector, graph, blob).
///
/// Assert: a single job leaves the blob stored plus one outbox intent per
///   derived sink — parsed once, fanned out by the pollers.
#[tokio::test]
async fn one_parse_fans_out_to_all_stores() {
    let Some((server, _data)) = common::assembled_server("one_parse_fans_out_to_all_stores").await
    else {
        return;
    };
    let (package, indexer) = ensured_job(&server).await;
    indexer.run_indexing_job(package).await.expect("the pipeline completes");

    for sink in [SinkKind::Text, SinkKind::Vector, SinkKind::Graph] {
        let intents = server
            .outbox()
            .read_since(sink, OutboxSeq(0), 1024)
            .await
            .expect("the outbox answers");
        assert!(
            intents.iter().any(|entry| entry.package == package),
            "one parse must leave a {sink:?} intent — every sink is fed from the same projection"
        );
    }
    // And the blob store holds the projection they all materialize from.
    let recomputed = indexer.content_hash(package).await;
    assert!(recomputed.is_ok(), "the shared blob projection is persisted");
}

/// Ensure the fixture package is tracked, returning its id and an indexer.
async fn ensured_job(
    server: &Arc<driver::Server<common::TestModel>>,
) -> (heart::PackageId, Indexer<common::TestModel>) {
    let coordinates = driver::registry::package::Coordinates {
        origin: heart::RegistryOrigin::CratesIo,
        name: driver::registry::package::PackageName::new(heart::Language::Rust, FIXTURE_NAME)
            .expect("fixture names are valid"),
        version: heart::PackageVersion::try_from((heart::Language::Rust, FIXTURE_VERSION))
            .expect("fixture versions are valid"),
    };
    let cap = common::write_cap(server);
    let initialized = server
        .ensure_initialized(&cap, &coordinates)
        .await
        .expect("the fixture package ensures cleanly");
    // The compiler daemon was removed (cage is ephemeral, SMOLVM-PLAN); the
    // Indexer no longer takes a compiler client.
    (initialized.package, Indexer::new(Arc::clone(server)))
}
