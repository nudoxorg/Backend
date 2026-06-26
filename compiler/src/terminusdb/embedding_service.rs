//! embedding_service.rs

//! Embedding ingestion types and helpers for vector index preparation.
//!
//! This module is part of the backend ingestion pipeline. It is responsible for
//! taking JSON-LD documents emitted into the `DocStore`, extracting the subset
//! of data needed for semantic indexing, generating vector-ready records, and
//! converting those records into Qdrant upload types. It does not implement
//! retrieval, ranking, graph traversal, or search-time expansion logic.
//!
//! ## Scope
//!
//! The code in this module is intentionally limited to ingestion-time concerns:
//!
//! - project JSON-LD `Entry` documents from the `DocStore` into
//!   `EmbeddingDocument`s
//! - validate required fields needed for indexing
//! - call an embedding provider to produce dense vectors
//! - preserve canonical graph linkage through the TerminusDB URI
//! - convert embedded records into Qdrant payloads and points
//!
//! The `DocStore` remains the canonical in-memory staging area for emitted
//! JSON-LD documents. This module treats that store as an input source and
//! derives vector-index records from it without changing its role as the
//! TerminusDB-oriented representation.
//!
//! ## Current indexing policy
//!
//! The first-pass implementation only indexes documents whose `@type` is
//! `"Entry"`. These documents already contain the stable node identity and
//! symbol-level metadata needed for an initial semantic entry-point index:
//!
//! - `@id`
//! - `name`
//! - `fq_name`
//! - `kind`
//! - optional `documentation`
//! - optional `aliases`
//!
//! `Kind` documents are not indexed here yet. That keeps the initial pipeline
//! simple and stable while leaving room for future projections that may join
//! `Entry` and `Kind` data into richer representations.
//!
//! ## Identity and record shape
//!
//! Every vector record produced by this module preserves the canonical node URI
//! used in TerminusDB. This is the bridge between the graph database and the
//! vector database. The payload also stores a `record_key`, which allows the
//! ingestion pipeline to move from a one-node/one-record model to a one-node/
//! many-record model later without changing the core data flow.
//!
//! Intended future extensions include:
//!
//! - multiple representations per node, such as docs/signature/context
//! - chunked records for long documentation
//! - structural representations for non-semantic similarity
//!
//! ## Failure stages
//!
//! Embedding-backed ingestion introduces more failure points than plain JSON-LD
//! emission, so errors are separated into broad stages:
//!
//! - extraction / projection failures from JSON-LD
//! - embedding provider failures
//! - point / payload conversion failures
//! - upload-time failures handled by the upload layer
//!
//! This module defines the error and data boundaries needed to make those
//! stages explicit while keeping the initial implementation lightweight.
//!
//! ## Non-goals
//!
//! This module does not:
//!
//! - execute search queries
//! - rank results
//! - expand graph neighborhoods
//! - implement A* or heuristic traversal
//! - own Qdrant upload transport
//!
//! Those concerns belong in separate runtime or retrieval-facing components.
use std::collections::HashMap;

use async_openai::{Client, config::OpenAIConfig, types::embeddings::CreateEmbeddingRequestArgs};
use async_trait::async_trait;
use qdrant_client::{Payload, qdrant::{PointId, PointStruct, Value}};
use tokio::task::JoinSet;
use tracing::info;

use crate::terminusdb::termdb::DocStore;

const DEFAULT_EMBED_CONCURRENCY: usize = 16;

#[derive(Debug, Clone, Copy)]
pub struct EmbeddingProgress {
	pub completed: usize,
	pub total:     usize,
}
/// Provider-agnostic embedding error.
///
/// Keep this structured early so the pipeline can distinguish between:
/// - source data issues
/// - provider failures
/// - upload/encoding failures

#[derive(Debug)]
pub enum EmbeddingError {
	MissingText,
	MissingVector,

