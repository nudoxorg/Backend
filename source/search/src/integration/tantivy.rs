//! Real tantivy-backed search index. Implements the v1 schema from the design
//! doc.

use std::path::Path;

use async_trait::async_trait;
use nudox_core::{BlobInfo, BlobRef, GlobalSymbolId, OccurrenceId, Result, SearchError, SearchHit, SearchIndex, SearchQuery, SymbolOrigin};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term, directory::MmapDirectory, schema::{Field, STORED, STRING, Schema, TEXT, Value as TantivyValue}};
use tokio::sync::{mpsc, oneshot};

/// How many queued writes are coalesced into a single `commit()` at most.
///
/// The owner loop always drains everything currently pending (so bursts batch
/// naturally) but caps a single commit at this many documents to bound latency.
const MAX_BATCH: usize = 512;

/// A unit of work sent to the single-owner writer task.
enum WriterCmd {
	/// Upsert one document (delete the previous occurrence, then add).
	Index { doc: Box<TantivyDocument>, prev: Term, ack: oneshot::Sender<Result<()>> },
	/// Upsert many documents under a single commit.
	IndexMany { docs: Vec<(Term, TantivyDocument)>, ack: oneshot::Sender<Result<()>> },
}

/// Tantivy-backed full-text / exact search index.
///
/// `index()` has upsert semantics: it deletes any existing document with the
/// same `occurrence_id` before adding the new one, so re-indexing a blob after
/// deferred resolution doesn't produce duplicate entries.
///
/// The `IndexWriter` is owned by a single background thread fed over an mpsc
/// channel. Writes are coalesced and committed in batches off the async
/// reactor; after each commit the shared [`IndexReader`] is reloaded so queries
/// observe the new documents. The reader is built once at open and reused for
/// every query.
pub struct TantivySearchIndex {
	index:  Index,
	reader: IndexReader,
	tx:     mpsc::UnboundedSender<WriterCmd>,
	fields: Fields,
}

#[derive(Clone)]
pub(crate) struct Fields {
	pub occurrence_id: Field,
	pub global_id:     Field,
	pub symbol_name:   Field,
	pub lib_name:      Field,
	pub lib_version:   Field,
	pub repo_id:       Field,
	pub blob_ref:      Field,
}

fn build_schema() -> (Schema, Fields) {
	let mut b = Schema::builder();
	let occurrence_id = b.add_text_field("occurrence_id", STRING | STORED);
	let global_id = b.add_text_field("global_id", STRING | STORED);
	let symbol_name = b.add_text_field("symbol_name", TEXT | STORED);
	let lib_name = b.add_text_field("lib_name", TEXT | STORED);
	let lib_version = b.add_text_field("lib_version", STORED);
	let repo_id = b.add_text_field("repo_id", STRING | STORED);
	let blob_ref = b.add_text_field("blob_ref", STORED);
	let schema = b.build();
	(schema, Fields {
		occurrence_id,
		global_id,
		symbol_name,
		lib_name,
		lib_version,
		repo_id,
		blob_ref,
	})
}

impl TantivySearchIndex {
	/// Open an existing tantivy index at `index_dir` or create one if absent.
	pub fn open_or_create(index_dir: &Path) -> Result<Self> {
		std::fs::create_dir_all(index_dir).map_err(|e| SearchError::CreateDir(Box::new(e)))?;
		let (schema, fields) = build_schema();
		let dir =
			MmapDirectory::open(index_dir).map_err(|e| SearchError::OpenDirectory(Box::new(e)))?;
		let index = Index::open_or_create(dir, schema)
			.map_err(|e| SearchError::OpenIndex(Box::new(e)))?;
		let writer = index.writer(50_000_000).map_err(|e| SearchError::CreateWriter(Box::new(e)))?;

		// Build the reader once. We drive reloads explicitly from the owner loop
		// after each commit, so use the manual reload policy.
		let reader = index
			.reader_builder()
			.reload_policy(ReloadPolicy::Manual)
			.try_into()
			.map_err(|e| SearchError::OpenReader(Box::new(e)))?;

		// Single-owner writer task: the only place `IndexWriter` is touched. It
		// runs the blocking add/commit off the async reactor on a dedicated
		// thread and reloads the shared reader after each commit.
		let (tx, rx) = mpsc::unbounded_channel::<WriterCmd>();
		let reader_for_owner = reader.clone();
		std::thread::Builder::new()
			.name("tantivy-writer".to_owned())
			.spawn(move || run_writer(writer, reader_for_owner, rx))
			.map_err(|e| SearchError::CreateWriter(Box::new(e)))?;

		Ok(Self { index, reader, tx, fields })
	}

