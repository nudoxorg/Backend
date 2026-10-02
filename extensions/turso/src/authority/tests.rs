//! Selection authority transaction, reopen, and fencing contracts.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::thread;

static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

struct AcceptVerifiedClosure;

impl DurableClosureVerifier for AcceptVerifiedClosure {
    type Error = &'static str;

    fn verify_closure(&self, claim: &ClosureClaim) -> Result<(), Self::Error> {
        if claim.target_root() == claim.closure_id() {
            Ok(())
        } else {
            Err("fixture closure claim mismatch")
        }
    }
}

fn path() -> PathBuf {
    let serial = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "backend-turso-index-authority-{}-{serial}.db",
        std::process::id()
    ))
}

fn namespace() -> AuthorityNamespace {
    AuthorityNamespace::package_metadata("pkg:cargo/widget", "registry:crates-io", "main", "stable")
        .unwrap_or_else(|error| panic!("namespace: {error}"))
}

fn observation(value: SourceObservationValue, observed_at_ms: u64) -> SourceObservation {
    SourceObservation::new(namespace(), Some([11; 32]), observed_at_ms, value)
        .unwrap_or_else(|error| panic!("observation: {error}"))
}

fn candidate(
    attempt: CandidateAttempt,
    candidate: u8,
    root: u8,
    pack: u8,
    closure: u8,
) -> CandidateGeneration {
    attempt.candidate([candidate; 32], [root; 32], [pack; 32], [closure; 32])
}

#[test]
fn attempt_recovery_requires_every_current_durable_fence() {
    futures_executor::block_on(async {
        let path = path();
        let mut authority = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        let observed = authority
            .record_source_observation(observation(SourceObservationValue::KnownCount(3), 100))
            .await
            .unwrap_or_else(|error| panic!("observe: {error}"));
        let attempt = authority
            .begin_attempt(&namespace(), [91; 32], &observed)
            .await
            .unwrap_or_else(|error| panic!("begin: {error}"));
        let claim = attempt.recovery_claim();
        drop(authority);

        let mut authority = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("cold open: {error}"));
        assert_eq!(
            authority
                .recover_candidate_attempt(&claim)
                .await
                .unwrap_or_else(|error| panic!("recover exact: {error}")),
            attempt
        );
        let mut mutated = claim.clone();
        mutated.fence[0] ^= 1;
        assert!(matches!(
            authority.recover_candidate_attempt(&mutated).await,
            Err(AuthorityError::StaleAttempt)
        ));
        let mut mutated = claim.clone();
        mutated.input_digest[0] ^= 1;
        assert!(matches!(
            authority.recover_candidate_attempt(&mutated).await,
            Err(AuthorityError::StaleAttempt)
        ));
        let mut mutated = claim.clone();
        mutated.attempt_id[0] ^= 1;
        assert!(matches!(
            authority.recover_candidate_attempt(&mutated).await,
            Err(AuthorityError::StaleAttempt)
        ));
        let mut mutated = claim.clone();
        mutated.observation_sequence += 1;
        assert!(matches!(
            authority.recover_candidate_attempt(&mutated).await,
            Err(AuthorityError::StaleObservation)
        ));
        let mut mutated = claim.clone();
        mutated.base_generation = 1;
        mutated.base_root = Some([17; 32]);
        assert!(matches!(
            authority.recover_candidate_attempt(&mutated).await,
            Err(AuthorityError::StaleFrontier)
        ));
        let replacement = authority
            .begin_attempt(&namespace(), [91; 32], &observed)
            .await
            .unwrap_or_else(|error| panic!("replace: {error}"));
        assert!(matches!(
            authority.recover_candidate_attempt(&claim).await,
            Err(AuthorityError::StaleAttempt)
        ));
        let replacement_claim = replacement.recovery_claim();
        let candidate = candidate(replacement, 92, 93, 94, 93);
        let receipt = authority
            .verify_closure(&candidate, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify: {error}"));
        authority
            .compare_and_select(candidate, receipt)
            .await
            .unwrap_or_else(|error| panic!("select: {error}"));
        drop(authority);
        let authority = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("cold open after selection: {error}"));
        assert!(matches!(
            authority
                .recover_candidate_attempt(&replacement_claim)
                .await,
            Err(AuthorityError::StaleFrontier | AuthorityError::StaleAttempt)
        ));
    });
}

