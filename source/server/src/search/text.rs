use std::{path::Path, sync::mpsc};

use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term, collector::TopDocs, directory::MmapDirectory, query::QueryParser, schema::{Field, STORED, STRING, Schema, TEXT, Value as TantivyValue}};

use nudox_core::Score;

use crate::http::error::{AppError, TextIndexError};
use crate::ingest::parsed_symbol::{PackageCoord, ParsedSymbol};

/// Cap on documents folded into a single coalesced `commit()`.
const MAX_BATCH: usize = 2048;

/// Work sent to the single-owner text-index writer thread.
enum TextWriterCmd {
	/// Upsert pre-built documents (each paired with the `uri` term to delete
	/// first) under a single commit; ack carries the number indexed.
	IndexBatch {
		docs: Vec<(Term, TantivyDocument)>,
		ack:  mpsc::Sender<Result<usize, AppError>>,
	},
}

#[derive(Debug)]
pub struct TextSearchHit {
	pub uri:         String,
	pub score:       Score,
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

pub struct SymbolTextIndex {
	index:  Index,
	reader: IndexReader,
	tx:     mpsc::Sender<TextWriterCmd>,
	fields: Fields,
}

impl SymbolTextIndex {
	pub fn open_or_create(dir: &Path) -> Result<Self, AppError> {
		std::fs::create_dir_all(dir)
			.map_err(|e| AppError::Storage { path: dir.to_path_buf(), source: e })?;
		let (schema, fields) = build_schema();
		let mmap = MmapDirectory::open(dir).map_err(|e| {
			AppError::TextIndex(TextIndexError::MmapDirectory { path: dir.to_path_buf(), source: e })
		})?;
		let index = Index::open_or_create(mmap, schema)
			.map_err(|e| AppError::TextIndex(TextIndexError::OpenOrCreate { source: e }))?;
		let writer = index
			.writer(50_000_000)
			.map_err(|e| AppError::TextIndex(TextIndexError::Writer { source: e }))?;

		// Reader built once; reloaded explicitly by the owner after each commit.
		let reader = index
			.reader_builder()
			.reload_policy(ReloadPolicy::Manual)
			.try_into()
			.map_err(|e| AppError::TextIndex(TextIndexError::Reader { source: e }))?;

		// Single-owner writer thread: the only place the `IndexWriter` is used.
		// It runs the blocking add/commit off the async reactor and reloads the
		// shared reader after each commit.
		let (tx, rx) = mpsc::channel::<TextWriterCmd>();
		let reader_for_owner = reader.clone();
		std::thread::Builder::new()
			.name("text-index-writer".to_owned())
			.spawn(move || run_writer(writer, reader_for_owner, rx))
			.map_err(|_| AppError::TextIndex(TextIndexError::LockPoisoned))?;

		Ok(Self { index, reader, tx, fields })
	}

	pub fn index_batch(
		&self,
		coord: &PackageCoord,
		symbols: &[ParsedSymbol],
	) -> Result<usize, AppError> {
		if symbols.is_empty() {
			return Ok(0);
		}

		// Build documents on the caller side, then hand the whole batch to the
		// owner thread for a single commit.
		let docs = symbols
			.iter()
			.map(|symbol| {
				let uri = symbol.entry_uri.to_string();
				let prev = Term::from_field_text(self.fields.uri, &uri);
				let mut td = TantivyDocument::new();
				td.add_text(self.fields.uri, &uri);
				td.add_text(self.fields.fq_name, &symbol.fq_name);
				td.add_text(self.fields.text, &symbol.embedding_text);
				td.add_text(self.fields.language, coord.language.as_str());
				td.add_text(self.fields.package, &coord.package);
				td.add_text(self.fields.version, &coord.version);
				td.add_text(self.fields.symbol_kind, symbol.kind.label());
				(prev, td)
			})
			.collect();

		let (ack, ack_rx) = mpsc::channel();
		self.tx
			.send(TextWriterCmd::IndexBatch { docs, ack })
			.map_err(|_| AppError::TextIndex(TextIndexError::LockPoisoned))?;
		ack_rx
			.recv()
			.map_err(|_| AppError::TextIndex(TextIndexError::LockPoisoned))?
	}

	pub fn search(&self, query_str: &str, limit: usize) -> Result<Vec<TextSearchHit>, AppError> {
		let searcher = self.reader.searcher();
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
			let doc: TantivyDocument = searcher
				.doc(addr)
				.map_err(|e| AppError::TextIndex(TextIndexError::DocFetch { source: e }))?;
			let get = |f: Field| doc.get_first(f).and_then(|v| v.as_str()).unwrap_or("").to_owned();
			let get_opt = |f: Field| {
				let s = get(f);
				if s.is_empty() { None } else { Some(s) }
			};
			hits.push(TextSearchHit {
				uri: get(self.fields.uri),
				score: Score::new(score),
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

/// Owner loop for the single text-index `IndexWriter`, on a dedicated thread.
///
/// Pops one command (blocking), drains anything else pending so bursts coalesce
/// into one `commit()`, reloads the shared reader, then acks each waiter with
/// the number of documents it contributed.
fn run_writer(
	mut writer: IndexWriter,
	reader: IndexReader,
	rx: mpsc::Receiver<TextWriterCmd>,
) {
	while let Ok(first) = rx.recv() {
		let mut acks: Vec<(mpsc::Sender<Result<usize, AppError>>, Result<usize, String>)> =
			Vec::new();
		let mut pending = 0usize;

		let mut cmd = Some(first);
		loop {
			if let Some(TextWriterCmd::IndexBatch { docs, ack }) = cmd.take() {
				let count = docs.len();
				let mut res: Result<usize, String> = Ok(count);
				for (prev, td) in docs {
					writer.delete_term(prev);
					if let Err(e) = writer.add_document(td) {
						res = Err(e.to_string());
						break;
					}
				}
				pending += count;
				acks.push((ack, res));
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
			let final_res: Result<usize, AppError> = match (add_res, &commit_res, &reload_res) {
				(Err(e), _, _) => Err(text_err(TextErrKind::Add, e)),
				(_, Err(e), _) => Err(text_err(TextErrKind::Commit, e.clone())),
				(_, _, Err(e)) => Err(text_err(TextErrKind::Reload, e.clone())),
				(Ok(n), _, _) => Ok(n),
			};
			let _ = ack.send(final_res);
		}
	}
}

enum TextErrKind {
	Add,
	Commit,
	Reload,
}

/// Rebuild a typed [`TextIndexError`] from a stringified tantivy error.
///
/// The owner loop stringifies errors so a single commit failure can be reported
/// to every waiter in the batch; the original `TantivyError` is not `Clone`.
fn text_err(kind: TextErrKind, msg: String) -> AppError {
	let source = tantivy::TantivyError::SystemError(msg);
	AppError::TextIndex(match kind {
		TextErrKind::Add => TextIndexError::AddDocument { source },
		TextErrKind::Commit => TextIndexError::Commit { source },
		TextErrKind::Reload => TextIndexError::Reader { source },
	})
}
