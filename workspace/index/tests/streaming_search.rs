#![cfg(feature = "server")]
//! Integration pins for the S2 unbuffering of `/search`
//! (`docs/LOCAL-REMOTE-CONTRACT.md` §3, deliverable 2 of the surface-contract
//! task): the route now streams `heart::surface::Frame<Symbols>` NDJSON
//! instead of collecting into a `Vec` first, federated sources are queried
//! **concurrently** instead of one source at a time, and a source that fails
//! **degrades** the answer instead of failing (or 5xx-ing) it.
//!
//! Gated the same way the rest of this suite is
//! (`server_common::required_assembled_server`): these tests need real
//! backends (`nu .config/scripts/local-backends.nu up`, then
//! `SERVER_TEST_BACKENDS=1`). Two of them additionally spawn a **second,
//! disposable** qdrant instance to get a genuinely two-source federation with
//! one source independently breakable — see [`spawn_disposable_qdrant`]'s own
//! doc comment for why that is the only real (non-mocked) way to isolate one
//! federated source's failure from the other's: this crate's federation is
//! concrete `SourceStores<M>`, not swappable, and `Server::assemble`'s
//! `connect_source` eagerly health-checks every source's qdrant endpoint
//! (`ensure_collection`, a real network round trip) — a source that is
//! unreachable *at assembly time* never gets far enough to become part of a
//! federation there is anything left to query — the query-time failure this
//! test needs has to be a source that dies *between* assembly and query.

mod server_common;

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use heart::client::query::{AbstractQuery, ExecutionQuery as Query, Filter, LiteralQuery, Search};
use heart::surface::{Frame, Symbols};
use heart::{Language, PackageId, PackageVersion, RegistryOrigin, ResolutionState};
use index::ecosystem::PackageNameExt as _;
use index::server::config::{Endpoints, ServerConfiguration, SourceConfig};
use index::server::http::router;
use index::server::registry::package::{Coordinates, PackageName};
use index::server::{Server, registry};
use registry::vector::JinaCodeV2;

/// Same brand `server_common::TestModel` uses — kept as its own alias here
/// (rather than importing `server_common::TestModel`) only because this file
/// also needs `JinaCodeV2` by name for `CollectionConfig::for_model`-adjacent
/// reasoning in its doc comments; the type itself is identical.
type TestModel = JinaCodeV2;

// ---------------------------------------------------------------------------
// 1. `/search` emits `Frame<Symbols>` NDJSON, terminated by exactly one `End`
// ---------------------------------------------------------------------------

/// The wire shape, pinned end-to-end through the real router: every line of
/// the response is a well-formed `Frame<Symbols>`, and the stream carries
/// **exactly one** terminal `Frame::End` (never zero — a truncated stream —
/// and never more than one, which would be a writer bug `Frame`'s own
/// terminal-frame discipline exists to make impossible).
///
/// An empty (no-match) corpus is deliberately the fixture here: this test is
/// about the *envelope*, not about finding real data — that is deliverable 3's
/// `search_bytes_leave_before_the_answer_is_complete` below.
#[tokio::test]
async fn search_streams_frame_symbols_ndjson_terminated_by_one_end() {
    let (server, _data) = server_common::required_assembled_server(
        "search_streams_frame_symbols_ndjson_terminated_by_one_end",
    )
    .await;
    let app = router::router(Arc::clone(&server));

    // Not `server_common::call`/`call_json`: both fully collect the body and
    // then attempt to parse it as ONE JSON document, which happens to
    // succeed for a single-frame (empty-corpus) body — masking the fact that
    // a real multi-frame NDJSON body is *not* one JSON document at all.
    // Collecting the raw bytes and splitting on lines ourselves is what
    // actually exercises the NDJSON framing this test is about.
    let request = server_common::post_json(
        "/search",
        &serde_json::json!({ "target": "Symbols", "text": "zz-no-such-symbol-zz" }),
    );
    let response = tower::ServiceExt::oneshot(app, request)
        .await
        .expect("router answers");
    let status = response.status();
    assert!(
        status.is_success(),
        "an empty-corpus search must not fail; got {status}"
    );
    let body = http_body_util::BodyExt::collect(response.into_body())
        .await
        .expect("body collects")
        .to_bytes();
    let text = String::from_utf8(body.to_vec()).expect("ndjson body is utf8");
    let frames = parse_ndjson_frames(&text);
    assert!(
        !frames.is_empty(),
        "the stream must carry at least the terminal frame"
    );

    let end_count = frames.iter().filter(|f| matches!(f, Frame::End(_))).count();
    assert_eq!(
        end_count, 1,
        "exactly one terminal End frame must be present, got {frames:?}"
    );
    assert!(
        matches!(frames.last(), Some(Frame::End(_))),
        "the End frame must be the LAST frame, got {frames:?}"
    );
    assert!(
        !frames.iter().any(|f| matches!(f, Frame::Failed(_))),
        "a healthy, empty-but-successful corpus must never carry a Failed frame, got {frames:?}"
    );
}

