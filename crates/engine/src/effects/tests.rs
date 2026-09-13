use super::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
struct BytesSpec;
impl EffectSpec for BytesSpec {
    type Intent = Arc<[u8]>;
    type Request = Arc<[u8]>;
    type Receipt = Arc<[u8]>;
    fn key(&self, intent: &Self::Intent) -> EffectKey {
        effect_key(intent)
    }
    fn request(&self, intent: &Self::Intent) -> Self::Request {
        Arc::clone(intent)
    }
    fn validate_receipt(
        &self,
        key: EffectKey,
        request: &Self::Request,
        receipt: &Self::Receipt,
    ) -> Result<(), EffectError> {
        if effect_key(request) == key && receipt.as_ref() == request.as_ref() {
            Ok(())
        } else {
            Err(EffectError::InvalidReceipt(
                "receipt does not bind to request".into(),
            ))
        }
    }
}

#[derive(Default)]
struct Sink {
    applied: BTreeMap<EffectKey, Arc<[u8]>>,
    apply_count: usize,
}
impl EffectSink<BytesSpec> for Sink {
    fn apply(
        &mut self,
        key: EffectKey,
        request: Arc<[u8]>,
    ) -> Result<SinkApply<Arc<[u8]>>, SinkError> {
        self.apply_count += 1;
        self.applied.insert(key, Arc::clone(&request));
        Ok(SinkApply::Confirmed(request))
    }
    fn apply_borrowed(
        &mut self,
        key: EffectKey,
        request: &Arc<[u8]>,
    ) -> Result<SinkApply<Arc<[u8]>>, SinkError> {
        self.apply_count += 1;
        self.applied.insert(key, Arc::clone(request));
        Ok(SinkApply::Confirmed(Arc::clone(request)))
    }
    fn reconcile(&mut self, key: EffectKey) -> Result<SinkObservation<Arc<[u8]>>, SinkError> {
        Ok(self
            .applied
            .get(&key)
            .cloned()
            .map_or(SinkObservation::Absent, SinkObservation::Applied))
    }
}

#[derive(Clone, Default)]
struct SharedIdempotentSink {
    applied: Arc<Mutex<BTreeMap<EffectKey, Arc<[u8]>>>>,
    apply_count: Arc<AtomicUsize>,
}

impl EffectSink<BytesSpec> for SharedIdempotentSink {
    fn apply(
        &mut self,
        key: EffectKey,
        request: Arc<[u8]>,
    ) -> Result<SinkApply<Arc<[u8]>>, SinkError> {
        self.apply_count.fetch_add(1, Ordering::Relaxed);
        let mut applied = self.applied.lock().map_err(|_| SinkError::Unknown)?;
        if let Some(existing) = applied.get(&key) {
            return if existing.as_ref() == request.as_ref() {
                Ok(SinkApply::Confirmed(Arc::clone(existing)))
            } else {
                Err(SinkError::Rejected)
            };
        }
        applied.insert(key, Arc::clone(&request));
        Ok(SinkApply::Confirmed(request))
    }

    fn reconcile(&mut self, key: EffectKey) -> Result<SinkObservation<Arc<[u8]>>, SinkError> {
        let applied = self.applied.lock().map_err(|_| SinkError::Unknown)?;
        Ok(applied
            .get(&key)
            .cloned()
            .map_or(SinkObservation::Absent, SinkObservation::Applied))
    }
}

#[derive(Clone, Default)]
struct BytesCodec;
impl EffectCodec<BytesSpec> for BytesCodec {
    fn encode_intent(&self, intent: &Arc<[u8]>) -> Result<Vec<u8>, EffectError> {
        Ok(intent.to_vec())
    }
    fn decode_intent(&self, bytes: &[u8]) -> Result<Arc<[u8]>, EffectError> {
        Ok(Arc::from(bytes))
    }
    fn encode_receipt(&self, receipt: &Arc<[u8]>) -> Result<Vec<u8>, EffectError> {
        Ok(receipt.to_vec())
    }
    fn decode_receipt(&self, bytes: &[u8]) -> Result<Arc<[u8]>, EffectError> {
        Ok(Arc::from(bytes))
    }
}

