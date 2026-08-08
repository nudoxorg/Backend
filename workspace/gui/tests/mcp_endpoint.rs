//! The endpoint the status bar displays is one a client can actually reach.
//!
//! # The gap this closes — LIMITATIONS.md **L35**
//!
//! `nudox-mcp` was complete and tested end-to-end and *nothing started it*.
//! `grep -rn 'nudox_mcp|McpEndpoint|NudoxMcpServer' workspace/gui` returned
//! zero hits and `StatusBar::set_mcp_endpoint` had one occurrence repo-wide —
//! its own definition. Every piece was real; the integration was not.
//!
//! The half-measure this file deliberately avoids is a test that starts the
//! service and asserts a field became `Some`. That test passes against the
//! exact stub L35 describes, because "a string was stored" is not "a server is
//! listening". So the flow here is, in order:
//!
//! 1. build a real engine over the real fixture corpus;
//! 2. start the MCP service the way `main.rs` does, and install it as a global;
//! 3. build the real [`StatusBar`] entity and feed it
//!    `McpStatus::from_app` — the same call `Shell::new` makes;
//! 4. read the URL and the pasteable client config **back out of the status
//!    bar**, not out of the host;
//! 5. open a socket to that URL, with that credential, and perform a real MCP
//!    handshake and a real `tools/call`;
//! 6. assert on the decoded JSON-RPC **result body** naming symbols and
//!    packages that really exist in the fixture corpus.
//!
//! Nothing short of a running server that shares the window's corpus can make
//! step 6 pass.
//!
//! # Why raw sockets and a hand-rolled HTTP reader
//!
//! Same reason `crates/nudox-mcp/tests/{endpoint,real_crate_memchr}.rs` give:
//! an HTTP client crate papers over exactly the framing under test — chunked
//! transfer encoding, SSE event framing, and the `Mcp-Session-Id` handshake.
//! And `lindsey` links no async runtime at all (LR-9), so the client here is
//! blocking `std::net` by necessity as well as by choice: a host that needed
//! `tokio` to check its own status bar would not be a host `lindsey` can be.

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::time::{Duration, Instant};

use gpui::{AppContext as _, BorrowAppContext as _, SharedString, TestAppContext};
use lindsey::app::mcp::{McpService, McpStatus};
use lindsey::workspace::status_bar::StatusBar;
use nudox_engine::runtime::{Engine, EngineConfig};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Blocking HTTP/1.1 + chunked + SSE reader
// ---------------------------------------------------------------------------

/// A decoded HTTP response: status, lower-cased header names, de-chunked body.
struct RawResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Undo HTTP/1.1 chunked transfer encoding (RFC 9112 §7.1).
///
/// `axum::serve` chunk-encodes every SSE response, because a stream body has no
/// known `Content-Length` up front.
fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(size_end) = find_subslice(raw, b"\r\n") else {
            break;
        };
        let size_line = std::str::from_utf8(&raw[..size_end]).unwrap_or("").trim();
        let size_str = size_line.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_str, 16) else {
            break;
        };
        let data_start = size_end + 2;
        if size == 0 {
            break; // the terminating zero-length chunk
        }
        let data_end = (data_start + size).min(raw.len());
        out.extend_from_slice(&raw[data_start..data_end]);
        if data_end >= raw.len() {
            break;
        }
        raw = &raw[data_end..];
        raw = raw.strip_prefix(b"\r\n").unwrap_or(raw);
    }
    out
}

fn parse_http_response(raw: &[u8]) -> RawResponse {
    let header_end = find_subslice(raw, b"\r\n\r\n").unwrap_or_else(|| {
        panic!(
            "response must have a header/body separator; got {} bytes: {:?}",
            raw.len(),
            String::from_utf8_lossy(&raw[..raw.len().min(200)]),
        )
    });
    let head = std::str::from_utf8(&raw[..header_end]).expect("headers must be UTF-8");
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }

    let raw_body = &raw[header_end + 4..];
    let chunked = headers
        .get("transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    let body = if chunked {
        dechunk(raw_body)
    } else {
        raw_body.to_vec()
    };

    RawResponse {
        status,
        headers,
        body,
    }
}

/// Every JSON-RPC message carried in an SSE body's `data:` lines, in order.
/// Priming events (an empty `data:`) carry no message and are skipped.
fn sse_json_messages(body: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(body)
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|data| !data.is_empty())
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .collect()
}

