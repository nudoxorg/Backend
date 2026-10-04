//! What the authority durably says happened to one exact attempt.
//!
//! The owner of a publication can die between selecting a generation, marking
//! its projections current and acknowledging its client. After a restart the
//! only thing it holds is the attempt's recovery claim, and the answer to
//! "what happened to that attempt" must come from the authority, never from the
//! presence of a package or from a failed re-submission.

#![allow(clippy::expect_used, clippy::panic)]

use super::tests::{AcceptVerifiedClosure, candidate, namespace, observation, path};
use super::*;
use std::path::PathBuf;

/// Removes the database and its sidecars when a test ends.
struct Database(PathBuf);

impl Database {
    fn new() -> Self {
        Self(path())
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", "-tshm"] {
            let mut sidecar = self.0.as_os_str().to_owned();
            sidecar.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(sidecar));
        }
    }
}

async fn open(database: &Database) -> TursoAuthority {
    TursoAuthority::open(&database.0)
        .await
        .unwrap_or_else(|error| panic!("open: {error}"))
}

async fn observe(authority: &mut TursoAuthority, count: u64) -> SourceObservationReceipt {
    authority
        .record_source_observation(observation(SourceObservationValue::KnownCount(count), 100))
        .await
        .unwrap_or_else(|error| panic!("observe: {error}"))
}

async fn begin(
    authority: &mut TursoAuthority,
    observed: &SourceObservationReceipt,
    input: u8,
) -> CandidateAttempt {
    authority
        .begin_attempt(&namespace(), [input; 32], observed)
        .await
        .unwrap_or_else(|error| panic!("begin: {error}"))
}

async fn select(
    authority: &mut TursoAuthority,
    attempt: CandidateAttempt,
    tag: u8,
) -> SelectedFrontier {
    let candidate = candidate(
        attempt,
        tag,
        tag.wrapping_add(1),
        tag.wrapping_add(2),
        tag.wrapping_add(1),
    );
    let receipt = authority
        .verify_closure(&candidate, &AcceptVerifiedClosure)
        .unwrap_or_else(|error| panic!("verify: {error}"));
    authority
        .compare_and_select(candidate, receipt)
        .await
        .unwrap_or_else(|error| panic!("select: {error}"))
}

async fn disposition(
    authority: &TursoAuthority,
    claim: &CandidateAttemptRecoveryClaim,
) -> AttemptDisposition {
    authority
        .attempt_disposition(claim)
        .await
        .unwrap_or_else(|error| panic!("disposition: {error}"))
}

#[test]
fn a_pending_attempt_is_pending_until_something_decides_it() {
    futures_executor::block_on(async {
        let database = Database::new();
        let mut authority = open(&database).await;
        let observed = observe(&mut authority, 3).await;
        let attempt = begin(&mut authority, &observed, 0x21).await;
        let claim = attempt.recovery_claim();

        assert_eq!(
            disposition(&authority, &claim).await,
            AttemptDisposition::Pending
        );
        // The restart path reopens it, and the disposition agrees after a cold
        // reopen of the file.
        drop(authority);
        let authority = open(&database).await;
        assert_eq!(
            disposition(&authority, &claim).await,
            AttemptDisposition::Pending
        );
        assert_eq!(
            authority
                .recover_candidate_attempt(&claim)
                .await
                .unwrap_or_else(|error| panic!("recover: {error}")),
            attempt
        );
    });
}

