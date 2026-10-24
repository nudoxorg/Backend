//! Actual owner/listener/client wait controls with a BEM2 protocol fixture.
//! These controls do not attest pretrained-model inference or ranking quality.
use super::*;
use crate::listener::{ListenerConfig, ListenerShutdown, UnixListenerService};
use crate::protocol::{EngineRequest, EngineStatus, ProtocolError};
use crate::service::{CommandOutcome, LocaldService, OwnerService};
use backend_client::{ClientError, RequestCancellationReason, RequestContext, Session};
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

struct AdapterOwner {
    fixture: AdapterFixture,
    abandoned: Arc<AtomicUsize>,
    changes: std::sync::mpsc::Receiver<BuiltinIntent>,
    cancellable_index_reads: Arc<AtomicUsize>,
}
impl OwnerService for AdapterOwner {
    fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        let (adapter, daemon) = self.fixture.parts();
        adapter
            .execute(daemon, body)
            .map_err(|error| ProtocolError::CommandExecution(error.to_string()))
    }
    fn command_or_defer(
        &mut self,
        body: &[u8],
        ticket: u64,
    ) -> Result<CommandOutcome, ProtocolError> {
        let (adapter, daemon) = self.fixture.parts();
        if backend_engine::decode_command_dto_for_owner(body, owner_cursor(daemon)).is_ok_and(
            |request| {
                matches!(
                    request.command,
                    Command::Surface(backend_library::SurfaceCommand::IndexSearch { limit: 2, .. })
                )
            },
        ) {
            self.cancellable_index_reads.fetch_add(1, Ordering::AcqRel);
        }
        adapter
            .execute_or_defer(daemon, body, ticket)
            .map(|outcome| match outcome {
                Executed::Reply(body) => CommandOutcome::Reply(body),
                Executed::Deferred => CommandOutcome::Deferred,
            })
            .map_err(|error| ProtocolError::CommandExecution(error.to_string()))
    }
    fn poll_deferred(&mut self) -> Vec<(u64, Result<Vec<u8>, ProtocolError>)> {
        let (adapter, daemon) = self.fixture.parts();
        adapter
            .poll_deferred(daemon)
            .into_iter()
            .map(|(ticket, reply)| {
                (
                    ticket,
                    reply.map_err(|error| ProtocolError::CommandExecution(error.to_string())),
                )
            })
            .collect()
    }
    fn abandon_deferred_reply(&mut self, ticket: u64) {
        self.fixture.parts().0.abandon_reply(ticket);
        self.abandoned.fetch_add(1, Ordering::AcqRel);
    }
    fn engine(&mut self, _: u64, _: EngineRequest) -> Result<EngineStatus, ProtocolError> {
        Err(ProtocolError::InvalidControl(
            "test owner has no engine control route",
        ))
    }
    fn serve_one(&mut self) -> bool {
        let Ok(intent) = self.changes.try_recv() else {
            return false;
        };
        let (adapter, daemon) = self.fixture.parts();
        super::super::commit_builtin_intent(daemon, 9001, &intent)
            .expect("actual new source generation");
        adapter
            .publish_view(daemon, Some(&intent))
            .expect("actual new selected view");
        true
    }
    fn close(&mut self) {
        self.fixture.parts().0.close();
    }
}

