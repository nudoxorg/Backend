//! Fan-out sinks for the parse-once pipeline.
//!
//! Each [`SymbolSink`] consumes the same `&[ParsedSymbol]` slice (and, for the
//! graph sink, a pre-built `DocStore` projected from the same parse). Running
//! them through one uniform trait gives a single [`IngestReport`] describing
//! what every store did, and one place to decide whether a failure is fatal.

use async_trait::async_trait;
use nudox_store::NudoxStore;
use serde_json::Value;
use tracing::{info, warn};

use crate::{config::QdrantSettings, ingest::parsed_symbol::{PackageCoord, ParsedSymbol}, sync_progress::{PackageSyncPhase, ProgressReporter}, terminusdb::{embedding_service::{EmbeddingProgress, EmbeddingService, OpenAIEmbeddingProvider, PointIdFactory, QdrantPointFactory}, qdrant_upload::{QdrantConfig, upload_points}, termdb::DocStore, upload::{DocumentUploadProgress, TerminusConfig, upload_documents, upload_schema}}, text_index::SymbolTextIndex};

/// A single store's contribution to one ingestion.
#[derive(Debug, Clone)]
pub struct SinkOutcome {
	pub name:    &'static str,
	pub indexed: usize,
	pub error:   Option<String>,
}

/// What every sink did during one ingestion.
#[derive(Debug, Default, Clone)]
pub struct IngestReport {
	pub outcomes: Vec<SinkOutcome>,
}

impl IngestReport {
	/// Number of records indexed by the sink with the given name (0 if absent or
	/// failed).
	pub fn indexed_by(&self, name: &str) -> usize {
		self.outcomes.iter().find(|o| o.name == name).map_or(0, |o| o.indexed)
	}
}

/// A destination that consumes the parse-once projection.
#[async_trait]
pub trait SymbolSink {
	fn name(&self) -> &'static str;

	/// Whether a failure should abort the whole ingestion (Terminus / vectors)
	/// rather than being recorded and skipped (occurrence store, text, symbol
	/// search).
	fn fatal(&self) -> bool { false }

	async fn accept(
		&self,
		symbols: &[ParsedSymbol],
		coord: &PackageCoord,
		progress: Option<&ProgressReporter>,
	) -> Result<usize, crate::error::AppError>;
}

/// Run every sink in order, honoring `fatal()`: a fatal sink's error aborts the
/// run; a non-fatal sink's error is recorded and the run continues.
pub async fn run_sinks(
	sinks: &[Box<dyn SymbolSink + '_>],
	symbols: &[ParsedSymbol],
	coord: &PackageCoord,
	progress: Option<&ProgressReporter>,
) -> Result<IngestReport, crate::error::AppError> {
	let mut report = IngestReport::default();
	for sink in sinks {
		match sink.accept(symbols, coord, progress).await {
			Ok(indexed) => {
				report.outcomes.push(SinkOutcome { name: sink.name(), indexed, error: None });
			}
			Err(e) if sink.fatal() => return Err(e),
			Err(e) => {
				warn!(sink = sink.name(), error = %e, "non-fatal sink failed");
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
pub struct SqliteRegisterSink<'a> {
	pub store: &'a NudoxStore,
}

#[async_trait]
impl SymbolSink for SqliteRegisterSink<'_> {
	fn name(&self) -> &'static str { "sqlite" }

	async fn accept(
		&self,
		symbols: &[ParsedSymbol],
		coord: &PackageCoord,
		_progress: Option<&ProgressReporter>,
	) -> Result<usize, crate::error::AppError> {
		let uris: Vec<String> = symbols.iter().map(|s| s.entry_uri.to_string()).collect();
		let n = self
			.store
			.register_library(&coord.language, &coord.package, &coord.version, uris.iter().map(String::as_str))
			.await
			.map_err(|e| crate::error::AppError::Internal {
				message: format!("occurrence store register_library failed: {e}"),
			})?;
		info!(lib = coord.package, count = n, "symbols registered in occurrence store");
		Ok(n)
	}
}

// ── Full-text index (/text-search) ──────────────────────────────────────────

pub struct TextIndexSink<'a> {
	pub index: &'a SymbolTextIndex,
}

#[async_trait]
impl SymbolSink for TextIndexSink<'_> {
	fn name(&self) -> &'static str { "text" }

	async fn accept(
		&self,
		symbols: &[ParsedSymbol],
		coord: &PackageCoord,
		_progress: Option<&ProgressReporter>,
	) -> Result<usize, crate::error::AppError> {
		let n = self.index.index_batch(coord, symbols)?;
		info!(lib = coord.package, count = n, "symbols indexed in text search");
		Ok(n)
	}
}

// ── Symbol-search orchestrator (/symbol-search) ─────────────────────────────

pub struct OrchestratorSink<'a> {
	pub orchestrator: &'a nudox_orchestrator::Orchestrator,
}