#[test]
fn retired_cancelled_attempt_reopens_terminal_without_moving_selected_head() {
    futures_executor::block_on(async {
        let path = path();
        let mut authority = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        let observed = authority
            .record_source_observation(observation(SourceObservationValue::KnownCount(1), 100))
            .await
            .unwrap_or_else(|error| panic!("observe: {error}"));

        // Establish a real nonempty selected head before the cancellation
        // attempt. This proves cleanup preserves usable prior authority.
        let selected_attempt = authority
            .begin_attempt(&namespace(), [91; 32], &observed)
            .await
            .unwrap_or_else(|error| panic!("begin selected attempt: {error}"));
        let selected_candidate = candidate(selected_attempt, 92, 93, 94, 93);
        let selected_receipt = authority
            .verify_closure(&selected_candidate, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify selected candidate: {error}"));
        authority
            .compare_and_select(selected_candidate, selected_receipt)
            .await
            .unwrap_or_else(|error| panic!("select prior generation: {error}"));

        let attempt = authority
            .begin_attempt(&namespace(), [95; 32], &observed)
            .await
            .unwrap_or_else(|error| panic!("begin: {error}"));
        let before = authority
            .selected_frontier(&namespace())
            .await
            .unwrap_or_else(|error| panic!("read initial head: {error}"));
        assert!(before.is_some(), "fixture must have a selected generation");

        let mut forged = attempt.clone();
        forged.fence[0] ^= 1;
        assert!(matches!(
            authority
                .retire_attempt(&forged, CandidateAttemptRetirementReason::Cancelled)
                .await,
            Err(AuthorityError::StaleAttempt)
        ));
        assert_eq!(
            authority
                .recover_candidate_attempt(&attempt.recovery_claim())
                .await
                .unwrap_or_else(|error| panic!("forged cleanup changed active attempt: {error}")),
            attempt
        );

        authority
            .retire_attempt(&attempt, CandidateAttemptRetirementReason::Cancelled)
            .await
            .unwrap_or_else(|error| panic!("retire: {error}"));
        // Repeated cleanup cannot overwrite the first typed durable cause.
        authority
            .retire_attempt(&attempt, CandidateAttemptRetirementReason::Failed)
            .await
            .unwrap_or_else(|error| panic!("repeat retirement: {error}"));
        assert_eq!(
            authority
                .selected_frontier(&namespace())
                .await
                .unwrap_or_else(|error| panic!("read unchanged head: {error}")),
            before
        );
        assert!(matches!(
            authority
                .recover_candidate_attempt(&attempt.recovery_claim())
                .await,
            Err(AuthorityError::StaleAttempt)
        ));

        // Reopen after dropping every connection. Neither the canceled active
        // attempt nor a later cleanup reason can displace the prior head.
        drop(authority);
        let authority = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("cold reopen after cancellation: {error}"));
        assert_eq!(
            authority
                .selected_frontier(&namespace())
                .await
                .unwrap_or_else(|error| panic!("read cold selected head: {error}")),
            before
        );
        assert!(matches!(
            authority
                .recover_candidate_attempt(&attempt.recovery_claim())
                .await,
            Err(AuthorityError::StaleAttempt)
        ));

        let mut active_rows = authority
            .connection
            .query(
                "SELECT 1 FROM backend_index_authority_attempts \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6 AND attempt_id=?7",
                turso::params![
                    namespace().package.as_ref(),
                    namespace().source.as_ref(),
                    namespace().branch.as_ref(),
                    namespace().environment.as_ref(),
                    namespace().plane.sql_parts().0,
                    namespace().plane.sql_parts().1,
                    attempt.attempt_id().as_slice()
                ],
            )
            .await
            .unwrap_or_else(|error| panic!("read active attempts: {error}"));
        assert!(
            active_rows
                .next()
                .await
                .unwrap_or_else(|error| panic!("active row: {error}"))
                .is_none(),
            "retired attempt remained recoverably pending"
        );
        let mut terminal_rows = authority
            .connection
            .query(
                "SELECT terminal_reason FROM backend_index_authority_attempt_terminals \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6 AND attempt_id=?7",
                turso::params![
                    namespace().package.as_ref(),
                    namespace().source.as_ref(),
                    namespace().branch.as_ref(),
                    namespace().environment.as_ref(),
                    namespace().plane.sql_parts().0,
                    namespace().plane.sql_parts().1,
                    attempt.attempt_id().as_slice()
                ],
            )
            .await
            .unwrap_or_else(|error| panic!("read terminal attempt: {error}"));
        let row = terminal_rows
            .next()
            .await
            .unwrap_or_else(|error| panic!("terminal row: {error}"))
            .unwrap_or_else(|| panic!("terminal attempt row missing"));
        assert_eq!(
            row.get::<i64>(0)
                .unwrap_or_else(|error| panic!("terminal reason: {error}")),
            CandidateAttemptRetirementReason::Cancelled.sql_code()
        );
    });
}

#[test]
fn newer_observation_proves_unselected_same_epoch_attempt_invalid_without_minting_replacement() {
    futures_executor::block_on(async {
        let path = path();
        let mut authority = TursoAuthority::open(&path).await.expect("open authority");
        let first = authority
            .record_source_observation(observation(SourceObservationValue::KnownCount(1), 100))
            .await
            .expect("first observation");
        let attempt = authority
            .begin_attempt(&namespace(), [91; 32], &first)
            .await
            .expect("attempt");
        let claim = attempt.recovery_claim();
        assert_eq!(
            authority
                .attempt_invalidated_by_observation_proof(&claim)
                .await
                .expect("proof read"),
            None,
        );
        let second = authority
            .record_source_observation(
                SourceObservation::new(
                    namespace(),
                    Some([12; 32]),
                    101,
                    SourceObservationValue::KnownCount(2),
                )
                .expect("second observation value"),
            )
            .await
            .expect("second observation");
        assert_eq!(second.sequence(), first.sequence() + 1);
        assert!(matches!(
            authority.recover_candidate_attempt(&claim).await,
            Err(AuthorityError::StaleObservation)
        ));
        assert_eq!(
            authority
                .superseded_attempt_proof(
                    &namespace(),
                    *attempt.attempt_id(),
                    attempt.epoch(),
                    attempt.fence_bytes()
                )
                .await
                .expect("superseded read"),
            None,
        );
        let proof = authority
            .attempt_invalidated_by_observation_proof(&claim)
            .await
            .expect("invalidated read")
            .expect("new observation proof");
        assert_eq!(proof.namespace(), &namespace());
        assert_eq!(proof.attempt_id(), attempt.attempt_id());
        assert_eq!(proof.epoch(), attempt.epoch());
        assert_eq!(proof.fence(), &attempt.fence_bytes());
        assert_eq!(proof.input_digest(), attempt.input_digest());
        assert_eq!(proof.attempt_observation_sequence(), first.sequence());
        assert_eq!(proof.current_observation_sequence(), second.sequence());
        assert_eq!(proof.current_revision(), Some([12; 32]));
        // The owner may close the attempt after observing the newer source
        // frontier. The close records Superseded, removes it from the
        // recoverable pending table, and keeps the exact rejection proof.
        authority
            .retire_attempt(&attempt, CandidateAttemptRetirementReason::Cancelled)
            .await
            .expect("retire invalidated attempt");
        assert_eq!(
            authority
                .attempt_invalidated_by_observation_proof(&claim)
                .await
                .expect("closed invalidation proof"),
            Some(proof.clone())
        );
        let mut forged = claim.clone();
        forged.fence[0] ^= 1;
        assert_eq!(
            authority
                .attempt_invalidated_by_observation_proof(&forged)
                .await
                .expect("forged read"),
            None
        );
        let mut forged = claim.clone();
        forged.input_digest[0] ^= 1;
        assert_eq!(
            authority
                .attempt_invalidated_by_observation_proof(&forged)
                .await
                .expect("wrong input read"),
            None
        );
        let mut forged = claim.clone();
        forged.observation_sequence = second.sequence();
        assert_eq!(
            authority
                .attempt_invalidated_by_observation_proof(&forged)
                .await
                .expect("wrong old observation read"),
            None
        );
        let mut forged = claim.clone();
        forged.namespace = AuthorityNamespace::package_metadata(
            "pkg:cargo/other",
            "registry:crates-io",
            "main",
            "stable",
        )
        .expect("other namespace");
        assert_eq!(
            authority
                .attempt_invalidated_by_observation_proof(&forged)
                .await
                .expect("wrong namespace read"),
            None
        );
        drop(authority);
        let mut authority = TursoAuthority::open(&path).await.expect("cold reopen");
        assert_eq!(
            authority
                .attempt_invalidated_by_observation_proof(&claim)
                .await
                .expect("cold proof"),
            Some(proof)
        );
        authority
            .begin_attempt(&namespace(), [92; 32], &second)
            .await
            .expect("replacement attempt");
        assert_eq!(
            authority
                .attempt_invalidated_by_observation_proof(&claim)
                .await
                .expect("new epoch proof"),
            None
        );
        assert!(
            authority
                .superseded_attempt_proof(
                    &namespace(),
                    *attempt.attempt_id(),
                    attempt.epoch(),
                    attempt.fence_bytes()
                )
                .await
                .expect("closed superseded proof")
                .is_some()
        );

        let selected_path = self::path();
        let mut selected = TursoAuthority::open(&selected_path)
            .await
            .expect("selected authority");
        let observed = selected
            .record_source_observation(observation(SourceObservationValue::KnownCount(1), 100))
            .await
            .expect("selected observation");
        let selected_attempt = selected
            .begin_attempt(&namespace(), [93; 32], &observed)
            .await
            .expect("selected attempt");
        let selected_claim = selected_attempt.recovery_claim();
        let candidate = candidate(selected_attempt, 94, 95, 96, 95);
        let receipt = selected
            .verify_closure(&candidate, &AcceptVerifiedClosure)
            .expect("selected receipt");
        selected
            .compare_and_select(candidate, receipt)
            .await
            .expect("select");
        selected
            .record_source_observation(observation(SourceObservationValue::KnownCount(2), 101))
            .await
            .expect("later selected observation");
        assert_eq!(
            selected
                .attempt_invalidated_by_observation_proof(&selected_claim)
                .await
                .expect("selected negative"),
            None
        );
    });
}

