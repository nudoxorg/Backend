//! The endpoint the status bar displays is one a client can actually reach.
//!
//! # The gap this closes — docs/LIMITATIONS.md **L35**
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

use gpui::{
    App, AppContext as _, BorrowAppContext as _, Bounds, Entity, SharedString, TestAppContext,
    WindowBounds, WindowOptions, point, px, size,
};
use lindsey::app::keymaps;
use lindsey::app::lifecycle::{self, Presence, WindowSession};
use lindsey::app::mcp::{McpService, McpStatus};

/// The account gate these tests run under.
///
/// Unmetered, and stated rather than defaulted. Every assertion in this file is
/// about *transport reachability* — that the URL the status bar displays is one
/// a client can open a socket to and get a JSON-RPC answer from. Wiring a real
/// account gate in would additionally make each of them depend on
/// `api.nudox.org` being up, which AGENTS-DOCTRINE §4 rules out. Account
/// behaviour has its own suite, against a fake this process controls:
/// `crates/nudox-mcp/tests/account_against_a_fake_service.rs`.
fn test_gate() -> nudox_mcp::AccountGate {
    nudox_mcp::AccountGate::unmetered(
        "gui transport test: asserts the displayed endpoint is reachable, not billing",
    )
}
use lindsey::motion::tokens::MotionTokens;
use lindsey::stores::events::OpenDisposition;
use lindsey::stores::search_model::SearchAccess as _;
use lindsey::stores::symbol::TabId as DocTabId;
use lindsey::stores::{PackageStore, SearchStore, SymbolStore};
use lindsey::theme::ext::NudoxThemeExt;
use lindsey::workspace::shell::Shell;
use lindsey::workspace::status_bar::StatusBar;
use nudox_engine::runtime::{Engine, EngineConfig};
use nudox_engine::wire::SymbolKey;
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
        cx.set_global(McpService::start(engine, test_gate()));

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

/// docs/LIMITATIONS.md L35, stated as an invariant: whatever endpoint the status bar
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
// Background residency
// ---------------------------------------------------------------------------

/// Boot the theme/motion/keymap globals a real [`Shell`] needs, then install a
/// [`WindowSession`] whose constructor builds exactly the window `main` builds.
///
/// The stores are created here, on the `App`, and only *cloned* into the
/// window's constructor — which is the property the whole restore story rests
/// on. If they were owned by the window, dismissing it would take the corpus
/// with it and no amount of lifecycle code could put the reader back.
fn install_shell_session(
    cx: &mut TestAppContext,
    engine: &nudox_engine::EngineHandle,
) -> (Entity<SearchStore>, Entity<SymbolStore>) {
    cx.update(|cx| {
        gpui_component::init(cx);
        NudoxThemeExt::init(cx).expect("bundled themes parse and install");
        cx.set_global(MotionTokens::new(1.0));
        cx.bind_keys(keymaps::all_bindings());

        let search = cx.new(|_| SearchStore::new(engine.clone()));
        let symbols = cx.new(|_| SymbolStore::new(engine.clone()));
        let packages = cx.new(|cx| PackageStore::new(engine.clone(), &[], cx));
        let index_jobs =
            cx.new(|_cx| lindsey::stores::index_jobs::IndexJobStore::new(engine.clone()));

        let handles = (search.clone(), symbols.clone());
        WindowSession::install(
            Bounds {
                origin: point(px(0.0), px(0.0)),
                size: size(px(1440.0), px(900.0)),
            },
            move |bounds, cx| {
                let (search, symbols, packages, index_jobs) = (
                    search.clone(),
                    symbols.clone(),
                    packages.clone(),
                    index_jobs.clone(),
                );
                cx.open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        focus: false,
                        show: false,
                        ..Default::default()
                    },
                    |window, cx| {
                        let shell = cx.new(|cx| {
                            Shell::new(search, symbols, packages, index_jobs, window, cx)
                        });
                        // `Input` (`SignInView`'s field) requires a
                        // `Root`-rooted window; see the identical comment in
                        // `main.rs`.
                        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
                    },
                )
                .map(Into::into)
            },
            cx,
        );
        lifecycle::wire(cx);
        handles
    })
}

/// Read the endpoint out of the *live window's* status bar.
///
/// Deliberately not out of the global: the invariant is that what the reader
/// sees and what a client can dial are the same string, and after a restore
/// that means the *rebuilt* status bar has to be showing it. A rebuilt window
/// that painted `Absent` while the server kept listening would be L35 again,
/// one layer up.
fn endpoint_in_the_window(cx: &mut TestAppContext) -> McpStatus {
    let handle = cx
        .update(|cx| cx.windows().first().copied())
        .expect("a window to read the status bar out of");
    cx.update(|cx| {
        cx.update_window(handle, |root, _window, cx| {
            let shell = root
                .downcast::<gpui_component::Root>()
                .expect("the session builds exactly one kind of window")
                .read(cx)
                .view()
                .clone()
                .downcast::<Shell>()
                .expect("Root's view is the Shell");
            shell.read(cx).status_bar().read(cx).mcp().clone()
        })
        .expect("read the live window")
    })
}

