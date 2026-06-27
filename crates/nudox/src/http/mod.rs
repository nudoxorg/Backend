pub mod dto;
pub mod error;
pub mod router;

use std::sync::Arc;

use crate::{config::PipelineConfig, ingest::IngestTargets, registry::LocalRegistry, search::SessionStore};

pub use error::{
	AppError, ConfigError, EmbeddingError, GitError, IngestError, PackageError, QdrantError,
	RegistryError, RegistryLookupError, TerminusError, TextIndexError,
};

#[derive(Clone)]
pub struct AppState {
	pub registry: Arc<LocalRegistry>,
	pub pipeline: PipelineConfig,
	pub sessions: SessionStore,
	pub targets:  IngestTargets,
}
