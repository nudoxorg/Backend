//! Production first-connect bridge under the actual MCP handler lifetime.
//! Scripted dials prove dispatch policy, not native owner startup/publication.

use super::*;
use std::collections::VecDeque;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

struct Script {
    dials: VecDeque<Result<(), ClientError>>,
    log: Vec<String>,
}

struct Endpoint {
    script: Arc<Mutex<Script>>,
    health: Arc<AtomicUsize>,
    expected: RequestContext,
    deadline: Instant,
    context: Option<RequestContext>,
}

impl reconnect::Endpoint for Endpoint {
    type Product = Fake;

    fn set_request_context(&mut self, context: Option<RequestContext>) {
        self.context = context;
    }

    fn connect(&mut self, previous: Option<Fake>) -> Result<Fake, ClientError> {
        assert!(previous.is_none(), "this is the initial product connection");
        let context = self.context.as_ref().expect("actual request admission");
        assert!(context.same_request(&self.expected));
        // This is the same production bridge called by SessionEndpoint,
        // rather than a fake Endpoint that hides the original failed dial.
        connect_session_owner_with(
            Path::new("/scope/owner.sock"),
            Some(context),
            |path| {
                assert_eq!(
                    context.read_deadline(Duration::from_secs(30))?,
                    self.deadline
                );
                let mut script = self.script.lock().expect("script");
                script.log.push(format!("dial {}", path.display()));
                script.dials.pop_front().expect("unexpected extra dial")?;
                Ok(Fake {
                    health_calls: Some(self.health.clone()),
                    ..Fake::default()
                })
            },
            || {
                assert_eq!(
                    context.read_deadline(Duration::from_secs(30))?,
                    self.deadline
                );
                self.script
                    .lock()
                    .expect("script")
                    .log
                    .push("bootstrap-ready".to_owned());
                Ok(PathBuf::from("/scope/owner.sock"))
            },
        )
    }
}

fn ready_endpoint<E: reconnect::Endpoint<Product = Fake>>(
    endpoint: E,
) -> Server<reconnect::Reconnecting<E>> {
    let mut server = Server::with_authority(
        reconnect::Reconnecting::disconnected(endpoint),
        PROJECT.to_owned(),
        [9; 32],
    );
    server.handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"initial-owner","version":"1"}}}"#).expect("initialize");
    assert!(
        server
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none()
    );
    server
}

#[test]
fn first_status_crosses_ready_then_refused_dial_before_one_health_dispatch() {
    for kind in [ErrorKind::ConnectionRefused, ErrorKind::NotFound] {
        let registry = requests::RequestRegistry::default();
        let message = json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"backend.status","arguments":{}}});
        let lease = registry
            .admit(&message)
            .expect("admission")
            .expect("request lease");
        let context = lease.context.clone();
        let deadline = context
            .read_deadline(Duration::from_secs(8))
            .expect("original caller bound");
        let script = Arc::new(Mutex::new(Script {
            dials: VecDeque::from([
                Err(ClientError::Disconnected(ErrorKind::ConnectionRefused)),
                Err(ClientError::Disconnected(kind)), // raw Ready did not preserve the listener
                Ok(()),
            ]),
            log: Vec::new(),
        }));
        let health = Arc::new(AtomicUsize::new(0));
        let mut server = ready_endpoint(Endpoint {
            script: script.clone(),
            health: health.clone(),
            expected: context.clone(),
            deadline,
            context: None,
        });
        let reply = server
            .handle_value(message, Some(context.clone()))
            .expect("one response");
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        assert_eq!(
            health.load(Ordering::Relaxed),
            1,
            "no read before authentication or hidden second command"
        );
        assert_eq!(
            context
                .read_deadline(Duration::from_secs(30))
                .expect("same scope"),
            deadline
        );
        let script = script.lock().expect("script");
        assert!(script.dials.is_empty());
        assert_eq!(
            script.log,
            [
                "dial /scope/owner.sock",
                "bootstrap-ready",
                "dial /scope/owner.sock",
                "dial /scope/owner.sock"
            ]
        );
        drop(lease);
    }
}

#[test]
fn first_connect_does_not_bootstrap_a_slow_or_refusing_authority() {
    for error in [
        ClientError::Disconnected(ErrorKind::TimedOut),
        ClientError::Disconnected(ErrorKind::WouldBlock),
        ClientError::Io("authority refused".to_owned()),
        ClientError::Protocol("foreign owner proof".to_owned()),
    ] {
        let context = RequestContext::new();
        context
            .read_deadline(Duration::from_secs(8))
            .expect("caller bound");
        let mut dials = 0;
        let result: Result<(), ClientError> = connect_session_owner_with(
            Path::new("/scope/owner.sock"),
            Some(&context),
            |_| {
                dials += 1;
                Err(error.clone())
            },
            || panic!("a live/invalid owner must never authorize a new candidate"),
        );
        assert_eq!(result, Err(error));
        assert_eq!(dials, 1);
    }
}

