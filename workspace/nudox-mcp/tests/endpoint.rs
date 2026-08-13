//! §L6 transport: loopback bind, ephemeral port, and the per-launch token.
//!
//! These tests drive the server over a raw TCP socket rather than through an
//! HTTP client crate. That is deliberate: the properties under test are
//! "which address did we bind" and "what happens to a request with no
//! credential", and a client library would helpfully paper over exactly those.
//!
//! # Runtime discipline
//!
//! `Engine::start` builds and owns a Tokio runtime, and dropping a runtime from
//! inside async context panics. Every test therefore creates its own runtime,
//! does the async work inside `block_on`, and lets the engine drop afterwards —
//! in sync context, where dropping a runtime is legal.

use std::net::SocketAddr;
use std::time::Duration;

use nudox_engine::{Engine, EngineConfig, EngineHandle};
use nudox_mcp::{AccountGate, McpEndpoint, NudoxMcpServer, SessionToken};
use nudox_store::source::fixtures::FixtureSource;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;

/// A JSON-RPC `initialize` call — the first thing any MCP client sends.
const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;

/// Build a runtime and an engine over the deterministic fixture corpus.
/// The account gate these tests run under.
///
/// Unmetered, and stated rather than defaulted. Every assertion in this file is
/// about the *session token* — the loopback transport credential — and about
/// HTTP status lines. The `ndx_` account key is a different secret with a
/// different lifetime (see `nudox_mcp::account::credential`), and its behaviour
/// is covered by `tests/account_against_a_fake_service.rs` against a fake this
/// process controls. Wiring a real account gate in here would make a transport
/// test depend on a paid service.
fn test_gate() -> AccountGate {
    AccountGate::unmetered("transport test: asserts on session-token auth, not on account auth")
}

fn harness() -> (Runtime, EngineHandle) {
    let runtime = Runtime::new().expect("test runtime must build");
    let engine = Engine::start(EngineConfig::default(), FixtureSource::rich());
    (runtime, engine)
}

/// POST to the endpoint over raw TCP and return the response's status line.
///
/// `Connection: close` plus a timeout keeps the read bounded even if the server
/// decides to answer with an event stream.
async fn post_status_line(addr: SocketAddr, token: Option<&str>, body: &str) -> String {
    let auth = match token {
        Some(t) => format!("Authorization: Bearer {t}\r\n"),
        None => String::new(),
    };
    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Content-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\n\
         {auth}\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        path = nudox_mcp::MCP_PATH,
        len = body.len(),
    );

    let mut stream = TcpStream::connect(addr)
        .await
        .expect("endpoint must accept connections");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("request must send");
    stream.flush().await.expect("request must flush");

    let mut buf = Vec::new();
    // The server may hold an SSE stream open; we only need the status line.
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf)).await;

    let text = String::from_utf8_lossy(&buf);
    text.lines().next().unwrap_or_default().to_owned()
}

#[test]
fn binds_loopback_on_an_ephemeral_port_and_reports_it() {
    let (runtime, engine) = harness();

    let (addr, url) = runtime.block_on(async {
        let endpoint = McpEndpoint::start(NudoxMcpServer::new(engine.clone(), test_gate()))
            .await
            .expect("server must bind loopback");
        let observed = (endpoint.addr(), endpoint.url());
        endpoint.stop().await;
        observed
    });

    assert!(addr.ip().is_loopback(), "must bind loopback, bound {addr}");
    assert_ne!(
        addr.port(),
        0,
        "the kernel-assigned port must be reported, not 0"
    );
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "url must be loopback: {url}"
    );
    assert!(
        url.ends_with(nudox_mcp::MCP_PATH),
        "url must include the mcp path: {url}"
    );

    drop(engine);
    drop(runtime);
}

#[test]
fn two_servers_get_different_ports() {
    // The whole reason for the ephemeral port (§L6): two lindsey windows must
    // not collide, and must not silently share a corpus.
    let (runtime, engine) = harness();

    let (first, second) = runtime.block_on(async {
        let a = McpEndpoint::start(NudoxMcpServer::new(engine.clone(), test_gate()))
            .await
            .expect("first server must bind");
        let b = McpEndpoint::start(NudoxMcpServer::new(engine.clone(), test_gate()))
            .await
            .expect("second server must bind");
        let ports = (a.addr().port(), b.addr().port());
        a.stop().await;
        b.stop().await;
        ports
    });

    assert_ne!(first, second, "two servers must not land on the same port");

    drop(engine);
    drop(runtime);
}

#[test]
fn a_request_without_the_session_token_is_rejected() {
    let (runtime, engine) = harness();

    let status = runtime.block_on(async {
        let endpoint = McpEndpoint::start(NudoxMcpServer::new(engine.clone(), test_gate()))
            .await
            .expect("server must bind");
        let status = post_status_line(endpoint.addr(), None, INITIALIZE).await;
        endpoint.stop().await;
        status
    });

    assert!(
        status.contains("401"),
        "an unauthenticated request must be rejected with 401, got {status:?}"
    );

    drop(engine);
    drop(runtime);
}

#[test]
fn a_request_with_the_wrong_token_is_rejected() {
    let (runtime, engine) = harness();

    let status = runtime.block_on(async {
        let endpoint = McpEndpoint::start_with_token(
            NudoxMcpServer::new(engine.clone(), test_gate()),
            SessionToken::from_secret("the-real-token"),
        )
        .await
        .expect("server must bind");
        let status = post_status_line(endpoint.addr(), Some("not-the-token"), INITIALIZE).await;
        endpoint.stop().await;
        status
    });

    assert!(
        status.contains("401"),
        "a wrong token must be rejected with 401, got {status:?}"
    );

    drop(engine);
    drop(runtime);
}

#[test]
fn a_request_with_the_correct_token_is_not_rejected() {
    // The complement of the rejection tests: proves the layer is gating on the
    // token rather than refusing everything.
    let (runtime, engine) = harness();

    let status = runtime.block_on(async {
        let token = SessionToken::generate();
        let secret = token.expose().to_owned();
        let endpoint = McpEndpoint::start_with_token(NudoxMcpServer::new(engine.clone(), test_gate()), token)
            .await
            .expect("server must bind");
        let status = post_status_line(endpoint.addr(), Some(&secret), INITIALIZE).await;
        endpoint.stop().await;
        status
    });

    assert!(
        !status.is_empty(),
        "server must answer an authenticated request"
    );
    assert!(
        !status.contains("401"),
        "the correct token must not be rejected, got {status:?}"
    );

    drop(engine);
    drop(runtime);
}

#[test]
fn the_client_config_snippet_carries_the_live_url_and_token() {
    // Settings → Connection pastes this verbatim; if it drifts from what the
    // server accepts, every agent that uses it fails with a 401.
    let (runtime, engine) = harness();

    let (snippet, url, secret) = runtime.block_on(async {
        let endpoint = McpEndpoint::start(NudoxMcpServer::new(engine.clone(), test_gate()))
            .await
            .expect("server must bind");
        let observed = (
            endpoint.client_config_snippet(),
            endpoint.url(),
            endpoint.token().expose().to_owned(),
        );
        endpoint.stop().await;
        observed
    });

    assert!(
        snippet.contains(&url),
        "snippet must point at the bound url"
    );
    assert!(
        snippet.contains(&secret),
        "snippet must carry the launch token"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&snippet).expect("the snippet must be valid JSON");
    assert!(
        parsed.pointer("/mcpServers/nudox/url").is_some(),
        "snippet must have the shape MCP clients expect"
    );

    drop(engine);
    drop(runtime);
}
