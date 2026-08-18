//! Shared fixtures for the server integration tests: symbol/package
//! constructors and the env-gated assembly of a full `Server` over live
//! localhost backends.
#![allow(dead_code, reason = "each integration test binary uses its own subset")]

#[allow(unused_imports)]
use index::server::registry;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use heart::PageSpecification;
use heart::client::query::{ExecutionQuery as Query, Filter, Search};
use heart::{EntryUri, Language, Name, PackageId, Symbol, SymbolId, SymbolKind};
use index::server::authz::{Principal, ReadCap, WriteCap};
use index::server::{BackgroundWorkers, Server, ServerConfiguration};
use smol_str::SmolStr;

/// The embedding-model brand every test monomorphizes over. The sealed model
/// set is now `JinaCodeV2` (parity) / `VoyageCode3` (premium); tests use the
/// self-hostable parity brand. The brand only matters for type identity here,
/// never for real vectors.
pub type TestModel = registry::vector::JinaCodeV2;

/// The graph-instance token test symbol ids are salted with.
pub const TEST_INSTANCE: &str = "test-org/test-db";

/// A deterministic fixture package id (the coordinates fingerprint of a name).
pub fn package_id(name: &str) -> PackageId {
    PackageId::from_name(&uuid::Uuid::NAMESPACE_OID, name.as_bytes())
}

/// A fixture symbol with its id minted the real way: through [`EntryUri`], so
/// determinism assertions in these tests exercise production identity.
pub fn rust_symbol(
    package: PackageId,
    plain: &str,
    fully_qualified: &str,
    kind: SymbolKind,
) -> Symbol {
    let uri = EntryUri {
        package,
        path: fully_qualified.split("::").map(SmolStr::new).collect(),
    };
    Symbol {
        id: uri.symbol_id(TEST_INSTANCE),
        package,
        ecosystem: Language::Rust,
        name: Name {
            plain: SmolStr::new(plain),
            fully_qualified: SmolStr::new(fully_qualified),
        },
        kind,
    }
}

/// The id a fixture symbol would have — for negative lookups.
pub fn absent_symbol_id() -> SymbolId {
    let uri = EntryUri {
        package: package_id("never-ingested"),
        path: ["never", "ingested"].map(SmolStr::new_static).into(),
    };
    uri.symbol_id(TEST_INSTANCE)
}

/// A test read capability — mints a read cap from the anonymous principal.
pub fn read_cap<M: registry::vector::EmbeddingModel>(server: &Server<M>) -> ReadCap {
    server
        .authorize_read(&Principal::anonymous(), "test.read")
        .expect("allow-all policy always grants a read cap in tests")
}

/// A test write capability — mints a write cap from the anonymous principal.
pub fn write_cap<M: registry::vector::EmbeddingModel>(server: &Server<M>) -> WriteCap {
    server
        .authorize_write(&Principal::anonymous(), "test.write")
        .expect("allow-all policy always grants a write cap in tests")
}

/// A literal search request over `query` with an unbounded filter.
pub fn literal_search(query: &str, limit: u32) -> Search<'static> {
    use heart::client::query::LiteralQuery;
    Search {
        query: Query::Literal(LiteralQuery::parse(query).expect("fixture queries are valid")),
        filter: Filter::default(),
        page: page(limit),
        _lifetime: std::marker::PhantomData,
    }
}

/// A page of `limit` results from the start.
pub fn page(limit: u32) -> PageSpecification {
    PageSpecification {
        limit,
        cursor: None,
    }
}

