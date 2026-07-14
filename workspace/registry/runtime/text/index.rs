//! The tantivy symbol/text index — the default, lightweight search surface for
//! finding items by name/signature.
//!
//! Owns a single replica-local tantivy directory. All indexing is synchronous
//! and CPU/disk-bound, so it runs on `spawn_blocking`.

use std::path::Path;
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard};

use tantivy::{
	Index, IndexReader, IndexWriter, TantivyDocument, TantivyError, Term, doc,
	directory::MmapDirectory,
	schema::{Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions},
};

use heart::{ContentHash, SymbolId, Symbol, SymbolKind};

use crate::error::TextError;
use crate::text::tokenizer;

/// Heap budget for the single writer — modest, since symbol documents are tiny.
const WRITER_MEMORY_BYTES: usize = 50_000_000;

/// The tantivy schema fields the symbol index is built over. Held so field
/// handles are resolved once, not per-operation.
///
/// Most fields are raw (untokenized) `STRING`: precise lookup works on whole
/// terms and dictionary regexes, and the stored fields carry everything needed
/// to rebuild a [`Symbol`] from a hit. The lowercased shadow fields make
/// contains-matching case-insensitive. On top of those, `name_tokens`/`fq_tokens`
/// are tokenized with the identifier analyzer ([`tokenizer`]) so a query for a
/// *part* of a name (`user` → `getUserById`, `http` → `HTTPServer`) matches —
/// which raw-string contains-matching cannot do across camel/snake boundaries.
/// That analyzer lives in the index's tokenizer manager and is re-registered on
/// every open (see [`TextIndex::open_or_create`]).
#[derive(Clone)]
pub struct TextSchema {
	pub(crate) schema: Schema,
	/// The [`SymbolId`] uuid — stored, and the upsert/delete key.
	pub(crate) id: Field,
	/// The owning package's uuid — stored.
	pub(crate) package: Field,
	/// The lowercase [`heart::Language`] token — stored, filterable.
	pub(crate) ecosystem: Field,
	/// The [`SymbolKind`] name — stored, filterable.
	pub(crate) kind: Field,
	/// The plain name, original case — stored.
	pub(crate) name: Field,
	/// The fully-qualified name, original case — stored.
	pub(crate) fq_name: Field,
	/// Lowercased plain name — the exact/contains match surface.
	pub(crate) name_lower: Field,
	/// Lowercased fully-qualified name — the path-contains match surface.
	pub(crate) fq_lower: Field,
	/// Plain name, identifier-tokenized — the subtoken match surface.
	pub(crate) name_tokens: Field,
	/// Fully-qualified name, identifier-tokenized — the path-subtoken surface.
	pub(crate) fq_tokens: Field,
}

impl TextSchema {
	/// Build the symbol schema: the [`SymbolId`] (stored, keyed for
	/// upsert-by-id), name + fq-name (raw + lowercased for exact/partial match,
	/// plus identifier-tokenized for subtoken match), kind, and ecosystem.
	pub fn build() -> Self {
		let mut builder = Schema::builder();
		let id = builder.add_text_field("id", STRING | STORED);
		let package = builder.add_text_field("package", STRING | STORED);
		let ecosystem = builder.add_text_field("ecosystem", STRING | STORED);
		let kind = builder.add_text_field("kind", STRING | STORED);
		let name = builder.add_text_field("name", STRING | STORED);
		let fq_name = builder.add_text_field("fq_name", STRING | STORED);
		let name_lower = builder.add_text_field("name_lower", STRING);
		let fq_lower = builder.add_text_field("fq_lower", STRING);
		let name_tokens = builder.add_text_field("name_tokens", Self::ident_options());
		let fq_tokens = builder.add_text_field("fq_tokens", Self::ident_options());
		let schema = builder.build();
		Self {
			schema, id, package, ecosystem, kind, name, fq_name, name_lower, fq_lower, name_tokens,
			fq_tokens,
		}
	}