/// Split an NDJSON body into typed `Frame<Symbols>` values, one per non-blank
/// line — the same discipline `heart::surface::decode_frames` applies
/// incrementally to a byte stream, applied here to an already-collected body.
fn parse_ndjson_frames(body: &str) -> Vec<Frame<Symbols>> {
    body.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("line {line:?} is not a Frame<Symbols>: {error}"))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 2. Bytes leave before the answer is complete
// ---------------------------------------------------------------------------

/// The tiny, dependency-free fixture crate `pipeline_end_to_end.rs` also
/// chews through — reused here (duplicated rather than imported, since test
/// binaries are separate crates and these helpers are file-private there) so
/// this test exercises a *real* hit, not an empty corpus, which is the whole
/// point: an empty corpus can only ever produce one frame (`End`), so it
/// cannot demonstrate "a hit arrived before the answer was complete" at all.
const FIXTURE_NAME: &str = "either";
const FIXTURE_VERSION: &str = "1.15.0";
const FIXTURE_SYMBOL: &str = "Either";
const INGEST_DEADLINE: Duration = Duration::from_secs(300);
const PROBE_INTERVAL: Duration = Duration::from_millis(500);

/// Extends the ≥3-body-frames idea already pinned at the unit level
/// (`server::http::handlers::search::tests::
/// symbol_answer_response_emits_incremental_ndjson_frames_not_one_static_body`,
/// itself the direct successor of the pre-S2
/// `hit_stream_emits_incremental_ndjson_frames_not_one_static_body`) all the
/// way through the **real** router, a **real** assembled server, and a
/// **real** ingested package — so this is the test that would fail if
/// `search()`'s `.try_collect()` (the exact buffer point this whole change
/// exists to remove) were ever reintroduced: collecting the whole answer
/// before writing anything would still produce a well-formed NDJSON body, but
/// as a single `Body::from_stream` chunk arriving only once every frame was
/// already known, not the ≥2 distinct chunks asserted below.
#[tokio::test]
async fn search_bytes_leave_before_the_answer_is_complete() {
    let (server, _data) = server_common::required_assembled_server(
        "search_bytes_leave_before_the_answer_is_complete",
    )
    .await;
    let _pollers = server_common::spawn_pollers(&server);

    let package = ensure_fixture_stored(&server).await;
    wait_for_text_hit(&server, FIXTURE_SYMBOL, package).await;

    let app = router::router(Arc::clone(&server));
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/search")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            serde_json::json!({ "target": "Symbols", "text": FIXTURE_SYMBOL }).to_string(),
        ))
        .expect("request builds");

    let response = tower::ServiceExt::oneshot(app, request)
        .await
        .expect("router answers");
    assert_eq!(response.status(), axum::http::StatusCode::OK);

    let mut body = response.into_body();
    let mut chunks: Vec<bytes::Bytes> = Vec::new();
    while let Some(frame) = http_body_util::BodyExt::frame(&mut body).await {
        let frame = frame.expect("stream body must not fail");
        if let Ok(data) = frame.into_data() {
            chunks.push(data);
        }
    }

    assert!(
        chunks.len() >= 2,
        "a real hit plus the terminal frame must arrive as at least two distinct \
         body chunks — a single chunk here is exactly what a reintroduced \
         `.try_collect()` before `Body::from_stream` would produce; got {} chunk(s)",
        chunks.len()
    );

    let joined: Vec<u8> = chunks.iter().flat_map(|chunk| chunk.iter().copied()).collect();
    let text = String::from_utf8(joined).expect("ndjson body is utf8");
    let frames = parse_ndjson_frames(&text);

    let (last, earlier) = frames.split_last().expect("at least one frame arrived");
    assert!(
        matches!(last, Frame::End(_)),
        "the last frame must be the terminal End, got {frames:?}"
    );
    assert!(
        earlier.iter().any(|f| matches!(f, Frame::Item(_))),
        "a hit must arrive BEFORE the terminal frame, not only alongside it — got {frames:?}"
    );
}

