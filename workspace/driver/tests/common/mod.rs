//! Shared fixtures for the server integration tests: symbol/package
//! constructors, a populated replica-local text index, and the env-gated
//! assembly of a full `Server` over live localhost backends.
#![allow(dead_code, reason = "each integration test binary uses its own subset")]

#[allow(unused_imports)]
use driver::{registry};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use heart::{EntryUri, Language, Name, PackageId, Symbol, SymbolId, SymbolKind};
use registry::runtime::text::TextIndex;
use driver::authz::{Principal, ReadCap, WriteCap};
use heart::PageSpecification;
use driver::search::query::{Filter, Query, Search};
use driver::{Server, ServerConfiguration};
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
        name: Name { plain: SmolStr::new(plain), fully_qualified: SmolStr::new(fully_qualified) },
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

/// A replica-local text index over `dir`, populated and committed.
pub fn populated_text_index(dir: &Path, symbols: &[Symbol]) -> TextIndex {
    let index = TextIndex::open_or_create(dir).expect("a fresh temp dir opens as an index");
    index.upsert_batch(symbols).expect("fixture symbols index cleanly");
    index.commit().expect("the fixture commit succeeds");
    index
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
    use driver::search::query::LiteralQuery;
    Search {
        query: Query::Literal(LiteralQuery::parse(query).expect("fixture queries are valid")),
        filter: Filter::default(),
        page: page(limit),
        _lifetime: std::marker::PhantomData,
    }
}

/// A page of `limit` results from the start.
pub fn page(limit: u32) -> PageSpecification {
    PageSpecification { limit, cursor: None }
}

/// A unique temporary directory removed on drop (for tantivy replicas and
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
    let data_directory = TempDir::new(test);
    let mut configuration = ServerConfiguration::resolve().expect("test configuration resolves");
    configuration.definitive.data_directory = Some(data_directory.path().to_path_buf());
    configuration.serving_address = free_loopback_address();
    match Server::assemble(configuration).await {
        Ok(server) => Some((Arc::new(server), data_directory)),
        Err(error) => {
            eprintln!("skipping {test}: backends opted in but unreachable: {error}");
            None
        }
    }
}

/// A fresh loopback address the OS just proved free (released before use, so a
/// parallel test could steal it — acceptable in an opt-in suite).
pub fn free_loopback_address() -> std::net::SocketAddr {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("loopback binds")
        .local_addr()
        .expect("a bound listener has an address")
}

/// Run the full server (HTTP + background pollers) for the rest of the test
/// process. The task is detached; the test binary's exit reaps it.
pub fn spawn_serving(server: Arc<Server<TestModel>>) {
    tokio::spawn(async move {
        if let Err(error) = server.serve().await {
            eprintln!("background serve exited: {error}");
        }
    });
}

/// One HTTP round-trip through the full router via `tower::ServiceExt`,
/// returning the status and the collected JSON body.
pub async fn call(
    router: axum::Router,
    request: axum::http::Request<axum::body::Body>,
) -> (axum::http::StatusCode, serde_json::Value) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let response = router.oneshot(request).await.expect("the router is infallible");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body collects").to_bytes();
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
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
