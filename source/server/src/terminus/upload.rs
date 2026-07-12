use std::collections::HashSet;
use std::time::Duration;

use serde_json::Value;
use terminusdb_client::{BranchSpec, DocumentInsertArgs, TerminusDBHttpClient};
use tracing::{debug, info, instrument, warn};
use url::Url;

use document::schema::{DocSink, EmitError, URI};
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

/// One emitted JSON-LD document, serialized once and ready to upload.
///
/// The `Value` tree is discarded the moment a document is produced: only its
/// compact JSON bytes plus the few fields needed to order it for upload are
/// retained. A corpus of these is several times smaller than the equivalent
/// corpus of live `serde_json::Value` trees, and only one *chunk* is ever
/// re-materialized into `Value`s at a time (at upload).
struct PreparedDoc {
	/// `true` for `@type: "Entry"` documents, `false` for Kind documents.
	is_entry: bool,
	/// Path-segment depth (entries only; 0 for kinds, which carry no `path`).
	depth:    usize,
	/// The document `@id`/URI — the upload-order tiebreak and dedup key.
	id:       URI,
	/// The document serialized exactly once to JSON bytes (`@context` included).
	bytes:    Box<[u8]>,
}

/// A streaming [`DocSink`]: serializes each finished document to compact bytes
/// and drops the `Value`, deduplicating by URI (first write wins, matching
/// [`DocStore`](document::schema::DocStore)). The whole corpus is therefore held
/// as bytes, never as live `Value` trees.
#[derive(Default)]
pub struct StreamingDocSink {
	docs: Vec<PreparedDoc>,
	seen: HashSet<URI>,
}

impl StreamingDocSink {
	pub fn new() -> Self { Self::default() }

	/// Finalize into an upload-ready, dependency-ordered corpus.
	pub fn into_corpus(mut self) -> PreparedCorpus {
		// Identical ordering to the former `documents_in_dependency_order`:
		// all Kinds first, then Entries deepest-path-first; `@id` ascending breaks
		// ties (and is the total order among Kinds).
		self.docs.sort_by(|a, b| {
			a.is_entry
				.cmp(&b.is_entry)
				.then_with(|| b.depth.cmp(&a.depth))
				.then_with(|| a.id.cmp(&b.id))
		});
		PreparedCorpus { docs: self.docs }
	}
}

impl DocSink for StreamingDocSink {
	fn accept(&mut self, uri: URI, doc: Value) -> Result<(), EmitError> {
		// First write wins, matching the `DocStore` BTreeMap-key semantics; the
		// streaming sink dedups silently rather than erroring on a repeat URI.
		if !self.seen.insert(uri.clone()) {
			return Ok(());
		}
		let is_entry = doc.get("@type").and_then(|ty| ty.as_str()) == Some("Entry");
		let depth =
			doc.get("path").and_then(|value| value.as_array()).map_or(0, |segments| segments.len());
		let bytes = serde_json::to_vec(&doc)
			.map_err(|_| EmitError::SerializationFailed { uri: uri.clone() })?;
		self.docs.push(PreparedDoc { is_entry, depth, id: uri, bytes: bytes.into_boxed_slice() });
		Ok(())
	}
}

/// A dependency-ordered corpus of serialized documents, ready to stream to
/// TerminusDB. Only a single chunk is parsed back into `Value`s at upload time.
pub struct PreparedCorpus {
	docs: Vec<PreparedDoc>,
}

impl PreparedCorpus {
	pub fn len(&self) -> usize { self.docs.len() }

