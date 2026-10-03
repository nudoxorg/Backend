//! Controlled synchronization tests; the actor records only mutation starts.
//! These prove save ordering/cancellation, not live compiler publication.
#![allow(clippy::expect_used)]
use super::*;
use crate::core::{LocalProjectId, VersionedRoot};
use crate::model::{ProjectPhase, ServiceMode};
use crate::runtime::owner::{OwnerGate, OwnerState};
use crate::runtime::{EngineActor, EngineClient, EngineDto, EngineFault, EngineRequest};
use gpui::TestAppContext;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, PoisonError, mpsc};

struct HeldMutation {
    store: PersistentState,
    sent: Arc<AtomicUsize>,
    release: Arc<(Mutex<bool>, Condvar)>,
}
impl EngineClient for HeldMutation {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        if let EngineRequest::IndexProject {
            project, operation, ..
        } = request
        {
            let disk = self
                .store
                .load()
                .expect("state synchronized before transport starts");
            assert!(
                disk.shelf
                    .iter()
                    .any(|row| row.operation.as_ref() == Some(operation)),
                "every first mutation owns an exact synchronized claim"
            );
            self.sent.fetch_add(1, Ordering::SeqCst);
            let (lock, changed) = &*self.release;
            let mut released = lock.lock().unwrap_or_else(PoisonError::into_inner);
            while !*released {
                released = changed
                    .wait(released)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            return Err(EngineFault::IndexUnconfirmed {
                project: project.clone(),
            });
        }
        Err(EngineFault::Cancelled)
    }
}
struct Rig {
    root: Entity<UiRootEntity>,
    store: Entity<DataStore>,
    gate: OwnerGate,
    project: LocalProjectId,
    persistence: PersistentState,
    directory: PathBuf,
    finished: std::cell::Cell<bool>,
    release_save: mpsc::Sender<()>,
    release_mutation: Arc<(Mutex<bool>, Condvar)>,
    sent: Arc<AtomicUsize>,
    writes: Arc<Mutex<Vec<crate::model::PersistedDesktopState>>>,
}
impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.release_save.send(());
        let (lock, changed) = &*self.release_mutation;
        *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
        changed.notify_all();
        if self.finished.get() {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
}
impl Rig {
    fn new(cx: &mut TestAppContext) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "nudox-async-preflight-{}-{nonce}",
            std::process::id()
        ));
        crate::host::private_dir(&directory).expect("private fixture");
        let project = LocalProjectId::from_path(&directory).expect("project identity");
        let key = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("async-save".into(), "one".into())]),
            1,
        );
        let snapshot = crate::navigation::reduce(
            &AppSnapshot::empty(key),
            Intent::AddProject {
                project: project.clone(),
            },
        )
        .snapshot;
        let persistence = PersistentState::at(directory.join("desktop.json"));
        let gate = OwnerGate::ready(key, ServiceMode::Embedded);
        let store = cx.update(|cx| {
            DataStore::install_with_owner(
                cx,
                Arc::new(snapshot.clone()),
                None,
                Some(gate.clone()),
                None,
            )
        });
        let sent = Arc::new(AtomicUsize::new(0));
        let release_mutation = Arc::new((Mutex::new(false), Condvar::new()));
        let actor = EngineActor::start(
            HeldMutation {
                store: persistence.clone(),
                sent: sent.clone(),
                release: release_mutation.clone(),
            },
            4,
        )
        .expect("actor");
        let (started, entered) = mpsc::channel();
        let (release_save, released) = mpsc::channel();
        let saving = persistence.clone();
        let writes = Arc::new(Mutex::new(Vec::new()));
        let trace = writes.clone();
        let mut first = true;
        let writer = PersistenceWriter::testing(persistence.path().to_path_buf(), move |state| {
            if std::mem::take(&mut first) {
                let _ = started.send(());
                released.recv().expect("release first synchronization");
            }
            saving.save(state).map_err(|error| WriteFailure {
                message: error.to_string().into(),
            })?;
            trace.lock().expect("writes").push(state.clone());
            Ok(())
        })
        .expect("writer");
        let attached = store.clone();
        let persistent = persistence.clone();
        let root = cx.update(|cx| {
            cx.new(|_| {
                let mut root =
                    UiRootEntity::new(DesktopRuntime::new(snapshot, actor), Some(persistent));
                root.pending.clear();
                root.store = Some(attached);
                root.published = Some(root.snapshot());
                root.persistence_writer = Some(writer);
                root
            })
        });
        root.update(cx, |root, cx| {
            root.schedule_pending_indexes(cx);
            root.flush_pending(cx);
        });
        entered
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("save started beside GUI");
        Self {
            root,
            store,
            gate,
            project,
            persistence,
            directory,
            finished: std::cell::Cell::new(false),
            release_save,
            release_mutation,
            sent,
            writes,
        }
    }
    fn release(&self) {
        self.release_save.send(()).expect("release save");
    }
    fn settle(&self, cx: &mut TestAppContext) {
        crate::runtime::wait::until("preflight continuation settled", || {
            cx.run_until_parked();
            self.root
                .read_with(cx, |root, _| root.index_preflights.is_empty())
        });
    }
    fn finish(&self, cx: &mut TestAppContext) {
        let finished = self
            .root
            .update(cx, |root, cx| root.finish_persistence(cx))
            .expect("writer");
        crate::runtime::wait::until_some("ordered quit drain", || finished.try_recv().ok())
            .expect("synchronized quit state");
        self.finished.set(true);
    }
}