#[test]
fn a_selected_attempt_is_published_with_the_exact_generation_it_produced() {
    futures_executor::block_on(async {
        let database = Database::new();
        let mut authority = open(&database).await;
        let observed = observe(&mut authority, 3).await;
        let attempt = begin(&mut authority, &observed, 0x22).await;
        let claim = attempt.recovery_claim();
        let frontier = select(&mut authority, attempt.clone(), 0x30).await;

        let AttemptDisposition::Published(published) = disposition(&authority, &claim).await else {
            panic!("a selected attempt must read back as published");
        };
        assert_eq!(published.generation(), frontier.generation());
        assert_eq!(published.candidate_id(), frontier.candidate_id());
        assert_eq!(published.target_root(), frontier.target_root());
        assert_eq!(published.attempt(), (&attempt.attempt_id, attempt.epoch));
        assert_eq!(published.input_digest(), &attempt.input_digest);
        assert_eq!(
            Some(published.clone()),
            authority
                .selected_generation(&namespace(), frontier.generation())
                .await
                .unwrap_or_else(|error| panic!("history: {error}")),
            "the receipt is the immutable history entry itself"
        );

        // The answer survives the owner dying and the file being reopened.
        drop(authority);
        let authority = open(&database).await;
        assert_eq!(
            disposition(&authority, &claim).await,
            AttemptDisposition::Published(published)
        );
    });
}

#[test]
fn selected_attempt_with_missing_history_is_corrupt_even_when_the_head_still_exists() {
    futures_executor::block_on(async {
        let database = Database::new();
        let mut authority = open(&database).await;
        let observed = observe(&mut authority, 3).await;
        let attempt = begin(&mut authority, &observed, 0x71).await;
        let claim = attempt.recovery_claim();
        let frontier = select(&mut authority, attempt, 0x72).await;

        // Simulate persisted corruption rather than relaxing the immutable
        // production schema. A mutable head cannot replace missing history.
        authority
            .connection
            .execute_batch(
                "DROP TRIGGER backend_index_authority_generation_history_immutable_delete; \
                 DELETE FROM backend_index_authority_generation_history;",
            )
            .await
            .expect("remove fixture history");
        assert_eq!(
            authority
                .selected_frontier(&namespace())
                .await
                .expect("retained head")
                .map(|head| head.generation()),
            Some(frontier.generation())
        );
        assert!(matches!(
            authority.attempt_disposition(&claim).await,
            Err(AuthorityError::CorruptRecord("selected_attempt_history"))
        ));
    });
}

#[test]
fn selected_attempt_with_multiple_history_entries_is_corrupt_not_the_first_match() {
    futures_executor::block_on(async {
        let database = Database::new();
        let mut authority = open(&database).await;
        let observed = observe(&mut authority, 3).await;
        let attempt = begin(&mut authority, &observed, 0x73).await;
        let claim = attempt.recovery_claim();
        let frontier = select(&mut authority, attempt, 0x74).await;

        // Generation is part of the primary key, so a corrupt importer could
        // repeat one attempt under another generation without a key conflict.
        authority
            .connection
            .execute_batch(
                "INSERT INTO backend_index_authority_generation_history (\
                    package, source, branch, environment, plane_kind, profile, \
                    generation, candidate_id, attempt_id, attempt_epoch, attempt_fence, \
                    input_digest, target_root, pack_id, closure_id, semantic_manifest_root, \
                    observation_sequence, selection_origin\
                 ) SELECT package, source, branch, environment, plane_kind, profile, \
                    generation + 1, candidate_id, attempt_id, attempt_epoch, attempt_fence, \
                    input_digest, target_root, pack_id, closure_id, semantic_manifest_root, \
                    observation_sequence, selection_origin \
                 FROM backend_index_authority_generation_history;",
            )
            .await
            .expect("duplicate fixture history");
        assert_eq!(
            authority
                .selected_frontier(&namespace())
                .await
                .expect("retained head")
                .map(|head| head.generation()),
            Some(frontier.generation())
        );
        assert!(matches!(
            authority.attempt_disposition(&claim).await,
            Err(AuthorityError::CorruptRecord("selected_attempt_history"))
        ));
    });
}