	MissingField(&'static str),
	InvalidFieldType(&'static str),
	InvalidDocumentShape(&'static str),
	UnsupportedDocumentType(String),

	InvalidPointId(String),
	Provider(String),
	Conversion(String),
}

impl std::fmt::Display for EmbeddingError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::MissingText => write!(f, "missing text for embedding"),
			Self::MissingVector => write!(f, "missing generated vector"),
			Self::MissingField(field) => write!(f, "missing required field: {field}"),
			Self::InvalidFieldType(field) => write!(f, "invalid field type for: {field}"),
			Self::InvalidDocumentShape(msg) => write!(f, "invalid document shape: {msg}"),
			Self::UnsupportedDocumentType(doc_type) => {
				write!(f, "unsupported document type for embedding extraction: {doc_type}")
			}
			Self::InvalidPointId(msg) => write!(f, "invalid point id: {msg}"),
			Self::Provider(msg) => write!(f, "embedding provider error: {msg}"),
			Self::Conversion(msg) => write!(f, "conversion error: {msg}"),
		}
	}
}
impl std::error::Error for EmbeddingError {}

/// The high-level semantic type of the vectorized record.
///
/// This is for storage / filtering / future retrieval mode distinctions.
/// Keep this compact and stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
	/// Main semantic view of a node for feature-oriented retrieval.
	SemanticNode,

	/// Future: shape/field/layout-oriented similarity.
	StructuralNode,
}

impl RecordKind {
	pub fn as_str(&self) -> &'static str {
		match self {
			Self::SemanticNode => "semantic_node",
			Self::StructuralNode => "structural_node",
		}
	}
}

/// The content view represented by this vector.
///
/// One graph node may emit multiple records later:
/// - docs
/// - signature
/// - context
/// - structural summary
///
/// This enum is intentionally small for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepresentationKind {
	Docs,
	Signature,
	Context,
	Structural,
}

impl RepresentationKind {
	pub fn as_str(&self) -> &'static str {
		match self {
			Self::Docs => "docs",
			Self::Signature => "signature",
			Self::Context => "context",
			Self::Structural => "structural",
		}
	}
}

/// Metadata stored in Qdrant payload.
///
/// This should be enough to:
/// - map back to the canonical TerminusDB URI
/// - filter results by kind/version/language later
/// - understand what representation produced the vector
///
/// Keep payload values flat and string-ish unless there is a strong reason not
/// to.
#[derive(Debug, Clone)]
pub struct VectorPayload {
	/// Canonical node URI shared with TerminusDB.
	pub uri: String,

	/// Stable logical identifier for this vector record.
	///
	/// This may be:
	/// - the URI itself
	/// - URI + suffix like "#docs"
	/// - URI + chunk/index suffix
	pub record_key: String,

	/// Optional fully qualified symbol name.
	pub fq_name: Option<String>,

	/// Language identifier, e.g. "rust".
	pub language: String,

	/// Library / crate / package name.
	pub package: String,

	/// Optional package/library version.
	pub version: Option<String>,

	/// Symbol kind, e.g. "function", "struct", "module".
	pub symbol_kind: Option<String>,

	/// Logical family of this vector record.
	pub record_kind: RecordKind,

	/// Concrete representation used for the text that was embedded.
	pub representation_kind: RepresentationKind,

	/// Embedding model name used to generate the vector.
	pub embedding_model: String,

	/// Future-proofing for one-to-many expansion.
	pub chunk_index: Option<u32>,
	pub chunk_count: Option<u32>,
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

/// The source-side store-agnostic input unit for embedding.
///
/// This is the object the ingestion pipeline constructs before embedding.
/// It is intentionally not tied to Qdrant.
///
/// Later, your DocStore or emitter can produce one or more of these per node.
#[derive(Debug, Clone)]
pub struct EmbeddingDocument {
	/// Stable logical key for this embedding record.
	///
	/// Example:
	/// - same as URI for 1:1 storage
	/// - "<uri>#docs"
	/// - "<uri>#context"
	pub record_key: String,

	/// Canonical URI shared with TerminusDB.
	pub uri: String,

	/// Text that will be sent to the embedding provider.
	pub text: String,

	/// Metadata that will be preserved into the vector payload.
	pub fq_name:     Option<String>,
	pub language:    String,
	pub package:     String,
	pub version:     Option<String>,
	pub symbol_kind: Option<String>,

	/// What category of vector record this is.
	pub record_kind:         RecordKind,
	pub representation_kind: RepresentationKind,

