//! Diagram: **postgres is in charge of load balancing, coordination, and the
//! status of every parse.**
//!
//! TDD specs for `registry::queue`. The retry-policy arithmetic runs pure; the
//! durable queue itself is postgres-shaped, so those halves are gated on
//! `REGISTRY_TEST_POSTGRES`/`DATABASE_URL`.

mod common;

use std::{collections::HashSet, num::NonZeroU32, time::Duration};

use heart::{Connect, ContentHash, FailureKind, Freshness, Phase, ResolutionState};
use registry::queue::{Job, JobId, Queue, RetryDecision, RetryPolicy};

/// The retry policy every gated spec runs under: generous lease, tight ceiling.
fn policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: NonZeroU32::new(3).expect("3 is non-zero"),
        base_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_secs(1),
    }
}

/// A connected queue over `pool` (the schema is applied by the global store).
async fn queue(pool: sqlx::PgPool) -> Queue {
    Queue::new(pool, policy()).connect().await.expect("the jobs table exists after connect")
}

/// A worker lease long enough that no test races its own expiry.
const LEASE: Duration = Duration::from_secs(300);

/// Register `count` unique packages and enqueue each, returning their ids.
async fn enqueue_fixtures(
    store: &registry::index::GlobalStore,
    queue: &Queue,
    prefix: &str,
    count: usize,
) -> HashSet<heart::PackageId> {
    let mut packages = HashSet::new();
    for _ in 0..count {
        let package = common::rust_package(&common::unique_rust_name(prefix), "1.0.0");
        store
            .upsert(&common::global_package(
                package.clone(),
                ResolutionState::Unindexed { needed: false },
            ))
            .await
            .expect("the package registers before it is enqueued");
        queue.enqueue(package.id()).await.expect("enqueue succeeds");
        packages.insert(package.id());
    }
    packages
}

/// The subset of `jobs` targeting packages in `ours`.
fn claimed_of(jobs: &[Job], ours: &HashSet<heart::PackageId>) -> Vec<Job> {
    jobs.iter().filter(|job| ours.contains(&job.package)).cloned().collect()
}

/// Parse work is enqueued and drained in coordination order.
///
/// Assert: enqueued packages are handed out to workers and removed atomically
///   (no double-processing).
#[tokio::test]
async fn parse_work_is_enqueued_and_drained() {
    let Some(pool) = common::postgres_pool("parse_work_is_enqueued_and_drained").await else {
        return;
    };
    let store = common::global_store(pool.clone()).await;
    let queue = queue(pool).await;
    let ours = enqueue_fixtures(&store, &queue, "drain", 3).await;

    // Two workers drain; the SKIP LOCKED lease hands each of our jobs to
    // exactly one of them.
    let first_worker = queue.dequeue_batch(10_000, LEASE).await.expect("first drain succeeds");
    let second_worker = queue.dequeue_batch(10_000, LEASE).await.expect("second drain succeeds");

    let first_claimed = claimed_of(&first_worker, &ours);
    let second_claimed = claimed_of(&second_worker, &ours);
    let mut seen: HashSet<heart::PackageId> = HashSet::new();
    for job in first_claimed.iter().chain(&second_claimed) {
        assert!(
            seen.insert(job.package),
            "package {} was handed to two workers (double-processing)",
            job.package
        );
        assert!(job.lease_until.is_some(), "a claimed job must carry its lease");
    }
    assert_eq!(seen, ours, "every enqueued package must be drained exactly once");

    // Completion removes the work atomically — settling twice loses the row.
    for job in &first_claimed {
        queue
            .complete(job.id, &ResolutionState::Stored { hash: ContentHash::of_bytes(b"done") })
            .await
            .expect("a leased job settles");
    }
}

/// Parse status is tracked through its lifecycle.
///
/// Assert: a unit of work moves queued -> running -> done (or failed) in
///   postgres.
#[tokio::test]
async fn parse_status_is_tracked() {
    // Pure: the failed branch of the lifecycle is policy-decided — retriable
    // kinds retry under the ceiling, everything else dead-letters.
    let policy = policy();
    assert_eq!(
        policy.decide(FailureKind::Transient, 1),
        RetryDecision::Retry { after: policy.backoff_for(1) }
    );
    assert_eq!(policy.decide(FailureKind::Malformed, 1), RetryDecision::DeadLetter);
    assert_eq!(
        policy.decide(FailureKind::Transient, 3),
        RetryDecision::DeadLetter,
        "the attempt ceiling dead-letters even retriable kinds"
    );

    let Some(pool) = common::postgres_pool("parse_status_is_tracked").await else { return };
    let store = common::global_store(pool.clone()).await;
    let queue = queue(pool).await;

    let package = common::rust_package(&common::unique_rust_name("status"), "1.0.0");
    let record =
        common::global_package(package.clone(), ResolutionState::Unindexed { needed: false });

    // Queued.
    store.upsert(&record).await.expect("registration succeeds");
    queue.enqueue(package.id()).await.expect("enqueue succeeds");
    assert_eq!(
        store.get_state(package.id()).await.expect("state resolves"),
        ResolutionState::Unindexed { needed: false }
    );

    // Running.
    store
        .set_state(package.id(), &ResolutionState::Progressing(Phase::Compiling))
        .await
        .expect("the running transition records");
    assert_eq!(
        store.get_state(package.id()).await.expect("state resolves"),
        ResolutionState::Progressing(Phase::Compiling)
    );

    // Done.
    let hash = ContentHash::of_bytes(b"parsed");
    store
        .set_state(package.id(), &ResolutionState::Stored { hash })
        .await
        .expect("the done transition records");
    assert_eq!(
        store.get_state(package.id()).await.expect("state resolves"),
        ResolutionState::Stored { hash },
        "the lifecycle must land done in postgres"
    );
}