/// The address and path a URL string dials.
///
/// Parsed from the *advertised string* rather than taken from a `SocketAddr`
/// beside it, because "is what we print dialable" is half of what this file
/// exists to prove. A URL that is malformed fails here, loudly.
fn dial_target(url: &str) -> (SocketAddr, String) {
    let rest = url
        .strip_prefix("http://")
        .unwrap_or_else(|| panic!("the displayed endpoint must be an http url: {url}"));
    let (authority, path) = match rest.find('/') {
        Some(ix) => (&rest[..ix], rest[ix..].to_owned()),
        None => (rest, "/".to_owned()),
    };
    let addr = authority
        .parse()
        .unwrap_or_else(|e| panic!("displayed authority {authority:?} must be dialable: {e}"));
    (addr, path)
}

/// POST one JSON-RPC message to the displayed URL over a fresh blocking socket.
///
/// `Connection: close` lets a plain `read_to_end` capture the whole response
/// without tracking keep-alive; the MCP *session* is tracked server-side by
/// `Mcp-Session-Id`, not by TCP connection identity, so a fresh connection per
/// call is spec-legal.
fn post_json_rpc(url: &str, token: &str, session: Option<&str>, body: &str) -> RawResponse {
    let (addr, path) = dial_target(url);
    let session_header = session.map_or_else(String::new, |id| format!("Mcp-Session-Id: {id}\r\n"));
    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Content-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\n\
         Authorization: Bearer {token}\r\n\
         {session_header}\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        len = body.len(),
    );

    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .unwrap_or_else(|e| panic!("the displayed endpoint {url} must accept connections: {e}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .expect("read timeout must apply");
    stream
        .write_all(request.as_bytes())
        .expect("request must send");
    stream.flush().expect("request must flush");

    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    parse_http_response(&raw)
}

// ---------------------------------------------------------------------------
// The status bar, read the way a reader reads it
// ---------------------------------------------------------------------------

/// The MCP credential a user would paste out of Settings → Connection.
///
/// Pulled from the status bar's own `client_config`, so the credential under
/// test is the one the product hands out — not one the test knows a back door
/// to.
fn bearer_from(client_config: &SharedString) -> String {
    let parsed: Value = serde_json::from_str(client_config)
        .unwrap_or_else(|e| panic!("the displayed client config must be JSON: {e}"));
    let header = parsed
        .pointer("/mcpServers/nudox/headers/authorization")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("client config must carry an Authorization header: {parsed}"));
    header
        .strip_prefix("Bearer ")
        .unwrap_or_else(|| panic!("the credential must be a bearer token: {header}"))
        .to_owned()
}

/// Build the engine, start the service exactly as `main.rs` does, install it as
/// a global, and hand back what a real [`StatusBar`] ends up displaying.
///
/// The status bar is fed through [`McpStatus::from_app`] — the same call
/// `Shell::new` makes — so this exercises the production path from the global
/// to the pixels, not a shortcut around it.
fn displayed_endpoint(
    cx: &mut TestAppContext,
    engine: &nudox_engine::EngineHandle,
) -> (SharedString, SharedString, Option<SharedString>) {
    cx.update(|cx| {
        cx.set_global(McpService::start(engine));

        let status = McpStatus::from_app(cx);
        let bar = cx.new(|cx| {
            let mut bar = StatusBar::new(cx);
            bar.set_mcp(status, cx);
            bar
        });

        let bar = bar.read(cx);
        let label = bar.mcp_label().cloned();
        match bar.mcp() {
            McpStatus::Listening { url, client_config } => {
                (url.clone(), client_config.clone(), label)
            }
            other => panic!("the status bar must show a listening server, showed {other:?}"),
        }
    })
}

// ---------------------------------------------------------------------------
// The load-bearing test
// ---------------------------------------------------------------------------

