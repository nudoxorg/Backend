//! Diagram flow: **initialization** (`driver::coordination::initialization`).
//!
//! "if index (otherwise let index) · tracks the usage of a library, coordinating
//! tiers, pulling down if unfresh · updates postgres status · loads into pg ·
//! ensure init (if not send req) · return responses".
//!
//! The decision table is pure and always runs; the catalog-loading round-trips
//! are opt-in via `SERVER_TEST_BACKENDS`.

mod common;

use heart::{ContentHash, Failure, Freshness, Phase, ResolutionState};
use index::ecosystem::PackageNameExt as _;
use driver::coordination::initialization::{
    InitializationDecision, initialization_decision,
};

/// An uninitialized library is indexed on first use.
///
/// Assert: a never-seen package (no state row) decides `Enqueue` — indexing is
///   kicked off (or deferred to the queue's owner) rather than answered cold.
#[tokio::test]
async fn first_use_triggers_indexing() {
    assert_eq!(initialization_decision(None, None), InitializationDecision::Enqueue);
    assert_eq!(
        initialization_decision(Some(&ResolutionState::Unindexed { needed: true }), None),
        InitializationDecision::Enqueue,
        "a known-but-unindexed package also (re-)enqueues"
    );
}

/// An already-initialized, fresh library is served without re-indexing.
///
/// Assert: when the stored hash still matches, no re-parse happens.
#[tokio::test]
async fn fresh_library_is_served_without_reindex() {
    let stored = ResolutionState::Stored { hash: fixture_hash(b"stored-blob") };
    assert_eq!(
        initialization_decision(Some(&stored), Some(Freshness::Fresh)),
        InitializationDecision::Serve
    );
    assert_eq!(
        initialization_decision(Some(&stored), None),
        InitializationDecision::Serve,
        "the ensure path treats a stored record as fresh; recomputation is the sync surface's job"
    );
}

/// A stale (unfresh) library is pulled down again.
///
/// Assert: when the recomputed hash differs from the recorded one, the library
///   is re-enqueued for re-fetch + re-index.
#[tokio::test]
async fn unfresh_library_is_pulled_down_again() {
    let stored = ResolutionState::Stored { hash: fixture_hash(b"stored-blob") };
    assert_eq!(
        initialization_decision(Some(&stored), Some(Freshness::Stale)),
        InitializationDecision::Enqueue
    );
}

/// `ensure init` returns immediately when ready, else requests initialization.
///
/// Assert: in-flight work is answered without re-enqueueing, dead-lettered work
///   is held for a human, and — over the live stack — `ensure_initialized`
///   reports `enqueued` exactly when the decision was `Enqueue`.
#[tokio::test]
async fn ensure_init_returns_or_requests() {
    assert_eq!(
        initialization_decision(Some(&ResolutionState::Progressing(Phase::Compiling)), None),
        InitializationDecision::AlreadyInFlight,
        "a worker already owns it: answer its state, do not re-enqueue"
    );
    assert_eq!(
        initialization_decision(Some(&ResolutionState::DeadLettered(fixture_failure())), None),
        InitializationDecision::Hold,
        "dead-lettered work is a human's now; never auto-requeue"
    );
    assert_eq!(
        initialization_decision(Some(&ResolutionState::Failed(fixture_failure())), None),
        InitializationDecision::Enqueue,
        "a retriable failure re-enqueues"
    );

    let Some((server, _data)) = common::assembled_server("ensure_init_returns_or_requests").await
    else {
        return;
    };
    let cap = common::write_cap(&server);
    let coordinates = fixture_coordinates();
    let first = server.ensure_initialized(&cap, &coordinates).await.expect("ensure answers");
    assert!(first.enqueued, "a never-seen package requests initialization");
    let second = server.ensure_initialized(&cap, &coordinates).await.expect("ensure answers");
    assert!(!second.enqueued, "an in-flight package returns state without re-requesting");
    assert_eq!(second.package, first.package, "identity is deterministic");
}

/// Initialization loads the library into the catalog.
///
/// Assert: after init, the catalog holds the package's metadata + status.
#[tokio::test]
async fn initialization_loads_into_catalog() {
    let Some((server, _data)) =
        common::assembled_server("initialization_loads_into_catalog").await
    else {
        return;
    };
    let cap = common::write_cap(&server);
    let coordinates = fixture_coordinates();
    let initialized = server.ensure_initialized(&cap, &coordinates).await.expect("ensure answers");

    let state = server
        .parse_status(initialized.package)
        .await
        .expect("the status lookup answers")
        .expect("the ensured package now has a catalog row");
    assert!(
        matches!(state, ResolutionState::Unindexed { needed: true } | ResolutionState::Progressing(_)),
        "the loaded row is on the pipeline's leading edge, got {state:?}"
    );
}

/// Library usage is tracked to coordinate tiers.
///
/// Assert: repeated ensure calls converge (idempotent identity, one live job) —
///   the usage signal every tiering decision is built on. (Tier *policy* is
///   deliberately out of scope today: usage lands as `packages_ensure_initialized`
///   decision metrics, not a persisted counter.)
#[tokio::test]
async fn usage_is_tracked_for_tiering() {
    let Some((server, _data)) = common::assembled_server("usage_is_tracked_for_tiering").await
    else {
        return;
    };
    let cap = common::write_cap(&server);
    let coordinates = fixture_coordinates();
    let first = server.ensure_initialized(&cap, &coordinates).await.expect("ensure answers");
    for _ in 0..3 {
        let repeat = server.ensure_initialized(&cap, &coordinates).await.expect("ensure answers");
        assert_eq!(repeat.package, first.package);
        assert!(!repeat.enqueued, "repeated use converges on the one live job");
    }
}

/// Unique fixture coordinates, collision-free across runs against a shared
/// database.
fn fixture_coordinates() -> driver::registry::package::Coordinates {
    driver::registry::package::Coordinates {
        origin: heart::RegistryOrigin::CratesIo,
        name: driver::registry::package::PackageName::new(
            heart::Language::Rust,
            format!("init-flow-{}", uuid::Uuid::new_v4().simple()),
        )
        .expect("fixture names are valid"),
        version: heart::PackageVersion::try_from((heart::Language::Rust, "1.0.0"))
            .expect("fixture versions are valid"),
    }
}

/// A deterministic content hash over fixture bytes.
fn fixture_hash(bytes: &[u8]) -> ContentHash {
    ContentHash::of_bytes(bytes)
}

/// A minimal recorded failure for the decision-table arms.
fn fixture_failure() -> Failure {
    Failure {
        attempts: 1,
        phase: Phase::Compiling,
        message: "fixture: the compiler rejected the package".into(),
        cause: Some(heart::ErrorDetails::Message("fixture: the compiler rejected the package".into())),
        at: chrono::Utc::now(),
    }
}