fn temp_path(label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "backend-effects-{label}-{}-{nonce}.log",
        std::process::id()
    ))
}

#[test]
fn confirmation_persistence_failure_is_live_ambiguous() {
    let mut persistence = MemoryPersistence::<BytesSpec>::default();
    persistence.fail_next_confirmation = true;
    let mut coordinator = EffectCoordinator::new(BytesSpec, persistence, Sink::default());
    let prepared = coordinator
        .prepare(Arc::from(&b"payload"[..]))
        .expect("prepare");
    let error = coordinator
        .execute(prepared.key)
        .expect_err("confirmation failure");
    assert!(matches!(error, EffectError::Persistence(_)));
    assert_eq!(
        coordinator.state(prepared.key),
        Some(EffectPhase::Ambiguous)
    );
    assert_eq!(
        coordinator
            .state_detail(prepared.key)
            .and_then(EffectState::ambiguous_reason),
        Some(AmbiguousReason::UnknownOutcome)
    );
    assert_eq!(
        coordinator
            .reconcile(prepared.key)
            .expect("reconcile")
            .phase,
        EffectPhase::Confirmed
    );
}

#[test]
fn confirmation_crash_reconciles_without_a_duplicate_sink_call() {
    let path = temp_path("confirmation-crash");
    let sink = SharedIdempotentSink::default();
    let faults = Arc::new(crate::fault::Faults::default());
    let (persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec).expect("open");
    let persistence = persistence.with_faults(Arc::clone(&faults));
    let mut coordinator = EffectCoordinator::new(BytesSpec, persistence, sink.clone());
    let key = coordinator
        .prepare(Arc::from(&b"confirmation-crash"[..]))
        .expect("prepare")
        .key;
    faults.arm(crate::fault::Boundary::EffectConfirmed);
    assert!(matches!(
        coordinator.execute(key),
        Err(EffectError::Injected(crate::fault::InjectedCrash {
            boundary: crate::fault::Boundary::EffectConfirmed
        }))
    ));
    assert_eq!(sink.apply_count.load(Ordering::Relaxed), 1);
    drop(coordinator);

    let (persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec).expect("reopen");
    let recovered = persistence.replay(&BytesSpec).expect("replay");
    let mut restarted = EffectCoordinator::new(BytesSpec, persistence, sink.clone());
    let handles = restarted
        .restore_and_reconcile(recovered)
        .expect("reconcile");
    assert_eq!(handles.len(), 1);
    assert_eq!(handles[0].phase, EffectPhase::Confirmed);
    assert_eq!(sink.apply_count.load(Ordering::Relaxed), 1);
    let _ = std::fs::remove_file(path);
}

#[test]
fn pre_call_crash_recovers_the_durable_fence_without_calling_the_sink() {
    let path = temp_path("pre-call-crash");
    let sink = SharedIdempotentSink::default();
    let faults = Arc::new(crate::fault::Faults::default());
    let (persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec).expect("open");
    let mut coordinator = EffectCoordinator::new(
        BytesSpec,
        persistence.with_faults(Arc::clone(&faults)),
        sink.clone(),
    );
    let key = coordinator
        .prepare(Arc::from(&b"pre-call-crash"[..]))
        .expect("prepare")
        .key;
    faults.arm(crate::fault::Boundary::EffectCall);
    assert!(matches!(
        coordinator.execute(key),
        Err(EffectError::Injected(crate::fault::InjectedCrash {
            boundary: crate::fault::Boundary::EffectCall
        }))
    ));
    assert_eq!(sink.apply_count.load(Ordering::Relaxed), 0);
    drop(coordinator);

    let (persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec).expect("reopen");
    let recovered = persistence.replay(&BytesSpec).expect("replay");
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].state.phase(), EffectPhase::Ambiguous);
    assert_eq!(
        recovered[0].state.ambiguous_reason(),
        Some(AmbiguousReason::CrashAfterCall)
    );
    let mut restarted = EffectCoordinator::new(BytesSpec, persistence, sink);
    let handles = restarted
        .restore_and_reconcile(recovered)
        .expect("reconcile absent fence");
    assert_eq!(handles[0].phase, EffectPhase::Prepared);
    let _ = std::fs::remove_file(path);
}