	/// Index many blobs under a single commit.
	///
	/// Equivalent to calling [`SearchIndex::index`] once per blob but with one
	/// coalesced commit and a single reader reload, so it is far cheaper for
	/// bulk ingestion.
	pub async fn index_many(&self, blobs: &[(BlobRef, BlobInfo)]) -> Result<()> {
		if blobs.is_empty() {
			return Ok(());
		}
		let docs = blobs
			.iter()
			.map(|(blob_ref, info)| {
				let prev = Term::from_field_text(
					self.fields.occurrence_id,
					&info.occurrence_id.to_string(),
				);
				(prev, self.build_doc(blob_ref, info))
			})
			.collect();
		let (ack, ack_rx) = oneshot::channel();
		self.tx
			.send(WriterCmd::IndexMany { docs, ack })
			.map_err(|_| SearchError::LockPoisoned)?;
		ack_rx.await.map_err(|_| SearchError::LockPoisoned)?
	}

	/// Build the tantivy document for one blob (shared by `index`/`index_many`).
	fn build_doc(&self, blob_ref: &BlobRef, info: &BlobInfo) -> TantivyDocument {
		let (lib_name, lib_version, repo_id) = match &info.symbol_origin {
			SymbolOrigin::Repo { repo_id } => (String::new(), String::new(), repo_id.0.clone()),
			SymbolOrigin::ExternalLib { lib } => (lib.name.clone(), lib.version.clone(), String::new()),
		};
		let global_id_str = info.resolved_global_id.map(|g| g.to_string()).unwrap_or_default();

		let mut doc = TantivyDocument::new();
		doc.add_text(self.fields.occurrence_id, info.occurrence_id.to_string());
		doc.add_text(self.fields.global_id, global_id_str);
		doc.add_text(self.fields.symbol_name, &info.symbol_name);
		doc.add_text(self.fields.lib_name, lib_name);
		doc.add_text(self.fields.lib_version, lib_version);
		doc.add_text(self.fields.repo_id, repo_id);
		doc.add_text(self.fields.blob_ref, &blob_ref.0);
		doc
	}

	/// Open a reader for querying. Callers do not normally need this directly —
	/// use [`SearchQuery`] instead.
	pub fn reader(&self) -> tantivy::Result<tantivy::IndexReader> { self.index.reader() }

	#[cfg(test)]
	pub(crate) fn index_handle(&self) -> &Index { &self.index }

	#[cfg(test)]
	pub(crate) fn fields(&self) -> &Fields { &self.fields }
}

/// Owner loop for the single `IndexWriter`. Runs on a dedicated OS thread.
///
/// Each iteration pops one command (blocking), then drains everything else
/// currently pending so a burst of writes coalesces into one `commit()`. After
/// committing it reloads the shared reader, then acks every waiter.
fn run_writer(mut writer: IndexWriter, reader: IndexReader, mut rx: mpsc::UnboundedReceiver<WriterCmd>) {
	while let Some(first) = rx.blocking_recv() {
		// Per-waiter ack channel paired with the result of *adding* its docs.
		let mut acks: Vec<(oneshot::Sender<Result<()>>, std::result::Result<(), String>)> =
			Vec::new();
		let mut pending = 0usize;

		let mut cmd = Some(first);
		loop {
			match cmd.take() {
				Some(WriterCmd::Index { doc, prev, ack }) => {
					writer.delete_term(prev);
					let r = writer.add_document(*doc).map(|_| ()).map_err(|e| e.to_string());
					pending += 1;
					acks.push((ack, r));
				}
				Some(WriterCmd::IndexMany { docs, ack }) => {
					let mut r = Ok(());
					for (prev, doc) in docs {
						writer.delete_term(prev);
						pending += 1;
						if let Err(e) = writer.add_document(doc) {
							r = Err(e.to_string());
							break;
						}
					}
					acks.push((ack, r));
				}
				None => {}
			}

			if pending >= MAX_BATCH {
				break;
			}
			match rx.try_recv() {
				Ok(next) => cmd = Some(next),
				Err(_) => break,
			}
		}

		let commit_res = writer.commit().map(|_| ()).map_err(|e| e.to_string());
		let reload_res = if commit_res.is_ok() {
			reader.reload().map_err(|e| e.to_string())
		} else {
			Ok(())
		};

		for (ack, add_res) in acks {
			let final_res: Result<()> = match (add_res, &commit_res, &reload_res) {
				(Err(e), _, _) => Err(SearchError::AddDocument(e.into()).into()),
				(_, Err(e), _) => Err(SearchError::Commit(e.clone().into()).into()),
				(_, _, Err(e)) => Err(SearchError::OpenReader(e.clone().into()).into()),
				_ => Ok(()),
			};
			let _ = ack.send(final_res);
		}
	}
}