	/// Indexing options for an identifier-tokenized field: the [`tokenizer`]
	/// analyzer, with positions so multi-word phrase queries (`get user`) work.
	/// Not stored — the raw `name`/`fq_name` fields carry the displayable value.
	fn ident_options() -> TextOptions {
		TextOptions::default().set_indexing_options(
			TextFieldIndexing::default()
				.set_tokenizer(tokenizer::IDENT_TOKENIZER)
				.set_index_option(IndexRecordOption::WithFreqsAndPositions),
		)
	}
}

/// The stored token for a [`SymbolKind`] — its `Display` name, matching what the
/// registry persists in postgres so the two projections can never drift.
pub(crate) fn kind_token(kind: SymbolKind) -> String { kind.to_string() }

/// Reconstruct a [`SymbolKind`] from its stored token.
///
/// Delegates to the `strum`-derived `FromStr` impl on [`SymbolKind`], which
/// matches the same PascalCase variant names that `Display` (and `to_string()`)
/// emits — encode/decode are guaranteed symmetric.
pub(crate) fn parse_kind(token: &str) -> Option<SymbolKind> {
	SymbolKind::from_str(token).ok()
}

/// A replica-local tantivy index over [`Symbol`] records.
///
/// One writer, one directory, one replica. Re-indexing the same
/// [`SymbolId`] *updates* the document rather than duplicating it
/// (delete-by-term then add), so the index is a projection of postgres, never a
/// growing append log.
pub struct TextIndex {
	schema: TextSchema,
	/// The single writer, serialized behind a mutex (one writer per directory).
	writer: Mutex<IndexWriter>,
	/// The reader; reloaded explicitly at each search so commits are visible
	/// immediately (no background reload thread to race against).
	pub(crate) reader: IndexReader,
}

impl TextIndex {
	/// Open an existing index at `dir`, or create it if absent.
	// runs on spawn_blocking
	pub fn open_or_create(dir: &Path) -> Result<Self, TextError> {
		std::fs::create_dir_all(dir).map_err(TextError::Io)?;
		let schema = TextSchema::build();
		let directory = MmapDirectory::open(dir)
			.map_err(|error| TextError::Engine(TantivyError::from(error)))?;
		let index =
			Index::open_or_create(directory, schema.schema.clone()).map_err(TextError::Engine)?;
		// The identifier analyzer lives in the per-index tokenizer manager, which
		// is in-memory state rebuilt on every open — register it before any write
		// or search touches the `*_tokens` fields.
		tokenizer::register(&index);
		let writer = index.writer(WRITER_MEMORY_BYTES).map_err(TextError::Engine)?;
		let reader = index
			.reader_builder()
			.reload_policy(tantivy::ReloadPolicy::Manual)
			.try_into()
			.map_err(TextError::Engine)?;
		tracing::info!(directory = %dir.display(), "text index opened");
		Ok(Self { schema, writer: Mutex::new(writer), reader })
	}

	/// The schema this index was built over.
	pub fn schema(&self) -> &TextSchema { &self.schema }

	/// The current index snapshot as a [`ContentHash`] over the searchable
	/// segments — the anchor a search [`heart::Cursor`] is minted against, so a
	/// resumed page can detect that the index moved underneath it.
	pub fn snapshot(&self) -> Result<ContentHash, TextError> {
		self.reader.reload().map_err(TextError::Engine)?;
		Ok(snapshot_hash(&self.reader.searcher()))
	}

