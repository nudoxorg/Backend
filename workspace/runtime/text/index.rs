//! The tantivy symbol/text index — the default, lightweight search surface for
//! finding items by name/signature.
//!
//! Owns a single replica-local tantivy directory. All indexing is synchronous
//! and CPU/disk-bound, so it runs on `spawn_blocking`.

use std::path::Path;

use heart::{SymbolId, Symbol};

use crate::error::TextError;

/// The tantivy schema fields the symbol index is built over. Held so field
/// handles are resolved once, not per-operation.
#[derive(Clone)]
pub struct TextSchema {
	// tantivy::schema::{Schema, Field ...}; kept opaque so the field layout can
	// evolve without churning the public API.
}

impl TextSchema {
	/// Build the symbol schema: the [`SymbolId`] (stored, keyed for
	/// upsert-by-id), name + fq-name (tokenized for exact/partial match), kind,
	/// and ecosystem.
	pub fn build() -> Self {
		todo!("define tantivy schema: id (stored+fast), name, fq_name, kind, ecosystem")
	}
}

/// A replica-local tantivy index over [`Symbol`] records.
///
/// One writer, one directory, one replica. Re-indexing the same
/// [`SymbolId`] *updates* the document rather than duplicating it
/// (delete-by-term then add), so the index is a projection of postgres, never a
/// growing append log.
pub struct TextIndex {
	schema: TextSchema,
	// tantivy::{Index, IndexWriter, IndexReader}; opaque.
}

impl TextIndex {
	/// Open an existing index at `dir`, or create it if absent.
	// runs on spawn_blocking
	pub fn open_or_create(dir: &Path) -> Result<Self, TextError> {
		let _ = dir;
		todo!("open the tantivy index at `dir` or create it with TextSchema::build()")
	}

	/// The schema this index was built over.
	pub fn schema(&self) -> &TextSchema { &self.schema }

	/// Upsert one symbol (delete-by-id then add), leaving the change uncommitted.
	// runs on spawn_blocking
	pub fn upsert(&self, symbol: &Symbol) -> Result<(), TextError> {
		let _ = (symbol, &self.schema);
		todo!("delete_term(id) then add_document(symbol); do not commit yet")
	}

	/// Upsert a batch of symbols, then commit once — the poller's write path.
	// runs on spawn_blocking
	pub fn upsert_batch(&self, symbols: &[Symbol]) -> Result<(), TextError> {
		let _ = symbols;
		todo!("delete-by-id + add each, then a single commit")
	}

	/// Remove a symbol's document by id.
	// runs on spawn_blocking
	pub fn remove(&self, id: SymbolId) -> Result<(), TextError> {
		let _ = id;
		todo!("delete_term over the id field")
	}

	/// Flush pending writes durably to disk.
	// runs on spawn_blocking
	pub fn commit(&self) -> Result<(), TextError> {
		todo!("IndexWriter::commit")
	}
}