	/// Optional future-proofing for chunked documents.
	pub chunk_index: Option<u32>,
	pub chunk_count: Option<u32>,
}

impl EmbeddingDocument {
	/// Basic validation before hitting the provider.
	pub fn validate(&self) -> Result<(), EmbeddingError> {
		if self.text.trim().is_empty() {
			return Err(EmbeddingError::MissingText);
		}
		Ok(())
	}
}

/// The store-agnostic result after embedding generation.
///
/// This is the main internal type that sits between:
/// - embedding provider
/// - Qdrant upload conversion
#[derive(Debug, Clone)]
pub struct EmbeddedRecord {
	/// Stable logical key for this vector record.
	pub record_key: String,

	/// Canonical graph URI.
	pub uri: String,

	/// The original text used to generate the vector.
	///
	/// Keep this for pipeline debugging/logging if useful.
	/// If you do not want to keep raw text here later, remove it.
	pub text: String,

	/// Dense embedding vector.
	pub vector: Vec<f32>,

	/// Provider/model metadata needed by payload creation.
	pub embedding_model: String,

	/// Metadata preserved for filtering + graph linkage.
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
	/// Convert the record metadata into a flat Qdrant payload.
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

/// Provider contract.
///
/// The backend pipeline can depend on this trait and swap implementations:
/// - OpenAI text embeddings
/// - local model
/// - mock test provider
///
/// Keep it synchronous for now if that simplifies your initial pipeline.
/// If your project is already async-heavy, convert this to async later.
#[async_trait]
pub trait EmbeddingProvider {
	/// Provider/model name used for payload and collection compatibility checks.
	fn model_name(&self) -> &str;

	/// Generate a vector from text.
	async fn embed_text(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;
}
/// Minimal service layer that coordinates:
/// - document validation
/// - provider call
/// - internal vector record construction This is the core boilerplate for
///   ingestion.
#[derive(Clone)]
pub struct EmbeddingService<P> {
	provider: P,
}

impl<P> EmbeddingService<P>
where
	P: EmbeddingProvider,
{
	pub fn new(provider: P) -> Self { Self { provider } }

	/// Embed a single document into a store-agnostic vector record.
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
			record_key: doc.record_key,
			uri: doc.uri,
			text: doc.text,
			vector,
			embedding_model: self.provider.model_name().to_string(),
			fq_name: doc.fq_name,
			language: doc.language,
			package: doc.package,
			version: doc.version,
			symbol_kind: doc.symbol_kind,
			record_kind: doc.record_kind,
			representation_kind: doc.representation_kind,
			chunk_index: doc.chunk_index,
			chunk_count: doc.chunk_count,
		})
	}

	/// Embed multiple documents in order.
	///
	/// This uses bounded concurrency because registry-backed package indexing
	/// often has thousands of entry documents and one-at-a-time embeddings make
	/// the pipeline look stuck for large packages.
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
			.map(|record| {
				record.ok_or_else(|| {
					EmbeddingError::Provider("embedding worker completed without a record".to_owned())
				})
			})
			.collect()
	}
}

/// OpenAI-backed embedding provider.
///
/// This uses the standard OpenAI API key flow. By default, `Client::new()`
/// reads `OPENAI_API_KEY` from the environment. If you want to inject a key
/// directly, use `new_with_api_key(...)` instead.
#[derive(Clone)]
pub struct OpenAIEmbeddingProvider {
	client:     Client<OpenAIConfig>,
	model_name: String,
}

impl OpenAIEmbeddingProvider {
	/// Construct from environment configuration.
	///
	/// Expects `OPENAI_API_KEY` to be set.
	pub fn new(model_name: impl Into<String>) -> Self {
		Self { client: Client::new(), model_name: model_name.into() }
	}

	/// Construct with an explicit API key.
	pub fn new_with_api_key(api_key: impl Into<String>, model_name: impl Into<String>) -> Self {
		let config = OpenAIConfig::new().with_api_key(api_key.into());
		let client = Client::with_config(config);

		Self { client, model_name: model_name.into() }
	}
}

#[async_trait]
impl EmbeddingProvider for OpenAIEmbeddingProvider {
	fn model_name(&self) -> &str { &self.model_name }

