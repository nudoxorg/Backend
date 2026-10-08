//! Source/store controls for the private prepared writer boundary.
use super::*;

/// Real product model/store law: a worker can prepare and durably select a
/// candidate while ordinary daemon queries remain bound to the prior admitted
/// snapshot. No synthetic transition or latest-HEAD authority is substituted.
#[test]
fn prepared_writer_keeps_old_product_reads_until_checked_install() {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::sync_channel;
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    let old = daemon.engine().daemon().owner().snapshot();
    let package = backend_engine::package_key("prepared-writer-project");
    let label = "prepared-writer-project";
    let intent = BuiltinIntent::add(package, label).expect("real product add intent");
    let writer = daemon
        .engine_mut()
        .daemon_mut()
        .reserve_workspace_writer()
        .expect("reserve sole writer");
    assert!(matches!(
        daemon.engine().daemon().owner().prepare(
            backend_engine::HeadExpectation::new(old.root(), old.sequence()),
            intent.clone()
        ),
        Err(backend_engine::WorkspaceError::WriterReserved)
    ));
    let (claim_tx, claim_rx) = sync_channel(1);
    let (grant_tx, grant_rx) = sync_channel(1);
    let (done_tx, done_rx) = sync_channel(1);
    let worker = std::thread::spawn(move || {
        let mut writer = writer;
        let candidate = writer
            .prepare(intent)
            .expect("real checked model preparation/durable CAS");
        let pending = writer
            .candidate_snapshot(&candidate)
            .expect("checked candidate snapshot");
        assert_ne!(pending.root(), old.root());
        claim_tx.send(candidate.claim()).expect("candidate claim");
        let grant = grant_rx.recv().expect("single-use owner grant");
        let committed = writer
            .publish(candidate, grant)
            .expect("existing physical publish law");
        done_tx
            .send((writer, committed))
            .expect("return unique writer");
    });
    let claim = claim_rx.recv().expect("prepared claim");
    let old_root = daemon.engine().daemon().owner().snapshot().root();
    let cancellation = AtomicBool::new(false);
    let grant = daemon
        .engine_mut()
        .daemon_mut()
        .grant_workspace_candidate(claim, &cancellation)
        .expect("grant exact private candidate");
    assert!(
        daemon
            .engine_mut()
            .daemon_mut()
            .grant_workspace_candidate(claim, &cancellation)
            .is_err(),
        "grant cannot be repeated"
    );
    grant_tx.send(grant).expect("grant to worker");
    let (writer, committed) = done_rx.recv().expect("durable result before install");
    let retained = daemon.engine().daemon().owner().snapshot();
    assert_eq!(
        retained.root(),
        old_root,
        "worker must never install the actor's read head"
    );
    assert!(
        retained.with_persisted_transition(|_, _| ()).is_err(),
        "fresh-selected API remains strict after physical selection"
    );
    retained
        .with_retained_transition(|_, _| ())
        .expect("retained checked read does not consult latest HEAD");
    let before = retained
        .relation::<BuiltinWorkspaceRelation>()
        .expect("old source relation");
    assert!(
        before
            .lookup(package.as_bytes())
            .expect("old root lookup")
            .is_none()
    );
    let query = daemon
        .client()
        .request(0xfeed, crate::Request::Query)
        .expect("queue ordinary query");
    assert!(daemon.serve_one());
    let backend_engine::DaemonReply::Query(result) = query.recv().expect("actual query reply")
    else {
        panic!("query lane")
    };
    assert_eq!(
        result.expect("old read available").workspace.root(),
        old_root
    );
    let retired = daemon
        .engine_mut()
        .daemon_mut()
        .install_workspace_candidate(writer, committed)
        .expect("fixed-width checked install");
    let selected = daemon.engine().daemon().owner().snapshot();
    assert_ne!(selected.root(), old_root);
    assert!(
        selected
            .relation::<BuiltinWorkspaceRelation>()
            .expect("new source relation")
            .lookup(package.as_bytes())
            .expect("new root lookup")
            .is_some()
    );
    assert_eq!(selected.sequence(), 1);
    worker.join().expect("owned worker retirement");
    drop(retired);
}

