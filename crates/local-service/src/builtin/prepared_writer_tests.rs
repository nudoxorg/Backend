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
        done_tx.send(committed).expect("return unique writer");
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
    let committed = done_rx.recv().expect("durable result before install");
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
        .install_workspace_candidate(committed)
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
    super::super::super::commands::commit_builtin_intent(&mut daemon, 1, &intent)
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
    // The published capability encloses the writer: there is no API that
    // can turn a selected result back into an unselected/cancelled writer.
    let retired = daemon
        .engine_mut()
        .daemon_mut()
        .install_workspace_candidate(committed)
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
    let failure = writer
        .publish(candidate, grant)
        .expect_err("actual pre-HEAD fault remains a failure");
    let backend_engine::WorkspacePublicationFailure::Unsettled { mut writer, .. } = failure else {
        panic!("physical publication was attempted");
    };
    let unselected = writer
        .prove_unselected()
        .expect("worker verifies unchanged physical head on same exclusive lease");
    daemon
        .engine_mut()
        .daemon_mut()
        .return_failed_workspace_writer(writer, unselected)
        .expect("failed attempt returns writer");
    super::super::super::commands::commit_builtin_intent(&mut daemon, 1, &intent)
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

#[test]
fn prepared_writer_grant_then_worker_unwind_before_publish_returns_same_lease() {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::sync_channel;
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    let identity = (
        daemon.engine().daemon().owner().lease().epoch(),
        daemon.engine().daemon().owner().lease().fence(),
    );
    let intent = BuiltinIntent::add(backend_engine::package_key("grant-gap"), "grant-gap")
        .expect("real intent");
    let writer = daemon
        .engine_mut()
        .daemon_mut()
        .reserve_workspace_writer()
        .expect("writer");
    let next_intent = intent.clone();
    let (claim_tx, claim_rx) = sync_channel(1);
    let (grant_tx, grant_rx) = sync_channel(1);
    let worker = std::thread::spawn(move || {
        let mut writer = writer;
        let candidate = writer.prepare(intent).expect("private durable candidate");
        claim_tx.send(candidate.claim()).expect("claim");
        let grant: backend_engine::PublishGrant = grant_rx.recv().expect("actual received grant");
        let failure = std::panic::catch_unwind(|| {
            panic!("controlled worker failure after grant before publish")
        });
        assert!(failure.is_err());
        // Even an error that lost the received grant before publication is
        // recoverable under the same lease. Proof seals this writer first.
        drop(grant);
        drop(candidate);
        let unselected = writer
            .prove_unselected()
            .expect("exact physical base unchanged before publish entry");
        (writer, unselected)
    });
    let claim = claim_rx.recv().expect("claim");
    let grant = daemon
        .engine_mut()
        .daemon_mut()
        .grant_workspace_candidate(claim, &AtomicBool::new(false))
        .expect("grant");
    grant_tx
        .send(grant)
        .expect("grant delivered before controlled failure");
    let (writer, unselected) = worker
        .join()
        .expect("owned worker returned after caught unwind");
    daemon
        .engine_mut()
        .daemon_mut()
        .return_failed_workspace_writer(writer, unselected)
        .expect("granted-but-not-started reservation settled");
    assert_eq!(
        (
            daemon.engine().daemon().owner().lease().epoch(),
            daemon.engine().daemon().owner().lease().fence()
        ),
        identity
    );
    super::super::super::commands::commit_builtin_intent(&mut daemon, 1, &next_intent)
        .expect("same lease can execute next real commit");
}

#[test]
fn prepared_writer_unselected_proof_seals_against_later_retained_grant() {
    use std::sync::atomic::AtomicBool;
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    let intent = BuiltinIntent::add(backend_engine::package_key("sealed-proof"), "sealed-proof")
        .expect("intent");
    let mut writer = daemon
        .engine_mut()
        .daemon_mut()
        .reserve_workspace_writer()
        .expect("writer");
    let candidate = writer.prepare(intent).expect("candidate");
    let grant = daemon
        .engine_mut()
        .daemon_mut()
        .grant_workspace_candidate(candidate.claim(), &AtomicBool::new(false))
        .expect("grant");
    let unselected = writer
        .prove_unselected()
        .expect("actual unchanged base proof");
    let failure = writer
        .publish(candidate, grant)
        .expect_err("sealed writer refuses a retained grant");
    let backend_engine::WorkspacePublicationFailure::Rejected { writer, .. } = failure else {
        panic!("sealed proof forbids any physical publication attempt");
    };
    daemon
        .engine_mut()
        .daemon_mut()
        .return_failed_workspace_writer(writer, unselected)
        .expect("sealed unchanged writer returns");
    assert_eq!(daemon.engine().daemon().owner().snapshot().sequence(), 0);
}

