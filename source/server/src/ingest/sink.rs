//! Fan-out sinks for the parse-once pipeline.
//!
//! Each [`SymbolSink`] consumes the same `&[ParsedSymbol]` slice (and, for the
//! graph sink, a pre-built `DocStore` projected from the same parse). Running
//! them through one uniform trait gives a single [`IngestReport`] describing
//! what every store did, and one place to decide whether a failure is fatal.

use std::sync::Arc;

use async_trait::async_trait;
use store::NudoxStore;
use serde_json::Value;
use tracing::{info, warn};

use crate::{config::QdrantSettings, http::error::{AppError, IngestError}, ingest::{embedding::{EmbeddingProgress, EmbeddingService, OpenAIEmbeddingProvider, PointIdFactory, QdrantPointFactory}, qdrant::{QdrantConfig, upload_points}, parsed_symbol::{Identity, PackageCoord, ParsedSymbol}}, sync_progress::{PackageSyncPhase, ProgressReporter}, terminus::{schema::DocStore, upload::{DocumentUploadProgress, TerminusConfig, upload_documents, upload_schema}}, search::text::SymbolTextIndex};

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
	) -> Result<usize, crate::http::error::AppError>;
}

/// Run every sink concurrently, honoring `fatal()`: a fatal sink's error aborts
/// the run; a non-fatal sink's error is recorded and the run continues.
///
/// The sinks are independent (each writes to its own store), so they are fanned
/// out with [`join_all`] and polled concurrently on this task — no spawning, so
/// the borrowed `symbols`/`coord`/`progress` need no `'static`/`Send` bound.
/// Error aggregation is preserved: outcomes are collected in sink order, the
/// first fatal failure is returned, and non-fatal failures are logged and
/// recorded with `indexed: 0`. (Concurrency means later sinks may have already
/// done work when a fatal sink fails; they ran in parallel rather than after it.)
pub async fn run_sinks(
	sinks: &[Box<dyn SymbolSink>],
	symbols: &[ParsedSymbol],
	coord: &PackageCoord,
	progress: &ProgressReporter,
) -> Result<IngestReport, crate::http::error::AppError> {
	let results = futures::future::join_all(sinks.iter().map(|sink| async move {
		let outcome = sink.accept(symbols, coord, progress).await;
		(sink.name(), sink.fatal(), outcome)
	}))
	.await;

	let mut report = IngestReport::default();
	for (name, fatal, outcome) in results {
		match outcome {
			Ok(indexed) => {
				report.outcomes.push(SinkOutcome { name, indexed, error: None });
			}
			Err(e) if fatal => return Err(e),
			Err(e) => {
				warn!(sink = %name, error = %e, "non-fatal sink failed");
				report.outcomes.push(SinkOutcome { name, indexed: 0, error: Some(e.to_string()) });
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
	) -> Result<usize, crate::http::error::AppError> {
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
	) -> Result<usize, crate::http::error::AppError> {
		let n = self.index.index_batch(coord, symbols)?;
		info!(lib = %coord.package, count = n, "symbols indexed in text search");
		Ok(n)
	}
}

// ── Symbol-search orchestrator (/symbol-search) ─────────────────────────────

pub struct OrchestratorSink {
	pub orchestrator: Arc<orchestrator::Orchestrator<orchestrator::WithSearcher>>,
	/// Pre-computed once in `finalize_pipeline`; carried here so `accept` is
	/// pure I/O with no identity re-derivation.
	pub identity:     Identity,
}

#[async_trait]
impl SymbolSink for OrchestratorSink {
	fn name(&self) -> SinkId { SinkId::Orchestrator }

	async fn accept(
		&self,
		symbols: &[ParsedSymbol],
		coord: &PackageCoord,
		_progress: &ProgressReporter,
	) -> Result<usize, crate::http::error::AppError> {
		let mut count = 0usize;
		for symbol in symbols {
			match self.orchestrator.ingest(symbol.to_blob_info(coord, &self.identity)).await {
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
	) -> Result<usize, crate::http::error::AppError> {
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

/// How many symbols are embedded + upserted as one unit. Caps peak memory at
/// `BATCH × MAX_INFLIGHT_BATCHES` embedded records/points rather than O(corpus).
const VECTOR_UPLOAD_BATCH: usize = 256;
/// How many batches may be embedding/uploading concurrently.
const VECTOR_MAX_INFLIGHT_BATCHES: usize = 2;

impl VectorSink {
	/// Embed and upload `symbols`, consuming them. Each symbol's `embedding_text`
	/// and `fq_name` are moved into the embedding document — no clone needed.
	///
	/// Records are streamed through bounded batches: instead of embedding the
	/// whole corpus, collecting every [`EmbeddedRecord`], then building every
	/// point and uploading once (peak memory O(corpus)), the documents are split
	/// into [`VECTOR_UPLOAD_BATCH`]-sized chunks and each chunk is embedded →
	/// built into points → upserted, with up to [`VECTOR_MAX_INFLIGHT_BATCHES`]
	/// chunks in flight. Peak memory is therefore O(batch × concurrency).
	pub async fn accept_owned(
		&self,
		symbols: Vec<ParsedSymbol>,
		coord: &PackageCoord,
		progress: &ProgressReporter,
	) -> Result<usize, crate::http::error::AppError> {
		use futures::stream::StreamExt;

		let docs: Vec<_> =
			symbols.into_iter().map(|s| s.into_embedding_document(coord)).collect();
		let total = docs.len();
		progress.phase_with_detail(
			PackageSyncPhase::Embedding,
			Some(format!("embedding {total} symbols")),
		);

		let service = EmbeddingService::new(OpenAIEmbeddingProvider::new(&self.model));
		let qdrant_config = QdrantConfig {
			endpoint:        self.settings.endpoint.clone(),
			collection_name: self.collection.clone(),
			vector_size:     self.settings.vector_size,
			distance:        self.settings.distance,
		};

		// Split the owned documents into bounded batches without cloning.
		let mut batches: Vec<Vec<_>> = Vec::new();
		let mut iter = docs.into_iter();
		loop {
			let batch: Vec<_> = iter.by_ref().take(VECTOR_UPLOAD_BATCH).collect();
			if batch.is_empty() {
				break;
			}
			batches.push(batch);
		}

		let service_ref = &service;
		let config_ref = &qdrant_config;
		let mut stream = futures::stream::iter(batches.into_iter().map(move |batch| async move {
			let records = service_ref.embed_documents(batch, |_p: EmbeddingProgress| {}).await?;
			let mut points = Vec::with_capacity(records.len());
			for record in records {
				let point_id = PointIdFactory::deterministic(&record.record_key);
				points.push(QdrantPointFactory::build_point(point_id, record)?);
			}
			let n = points.len();
			upload_points(config_ref, points).await?;
			Ok::<usize, crate::http::error::AppError>(n)
		}))
		.buffer_unordered(VECTOR_MAX_INFLIGHT_BATCHES);

		let mut count = 0usize;
		while let Some(result) = stream.next().await {
			count += result?;
			progress.phase_with_detail(
				PackageSyncPhase::UploadingVectors,
				Some(format!("uploaded {count}/{total} vectors")),
			);
		}

		info!(collection = self.collection, count, "vector upload complete");
		Ok(count)
	}
}
