//! Parse-once ingestion fan-out.
//!
//! A package is parsed to one [`Index`], projected **once** into a
//! `Vec<ParsedSymbol>` (and one Terminus `DocStore`), and that single
//! projection feeds every sink — the occurrence store, full-text index,
//! symbol-search orchestrator, TerminusDB graph, and the vector index. No sink
//! re-parses another store's output.

pub mod parsed_symbol;
pub mod sink;

use std::{collections::HashMap, path::Path, sync::Arc};

use ir::entry::Index;
use nudox_store::NudoxStore;
use semver::Version;
use tokio::task::spawn_blocking;
use tracing::info;

use crate::{config::{PipelineConfig, QdrantSettings}, core::{rust::RustPackage, ts::TsPackage}, error::{AppError, IngestError}, ingest::{parsed_symbol::{Identity, PackageCoord, project}, sink::{OrchestratorSink, SinkId, SqliteRegisterSink, SymbolSink, TerminusSink, TextIndexSink, VectorSink, run_sinks}}, sync_progress::ProgressReporter, terminusdb::{Runner, termdb::{CrateInfo, DocCtx, DocStore}}, text_index::SymbolTextIndex};

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

pub async fn run_rust_pipeline(
	package: &RustPackage,
	version: &Version,
	workspace: &Path,
	config: &PipelineConfig,
	progress: &ProgressReporter,
	nudox_store: Option<&Arc<NudoxStore>>,
	text_index: Option<&Arc<SymbolTextIndex>>,
	orchestrator: Option<&Arc<nudox_orchestrator::Orchestrator>>,
) -> Result<IngestionSummary, AppError> {
	let pkg = package.clone();
	let ws = workspace.to_path_buf();
	let ver = version.clone();
	// IR generation and rustdoc JSON parsing are CPU/blocking-I/O; run off the async
	// executor. The source map for tree-sitter is built from the same parsed crate.
	let (ir, source_map) = spawn_blocking(move || -> Result<_, AppError> {
		pkg.generate_ir_with_sources(&ws, &ver).map_err(|source| {
			AppError::Ingest(IngestError::IrGeneration { package: pkg.name.clone(), source })
		})
	})
	.await
	.map_err(|source| AppError::TaskJoin { action: "Rust IR generation", source })??;

	finalize_pipeline(
		nudox_core::Language::Rust,
		&package.name,
		version,
		ir.index().into_index(),
		config,
		progress,
		nudox_store,
		text_index,
		orchestrator,
		&source_map,
	)
	.await
}

pub async fn run_typescript_pipeline(
	package: &TsPackage,
	version: &Version,
	config: &PipelineConfig,
	progress: &ProgressReporter,
	nudox_store: Option<&Arc<NudoxStore>>,
	text_index: Option<&Arc<SymbolTextIndex>>,
	orchestrator: Option<&Arc<nudox_orchestrator::Orchestrator>>,
) -> Result<IngestionSummary, AppError> {
	let pkg = package.clone();
	let ir = spawn_blocking(move || -> Result<_, AppError> {
		pkg.generate_ir().map_err(|source| AppError::Ingest(IngestError::TsIrGeneration {
			package: pkg.name.clone(),
			source,
		}))
	})
	.await
	.map_err(|source| AppError::TaskJoin { action: "TypeScript IR generation", source })??;

	finalize_pipeline(
		nudox_core::Language::TypeScript,
		&package.name,
		version,
		ir.index().into_index(),
		config,
		progress,
		nudox_store,
		text_index,
		orchestrator,
		&HashMap::new(),
	)
	.await
}

#[allow(clippy::too_many_arguments)]
async fn finalize_pipeline(
	language: nudox_core::Language,
	package_name: &str,
	version: &Version,
	index: Index,
	config: &PipelineConfig,
	progress: &ProgressReporter,
	nudox_store: Option<&Arc<NudoxStore>>,
	text_index: Option<&Arc<SymbolTextIndex>>,
	orchestrator: Option<&Arc<nudox_orchestrator::Orchestrator>>,
	source_map: &HashMap<String, String>,
) -> Result<IngestionSummary, AppError> {
	let coord = PackageCoord::new(language, package_name, &version.to_string());
	let terminus_instance = config.terminus.as_ref().map(|t| format!("{}/{}", t.org, t.db));
	let identity = match terminus_instance.as_deref() {
		Some(instance) => Identity::Deterministic { instance },
		None => Identity::Local,
	};
	let entry_count = index.entries_by_path.len();

	// The single parse-once projection. `project` borrows the index; the graph
	// emission then consumes it. Both walk the same in-memory IR — neither
	// re-parses the other's output.
	let symbols = project(&coord, &index, source_map, &identity);
	let doc_store = emit_store(language.as_str(), package_name, version, index);
	let document_count = doc_store.docs.len();

	// Assemble non-consuming sinks (these borrow `&[ParsedSymbol]`).
	let mut sinks: Vec<Box<dyn SymbolSink>> = Vec::new();
	if let Some(store) = nudox_store {
		sinks.push(Box::new(SqliteRegisterSink { store: Arc::clone(store) }));
	}
	if let Some(idx) = text_index {
		sinks.push(Box::new(TextIndexSink { index: Arc::clone(idx) }));
	}
	if let Some(orch) = orchestrator {
		sinks.push(Box::new(OrchestratorSink {
			orchestrator:      Arc::clone(orch),
			terminus_instance: terminus_instance.clone(),
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
		let collection = collection_name(qdrant, language.as_str(), package_name, version);
		VectorSink { settings: qdrant.clone(), model: config.embedding_model.clone(), collection }
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

fn emit_store(language: &str, package_name: &str, version: &Version, index: Index) -> DocStore {
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

fn collection_name(
	settings: &QdrantSettings,
	language: &str,
	package_name: &str,
	version: &Version,
) -> String {
	format!(
		"{}_{}_{}_{}",
		sanitize_collection_segment(&settings.collection_prefix),
		sanitize_collection_segment(language),
		sanitize_collection_segment(package_name),
		sanitize_collection_segment(&version.to_string()),
	)
}

fn sanitize_collection_segment(value: &str) -> String { crate::util::slug::ascii_segment(value) }
