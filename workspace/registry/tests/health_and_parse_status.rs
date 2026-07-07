//! Diagram: **postgres is in charge of the health of a particular project and
//! whether it's been parsed, etc.**
//!
//! TDD specs for `registry::health` and the postgres-backed parse status.
//!
//! The pure halves (the readiness policy over probes, the progress view of a
//! lifecycle state, the loaded-store link bitmap) run unconditionally; the
//! postgres round-trips are gated on `REGISTRY_TEST_POSTGRES`/`DATABASE_URL`.

mod common;

use heart::{
    BackendKind, ContentHash, Failure, JobProgress, Percent, Phase, Progressive, ResolutionState,
};
use registry::{
    health::{Health, Probe, aggregate},
    metadata::StoreLinks,
};

/// A probe result for `backend` with the given verdict.
fn probe(backend: BackendKind, healthy: bool) -> Probe {
    Probe {
        backend,
        healthy,
        latency_ms: healthy.then_some(2),
        detail: (!healthy).then(|| "connection refused".to_owned()),
    }
}

/// Every backend the registry fronts, all healthy.
fn all_healthy() -> Vec<Probe> {
    [
        BackendKind::Postgres,
        BackendKind::ObjectStore,
        BackendKind::Qdrant,
        BackendKind::Terminus,
        BackendKind::Tantivy,
    ]
    .into_iter()
    .map(|backend| probe(backend, true))
    .collect()
}

/// The zero intra-phase progress view of `state`.
fn progress_of(state: ResolutionState) -> JobProgress {
    JobProgress { state, phase_fraction: Percent::try_new(0).expect("0 is a valid percent") }
}

/// A recorded parse failure with a human-readable reason.
fn parse_failure(error: &str) -> Failure {
    Failure { attempts: 1, phase: Phase::Compiling, error: error.to_owned(), at: chrono::Utc::now() }
}

/// A new project is unparsed / pending.
///
/// Assert: health starts as not-yet-parsed.
#[tokio::test]
async fn new_project_is_unparsed() {
    // Pure: the initial lifecycle state reads as never-parsed, zero progress.
    let initial = ResolutionState::Unindexed { needed: false };
    let progress = progress_of(initial.clone());
    assert!(!progress.is_complete(), "a new project must not read as parsed");
    assert_eq!(progress.current_phase(), None, "no parse phase is active yet");

    // Gated: postgres reports the same for a freshly registered package.
    let Some(pool) = common::postgres_pool("new_project_is_unparsed").await else { return };
    let store = common::global_store(pool).await;
    let package = common::rust_package(&common::unique_rust_name("fresh"), "1.0.0");
    store
        .upsert(&common::global_package(package.clone(), initial.clone()))
        .await
        .expect("registering a fresh package succeeds");
    assert_eq!(
        store.get_state(package.id()).await.expect("the fresh package has a lifecycle row"),
        initial,
        "a newly registered project must start unparsed"
    );
    assert_eq!(
        store.generation(package.id()).await.expect("generation query succeeds"),
        None,
        "an unparsed project has no recorded snapshot hash"
    );
}

/// A successfully indexed project reports healthy + parsed + loaded.
///
/// Assert: after indexing, health is parsed and loaded into the runtime stores.
#[tokio::test]
async fn indexed_project_is_healthy_and_loaded() {
    // Pure: all backends healthy rolls up to Ready (serving)...
    let verdict = aggregate(&all_healthy());
    assert_eq!(verdict, Health::Ready);
    assert!(verdict.is_serving());

    // ...a Stored project reads as fully parsed...
    let hash = ContentHash::of_bytes(b"indexed snapshot");
    assert!(progress_of(ResolutionState::Stored { hash }).is_complete());

    // ...and "loaded" means every derived store has materialized it.
    let loaded = StoreLinks { vector: true, graph: true, text: true };
    assert!(loaded.fully_linked());

    // Gated: postgres records the indexed state + snapshot.
    let Some(pool) = common::postgres_pool("indexed_project_is_healthy_and_loaded").await else {
        return;
    };
    let store = common::global_store(pool).await;
    let package = common::rust_package(&common::unique_rust_name("indexed"), "1.0.0");
    store
        .upsert(&common::global_package(package.clone(), ResolutionState::Stored { hash }))
        .await
        .expect("publishing an indexed package succeeds");
    assert_eq!(
        store.get_state(package.id()).await.expect("state resolves"),
        ResolutionState::Stored { hash },
        "an indexed project must read parsed (Stored)"
    );
    assert_eq!(
        store.generation(package.id()).await.expect("generation resolves"),
        Some(hash),
        "the loaded snapshot hash must be recorded"
    );
}