#[gpui::test]
fn first_send_waits_for_sync_and_preference_saves_preserve_the_pending_claim(
    cx: &mut TestAppContext,
) {
    let rig = Rig::new(cx);
    assert_eq!(rig.sent.load(Ordering::SeqCst), 0);
    rig.root.update(cx, |root, cx| {
        root.dispatch(Intent::SetCacheEnabled(false), cx)
    });
    assert_eq!(
        rig.sent.load(Ordering::SeqCst),
        0,
        "GUI preferences remain usable while synchronization is blocked"
    );
    rig.release();
    rig.settle(cx);
    crate::runtime::wait::until("one mutation admitted after synchronization", || {
        rig.sent.load(Ordering::SeqCst) == 1
    });
    rig.finish(cx);
    let saved = rig.persistence.load().expect("durable state");
    assert!(!saved.cache_enabled);
    assert!(saved.shelf[0].operation.is_some());
    assert!(
        rig.writes
            .lock()
            .expect("writes")
            .iter()
            .all(|state| state.shelf[0].operation == saved.shelf[0].operation),
        "ordinary preferences cannot erase the saved claim across the barrier"
    );
}

#[gpui::test]
fn cancellation_during_sync_never_sends_and_cold_state_keeps_the_cancellation(
    cx: &mut TestAppContext,
) {
    let rig = Rig::new(cx);
    rig.root.update(cx, |root, cx| {
        root.dispatch(Intent::CancelIndex(rig.project.clone()), cx)
    });
    rig.release();
    rig.settle(cx);
    rig.finish(cx);
    assert_eq!(rig.sent.load(Ordering::SeqCst), 0);
    let saved = rig.persistence.load().expect("cancelled durable state");
    assert_eq!(
        saved.shelf[0].phase,
        crate::model::PersistedProjectPhase::Cancelled
    );
    assert!(saved.shelf[0].operation.is_none());
    assert_eq!(
        rig.persistence.cold_workspace(&saved).projects[0].phase,
        ProjectPhase::Cancelled
    );
}

