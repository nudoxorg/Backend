//! One ordered, bounded durable-state writer. Only adjacent ordinary saves
//! coalesce; a first-send barrier is immutable and acknowledged after save.
//! No filesystem work or worker join belongs to the UI thread.

use super::wake::{WakeReceiver, WakeSender, wake_channel};
use crate::model::{PersistedDesktopState, PersistentState};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

pub(crate) const MAX_INDEX_PREFLIGHTS: usize = 16;
const CAPACITY: usize = MAX_INDEX_PREFLIGHTS;
static NEXT_WRITER: AtomicU64 = AtomicU64::new(1);

/// Local write ordering only; never an owner or operation authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct WriteRevision {
    writer: u64,
    sequence: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct WriteAck {
    pub(crate) revision: WriteRevision,
    pub(crate) state: Arc<PersistedDesktopState>,
}
#[derive(Clone, Debug)]
pub(crate) struct WriteFailure {
    pub(crate) message: Arc<str>,
}
impl WriteFailure {
    fn new(message: impl std::fmt::Display) -> Self {
        Self {
            message: message
                .to_string()
                .chars()
                .filter(|c| !c.is_control())
                .take(240)
                .collect::<String>()
                .into(),
        }
    }
}
pub(crate) type WriteResult = Result<WriteAck, WriteFailure>;
pub(crate) type WriteReceiver = async_channel::Receiver<WriteResult>;

struct Job {
    revision: WriteRevision,
    state: Arc<PersistedDesktopState>,
    barrier: Option<async_channel::Sender<WriteResult>>,
}
struct Inner {
    next: u64,
    jobs: VecDeque<Job>,
    desired: Option<(WriteRevision, Arc<PersistedDesktopState>)>,
    /// One latest completion: obsolete successes cannot clear a later error.
    outcome: Option<(WriteRevision, Result<(), WriteFailure>)>,
    closing: bool,
    finished: bool,
    finish: Option<async_channel::Sender<WriteResult>>,
}
struct Shared {
    id: u64,
    inner: Mutex<Inner>,
    changed: Condvar,
    wake: WakeSender,
}

pub(crate) struct PersistenceWriter {
    shared: Arc<Shared>,
    path: std::path::PathBuf,
    wake: Option<WakeReceiver>,
}

impl PersistenceWriter {
    pub(crate) fn start(store: PersistentState) -> Result<Self, WriteFailure> {
        let path = store.path().to_path_buf();
        Self::start_with(path, move |state| {
            store.save(state).map_err(WriteFailure::new)
        })
    }