#[test]
fn replaying_a_published_attempt_never_selects_twice_or_revives_it() {
    futures_executor::block_on(async {
        let database = Database::new();
        let mut authority = open(&database).await;
        let observed = observe(&mut authority, 3).await;
        let attempt = begin(&mut authority, &observed, 0x23).await;
        let claim = attempt.recovery_claim();
        let frontier = select(&mut authority, attempt.clone(), 0x31).await;

        // A restarted owner that does not know whether it published tries to
        // finish the same candidate again: it is refused, not applied twice.
        let again = candidate(attempt.clone(), 0x31, 0x32, 0x33, 0x32);
        let receipt = authority
            .verify_closure(&again, &AcceptVerifiedClosure)
            .unwrap_or_else(|error| panic!("verify: {error}"));
        assert!(matches!(
            authority.compare_and_select(again, receipt).await,
            Err(AuthorityError::StaleAttempt | AuthorityError::StaleFrontier)
        ));
        // Recording a failure after the fact is an idempotent no-op that cannot
        // turn a published attempt into a failed one.
        authority
            .retire_attempt(&attempt, CandidateAttemptRetirementReason::Failed)
            .await
            .unwrap_or_else(|error| panic!("late retirement: {error}"));
        assert!(matches!(
            authority.recover_candidate_attempt(&claim).await,
            Err(AuthorityError::StaleAttempt | AuthorityError::StaleFrontier)
        ));
        assert!(matches!(
            disposition(&authority, &claim).await,
            AttemptDisposition::Published(_)
        ));
        assert_eq!(
            authority
                .selected_frontier(&namespace())
                .await
                .unwrap_or_else(|error| panic!("frontier: {error}"))
                .map(|frontier| frontier.generation()),
            Some(frontier.generation()),
            "exactly one generation was ever selected"
        );
        assert!(
            authority
                .selected_generation(&namespace(), frontier.generation() + 1)
                .await
                .unwrap_or_else(|error| panic!("history: {error}"))
                .is_none()
        );
    });
}

#[test]
fn a_newer_attempt_fences_the_older_one_and_retiring_it_records_supersession() {
    futures_executor::block_on(async {
        let database = Database::new();
        let mut authority = open(&database).await;
        let observed = observe(&mut authority, 3).await;
        let older = begin(&mut authority, &observed, 0x24).await;
        let newer = begin(&mut authority, &observed, 0x25).await;

        assert_eq!(
            disposition(&authority, &older.recovery_claim()).await,
            AttemptDisposition::Fenced
        );
        assert_eq!(
            disposition(&authority, &newer.recovery_claim()).await,
            AttemptDisposition::Pending
        );
        // Whatever reason the owner offers, a fenced attempt records the
        // durable cause, and the answer becomes a recorded terminal.
        authority
            .retire_attempt(&older, CandidateAttemptRetirementReason::Cancelled)
            .await
            .unwrap_or_else(|error| panic!("retire: {error}"));
        assert_eq!(
            disposition(&authority, &older.recovery_claim()).await,
            AttemptDisposition::Retired(CandidateAttemptRetirementReason::Superseded)
        );
    });
}

#[test]
fn a_newer_source_observation_fences_a_pending_attempt() {
    futures_executor::block_on(async {
        let database = Database::new();
        let mut authority = open(&database).await;
        let observed = observe(&mut authority, 3).await;
        let attempt = begin(&mut authority, &observed, 0x26).await;
        assert_eq!(
            disposition(&authority, &attempt.recovery_claim()).await,
            AttemptDisposition::Pending
        );
        let _newer = observe(&mut authority, 4).await;
        assert_eq!(
            disposition(&authority, &attempt.recovery_claim()).await,
            AttemptDisposition::Fenced
        );
    });
}

#[test]
fn every_retirement_reason_reads_back_exactly_and_the_first_one_wins() {
    futures_executor::block_on(async {
        let database = Database::new();
        let mut authority = open(&database).await;
        let observed = observe(&mut authority, 3).await;
        for (input, reason) in [
            (0x41, CandidateAttemptRetirementReason::Cancelled),
            (0x42, CandidateAttemptRetirementReason::Refused),
            (0x43, CandidateAttemptRetirementReason::Failed),
        ] {
            let attempt = begin(&mut authority, &observed, input).await;
            authority
                .retire_attempt(&attempt, reason)
                .await
                .unwrap_or_else(|error| panic!("retire: {error}"));
            // A repeated, different retirement cannot rewrite the recorded cause.
            authority
                .retire_attempt(&attempt, CandidateAttemptRetirementReason::Failed)
                .await
                .unwrap_or_else(|error| panic!("repeat retire: {error}"));
            assert_eq!(
                disposition(&authority, &attempt.recovery_claim()).await,
                AttemptDisposition::Retired(reason)
            );
        }
    });
}

