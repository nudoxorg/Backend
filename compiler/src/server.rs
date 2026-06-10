use std::sync::Arc;

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

	let mut registry =
		LocalRegistry::new(storage, config.monitor_interval, config.pipeline.clone());
	if let Some(store) = nudox_store {
		registry = registry.with_nudox_store(store);
	}
	if let Some(idx) = text_index.clone() {
		registry = registry.with_text_index(idx);
	}
	let registry = Arc::new(registry);

	let monitor_registry = Arc::clone(&registry);
	tokio::spawn(async move {
		monitor_registry.run_monitor().await;
	});

	let state = AppState { registry, pipeline: config.pipeline.clone(), sessions, text_index };
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