/// LIMITATIONS.md L35, stated as an invariant: whatever endpoint the status bar
/// puts on screen, a client that dials exactly that gets real answers about the
/// corpus this window is showing.
#[gpui::test]
async fn the_endpoint_the_status_bar_displays_answers_a_real_tools_call(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let (url, client_config, label) = displayed_endpoint(cx, &engine);
    let token = bearer_from(&client_config);

    // The reader must be able to copy the address out of the window by hand.
    let label = label.expect("a listening server must paint a segment");
    assert!(
        label.contains(url.as_ref()),
        "the painted segment {label:?} must contain the dialable url {url:?}",
    );

    // --- 1. initialize ------------------------------------------------------
    let init = post_json_rpc(
        &url,
        &token,
        None,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"lindsey-status-bar-test","version":"0"}}}"#,
    );
    assert_eq!(
        init.status, 200,
        "initialize against the displayed url must succeed",
    );
    let session = init
        .headers
        .get("mcp-session-id")
        .cloned()
        .expect("initialize must return an Mcp-Session-Id");
    let init_result = sse_json_messages(&init.body)
        .iter()
        .find_map(|m| m.pointer("/result").cloned())
        .expect("initialize must carry a JSON-RPC result");
    assert_eq!(
        init_result.get("protocolVersion").and_then(Value::as_str),
        Some("2025-11-25"),
        "the server must negotiate the protocol, not merely accept bytes: {init_result}",
    );

    // --- 2. notifications/initialized --------------------------------------
    let notified = post_json_rpc(
        &url,
        &token,
        Some(&session),
        r#"{"method":"notifications/initialized","jsonrpc":"2.0"}"#,
    );
    assert_eq!(notified.status, 202, "the initialized notification must be accepted");

    // --- 3. tools/call list_packages ---------------------------------------
    //
    // Retried: the corpus seeds on the engine's runtime, so an agent that
    // attaches in the first few milliseconds legitimately sees an empty corpus.
    // That is a race in the *test*, not in the product — the tool answers
    // correctly either way, it just has less to say.
    let deadline = Instant::now() + Duration::from_secs(20);
    let packages = loop {
        let response = post_json_rpc(
            &url,
            &token,
            Some(&session),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_packages","arguments":{}}}"#,
        );
        assert_eq!(response.status, 200, "list_packages must succeed");
        let message = sse_json_messages(&response.body)
            .into_iter()
            .find(|m| m.get("id").and_then(Value::as_i64) == Some(2))
            .expect("list_packages must answer the id it was asked with");
        assert!(
            message.get("error").is_none(),
            "list_packages must not return a JSON-RPC error: {message}",
        );
        let listed = message
            .pointer("/result/structuredContent/packages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !listed.is_empty() || Instant::now() >= deadline {
            break listed;
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    let rich = packages
        .iter()
        .find(|p| p.get("name").and_then(Value::as_str) == Some("nudox-fixture-rich"))
        .unwrap_or_else(|| {
            panic!(
                "the agent must see the same corpus the window does; \
                 `nudox-fixture-rich` missing from {packages:?}"
            )
        });
    assert_eq!(
        rich.get("ecosystem").and_then(Value::as_str),
        Some("fixture"),
        "package rows must carry real lineage data: {rich}",
    );

    // --- 4. tools/call search_symbols --------------------------------------
    //
    // `Point` is a real top-level symbol in `build_rich_view()`
    // (crates/nudox-store/src/source/fixtures.rs, table entry id=3), and is the
    // same symbol `shell_flow.rs` searches for through the GUI. An agent and
    // the window must be able to find it by the same name.
    let response = post_json_rpc(
        &url,
        &token,
        Some(&session),
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_symbols","arguments":{"query":"Point","limit":50}}}"#,
    );
    assert_eq!(response.status, 200, "search_symbols must succeed");
    let message = sse_json_messages(&response.body)
        .into_iter()
        .find(|m| m.get("id").and_then(Value::as_i64) == Some(3))
        .expect("search_symbols must answer the id it was asked with");
    assert!(
        message.get("error").is_none(),
        "search_symbols must not return a JSON-RPC error: {message}",
    );

    let hits = message
        .pointer("/result/structuredContent/hits")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("search_symbols must carry structuredContent.hits: {message}"));
    let names: Vec<&str> = hits
        .iter()
        .filter_map(|hit| hit.get("display_name").and_then(Value::as_str))
        .collect();
    assert!(
        names.iter().any(|n| n.contains("Point")),
        "an agent searching the window's corpus for `Point` must find it; got {names:?}",
    );

    // Every key must carry the fixture lineage: proof the answer came from this
    // window's engine and not from anything the MCP layer made up.
    for hit in hits {
        let key = hit
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("every hit must carry a string key: {hit}"));
        assert!(
            key.starts_with("fixture:"),
            "hits must come from the fixture corpus this window loaded; got {key}",
        );
    }

    cx.update(|cx| {
        cx.update_global::<McpService, _>(|service, _| service.stop());
    });
    drop(engine);
}