#[test]
fn prepared_writer_selected_head_before_ack_recovers_truthful_pending_status() {
    use std::sync::atomic::AtomicBool;
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    let intent = BuiltinIntent::add(backend_engine::package_key("before-ack"), "before-ack")
        .expect("intent");
    let mut writer = daemon
        .engine_mut()
        .daemon_mut()
        .reserve_workspace_writer()
        .expect("writer");
    let candidate = writer.prepare(intent).expect("candidate");
    let grant = daemon
        .engine_mut()
        .daemon_mut()
        .grant_workspace_candidate(candidate.claim(), &AtomicBool::new(false))
        .expect("grant");
    // This hook is after the actual FileStore HEAD linearization point and
    // before acknowledge_selection can append Select or write diagnostics.
    daemon
        .engine()
        .daemon()
        .owner()
        .faults()
        .arm(backend_engine::Boundary::OwnerAcknowledgement);
    let committed = writer
        .publish(candidate, grant)
        .expect("actual selected root recovered on worker");
    let status = committed.status();
    assert!(status.journal_select_pending());
    assert!(status.journal_flush_pending());
    assert!(status.head_write_pending());
    assert!(status.head_sync_pending());
    assert!(status.head_selection_pending());
    assert!(status.journal_published_pending());
    assert!(status.notification_pending());
    assert!(!status.is_confirmed());
    // Selected authority is inseparable from its result, so only checked
    // installation (or a refusal returning the whole capability) is possible.
    let retired = daemon
        .engine_mut()
        .daemon_mut()
        .install_workspace_candidate(committed)
        .expect("truthful selected head installed before reply");
    drop(retired);
    let root = daemon.engine().daemon().owner().snapshot().root();
    drop(daemon);
    let reopened = open_daemon(workspace.0.path());
    assert_eq!(reopened.engine().daemon().owner().snapshot().root(), root);
    assert_eq!(
        reopened.engine().daemon().owner().snapshot().sequence(),
        1,
        "cold recovery selects durable Published despite missing engine ack frames"
    );
}

#[test]
fn prepared_writer_foreign_grant_and_publication_routes_preserve_all_authority() {
    use std::sync::atomic::AtomicBool;
    let first_home = TempWorkspace::new();
    let second_home = TempWorkspace::new();
    let mut first = open_daemon(first_home.0.path());
    let mut second = open_daemon(second_home.0.path());
    let first_intent =
        BuiltinIntent::add(backend_engine::package_key("route-first"), "route-first")
            .expect("first intent");
    let second_intent =
        BuiltinIntent::add(backend_engine::package_key("route-second"), "route-second")
            .expect("second intent");
    let mut first_writer = first
        .engine_mut()
        .daemon_mut()
        .reserve_workspace_writer()
        .expect("first writer");
    let mut second_writer = second
        .engine_mut()
        .daemon_mut()
        .reserve_workspace_writer()
        .expect("second writer");
    let first_candidate = first_writer
        .prepare(first_intent.clone())
        .expect("first candidate");
    let second_candidate = second_writer
        .prepare(second_intent)
        .expect("second candidate");
    let second_grant = second
        .engine_mut()
        .daemon_mut()
        .grant_workspace_candidate(second_candidate.claim(), &AtomicBool::new(false))
        .expect("second grant");
    let refusal = first_writer
        .publish(first_candidate, second_grant)
        .expect_err("wrong-writer grant must be rejected before publication");
    let backend_engine::WorkspacePublicationFailure::Rejected {
        writer: first_writer,
        candidate: first_candidate,
        grant: second_grant,
        ..
    } = refusal
    else {
        panic!("preflight must retain every supplied capability");
    };
    let published = second_writer
        .publish(second_candidate, second_grant)
        .expect("the returned unique grant still publishes on the correct writer");
    let (published, _) = first
        .engine_mut()
        .daemon_mut()
        .install_workspace_candidate(published)
        .expect_err("foreign owner cannot install the selected result");
    let retired = second
        .engine_mut()
        .daemon_mut()
        .install_workspace_candidate(published)
        .expect("foreign refusal returns the inseparable selected result and writer");
    assert_eq!(second.engine().daemon().owner().snapshot().sequence(), 1);
    assert_eq!(first.engine().daemon().owner().snapshot().sequence(), 0);
    drop(retired);
    drop(first_candidate);
    first
        .engine_mut()
        .daemon_mut()
        .return_unselected_workspace_writer(first_writer)
        .expect("first owner retains its original live writer");
    super::super::super::commands::commit_builtin_intent(&mut first, 1, &first_intent)
        .expect("foreign routing refuses without stranding either owner");
}

