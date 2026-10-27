//! Real completed filesystem scan, production LocaldOwner/deferred adapter,
//! real framed listener, and independent public subscription clients.
use super::*;
use crate::builtin::commands::adapter::index_publication_worker::{TestHook, TestStage};
use crate::listener::{ListenerConfig, ListenerShutdown, UnixListenerService};
use crate::service::LocaldService;
use backend_client::{LocalSubscriptionTransport, Session};
use backend_engine::{
    LocalSubscriptionResponse, SnapshotHydrator, SnapshotPageClaim, WorkspaceViewProducerAdmission,
};
use backend_library::CursorRead;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::Instant;

/// Observation only: copied from the real daemon after actual public commands.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ReadHeadIdentity {
    workspace: backend_version::WorkspaceRoot,
    commit: [u8; 32],
    sequence: u64,
    view: [u8; 32],
    cursor: backend_engine::Cursor,
}
impl ReadHeadIdentity {
    fn capture(daemon: &ProductDaemon) -> Self {
        let owner = daemon.engine().daemon().owner();
        let library = daemon.engine().daemon().library();
        Self {
            workspace: owner.head().root(),
            commit: *owner.head().commit().id().as_bytes(),
            sequence: owner.head().sequence(),
            view: *library.view().root().as_bytes(),
            cursor: library.cursor(),
        }
    }
}

struct SourceWire {
    // Keep the fixture's private workspace alive until the production owner
    // has closed and every tracked worker has joined.
    fixture: AdapterFixture,
    commands: Arc<Mutex<CommandAdapter>>,
    read_head: Arc<Mutex<Option<ReadHeadIdentity>>>,
    endpoint: PathBuf,
    stopped: ListenerShutdown,
    thread: Option<std::thread::JoinHandle<()>>,
    release: Option<SyncSender<()>>,
    entered: Receiver<()>,
    old: backend_engine::WorkspaceSnapshot,
    old_view: backend_engine::ViewRoot,
    cursor: backend_engine::Cursor,
    operation: backend_library::IndexOperationKey,
    ticket: backend_library::IndexJobTicket,
    source: PathBuf,
}

impl SourceWire {
    fn start(stage: TestStage, panic_after_release: bool) -> Self {
        Self::start_with_cluster(stage, panic_after_release, false)
    }

    fn start_with_cluster(stage: TestStage, panic_after_release: bool, cluster: bool) -> Self {
        Self::start_options(stage, panic_after_release, cluster, false)
    }

    fn start_options(
        stage: TestStage,
        panic_after_release: bool,
        cluster: bool,
        refuse_start: bool,
    ) -> Self {
        Self::start_kind(stage, panic_after_release, cluster, refuse_start, false)
    }