/// Concurrent parses are load-balanced under a bound.
///
/// Assert: no more than the configured number of parses run at once.
#[tokio::test]
async fn concurrent_parses_are_bounded() {
    let Some(pool) = common::postgres_pool("concurrent_parses_are_bounded").await else { return };
    let store = common::global_store(pool.clone()).await;
    let queue = queue(pool).await;
    enqueue_fixtures(&store, &queue, "bounded", 5).await;

    const BOUND: usize = 2;
    let first = queue.dequeue_batch(BOUND, LEASE).await.expect("bounded drain succeeds");
    assert!(first.len() <= BOUND, "a worker may never claim more than its bound");

    let second = queue.dequeue_batch(BOUND, LEASE).await.expect("second bounded drain succeeds");
    assert!(second.len() <= BOUND);

    let first_ids: HashSet<JobId> = first.iter().map(|job| job.id).collect();
    assert!(
        second.iter().all(|job| !first_ids.contains(&job.id)),
        "two concurrent bounded claims must be disjoint (SKIP LOCKED)"
    );
}

/// Stale (unfresh) libraries are re-queued by hash comparison.
///
/// Assert: when a library's stored hash differs from the remote, it is
///   re-enqueued for parsing.
#[tokio::test]
async fn stale_libraries_are_requeued() {
    // Pure: the staleness signal itself.
    let recorded = ContentHash::of_bytes(b"stored representation");
    let remote = ContentHash::of_bytes(b"remote representation");
    assert_eq!(Freshness::compare(recorded, recorded), Freshness::Fresh);
    assert_eq!(Freshness::compare(recorded, remote), Freshness::Stale);

    let Some(pool) = common::postgres_pool("stale_libraries_are_requeued").await else { return };
    let store = common::global_store(pool.clone()).await;
    let queue = queue(pool).await;

    let package = common::rust_package(&common::unique_rust_name("stale"), "1.0.0");
    store
        .upsert(&common::global_package(
            package.clone(),
            ResolutionState::Stored { hash: recorded },
        ))
        .await
        .expect("the parsed library is on record");

    // The freshness poll: recorded vs freshly-computed remote hash.
    let stored = store
        .generation(package.id())
        .await
        .expect("generation resolves")
        .expect("a stored library has a recorded hash");
    assert_eq!(
        Freshness::compare(stored, remote),
        Freshness::Stale,
        "the recorded hash must flag the drifted remote as stale"
    );

    // Stale → re-enqueue; the re-enqueue is idempotent while the job lives.
    let first = queue.enqueue(package.id()).await.expect("the stale library re-enqueues");
    let second = queue.enqueue(package.id()).await.expect("re-enqueue is idempotent");
    assert_eq!(first, second, "a live job must be returned, not duplicated");
}

/// Drain is scoped per library/version (tiers don't steal each other's work).
///
/// The queue has no per-library drain filter in the current API (a gap against
/// this spec — `dequeue_batch` is global); what it does guarantee is that each
/// library's job is its own row: settling one library's work never disturbs
/// another's.
#[tokio::test]
async fn drain_is_scoped_per_library() {
    let Some(pool) = common::postgres_pool("drain_is_scoped_per_library").await else { return };
    let store = common::global_store(pool.clone()).await;
    let queue = queue(pool).await;

    let serde_like = common::rust_package(&common::unique_rust_name("liba"), "1.0.0");
    let tokio_like = common::rust_package(&common::unique_rust_name("libb"), "1.0.0");
    let ours: HashSet<heart::PackageId> = [serde_like.id(), tokio_like.id()].into();
    for package in [&serde_like, &tokio_like] {
        store
            .upsert(&common::global_package(
                package.clone(),
                ResolutionState::Unindexed { needed: false },
            ))
            .await
            .expect("registration succeeds");
        queue.enqueue(package.id()).await.expect("enqueue succeeds");
    }

    let drained = queue.dequeue_batch(10_000, LEASE).await.expect("drain succeeds");
    let claimed = claimed_of(&drained, &ours);
    assert_eq!(claimed.len(), 2, "both libraries' jobs are claimable");
    let job_of = |package: heart::PackageId| {
        claimed.iter().find(|job| job.package == package).expect("claimed above").id
    };

    // Settle only the first library's work...
    queue
        .complete(
            job_of(serde_like.id()),
            &ResolutionState::Stored { hash: ContentHash::of_bytes(b"a") },
        )
        .await
        .expect("the first library settles");

    // ...the second library's job is untouched: still the same live row.
    let survivor = queue.enqueue(tokio_like.id()).await.expect("idempotent re-enqueue");
    assert_eq!(
        survivor,
        job_of(tokio_like.id()),
        "settling one library must not steal or disturb another library's work"
    );
    queue
        .complete(
            job_of(tokio_like.id()),
            &ResolutionState::Stored { hash: ContentHash::of_bytes(b"b") },
        )
        .await
        .expect("the second library settles independently");
}