/// Track the fixture package and wait until the pipeline reaches `Stored`.
async fn ensure_fixture_stored(server: &Arc<Server<TestModel>>) -> PackageId {
    let coordinates = Coordinates {
        origin: RegistryOrigin::CratesIo,
        name: PackageName::new(Language::Rust, FIXTURE_NAME).expect("fixture name is valid"),
        version: PackageVersion::try_from((Language::Rust, FIXTURE_VERSION))
            .expect("fixture version is valid"),
    };
    let cap = server_common::write_cap(server);
    let package = server
        .ensure_initialized(&cap, &coordinates)
        .await
        .expect("fixture package ensures cleanly")
        .package;

    let deadline = Instant::now() + INGEST_DEADLINE;
    loop {
        match server.parse_status(package).await {
            Ok(Some(ResolutionState::Stored { .. })) => return package,
            Ok(Some(ResolutionState::DeadLettered(failure) | ResolutionState::Failed(failure))) => {
                panic!("the pipeline failed: {failure:?}")
            }
            _ => {}
        }
        assert!(
            Instant::now() <= deadline,
            "fixture package did not reach Stored within {INGEST_DEADLINE:?}"
        );
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
}

/// Wait until a literal (precise) search for `name` surfaces a hit from
/// `package` — the text poller has to catch up to the just-`Stored` package
/// before `search_symbols` can find it.
async fn wait_for_text_hit(server: &Arc<Server<TestModel>>, name: &str, package: PackageId) {
    let request = Search {
        query: Query::Literal(LiteralQuery::parse(name).expect("fixture query is valid")),
        filter: Filter::default(),
        page: server_common::page(16),
        _lifetime: std::marker::PhantomData,
    };
    let deadline = Instant::now() + INGEST_DEADLINE;
    loop {
        let cap = server_common::read_cap(server);
        if let Ok(answer) = server.search_symbols(&cap, &request).await
            && let Ok(hits) = server_common::collect_symbol_answer(answer).await
            && hits.iter().any(|hit| hit.value.package == package)
        {
            return;
        }
        assert!(
            Instant::now() <= deadline,
            "text search for {name:?} never surfaced package {package:?} within {INGEST_DEADLINE:?}"
        );
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// 3 & 4. A genuine two-source federation: concurrency and degradation
// ---------------------------------------------------------------------------
//
// Both properties need >=1 federated overlay in addition to the suite's usual
// single (definitive-only) server, and — for degradation specifically — a way
// to make exactly ONE of the two sources fail without touching the other.
// `SourceStores<M>`'s only per-source, over-the-network-and-therefore-
// independently-breakable backend is its qdrant `RemoteStore` (catalog/
// object-store are local directories; the embedder is shared server-wide,
// not per-source — see `coordination::search::semantic_source_answer`'s own
// doc comment). Both tests below therefore give the overlay its own,
// test-owned qdrant process rather than the shared one `local-backends.nu`
// manages, so it can be reasoned about (delayed, killed) independently of the
// real backend the rest of this suite depends on.

/// A disposable, test-owned qdrant process bound to test-chosen loopback
/// ports, killed on drop.
///
/// # Why this exists instead of a mock
///
/// `Server<M>`'s federation is `heart::Federation<SourceStores<M>>` —
/// concrete stores, not `Box<dyn SearchTarget>` or anything else swappable —
/// so there is no seam in this crate to hand a federated source a fake vector
/// store. Only a real qdrant process the test controls can play the role of
/// "a source whose backend the test can independently take away."
struct DisposableQdrant {
    child: Child,
    grpc_port: u16,
    #[allow(dead_code, reason = "keeps the storage directory alive for the process's lifetime")]
    storage_dir: server_common::TempDir,
}

impl Drop for DisposableQdrant {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Locate a `qdrant` binary the same way `.config/scripts/local-backends.nu`
/// does: `PATH` first, then the newest already-realized `/nix/store/*-qdrant-*`
/// output. Returns `None` (never panics) so a machine without a second
/// realizable qdrant skips these two tests loudly rather than failing the
/// whole suite over an environment gap unrelated to the code under test.
fn find_qdrant_binary() -> Option<PathBuf> {
    if let Ok(output) = Command::new("which").arg("qdrant").output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    let mut candidates: Vec<PathBuf> = std::fs::read_dir("/nix/store")
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("-qdrant-"))
        })
        .filter(|path| path.join("bin").join("qdrant").is_file())
        .collect();
    candidates.sort();
    candidates.pop().map(|path| path.join("bin").join("qdrant"))
}

/// Bind an ephemeral loopback port and immediately release it, the same
/// "let the OS pick, accept the tiny TOCTOU race" idiom `server_common::
/// loopback_listener`'s callers already rely on (and the same one
/// `ServerConfiguration::serving_address = ([127,0,0,1], 0)` uses for the
/// server itself throughout this suite).
fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("local_addr")
        .port()
}

