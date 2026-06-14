use std::path::Path;
use std::sync::Arc;

use ir::entry::Index;
use nudox_core::{
    BLOB_SCHEMA_VERSION, BlobInfo, ByteSpan, ChunkMetadata, Language as NudoxLanguage, LibRef,
    OccurrenceId, RepoId, SourceChunk, SymbolKind, SymbolOrigin, TreesitterRepr,
};
use nudox_store::NudoxStore;
use semver::Version;
use tokio::runtime::Handle;
use tracing::{info, warn};

use crate::{config::{PipelineConfig, QdrantSettings}, core::{rust::RustPackage, ts::TsPackage}, error::AppError, sync_progress::{PackageSyncPhase, ProgressReporter}, terminusdb::{Runner, embedding_service::{EmbeddingProgress, EmbeddingService, EmbeddingDocument, OpenAIEmbeddingProvider, PointIdFactory, QdrantPointFactory, embedding_documents_from_docstore}, qdrant_upload::{QdrantConfig, upload_points}, termdb::{CrateInfo, DocCtx, DocStore}, upload::{DocumentUploadProgress, upload_documents, upload_schema}}, text_index::SymbolTextIndex};

#[derive(Debug, Clone)]
pub struct IngestionSummary {
	pub entry_count:    usize,
	pub document_count: usize,
	pub vector_count:   usize,
	/// Symbols registered in the SQLite occurrence store (0 if no store is wired).
	pub symbols_registered: usize,
	/// Symbols indexed in the local text search index (0 if no index is wired).
	pub symbols_text_indexed: usize,
	/// Symbols fed into the nudox symbol-search Orchestrator (0 if not wired).
	pub symbols_orchestrated: usize,
}

pub fn run_rust_pipeline(
	package: &RustPackage,
	version: &Version,
	workspace: &Path,
	config: &PipelineConfig,
	runtime: &Handle,
	progress: Option<&ProgressReporter>,
	nudox_store: Option<&Arc<NudoxStore>>,
	text_index: Option<&Arc<SymbolTextIndex>>,
	orchestrator: Option<&Arc<nudox_orchestrator::Orchestrator>>,
) -> Result<IngestionSummary, AppError> {
	let ir = package.generate_ir(&workspace.to_path_buf(), version).map_err(|source| {
		AppError::Internal { message: format!("IR generation failed for `{}`: {source}", package.name) }
	})?;

	finalize_pipeline(
		"rust",
		&package.name,
		version,
		ir.index().into_index(),
		config,
		runtime,
		progress,
		nudox_store,
		text_index,
		orchestrator,
	)
}

pub fn run_typescript_pipeline(
	package: &TsPackage,
	version: &Version,
	config: &PipelineConfig,
	runtime: &Handle,
	progress: Option<&ProgressReporter>,
	nudox_store: Option<&Arc<NudoxStore>>,
	text_index: Option<&Arc<SymbolTextIndex>>,
	orchestrator: Option<&Arc<nudox_orchestrator::Orchestrator>>,
) -> Result<IngestionSummary, AppError> {
	let ir = package.generate_ir().map_err(|source| AppError::Internal {
		message: format!("IR generation failed for `{}`: {source}", package.name),
	})?;

	finalize_pipeline(
		"typescript",
		&package.name,
		version,
		ir.index().into_index(),
		config,
		runtime,
		progress,
		nudox_store,
		text_index,
		orchestrator,
	)
}