// ---------------------------------------------------------------------------
// Adversarial
// ---------------------------------------------------------------------------

/// A window in a process that hosts no server must not imply one exists.
///
/// This is the state every `#[gpui::test]` app is in, and the one the old
/// `Option<SharedString>` shared with "the server failed to bind".
#[gpui::test]
async fn a_process_with_no_service_shows_no_endpoint_at_all(cx: &mut TestAppContext) {
    cx.update(|cx| {
        assert_eq!(
            McpStatus::from_app(cx),
            McpStatus::Absent,
            "with no service installed the app must report Absent, not a blank Listening",
        );

        let bar = cx.new(|cx| {
            let mut bar = StatusBar::new(cx);
            bar.set_mcp(McpStatus::from_app(cx), cx);
            bar
        });
        let bar = bar.read(cx);
        assert_eq!(bar.mcp(), &McpStatus::Absent);
        assert_eq!(bar.mcp_label(), None, "there is no segment to paint");
        assert_eq!(bar.mcp().url(), None, "and nothing to dial");
    });
}

/// Shutting the server down mid-session: the status bar must stop advertising
/// the address, and the address must stop answering. A stale URL is worse than
/// none — the port may be reused by another process.
#[gpui::test]
async fn stopping_the_service_retracts_the_endpoint_and_closes_the_socket(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();

    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let (url, client_config, _) = displayed_endpoint(cx, &engine);
    let token = bearer_from(&client_config);
    let (addr, _) = dial_target(&url);

    // Established mid-session: a real client is talking to it.
    let init = post_json_rpc(
        &url,
        &token,
        None,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"lindsey-shutdown-test","version":"0"}}}"#,
    );
    assert_eq!(init.status, 200, "precondition: the session is live");

    let after = cx.update(|cx| {
        cx.update_global::<McpService, _>(|service, _| service.stop());
        let status = McpStatus::from_app(cx);
        let bar = cx.new(|cx| {
            let mut bar = StatusBar::new(cx);
            bar.set_mcp(status.clone(), cx);
            bar
        });
        assert_eq!(bar.read(cx).mcp(), &status);
        status
    });

    assert_eq!(after, McpStatus::Stopped);
    assert_eq!(after.url(), None, "a stopped server must advertise nothing");
    assert!(
        after.label().is_some_and(|l| l.contains("stopped")),
        "the reader must be told it stopped, not shown nothing: {after:?}",
    );

    // The socket is gone. Retried, because the accept loop unwinds
    // asynchronously after cancellation.
    let refused = (0..50).any(|_| {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
        false
    });
    assert!(
        refused,
        "the endpoint at {addr} must stop answering once the service is stopped",
    );

    drop(engine);
}

/// A bind failure must reach the screen. The status bar shows a *warning*
/// segment naming the cause, rather than the nothing that made L35 invisible.
///
/// Constructed from a real `McpError::Bind` rather than by forcing a collision:
/// `LOOPBACK_BIND` is port 0, so the kernel picks a free port on every bind and
/// "the port is already taken" is unreachable by construction — which is §L6's
/// design, not an untested path. What *is* testable, and what matters, is that
/// the error becomes something a reader can see and act on.
#[gpui::test]
async fn a_server_that_could_not_bind_is_visible_rather_than_absent(cx: &mut TestAppContext) {
    let error = nudox_mcp::McpError::Bind {
        addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "Operation not permitted"),
    };
    let status = McpStatus::failed(&error);

    cx.update(|cx| {
        let bar = cx.new(|cx| {
            let mut bar = StatusBar::new(cx);
            bar.set_mcp(status.clone(), cx);
            bar
        });
        let bar = bar.read(cx);

        assert!(bar.mcp().is_failure());
        assert_ne!(
            bar.mcp(),
            &McpStatus::Absent,
            "a failed bind must not render as a process that hosts no server",
        );
        let label = bar
            .mcp_label()
            .expect("a failed server must still paint a segment");
        assert!(
            label.contains("unavailable"),
            "the segment must say the endpoint is unavailable: {label:?}",
        );
        assert_eq!(bar.mcp().url(), None, "there is nothing to dial");
    });

    let McpStatus::Failed { reason } = status else {
        panic!("a bind error must produce Failed");
    };
    assert!(
        reason.contains("Operation not permitted"),
        "the cause must survive to the UI layer: {reason}",
    );
}
