/// Integration tests for the `POST /symbol-search` endpoint.
///
/// These tests wire an in-memory `Orchestrator` directly into `AppState` —
/// no real package ingestion, no external services — so they run fast and
/// deterministically.
use std::sync::Arc;

use axum::{body::Body, http::{Request, StatusCode}};
use http_body_util::BodyExt;
use nudox::{api::AppState, config::PipelineConfig, ingest::IngestTargets, local_registry::LocalRegistry, search::SessionStore, storage::StorageLayout};
use nudox_blobstore::InMemoryBlobStore;
use nudox_core::{BLOB_SCHEMA_VERSION, BlobInfo, BlobStore, ByteSpan, ChunkMetadata, Language, OccurrenceId, RepoId, SearchIndex, SearchQuery, SourceChunk, SymbolKind, SymbolOrigin, TreesitterRepr, VectorIndex, VectorQuery};
use nudox_embed::PlaceholderEmbedder;
use nudox_orchestrator::{Orchestrator, memory::{InMemoryFutureParseQueue, InMemoryGlobalSymbolStore}};
use nudox_search::{InMemoryVectorIndex, SymbolSearcher, TantivySearchIndex};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

// ── helpers
// ───────────────────────────────────────────────────────────────────

fn make_blob(symbol_name: &str, lib: &str, kind: SymbolKind, raw_code: &str) -> BlobInfo {
	// Use Repo origin so the orchestrator indexes the blob immediately.
	// ExternalLib blobs are deferred until the library's global IDs are registered.
	let repo_id = RepoId(format!("lib:{lib}"));
	BlobInfo {
		occurrence_id:      OccurrenceId(uuid::Uuid::new_v4()),
		symbol_name:        symbol_name.to_owned(),
		symbol_origin:      SymbolOrigin::Repo { repo_id: repo_id.clone() },
		resolved_global_id: None,
		kind:               Some(kind),
		source:             SourceChunk {
			raw_code:        raw_code.to_owned(),
            treesitter_repr: Some(TreesitterRepr(vec![])),
			symbol_span:     ByteSpan { start: 0, end: raw_code.len() },
		},
		embeddings:         vec![],
		metadata:           ChunkMetadata {
			repo_id,
			file_path: "src/lib.rs".into(),
			file_span: ByteSpan { start: 0, end: 0 },
			parsed_at: chrono::Utc::now(),
			lang: Language::Rust,
			lang_version: None,
			blob_schema_version: BLOB_SCHEMA_VERSION,
		},
	}
}

/// Build a fully wired `AppState` with an in-memory symbol-search stack.
/// The returned `TempDir` must stay alive for the Tantivy index to work.
async fn build_state_with_orchestrator() -> (AppState, TempDir) {
	let tmp = TempDir::new().unwrap();
	let storage = StorageLayout::new(tmp.path().join("storage"));
	storage.ensure().unwrap();

	let pipeline = PipelineConfig {
		terminus:        None,
		qdrant:          None,
		embedding_model: "text-embedding-3-small".to_owned(),
		upload_schema:   false,
	};

	let registry =
		Arc::new(LocalRegistry::new(storage, std::time::Duration::from_secs(3600), pipeline.clone()));

	// Symbol-search stack.
	let index_dir = tmp.path().join("nudox-symbol-index");
	let text = Arc::new(TantivySearchIndex::open_or_create(&index_dir).unwrap());
	let vector = Arc::new(InMemoryVectorIndex::new());
	let blobs = Arc::new(InMemoryBlobStore::new());
	let global = Arc::new(InMemoryGlobalSymbolStore::new());
	let queue = Arc::new(InMemoryFutureParseQueue::new());
	let embedder = Arc::new(PlaceholderEmbedder::new("placeholder", 128));

	let searcher = Arc::new(SymbolSearcher::new(
		Arc::clone(&text) as Arc<dyn SearchQuery>,
		Arc::clone(&vector) as Arc<dyn VectorQuery>,
		Arc::clone(&blobs) as Arc<dyn BlobStore>,
		embedder,
		None,
	));

	let orchestrator = Arc::new(
		Orchestrator::new(
			global,
			blobs,
			queue,
			text as Arc<dyn SearchIndex>,
			vector as Arc<dyn VectorIndex>,
		)
		.with_symbol_search(searcher),
	);

	let state = AppState {
		registry,
		pipeline,
		sessions: SessionStore::default(),
		targets: IngestTargets { orchestrator: Some(orchestrator), ..Default::default() },
	};
	(state, tmp)
}

/// Feed a `BlobInfo` directly into the orchestrator inside `AppState`.
async fn ingest_blob(state: &AppState, blob: BlobInfo) {
	let orch = state.targets.orchestrator.as_ref().unwrap();
	orch.ingest(blob).await.expect("orchestrator ingest failed");
}

