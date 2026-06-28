use std::sync::Arc;

use blobstore::ObjectStoreBlobStore;
use nudox_core::{BlobStore, Embedder, FutureParseQueue, GlobalSymbolStore, ModelType, SearchIndex, SearchQuery, VectorIndex, VectorQuery};
use embed::{PlaceholderEmbedder, RemoteEmbedder};
use orchestrator::{Orchestrator, memory::{InMemoryFutureParseQueue, InMemoryGlobalSymbolStore}};
use search::{InMemoryVectorIndex, QdrantVectorIndex, SymbolSearcher, TantivySearchIndex};
use qdrant_client::Qdrant;
use store::NudoxStore;
use tokio::signal;
use tracing::{info, warn};
use url::Url;

use crate::{config::AppConfig, http::{AppState, error::AppError, router::router}, ingest::{IngestTargets, embedding::OpenAIEmbeddingProvider}, registry::LocalRegistry, search::{SessionStore, text::SymbolTextIndex}, storage::StorageLayout};

pub async fn run(config: AppConfig) -> Result<(), AppError> {
	let storage = StorageLayout::new(config.storage_root.clone());
	storage.ensure()?;
	let sessions = SessionStore::new(storage.sessions_dir())?;

	// Open the SQLite occurrence store when a TerminusDB instance is configured.
	// The terminus_instance key ("org/db") is used to derive stable
	// GlobalSymbolIds.
	let store = if let Some(ref terminus) = config.pipeline.terminus {
		let db_path = config.storage_root.join("nudox-links.db");
		let instance = format!("{}/{}", terminus.org, terminus.db);
		match store::NudoxStore::open(&db_path, instance).await {
			Ok(store) => {
				info!(db = %db_path.display(), "nudox-store SQLite opened");
				Some(store)
			}
			Err(e) => {
				warn!(error = %e, "failed to open nudox-store SQLite; occurrence linking disabled");
				None
			}
		}
	} else {
		None
	};

	let text_index_dir = config.storage_root.join("tantivy");
	let text_index = match SymbolTextIndex::open_or_create(&text_index_dir) {
		Ok(idx) => {
			info!(dir = %text_index_dir.display(), "text search index opened");
			Some(Arc::new(idx))
		}
		Err(e) => {
			warn!(error = %e, "failed to open text search index; /text-search will be unavailable");
			None
		}
	};

	// Build the symbol-search stack (now async: wires QdrantVectorIndex and
	// NudoxStore when configured). Clone the store handle so it can also flow
	// into IngestTargets below.
	let symbol_orchestrator =
		build_symbol_orchestrator(&config, store.clone()).await;

	// All configured fan-out destinations, assembled once. The registry writes to
	// them during ingestion; the API layer reads `text_index`/`orchestrator` from
	// the same handles. No per-backend threading or builder triplet.
	let targets = IngestTargets {
		store,
		text_index:   text_index.clone(),
		orchestrator: symbol_orchestrator.clone(),
	};

	let registry = Arc::new(
		LocalRegistry::new(storage, config.monitor_interval, config.pipeline.clone())
			.with_targets(targets.clone()),
	);

	let monitor_registry = Arc::clone(&registry);
	tokio::spawn(async move {
		monitor_registry.run_monitor().await;
	});

	// Shared legacy-search handles, built once at startup instead of per `/search`
	// request: one Qdrant gRPC client (reused connection pool) and one embedding
	// provider (reused reqwest::Client). Both feed the `/search` and `/run` paths.
	let search_embedder =
		Arc::new(OpenAIEmbeddingProvider::new(config.pipeline.embedding_model.clone()));
	let search_qdrant = match config.pipeline.qdrant {
		Some(ref qdrant) => match Qdrant::from_url(qdrant.endpoint.as_str()).build() {
			Ok(client) => Some(Arc::new(client)),
			Err(error) => {
				warn!(error = %error, "failed to build shared qdrant client; /search will be unavailable");
				None
			}
		},
		None => None,
	};

	let state = AppState {
		registry,
		pipeline: Arc::new(config.pipeline.clone()),
		sessions,
		targets,
		search_qdrant,
		search_embedder,
	};
	let app = router(state);
	let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;

	info!(
		address = %config.bind_addr,
		storage_root = %config.storage_root.display(),
		"nudox server listening"
	);

	axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;

	Ok(())
}

async fn shutdown_signal() { let _ = signal::ctrl_c().await; }

