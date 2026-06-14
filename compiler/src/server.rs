use std::sync::Arc;

use nudox_blobstore::ObjectStoreBlobStore;
use nudox_core::{BlobStore, SearchIndex, SearchQuery, VectorIndex, VectorQuery};
use nudox_embed::PlaceholderEmbedder;
use nudox_orchestrator::{Orchestrator, memory::{InMemoryFutureParseQueue, InMemoryGlobalSymbolStore}};
use nudox_search::{InMemoryVectorIndex, SymbolSearcher, TantivySearchIndex};
use tokio::signal;
use tracing::{info, warn};

use crate::{api::{self, AppState}, config::AppConfig, error::AppError, local_registry::LocalRegistry, search::SessionStore, storage::StorageLayout, text_index::SymbolTextIndex};

pub async fn run(config: AppConfig) -> Result<(), AppError> {
	let storage = StorageLayout::new(config.storage_root.clone());
	storage.ensure()?;
	let sessions = SessionStore::new(storage.sessions_dir())?;

	// Open the SQLite occurrence store when a TerminusDB instance is configured.
	// The terminus_instance key ("org/db") is used to derive stable GlobalSymbolIds.
	let nudox_store = if let Some(ref terminus) = config.pipeline.terminus {
		let db_path = config.storage_root.join("nudox-links.db");
		let instance = format!("{}/{}", terminus.org, terminus.db);
		match nudox_store::NudoxStore::open(&db_path, instance).await {
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

	// Build the symbol-search stack.
	// TantivySearchIndex   → storage_root/nudox-symbol-index  (persisted)
	// ObjectStoreBlobStore → storage_root/nudox-blobs          (persisted, local FS for now)
	// Vector index         → in-memory (pending embedding decision)
	let symbol_orchestrator = build_symbol_orchestrator(&config.storage_root);

	let mut registry =
		LocalRegistry::new(storage, config.monitor_interval, config.pipeline.clone());
	if let Some(store) = nudox_store {
		registry = registry.with_nudox_store(store);
	}
	if let Some(idx) = text_index.clone() {
		registry = registry.with_text_index(idx);
	}
	if let Some(ref orch) = symbol_orchestrator {
		registry = registry.with_orchestrator(Arc::clone(orch));
	}
	let registry = Arc::new(registry);

	let monitor_registry = Arc::clone(&registry);
	tokio::spawn(async move {
		monitor_registry.run_monitor().await;
	});

	let state = AppState {
		registry,
		pipeline: config.pipeline.clone(),
		sessions,
		text_index,
		symbol_orchestrator,
	};
	let app = api::router(state);
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
/// Persistent backends:
///   - `storage_root/nudox-symbol-index`  — Tantivy full-text index
///   - `storage_root/nudox-blobs`          — ObjectStore blob store (local FS)
///
/// Still in-memory (pending embedding decision):
///   - vector index (InMemoryVectorIndex)
///   - global symbol store + future-parse queue
///
/// body_query search returns 501 at the HTTP layer until embeddings are wired.
fn build_symbol_orchestrator(storage_root: &std::path::Path) -> Option<Arc<Orchestrator>> {
	let index_dir = storage_root.join("nudox-symbol-index");
	let blobs_dir = storage_root.join("nudox-blobs");

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
	let vector   = Arc::new(InMemoryVectorIndex::new());
	let global   = Arc::new(InMemoryGlobalSymbolStore::new());
	let queue    = Arc::new(InMemoryFutureParseQueue::new());
	let embedder = Arc::new(PlaceholderEmbedder::new("placeholder", 128));

	let searcher = Arc::new(SymbolSearcher::new(
		Arc::clone(&text)   as Arc<dyn SearchQuery>,
		Arc::clone(&vector) as Arc<dyn VectorQuery>,
		Arc::clone(&blobs)  as Arc<dyn BlobStore>,
		embedder,
		None,
	));

	let orchestrator = Orchestrator::new(
		global,
		blobs,
		queue,
		text   as Arc<dyn SearchIndex>,
		vector as Arc<dyn VectorIndex>,
	)
	.with_symbol_search(searcher);

	Some(Arc::new(orchestrator))
}
