//! Owner-local recovery hints, never durable acceptance or writer authority.
//!
//! One reader sleeps on a coalesced notification channel. Local transactions
//! and native filesystem events wake it; quiet owner polls do no SQL. External
//! notifications may be delayed, so a cached empty snapshot must never replace
//! the fresh prepared-state check immediately before admitting a queued writer.

use super::index_operation::{IndexOperationJournal, JournalError};
use backend_library::{Cursor, IndexOperationKey};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Pending {
    pub(super) first: Option<IndexOperationKey>,
    pub(super) prepared: bool,
    pub(super) first_payload: Option<[u8; 32]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Stamp {
    owner_epoch: [u8; 16],
    event_sequence: Option<u64>,
}

/// Equality only schedules a finite retry. The allocation identity lets an
/// explicit retry remain distinct after notification sequence exhaustion.
#[derive(Clone)]
pub(super) struct RetryToken {
    event_sequence: Option<u64>,
    explicit: Arc<()>,
}

impl PartialEq for RetryToken {
    fn eq(&self, other: &Self) -> bool {
        self.event_sequence == other.event_sequence && Arc::ptr_eq(&self.explicit, &other.explicit)
    }
}

impl Eq for RetryToken {}

enum Wake {
    Changed,
    Close,
}

/// Only the small wake counter/channel cross threads. The journal connection,
/// inventory and reconciliation residence each have exactly one owner.
#[derive(Clone)]
pub(super) struct Changed {
    sequence: Arc<AtomicU64>,
    watch_healthy: Arc<AtomicBool>,
    wake: mpsc::SyncSender<Wake>,
}

impl Changed {
    pub(super) fn invalidate(&self) {
        // Saturation is permanently unknown, never a wrapped reusable stamp.
        let _ = self
            .sequence
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |s| {
                Some(s.saturating_add(1))
            });
        let _ = self.wake.try_send(Wake::Changed);
    }

    fn sequence(&self) -> Option<u64> {
        let sequence = self.sequence.load(Ordering::Acquire);
        (sequence != u64::MAX).then_some(sequence)
    }
}