fn finalize_pipeline(
	language: &str,
	package_name: &str,
	version: &Version,
	index: Index,
	config: &PipelineConfig,
	runtime: &Handle,
	progress: Option<&ProgressReporter>,
	nudox_store: Option<&Arc<NudoxStore>>,
	text_index: Option<&Arc<SymbolTextIndex>>,
	orchestrator: Option<&Arc<nudox_orchestrator::Orchestrator>>,
) -> Result<IngestionSummary, AppError> {
	let entry_count = index.entries_by_path.len();
	let doc_store = emit_store(language, package_name, version, index);
	let document_count = doc_store.docs.len();

	// Register all Entry/* symbols in the SQLite occurrence store so that
	// any deferred occurrence blobs waiting on this library can be resolved.
	let symbols_registered = if let Some(store) = nudox_store {
		let entry_uris: Vec<&str> = doc_store
			.docs
			.keys()
			.filter(|k| k.starts_with("Entry/"))
			.map(|k| k.as_str())
			.collect();
		match runtime.block_on(store.register_library(
			language,
			package_name,
			&version.to_string(),
			entry_uris.iter().copied(),
		)) {
			Ok(n) => n,
			Err(e) => {
				// Non-fatal: log and continue. The occurrence system will simply
				// leave blobs deferred until the next successful ingestion.
				warn!(error = %e, lib = package_name, "failed to register symbols in occurrence store");
				0
			}
		}
	} else {
		0
	};

	// Index symbols into the local Tantivy text search index.
	let symbols_text_indexed = if let Some(idx) = text_index {
		match embedding_documents_from_docstore(&doc_store, language, package_name, Some(&version.to_string())) {
			Ok(docs) => match idx.index_batch(&docs) {
				Ok(n) => {
					info!(lib = package_name, count = n, "symbols indexed in text search");
					n
				}
				Err(e) => {
					warn!(error = %e, lib = package_name, "failed to index symbols in text search");
					0
				}
			},
			Err(e) => {
				warn!(error = %e, lib = package_name, "failed to extract docs for text search");
				0
			}
		}
	} else {
		0
	};

	// Feed each symbol into the nudox-search Orchestrator so /symbol-search works.
	// Only Rust packages are bridged for now; nudox_core::Language only has Rust.
	let symbols_orchestrated = if let Some(orch) = orchestrator {
		if language == "rust" {
			match embedding_documents_from_docstore(
				&doc_store,
				language,
				package_name,
				Some(&version.to_string()),
			) {
				Ok(docs) => {
					let mut count = 0usize;
					for doc in &docs {
						let blob = embedding_doc_to_blob_info(doc, package_name, version);
						match runtime.block_on(orch.ingest(blob)) {
							Ok(_) => count += 1,
							Err(e) => {
								warn!(
									error = %e,
									lib = package_name,
									symbol = %doc.fq_name.as_deref().unwrap_or(&doc.uri),
									"orchestrator ingest failed for symbol"
								);
							}
						}
					}
					info!(lib = package_name, count, "symbols fed to symbol-search orchestrator");
					count
				}
				Err(e) => {
					warn!(error = %e, lib = package_name, "failed to extract docs for orchestrator ingest");
					0
				}
			}
		} else {
			0
		}
	} else {
		0
	};

	let vector_count =
		runtime.block_on(upload_outputs(config, language, package_name, version, &doc_store, progress))?;

	Ok(IngestionSummary {
		entry_count,
		document_count,
		vector_count,
		symbols_registered,
		symbols_text_indexed,
		symbols_orchestrated,
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
		CrateInfo::new(language.to_owned(), package_name.to_owned(), version.to_string()),
		context_object,
	));
	runner.run(index.entries_by_path.into_values());
	let store = runner.into_docs();
	info!(documents = store.docs.len(), package = package_name, version = %version, "emission complete");
	store
}

async fn upload_outputs(
	config: &PipelineConfig,
	language: &str,
	package_name: &str,
	version: &Version,
	store: &DocStore,
	progress: Option<&ProgressReporter>,
) -> Result<usize, AppError> {
	if let Some(terminus) = &config.terminus {
		if config.upload_schema {
			if let Some(progress) = progress {
				progress.phase_with_detail(
					PackageSyncPhase::UploadingSchema,
					Some("uploading TerminusDB schema".to_owned()),
				);
			}
			let schema_json: Vec<serde_json::Value> =
				serde_json::from_str(include_str!("../schema.json"))?;
			upload_schema(terminus, schema_json).await?;
		}
		if let Some(progress) = progress {
			progress.phase_with_detail(
				PackageSyncPhase::UploadingDocuments,
				Some(format!("uploading {} documents", store.docs.len())),
			);
		}
		let progress_callback = |upload_progress: DocumentUploadProgress| {
			if let Some(progress) = progress {
				progress.phase_with_detail(
					PackageSyncPhase::UploadingDocuments,
					Some(format!(
						"uploaded {}/{} documents (chunk {}/{})",
						upload_progress.completed_docs,
						upload_progress.total_docs,
						upload_progress.completed_chunks,
						upload_progress.total_chunks,
					)),
				);
			}
		};
		upload_documents(terminus, store, progress_callback).await?;
	}

	match &config.qdrant {
		Some(qdrant) => {
			if let Some(progress) = progress {
				progress.phase_with_detail(
					PackageSyncPhase::Embedding,
					Some(format!("embedding {} symbols", store.docs.len() / 2)),
				);
			}
			upload_embeddings(
				qdrant,
				&config.embedding_model,
				language,
				package_name,
				version,
				store,
				progress,
			)
			.await
		}
		None => Ok(0),
	}
}

