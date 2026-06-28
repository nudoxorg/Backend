pub mod dto;
pub mod error;
pub mod router;

use std::sync::Arc;

use axum::extract::FromRef;
pub use error::{AppError, ConfigError, EmbeddingError, GitError, IngestError, PackageError, QdrantError, RegistryError, RegistryLookupError, TerminusError, TextIndexError};
use qdrant_client::Qdrant;

use crate::{config::PipelineConfig, ingest::{IngestTargets, embedding::OpenAIEmbeddingProvider}, registry::LocalRegistry, search::SessionStore};

#[derive(Clone)]
pub struct AppState {
	pub registry:        Arc<LocalRegistry>,
	pub pipeline:        Arc<PipelineConfig>,
	pub sessions:        SessionStore,
	pub targets:         IngestTargets,
	/// Shared Qdrant client for the legacy `/search`+`/run` path, built once at
	/// startup. `None` when Qdrant is unconfigured. Reuses one gRPC connection
	/// pool instead of dialing per request.
	pub search_qdrant:   Option<Arc<Qdrant>>,
	/// Shared embedding provider for the legacy search path (reuses one
	/// `reqwest::Client` rather than rebuilding it per request).
	pub search_embedder: Arc<OpenAIEmbeddingProvider>,
}

/// Read plane: handlers that only query indexes, sessions, and the shared
/// legacy-search clients. Holds no handle the ingest/registry path writes
/// through, so a sync in progress never contends with a query.
#[derive(Clone)]
pub struct QueryState {
	pub pipeline:        Arc<PipelineConfig>,
	pub sessions:        SessionStore,
	pub targets:         IngestTargets,
	pub search_qdrant:   Option<Arc<Qdrant>>,
	pub search_embedder: Arc<OpenAIEmbeddingProvider>,
}

/// Write plane: handlers that mutate the package registry.
#[derive(Clone)]
pub struct AdminState {
	pub registry: Arc<LocalRegistry>,
}

impl FromRef<AppState> for QueryState {
	fn from_ref(state: &AppState) -> Self {
		Self {
			pipeline:        state.pipeline.clone(),
			sessions:        state.sessions.clone(),
			targets:         state.targets.clone(),
			search_qdrant:   state.search_qdrant.clone(),
			search_embedder: state.search_embedder.clone(),
		}
	}
}

impl FromRef<AppState> for AdminState {
	fn from_ref(state: &AppState) -> Self { Self { registry: state.registry.clone() } }
}