/// Drain a `heart::surface::Answer<heart::surface::Symbols>` into a plain
/// `Vec`, the way callers collected `search_symbols`'s returned stream before
/// S2 (`Vec<Scored<Symbol>> = server.search_symbols(..).await?.try_collect().await?`).
///
/// `search_symbols` now returns an `Answer<Symbols>` instead of a fallible
/// `Stream` — under the streaming envelope a failure is a *terminal* frame
/// (`Frame::Failed`), not a per-item `Err` interleaved with hits, so the
/// closest equivalent to the old `Result<Vec<_>, _>` is "the whole answer
/// either finished with `End` or it didn't": every `Frame::Item` collects into
/// the `Vec`, and a `Frame::Failed` fails the whole collection rather than
/// just the one item that triggered it. `Frame::Degraded` is deliberately
/// *not* treated as failure here — same as everywhere else this module's
/// `merge` semantics apply, a degraded source does not sink an answer other
/// sources already partly satisfied.
pub async fn collect_symbol_answer(
    answer: heart::surface::Answer<heart::surface::Symbols>,
) -> Result<Vec<heart::Scored<heart::surface::SymbolHit>>, heart::stream::WireError> {
    let mut hits = Vec::new();
    while let Some(frame) = answer.recv().await {
        match frame {
            heart::surface::Frame::Item(located) => hits.push(located.value),
            heart::surface::Frame::End(_) => return Ok(hits),
            heart::surface::Frame::Failed(error) => return Err(error),
            // `Frame` is `#[non_exhaustive]` outside `heart`; `Degraded`,
            // `Note`, and any future frame kind (a progress marker, say) all
            // fold into "keep reading" here — this helper only cares about
            // items and the terminal outcome.
            _ => {}
        }
    }
    Ok(hits)
}