struct Observation {
    stamp: Stamp,
    pending: Result<Pending, JournalError>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Unknown {
    AwaitingChange,
    ReadFailed,
    WatchFailed,
    EventSequenceExhausted,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Readiness {
    Unknown(Unknown),
    Observed(Stamp, Pending),
    /// A finite fresh read is useful for positive recovery even when no
    /// reliable future change notifications are available. Never an absence
    /// certificate; another explicit refresh is required to update it.
    Degraded(Stamp, Pending),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RecoveryOutcome {
    Consumed,
    RetryableFailure,
}

struct Reconciliation {
    stamp: Stamp,
    pending: Pending,
    cursor: Cursor,
    retry: RetryToken,
    outcome: RecoveryOutcome,
}

pub(super) struct JournalReadiness {
    epoch: [u8; 16],
    changed: Changed,
    watcher: Option<RecommendedWatcher>,
    observations: Option<mpsc::Receiver<Observation>>,
    worker: Option<JoinHandle<()>>,
    selected: Readiness,
    reconciled: Option<Reconciliation>,
    explicit_retry: Arc<()>,
}

impl JournalReadiness {
    pub(super) fn start(path: PathBuf, epoch: [u8; 16]) -> Result<Self, String> {
        Self::start_with_watch(path, epoch, false)
    }

    fn start_with_watch(
        path: PathBuf,
        epoch: [u8; 16],
        fail_registration: bool,
    ) -> Result<Self, String> {
        let (wake, wakes) = mpsc::sync_channel(1);
        let (complete, observations) = mpsc::sync_channel(1);
        let changed = Changed {
            sequence: Arc::new(AtomicU64::new(0)),
            watch_healthy: Arc::new(AtomicBool::new(true)),
            wake,
        };
        let callback = changed.clone();
        let watched = path.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            match event {
                Ok(event)
                    if event.need_rescan()
                        || (!event.kind.is_access()
                            && event.paths.iter().any(|p| relevant(p, &watched))) =>
                {
                    callback.invalidate();
                }
                Err(_) => {
                    // A broken watcher cannot establish a quiet recovery hint.
                    callback.watch_healthy.store(false, Ordering::Release);
                    callback.invalidate();
                }
                Ok(_) => {}
            }
        });
        // Register before the initial read so a foreign commit cannot fall
        // between an empty read and watch registration.
        let watcher = if fail_registration {
            None
        } else {
            match watcher {
                Ok(mut watcher) => watcher
                    .watch(
                        path.parent().ok_or("journal has no directory")?,
                        RecursiveMode::NonRecursive,
                    )
                    .is_ok()
                    .then_some(watcher),
                Err(_) => None,
            }
        };
        if watcher.is_none() {
            changed.watch_healthy.store(false, Ordering::Release);
        }
        let worker_changed = changed.clone();
        let worker = std::thread::Builder::new()
            .name("locald-index-journal".to_owned())
            .spawn(move || {
                let mut journal = IndexOperationJournal::open(path.clone());
                while let Ok(Wake::Changed) = wakes.recv() {
                    let sequence = worker_changed.sequence();
                    // Failed opens are retried on an explicit change, never a
                    // hot timer. A failure receipt cannot be read as empty.
                    if journal.is_err() {
                        journal = IndexOperationJournal::open(path.clone());
                    }
                    let pending = match &mut journal {
                        Ok(journal) => journal.pending_snapshot(),
                        Err(error) => Err(JournalError::Database(error.to_string())),
                    };
                    let observation = Observation {
                        stamp: Stamp {
                            owner_epoch: epoch,
                            event_sequence: sequence,
                        },
                        pending,
                    };
                    if complete.send(observation).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| format!("start index journal reader: {error}"))?;
        let lane = Self {
            epoch,
            changed,
            watcher,
            observations: Some(observations),
            worker: Some(worker),
            selected: Readiness::Unknown(Unknown::AwaitingChange),
            reconciled: None,
            explicit_retry: Arc::new(()),
        };
        lane.changed.invalidate();
        Ok(lane)
    }

    pub(super) fn changed(&self) -> Changed {
        self.changed.clone()
    }

    pub(super) fn refresh(&mut self) {
        self.explicit_retry = Arc::new(());
        self.changed.invalidate();
    }

    pub(super) fn poll(&mut self) {
        // Drain at most one bounded receipt per owner turn; a noisy foreign
        // writer cannot starve health by replenishing this slot indefinitely.
        if let Some(observation) = self.observations.as_ref().and_then(|r| r.try_recv().ok()) {
            self.admit(observation);
        }
        if matches!(self.selected, Readiness::Observed(stamp, _) | Readiness::Degraded(stamp, _) if stamp.event_sequence != self.changed.sequence())
        {
            self.selected = Readiness::Unknown(if self.changed.sequence().is_none() {
                Unknown::EventSequenceExhausted
            } else if self.changed.watch_healthy.load(Ordering::Acquire) {
                Unknown::AwaitingChange
            } else {
                Unknown::WatchFailed
            });
        } else if self.changed.sequence().is_none()
            && !matches!(self.selected, Readiness::Degraded(_, _))
        {
            self.selected = Readiness::Unknown(Unknown::EventSequenceExhausted);
        } else if !self.changed.watch_healthy.load(Ordering::Acquire)
            && let Readiness::Observed(stamp, pending) = self.selected
        {
            self.selected = Readiness::Degraded(stamp, pending);
        } else if !self.changed.watch_healthy.load(Ordering::Acquire)
            && self.selected == Readiness::Unknown(Unknown::AwaitingChange)
        {
            self.selected = Readiness::Unknown(Unknown::WatchFailed);
        }
    }

    fn admit(&mut self, observation: Observation) {
        if observation.stamp.owner_epoch == self.epoch
            && observation.stamp.event_sequence == self.changed.sequence()
        {
            if observation.stamp.event_sequence.is_none() {
                // Exhausted notifications cannot fence a snapshot. A receipt
                // is still a positive recovery hint, consumed once, and can
                // wake a retained candidate without hot owner-loop reads.
                self.explicit_retry = Arc::new(());
                self.reconciled = None;
            }
            self.selected = match observation.pending {
                Ok(pending)
                    if observation.stamp.event_sequence.is_some()
                        && self.changed.watch_healthy.load(Ordering::Acquire) =>
                {
                    Readiness::Observed(observation.stamp, pending)
                }
                Ok(pending) => Readiness::Degraded(observation.stamp, pending),
                Err(_) => Readiness::Unknown(Unknown::ReadFailed),
            };
        }
    }

    /// Resolve each exact recovery hint once per selected workspace cursor.
    /// Persisted Unresolved is not a reason to retry SQL on every owner poll.
    pub(super) fn recovery(&mut self, cursor: Cursor) -> Option<IndexOperationKey> {
        let degraded = matches!(self.selected, Readiness::Degraded(_, _));
        let (Readiness::Observed(stamp, pending) | Readiness::Degraded(stamp, pending)) =
            self.selected
        else {
            return None;
        };
        if self.reconciled.as_ref().is_some_and(|previous| {
            previous.stamp.owner_epoch == stamp.owner_epoch
                && previous.pending == pending
                && previous.cursor == cursor
                && if previous.outcome == RecoveryOutcome::RetryableFailure {
                    previous.retry == self.retry_token()
                } else {
                    !degraded || previous.stamp.event_sequence == stamp.event_sequence
                }
        }) {
            return None;
        }
        self.reconciled = Some(Reconciliation {
            stamp,
            pending,
            cursor,
            retry: self.retry_token(),
            outcome: RecoveryOutcome::Consumed,
        });
        pending.first
    }

    pub(super) fn recovery_finished(
        &mut self,
        operation_key: IndexOperationKey,
        result: &Result<backend_library::IndexOperationObservation, JournalError>,
    ) {
        let Some(previous) = self
            .reconciled
            .as_mut()
            .filter(|previous| previous.pending.first == Some(operation_key))
        else {
            return;
        };
        previous.outcome = match result {
            Err(_) => RecoveryOutcome::RetryableFailure,
            Ok(backend_library::IndexOperationObservation::Known(status))
                if matches!(status.state, backend_library::IndexOperationState::Unresolved {
                    reason: backend_library::IndexOperationUnresolvedReason::ReceiptPersistenceFailed,
                    ..
                }) => RecoveryOutcome::RetryableFailure,
            _ => RecoveryOutcome::Consumed,
        };
    }

    #[cfg(test)]
    pub(super) fn prepared_hint(&self) -> bool {
        matches!(self.selected, Readiness::Observed(_, pending) | Readiness::Degraded(_, pending) if pending.prepared)
    }

    /// Scheduling token only; neither notification delivery nor its absence
    /// supplies durable writer authority.
    pub(super) fn retry_token(&self) -> RetryToken {
        RetryToken {
            event_sequence: self.changed.sequence(),
            explicit: Arc::clone(&self.explicit_retry),
        }
    }

    #[cfg(test)]
    pub(super) fn exhaust_notifications(&self) {
        self.changed.sequence.store(u64::MAX, Ordering::Release);
    }

    #[cfg(test)]
    pub(super) fn disconnect_reader(&mut self) {
        let selected = self.selected;
        self.close();
        self.selected = selected;
    }

    #[cfg(test)]
    pub(super) fn lose_notifications(&mut self) {
        self.watcher.take();
    }

    pub(super) fn close(&mut self) {
        self.watcher.take();
        // Release a possibly full completion slot before joining the worker.
        self.observations.take();
        if let Some(worker) = self.worker.take() {
            let _ = self.changed.wake.send(Wake::Close);
            let _ = worker.join();
        }
        self.selected = Readiness::Unknown(Unknown::Closed);
    }
}

impl Drop for JournalReadiness {
    fn drop(&mut self) {
        self.close();
    }
}

fn relevant(path: &Path, database: &Path) -> bool {
    path == database
        || path == database.parent().unwrap_or(database)
        || path
            .file_name()
            .zip(database.file_name())
            .is_some_and(|(event, name)| {
                let event = event.to_string_lossy();
                let name = name.to_string_lossy();
                // SHM read marks and lock traffic are not commit notifications.
                // Committed work changes the WAL or checkpointed database itself.
                event
                    .strip_prefix(name.as_ref())
                    .is_some_and(|tail| matches!(tail, "-wal" | "-journal"))
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_library::{
        CompileExecutionIntent, IndexOperationFailureReason, PackageReference, ProductText,
    };
    use std::time::{Duration, Instant};

    const FOREIGN_WRITER: &str = "BACKEND_JOURNAL_READINESS_FOREIGN_WRITER";

    #[test]
    fn independent_writer_child() {
        let Some(path) = std::env::var_os(FOREIGN_WRITER) else {
            return;
        };
        let mut writer = IndexOperationJournal::open(PathBuf::from(path)).expect("child journal");
        writer
            .accept(
                key(),
                PackageReference::parse("/workspace/docs").expect("package"),
                CompileExecutionIntent::Interactive,
            )
            .expect("independent durable acceptance");
    }

    fn key() -> IndexOperationKey {
        IndexOperationKey::from_bytes([7; 32]).expect("nonzero operation key")
    }

    fn wait_for(lane: &mut JournalReadiness, wanted: Pending) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            lane.poll();
            if matches!(lane.selected, Readiness::Observed(_, pending) | Readiness::Degraded(_, pending) if pending == wanted)
            {
                return;
            }
            let receipt = lane
                .observations
                .as_ref()
                .expect("open lane")
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("native change notification must lead to a snapshot");
            lane.admit(receipt);
        }
    }

    #[test]
    fn native_notifications_observe_foreign_commits_and_cold_prepared_recovery() {
        let scratch = tempfile::tempdir().expect("journal scratch");
        let path = scratch.path().join("workspace/operations.turso");
        let mut foreign = IndexOperationJournal::open(&path).expect("foreign writer");
        let mut lane = JournalReadiness::start(path.clone(), [1; 16]).expect("watch reader");
        wait_for(
            &mut lane,
            Pending {
                first: None,
                prepared: false,
                first_payload: None,
            },
        );
        let output = std::process::Command::new(std::env::current_exe().expect("test image"))
            .arg("journal_readiness::tests::independent_writer_child")
            .arg("--nocapture")
            .env(FOREIGN_WRITER, &path)
            .output()
            .expect("independent writer");
        assert!(
            output.status.success(),
            "child durable acceptance failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        // No local Changed handle is attached to the foreign connection.
        wait_for(
            &mut lane,
            foreign.pending_snapshot().expect("accepted snapshot"),
        );
        let cursor = Cursor::at(backend_library::view_state_root(&[]), 0);
        assert_eq!(lane.recovery(cursor), Some(key()));
        assert_eq!(
            lane.recovery(cursor),
            None,
            "Unresolved recovery is not a poll loop"
        );
        assert_eq!(
            lane.recovery(Cursor::at(cursor.root(), 1)),
            Some(key()),
            "a new workspace cursor permits exact recovery again"
        );
        foreign
            .prepare(key(), Some([4; 32]), [5; 32], 9)
            .expect("foreign prepared commit");
        wait_for(
            &mut lane,
            foreign.pending_snapshot().expect("prepared snapshot"),
        );
        lane.close();
        let mut cold = JournalReadiness::start(path, [2; 16]).expect("cold owner observer");
        wait_for(
            &mut cold,
            foreign.pending_snapshot().expect("cold prepared snapshot"),
        );
        assert_eq!(cold.recovery(cursor), Some(key()));
        foreign
            .failed(
                key(),
                IndexOperationFailureReason::WorkerFailed,
                ProductText::from_static("durable terminal"),
            )
            .expect("foreign terminal commit");
        wait_for(
            &mut cold,
            Pending {
                first: None,
                prepared: false,
                first_payload: None,
            },
        );
        assert!(!cold.prepared_hint());
        cold.close();
    }

    #[test]
    fn failed_registration_and_watch_error_allow_finite_explicit_refresh() {
        let scratch = tempfile::tempdir().expect("journal scratch");
        let path = scratch.path().join("workspace/operations.turso");
        let mut foreign = IndexOperationJournal::open(&path).expect("foreign journal");
        let mut lane = JournalReadiness::start_with_watch(path.clone(), [1; 16], true)
            .expect("degraded observer still starts");
        wait_for(
            &mut lane,
            foreign.pending_snapshot().expect("initial snapshot"),
        );
        assert!(matches!(lane.selected, Readiness::Degraded(_, _)));
        foreign
            .accept(
                key(),
                PackageReference::parse("/workspace/docs").expect("package"),
                CompileExecutionIntent::Interactive,
            )
            .expect("foreign accepted");
        lane.refresh();
        wait_for(
            &mut lane,
            foreign.pending_snapshot().expect("accepted snapshot"),
        );
        assert!(matches!(lane.selected, Readiness::Degraded(_, _)));
        let cursor = Cursor::at(backend_library::view_state_root(&[]), 0);
        assert_eq!(lane.recovery(cursor), Some(key()));
        assert_eq!(lane.recovery(cursor), None);
        lane.refresh();
        wait_for(
            &mut lane,
            foreign.pending_snapshot().expect("explicit retry snapshot"),
        );
        assert_eq!(
            lane.recovery(cursor),
            Some(key()),
            "degraded explicit refresh retries once even at the same cursor"
        );
        lane.close();
        let mut lane = JournalReadiness::start(path, [2; 16]).expect("registered observer");
        wait_for(
            &mut lane,
            foreign.pending_snapshot().expect("registered snapshot"),
        );
        // Simulate callback failure after a successful registration. A fresh
        // one-shot read still works without claiming restored watch coverage.
        lane.changed.watch_healthy.store(false, Ordering::Release);
        foreign
            .prepare(key(), Some([4; 32]), [5; 32], 9)
            .expect("prepared");
        lane.changed.watch_healthy.store(false, Ordering::Release);
        lane.changed.invalidate();
        wait_for(
            &mut lane,
            foreign.pending_snapshot().expect("prepared snapshot"),
        );
        assert!(lane.prepared_hint());
        foreign
            .failed(
                key(),
                IndexOperationFailureReason::WorkerFailed,
                ProductText::from_static("foreign terminal"),
            )
            .expect("terminal");
        lane.refresh();
        wait_for(
            &mut lane,
            foreign.pending_snapshot().expect("terminal snapshot"),
        );
        assert!(!lane.prepared_hint());
        assert!(
            !lane.changed.watch_healthy.load(Ordering::Acquire),
            "one-shot reads must not pretend notification coverage recovered"
        );
        lane.close();
    }

    #[test]
    fn exhausted_notifications_still_allow_finite_positive_recovery_and_retry() {
        let scratch = tempfile::tempdir().expect("journal scratch");
        let path = scratch.path().join("workspace/operations.turso");
        let mut foreign = IndexOperationJournal::open(&path).expect("foreign journal");
        foreign
            .accept(
                key(),
                PackageReference::parse("/workspace/docs").expect("package"),
                CompileExecutionIntent::Interactive,
            )
            .expect("foreign accepted");
        let mut lane =
            JournalReadiness::start_with_watch(path, [1; 16], true).expect("one-shot observer");
        lane.exhaust_notifications();
        let before = lane.retry_token();
        lane.refresh();
        assert!(before != lane.retry_token());
        let pending = foreign.pending_snapshot().expect("accepted snapshot");
        wait_for(&mut lane, pending);
        assert!(matches!(
            lane.selected,
            Readiness::Degraded(
                Stamp {
                    event_sequence: None,
                    ..
                },
                _
            )
        ));
        let cursor = Cursor::at(backend_library::view_state_root(&[]), 0);
        assert_eq!(lane.recovery(cursor), Some(key()));
        assert_eq!(lane.recovery(cursor), None);
        // An explicit retry stays distinct after saturation without wrapping
        // a sequence or introducing periodic database reads.
        let token = lane.retry_token();
        lane.refresh();
        assert!(token != lane.retry_token());
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut recovered = false;
        while Instant::now() < deadline {
            lane.poll();
            if lane.recovery(cursor) == Some(key()) {
                recovered = true;
                break;
            }
            std::thread::yield_now();
        }
        assert!(
            recovered,
            "saturated explicit refresh must deliver another finite receipt"
        );
        lane.close();
    }

    #[test]
    fn changed_epoch_overflow_and_watch_failure_are_unknown_not_empty() {
        let scratch = tempfile::tempdir().expect("journal scratch");
        let path = scratch.path().join("workspace/operations.turso");
        let _writer = IndexOperationJournal::open(&path).expect("journal");
        let mut lane = JournalReadiness::start(path, [1; 16]).expect("watch reader");
        wait_for(
            &mut lane,
            Pending {
                first: None,
                prepared: false,
                first_payload: None,
            },
        );
        lane.close();
        let sequence = lane.changed.sequence();
        let stamp = Stamp {
            owner_epoch: lane.epoch,
            event_sequence: sequence,
        };
        lane.selected = Readiness::Observed(
            stamp,
            Pending {
                first: None,
                prepared: false,
                first_payload: None,
            },
        );
        // A previous process cannot supply this owner's recovery hint.
        let stale = Observation {
            stamp: Stamp {
                owner_epoch: [9; 16],
                event_sequence: sequence,
            },
            pending: Ok(Pending {
                first: Some(key()),
                prepared: false,
                first_payload: Some([7; 32]),
            }),
        };
        lane.admit(stale);
        assert_eq!(
            lane.selected,
            Readiness::Observed(
                stamp,
                Pending {
                    first: None,
                    prepared: false,
                    first_payload: None
                }
            )
        );
        lane.admit(Observation {
            stamp,
            pending: Err(JournalError::Database("unavailable".to_owned())),
        });
        assert_eq!(lane.selected, Readiness::Unknown(Unknown::ReadFailed));
        lane.selected = Readiness::Observed(
            stamp,
            Pending {
                first: None,
                prepared: false,
                first_payload: None,
            },
        );
        lane.changed.invalidate();
        lane.poll();
        assert_eq!(lane.selected, Readiness::Unknown(Unknown::AwaitingChange));
        lane.changed.watch_healthy.store(false, Ordering::Release);
        lane.poll();
        assert_eq!(lane.selected, Readiness::Unknown(Unknown::WatchFailed));
        lane.changed.watch_healthy.store(true, Ordering::Release);
        lane.changed.sequence.store(u64::MAX, Ordering::Release);
        lane.changed.invalidate();
        lane.poll();
        assert_eq!(
            lane.selected,
            Readiness::Unknown(Unknown::EventSequenceExhausted)
        );
        lane.close();
    }

    #[test]
    fn coalesced_wakes_and_full_reply_slot_join_on_close() {
        let scratch = tempfile::tempdir().expect("journal scratch");
        let path = scratch.path().join("workspace/operations.turso");
        let _writer = IndexOperationJournal::open(&path).expect("journal");
        let mut lane = JournalReadiness::start(path, [1; 16]).expect("watch reader");
        // Thousands of events retain only one queued wake and one reply, even
        // if the reader is blocked sending the previous result at shutdown.
        for _ in 0..10000 {
            lane.refresh();
        }
        lane.close();
        assert!(lane.worker.is_none());
        assert_eq!(lane.selected, Readiness::Unknown(Unknown::Closed));
        // Deterministically fill the same bounded reply slot, then park a
        // sender on its second receipt. Production close must release it
        // before joining, independently of filesystem event scheduling.
        let (complete, receipts) = mpsc::sync_channel(1);
        let (entered, blocked) = mpsc::sync_channel(1);
        let (wake, _wakes) = mpsc::sync_channel(1);
        lane.changed.wake = wake;
        lane.observations = Some(receipts);
        lane.worker = Some(std::thread::spawn(move || {
            let receipt = || Observation {
                stamp: Stamp {
                    owner_epoch: [1; 16],
                    event_sequence: Some(1),
                },
                pending: Ok(Pending {
                    first: None,
                    prepared: false,
                    first_payload: None,
                }),
            };
            complete.send(receipt()).expect("fill reply slot");
            entered.send(()).expect("reply slot filled");
            assert!(
                complete.send(receipt()).is_err(),
                "close releases the blocked send"
            );
        }));
        blocked
            .recv_timeout(Duration::from_secs(2))
            .expect("reader reached full reply slot");
        lane.close();
        assert!(lane.worker.is_none());
    }
}