    fn start_with(
        path: std::path::PathBuf,
        save: impl FnMut(&PersistedDesktopState) -> Result<(), WriteFailure> + Send + 'static,
    ) -> Result<Self, WriteFailure> {
        let (wake, receiver) = wake_channel();
        let shared = Arc::new(Shared {
            id: NEXT_WRITER
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                    next.checked_add(1)
                })
                .map_err(|_| {
                    WriteFailure::new(
                        "The local state writer identity is exhausted. Nothing was sent.",
                    )
                })?,
            inner: Mutex::new(Inner {
                next: 1,
                jobs: VecDeque::new(),
                desired: None,
                outcome: None,
                closing: false,
                finished: false,
                finish: None,
            }),
            changed: Condvar::new(),
            wake,
        });
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("nudox-state-writer".into())
            .spawn(move || run(worker, save))
            .map_err(WriteFailure::new)?;
        Ok(Self {
            shared,
            path,
            wake: Some(receiver),
        })
    }

    /// Only deterministic tests may replace synchronized filesystem saving.
    #[cfg(test)]
    pub(crate) fn testing(
        path: std::path::PathBuf,
        save: impl FnMut(&PersistedDesktopState) -> Result<(), WriteFailure> + Send + 'static,
    ) -> Result<Self, WriteFailure> {
        Self::start_with(path, save)
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }
    pub(crate) fn accepts(&self, revision: WriteRevision) -> bool {
        revision.writer == self.shared.id
    }
    pub(crate) fn take_wake(&mut self) -> Option<WakeReceiver> {
        self.wake.take()
    }
    pub(crate) fn take_outcome(&self) -> Option<(WriteRevision, Result<(), WriteFailure>)> {
        self.shared
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .outcome
            .take()
    }

    /// One slot is reserved for the latest ordinary state. No preference
    /// change can be dropped merely because mutation barriers fill the lane.
    pub(crate) fn ordinary(&self, state: PersistedDesktopState) -> Result<bool, WriteFailure> {
        let mut inner = self
            .shared
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if inner.closing {
            return Err(WriteFailure::new(
                "The local state writer is closing. Previous saved state remains available.",
            ));
        }
        if inner
            .desired
            .as_ref()
            .is_some_and(|(_, previous)| **previous == state)
        {
            return Ok(false);
        }
        let revision = WriteRevision {
            writer: self.shared.id,
            sequence: inner.next,
        };
        inner.next = inner.next.checked_add(1).ok_or_else(|| {
            WriteFailure::new("The local state write revision is exhausted. Nothing was sent.")
        })?;
        let state = Arc::new(state);
        inner.desired = Some((revision, state.clone()));
        let job = Job {
            revision,
            state,
            barrier: None,
        };
        if inner.jobs.back().is_some_and(|job| job.barrier.is_none()) {
            let _ = inner.jobs.pop_back();
        }
        debug_assert!(
            inner.jobs.len() < CAPACITY,
            "barriers reserve the ordinary slot"
        );
        inner.jobs.push_back(job);
        drop(inner);
        self.shared.changed.notify_one();
        Ok(true)
    }

    /// Reserve capacity before constructing the immutable barrier payload.
    pub(crate) fn barrier(
        &self,
        state: impl FnOnce() -> PersistedDesktopState,
    ) -> Result<(WriteRevision, WriteReceiver), WriteFailure> {
        let mut inner = self
            .shared
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if inner.closing || inner.jobs.len() >= CAPACITY - 1 {
            return Err(WriteFailure::new(
                "The local state writer cannot accept another index request yet. Nothing was sent.",
            ));
        }
        let revision = WriteRevision {
            writer: self.shared.id,
            sequence: inner.next,
        };
        inner.next = inner.next.checked_add(1).ok_or_else(|| {
            WriteFailure::new("The local state write revision is exhausted. Nothing was sent.")
        })?;
        let state = Arc::new(state());
        let (sent, received) = async_channel::bounded(1);
        inner.desired = Some((revision, state.clone()));
        inner.jobs.push_back(Job {
            revision,
            state,
            barrier: Some(sent),
        });
        drop(inner);
        self.shared.changed.notify_one();
        Ok((revision, received))
    }

    /// A close checkpoint uses the reserved ordinary slot, acknowledges this
    /// exact state, and leaves the lane open if the person cancels closing.
    pub(crate) fn checkpoint(&self, state: PersistedDesktopState) -> Result<WriteReceiver, WriteFailure> {
        let mut inner = self.shared.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if inner.closing { return Err(WriteFailure::new("The local state writer is already closing.")); }
        if inner.jobs.back().is_some_and(|job| job.barrier.is_none()) { inner.jobs.pop_back(); }
        if inner.jobs.len() >= CAPACITY {
            return Err(WriteFailure::new("The local writer is still finishing a previous checkpoint. Try again after it settles."));
        }
        let revision = WriteRevision { writer: self.shared.id, sequence: inner.next };
        inner.next = inner.next.checked_add(1).ok_or_else(|| WriteFailure::new("The local write revision is exhausted."))?;
        let state = Arc::new(state);
        let (sent, received) = async_channel::bounded(1);
        inner.desired = Some((revision, state.clone()));
        inner.jobs.push_back(Job { revision, state, barrier: Some(sent) });
        drop(inner);
        self.shared.changed.notify_one();
        Ok(received)
    }

    /// Quit drains the bounded lane, including the latest ordinary state. A
    /// failed last write is retried once, never an unbounded background loop.
    pub(crate) fn finish(&self) -> WriteReceiver {
        let (sent, received) = async_channel::bounded(1);
        let mut inner = self
            .shared
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if inner.finished || inner.finish.is_some() {
            let _ = sent.try_send(Err(WriteFailure::new(
                "The local state writer has already closed.",
            )));
        } else {
            inner.closing = true;
            inner.finish = Some(sent);
        }
        drop(inner);
        self.shared.changed.notify_one();
        received
    }
}

impl Drop for PersistenceWriter {
    fn drop(&mut self) {
        self.shared
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closing = true;
        self.shared.changed.notify_one(); // the worker is detached; never join GPUI
    }
}

