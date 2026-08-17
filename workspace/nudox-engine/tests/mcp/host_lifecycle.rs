//! `McpHost` — the synchronous lifecycle `lindsey` drives (docs/LIMITATIONS.md L35).
//!
//! `tests/endpoint.rs` covers the async [`McpEndpoint`] surface. This file
//! covers the wrapper a host with **no runtime of its own** uses, because that
//! is the surface the GUI actually calls and the one whose failure modes reach
//! a user: a server that never bound, a server that was stopped, and an address
//! that outlived the socket behind it.
//!
//! # Everything here talks to a real socket
//!
//! Assertions are on whether a `TcpStream::connect` to the advertised address
//! succeeds or is refused — not on whether a field is `Some`. A field-shaped
//! test would pass against the exact stub L35 describes.
//!
//! Blocking `std::net`, not `tokio::net`: a host has no runtime, so the test
//! that stands in for it must not need one either.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

use nudox_engine::{Engine, EngineConfig, EngineHandle};
use nudox_engine::mcp::{AccountGate, McpHost, SessionToken, ShutdownOutcome};
use nudox_engine::store::source::fixtures::FixtureSource;

/// A JSON-RPC `initialize` call — the first thing any MCP client sends.
const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"host-lifecycle-test","version":"0"}}}"#;

/// An engine over the deterministic fixture corpus.
///
/// Note there is no `Runtime` here, unlike every test in `endpoint.rs`. That
/// absence *is* the property under test: `McpHost` must work for a caller who
/// has none.
fn engine() -> EngineHandle {
    Engine::start(EngineConfig::default(), FixtureSource::rich())
}

/// The account gate these tests run under.
///
/// Unmetered on purpose, and stated rather than defaulted. This file is about
/// the *lifecycle* — bind, advertise, stop, drop — and every one of its
/// assertions is on whether a socket is reachable. Wiring a real account gate
/// in would make each of them additionally depend on `api.nudox.org` being up,
/// which is the shape doctrine §4 rules out. Account behaviour has its own
/// suite: `tests/account_against_a_fake_service.rs`.
fn test_gate() -> AccountGate {
    AccountGate::unmetered("lifecycle test: asserts on socket reachability, not on billing")
}

fn case_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Parse `http://127.0.0.1:PORT/mcp` back into the address a client dials.
///
/// Tests dial the *advertised string*, not the `SocketAddr` beside it, so a
/// URL that is malformed or points somewhere else fails here rather than
/// looking correct in a `Debug` print.
fn dial_target(url: &str) -> SocketAddr {
    let rest = url
        .strip_prefix("http://")
        .unwrap_or_else(|| panic!("advertised url must be http: {url}"));
    let authority = rest
        .split_once('/')
        .map_or(rest, |(authority, _path)| authority);
    authority
        .parse()
        .unwrap_or_else(|e| panic!("advertised authority {authority:?} must be dialable: {e}"))
}

/// POST one JSON-RPC message to `url` over a blocking socket and return the
/// raw response bytes. `None` when the connection was refused.
fn post(url: &str, token: &str, body: &str) -> Option<String> {
    let addr = dial_target(url);
    let path = url
        .strip_prefix("http://")
        .and_then(|rest| rest.find('/').map(|ix| &rest[ix..]))
        .unwrap_or(nudox_engine::mcp::MCP_PATH);

    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Content-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        len = body.len(),
    );

    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout must apply");
    stream
        .write_all(request.as_bytes())
        .expect("request must send");
    stream.flush().expect("request must flush");

    let mut raw = Vec::new();
    // A read timeout surfaces as an error after some bytes have arrived; the
    // status line is always in the first frame, so partial reads are fine.
    let _ = stream.read_to_end(&mut raw);
    Some(String::from_utf8_lossy(&raw).into_owned())
}

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

/// The L35 invariant at its smallest: a host with no runtime can start the
/// server, and the address it is handed is one a client connects to and gets an
/// MCP answer from.
#[test]
fn a_host_without_a_runtime_starts_a_server_a_client_can_talk_to() {
    let (_, _cost) = heart::cost::measured(
        "mcp_host/fixtures/sync_start_is_reachable",
        case_dir(),
        || {
            let engine = engine();
            let token = SessionToken::generate();
            let secret = token.expose().to_owned();

            let mut host = McpHost::start_with_token(&engine, test_gate(), token)
                .expect("a host must be able to start the server without owning a runtime");

            let url = host.url().expect("a running host must advertise a url");
            let response = post(&url, &secret, INITIALIZE)
                .unwrap_or_else(|| panic!("the advertised url {url} refused a connection"));

            assert!(
                response.starts_with("HTTP/1.1 200"),
                "initialize over the advertised url must succeed, got:\n{response}",
            );
            assert!(
                response.contains("\"protocolVersion\":\"2025-11-25\""),
                "the response must be a real MCP handshake, not just an open socket:\n{response}",
            );

            assert_eq!(host.stop(), ShutdownOutcome::Drained);
            drop(engine);
        },
    );
}