    fn start_kind(
        stage: TestStage,
        panic_after_release: bool,
        cluster: bool,
        refuse_start: bool,
        structural: bool,
    ) -> Self {
        let mut fixture = AdapterFixture::new();
        let (package, label) = fixture.add_target();
        let source = Path::new(&label).join(if structural {
            "notes.txt"
        } else {
            "changed.ts"
        });
        fs::write(&source, "export const original = 7;\n").expect("real source input");
        // Two actual admitted package rows force a multipage reset at credit 1.
        let (adapter, daemon) = fixture.parts();
        let add = BuiltinIntent::add(package, label.clone()).expect("target source scope");
        super::super::commit_builtin_intent(daemon, 0xc001, &add).expect("admit target");
        adapter
            .publish_view(daemon, Some(&add))
            .expect("publish target");
        let (entered, observed) = sync_channel(1);
        let (release, released) = sync_channel(1);
        if cluster {
            // A real persisted owner identity, actual loopback endpoint and
            // empty trust policy exercise the production configured route.
            let directory = Path::new(&label).parent().expect("workspace");
            let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("reserve cluster address");
            let address = socket.local_addr().expect("local address");
            drop(socket);
            crate::cluster_owner::ClusterOwnerConfig::create(
                directory.join("cluster-owner.v1"),
                address,
                address,
            )
            .expect("persist configured cluster identity");
            adapter.owner_cluster =
                crate::builtin::cluster_dispatch::OwnerCompilerClusterRuntime::open_if_configured(
                    directory,
                    adapter.semantic_authority.store(),
                    &crate::test_support::socket_path("source-cluster"),
                )
                .expect("actual configured cluster owner")
                .map(Arc::new);
            assert!(adapter.owner_cluster.is_some());
        }
        adapter.source_worker_start_refusal = refuse_start;
        adapter.source_test_hook = Some(TestHook {
            stage,
            run: Box::new(move || {
                entered
                    .send(())
                    .expect("observe actual production source seam");
                released
                    .recv_timeout(Duration::from_secs(10))
                    .expect("bounded test release");
                assert!(!panic_after_release, "injected source preparation unwind");
            }),
        });
        let operation = backend_library::IndexOperationKey::from_bytes([0xc1; 32]).expect("key");
        adapter
            .start_index_operation(
                daemon,
                operation,
                backend_library::PackageReference::parse(label).expect("reference"),
                CompileExecutionIntent::Interactive,
                0xc002,
            )
            .expect("actual keyed source scan admission");
        let ticket = adapter
            .indexing
            .as_ref()
            .expect("live scan")
            .owner_ticket
            .clone();
        let old = daemon.engine().daemon().owner().snapshot();
        let old_view = daemon.engine().daemon().library().view().clone();
        let cursor = owner_cursor(daemon);
        let commands = Arc::new(Mutex::new(fixture.adapter.take().expect("single adapter")));
        let command_owner = Arc::clone(&commands);
        let read_head = Arc::new(Mutex::new(None));
        let observed_head = Arc::clone(&read_head);
        let owner = fixture
            .daemon
            .take()
            .expect("single daemon")
            .into_owner(move |daemon: &mut ProductDaemon, body: &[u8]| {
                let result = command_owner
                    .lock()
                    .expect("serial owner adapter")
                    .execute(daemon, body)
                    .map_err(|error| error.to_string());
                *observed_head.lock().expect("read-only head observer") =
                    Some(ReadHeadIdentity::capture(daemon));
                result
            })
            .with_deferred_commands(Box::new(crate::builtin::DeferredBuiltinCommands(
                Arc::clone(&commands),
            )));
        let endpoint = crate::test_support::socket_path("source-publication");
        let mut config = ListenerConfig::new(&endpoint);
        config.poll_interval = Duration::from_millis(1);
        config.owner_reply_timeout = Duration::from_secs(2);
        config.idle_timeout = None;
        let mut listener = UnixListenerService::bind(
            LocaldService::new(owner, crate::FrameLimits::default()).expect("production service"),
            config,
        )
        .expect("private listener");
        let stopped = listener.shutdown_handle();
        let thread = std::thread::spawn(move || {
            listener.run().expect("owner listener");
        });
        Self {
            fixture,
            commands,
            read_head,
            endpoint,
            stopped,
            thread: Some(thread),
            release: Some(release),
            entered: observed,
            old,
            old_view,
            cursor,
            operation,
            ticket,
            source,
        }
    }

    fn wait_at_source(&self) {
        self.entered
            .recv_timeout(Duration::from_secs(5))
            .expect("completed scan reaches real source worker");
    }

    fn release(&mut self) {
        if let Some(release) = self.release.take() {
            release.send(()).expect("release worker");
        }
    }