	async fn embed_text(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
		if text.trim().is_empty() {
			return Err(EmbeddingError::MissingText);
		}

		let request = CreateEmbeddingRequestArgs::default()
			.model(self.model_name.clone())
			.input(text)
			.build()
			.map_err(|e| EmbeddingError::Provider(format!("failed to build embedding request: {e}")))?;

		let response = self
			.client
			.embeddings()
			.create(request)
			.await
			.map_err(|e| EmbeddingError::Provider(format!("openai embeddings request failed: {e}")))?;

		let embedding =
			response.data.into_iter().next().ok_or(EmbeddingError::MissingVector)?.embedding;

		if embedding.is_empty() {
			return Err(EmbeddingError::MissingVector);
		}

		Ok(embedding)
	}
}

/// Qdrant-facing conversion helpers.
///
/// This is intentionally simple:
/// - convert an embedded record into PointStruct
/// - keep Qdrant-specific logic contained here
pub struct QdrantPointFactory;

impl QdrantPointFactory {
	/// Build a Qdrant point from an embedded record.
	///
	/// The caller supplies the final Qdrant point id.
	/// This keeps point-id policy separate from embedding generation.
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
		.map_err(|error| EmbeddingError::Provider(format!("embedding task join failed: {error}")))??;
	out[index] = Some(record);
	*completed += 1;
	if *completed == total || total <= 50 || *completed % 10 == 0 {
		on_progress(EmbeddingProgress { completed: *completed, total });
		info!(completed = *completed, total, "embedding progress");
	}
	Ok(())
}

/// Utilities for point id policy.
///
/// Qdrant supports multiple point id forms. Do not hard-code a numeric `1_u64`.
/// Keep the policy explicit here so you can later switch to:
/// - deterministic hash -> u64
/// - string ids if supported by your chosen qdrant-client API version For now
///   this helper provides the minimal conversion surface.
pub struct PointIdFactory;

impl PointIdFactory {
	/// Use a u64 when you already have one from a deterministic id scheme.
	pub fn from_u64(id: u64) -> PointId { PointId::from(id) }

	/// Temporary non-stable point id generation for sprint usage.
	///
	/// This ignores the record key and generates a pseudo-random-ish u64 from the
	/// current time. This is NOT suitable for long-term idempotent upserts.
	/// Replace this later with a deterministic hash of `record_key`.
	pub fn from_record_key(_record_key: &str) -> Result<PointId, EmbeddingError> {
		use std::time::{SystemTime, UNIX_EPOCH};

		let nanos = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.map_err(|e| EmbeddingError::InvalidPointId(format!("system clock error: {e}")))?
			.as_nanos();

		let id = (nanos & u64::MAX as u128) as u64;
		Ok(PointId::from(id))
	}
}
/// Small helper for future ingestion code.
///
/// This is the likely shape of your backend flow:
/// 1. build EmbeddingDocument(s) from source records
/// 2. call EmbeddingService
/// 3. assign point ids
/// 4. convert to Qdrant points
///
/// This helper keeps that boilerplate together for one document.
pub async fn embed_and_build_qdrant_point<P>(
	service: &EmbeddingService<P>,
	point_id: PointId,
	doc: EmbeddingDocument,
) -> Result<PointStruct, EmbeddingError>
where
	P: EmbeddingProvider,
{
	let record = service.embed_document(doc).await?;
	QdrantPointFactory::build_point(point_id, record)
}

/// Helper functions
fn get_object(
	value: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>, EmbeddingError> {
	value.as_object().ok_or(EmbeddingError::InvalidDocumentShape("expected top-level JSON object"))
}

fn get_required_str<'a>(
	obj: &'a serde_json::Map<String, serde_json::Value>,
	field: &'static str,
) -> Result<&'a str, EmbeddingError> {
	let value = obj.get(field).ok_or(EmbeddingError::MissingField(field))?;
	value.as_str().ok_or(EmbeddingError::InvalidFieldType(field))
}

fn get_optional_str<'a>(
	obj: &'a serde_json::Map<String, serde_json::Value>,
	field: &'static str,
) -> Result<Option<&'a str>, EmbeddingError> {
	match obj.get(field) {
		Some(value) => value.as_str().map(Some).ok_or(EmbeddingError::InvalidFieldType(field)),
		None => Ok(None),
	}
}

