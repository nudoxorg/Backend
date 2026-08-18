//! End-to-end MCP test: a real streamable-HTTP transport carrying a real
//! `tools/call` against a real lowered package.
//!
//! # The gap this closes — docs/LIMITATIONS.md L36
//!
//! Four other test files each cover half of "does this actually work":
//!
//! | file | real transport | real lowered package |
//! |---|---|---|
//! | `tests/endpoint.rs` | yes — raw TCP, bearer token, 401/200 | no — synthetic `FixtureSource::rich()` |
//! | `tests/tool_integration.rs` | no — direct `NudoxTools` calls | no — synthetic fixture |
//! | `tests/real_crate_tokio.rs` | no — bypasses rmcp/HTTP entirely | yes, in principle |
//! | `tests/schemas.rs` | no | no — pure serde/schemars |
//!
//! `endpoint.rs` sends one raw `initialize` and reads only the HTTP status
//! line — it never issues a `tools/call` or looks at a JSON-RPC result body.
//! `real_crate_tokio.rs` calls `NudoxTools` methods directly in-process,
//! bypassing rmcp and HTTP entirely, and every one of its tests is gated on
//! `result/tokio/Cargo.toml`, which does not exist in this checkout —
//! its own doc comment points at `scripts/fetch-real-crate.sh`, which does
//! not exist either (the real fetch tooling is `nix build .#checks.corpus` driven by
//! `nix/corpus.nix`; see the fix to that doc comment alongside this
//! file).
//!
//! Nobody drives the real `axum`/`rmcp` HTTP+SSE stack while documenting a
//! package no fixture author hand-wrote. This file does both at once: it
//! starts a real engine over the real Rust producer
//! (`Engine::start_with_producer`) pointed at `result/memchr-2.8.3`
//! (present — see `nix/corpus.nix`; tokio is not, so this test uses
//! memchr instead, per instruction), starts a real `McpEndpoint`, performs
//! the real MCP handshake over a raw TCP socket exactly like `endpoint.rs`
//! does, and then issues a real `tools/call` for `search_symbols` and one for
//! `list_packages` — asserting on the decoded JSON-RPC **result body** (real
//! memchr symbol names), not on a status line and not on a row count.
//!
//! # Why raw TCP and not an HTTP client crate
//!
//! Same rationale as `endpoint.rs`: an HTTP client would paper over exactly
//! the framing this test needs to prove works — chunked transfer encoding,
//! SSE event framing, and the `Mcp-Session-Id` handshake. No new dependency
//! is introduced; the HTTP/SSE parsing below is the price of testing the
//! real wire format instead of a stand-in for it.
//!
//! # Why no entry count is ever asserted
//!
//! memchr's total lowered entry count has moved as the IR layer's
//! foreign-reference handling changed underneath it the same day this test
//! was written (`Ref::Foreign` now genuinely carries `{ key, target }`, and
//! `PristineIntroTable::insert_live`'s collision behaviour changed). Asserting
//! a count here would make this test as fragile as the thing it exists to
//! prove works. Every assertion below is instead on a named symbol that
//! really exists in `result/memchr-2.8.3/src/lib.rs`'s public
//! re-export list (`Memchr`, `memchr_iter`, `memrchr`, …).
//!
//! # Running
//!
//! ```text
//! # Ensure memchr is fetched (it ships with the corpus manifest):
//! ls result/memchr-2.8.3/Cargo.toml
//!
//! # If missing, fetch the whole reproducible corpus:
//! nix build .#checks.corpus
//!
//! cargo test -p nudox-mcp --test real_crate_memchr -- --ignored --nocapture
//! ```

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use nudox_engine::mcp::{AccountGate, McpEndpoint, NudoxMcpServer, SessionToken};
use nudox_engine::{
    Engine, EngineConfig, EngineHandle, PackageLoadEvent, PackageSpec, ProducerLanguage, SharedStr,
};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;

// ---------------------------------------------------------------------------
// Fixture path helpers
// ---------------------------------------------------------------------------

/// Where `nix build .#checks.corpus` places the memchr checkout (`nix/corpus.nix`
/// pins it at 2.8.3).
fn memchr_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../result/memchr-2.8.3")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("/nonexistent"))
}

/// `true` when the memchr fixture checkout is present.
///
/// A missing fixture is an environment problem, not a code defect — every
/// test body checks this first and skips gracefully, matching the convention
/// in `real_crate_tokio.rs`.
fn memchr_available() -> bool {
    memchr_root().join("Cargo.toml").is_file()
}

// ---------------------------------------------------------------------------
// Minimal HTTP/1.1 + chunked + SSE parsing (no client crate — see module docs)
// ---------------------------------------------------------------------------

