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

fn ready_endpoint(endpoint: Endpoint) -> Server<reconnect::Reconnecting<Endpoint>> {
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
