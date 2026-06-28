pub mod dto;
pub mod error;
pub mod router;

use std::sync::Arc;

use qdrant_client::Qdrant;

use crate::{config::PipelineConfig, ingest::{IngestTargets, embedding::OpenAIEmbeddingProvider}, registry::LocalRegistry, search::SessionStore};

pub use error::{
	AppError, ConfigError, EmbeddingError, GitError, IngestError, PackageError, QdrantError,
	RegistryError, RegistryLookupError, TerminusError, TextIndexError,
};

#[derive(Clone)]
pub struct AppState {
	pub registry: Arc<LocalRegistry>,
	pub pipeline: Arc<PipelineConfig>,
	pub sessions: SessionStore,
	pub targets:  IngestTargets,
	/// Shared Qdrant client for the legacy `/search`+`/run` path, built once at
	/// startup. `None` when Qdrant is unconfigured. Reuses one gRPC connection
	/// pool instead of dialing per request.
	pub search_qdrant:   Option<Arc<Qdrant>>,
	/// Shared embedding provider for the legacy search path (reuses one
	/// `reqwest::Client` rather than rebuilding it per request).
	pub search_embedder: Arc<OpenAIEmbeddingProvider>,
}