/// POST /symbol-search and return (status, parsed JSON body).
async fn post_symbol_search(state: AppState, body: Value) -> (StatusCode, Value) {
	let app = nudox::api::router(state);
	let req = Request::builder()
		.method("POST")
		.uri("/symbol-search")
		.header("content-type", "application/json")
		.body(Body::from(serde_json::to_vec(&body).unwrap()))
		.unwrap();
	let resp = app.oneshot(req).await.unwrap();
	let status = resp.status();
	let bytes = resp.into_body().collect().await.unwrap().to_bytes();
	let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
	(status, json)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn empty_index_returns_empty_array() {
	let (state, _tmp) = build_state_with_orchestrator().await;

	let (status, body) = post_symbol_search(state, json!({"name_pattern": "Router"})).await;

	assert_eq!(status, StatusCode::OK);
	assert_eq!(body, json!([]));
}

#[tokio::test]
async fn finds_ingested_symbol_by_name() {
	let (state, _tmp) = build_state_with_orchestrator().await;

	let blob = make_blob("axum::Router", "axum", SymbolKind::Struct, "pub struct Router {}");
	ingest_blob(&state, blob).await;

	let (status, body) =
		post_symbol_search(state, json!({"name_pattern": "Router", "limit": 10})).await;

	assert_eq!(status, StatusCode::OK);
	let hits = body.as_array().unwrap();
	assert_eq!(hits.len(), 1);
	assert_eq!(hits[0]["symbol_name"], "axum::Router");
	assert_eq!(hits[0]["kind"], "Struct");
	assert_eq!(hits[0]["repo_id"], "lib:axum");
	assert!(hits[0]["lib_name"].is_null());
}

#[tokio::test]
async fn finds_multiple_symbols_by_partial_name() {
	let (state, _tmp) = build_state_with_orchestrator().await;

	ingest_blob(
		&state,
		make_blob("tokio::spawn", "tokio", SymbolKind::Function, "pub fn spawn<F>(f: F) {}"),
	)
	.await;
	ingest_blob(
		&state,
		make_blob(
			"tokio::spawn_blocking",
			"tokio",
			SymbolKind::Function,
			"pub fn spawn_blocking<F, R>(f: F) {}",
		),
	)
	.await;
	ingest_blob(
		&state,
		make_blob("serde::Serialize", "serde", SymbolKind::Trait, "pub trait Serialize {}"),
	)
	.await;

	let (status, body) =
		post_symbol_search(state, json!({"name_pattern": "spawn", "limit": 10})).await;

	assert_eq!(status, StatusCode::OK);
	let hits = body.as_array().unwrap();
	assert_eq!(hits.len(), 2, "expected 2 spawn* symbols, got {hits:?}");
	let names: Vec<&str> = hits.iter().map(|h| h["symbol_name"].as_str().unwrap()).collect();
	assert!(names.contains(&"tokio::spawn"));
	assert!(names.contains(&"tokio::spawn_blocking"));
}

#[tokio::test]
async fn body_query_returns_501_not_implemented() {
	let (state, _tmp) = build_state_with_orchestrator().await;

	let (status, body) = post_symbol_search(
		state,
		json!({
				"name_pattern": "Router",
				"body_query":   { "NaturalLanguage": "HTTP routing handler" }
		}),
	)
	.await;

	assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
	let error_msg = body["error"].as_str().unwrap_or_default();
	assert!(
		error_msg.contains("body_query"),
		"error message should mention body_query, got: {error_msg}"
	);
}

#[tokio::test]
async fn missing_orchestrator_returns_500() {
	let tmp = TempDir::new().unwrap();
	let storage = StorageLayout::new(tmp.path().join("storage"));
	storage.ensure().unwrap();
	let pipeline = PipelineConfig {
		terminus:        None,
		qdrant:          None,
		embedding_model: "text-embedding-3-small".to_owned(),
		upload_schema:   false,
	};
	let registry =
		Arc::new(LocalRegistry::new(storage, std::time::Duration::from_secs(3600), pipeline.clone()));

	let state = AppState {
		registry,
		pipeline,
		sessions: SessionStore::default(),
		targets: IngestTargets::default(), // not configured
	};

	let (status, _) = post_symbol_search(state, json!({"name_pattern": "x"})).await;
	assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn limit_is_respected() {
	let (state, _tmp) = build_state_with_orchestrator().await;

	for i in 0..10 {
		ingest_blob(
			&state,
			make_blob(
				&format!("mylib::func_{i}"),
				"mylib",
				SymbolKind::Function,
				&format!("pub fn func_{i}() {{}}"),
			),
		)
		.await;
	}

	let (status, body) = post_symbol_search(state, json!({"name_pattern": "func", "limit": 3})).await;

	assert_eq!(status, StatusCode::OK);
	assert_eq!(body.as_array().unwrap().len(), 3, "limit should cap results at 3");
}

#[tokio::test]
async fn response_includes_snippet_field() {
	let (state, _tmp) = build_state_with_orchestrator().await;

	let code = "pub fn hello() { println!(\"hi\"); }";
	ingest_blob(&state, make_blob("mylib::hello", "mylib", SymbolKind::Function, code)).await;

	let (status, body) =
		post_symbol_search(state, json!({"name_pattern": "hello", "limit": 1})).await;

	assert_eq!(status, StatusCode::OK);
	let hit = &body.as_array().unwrap()[0];
	assert_eq!(hit["snippet"].as_str().unwrap(), code);
}

#[tokio::test]
async fn kind_only_returns_matching_kinds() {
	let (state, _tmp) = build_state_with_orchestrator().await;

	ingest_blob(
		&state,
		make_blob("mylib::Router", "mylib", SymbolKind::Struct, "pub struct Router {}"),
	)
	.await;
	ingest_blob(
		&state,
		make_blob("mylib::handle", "mylib", SymbolKind::Function, "pub fn handle() {}"),
	)
	.await;
	ingest_blob(&state, make_blob("mylib::Error", "mylib", SymbolKind::Enum, "pub enum Error {}"))
		.await;

	let (status, body) = post_symbol_search(state, json!({"kind": "Struct", "limit": 10})).await;

	assert_eq!(status, StatusCode::OK);
	let hits = body.as_array().unwrap();
	assert_eq!(hits.len(), 1, "only the Struct should match");
	assert_eq!(hits[0]["symbol_name"], "mylib::Router");
	assert_eq!(hits[0]["kind"], "Struct");
}