/// A failed parse reports degraded with the error recorded.
///
/// Assert: a parse failure flips health to degraded and stores the reason.
#[tokio::test]
async fn failed_parse_is_degraded() {
    // Pure: an impaired derived backend degrades (still serving), and the
    // verdict carries exactly which backend is impaired...
    let mut probes = all_healthy();
    probes.retain(|probe| probe.backend != BackendKind::Qdrant);
    probes.push(probe(BackendKind::Qdrant, false));
    let verdict = aggregate(&probes);
    assert_eq!(verdict, Health::Degraded(vec![BackendKind::Qdrant]));
    assert!(verdict.is_serving());

    // ...while a required backend down means not serving at all.
    let mut spine_down = all_healthy();
    spine_down.retain(|probe| probe.backend != BackendKind::Postgres);
    spine_down.push(probe(BackendKind::Postgres, false));
    assert_eq!(aggregate(&spine_down), Health::Down);
    assert!(!aggregate(&spine_down).is_serving());

    // Gated: the failure — reason included — is recorded in postgres.
    let Some(pool) = common::postgres_pool("failed_parse_is_degraded").await else { return };
    let store = common::global_store(pool).await;
    let package = common::rust_package(&common::unique_rust_name("degraded"), "1.0.0");
    let failure = parse_failure("rustc exited with signal 9");
    store
        .upsert(&common::global_package(package.clone(), ResolutionState::Failed(failure.clone())))
        .await
        .expect("recording a failed parse succeeds");

    let state = store.get_state(package.id()).await.expect("state resolves");
    let ResolutionState::Failed(recorded) = state else {
        panic!("a failed parse must read Failed, got {state:?}");
    };
    assert_eq!(recorded.error, failure.error, "the failure reason must be stored verbatim");
    assert_eq!(recorded.phase, Phase::Compiling, "the failing phase must be recorded");
    assert_eq!(recorded.attempts, failure.attempts);
}

/// Health reflects whether a project is loaded into the runtime stores.
///
/// Assert: a project parsed but not yet loaded is distinguishable from a fully
///   loaded one.
#[tokio::test]
async fn health_distinguishes_parsed_from_loaded() {
    let hash = ContentHash::of_bytes(b"parsed snapshot");
    let parsed = ResolutionState::Stored { hash };

    // Parsed is the lifecycle fact; loaded is the per-store link bitmap the
    // outbox pollers advance. A parsed-but-unloaded project has the first
    // without the second.
    assert!(progress_of(parsed.clone()).is_complete(), "the project is parsed");
    let just_parsed = StoreLinks::default();
    assert!(
        !just_parsed.fully_linked(),
        "a freshly parsed project is not yet loaded into any runtime store"
    );

    // The distinction is per-store: text loaded alone is still not "loaded".
    let partially_loaded = StoreLinks { text: true, ..StoreLinks::default() };
    assert!(!partially_loaded.fully_linked());
    assert_ne!(just_parsed, partially_loaded, "partial materialization must be observable");

    let fully_loaded = StoreLinks { vector: true, graph: true, text: true };
    assert!(fully_loaded.fully_linked(), "all three derived stores caught up means loaded");
    assert_ne!(partially_loaded, fully_loaded);
}