#[test]
fn prepared_writer_cancellation_before_grant_returns_the_same_live_writer() {
    use std::sync::atomic::AtomicBool;
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    let identity = (
        daemon.engine().daemon().owner().lease().epoch(),
        daemon.engine().daemon().owner().lease().fence(),
    );
    let intent = BuiltinIntent::add(
        backend_engine::package_key("cancel-before"),
        "cancel-before",
    )
    .expect("real intent");
    let mut writer = daemon
        .engine_mut()
        .daemon_mut()
        .reserve_workspace_writer()
        .expect("writer");
    let candidate = writer
        .prepare(intent.clone())
        .expect("private durable candidate");
    assert!(matches!(
        daemon
            .engine_mut()
            .daemon_mut()
            .grant_workspace_candidate(candidate.claim(), &AtomicBool::new(true)),
        Err(backend_engine::WorkspaceError::PublicationCancelled)
    ));
    drop(candidate);
    daemon
        .engine_mut()
        .daemon_mut()
        .return_unselected_workspace_writer(writer)
        .expect("recover writer without a second lease");
    assert_eq!(daemon.engine().daemon().owner().snapshot().sequence(), 0);
    assert_eq!(
        (
            daemon.engine().daemon().owner().lease().epoch(),
            daemon.engine().daemon().owner().lease().fence()
        ),
        identity
    );
    super::super::commands::commit_builtin_intent(&mut daemon, 1, &intent)
        .expect("reservation does not deadlock the next mutation");
}

#[test]
fn prepared_writer_cancellation_after_grant_cannot_discard_a_durable_publication() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    let intent = BuiltinIntent::add(backend_engine::package_key("cancel-after"), "cancel-after")
        .expect("real intent");
    let cancellation = AtomicBool::new(false);
    let mut writer = daemon
        .engine_mut()
        .daemon_mut()
        .reserve_workspace_writer()
        .expect("writer");
    let candidate = writer.prepare(intent).expect("private candidate");
    let grant = daemon
        .engine_mut()
        .daemon_mut()
        .grant_workspace_candidate(candidate.claim(), &cancellation)
        .expect("owner wins arbitration");
    cancellation.store(true, Ordering::Release);
    // Notification failure is after durable selection; current recovery law
    // already represents it as a successful publication with pending ack.
    daemon
        .engine()
        .daemon()
        .owner()
        .faults()
        .arm(backend_engine::Boundary::Notification);
    let committed = writer
        .publish(candidate, grant)
        .expect("publication wins after grant");
    assert!(committed.status().notification_pending());
    assert!(
        writer.prove_unselected().is_err(),
        "selected root cannot be returned as an unselected/cancelled attempt"
    );
    let (writer, _) = daemon
        .engine_mut()
        .daemon_mut()
        .return_unselected_workspace_writer(writer)
        .expect_err("granted writer requires truthful settlement");
    let retired = daemon
        .engine_mut()
        .daemon_mut()
        .install_workspace_candidate(writer, committed)
        .expect("install selected result");
    assert_eq!(daemon.engine().daemon().owner().snapshot().sequence(), 1);
    drop(retired);
    drop(daemon);
    let reopened = open_daemon(workspace.0.path());
    assert_eq!(
        reopened.engine().daemon().owner().snapshot().sequence(),
        1,
        "restart recovers durable Published, never Cancelled"
    );
}

#[test]
fn prepared_writer_failed_grant_returns_authority_after_exact_worker_reconciliation() {
    use std::sync::atomic::AtomicBool;
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    let intent = BuiltinIntent::add(backend_engine::package_key("failed-grant"), "failed-grant")
        .expect("real intent");
    let mut writer = daemon
        .engine_mut()
        .daemon_mut()
        .reserve_workspace_writer()
        .expect("writer");
    let candidate = writer.prepare(intent.clone()).expect("candidate");
    let grant = daemon
        .engine_mut()
        .daemon_mut()
        .grant_workspace_candidate(candidate.claim(), &AtomicBool::new(false))
        .expect("grant");
    daemon
        .engine()
        .daemon()
        .owner()
        .faults()
        .arm(backend_engine::Boundary::HeadWrite);
    writer
        .publish(candidate, grant)
        .expect_err("actual pre-HEAD fault remains a failure");
    let unselected = writer
        .prove_unselected()
        .expect("worker verifies unchanged physical head on same exclusive lease");
    daemon
        .engine_mut()
        .daemon_mut()
        .return_failed_workspace_writer(writer, unselected)
        .expect("failed attempt returns writer");
    super::super::commands::commit_builtin_intent(&mut daemon, 1, &intent)
        .expect("next real commit cannot deadlock");
}