fn run(
    shared: Arc<Shared>,
    mut save: impl FnMut(&PersistedDesktopState) -> Result<(), WriteFailure>,
) {
    let mut saved: Option<Arc<PersistedDesktopState>> = None;
    loop {
        let job = {
            let mut inner = shared.inner.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if let Some(job) = inner.jobs.pop_front() {
                    break Some(job);
                }
                if inner.closing {
                    break None;
                }
                inner = shared
                    .changed
                    .wait(inner)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let Some(job) = job else {
            break;
        };
        let result = save_state(&mut save, &mut saved, &job.state);
        if let Some(reply) = job.barrier {
            let _ = reply.try_send(result.clone().map(|()| WriteAck {
                revision: job.revision,
                state: job.state.clone(),
            }));
        }
        shared
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .outcome = Some((job.revision, result));
        shared.wake.wake();
    }
    let (desired, finish) = {
        let mut inner = shared.inner.lock().unwrap_or_else(PoisonError::into_inner);
        (inner.desired.clone(), inner.finish.take())
    };
    if let Some(reply) = finish {
        let result = match desired {
            Some((revision, state)) => {
                save_state(&mut save, &mut saved, &state).map(|()| WriteAck { revision, state })
            }
            None => Ok(WriteAck {
                revision: WriteRevision {
                    writer: shared.id,
                    sequence: 0,
                },
                state: Arc::new(PersistedDesktopState::default()),
            }),
        };
        let _ = reply.try_send(result);
    }
    shared
        .inner
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .finished = true;
    shared.wake.close();
}

fn save_state(
    save: &mut impl FnMut(&PersistedDesktopState) -> Result<(), WriteFailure>,
    saved: &mut Option<Arc<PersistedDesktopState>>,
    state: &Arc<PersistedDesktopState>,
) -> Result<(), WriteFailure> {
    if saved.as_ref().is_some_and(|previous| previous == state) {
        return Ok(());
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| save(state))).unwrap_or_else(
        |_| {
            Err(WriteFailure::new(
                "The local state writer stopped before saving this change.",
            ))
        },
    )?;
    *saved = Some(state.clone());
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    fn state(width: u32) -> PersistedDesktopState {
        PersistedDesktopState {
            window: Some(crate::model::persistence::PersistedWindow { width, height: 500 }),
            ..PersistedDesktopState::default()
        }
    }
    fn receive(receiver: &WriteReceiver) -> WriteResult {
        crate::runtime::wait::until_some("bounded state acknowledgment", || {
            receiver.try_recv().ok()
        })
    }
    fn blocked(
        trace: Arc<Mutex<Vec<u32>>>,
    ) -> (PersistenceWriter, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (entered, waiting) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let mut first = true;
        let writer = PersistenceWriter::testing("/unused-test-state".into(), move |state| {
            if std::mem::take(&mut first) {
                entered.send(()).expect("write started");
                released.recv().expect("release synchronization");
            }
            trace
                .lock()
                .expect("trace")
                .push(state.window.expect("window").width);
            Ok(())
        })
        .expect("writer");
        (writer, waiting, release)
    }

    #[test]
    fn cancelling_close_checkpoint_keeps_writer_open_and_new_edits_follow_the_exact_ack() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let (writer, entered, release) = blocked(trace.clone());
        let checkpoint = writer.checkpoint(state(10)).expect("checkpoint admitted");
        entered.recv_timeout(std::time::Duration::from_secs(1)).expect("checkpoint writing");
        assert!(checkpoint.try_recv().is_err(), "admission is not synchronization");
        // Cancelling the UI wait never cancels an admitted durable write.
        drop(checkpoint);
        writer.ordinary(state(20)).expect("new edit after cancelling close");
        let current = writer.checkpoint(state(30)).expect("new close checkpoint");
        release.send(()).expect("release first write");
        assert_eq!(receive(&current).expect("new exact checkpoint").state.window.expect("window").width, 30);
        receive(&writer.finish()).expect("irreversible finish only after checkpoint");
        assert_eq!(*trace.lock().expect("trace"), [10, 30]);
    }

    #[test]
    fn unchanged_durable_state_does_not_repeat_disk_writes() {
        let saves = Arc::new(AtomicUsize::new(0));
        let recorded = saves.clone();
        let writer = PersistenceWriter::testing("/unused-test-state".into(), move |_| {
            recorded.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("writer");
        let (revision, acknowledgment) = writer.barrier(|| state(600)).expect("barrier");
        let ack = receive(&acknowledgment).expect("synchronized save");
        assert_eq!(ack.revision, revision);
        for _ in 0..100 {
            assert!(
                !writer
                    .ordinary(state(600))
                    .expect("unchanged ordinary snapshot")
            );
        }
        receive(&writer.finish()).expect("finish skips already synchronized state");
        assert_eq!(saves.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ordinary_coalescing_preserves_barrier_order_and_cannot_ack_before_synchronization() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let (writer, entered, release) = blocked(trace.clone());
        let (_, first) = writer.barrier(|| state(1)).expect("first barrier");
        entered
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("write started");
        assert!(
            first.try_recv().is_err(),
            "an in-progress fsync supplies no acknowledgment"
        );
        writer.ordinary(state(2)).expect("pre-barrier preference");
        let (_, second) = writer
            .barrier(|| state(3))
            .expect("second immutable barrier");
        writer.ordinary(state(4)).expect("later preference");
        writer.ordinary(state(5)).expect("coalesced preference");
        release.send(()).expect("release");
        assert_eq!(
            receive(&first)
                .expect("first")
                .state
                .window
                .expect("window")
                .width,
            1
        );
        assert_eq!(
            receive(&second)
                .expect("second")
                .state
                .window
                .expect("window")
                .width,
            3
        );
        receive(&writer.finish()).expect("drained latest");
        assert_eq!(*trace.lock().expect("trace"), [1, 2, 3, 5]);
    }

    #[test]
    fn a_full_barrier_lane_reserves_the_latest_preference_slot_before_materialization() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let (writer, entered, release) = blocked(trace.clone());
        let _ = writer.barrier(|| state(1)).expect("active barrier");
        entered
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("active write");
        for value in 2..=CAPACITY as u32 {
            let _ = writer
                .barrier(|| state(value))
                .expect("bounded barrier capacity");
        }
        writer.ordinary(state(90)).expect("reserved ordinary slot");
        writer
            .ordinary(state(91))
            .expect("latest ordinary value coalesces");
        let materialized = AtomicUsize::new(0);
        assert!(
            writer
                .barrier(|| {
                    materialized.fetch_add(1, Ordering::SeqCst);
                    state(99)
                })
                .is_err()
        );
        assert_eq!(materialized.load(Ordering::SeqCst), 0);
        assert_eq!(
            writer.shared.inner.lock().expect("queue").jobs.len(),
            CAPACITY
        );
        release.send(()).expect("release");
        receive(&writer.finish()).expect("bounded drain");
        assert_eq!(trace.lock().expect("trace").last(), Some(&91));
    }

    #[test]
    fn failed_save_never_acknowledges_success_and_quit_retries_only_once() {
        let saves = Arc::new(AtomicUsize::new(0));
        let recorded = saves.clone();
        let writer = PersistenceWriter::testing("/unused-test-state".into(), move |_| {
            recorded.fetch_add(1, Ordering::SeqCst);
            Err(WriteFailure::new("storage unavailable"))
        })
        .expect("writer");
        let (_, acknowledgment) = writer.barrier(|| state(1)).expect("barrier");
        assert!(receive(&acknowledgment).is_err());
        for _ in 0..100 {
            assert!(
                !writer
                    .ordinary(state(1))
                    .expect("failed unchanged state is not a retry loop")
            );
        }
        assert!(receive(&writer.finish()).is_err());
        assert_eq!(saves.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn exhausted_write_revisions_fail_closed_without_reusing_an_acknowledgment() {
        let writer =
            PersistenceWriter::testing("/unused-test-state".into(), |_| Ok(())).expect("writer");
        writer.shared.inner.lock().expect("revision").next = u64::MAX;
        assert!(writer.barrier(|| state(1)).is_err());
        assert!(writer.ordinary(state(2)).is_err());
        assert!(writer.shared.inner.lock().expect("queue").jobs.is_empty());
        receive(&writer.finish()).expect("empty lane closes cleanly");
    }

    #[test]
    fn a_panicking_storage_backend_is_a_failed_barrier_and_preferences_remain_admissible() {
        let mut first = true;
        let writer = PersistenceWriter::testing("/unused-test-state".into(), move |_| {
            assert!(!std::mem::take(&mut first), "the storage backend panicked");
            Ok(())
        })
        .expect("writer");
        let (_, acknowledgment) = writer.barrier(|| state(1)).expect("barrier");
        assert!(receive(&acknowledgment).is_err());
        assert!(
            writer
                .ordinary(state(2))
                .expect("ordinary changes still admitted")
        );
        assert_eq!(
            receive(&writer.finish())
                .expect("recovered save")
                .state
                .window
                .expect("window")
                .width,
            2
        );
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod disk_tests {
    use super::*;
    use crate::core::{LocalProjectId, VersionedRoot};
    use crate::model::{AppSnapshot, ProjectPhase};
    use crate::navigation::{Intent, RequestId};

    fn fixture() -> (
        std::path::PathBuf,
        PersistentState,
        PersistedDesktopState,
        PersistedDesktopState,
    ) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "nudox-durable-writer-{}-{nonce}",
            std::process::id()
        ));
        crate::host::private_dir(&root).expect("private fixture");
        let project = LocalProjectId::from_path(&root).expect("identity");
        let key = VersionedRoot::synthetic(backend_library::view_state_root(&[]), 1);
        let snapshot = crate::navigation::reduce(
            &AppSnapshot::empty(key),
            Intent::AddProject {
                project: project.clone(),
            },
        )
        .snapshot;
        let operation = crate::model::index_operation::tests::claim(&project, 7);
        let snapshot = crate::navigation::reduce(
            &snapshot,
            Intent::IndexProject {
                project,
                operation,
                basis: key,
                request: RequestId::new(1),
            },
        )
        .snapshot;
        let before = PersistentState::project(&snapshot);
        let mut workspace = snapshot.workspace().clone();
        let mut rows = workspace.projects.to_vec();
        rows[0].phase = ProjectPhase::Ready;
        rows[0].request = None;
        let claim = rows[0].operation.as_mut().expect("saved claim");
        claim.observation = Some(crate::model::index_operation::tests::published(claim));
        workspace.projects = rows.into();
        let terminal = PersistentState::project(&snapshot.with_workspace(workspace));
        (
            root.clone(),
            PersistentState::at(root.join("desktop.json")),
            before,
            terminal,
        )
    }
    fn receive(receiver: &WriteReceiver) -> WriteResult {
        crate::runtime::wait::until_some("state writer disk receipt", || receiver.try_recv().ok())
    }

    #[test]
    fn terminal_evidence_survives_quit_and_cold_reload_with_the_exact_claim() {
        let (root, store, before, terminal) = fixture();
        let writer = PersistenceWriter::start(store.clone()).expect("writer");
        let (_, acknowledged) = writer
            .barrier(|| before.clone())
            .expect("exact claim barrier");
        receive(&acknowledged).expect("claim synchronized");
        writer
            .ordinary(terminal.clone())
            .expect("terminal evidence");
        receive(&writer.finish()).expect("quit synchronized terminal state");
        assert_eq!(store.load().expect("cold state"), terminal);
        let cold = store.cold_workspace(&store.load().expect("cold state"));
        assert_eq!(cold.projects[0].phase, ProjectPhase::Ready);
        assert_eq!(cold.projects[0].operation, terminal.shelf[0].operation);
        std::fs::remove_dir_all(root).expect("fixture removed");
    }

    #[test]
    fn a_terminal_save_failure_preserves_the_synchronized_claim_for_cold_reconciliation() {
        let (root, store, before, terminal) = fixture();
        let saving = store.clone();
        let writer = PersistenceWriter::testing(store.path().to_path_buf(), move |state| {
            if state
                .shelf
                .iter()
                .any(|row| row.phase == crate::model::PersistedProjectPhase::Ready)
            {
                Err(WriteFailure::new("terminal storage failure"))
            } else {
                saving.save(state).map_err(WriteFailure::new)
            }
        })
        .expect("writer");
        let (_, acknowledged) = writer
            .barrier(|| before.clone())
            .expect("exact claim barrier");
        receive(&acknowledged).expect("claim synchronized");
        writer.ordinary(terminal).expect("terminal queued");
        assert!(receive(&writer.finish()).is_err());
        let retained = store.load().expect("previous saved claim remains readable");
        assert_eq!(retained, before);
        let cold = store.cold_workspace(&retained);
        assert_eq!(cold.projects[0].phase, ProjectPhase::Unconfirmed);
        assert_eq!(cold.projects[0].operation, before.shelf[0].operation);
        assert!(
            cold.projects[0]
                .operation
                .as_ref()
                .expect("saved exact key")
                .needs_observation()
        );
        std::fs::remove_dir_all(root).expect("fixture removed");
    }
}
