//! Real product model/store/view-journal controls for coherent worker handoff.
use super::*;
use backend_engine::{
    Cursor, CursorEvent, ReadHeadPublicationFailure, ViewPersistence, ViewRoot, WorkspaceRoot,
};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

fn seed_view(daemon: &mut super::super::super::ProductDaemon) {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let (view, cursor) =
        super::super::super::initial_view_for_workspace(&snapshot).expect("real producer view");
    let admission = super::super::super::BuiltinViewAdmission {
        workspace_root: snapshot.root(),
        source_root: view.basis().root,
    };
    daemon
        .engine_mut()
        .daemon_mut()
        .publish_view(view, cursor, &admission, None)
        .expect("seed admitted read head");
}

fn prepare_view(
    writer: &mut backend_engine::ReadHeadWriter<BuiltinModel>,
    candidate: &backend_engine::PreparedWorkspaceCandidate,
    base: ViewRoot,
) -> (ViewRoot, Cursor) {
    let snapshot = writer
        .candidate_snapshot(candidate)
        .expect("private checked candidate");
    let (target, _) = super::super::super::initial_view_for_workspace(&snapshot)
        .expect("real candidate producer");
    let capability = super::super::super::builtin_view_capability_for_workspace(&snapshot)
        .expect("exact candidate capability");
    let delta = base
        .prepare(
            backend_engine::ViewDelta::Reset {
                root: Box::new(target),
            },
            capability,
        )
        .expect("checked view transition");
    let (view, delta) = base
        .commit(delta)
        .expect("committed private view transition");
    let event = CursorEvent::View {
        delta: Box::new(delta),
    };
    let cursor = Cursor::for_view_root(&view);
    let admission = super::super::super::BuiltinViewAdmission {
        workspace_root: snapshot.root(),
        source_root: view.basis().root,
    };
    writer
        .prepare_view(candidate, view.clone(), cursor, &admission, Some(event))
        .expect("prepare checked library and event privately");
    (view, cursor)
}

struct BlockingJournal {
    journal: super::super::super::ViewJournal,
    entered: SyncSender<()>,
    release: Receiver<()>,
}
impl ViewPersistence for BlockingJournal {
    fn persist(
        &mut self,
        root: WorkspaceRoot,
        view: &ViewRoot,
        cursor: Cursor,
        event: Option<&CursorEvent>,
    ) -> Result<(), String> {
        self.entered.send(()).map_err(|error| error.to_string())?;
        self.release.recv().map_err(|error| error.to_string())?;
        self.journal.persist(root, view, cursor, event)
    }
}

