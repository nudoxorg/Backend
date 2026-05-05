use std::path::Path;

use color_eyre::eyre::WrapErr;
use ir::entry::Index;
use semver::Version;
use tokio::runtime::Handle;
use tracing::info;

use crate::{config::{PipelineConfig, QdrantSettings}, core::{rust::RustPackage, ts::TsPackage}, error::AppError, sync_progress::{PackageSyncPhase, ProgressReporter}, terminusdb::{Runner, embedding_service::{EmbeddingService, OpenAIEmbeddingProvider, PointIdFactory, QdrantPointFactory, embedding_documents_from_docstore}, qdrant_upload::{QdrantConfig, upload_points}, termdb::{CrateInfo, DocCtx, DocStore}, upload::{upload_documents, upload_schema}}};

#[derive(Debug, Clone)]
pub struct IngestionSummary {
	pub entry_count:    usize,
	pub document_count: usize,
	pub vector_count:   usize,
}

pub fn run_rust_pipeline(
	package: &RustPackage,
	version: &Version,
	workspace: &Path,
	config: &PipelineConfig,
	runtime: &Handle,
	progress: Option<&ProgressReporter>,
) -> Result<IngestionSummary, AppError> {
	let ir = package
		.generate_ir(&workspace.to_path_buf())
		.wrap_err_with(|| format!("IR generation failed for `{}`", package.name))?;

	finalize_pipeline(
		"rust",
		&package.name,
		version,
		ir.index().into_index(),
		config,
		runtime,
		progress,
	)
}

pub fn run_typescript_pipeline(
	package: &TsPackage,
	version: &Version,
	config: &PipelineConfig,
	runtime: &Handle,
	progress: Option<&ProgressReporter>,
) -> Result<IngestionSummary, AppError> {
	let ir = package
		.generate_ir()
		.wrap_err_with(|| format!("IR generation failed for `{}`", package.name))?;

	finalize_pipeline(
		"typescript",
		&package.name,
		version,
		ir.index().into_index(),
		config,
		runtime,
		progress,
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
) -> Result<IngestionSummary, AppError> {
	let entry_count = index.entries_by_path.len();
	let store = emit_store(language, package_name, version, index);
	let document_count = store.docs.len();
	let vector_count =
		runtime.block_on(upload_outputs(config, language, package_name, version, &store, progress))?;

	Ok(IngestionSummary { entry_count, document_count, vector_count })
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
		upload_documents(terminus, store).await?;
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
		.embed_documents(embedding_docs)
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
