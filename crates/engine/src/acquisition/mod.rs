//! Bounded source acquisition and versioned source deltas.
//!
//! The registry modules decode ecosystem wire formats.  This module owns the
//! protocol that follows decoding: canonical source identities, coalesced
//! effects, durable publication fences, negative facts, retry/breaker state,
//! and the immutable delta applied to a source snapshot.
#![allow(clippy::module_name_repetitions)]

mod content_addressed;
pub use content_addressed::{
    ArchiveBudget, ArchiveManifest, ArchiveManifestBuilder, ContentAddressedStore,
    ContentStoreError, ObjectAdmission, ResumableTransfer, TransferCheckpoint, TransferId,
    TransferResetReason, TransferTelemetry, TransferValidator,
};

mod delta;
mod freshness;
mod identity;
mod lease;
mod outcome;
mod permit;
mod phase;
mod retry;
mod service;
mod telemetry;

pub use delta::{AcquisitionDelta, DeltaChange, DeltaError};
pub use freshness::FactFreshness;
pub use identity::{
    AcquisitionDeltaId, AcquisitionReceiptId, IdentityError, ManifestEntry, PublicationRootId,
    RawArchiveObjectId, ReleaseClaim, ReleaseClaimId, SourceSnapshot, SourceSnapshotId,
    TreeManifest, TreeManifestId,
};
pub use lease::{
    AcquisitionLease, CasAdmission, LeaseGuard, LeaseStore, PrivateTemp, RootPublisher,
};
pub use outcome::{
    AcquisitionOutcome, CircuitOpen, CorruptReason, NegativeCache, NegativeFact, NegativeFactKind,
    Offline, RejectReason, RetryAt, Unavailable,
};
pub use permit::{BytePermit, BytePermitPool, Permit, PermitError, PermitPool};
pub use phase::{
    AcquisitionPhase, AcquisitionReceipt, AcquisitionRequest, AcquisitionState, Metadata,
    MetadataRecord, Object, Policy, PublishedDelta, Resolve, VerifiedObject, admit_archive,
};
pub use retry::{
    AttemptFailure, CircuitBreaker, CircuitPermit, CircuitState, RetryClass, RetryPolicy,
};
pub use service::{AcquisitionCoordinator, AcquisitionService, RegistryAcquisitionResult};
pub use telemetry::{AcquisitionTelemetry, Telemetry};

use delta::{merge_manifest_entries, snapshot_changes};
use freshness::{
    FactObservation, MAX_FACT_OBSERVATIONS, observed_facts_at, remember_fact_observation,
};
use identity::{
    CHUNK_BYTES, ID_BYTES, canonical_text, collect_directory, digest, frame, now_millis,
};
use lease::TOKEN_COUNTER;
use outcome::{promote_bytes_outcome, promote_void_outcome};
use service::{
    CatalogSnapshotKey, ExistingRegistryTransport, SharedSlot, SharedSlotValue,
    registry_catalog_snapshot, registry_error_outcome, registry_result,
};