#[test]
fn prepare_execute_confirm_order_is_durable() {
    let mut coordinator =
        EffectCoordinator::new(BytesSpec, MemoryPersistence::default(), Sink::default());
    let key = coordinator
        .prepare(Arc::from(&b"ordered"[..]))
        .expect("prepare")
        .key;
    coordinator.execute(key).expect("execute");
    let records = &coordinator.persistence_mut().records;
    assert!(matches!(records[0], EffectRecord::Prepared(_)));
    assert!(matches!(records[1], EffectRecord::Executing(_, _)));
    assert!(matches!(records[2], EffectRecord::Confirmed(_, _)));
}

#[test]
fn replay_uses_fixed_width_key_index_and_recovers_history() {
    let path = temp_path("replay");
    let (mut persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec).expect("open");
    let mut keys = Vec::new();
    for i in 0..256u16 {
        let intent: Arc<[u8]> = Arc::from(format!("effect-{i}").into_bytes());
        let key = effect_key(&intent);
        persistence.prepared(key, &intent).expect("prepared");
        let ordinal = u64::from(i) + 1;
        let fence = state::fence(key, ordinal);
        persistence
            .executing_with_ordinal(key, fence, ordinal)
            .expect("executing");
        keys.push((key, intent, fence, ordinal));
    }
    let recovered = persistence.replay(&BytesSpec).expect("replay");
    assert_eq!(recovered.len(), keys.len());
    assert!(
        recovered
            .iter()
            .all(|value| value.state.phase() == EffectPhase::Ambiguous)
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn effect_retry_reuses_an_exact_tail_after_an_uncertain_append() {
    let path = temp_path("effect-retry");
    let record = EffectJournalRecord::Prepared {
        key: [1; 32],
        intent: b"effect-retry".to_vec(),
    };
    let receipt = crate::journal::HashChainJournal::<EffectLog>::open(&path)
        .expect("open")
        .0
        .append(&record)
        .expect("append");
    let uncertain = crate::journal::JournalError::AppendUncertain {
        offset: receipt.offset,
        sequence: receipt.sequence,
        chain: *receipt.chain.as_bytes(),
        record: *receipt.record.as_bytes(),
    };
    let candidate = uncertain
        .uncertain_receipt::<EffectLog>()
        .expect("uncertain receipt");

    let journal = crate::journal::HashChainJournal::<EffectLog>::open(&path)
        .expect("reopen")
        .0;
    assert!(matches!(
        journal.retry(
            candidate,
            &EffectJournalRecord::Prepared {
                key: [1; 32],
                intent: b"different".to_vec(),
            },
        ),
        Err(crate::journal::JournalError::Corrupt("append receipt"))
    ));
    drop(journal);

    let journal = crate::journal::HashChainJournal::<EffectLog>::open(&path)
        .expect("reopen for exact retry")
        .0;
    assert_eq!(
        journal.retry(candidate, &record).expect("exact retry"),
        receipt
    );
    assert_eq!(
        journal
            .scan_stream(crate::journal::JournalLimits::default(), |_| Ok(()))
            .expect("scan")
            .frames_scanned,
        1
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn replay_limit_rejects_history_before_unbounded_work() {
    let path = temp_path("replay-limit");
    let (mut persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec).expect("open");
    for i in 0..16u8 {
        let intent: Arc<[u8]> = Arc::from(vec![i; 32]);
        persistence
            .prepared(effect_key(&intent), &intent)
            .expect("prepared");
    }
    let error = persistence
        .replay_with_limits(
            &BytesSpec,
            crate::journal::JournalLimits {
                max_frames: 8,
                max_bytes: 1024 * 1024,
            },
        )
        .expect_err("bounded replay must reject excess history");
    assert!(matches!(
        error,
        EffectError::Journal(crate::journal::JournalError::Bounds)
    ));
    let _ = std::fs::remove_file(path);
}

#[test]
fn stale_or_malformed_fence_is_rejected_during_replay() {
    let path = temp_path("fence");
    let (journal, _) = crate::journal::HashChainJournal::<EffectLog>::open(&path).expect("open");
    let intent: Arc<[u8]> = Arc::from(&b"fenced"[..]);
    let key = effect_key(&intent);
    journal
        .append(&EffectJournalRecord::Prepared {
            key: key.to_bytes(),
            intent: intent.to_vec(),
        })
        .expect("prepared");
    journal
        .append(&EffectJournalRecord::Executing {
            key: key.to_bytes(),
            fence: state::fence(key, 2),
            ordinal: 1,
        })
        .expect("executing");
    let (persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec).expect("reopen");
    assert!(matches!(
        persistence.replay(&BytesSpec),
        Err(EffectError::ConflictingHistory)
    ));
    let _ = std::fs::remove_file(path);
}

#[test]
fn replay_accepts_an_idempotent_prepared_retry_after_absent_reconciliation() {
    let path = temp_path("prepared-retry");
    let (journal, _) = crate::journal::HashChainJournal::<EffectLog>::open(&path).expect("open");
    let intent: Arc<[u8]> = Arc::from(&b"retry-me"[..]);
    let key = effect_key(&intent);
    let append = |record| journal.append(&record).expect("append");
    append(EffectJournalRecord::Prepared {
        key: key.to_bytes(),
        intent: intent.to_vec(),
    });
    let effect_fence = state::fence(key, 1);
    append(EffectJournalRecord::Executing {
        key: key.to_bytes(),
        fence: effect_fence,
        ordinal: 1,
    });
    append(EffectJournalRecord::Ambiguous {
        key: key.to_bytes(),
        fence: effect_fence,
        ordinal: 1,
        reason: AmbiguousReason::Timeout,
    });
    // SinkObservation::Absent is represented by a new Prepared marker before
    // the next fenced attempt.  Replaying that cycle must remain valid.
    append(EffectJournalRecord::Prepared {
        key: key.to_bytes(),
        intent: intent.to_vec(),
    });
    let _ = append;
    drop(journal);

    let (persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec).expect("reopen");
    let recovered = persistence.replay(&BytesSpec).expect("replay");
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].state.phase(), EffectPhase::Prepared);
    let _ = std::fs::remove_file(path);
}

#[test]
fn duplicate_confirmation_is_idempotent_but_conflicting_receipt_is_not() {
    let path = temp_path("duplicate-confirmed");
    let (journal, _) = crate::journal::HashChainJournal::<EffectLog>::open(&path).expect("open");
    let intent: Arc<[u8]> = Arc::from(&b"confirm-me"[..]);
    let key = effect_key(&intent);
    let append = |record| journal.append(&record).expect("append");
    append(EffectJournalRecord::Prepared {
        key: key.to_bytes(),
        intent: intent.to_vec(),
    });
    let effect_fence = state::fence(key, 1);
    append(EffectJournalRecord::Executing {
        key: key.to_bytes(),
        fence: effect_fence,
        ordinal: 1,
    });
    append(EffectJournalRecord::Confirmed {
        key: key.to_bytes(),
        receipt: intent.to_vec(),
    });
    append(EffectJournalRecord::Confirmed {
        key: key.to_bytes(),
        receipt: intent.to_vec(),
    });
    let _ = append;
    drop(journal);
    let (persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec).expect("reopen");
    let recovered = persistence.replay(&BytesSpec).expect("duplicate replay");
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].state.phase(), EffectPhase::Confirmed);
    drop(persistence);

    let journal = crate::journal::HashChainJournal::<EffectLog>::open(&path)
        .expect("open for conflict")
        .0;
    journal
        .append(&EffectJournalRecord::Confirmed {
            key: key.to_bytes(),
            receipt: b"different".to_vec(),
        })
        .expect("append conflicting receipt");
    drop(journal);
    let (persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec).expect("reopen");
    assert!(matches!(
        persistence.replay(&BytesSpec),
        Err(EffectError::ConflictingHistory)
    ));
    let _ = std::fs::remove_file(path);
}