#[async_trait]
impl SearchIndex for TantivySearchIndex {
	#[tracing::instrument(skip(self, info), fields(blob_ref = %blob_ref, occurrence_id = %info.occurrence_id))]
	async fn index(&self, blob_ref: &BlobRef, info: &BlobInfo) -> Result<()> {
		// Delete any previous entry for this occurrence so re-indexing after
		// deferred resolution doesn't produce duplicate documents.
		let prev = Term::from_field_text(self.fields.occurrence_id, &info.occurrence_id.to_string());
		let doc = Box::new(self.build_doc(blob_ref, info));

		let (ack, ack_rx) = oneshot::channel();
		self.tx
			.send(WriterCmd::Index { doc, prev, ack })
			.map_err(|_| SearchError::LockPoisoned)?;
		ack_rx.await.map_err(|_| SearchError::LockPoisoned)?
	}

	/// Batch-index many blobs with a single `commit()` for the whole batch,
	/// instead of one fsync+segment-seal per document. The writer lock is taken
	/// once. Upsert semantics are preserved per item (delete-by-occurrence first).
	#[tracing::instrument(skip(self, items), fields(count = items.len()))]
	async fn index_many(&self, items: &[(BlobRef, BlobInfo)]) -> Result<()> {
		if items.is_empty() {
			return Ok(());
		}

		// Build every document up front, then hand the whole batch to the single
		// writer task for one coalesced `commit()` (no fsync+segment-seal per doc).
		let docs: Vec<(Term, TantivyDocument)> = items
			.iter()
			.map(|(blob_ref, info)| {
				let prev =
					Term::from_field_text(self.fields.occurrence_id, &info.occurrence_id.to_string());
				(prev, self.build_doc(blob_ref, info))
			})
			.collect();

		let (ack, ack_rx) = oneshot::channel();
		self.tx
			.send(WriterCmd::IndexMany { docs, ack })
			.map_err(|_| SearchError::LockPoisoned)?;
		ack_rx.await.map_err(|_| SearchError::LockPoisoned)?
	}
}

#[async_trait]
impl SearchQuery for TantivySearchIndex {
	async fn search(&self, query_str: &str, limit: usize) -> Result<Vec<SearchHit>> {
		let searcher = self.reader.searcher();
		let qp = tantivy::query::QueryParser::for_index(&self.index, vec![
			self.fields.symbol_name,
			self.fields.lib_name,
			self.fields.repo_id,
		]);
		let query =
			qp.parse_query(query_str).map_err(|e| SearchError::ParseQuery(Box::new(e)))?;
		let top_docs = searcher
			.search(&query, &tantivy::collector::TopDocs::with_limit(limit))
			.map_err(|e| SearchError::Search(Box::new(e)))?;

		let mut hits = Vec::with_capacity(top_docs.len());
		for (score, addr) in top_docs {
			let doc: tantivy::TantivyDocument =
				searcher.doc(addr).map_err(|e| SearchError::FetchDocument(Box::new(e)))?;
			let blob_ref = BlobRef(
				doc.get_first(self.fields.blob_ref).and_then(|v| v.as_str()).unwrap_or("").to_string(),
			);
			let occ_str = doc.get_first(self.fields.occurrence_id).and_then(|v| v.as_str()).unwrap_or("");
			let occurrence_id =
				OccurrenceId(uuid::Uuid::parse_str(occ_str).unwrap_or_else(|_| uuid::Uuid::nil()));
			let symbol_name =
				doc.get_first(self.fields.symbol_name).and_then(|v| v.as_str()).unwrap_or("").to_string();
			hits.push(SearchHit { blob_ref, occurrence_id, symbol_name, score });
		}
		Ok(hits)
	}