#[test]
fn prepared_read_head_serves_old_coherent_queries_during_actual_view_persistence() {
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    seed_view(&mut daemon);
    let old = daemon.engine().daemon().owner().snapshot();
    let old_view = daemon.engine().daemon().library().view().clone();
    let old_cursor = daemon.engine().daemon().library().cursor();
    let path = workspace.0.path().join("worker-view.journal");
    let journal = super::super::super::ViewJournal::open(&path).expect("real journal");
    let (entered_tx, entered_rx) = sync_channel(1);
    let (release_tx, release_rx) = sync_channel(1);
    daemon
        .engine_mut()
        .daemon_mut()
        .set_view_persistence(Box::new(BlockingJournal {
            journal,
            entered: entered_tx,
            release: release_rx,
        }))
        .unwrap_or_else(|(_, error)| panic!("sink install: {error}"));
    let mut writer = daemon
        .engine_mut()
        .daemon_mut()
        .reserve_read_head_writer()
        .expect("transfer both unique writers");
    let (claim_tx, claim_rx) = sync_channel(1);
    let (grant_tx, grant_rx) = sync_channel(1);
    let (done_tx, done_rx) = sync_channel(1);
    let base = old_view.clone();
    let worker = std::thread::spawn(move || {
        let candidate = writer
            .prepare(
                BuiltinIntent::add(
                    backend_engine::package_key("coherent-worker"),
                    "coherent-worker",
                )
                .expect("intent"),
            )
            .expect("actual private workspace preparation");
        let expected = prepare_view(&mut writer, &candidate, base);
        claim_tx.send(candidate.claim()).expect("claim");
        let grant = grant_rx.recv().expect("owner grant");
        let published = writer
            .publish(candidate, grant)
            .expect("durable coherent pair");
        done_tx
            .send((published, expected))
            .expect("retain published authority through transfer");
    });
    let claim = claim_rx.recv().expect("checked claim");
    let grant = daemon
        .engine_mut()
        .daemon_mut()
        .grant_workspace_candidate(claim, &AtomicBool::new(false))
        .expect("arbitrate against old head");
    grant_tx.send(grant).expect("single use grant");
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("actual selected HEAD reached durable view sink");
    let request = daemon
        .client()
        .request(0xfeed, crate::Request::Query)
        .expect("ordinary query while sink blocked");
    assert!(daemon.serve_one());
    let backend_engine::DaemonReply::Query(reply) = request.recv().expect("actual queued response")
    else {
        panic!("query lane")
    };
    let read = reply.expect("old head remains readable");
    assert_eq!(read.workspace.root(), old.root());
    assert_eq!(read.view, old_view);
    assert!(read.binding.is_current());
    assert_eq!(daemon.engine().daemon().library().cursor(), old_cursor);
    assert!(
        read.workspace
            .relation::<BuiltinWorkspaceRelation>()
            .expect("retained admitted relation")
            .lookup(backend_engine::package_key("coherent-worker").as_bytes())
            .expect("old selected membership")
            .is_none()
    );
    // The original sink cannot be replaced while its writer is held remotely.
    assert!(
        daemon
            .engine_mut()
            .daemon_mut()
            .set_view_persistence(Box::new(
                super::super::super::ViewJournal::open(workspace.0.path().join("foreign.journal"))
                    .expect("foreign sink")
            ))
            .is_err()
    );
    release_tx.send(()).expect("release only the owned sink");
    let (published, (expected_view, expected_cursor)) = done_rx
        .recv()
        .expect("complete persistence before response");
    let retired = daemon
        .engine_mut()
        .daemon_mut()
        .install_read_head(published)
        .expect("one coherent owner install");
    assert_ne!(daemon.engine().daemon().owner().head().root(), old.root());
    assert_eq!(daemon.engine().daemon().library().view(), &expected_view);
    assert_eq!(daemon.engine().daemon().library().cursor(), expected_cursor);
    let selected = daemon.engine().daemon().owner().snapshot();
    let capability = super::super::super::builtin_view_capability_for_workspace(&selected)
        .expect("selected capability");
    let reopened = super::super::super::ViewJournal::open(path)
        .expect("journal reopen")
        .load_for_workspace(selected.root(), &capability)
        .expect("exact durable view")
        .expect("selected view persisted");
    assert_eq!(reopened.view, expected_view);
    assert_eq!(reopened.cursor, expected_cursor);
    worker.join().expect("owned worker kernel retirement");
    std::thread::spawn(move || drop(retired))
        .join()
        .expect("old projections retire off actor");
}

struct FailingJournal {
    journal: super::super::super::ViewJournal,
    first: bool,
    panic: bool,
}
impl ViewPersistence for FailingJournal {
    fn persist(
        &mut self,
        root: WorkspaceRoot,
        view: &ViewRoot,
        cursor: Cursor,
        event: Option<&CursorEvent>,
    ) -> Result<(), String> {
        if std::mem::take(&mut self.first) {
            if self.panic {
                panic!("controlled view sink unwind after actual HEAD selection");
            }
            return Err("controlled view sink failure after actual HEAD selection".to_owned());
        }
        self.journal.persist(root, view, cursor, event)
    }
}

#[test]
fn prepared_read_head_selected_sink_error_stays_pending_until_same_writer_retry() {
    assert_selected_sink_retry(false);
}
#[test]
fn prepared_read_head_selected_sink_panic_stays_pending_until_same_writer_retry() {
    assert_selected_sink_retry(true);
}

fn assert_selected_sink_retry(panic: bool) {
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    seed_view(&mut daemon);
    let old = daemon.engine().daemon().owner().snapshot();
    let identity = (
        daemon.engine().daemon().owner().lease().epoch(),
        daemon.engine().daemon().owner().lease().fence(),
    );
    let base = daemon.engine().daemon().library().view().clone();
    let path = workspace.0.path().join("retry-view.journal");
    daemon
        .engine_mut()
        .daemon_mut()
        .set_view_persistence(Box::new(FailingJournal {
            journal: super::super::super::ViewJournal::open(&path).expect("real sink"),
            first: true,
            panic,
        }))
        .unwrap_or_else(|(_, error)| panic!("sink: {error}"));
    let mut writer = daemon
        .engine_mut()
        .daemon_mut()
        .reserve_read_head_writer()
        .expect("unique writers");
    let candidate = writer
        .prepare(
            BuiltinIntent::add(backend_engine::package_key("sink-retry"), "sink-retry")
                .expect("intent"),
        )
        .expect("actual durable candidate");
    let (view, cursor) = prepare_view(&mut writer, &candidate, base);
    let selected_snapshot = writer
        .candidate_snapshot(&candidate)
        .expect("candidate root");
    let grant = daemon
        .engine_mut()
        .daemon_mut()
        .grant_workspace_candidate(candidate.claim(), &AtomicBool::new(false))
        .expect("grant");
    let Err(ReadHeadPublicationFailure::SelectedPending { selected, .. }) =
        writer.publish(candidate, grant)
    else {
        panic!("selected view acknowledgement must remain pending")
    };
    assert!(
        selected.workspace_status().is_confirmed(),
        "workspace status is distinct from the pending view receipt"
    );
    assert_eq!(daemon.engine().daemon().owner().head().root(), old.root());
    assert_ne!(selected_snapshot.root(), old.root());
    let published = selected
        .persist()
        .expect("retry exact view with same sink and writer");
    let retired = daemon
        .engine_mut()
        .daemon_mut()
        .install_read_head(published)
        .expect("install only after persistence");
    assert_eq!(
        (
            daemon.engine().daemon().owner().lease().epoch(),
            daemon.engine().daemon().owner().lease().fence()
        ),
        identity
    );
    let capability = super::super::super::builtin_view_capability_for_workspace(&selected_snapshot)
        .expect("selected producer");
    let recovered = super::super::super::ViewJournal::open(path)
        .expect("reopen")
        .load_for_workspace(selected_snapshot.root(), &capability)
        .expect("recover")
        .expect("durable selected view");
    assert_eq!(recovered.view, view);
    assert_eq!(recovered.cursor, cursor);
    drop(retired);
    drop(daemon);
    assert_eq!(
        open_daemon(workspace.0.path())
            .engine()
            .daemon()
            .owner()
            .head()
            .root(),
        selected_snapshot.root(),
        "restart classifies the actually selected HEAD as Published"
    );
}

