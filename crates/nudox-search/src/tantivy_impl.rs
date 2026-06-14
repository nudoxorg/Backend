//! Real tantivy-backed search index. Implements the v1 schema from the design
//! doc.

use std::{path::Path, sync::{Arc, Mutex}};

use async_trait::async_trait;
use nudox_core::{BlobInfo, BlobRef, Error, GlobalSymbolId, OccurrenceId, Result, SearchHit, SearchIndex, SearchQuery, SymbolOrigin};
use tantivy::{Index, IndexWriter, TantivyDocument, Term, directory::MmapDirectory, schema::{Field, STORED, STRING, Schema, TEXT, Value as TantivyValue}};

/// Tantivy-backed full-text / exact search index.
///
/// `index()` has upsert semantics: it deletes any existing document with the
/// same `occurrence_id` before adding the new one, so re-indexing a blob after
/// deferred resolution doesn't produce duplicate entries.
///
/// Each call commits immediately. Batched commits and background merge policy
/// are deferred.
pub struct TantivySearchIndex {
	index:  Index,
	writer: Arc<Mutex<IndexWriter>>,
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
		std::fs::create_dir_all(index_dir)
			.map_err(|e| Error::Search(format!("create_dir_all({}): {e}", index_dir.display())))?;
		let (schema, fields) = build_schema();
		let dir =
			MmapDirectory::open(index_dir).map_err(|e| Error::Search(format!("MmapDirectory: {e}")))?;
		let index = Index::open_or_create(dir, schema)
			.map_err(|e| Error::Search(format!("open_or_create: {e}")))?;
		let writer = index.writer(50_000_000).map_err(|e| Error::Search(format!("writer: {e}")))?;
		Ok(Self { index, writer: Arc::new(Mutex::new(writer)), fields })
	}

	/// Open a reader for querying. Callers do not normally need this directly —
	/// use [`SearchQuery`] instead.
	pub fn reader(&self) -> tantivy::Result<tantivy::IndexReader> { self.index.reader() }

	#[cfg(test)]
	pub(crate) fn index_handle(&self) -> &Index { &self.index }

	#[cfg(test)]
	pub(crate) fn fields(&self) -> &Fields { &self.fields }
}

#[async_trait]
impl SearchIndex for TantivySearchIndex {
	#[tracing::instrument(skip(self, info), fields(blob_ref = %blob_ref, occurrence_id = %info.occurrence_id))]
	async fn index(&self, blob_ref: &BlobRef, info: &BlobInfo) -> Result<()> {
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

		let mut writer =
			self.writer.lock().map_err(|e| Error::Search(format!("lock poisoned: {e}")))?;

		// Delete any previous entry for this occurrence so re-indexing after
		// deferred resolution doesn't produce duplicate documents.
		let prev = Term::from_field_text(self.fields.occurrence_id, &info.occurrence_id.to_string());
		writer.delete_term(prev);

		writer.add_document(doc).map_err(|e| Error::Search(format!("add_document: {e}")))?;
		writer.commit().map_err(|e| Error::Search(format!("commit: {e}")))?;
		Ok(())
	}
}

#[async_trait]
impl SearchQuery for TantivySearchIndex {
	async fn search(&self, query_str: &str, limit: usize) -> Result<Vec<SearchHit>> {
		let reader = self.index.reader().map_err(|e| Error::Search(e.to_string()))?;
		let searcher = reader.searcher();
		let qp = tantivy::query::QueryParser::for_index(&self.index, vec![
			self.fields.symbol_name,
			self.fields.lib_name,
			self.fields.repo_id,
		]);
		let query =
			qp.parse_query(query_str).map_err(|e| Error::Search(format!("parse_query: {e}")))?;
		let top_docs = searcher
			.search(&query, &tantivy::collector::TopDocs::with_limit(limit))
			.map_err(|e| Error::Search(e.to_string()))?;

		let mut hits = Vec::with_capacity(top_docs.len());
		for (score, addr) in top_docs {
			let doc: tantivy::TantivyDocument =
				searcher.doc(addr).map_err(|e| Error::Search(e.to_string()))?;
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
		let reader = self.index.reader().map_err(|e| Error::Search(e.to_string()))?;
		let searcher = reader.searcher();
		let term = tantivy::Term::from_field_text(self.fields.global_id, &global_id.to_string());
		let query = tantivy::query::TermQuery::new(term, tantivy::schema::IndexRecordOption::Basic);
		let top_docs = searcher
			.search(&query, &tantivy::collector::TopDocs::with_limit(limit))
			.map_err(|e| Error::Search(e.to_string()))?;

		let mut hits = Vec::with_capacity(top_docs.len());
		for (score, addr) in top_docs {
			let doc: tantivy::TantivyDocument =
				searcher.doc(addr).map_err(|e| Error::Search(e.to_string()))?;
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
		let reader = self.index.reader().map_err(|e| Error::Search(e.to_string()))?;
		let searcher = reader.searcher();
		let top_docs = searcher
			.search(&tantivy::query::AllQuery, &tantivy::collector::TopDocs::with_limit(limit))
			.map_err(|e| Error::Search(e.to_string()))?;

		let mut hits = Vec::with_capacity(top_docs.len());
		for (score, addr) in top_docs {
			let doc: tantivy::TantivyDocument =
				searcher.doc(addr).map_err(|e| Error::Search(e.to_string()))?;
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
	use nudox_core::{BLOB_SCHEMA_VERSION, ByteSpan, ChunkMetadata, GlobalSymbolId, Language, LibRef, OccurrenceId, RepoId, SourceChunk, TreesitterRepr};
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
				treesitter_repr: TreesitterRepr(vec![]),
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
				raw_code:        format!("use {name}::{symbol};"),
				treesitter_repr: TreesitterRepr(vec![]),
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