	pub fn is_empty(&self) -> bool { self.docs.is_empty() }
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
pub async fn upload_prepared_documents(
	config: &TerminusConfig,
	corpus: &PreparedCorpus,
	mut on_progress: impl FnMut(DocumentUploadProgress),
) -> Result<(), AppError> {
	let client = terminus_client_with_retry(config).await?;

	if corpus.is_empty() {
		warn!("no documents to upload");
		return Ok(());
	}

	let total_docs = corpus.len();
	info!(count = total_docs, "uploading documents");

	let mut inserted = 0usize;
	let mut updated = 0usize;
	let chunk_count = total_docs.div_ceil(DOCUMENT_UPLOAD_CHUNK_SIZE);
	on_progress(DocumentUploadProgress {
		completed_chunks: 0,
		total_chunks: chunk_count,
		completed_docs: 0,
		total_docs,
	});

	for (chunk_index, chunk) in corpus.docs.chunks(DOCUMENT_UPLOAD_CHUNK_SIZE).enumerate() {
		info!(
			chunk_index = chunk_index + 1,
			chunk_count,
			chunk_size = chunk.len(),
			"uploading document chunk"
		);

		// Re-materialize only this chunk's documents into `Value`s — the client
		// wants `&Value`s, but at most `DOCUMENT_UPLOAD_CHUNK_SIZE` are ever live.
		let chunk_values: Vec<Value> = chunk
			.iter()
			.filter_map(|doc| match serde_json::from_slice::<Value>(&doc.bytes) {
				Ok(value) => Some(value),
				Err(error) => {
					warn!(id = %doc.id, %error, "skipping document with unparseable cached JSON");
					None
				}
			})
			.collect();

		let spec = BranchSpec::new(&config.db);
		let args = DocumentInsertArgs {
			spec,
			author: "nudox-compiler".to_string(),
			message: "automated upload from compiler pipeline".to_string(),
			skip_existence_check: true,
			..Default::default()
		}
		.with_timeout(DOCUMENT_UPLOAD_TIMEOUT);

		let doc_refs: Vec<&Value> = chunk_values.iter().collect();
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

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::*;

	fn ordered_ids(corpus: &PreparedCorpus) -> Vec<&str> {
		corpus.docs.iter().map(|d| d.id.as_str()).collect()
	}

	/// Build a `DocumentUri` from an arbitrary string via its transparent serde
	/// repr (its only public constructors are `From<EntryUri>`/`From<KindUri>`).
	fn uri(s: &str) -> URI {
		serde_json::from_value(Value::String(s.to_owned())).unwrap()
	}

	/// The streamed corpus must reproduce the exact upload order the former
	/// `documents_in_dependency_order` produced from a `DocStore`: all Kinds
	/// first (`@id` ascending), then Entries deepest-path-first, `@id` ascending
	/// breaking ties.
	#[test]
	fn streaming_sink_preserves_dependency_upload_order() {
		let mut sink = StreamingDocSink::new();
		// Intentionally accepted out of final order.
		sink.accept(uri("Entry/a"), json!({"@type": "Entry", "path": ["a"]})).unwrap();
		sink.accept(uri("Kind/z"), json!({"@type": "Function"})).unwrap();
		sink.accept(uri("Entry/a/n"), json!({"@type": "Entry", "path": ["a", "n"]})).unwrap();
		sink.accept(uri("Entry/a/b/c"), json!({"@type": "Entry", "path": ["a", "b", "c"]})).unwrap();
		sink.accept(uri("Kind/a"), json!({"@type": "RecordType"})).unwrap();
		sink.accept(uri("Entry/a/m"), json!({"@type": "Entry", "path": ["a", "m"]})).unwrap();

		let corpus = sink.into_corpus();
		assert_eq!(ordered_ids(&corpus), vec![
			// Kinds first, @id ascending.
			"Kind/a",
			"Kind/z",
			// Entries deepest first; equal depth → @id ascending.
			"Entry/a/b/c",
			"Entry/a/m",
			"Entry/a/n",
			"Entry/a",
		]);
	}

	/// Matches `DocStore`'s BTreeMap-key semantics: the first write at a URI wins
	/// and later writes are dropped, so a URI is never uploaded twice.
	#[test]
	fn streaming_sink_dedups_by_uri_first_write_wins() {
		let mut sink = StreamingDocSink::new();
		sink.accept(uri("Entry/x"), json!({"@type": "Entry", "path": ["x"], "v": 1})).unwrap();
		sink.accept(uri("Entry/x"), json!({"@type": "Entry", "path": ["x"], "v": 2})).unwrap();

		let corpus = sink.into_corpus();
		assert_eq!(corpus.len(), 1);
		let value: Value = serde_json::from_slice(&corpus.docs[0].bytes).unwrap();
		assert_eq!(value["v"], json!(1), "first write must be retained");
	}
}
