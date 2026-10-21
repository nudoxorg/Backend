//! Physical workspace, durable view and original SQL writer controls. The
//! low-level settlement cases use empty-project intents; the native dispatch
//! controls below also require genuine compiled Python claims and partials.
//! None of these controls simulates process death during publication.
use super::*;
use crate::builtin::commands::adapter::index_profile_worker::{
    ProfileCompletion, ProfileOutcome, ProfileWorker,
};
use crate::builtin::commands::adapter::index_publication_worker::{
    Completion, Disposition, Event, Head, TestHook, TestStage,
};
use crate::builtin::commands::adapter::{
    FinishAddError, capture_recovery, commit_builtin_intent, prepare_builtin_intent_at_snapshot,
    selected_semantic_removals,
};
use crate::builtin::{BuiltinModelError, BuiltinSemanticChange};
use backend_engine::builtin::ProductSemanticPublicationRecord;
use backend_library::CommandReply;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::time::{Duration, Instant};

use crate::test_support::TestValue as _;
struct Release(Option<SyncSender<()>>);
impl Release {
    fn now(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}
impl Drop for Release {
    fn drop(&mut self) {
        self.now();
    }
}

fn tick(fixture: &mut AdapterFixture) {
    let (adapter, daemon) = fixture.parts();
    let _ = adapter.poll_deferred(daemon);
}

fn until(
    fixture: &mut AdapterFixture,
    mut ready: impl FnMut(&CommandAdapter, &ProductDaemon) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        tick(fixture);
        let (adapter, daemon) = fixture.parts();
        if ready(adapter, daemon) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "actual publication stage did not finish in30s"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn profile_completion_state(completion: &ProfileCompletion) -> String {
    let outcome = match &completion.outcome {
        ProfileOutcome::Cancelled => "Cancelled".to_owned(),
        ProfileOutcome::Admitted(result) => format!("Admitted({result:?})"),
    };
    format!(
        "profile={:?}, ordinal={}/{}, outcome={outcome}, read_head_present={}, semantic_writer_present={}",
        completion.work.identity.profile,
        completion.work.identity.ordinal,
        completion.work.identity.total,
        completion.work.read_head.is_some(),
        completion.work.semantic.is_some(),
    )
}

fn profile_worker_state(worker: &ProfileWorker) -> String {
    match worker {
        ProfileWorker::Running(handle) => format!("Running(finished={})", handle.is_finished()),
        ProfileWorker::Owned { handle, .. } => {
            format!(
                "Owned(finished={}, original_completion_channel)",
                handle.is_finished()
            )
        }
        ProfileWorker::Returned(completion) => {
            format!("Returned({})", profile_completion_state(completion))
        }
        ProfileWorker::Retiring { handle, completion } => format!(
            "Retiring(finished={}, {})",
            handle.is_finished(),
            profile_completion_state(completion)
        ),
    }
}

// Observe the original actor/worker channels. Reading keyed status here would
// admit an explicit retry and could hide the attempt that missed this barrier.
fn after_grant_state(
    fixture: &mut AdapterFixture,
    operation: Option<backend_library::IndexOperationKey>,
    ticket: &backend_library::IndexJobTicket,
) -> String {
    let (adapter, daemon) = fixture.parts();
    let work = adapter.indexing.as_ref().map(|job| {
        let phase = match &job.work {
            IndexJobWork::Acquiring(_) => "Acquiring".to_owned(),
            IndexJobWork::Scanning(_) => "Scanning".to_owned(),
            IndexJobWork::Compiling { worker } => {
                format!("Compiling({})", profile_worker_state(worker))
            }
            IndexJobWork::Publishing { .. } => "Publishing".to_owned(),
            IndexJobWork::PublicationWorker {
                worker,
                completion,
                blocked,
            } => {
                let completed = completion.as_ref().map(|done| {
                    let disposition = match &done.disposition {
                        Disposition::Ready => "Ready".to_owned(),
                        Disposition::Refreshed => "Refreshed".to_owned(),
                        Disposition::SourcePrepared => "SourcePrepared".to_owned(),
                        Disposition::SourceCaptured => "SourceCaptured".to_owned(),
                        Disposition::Unselected(error) => format!("Unselected({error:?})"),
                        Disposition::Pending(error) => format!("Pending({error:?})"),
                    };
                    let head = match &done.work.head {
                        Head::Writer(_) => "Writer",
                        Head::Unprepared(_) => "Unprepared",
                        Head::Pending(_) => "Pending",
                        Head::Published(_) => "Published",
                        Head::Refresh(_) => "Refresh",
                        Head::Refreshed(_) => "Refreshed",
                        Head::Moving => "Moving",
                    };
                    format!(
                        "disposition={disposition}, head={head}, proof={}, outcome={:?}",
                        done.work.proof.is_some(),
                        done.work.outcome
                    )
                });
                format!(
                    "PublicationWorker(finished={:?}, blocked={}, observation={:?}, completion={completed:?})",
                    worker.as_ref().map(|worker| worker.is_finished()),
                    blocked.is_some(),
                    worker.as_ref().map(|worker| worker.observed_state())
                )
            }
            IndexJobWork::PublicationRetiring { worker, outcome } => {
                format!(
                    "PublicationRetiring(finished={}, observation={}, outcome={outcome:?})",
                    worker.is_finished(),
                    worker.observed_state()
                )
            }
            IndexJobWork::PreparingSource { worker, .. } => format!("PreparingSource(finished={:?})", worker.as_ref().map(|worker| worker.is_finished())),
            IndexJobWork::SourceRetiring { .. } => "SourceRetiring".to_owned(),
            IndexJobWork::RetirementPending { .. } => "RetirementPending".to_owned(),
            IndexJobWork::Transition => "Transition".to_owned(),
        };
        format!(
            "ticket={:?}, operation={:?}, progress={:?}/{}, cancelled={}, work={phase}",
            job.owner_ticket,
            job.operation_key,
            job.progress_stage,
            job.progress_sequence,
            job.cancelled.load(Ordering::Acquire)
        )
    });
    let terminal = adapter
        .index_terminals
        .iter()
        .find(|terminal| &terminal.ticket == ticket);
    let capabilities = adapter.compiler.capabilities();
    let typescript = capabilities
        .for_profile(backend_semantic::vocabulary::LanguageProfile::TypeScript(
            backend_semantic::vocabulary::TypeScriptSource::TypeScript,
        ))
        .state();
    let python = capabilities
        .for_profile(backend_semantic::vocabulary::LanguageProfile::Python(
            backend_semantic::vocabulary::PythonVersion::Python314,
        ))
        .state();
    let receipt = match operation {
        Some(operation) => match adapter.index_operations.entry(operation) {
            Ok(Some(JournalEntry::Retained(entry))) => format!("Retained({entry:?})"),
            Ok(Some(JournalEntry::OutsideReceiptWindow { request_digest })) => {
                format!("OutsideReceiptWindow(request_digest={request_digest:?})")
            }
            Ok(None) => "Missing".to_owned(),
            Err(error) => format!("ReadRefused({error})"),
        },
        None => "NotRequested(ticket-only publication)".to_owned(),
    };
    format!(
        "expected_ticket={ticket:?}, operation={operation:?}, active={work:?}, terminal={terminal:?}, \
             readonly_receipt={receipt}, captured_typescript={typescript:?}, captured_python={python:?}, \
             actor_hook_pending={}, owner_root={:?}, owner_sequence={}",
        adapter.publication_test_hook.is_some(),
        daemon.engine().daemon().owner().head().root(),
        daemon.engine().daemon().owner().head().sequence()
    )
}

fn await_after_grant(
    fixture: &mut AdapterFixture,
    entered: &Receiver<()>,
    operation: backend_library::IndexOperationKey,
    ticket: &backend_library::IndexJobTicket,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        tick(fixture);
        let reason = match entered.try_recv() {
            Ok(()) => return Ok(()),
            Err(TryRecvError::Disconnected) => Some("stage channel disconnected"),
            Err(TryRecvError::Empty) => {
                let adapter = fixture.adapter.as_ref().expect("adapter");
                if adapter
                    .index_terminals
                    .iter()
                    .any(|terminal| &terminal.ticket == ticket)
                {
                    Some("original ticket became terminal before the stage")
                } else if Instant::now() >= deadline {
                    Some("original 30s stage deadline elapsed")
                } else {
                    None
                }
            }
        };
        if let Some(reason) = reason {
            return Err(format!(
                "AfterGrant was not observed: {reason}; {}",
                after_grant_state(fixture, Some(operation), ticket)
            ));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn block_at(fixture: &mut AdapterFixture, stage: TestStage) -> (Receiver<()>, Release) {
    let (entered, observed) = sync_channel(1);
    let (release, released) = sync_channel(1);
    fixture
        .adapter
        .as_mut()
        .expect("adapter")
        .publication_test_hook = Some(TestHook {
        stage,
        run: Box::new(move || {
            entered
                .send(())
                .expect("test observes actual publication stage");
            released
                .recv_timeout(Duration::from_secs(30))
                .expect("bounded stage release");
        }),
    });
    (observed, Release(Some(release)))
}

fn start(
    fixture: &mut AdapterFixture,
    keyed: bool,
    cancelled: bool,
) -> (
    backend_engine::PackageKey,
    backend_library::IndexJobTicket,
    Option<backend_library::IndexOperationKey>,
    PathBuf,
) {
    let (package, label) = fixture.add_target();
    let journal_path = fixture
        .root
        .0
        .join("workspace/publication-worker-view.journal");
    let journal =
        crate::builtin::ViewJournal::open(&journal_path).expect("actual durable view journal");
    let (adapter, daemon) = fixture.parts();
    daemon
        .engine_mut()
        .daemon_mut()
        .set_view_persistence(Box::new(journal))
        .unwrap_or_else(|_| panic!("install original view sink"));
    let reference = backend_library::PackageReference::parse(&label).expect("real local path");
    let ticket = adapter
        .issue_index_ticket(reference.clone())
        .expect("actual owner-issued ticket");
    let operation =
        keyed.then(|| backend_library::IndexOperationKey::from_bytes([0x93; 32]).expect("key"));
    if let Some(key) = operation {
        adapter
            .index_operations
            .accept(key, reference, CompileExecutionIntent::Interactive)
            .expect("actual durable acceptance before worker transfer");
    }
    let mut job = IndexJob {
        owner_ticket: ticket.clone(),
        operation_key: operation,
        captures: BTreeMap::new(),
        legacy_add: None,
        awaiters: Vec::new(),
        cancelled: Arc::new(AtomicBool::new(cancelled)),
        progress_sequence: 0,
        progress_stage: Some(backend_library::IndexJobStage::Publishing),
        request_id: 0x931,
        captured_package: package,
        captured_label: label.clone(),
        requested_package: package,
        execution_intent: CompileExecutionIntent::Interactive,
        _staged_project: None,
        work: IndexJobWork::Transition,
    };
    adapter
        .start_index_publication(
            daemon,
            &mut job,
            PreparedProductSelection {
                intent: Some(
                    BuiltinIntent::add(package, label).expect("actual empty-project intent"),
                ),
                selected: Vec::new(),
                revision_fence: None,
                profile_refusals: Box::new([]),
                partial_plan: None,
            },
        )
        .expect("transfer original writers to actual publication thread");
    adapter.indexing = Some(job);
    (package, ticket, operation, journal_path)
}

fn assert_old_read(
    fixture: &mut AdapterFixture,
    old: &backend_engine::WorkspaceSnapshot,
    old_view: &backend_engine::ViewRoot,
    old_cursor: backend_library::Cursor,
    new_package: backend_engine::PackageKey,
) {
    let seeded = fixture.package;
    let (adapter, daemon) = fixture.parts();
    let current = daemon.engine().daemon().owner().snapshot();
    assert_eq!(current.root(), old.root());
    assert_eq!(daemon.engine().daemon().library().view(), old_view);
    assert_eq!(owner_cursor(daemon), old_cursor);
    assert!(project_is_admitted(daemon, seeded));
    assert!(!project_is_admitted(daemon, new_package));
    let relation = old
        .relation::<BuiltinWorkspaceRelation>()
        .expect("retained actual relation");
    assert!(
        relation
            .lookup(seeded.as_bytes())
            .expect("old member")
            .is_some()
    );
    assert!(
        relation
            .lookup(new_package.as_bytes())
            .expect("new member absent in old read")
            .is_none()
    );
    let health = serde_json::to_vec(&backend_engine::CommandDto::new(0x932, Command::Health))
        .expect("health");
    assert!(matches!(
        adapter.execute_or_defer(daemon, &health, 0x932),
        Ok(Executed::Reply(_))
    ));
}

fn published(fixture: &mut AdapterFixture, ticket: &backend_library::IndexJobTicket) {
    let operation = fixture
        .adapter
        .as_ref()
        .expect("adapter")
        .indexing
        .as_ref()
        .filter(|job| &job.owner_ticket == ticket)
        .and_then(|job| job.operation_key);
    let started = Instant::now();
    let deadline = started + Duration::from_secs(30);
    loop {
        tick(fixture);
        if fixture
            .adapter
            .as_ref()
            .expect("adapter")
            .index_terminals
            .iter()
            .any(|terminal| &terminal.ticket == ticket)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "actual publication terminal was not observed in30s (elapsed={:?}); {}",
            started.elapsed(),
            after_grant_state(fixture, operation, ticket)
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let terminal = fixture
        .adapter
        .as_ref()
        .expect("adapter")
        .index_terminals
        .iter()
        .find(|terminal| &terminal.ticket == ticket)
        .expect("actual terminal");
    assert!(
        matches!(
            terminal.outcome,
            backend_library::IndexJobOutcome::Published
        ),
        "{terminal:?}"
    );
}

#[test]
fn index_publication_worker_after_grant_wait_reports_original_pregrant_cancellation() {
    let mut fixture = AdapterFixture::new();
    let before = fixture.parts().1.engine().daemon().owner().head().root();
    let (entered, _release) = block_at(&mut fixture, TestStage::AfterGrant);
    let (_, ticket, operation, _) = start(&mut fixture, true, true);
    let operation = operation.expect("original accepted operation");
    let detail = await_after_grant(&mut fixture, &entered, operation, &ticket)
        .expect_err("pre-grant cancellation cannot satisfy the grant barrier");
    assert!(detail.contains("AfterGrant was not observed"), "{detail}");
    assert!(
        detail.contains("Cancelled"),
        "original cancellation is diagnosed: {detail}"
    );
    let (adapter, daemon) = fixture.parts();
    assert_eq!(daemon.engine().daemon().owner().head().root(), before);
    assert!(matches!(
        adapter.index_operations.entry(operation).expect("original read-only receipt"),
        Some(JournalEntry::Retained(entry)) if matches!(entry.state,
            StoredOperationState::Failed {
                reason: backend_library::IndexOperationFailureReason::Cancelled, ..
            })
    ));
}

#[test]
fn index_publication_worker_selected_write_keeps_old_queries_and_late_cancel_loses() {
    let mut fixture = AdapterFixture::new();
    let (old, view, cursor) = {
        let (_, daemon) = fixture.parts();
        (
            daemon.engine().daemon().owner().snapshot(),
            daemon.engine().daemon().library().view().clone(),
            owner_cursor(daemon),
        )
    };
    let (entered, mut release) = block_at(&mut fixture, TestStage::AfterSelection);
    let (package, ticket, _, journal_path) = start(&mut fixture, false, false);
    until(&mut fixture, |_, _| entered.try_recv().is_ok());
    assert_old_read(&mut fixture, &old, &view, cursor, package);
    let observed = after_grant_state(&mut fixture, None, &ticket);
    assert!(
        observed.contains(&format!("expected_ticket={ticket:?}"))
            && observed.contains("PublicationWorker(finished=Some(false), blocked=false")
            && observed.contains("phase=physical-publish")
            && observed.contains("granted=true")
            && observed.contains("readonly_receipt=NotRequested(ticket-only publication)"),
        "read-only diagnostics must identify the original held ticket-only worker: {observed}"
    );
    assert_old_read(&mut fixture, &old, &view, cursor, package);
    {
        let (adapter, daemon) = fixture.parts();
        adapter
            .cancel_index_job(daemon, ticket.clone(), 0x933)
            .expect("actual late cancel request");
    }
    release.now();
    published(&mut fixture, &ticket);
    let (root, expected_view, expected_cursor) = {
        let (_, daemon) = fixture.parts();
        assert!(project_is_admitted(daemon, package));
        let snapshot = daemon.engine().daemon().owner().snapshot();
        assert_ne!(snapshot.root(), old.root());
        let capability = crate::builtin::builtin_view_capability_for_workspace(&snapshot)
            .expect("selected capability");
        let recovered = crate::builtin::ViewJournal::open(&journal_path)
            .expect("independent journal")
            .load_for_workspace(snapshot.root(), &capability)
            .expect("actual checked journal load")
            .expect("selected exact view persisted before terminal");
        assert_eq!(&recovered.view, daemon.engine().daemon().library().view());
        assert_eq!(recovered.cursor, owner_cursor(daemon));
        (snapshot.root(), recovered.view, recovered.cursor)
    };
    let mut cold = fixture.reopen_with_view_journal(Some(journal_path));
    let (_, daemon) = cold.parts();
    assert_eq!(daemon.engine().daemon().owner().snapshot().root(), root);
    assert!(project_is_admitted(daemon, package));
    // Reopen the original sink after actual retirement, restore its checked
    // view/events/cursor, then run ordinary startup repair without reindexing.
    assert_eq!(
        expected_view.root(),
        daemon.engine().daemon().library().view().root()
    );
    assert!(expected_cursor.sequence() > cursor.sequence());
    assert_eq!(owner_cursor(daemon), expected_cursor);
}

#[test]
fn index_publication_worker_sql_terminal_is_not_an_installed_owner_head() {
    let mut fixture = AdapterFixture::new();
    let old = fixture
        .daemon
        .as_ref()
        .expect("daemon")
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .root();
    let (entered, mut release) = block_at(&mut fixture, TestStage::AfterReceipt);
    let (_, ticket, operation, _) = start(&mut fixture, true, false);
    until(&mut fixture, |_, _| entered.try_recv().is_ok());
    let (adapter, daemon) = fixture.parts();
    let key = operation.expect("actual accepted key");
    assert!(
        matches!(adapter.index_operations.entry(key).expect("readonly real WAL receipt"),
        Some(JournalEntry::Retained(entry)) if matches!(entry.state, StoredOperationState::Published { .. }))
    );
    let observation = adapter
        .index_operations
        .observation(
            key,
            Some((ticket.clone(), backend_library::IndexJobStage::Publishing)),
        )
        .expect("read-only active observation")
        .expect("retained key");
    assert!(matches!(
        observation,
        backend_library::IndexOperationObservation::Known(backend_library::IndexOperationStatus {
            state: backend_library::IndexOperationState::Active { .. },
            ..
        })
    ));
    assert_eq!(daemon.engine().daemon().owner().snapshot().root(), old);
    assert!(adapter.index_terminals.iter().all(|t| t.ticket != ticket));
    release.now();
    published(&mut fixture, &ticket);
    let (adapter, daemon) = fixture.parts();
    assert_ne!(daemon.engine().daemon().owner().snapshot().root(), old);
    assert!(matches!(
        adapter
            .index_operations
            .observation(key, None)
            .expect("settled journal"),
        Some(backend_library::IndexOperationObservation::Known(
            backend_library::IndexOperationStatus {
                state: backend_library::IndexOperationState::Published(_),
                ..
            }
        ))
    ));
}

#[test]
fn index_publication_worker_selected_unwind_retains_writer_until_explicit_retry() {
    let mut fixture = AdapterFixture::new();
    let (old, view, cursor) = {
        let (_, daemon) = fixture.parts();
        (
            daemon.engine().daemon().owner().snapshot(),
            daemon.engine().daemon().library().view().clone(),
            owner_cursor(daemon),
        )
    };
    fixture
        .adapter
        .as_mut()
        .expect("adapter")
        .publication_test_hook = Some(TestHook {
        stage: TestStage::AfterSelection,
        run: Box::new(|| panic!("one-shot after actual physical selection")),
    });
    let (package, ticket, operation, _) = start(&mut fixture, true, false);
    until(&mut fixture, |adapter, _| {
        matches!(
            adapter.indexing.as_ref().map(|j| &j.work),
            Some(IndexJobWork::PublicationWorker {
                completion: Some(Completion {
                    disposition: Disposition::Pending(_),
                    ..
                }),
                ..
            })
        )
    });
    let diagnostic_bytes = std::fs::read(
        fixture.root.0.join("workspace/index-publication-pending-v1.json"),
    )
    .expect("selected failure retains a bounded private diagnostic");
    assert!(diagnostic_bytes.len() <= 8192);
    let diagnostic: serde_json::Value =
        serde_json::from_slice(&diagnostic_bytes).expect("diagnostic JSON");
    assert_eq!(diagnostic["phase"], "post-head-projection");
    assert_eq!(diagnostic["workspace_selected"], true);
    assert_eq!(diagnostic["terminal_receipt"], false);
    assert_eq!(diagnostic["readiness_authority"], false);
    assert_eq!(diagnostic["operation_key"], serde_json::to_value(operation).expect("key"));
    assert!(diagnostic["detail"].as_str().expect("error detail").contains("unwound"));
    assert_old_read(&mut fixture, &old, &view, cursor, package);
    // This selected failure precedes SQL/receipt writes. Remove every native
    // journal wake so only the caller's explicit keyed status can retry it.
    fixture
        .adapter
        .as_mut()
        .expect("adapter")
        .journal_readiness
        .close();
    {
        let (adapter, daemon) = fixture.parts();
        adapter
            .cancel_index_job(daemon, ticket.clone(), 0x934)
            .expect("post-grant cancel");
    }
    let reads = fixture
        .adapter
        .as_ref()
        .expect("adapter")
        .index_operations
        .read_query_count();
    let retry = fixture
        .adapter
        .as_ref()
        .expect("adapter")
        .journal_readiness
        .retry_token();
    for _ in 0..1000 {
        tick(&mut fixture);
    }
    let adapter = fixture.adapter.as_ref().expect("adapter");
    assert_eq!(adapter.index_operations.read_query_count(), reads);
    assert!(adapter.journal_readiness.retry_token() == retry);
    assert!(matches!(
        adapter.indexing.as_ref().map(|job| &job.work),
        Some(IndexJobWork::PublicationWorker {
            completion: Some(Completion {
                disposition: Disposition::Pending(_),
                ..
            }),
            ..
        })
    ));
    assert!(
        fixture
            .adapter
            .as_ref()
            .expect("adapter")
            .indexing
            .is_some(),
        "post-selection cancellation cannot fabricate a terminal"
    );
    let (adapter, daemon) = fixture.parts();
    let operation = operation.expect("actual accepted key");
    let command = Command::Surface(backend_library::SurfaceCommand::IndexOperationStatus {
        operation_key: operation,
    });
    let body = serde_json::to_vec(&backend_engine::CommandDto::new(0x939, command))
        .expect("actual explicit keyed status request");
    assert!(matches!(
        adapter.execute_or_defer(daemon, &body, 0x939),
        Ok(Executed::Reply(_))
    ));
    published(&mut fixture, &ticket);
    assert!(project_is_admitted(
        fixture.daemon.as_ref().expect("daemon"),
        package
    ));
}

#[test]
fn index_publication_worker_cancel_before_grant_returns_all_original_writers() {
    let mut fixture = AdapterFixture::new();
    let old = fixture
        .daemon
        .as_ref()
        .expect("daemon")
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .root();
    let (package, ticket, _, _) = start(&mut fixture, false, true);
    until(&mut fixture, |adapter, _| {
        adapter.index_terminals.iter().any(|t| t.ticket == ticket)
    });
    let (adapter, daemon) = fixture.parts();
    assert!(matches!(
        adapter
            .index_terminals
            .back()
            .expect("actual terminal")
            .outcome,
        backend_library::IndexJobOutcome::Cancelled
    ));
    assert_eq!(daemon.engine().daemon().owner().snapshot().root(), old);
    assert!(!project_is_admitted(daemon, package));
    adapter
        .sql_projection
        .writer_mut()
        .expect("original SQL writer returned");
    assert!(!adapter.index_operations.publication_writer_reserved());
    let intent = BuiltinIntent::add(
        backend_engine::package_key("next-after-cancel"),
        "next-after-cancel",
    )
    .expect("next intent");
    commit_builtin_intent(daemon, 0x935, &intent)
        .expect("actual next commit proves lease returned");
}

#[test]
fn index_publication_worker_close_joins_selected_publisher_before_owner_retires() {
    let mut fixture = AdapterFixture::new();
    let (entered, mut release) = block_at(&mut fixture, TestStage::AfterSelection);
    let (package, _, _, journal_path) = start(&mut fixture, false, false);
    until(&mut fixture, |_, _| entered.try_recv().is_ok());
    let releaser = std::thread::spawn(move || {
        release.now();
    });
    fixture.adapter.as_mut().expect("adapter").close();
    releaser.join().expect("owned test barrier retired");
    assert!(matches!(
        fixture
            .adapter
            .as_ref()
            .expect("adapter")
            .indexing
            .as_ref()
            .expect("job")
            .work,
        IndexJobWork::Transition
    ));
    let mut cold = fixture.reopen();
    let (_, daemon) = cold.parts();
    let selected = daemon.engine().daemon().owner().snapshot();
    assert!(
        project_is_admitted(daemon, package),
        "durably selected publication survives closing before owner install"
    );
    let capability =
        crate::builtin::builtin_view_capability_for_workspace(&selected).expect("capability");
    assert!(
        crate::builtin::ViewJournal::open(journal_path)
            .expect("real journal")
            .load_for_workspace(selected.root(), &capability)
            .expect("cold durable view")
            .is_some()
    );
}

#[test]
fn index_publication_worker_refused_install_retains_the_original_unselected_capability() {
    let mut fixture = AdapterFixture::new();
    let (_, _, _, _) = start(&mut fixture, false, true);
    let mut job = fixture
        .adapter
        .as_mut()
        .expect("adapter")
        .indexing
        .take()
        .expect("job");
    let IndexJobWork::PublicationWorker {
        worker: Some(worker),
        ..
    } = std::mem::replace(&mut job.work, IndexJobWork::Transition)
    else {
        panic!("actual worker")
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut done = loop {
        if let Some(event) = worker.poll() {
            match event {
                Event::Complete(done) => break done,
                Event::Grant { reply, .. } => drop(reply),
                Event::RefreshApproval { reply, .. } => drop(reply),
            }
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(matches!(done.disposition, Disposition::Unselected(_)));
    let (adapter, daemon) = fixture.parts();
    done.disposition = Disposition::Ready;
    assert!(adapter.settle_index_publication(daemon, &mut done).is_err());
    assert!(
        matches!(done.work.head, Head::Writer(_)),
        "wrong disposition must return the exact unique writer"
    );
    assert!(
        done.work.semantic.is_some()
            && done.work.journal.is_some()
            && done.work.projection.is_some()
    );
    done.disposition =
        Disposition::Unselected(BuiltinModelError("test pre-grant cancellation".to_owned()));
    let retired = adapter
        .settle_index_publication(daemon, &mut done)
        .unwrap_or_else(|_| panic!("correct settlement remains available"));
    worker.retire(done.work, retired);
    worker.join();
    commit_builtin_intent(
        daemon,
        0x936,
        &BuiltinIntent::add(
            backend_engine::package_key("next-after-refusal"),
            "next-after-refusal",
        )
        .expect("intent"),
    )
    .expect("actual next write proves refusal preserved ownership");
}

#[test]
fn index_publication_worker_grant_then_prepublication_unwind_is_failed_even_after_cancel() {
    let mut fixture = AdapterFixture::new();
    let old = fixture
        .daemon
        .as_ref()
        .expect("daemon")
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .root();
    let (entered, mut release) = block_at(&mut fixture, TestStage::AfterGrant);
    let (_, ticket, _, _) = start(&mut fixture, false, false);
    until(&mut fixture, |_, _| entered.try_recv().is_ok());
    {
        let (adapter, daemon) = fixture.parts();
        adapter
            .cancel_index_job(daemon, ticket.clone(), 0x937)
            .expect("cancel after actual grant");
    }
    // Dropping the release sender makes the hook's receive fail and unwind
    // before publish is entered, while the real grant has already linearized.
    drop(release.0.take());
    until(&mut fixture, |adapter, _| {
        adapter.index_terminals.iter().any(|t| t.ticket == ticket)
    });
    let (adapter, daemon) = fixture.parts();
    assert!(
        matches!(
            adapter
                .index_terminals
                .back()
                .expect("actual terminal")
                .outcome,
            backend_library::IndexJobOutcome::Failed(_)
        ),
        "late cancellation loses at the grant even when later preparation fails"
    );
    assert_eq!(daemon.engine().daemon().owner().snapshot().root(), old);
    commit_builtin_intent(
        daemon,
        0x938,
        &BuiltinIntent::add(
            backend_engine::package_key("next-after-grant-failure"),
            "next-after-grant-failure",
        )
        .expect("intent"),
    )
    .expect("exact grant settlement returned the original writer");
}

// These controls use the existing compiled Python producer. Only the
// Clang SDK is explicitly absent; no semantic claim is constructed by
// the test, and actual dispatch must reach the nonempty selected branch.
fn native_fixture() -> AdapterFixture {
    let mut fixture = AdapterFixture::new();
    let prior = Path::new(&fixture.label);
    fs::write(
        prior.join("pyproject.toml"),
        "[project]\nname=\"prior_native\"\nversion=\"1.0.0\"\ndependencies=[\"attrs>=23\",\"typing-extensions>=4\"]\n",
    )
    .expect("actual prior project manifest");
    fs::write(
        prior.join("prior.py"),
        "def prior_native(value: int) -> int:\n    return value + 1\n",
    )
    .expect("actual prior native source");
    install_mixed_native_compiler(&mut fixture);
    let journal = crate::builtin::ViewJournal::open(
        fixture
            .root
            .0
            .join("workspace/native-publication-view.journal"),
    )
    .expect("actual durable view sink");
    fixture
        .parts()
        .1
        .engine_mut()
        .daemon_mut()
        .set_view_persistence(Box::new(journal))
        .unwrap_or_else(|_| panic!("install original durable view writer"));
    let label = fixture.label.clone();
    let status = run_mixed_native_operation(&mut fixture, &label, 0xb1);
    assert_python_published(&status);
    let package = fixture.package;
    assert!(
        !fixture
            .parts()
            .0
            .semantic_authority
            .selected_product_keys_for_package(package)
            .expect("actual prior semantic selector read")
            .is_empty()
    );
    fixture
}

fn assert_python_published(status: &backend_library::IndexOperationStatus) {
    let capture = status
        .source_capture
        .as_ref()
        .expect("actual source receipt");
    assert!(
        capture.profiles().iter().any(|profile| {
            profile.profile.name().expect("closed language profile") == "python"
                && matches!(
                    profile.state,
                    backend_library::IndexOperationSemanticProfileState::Published {
                        coverage: backend_library::IndexOperationSemanticCoverage::Complete,
                        ..
                    }
                )
        }),
        "genuine Python publication is required: {status:?}"
    );
}

fn native_dispatch(
    fixture: &mut AdapterFixture,
    partial: bool,
    seed: u8,
) -> (
    backend_engine::PackageKey,
    String,
    backend_library::IndexOperationKey,
    backend_library::IndexJobTicket,
) {
    let (package, label) = fixture.add_target();
    fs::write(
        Path::new(&label).join("pyproject.toml"),
        "[project]\nname=\"new_native\"\nversion=\"1.0.0\"\n",
    )
    .expect("real native project manifest");
    fs::write(
        Path::new(&label).join("source.py"),
        "def published_native(value: int) -> int:\n    return value + 2\n",
    )
    .expect("real native source");
    if partial {
        fs::write(
            Path::new(&label).join("panel.c"),
            "int unavailable_sdk(int value) { return value + 2; }\n",
        )
        .expect("genuine missing-SDK profile source");
    }
    let operation = backend_library::IndexOperationKey::from_bytes([seed; 32]).expect("key");
    let command = Command::Surface(backend_library::SurfaceCommand::IndexOperationStart {
        operation_key: operation,
        package: backend_library::PackageReference::parse(label.clone()).expect("actual project"),
        execution_intent: CompileExecutionIntent::Interactive,
    });
    let body = serde_json::to_vec(&backend_engine::CommandDto::new(u64::from(seed), command))
        .expect("actual production dispatch DTO");
    let (adapter, daemon) = fixture.parts();
    assert!(matches!(
        adapter.execute_or_defer(daemon, &body, u64::from(seed)),
        Ok(Executed::Reply(_))
    ));
    let ticket = adapter
        .indexing
        .as_ref()
        .expect("actual production index job")
        .owner_ticket
        .clone();
    (package, label, operation, ticket)
}

fn keyed_status(
    fixture: &mut AdapterFixture,
    operation: backend_library::IndexOperationKey,
) -> backend_library::IndexOperationStatus {
    let command = Command::Surface(backend_library::SurfaceCommand::IndexOperationStatus {
        operation_key: operation,
    });
    let command = backend_engine::CommandDto::new(0xb90, command);
    let body = serde_json::to_vec(&command).expect("actual explicit status DTO");
    let (adapter, daemon) = fixture.parts();
    let Executed::Reply(bytes) = adapter
        .execute_or_defer(daemon, &body, 0xb90)
        .expect("actual keyed status dispatch")
    else {
        panic!("status is a read");
    };
    let reply = backend_library::decode_reply_body(&bytes).expect("actual client reply decoding");
    backend_library::admit_reply(&command, &reply).expect("exact client status admission");
    let CommandReply::Surface(backend_library::SurfaceReply::IndexOperationStatus(
        backend_library::IndexOperationObservation::Known(status),
    )) = reply.reply
    else {
        panic!("known exact keyed operation");
    };
    backend_library::SurfaceReply::IndexOperationStatus(
        backend_library::IndexOperationObservation::Known(status.clone()),
    )
    .admit(backend_library::CommandId::IndexProgress)
    .expect("public status preserves terminal/capture shape");
    status
}

fn graph_page(
    fixture: &mut AdapterFixture,
    request: backend_library::PackageGraphPageRequest,
) -> backend_library::PackageGraphPage {
    let command = Command::Surface(backend_library::SurfaceCommand::PackageGraphPage { request });
    let body = serde_json::to_vec(&backend_engine::CommandDto::new(0xb91, command))
        .expect("actual graph DTO");
    let (adapter, daemon) = fixture.parts();
    let Executed::Reply(bytes) = adapter
        .execute_or_defer(daemon, &body, 0xb91)
        .expect("actual graph dispatch")
    else {
        panic!("graph read cannot wait for the publisher");
    };
    let reply = backend_engine::decode_reply_dto(&bytes).expect("strict actual graph reply");
    match reply.reply {
        CommandReply::Surface(backend_library::SurfaceReply::PackageGraphPage(page)) => page,
        other => panic!("actual package graph page, not a projection refusal: {other:?}"),
    }
}

#[test]
fn native_graph_frontier_refuses_forgery_and_resumes_with_same_cold_owner_key() {
    let mut fixture = native_fixture();
    let (source, _) =
        crate::builtin::local_manifest::local_dependency_facts(Path::new(&fixture.label))
            .expect("real manifest")
            .expect("real selected local authority");
    let request = backend_library::PackageGraphPageRequest::new(
        source.coordinate,
        backend_library::PackageGraphDirection::Dependencies,
        Some(source.authority),
        1,
    )
    .expect("one edge page");
    let first = graph_page(&mut fixture, request.clone());
    assert_eq!(first.limit, 1);
    let backend_library::PackageGraphPageTerminal::More(cursor) = first.terminal.clone() else {
        panic!("real two-edge manifest continues");
    };
    assert_ne!(cursor.seal, [0; 32]);
    let second = graph_page(&mut fixture, request.clone().with_cursor(cursor.clone()));
    assert_eq!(second.rows.len(), 1);
    assert_ne!(first.rows[0].facts_version, second.rows[0].facts_version);

    let mut forged = cursor.clone();
    forged.after_edge_id = [0xff; 32];
    let command = Command::Surface(backend_library::SurfaceCommand::PackageGraphPage {
        request: request.clone().with_cursor(forged),
    });
    let body =
        serde_json::to_vec(&backend_engine::CommandDto::new(0xb92, command)).expect("actual wire");
    let (adapter, daemon) = fixture.parts();
    let Executed::Reply(bytes) = adapter
        .execute_or_defer(daemon, &body, 0xb92)
        .expect("actual hostile dispatch")
    else {
        panic!("read cannot queue")
    };
    let reply = backend_engine::decode_reply_dto(&bytes).expect("bounded correlated refusal");
    assert!(
        matches!(reply.reply, CommandReply::Failed(_)),
        "hostile frontier must never return an empty successful Complete page: {:?}",
        reply.reply
    );

    let journal = fixture
        .root
        .0
        .join("workspace/native-publication-view.journal");
    fixture = fixture.reopen_with_view_journal(Some(journal));
    let cold = graph_page(&mut fixture, request.with_cursor(cursor));
    assert_eq!(
        cold, second,
        "persisted key and checked roots authenticate cold continuation"
    );
}

fn native_dispatch_receipt_control(partial: bool) {
    let mut fixture = native_fixture();
    let (entered, mut release) = block_at(&mut fixture, TestStage::AfterReceipt);
    let (_, _, operation, ticket) =
        native_dispatch(&mut fixture, partial, if partial { 0xb3 } else { 0xb2 });
    // The actual scan has published its Pending source basis. Warm the graph
    // for that admitted head before its final publisher reserves the SQL writer.
    until(&mut fixture, |adapter, _| {
        matches!(
            adapter.indexing.as_ref().map(|job| &job.work),
            Some(IndexJobWork::Compiling { .. })
        )
    });
    // Graph pages address exact typed source coordinates. The manifest parser
    // publishes this local project's facts at its PURL with a Local authority;
    // a path is the operand of the separate ordinary Dependencies surface.
    let (source, facts) =
        crate::builtin::local_manifest::local_dependency_facts(Path::new(&fixture.label))
            .expect("read actual prior manifest dependency facts")
            .expect("prior native project has a declared package identity");
    assert!(
        matches!(&facts, backend_library::DependencyFacts::Known(rows) if rows.len() == 2),
        "the real prior manifest must supply both old edges: {facts:?}"
    );
    let request = backend_library::PackageGraphPageRequest::new(
        source.coordinate.clone(),
        backend_library::PackageGraphDirection::Dependencies,
        Some(source.authority),
        1,
    )
    .expect("actual bounded package-graph read");
    let first = graph_page(&mut fixture, request.clone());
    assert_eq!(
        first.rows.len(),
        1,
        "actual first old graph page: {first:?}"
    );
    assert_eq!(first.source.as_ref(), Some(&source));
    let backend_library::PackageGraphPageTerminal::More(cursor) = first.terminal.clone() else {
        panic!("two genuine manifest declarations must page");
    };
    let next = request.clone().with_cursor(cursor);
    let second = graph_page(&mut fixture, next.clone());
    assert_eq!(
        second.rows.len(),
        1,
        "actual second old graph page: {second:?}"
    );
    let prior_package = fixture.package;
    let (old, view, cursor, keys) = {
        let (adapter, daemon) = fixture.parts();
        (
            daemon.engine().daemon().owner().snapshot(),
            daemon.engine().daemon().library().view().clone(),
            owner_cursor(daemon),
            adapter
                .semantic_authority
                .selected_product_keys_for_package(prior_package)
                .expect("actual prior semantic read"),
        )
    };
    until(&mut fixture, |_, _| entered.try_recv().is_ok());
    {
        let (adapter, daemon) = fixture.parts();
        assert!(
            matches!(
                adapter.indexing.as_ref().map(|job| &job.work),
                Some(IndexJobWork::PublicationWorker { .. })
            ),
            "actual production selected dispatch must own this worker"
        );
        assert_eq!(
            daemon.engine().daemon().owner().snapshot().root(),
            old.root()
        );
        assert_eq!(daemon.engine().daemon().library().view(), &view);
        assert_eq!(owner_cursor(daemon), cursor);
        assert!(
            adapter.sql_projection.writer_mut().is_err(),
            "the original SQL writer is actually transferred"
        );
        assert_eq!(
            adapter
                .semantic_authority
                .selected_product_keys_for_package(prior_package)
                .expect("captured prior semantic selector"),
            keys
        );
        assert!(matches!(
            adapter
                .index_operations
                .entry(operation)
                .expect("actual readonly terminal receipt"),
            Some(JournalEntry::Retained(StoredOperation {
                state: StoredOperationState::Published { .. }
                    | StoredOperationState::PartiallyPublished { .. },
                ..
            }))
        ));
    }
    let refusal_progress = if partial {
        let (adapter, daemon) = fixture.parts();
        let Executed::Reply(bytes) = adapter
            .read_index_progress(daemon, ticket.clone(), 0, 0xb93)
            .expect("actual public progress before coherent publication")
        else {
            panic!("progress is a read");
        };
        let reply = backend_engine::decode_reply_dto(&bytes).expect("actual progress wire");
        let CommandReply::Surface(backend_library::SurfaceReply::IndexProgress(
            backend_library::IndexJobObservation::Pending(page),
        )) = reply.reply
        else {
            panic!("blocked publication remains pending");
        };
        assert_eq!(page.ticket, ticket);
        let failures = page
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                backend_library::IndexJobProgressKind::ProfileRefused {
                    profile, failure, ..
                } => {
                    assert_eq!(profile.name().expect("profile"), "c");
                    Some(failure.as_ref().clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            failures.len(),
            1,
            "the native refusal is visible before terminal publication"
        );
        Some(failures[0].clone())
    } else {
        None
    };
    assert_eq!(graph_page(&mut fixture, request), first);
    assert_eq!(graph_page(&mut fixture, next), second);
    assert!(
        matches!(
            keyed_status(&mut fixture, operation).state,
            backend_library::IndexOperationState::Active { .. }
        ),
        "SQL terminal cannot precede coherent owner installation"
    );
    release.now();
    until(&mut fixture, |adapter, _| adapter.indexing.is_none());
    let terminal = keyed_status(&mut fixture, operation);
    assert_python_published(&terminal);
    let receipt = if partial {
        let backend_library::IndexOperationState::PartiallyPublished {
            receipt,
            refused_profiles,
        } = &terminal.state
        else {
            panic!("actual Python+missing Clang is partial: {terminal:?}");
        };
        assert_eq!(refused_profiles.len(), 1);
        assert_eq!(refused_profiles[0].profile.name().expect("profile"), "c");
        assert!(
            refused_profiles[0]
                .compiler_failure
                .as_ref()
                .expect("actual SDK refusal")
                .requires_tool_configuration()
        );
        assert_eq!(
            refusal_progress.as_ref(),
            refused_profiles[0].compiler_failure.as_ref(),
            "early progress retains the exact eventual source-bound native failure"
        );
        receipt
    } else {
        let backend_library::IndexOperationState::Published(receipt) = &terminal.state else {
            panic!("actual native Python normal publication: {terminal:?}");
        };
        receipt
    };
    let (adapter, daemon) = fixture.parts();
    assert_eq!(
        receipt.workspace_root(),
        daemon
            .engine()
            .daemon()
            .owner()
            .snapshot()
            .root()
            .as_bytes()
    );
    assert_eq!(
        receipt.view_root(),
        daemon.engine().daemon().library().view().root().as_bytes()
    );
    assert!(
        adapter
            .index_terminals
            .iter()
            .any(|item| item.ticket == ticket)
    );
}

#[test]
fn index_publication_worker_native_dispatch_preserves_old_graph_through_receipt() {
    native_dispatch_receipt_control(false);
}

#[test]
fn index_publication_worker_native_partial_dispatch_preserves_old_graph_through_receipt() {
    native_dispatch_receipt_control(true);
}

#[test]
fn index_publication_worker_native_grant_unwind_preserves_selector_for_next_publication() {
    let mut fixture = native_fixture();
    let prior_package = fixture.package;
    let prior_keys = fixture
        .parts()
        .0
        .semantic_authority
        .selected_product_keys_for_package(prior_package)
        .expect("native prior selector");
    let (entered, mut release) = block_at(&mut fixture, TestStage::AfterGrant);
    let (package, label, operation, ticket) = native_dispatch(&mut fixture, false, 0xb4);
    await_after_grant(&mut fixture, &entered, operation, &ticket)
        .unwrap_or_else(|detail| panic!("{detail}"));
    {
        let (adapter, daemon) = fixture.parts();
        adapter
            .cancel_index_job(daemon, ticket, 0xb92)
            .expect("actual cancel after grant");
    }
    let original_root = fixture
        .parts()
        .1
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .root();
    drop(release.0.take());
    // Do not poll the owner until its worker has durably recorded the failed
    // original attempt. This observes the real read-only journal connection
    // while the original workspace/read head has not yet been returned.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (adapter, daemon) = fixture.parts();
        assert_eq!(
            daemon.engine().daemon().owner().snapshot().root(),
            original_root
        );
        if let Some(JournalEntry::Retained(entry)) = adapter
            .index_operations
            .entry(operation)
            .expect("read actual worker failure receipt before capture-only commit")
            && let StoredOperationState::Failed { reason, .. } = entry.state
        {
            assert_eq!(
                reason,
                backend_library::IndexOperationFailureReason::WorkerFailed
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "durable failed receipt did not precede writer return"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    until(&mut fixture, |adapter, _| adapter.indexing.is_none());
    let status = keyed_status(&mut fixture, operation);
    assert!(
        matches!(
            status.state,
            backend_library::IndexOperationState::Failed { .. }
        ),
        "a cancellation after grant cannot win: {status:?}"
    );
    assert!(
        status.source_capture.as_ref().is_some_and(|capture| {
            capture.profiles().iter().all(|profile| {
                matches!(
                    profile.state,
                    backend_library::IndexOperationSemanticProfileState::Unavailable { .. }
                        | backend_library::IndexOperationSemanticProfileState::Failed { .. }
                )
            })
        }),
        "failed operation must retain actual terminal capture outcomes: {status:?}"
    );
    assert_eq!(
        fixture
            .parts()
            .0
            .semantic_authority
            .selected_product_keys_for_package(prior_package)
            .expect("real semantic read must remain unpoisoned after callback unwind"),
        prior_keys
    );
    fixture
        .parts()
        .0
        .semantic_authority
        .capture_image_loader()
        .expect("admitted image selection snapshot remains readable");
    fs::write(
        Path::new(&label).join("source.py"),
        "def next_native(value: int) -> int:\n    return value + 3\n",
    )
    .expect("changed actual source forces a new native generation");
    let next = run_mixed_native_operation(&mut fixture, &label, 0xb5);
    assert!(
        matches!(
            next.state,
            backend_library::IndexOperationState::Published(_)
        ),
        "next actual publication must use the returned selector: {next:?}"
    );
    assert_python_published(&next);
    assert!(
        !fixture
            .parts()
            .0
            .semantic_authority
            .selected_product_keys_for_package(package)
            .expect("new actual semantic selection")
            .is_empty()
    );
}

#[test]
fn index_publication_worker_failed_capture_cold_retry_preserves_original_terminal() {
    failed_capture_cold_retry_control(false);
    failed_capture_cold_retry_control(true);
}

fn failed_capture_cold_retry_control(refuse_receipt_refresh: bool) {
    use backend_engine::builtin::{ProductSemanticCaptureOutcome, semantic_capture_relation};
    use backend_library::IndexOperationSemanticProfileState as ProfileState;

    let mut fixture = native_fixture();
    let (entered, mut release) = block_at(&mut fixture, TestStage::AfterGrant);
    // Actual Python selection and a genuine absent Clang SDK create two
    // Pending profiles before the selected candidate is granted.
    let (_, label, operation, ticket) = native_dispatch(&mut fixture, true, 0xc1);
    await_after_grant(&mut fixture, &entered, operation, &ticket)
        .unwrap_or_else(|detail| panic!("{detail}"));
    let (old_root, old_sequence) = {
        let (adapter, daemon) = fixture.parts();
        let head = daemon.engine().daemon().owner().head();
        let old_root = head.root();
        let old_sequence = head.sequence();
        adapter
            .recover_orphaned_package_label(daemon, &label, 0xc2)
            .expect("live worker keeps its exact Pending captures protected");
        assert_eq!(daemon.engine().daemon().owner().head().root(), old_root);
        (old_root, old_sequence)
    };
    drop(release.0.take());
    let deadline = Instant::now() + Duration::from_secs(30);
    let original = loop {
        let (adapter, daemon) = fixture.parts();
        assert_eq!(daemon.engine().daemon().owner().head().root(), old_root);
        if let Some(JournalEntry::Retained(entry)) = adapter
            .index_operations
            .entry(operation)
            .expect("original read-only failure receipt")
            && matches!(entry.state, StoredOperationState::Failed { .. })
        {
            let capture = entry
                .source_capture
                .as_ref()
                .expect("actual source receipt");
            assert_eq!(capture.profiles().len(), 2);
            assert!(
                capture
                    .profiles()
                    .iter()
                    .all(|profile| matches!(profile.state, ProfileState::Pending { .. }))
            );
            break entry;
        }
        assert!(Instant::now() < deadline, "durable Failed capture gap");
        std::thread::sleep(Duration::from_millis(1));
    };
    let original_state = serde_json::to_value(&original.state).expect("exact Failed tuple");
    let original_capture = original.source_capture.clone().expect("source capture");
    // Close/join the real worker at the exact persisted failure-before-capture
    // boundary. This models its cold durable state; it is not a process-kill
    // or crash-injection claim, and never polls the capture terminalization.
    fixture = fixture.reopen();
    let before = keyed_status(&mut fixture, operation);
    assert_eq!(before.source_capture.as_ref(), Some(&original_capture));
    assert!(matches!(
        before.state,
        backend_library::IndexOperationState::Unresolved {
            reason:
                backend_library::IndexOperationUnresolvedReason::SemanticWorkInterruptedAfterCapture,
            ..
        }
    ));
    {
        let (adapter, daemon) = fixture.parts();
        assert_eq!(daemon.engine().daemon().owner().head().root(), old_root);
        assert_eq!(
            daemon.engine().daemon().owner().head().sequence(),
            old_sequence
        );
        let relation = semantic_capture_relation(&daemon.engine().daemon().owner().snapshot())
            .expect("actual cold capture relation")
            .expect("selected capture markers");
        let reference = original.source_package().clone();
        let page = relation
            .page_from(
                &ProductSemanticPublicationKey::package_lower_bound(reference.clone()),
                8,
            )
            .expect("actual cold profile page");
        let (key, record) = page
            .entries()
            .iter()
            .find(|(key, record)| {
                key.package() == &reference
                    && record.operation_key() == Some(operation.as_bytes())
                    && matches!(
                        record.outcome(),
                        ProductSemanticCaptureOutcome::Pending { .. }
                    )
            })
            .expect("actual Pending marker for wrong-base refusal");
        capture_recovery::assert_recovery_rejects_changed_base(
            adapter,
            daemon,
            key.clone(),
            record,
        );
    }
    if refuse_receipt_refresh {
        let writer = fixture
            .parts()
            .0
            .index_operations
            .detach_publication_writer()
            .expect("hold the actual receipt writer");
        let selected_root = {
            let (adapter, daemon) = fixture.parts();
            assert!(
                adapter
                    .recover_orphaned_package_label(daemon, &label, 0xc5)
                    .is_err(),
                "actual capture commit cannot refresh a reserved receipt writer"
            );
            assert_eq!(
                daemon.engine().daemon().owner().head().sequence(),
                old_sequence + 1
            );
            daemon.engine().daemon().owner().head().root()
        };
        let unresolved = keyed_status(&mut fixture, operation);
        assert!(
            matches!(
                unresolved.state,
                backend_library::IndexOperationState::Unresolved {
                    reason:
                        backend_library::IndexOperationUnresolvedReason::ReceiptPersistenceFailed,
                    ..
                }
            ),
            "a failed receipt refresh must be observable: {unresolved:?}"
        );
        {
            let (adapter, daemon) = fixture.parts();
            assert_eq!(
                daemon.engine().daemon().owner().head().root(),
                selected_root,
                "status never publishes or repeats the capture commit"
            );
            let Some(JournalEntry::Retained(entry)) = adapter
                .index_operations
                .entry(operation)
                .expect("original durable failure after refused refresh")
            else {
                panic!("failure retained");
            };
            assert_eq!(
                serde_json::to_value(&entry.state).expect("Failed tuple"),
                original_state
            );
            assert_eq!(entry.source_capture.as_ref(), Some(&original_capture));
            adapter
                .index_operations
                .restore_publication_writer(writer)
                .unwrap_or_else(|_| panic!("restore the same original receipt writer"));
        }
        // A second cold reopen covers selected terminal markers whose receipt
        // refresh failed, without repeating compilation or the capture commit.
        fixture = fixture.reopen();
    }
    {
        let (adapter, daemon) = fixture.parts();
        assert!(
            matches!(
                adapter.start_index_operation(
                    daemon,
                    operation,
                    original.package.clone(),
                    CompileExecutionIntent::Interactive,
                    0xc3,
                ),
                Ok(Executed::Reply(_))
            ),
            "explicit retry must close the existing failure, not start another compiler"
        );
        assert!(adapter.indexing.is_none());
        assert_eq!(
            daemon.engine().daemon().owner().head().sequence(),
            old_sequence + 1,
            "both Pending profiles close in one capture-only commit"
        );
        let Some(JournalEntry::Retained(recovered)) = adapter
            .index_operations
            .entry(operation)
            .expect("recovered terminal")
        else {
            panic!("retained failure");
        };
        assert_eq!(
            serde_json::to_value(&recovered.state).expect("Failed tuple"),
            original_state
        );
    }
    let terminal = keyed_status(&mut fixture, operation);
    assert!(matches!(
        terminal.state,
        backend_library::IndexOperationState::Failed { .. }
    ));
    let capture = terminal
        .source_capture
        .as_ref()
        .expect("closed source receipt");
    assert_eq!(
        capture.commit_identity(),
        original_capture.commit_identity()
    );
    assert_eq!(capture.workspace_root(), original_capture.workspace_root());
    assert_eq!(
        capture.workspace_sequence(),
        original_capture.workspace_sequence()
    );
    assert_eq!(capture.profiles().len(), 2);
    assert!(
        capture.profiles().iter().all(|profile| matches!(
            profile.state,
            ProfileState::Unavailable { .. } | ProfileState::Failed { .. }
        )),
        "{terminal:?}"
    );
    fixture = fixture.reopen();
    assert_eq!(keyed_status(&mut fixture, operation), terminal);
    let (adapter, daemon) = fixture.parts();
    let root = daemon.engine().daemon().owner().head().root();
    adapter
        .recover_orphaned_package_label(daemon, &label, 0xc4)
        .expect("repeated explicit recovery is idempotent");
    assert_eq!(daemon.engine().daemon().owner().head().root(), root);
}

#[test]
fn index_publication_worker_panicking_payload_drop_preserves_exact_selector_and_next_commit() {
    struct PanickingPayload;
    impl Drop for PanickingPayload {
        fn drop(&mut self) {
            panic!("the caught callback payload itself panics during destruction");
        }
    }

    let mut fixture = native_fixture();
    let package = fixture.package;
    let (keys, loader, pairs) = {
        let authority = &fixture.parts().0.semantic_authority;
        let keys = authority
            .selected_product_keys_for_package(package)
            .expect("actual native prior selection");
        let loader = authority.native_history_loader_for_test();
        let pairs = keys
            .iter()
            .map(|key| {
                loader
                    .committed_pair(key)
                    .expect("exact admitted prior pair")
            })
            .collect::<Vec<_>>();
        assert!(
            !pairs.is_empty(),
            "a genuine compiled selection is required"
        );
        (keys, loader, pairs)
    };
    let destructor_unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fixture
            .parts()
            .0
            .semantic_authority
            .commit_product_selection_changes::<()>(Vec::new(), Vec::new(), || {
                std::panic::panic_any(PanickingPayload)
            })
    }));
    let destructor_unwind = destructor_unwind
        .expect_err("the outer boundary must observe the payload destructor panic");
    assert_eq!(
        destructor_unwind.downcast_ref::<&str>().copied(),
        Some("the caught callback payload itself panics during destruction"),
        "an unrelated panic cannot satisfy this destructor control"
    );
    for (key, expected) in keys.iter().zip(&pairs) {
        assert_eq!(
            &loader
                .committed_pair(key)
                .expect("payload destructor must not poison the selector"),
            expected,
            "neither callback unwind can change the exact claim/generation"
        );
        fixture
            .parts()
            .0
            .semantic_authority
            .resolve_current_selected(key)
            .expect("actual immutable semantic CAS read remains admitted");
    }
    fs::write(
        Path::new(&fixture.label).join("prior.py"),
        "def prior_native(value: int) -> int:\n    return value + 7\n",
    )
    .expect("changed genuine source for the next publication");
    let label = fixture.label.clone();
    let next = run_mixed_native_operation(&mut fixture, &label, 0xb6);
    assert!(
        matches!(
            next.state,
            backend_library::IndexOperationState::Published(_)
        ),
        "the unchanged selector must admit the next real commit: {next:?}"
    );
    assert_python_published(&next);
    let after_keys = fixture
        .parts()
        .0
        .semantic_authority
        .selected_product_keys_for_package(package)
        .expect("next admitted selector read");
    assert_eq!(after_keys, keys);
    assert!(
        keys.iter().zip(&pairs).any(|(key, prior)| {
            loader
                .committed_pair(key)
                .expect("next actual selected generation")
                != *prior
        }),
        "the next native publication must replace its actual generation"
    );
}

// Typed descriptor volume exercises the production stage boundary. Only the
// initial selection is a native compiler claim; filler rows are unavailable.
fn staged_selection_removal(
    fixture: &mut AdapterFixture,
) -> Result<PreparedProductSelection, String> {
    use backend_engine::builtin::SemanticUnavailableReason;
    use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, PythonVersion};
    let package = fixture.package;
    let label = fixture.label.clone();
    let mut keys = fixture
        .parts()
        .0
        .semantic_authority
        .selected_product_keys_for_package(package)
        .expect("actual prior selection");
    assert!(!keys.is_empty(), "native prior generation is required");
    // Revoke the synthetic descriptor selection through the same capture
    // state machine as production: Published -> Pending -> Failed(prior).
    // Reuse only the actual unchanged source evidence, with an unkeyed
    // request; this is not a second native compilation/publication claim.
    use crate::builtin::BuiltinCaptureChange;
    use backend_engine::builtin::{
        ProductSemanticCaptureOutcome, SemanticPublicationVersion, SemanticSourceCapture,
        semantic_capture_relation,
    };
    let before = fixture.parts().1.engine().daemon().owner().snapshot();
    let captures = semantic_capture_relation(&before)
        .test_value()?
        .test_value()?;
    let publications = before.relation::<BuiltinSemanticRelation>().test_value()?;
    let mut pending_changes = Vec::new();
    for key in &keys {
        let current = captures.lookup(key).test_value()?.test_value()?;
        let ProductSemanticCaptureOutcome::Published { coverage, claim } = current.outcome() else {
            panic!("actual published capture is required before selection removal");
        };
        assert_eq!(
            publications.lookup(key).test_value()?,
            Some(ProductSemanticPublicationRecord::Published { coverage, claim }),
        );
        let source = current.capture();
        let capture = SemanticSourceCapture::new(
            None,
            *source.source_version(),
            *source.input_digest(),
            source.observation_sequence(),
            source.source_count(),
        )
        .test_value()?;
        pending_changes.push(BuiltinCaptureChange {
            key: key.clone(),
            expected: Some(current),
            capture,
            outcome: ProductSemanticCaptureOutcome::Pending {
                prior: Some(SemanticPublicationVersion::new(coverage, claim)),
            },
            compiler_failure: None,
        });
    }
    let pending = BuiltinIntent::index_with_capture(
        package,
        label.clone(),
        Vec::new(),
        Vec::new(),
        pending_changes.clone(),
    )
    .test_value()?;
    commit_builtin_intent(fixture.parts().1, 0xd0, &pending).test_value()?;
    let base = fixture.parts().1.engine().daemon().owner().snapshot();
    let captures = semantic_capture_relation(&base)
        .test_value()?
        .test_value()?;
    let mut capture_changes = Vec::new();
    for change in pending_changes {
        let current = captures.lookup(&change.key).test_value()?.test_value()?;
        assert_eq!(current.capture(), change.capture);
        assert_eq!(current.outcome(), change.outcome);
        assert_eq!(current.base_workspace_root(), before.root().as_bytes());
        assert_eq!(current.source_workspace_root(), base.root().as_bytes());
        let ProductSemanticCaptureOutcome::Pending { prior: Some(prior) } = current.outcome()
        else {
            panic!("actual committed Pending capture must preserve its prior generation");
        };
        capture_changes.push(BuiltinCaptureChange {
            key: change.key,
            expected: Some(current),
            capture: change.capture,
            outcome: ProductSemanticCaptureOutcome::Failed {
                prior,
                reason: SemanticUnavailableReason::ProjectAuthority,
            },
            compiler_failure: None,
        });
    }
    let coordinate = keys[0]
        .coordinate()
        .as_str()
        .split('#')
        .next()
        .test_value()?
        .to_owned();
    let reference = backend_engine::PackageReference::parse(label.clone()).test_value()?;
    let prefix = "directory/".repeat(20);
    for index in 0..6_000 {
        keys.push(
            ProductSemanticPublicationKey::new(
                reference.clone(),
                PackageUrl::parse(format!("{coordinate}#{prefix}unavailable_{index:05}.py"))
                    .test_value()?,
                LanguageProfile::Python(PythonVersion::Python314),
            )
            .test_value()?,
        );
    }
    let intent = BuiltinIntent::index_with_capture(
        package,
        label,
        Vec::new(),
        keys.into_iter()
            .map(|key| BuiltinSemanticChange {
                key,
                after: Some(ProductSemanticPublicationRecord::Unavailable(
                    SemanticUnavailableReason::ProjectAuthority,
                )),
            })
            .collect(),
        capture_changes,
    )
    .test_value()?;
    let base = fixture.parts().1.engine().daemon().owner().snapshot();
    let staged =
        prepare_builtin_intent_at_snapshot(&base, &intent, None, Arc::new(AtomicBool::new(false)))
            .expect("actual production staging");
    assert!(
        staged.intent.staged().is_some(),
        "must cross CAS staging threshold"
    );
    assert!(
        !selected_semantic_removals(&staged.intent).is_empty(),
        "selection planner must receive retained descriptors before queue transfer"
    );
    Ok(PreparedProductSelection {
        intent: Some(intent),
        selected: Vec::new(),
        revision_fence: None,
        profile_refusals: Box::new([]),
        partial_plan: None,
    })
}

fn assert_selection_removed(
    fixture: &mut AdapterFixture,
    keys: &[ProductSemanticPublicationKey],
) -> Result<(), String> {
    let package = fixture.package;
    let (adapter, daemon) = fixture.parts();
    assert!(
        adapter
            .semantic_authority
            .selected_product_keys_for_package(package)
            .expect("actual selector after publication")
            .is_empty()
    );
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .test_value()?;
    let captures = backend_engine::builtin::semantic_capture_relation(&snapshot)
        .test_value()?
        .test_value()?;
    for key in keys {
        assert!(matches!(
            relation.lookup(key).test_value()?,
            Some(ProductSemanticPublicationRecord::Unavailable(_))
        ));
        let capture = captures.lookup(key).test_value()?.test_value()?;
        let backend_engine::builtin::ProductSemanticCaptureOutcome::Failed { prior, reason } =
            capture.outcome()
        else {
            panic!("selection removal must terminalize its actual Pending capture");
        };
        assert_eq!(
            reason,
            backend_engine::builtin::SemanticUnavailableReason::ProjectAuthority
        );
        let history = key.for_generation(prior.claim().binding().identity);
        assert_eq!(
            relation.lookup(&history).test_value()?,
            Some(ProductSemanticPublicationRecord::Published {
                coverage: prior.coverage(),
                claim: prior.claim(),
            }),
            "removing the selected generation must retain its exact prior claim in history",
        );
    }
    Ok(())
}

#[test]
fn legacy_finish_add_removes_native_selection_after_staging_before_queue_compaction()
-> Result<(), String> {
    let mut fixture = native_fixture();
    let package = fixture.package;
    let keys = fixture
        .parts()
        .0
        .semantic_authority
        .selected_product_keys_for_package(package)
        .test_value()?;
    let prepared = staged_selection_removal(&mut fixture)?;
    let reference = backend_library::PackageReference::parse(fixture.label.clone()).test_value()?;
    let (adapter, daemon) = fixture.parts();
    adapter
        .finish_add(
            daemon,
            &prepared,
            0xd1,
            package,
            reference,
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap_or_else(|error| match error {
            FinishAddError::PublicationBusy => {
                panic!("actual legacy publication refused: durable publication busy")
            }
            FinishAddError::Failed(cause) => {
                // Keep the typed refusal and exact bounded cause identity;
                // arbitrary model-error text can contain private source/URI data.
                panic!(
                    "actual legacy publication failed: BuiltinModelError (cause bytes={}, cause blake3={})",
                    cause.0.len(),
                    blake3::hash(cause.0.as_bytes()),
                )
            }
        });
    assert_selection_removed(&mut fixture, &keys)?;
    Ok(())
}

#[test]
fn publication_worker_removes_native_selection_after_staging() -> Result<(), String> {
    let mut fixture = native_fixture();
    let package = fixture.package;
    let label = fixture.label.clone();
    let keys = fixture
        .parts()
        .0
        .semantic_authority
        .selected_product_keys_for_package(package)
        .test_value()?;
    let prepared = staged_selection_removal(&mut fixture)?;
    let reference = backend_library::PackageReference::parse(label.clone()).test_value()?;
    let (adapter, daemon) = fixture.parts();
    let ticket = adapter.issue_index_ticket(reference).test_value()?;
    let mut job = IndexJob {
        owner_ticket: ticket.clone(),
        operation_key: None,
        captures: BTreeMap::new(),
        legacy_add: None,
        awaiters: Vec::new(),
        cancelled: Arc::new(AtomicBool::new(false)),
        progress_sequence: 0,
        progress_stage: Some(backend_library::IndexJobStage::Publishing),
        request_id: 0xd2,
        captured_package: package,
        captured_label: label,
        requested_package: package,
        execution_intent: CompileExecutionIntent::Interactive,
        _staged_project: None,
        work: IndexJobWork::Transition,
    };
    adapter
        .start_index_publication(daemon, &mut job, prepared)
        .test_value()?;
    adapter.indexing = Some(job);
    published(&mut fixture, &ticket);
    assert_selection_removed(&mut fixture, &keys)?;
    Ok(())
}

#[test]
fn index_publication_worker_selected_sink_error_is_observed_without_a_terminal() {
    let mut fixture = AdapterFixture::new();
    let (old, view, cursor) = {
        let (_, daemon) = fixture.parts();
        (
            daemon.engine().daemon().owner().snapshot(),
            daemon.engine().daemon().library().view().clone(),
            owner_cursor(daemon),
        )
    };
    let (entered, mut release) = block_at(&mut fixture, TestStage::AfterGrant);
    let (package, ticket, operation, journal_path) = start(&mut fixture, true, false);
    until(&mut fixture, |_, _| entered.try_recv().is_ok());
    // The real selected view sink will get an I/O refusal. This is not a
    // compiler oracle, a forged selected head, or a synthetic terminal result.
    std::fs::create_dir(&journal_path).expect("block only the owned test view sink");
    release.now();
    until(&mut fixture, |adapter, _| {
        matches!(
            adapter.indexing.as_ref().map(|job| &job.work),
            Some(IndexJobWork::PublicationWorker {
                completion: Some(Completion {
                    disposition: Disposition::Pending(_),
                    ..
                }),
                ..
            })
        )
    });
    let bytes = std::fs::read(
        fixture.root.0.join("workspace/index-publication-pending-v1.json"),
    ).expect("actual sink failure diagnostic");
    assert!(bytes.len() <= 8192);
    let diagnostic: serde_json::Value = serde_json::from_slice(&bytes).expect("diagnostic");
    assert_eq!(diagnostic["phase"], "selected-view-persistence");
    assert_eq!(diagnostic["workspace_selected"], true);
    assert_eq!(diagnostic["operation_key"], serde_json::to_value(operation).expect("key"));
    assert_eq!(diagnostic["terminal_receipt"], false);
    assert_eq!(diagnostic["readiness_authority"], false);
    assert!(!diagnostic["detail"].as_str().expect("retained actual sink error").is_empty());
    assert_old_read(&mut fixture, &old, &view, cursor, package);
    let pending_sequence = {
        let (adapter, daemon) = fixture.parts();
        let command = backend_engine::CommandDto::new(
            0x939,
            Command::Surface(backend_library::SurfaceCommand::IndexProgress {
                ticket: ticket.clone(),
                after_sequence: 0,
            }),
        );
        let body = serde_json::to_vec(&command).expect("actual public progress command");
        let Executed::Reply(bytes) = adapter
            .execute_or_defer(daemon, &body, 0x939)
            .expect("actual owner progress dispatch")
        else {
            panic!("progress is an immediate read")
        };
        let reply = backend_library::decode_reply_body(&bytes).expect("actual public reply");
        backend_library::admit_reply(&command, &reply).expect("exact public ticket admission");
        let CommandReply::Surface(backend_library::SurfaceReply::IndexProgress(
            backend_library::IndexJobObservation::Pending(page),
        )) = reply.reply
        else {
            panic!("sink error cannot become a terminal")
        };
        assert_eq!(page.stage, backend_library::IndexJobStage::Publishing);
        assert!(page.events.iter().any(|event| matches!(&event.kind,
            backend_library::IndexJobProgressKind::PublicationPending { detail, detail_truncated: false }
                if detail.as_str() == diagnostic["detail"].as_str().expect("same original sink error")
        )));
        page.next_sequence
    };
    assert!(fixture.adapter.as_ref().expect("adapter").index_terminals.iter()
        .all(|terminal| terminal.ticket != ticket));
    // A selected sink failure deliberately retains the original work until
    // a journal change or an explicit durable observation admits its retry.
    // Disable native journal wakes so recovery below must use the same-key
    // protocol read, rather than depending on timing or filesystem events.
    fixture.adapter.as_mut().expect("adapter").journal_readiness.close();
    let retry = fixture.adapter.as_ref().expect("adapter").journal_readiness.retry_token();
    std::fs::remove_dir(&journal_path).expect("restore the owned test sink");
    for _ in 0..32 {
        tick(&mut fixture);
    }
    let adapter = fixture.adapter.as_ref().expect("adapter");
    assert!(adapter.journal_readiness.retry_token() == retry);
    assert_eq!(
        adapter
            .indexing
            .as_ref()
            .expect("same active job")
            .progress_sequence,
        pending_sequence,
        "unchanged retained failure cannot manufacture progress on ordinary ticks"
    );
    assert!(matches!(adapter.indexing.as_ref().map(|job| &job.work),
        Some(IndexJobWork::PublicationWorker {
            completion: Some(Completion { disposition: Disposition::Pending(_), .. }), ..
        })), "ordinary owner ticks cannot silently retry the retained selected failure");
    assert!(adapter.index_terminals.iter().all(|terminal| terminal.ticket != ticket));
    assert_old_read(&mut fixture, &old, &view, cursor, package);
    let operation = operation.expect("original accepted durable key");
    let command = Command::Surface(backend_library::SurfaceCommand::IndexOperationStatus {
        operation_key: operation,
    });
    let body = serde_json::to_vec(&backend_engine::CommandDto::new(0x93a, command))
        .expect("real same-key durable observation authorizes one retained-work retry");
    let (adapter, daemon) = fixture.parts();
    assert!(matches!(adapter.execute_or_defer(daemon, &body, 0x93a), Ok(Executed::Reply(_))));
    assert!(adapter.journal_readiness.retry_token() != retry,
        "the actual status protocol must authorize retry, not the test helper");
    tick(&mut fixture);
    let retry_progress = fixture
        .adapter
        .as_ref()
        .expect("same owner")
        .index_progress
        .back()
        .expect("actual admitted retry milestone");
    assert_eq!(retry_progress.ticket, ticket);
    assert!(matches!(
        retry_progress.kind,
        backend_library::IndexJobProgressKind::PublicationRetryAdmitted
    ));
    assert_eq!(retry_progress.sequence, pending_sequence + 1);
    published(&mut fixture, &ticket);
    let (adapter, daemon) = fixture.parts();
    assert!(project_is_admitted(daemon, package));
    assert!(matches!(adapter.index_operations.entry(operation).expect("same durable operation"),
        Some(JournalEntry::Retained(entry)) if matches!(entry.state, StoredOperationState::Published { .. })));
    assert_eq!(adapter.index_terminals.iter().filter(|terminal| terminal.ticket == ticket).count(), 1);
}