	/// The writer guard. Poisoning would require a panic inside tantivy while
	/// the lock is held; surfaced as an engine error rather than propagating the
	/// panic into every caller.
	fn writer(&self) -> Result<MutexGuard<'_, IndexWriter>, TextError> {
		self.writer.lock().map_err(|_| {
			TextError::Engine(TantivyError::InternalError(
				"text index writer poisoned by a panicked indexing thread".to_owned(),
			))
		})
	}

	/// The delete-key term for a symbol id.
	fn id_term(&self, id: SymbolId) -> Term {
		Term::from_field_text(self.schema.id, &id.as_uuid().to_string())
	}

	/// Upsert one symbol (delete-by-id then add), leaving the change uncommitted.
	// runs on spawn_blocking
	pub fn upsert(&self, symbol: &Symbol) -> Result<(), TextError> {
		let writer = self.writer()?;
		self.upsert_with(&writer, symbol)
	}

	/// The shared upsert body, so the batch path locks the writer exactly once.
	fn upsert_with(&self, writer: &IndexWriter, symbol: &Symbol) -> Result<(), TextError> {
		let fields = &self.schema;
		writer.delete_term(self.id_term(symbol.id));
		writer
			.add_document(doc!(
				fields.id => symbol.id.as_uuid().to_string(),
				fields.package => symbol.package.as_uuid().to_string(),
				fields.ecosystem => symbol.ecosystem.as_token().to_owned(),
				fields.kind => kind_token(symbol.kind),
				fields.name => symbol.name.plain.to_string(),
				fields.fq_name => symbol.name.fully_qualified.to_string(),
				fields.name_lower => symbol.name.plain.to_lowercase(),
				fields.fq_lower => symbol.name.fully_qualified.to_lowercase(),
				fields.name_tokens => symbol.name.plain.to_string(),
				fields.fq_tokens => symbol.name.fully_qualified.to_string(),
			))
			.map_err(TextError::Engine)?;
		Ok(())
	}

	/// Upsert a batch of symbols, then commit once — the poller's write path.
	// runs on spawn_blocking
	pub fn upsert_batch(&self, symbols: &[Symbol]) -> Result<(), TextError> {
		let mut writer = self.writer()?;
		for symbol in symbols {
			self.upsert_with(&writer, symbol)?;
		}
		writer.commit().map_err(TextError::Engine)?;
		tracing::debug!(symbols = symbols.len(), "text index batch committed");
		Ok(())
	}

	/// Remove a symbol's document by id.
	// runs on spawn_blocking
	pub fn remove(&self, id: SymbolId) -> Result<(), TextError> {
		self.writer()?.delete_term(self.id_term(id));
		Ok(())
	}

	/// Flush pending writes durably to disk.
	// runs on spawn_blocking
	pub fn commit(&self) -> Result<(), TextError> {
		self.writer()?.commit().map_err(TextError::Engine)?;
		Ok(())
	}
}

/// Hash the searchable state (segment ids + live/deleted doc counts) into the
/// snapshot a cursor anchors to. Any commit that changes visible documents
/// changes this hash.
pub(crate) fn snapshot_hash(searcher: &tantivy::Searcher) -> ContentHash {
	let mut hasher = ContentHash::builder();
	for segment in searcher.segment_readers() {
		hasher.update(segment.segment_id().uuid_string().as_bytes());
		hasher.update(&segment.num_docs().to_le_bytes());
		hasher.update(&segment.num_deleted_docs().to_le_bytes());
	}
	hasher.finalize()
}

/// Rebuild the [`Symbol`] a stored document projects.
pub(crate) fn symbol_from_document(
	schema: &TextSchema,
	document: &TantivyDocument,
) -> Result<Symbol, TextError> {
	let text = |field: Field, label: &str| -> Result<&str, TextError> {
		use tantivy::schema::Value;
		document.get_first(field).and_then(|value| value.as_str()).ok_or_else(|| {
			TextError::Engine(TantivyError::InternalError(format!(
				"stored document missing field {label}"
			)))
		})
	};
	let malformed = |label: &str, raw: &str| {
		TextError::Engine(TantivyError::InternalError(format!(
			"stored document field {label} holds malformed value {raw:?}"
		)))
	};

	let id = text(schema.id, "id")?;
	let package = text(schema.package, "package")?;
	let ecosystem = text(schema.ecosystem, "ecosystem")?;
	let kind = text(schema.kind, "kind")?;
	Ok(Symbol {
		id: id
			.parse::<heart::Guid>()
			.map(SymbolId::from_uuid)
			.map_err(|_| malformed("id", id))?,
		package: heart::PackageId::from_uuid(
			package.parse::<heart::Guid>().map_err(|_| malformed("package", package))?,
		),
		ecosystem: ecosystem.parse().map_err(|_| malformed("ecosystem", ecosystem))?,
		name: heart::Name {
			plain: text(schema.name, "name")?.into(),
			fully_qualified: text(schema.fq_name, "fq_name")?.into(),
		},
		kind: parse_kind(kind).ok_or_else(|| malformed("kind", kind))?,
	})
}