#[test]
fn startup_refusal_and_cancellation_remain_typed_without_read_dispatch() {
    let context = RequestContext::new();
    context
        .read_deadline(Duration::from_secs(8))
        .expect("caller bound");
    for refused in [
        ClientError::from(backend_runtime::RuntimeError::InvalidCredential(
            "/scope/secret".into(),
        )),
        map_runtime_startup_error(
            backend_runtime::RuntimeError::Io(ErrorKind::NotFound.into()),
            Some(&context),
        ),
    ] {
        let mut dials = 0;
        let mut starts = 0;
        let result: Result<(), ClientError> = connect_session_owner_with(
            Path::new("/scope/owner.sock"),
            Some(&context),
            |_| {
                dials += 1;
                assert_eq!(dials, 1, "startup failure is not another lost dial");
                Err(ClientError::Disconnected(ErrorKind::ConnectionRefused))
            },
            || {
                starts += 1;
                assert_eq!(starts, 1);
                Err(refused.clone())
            },
        );
        assert_eq!(result, Err(refused));
    }

    let result: Result<(), ClientError> = connect_session_owner_with(
        Path::new("/scope/owner.sock"),
        Some(&context),
        |_| {
            context.cancel(RequestCancellationReason::Explicit);
            Err(ClientError::Disconnected(ErrorKind::ConnectionRefused))
        },
        || panic!("cancelled original request cannot start a candidate"),
    );
    assert!(matches!(
        result,
        Err(ClientError::RequestCancelled(
            RequestCancellationReason::Explicit
        ))
    ));
}

#[test]
fn unbounded_mutation_context_keeps_one_startup_and_dial_without_a_read_deadline() {
    let context = RequestContext::new();
    let log = std::cell::RefCell::new(Vec::new());
    let result: Result<(), ClientError> = connect_session_owner_with(
        Path::new("/scope/owner.sock"),
        Some(&context),
        |_| {
            log.borrow_mut().push("dial");
            Err(ClientError::Disconnected(ErrorKind::ConnectionRefused))
        },
        || {
            log.borrow_mut().push("bootstrap");
            Ok(PathBuf::from("/scope/owner.sock"))
        },
    );
    assert_eq!(
        result,
        Err(ClientError::Disconnected(ErrorKind::ConnectionRefused))
    );
    assert_eq!(*log.borrow(), ["bootstrap", "dial"]);
    assert_eq!(context.remaining_read_budget().expect("caller scope"), None);
}

#[test]
fn startup_observation_budget_does_not_become_a_mutation_execution_deadline() {
    let mutation = RequestContext::new();
    assert_eq!(
        session_owner_startup_budget(Some(&mutation)).expect("startup observation"),
        Duration::from_secs(5)
    );
    assert_eq!(mutation.remaining_read_budget().expect("mutation"), None);
    let read = RequestContext::new();
    let deadline = read.read_deadline(Duration::from_secs(2)).expect("read");
    assert!(
        session_owner_startup_budget(Some(&read)).expect("remaining") <= Duration::from_secs(2)
    );
    assert_eq!(
        read.read_deadline(Duration::from_secs(30))
            .expect("same read"),
        deadline
    );
    for reason in [
        RequestCancellationReason::Explicit,
        RequestCancellationReason::ClientDisconnected,
        RequestCancellationReason::Shutdown,
    ] {
        let cancelled = RequestContext::new();
        cancelled.cancel(reason);
        assert_eq!(
            session_owner_startup_budget(Some(&cancelled)),
            Err(ClientError::RequestCancelled(reason))
        );
    }
}

#[cfg(unix)]
struct MutationStartupEndpoint {
    paths: backend_runtime::WorkspacePaths,
    expected: RequestContext,
    context: Option<RequestContext>,
    pending: bool,
    cancel: Option<RequestCancellationReason>,
    admitted: std::sync::mpsc::SyncSender<()>,
    dispatches: Arc<AtomicUsize>,
    reply: SurfaceReply,
}

#[cfg(unix)]
impl reconnect::Endpoint for MutationStartupEndpoint {
    type Product = Fake;

    fn set_request_context(&mut self, context: Option<RequestContext>) {
        self.context = context;
    }