/// A unique temporary directory removed on drop (for package-index replicas and
/// per-test server data directories).
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("server-test-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("temp dir under std::env::temp_dir is creatable");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The fully-assembled server the infrastructure-gated specs run against, or
/// `None` (with a skip note) when the backends are not opted in / reachable —
/// so the suite stays green offline while remaining a real test where the
/// stack (catalog, qdrant, terminus, object store) exists.
///
/// Opt in with `SERVER_TEST_BACKENDS=1`; endpoints come from the same
/// `NUDOX_*` layering production uses, defaulting to localhost.
pub async fn assembled_server(test: &str) -> Option<(Arc<Server<TestModel>>, TempDir)> {
    if std::env::var_os("SERVER_TEST_BACKENDS").is_none() {
        eprintln!(
            "skipping {test}: set SERVER_TEST_BACKENDS=1 (with catalog/qdrant/terminus/object \
             store reachable) to run server integration tests"
        );
        return None;
    }
    match assemble_server(test).await {
        Ok(server) => Some(server),
        Err(error) => {
            eprintln!("skipping {test}: backends opted in but unreachable: {error}");
            None
        }
    }
}

/// Assemble the live backend stack and fail the test if it is not available.
///
/// Unlike [`assembled_server`], this is for tests whose assertions are only
/// meaningful against real dependencies. Missing opt-in, configuration errors,
/// and unreachable backends are all hard failures rather than skips.
pub async fn required_assembled_server(test: &str) -> (Arc<Server<TestModel>>, TempDir) {
    if !std::env::var_os("SERVER_TEST_BACKENDS").is_some_and(|value| !value.as_os_str().is_empty())
    {
        panic!(
            "{test} requires live backends: set SERVER_TEST_BACKENDS=1 and make the \
             catalog/qdrant/object-store dependencies reachable"
        );
    }

    assemble_server(test).await.unwrap_or_else(|error| {
        panic!("{test} requires live backends, but assembly failed: {error}")
    })
}

async fn assemble_server(test: &str) -> Result<(Arc<Server<TestModel>>, TempDir), String> {
    let data_directory = TempDir::new(test);
    let mut configuration = ServerConfiguration::resolve()
        .map_err(|error| format!("test configuration resolves: {error}"))?;
    configuration.definitive.data_directory = Some(data_directory.path().to_path_buf());

    // `Endpoints::localhost_defaults()` (workspace/index/server/config.rs) —
    // what `ServerConfiguration::resolve()` falls back to when no
    // `NUDOX_DEFINITIVE__ENDPOINTS__*` override is set — points
    // `catalog_directory` and `object_store` at *fixed* paths
    // (`./data/catalog`, `$TMPDIR/nudox/blobs`), not the per-test
    // `data_directory` above. Left alone, every server-integration test in
    // the suite opens the *same* on-disk DoltLite catalog and blob store.
    // Under real parallelism (cargo-nextest's default is one process per
    // test) that produced, empirically: `SQLITE_BUSY`/"database is locked"
    // panics (no `busy_timeout` is configured on the catalog connection —
    // workspace/vendor/rusqdoltlite/connection.rs's `Connection::open`, a
    // separate product-side gap, reported rather than patched here) and
    // cross-test data contamination (one test's added package visible to
    // another's "does this add return the same identity" assertion). Each
    // test gets its own catalog directory and blob store here so the
    // isolation the per-test `data_directory` above already implies is
    // actually complete.
    configuration.definitive.endpoints.catalog_directory = data_directory.path().join("catalog");
    configuration.definitive.endpoints.object_store =
        url::Url::from_file_path(data_directory.path().join("blobs"))
            .map_err(|()| "test data directory path is not a valid file:// base".to_owned())?;

    // Server::serve binds this address itself. Port zero lets that bind choose
    // an ephemeral port atomically instead of reserving a port and releasing it
    // before the server starts, which leaves a bind-then-release race.
    configuration.serving_address = SocketAddr::from(([127, 0, 0, 1], 0));
    let server = Server::assemble(configuration)
        .await
        .map_err(|error| format!("backends are unreachable: {error}"))?;
    Ok((Arc::new(server), data_directory))
}

/// Reserve a loopback listener for helpers that need to keep the port held
/// across setup. The caller owns the listener and must keep it alive until the
/// consumer has taken over the socket.
pub fn loopback_listener() -> std::io::Result<std::net::TcpListener> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Start this test server's background pollers (fan-out consumers + the
/// replica-local index sync/watermark loops) so a tracked package's symbols
/// actually get materialized into the text/vector/graph stores — the same
/// consume path production runs, exercised for real rather than stubbed.
///
/// This calls [`Server::spawn_background_workers`] — the poller-only seam
/// [`Server::serve`] itself is built on — instead of `serve()`/`serve_on()`.
/// None of these tests talk to the server over HTTP (they call
/// `search_symbols`/`expand`/`outbox()`/… directly in-process), so binding a
/// TCP listener would be pure overhead and, with cargo-nextest running many
/// of these tests as concurrent processes, unnecessary port-contention risk;
/// the narrower seam keeps the test hermetic.
///
/// The returned [`BackgroundWorkers`] owns every spawned task, pollers and
/// (if this test server's role runs one) the indexing queue worker alike:
/// hold it for as long as the test needs materialization to keep happening,
/// then let it drop before the test's `TempDir` does (declare the binding
/// *after* the `(server, _data)` one — Rust drops locals in reverse
/// declaration order, so the pollers stop writing before their directory is
/// removed). Dropping it aborts everything at the workers' next await point;
/// call [`BackgroundWorkers::shutdown`] instead for a graceful queue drain.
/// Either way nothing outlives the guard — unlike a bare `tokio::spawn` of
/// `serve()`, which has no caller-side handle to stop it with (`serve()`'s
/// only shutdown path waits for a process-level SIGINT/SIGTERM, which a test
/// process never sends) and — even wrapped in a bounded
/// `tokio::time::timeout`, as this helper used to — leaked the queue worker
/// past that timeout anyway: it was a bare `JoinHandle` local to `serve_on`,
/// not tracked by the `pollers` `JoinSet` that timing out the future
/// aborts. cargo-nextest's leak detector caught exactly this on the prior
/// version of this helper (`deferred_external_symbol_resolves_later` marked
/// LEAK: it and its sibling tests reached their assertions in ~31s in
/// isolation, but the un-cancelled poller/queue-worker tasks from earlier
/// tests in the same `cargo nextest run` kept running and contending for
/// CPU/the shared embeddings stub afterward, degrading the *other* tests
/// enough that their probes missed the 300s `PIPELINE_DEADLINE`).
#[must_use = "dropping this immediately stops the pollers it started"]
pub fn spawn_pollers(server: &Arc<Server<TestModel>>) -> BackgroundWorkers {
    server.spawn_background_workers()
}

/// A bounded, abort-on-drop server task for tests that need explicit lifetime
/// management. `Server::serve` has process-signal shutdown only, so this helper
/// supplies the test-side bound and makes dropping the handle stop the task.
pub struct ServingTask {
    handle: Option<tokio::task::JoinHandle<Result<(), String>>>,
}

impl ServingTask {
    /// Wait for serving to finish or report its bounded failure.
    pub async fn wait(mut self) -> Result<(), String> {
        let handle = self.handle.take().expect("serving task handle is present");
        handle
            .await
            .map_err(|error| format!("serving task panicked or was cancelled: {error}"))?
    }

    /// Abort serving immediately. Dropping the task also performs this action.
    pub fn abort(&self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }

    /// Abort and reap the serving task before returning.
    pub async fn shutdown(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
            let _ = handle.await;
        }
    }
}