#[test]
fn prepared_writer_repeated_real_intent_preserves_current_view_binding() {
    use std::sync::atomic::AtomicBool;
    let home = TempWorkspace::new();
    let mut daemon = open_daemon(home.0.path());
    let intent = BuiltinIntent::add(
        backend_engine::package_key("same-root-replay"),
        "same-root-replay",
    )
    .expect("real repeated intent");
    for replay in [false, true] {
        let mut writer = daemon
            .engine_mut()
            .daemon_mut()
            .reserve_workspace_writer()
            .expect("writer");
        let candidate = writer
            .prepare(intent.clone())
            .expect("real model preparation or exact idempotent retry");
        let grant = daemon
            .engine_mut()
            .daemon_mut()
            .grant_workspace_candidate(candidate.claim(), &AtomicBool::new(false))
            .expect("grant");
        let published = writer
            .publish(candidate, grant)
            .expect("physical publish or selected-head replay");
        let retired = daemon
            .engine_mut()
            .daemon_mut()
            .install_workspace_candidate(published)
            .expect("install");
        drop(retired);
        if !replay {
            let snapshot = daemon.engine().daemon().owner().snapshot();
            let (view, cursor) = super::super::super::initial_view_for_workspace(&snapshot)
                .expect("actual checked product view");
            let admission = super::super::super::BuiltinViewAdmission {
                workspace_root: snapshot.root(),
                source_root: view.basis().root,
            };
            daemon
                .engine_mut()
                .daemon_mut()
                .publish_view(view, cursor, &admission, None)
                .expect("current product view before replay");
        }
        let query = daemon
            .client()
            .request(if replay { 2 } else { 1 }, crate::Request::Query)
            .expect("ordinary query");
        assert!(daemon.serve_one());
        let backend_engine::DaemonReply::Query(result) = query.recv().expect("query reply") else {
            panic!("query lane");
        };
        let result = result.expect("admitted query pair");
        assert_eq!(
            result.workspace.sequence(),
            1,
            "replay never mints another generation"
        );
        assert!(
            result.binding.is_current(),
            "same-head install must retain the existing coherent view binding"
        );
    }
}

#[test]
fn prepared_writer_selected_head_read_failure_stays_pending_and_retryable() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct FailOnceAdmission(Arc<AtomicBool>);
    impl WorkspaceModel for FailOnceAdmission {
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
            if self.0.swap(false, Ordering::AcqRel) {
                return Err(BuiltinModelError(
                    "controlled selected-head admission read failure".to_owned(),
                ));
            }
            BuiltinModel.admit_persisted_with_store(persisted, store)
        }
    }
    let home = TempWorkspace::new();
    let fail = Arc::new(AtomicBool::new(false));
    let mut owner = backend_engine::WorkspaceOwner::open_with_registry(
        home.0.path(),
        FailOnceAdmission(Arc::clone(&fail)),
        super::super::super::genesis().expect("genesis"),
        super::super::super::product_relation_registry().expect("registry"),
    )
    .expect("actual model/store owner");
    let identity = (owner.lease().epoch(), owner.lease().fence());
    let mut writer = owner.reserve_writer().expect("writer");
    let candidate = writer
        .prepare(
            BuiltinIntent::add(backend_engine::package_key("read-retry"), "read-retry")
                .expect("intent"),
        )
        .expect("actual durable candidate");
    let grant = owner
        .grant_candidate(candidate.claim(), &AtomicBool::new(false))
        .expect("grant");
    owner
        .faults()
        .arm(backend_engine::Boundary::OwnerAcknowledgement);
    fail.store(true, Ordering::Release);
    let failure = writer
        .publish(candidate, grant)
        .expect_err("selected root is not yet admissible");
    let backend_engine::WorkspacePublicationFailure::Unsettled { mut writer, error } = failure
    else {
        panic!("publication really reached physical HEAD");
    };
    assert!(
        matches!(
            error,
            backend_engine::WorkspaceError::PublicationPending { .. }
        ),
        "selected-head read failure must remain pending, never Failed/Cancelled"
    );
    assert!(
        writer.prove_unselected().is_err(),
        "actual selected HEAD prevents failed settlement"
    );
    let published = writer
        .reconcile_publication()
        .expect("same writer recovers when the read succeeds");
    assert!(!published.status().is_confirmed());
    drop(
        owner
            .install_candidate(published)
            .expect("recovered selected result installs"),
    );
    assert_eq!((owner.lease().epoch(), owner.lease().fence()), identity);
    let root = owner.head().root();
    drop(owner);
    let reopened = open_daemon(home.0.path());
    assert_eq!(reopened.engine().daemon().owner().snapshot().root(), root);
    assert_eq!(reopened.engine().daemon().owner().snapshot().sequence(), 1);
}
