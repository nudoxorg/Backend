use std::{net::SocketAddr, path::PathBuf, time::Duration};

use nudox_core::ModelId;
use url::Url;

use crate::terminus::upload::TerminusConfig;

#[derive(Clone, Copy, Debug)]
pub enum VectorDistance {
	Cosine,
	Dot,
	Euclid,
	Manhattan,
}

#[derive(Clone, Debug)]
pub struct AppConfig {
	pub bind_addr:        SocketAddr,
	pub storage_root:     PathBuf,
	pub monitor_interval: Duration,
	pub tracing_filter:   String,
	pub pipeline:         PipelineConfig,
}

#[derive(Clone, Debug)]
pub struct PipelineConfig {
	pub terminus:        Option<TerminusConfig>,
	pub qdrant:          Option<QdrantSettings>,
	pub embedding_model: ModelId,
	pub upload_schema:   bool,
}

#[derive(Clone, Debug)]
pub struct QdrantSettings {
	pub endpoint:          Url,
	pub collection_prefix: String,
	pub vector_size:       u64,
	pub distance:          VectorDistance,
}
