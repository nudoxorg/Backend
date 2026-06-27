//! Fan-out sinks for the parse-once pipeline.
//!
//! Each [`SymbolSink`] consumes the same `&[ParsedSymbol]` slice (and, for the
//! graph sink, a pre-built `DocStore` projected from the same parse). Running
//! them through one uniform trait gives a single [`IngestReport`] describing
//! what every store did, and one place to decide whether a failure is fatal.

use std::sync::Arc;

use async_trait::async_trait;
use nudox_store::NudoxStore;
use serde_json::Value;
use tracing::{info, warn};

use crate::{config::QdrantSettings, error::{AppError, IngestError}, ingest::parsed_symbol::{Identity, PackageCoord, ParsedSymbol}, sync_progress::{PackageSyncPhase, ProgressReporter}, terminusdb::{embedding_service::{EmbeddingProgress, EmbeddingService, OpenAIEmbeddingProvider, PointIdFactory, QdrantPointFactory}, qdrant_upload::{QdrantConfig, upload_points}, termdb::DocStore, upload::{DocumentUploadProgress, TerminusConfig, upload_documents, upload_schema}}, text_index::SymbolTextIndex};

/// Identifies one sink in the fan-out pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkId {
	Sqlite,
	Text,
	Orchestrator,
	Terminus,
	Qdrant,
}

impl SinkId {
	pub fn as_str(self) -> &'static str {
		match self {
			SinkId::Sqlite => "sqlite",
			SinkId::Text => "text",
			SinkId::Orchestrator => "orchestrator",
			SinkId::Terminus => "terminus",
			SinkId::Qdrant => "qdrant",
		}
	}
}

impl std::fmt::Display for SinkId {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.as_str())
	}
}

/// A single store's contribution to one ingestion.
#[derive(Debug, Clone)]
pub struct SinkOutcome {
	pub name:    SinkId,
	pub indexed: usize,
	pub error:   Option<String>,
}

/// What every sink did during one ingestion.
#[derive(Debug, Default, Clone)]
pub struct IngestReport {
	pub outcomes: Vec<SinkOutcome>,
}

impl IngestReport {
	/// Number of records indexed by the named sink (0 if absent or failed).
	pub fn indexed_by(&self, id: SinkId) -> usize {
		self.outcomes.iter().find(|o| o.name == id).map_or(0, |o| o.indexed)
	}
}

/// A destination that consumes the parse-once projection.
#[async_trait]
pub trait SymbolSink: Send + Sync {
	fn name(&self) -> SinkId;

	/// Whether a failure should abort the whole ingestion (Terminus / vectors)
	/// rather than being recorded and skipped (occurrence store, text, symbol
	/// search).
	fn fatal(&self) -> bool { false }

	async fn accept(
		&self,
		symbols: &[ParsedSymbol],
		coord: &PackageCoord,
		progress: &ProgressReporter,
	) -> Result<usize, crate::error::AppError>;
}

/// Run every sink in order, honoring `fatal()`: a fatal sink's error aborts the
/// run; a non-fatal sink's error is recorded and the run continues.
pub async fn run_sinks(
	sinks: &[Box<dyn SymbolSink>],
	symbols: &[ParsedSymbol],
	coord: &PackageCoord,
	progress: &ProgressReporter,
) -> Result<IngestReport, crate::error::AppError> {
	let mut report = IngestReport::default();
	for sink in sinks {
		match sink.accept(symbols, coord, progress).await {
			Ok(indexed) => {
				report.outcomes.push(SinkOutcome { name: sink.name(), indexed, error: None });
			}
			Err(e) if sink.fatal() => return Err(e),
			Err(e) => {
				warn!(sink = %sink.name(), error = %e, "non-fatal sink failed");
				report.outcomes.push(SinkOutcome {
					name:    sink.name(),
					indexed: 0,
					error:   Some(e.to_string()),
				});
			}
		}
	}
	Ok(report)
}

// ── SQLite occurrence store ─────────────────────────────────────────────────

/// Registers every `Entry/*` URI so deferred occurrence blobs waiting on this
/// library can resolve.
pub struct SqliteRegisterSink {
	pub store: Arc<NudoxStore>,
}

#[async_trait]
impl SymbolSink for SqliteRegisterSink {
	fn name(&self) -> SinkId { SinkId::Sqlite }

	async fn accept(
		&self,
		symbols: &[ParsedSymbol],
		coord: &PackageCoord,
		_progress: &ProgressReporter,
	) -> Result<usize, crate::error::AppError> {
		let uris: Vec<String> = symbols.iter().map(|s| s.entry_uri.to_string()).collect();
		let n = self
			.store
			.register_library(coord.language.as_str(), &coord.package, &coord.version, uris.iter().map(String::as_str))
			.await
			.map_err(|source| AppError::Ingest(IngestError::StoreRegister { package: coord.package.to_string(), source }))?;
		info!(lib = %coord.package, count = n, "symbols registered in occurrence store");
		Ok(n)
	}
}

// ── Full-text index (/text-search) ──────────────────────────────────────────

pub struct TextIndexSink {
	pub index: Arc<SymbolTextIndex>,
}

#[async_trait]
impl SymbolSink for TextIndexSink {
	fn name(&self) -> SinkId { SinkId::Text }