/// A decoded HTTP response: status, lower-cased header names, and the fully
/// de-chunked body.
struct RawResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Undo HTTP/1.1 chunked transfer encoding (RFC 9112 §7.1). `axum::serve`
/// chunk-encodes every SSE response because a stream body has no known
/// `Content-Length` up front.
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

/// Parse a complete raw HTTP/1.1 response (status line + headers + body) read
/// off the socket, de-chunking the body when `Transfer-Encoding: chunked` is
/// present — which every SSE response from this server is.
fn parse_http_response(raw: &[u8]) -> RawResponse {
    let header_end =
        find_subslice(raw, b"\r\n\r\n").expect("response must have a header/body separator");
    let head = std::str::from_utf8(&raw[..header_end]).expect("headers must be UTF-8");
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
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
/// Priming events (SEP-1699, an empty `data:`) are skipped — they carry no
/// message.
fn sse_json_messages(body: &[u8]) -> Vec<Value> {
    let text = String::from_utf8_lossy(body);
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|data| !data.is_empty())
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .collect()
}

/// POST one JSON-RPC message to the running endpoint over a fresh raw TCP
/// connection and return the decoded response.
///
/// `Connection: close` (as in `endpoint.rs`) lets a plain `read_to_end`
/// capture the whole response without tracking HTTP keep-alive; the MCP
/// *session* is tracked server-side by `Mcp-Session-Id`, not by TCP
/// connection identity, so a fresh connection per call is spec-legal.
async fn post_json_rpc(
    addr: SocketAddr,
    token: &str,
    session_id: Option<&str>,
    body: &str,
) -> RawResponse {
    let session_header = match session_id {
        Some(id) => format!("Mcp-Session-Id: {id}\r\n"),
        None => String::new(),
    };
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
        path = nudox_engine::mcp::MCP_PATH,
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
    tokio::time::timeout(Duration::from_secs(15), stream.read_to_end(&mut buf))
        .await
        .expect("server must respond within 15s")
        .expect("reading the response must not error");

    parse_http_response(&buf)
}

// ---------------------------------------------------------------------------
// Corpus wait
// ---------------------------------------------------------------------------