    fn assert_leases_answer_from_old_root(&self) {
        let started = Instant::now();
        let mut client = LocalSubscriptionTransport::connect_with_timeouts(
            &self.endpoint,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("independent lease client");
        let term = backend_client::lease_contract::PUBLICATION_LEASE.get();
        let LocalSubscriptionResponse::Opened { lease, cursor, .. } = client
            .open_lease(Some(self.cursor), 4, term)
            .expect("Open while source work held")
        else {
            panic!("quiet old-root Open");
        };
        assert_eq!(cursor, self.cursor.encode_control());
        assert!(matches!(
            client
                .resume_lease(lease, self.cursor, 4, term)
                .expect("actual empty cursor poll"),
            LocalSubscriptionResponse::Resumed { .. }
        ));
        client
            .renew_lease(lease, self.cursor, 4, term)
            .expect("Renew while source worker held");
        client
            .ack_lease(lease, self.cursor)
            .expect("Ack while source worker held");
        client.cancel_lease(lease).expect("close quiet lease");

        // Force fresh producer certificates and more than one reset page.
        let mut reset = LocalSubscriptionTransport::connect_with_timeouts(
            &self.endpoint,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("separate reset client");
        let LocalSubscriptionResponse::SnapshotPage {
            lease,
            payload,
            mut next,
            ..
        } = reset
            .open_lease(None, 1, term)
            .expect("reset Open while source worker held")
        else {
            panic!("production certified reset");
        };
        let verifier = WorkspaceViewProducerAdmission::from_snapshot(
            &self.old,
            backend_engine::object_version(crate::builtin::VIEW_SOURCE_VALUE),
        );
        let claim = SnapshotPageClaim::decode_with_verifier(&payload, self.cursor, None, &verifier)
            .expect("actual page certificate, no stub authority");
        let mut hydration = SnapshotHydrator::start(self.cursor, claim).expect("page admission");
        let mut pages = 1;
        while let Some(token) = next {
            let LocalSubscriptionResponse::SnapshotPage {
                payload,
                next: following,
                ..
            } = reset
                .snapshot_page(lease, token, 1)
                .expect("Page while source work held")
            else {
                panic!("production continuation page");
            };
            let claim =
                SnapshotPageClaim::decode_with_verifier(&payload, self.cursor, None, &verifier)
                    .expect("continuation certificate");
            hydration
                .push_page(claim)
                .expect("same exact reset descriptor");
            next = following;
            pages += 1;
        }
        let CursorRead::Reset { root, cursor, .. } =
            hydration.finish().expect("complete checked reset")
        else {
            panic!("checked reset root");
        };
        assert!(pages >= 2);
        assert_eq!(root.root(), self.old_view.root());
        assert_eq!(cursor, self.cursor);
        reset.ack_lease(lease, cursor).expect("ack checked reset");
        reset
            .renew_lease(lease, cursor, 4, term)
            .expect("renew checked reset");
        reset.cancel_lease(lease).expect("release reset root");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "actual wire requests remained responsive"
        );
    }

    fn wait_terminal(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if self.commands.lock().expect("observer").indexing.is_none() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "worker custody and terminal settlement"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for SourceWire {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        self.stopped.request();
        if let Some(thread) = self.thread.take() {
            thread.join().expect("all owner workers joined");
        }
        // The production service has closed before its fixture is removed.
        let _ = &self.fixture;
    }
}

#[test]
fn completed_scan_preparation_and_candidate_view_keep_real_lease_clients_responsive() {
    for stage in [
        TestStage::BeforeSourcePreparation,
        TestStage::BeforeSourceMaterialization,
        TestStage::BeforeViewPreparation,
    ] {
        let mut wire = SourceWire::start(stage, false);
        wire.wait_at_source();
        wire.assert_leases_answer_from_old_root();
        Session::connect(&wire.endpoint)
            .expect("independent cancel session")
            .surface(backend_library::SurfaceCommand::IndexCancel {
                ticket: wire.ticket.clone(),
            })
            .expect("actual owner cancellation before grant");
        wire.release();
        wire.wait_terminal();
        let mut lease = LocalSubscriptionTransport::connect(&wire.endpoint)
            .expect("old publication still readable");
        assert!(matches!(
            lease
                .open_lease(
                    Some(wire.cursor),
                    1,
                    backend_client::lease_contract::PUBLICATION_LEASE.get()
                )
                .expect("cancel preserves root"),
            LocalSubscriptionResponse::Opened { .. }
        ));
    }
}

#[test]
fn source_changed_after_materialization_never_commits_the_private_capture() {
    let mut wire = SourceWire::start(TestStage::BeforeViewPreparation, false);
    wire.wait_at_source();
    wire.assert_leases_answer_from_old_root();
    fs::write(
        &wire.source,
        "export const changed_after_materialization = 42;\n",
    )
    .expect("actual source revision mutation before grant");
    wire.release();
    wire.wait_terminal();
    let commands = wire.commands.lock().expect("same original adapter");
    let terminal = commands
        .index_terminals
        .iter()
        .find(|terminal| terminal.ticket == wire.ticket)
        .expect("exact original terminal");
    assert!(matches!(
        terminal.outcome,
        backend_library::IndexJobOutcome::Failed(_)
    ));
    let Some(JournalEntry::Retained(entry)) = commands
        .index_operations
        .entry(wire.operation)
        .expect("original journal read")
    else {
        panic!("exact operation receipt retained");
    };
    assert!(
        entry.source_capture.is_none(),
        "stale scan cannot claim SourceCaptured"
    );
}

#[test]
fn source_preparation_unwind_returns_original_writers_and_preserves_lease_service() {
    let mut wire = SourceWire::start(TestStage::BeforeViewPreparation, true);
    wire.wait_at_source();
    wire.assert_leases_answer_from_old_root();
    wire.release();
    wire.wait_terminal();
    wire.assert_leases_answer_from_old_root();
    let commands = wire.commands.lock().expect("original adapter");
    assert!(!commands.index_operations.publication_writer_reserved());
    assert!(matches!(
        commands.index_terminals.back().expect("failure").outcome,
        backend_library::IndexJobOutcome::Failed(_)
    ));
}

#[test]
fn configured_cluster_source_preparation_uses_the_same_responsive_owner_handoff() {
    let mut wire = SourceWire::start_with_cluster(TestStage::BeforeSourcePreparation, false, true);
    wire.wait_at_source();
    wire.assert_leases_answer_from_old_root();
    Session::connect(&wire.endpoint)
        .expect("cancel configured source")
        .surface(backend_library::SurfaceCommand::IndexCancel {
            ticket: wire.ticket.clone(),
        })
        .expect("cancellation is not queued behind cluster preparation");
    wire.release();
    wire.wait_terminal();
}

#[test]
fn configured_cluster_compile_and_local_fallback_follow_nonterminal_source_capture() {
    for stage in [
        TestStage::BeforeClusterCompile,
        TestStage::BeforeClusterFallback,
    ] {
        let mut wire = SourceWire::start_with_cluster(stage, false, true);
        wire.wait_at_source();
        {
            let commands = wire.commands.lock().expect("serial observer");
            let Some(JournalEntry::Retained(entry)) = commands
                .index_operations
                .entry(wire.operation)
                .expect("original journal observer")
            else {
                panic!("retained exact operation");
            };
            assert!(matches!(entry.state, StoredOperationState::Accepted));
            let capture = entry
                .source_capture
                .expect("independent source receipt installed before compile");
            assert_eq!(capture.operation_key(), wire.operation);
            assert!(capture.profiles().iter().all(|profile| matches!(
                profile.state,
                backend_library::IndexOperationSemanticProfileState::Pending { .. }
            )));
            assert!(
                commands
                    .index_terminals
                    .iter()
                    .all(|terminal| terminal.ticket != wire.ticket)
            );
        }
        let mut client = LocalSubscriptionTransport::connect_with_timeouts(
            &wire.endpoint,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("independent client while actual cluster continuation is held");
        let mut observed = client
            .acquire_publications(Arc::new(wire.old_view.clone()), wire.cursor, &|| false)
            .expect("production client admits the installed SourceCaptured root");
        assert_ne!(observed.cursor(), wire.cursor);
        client
            .resume_publications(&mut observed, &|| false)
            .expect("quiet poll during cluster work");
        client
            .renew_publications(&mut observed)
            .expect("lease renewal during cluster work");
        Session::connect(&wire.endpoint)
            .expect("actual cancel session")
            .surface(backend_library::SurfaceCommand::IndexCancel {
                ticket: wire.ticket.clone(),
            })
            .expect("cancel before any cluster or fallback candidate can select");
        wire.release();
        wire.wait_terminal();
        let commands = wire.commands.lock().expect("terminal observer");
        assert!(matches!(
            commands.index_terminals.back().expect("terminal").outcome,
            backend_library::IndexJobOutcome::Cancelled
        ));
    }
}

#[test]
fn source_worker_start_refusal_restores_custody_and_retires_scan_off_the_actor() {
    let wire = SourceWire::start_options(TestStage::BeforeSourcePreparation, false, false, true);
    wire.wait_terminal();
    wire.assert_leases_answer_from_old_root();
    let commands = wire.commands.lock().expect("same original adapter");
    assert!(!commands.index_operations.publication_writer_reserved());
    assert!(matches!(
        commands
            .index_terminals
            .back()
            .expect("original failed terminal")
            .outcome,
        backend_library::IndexJobOutcome::Failed(_)
    ));
    let Some(JournalEntry::Retained(entry)) = commands
        .index_operations
        .entry(wire.operation)
        .expect("same original receipt writer")
    else {
        panic!("exact operation retained");
    };
    assert!(entry.source_capture.is_none());
}

#[test]
fn cancellation_after_source_grant_keeps_the_exact_nonterminal_source_receipt() {
    let mut wire = SourceWire::start(TestStage::AfterGrant, false);
    wire.wait_at_source();
    wire.assert_leases_answer_from_old_root();
    Session::connect(&wire.endpoint)
        .expect("sibling cancel connection")
        .surface(backend_library::SurfaceCommand::IndexCancel {
            ticket: wire.ticket.clone(),
        })
        .expect("actual cancellation after the original grant");
    wire.release();
    wire.wait_terminal();
    let commands = wire.commands.lock().expect("original operation observer");
    let Some(JournalEntry::Retained(entry)) = commands
        .index_operations
        .entry(wire.operation)
        .expect("same original keyed journal")
    else {
        panic!("exact operation retained");
    };
    let source = entry
        .source_capture
        .expect("accepted grant wins for the independent source capture");
    assert_eq!(source.operation_key(), wire.operation);
    assert_eq!(source.workspace_sequence(), wire.old.sequence() + 1);
    assert!(matches!(
        entry.state,
        StoredOperationState::Failed {
            reason: backend_library::IndexOperationFailureReason::Cancelled,
            ..
        }
    ));
    assert!(matches!(
        commands
            .index_terminals
            .back()
            .expect("cancelled original operation")
            .outcome,
        backend_library::IndexJobOutcome::Cancelled
    ));
}

#[test]
fn completed_profile_frontier_validation_stays_off_the_real_lease_control_loop() {
    let mut wire = SourceWire::start(TestStage::BeforeFinalization, false);
    wire.wait_at_source();
    let mut client = LocalSubscriptionTransport::connect_with_timeouts(
        &wire.endpoint,
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .expect("independent client during completed-profile finalization");
    let mut observed = client
        .acquire_publications(Arc::new(wire.old_view.clone()), wire.cursor, &|| false)
        .expect("fresh admitted source capture before finalization");
    client
        .resume_publications(&mut observed, &|| false)
        .expect("quiet poll while final frontier walk is held");
    client
        .renew_publications(&mut observed)
        .expect("renew while final frontier walk is held");
    Session::connect(&wire.endpoint)
        .expect("cancel through the actual independent command lane")
        .surface(backend_library::SurfaceCommand::IndexCancel {
            ticket: wire.ticket.clone(),
        })
        .expect("cancel is responsive during finalization");
    wire.release();
    wire.wait_terminal();
}

#[test]
fn cleanup_start_refusal_retains_exact_attempts_and_cancelled_outcome_for_explicit_retry() {
    let mut wire = SourceWire::start(TestStage::AfterGrant, false);
    wire.wait_at_source();
    {
        let mut commands = wire.commands.lock().expect("original adapter");
        commands.cleanup_worker_start_refusal = true;
        commands.journal_readiness.close();
    }
    Session::connect(&wire.endpoint)
        .expect("sibling cancel")
        .surface(backend_library::SurfaceCommand::IndexCancel {
            ticket: wire.ticket.clone(),
        })
        .expect("cancel after source grant");
    wire.release();
    let deadline = Instant::now() + Duration::from_secs(10);
    let inventory = loop {
        let commands = wire.commands.lock().expect("same adapter observer");
        let inventory = commands.indexing.as_ref().and_then(|job| match &job.work {
            IndexJobWork::PublicationWorker {
                worker: None,
                completion: Some(done),
                blocked: Some(_),
            } if matches!(
                done.disposition,
                index_publication_worker::Disposition::Pending(_)
            ) =>
            {
                done.work.cleanup_inventory()
            }
            _ => None,
        });
        if let Some(inventory) = inventory {
            break inventory;
        }
        drop(commands);
        assert!(Instant::now() < deadline, "retain refused cleanup startup");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert!(
        !inventory.0.is_empty(),
        "real unselected profile attempt must survive startup refusal"
    );
    assert!(matches!(
        inventory.1,
        backend_library::IndexJobOutcome::Cancelled
    ));
    let mut client = LocalSubscriptionTransport::connect_with_timeouts(
        &wire.endpoint,
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .expect("independent client during retained cleanup");
    let mut observed = client
        .acquire_publications(Arc::new(wire.old_view.clone()), wire.cursor, &|| false)
        .expect("admit coherent source capture during retained cleanup");
    for _ in 0..4 {
        client
            .resume_publications(&mut observed, &|| false)
            .expect("quiet poll without retry");
        client
            .renew_publications(&mut observed)
            .expect("renew without cleanup retry");
    }
    {
        let commands = wire.commands.lock().expect("unchanged original plan");
        let job = commands.indexing.as_ref().expect("cleanup still pending");
        let IndexJobWork::PublicationWorker {
            worker: None,
            completion: Some(done),
            ..
        } = &job.work
        else {
            panic!("ordinary polls cannot reschedule refused cleanup");
        };
        assert_eq!(done.work.cleanup_inventory(), Some(inventory.clone()));
    }
    Session::connect(&wire.endpoint)
        .expect("explicit retry session")
        .surface(backend_library::SurfaceCommand::IndexOperationStatus {
            operation_key: wire.operation,
        })
        .expect("bounded explicit retry of original cleanup");
    wire.wait_terminal();
    let commands = wire.commands.lock().expect("original writers returned");
    assert!(matches!(
        commands.index_terminals.back().expect("terminal").outcome,
        backend_library::IndexJobOutcome::Cancelled
    ));
    assert!(!commands.index_operations.publication_writer_reserved());
    for attempt in inventory.0 {
        assert!(
            commands
                .semantic_authority
                .recover_candidate_attempt(&attempt.recovery_claim())
                .is_err(),
            "exact cancelled attempt cannot remain pending or reopen"
        );
    }
    let Some(JournalEntry::Retained(entry)) = commands
        .index_operations
        .entry(wire.operation)
        .expect("receipt")
    else {
        panic!("retained exact receipt");
    };
    assert!(matches!(
        entry.state,
        StoredOperationState::Failed {
            reason: backend_library::IndexOperationFailureReason::Cancelled,
            ..
        }
    ));
}

#[test]
fn source_edit_after_durable_prepared_barrier_is_refused_before_final_grant() {
    let mut wire = SourceWire::start(TestStage::BeforeFinalization, false);
    wire.wait_at_source();
    let (entered, observed) = sync_channel(1);
    let (release, released) = sync_channel(1);
    {
        let mut commands = wire.commands.lock().expect("same adapter");
        commands.publication_test_hook = Some(TestHook {
            stage: TestStage::BeforeGrantValidation,
            run: Box::new(move || {
                entered.send(()).expect("observe actual post-barrier seam");
                released
                    .recv_timeout(Duration::from_secs(10))
                    .expect("bounded release");
            }),
        });
    }
    wire.release();
    observed
        .recv_timeout(Duration::from_secs(10))
        .expect("final Prepared before validation");
    let journal_path = wire
        .fixture
        .root
        .0
        .join("workspace/index-operations-v1.turso");
    let journal =
        IndexOperationJournal::open(journal_path).expect("independent durable receipt read");
    let Some(JournalEntry::Retained(entry)) =
        journal.entry(wire.operation).expect("fresh durable read")
    else {
        panic!("same original operation");
    };
    assert!(
        matches!(entry.state, StoredOperationState::Prepared { .. }),
        "the held seam follows the actual durable barrier"
    );
    let original_capture = entry
        .source_capture
        .expect("source capture already coherent");
    let mut client = LocalSubscriptionTransport::connect_with_timeouts(
        &wire.endpoint,
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .expect("separate lease client");
    let mut admitted = client
        .acquire_publications(Arc::new(wire.old_view.clone()), wire.cursor, &|| false)
        .expect("actual source-capture admission");
    client
        .resume_publications(&mut admitted, &|| false)
        .expect("poll while final validation held");
    fs::write(&wire.source, "export const edited_after_barrier = 111;\n")
        .expect("real post-barrier edit");
    release.send(()).expect("allow final validation");
    wire.wait_terminal();
    let commands = wire.commands.lock().expect("original adapter");
    let Some(JournalEntry::Retained(entry)) = commands
        .index_operations
        .entry(wire.operation)
        .expect("receipt")
    else {
        panic!("exact operation retained");
    };
    assert!(
        matches!(entry.state, StoredOperationState::Failed { .. }),
        "changed input cannot record Published after the barrier"
    );
    let capture = entry
        .source_capture
        .expect("same independently captured source");
    assert_eq!(
        capture, original_capture,
        "exact durable source capture is retained"
    );
    assert_eq!(capture.operation_key(), original_capture.operation_key());
    assert!(matches!(
        commands.index_terminals.back().expect("terminal").outcome,
        backend_library::IndexJobOutcome::Failed(_)
    ));
}

#[test]
fn no_op_missing_view_witness_refreshes_off_loop_without_workspace_commit() {
    no_op_refresh_control(false);
}

#[test]
fn cancelled_no_op_missing_witness_refresh_returns_all_original_custody() {
    no_op_refresh_control(true);
}

fn no_op_refresh_control(cancel_before_approval: bool) {
    let mut wire = SourceWire::start_kind(
        TestStage::BeforeSourcePreparation,
        false,
        false,
        false,
        true,
    );
    wire.wait_at_source();
    wire.release();
    wire.wait_terminal();
    Session::connect(&wire.endpoint)
        .expect("first status session")
        .surface(backend_library::SurfaceCommand::IndexOperationStatus {
            operation_key: wire.operation,
        })
        .expect("observe actual first selected read head");
    let first_head = wire
        .read_head
        .lock()
        .expect("observed daemon")
        .clone()
        .expect("actual selected base");
    let first = {
        let commands = wire.commands.lock().expect("same original adapter");
        let Some(JournalEntry::Retained(entry)) = commands
            .index_operations
            .entry(wire.operation)
            .expect("first receipt")
        else {
            panic!("first operation retained");
        };
        let StoredOperationState::Published { receipt, .. } = entry.state else {
            panic!("structural first operation published");
        };
        receipt
    };
    let mut client = LocalSubscriptionTransport::connect_with_timeouts(
        &wire.endpoint,
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .expect("independent public lease");
    let mut observed = client
        .acquire_publications(Arc::new(wire.old_view.clone()), wire.cursor, &|| false)
        .expect("admit first structural publication");
    let before_cursor = observed.cursor();
    let before_view = observed.root();
    let (entered, at_view) = sync_channel(1);
    let (release, released) = sync_channel(1);
    {
        let mut commands = wire.commands.lock().expect("original adapter");
        // Reproduce a missing optimization witness; the actual admitted
        // immutable workspace/view remain owned by the production daemon.
        commands.published = None;
        commands.publication_test_hook = Some(TestHook {
            stage: TestStage::BeforeViewPreparation,
            run: Box::new(move || {
                entered.send(()).expect("actual no-op hydration seam");
                released
                    .recv_timeout(Duration::from_secs(10))
                    .expect("bounded release");
            }),
        });
    }
    wire.release = Some(release);
    wire.operation =
        backend_library::IndexOperationKey::from_bytes([0xc2; 32]).expect("second key");
    let package = backend_library::PackageReference::parse(
        wire.source
            .parent()
            .expect("source root")
            .to_str()
            .expect("UTF8")
            .to_owned(),
    )
    .expect("same package");
    Session::connect(&wire.endpoint)
        .expect("actual no-op start session")
        .surface(backend_library::SurfaceCommand::IndexOperationStart {
            cargo_target: None,
            operation_key: wire.operation,
            package,
            execution_intent: CompileExecutionIntent::Interactive,
        })
        .expect("admit unchanged real scan");
    at_view
        .recv_timeout(Duration::from_secs(10))
        .expect("no-op reaches worker view preparation");
    client
        .resume_publications(&mut observed, &|| false)
        .expect("quiet poll during no-op hydration");
    client
        .renew_publications(&mut observed)
        .expect("renew during no-op hydration");
    let mut sibling = LocalSubscriptionTransport::connect_with_timeouts(
        &wire.endpoint,
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .expect("sibling Open while actual hydration held");
    let term = backend_client::lease_contract::PUBLICATION_LEASE.get();
    let LocalSubscriptionResponse::Opened { lease, .. } = sibling
        .open_lease(Some(before_cursor), 1, term)
        .expect("responsive Open during no-op preparation")
    else {
        panic!("unchanged admitted root");
    };
    sibling
        .resume_lease(lease, before_cursor, 1, term)
        .expect("sibling quiet poll");
    sibling.cancel_lease(lease).expect("release sibling lease");
    if cancel_before_approval {
        let ticket = {
            let commands = wire.commands.lock().expect("same active adapter");
            let job = commands.indexing.as_ref().expect("held no-op refresh");
            assert_eq!(job.operation_key, Some(wire.operation));
            job.owner_ticket.clone()
        };
        Session::connect(&wire.endpoint)
            .expect("independent cancellation session")
            .surface(backend_library::SurfaceCommand::IndexCancel { ticket })
            .expect("actual cancellation before refresh approval");
    }
    wire.release();
    wire.wait_terminal();
    client
        .resume_publications(&mut observed, &|| false)
        .expect("quiet after coherent refresh");
    assert_eq!(observed.cursor(), before_cursor);
    assert_eq!(observed.root().root(), before_view.root());
    Session::connect(&wire.endpoint)
        .expect("terminal status session")
        .surface(backend_library::SurfaceCommand::IndexOperationStatus {
            operation_key: wire.operation,
        })
        .expect("observe actual selected read head after worker retirement");
    let after = wire
        .read_head
        .lock()
        .expect("observed daemon")
        .clone()
        .expect("actual read head");
    assert_eq!(
        after, first_head,
        "root/view/cursor/commit/sequence remain the exact admitted predecessor"
    );
    {
        let mut commands = wire.commands.lock().expect("original writers returned");
        let Some(JournalEntry::Retained(entry)) = commands
            .index_operations
            .entry(wire.operation)
            .expect("second receipt")
        else {
            panic!("same second operation");
        };
        if cancel_before_approval {
            assert!(matches!(
                entry.state,
                StoredOperationState::Failed {
                    reason: backend_library::IndexOperationFailureReason::Cancelled,
                    ..
                }
            ));
            assert!(matches!(
                commands
                    .index_terminals
                    .back()
                    .expect("original cancelled terminal")
                    .outcome,
                backend_library::IndexJobOutcome::Cancelled
            ));
        } else {
            let StoredOperationState::Published { receipt, .. } = entry.state else {
                panic!("no-op refresh publishes only its operation receipt");
            };
            assert_eq!(receipt.workspace_root(), first.workspace_root());
            assert_eq!(receipt.workspace_sequence(), first.workspace_sequence());
            assert_eq!(receipt.commit_identity(), first.commit_identity());
            assert!(
                receipt.request_identity().is_none(),
                "no synthetic workspace request"
            );
        }
        assert!(!commands.index_operations.publication_writer_reserved());
        commands
            .sql_projection
            .writer_mut()
            .expect("original projection writer returned");
        let root = first_head.workspace;
        let semantic = commands
            .semantic_authority
            .detach_index_writer(root)
            .expect("original semantic writer returned");
        let returned = commands
            .semantic_authority
            .prepare_index_writer_return(root, semantic)
            .unwrap_or_else(|_| panic!("same original semantic custody"));
        drop(returned.install());
    }
    if cancel_before_approval {
        // A subsequent real changed-source operation must use the returned
        // kernel workspace lease and all the original SQL/journal writers.
        fs::write(
            &wire.source,
            "real structural edit after cancelled refresh\n",
        )
        .expect("next real edit");
        wire.operation =
            backend_library::IndexOperationKey::from_bytes([0xc3; 32]).expect("next key");
        let package = backend_library::PackageReference::parse(
            wire.source
                .parent()
                .expect("root")
                .to_str()
                .expect("UTF8")
                .to_owned(),
        )
        .expect("same source package");
        Session::connect(&wire.endpoint)
            .expect("next actual writer session")
            .surface(backend_library::SurfaceCommand::IndexOperationStart {
                cargo_target: None,
                operation_key: wire.operation,
                package,
                execution_intent: CompileExecutionIntent::Interactive,
            })
            .expect("returned custody admits actual next write");
        wire.wait_terminal();
        let commands = wire.commands.lock().expect("same original adapter");
        let Some(JournalEntry::Retained(entry)) = commands
            .index_operations
            .entry(wire.operation)
            .expect("next receipt")
        else {
            panic!("next operation retained");
        };
        let StoredOperationState::Published { receipt, .. } = entry.state else {
            panic!("actual next write uses original returned custody");
        };
        assert!(receipt.workspace_sequence() > first_head.sequence);
        assert_ne!(receipt.workspace_root(), first_head.workspace.as_bytes());
    }
}