impl Drop for ServingTask {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

/// Start serving with a hard upper bound and an explicit task lifetime.
pub fn spawn_serving_bounded(server: Arc<Server<TestModel>>, timeout: Duration) -> ServingTask {
    let handle = tokio::spawn(async move {
        match tokio::time::timeout(timeout, server.serve()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(format!("background serve exited: {error}")),
            Err(_) => Err(format!("background serve exceeded {timeout:?}")),
        }
    });
    ServingTask {
        handle: Some(handle),
    }
}

/// Errors returned by [`call_json`].
#[derive(Debug)]
pub enum JsonResponseError {
    Router(String),
    Body(String),
    Empty {
        status: axum::http::StatusCode,
    },
    InvalidJson {
        status: axum::http::StatusCode,
        source: serde_json::Error,
    },
}

impl fmt::Display for JsonResponseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Router(error) => write!(formatter, "router failed: {error}"),
            Self::Body(error) => write!(formatter, "response body failed: {error}"),
            Self::Empty { status } => write!(formatter, "{status} response has an empty body"),
            Self::InvalidJson { status, source } => {
                write!(formatter, "{status} response is not valid JSON: {source}")
            }
        }
    }
}

impl std::error::Error for JsonResponseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidJson { source, .. } => Some(source),
            Self::Router(_) | Self::Body(_) | Self::Empty { .. } => None,
        }
    }
}

async fn response_bytes(
    router: axum::Router,
    request: axum::http::Request<axum::body::Body>,
) -> Result<(axum::http::StatusCode, bytes::Bytes), JsonResponseError> {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let response = router
        .oneshot(request)
        .await
        .map_err(|error| JsonResponseError::Router(error.to_string()))?;
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .map_err(|error| JsonResponseError::Body(error.to_string()))?
        .to_bytes();
    Ok((status, bytes))
}

/// A strict JSON round-trip. Empty and malformed bodies are errors, never
/// converted into `serde_json::Value::Null`.
pub async fn call_json(
    router: axum::Router,
    request: axum::http::Request<axum::body::Body>,
) -> Result<(axum::http::StatusCode, serde_json::Value), JsonResponseError> {
    let (status, bytes) = response_bytes(router, request).await?;
    if bytes.is_empty() {
        return Err(JsonResponseError::Empty { status });
    }
    let body = serde_json::from_slice(&bytes)
        .map_err(|source| JsonResponseError::InvalidJson { status, source })?;
    Ok((status, body))
}

/// One HTTP round-trip through the full router via `tower::ServiceExt`,
/// returning the status and the collected JSON body.
pub async fn call(
    router: axum::Router,
    request: axum::http::Request<axum::body::Body>,
) -> (axum::http::StatusCode, serde_json::Value) {
    let (status, bytes) = response_bytes(router, request)
        .await
        .unwrap_or_else(|error| panic!("HTTP response collection failed: {error}"));
    let body = if bytes.is_empty() {
        // Keep the old no-body compatibility behavior for status-only callers.
        serde_json::Value::Null
    } else {
        // Not every response this helper sees is an application-authored JSON
        // envelope: axum's own built-in rejections (a failed `Json<T>`
        // extractor -> 422, `RequestBodyLimitLayer` -> 413, the router's
        // method-not-allowed fallback -> 405) answer with a plain-text body,
        // by framework default, not a bug in the handler being exercised.
        // Every real caller of `call()` that hit one of those paths only
        // asserted on `status` and discarded the body (`let (status, _) =
        // ...`), so a hard panic here was strictly worse than useless — it
        // failed the *status* assertion's test before it ever ran. Callers
        // that need a guaranteed-JSON body should use `call_json`, which
        // still surfaces `JsonResponseError::InvalidJson` instead of
        // silently downgrading it.
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
        })
    };
    (status, body)
}

/// A JSON `POST` request.
pub fn post_json(path: &str, body: &serde_json::Value) -> axum::http::Request<axum::body::Body> {
    axum::http::Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .expect("fixture requests are well-formed")
}

/// A bare `GET` request.
pub fn get(path: &str) -> axum::http::Request<axum::body::Body> {
    axum::http::Request::builder()
        .method("GET")
        .uri(path)
        .body(axum::body::Body::empty())
        .expect("fixture requests are well-formed")
}