/// Build the nudox symbol-search stack.
///
/// Persistent backends (always on):
///   - `storage_root/nudox-symbol-index`  — Tantivy full-text index
///   - `storage_root/nudox-blobs`          — ObjectStore blob store (local FS)
///
/// Conditional backends (wired when the relevant config is present):
///   - `NUDOX_QDRANT_ENDPOINT`            → `QdrantVectorIndex` (otherwise in-memory)
///   - `store` arg                  → SQLite `GlobalSymbolStore` + `FutureParseQueue`
///   - `OPENAI_API_KEY` / `NUDOX_EMBEDDING_ENDPOINT` → `RemoteEmbedder` (otherwise placeholder)
async fn build_symbol_orchestrator(
	config: &AppConfig,
	store: Option<Arc<NudoxStore>>,
) -> Option<Arc<Orchestrator>> {
	let index_dir = config.storage_root.join("nudox-symbol-index");
	let blobs_dir = config.storage_root.join("nudox-blobs");

	let text = match TantivySearchIndex::open_or_create(&index_dir) {
		Ok(idx) => {
			info!(dir = %index_dir.display(), "nudox symbol text index opened");
			Arc::new(idx)
		}
		Err(e) => {
			warn!(error = %e, "nudox symbol text index unavailable; /symbol-search disabled");
			return None;
		}
	};
	let blobs = match ObjectStoreBlobStore::local(blobs_dir.clone()) {
		Ok(store) => {
			info!(dir = %blobs_dir.display(), "nudox blob store opened");
			Arc::new(store)
		}
		Err(e) => {
			warn!(error = %e, "nudox blob store unavailable; /symbol-search disabled");
			return None;
		}
	};

	// Vector index: use QdrantVectorIndex when endpoint is configured. Both the
	// orchestrator (VectorIndex) and the searcher (VectorQuery) need a handle,
	// so we keep the concrete Arc until we coerce to the two trait objects.
	let (vector_index, vector_query): (Arc<dyn VectorIndex>, Arc<dyn VectorQuery>) =
		if let Some(ref qdrant) = config.pipeline.qdrant {
			let collection = format!("{}_blobs", qdrant.collection_prefix);
			match QdrantVectorIndex::connect(qdrant.endpoint.as_str(), collection.clone()).await {
				Ok(idx) => match idx.ensure_collection(qdrant.vector_size).await {
					Ok(_) => {
						info!(collection, "qdrant vector index wired for symbol-search");
						let shared = Arc::new(idx);
						(Arc::clone(&shared) as _, shared as _)
					}
					Err(e) => {
						warn!(error = %e, "qdrant collection init failed; falling back to in-memory");
						let shared = Arc::new(InMemoryVectorIndex::new());
						(Arc::clone(&shared) as _, shared as _)
					}
				},
				Err(e) => {
					warn!(error = %e, "qdrant connect failed; falling back to in-memory");
					let shared = Arc::new(InMemoryVectorIndex::new());
					(Arc::clone(&shared) as _, shared as _)
				}
			}
		} else {
			let shared = Arc::new(InMemoryVectorIndex::new());
			(Arc::clone(&shared) as _, shared as _)
		};

	// GlobalSymbolStore + FutureParseQueue: reuse the SQLite store when available.
	let (global, queue): (Arc<dyn GlobalSymbolStore>, Arc<dyn FutureParseQueue>) =
		if let Some(ref store) = store {
			info!("wiring NudoxStore as global symbol store and future-parse queue");
			(Arc::clone(store) as _, Arc::clone(store) as _)
		} else {
			(Arc::new(InMemoryGlobalSymbolStore::new()), Arc::new(InMemoryFutureParseQueue::new()))
		};

	// Embedder: use RemoteEmbedder when an API key or custom endpoint is set.
	let vector_dim = config.pipeline.qdrant.as_ref().map_or(128, |q| q.vector_size as usize);
	let embedder: Arc<dyn Embedder> = {
		let api_key = std::env::var("OPENAI_API_KEY").ok();
		let endpoint = std::env::var("NUDOX_EMBEDDING_ENDPOINT")
			.ok()
			.and_then(|v| Url::parse(&v).ok());
		if api_key.is_some() || endpoint.is_some() {
			let ep = endpoint.unwrap_or_else(|| {
				Url::parse("https://api.openai.com/v1/embeddings").expect("valid default URL")
			});
			let mut builder = RemoteEmbedder::builder(ep, config.pipeline.embedding_model.clone())
				.model_type(ModelType::Openai);
			if let Some(key) = api_key {
				builder = builder.api_key(key);
			}
			info!(model = %config.pipeline.embedding_model, "using RemoteEmbedder for symbol-search");
			Arc::new(builder.build())
		} else {
			Arc::new(PlaceholderEmbedder::new("placeholder", vector_dim))
		}
	};

	let searcher = Arc::new(SymbolSearcher::new(
		Arc::clone(&text) as Arc<dyn SearchQuery>,
		vector_query,
		Arc::clone(&blobs) as Arc<dyn BlobStore>,
		embedder,
		None,
	));

	let orchestrator = Orchestrator::new(
		global,
		blobs,
		queue,
		text as Arc<dyn SearchIndex>,
		vector_index,
	)
	.with_symbol_search(searcher);

	Some(Arc::new(orchestrator))
}
