//! Regression test for the telemetry/pyroscope abort (INDEX-PLAN telemetry
//! defect): `nudox-serve`, run with telemetry **enabled** (the production
//! default — `OTEL_SDK_DISABLED` unset), used to die within a couple of
//! minutes with
//!
//! ```text
//! fatal runtime error: current thread handle already set during thread spawn
//! ```
//!
//! That is a `std`-level `rtabort!` (`library/std/src/thread/lifecycle.rs`'s
//! `ThreadInit::init`), not a Rust panic: it calls `abort()` directly, with no
//! unwind and no chance to log. An in-process test harness that hosted the
//! server itself would die with it, so — same reasoning as any test that
//! needs to observe a *process*-level outcome — this drives the real
//! `nudox-serve` binary as a child process and asserts on its exit status.
//!
//! Root cause (see `workspace/heart/telemetry/pyroscope.rs` and
//! `workspace/vendor/pyroscope/src/session.rs`'s fork patch): the `pyroscope`
//! crate's `Session::upload` built a brand-new `reqwest::blocking::Client` —
//! its own dedicated OS thread driving a private Tokio runtime — on every
//! periodic flush (every ~10s, forever, for the life of the agent). Each of
//! those `pthread_create`s raced `pyroscope_pprofrs`'s SIGPROF-based sampling
//! timer (armed at 100Hz, `heart::telemetry::pyroscope::PyroscopeHandle`)
//! against `std::thread`'s per-thread "current thread handle" init: if the
//! kernel delivered SIGPROF to the brand-new pthread before Rust's own
//! `ThreadInit::init` ran, the abort followed. It reproduced from wall-clock
//! time alone (an otherwise-idle server, zero requests) as reliably as it did
//! under load — this test does both: a burst of requests, then enough idle
//! time to span several flush cycles.
//!
//! Needs `SERVER_TEST_BACKENDS=1` with qdrant reachable, exactly like the
//! rest of the server-integration tier (`tests/server_common::assembled_server`),
//! plus a real local Ollama serving `nomic-embed-text` on `:11434` — the
//! binary under test is compiled against the real `NomicEmbedText` brand
//! (`server/main.rs`), not the test tier's `JinaCodeV2` stub. Mirrors
//! `.config/scripts/local-backends.nu`'s `serve` subcommand exactly. Skips
//! (does not fail) when that extra infrastructure is not opted into, the same
//! "stays green offline, real elsewhere" contract `assembled_server` uses.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Requests fired before the idle-survival window — comfortably past the
/// "handful of requests" the bug report described, and past the `>= 50`
/// verification bar.
const REQUEST_BURST: usize = 60;

/// Total time the child must stay alive, measured from spawn. Empirically,
/// the unpatched binary aborted within 90s under load and within ~140s fully
/// idle (see the fork patch's doc comment); this clears both with margin
/// without making the suite glacially slow.
const SURVIVAL_WINDOW: Duration = Duration::from_secs(150);

fn tcp_reachable(addr: &str) -> bool {
    addr.parse::<SocketAddr>()
        .ok()
        .and_then(|addr| TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok())
        .is_some()
}

/// A minimal blocking HTTP/1.1 round trip — no client dependency needed for
/// a health probe. Returns the status line's status code, if the connection
/// and response parse succeeded.
fn http_status(addr: SocketAddr, method: &str, path: &str, body: &str) -> Option<u16> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(800)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .ok()?;
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
    if !body.is_empty() {
        use std::fmt::Write as _;
        request.push_str("Content-Type: application/json\r\n");
        let _ = write!(request, "Content-Length: {}\r\n", body.len());
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    let text = String::from_utf8_lossy(&response);
    let status_line = text.lines().next()?;
    // "HTTP/1.1 200 OK" -> 200
    status_line.split_whitespace().nth(1)?.parse().ok()
}

/// Reserve an ephemeral loopback port for the child to bind, the same
/// reserve-then-release pattern `tests/server_common::loopback_listener`
/// uses for the same reason: `Server::serve` (here, the child process) does
/// its own bind, so nothing can hand it an already-open socket.
fn reserve_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("loopback bind for port reservation");
    listener
        .local_addr()
        .expect("bound listener has an address")
        .port()
}