    fn connect(&mut self, previous: Option<Fake>) -> Result<Fake, ClientError> {
        assert!(previous.is_none());
        let context = self.context.as_ref().expect("actual MCP request lifetime");
        assert!(context.same_request(&self.expected));
        connect_session_owner_with(
            self.paths.endpoint(),
            Some(context),
            |_| {
                assert_eq!(context.remaining_read_budget()?, None);
                Ok(Fake {
                    surface_admitted: Some(self.admitted.clone()),
                    surface_dispatches: Some(self.dispatches.clone()),
                    surface_reply: Some(self.reply.clone()),
                    ..Fake::default()
                })
            },
            || {
                assert_eq!(
                    session_owner_startup_budget(Some(context))?,
                    MCP_OWNER_STARTUP_BUDGET
                );
                if !self.pending {
                    return Ok(self.paths.endpoint().to_path_buf());
                }
                // Genuine zero-budget observation: no invented candidate PID.
                // Runtime's owned-child controls separately cover live Pending.
                let error = backend_runtime::ensure_locald_cancellable(
                    &self.paths,
                    Some(Duration::ZERO),
                    || true,
                )
                .expect_err("startup observation expired before dispatch");
                if let Some(reason) = self.cancel {
                    context.cancel(reason);
                }
                Err(map_runtime_startup_error(error, Some(context)))
            },
        )
    }
}

#[cfg(unix)]
#[test]
fn first_mutation_starting_reply_is_unsent_and_preserves_exact_key_for_explicit_retry() {
    let fixture = LazyAuthorityFixture::new();
    let key = backend_library::IndexOperationKey::from_bytes([0x72; 32]).expect("key");
    let ticket = index_job_ticket();
    let arguments = json!({"package": ticket.package().as_str(), "operation_key": key, "execution_intent":"background", "detail":"full"});
    let message = json!({"jsonrpc":"2.0", "id":7, "method":"tools/call", "params":{"name":INDEX_START_TOOL, "arguments":arguments}});
    for (pending, cancel) in [
        (true, None),
        (false, None),
        (true, Some(RequestCancellationReason::Explicit)),
    ] {
        let registry = requests::RequestRegistry::default();
        let lease = registry.admit(&message).expect("admit").expect("lease");
        let context = lease.context.clone();
        let (admitted, received) = std::sync::mpsc::sync_channel(1);
        let dispatches = Arc::new(AtomicUsize::new(0));
        let reply =
            SurfaceReply::IndexOperationStarted(backend_library::IndexOperationObservation::Known(
                backend_library::IndexOperationStatus::new(
                    key,
                    ticket.package().clone(),
                    CompileExecutionIntent::Background,
                    backend_library::IndexOperationState::Active {
                        ticket: ticket.clone(),
                        stage: IndexJobStage::Acquiring,
                    },
                ),
            ));
        let mut server = ready_endpoint(MutationStartupEndpoint {
            paths: fixture.paths.clone(),
            expected: context.clone(),
            context: None,
            pending,
            cancel,
            admitted,
            dispatches: Arc::clone(&dispatches),
            reply,
        });
        let wire = server
            .handle_value(message.clone(), Some(context.clone()))
            .expect("correlated response");
        assert_eq!(wire["id"], 7);
        if cancel.is_some() {
            assert_eq!(wire["error"]["code"], -32800);
            assert!(wire.get("result").is_none());
            assert!(received.try_recv().is_err());
        } else if pending {
            let result = &wire["result"];
            assert_eq!(result["isError"], true);
            assert_eq!(
                result["structuredContent"]["call"],
                json!({"name":INDEX_START_TOOL, "arguments":arguments})
            );
            assert!(
                result["structuredContent"].get("surface").is_none(),
                "Starting is not an accepted index operation"
            );
            assert!(
                received.try_recv().is_err(),
                "no mutating owner command before readiness"
            );
            assert_eq!(context.remaining_read_budget().expect("mutation"), None);
        } else {
            assert_eq!(wire["result"]["isError"], false, "{wire}");
            assert_eq!(
                received.try_recv(),
                Ok(()),
                "ready dispatches the original mutation once"
            );
            assert!(received.try_recv().is_err());
            assert_eq!(
                context
                    .remaining_read_budget()
                    .expect("mutation after dispatch"),
                None
            );
            assert_eq!(
                wire["result"]["structuredContent"]["index_poll"]["call"]["arguments"]["operation_key"],
                serde_json::to_value(key).expect("same key")
            );
        }
        assert_eq!(
            dispatches.load(Ordering::Relaxed),
            if pending { 0 } else { 1 },
            "exact mutation dispatch count"
        );
        assert!(!fixture.paths.data().exists());
    }
}