/// Spawn a disposable qdrant on fresh loopback ports with its own storage
/// directory, and wait until it answers `health_check`. `None` when no
/// `qdrant` binary is discoverable at all — the caller must skip, not fail.
async fn spawn_disposable_qdrant(label: &str) -> Option<DisposableQdrant> {
    let binary = find_qdrant_binary()?;
    let storage_dir = server_common::TempDir::new(&format!("{label}-qdrant-storage"));
    let grpc_port = free_port();
    let http_port = free_port();

    let child = Command::new(&binary)
        .env("QDRANT__STORAGE__STORAGE_PATH", storage_dir.path())
        .env("QDRANT__SERVICE__GRPC_PORT", grpc_port.to_string())
        .env("QDRANT__SERVICE__HTTP_PORT", http_port.to_string())
        .env("QDRANT__TELEMETRY_DISABLED", "true")
        .env("QDRANT__LOG_LEVEL", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn disposable qdrant ({}): {error}", binary.display()));

    let mut disposable = DisposableQdrant {
        child,
        grpc_port,
        storage_dir,
    };

    let grpc_url = format!("http://127.0.0.1:{grpc_port}");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(client) = qdrant_client::Qdrant::from_url(&grpc_url).build()
            && client.health_check().await.is_ok()
        {
            return Some(disposable);
        }
        if Instant::now() > deadline {
            let _ = disposable.child.kill();
            panic!(
                "disposable qdrant ({}) did not become healthy on {grpc_url} within 30s",
                binary.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// A minimal TCP proxy: accept on an ephemeral loopback port, and for every
/// chunk read in *either* direction, sleep `delay` before forwarding it to
/// `target_port` on `127.0.0.1`.
///
/// This is what turns "two real sources" into "two real sources with a
/// controlled, large, jitter-dominating latency difference" for the
/// concurrency test below — real per-request qdrant round trips on an empty,
/// freshly-created local collection are fast (tens of milliseconds) and too
/// close to each other to distinguish "awaited concurrently" from "awaited in
/// series" without either a proxy like this or a flaky, tight timing budget.
///
/// # Why per-chunk, not a one-time delay before the connection opens
///
/// The first cut of this delayed only the initial TCP connect. That looked
/// right but measured nothing: qdrant's client speaks gRPC over one
/// persistent HTTP/2 connection, established once inside `Server::assemble`'s
/// `connect_source` (`ensure_collection`, a real round trip) — long before
/// this test's own timer starts. A later `search_symbols` call reuses that
/// already-open, already-delayed-once connection, so a connect-only delay is
/// paid exactly once, at assembly, and invisible to a query issued afterward
/// — which is exactly why the first version of this test measured ~50ms
/// against a nominal 2-second delay and still reported a false pass. Delaying
/// every chunk instead means the delay lands on the actual request/response
/// frames of the *query* this test measures, not merely on the handshake.
///
/// Returns the proxy's bound port; the proxy task runs for the lifetime of
/// the test process (a `tokio::test` process-per-test, so nothing to clean up
/// explicitly).
async fn spawn_delaying_proxy(target_port: u16, delay: Duration) -> u16 {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind proxy listener");
    let proxy_port = listener.local_addr().expect("local_addr").port();
    tokio::spawn(async move {
        loop {
            let Ok((inbound, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let Ok(outbound) =
                    tokio::net::TcpStream::connect(("127.0.0.1", target_port)).await
                else {
                    return;
                };
                let (inbound_read, inbound_write) = inbound.into_split();
                let (outbound_read, outbound_write) = outbound.into_split();
                tokio::join!(
                    throttled_copy(inbound_read, outbound_write, delay),
                    throttled_copy(outbound_read, inbound_write, delay),
                );
            });
        }
    });
    proxy_port
}

/// Copy from `read` to `write`, sleeping `delay` before forwarding each chunk
/// — see [`spawn_delaying_proxy`]'s doc comment for why per-chunk (not
/// per-connection) is what actually delays a request issued over an
/// already-open connection.
async fn throttled_copy(
    mut read: tokio::net::tcp::OwnedReadHalf,
    mut write: tokio::net::tcp::OwnedWriteHalf,
    delay: Duration,
) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let mut buffer = [0_u8; 8192];
    loop {
        let read_bytes = match read.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        tokio::time::sleep(delay).await;
        if write.write_all(&buffer[..read_bytes]).await.is_err() {
            return;
        }
    }
}

/// Assemble a real two-source federation: the suite's usual definitive base
/// (same config-resolution path `server_common::assemble_server` uses) plus
/// one overlay bound to `overlay_qdrant`. When `definitive_qdrant` is `Some`,
/// it overrides the definitive base's qdrant endpoint too — used by the
/// concurrency test below to point *both* sources at independently-proxied
/// connections to the same disposable qdrant instance, so the measurement
/// never touches (or contends with) the shared real backend the rest of this
/// suite depends on.
async fn two_source_server(
    label: &str,
    overlay_qdrant: url::Url,
    definitive_qdrant: Option<url::Url>,
) -> (Arc<Server<TestModel>>, server_common::TempDir, server_common::TempDir) {
    let data_directory = server_common::TempDir::new(label);
    let overlay_directory = server_common::TempDir::new(&format!("{label}-overlay"));

    let mut configuration =
        ServerConfiguration::resolve().expect("test configuration resolves");
    configuration.definitive.data_directory = Some(data_directory.path().to_path_buf());
    configuration.definitive.endpoints.catalog_directory = data_directory.path().join("catalog");
    configuration.definitive.endpoints.object_store =
        url::Url::from_file_path(data_directory.path().join("blobs"))
            .expect("test data directory path is a valid file:// base");
    configuration.serving_address = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
    if let Some(definitive_qdrant) = definitive_qdrant {
        configuration.definitive.endpoints.qdrant = definitive_qdrant;
    }

    let mut overlay_endpoints = Endpoints::localhost_defaults();
    overlay_endpoints.catalog_directory = overlay_directory.path().join("catalog");
    overlay_endpoints.object_store =
        url::Url::from_file_path(overlay_directory.path().join("blobs"))
            .expect("overlay directory path is a valid file:// base");
    overlay_endpoints.embeddings = configuration.definitive.endpoints.embeddings.clone();
    overlay_endpoints.embeddings_api_key =
        configuration.definitive.endpoints.embeddings_api_key.clone();
    overlay_endpoints.qdrant = overlay_qdrant;

    configuration.overlays.push(SourceConfig {
        name: "disposable-overlay".into(),
        endpoints: overlay_endpoints,
        data_directory: Some(overlay_directory.path().to_path_buf()),
        sync_endpoint: None,
    });

    let server = Server::<TestModel>::assemble(configuration)
        .await
        .expect("two-source federation assembles (both qdrant endpoints answered ensure_collection)");

    (Arc::new(server), data_directory, overlay_directory)
}

fn natural_language_search(text: &str) -> Search<'static> {
    Search {
        query: Query::Abstract(AbstractQuery::NaturalLanguage(text.to_owned())),
        filter: Filter::default(),
        page: server_common::page(8),
        _lifetime: std::marker::PhantomData,
    }
}

/// A generous ceiling well under what a genuinely *sequential* two-source
/// wait would take (`PROXY_DELAY` alone, added on top of whatever the fast
/// source and the embed call cost, comfortably clears this). Not tight
/// against `PROXY_DELAY` itself, to absorb real scheduler/network jitter —
/// see `spawn_delaying_proxy`'s doc comment for why a large, controlled delay
/// (rather than a tight budget against real unproxied qdrant latency) is what
/// makes this assertion meaningfully non-flaky.
const PROXY_DELAY: Duration = Duration::from_millis(300);
const CONCURRENCY_CEILING: Duration = Duration::from_millis(1800);

/// Sources are queried **concurrently**, not one after another.
///
/// `coordination::search::precise_hits`/`semantic_hits` used to `for`-loop
/// over `self.federation().in_precedence()`, awaiting each source's *entire*
/// search before starting the next — a federation of two 2-second sources
/// took 4 seconds, not 2. They now fan out via `futures::future::join_all`
/// before folding with `heart::surface::merge` (itself already pinned
/// concurrent at the `heart` level by
/// `merge::tests::sources_are_polled_concurrently_not_sequentially`). This is
/// the same property, proven through the real federation this server
/// actually assembles rather than a synthetic `heart`-level fixture.
///
/// Both sources — not just the overlay — sit behind their own independent
/// [`spawn_delaying_proxy`] in front of the *same* disposable qdrant
/// instance, each adding `PROXY_DELAY` per chunk. This is deliberate: an
/// earlier version of this test delayed only the overlay while the
/// definitive base answered a real, fast (tens-of-milliseconds) qdrant round
/// trip — under which a *sequential* regression (base + overlay) and true
/// concurrency (`max(base, overlay)`) are nearly indistinguishable, because
/// the fast source barely adds to the slow one either way. Giving both
/// sources comparable, substantial latency makes the two hypotheses cleanly
/// separable: concurrent ≈ one delay's worth of round trips; sequential ≈
/// two. `CONCURRENCY_CEILING` sits between them.
#[tokio::test]
async fn semantic_search_queries_federated_sources_concurrently() {
    let label = "semantic_search_queries_federated_sources_concurrently";
    let Some(qdrant) = spawn_disposable_qdrant(label).await else {
        eprintln!("SKIP {label}: no qdrant binary discoverable to spawn a second instance");
        return;
    };
    let overlay_proxy_port = spawn_delaying_proxy(qdrant.grpc_port, PROXY_DELAY).await;
    let definitive_proxy_port = spawn_delaying_proxy(qdrant.grpc_port, PROXY_DELAY).await;
    let overlay_endpoint =
        url::Url::parse(&format!("http://127.0.0.1:{overlay_proxy_port}")).expect("valid url");
    let definitive_endpoint =
        url::Url::parse(&format!("http://127.0.0.1:{definitive_proxy_port}")).expect("valid url");

    let (server, _data, _overlay_data) =
        two_source_server(label, overlay_endpoint, Some(definitive_endpoint)).await;
    let cap = server_common::read_cap(&server);
    let request = natural_language_search("a value that is one of two alternatives");

    let started = Instant::now();
    let answer = server
        .search_symbols(&cap, &request)
        .await
        .expect("a two-source semantic search over empty corpora answers cleanly");
    let outcome = server_common::collect_symbol_answer(answer).await;
    let elapsed = started.elapsed();

    outcome.expect("both sources are healthy: no degradation expected in this test");
    eprintln!(
        "{label}: federated query (both sources proxied, {PROXY_DELAY:?}/chunk) took {elapsed:?}"
    );
    assert!(
        elapsed < CONCURRENCY_CEILING,
        "a two-source federation with BOTH sources delayed ({PROXY_DELAY:?}/chunk) took \
         {elapsed:?} — consistent with the sources being awaited sequentially again \
         (roughly source_a + source_b) rather than concurrently (roughly \
         max(source_a, source_b)); ceiling is {CONCURRENCY_CEILING:?}"
    );
}

/// A failing source degrades the answer; it does not fail it, and the HTTP
/// route never turns it into a 5xx.
///
/// The overlay's disposable qdrant is killed *after* the federation has
/// successfully assembled (both `ensure_collection` checks passed) but
/// *before* the query — so this is a genuine "a real backend has died between
/// connect and query," not a mock. `heart::surface::merge` is what turns that
/// source's resulting `Frame::Failed` into a `Frame::Degraded` plus a
/// `Completeness::Partial` `End` (`merge::tests::
/// one_source_failing_degrades_the_answer_but_does_not_fail_it` pins the same
/// rule at the `heart` level); this test pins that `coordination::search`
/// wires a real per-source failure into that same path, and that the HTTP
/// handler never turns it into a 5xx status.
#[tokio::test]
async fn a_failing_federated_source_degrades_the_answer_not_a_5xx() {
    let label = "a_failing_federated_source_degrades_the_answer_not_a_5xx";
    let Some(mut qdrant) = spawn_disposable_qdrant(label).await else {
        eprintln!("SKIP {label}: no qdrant binary discoverable to spawn a second instance");
        return;
    };
    let overlay_endpoint =
        url::Url::parse(&format!("http://127.0.0.1:{}", qdrant.grpc_port)).expect("valid url");

    let (server, _data, _overlay_data) = two_source_server(label, overlay_endpoint, None).await;

    // The federation assembled successfully — both sources answered
    // `ensure_collection` — so kill the overlay's backend now, strictly
    // between "connect" and "query."
    let _ = qdrant.child.kill();
    let _ = qdrant.child.wait();

    let app = router::router(Arc::clone(&server));
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/search")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            serde_json::json!({
                "target": "Symbols",
                "text": "a value that is one of two alternatives",
                "mode": "semantic"
            })
            .to_string(),
        ))
        .expect("request builds");

    let response = tower::ServiceExt::oneshot(app, request)
        .await
        .expect("router answers");
    assert!(
        response.status().is_success(),
        "a degraded (not fully failed) federation must answer 2xx, got {}",
        response.status()
    );

    let body = http_body_util::BodyExt::collect(response.into_body())
        .await
        .expect("body collects")
        .to_bytes();
    let text = String::from_utf8(body.to_vec()).expect("ndjson body is utf8");
    let frames = parse_ndjson_frames(&text);

    assert!(
        !frames.iter().any(|f| matches!(f, Frame::Failed(_))),
        "the answer must not be terminally Failed — the OTHER source is healthy; got {frames:?}"
    );
    assert!(
        frames.iter().any(|f| matches!(f, Frame::Degraded(_))),
        "the dead overlay must surface as a Degraded frame; got {frames:?}"
    );
    match frames.last() {
        Some(Frame::End(summary)) => assert!(
            !summary.is_complete(),
            "an answer that lost a source must report Partial, not Complete; got {summary:?}"
        ),
        other => panic!("the stream must end with End(Partial), got {other:?}"),
    }
}