#[test]
fn replay_rejects_reused_attempt_ordinals_after_a_retry() {
    let path = temp_path("ordinal-reuse");
    let (journal, _) = crate::journal::HashChainJournal::<EffectLog>::open(&path).expect("open");
    let intent: Arc<[u8]> = Arc::from(&b"ordinal"[..]);
    let key = effect_key(&intent);
    let append = |record| journal.append(&record).expect("append");
    append(EffectJournalRecord::Prepared {
        key: key.to_bytes(),
        intent: intent.to_vec(),
    });
    let first_fence = state::fence(key, 1);
    append(EffectJournalRecord::Executing {
        key: key.to_bytes(),
        fence: first_fence,
        ordinal: 1,
    });
    append(EffectJournalRecord::Ambiguous {
        key: key.to_bytes(),
        fence: first_fence,
        ordinal: 1,
        reason: AmbiguousReason::Timeout,
    });
    append(EffectJournalRecord::Prepared {
        key: key.to_bytes(),
        intent: intent.to_vec(),
    });
    append(EffectJournalRecord::Executing {
        key: key.to_bytes(),
        fence: first_fence,
        ordinal: 1,
    });
    let _ = append;
    drop(journal);
    let (persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec).expect("reopen");
    assert!(matches!(
        persistence.replay(&BytesSpec),
        Err(EffectError::ConflictingHistory)
    ));
    let _ = std::fs::remove_file(path);
}

