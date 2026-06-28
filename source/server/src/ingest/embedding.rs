use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use nudox_core::{ByteSpan, EmbeddingPurpose, ModelType, SourceChunk};
use embed::RemoteEmbedder;
use url::Url;
use qdrant_client::{Payload, qdrant::{PointId, PointStruct, Value}};
use tokio::task::JoinSet;
use tracing::info;

use crate::http::error::EmbeddingError;
use crate::ingest::embedding_types::{EmbeddingDocument, RecordKind, RepresentationKind};
pub use crate::ingest::embedding_types::build_entry_embedding_text;

const DEFAULT_EMBED_CONCURRENCY: usize = 16;

#[derive(Debug, Clone, Copy)]
pub struct EmbeddingProgress {
	pub completed: usize,
	pub total:     usize,
}

#[derive(Debug, Clone)]
pub struct VectorPayload {
	pub uri:                 String,
	pub record_key:          String,
	pub fq_name:             Option<String>,
	pub language:            String,
	pub package:             String,
	pub version:             Option<String>,
	pub symbol_kind:         Option<String>,
	pub record_kind:         RecordKind,
	pub representation_kind: RepresentationKind,
	pub embedding_model:     String,
	pub chunk_index:         Option<u32>,
	pub chunk_count:         Option<u32>,
}

impl From<VectorPayload> for Payload {
	fn from(payload: VectorPayload) -> Self {
		let mut fields: HashMap<String, Value> = HashMap::new();

		fields.insert("uri".into(), payload.uri.into());
		fields.insert("record_key".into(), payload.record_key.into());
		fields.insert("language".into(), payload.language.into());
		fields.insert("package".into(), payload.package.into());
		fields.insert("record_kind".into(), payload.record_kind.as_str().into());
		fields.insert("representation_kind".into(), payload.representation_kind.as_str().into());
		fields.insert("embedding_model".into(), payload.embedding_model.into());

		if let Some(fq_name) = payload.fq_name {
			fields.insert("fq_name".into(), fq_name.into());
		}
		if let Some(version) = payload.version {
			fields.insert("version".into(), version.into());
		}
		if let Some(symbol_kind) = payload.symbol_kind {
			fields.insert("symbol_kind".into(), symbol_kind.into());
		}
		if let Some(chunk_index) = payload.chunk_index {
			fields.insert("chunk_index".into(), (chunk_index as i64).into());
		}
		if let Some(chunk_count) = payload.chunk_count {
			fields.insert("chunk_count".into(), (chunk_count as i64).into());
		}

		fields.into()
	}
}

#[derive(Debug, Clone)]
pub struct EmbeddedRecord {
	pub record_key:          String,
	pub uri:                 String,
	pub text:                String,
	pub vector:              Vec<f32>,
	pub embedding_model:     String,
	pub fq_name:             Option<String>,
	pub language:            String,
	pub package:             String,
	pub version:             Option<String>,
	pub symbol_kind:         Option<String>,
	pub record_kind:         RecordKind,
	pub representation_kind: RepresentationKind,
	pub chunk_index:         Option<u32>,
	pub chunk_count:         Option<u32>,
}

impl EmbeddedRecord {
	pub fn into_payload(self) -> VectorPayload {
		VectorPayload {
			uri:                 self.uri,
			record_key:          self.record_key,
			fq_name:             self.fq_name,
			language:            self.language,
			package:             self.package,
			version:             self.version,
			symbol_kind:         self.symbol_kind,
			record_kind:         self.record_kind,
			representation_kind: self.representation_kind,
			embedding_model:     self.embedding_model,
			chunk_index:         self.chunk_index,
			chunk_count:         self.chunk_count,
		}
	}
}