#[test]
fn committed_frontier_reopens_exactly_and_projection_lag_is_visible() {
    futures_executor::block_on(async {
        let scope = namespace();
        assert_eq!(scope.namespace_id(), namespace().namespace_id());
        assert_ne!(
            scope.namespace_id(),
            AuthorityNamespace::package_metadata(
                "pkg:cargo/widget",
                "registry:crates-io",
                "next",
                "stable",
            )
            .unwrap_or_else(|error| panic!("alternate namespace: {error}"))
            .namespace_id()
        );
        let path = path();
        let mut authority = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        for table in [
            "backend_index_authority_meta",
            "backend_index_authority_scopes",
            "backend_index_authority_observations",
            "backend_index_authority_attempts",
            "backend_index_authority_frontiers",
            "backend_index_authority_generation_history",
            "backend_index_authority_projection_watermarks",
        ] {
            let mut columns = authority
                .connection
                .query(&format!("PRAGMA table_info({table})"), ())
                .await
                .unwrap_or_else(|error| panic!("inspect authority columns: {error}"));
            while let Some(row) = columns
                .next()
                .await
                .unwrap_or_else(|error| panic!("read authority columns: {error}"))
            {
                let name: String = row
                    .get(1)
                    .unwrap_or_else(|error| panic!("column name: {error}"));
                assert!(!name.contains("payload") && !name.contains("bytes"));
            }
        }
        let source = authority
            .record_source_observation(observation(SourceObservationValue::KnownCount(0), 100))
            .await
            .unwrap_or_else(|error| panic!("record zero observation: {error}"));
        let attempt = authority
            .begin_attempt(&namespace(), [21; 32], &source)
            .await
            .unwrap_or_else(|error| panic!("begin attempt: {error}"));
        let candidate = candidate(attempt, 31, 41, 51, 41);
        let receipt = authority
            .verify_closure(&candidate, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify closure: {error}"));
        let durable_observation = authority
            .latest_source_observation(&namespace())
            .await
            .unwrap_or_else(|error| panic!("reopen observation checkpoint: {error}"))
            .unwrap_or_else(|| panic!("missing durable observation"));
        assert_eq!(durable_observation, source);
        assert_eq!(
            authority
                .selected_frontier(&namespace())
                .await
                .unwrap_or_else(|error| panic!("read pre-selection checkpoint: {error}")),
            None
        );
        drop(authority);
        let mut authority = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("reopen before selection: {error}"));
        let selected = authority
            .compare_and_select(candidate, receipt)
            .await
            .unwrap_or_else(|error| panic!("select: {error}"));
        assert_eq!(selected.generation(), 1);
        assert_eq!(selected.target_root(), &[41; 32]);
        let (attempt_id, fence) = selected.scheduler_fence();
        assert_ne!(attempt_id, 0);
        assert_ne!(fence, [0; 32]);
        assert!(
            selected
                .projections()
                .iter()
                .all(|watermark| !watermark.is_current())
        );
        assert_eq!(
            selected.observation().observation().value(),
            &SourceObservationValue::KnownCount(0)
        );

        let catalog = authority
            .mark_projection_current(&namespace(), ProjectionKind::Catalog, 1, [41; 32])
            .await
            .unwrap_or_else(|error| panic!("mark catalog: {error}"));
        assert!(catalog.is_current());
        drop(authority);

        let reopened = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("cold reopen: {error}"));
        let persisted = reopened
            .selected_frontier(&namespace())
            .await
            .unwrap_or_else(|error| panic!("read selected: {error}"))
            .unwrap_or_else(|| panic!("missing selected frontier"));
        assert_eq!(persisted.generation(), selected.generation());
        assert_eq!(persisted.candidate_id(), selected.candidate_id());
        assert_eq!(persisted.attempt(), selected.attempt());
        assert_eq!(persisted.scheduler_fence(), selected.scheduler_fence());
        assert_eq!(persisted.input_digest(), selected.input_digest());
        assert_eq!(persisted.target_root(), selected.target_root());
        assert_eq!(persisted.pack_id(), selected.pack_id());
        assert_eq!(persisted.closure_id(), selected.closure_id());
        assert_eq!(persisted.observation(), selected.observation());
        assert!(persisted.projections().iter().any(|watermark| {
            watermark.projector() == ProjectionKind::Catalog && watermark.is_current()
        }));
        assert!(persisted.projections().iter().any(|watermark| {
            watermark.projector() == ProjectionKind::Graph && !watermark.is_current()
        }));
    });
}

