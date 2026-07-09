//! The emission pipeline: project an `ir::Index` into graph documents and stream
//! them to a sink.
//!
//! The index is projected to a [`crate::graph::from_ir::GraphCorpus`] via
//! [`crate::graph::from_ir::project`], then serialized to JSON-LD documents and
//! handed to the [`DocumentSink`] in the wave order the graph store's
//! referential integrity expects (GRAPH-ARCHITECTURE.md §5): packages → bare
//! symbols → symbols with links → reified relations → version membership.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use ir::entry::Index;
use serde_json::Value;
use terminusdb_schema::{ToJson, ToTDBInstance};

use crate::error::{EmitLinkedDataError, GenerateError};
use crate::graph::from_ir::project;
use crate::graph::link::PackageCtx;

/// The `Symbol` fields that are links to other symbols. Wave 2 emits symbols
/// with these stripped (targets may not exist yet); wave 3 re-emits the full
/// documents once every target does. `resolves_to` lives on the shape's name
/// leaves, so the strip is recursive.
const EDGE_FIELDS: [&str; 7] =
	["member_of", "implements", "extends", "mentions", "takes", "returns", "resolves_to"];

/// A destination for emitted graph documents — e.g. an NDJSON writer, a batching
/// uploader to the graph store, or a test collector. Implementors decide how (and
/// whether) to batch; the emitter just streams.
pub trait DocumentSink {
	/// Accept one serialized graph document.
	fn write(&mut self, document: Value) -> Result<(), GenerateError>;

	/// Flush any buffered batch. Called once at the end of [`emit`].
	fn flush(&mut self) -> Result<(), GenerateError> {
		Ok(())
	}
}

/// Per-wave first-write-wins bookkeeping: re-emitting an identical body for an
/// `@id` is a no-op, a *different* body for the same `@id` within a wave is a
/// conflict. (Across waves, re-emission is the §5 full-replace step and is
/// expected.) Only the body hash is retained, so memory stays bounded.
#[derive(Default)]
struct EmitState {
	seen: HashMap<(u8, String), u64>,
}

impl EmitState {
	fn write<S: DocumentSink>(
		&mut self,
		sink: &mut S,
		wave: u8,
		document: Value,
	) -> Result<(), GenerateError> {
		if let Some(id) = document.get("@id").and_then(Value::as_str) {
			let body_hash = hash_body(&document);
			match self.seen.entry((wave, id.to_owned())) {
				std::collections::hash_map::Entry::Occupied(existing) => {
					if *existing.get() == body_hash {
						// Identical re-emission: first write wins, silently.
						return Ok(());
					}
					return Err(EmitLinkedDataError::ConflictingDocumentBody { id: id.to_owned() }.into());
				}
				std::collections::hash_map::Entry::Vacant(slot) => {
					slot.insert(body_hash);
				}
			}
		}
		sink.write(document)
	}
}

fn hash_body(document: &Value) -> u64 {
	let mut hasher = std::collections::hash_map::DefaultHasher::new();
	document.to_string().hash(&mut hasher);
	hasher.finish()
}

/// Recursively remove the symbol→symbol edge fields from a document, leaving
/// the scalars and the shape payload — the "bare" wave-2 form.
fn strip_edges(document: &mut Value) {
	match document {
		Value::Object(map) => {
			for field in EDGE_FIELDS {
				map.remove(field);
			}
			for value in map.values_mut() {
				strip_edges(value);
			}
		}
		Value::Array(items) => {
			for item in items {
				strip_edges(item);
			}
		}
		_ => {}
	}
}

/// Stream every document of `index`'s graph corpus into `sink`, in the
/// deterministic wave order the corpus upload expects. `ctx` names the package
/// the index describes (the projection mints every IRI under it).
pub fn emit<S: DocumentSink>(
	index: &Index,
	ctx: PackageCtx,
	sink: &mut S,
) -> Result<(), GenerateError> {
	let corpus = project(index, ctx);
	let mut state = EmitState::default();

	// Wave 1: package nodes — no symbol links.
	for package in &corpus.packages {
		state.write(sink, 1, package.to_instance(None).to_json())?;
	}

	// Wave 2: every symbol, bare (edge fields stripped).
	for symbol in &corpus.symbols {
		let mut document = symbol.to_instance(None).to_json();
		strip_edges(&mut document);
		state.write(sink, 2, document)?;
	}

	// Wave 3: every symbol again, full replace — all link targets now exist.
	for symbol in &corpus.symbols {
		state.write(sink, 3, symbol.to_instance(None).to_json())?;
	}

	// Wave 4: reified relations.
	for implementation in &corpus.implementations {
		state.write(sink, 4, implementation.to_instance(None).to_json())?;
	}
	for reference in &corpus.references {
		state.write(sink, 4, reference.to_instance(None).to_json())?;
	}

	// Wave 5: version membership (`declares` needs every symbol present).
	if let Some(version) = &corpus.version {
		state.write(sink, 5, version.to_instance(None).to_json())?;
	}

	sink.flush()
}