#[async_trait]
pub trait EmbeddingProvider {
	fn model_name(&self) -> &str;
	async fn embed_text(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;
}

#[derive(Clone)]
pub struct EmbeddingService<P> {
	provider: P,
}

impl<P> EmbeddingService<P>
where
	P: EmbeddingProvider,
{
	pub fn new(provider: P) -> Self { Self { provider } }

	pub async fn embed_document(
		&self,
		doc: EmbeddingDocument,
	) -> Result<EmbeddedRecord, EmbeddingError> {
		doc.validate()?;

		let vector = self.provider.embed_text(&doc.text).await?;
		if vector.is_empty() {
			return Err(EmbeddingError::MissingVector);
		}

		Ok(EmbeddedRecord {
			record_key:          doc.record_key,
			uri:                 doc.uri,
			text:                doc.text,
			vector,
			embedding_model:     self.provider.model_name().to_string(),
			fq_name:             doc.fq_name,
			language:            doc.language,
			package:             doc.package,
			version:             doc.version,
			symbol_kind:         doc.symbol_kind,
			record_kind:         doc.record_kind,
			representation_kind: doc.representation_kind,
			chunk_index:         doc.chunk_index,
			chunk_count:         doc.chunk_count,
		})
	}

	pub async fn embed_documents(
		&self,
		docs: impl IntoIterator<Item = EmbeddingDocument>,
		mut on_progress: impl FnMut(EmbeddingProgress),
	) -> Result<Vec<EmbeddedRecord>, EmbeddingError>
	where
		P: Clone + Send + Sync + 'static,
	{
		let docs: Vec<EmbeddingDocument> = docs.into_iter().collect();
		if docs.is_empty() {
			return Ok(Vec::new());
		}

		let total = docs.len();
		let concurrency = DEFAULT_EMBED_CONCURRENCY.min(total);
		info!(count = total, concurrency, "embedding documents");
		on_progress(EmbeddingProgress { completed: 0, total });

		let mut join_set = JoinSet::new();
		let mut completed = 0usize;
		let mut out: Vec<Option<EmbeddedRecord>> = vec![None; total];

		for (index, doc) in docs.into_iter().enumerate() {
			let service = self.clone();
			join_set
				.spawn(async move { service.embed_document(doc).await.map(|record| (index, record)) });

			if join_set.len() >= concurrency {
				collect_embedded_record(&mut join_set, &mut out, &mut completed, total, &mut on_progress)
					.await?;
			}
		}

		while !join_set.is_empty() {
			collect_embedded_record(&mut join_set, &mut out, &mut completed, total, &mut on_progress)
				.await?;
		}

		out
			.into_iter()
			.map(|record| record.ok_or(EmbeddingError::EmptyResponse))
			.collect()
	}
}

const DEFAULT_EMBEDDING_ENDPOINT: &str = "https://api.openai.com/v1/embeddings";

#[derive(Clone)]
pub struct OpenAIEmbeddingProvider {
	embedder:   Arc<RemoteEmbedder>,
	model_name: String,
}

impl OpenAIEmbeddingProvider {
	pub fn new(model_name: impl Into<String>) -> Self {
		let api_key = std::env::var("OPENAI_API_KEY").ok();
		Self::build(api_key, model_name)
	}

	pub fn new_with_api_key(api_key: impl Into<String>, model_name: impl Into<String>) -> Self {
		Self::build(Some(api_key.into()), model_name)
	}

	fn build(api_key: Option<String>, model_name: impl Into<String>) -> Self {
		let model_name = model_name.into();
		let endpoint = std::env::var("NUDOX_EMBEDDING_ENDPOINT")
			.ok()
			.and_then(|value| Url::parse(&value).ok())
			.unwrap_or_else(|| Url::parse(DEFAULT_EMBEDDING_ENDPOINT).expect("valid default endpoint"));

		let mut builder =
			RemoteEmbedder::builder(endpoint, model_name.clone()).model_type(ModelType::Openai);
		if let Some(key) = api_key {
			builder = builder.api_key(key);
		}

		Self { embedder: Arc::new(builder.build()), model_name }
	}
}

#[async_trait]
impl EmbeddingProvider for OpenAIEmbeddingProvider {
	fn model_name(&self) -> &str { &self.model_name }

	async fn embed_text(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
		if text.trim().is_empty() {
			return Err(EmbeddingError::MissingText);
		}

		let chunk = SourceChunk {
			raw_code:        text.into(),
			treesitter_repr: None,
			symbol_span:     ByteSpan::covering(0, text.len()),
		};

		let embedding = nudox_core::Embedder::embed(&*self.embedder, &chunk, EmbeddingPurpose::Code)
			.await
			.map_err(|source| EmbeddingError::Provider { source })?;

		Ok(embedding.into_vec())
	}
}

pub struct QdrantPointFactory;

impl QdrantPointFactory {
	pub fn build_point(
		point_id: PointId,
		record: EmbeddedRecord,
	) -> Result<PointStruct, EmbeddingError> {
		if record.vector.is_empty() {
			return Err(EmbeddingError::MissingVector);
		}

		let payload: Payload = record.clone().into_payload().into();

		Ok(PointStruct::new(point_id, record.vector, payload))
	}
}

pub struct PointIdFactory;

impl PointIdFactory {
	pub fn from_u64(id: u64) -> PointId { PointId::from(id) }

	pub fn deterministic(record_key: &str) -> PointId {
		let uuid = uuid::Uuid::new_v5(&store::NUDOX_SYMBOL_NS, record_key.as_bytes());
		PointId::from(uuid.to_string())
	}
}

async fn collect_embedded_record(
	join_set: &mut JoinSet<Result<(usize, EmbeddedRecord), EmbeddingError>>,
	out: &mut [Option<EmbeddedRecord>],
	completed: &mut usize,
	total: usize,
	on_progress: &mut impl FnMut(EmbeddingProgress),
) -> Result<(), EmbeddingError> {
	let Some(result) = join_set.join_next().await else {
		return Ok(());
	};
	let (index, record) = result
		.map_err(|source| EmbeddingError::TaskJoin { source })??;
	out[index] = Some(record);
	*completed += 1;
	if *completed == total || total <= 50 || (*completed).is_multiple_of(10) {
		on_progress(EmbeddingProgress { completed: *completed, total });
		info!(completed = *completed, total, "embedding progress");
	}
	Ok(())
}