	async fn find_by_global_id(
		&self,
		global_id: GlobalSymbolId,
		limit: usize,
	) -> Result<Vec<SearchHit>> {
		let searcher = self.reader.searcher();
		let term = tantivy::Term::from_field_text(self.fields.global_id, &global_id.to_string());
		let query = tantivy::query::TermQuery::new(term, tantivy::schema::IndexRecordOption::Basic);
		let top_docs = searcher
			.search(&query, &tantivy::collector::TopDocs::with_limit(limit))
			.map_err(|e| SearchError::Search(Box::new(e)))?;

		let mut hits = Vec::with_capacity(top_docs.len());
		for (score, addr) in top_docs {
			let doc: tantivy::TantivyDocument =
				searcher.doc(addr).map_err(|e| SearchError::FetchDocument(Box::new(e)))?;
			let blob_ref = BlobRef(
				doc.get_first(self.fields.blob_ref).and_then(|v| v.as_str()).unwrap_or("").to_string(),
			);
			let occ_str = doc.get_first(self.fields.occurrence_id).and_then(|v| v.as_str()).unwrap_or("");
			let occurrence_id =
				OccurrenceId(uuid::Uuid::parse_str(occ_str).unwrap_or_else(|_| uuid::Uuid::nil()));
			let symbol_name =
				doc.get_first(self.fields.symbol_name).and_then(|v| v.as_str()).unwrap_or("").to_string();
			hits.push(SearchHit { blob_ref, occurrence_id, symbol_name, score });
		}
		Ok(hits)
	}

	async fn list_all(&self, limit: usize) -> Result<Vec<SearchHit>> {
		let searcher = self.reader.searcher();
		let top_docs = searcher
			.search(&tantivy::query::AllQuery, &tantivy::collector::TopDocs::with_limit(limit))
			.map_err(|e| SearchError::Search(Box::new(e)))?;

		let mut hits = Vec::with_capacity(top_docs.len());
		for (score, addr) in top_docs {
			let doc: tantivy::TantivyDocument =
				searcher.doc(addr).map_err(|e| SearchError::FetchDocument(Box::new(e)))?;
			let blob_ref = BlobRef(
				doc.get_first(self.fields.blob_ref).and_then(|v| v.as_str()).unwrap_or("").to_string(),
			);
			let occ_str = doc.get_first(self.fields.occurrence_id).and_then(|v| v.as_str()).unwrap_or("");
			let occurrence_id =
				OccurrenceId(uuid::Uuid::parse_str(occ_str).unwrap_or_else(|_| uuid::Uuid::nil()));
			let symbol_name =
				doc.get_first(self.fields.symbol_name).and_then(|v| v.as_str()).unwrap_or("").to_string();
			hits.push(SearchHit { blob_ref, occurrence_id, symbol_name, score });
		}
		Ok(hits)
	}
}

#[cfg(test)]
mod tests {
	use nudox_core::{BLOB_SCHEMA_VERSION, ByteSpan, ChunkMetadata, GlobalSymbolId, Language, LibRef, OccurrenceId, RepoId, SourceChunk};
	use tantivy::{collector::TopDocs, query::QueryParser, schema::Value};
	use tempfile::TempDir;

	use super::*;

	fn blob_repo_local(repo: &str, symbol: &str) -> BlobInfo {
		BlobInfo {
			occurrence_id:      OccurrenceId(uuid::Uuid::new_v4()),
			symbol_name:        symbol.into(),
			symbol_origin:      SymbolOrigin::Repo { repo_id: RepoId(repo.into()) },
			resolved_global_id: None,
			kind:               None,
			source:             SourceChunk {
				raw_code:        "fn x() {}".into(),
				treesitter_repr: None,
				symbol_span:     ByteSpan { start: 0, end: 1 },
			},
			embeddings:         vec![],
			metadata:           ChunkMetadata {
				repo_id:             RepoId(repo.into()),
				file_path:           "src/lib.rs".into(),
				file_span:           ByteSpan { start: 0, end: 1 },
				parsed_at:           chrono::Utc::now(),
				lang:                Language::Rust,
				lang_version:        None,
				blob_schema_version: BLOB_SCHEMA_VERSION,
			},
		}
	}

