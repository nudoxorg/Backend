//! Controlled synchronization tests; the actor records only mutation starts.
//! These prove save ordering/cancellation, not live compiler publication.
#![allow(clippy::expect_used)]
use super::*;
use crate::core::{LocalProjectId, VersionedRoot};
use crate::model::{ProjectPhase, ServiceMode};
use crate::runtime::owner::{OwnerGate, OwnerState};
use crate::runtime::{EngineActor, EngineClient, EngineDto, EngineFault, EngineRequest};
use gpui::TestAppContext;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, PoisonError, mpsc};

struct HeldMutation {
    store: PersistentState,
    sent: Arc<AtomicUsize>,
    release: Arc<(Mutex<bool>, Condvar)>,
    observed: Arc<Mutex<Vec<(crate::navigation::RequestId, crate::model::IndexOperationClaim, VersionedRoot)>>>,
    hold_root: Arc<AtomicBool>,
    root_entered: mpsc::Sender<()>,
}
impl EngineClient for HeldMutation {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        if matches!(request, EngineRequest::Root { .. }) && self.hold_root.load(Ordering::SeqCst) {
            let _ = self.root_entered.send(());
            let (lock, changed) = &*self.release;
            let mut released = lock.lock().unwrap_or_else(PoisonError::into_inner);
            while !*released {
                released = changed.wait(released).unwrap_or_else(PoisonError::into_inner);
            }
            return Err(EngineFault::Cancelled);
        }
        if let EngineRequest::IndexProject {
            project, operation, request, basis, ..
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
            self.observed.lock().expect("request lineage").push((*request, operation.clone(), *basis));
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
    observed: Arc<Mutex<Vec<(crate::navigation::RequestId, crate::model::IndexOperationClaim, VersionedRoot)>>>,
    hold_root: Arc<AtomicBool>,
    root_entered: mpsc::Receiver<()>,
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.gate.close();
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
        let view = crate::runtime::owner::publication_tests::view();
        let key = VersionedRoot::from_revision(1, backend_library::Cursor::for_view_root_at(&view, 0), 0);
        let snapshot = crate::navigation::reduce(
            &AppSnapshot::empty(key),
            Intent::AddProject {
                project: project.clone(),
            },
        )
        .snapshot;
        let persistence = PersistentState::at(directory.join("desktop.json"));
        let gate = OwnerGate::starting();
        gate.publish(OwnerState::Ready { key, mode: ServiceMode::Embedded });
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
        let observed = Arc::new(Mutex::new(Vec::new()));
        let hold_root = Arc::new(AtomicBool::new(false));
        let (root_started, root_entered) = mpsc::channel();
        let actor = EngineActor::start(
            HeldMutation {
                store: persistence.clone(),
                sent: sent.clone(),
                release: release_mutation.clone(),
                observed: observed.clone(),
                hold_root: hold_root.clone(),
                root_entered: root_started,
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
        cx.update(|cx| crate::runtime::owner::watch(gate.clone(), &root, &store, cx));
        cx.run_until_parked();
        assert_eq!(root.read_with(cx, |root, _| root.snapshot().settings().confirmed_service_mode),
            Some(ServiceMode::Embedded), "the real watcher admits initial Ready before the held save");
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
            observed,
            hold_root,
            root_entered,
        }
    }
    fn hold_actor(&self, cx: &mut TestAppContext) -> RegressionResult {
        self.hold_root.store(true, Ordering::SeqCst);
        self.root.update(cx, |root, cx| root.refresh_root(cx));
        self.root_entered.recv_timeout(std::time::Duration::from_secs(1))?;
        Ok(())
    }
    fn release_actor(&self) {
        self.hold_root.store(false, Ordering::SeqCst);
        let (lock, changed) = &*self.release_mutation;
        *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
        changed.notify_all();
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

type RegressionResult = Result<(), Box<dyn std::error::Error>>;

fn lineage(rig: &Rig, cx: &TestAppContext) -> Result<Intent, Box<dyn std::error::Error>> {
    rig.root.read_with(cx, |root, _| root.index_preflights.get(&rig.project).map(|pending| pending.intent.clone()))
        .ok_or_else(|| "the exact first-send preflight must exist".into())
}

fn advance(rig: &Rig) -> Result<VersionedRoot, Box<dyn std::error::Error>> {
    use backend_library::{Cursor, Row, RowId, ViewDelta, symbol_key};
    let view = crate::runtime::owner::publication_tests::view();
    let prepared = view.prepare(ViewDelta::Upsert {
        row: Row::new(RowId::Symbol(symbol_key("preflight-publication")), view.basis(), "New publication"),
    }, view.capability().ok_or("checked publication capability")?).map_err(|error| format!("{error:?}"))?;
    let (next, _) = prepared.commit(&view).map_err(|error| format!("{error:?}"))?;
    let cursor = Cursor::for_view_root_at(&next, 1);
    let attachment = rig.gate.ready_epoch().ok_or("live read attachment")?;
    assert_eq!(rig.gate.publish_view(attachment, Arc::new(next), cursor), crate::runtime::owner::PublicationAdmission::Admitted);
    Ok(VersionedRoot::from_revision(1, cursor, 0))
}

fn assert_sent_lineage(rig: &Rig, intent: &Intent, basis: VersionedRoot) -> RegressionResult {
    let Intent::IndexProject { request, operation, .. } = intent else { return Err("expected index intent".into()); };
    crate::runtime::wait::until("one exact first send", || rig.sent.load(Ordering::SeqCst) == 1);
    let observed = rig.observed.lock().map_err(|_| "request trace poisoned")?;
    assert_eq!(observed.as_slice(), &[(*request, operation.clone(), basis)], "a root advance cannot allocate a new request or claim");
    Ok(())
}

fn wait_saved(rig: &Rig, cx: &mut TestAppContext) {
    crate::runtime::wait::until("the saved proof waits for certified readiness", || {
        cx.run_until_parked();
        rig.root.read_with(cx, |root, _| root.index_preflights.get(&rig.project)
            .is_some_and(|pending| matches!(pending.save, IndexPreflightSave::Saved)))
    });
}

#[gpui::test]
fn delayed_save_survives_a_checked_same_owner_publication_with_exact_lineage(cx: &mut TestAppContext) {
    let result = exercise_publication(cx);
    assert!(result.is_ok(), "index preflight regression: {result:?}");
}
fn exercise_publication(cx: &mut TestAppContext) -> RegressionResult {
    let rig = Rig::new(cx);
    let intent = lineage(&rig, cx)?;
    let next = advance(&rig)?;
    cx.run_until_parked();
    assert_eq!(rig.sent.load(Ordering::SeqCst), 0, "publication cannot bypass synchronization");
    rig.release();
    rig.settle(cx);
    assert_sent_lineage(&rig, &intent, next)?;
    rig.root.update(cx, |root, cx| root.resume_saved_indexes(cx));
    assert_eq!(rig.sent.load(Ordering::SeqCst), 1, "one validated receipt can dispatch only once");
    rig.finish(cx);
    Ok(())
}

#[gpui::test]
fn saved_request_waits_through_freshness_recovery_without_a_false_failure(cx: &mut TestAppContext) {
    let result = exercise_recovery(cx, false);
    assert!(result.is_ok(), "index preflight regression: {result:?}");
}
#[gpui::test]
fn rejected_read_lease_reacquisition_retains_the_unsent_mutation_lifetime(cx: &mut TestAppContext) {
    let result = exercise_recovery(cx, true);
    assert!(result.is_ok(), "index preflight regression: {result:?}");
}
fn exercise_recovery(cx: &mut TestAppContext, replace: bool) -> RegressionResult {
    let rig = Rig::new(cx);
    let intent = lineage(&rig, cx)?;
    let before = rig.root.read_with(cx, |root, _| root.snapshot());
    let old = rig.gate.ready_epoch().ok_or("old read lease")?;
    let view = crate::runtime::owner::publication_tests::view();
    assert_eq!(rig.gate.publish_view(old, view.clone(), before.key().revision()), crate::runtime::owner::PublicationAdmission::Admitted);
    let renewed = if replace {
        rig.gate.replace_observation(old).ok_or("replace rejected read lease")?.0
    } else { rig.gate.suspend_observation(old).ok_or("suspend freshness")? };
    assert_ne!(old, renewed);
    rig.release();
    wait_saved(&rig, cx);
    assert_eq!(rig.sent.load(Ordering::SeqCst), 0);
    let saved = rig.persistence.load()?;
    let Intent::IndexProject { operation, .. } = &intent else { return Err("index lineage".into()); };
    assert_eq!(saved.shelf[0].operation.as_ref(), Some(operation));
    assert_eq!(saved.shelf[0].phase, crate::model::PersistedProjectPhase::Indexing);
    rig.root.read_with(cx, |root, _| {
        let snapshot = root.snapshot();
        assert_eq!(snapshot.workspace().projects[0].phase, ProjectPhase::Indexing);
        assert!(snapshot.workspace().projects[0].error.is_none());
        assert_eq!(snapshot.route(), before.route());
        assert_eq!(snapshot.session().back, before.session().back);
    });
    assert_eq!(rig.gate.publish_view(renewed, view, before.key().revision()), crate::runtime::owner::PublicationAdmission::Admitted);
    assert!(rig.gate.complete_observation(renewed));
    rig.settle(cx);
    assert_sent_lineage(&rig, &intent, before.key())?;
    rig.finish(cx);
    Ok(())
}

#[gpui::test]
fn saved_wait_cancellation_and_retry_persist_a_new_claim_and_ignore_the_old_ack(cx: &mut TestAppContext) {
    let result = exercise_saved_retry(cx);
    assert!(result.is_ok(), "index preflight regression: {result:?}");
}
fn exercise_saved_retry(cx: &mut TestAppContext) -> RegressionResult {
    let rig = Rig::new(cx);
    let original = lineage(&rig, cx)?;
    let (revision, state) = rig.root.read_with(cx, |root, _| {
        let pending = root.index_preflights.get(&rig.project).ok_or("old preflight")?;
        Ok::<_, Box<dyn std::error::Error>>((pending.revision,
            PersistentState::project(&crate::navigation::reduce(&root.snapshot(), pending.intent.clone()).snapshot)))
    })?;
    let key = rig.root.read_with(cx, |root, _| root.snapshot().key());
    let old = rig.gate.ready_epoch().ok_or("old read lease")?;
    let renewed = rig.gate.suspend_observation(old).ok_or("withdraw freshness")?;
    rig.release();
    wait_saved(&rig, cx);
    rig.root.update(cx, |root, cx| root.dispatch(Intent::CancelIndex(rig.project.clone()), cx));
    assert_eq!(rig.root.read_with(cx, |root, _| root.snapshot().workspace().projects[0].phase), ProjectPhase::Cancelled);
    rig.root.update(cx, |root, cx| root.dispatch(Intent::RetryIndex(rig.project.clone()), cx));
    assert_eq!(rig.sent.load(Ordering::SeqCst), 0, "Retry remains local while freshness is withdrawn");
    assert_eq!(rig.gate.publish_view(renewed, crate::runtime::owner::publication_tests::view(), key.revision()), crate::runtime::owner::PublicationAdmission::Admitted);
    assert!(rig.gate.complete_observation(renewed));
    rig.store.update(cx, |store, cx| store.owner_ready(cx));
    rig.root.update(cx, |root, cx| {
        root.schedule_pending_indexes(cx);
        root.flush_pending(cx);
        root.complete_index_preflight(rig.project.clone(), revision, Ok(WriteAck { revision, state: Arc::new(state) }), cx);
    });
    rig.settle(cx);
    crate::runtime::wait::until("Retry actually dispatches", || rig.sent.load(Ordering::SeqCst) == 1);
    let Intent::IndexProject { request, operation, .. } = original else { return Err("old index lineage".into()); };
    let observed = rig.observed.lock().map_err(|_| "lineage trace poisoned")?;
    assert_eq!(observed.len(), 1);
    assert_ne!(observed[0].0, request);
    assert_ne!(observed[0].1.key, operation.key);
    assert_eq!(rig.persistence.load()?.shelf[0].operation.as_ref(), Some(&observed[0].1));
    drop(observed);
    rig.finish(cx);
    Ok(())
}

#[gpui::test]
fn changed_producer_epoch_refuses_the_saved_claim_and_retry_really_rewrites_it(cx: &mut TestAppContext) {
    let result = exercise_epoch_replacement(cx);
    assert!(result.is_ok(), "index preflight regression: {result:?}");
}
fn exercise_epoch_replacement(cx: &mut TestAppContext) -> RegressionResult {
    let rig = Rig::new(cx);
    let original = lineage(&rig, cx)?;
    let key = rig.root.read_with(cx, |root, _| root.snapshot().key());
    let replacement = VersionedRoot::from_revision(key.producer_epoch() + 1, key.revision(), 0);
    rig.gate.publish(OwnerState::Starting);
    rig.gate.publish(OwnerState::Ready { key: replacement, mode: ServiceMode::Embedded });
    rig.store.update(cx, |store, cx| store.owner_ready(cx));
    rig.root.update(cx, |root, cx| root.dispatch_runtime(Intent::OwnerReady { key: replacement, mode: ServiceMode::Embedded }, cx));
    rig.release();
    rig.settle(cx);
    assert_eq!(rig.sent.load(Ordering::SeqCst), 0);
    let failed = rig.root.read_with(cx, |root, _| root.snapshot());
    assert_eq!(failed.workspace().projects[0].phase, ProjectPhase::Failed);
    assert!(failed.workspace().projects[0].error.as_deref().is_some_and(|error| error.contains("producer stream")));
    rig.root.update(cx, |root, cx| root.dispatch(Intent::RetryIndex(rig.project.clone()), cx));
    rig.settle(cx);
    crate::runtime::wait::until("explicit Retry reaches the new lifetime", || rig.sent.load(Ordering::SeqCst) == 1);
    let Intent::IndexProject { request, operation, .. } = original else { return Err("old request".into()); };
    let observed = rig.observed.lock().map_err(|_| "request trace poisoned")?;
    assert_ne!(observed[0].0, request);
    assert_ne!(observed[0].1.key, operation.key);
    assert_eq!(observed[0].2, replacement);
    drop(observed);
    rig.finish(cx);
    Ok(())
}

#[gpui::test]
fn queued_first_send_cancellation_is_terminal_and_retry_persists_a_new_claim(cx: &mut TestAppContext) {
    let result = exercise_queued_unsent(cx, false);
    assert!(result.is_ok(), "queued cancellation regression: {result:?}");
}

#[gpui::test]
fn queued_first_send_owner_replacement_is_unsent_and_retryable(cx: &mut TestAppContext) {
    let result = exercise_queued_unsent(cx, true);
    assert!(result.is_ok(), "queued owner replacement regression: {result:?}");
}

fn exercise_queued_unsent(cx: &mut TestAppContext, replace_owner: bool) -> RegressionResult {
    let rig = Rig::new(cx);
    let original = lineage(&rig, cx)?;
    rig.hold_actor(cx)?;
    rig.release();
    rig.settle(cx);
    let queued = rig.root.read_with(cx, |root, _| root.snapshot());
    let row = &queued.workspace().projects[0];
    assert!(row.request.is_some(), "the synchronized first send is queued behind the held root read");
    assert_eq!(rig.sent.load(Ordering::SeqCst), 0);
    let key = queued.key();
    if replace_owner {
        rig.gate.publish(OwnerState::Starting);
        rig.gate.publish(OwnerState::Ready { key, mode: ServiceMode::Embedded });
        rig.store.update(cx, |store, cx| store.owner_ready(cx));
    } else {
        rig.root.update(cx, |root, cx| root.dispatch(Intent::CancelIndex(rig.project.clone()), cx));
        assert_eq!(rig.root.read_with(cx, |root, _| root.snapshot().workspace().projects[0].phase), ProjectPhase::Cancelling);
    }
    rig.release_actor();
    let stopped = if replace_owner { ProjectPhase::Failed } else { ProjectPhase::Cancelled };
    crate::runtime::wait::until("queued first send settles without uncertainty", || {
        cx.run_until_parked();
        rig.root.update(cx, |root, cx| root.drain_engine(cx));
        rig.root.read_with(cx, |root, _| root.snapshot().workspace().projects[0].phase == stopped)
    });
    assert_eq!(rig.sent.load(Ordering::SeqCst), 0, "the actor never entered the first-send adapter");
    crate::runtime::wait::until("known-unsent terminal state is synchronized", || {
        rig.persistence.load().is_ok_and(|state| state.shelf[0].operation.is_none()
            && rig.persistence.cold_workspace(&state).projects[0].phase == stopped)
    });
    let disk = rig.persistence.load()?;
    assert!(disk.shelf[0].operation.is_none(), "known-unsent cancellation cannot become an unknown owner operation");
    assert_eq!(rig.persistence.cold_workspace(&disk).projects[0].phase, stopped);
    rig.root.update(cx, |root, cx| root.dispatch(Intent::RetryIndex(rig.project.clone()), cx));
    rig.settle(cx);
    crate::runtime::wait::until("safe explicit Retry enters the adapter once", || rig.sent.load(Ordering::SeqCst) == 1);
    let Intent::IndexProject { request, operation, .. } = original else { return Err("original index lineage".into()); };
    let observed = rig.observed.lock().map_err(|_| "request lineage poisoned")?;
    assert_eq!(observed.len(), 1);
    assert_ne!(observed[0].0, request);
    assert_ne!(observed[0].1.key, operation.key);
    assert_eq!(rig.persistence.load()?.shelf[0].operation.as_ref(), Some(&observed[0].1));
    drop(observed);
    rig.finish(cx);
    Ok(())
}