#[async_trait]
impl SymbolSink for OrchestratorSink<'_> {
	fn name(&self) -> &'static str { "orchestrator" }

	async fn accept(
		&self,
		symbols: &[ParsedSymbol],
		coord: &PackageCoord,
		_progress: Option<&ProgressReporter>,
	) -> Result<usize, crate::error::AppError> {
		// nudox_core::Language only models Rust today.
		if coord.language != "rust" {
			return Ok(0);
		}
		let mut count = 0usize;
		for symbol in symbols {
			match self.orchestrator.ingest(symbol.to_blob_info(coord)).await {
				Ok(_) => count += 1,
				Err(e) => warn!(
					lib = coord.package,
					symbol = %symbol.fq_name,
					error = %e,
					"orchestrator ingest failed for symbol"
				),
			}
		}
		info!(lib = coord.package, count, "symbols fed to symbol-search orchestrator");
		Ok(count)
	}
}

// ── TerminusDB graph (/terminus_search, /expand, /run) ──────────────────────

/// The graph sink. Fed the `DocStore` projected from the same parse; uploads
/// schema (when requested) then the JSON-LD documents.
pub struct TerminusSink<'a> {
	pub config:        &'a TerminusConfig,
	pub schema:        Option<Vec<Value>>,
	pub store:         DocStore,
}

#[async_trait]
impl SymbolSink for TerminusSink<'_> {
	fn name(&self) -> &'static str { "terminus" }

	fn fatal(&self) -> bool { true }

	async fn accept(
		&self,
		_symbols: &[ParsedSymbol],
		_coord: &PackageCoord,
		progress: Option<&ProgressReporter>,
	) -> Result<usize, crate::error::AppError> {
		if let Some(schema) = &self.schema {
			if let Some(progress) = progress {
				progress.phase_with_detail(
					PackageSyncPhase::UploadingSchema,
					Some("uploading TerminusDB schema".to_owned()),
				);
			}
			upload_schema(self.config, schema.clone()).await?;
		}

		let total = self.store.docs.len();
		if let Some(progress) = progress {
			progress.phase_with_detail(
				PackageSyncPhase::UploadingDocuments,
				Some(format!("uploading {total} documents")),
			);
		}
		upload_documents(self.config, &self.store, |p: DocumentUploadProgress| {
			if let Some(progress) = progress {
				progress.phase_with_detail(
					PackageSyncPhase::UploadingDocuments,
					Some(format!(
						"uploaded {}/{} documents (chunk {}/{})",
						p.completed_docs, p.total_docs, p.completed_chunks, p.total_chunks,
					)),
				);
			}
		})
		.await?;
		Ok(total)
	}
}

// ── Vector index (/search) ──────────────────────────────────────────────────

/// Embeds every symbol and upserts the vectors into Qdrant with deterministic,
/// idempotent point ids.
pub struct VectorSink<'a> {
	pub settings:    &'a QdrantSettings,
	pub model:       &'a str,
	pub collection:  String,
}

#[async_trait]
impl SymbolSink for VectorSink<'_> {
	fn name(&self) -> &'static str { "qdrant" }

	fn fatal(&self) -> bool { true }

	async fn accept(
		&self,
		symbols: &[ParsedSymbol],
		coord: &PackageCoord,
		progress: Option<&ProgressReporter>,
	) -> Result<usize, crate::error::AppError> {
		let docs: Vec<_> = symbols.iter().map(|s| s.to_embedding_document(coord)).collect();
		if let Some(progress) = progress {
			progress.phase_with_detail(
				PackageSyncPhase::Embedding,
				Some(format!("embedding {} symbols", docs.len())),
			);
		}

		let service = EmbeddingService::new(OpenAIEmbeddingProvider::new(self.model));
		let records = service
			.embed_documents(docs, |p: EmbeddingProgress| {
				if let Some(progress) = progress {
					progress.phase_with_detail(
						PackageSyncPhase::Embedding,
						Some(format!("embedded {}/{} symbols", p.completed, p.total)),
					);
				}
			})
			.await
			.map_err(|e| crate::error::AppError::Embedding(e.to_string()))?;

		let count = records.len();
		if let Some(progress) = progress {
			progress.phase_with_detail(
				PackageSyncPhase::UploadingVectors,
				Some(format!("uploading {count} vectors")),
			);
		}

		let mut points = Vec::with_capacity(count);
		for record in records {
			let point_id = PointIdFactory::deterministic(&record.record_key);
			points.push(
				QdrantPointFactory::build_point(point_id, record)
					.map_err(|e| crate::error::AppError::Embedding(e.to_string()))?,
			);
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