	fn blob_for_lib(name: &str, symbol: &str) -> BlobInfo {
		BlobInfo {
			occurrence_id:      OccurrenceId(uuid::Uuid::new_v4()),
			symbol_name:        symbol.into(),
			symbol_origin:      SymbolOrigin::ExternalLib {
				lib: LibRef { name: name.into(), version: "1.0.0".into() },
			},
			resolved_global_id: None,
			kind:               None,
			source:             SourceChunk {
				raw_code:        format!("use {name}::{symbol};").into(),
				treesitter_repr: None,
				symbol_span:     ByteSpan { start: 0, end: 1 },
			},
			embeddings:         vec![],
			metadata:           ChunkMetadata {
				repo_id:             RepoId("r".into()),
				file_path:           "src/lib.rs".into(),
				file_span:           ByteSpan { start: 0, end: 1 },
				parsed_at:           chrono::Utc::now(),
				lang:                Language::Rust,
				lang_version:        None,
				blob_schema_version: BLOB_SCHEMA_VERSION,
			},
		}
	}

	#[tokio::test]
	async fn open_or_create_fresh_directory() {
		let tmp = TempDir::new().unwrap();
		let _ix = TantivySearchIndex::open_or_create(tmp.path()).unwrap();
	}

	#[tokio::test]
	async fn index_and_search_round_trip() {
		let tmp = TempDir::new().unwrap();
		let ix = TantivySearchIndex::open_or_create(tmp.path()).unwrap();
		let blob_ref = BlobRef("ref-1".into());
		let info = blob_for_lib("serde", "Serialize");
		ix.index(&blob_ref, &info).await.unwrap();

		let reader = ix.index_handle().reader().unwrap();
		let searcher = reader.searcher();
		let qp = QueryParser::for_index(ix.index_handle(), vec![ix.fields().symbol_name]);
		let query = qp.parse_query("Serialize").unwrap();
		let top_docs = searcher.search(&query, &TopDocs::with_limit(10)).unwrap();
		assert_eq!(top_docs.len(), 1);

		let doc: TantivyDocument = searcher.doc(top_docs[0].1).unwrap();
		let stored_ref = doc.get_first(ix.fields().blob_ref).and_then(|v| v.as_str()).unwrap();
		assert_eq!(stored_ref, "ref-1");
	}

	#[tokio::test]
	async fn search_filters_by_lib_name() {
		let tmp = TempDir::new().unwrap();
		let ix = TantivySearchIndex::open_or_create(tmp.path()).unwrap();
		ix.index(&BlobRef("a".into()), &blob_for_lib("serde", "X")).await.unwrap();
		ix.index(&BlobRef("b".into()), &blob_for_lib("tokio", "X")).await.unwrap();

		let reader = ix.index_handle().reader().unwrap();
		let searcher = reader.searcher();
		let qp = QueryParser::for_index(ix.index_handle(), vec![ix.fields().lib_name]);
		let query = qp.parse_query("serde").unwrap();
		let top_docs = searcher.search(&query, &TopDocs::with_limit(10)).unwrap();
		assert_eq!(top_docs.len(), 1);
	}

	#[tokio::test]
	async fn search_by_occurrence_id() {
		let tmp = TempDir::new().unwrap();
		let ix = TantivySearchIndex::open_or_create(tmp.path()).unwrap();
		let info = blob_for_lib("serde", "Serialize");
		let occ_id = info.occurrence_id.0.to_string();
		ix.index(&BlobRef("r".into()), &info).await.unwrap();

		let reader = ix.index_handle().reader().unwrap();
		let searcher = reader.searcher();
		let qp = QueryParser::for_index(ix.index_handle(), vec![ix.fields().occurrence_id]);
		let query = qp.parse_query(&occ_id).unwrap();
		let top_docs = searcher.search(&query, &TopDocs::with_limit(10)).unwrap();
		assert_eq!(top_docs.len(), 1);
	}