/// Ask the endpoint a real question and return the display names it answered
/// with. Panics — loudly, with the transport detail — on anything short of a
/// decoded JSON-RPC result.
fn search_over_the_wire(url: &str, token: &str, query: &str) -> Vec<String> {
    let init = post_json_rpc(
        url,
        token,
        None,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"lindsey-residency-test","version":"0"}}}"#,
    );
    assert_eq!(init.status, 200, "initialize must succeed against {url}");
    let session = init
        .headers
        .get("mcp-session-id")
        .cloned()
        .expect("initialize must return an Mcp-Session-Id");
    post_json_rpc(
        url,
        token,
        Some(&session),
        r#"{"method":"notifications/initialized","jsonrpc":"2.0"}"#,
    );

    let body = format!(
        r#"{{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{{"name":"search_symbols","arguments":{{"query":"{query}","limit":50}}}}}}"#,
    );
    let response = post_json_rpc(url, token, Some(&session), &body);
    assert_eq!(response.status, 200, "search_symbols must succeed");
    let message = sse_json_messages(&response.body)
        .into_iter()
        .find(|m| m.get("id").and_then(Value::as_i64) == Some(7))
        .expect("search_symbols must answer the id it was asked with");
    assert!(
        message.get("error").is_none(),
        "search_symbols must not return a JSON-RPC error: {message}",
    );
    message
        .pointer("/result/structuredContent/hits")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("search_symbols must carry structuredContent.hits: {message}"))
        .iter()
        .filter_map(|hit| hit.get("display_name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

/// The whole point of §L6 residency, as one invariant: **an agent's connection
/// does not depend on the human keeping a window open.**
///
/// The dismissal here is real in the only sense that matters — after it,
/// `cx.windows()` is empty and the `Shell` entity that owned every view is
/// dropped. There is no window to be "merely hidden". And the proof that the
/// endpoint survived is a socket opened after that point, carrying a real
/// `tools/call` whose answer names a symbol that really exists in the corpus
/// this process loaded. An assertion that `McpStatus` is still `Listening`
/// would pass against a server that had stopped accepting connections; this
/// cannot.
#[gpui::test]
async fn dismissing_the_window_leaves_the_endpoint_answering(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let _stores = install_shell_session(cx, &engine);

    // ── Onscreen ────────────────────────────────────────────────────────────
    cx.update(|cx| {
        cx.set_global(McpService::start(&engine, test_gate()));
        assert_eq!(Presence::of(cx), Presence::Dismissed, "no window yet");
        assert_eq!(WindowSession::open_first(cx), Presence::Onscreen);
    });
    cx.run_until_parked();

    let McpStatus::Listening { url, client_config } = endpoint_in_the_window(cx) else {
        panic!("the window must be showing a listening server before we dismiss it");
    };
    let token = bearer_from(&client_config);

    // Give the corpus a moment to seed, so a later empty result cannot be
    // blamed on timing. The same retry `the_endpoint_the_status_bar_displays…`
    // uses, for the same reason.
    let deadline = Instant::now() + Duration::from_secs(20);
    while search_over_the_wire(&url, &token, "Point").is_empty() {
        assert!(
            Instant::now() < deadline,
            "precondition: the corpus must answer *before* the dismissal, or \
             this test proves nothing about the dismissal",
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // ── Dismissed ───────────────────────────────────────────────────────────
    let after_dismiss = cx.update(WindowSession::dismiss);
    assert_eq!(after_dismiss, Presence::Dismissed);
    cx.update(|cx| {
        assert!(
            cx.windows().is_empty(),
            "the window must really be gone — a hidden window would make the \
             request below prove nothing",
        );
    });
    cx.run_until_parked();

    // The load-bearing request: a real socket, a real handshake, a real
    // `tools/call`, against a process that currently has no user interface.
    let names = search_over_the_wire(&url, &token, "Point");
    assert!(
        names.iter().any(|n| n.contains("Point")),
        "an agent must still find `Point` in the corpus of a lindsey whose \
         window has been dismissed; got {names:?}",
    );

    // ── Restored ────────────────────────────────────────────────────────────
    let after_show = cx.update(WindowSession::show);
    assert_eq!(after_show, Presence::Onscreen, "there must be a way back");
    cx.run_until_parked();

    assert_eq!(
        endpoint_in_the_window(cx),
        McpStatus::Listening {
            url: url.clone(),
            client_config: client_config.clone(),
        },
        "the rebuilt window must advertise the same endpoint that stayed up — \
         a fresh `Absent` would tell the reader the server died when it did not",
    );

    cx.update(|cx| {
        cx.update_global::<McpService, _>(|service, _| service.stop());
    });
    drop(engine);
}

/// A restored window is not a *new* window: the documents the reader had open
/// come back, in the order they opened them, with the one they were reading
/// active.
///
/// This is the property that makes destroying the window an acceptable way to
/// dismiss it (`app::lifecycle`). Without it, "dismiss" would quietly mean
/// "throw away the reader's session", and the defect would be invisible in
/// review because `Shell::new` is perfectly correct for a cold launch.
#[gpui::test]
async fn a_restored_window_brings_back_the_documents_that_were_open(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let (search, symbols) = install_shell_session(cx, &engine);
    cx.update(|cx| {
        WindowSession::open_first(cx);
    });
    cx.run_until_parked();

    // Open two real documents through the store, exactly as the search overlay
    // does. The keys come out of a real search rather than being hard-coded, so
    // this cannot pass against a corpus that stopped containing them
    // (doctrine §4).
    let keys = wait_for_two_fixture_symbols(cx, &search);
    let opened: Vec<DocTabId> = cx.update(|cx| {
        symbols.update(cx, |store, cx| {
            keys.iter()
                .map(|key| store.open(key.clone(), OpenDisposition::Background, cx))
                .collect()
        })
    });
    cx.run_until_parked();
    assert_eq!(opened.len(), 2, "two distinct fixture symbols must open");

    let before = cx.update(|cx| shell_of(cx).read(cx).open_documents(cx));
    assert_eq!(
        before, opened,
        "precondition: the pane must be showing both documents in open order",
    );

    cx.update(WindowSession::dismiss);
    cx.run_until_parked();
    cx.update(WindowSession::show);
    cx.run_until_parked();

    let after = cx.update(|cx| shell_of(cx).read(cx).open_documents(cx));
    assert_eq!(
        after, before,
        "a restored window must come back with the same documents in the same \
         order, not as an empty shell over a store that still holds them",
    );

    drop(engine);
}

/// The live window's `Shell`.
///
/// The window's first layer is `gpui_component::Root` now, not `Shell` —
/// `Input` (`SignInView`'s field) requires it; see `main.rs`. One level
/// deeper than it used to be.
fn shell_of(cx: &mut App) -> Entity<Shell> {
    let handle = cx.windows().first().copied().expect("a live window");
    let root = handle
        .downcast::<gpui_component::Root>()
        .expect("the session builds exactly one kind of window")
        .root(cx)
        .expect("the window's root view");
    root.read(cx)
        .view()
        .clone()
        .downcast::<Shell>()
        .expect("Root's view is the Shell")
}

/// Two distinct symbol keys that really exist in the fixture corpus.
///
/// # Why the retry is coarse
///
/// The corpus seeds on the engine's own Tokio threads, so a query issued in the
/// first few milliseconds legitimately finds nothing and the store does not
/// re-issue it. The query therefore has to be repeated — but `set_input`
/// *restarts the debounce and supersedes the in-flight generation*, so a tight
/// retry loop cancels every query with the next one and finds nothing forever.
/// The first draft of this helper did exactly that and passed only by luck.
///
/// So: re-issue at most once per full round-trip window, and inside that window
/// move both clocks (AGENTS-DOCTRINE, "Two clocks have to move") — the
/// simulated one to fire the 24 ms debounce, and real time to let the engine's
/// threads actually answer.
fn wait_for_two_fixture_symbols(
    cx: &mut TestAppContext,
    search: &Entity<SearchStore>,
) -> Vec<SymbolKey> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        cx.update(|cx| {
            search.update(cx, |store, cx| store.set_input("Point".into(), cx));
        });

        for _ in 0..20 {
            cx.run_until_parked();
            cx.executor().advance_clock(Duration::from_millis(30));
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(50));
        }
        cx.run_until_parked();

        let mut distinct: Vec<SymbolKey> = Vec::new();
        cx.update(|cx| {
            for section in search.read(cx).snapshot().sections.iter() {
                for row in section.rows.iter() {
                    if !distinct.contains(&row.key) {
                        distinct.push(row.key.clone());
                    }
                }
            }
        });
        if distinct.len() >= 2 {
            distinct.truncate(2);
            return distinct;
        }
        assert!(
            Instant::now() < deadline,
            "the fixture corpus must offer at least two distinct symbols for \
             `Point`; found {}",
            distinct.len(),
        );
    }
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
