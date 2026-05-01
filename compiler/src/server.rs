use std::sync::Arc;

use tokio::signal;
use tracing::info;

use crate::{api::{self, AppState}, config::AppConfig, error::AppError, local_registry::LocalRegistry, search::SessionStore, storage::StorageLayout};

pub async fn run(config: AppConfig) -> Result<(), AppError> {
	let storage = StorageLayout::new(config.storage_root.clone());
	storage.ensure()?;
	let sessions = SessionStore::new(storage.sessions_dir())?;

	let registry =
		Arc::new(LocalRegistry::new(storage, config.monitor_interval, config.pipeline.clone()));

	let monitor_registry = Arc::clone(&registry);
	tokio::spawn(async move {
		monitor_registry.run_monitor().await;
	});

	let state = AppState { registry, pipeline: config.pipeline.clone(), sessions };
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