#[test]
fn streaming_open_and_replay_bound_history_scan_to_the_frame_envelope() {
    let path = temp_path("streaming-replay");
    let limits = crate::journal::JournalLimits {
        max_frames: 4_096,
        max_bytes: 8 * 1024 * 1024,
    };
    let (mut persistence, scan) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open_streaming(
            &path, BytesCodec, limits,
        )
        .expect("open streaming");
    assert_eq!(scan.frames_scanned, 0);
    for index in 0..2_048u16 {
        let intent: Arc<[u8]> = Arc::from(format!("stream-{index}").into_bytes());
        persistence
            .prepared(effect_key(&intent), &intent)
            .expect("prepared");
    }
    let recovered = persistence
        .replay_with_limits(&BytesSpec, limits)
        .expect("streaming replay");
    assert_eq!(recovered.len(), 2_048);
    let _ = std::fs::remove_file(path);
}

#[test]
fn confirmed_history_keeps_the_attempt_watermark_across_coordinator_restart() {
    let path = temp_path("ordinal-watermark");
    let (mut persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec).expect("open");
    let first: Arc<[u8]> = Arc::from(&b"first-confirmed"[..]);
    let first_key = effect_key(&first);
    persistence.prepared(first_key, &first).expect("prepared");
    let first_fence = state::fence(first_key, 1);
    persistence
        .executing_with_ordinal(first_key, first_fence, 1)
        .expect("executing");
    persistence.confirmed(first_key, &first).expect("confirmed");
    let recovered = persistence.replay(&BytesSpec).expect("replay");

    let mut coordinator = EffectCoordinator::new(BytesSpec, persistence, Sink::default());
    coordinator
        .restore_recovered(recovered)
        .expect("restore confirmed");
    let second: Arc<[u8]> = Arc::from(&b"second-confirmed"[..]);
    let second_key = coordinator.prepare(second).expect("prepare second").key;
    coordinator.execute(second_key).expect("execute second");
    let replayed = coordinator
        .persistence_mut()
        .replay(&BytesSpec)
        .expect("replay after restart");
    assert_eq!(replayed.len(), 2);
    assert!(
        replayed
            .iter()
            .all(|entry| entry.state.phase() == EffectPhase::Confirmed)
    );
    let _ = std::fs::remove_file(path);
}