#[test]
fn latest_attempt_and_observation_fence_old_completions() {
    futures_executor::block_on(async {
        let path = path();
        let mut authority = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        let zero = authority
            .record_source_observation(observation(SourceObservationValue::KnownCount(0), 100))
            .await
            .unwrap_or_else(|error| panic!("record zero: {error}"));
        let first_attempt = authority
            .begin_attempt(&namespace(), [22; 32], &zero)
            .await
            .unwrap_or_else(|error| panic!("first attempt: {error}"));
        let first_candidate = candidate(first_attempt, 32, 42, 52, 42);
        let first_receipt = authority
            .verify_closure(&first_candidate, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify first: {error}"));

        let replacement_attempt = authority
            .begin_attempt(&namespace(), [22; 32], &zero)
            .await
            .unwrap_or_else(|error| panic!("replacement attempt: {error}"));
        let replacement_candidate = candidate(replacement_attempt, 37, 47, 57, 47);
        let replacement_receipt = authority
            .verify_closure(&replacement_candidate, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify replacement: {error}"));
        assert!(matches!(
            authority
                .compare_and_select(first_candidate, first_receipt)
                .await,
            Err(AuthorityError::StaleAttempt)
        ));

        let unknown = authority
            .record_source_observation(observation(SourceObservationValue::Unknown, 101))
            .await
            .unwrap_or_else(|error| panic!("record unknown: {error}"));
        assert_eq!(
            unknown.observation().value(),
            &SourceObservationValue::Unknown
        );
        assert!(matches!(
            authority.begin_attempt(&namespace(), [23; 32], &zero).await,
            Err(AuthorityError::StaleObservation)
        ));

        assert!(matches!(
            authority
                .compare_and_select(replacement_candidate, replacement_receipt)
                .await,
            Err(AuthorityError::StaleObservation)
        ));
        assert_eq!(
            authority
                .selected_frontier(&namespace())
                .await
                .unwrap_or_else(|error| panic!("read empty head: {error}")),
            None
        );

        let second_attempt = authority
            .begin_attempt(&namespace(), [23; 32], &unknown)
            .await
            .unwrap_or_else(|error| panic!("second attempt: {error}"));
        let second_candidate = candidate(second_attempt, 33, 43, 53, 43);
        let second_receipt = authority
            .verify_closure(&second_candidate, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify second: {error}"));
        let selected = authority
            .compare_and_select(second_candidate, second_receipt)
            .await
            .unwrap_or_else(|error| panic!("select second: {error}"));
        assert_eq!(
            selected.observation().observation().value(),
            &SourceObservationValue::Unknown
        );
        let unavailable = authority
            .record_source_observation(observation(
                SourceObservationValue::Unavailable("source offline".into()),
                102,
            ))
            .await
            .unwrap_or_else(|error| panic!("record unavailable: {error}"));
        assert_eq!(
            unavailable.observation().value(),
            &SourceObservationValue::Unavailable("source offline".into())
        );
        assert_eq!(
            authority
                .latest_source_observation(&namespace())
                .await
                .unwrap_or_else(|error| panic!("read unavailable: {error}")),
            Some(unavailable)
        );
    });
}

#[test]
fn one_attempt_cannot_select_two_racing_completions() {
    futures_executor::block_on(async {
        let path = path();
        let mut setup = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("open setup: {error}"));
        let observed = setup
            .record_source_observation(observation(SourceObservationValue::KnownCount(2), 200))
            .await
            .unwrap_or_else(|error| panic!("record: {error}"));
        let attempt = setup
            .begin_attempt(&namespace(), [24; 32], &observed)
            .await
            .unwrap_or_else(|error| panic!("begin: {error}"));
        let candidate = candidate(attempt, 34, 44, 54, 44);
        let first_receipt = setup
            .verify_closure(&candidate, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify first: {error}"));
        drop(setup);
        let barrier = Arc::new(Barrier::new(3));
        let (send, receive) = mpsc::channel();
        let left_barrier = Arc::clone(&barrier);
        let left_send = send.clone();
        let left_candidate = candidate.clone();
        let left_path = path.clone();
        let left_thread = thread::spawn(move || {
            left_barrier.wait();
            let mut left = futures_executor::block_on(TursoAuthority::open(&left_path))
                .unwrap_or_else(|error| panic!("open left: {error}"));
            let result =
                futures_executor::block_on(left.compare_and_select(left_candidate, first_receipt));
            let _ = left_send.send(
                result
                    .map(|selected| selected.generation())
                    .map_err(|error| error.to_string()),
            );
        });
        let right_barrier = Arc::clone(&barrier);
        let right_path = path.clone();
        let right_thread = thread::spawn(move || {
            right_barrier.wait();
            let mut right = futures_executor::block_on(TursoAuthority::open(&right_path))
                .unwrap_or_else(|error| panic!("open right: {error}"));
            let receipt = right
                .verify_closure(&candidate, &AcceptVerifiedClosure)
                .unwrap_or_else(|error| panic!("verify second: {error}"));
            let result = futures_executor::block_on(right.compare_and_select(candidate, receipt));
            let _ = send.send(
                result
                    .map(|selected| selected.generation())
                    .map_err(|error| error.to_string()),
            );
        });
        barrier.wait();
        left_thread.join().expect("left writer thread");
        right_thread.join().expect("right writer thread");
        let outcomes: Vec<_> = receive.try_iter().collect();
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert!(
            outcomes
                .iter()
                .filter_map(|result| result.as_ref().err())
                .any(|error| error.contains("selected package frontier changed"))
        );
        assert_eq!(
            outcomes
                .iter()
                .filter_map(|result| result.as_ref().ok())
                .copied()
                .collect::<Vec<_>>(),
            vec![1]
        );
        let reopened = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("reopen: {error}"));
        assert_eq!(
            reopened
                .selected_frontier(&namespace())
                .await
                .unwrap_or_else(|error| panic!("read winner: {error}"))
                .map(|selected| selected.generation()),
            Some(1)
        );
    });
}

#[test]
fn semantic_profiles_have_independent_authority_heads() {
    futures_executor::block_on(async {
        let path = path();
        let mut authority = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        let lower_ir = AuthorityNamespace::semantic_profile(
            "pkg:cargo/widget",
            "registry:crates-io",
            "main",
            "stable",
            "rust/2024/lower-ir",
        )
        .unwrap_or_else(|error| panic!("lower-ir namespace: {error}"));
        let typecheck = AuthorityNamespace::semantic_profile(
            "pkg:cargo/widget",
            "registry:crates-io",
            "main",
            "stable",
            "rust/2024/typecheck",
        )
        .unwrap_or_else(|error| panic!("typecheck namespace: {error}"));
        assert_ne!(lower_ir.namespace_id(), typecheck.namespace_id());

        let lower_observation = SourceObservation::new(
            lower_ir.clone(),
            Some([51; 32]),
            500,
            SourceObservationValue::KnownCount(3),
        )
        .unwrap_or_else(|error| panic!("lower-ir observation: {error}"));
        let lower_observation = authority
            .record_source_observation(lower_observation)
            .await
            .unwrap_or_else(|error| panic!("store lower-ir observation: {error}"));
        let typecheck_observation = SourceObservation::new(
            typecheck.clone(),
            Some([51; 32]),
            500,
            SourceObservationValue::KnownCount(3),
        )
        .unwrap_or_else(|error| panic!("typecheck observation: {error}"));
        let typecheck_observation = authority
            .record_source_observation(typecheck_observation)
            .await
            .unwrap_or_else(|error| panic!("store typecheck observation: {error}"));

        let lower_attempt = authority
            .begin_attempt(&lower_ir, [61; 32], &lower_observation)
            .await
            .unwrap_or_else(|error| panic!("begin lower-ir: {error}"));
        let typecheck_attempt = authority
            .begin_attempt(&typecheck, [62; 32], &typecheck_observation)
            .await
            .unwrap_or_else(|error| panic!("begin typecheck: {error}"));
        assert_eq!(lower_attempt.epoch(), 1);
        assert_eq!(typecheck_attempt.epoch(), 1);

        let lower_candidate = lower_attempt.candidate([63; 32], [64; 32], [65; 32], [64; 32]);
        let typecheck_candidate =
            typecheck_attempt.candidate([73; 32], [74; 32], [75; 32], [74; 32]);
        let lower_receipt = authority
            .verify_closure(&lower_candidate, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify lower-ir: {error}"));
        let typecheck_receipt = authority
            .verify_closure(&typecheck_candidate, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify typecheck: {error}"));
        authority
            .compare_and_select(lower_candidate, lower_receipt)
            .await
            .unwrap_or_else(|error| panic!("select lower-ir: {error}"));
        authority
            .compare_and_select(typecheck_candidate, typecheck_receipt)
            .await
            .unwrap_or_else(|error| panic!("select typecheck: {error}"));

        let lower_head = authority
            .selected_frontier(&lower_ir)
            .await
            .unwrap_or_else(|error| panic!("read lower-ir: {error}"))
            .unwrap_or_else(|| panic!("lower-ir head missing"));
        let typecheck_head = authority
            .selected_frontier(&typecheck)
            .await
            .unwrap_or_else(|error| panic!("read typecheck: {error}"))
            .unwrap_or_else(|| panic!("typecheck head missing"));
        assert_eq!(lower_head.generation(), 1);
        assert_eq!(lower_head.target_root(), &[64; 32]);
        assert_eq!(typecheck_head.generation(), 1);
        assert_eq!(typecheck_head.target_root(), &[74; 32]);
        let selected_lower_again = authority
            .select_existing_generation(
                &lower_ir,
                Some(&lower_head),
                1,
                ExistingGenerationSelection::RequireCurrentObservation,
                &AcceptVerifiedClosure,
            )
            .await
            .unwrap_or_else(|error| panic!("select current lower-ir history: {error}"));
        assert_eq!(selected_lower_again.generation(), 2);
        assert_eq!(
            selected_lower_again.selection_origin(),
            SelectionOrigin::RetainedCurrentObservation
        );
        assert_eq!(
            authority
                .selected_frontier(&typecheck)
                .await
                .unwrap_or_else(|error| panic!("reread typecheck: {error}"))
                .map(|frontier| frontier.generation()),
            Some(1)
        );
        assert_eq!(
            authority
                .selected_frontier(&namespace())
                .await
                .unwrap_or_else(|error| panic!("read package metadata: {error}")),
            None
        );
    });
}

#[test]
fn candidate_receipt_is_bound_to_exact_candidate_and_repack_preserves_root() {
    futures_executor::block_on(async {
        let path = path();
        let mut authority = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        let observed = authority
            .record_source_observation(observation(SourceObservationValue::KnownCount(3), 300))
            .await
            .unwrap_or_else(|error| panic!("record: {error}"));
        let first_attempt = authority
            .begin_attempt(&namespace(), [25; 32], &observed)
            .await
            .unwrap_or_else(|error| panic!("begin first: {error}"));
        let first = candidate(first_attempt, 35, 45, 55, 45);
        let first_receipt = authority
            .verify_closure(&first, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify first: {error}"));
        let mut wrong_fence = first.clone();
        wrong_fence.attempt.fence[0] ^= 0x80;
        assert!(matches!(
            authority
                .compare_and_select(wrong_fence, first_receipt)
                .await,
            Err(AuthorityError::ClosureMismatch)
        ));
        let pack_receipt = authority
            .verify_closure(&first, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify pack mismatch: {error}"));
        let wrong_pack = candidate(first.attempt.clone(), 35, 45, 56, 45);
        assert!(matches!(
            authority.compare_and_select(wrong_pack, pack_receipt).await,
            Err(AuthorityError::ClosureMismatch)
        ));
        let first_attempt = authority
            .begin_attempt(&namespace(), [25; 32], &observed)
            .await
            .unwrap_or_else(|error| panic!("begin retry: {error}"));
        let first = candidate(first_attempt, 35, 45, 55, 45);
        let receipt = authority
            .verify_closure(&first, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify retry: {error}"));
        let selected_first = authority
            .compare_and_select(first, receipt)
            .await
            .unwrap_or_else(|error| panic!("select first: {error}"));

        let repack_attempt = authority
            .begin_attempt(&namespace(), [25; 32], &observed)
            .await
            .unwrap_or_else(|error| panic!("begin repack: {error}"));
        let repacked = candidate(repack_attempt, 36, 45, 57, 45);
        let repacked_receipt = authority
            .verify_closure(&repacked, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify repack: {error}"));
        let selected_repacked = authority
            .compare_and_select(repacked, repacked_receipt)
            .await
            .unwrap_or_else(|error| panic!("select repack: {error}"));
        assert_eq!(
            selected_first.target_root(),
            selected_repacked.target_root()
        );
        assert_ne!(selected_first.pack_id(), selected_repacked.pack_id());
        assert_eq!(
            selected_repacked.generation(),
            selected_first.generation() + 1
        );
        let first_history = authority
            .selected_generation(&namespace(), selected_first.generation())
            .await
            .unwrap_or_else(|error| panic!("read first history: {error}"))
            .unwrap_or_else(|| panic!("missing first history"));
        let second_history = authority
            .selected_generation(&namespace(), selected_repacked.generation())
            .await
            .unwrap_or_else(|error| panic!("read second history: {error}"))
            .unwrap_or_else(|| panic!("missing second history"));
        assert_eq!(first_history.target_root(), second_history.target_root());
        assert_ne!(first_history.pack_id(), second_history.pack_id());
        assert_eq!(
            authority
                .selected_generation(&namespace(), selected_repacked.generation() + 1)
                .await
                .unwrap_or_else(|error| panic!("read absent history: {error}")),
            None
        );
    });
}

#[test]
fn selecting_retained_generation_requires_history_ack_and_uses_head_cas() {
    futures_executor::block_on(async {
        let path = path();
        let mut authority = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        let first_observation = authority
            .record_source_observation(observation(SourceObservationValue::KnownCount(3), 300))
            .await
            .unwrap_or_else(|error| panic!("record first observation: {error}"));
        let first_attempt = authority
            .begin_attempt(&namespace(), [25; 32], &first_observation)
            .await
            .unwrap_or_else(|error| panic!("begin first attempt: {error}"));
        let first = candidate(first_attempt, 35, 45, 55, 45);
        let first_receipt = authority
            .verify_closure(&first, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify first: {error}"));
        let selected_first = authority
            .compare_and_select(first, first_receipt)
            .await
            .unwrap_or_else(|error| panic!("select first: {error}"));

        let latest_observation = authority
            .record_source_observation(observation(SourceObservationValue::KnownCount(4), 400))
            .await
            .unwrap_or_else(|error| panic!("record changed observation: {error}"));
        let latest_attempt = authority
            .begin_attempt(&namespace(), [26; 32], &latest_observation)
            .await
            .unwrap_or_else(|error| panic!("begin latest attempt: {error}"));
        let latest = candidate(latest_attempt, 36, 46, 56, 46);
        let latest_receipt = authority
            .verify_closure(&latest, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify latest: {error}"));
        let selected_latest = authority
            .compare_and_select(latest, latest_receipt)
            .await
            .unwrap_or_else(|error| panic!("select latest: {error}"));
        assert_eq!(
            selected_latest.generation(),
            selected_first.generation() + 1
        );

        assert!(matches!(
            authority
                .select_existing_generation(
                    &namespace(),
                    Some(&selected_latest),
                    selected_first.generation(),
                    ExistingGenerationSelection::RequireCurrentObservation,
                    &AcceptVerifiedClosure,
                )
                .await,
            Err(AuthorityError::HistoricalSelectionRequiresAcknowledgement)
        ));

        let barrier = Arc::new(Barrier::new(3));
        let (send, receive) = mpsc::channel();
        let mut threads = Vec::new();
        for _ in 0..2 {
            let barrier = Arc::clone(&barrier);
            let send = send.clone();
            let path = path.clone();
            let expected = selected_latest.clone();
            let generation = selected_first.generation();
            threads.push(thread::spawn(move || {
                barrier.wait();
                let mut authority = futures_executor::block_on(TursoAuthority::open(&path))
                    .unwrap_or_else(|error| panic!("open racing authority: {error}"));
                let result = futures_executor::block_on(authority.select_existing_generation(
                    &namespace(),
                    Some(&expected),
                    generation,
                    ExistingGenerationSelection::AcknowledgeHistorical,
                    &AcceptVerifiedClosure,
                ));
                let _ = send.send(
                    result
                        .map(|selected| selected.generation())
                        .map_err(|error| error.to_string()),
                );
            }));
        }
        barrier.wait();
        for handle in threads {
            handle.join().expect("selection race thread");
        }
        let outcomes: Vec<_> = receive.try_iter().collect();
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter_map(|result| result.as_ref().ok())
                .copied()
                .collect::<Vec<_>>(),
            vec![selected_latest.generation() + 1]
        );
        assert!(
            outcomes
                .iter()
                .filter_map(|result| result.as_ref().err())
                .any(|error| error.contains("selected package frontier changed"))
        );

        let reopened = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("cold reopen: {error}"));
        let frontier = reopened
            .selected_frontier(&namespace())
            .await
            .unwrap_or_else(|error| panic!("read selected head: {error}"))
            .unwrap_or_else(|| panic!("selected head missing"));
        assert_eq!(frontier.generation(), selected_latest.generation() + 1);
        assert_eq!(frontier.target_root(), &[45; 32]);
        assert_eq!(frontier.input_digest(), &[25; 32]);
        assert_eq!(frontier.observation(), &first_observation);
        assert_eq!(
            frontier.selection_origin(),
            SelectionOrigin::RetainedHistoricalObservation
        );
        assert!(
            frontier
                .projections()
                .iter()
                .all(|watermark| !watermark.is_current())
        );
        assert_eq!(
            reopened
                .latest_input_digest(&namespace())
                .await
                .unwrap_or_else(|error| panic!("read latest input: {error}")),
            Some([26; 32])
        );
        let selected_frontiers = reopened
            .selected_frontiers(None, 8)
            .await
            .unwrap_or_else(|error| panic!("enumerate selected namespaces: {error}"));
        assert_eq!(selected_frontiers.len(), 1);
        assert_eq!(selected_frontiers[0], frontier);

        let first_page = reopened
            .selected_generations(&namespace(), None, 2)
            .await
            .unwrap_or_else(|error| panic!("read first history page: {error}"));
        let second_page = reopened
            .selected_generations(&namespace(), Some(2), 2)
            .await
            .unwrap_or_else(|error| panic!("read second history page: {error}"));
        assert_eq!(
            first_page
                .iter()
                .map(SelectedGeneration::generation)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(second_page.len(), 1);
        assert_eq!(second_page[0].generation(), 3);
        assert_eq!(
            second_page[0].selection_origin(),
            SelectionOrigin::RetainedHistoricalObservation
        );
    });
}

#[test]
fn superseded_attempt_proof_reopens_by_exact_namespace_epoch_and_fence() {
    futures_executor::block_on(async {
        let path = path();
        let mut authority = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        let observed = authority
            .record_source_observation(observation(SourceObservationValue::KnownCount(1), 100))
            .await
            .unwrap_or_else(|error| panic!("record observation: {error}"));
        let retired = authority
            .begin_attempt(&namespace(), [71; 32], &observed)
            .await
            .unwrap_or_else(|error| panic!("begin retired attempt: {error}"));
        let current = authority
            .begin_attempt(&namespace(), [72; 32], &observed)
            .await
            .unwrap_or_else(|error| panic!("begin current attempt: {error}"));

        assert_eq!(
            authority
                .superseded_attempt_proof_for_fence(
                    &namespace(),
                    current.epoch(),
                    current.fence_bytes(),
                )
                .await
                .unwrap_or_else(|error| panic!("check current attempt: {error}")),
            None,
        );
        assert_eq!(
            authority
                .superseded_attempt_proof_for_fence(&namespace(), retired.epoch(), [0xEE; 32],)
                .await
                .unwrap_or_else(|error| panic!("check unknown fence: {error}")),
            None,
        );
        assert_eq!(
            authority
                .superseded_attempt_proof(
                    &namespace(),
                    [0x99; 16],
                    retired.epoch(),
                    retired.fence_bytes(),
                )
                .await
                .unwrap_or_else(|error| panic!("check mismatched nonce: {error}")),
            None,
        );
        drop(authority);

        let reopened = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("cold reopen: {error}"));
        let proof = reopened
            .superseded_attempt_proof_for_fence(
                &namespace(),
                retired.epoch(),
                retired.fence_bytes(),
            )
            .await
            .unwrap_or_else(|error| panic!("read retired attempt proof after restart: {error}"))
            .unwrap_or_else(|| panic!("retired attempt proof missing after restart"));
        assert_eq!(proof.namespace(), &namespace());
        assert_eq!(proof.attempt_id(), retired.attempt_id());
        assert_eq!(proof.epoch(), retired.epoch());
        assert_eq!(proof.fence(), &retired.fence_bytes());
        assert_eq!(proof.input_digest(), retired.input_digest());
        assert_eq!(proof.current_attempt_id(), current.attempt_id());
        assert_eq!(proof.current_epoch(), current.epoch());
    });
}

#[test]
fn no_result_barrier_is_exact_idempotent_and_fences_candidate_selection() {
    futures_executor::block_on(async {
        let path = path();
        let namespace = namespace();
        let mut authority = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        let observed = authority
            .record_source_observation(observation(SourceObservationValue::KnownCount(1), 100))
            .await
            .unwrap_or_else(|error| panic!("observe: {error}"));
        let attempt = authority
            .begin_attempt(&namespace, [0x51; 32], &observed)
            .await
            .unwrap_or_else(|error| panic!("begin: {error}"));
        let work_id = [0x61; 16];
        let barrier = authority
            .mint_no_result_retirement_barrier(
                &namespace,
                work_id,
                attempt.epoch(),
                attempt.fence_bytes(),
            )
            .await
            .unwrap_or_else(|error| panic!("mint barrier: {error}"));
        assert_eq!(barrier.namespace(), &namespace);
        assert_eq!(barrier.terminal_work_id(), &work_id);
        assert_eq!(barrier.terminal_epoch(), attempt.epoch());
        assert_eq!(barrier.terminal_fence(), &attempt.fence_bytes());
        assert!(barrier.barrier_epoch() > attempt.epoch());
        assert_eq!(barrier.retired_through_epoch(), attempt.epoch());
        assert_eq!(
            authority
                .authority_namespace_for_id(namespace.namespace_id())
                .await
                .unwrap_or_else(|error| panic!("resolve typed namespace: {error}")),
            Some(namespace.clone())
        );

        let duplicate = authority
            .mint_no_result_retirement_barrier(
                &namespace,
                work_id,
                attempt.epoch(),
                attempt.fence_bytes(),
            )
            .await
            .unwrap_or_else(|error| panic!("repeat barrier: {error}"));
        assert_eq!(duplicate, barrier);
        assert!(matches!(
            authority
                .mint_no_result_retirement_barrier(
                    &namespace,
                    work_id,
                    attempt.epoch(),
                    [0xEE; 32],
                )
                .await,
            Err(AuthorityError::StaleAttempt)
        ));
        let wrong_namespace = AuthorityNamespace::package_metadata(
            "pkg:cargo/other",
            "registry:crates-io",
            "main",
            "stable",
        )
        .unwrap_or_else(|error| panic!("wrong namespace: {error}"));
        assert!(matches!(
            authority
                .mint_no_result_retirement_barrier(
                    &wrong_namespace,
                    work_id,
                    attempt.epoch(),
                    attempt.fence_bytes(),
                )
                .await,
            Err(AuthorityError::StaleAttempt)
        ));

        let candidate = candidate(attempt, 91, 92, 93, 92);
        let receipt = authority
            .verify_closure(&candidate, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify old candidate: {error}"));
        assert!(matches!(
            authority.compare_and_select(candidate, receipt).await,
            Err(AuthorityError::StaleAttempt)
        ));
        let after_barrier = authority
            .begin_attempt(&namespace, [0x52; 32], &observed)
            .await
            .unwrap_or_else(|error| panic!("begin after barrier: {error}"));
        assert!(after_barrier.epoch() > barrier.barrier_epoch());
    });
}

#[test]
fn no_result_barrier_and_begin_attempt_share_one_serialized_epoch_lane() {
    let path = path();
    let namespace = namespace();
    let (terminal, observed) = futures_executor::block_on(async {
        let mut authority = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        let observed = authority
            .record_source_observation(observation(SourceObservationValue::KnownCount(1), 100))
            .await
            .unwrap_or_else(|error| panic!("observe: {error}"));
        let terminal = authority
            .begin_attempt(&namespace, [0x52; 32], &observed)
            .await
            .unwrap_or_else(|error| panic!("begin terminal: {error}"));
        authority
            .retire_attempt(&terminal, CandidateAttemptRetirementReason::Cancelled)
            .await
            .unwrap_or_else(|error| panic!("terminalize no-result attempt: {error}"));
        (terminal, observed)
    });
    let terminal_epoch = terminal.epoch();
    let terminal_fence = terminal.fence_bytes();
    let gate = Arc::new(Barrier::new(3));
    let barrier_gate = Arc::clone(&gate);
    let barrier_path = path.clone();
    let barrier_namespace = namespace.clone();
    let barrier_thread = thread::spawn(move || {
        barrier_gate.wait();
        futures_executor::block_on(async {
            let mut authority = TursoAuthority::open(barrier_path)
                .await
                .unwrap_or_else(|error| panic!("barrier open: {error}"));
            authority
                .mint_no_result_retirement_barrier(
                    &barrier_namespace,
                    [0x71; 16],
                    terminal_epoch,
                    terminal_fence,
                )
                .await
                .unwrap_or_else(|error| panic!("race barrier: {error}"))
        })
    });
    let attempt_gate = Arc::clone(&gate);
    let attempt_path = path.clone();
    let attempt_namespace = namespace.clone();
    let attempt_thread = thread::spawn(move || {
        attempt_gate.wait();
        futures_executor::block_on(async {
            let mut authority = TursoAuthority::open(attempt_path)
                .await
                .unwrap_or_else(|error| panic!("attempt open: {error}"));
            authority
                .begin_attempt(&attempt_namespace, [0x53; 32], &observed)
                .await
                .unwrap_or_else(|error| panic!("race begin: {error}"))
        })
    });
    gate.wait();
    let barrier = barrier_thread
        .join()
        .unwrap_or_else(|_| panic!("barrier thread panicked"));
    let later_attempt = attempt_thread
        .join()
        .unwrap_or_else(|_| panic!("attempt thread panicked"));
    assert!(barrier.barrier_epoch() > terminal_epoch);
    assert_ne!(barrier.barrier_epoch(), later_attempt.epoch());
    assert_eq!(barrier.retired_through_epoch(), terminal_epoch);

    futures_executor::block_on(async {
        let mut reopened = TursoAuthority::open(&path)
            .await
            .unwrap_or_else(|error| panic!("reopen: {error}"));
        let retry = reopened
            .mint_no_result_retirement_barrier(
                &namespace,
                [0x71; 16],
                terminal_epoch,
                terminal_fence,
            )
            .await
            .unwrap_or_else(|error| panic!("retry barrier: {error}"));
        assert!(retry.barrier_epoch() > later_attempt.epoch());
        let duplicate = reopened
            .mint_no_result_retirement_barrier(
                &namespace,
                [0x71; 16],
                terminal_epoch,
                terminal_fence,
            )
            .await
            .unwrap_or_else(|error| panic!("repeat refreshed barrier: {error}"));
        assert_eq!(retry, duplicate);

        let mut attempt_rows = reopened
            .connection
            .query(
                "SELECT state FROM backend_index_authority_attempts \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6 AND attempt_id=?7",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    later_attempt.attempt_id().as_slice()
                ],
            )
            .await
            .unwrap_or_else(|error| panic!("query newer attempt after barrier retry: {error}"));
        let row = attempt_rows
            .next()
            .await
            .unwrap_or_else(|error| panic!("read newer attempt after barrier retry: {error}"))
            .unwrap_or_else(|| panic!("barrier must retain the newer candidate attempt"));
        let state: i64 = row
            .get(0)
            .unwrap_or_else(|error| panic!("read newer attempt state: {error}"));
        assert_eq!(state, 0, "barrier only retires the terminal epoch");
        drop(attempt_rows);
    });
}

#[test]
fn no_result_barrier_retry_handles_both_epoch_orders_after_cold_reopen() {
    for barrier_first in [true, false] {
        let path = path();
        let namespace = namespace();
        let (terminal, observed) = futures_executor::block_on(async {
            let mut authority = TursoAuthority::open(&path)
                .await
                .unwrap_or_else(|error| panic!("open: {error}"));
            let observed = authority
                .record_source_observation(observation(SourceObservationValue::KnownCount(1), 100))
                .await
                .unwrap_or_else(|error| panic!("observe: {error}"));
            let terminal = authority
                .begin_attempt(&namespace, [0x52; 32], &observed)
                .await
                .unwrap_or_else(|error| panic!("begin terminal: {error}"));
            authority
                .retire_attempt(&terminal, CandidateAttemptRetirementReason::Cancelled)
                .await
                .unwrap_or_else(|error| panic!("retire terminal: {error}"));
            (terminal, observed)
        });
        let terminal_epoch = terminal.epoch();
        let terminal_fence = terminal.fence_bytes();
        let terminal_work_id = [0x71; 16];

        let (first_barrier, later_attempt) = futures_executor::block_on(async {
            let mut authority = TursoAuthority::open(&path)
                .await
                .unwrap_or_else(|error| panic!("reopen before ordered interleaving: {error}"));
            if barrier_first {
                let barrier = authority
                    .mint_no_result_retirement_barrier(
                        &namespace,
                        terminal_work_id,
                        terminal_epoch,
                        terminal_fence,
                    )
                    .await
                    .unwrap_or_else(|error| panic!("mint barrier before new attempt: {error}"));
                let later = authority
                    .begin_attempt(&namespace, [0x53; 32], &observed)
                    .await
                    .unwrap_or_else(|error| panic!("begin newer attempt after barrier: {error}"));
                (barrier, later)
            } else {
                let later = authority
                    .begin_attempt(&namespace, [0x53; 32], &observed)
                    .await
                    .unwrap_or_else(|error| panic!("begin newer attempt before barrier: {error}"));
                let barrier = authority
                    .mint_no_result_retirement_barrier(
                        &namespace,
                        terminal_work_id,
                        terminal_epoch,
                        terminal_fence,
                    )
                    .await
                    .unwrap_or_else(|error| panic!("mint barrier after newer attempt: {error}"));
                (barrier, later)
            }
        });
        assert!(first_barrier.barrier_epoch() > terminal_epoch);
        assert_eq!(first_barrier.retired_through_epoch(), terminal_epoch);
        if barrier_first {
            assert!(first_barrier.barrier_epoch() < later_attempt.epoch());
        } else {
            assert!(first_barrier.barrier_epoch() > later_attempt.epoch());
        }

        futures_executor::block_on(async {
            let mut authority = TursoAuthority::open(&path)
                .await
                .unwrap_or_else(|error| panic!("cold reopen with outstanding barrier: {error}"));
            let retry = authority
                .mint_no_result_retirement_barrier(
                    &namespace,
                    terminal_work_id,
                    terminal_epoch,
                    terminal_fence,
                )
                .await
                .unwrap_or_else(|error| panic!("retry terminal barrier: {error}"));
            assert!(retry.barrier_epoch() > later_attempt.epoch());
            assert_eq!(retry.retired_through_epoch(), terminal_epoch);
            let duplicate = authority
                .mint_no_result_retirement_barrier(
                    &namespace,
                    terminal_work_id,
                    terminal_epoch,
                    terminal_fence,
                )
                .await
                .unwrap_or_else(|error| panic!("repeat terminal barrier: {error}"));
            assert_eq!(duplicate, retry, "unchanged scope retries are exact");

            let mut active_rows = authority
                .connection
                .query(
                    "SELECT state FROM backend_index_authority_attempts \
                     WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                       AND plane_kind=?5 AND profile=?6 AND attempt_id=?7",
                    turso::params![
                        namespace.package.as_ref(),
                        namespace.source.as_ref(),
                        namespace.branch.as_ref(),
                        namespace.environment.as_ref(),
                        namespace.plane.sql_parts().0,
                        namespace.plane.sql_parts().1,
                        later_attempt.attempt_id().as_slice()
                    ],
                )
                .await
                .unwrap_or_else(|error| panic!("query newer active attempt: {error}"));
            let row = active_rows
                .next()
                .await
                .unwrap_or_else(|error| panic!("read newer active attempt: {error}"))
                .unwrap_or_else(|| panic!("barrier retry must preserve newer work"));
            let state: i64 = row
                .get(0)
                .unwrap_or_else(|error| panic!("read newer active attempt state: {error}"));
            assert_eq!(state, 0);
            drop(active_rows);

            let newest = authority
                .begin_attempt(&namespace, [0x54; 32], &observed)
                .await
                .unwrap_or_else(|error| panic!("begin after refreshed barrier: {error}"));
            assert!(newest.epoch() > retry.barrier_epoch());
            assert_eq!(
                authority
                    .selected_frontier(&namespace)
                    .await
                    .unwrap_or_else(|error| panic!("read selected head after barriers: {error}")),
                None
            );
        });
    }
}
