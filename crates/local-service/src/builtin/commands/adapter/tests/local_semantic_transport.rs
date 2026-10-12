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
fn query_calls(path: &std::path::Path) -> usize {
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
    fixture.parts().0.remote_semantic = crate::builtin::query::RemoteSemantic::select_owner(
        &producer.provision,
        &producer.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
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
    assert!(matches!(
        reply.reply,
        backend_library::CommandReply::Search(_)
    ));
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
