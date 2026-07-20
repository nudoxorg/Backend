//! Diagram: **catalog-backed queue coordination and parse status.**
//!
//! TDD specs for `registry::queue`. The retry-policy arithmetic runs pure; the
//! durable queue itself is scratch-backed (SQLite), so all specs run offline —
//! no network required.

mod common;

use std::{collections::HashSet, num::NonZeroU32, sync::{Arc, Mutex}, time::Duration};

use chrono::Utc;

use heart::{ContentHash, FailureKind, Freshness, Phase, ResolutionState};
use registry::queue::{JobId, LeasedJob, Queue, RetryDecision, RetryPolicy};

/// The retry policy every spec runs under: generous lease, tight ceiling.
fn policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: NonZeroU32::new(3).expect("3 is non-zero"),
        base_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_secs(1),
    }
}

/// A fresh scratch-backed queue.
fn make_queue(label: &str) -> Queue {
    let scratch_path = std::env::temp_dir()
        .join(format!("registry-queue-test-{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&scratch_path).expect("temp dir created");
    let scratch = index::scratch::ScratchStore::open(&scratch_path.join("scratch.sqlite"))
        .expect("scratch store opens");
    Queue::new(Arc::new(Mutex::new(scratch)), format!("worker-{label}"), policy())
}

/// A worker lease long enough that no test races its own expiry.
const LEASE: Duration = Duration::from_secs(300);

/// Register `count` unique packages and enqueue each, returning their ids.
async fn enqueue_fixtures(
    store: &registry::index::GlobalStore<index::engine::memory::MemoryEngine>,
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
fn claimed_of<'a>(
    jobs: &'a [LeasedJob],
    ours: &HashSet<heart::PackageId>,
) -> Vec<&'a LeasedJob> {
    jobs.iter().filter(|job| ours.contains(&job.package())).collect()
}

/// Parse work is enqueued and drained in coordination order.
///
/// Assert: enqueued packages are handed out to workers and removed atomically
///   (no double-processing).
#[tokio::test]
async fn parse_work_is_enqueued_and_drained() {
    let (store, _writer) = common::catalog_store("parse_work_is_enqueued_and_drained");
    let queue = make_queue("drain");
    let ours = enqueue_fixtures(&store, &queue, "drain", 3).await;

    let first_worker = queue.dequeue_batch(10_000, LEASE).await.expect("first drain succeeds");
    let second_worker = queue.dequeue_batch(10_000, LEASE).await.expect("second drain succeeds");

    let first_claimed = claimed_of(&first_worker, &ours);
    let second_claimed = claimed_of(&second_worker, &ours);
    let mut seen: HashSet<heart::PackageId> = HashSet::new();
    for job in first_claimed.iter().chain(second_claimed.iter()) {
        assert!(
            seen.insert(job.package()),
            "package {} was handed to two workers (double-processing)",
            job.package()
        );
        assert!(
            job.lease_until() > Utc::now() - chrono::Duration::seconds(1),
            "a claimed job must carry its lease"
        );
    }
    assert_eq!(seen, ours, "every enqueued package must be drained exactly once");

    for job in first_worker.into_iter().filter(|j| ours.contains(&j.package())) {
        queue
            .complete(job, &ResolutionState::Stored { hash: ContentHash::of_bytes(b"done") })
            .await
            .expect("a leased job settles");
    }
}

/// Parse status is tracked through its lifecycle.
///
/// Assert: a unit of work moves queued -> running -> done in the catalog.
#[tokio::test]
async fn parse_status_is_tracked() {
    // Pure: the failed branch of the lifecycle is policy-decided.
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

    let (store, _writer) = common::catalog_store("parse_status_is_tracked");
    let queue = make_queue("status");

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
        "the lifecycle must land done in the catalog"
    );
}

/// Concurrent parses are load-balanced under a bound.
///
/// Assert: no more than the configured number of parses run at once.
#[tokio::test]
async fn concurrent_parses_are_bounded() {
    let (store, _writer) = common::catalog_store("concurrent_parses_are_bounded");
    let queue = make_queue("bounded");
    enqueue_fixtures(&store, &queue, "bounded", 5).await;

    const BOUND: usize = 2;
    let first = queue.dequeue_batch(BOUND, LEASE).await.expect("bounded drain succeeds");
    assert!(first.len() <= BOUND, "a worker may never claim more than its bound");

    let second = queue.dequeue_batch(BOUND, LEASE).await.expect("second bounded drain succeeds");
    assert!(second.len() <= BOUND);

    let first_ids: HashSet<JobId> = first.iter().map(|job| job.id()).collect();
    assert!(
        second.iter().all(|job| !first_ids.contains(&job.id())),
        "two concurrent bounded claims must be disjoint"
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

    let (store, _writer) = common::catalog_store("stale_libraries_are_requeued");
    let queue = make_queue("stale");

    let package = common::rust_package(&common::unique_rust_name("stale"), "1.0.0");
    store
        .upsert(&common::global_package(
            package.clone(),
            ResolutionState::Stored { hash: recorded },
        ))
        .await
        .expect("the parsed library is on record");

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
#[tokio::test]
async fn drain_is_scoped_per_library() {
    let (store, _writer) = common::catalog_store("drain_is_scoped_per_library");
    let queue = make_queue("scope");

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

    let mut drained = queue.dequeue_batch(10_000, LEASE).await.expect("drain succeeds");
    let tokio_idx = drained.iter().position(|j| j.package() == tokio_like.id())
        .expect("tokio_like job was dequeued");
    let serde_idx = drained.iter().position(|j| j.package() == serde_like.id())
        .expect("serde_like job was dequeued");
    assert_eq!(
        drained.iter().filter(|j| ours.contains(&j.package())).count(),
        2,
        "both libraries' jobs are claimable"
    );

    let tokio_job_id = drained[tokio_idx].id();
    let (first_idx, second_idx) = if serde_idx < tokio_idx {
        (serde_idx, tokio_idx)
    } else {
        (tokio_idx, serde_idx)
    };
    let second_leased = drained.swap_remove(second_idx);
    let first_leased = drained.swap_remove(first_idx);
    let (serde_leased, tokio_leased) = if first_leased.package() == serde_like.id() {
        (first_leased, second_leased)
    } else {
        (second_leased, first_leased)
    };

    queue
        .complete(
            serde_leased,
            &ResolutionState::Stored { hash: ContentHash::of_bytes(b"a") },
        )
        .await
        .expect("the first library settles");

    // The second library's job must still be alive — same id.
    let survivor = queue.enqueue(tokio_like.id()).await.expect("idempotent re-enqueue");
    assert_eq!(
        survivor,
        tokio_job_id,
        "settling one library must not steal or disturb another library's work"
    );
    queue
        .complete(
            tokio_leased,
            &ResolutionState::Stored { hash: ContentHash::of_bytes(b"b") },
        )
        .await
        .expect("the second library settles independently");
}