#[test]
fn prepared_read_head_admission_unwind_returns_both_ungranted_writers() {
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    seed_view(&mut daemon);
    let mut writer = daemon
        .engine_mut()
        .daemon_mut()
        .reserve_read_head_writer()
        .expect("writer");
    let intent = BuiltinIntent::add(backend_engine::package_key("view-unwind"), "view-unwind")
        .expect("intent");
    let candidate = writer.prepare(intent.clone()).expect("actual preparation");
    let snapshot = writer
        .candidate_snapshot(&candidate)
        .expect("candidate snapshot");
    let (view, cursor) =
        super::super::super::initial_view_for_workspace(&snapshot).expect("producer");
    assert!(
        writer
            .prepare_view(
                &candidate,
                view,
                cursor,
                &|_: &backend_engine::WorkspaceSnapshot, _: &ViewRoot| -> Result<(), String> {
                    panic!("controlled producer admission panic")
                },
                None
            )
            .is_err()
    );
    let retired = daemon
        .engine_mut()
        .daemon_mut()
        .return_unselected_read_head_writer(writer)
        .expect("pre-grant panic restores original authority");
    drop(retired);
    let mut next = daemon
        .engine_mut()
        .daemon_mut()
        .reserve_read_head_writer()
        .expect("next reservation does not deadlock");
    let _candidate = next
        .prepare(intent)
        .expect("same live lease prepares again");
    drop(
        daemon
            .engine_mut()
            .daemon_mut()
            .return_unselected_read_head_writer(next)
            .expect("return next writer"),
    );
}

#[test]
fn prepared_read_head_foreign_install_preserves_the_complete_selected_pair() {
    let first = TempWorkspace::new();
    let second = TempWorkspace::new();
    let mut a = open_daemon(first.0.path());
    let mut b = open_daemon(second.0.path());
    seed_view(&mut a);
    seed_view(&mut b);
    let base = b.engine().daemon().library().view().clone();
    let mut writer = b
        .engine_mut()
        .daemon_mut()
        .reserve_read_head_writer()
        .expect("B writer");
    let candidate = writer
        .prepare(
            BuiltinIntent::add(backend_engine::package_key("owned-B"), "owned-B").expect("intent"),
        )
        .expect("B candidate");
    let (view, cursor) = prepare_view(&mut writer, &candidate, base);
    let grant = b
        .engine_mut()
        .daemon_mut()
        .grant_workspace_candidate(candidate.claim(), &AtomicBool::new(false))
        .expect("B grant");
    let selected = writer.publish(candidate, grant).expect("B durable pair");
    let (selected, _) = a
        .engine_mut()
        .daemon_mut()
        .install_read_head(selected)
        .expect_err("foreign owner must return every authority");
    drop(
        b.engine_mut()
            .daemon_mut()
            .install_read_head(selected)
            .expect("correct B install still works"),
    );
    assert_eq!(b.engine().daemon().library().view(), &view);
    assert_eq!(b.engine().daemon().library().cursor(), cursor);
    let mut writer = a
        .engine_mut()
        .daemon_mut()
        .reserve_read_head_writer()
        .expect("A untouched writer");
    let _candidate = writer
        .prepare(
            BuiltinIntent::add(backend_engine::package_key("owned-A"), "owned-A").expect("intent"),
        )
        .expect("A remains writable");
    drop(
        a.engine_mut()
            .daemon_mut()
            .return_unselected_read_head_writer(writer)
            .expect("A return"),
    );
}
