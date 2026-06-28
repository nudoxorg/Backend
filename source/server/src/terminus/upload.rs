use std::time::Duration;

use serde_json::Value;
use terminusdb_client::{BranchSpec, DocumentInsertArgs, TerminusDBHttpClient};
use tracing::{debug, info, instrument, warn};
use url::Url;

use super::schema::DocStore;
use crate::http::error::{AppError, TerminusError};
use crate::util::retry;

const DOCUMENT_UPLOAD_CHUNK_SIZE: usize = 100;
const DOCUMENT_UPLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const DOCUMENT_UPLOAD_MAX_ATTEMPTS: usize = 3;
const TERMINUS_CLIENT_MAX_ATTEMPTS: usize = 3;
const TERMINUS_RETRY_BASE_DELAY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy)]
pub struct DocumentUploadProgress {
	pub completed_chunks: usize,
	pub total_chunks:     usize,
	pub completed_docs:   usize,
	pub total_docs:       usize,
}

pub async fn terminus_client_with_retry(
	config: &TerminusConfig,
) -> Result<TerminusDBHttpClient, AppError> {
	retry::with_backoff(TERMINUS_CLIENT_MAX_ATTEMPTS, TERMINUS_RETRY_BASE_DELAY, |_| async {
		TerminusDBHttpClient::new_with_database(
			config.endpoint.clone(),
			&config.user,
			&config.password,
			&config.db,
			&config.org,
		)
		.await
		.map_err(|source| AppError::Terminus(TerminusError::ClientCreation {
			org: config.org.clone(),
			db: config.db.clone(),
			source,
		}))
	})
	.await
}

fn documents_in_dependency_order(store: &DocStore) -> Vec<Value> {
	let mut kinds = Vec::new();
	let mut entries = Vec::new();

	for (_uri, value) in store.documents_sorted() {
		match value.get("@type").and_then(|ty| ty.as_str()) {
			Some("Entry") => entries.push(value.clone()),
			_ => kinds.push(value.clone()),
		}
	}

	entries.sort_by(|left, right| {
		let left_depth =
			left.get("path").and_then(|value| value.as_array()).map_or(0, |segments| segments.len());
		let right_depth =
			right.get("path").and_then(|value| value.as_array()).map_or(0, |segments| segments.len());
		right_depth.cmp(&left_depth).then_with(|| {
			left
				.get("@id")
				.and_then(|value| value.as_str())
				.cmp(&right.get("@id").and_then(|value| value.as_str()))
		})
	});

	kinds.extend(entries);
	kinds
}

/// Configuration for connecting to a TerminusDB instance.
#[derive(Clone, Debug)]
pub struct TerminusConfig {
	pub endpoint: Url,
	pub user:     String,
	pub password: String,
	pub org:      String,
	pub db:       String,
}

#[instrument(skip_all, fields(org = %config.org, db = %config.db))]
pub async fn upload_documents(
	config: &TerminusConfig,
	store: &DocStore,
	mut on_progress: impl FnMut(DocumentUploadProgress),
) -> Result<(), AppError> {
	let client = terminus_client_with_retry(config).await?;

	let documents = documents_in_dependency_order(store);
	if documents.is_empty() {
		warn!("no documents to upload");
		return Ok(());
	}

	info!(count = documents.len(), "uploading documents");

	let mut inserted = 0usize;
	let mut updated = 0usize;
	let chunk_count = documents.len().div_ceil(DOCUMENT_UPLOAD_CHUNK_SIZE);
	let total_docs = documents.len();
	on_progress(DocumentUploadProgress {
		completed_chunks: 0,
		total_chunks: chunk_count,
		completed_docs: 0,
		total_docs,
	});

	for (chunk_index, chunk) in documents.chunks(DOCUMENT_UPLOAD_CHUNK_SIZE).enumerate() {
		info!(
			chunk_index = chunk_index + 1,
			chunk_count,
			chunk_size = chunk.len(),
			"uploading document chunk"
		);

		let spec = BranchSpec::new(&config.db);
		let args = DocumentInsertArgs {
			spec,
			author: "nudox-compiler".to_string(),
			message: "automated upload from compiler pipeline".to_string(),
			skip_existence_check: true,
			..Default::default()
		}
		.with_timeout(DOCUMENT_UPLOAD_TIMEOUT);

		let doc_refs: Vec<&Value> = chunk.iter().collect();
		let result = retry::with_backoff(
			DOCUMENT_UPLOAD_MAX_ATTEMPTS,
			Duration::from_secs(2),
			|_| async {
				client
					.insert_documents(doc_refs.clone(), args.clone())
					.await
					.map_err(|source| AppError::Terminus(TerminusError::DocumentUpload { source }))
			},
		)
		.await
		.inspect_err(|e| {
			warn!(
				chunk_index = chunk_index + 1,
				chunk_count,
				chunk_size = chunk.len(),
				error = %e,
				"document upload chunk failed permanently"
			);
		})?;

		if let Some(commit_id) = result.extract_commit_id() {
			info!(commit = %commit_id, chunk_index = chunk_index + 1, "upload chunk committed");
		}

		for res in result.values() {
			match res {
				terminusdb_client::TDBInsertInstanceResult::Inserted(_) => inserted += 1,
				terminusdb_client::TDBInsertInstanceResult::AlreadyExists(_) => updated += 1,
			}
		}
		on_progress(DocumentUploadProgress {
			completed_chunks: chunk_index + 1,
			total_chunks: chunk_count,
			completed_docs: inserted + updated,
			total_docs,
		});
	}
	debug!(inserted, updated, "upload result");

	Ok(())
}

#[instrument(skip_all, fields(org = %config.org, db = %config.db))]
pub async fn upload_schema(config: &TerminusConfig, schema_docs: Vec<Value>) -> Result<(), AppError> {
	let client = terminus_client_with_retry(config).await?;

	if schema_docs.is_empty() {
		warn!("no schema documents to upload");
		return Ok(());
	}

	info!(count = schema_docs.len(), "uploading schema");

	let spec = BranchSpec::new(&config.db);
	let args = DocumentInsertArgs {
		spec,
		author: "nudox-compiler".to_string(),
		message: "automated schema upload from compiler pipeline".to_string(),
		skip_existence_check: true,
		..Default::default()
	}
	.as_schema();

	let doc_refs: Vec<&Value> = schema_docs.iter().collect();
	let result = retry::with_backoff(
		DOCUMENT_UPLOAD_MAX_ATTEMPTS,
		Duration::from_secs(2),
		|_| async {
			client
				.insert_documents(doc_refs.clone(), args.clone())
				.await
				.map_err(|source| AppError::Terminus(TerminusError::SchemaUpload { source }))
		},
	)
	.await?;

	if let Some(commit_id) = result.extract_commit_id() {
		info!(commit = %commit_id, "schema upload committed");
	}

	Ok(())
}