#[test]
fn an_unknown_attempt_is_unrecorded_never_success_and_never_failure() {
    futures_executor::block_on(async {
        let database = Database::new();
        let mut authority = open(&database).await;
        let observed = observe(&mut authority, 3).await;
        let attempt = begin(&mut authority, &observed, 0x27).await;
        let mut stranger = attempt.recovery_claim();
        stranger.attempt_id[0] ^= 0xFF;
        assert_eq!(
            disposition(&authority, &stranger).await,
            AttemptDisposition::Unrecorded
        );
        // The same attempt identity under another namespace is another attempt.
        let other = AuthorityNamespace::package_metadata(
            "pkg:cargo/other",
            "registry:crates-io",
            "main",
            "stable",
        )
        .unwrap_or_else(|error| panic!("namespace: {error}"));
        let mut foreign = attempt.recovery_claim();
        foreign.namespace = other;
        assert_eq!(
            disposition(&authority, &foreign).await,
            AttemptDisposition::Unrecorded
        );
    });
}

#[test]
fn a_forged_claim_for_a_known_attempt_is_refused_rather_than_answered() {
    futures_executor::block_on(async {
        let database = Database::new();
        let mut authority = open(&database).await;
        let observed = observe(&mut authority, 3).await;
        let published = begin(&mut authority, &observed, 0x28).await;
        let published_claim = published.recovery_claim();
        select(&mut authority, published, 0x34).await;
        let observed = observe(&mut authority, 4).await;
        let retired = begin(&mut authority, &observed, 0x29).await;
        let retired_claim = retired.recovery_claim();
        authority
            .retire_attempt(&retired, CandidateAttemptRetirementReason::Refused)
            .await
            .unwrap_or_else(|error| panic!("retire: {error}"));
        let pending = begin(&mut authority, &observed, 0x2A).await;
        let pending_claim = pending.recovery_claim();

        // Every claim names a real attempt of each disposition; each is
        // altered in exactly one fence field and must be refused, so a forged
        // claim cannot read another attempt's outcome.
        for claim in [&published_claim, &retired_claim, &pending_claim] {
            let mutations: [(&str, Box<dyn Fn(&mut CandidateAttemptRecoveryClaim)>); 6] = [
                ("epoch", Box::new(|claim| claim.epoch += 1)),
                ("fence", Box::new(|claim| claim.fence[0] ^= 1)),
                ("input", Box::new(|claim| claim.input_digest[0] ^= 1)),
                (
                    "base generation",
                    Box::new(|claim| {
                        claim.base_generation += 1;
                        claim.base_root = Some([9; 32]);
                    }),
                ),
                (
                    "base root",
                    Box::new(|claim| claim.base_root = Some([9; 32])),
                ),
                (
                    "observation",
                    Box::new(|claim| claim.observation_sequence += 1),
                ),
            ];
            for (field, mutate) in mutations {
                let mut forged = claim.clone();
                mutate(&mut forged);
                if forged == *claim {
                    continue;
                }
                assert!(
                    matches!(
                        authority.attempt_disposition(&forged).await,
                        Err(AuthorityError::StaleAttempt)
                    ),
                    "a claim with a forged {field} must be refused"
                );
            }
        }
        // Structurally impossible claims are refused before touching the file.
        let mut zero = pending_claim.clone();
        zero.fence = [0; 32];
        assert!(matches!(
            authority.attempt_disposition(&zero).await,
            Err(AuthorityError::StaleAttempt)
        ));
    });
}
