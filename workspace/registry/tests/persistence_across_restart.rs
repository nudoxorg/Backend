//! Pipeline part: **registry persistence** (`registry::persist`).
//!
//! TDD specs for surviving a process restart durably, and rehydrating into a
//! clean, re-enqueueable state. The pure reset rule runs unconditionally; the
//! postgres round-trips are gated on `REGISTRY_TEST_POSTGRES`/`DATABASE_URL`.

mod common;

use heart::{ContentHash, Phase, ResolutionState};
use registry::persist::{reconcile_on_start, reset_transient};

/// A published registry survives a restart.
///
/// Assert: after persisting and reloading, the same packages (and their ids)
///   are present.
#[tokio::test]
async fn registry_persists_across_restart() {
    let Some(pool) = common::postgres_pool("registry_persists_across_restart").await else {
        return;
    };
    let package = common::rust_package(&common::unique_rust_name("survivor"), "1.0.0");
    let hash = ContentHash::of_bytes(b"published generation");

    // First process: publish, then die.
    {
        let store = common::global_store(pool.clone()).await;
        store
            .upsert(&common::global_package(package.clone(), ResolutionState::Stored { hash }))
            .await
            .expect("publishing succeeds");
    }

    // Second process: a brand new handle over the durable spine.
    let reloaded = common::global_store(pool).await;
    let record = reloaded.get(package.id()).await.expect("the package survives the restart");
    assert_eq!(record.id, package.id(), "the deterministic id must survive the reload");
    assert_eq!(record.package.coordinates, package.coordinates);
    assert_eq!(record.package.toolchain, package.toolchain);
    assert_eq!(record.state, ResolutionState::Stored { hash }, "terminal state is durable truth");
}

/// Re-adding a persisted package returns the existing id, not a duplicate.
///
/// Assert: adding an identical `PackageKey` after reload returns the original
///   id rather than allocating a new one.
#[tokio::test]
async fn duplicate_add_after_reload_returns_existing_id() {
    // Identity is a recomputation, so the "existing id" is a mathematical
    // fact before it is a storage fact.
    let name = common::unique_rust_name("dedup");
    let original = common::rust_package(&name, "1.0.0");
    let readded = common::rust_package(&name, "1.0.0");
    assert_eq!(original.id(), readded.id(), "an identical key must recompute the same id");

    let Some(pool) =
        common::postgres_pool("duplicate_add_after_reload_returns_existing_id").await
    else {
        return;
    };

    {
        let store = common::global_store(pool.clone()).await;
        store
            .upsert(&common::global_package(
                original.clone(),
                ResolutionState::Unindexed { needed: false },
            ))
            .await
            .expect("first add succeeds");
    }

    // After the reload, adding the identical key upserts the original row.
    let reloaded = common::global_store(pool).await;
    reloaded
        .upsert(&common::global_package(
            readded.clone(),
            ResolutionState::Unindexed { needed: false },
        ))
        .await
        .expect("the duplicate add is an idempotent upsert");
    let record = reloaded.get(original.id()).await.expect("one coherent record resolves");
    assert_eq!(record.id, original.id(), "the duplicate add must resolve to the original id");
}

/// Rehydration clears transient sync progress.
///
/// Assert: a package that was mid-sync when persisted reloads with its sync
///   state reset to idle (so it can be safely re-enqueued).
#[tokio::test]
async fn rehydrate_clears_transient_sync_progress() {
    // Pure: the reset rule — mid-flight progress is meaningless across a
    // restart; everything durable passes through untouched.
    assert_eq!(
        reset_transient(&ResolutionState::Progressing(Phase::Compiling)),
        ResolutionState::Unindexed { needed: false },
        "transient progress must reset to the clean re-enqueueable state"
    );
    let stored = ResolutionState::Stored { hash: ContentHash::of_bytes(b"snapshot") };
    assert_eq!(reset_transient(&stored), stored, "terminal states are durable truth");
    let unstarted = ResolutionState::Unindexed { needed: true };
    assert_eq!(reset_transient(&unstarted), unstarted, "never-started state passes through");

    let Some(pool) = common::postgres_pool("rehydrate_clears_transient_sync_progress").await
    else {
        return;
    };
    let store = common::global_store(pool.clone()).await;
    let queue = registry::queue::Queue::new(
        pool,
        registry::queue::RetryPolicy {
            max_attempts: std::num::NonZeroU32::new(3).expect("3 is non-zero"),
            base_backoff: std::time::Duration::from_millis(10),
            max_backoff: std::time::Duration::from_secs(1),
        },
    );
    let queue = heart::Connect::connect(queue).await.expect("the queue connects");

    // A package dies mid-sync...
    let package = common::rust_package(&common::unique_rust_name("midsync"), "1.0.0");
    store
        .upsert(&common::global_package(
            package.clone(),
            ResolutionState::Progressing(Phase::Extracting),
        ))
        .await
        .expect("the mid-sync state persists");

    // ...and the boot reconciliation rehydrates it clean and re-enqueued.
    let recovered = reconcile_on_start(&store, &queue).await.expect("reconciliation succeeds");
    assert!(
        recovered.reset.contains(&package.id()),
        "the stranded package must be among the reset set"
    );
    assert!(
        recovered.requeued.contains(&package.id()),
        "the stranded package must be re-enqueued for a fresh attempt"
    );
    assert_eq!(
        store.get_state(package.id()).await.expect("state resolves"),
        ResolutionState::Unindexed { needed: false },
        "rehydration must land on the idle, re-enqueueable state"
    );
}