/// §L6's reason for the ephemeral port, restated for the sync path: two hosts
/// in one process must not collide, so "the port is already taken" is not a
/// reachable failure — the kernel picks a free one per bind.
#[test]
fn two_hosts_in_one_process_bind_different_ports() {
    let engine = engine();

    let mut first = McpHost::start(&engine, test_gate()).expect("first host must bind");
    let mut second = McpHost::start(&engine, test_gate()).expect("second host must bind");

    let a = first.addr().expect("first host must report its address");
    let b = second.addr().expect("second host must report its address");
    assert_ne!(a.port(), b.port(), "ephemeral ports must not collide");
    assert!(a.ip().is_loopback() && b.ip().is_loopback());

    // Both are simultaneously live — a bind that silently reused the first
    // port would answer here from one server.
    assert!(
        TcpStream::connect_timeout(&a, Duration::from_secs(2)).is_ok(),
        "first host must still be accepting while the second runs",
    );
    assert!(TcpStream::connect_timeout(&b, Duration::from_secs(2)).is_ok());

    assert_eq!(first.stop(), ShutdownOutcome::Drained);
    assert_eq!(second.stop(), ShutdownOutcome::Drained);
    drop(engine);
}

// ---------------------------------------------------------------------------
// Stop
// ---------------------------------------------------------------------------

/// Window close. The socket must actually close — and the host must stop
/// advertising an address, because a stale address in the status bar is worse
/// than none: it sends a reader to a port that may since have been reused.
#[test]
fn stopping_closes_the_socket_and_retracts_the_advertised_address() {
    let engine = engine();
    let mut host = McpHost::start(&engine, test_gate()).expect("host must bind");

    let url = host.url().expect("a running host advertises a url");
    let addr = dial_target(&url);
    assert!(
        TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok(),
        "precondition: the server is accepting before stop",
    );

    assert_eq!(host.stop(), ShutdownOutcome::Drained);

    assert_eq!(host.url(), None, "a stopped host must advertise nothing");
    assert_eq!(host.addr(), None);
    assert_eq!(host.client_config_snippet(), None);

    // The listener is gone. Retried, because the accept loop unwinds
    // asynchronously after the cancellation token fires.
    let refused = (0..50).any(|_| {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
        false
    });
    assert!(refused, "the socket at {addr} must stop accepting after stop");

    drop(engine);
}

/// Idempotence, as a typed answer rather than a silent second no-op: a caller
/// that stops twice learns which of its calls did the work.
#[test]
fn stopping_twice_reports_that_the_second_call_did_nothing() {
    let engine = engine();
    let mut host = McpHost::start(&engine, test_gate()).expect("host must bind");

    assert_eq!(host.stop(), ShutdownOutcome::Drained);
    assert_eq!(host.stop(), ShutdownOutcome::AlreadyStopped);
    assert_eq!(host.stop(), ShutdownOutcome::AlreadyStopped);

    drop(engine);
}

/// A host that is dropped without `stop` must still close its listener.
///
/// This is the path that actually runs when a window closes without a
/// lifecycle hook firing, so "we cancel in `Drop`" cannot be left as a comment.
#[test]
fn dropping_a_host_closes_the_listener_even_without_stop() {
    let engine = engine();

    let addr = {
        let host = McpHost::start(&engine, test_gate()).expect("host must bind");
        let addr = host.addr().expect("running host reports an address");
        assert!(
            TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok(),
            "precondition: accepting before drop",
        );
        addr
    };

    let refused = (0..50).any(|_| {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
        false
    });
    assert!(
        refused,
        "dropping the host must close the listener at {addr}",
    );

    drop(engine);
}

/// Calling `stop` from inside the engine's own runtime would block a worker on
/// a task scheduled to that worker's pool. The host must detect that and
/// downgrade to a signal instead of deadlocking — the outcome says so.
#[test]
fn stopping_from_inside_the_runtime_signals_rather_than_deadlocking() {
    let engine = engine();
    let mut host = McpHost::start(&engine, test_gate()).expect("host must bind");
    let addr = host.addr().expect("running host reports an address");

    let outcome = engine.runtime_handle().block_on(async { host.stop() });
    assert_eq!(
        outcome,
        ShutdownOutcome::Signalled,
        "an in-runtime stop must not claim a drain it never awaited",
    );

    // Signalled still means stopped: the socket closes, only the wait is
    // skipped.
    let refused = (0..50).any(|_| {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
        false
    });
    assert!(refused, "a signalled shutdown must still close {addr}");

    drop(engine);
}

// ---------------------------------------------------------------------------
// The snippet the user pastes
// ---------------------------------------------------------------------------

/// Settings → Connection pastes this verbatim. It must carry the same url the
/// status bar shows and a token the running server accepts — proved by using
/// both against the live socket, not by string comparison alone.
#[test]
fn the_pasted_client_config_authenticates_against_the_running_server() {
    let engine = engine();
    let mut host = McpHost::start(&engine, test_gate()).expect("host must bind");

    let snippet = host
        .client_config_snippet()
        .expect("a running host must render a client config");
    let parsed: serde_json::Value =
        serde_json::from_str(&snippet).expect("the snippet must be valid JSON");

    let url = parsed
        .pointer("/mcpServers/nudox/url")
        .and_then(serde_json::Value::as_str)
        .expect("the snippet must carry a url")
        .to_owned();
    assert_eq!(
        Some(url.clone()),
        host.url(),
        "the pasted url and the displayed url must be the same string",
    );

    let bearer = parsed
        .pointer("/mcpServers/nudox/headers/authorization")
        .and_then(serde_json::Value::as_str)
        .expect("the snippet must carry an Authorization header");
    let secret = bearer
        .strip_prefix("Bearer ")
        .expect("the header must be a bearer credential");

    let response = post(&url, secret, INITIALIZE)
        .unwrap_or_else(|| panic!("the pasted url {url} refused a connection"));
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "the pasted credential must authenticate, got:\n{response}",
    );

    assert_eq!(host.stop(), ShutdownOutcome::Drained);
    drop(engine);
}
