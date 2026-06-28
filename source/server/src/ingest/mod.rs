//! Parse-once ingestion fan-out.
//!
//! A package is parsed to one [`Index`], projected **once** into a
//! `Vec<ParsedSymbol>` (and one Terminus `DocStore`), and that single
//! projection feeds every sink — the occurrence store, full-text index,
//! symbol-search orchestrator, TerminusDB graph, and the vector index. No sink
//! re-parses another store's output.

pub mod embedding;
pub mod embedding_types;
pub mod parsed_symbol;
pub mod qdrant;
pub mod sink;

use std::{collections::HashMap, path::Path, sync::Arc};

use ir::entry::Index;
use store::NudoxStore;
use semver::Version;
use tokio::task::spawn_blocking;
use tracing::info;

use crate::{core::backend::LanguageBackend, config::PipelineConfig, http::error::AppError, ingest::{parsed_symbol::{Identity, PackageCoord, project}, sink::{OrchestratorSink, SinkId, SqliteRegisterSink, SymbolSink, TerminusSink, TextIndexSink, VectorSink, run_sinks}}, sync_progress::{PackageSyncPhase, ProgressReporter}, emit::Runner, terminus::schema::{CrateInfo, DocCtx, DocStore}, search::text::SymbolTextIndex};

#[derive(Debug, Clone)]
pub struct IngestionSummary {
	pub entry_count:          usize,
	pub document_count:       usize,
	pub vector_count:         usize,
	/// Symbols registered in the SQLite occurrence store (0 if no store is
	/// wired).
	pub symbols_registered:   usize,
	/// Symbols indexed in the local text search index (0 if no index is wired).
	pub symbols_text_indexed: usize,
	/// Symbols fed into the nudox symbol-search Orchestrator (0 if not wired).
	pub symbols_orchestrated: usize,
}

/// The configured ingestion fan-out destinations.
///
/// Built once at startup and shared (as cloned `Arc`s) by the registry — which
/// writes to them during ingestion — and the API layer, which reads from
/// `text_index` and `orchestrator` to serve `/text-search` and `/symbol-search`.
///
/// This replaces the three `Option<Arc<_>>` handles that were previously
/// threaded by hand through `LocalRegistry` → sync → pipeline → sink assembly
/// (and the matching `with_*` builder triplet). A configured backend is `Some`;
/// an unconfigured one is `None` and contributes no sink.
#[derive(Clone, Default)]
pub struct IngestTargets {
	/// SQLite occurrence store; enables deferred occurrence resolution.
	pub store:  Option<Arc<NudoxStore>>,
	/// Local Tantivy full-text index backing `/text-search`.
	pub text_index:   Option<Arc<SymbolTextIndex>>,
	/// nudox-search orchestrator backing `/symbol-search`.
	pub orchestrator: Option<Arc<orchestrator::Orchestrator<orchestrator::WithSearcher>>>,
}

/// Generate IR for `package` off the async executor, project it once, and fan
/// it out to every configured sink.
///
/// This is the single, language-agnostic pipeline entry. `B` supplies IR
/// generation and the neutral language tag; everything downstream of
/// [`finalize_pipeline`] is language-neutral and never matches on a concrete
/// frontend.
pub async fn run_pipeline<B: LanguageBackend>(
	package: &B::Package,
	version: &Version,
	workspace: &Path,
	config: &PipelineConfig,
	progress: &ProgressReporter,
	targets: &IngestTargets,
) -> Result<IngestionSummary, AppError> {
	progress.phase_with_detail(
		PackageSyncPhase::GeneratingIr,
		Some(format!("generating {} IR", B::LANGUAGE.as_str())),
	);

	let pkg = package.clone();
	let ws = workspace.to_path_buf();
	let ver = version.clone();
	// IR generation (rustdoc / deno-doc) is CPU/blocking-I/O; run it off the async
	// executor. The source map for tree-sitter comes back from the same parse.
	let (index, source_map) = spawn_blocking(move || B::generate_ir(&pkg, &ws, &ver))
		.await
		.map_err(|source| AppError::TaskJoin { action: "IR generation", source })??;

	let coord = PackageCoord::new(B::LANGUAGE, B::package_name(package), &version.to_string());
	finalize_pipeline(coord, index, config, progress, targets, source_map).await
}