#[cfg(test)]
mod tests {
    use super::*;
    use backend_execution::Cancellation;
    use std::sync::{
        Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    use std::{collections::BTreeMap, fs, sync::Arc, time::Duration};

    fn snapshot(value: u8) -> Arc<SourceSnapshot> {
        let object = RawArchiveObjectId::from_bytes(&[value]);
        let manifest = Arc::new(
            TreeManifest::new(vec![ManifestEntry {
                path: Arc::from("src/lib.rs"),
                object,
                mode: 0,
            }])
            .expect("manifest"),
        );
        Arc::new(SourceSnapshot::new([1; 32], [2; 32], 7, manifest, Vec::new()).expect("snapshot"))
    }

    #[test]
    fn fact_observations_are_epoch_bound_and_bounded() {
        let mut observations = BTreeMap::new();
        remember_fact_observation(&mut observations, "pkg:one@1", 0, 7);
        assert_eq!(observed_facts_at(&observations, "pkg:one@1", 7), Some(0));
        assert_eq!(observed_facts_at(&observations, "pkg:one@1", 8), None);

        for index in 1..=MAX_FACT_OBSERVATIONS {
            remember_fact_observation(
                &mut observations,
                &format!("pkg:{index}@1"),
                index as u64,
                7,
            );
        }
        assert_eq!(observations.len(), MAX_FACT_OBSERVATIONS);
        assert!(observed_facts_at(&observations, "pkg:one@1", 7).is_none());
        assert_eq!(
            observed_facts_at(&observations, &format!("pkg:{MAX_FACT_OBSERVATIONS}@1"), 7),
            Some(MAX_FACT_OBSERVATIONS as u64)
        );
    }

    #[test]
    fn delta_binds_roots_and_repeated_apply_is_idempotent() {
        let base = snapshot(1);
        let target = snapshot(2);
        let before = base.manifest().entries()[0].object;
        let after = target.manifest().entries()[0].object;
        let delta = AcquisitionDelta::new(
            &base,
            Arc::clone(&target),
            vec![DeltaChange {
                path: Arc::from("src/lib.rs"),
                before: Some(before),
                before_mode: Some(0),
                after: Some(after),
                after_mode: Some(0),
            }],
        )
        .expect("delta");
        assert_eq!(delta.apply(&base).expect("apply").id(), target.id());
        assert_eq!(delta.apply(&target).expect("repeat").id(), target.id());
        assert!(matches!(
            delta.apply(&snapshot(3)),
            Err(DeltaError::StaleBase { .. })
        ));
    }

    #[test]
    fn sparse_delta_merges_a_large_manifest_without_suffix_shifts() {
        let mut base_entries = Vec::with_capacity(4_096);
        for index in 0..4_096 {
            base_entries.push(ManifestEntry {
                path: Arc::from(format!("src/{index:04}.rs")),
                object: RawArchiveObjectId::from_bytes(&[index as u8]),
                mode: 0o644,
            });
        }
        let base_manifest = Arc::new(TreeManifest::from_sorted(base_entries).expect("base"));
        let base = Arc::new(
            SourceSnapshot::new([1; 32], [2; 32], 7, base_manifest, Vec::new()).expect("snapshot"),
        );
        let mut target_entries = base.manifest().entries().to_vec();
        target_entries[2_048].object = RawArchiveObjectId::from_bytes(b"changed");
        let target_manifest = Arc::new(TreeManifest::from_sorted(target_entries).expect("target"));
        let target = Arc::new(
            SourceSnapshot::new([1; 32], [3; 32], 7, target_manifest, Vec::new())
                .expect("snapshot"),
        );
        let delta = AcquisitionDelta::new(
            &base,
            Arc::clone(&target),
            vec![DeltaChange {
                path: Arc::from("src/2048.rs"),
                before: Some(base.manifest().entries()[2_048].object),
                before_mode: Some(0o644),
                after: Some(target.manifest().entries()[2_048].object),
                after_mode: Some(0o644),
            }],
        )
        .expect("delta");
        let applied = delta.apply(&base).expect("apply");
        assert_eq!(applied.id(), target.id());
        assert_eq!(applied.manifest().entries().len(), 4_096);
        assert_eq!(applied.manifest().entries()[2_048].mode, 0o644);
    }

    #[test]
    fn distinct_keys_run_in_parallel_while_same_key_remains_singleflight() {
        let coordinator = Arc::new(AcquisitionCoordinator::new(64, 128));
        let barrier = Arc::new(Barrier::new(32));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        for index in 0..32_u8 {
            let coordinator = Arc::clone(&coordinator);
            let barrier = Arc::clone(&barrier);
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            threads.push(std::thread::spawn(move || {
                let request = AcquisitionRequest::new(
                    [index; 32],
                    format!("pkg@{index}"),
                    RawArchiveObjectId::from_bytes(&[index]),
                    1,
                    4,
                )
                .expect("request");
                barrier.wait();
                let outcome = coordinator.coordinate(&request, || {
                    let now = active.fetch_add(1, Ordering::AcqRel) + 1;
                    maximum.fetch_max(now, Ordering::AcqRel);
                    std::thread::sleep(Duration::from_millis(20));
                    active.fetch_sub(1, Ordering::AcqRel);
                    AcquisitionOutcome::Hit(Arc::<[u8]>::from(&[index][..]))
                });
                assert!(matches!(outcome, AcquisitionOutcome::Hit(_)));
            }));
        }
        for thread in threads {
            thread.join().expect("parallel caller");
        }
        assert!(maximum.load(Ordering::Acquire) > 1);
        assert_eq!(coordinator.telemetry().snapshot().leaders, 32);
    }

    #[test]
    fn thirty_two_callers_share_one_leader_effect() {
        let artifact = RawArchiveObjectId::from_bytes(b"archive");
        let request =
            Arc::new(AcquisitionRequest::new([9; 32], "pkg@1", artifact, 1, 4).expect("request"));
        let coordinator = Arc::new(AcquisitionCoordinator::new(4, 64));
        let barrier = Arc::new(Barrier::new(32));
        let effects = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        for _ in 0..32 {
            let coordinator = Arc::clone(&coordinator);
            let request = Arc::clone(&request);
            let barrier = Arc::clone(&barrier);
            let effects = Arc::clone(&effects);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                coordinator.coordinate(&request, || {
                    effects.fetch_add(1, Ordering::AcqRel);
                    std::thread::sleep(Duration::from_millis(20));
                    AcquisitionOutcome::Hit(Arc::<[u8]>::from(&b"receipt"[..]))
                })
            }));
        }
        let outcomes: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().expect("join"))
            .collect();
        assert_eq!(effects.load(Ordering::Acquire), 1);
        assert!(outcomes.iter().all(|outcome| {
            matches!(outcome, AcquisitionOutcome::Hit(bytes) if bytes.as_ref() == b"receipt")
        }));
        assert_eq!(coordinator.telemetry().snapshot().leaders, 1);
        assert_eq!(coordinator.telemetry().snapshot().followers, 31);
    }

    #[test]
    fn follower_cancellation_does_not_cancel_leader() {
        let request = Arc::new(
            AcquisitionRequest::new(
                [8; 32],
                "pkg@2",
                RawArchiveObjectId::from_bytes(b"archive-2"),
                1,
                4,
            )
            .expect("request"),
        );
        let coordinator = Arc::new(AcquisitionCoordinator::new(2, 4));
        let leader_coordinator = Arc::clone(&coordinator);
        let leader_request = Arc::clone(&request);
        let leader = std::thread::spawn(move || {
            leader_coordinator.coordinate(&leader_request, || {
                std::thread::sleep(Duration::from_millis(40));
                AcquisitionOutcome::Hit(Arc::<[u8]>::from(&b"shared"[..]))
            })
        });
        std::thread::sleep(Duration::from_millis(5));
        let (cancellation, handle) = Cancellation::new();
        let follower_coordinator = Arc::clone(&coordinator);
        let follower_request = Arc::clone(&request);
        let follower = std::thread::spawn(move || {
            follower_coordinator.coordinate_with_cancellation(
                &follower_request,
                &cancellation,
                || AcquisitionOutcome::Hit(Arc::<[u8]>::from(&b"wrong"[..])),
            )
        });
        std::thread::sleep(Duration::from_millis(10));
        handle.cancel();
        assert!(matches!(
            follower.join().expect("follower"),
            AcquisitionOutcome::Cancelled
        ));
        assert!(
            matches!(leader.join().expect("leader"), AcquisitionOutcome::Hit(bytes) if bytes.as_ref() == b"shared")
        );
    }

    #[test]
    fn lease_and_cas_are_first_writer_wins() {
        let root = std::env::temp_dir().join(format!("acquisition-{}", now_millis()));
        let _ = fs::remove_dir_all(&root);
        let store = LeaseStore::open(&root).expect("store");
        let store_other = LeaseStore::open(&root).expect("store");
        let request = AcquisitionRequest::new(
            [7; 32],
            "pkg@1",
            RawArchiveObjectId::from_bytes(b"archive"),
            1,
            1,
        )
        .expect("request");
        let guard = store
            .acquire(request.work_key(), Duration::from_secs(60))
            .expect("lease");
        assert!(guard.is_some());
        assert!(
            store_other
                .acquire(request.work_key(), Duration::from_secs(60))
                .expect("lease")
                .is_none()
        );
        drop(guard);
        let mut first = store.temp("archive").expect("temp");
        first.write_all(b"bytes").expect("write");
        let object = RawArchiveObjectId::from_bytes(b"bytes");
        assert_eq!(
            store.cas_admit(&mut first, object).expect("cas"),
            CasAdmission::Winner
        );
        let mut second = store_other.temp("archive").expect("temp");
        second.write_all(b"bytes").expect("write");
        assert_eq!(
            store_other.cas_admit(&mut second, object).expect("cas"),
            CasAdmission::Existing
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn transient_failures_never_make_negative_facts_and_breaker_has_one_probe() {
        assert_eq!(AttemptFailure::Timeout.negative_kind(), None);
        assert_eq!(AttemptFailure::Integrity.negative_kind(), None);
        assert_eq!(
            AttemptFailure::NotFound.negative_kind(),
            Some(NegativeFactKind::NotFound)
        );
        let breaker = CircuitBreaker::new(2, Duration::from_millis(10));
        breaker.failure(10);
        breaker.failure(10);
        assert!(breaker.allow(10).is_err());
        let probe = breaker.allow(20).expect("probe");
        assert!(breaker.allow(20).is_err());
        breaker.success();
        drop(probe);
        assert_eq!(breaker.state(), CircuitState::Closed);
        let policy =
            RetryPolicy::with_seed(Duration::from_millis(10), Duration::from_millis(100), 4, 1);
        assert!(policy.delay(8, Some(Duration::from_secs(2))) <= Duration::from_millis(100));
    }
}