#[test]
fn prepared_writer_rejects_foreign_and_late_claims_before_grant() {
    use std::sync::atomic::AtomicBool;
    let first_home = TempWorkspace::new();
    let second_home = TempWorkspace::new();
    let mut first = open_daemon(first_home.0.path());
    let mut second = open_daemon(second_home.0.path());
    let intent = BuiltinIntent::add(backend_engine::package_key("claim-test"), "claim-test")
        .expect("real intent");
    let mut writer = first
        .engine_mut()
        .daemon_mut()
        .reserve_workspace_writer()
        .expect("first writer");
    let candidate = writer.prepare(intent).expect("candidate");
    let old_claim = candidate.claim();
    let second_writer = second
        .engine_mut()
        .daemon_mut()
        .reserve_workspace_writer()
        .expect("second writer");
    assert!(
        second
            .engine_mut()
            .daemon_mut()
            .grant_workspace_candidate(old_claim, &AtomicBool::new(false))
            .is_err(),
        "same-shaped base in a foreign owner is not authority"
    );
    second
        .engine_mut()
        .daemon_mut()
        .return_unselected_workspace_writer(second_writer)
        .expect("foreign owner retains its own writer");
    drop(candidate);
    first
        .engine_mut()
        .daemon_mut()
        .return_unselected_workspace_writer(writer)
        .expect("unselected reservation return");
    let next_writer = first
        .engine_mut()
        .daemon_mut()
        .reserve_workspace_writer()
        .expect("next reservation");
    assert!(
        first
            .engine_mut()
            .daemon_mut()
            .grant_workspace_candidate(old_claim, &AtomicBool::new(false))
            .is_err(),
        "late claim cannot enter a newer nonce reservation"
    );
    first
        .engine_mut()
        .daemon_mut()
        .return_unselected_workspace_writer(next_writer)
        .expect("late refusal does not strand writer");
}

#[test]
fn prepared_writer_model_unwind_keeps_the_exclusive_lease_recoverable() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct PanicOnceModel(AtomicBool);
    impl WorkspaceModel for PanicOnceModel {
        type Intent = BuiltinIntent;
        type Error = BuiltinModelError;
        fn request_id(&self, intent: &BuiltinIntent) -> [u8; 32] {
            BuiltinModel.request_id(intent)
        }
        fn prepare(
            &self,
            base: &backend_engine::WorkspaceSnapshot,
            intent: &BuiltinIntent,
            transaction: backend_engine::TransactionId,
        ) -> Result<backend_engine::PreparedTransition, BuiltinModelError> {
            if !self.0.swap(true, Ordering::AcqRel) {
                panic!("controlled candidate preparation unwind");
            }
            BuiltinModel.prepare(base, intent, transaction)
        }
        fn admit_persisted(
            &self,
            persisted: &backend_engine::PersistedTransition,
        ) -> Result<backend_engine::PreparedTransition, BuiltinModelError> {
            BuiltinModel.admit_persisted(persisted)
        }
        fn admit_persisted_with_store(
            &self,
            persisted: &backend_engine::PersistedTransition,
            store: &backend_engine::FileStore,
        ) -> Result<backend_engine::PreparedTransition, BuiltinModelError> {
            BuiltinModel.admit_persisted_with_store(persisted, store)
        }
    }
    let workspace = TempWorkspace::new();
    let mut owner = backend_engine::WorkspaceOwner::open_with_registry(
        workspace.0.path(),
        PanicOnceModel(AtomicBool::new(false)),
        super::super::super::genesis().expect("real genesis"),
        super::super::super::product_relation_registry().expect("real product registry"),
    )
    .expect("real model owner");
    let epoch = owner.lease().epoch();
    let mut writer = owner.reserve_writer().expect("writer");
    let intent = BuiltinIntent::add(
        backend_engine::package_key("unwind-return"),
        "unwind-return",
    )
    .expect("intent");
    assert!(matches!(
        writer.prepare(intent.clone()),
        Err(backend_engine::WorkspaceError::Model(_))
    ));
    owner
        .return_unselected_writer(writer)
        .expect("unwind returns still-held unique writer");
    assert_eq!(
        owner.lease().epoch(),
        epoch,
        "no reopen/reacquisition is used for unwind recovery"
    );
    let prepared = owner
        .prepare(owner.head().expectation(), intent)
        .expect("next real model preparation");
    let durable = owner.durable(prepared).expect("real durable candidate");
    owner
        .publish(durable)
        .expect("recovered writer can publish");
}