	async fn accept(
		&self,
		symbols: &[ParsedSymbol],
		coord: &PackageCoord,
		_progress: &ProgressReporter,
	) -> Result<usize, crate::error::AppError> {
		let n = self.index.index_batch(coord, symbols)?;
		info!(lib = %coord.package, count = n, "symbols indexed in text search");
		Ok(n)
	}
}

// ── Symbol-search orchestrator (/symbol-search) ─────────────────────────────

pub struct OrchestratorSink {
	pub orchestrator:       Arc<nudox_orchestrator::Orchestrator>,
	/// `"{org}/{db}"` when Terminus is configured; `None` → `Identity::Local`.
	pub terminus_instance:  Option<String>,
}

#[async_trait]
impl SymbolSink for OrchestratorSink {
	fn name(&self) -> SinkId { SinkId::Orchestrator }

	async fn accept(
		&self,
		symbols: &[ParsedSymbol],
		coord: &PackageCoord,
		_progress: &ProgressReporter,
	) -> Result<usize, crate::error::AppError> {
		if !matches!(coord.language, nudox_core::Language::Rust) {
			return Ok(0);
		}
		let identity = match self.terminus_instance.as_deref() {
			Some(instance) => Identity::Deterministic { instance },
			None => Identity::Local,
		};
		let mut count = 0usize;
		for symbol in symbols {
			match self.orchestrator.ingest(symbol.to_blob_info(coord, &identity)).await {
				Ok(_) => count += 1,
				Err(e) => warn!(
					lib = %coord.package,
					symbol = %symbol.fq_name,
					error = %e,
					"orchestrator ingest failed for symbol"
				),
			}
		}
		info!(lib = %coord.package, count, "symbols fed to symbol-search orchestrator");
		Ok(count)
	}
}

// ── TerminusDB graph (/terminus_search, /expand, /run) ──────────────────────

/// The graph sink. Fed the `DocStore` projected from the same parse; uploads
/// schema (when requested) then the JSON-LD documents.
pub struct TerminusSink {
	pub config:        TerminusConfig,
	pub schema:        Option<Vec<Value>>,
	pub store:         DocStore,
}

#[async_trait]
impl SymbolSink for TerminusSink {
	fn name(&self) -> SinkId { SinkId::Terminus }

	fn fatal(&self) -> bool { true }

	async fn accept(
		&self,
		_symbols: &[ParsedSymbol],
		_coord: &PackageCoord,
		progress: &ProgressReporter,
	) -> Result<usize, crate::error::AppError> {
		if let Some(schema) = &self.schema {
			progress.phase_with_detail(
				PackageSyncPhase::UploadingSchema,
				Some("uploading TerminusDB schema".to_owned()),
			);
			upload_schema(&self.config, schema.clone()).await?;
		}

		let total = self.store.docs.len();
		progress.phase_with_detail(
			PackageSyncPhase::UploadingDocuments,
			Some(format!("uploading {total} documents")),
		);
		upload_documents(&self.config, &self.store, |p: DocumentUploadProgress| {
			progress.phase_with_detail(
				PackageSyncPhase::UploadingDocuments,
				Some(format!(
					"uploaded {}/{} documents (chunk {}/{})",
					p.completed_docs, p.total_docs, p.completed_chunks, p.total_chunks,
				)),
			);
		})
		.await?;
		Ok(total)
	}
}

// ── Vector index (/search) ──────────────────────────────────────────────────

/// Embeds every symbol and upserts the vectors into Qdrant with deterministic,
/// idempotent point ids.
pub struct VectorSink {
	pub settings:   QdrantSettings,
	pub model:      String,
	pub collection: String,
}

impl VectorSink {
	/// Embed and upload `symbols`, consuming them. Each symbol's `embedding_text`
	/// and `fq_name` are moved into the embedding document — no clone needed.
	pub async fn accept_owned(
		&self,
		symbols: Vec<ParsedSymbol>,
		coord: &PackageCoord,
		progress: &ProgressReporter,
	) -> Result<usize, crate::error::AppError> {
		let docs: Vec<_> =
			symbols.into_iter().map(|s| s.into_embedding_document(coord)).collect();
		progress.phase_with_detail(
			PackageSyncPhase::Embedding,
			Some(format!("embedding {} symbols", docs.len())),
		);

		let service = EmbeddingService::new(OpenAIEmbeddingProvider::new(&self.model));
		let records = service
			.embed_documents(docs, |p: EmbeddingProgress| {
				progress.phase_with_detail(
					PackageSyncPhase::Embedding,
					Some(format!("embedded {}/{} symbols", p.completed, p.total)),
				);
			})
			.await?;

		let count = records.len();
		progress.phase_with_detail(
			PackageSyncPhase::UploadingVectors,
			Some(format!("uploading {count} vectors")),
		);

		let mut points = Vec::with_capacity(count);
		for record in records {
			let point_id = PointIdFactory::deterministic(&record.record_key);
			points.push(QdrantPointFactory::build_point(point_id, record)?);
		}

		let qdrant_config = QdrantConfig {
			endpoint:        self.settings.endpoint.clone(),
			collection_name: self.collection.clone(),
			vector_size:     self.settings.vector_size,
			distance:        self.settings.distance,
		};
		upload_points(&qdrant_config, points).await?;
		info!(collection = self.collection, count, "vector upload complete");
		Ok(count)
	}
}
