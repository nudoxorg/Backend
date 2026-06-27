//! Tantivy-backed full-text symbol search.
//!
//! Fed directly by the parse-once [`ParsedSymbol`] projection — no embeddings
//! or external services required. Persists across restarts. Re-indexing a URI
//! replaces the previous entry (upsert semantics).

use std::{path::Path, sync::{Arc, Mutex}};

use tantivy::{Index, IndexWriter, TantivyDocument, Term, collector::TopDocs, directory::MmapDirectory, query::QueryParser, schema::{Field, STORED, STRING, Schema, TEXT, Value as TantivyValue}};

use crate::error::{AppError, TextIndexError};
use crate::ingest::parsed_symbol::{PackageCoord, ParsedSymbol};

/// A single result from [`SymbolTextIndex::search`].
#[derive(Debug)]
pub struct TextSearchHit {
	pub uri:         String,
	pub score:       f32,
	pub fq_name:     Option<String>,
	pub language:    String,
	pub package:     String,
	pub version:     Option<String>,
	pub symbol_kind: Option<String>,
}

struct Fields {
	uri:         Field,
	fq_name:     Field,
	text:        Field,
	language:    Field,
	package:     Field,
	version:     Field,
	symbol_kind: Field,
}

fn build_schema() -> (Schema, Fields) {
	let mut b = Schema::builder();
	let uri = b.add_text_field("uri", STRING | STORED);
	let fq_name = b.add_text_field("fq_name", TEXT | STORED);
	let text = b.add_text_field("text", TEXT);
	let language = b.add_text_field("language", STRING | STORED);
	let package = b.add_text_field("package", TEXT | STORED);
	let version = b.add_text_field("version", STORED);
	let symbol_kind = b.add_text_field("symbol_kind", STORED);
	let schema = b.build();
	(schema, Fields { uri, fq_name, text, language, package, version, symbol_kind })
}

/// Tantivy-backed full-text index over symbol documentation.
pub struct SymbolTextIndex {
	index:  Index,
	writer: Arc<Mutex<IndexWriter>>,
	fields: Fields,
}

impl SymbolTextIndex {
	/// Open (or create) the on-disk index at `dir`.
	pub fn open_or_create(dir: &Path) -> Result<Self, AppError> {
		std::fs::create_dir_all(dir)
			.map_err(|e| AppError::Storage { path: dir.to_path_buf(), source: e })?;
		let (schema, fields) = build_schema();
	let mmap = MmapDirectory::open(dir)
		.map_err(|e| AppError::TextIndex(TextIndexError::MmapDirectory { path: dir.to_path_buf(), source: e }))?;
	let index = Index::open_or_create(mmap, schema)
		.map_err(|e| AppError::TextIndex(TextIndexError::OpenOrCreate { source: e }))?;
	let writer = index
		.writer(50_000_000)
		.map_err(|e| AppError::TextIndex(TextIndexError::Writer { source: e }))?;
		Ok(Self { index, writer: Arc::new(Mutex::new(writer)), fields })
	}

	/// Index a batch of [`ParsedSymbol`]s in a single commit.
	///
	/// Each symbol's canonical entry URI is the deduplication key: any previous
	/// entry for the same URI is deleted before the new one is added.
	pub fn index_batch(
		&self,
		coord: &PackageCoord,
		symbols: &[ParsedSymbol],
	) -> Result<usize, AppError> {
		let mut writer = self
			.writer
			.lock()
			.map_err(|_| AppError::TextIndex(TextIndexError::LockPoisoned))?;

		for symbol in symbols {
			let uri = symbol.entry_uri.to_string();
			writer.delete_term(Term::from_field_text(self.fields.uri, &uri));

			let mut td = TantivyDocument::new();
			td.add_text(self.fields.uri, &uri);
			td.add_text(self.fields.fq_name, &symbol.fq_name);
			td.add_text(self.fields.text, &symbol.embedding_text);
			td.add_text(self.fields.language, coord.language.as_str());
			td.add_text(self.fields.package, &coord.package);
			td.add_text(self.fields.version, &coord.version);
			td.add_text(self.fields.symbol_kind, symbol.kind.label());

			writer
				.add_document(td)
				.map_err(|e| AppError::TextIndex(TextIndexError::AddDocument { source: e }))?;
		}

		writer.commit().map_err(|e| AppError::TextIndex(TextIndexError::Commit { source: e }))?;
		Ok(symbols.len())
	}

	/// Full-text search over symbol names, qualified names, and documentation.
	/// Returns up to `limit` results ordered by relevance score.
	pub fn search(&self, query_str: &str, limit: usize) -> Result<Vec<TextSearchHit>, AppError> {
		let reader = self
			.index
			.reader()
			.map_err(|e| AppError::TextIndex(TextIndexError::Reader { source: e }))?;
		let searcher = reader.searcher();
		let qp = QueryParser::for_index(&self.index, vec![
			self.fields.fq_name,
			self.fields.text,
			self.fields.package,
		]);
		let query = qp.parse_query(query_str).map_err(|e| {
			AppError::TextIndex(TextIndexError::ParseQuery { query: query_str.to_owned(), source: e })
		})?;
		let top_docs = searcher
			.search(&query, &TopDocs::with_limit(limit.max(1)))
			.map_err(|e| AppError::TextIndex(TextIndexError::Search { source: e }))?;

		let mut hits = Vec::with_capacity(top_docs.len());
		for (score, addr) in top_docs {
			let doc: TantivyDocument =
				searcher.doc(addr).map_err(|e| AppError::TextIndex(TextIndexError::DocFetch { source: e }))?;
			let get = |f: Field| doc.get_first(f).and_then(|v| v.as_str()).unwrap_or("").to_owned();
			let get_opt = |f: Field| {
				let s = get(f);
				if s.is_empty() { None } else { Some(s) }
			};
			hits.push(TextSearchHit {
				uri: get(self.fields.uri),
				score,
				fq_name: get_opt(self.fields.fq_name),
				language: get(self.fields.language),
				package: get(self.fields.package),
				version: get_opt(self.fields.version),
				symbol_kind: get_opt(self.fields.symbol_kind),
			});
		}
		Ok(hits)
	}
}