	#[tokio::test]
	async fn search_by_global_id_when_resolved() {
		let tmp = TempDir::new().unwrap();
		let ix = TantivySearchIndex::open_or_create(tmp.path()).unwrap();
		let mut info = blob_for_lib("serde", "Serialize");
		let gid = GlobalSymbolId(uuid::Uuid::new_v4());
		info.resolved_global_id = Some(gid);
		ix.index(&BlobRef("r".into()), &info).await.unwrap();

		let reader = ix.index_handle().reader().unwrap();
		let searcher = reader.searcher();
		let qp = QueryParser::for_index(ix.index_handle(), vec![ix.fields().global_id]);
		let query = qp.parse_query(&gid.0.to_string()).unwrap();
		let top_docs = searcher.search(&query, &TopDocs::with_limit(10)).unwrap();
		assert_eq!(top_docs.len(), 1);
	}

	#[tokio::test]
	async fn search_by_repo_id_for_repo_local_symbol() {
		let tmp = TempDir::new().unwrap();
		let ix = TantivySearchIndex::open_or_create(tmp.path()).unwrap();
		ix.index(&BlobRef("a".into()), &blob_repo_local("my-repo", "foo")).await.unwrap();
		ix.index(&BlobRef("b".into()), &blob_repo_local("other-repo", "bar")).await.unwrap();

		let reader = ix.index_handle().reader().unwrap();
		let searcher = reader.searcher();
		let qp = QueryParser::for_index(ix.index_handle(), vec![ix.fields().repo_id]);
		let query = qp.parse_query("\"my-repo\"").unwrap();
		let top_docs = searcher.search(&query, &TopDocs::with_limit(10)).unwrap();
		assert_eq!(top_docs.len(), 1);
		let doc: TantivyDocument = searcher.doc(top_docs[0].1).unwrap();
		let stored_ref = doc.get_first(ix.fields().blob_ref).and_then(|v| v.as_str()).unwrap();
		assert_eq!(stored_ref, "a");
	}

	#[tokio::test]
	async fn multiple_symbols_in_same_lib_all_indexed() {
		let tmp = TempDir::new().unwrap();
		let ix = TantivySearchIndex::open_or_create(tmp.path()).unwrap();
		ix.index(&BlobRef("a".into()), &blob_for_lib("serde", "Serialize")).await.unwrap();
		ix.index(&BlobRef("b".into()), &blob_for_lib("serde", "Deserialize")).await.unwrap();
		ix.index(&BlobRef("c".into()), &blob_for_lib("serde", "Serializer")).await.unwrap();

		let reader = ix.index_handle().reader().unwrap();
		let searcher = reader.searcher();
		let qp = QueryParser::for_index(ix.index_handle(), vec![ix.fields().lib_name]);
		let query = qp.parse_query("serde").unwrap();
		let top_docs = searcher.search(&query, &TopDocs::with_limit(10)).unwrap();
		assert_eq!(top_docs.len(), 3);
	}

	#[tokio::test]
	async fn deferred_blob_has_empty_global_id() {
		let tmp = TempDir::new().unwrap();
		let ix = TantivySearchIndex::open_or_create(tmp.path()).unwrap();
		let info = blob_for_lib("serde", "Serialize");
		assert!(info.resolved_global_id.is_none());
		ix.index(&BlobRef("r".into()), &info).await.unwrap();

		let reader = ix.index_handle().reader().unwrap();
		let searcher = reader.searcher();
		let qp = QueryParser::for_index(ix.index_handle(), vec![ix.fields().symbol_name]);
		let query = qp.parse_query("Serialize").unwrap();
		let top_docs = searcher.search(&query, &TopDocs::with_limit(10)).unwrap();
		let doc: TantivyDocument = searcher.doc(top_docs[0].1).unwrap();
		let global = doc.get_first(ix.fields().global_id).and_then(|v| v.as_str()).unwrap();
		assert_eq!(global, "");
	}

	#[tokio::test]
	async fn reindex_resolves_without_duplicates() {
		let tmp = TempDir::new().unwrap();
		let ix = TantivySearchIndex::open_or_create(tmp.path()).unwrap();
		let mut info = blob_for_lib("serde", "Serialize");
		// First index with no global_id (deferred).
		ix.index(&BlobRef("r".into()), &info).await.unwrap();
		// Re-index after resolution with a global_id.
		info.resolved_global_id = Some(GlobalSymbolId(uuid::Uuid::new_v4()));
		ix.index(&BlobRef("r".into()), &info).await.unwrap();

		let reader = ix.index_handle().reader().unwrap();
		let searcher = reader.searcher();
		let qp = QueryParser::for_index(ix.index_handle(), vec![ix.fields().symbol_name]);
		let query = qp.parse_query("Serialize").unwrap();
		let top_docs = searcher.search(&query, &TopDocs::with_limit(10)).unwrap();
		assert_eq!(top_docs.len(), 1, "re-index must update, not duplicate");
		let doc: TantivyDocument = searcher.doc(top_docs[0].1).unwrap();
		let gid = doc.get_first(ix.fields().global_id).and_then(|v| v.as_str()).unwrap();
		assert!(!gid.is_empty(), "resolved global_id should be stored");
	}