#[gpui::test]
fn owner_replacement_during_sync_cannot_consume_the_previous_acknowledgment(
    cx: &mut TestAppContext,
) {
    let rig = Rig::new(cx);
    let key = rig.root.read_with(cx, |root, _| root.snapshot().key());
    rig.gate.publish(OwnerState::Starting);
    rig.gate.publish(OwnerState::Ready {
        key,
        mode: ServiceMode::Embedded,
    });
    rig.store.update(cx, |store, cx| store.owner_ready(cx));
    rig.release();
    rig.settle(cx);
    rig.finish(cx);
    assert_eq!(rig.sent.load(Ordering::SeqCst), 0);
    rig.root.read_with(cx, |root, _| {
        let snapshot = root.snapshot();
        let row = &snapshot.workspace().projects[0];
        assert_eq!(row.phase, ProjectPhase::Failed);
        assert!(row.operation.is_none());
        assert!(
            row.error
                .as_deref()
                .expect("local refusal")
                .contains("Nothing was sent")
        );
    });
}

#[gpui::test]
fn removal_during_sync_and_a_late_receipt_cannot_resurrect_the_row(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (revision, state) = rig.root.read_with(cx, |root, _| {
        let pending = root.index_preflights.get(&rig.project).expect("pending");
        let submitted =
            crate::navigation::reduce(&root.snapshot(), pending.intent.clone()).snapshot;
        (pending.revision, PersistentState::project(&submitted))
    });
    rig.root.update(cx, |root, cx| {
        root.dispatch(Intent::RemoveProject(rig.project.clone()), cx);
        root.complete_index_preflight(
            rig.project.clone(),
            revision,
            Ok(WriteAck {
                revision,
                state: Arc::new(state),
            }),
            cx,
        );
    });
    rig.release();
    rig.settle(cx);
    rig.finish(cx);
    assert_eq!(rig.sent.load(Ordering::SeqCst), 0);
    assert!(
        rig.persistence
            .load()
            .expect("removed durable shelf")
            .shelf
            .is_empty()
    );
    rig.root.read_with(cx, |root, _| {
        assert!(root.snapshot().workspace().projects.is_empty())
    });
}

#[gpui::test]
fn same_claim_with_an_obsolete_revision_cannot_send_or_clear_a_newer_save_error(
    cx: &mut TestAppContext,
) {
    let rig = Rig::new(cx);
    let (old, state) = rig.root.read_with(cx, |root, _| {
        let pending = root.index_preflights.get(&rig.project).expect("pending");
        let submitted =
            crate::navigation::reduce(&root.snapshot(), pending.intent.clone()).snapshot;
        (pending.revision, PersistentState::project(&submitted))
    });
    let (new, acknowledgment) = rig.root.update(cx, |root, cx| {
        let (new, ack) = root
            .persistence_writer
            .as_ref()
            .expect("writer")
            .barrier(|| state.clone())
            .expect("new revision of same claim");
        root.index_preflights
            .get_mut(&rig.project)
            .expect("pending")
            .revision = new;
        root.record_persistence_outcome(
            new,
            Err(WriteFailure {
                message: "newer save error".into(),
            }),
            cx,
        );
        root.complete_index_preflight(
            rig.project.clone(),
            old,
            Ok(WriteAck {
                revision: old,
                state: Arc::new(state.clone()),
            }),
            cx,
        );
        root.record_persistence_outcome(old, Ok(()), cx);
        assert!(
            root.snapshot()
                .workspace()
                .notes
                .iter()
                .any(|note| matches!(note, crate::model::Note::StateNotSaved { .. }))
        );
        (new, ack)
    });
    assert_eq!(rig.sent.load(Ordering::SeqCst), 0);
    rig.release();
    let saved = crate::runtime::wait::until_some("new exact write acknowledgment", || {
        acknowledgment.try_recv().ok()
    });
    rig.root.update(cx, |root, cx| {
        root.complete_index_preflight(rig.project.clone(), new, saved, cx)
    });
    crate::runtime::wait::until("new exact revision admitted once", || {
        rig.sent.load(Ordering::SeqCst) == 1
    });
    rig.finish(cx);
}