fn get_optional_string_array(
	obj: &serde_json::Map<String, serde_json::Value>,
	field: &'static str,
) -> Result<Vec<String>, EmbeddingError> {
	match obj.get(field) {
		Some(value) => {
			let arr = value.as_array().ok_or(EmbeddingError::InvalidFieldType(field))?;

			let mut out = Vec::with_capacity(arr.len());
			for item in arr {
				let s = item.as_str().ok_or(EmbeddingError::InvalidFieldType(field))?;
				out.push(s.to_owned());
			}
			Ok(out)
		}
		None => Ok(Vec::new()),
	}
}

/// Extract the symbol kind prefix from the `kind` field stored on Entry docs.
///
/// Current `Entry.emit(...)` stores:
/// - `kind = prefix + ctx.uri_path(...)`
///
/// For a first pass, use the leading alpha prefix as the kind label.
/// If this cannot be derived, return `None` rather than failing the whole
/// record.
fn extract_symbol_kind(kind_ref: &str) -> Option<String> {
	let prefix: String =
		kind_ref.chars().take_while(|c| c.is_ascii_alphabetic() || *c == '_').collect();

	if prefix.is_empty() { None } else { Some(prefix.to_lowercase()) }
}

/// Build the text body to send to the embedding provider.
///
/// First-pass policy:
/// - always include fq_name and name
/// - include symbol kind if available
/// - include aliases if present
/// - include documentation if present
///
/// Do not inline members/context yet. Keep the first semantic view focused.
fn build_entry_embedding_text(
	name: &str,
	fq_name: &str,
	symbol_kind: Option<&str>,
	aliases: &[String],
	documentation: Option<&str>,
) -> String {
	let mut parts: Vec<String> = Vec::new();

	parts.push(format!("fq_name: {fq_name}"));
	parts.push(format!("name: {name}"));

	if let Some(kind) = symbol_kind {
		parts.push(format!("kind: {kind}"));
	}

	if !aliases.is_empty() {
		parts.push(format!("aliases: {}", aliases.join(", ")));
	}

	if let Some(doc) = documentation {
		let trimmed = doc.trim();
		if !trimmed.is_empty() {
			parts.push(format!("documentation: {trimmed}"));
		}
	}

	parts.join("\n")
}

/// Extracts document from an Entry as a serde_json::VAlue
pub fn embedding_document_from_entry_value(
	value: &serde_json::Value,
	language: &str,
	package: &str,
	version: Option<&str>,
) -> Result<EmbeddingDocument, EmbeddingError> {
	let obj = get_object(value)?;

	let doc_type = get_required_str(obj, "@type")?;
	if doc_type != "Entry" {
		return Err(EmbeddingError::UnsupportedDocumentType(doc_type.to_owned()));
	}

	let uri = get_required_str(obj, "@id")?.to_owned();
	let name = get_required_str(obj, "name")?;
	let fq_name = get_required_str(obj, "fq_name")?.to_owned();
	let kind_ref = get_required_str(obj, "kind")?;
	let documentation = get_optional_str(obj, "documentation")?;
	let aliases = get_optional_string_array(obj, "aliases")?;

	let symbol_kind = extract_symbol_kind(kind_ref);
	let text =
		build_entry_embedding_text(name, &fq_name, symbol_kind.as_deref(), &aliases, documentation);

	Ok(EmbeddingDocument {
		record_key: uri.clone(),
		uri,
		text,
		fq_name: Some(fq_name),
		language: language.to_owned(),
		package: package.to_owned(),
		version: version.map(str::to_owned),
		symbol_kind,
		record_kind: RecordKind::SemanticNode,
		representation_kind: RepresentationKind::Docs,
		chunk_index: None,
		chunk_count: None,
	})
}

pub fn embedding_documents_from_docstore(
	store: &DocStore,
	language: &str,
	package: &str,
	version: Option<&str>,
) -> Result<Vec<EmbeddingDocument>, EmbeddingError> {
	let mut out = Vec::new();

	for (_uri, value) in store.documents_sorted() {
		let obj = match value.as_object() {
			Some(obj) => obj,
			None => continue,
		};

		let doc_type = match obj.get("@type").and_then(serde_json::Value::as_str) {
			Some(t) => t,
			None => continue,
		};

		if doc_type != "Entry" {
			continue;
		}

		let doc = embedding_document_from_entry_value(value, language, package, version)?;
		out.push(doc);
	}

	Ok(out)
}