struct RunningOwner {
    stop: ListenerShutdown,
    thread: Option<
        std::thread::JoinHandle<Result<crate::listener::RunReport, crate::listener::ListenerError>>,
    >,
}
impl Drop for RunningOwner {
    fn drop(&mut self) {
        self.stop.request();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "owned native fixture did not reach its gate"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn query_calls(path: &Path) -> usize {
    fs::read_to_string(path)
        .expect("producer raw call log")
        .lines()
        .filter(|line| {
            serde_json::from_str::<serde_json::Value>(line).expect("producer call row")["purpose"]
                == 1
        })
        .count()
}

#[test]
fn local_semantic_unix_default_first_reply_waits_and_cancelled_waiter_reuses_worker() {
    let producer =
        crate::builtin::query::local_semantic::tests::fixture_with_mode(4096, "gate-query")
            .expect("actual supervised protocol producer");
    let mut fixture = AdapterFixture::new();
    fixture.parts().0.remote_semantic =
        crate::builtin::query::local_semantic::tests::fixture_owner(&producer);
    let directory = tempfile::Builder::new()
        .prefix("nsem-")
        .tempdir_in("/tmp")
        .expect("short private socket root");
    let path = directory.path().join("owner.sock");
    let mut config = ListenerConfig::new(&path);
    config.owner_reply_timeout = Duration::from_secs(2);
    config.poll_interval = Duration::from_millis(1);
    config.idle_timeout = None;
    let abandoned = Arc::new(AtomicUsize::new(0));
    let (changed_package, changed_label) = fixture.add_target();
    let (change_sender, changes) = std::sync::mpsc::channel();
    let service = LocaldService::new(
        AdapterOwner {
            fixture,
            abandoned: Arc::clone(&abandoned),
            changes,
            cancellable_index_reads: Arc::new(AtomicUsize::new(0)),
        },
        config.limits,
    )
    .expect("one real adapter owner service");
    let mut listener =
        UnixListenerService::bind(service, config).expect("private authenticated listener");
    let runner = RunningOwner {
        stop: listener.shutdown_handle(),
        thread: Some(std::thread::spawn(move || listener.run())),
    };

    // A short initial admission window makes the source control discriminating
    // without waiting thirty seconds. Session still uses its ordinary default API.
    let context = RequestContext::new();
    context
        .begin_default_search(Duration::from_millis(100))
        .expect("initial admission");
    let request_scope = context.clone();
    let query_path = path.clone();
    let first = std::thread::spawn(move || {
        Session::connect_with_context(query_path, request_scope)
            .expect("authenticated first session")
            .search("first intent", 3)
    });
    wait_until(|| producer.entered.exists());
    std::thread::sleep(Duration::from_millis(150));
    context
        .check()
        .expect("only actual exact owner ACK can extend the initial scope");
    assert!(
        !first.is_finished(),
        "first search cannot return lexical while actual query worker is held"
    );
    let started = Instant::now();
    Session::connect(&path)
        .expect("sibling session")
        .health()
        .expect("health while inference is held");
    assert!(started.elapsed() < Duration::from_millis(500));
    fs::write(&producer.release, b"release first actual producer").expect("release owned gate");
    let reply = first
        .join()
        .expect("first client joined")
        .expect("one ordinary first-call result");
    assert!(matches!(reply.reply, CommandReply::Search(_)));
    assert_eq!(
        reply.semantic_search_status(),
        Some(backend_library::SemanticSearchStatus::Available)
    );

    fs::remove_file(&producer.release).expect("next producer gate");
    fs::remove_file(&producer.entered).expect("fresh entry witness");
    let cancellation = RequestContext::new();
    cancellation
        .begin_default_search(Duration::from_millis(100))
        .expect("next independent scope");
    let waiting_scope = cancellation.clone();
    let query_path = path.clone();
    let cancelled = std::thread::spawn(move || {
        Session::connect_with_context(query_path, waiting_scope)
            .expect("second authenticated session")
            .search("second intent", 3)
    });
    wait_until(|| producer.entered.exists());
    let calls = query_calls(&producer.log);
    cancellation.cancel(RequestCancellationReason::Explicit);
    assert!(matches!(
        cancelled.join().expect("cancelled client joined"),
        Err(ClientError::RequestCancelled(
            RequestCancellationReason::Explicit
        ))
    ));
    wait_until(|| abandoned.load(Ordering::Acquire) > 0);
    Session::connect(&path)
        .expect("health connection after cancellation")
        .health()
        .expect("owner remains responsive");
    assert_eq!(
        query_calls(&producer.log),
        calls,
        "abandonment does not restart shared work"
    );
    fs::write(&producer.release, b"finish shared second producer")
        .expect("release retained worker");
    let warm = Session::connect(&path)
        .expect("warm ordinary session")
        .search_page("second intent", 3, None)
        .expect("abandoned producer answer is delivered through the default page API");
    assert_eq!(
        warm.semantic_search_status(),
        Some(backend_library::SemanticSearchStatus::Available)
    );
    assert_eq!(
        query_calls(&producer.log),
        calls,
        "warm request reuses the exact answer, without inference"
    );
    // Explicit caller budgets stay absolute despite the genuine owner ACK.
    fs::remove_file(&producer.release).expect("explicit-budget gate");
    fs::remove_file(&producer.entered).expect("fresh explicit-budget entry");
    let explicit = RequestContext::new();
    explicit
        .read_deadline(Duration::from_millis(100))
        .expect("caller absolute budget");
    let query_path = path.clone();
    let timed_out = std::thread::spawn(move || {
        Session::connect_with_context(query_path, explicit)
            .expect("explicit-budget session")
            .search("third intent", 3)
    });
    wait_until(|| producer.entered.exists());
    assert!(matches!(
        timed_out.join().expect("absolute waiter joined"),
        Err(ClientError::RemoteDeadlineExceeded)
    ));
    wait_until(|| abandoned.load(Ordering::Acquire) > 1);
    let calls = query_calls(&producer.log);
    fs::write(&producer.release, b"finish timed-out caller's shared work").expect("release work");
    let warm = Session::connect(&path)
        .expect("warm after timeout")
        .search("third intent", 3)
        .expect("cached complete third answer");
    assert_eq!(
        warm.semantic_search_status(),
        Some(backend_library::SemanticSearchStatus::Available)
    );
    assert_eq!(
        query_calls(&producer.log),
        calls,
        "deadline abandons only waiter, without restarting inference"
    );

    // A pending receipt keeps its original source/basis and cannot become a
    // success against the new view, even though the owner can still answer health.
    fs::remove_file(&producer.release).expect("stale-view gate");
    fs::remove_file(&producer.entered).expect("fresh stale-view entry");
    let query_path = path.clone();
    let stale = std::thread::spawn(move || {
        Session::connect(query_path)
            .expect("original selected session")
            .search("fourth intent", 3)
    });
    wait_until(|| producer.entered.exists());
    change_sender
        .send(BuiltinIntent::add(changed_package, changed_label).expect("new source intent"))
        .expect("same owner mutation lane");
    assert!(matches!(
        stale.join().expect("stale waiter joined"),
        Err(ClientError::CommandFailed(
            backend_library::CommandFailure::WrongBasis { .. }
        ))
    ));
    Session::connect(&path)
        .expect("new-view health session")
        .health()
        .expect("owner survives stale semantic work");
    drop(runner);
}

#[test]
fn cold_index_search_preparation_keeps_unix_status_and_cancel_scopes_responsive() {
    let mut fixture = AdapterFixture::new();
    let package = fixture.package;
    let label = fixture.label.clone();
    let basis = fixture.parts().1.engine().daemon().library().view().basis();
    let coordinates = (0..3)
        .map(|index| format!("{label}::src/lib.rs:{}::cold_needle_{index}", index + 1))
        .collect::<Vec<_>>();
    fixture.publish_rows(
        coordinates
            .iter()
            .enumerate()
            .map(|(index, coordinate)| {
                backend_library::Row::in_package(
                    backend_library::RowId::Symbol(backend_library::symbol_key(coordinate)),
                    basis,
                    package,
                    coordinate.clone(),
                )
                .with_source(
                    backend_library::SourceLocation::new("src/lib.rs", index as u32 + 1)
                        .expect("captured source"),
                )
            })
            .collect(),
    );
    let (changed_package, changed_label) = fixture.add_target();
    let change = BuiltinIntent::add(changed_package, changed_label).expect("actual source change");
    let expected_root = fixture.parts().1.engine().daemon().library().view().root();
    let (entered, release) = fixture.parts().0.local_search_lane.hold_next();
    let directory = tempfile::Builder::new()
        .prefix("nis-")
        .tempdir_in("/tmp")
        .expect("private short socket root");
    let path = directory.path().join("owner.sock");
    let mut config = ListenerConfig::new(&path);
    config.owner_reply_timeout = Duration::from_secs(2);
    config.poll_interval = Duration::from_millis(1);
    config.idle_timeout = None;
    let (change_sender, changes) = std::sync::mpsc::channel();
    let cancellable_index_reads = Arc::new(AtomicUsize::new(0));
    let service = LocaldService::new(
        AdapterOwner {
            fixture,
            abandoned: Arc::new(AtomicUsize::new(0)),
            changes,
            cancellable_index_reads: Arc::clone(&cancellable_index_reads),
        },
        config.limits,
    )
    .expect("real owner service");
    let mut listener = UnixListenerService::bind(service, config).expect("authenticated listener");
    let runner = RunningOwner {
        stop: listener.shutdown_handle(),
        thread: Some(std::thread::spawn(move || listener.run())),
    };
    let query = backend_library::SurfaceCommand::IndexSearch {
        query: backend_library::ProductText::from_static("cold_needle"),
        limit: 1,
        cursor: None,
    };
    let context = RequestContext::new();
    context
        .read_deadline(Duration::from_secs(3))
        .expect("one bounded read scope");
    let query_path = path.clone();
    let original = query.clone();
    let first = std::thread::spawn(move || {
        Session::connect_with_context(query_path, context)
            .expect("query client")
            .surface(original)
    });
    entered
        .recv_timeout(Duration::from_secs(2))
        .expect("actual cold worker is held");
    assert!(
        !first.is_finished(),
        "ordinary first-use call waits for its complete result"
    );
    // A separate authenticated connection proves that native preparation holds
    // no owner mutex; this is the real Session/listener path, not a helper timer.
    let control = RequestContext::new();
    control
        .read_deadline(Duration::from_secs(2))
        .expect("bounded independent status");
    Session::connect_with_context(&path, control)
        .expect("independent status client")
        .health()
        .expect("owner status while build is held");

    let cancelled = RequestContext::new();
    cancelled
        .read_deadline(Duration::from_secs(2))
        .expect("independent cancellation scope");
    let scope = cancelled.clone();
    let query_path = path.clone();
    let cancelled_reader = std::thread::spawn(move || {
        Session::connect_with_context(query_path, scope)
            .expect("independent cancellable client")
            .surface(backend_library::SurfaceCommand::IndexSearch {
                query: backend_library::ProductText::from_static("cold_needle"),
                limit: 2,
                cursor: None,
            })
    });
    wait_until(|| cancellable_index_reads.load(Ordering::Acquire) > 0);
    cancelled.cancel(RequestCancellationReason::Explicit);
    assert!(matches!(
        cancelled_reader.join().expect("actual cancelled client"),
        Err(ClientError::RequestCancelled(
            RequestCancellationReason::Explicit
        ))
    ));
    let mut observer_reader =
        Session::connect(&path).expect("independent observer-cancelled reader");
    observer_reader.observe_query_preparation(|_| false);
    assert!(
        matches!(observer_reader.surface(backend_library::SurfaceCommand::IndexSearch {
        query: backend_library::ProductText::from_static("cold_needle"), limit: 2, cursor: None,
    }), Err(ClientError::CommandFailed(backend_library::CommandFailure::InvalidQuery(detail)))
        if detail == "query preparation wait cancelled")
    );
    assert!(
        !first.is_finished(),
        "observer cancellation affects only its read, not the held shared worker"
    );
    let expired = RequestContext::new();
    expired
        .read_deadline(Duration::from_millis(80))
        .expect("independent short deadline");
    assert!(matches!(
        Session::connect_with_context(&path, expired)
            .expect("deadline client")
            .surface(backend_library::SurfaceCommand::IndexSearch {
                query: backend_library::ProductText::from_static("cold_needle"),
                limit: 3,
                cursor: None,
            }),
        Err(ClientError::RemoteDeadlineExceeded)
    ));
    assert!(
        !first.is_finished(),
        "other reader cancellation/deadline cannot retire shared work"
    );
    Session::connect(&path)
        .expect("status after cancelled and expired readers")
        .health()
        .expect("owner remains responsive");
    release.send(()).expect("finish actual cold native build");
    let first_page = first
        .join()
        .expect("original first-use reader")
        .expect("complete ordinary reply");
    let backend_library::SurfaceReply::IndexSearchPage(page) = first_page else {
        panic!("index page");
    };
    assert_eq!(page.hits.len(), 1);
    let issued = page
        .next_cursor
        .clone()
        .expect("real current-view continuation");
    match &page.hits[0] {
        backend_library::RegistrySearchHit::LocalDeclaration(record) => {
            assert_eq!(record.coordinate.as_str(), coordinates[0]);
            assert_eq!(record.name.as_str(), "cold_needle_0");
            assert_eq!(
                record.source,
                backend_library::LocalDeclarationSource::Captured {
                    path: backend_library::ProductText::from_static("src/lib.rs"),
                    line: std::num::NonZeroU32::new(1).expect("line"),
                }
            );
        }
        other => panic!("local source cannot become a registry release: {other:?}"),
    }
    let mut all = page.hits.to_vec();
    let mut cursor = Some(issued.clone());
    while let Some(current) = cursor {
        let reply = Session::connect(&path)
            .expect("continuation client")
            .surface(backend_library::SurfaceCommand::IndexSearch {
                query: backend_library::ProductText::from_static("cold_needle"),
                limit: 1,
                cursor: Some(current),
            })
            .expect("original exact continuation");
        let backend_library::SurfaceReply::IndexSearchPage(page) = reply else {
            panic!("index page");
        };
        assert_eq!(page.hits.len(), 1);
        all.extend(page.hits.iter().cloned());
        cursor = page.next_cursor;
        assert!(all.len() <= 3, "complete continuation cannot repeat a row");
    }
    let actual = all
        .iter()
        .map(|hit| match hit {
            backend_library::RegistrySearchHit::LocalDeclaration(record) => {
                record.coordinate.as_str()
            }
            other => panic!("unexpected result plane: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        coordinates.iter().map(String::as_str).collect::<Vec<_>>(),
        "cold client wait preserves the full exact ordered row union"
    );
    let warm = Session::connect(&path)
        .expect("warm client")
        .surface(query)
        .expect("warm exact result");
    let backend_library::SurfaceReply::IndexSearchPage(warm) = warm else {
        panic!("warm page");
    };
    assert_eq!(
        warm.hits[0], all[0],
        "warm query keeps the same exact first source row"
    );

    change_sender
        .send(change)
        .expect("actual owner source change");
    wait_until(|| {
        Session::connect(&path)
            .expect("changed view client")
            .revision()
            .expect("actual current revision")
            .root
            != expected_root
    });
    let stale = backend_library::SurfaceCommand::IndexSearch {
        query: backend_library::ProductText::from_static("cold_needle"),
        limit: 1,
        cursor: Some(issued),
    };
    assert!(
        matches!(Session::connect(&path).expect("old issued cursor client").surface(stale),
        Err(ClientError::CommandFailed(backend_library::CommandFailure::InvalidQuery(detail)))
            if detail.contains("cursor does not match")),
        "client preparation retries never repair or clear an old issued cursor"
    );
    drop(runner);
}