#[test]
fn nudox_serve_survives_pyroscope_telemetry() {
    if std::env::var_os("SERVER_TEST_BACKENDS").is_none() {
        eprintln!(
            "SKIP nudox_serve_survives_pyroscope: set SERVER_TEST_BACKENDS=1 \
             (qdrant reachable on 127.0.0.1:6333) to run"
        );
        return;
    }
    if !tcp_reachable("127.0.0.1:6333") {
        eprintln!(
            "SKIP nudox_serve_survives_pyroscope: qdrant is not reachable on \
             127.0.0.1:6333 -- run `nu .config/scripts/local-backends.nu up` first"
        );
        return;
    }
    // `nudox-serve` (server/main.rs) is compiled against the real
    // `NomicEmbedText` brand, which means a real Ollama, not the test tier's
    // stub -- see this file's module docs.
    if !tcp_reachable("127.0.0.1:11434") {
        eprintln!(
            "SKIP nudox_serve_survives_pyroscope: no Ollama reachable on \
             127.0.0.1:11434 -- nudox-serve requires a real embedder to start \
             (`ollama pull nomic-embed-text`)"
        );
        return;
    }

    let data_dir = tempfile::tempdir().expect("temp data directory");
    let catalog_dir = data_dir.path().join("catalog");
    let blobs_dir = data_dir.path().join("blobs");
    std::fs::create_dir_all(&catalog_dir).expect("catalog dir");
    std::fs::create_dir_all(&blobs_dir).expect("blobs dir");
    let object_store_url =
        url::Url::from_file_path(&blobs_dir).expect("blobs dir is a valid file:// base");

    let port = reserve_port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().expect("valid address");

    let binary = env!("CARGO_BIN_EXE_nudox-serve");
    let mut child = Command::new(binary)
        .env("NUDOX_SERVING_ADDRESS", addr.to_string())
        .env("NUDOX_DEFINITIVE__DATA_DIRECTORY", data_dir.path())
        .env(
            "NUDOX_DEFINITIVE__ENDPOINTS__CATALOG_DIRECTORY",
            &catalog_dir,
        )
        .env(
            "NUDOX_DEFINITIVE__ENDPOINTS__OBJECT_STORE",
            object_store_url.as_str(),
        )
        .env(
            "NUDOX_DEFINITIVE__ENDPOINTS__EMBEDDINGS",
            "http://127.0.0.1:11434/v1/embeddings",
        )
        .env("RUST_LOG", "warn")
        // The whole point: telemetry — and therefore pyroscope — must be
        // ON. Strip any ambient override so a CI/dev shell that happens to
        // export the workaround cannot silently defeat this test.
        .env_remove("OTEL_SDK_DISABLED")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn nudox-serve");

    // Wait for /healthz, bounded — startup does its own qdrant/embedder
    // probing (`local-backends.nu`'s `serve` mirrors this exact wait).
    let ready_deadline = Instant::now() + Duration::from_mins(1);
    let mut ready = false;
    while Instant::now() < ready_deadline {
        if let Some(status) = child.try_wait().expect("poll child status") {
            let mut stderr = String::new();
            child
                .stderr
                .take()
                .expect("stderr piped")
                .read_to_string(&mut stderr)
                .ok();
            panic!("nudox-serve exited during startup: {status:?}\n--- stderr ---\n{stderr}");
        }
        if http_status(addr, "GET", "/healthz", "") == Some(200) {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(ready, "nudox-serve did not become healthy within 60s");

    // A burst of real requests — comfortably past both the bug report's
    // "handful" and the >= 50 verification bar.
    let search_body = r#"{"target":"Symbols","text":"foo","page":{"limit":5}}"#;
    for i in 0..REQUEST_BURST {
        let status = http_status(addr, "POST", "/search", search_body);
        assert!(
            status.is_some(),
            "request {i} of {REQUEST_BURST} got no response (nudox-serve pid {}, alive: {})",
            child.id(),
            child.try_wait().ok().flatten().is_none(),
        );
    }

    // The abort this guards against did not need request load at all — it
    // reproduced from pyroscope's periodic (~10s) session-flush thread churn
    // racing pprof2's SIGPROF sampler on an otherwise-idle server. Hold here
    // long enough to span several of those cycles.
    let start = Instant::now();
    while start.elapsed() < SURVIVAL_WINDOW {
        if let Some(status) = child.try_wait().expect("poll child status") {
            let mut stderr = String::new();
            child
                .stderr
                .take()
                .expect("stderr piped")
                .read_to_string(&mut stderr)
                .ok();
            panic!(
                "nudox-serve aborted after {:?} with telemetry enabled: {status:?}\n\
                 --- stderr (tail) ---\n{}",
                start.elapsed(),
                stderr
                    .lines()
                    .rev()
                    .take(20)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        std::thread::sleep(Duration::from_secs(5));
    }

    // Still alive, still answering — telemetry did not kill it.
    let status = http_status(addr, "GET", "/healthz", "");
    assert_eq!(
        status,
        Some(200),
        "nudox-serve survived the abort window but stopped answering /healthz"
    );

    let _ = child.kill();
    let _ = child.wait();
}