async fn finalize_pipeline(
	coord: PackageCoord,
	index: Index,
	config: &PipelineConfig,
	progress: &ProgressReporter,
	targets: &IngestTargets,
	source_map: HashMap<String, String>,
) -> Result<IngestionSummary, AppError> {
	let identity = match config.terminus.as_ref() {
		Some(t) => Identity::Deterministic { instance: format!("{}/{}", t.org, t.db).into() },
		None => Identity::Local,
	};
	let entry_count = index.entries_by_path.len();

	// The single parse-once projection, run off the async reactor. `project`
	// fans the IR out across a rayon worker pool (one core each); the graph
	// emission then consumes the same in-memory `Index`. Both are CPU-bound and
	// must not block the tokio reactor, so the whole projection+emission runs
	// inside one `spawn_blocking`. Neither re-parses the other's output.
	let coord_blocking = coord.clone();
	let identity_blocking = identity.clone();
	let lang = coord.language.as_str().to_owned();
	let pkg = coord.package.to_string();
	let ver = coord.version.to_string();
	let (symbols, doc_store) = spawn_blocking(move || {
		let symbols = project(&coord_blocking, &index, &source_map, &identity_blocking);
		let doc_store = emit_store(&lang, &pkg, &ver, index);
		(symbols, doc_store)
	})
	.await
	.map_err(|source| AppError::TaskJoin { action: "projection", source })?;
	let document_count = doc_store.docs.len();

	// Assemble non-consuming sinks (these borrow `&[ParsedSymbol]`).
	let mut sinks: Vec<Box<dyn SymbolSink>> = Vec::new();
	if let Some(store) = &targets.store {
		sinks.push(Box::new(SqliteRegisterSink { store: Arc::clone(store) }));
	}
	if let Some(idx) = &targets.text_index {
		sinks.push(Box::new(TextIndexSink { index: Arc::clone(idx) }));
	}
	// Language policy lives here, not in the sink: the orchestrator is
	// Rust-only for now because only the Rust parser produces blob-level source.
	if let Some(orch) = &targets.orchestrator
		&& matches!(coord.language, nudox_core::Language::Rust) {
			sinks.push(Box::new(OrchestratorSink {
				orchestrator: Arc::clone(orch),
				identity:     identity.clone(),
			}));
		}
	if let Some(terminus) = &config.terminus {
		let schema = if config.upload_schema {
			Some(serde_json::from_str::<Vec<serde_json::Value>>(include_str!("../../schema.json"))?)
		} else {
			None
		};
		sinks.push(Box::new(TerminusSink { config: terminus.clone(), schema, store: doc_store }));
	}

	// Build the consuming vector sink separately — it drains `symbols` after all
	// non-consuming sinks are done, moving `embedding_text`/`fq_name` instead of
	// cloning them.
	let vector_sink = config.qdrant.as_ref().map(|qdrant| {
		let collection = symbols_collection_name(&qdrant.collection_prefix);
		VectorSink { settings: qdrant.clone(), model: config.embedding_model.to_string(), collection }
	});

	let report = run_sinks(&sinks, &symbols, &coord, progress).await?;

	// VectorSink runs last and consumes symbols (into_embedding_document moves data).
	let vector_count = if let Some(vs) = vector_sink {
		vs.accept_owned(symbols, &coord, progress).await?
	} else {
		0
	};

	Ok(IngestionSummary {
		entry_count,
		document_count,
		vector_count,
		symbols_registered:   report.indexed_by(SinkId::Sqlite),
		symbols_text_indexed: report.indexed_by(SinkId::Text),
		symbols_orchestrated: report.indexed_by(SinkId::Orchestrator),
	})
}

fn emit_store(language: &str, package_name: &str, version: &str, index: Index) -> DocStore {
	let context_object = serde_json::json!({
		"@type": "@context",
		"@schema": "terminusdb:///schema#",
		"@base": "terminusdb:///data/",
		"xsd": "http://www.w3.org/2001/XMLSchema#",
		"sys": "http://terminusdb.com/schema/sys#"
	});

	let mut runner = Runner::new(DocCtx::init(
		CrateInfo::new(language.to_owned(), package_name.to_owned()),
		context_object,
	));
	runner.run(index.entries_by_path.into_values());
	let store = runner.into_docs();
	info!(documents = store.docs.len(), package = package_name, version = %version, "emission complete");
	store
}

/// The single Qdrant collection that holds every package-version's vectors.
///
/// Replaces the former collection-per-`{lang}_{pkg}_{ver}` scheme: scoping now
/// lives in each point's payload (`language`/`package`/`version`), so search is
/// one query against one global HNSW graph instead of an N-collection fan-out.
/// Both the ingest vector sink and the `/search` read path must agree on this.
pub(crate) fn symbols_collection_name(prefix: &str) -> String {
	format!("{}_symbols", sanitize_collection_segment(prefix))
}

fn sanitize_collection_segment(value: &str) -> String { crate::util::slug::ascii_segment(value) }
