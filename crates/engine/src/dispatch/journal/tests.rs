use super::*;
use crate::fault::{Boundary, Faults};
use crate::journal::{JournalCodec, JournalError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_PATH: AtomicU64 = AtomicU64::new(1);

fn journal_path() -> PathBuf {
    let id = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "backend-dispatch-journal-{}-{id}.log",
        std::process::id()
    ))
}

fn intent(path: &PathBuf) -> (DispatchAttemptKey, RemoteAttemptIntent) {
    let key = DispatchAttemptKey::new([1; 32], 1).unwrap_or_else(|error| panic!("{error}"));
    let value = RemoteAttemptIntent::new(
        key,
        Box::<[u8]>::from([9, 8, 7, 6]),
        [2; 32],
        [3; 32],
        7,
        100,
        80,
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let value = value.with_revocation_version(11);
    let _ = std::fs::remove_file(path);
    (key, value)
}

fn owner_authority(epoch: u64, revocation: u64, cursor: u64) -> OwnerRestartAuthority {
    OwnerRestartAuthority::mint([2; 32], epoch, [3; 32], revocation, cursor)
}

fn store_receipt() -> StorePublicationReceipt {
    StorePublicationReceipt::from_parts([7; 32], 11, [8; 32], [8; 32])
        .unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn every_record_has_a_round_trip_and_strict_version_tag() {
    let key = DispatchAttemptKey::new([1; 32], 1).unwrap_or_else(|error| panic!("{error}"));
    let admitted =
        RemoteAttemptIntent::new(key, Box::<[u8]>::from([9, 8]), [2; 32], [3; 32], 7, 100, 80)
            .unwrap_or_else(|error| panic!("{error}"))
            .with_revocation_version(11);
    let records = [
        DispatchRecord::Admitted { intent: admitted },
        DispatchRecord::Transfer {
            key,
            progress: TransferCheckpointRef::new([4; 32], [5; 32], 4, 1, DispatchPhase::Executing)
                .unwrap_or_else(|error| panic!("{error}")),
        },
        DispatchRecord::Accepted {
            key,
            proof: AcceptedResultProof::new([6; 32], Box::<[u8]>::from([1]), 2)
                .unwrap_or_else(|error| panic!("{error}")),
        },
        DispatchRecord::PublicationPending { key },
        DispatchRecord::Published {
            key,
            ack: PublicationAck {
                output_root: [6; 32],
                store: store_receipt(),
                owner_epoch: 7,
                notification_cursor: 3,
            },
        },
        DispatchRecord::PublishedWithCursor {
            key,
            ack: PublicationAck {
                output_root: [6; 32],
                store: store_receipt(),
                owner_epoch: 7,
                notification_cursor: 3,
            },
            cursor: NotificationCursor::new(1, 2, Box::<[u8]>::from([3, 4]))
                .unwrap_or_else(|error| panic!("{error}")),
        },
        DispatchRecord::Cursor {
            cursor: DispatchCursor {
                key,
                position: NotificationCursor::new(1, 2, Box::<[u8]>::from([3, 4]))
                    .unwrap_or_else(|error| panic!("{error}")),
            },
        },
        DispatchRecord::Fenced {
            key,
            owner_epoch: 8,
            fence: [9; 32],
            reason: 2,
        },
        DispatchRecord::Terminal {
            key,
            terminal: TerminalState::Fallback {
                output_root: [10; 32],
                reason: 2,
                notification_cursor: 4,
            },
        },
    ];
    for expected in records {
        let mut bytes = Vec::new();
        DispatchLog::encode(&expected, &mut bytes);
        let decoded =
            DispatchLog::decode(&bytes).unwrap_or_else(|error| panic!("decode failed: {error}"));
        assert_eq!(decoded, expected);
        bytes[0] = DISPATCH_RECORD_VERSION.saturating_add(1);
        assert!(matches!(
            DispatchLog::decode(&bytes),
            Err(JournalError::Record(_))
        ));
    }
}

#[test]
fn full_lifecycle_reopens_from_one_streaming_fold() {
    let path = journal_path();
    let (key, admitted) = intent(&path);
    let limits = DispatchJournalLimits::default();
    let (journal, opened) =
        DispatchJournal::open(&path, limits).unwrap_or_else(|error| panic!("{error}"));
    assert!(opened.is_empty());
    journal
        .admit(admitted.clone())
        .unwrap_or_else(|error| panic!("{error}"));
    let progress = TransferCheckpointRef::new([4; 32], [5; 32], 64, 2, DispatchPhase::Transferring)
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .record_transfer(key, progress)
        .unwrap_or_else(|error| panic!("{error}"));
    let proof = AcceptedResultProof::new([6; 32], Box::<[u8]>::from([1, 2, 3]), 42)
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .accept_result(key, proof.clone())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .begin_publication(key)
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .update_cursor(
            key,
            NotificationCursor::new(3, 4, Box::<[u8]>::from([7, 7]))
                .unwrap_or_else(|error| panic!("{error}")),
        )
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .acknowledge_publication(
            key,
            PublicationAck {
                output_root: [6; 32],
                store: store_receipt(),
                owner_epoch: 7,
                notification_cursor: 5,
            },
        )
        .unwrap_or_else(|error| panic!("{error}"));
    drop(journal);

    let (reopened, recovery) =
        DispatchJournal::open(&path, limits).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(recovery.frames_scanned, 6);
    let attempt = recovery
        .get(key)
        .unwrap_or_else(|| panic!("attempt missing after reopen"));
    assert_eq!(attempt.intent, admitted);
    assert_eq!(attempt.phase, DispatchPhase::Published);
    assert_eq!(attempt.accepted.as_ref(), Some(&proof));
    assert_eq!(attempt.current_fence, [3; 32]);
    assert_eq!(attempt.cursor.as_ref().map(|cursor| cursor.waiter), Some(3));
    assert_eq!(
        reopened
            .snapshot()
            .unwrap_or_else(|error| panic!("{error}"))
            .get(key)
            .map(|attempt| attempt.phase),
        Some(DispatchPhase::Published)
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn accepted_proof_survives_crash_after_synced_append() {
    let path = journal_path();
    let (key, admitted) = intent(&path);
    let faults = std::sync::Arc::new(Faults::default());
    let (journal, _) = DispatchJournal::open_with_faults(
        &path,
        DispatchJournalLimits::default(),
        std::sync::Arc::clone(&faults),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    journal
        .admit(admitted)
        .unwrap_or_else(|error| panic!("{error}"));
    let proof = AcceptedResultProof::new([6; 32], Box::<[u8]>::from([1, 2, 3]), 42)
        .unwrap_or_else(|error| panic!("{error}"));
    faults.arm(Boundary::JournalFlush);
    let error = journal
        .accept_result(key, proof.clone())
        .expect_err("fault must abort after the durable append");
    assert!(matches!(
        error,
        DispatchJournalError::Fault(crate::fault::InjectedCrash {
            boundary: Boundary::JournalFlush
        })
    ));
    drop(journal);

    let (reopened, recovery) = DispatchJournal::open(&path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        recovery
            .get(key)
            .and_then(|attempt| attempt.accepted.as_ref()),
        Some(&proof)
    );
    let actions = reopened
        .restart(10, &owner_authority(7, 11, 0))
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(matches!(
        actions.as_slice(),
        [DispatchRecoveryAction::PublishAccepted { proof: found, mode: PublicationRecoveryMode::Start, .. }]
            if found == &proof
    ));
    let _ = std::fs::remove_file(path);
}

#[test]
fn publication_pending_restarts_as_a_proof_publication() {
    let log_path = journal_path();
    let (key, admitted) = intent(&log_path);
    let (journal, _) = DispatchJournal::open(&log_path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .admit(admitted)
        .unwrap_or_else(|error| panic!("{error}"));
    let proof = AcceptedResultProof::new([6; 32], Box::<[u8]>::from([1, 2]), 4)
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .accept_result(key, proof.clone())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .begin_publication(key)
        .unwrap_or_else(|error| panic!("{error}"));
    let actions = journal
        .restart(101, &owner_authority(7, 11, 0))
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(matches!(
        actions.as_slice(),
        [DispatchRecoveryAction::PublishAccepted { proof: found, mode: PublicationRecoveryMode::Reconcile, .. }]
            if found == &proof
    ));
    let _ = std::fs::remove_file(log_path);
}

#[test]
fn every_torn_publication_ack_suffix_recovers_as_reconciliation() {
    let path = journal_path();
    let (key, admitted) = intent(&path);
    let (journal, _) = DispatchJournal::open(&path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .admit(admitted)
        .unwrap_or_else(|error| panic!("{error}"));
    let proof = AcceptedResultProof::new([6; 32], Box::<[u8]>::from([1, 2]), 4)
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .accept_result(key, proof.clone())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .begin_publication(key)
        .unwrap_or_else(|error| panic!("{error}"));
    let durable_prefix = usize::try_from(
        std::fs::metadata(&path)
            .unwrap_or_else(|error| panic!("{error}"))
            .len(),
    )
    .unwrap_or_else(|error| panic!("journal length does not fit usize: {error}"));
    journal
        .acknowledge_publication(
            key,
            PublicationAck {
                output_root: [6; 32],
                store: store_receipt(),
                owner_epoch: 7,
                notification_cursor: 0,
            },
        )
        .unwrap_or_else(|error| panic!("{error}"));
    drop(journal);
    let complete = std::fs::read(&path).unwrap_or_else(|error| panic!("{error}"));

    for cut in durable_prefix + 1..complete.len() {
        std::fs::write(&path, &complete[..cut]).unwrap_or_else(|error| panic!("{error}"));
        let (reopened, recovery) = DispatchJournal::open(&path, DispatchJournalLimits::default())
            .unwrap_or_else(|error| panic!("cut {cut}: {error}"));
        assert!(recovery.truncated_tail, "cut {cut}");
        let actions = reopened
            .restart(10, &owner_authority(7, 11, 0))
            .unwrap_or_else(|error| panic!("cut {cut}: {error}"));
        assert!(matches!(
            actions.as_slice(),
            [DispatchRecoveryAction::PublishAccepted {
                mode: PublicationRecoveryMode::Reconcile,
                proof: found,
                ..
            }] if found == &proof
        ));
        drop(reopened);
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn store_publication_receipt_rejects_cross_root_composition() {
    assert!(matches!(
        StorePublicationReceipt::from_parts([7; 32], 11, [8; 32], [9; 32]),
        Err(DispatchRecordError::InvalidIdentifier)
    ));
}

#[test]
fn revocation_epoch_fences_even_when_owner_epoch_matches() {
    let path = journal_path();
    let (key, admitted) = intent(&path);
    let (journal, _) = DispatchJournal::open(&path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .admit(admitted)
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .update_cursor(
            key,
            NotificationCursor::new(17, 23, Box::<[u8]>::from([4]))
                .unwrap_or_else(|error| panic!("{error}")),
        )
        .unwrap_or_else(|error| panic!("{error}"));
    let actions = journal
        .restart(10, &owner_authority(7, 12, 31))
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(matches!(
        actions.as_slice(),
        [DispatchRecoveryAction::Fallback {
            reason: RESTART_AUTHORITY_REVOKED,
            ..
        }]
    ));
    assert_eq!(
        journal
            .snapshot()
            .unwrap_or_else(|error| panic!("{error}"))
            .get(key)
            .map(|attempt| attempt.phase),
        Some(DispatchPhase::Fallback)
    );
    assert_eq!(
        journal
            .snapshot()
            .unwrap_or_else(|error| panic!("{error}"))
            .get(key)
            .and_then(|attempt| attempt.terminal)
            .map(|terminal| match terminal {
                TerminalState::Cancelled {
                    notification_cursor,
                    ..
                }
                | TerminalState::Fallback {
                    notification_cursor,
                    ..
                } => notification_cursor,
            }),
        Some(31)
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn takeover_fences_at_the_current_epoch_and_preserves_cursor() {
    let path = journal_path();
    let (key, admitted) = intent(&path);
    let (journal, _) = DispatchJournal::open(&path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .admit(admitted)
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .update_cursor(
            key,
            NotificationCursor::new(41, 5, Box::<[u8]>::from([7, 8]))
                .unwrap_or_else(|error| panic!("{error}")),
        )
        .unwrap_or_else(|error| panic!("{error}"));
    let actions = journal
        .restart(10, &owner_authority(42, 11, 43))
        .unwrap_or_else(|error| panic!("{error}"));
    let DispatchRecoveryAction::Fallback {
        attempt, reason, ..
    } = &actions[0]
    else {
        panic!("takeover must fence and select fallback");
    };
    assert_eq!(*reason, RESTART_OWNER_TAKEOVER);
    assert_eq!(attempt.fenced, Some((42, RESTART_OWNER_TAKEOVER)));
    assert_eq!(
        attempt.terminal,
        Some(TerminalState::Fallback {
            output_root: [0; 32],
            reason: RESTART_OWNER_TAKEOVER,
            notification_cursor: 43,
        })
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn exact_authority_and_live_lease_rebind_without_mutating_the_journal() {
    let path = journal_path();
    let (key, admitted) = intent(&path);
    let (journal, _) = DispatchJournal::open(&path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .admit(admitted)
        .unwrap_or_else(|error| panic!("{error}"));
    let actions = journal
        .restart(10, &owner_authority(7, 11, 0))
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(matches!(
        actions.as_slice(),
        [DispatchRecoveryAction::Resend { attempt }] if attempt.key() == key
    ));
    assert_eq!(
        journal
            .snapshot()
            .unwrap_or_else(|error| panic!("{error}"))
            .get(key)
            .map(|attempt| attempt.phase),
        Some(DispatchPhase::Admitted)
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn expired_lease_fences_old_owner_and_selects_fallback() {
    let path = journal_path();
    let (key, admitted) = intent(&path);
    let (journal, _) = DispatchJournal::open(&path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .admit(admitted)
        .unwrap_or_else(|error| panic!("{error}"));
    let actions = journal
        .restart(101, &owner_authority(7, 11, 0))
        .unwrap_or_else(|error| panic!("{error}"));
    let DispatchRecoveryAction::Fallback {
        attempt, reason, ..
    } = &actions[0]
    else {
        panic!("expired lease must select local fallback");
    };
    assert_eq!(*reason, RESTART_LEASE_EXPIRED);
    assert_eq!(attempt.phase, DispatchPhase::Fallback);
    assert_ne!(attempt.current_fence, [3; 32]);
    drop(journal);
    let (_, recovery) = DispatchJournal::open(&path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        recovery.get(key).map(|attempt| attempt.phase),
        Some(DispatchPhase::Fallback)
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn fallback_can_publish_its_local_output_after_takeover() {
    let path = journal_path();
    let (key, admitted) = intent(&path);
    let (journal, _) = DispatchJournal::open(&path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .admit(admitted)
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .fallback(key, [0; 32], RESTART_LEASE_EXPIRED, 0)
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .fallback(key, [9; 32], RESTART_LEASE_EXPIRED, 1)
        .unwrap_or_else(|error| panic!("{error}"));
    let attempt = journal
        .snapshot()
        .unwrap_or_else(|error| panic!("{error}"))
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("fallback attempt missing"));
    assert_eq!(
        attempt.terminal,
        Some(TerminalState::Fallback {
            output_root: [9; 32],
            reason: RESTART_LEASE_EXPIRED,
            notification_cursor: 1,
        })
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn accepted_retry_ignores_timestamp_after_fallback_fence() {
    let path = journal_path();
    let (key, admitted) = intent(&path);
    let (journal, _) = DispatchJournal::open(&path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .admit(admitted)
        .unwrap_or_else(|error| panic!("{error}"));
    let first = AcceptedResultProof::new([6; 32], Box::<[u8]>::from([1, 2]), 10)
        .unwrap_or_else(|error| panic!("{error}"))
        .with_staged_objects([7; 32], [8; 32], Box::<[u8]>::from([9]))
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .accept_result(key, first.clone())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .fence(key, 8, [4; 32], RESTART_LEASE_EXPIRED)
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .fallback(key, [0; 32], RESTART_LEASE_EXPIRED, 3)
        .unwrap_or_else(|error| panic!("{error}"));

    // A deterministic local retry has the same immutable payload but a new
    // owner-clock observation.  It must reuse the original accepted proof
    // rather than turn the timestamp into an identity collision.
    let retry = AcceptedResultProof {
        accepted_at: 99,
        ..first
    };
    journal
        .accept_result(key, retry)
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .begin_publication(key)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        journal
            .point(key)
            .unwrap_or_else(|error| panic!("{error}"))
            .map(|attempt| attempt.phase),
        Some(DispatchPhase::PublicationPending)
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn same_key_with_different_content_is_rejected() {
    let path = journal_path();
    let (key, admitted) = intent(&path);
    let (journal, _) = DispatchJournal::open(&path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .admit(admitted.clone())
        .unwrap_or_else(|error| panic!("{error}"));
    let mut collision = admitted;
    collision.request = Box::<[u8]>::from([0]);
    let error = journal
        .admit(collision)
        .expect_err("same attempt identity must not accept another request");
    assert!(matches!(
        error,
        DispatchJournalError::Record(DispatchRecordError::IdentityCollision)
    ));
    assert_eq!(
        journal
            .snapshot()
            .unwrap_or_else(|error| panic!("{error}"))
            .get(key)
            .map(|attempt| attempt.intent.request.as_ref()),
        Some([9, 8, 7, 6].as_slice())
    );
    drop(journal);
    let _ = std::fs::remove_file(path);
}

#[test]
fn publication_cannot_precede_or_change_accepted_proof() {
    let path = journal_path();
    let (key, admitted) = intent(&path);
    let (journal, _) = DispatchJournal::open(&path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .admit(admitted)
        .unwrap_or_else(|error| panic!("{error}"));
    let error = journal
        .acknowledge_publication(
            key,
            PublicationAck {
                output_root: [6; 32],
                store: store_receipt(),
                owner_epoch: 7,
                notification_cursor: 0,
            },
        )
        .expect_err("publication requires an accepted proof");
    assert!(matches!(
        error,
        DispatchJournalError::Record(DispatchRecordError::PublicationMismatch)
    ));
    let proof = AcceptedResultProof::new([6; 32], Box::<[u8]>::from([1]), 1)
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .accept_result(key, proof)
        .unwrap_or_else(|error| panic!("{error}"));
    let error = journal
        .acknowledge_publication(
            key,
            PublicationAck {
                output_root: [7; 32],
                store: store_receipt(),
                owner_epoch: 7,
                notification_cursor: 0,
            },
        )
        .expect_err("publication output must match the durable proof");
    assert!(matches!(
        error,
        DispatchJournalError::Record(DispatchRecordError::PublicationMismatch)
    ));
    let error = journal
        .fallback(key, [7; 32], RESTART_LEASE_EXPIRED, 0)
        .expect_err("fallback cannot replace an accepted output identity");
    assert!(matches!(
        error,
        DispatchJournalError::Record(DispatchRecordError::PublicationMismatch)
    ));
    drop(journal);
    let _ = std::fs::remove_file(path);
}

#[test]
fn point_lookup_and_fenced_publication_do_not_clone_or_ack_a_replacement() {
    let path = journal_path();
    let (key, admitted) = intent(&path);
    let (journal, _) = DispatchJournal::open(&path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .admit(admitted)
        .unwrap_or_else(|error| panic!("{error}"));
    let proof = AcceptedResultProof::new([6; 32], Box::<[u8]>::from([1, 2]), 4)
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .accept_result(key, proof)
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .begin_publication(key)
        .unwrap_or_else(|error| panic!("{error}"));

    let point = journal
        .point(key)
        .unwrap_or_else(|error| panic!("{error}"))
        .unwrap_or_else(|| panic!("point attempt missing"));
    assert_eq!(point.current_fence, [3; 32]);
    assert_eq!(point.phase, DispatchPhase::PublicationPending);
    assert!(
        journal
            .point_if(key, [3; 32])
            .unwrap_or_else(|error| panic!("{error}"))
            .is_some()
    );
    assert!(matches!(
        journal.point_if(key, [4; 32]),
        Err(DispatchJournalError::Record(
            DispatchRecordError::PublicationMismatch
        ))
    ));

    let ack = PublicationAck {
        output_root: [6; 32],
        store: store_receipt(),
        owner_epoch: 7,
        notification_cursor: 5,
    };
    let stale = journal.acknowledge_publication_if(key, [4; 32], ack);
    assert!(matches!(
        stale,
        Err(DispatchJournalError::Record(
            DispatchRecordError::PublicationMismatch
        ))
    ));
    assert_eq!(
        journal
            .point(key)
            .unwrap_or_else(|error| panic!("{error}"))
            .map(|attempt| attempt.phase),
        Some(DispatchPhase::PublicationPending)
    );

    journal
        .acknowledge_publication_if(key, [3; 32], ack)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        journal
            .point(key)
            .unwrap_or_else(|error| panic!("{error}"))
            .map(|attempt| attempt.phase),
        Some(DispatchPhase::Published)
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn owner_restart_capability_rejects_stale_workspace_or_fence() {
    let log_path = journal_path();
    let (_key, admitted) = intent(&log_path);
    let (journal, _) = DispatchJournal::open(&log_path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .admit(admitted)
        .unwrap_or_else(|error| panic!("{error}"));

    let stale_workspace = OwnerRestartAuthority::mint([9; 32], 7, [3; 32], 11, 0);
    let actions = journal
        .restart(10, &stale_workspace)
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(matches!(
        actions.as_slice(),
        [DispatchRecoveryAction::Fallback { reason, .. }]
            if *reason == RESTART_OWNER_TAKEOVER
    ));
    drop(journal);

    let stale_path = journal_path();
    let (stale_journal, _) = DispatchJournal::open(&stale_path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    let (_, stale_admitted) = intent(&stale_path);
    stale_journal
        .admit(stale_admitted)
        .unwrap_or_else(|error| panic!("{error}"));
    let stale_fence = OwnerRestartAuthority::mint([2; 32], 7, [8; 32], 11, 0);
    let actions = stale_journal
        .restart(10, &stale_fence)
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(matches!(
        actions.as_slice(),
        [DispatchRecoveryAction::Fallback { reason, .. }]
            if *reason == RESTART_OWNER_TAKEOVER
    ));
    drop(stale_journal);
    let _ = std::fs::remove_file(stale_path);
    let _ = std::fs::remove_file(log_path);
}

#[test]
fn stale_owner_capability_cannot_resume_after_epoch_takeover() {
    let stale = owner_authority(7, 11, 19);
    assert!(stale.matches_owner([2; 32], 7, [3; 32]));
    assert!(!stale.matches_owner([2; 32], 8, [4; 32]));
    assert!(!stale.matches_owner([2; 32], 8, [3; 32]));
    assert!(!stale.matches_owner([9; 32], 7, [3; 32]));

    let path = journal_path();
    let (key, mut admitted) = intent(&path);
    admitted.owner_epoch = 8;
    let (journal, _) = DispatchJournal::open(&path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .admit(admitted.clone())
        .unwrap_or_else(|error| panic!("{error}"));

    // A capability from the previous owner is older than the durable intent.
    // It must defer without appending a fence or fallback record, leaving the
    // current owner the only authority able to classify this attempt.
    let actions = journal
        .restart(10, &stale)
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(actions.is_empty());
    let attempt = journal
        .point(key)
        .unwrap_or_else(|error| panic!("{error}"))
        .unwrap_or_else(|| panic!("attempt missing"));
    assert_eq!(attempt.intent, admitted);
    assert_eq!(attempt.phase, DispatchPhase::Admitted);
    assert_eq!(attempt.current_fence, [3; 32]);
    drop(journal);
    let _ = std::fs::remove_file(path);
}

#[test]
fn fused_publication_cursor_survives_notification_crash() {
    let path = journal_path();
    let (key, admitted) = intent(&path);
    let faults = std::sync::Arc::new(Faults::default());
    let (journal, _) = DispatchJournal::open_with_faults(
        &path,
        DispatchJournalLimits::default(),
        std::sync::Arc::clone(&faults),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    journal
        .admit(admitted)
        .unwrap_or_else(|error| panic!("{error}"));
    let proof = AcceptedResultProof::new([6; 32], Box::<[u8]>::from([1, 2]), 4)
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .accept_result(key, proof)
        .unwrap_or_else(|error| panic!("{error}"));
    journal
        .begin_publication(key)
        .unwrap_or_else(|error| panic!("{error}"));
    let position = NotificationCursor::new(17, 23, Box::<[u8]>::from([4, 5]))
        .unwrap_or_else(|error| panic!("{error}"));
    faults.arm(Boundary::Notification);
    let error = journal
        .acknowledge_publication_with_cursor_if(
            key,
            [3; 32],
            PublicationAck {
                output_root: [6; 32],
                store: store_receipt(),
                owner_epoch: 7,
                notification_cursor: 23,
            },
            position.clone(),
        )
        .expect_err("notification fault must be observable after fused append");
    assert!(matches!(
        error,
        DispatchJournalError::Fault(crate::fault::InjectedCrash {
            boundary: Boundary::Notification
        })
    ));
    drop(journal);

    let (_, recovery) = DispatchJournal::open(&path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    let attempt = recovery
        .get(key)
        .unwrap_or_else(|| panic!("attempt missing after fused publication"));
    assert_eq!(attempt.phase, DispatchPhase::Published);
    assert_eq!(attempt.cursor.as_ref(), Some(&position));
    let _ = std::fs::remove_file(path);
}

#[test]
fn every_lifecycle_transition_has_a_cooperative_abort_seam() {
    let boundaries = [
        Boundary::Prepare,
        Boundary::Transfer,
        Boundary::OutputAdmission,
        Boundary::JournalPrepared,
        Boundary::JournalPublished,
        Boundary::OwnerFence,
        Boundary::Notification,
    ];
    for boundary in boundaries {
        let path = journal_path();
        let (key, admitted) = intent(&path);
        let faults = std::sync::Arc::new(Faults::default());
        let (journal, _) = DispatchJournal::open_with_faults(
            &path,
            DispatchJournalLimits::default(),
            std::sync::Arc::clone(&faults),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        let proof = AcceptedResultProof::new([6; 32], Box::<[u8]>::from([1]), 4)
            .unwrap_or_else(|error| panic!("{error}"));
        if boundary != Boundary::Prepare {
            journal
                .admit(admitted.clone())
                .unwrap_or_else(|error| panic!("{error}"));
        }
        if matches!(
            boundary,
            Boundary::JournalPrepared | Boundary::JournalPublished
        ) {
            journal
                .accept_result(key, proof.clone())
                .unwrap_or_else(|error| panic!("{error}"));
        }
        if boundary == Boundary::JournalPublished {
            journal
                .begin_publication(key)
                .unwrap_or_else(|error| panic!("{error}"));
        }
        faults.arm(boundary);
        let result = match boundary {
            Boundary::Prepare => journal.admit(admitted),
            Boundary::Transfer => journal.record_transfer(
                key,
                TransferCheckpointRef::new([4; 32], [5; 32], 2, 1, DispatchPhase::Transferring)
                    .unwrap_or_else(|error| panic!("{error}")),
            ),
            Boundary::OutputAdmission => journal.accept_result(key, proof),
            Boundary::JournalPrepared => journal.begin_publication(key),
            Boundary::JournalPublished => journal.acknowledge_publication(
                key,
                PublicationAck {
                    output_root: [6; 32],
                    store: store_receipt(),
                    owner_epoch: 7,
                    notification_cursor: 1,
                },
            ),
            Boundary::OwnerFence => journal.fence(key, 8, [9; 32], 1),
            Boundary::Notification => journal.update_cursor(
                key,
                NotificationCursor::new(1, 1, Box::<[u8]>::from([2]))
                    .unwrap_or_else(|error| panic!("{error}")),
            ),
            _ => unreachable!("all transition boundaries are covered"),
        };
        assert!(matches!(
            result,
            Err(DispatchJournalError::Fault(crate::fault::InjectedCrash {
                boundary: found
            })) if found == boundary
        ));
        drop(journal);
        let (_, recovery) = DispatchJournal::open(&path, DispatchJournalLimits::default())
            .unwrap_or_else(|error| panic!("{error}"));
        if boundary == Boundary::Prepare {
            assert!(recovery.is_empty());
        } else {
            assert!(recovery.get(key).is_some());
        }
        let _ = std::fs::remove_file(path);
    }
}

#[test]
fn concurrent_admission_serializes_one_chain_without_duplicate_state() {
    let path = journal_path();
    let (journal, _) = DispatchJournal::open(&path, DispatchJournalLimits::default())
        .unwrap_or_else(|error| panic!("{error}"));
    let journal = std::sync::Arc::new(journal);
    let mut workers = Vec::new();
    for ordinal in 1..=16_u64 {
        let journal = std::sync::Arc::clone(&journal);
        workers.push(std::thread::spawn(move || {
            let byte = u8::try_from(ordinal).unwrap_or_else(|error| panic!("{error}"));
            let key = DispatchAttemptKey::new([byte; 32], ordinal)
                .unwrap_or_else(|error| panic!("{error}"));
            let admitted = RemoteAttemptIntent::new(
                key,
                Box::<[u8]>::from([byte]),
                [2; 32],
                [byte; 32],
                1,
                100,
                90,
            )
            .unwrap_or_else(|error| panic!("{error}"));
            journal
                .admit(admitted)
                .unwrap_or_else(|error| panic!("{error}"));
        }));
    }
    for worker in workers {
        worker.join().unwrap_or_else(|_| panic!("worker panicked"));
    }
    let recovery = journal.recover().unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(recovery.frames_scanned, 16);
    assert_eq!(recovery.len(), 16);
    let _ = std::fs::remove_file(path);
}