	#[tokio::test]
	async fn search_returns_zero_for_unknown_term() {
		let tmp = TempDir::new().unwrap();
		let ix = TantivySearchIndex::open_or_create(tmp.path()).unwrap();
		ix.index(&BlobRef("a".into()), &blob_for_lib("serde", "Serialize")).await.unwrap();

		let reader = ix.index_handle().reader().unwrap();
		let searcher = reader.searcher();
		let qp = QueryParser::for_index(ix.index_handle(), vec![ix.fields().symbol_name]);
		let query = qp.parse_query("NonExistent").unwrap();
		let top_docs = searcher.search(&query, &TopDocs::with_limit(10)).unwrap();
		assert_eq!(top_docs.len(), 0);
	}

	#[tokio::test]
	async fn index_persists_across_reopen() {
		let tmp = TempDir::new().unwrap();
		{
			let ix = TantivySearchIndex::open_or_create(tmp.path()).unwrap();
			ix.index(&BlobRef("ref-x".into()), &blob_for_lib("serde", "Serialize")).await.unwrap();
		}
		// Re-open the same directory and confirm the indexed doc is still there.
		let ix2 = TantivySearchIndex::open_or_create(tmp.path()).unwrap();
		let reader = ix2.index_handle().reader().unwrap();
		let searcher = reader.searcher();
		let qp = QueryParser::for_index(ix2.index_handle(), vec![ix2.fields().symbol_name]);
		let query = qp.parse_query("Serialize").unwrap();
		let top_docs = searcher.search(&query, &TopDocs::with_limit(10)).unwrap();
		assert_eq!(top_docs.len(), 1);
		let doc: TantivyDocument = searcher.doc(top_docs[0].1).unwrap();
		let stored_ref = doc.get_first(ix2.fields().blob_ref).and_then(|v| v.as_str()).unwrap();
		assert_eq!(stored_ref, "ref-x");
	}

	#[tokio::test]
	async fn search_query_finds_by_symbol_name() {
		use nudox_core::SearchQuery;

		let tmp = TempDir::new().unwrap();
		let ix = TantivySearchIndex::open_or_create(tmp.path()).unwrap();
		ix.index(&BlobRef("r1".into()), &blob_for_lib("serde", "Serialize")).await.unwrap();
		ix.index(&BlobRef("r2".into()), &blob_for_lib("tokio", "spawn")).await.unwrap();

		let hits = ix.search("Serialize", 10).await.unwrap();
		assert_eq!(hits.len(), 1);
		assert_eq!(hits[0].blob_ref, BlobRef("r1".into()));
		assert_eq!(hits[0].symbol_name, "Serialize");
	}

	#[tokio::test]
	async fn search_query_find_by_global_id() {
		use nudox_core::SearchQuery;

		let tmp = TempDir::new().unwrap();
		let ix = TantivySearchIndex::open_or_create(tmp.path()).unwrap();
		let gid = GlobalSymbolId(uuid::Uuid::new_v4());
		let mut info = blob_for_lib("serde", "Serialize");
		info.resolved_global_id = Some(gid);
		ix.index(&BlobRef("r1".into()), &info).await.unwrap();

		// Different symbol, no global_id.
		ix.index(&BlobRef("r2".into()), &blob_for_lib("tokio", "spawn")).await.unwrap();

		let hits = ix.find_by_global_id(gid, 10).await.unwrap();
		assert_eq!(hits.len(), 1);
		assert_eq!(hits[0].blob_ref, BlobRef("r1".into()));

		let none = ix.find_by_global_id(GlobalSymbolId(uuid::Uuid::new_v4()), 10).await.unwrap();
		assert!(none.is_empty());
	}
}