fn compacted_fixture(
    path: &std::path::Path,
    limits: crate::journal::JournalLimits,
) -> (Arc<[u8]>, Arc<[u8]>) {
    let (mut persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(path, BytesCodec).expect("open");
    let pending: Arc<[u8]> = Arc::from(&b"pending-after-compaction"[..]);
    let pending_key = effect_key(&pending);
    persistence
        .prepared(pending_key, &pending)
        .expect("prepared");
    let applied: Arc<[u8]> = Arc::from(&b"confirmed-before-compaction"[..]);
    let applied_key = effect_key(&applied);
    persistence
        .prepared(applied_key, &applied)
        .expect("prepared");
    let ordinal = 1;
    persistence
        .executing_with_ordinal(applied_key, state::fence(applied_key, ordinal), ordinal)
        .expect("executing");
    persistence
        .confirmed(applied_key, &applied)
        .expect("confirmed");
    let expected = persistence
        .replay(&BytesSpec)
        .expect("replay before checkpoint");
    let receipt = persistence
        .checkpoint_and_compact(
            &BytesSpec,
            expected,
            EffectSnapshotLimits {
                max_entries: 8,
                max_bytes: 64 * 1024,
            },
            limits,
        )
        .expect("checkpoint and compact");
    assert_ne!(receipt.record, [0; 32]);
    assert!(std::fs::metadata(path).expect("compacted journal").len() > 0);
    (pending, applied)
}

fn assert_compacted_restart(
    path: &std::path::Path,
    limits: crate::journal::JournalLimits,
    pending: &Arc<[u8]>,
    applied: &Arc<[u8]>,
) {
    let (reopened, scan) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open_streaming(path, BytesCodec, limits)
            .expect("reopen streaming");
    assert_eq!(scan.frames_scanned, 1);
    let recovered = reopened
        .replay_with_limits(&BytesSpec, limits)
        .expect("replay snapshot");
    assert_eq!(recovered.len(), 2);
    assert!(
        recovered
            .iter()
            .any(|entry| entry.intent.as_ref() == pending.as_ref()
                && entry.state.phase() == EffectPhase::Prepared)
    );
    assert!(
        recovered
            .iter()
            .any(|entry| entry.intent.as_ref() == applied.as_ref()
                && entry.state.phase() == EffectPhase::Confirmed)
    );
    drop(reopened);

    let pointer_path = path.with_file_name(format!(
        "{}.snapshot.pointer",
        path.file_name().expect("journal name").to_string_lossy()
    ));
    std::fs::remove_file(&pointer_path).expect("remove selected pointer");
    let (reopened, scan) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open_streaming(path, BytesCodec, limits)
            .expect("reopen from compacted marker");
    assert_eq!(scan.frames_scanned, 1);
    assert_eq!(
        reopened
            .replay_with_limits(&BytesSpec, limits)
            .expect("replay from compacted marker")
            .len(),
        2
    );
}

fn assert_compacted_tail_restart(path: &std::path::Path, limits: crate::journal::JournalLimits) {
    let tail: Arc<[u8]> = Arc::from(&b"tail-after-compaction"[..]);
    let (mut reopened, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open_streaming(path, BytesCodec, limits)
            .expect("reopen before tail");
    reopened
        .prepared(effect_key(&tail), &tail)
        .expect("tail prepared");
    drop(reopened);
    let (reopened, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open_streaming(path, BytesCodec, limits)
            .expect("reopen with tail");
    let recovered = reopened
        .replay_with_limits(&BytesSpec, limits)
        .expect("replay tail");
    assert_eq!(recovered.len(), 3);
    assert!(
        recovered
            .iter()
            .any(|entry| entry.intent.as_ref() == tail.as_ref())
    );
}

#[test]
fn checkpoint_compaction_restarts_from_active_state_and_authenticated_marker() {
    let path = temp_path("checkpoint-compaction");
    let limits = crate::journal::JournalLimits {
        max_frames: 16,
        max_bytes: 1024 * 1024,
    };
    let (pending, applied) = compacted_fixture(&path, limits);
    assert_compacted_restart(&path, limits, &pending, &applied);
    assert_compacted_tail_restart(&path, limits);
    let snapshot_path = path.with_file_name(format!(
        "{}.snapshot",
        path.file_name().expect("journal name").to_string_lossy()
    ));
    let pointer_path = path.with_file_name(format!(
        "{}.snapshot.pointer",
        path.file_name().expect("journal name").to_string_lossy()
    ));
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(snapshot_path);
    let _ = std::fs::remove_file(pointer_path);
}

#[test]
fn selected_snapshot_rejects_a_truncated_prefix_without_compaction_marker() {
    let path = temp_path("checkpoint-truncated");
    let (mut persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec).expect("open");
    let intent: Arc<[u8]> = Arc::from(&b"snapshot-state"[..]);
    let key = effect_key(&intent);
    persistence.prepared(key, &intent).expect("prepared");
    let recovered = persistence.replay(&BytesSpec).expect("replay");
    persistence
        .checkpoint(&BytesSpec, recovered, EffectSnapshotLimits::default())
        .expect("checkpoint");
    drop(persistence);

    // A short ordinary journal is not evidence of an atomic replacement. The
    // selected snapshot must fail closed instead of reviving stale state.
    std::fs::write(&path, []).expect("truncate journal");
    let (persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec)
            .expect("open truncated journal for fail-closed replay");
    let error = persistence
        .replay(&BytesSpec)
        .expect_err("truncated journal");
    assert!(matches!(
        error,
        EffectError::Journal(crate::journal::JournalError::Corrupt("checkpoint receipt"))
    ));

    let mut snapshot_name = path.file_name().expect("journal name").to_os_string();
    snapshot_name.push(".snapshot");
    let snapshot_path = path.parent().expect("journal parent").join(snapshot_name);
    let mut pointer_name = path.file_name().expect("journal name").to_os_string();
    pointer_name.push(".snapshot.pointer");
    let pointer_path = path.parent().expect("journal parent").join(pointer_name);
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(snapshot_path);
    let _ = std::fs::remove_file(pointer_path);
}

#[test]
fn snapshot_pointer_faults_keep_the_old_or_new_generation_recoverable() {
    let path = temp_path("checkpoint-pointer-faults");
    let limits = crate::journal::JournalLimits {
        max_frames: 16,
        max_bytes: 1024 * 1024,
    };
    let intent: Arc<[u8]> = Arc::from(&b"pointer-fault"[..]);
    let key = effect_key(&intent);
    let (mut persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec).expect("open");
    persistence.prepared(key, &intent).expect("prepared");
    let first = persistence.replay(&BytesSpec).expect("replay");
    persistence
        .checkpoint(&BytesSpec, first, EffectSnapshotLimits::default())
        .expect("initial checkpoint");

    let suffix: Arc<[u8]> = Arc::from(&b"pointer-fault-suffix"[..]);
    persistence
        .prepared(effect_key(&suffix), &suffix)
        .expect("suffix prepared");
    let faults = Arc::new(crate::fault::Faults::default());
    let mut persistence = persistence.with_faults(Arc::clone(&faults));
    let second = persistence.replay(&BytesSpec).expect("replay second");
    faults.arm(crate::fault::Boundary::Rename);
    assert!(matches!(
        persistence.checkpoint(&BytesSpec, second, EffectSnapshotLimits::default()),
        Err(EffectError::Injected(_))
    ));
    drop(persistence);

    // Rename was armed before the atomic replacement, so the old pointer is
    // still authoritative and the unselected snapshot frame is harmless.
    let (persistence, _) = JournalEffectPersistence::<BytesSpec, BytesCodec>::open_streaming(
        &path, BytesCodec, limits,
    )
    .expect("reopen old pointer");
    assert_eq!(
        persistence
            .replay_with_limits(&BytesSpec, limits)
            .expect("replay old")
            .len(),
        2
    );
    drop(persistence);

    let (mut persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec).expect("open");
    let third = persistence.replay(&BytesSpec).expect("replay third");
    let faults = Arc::new(crate::fault::Faults::default());
    persistence = persistence.with_faults(Arc::clone(&faults));
    faults.arm(crate::fault::Boundary::DirSync);
    assert!(matches!(
        persistence.checkpoint(&BytesSpec, third, EffectSnapshotLimits::default()),
        Err(EffectError::Injected(_))
    ));
    drop(persistence);

    // Directory sync was armed after the rename. Either the new pointer is
    // visible or the old one survived the crash; both pair with the intact
    // effect journal and replay to the same state.
    let (persistence, _) = JournalEffectPersistence::<BytesSpec, BytesCodec>::open_streaming(
        &path, BytesCodec, limits,
    )
    .expect("reopen new pointer");
    assert_eq!(
        persistence
            .replay_with_limits(&BytesSpec, limits)
            .expect("replay new")
            .len(),
        2
    );

    let mut snapshot_name = path.file_name().expect("journal name").to_os_string();
    snapshot_name.push(".snapshot");
    let snapshot_path = path.parent().expect("journal parent").join(snapshot_name);
    let mut pointer_name = path.file_name().expect("journal name").to_os_string();
    pointer_name.push(".snapshot.pointer");
    let pointer_path = path.parent().expect("journal parent").join(pointer_name);
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(snapshot_path);
    let _ = std::fs::remove_file(pointer_path);
}

#[test]
fn compaction_faults_before_and_after_replace_leave_a_recoverable_generation() {
    let path = temp_path("checkpoint-compaction-faults");
    let limits = crate::journal::JournalLimits {
        max_frames: 16,
        max_bytes: 1024 * 1024,
    };
    let intent: Arc<[u8]> = Arc::from(&b"compaction-fault"[..]);
    let key = effect_key(&intent);
    let (mut persistence, _) =
        JournalEffectPersistence::<BytesSpec, BytesCodec>::open(&path, BytesCodec).expect("open");
    persistence.prepared(key, &intent).expect("prepared");
    let recovered = persistence.replay(&BytesSpec).expect("replay");
    let snapshot = persistence
        .checkpoint(&BytesSpec, recovered, EffectSnapshotLimits::default())
        .expect("checkpoint");
    let faults = Arc::new(crate::fault::Faults::default());
    persistence = persistence.with_faults(Arc::clone(&faults));

    for boundary in [
        crate::fault::Boundary::TempCreate,
        crate::fault::Boundary::TempWrite,
        crate::fault::Boundary::FileSync,
        crate::fault::Boundary::Rename,
        crate::fault::Boundary::DirSync,
    ] {
        faults.arm(boundary);
        assert!(matches!(
            persistence.compact_journal(snapshot, limits),
            Err(EffectError::Injected(_))
        ));
        assert_eq!(
            persistence.replay(&BytesSpec).expect("live replay").len(),
            1
        );
    }
    drop(persistence);

    let (persistence, _) = JournalEffectPersistence::<BytesSpec, BytesCodec>::open_streaming(
        &path, BytesCodec, limits,
    )
    .expect("reopen");
    assert_eq!(persistence.replay(&BytesSpec).expect("replay").len(), 1);

    let mut snapshot_name = path.file_name().expect("journal name").to_os_string();
    snapshot_name.push(".snapshot");
    let snapshot_path = path.parent().expect("journal parent").join(snapshot_name);
    let mut pointer_name = path.file_name().expect("journal name").to_os_string();
    pointer_name.push(".snapshot.pointer");
    let pointer_path = path.parent().expect("journal parent").join(pointer_name);
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(snapshot_path);
    let _ = std::fs::remove_file(pointer_path);
}