async fn upload_embeddings(
	settings: &QdrantSettings,
	model_name: &str,
	language: &str,
	package_name: &str,
	version: &Version,
	store: &DocStore,
	progress: Option<&ProgressReporter>,
) -> Result<usize, AppError> {
	let version_string = version.to_string();
	let embedding_docs =
		embedding_documents_from_docstore(store, language, package_name, Some(version_string.as_str()))
			.map_err(|source| AppError::Embedding(source.to_string()))?;
	info!(
		package = package_name,
		version = %version,
		count = embedding_docs.len(),
		"preparing embeddings"
	);

	let embedding_provider = OpenAIEmbeddingProvider::new(model_name);
	let embedding_service = EmbeddingService::new(embedding_provider);
	let embedded_records = embedding_service
		.embed_documents(embedding_docs, |embedding_progress: EmbeddingProgress| {
			if let Some(progress) = progress {
				progress.phase_with_detail(
					PackageSyncPhase::Embedding,
					Some(format!(
						"embedded {}/{} symbols",
						embedding_progress.completed, embedding_progress.total
					)),
				);
			}
		})
		.await
		.map_err(|source| AppError::Embedding(source.to_string()))?;

	let vector_count = embedded_records.len();
	if let Some(progress) = progress {
		progress.phase_with_detail(
			PackageSyncPhase::UploadingVectors,
			Some(format!("uploading {} vectors", vector_count)),
		);
	}
	let mut points = Vec::with_capacity(embedded_records.len());
	for (index, record) in embedded_records.into_iter().enumerate() {
		let point_id = PointIdFactory::from_u64(index as u64 + 1);
		let point = QdrantPointFactory::build_point(point_id, record)
			.map_err(|source| AppError::Embedding(source.to_string()))?;
		points.push(point);
	}

	let qdrant_config = QdrantConfig {
		endpoint:        settings.endpoint.clone(),
		collection_name: collection_name(settings, language, package_name, version),
		vector_size:     settings.vector_size,
		distance:        settings.distance,
	};
	upload_points(&qdrant_config, points).await?;
	info!(
		package = package_name,
		version = %version,
		count = vector_count,
		"vector upload complete"
	);

	Ok(vector_count)
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

fn sanitize_collection_segment(value: &str) -> String {
	value
		.chars()
		.map(|ch| match ch {
			'a'..='z' | '0'..='9' => ch,
			'A'..='Z' => ch.to_ascii_lowercase(),
			_ => '_',
		})
		.collect()
}

/// Convert an `EmbeddingDocument` (extracted from the TerminusDB doc store) into
/// a `BlobInfo` suitable for `Orchestrator::ingest`. No embeddings are stored here;
/// body_query search returns 501 at the HTTP layer until embeddings are wired.
fn embedding_doc_to_blob_info(doc: &EmbeddingDocument, package_name: &str, version: &Version) -> BlobInfo {
	let symbol_name = doc
		.fq_name
		.clone()
		.unwrap_or_else(|| doc.uri.split('/').next_back().unwrap_or(&doc.uri).to_owned());
	let kind = doc.symbol_kind.as_deref().map(str_to_symbol_kind);
	// Use Repo origin so these symbols are indexed immediately — they represent
	// the library's own exported surface, not occurrences of external usage.
	let repo_id = RepoId(format!("lib:{package_name}:{}", version));
	BlobInfo {
		occurrence_id:      OccurrenceId(uuid::Uuid::new_v4()),
		symbol_name,
		symbol_origin:      SymbolOrigin::Repo { repo_id: repo_id.clone() },
		resolved_global_id: None,
		kind,
		source: SourceChunk {
			raw_code:         doc.text.clone(),
			treesitter_repr:  TreesitterRepr(vec![]),
			symbol_span:      ByteSpan { start: 0, end: doc.text.len() },
		},
		embeddings:         vec![],
		metadata:           ChunkMetadata {
			repo_id,
			file_path:            std::path::PathBuf::from(&doc.uri),
			file_span:            ByteSpan { start: 0, end: 0 },
			parsed_at:            chrono::Utc::now(),
			lang:                 NudoxLanguage::Rust,
			lang_version:         None,
			blob_schema_version:  BLOB_SCHEMA_VERSION,
		},
	}
}

fn str_to_symbol_kind(s: &str) -> SymbolKind {
	match s.to_lowercase().as_str() {
		"function" | "fn"   => SymbolKind::Function,
		"struct"             => SymbolKind::Struct,
		"enum"               => SymbolKind::Enum,
		"trait"              => SymbolKind::Trait,
		"method"             => SymbolKind::Method,
		"closure"            => SymbolKind::Closure,
		"typealias" | "type" => SymbolKind::TypeAlias,
		"const" | "static"   => SymbolKind::Const,
		_                    => SymbolKind::Other,
	}
}