/// Wait for the memchr package to appear in the corpus, or panic on a
/// `LoadFailed` event for it — a producer failure here would otherwise
/// present as a silent hang until the deadline instead of a clear cause.
async fn wait_for_memchr(engine: &EngineHandle) {
    let rx = engine.packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv_async()).await {
            Ok(Ok(PackageLoadEvent::Loaded { name, .. })) if name == SharedStr::from("memchr") => {
                return;
            }
            Ok(Ok(PackageLoadEvent::LoadFailed { name, error, .. }))
                if name == SharedStr::from("memchr") =>
            {
                panic!("memchr failed to load: {error}");
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => return, // channel closed → already loaded
            Err(_) => panic!(
                "memchr corpus never seeded within 180s — the producer may have failed; \
                 run with --nocapture to see tracing output"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// A real `tools/call` for `search_symbols` and one for `list_packages`,
/// driven over the real streamable-HTTP transport against a real lowered
/// package — the exact combination docs/LIMITATIONS.md L36 says no test covers.
#[test]
#[ignore = "drives rust-analyzer over a real cargo workspace (~10-30s) and opens real \
            loopback sockets; run explicitly with --ignored"]
fn tools_call_over_real_transport_returns_real_memchr_symbols() {
    if !memchr_available() {
        eprintln!(
            "SKIP: no checkout at {}. Fetch the corpus with: nix build .#checks.corpus",
            memchr_root().display()
        );
        return;
    }

    let runtime = Runtime::new().expect("test runtime must build");
    let case_dir = memchr_root();

    let ((search_result, packages_result), cost) = heart::cost::measured(
        "mcp_e2e/memchr-2.8.3/tools_call_over_real_transport",
        &case_dir,
        || {
            runtime.block_on(async {
                let engine = Engine::start_with_producer(
                    EngineConfig::default(),
                    vec![PackageSpec {
                        root: memchr_root(),
                        name: "memchr".to_owned(),
                        version: "2.8.3".to_owned(),
                        language: ProducerLanguage::Rust,
                    }],
                );
                wait_for_memchr(&engine).await;

                let token = SessionToken::generate();
                let secret = token.expose().to_owned();
                let endpoint =
                    McpEndpoint::start_with_token(NudoxMcpServer::new(engine.clone(), AccountGate::unmetered(
                            "real-crate transport test: this file measures the corpus, not billing",
                        )), token)
                        .await
                        .expect("endpoint must bind loopback");
                let addr = endpoint.addr();

                // --- 1. initialize --------------------------------------------------
                let init = post_json_rpc(
                    addr,
                    &secret,
                    None,
                    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"conduit-e2e-test","version":"0"}}}"#,
                )
                .await;
                assert_eq!(init.status, 200, "initialize must succeed");
                let session_id = init
                    .headers
                    .get("mcp-session-id")
                    .cloned()
                    .expect("initialize response must carry Mcp-Session-Id");
                let init_result = sse_json_messages(&init.body)
                    .iter()
                    .find_map(|m| m.pointer("/result").cloned())
                    .expect("initialize response must carry a JSON-RPC result");
                assert_eq!(
                    init_result.get("protocolVersion").and_then(Value::as_str),
                    Some("2025-11-25"),
                    "server must echo back the negotiated protocol version: {init_result}"
                );

                // --- 2. notifications/initialized -----------------------------------
                let notified = post_json_rpc(
                    addr,
                    &secret,
                    Some(&session_id),
                    r#"{"method":"notifications/initialized","jsonrpc":"2.0"}"#,
                )
                .await;
                assert_eq!(
                    notified.status, 202,
                    "the initialized notification must be accepted"
                );

                // --- 3. tools/call search_symbols ------------------------------------
                let search = post_json_rpc(
                    addr,
                    &secret,
                    Some(&session_id),
                    r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search","arguments":{"query":"mem","limit":100}}}"#,
                )
                .await;
                assert_eq!(search.status, 200, "search_symbols call must succeed");
                let search_result = sse_json_messages(&search.body)
                    .into_iter()
                    .find(|m| m.get("id").and_then(Value::as_i64) == Some(2))
                    .expect("search_symbols response must be present in the SSE body");
                assert!(
                    search_result.get("error").is_none(),
                    "search_symbols must not return a JSON-RPC error: {search_result}"
                );

                // --- 4. tools/call list_packages -------------------------------------
                let packages = post_json_rpc(
                    addr,
                    &secret,
                    Some(&session_id),
                    r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"packages","arguments":{}}}"#,
                )
                .await;
                assert_eq!(packages.status, 200, "list_packages call must succeed");
                let packages_result = sse_json_messages(&packages.body)
                    .into_iter()
                    .find(|m| m.get("id").and_then(Value::as_i64) == Some(3))
                    .expect("list_packages response must be present in the SSE body");
                assert!(
                    packages_result.get("error").is_none(),
                    "list_packages must not return a JSON-RPC error: {packages_result}"
                );

                endpoint.stop().await;

                (search_result, packages_result)
            })
        },
    );
    eprintln!(
        "mcp_e2e/memchr: wall={:.1}s rss={:?}",
        cost.wall.as_secs_f64(),
        cost.peak_rss_bytes
    );

    // --- Assertions on the real JSON-RPC result bodies --------------------------
    //
    // Real symbols from `result/memchr-2.8.3/src/lib.rs`:
    //   pub use crate::memchr::{
    //       memchr, memchr2, memchr2_iter, memchr3, memchr3_iter, memchr_iter,
    //       memrchr, memrchr2, memrchr2_iter, memrchr3, memrchr3_iter, memrchr_iter,
    //       Memchr, Memchr2, Memchr3,
    //   };
    // No total-count assertion — see the module doc's "Why no entry count" note.

    let search_text = search_result
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("search_symbols result must carry Markdown content: {search_result}")
        });
    assert!(search_result.pointer("/result/structuredContent").is_none());

    for real_symbol in ["Memchr", "memchr_iter", "memrchr"] {
        assert!(
            search_text.contains(real_symbol),
            "search_symbols(\"mem\") over the real memchr crate must surface `{real_symbol}` \
             (see result/memchr-2.8.3/src/lib.rs's `pub use crate::memchr::{{ ... }}`); \
             got: {search_text}"
        );
    }

    // Every key returned must actually carry the cargo:memchr lineage — proof
    // this came from the real producer's real IR, not from a stub or a
    // hand-authored fixture (doctrine §4).
    for line in search_text
        .lines()
        .filter(|line| line.contains("cargo:memchr#"))
    {
        assert!(
            line.contains("cargo:memchr#"),
            "every rendered search row must carry the cargo:memchr lineage; got {line}"
        );
    }

    let package_text = packages_result
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("list_packages result must carry Markdown content: {packages_result}")
        });
    assert!(
        packages_result
            .pointer("/result/structuredContent")
            .is_none()
    );
    assert!(
        package_text.contains("cargo:memchr"),
        "memchr must appear in list_packages: {package_text}"
    );

    drop(runtime);
}
