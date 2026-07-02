//! The emission pipeline: project an `ir::Index` into graph documents and stream
//! them to a sink.
//!
//! Each IR entry is projected to the graph model via [`crate::graph::from_ir`],
//! serialized to a JSON-LD document, and handed to the [`DocumentSink`] one (or
//! one small batch) at a time — so a large package never materializes its whole
//! document set in memory. Documents are emitted in a deterministic order
//! (kinds-first, deepest-entry-first) so the corpus upload is reproducible.

use ir::entry::Index;
use serde_json::Value;

use crate::error::GenerateError;

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

/// Stream every entry in `index` as a JSON-LD graph document into `sink`, in the
/// deterministic kinds-first / deepest-first order the corpus upload expects.
pub fn emit<S: DocumentSink>(index: &Index, sink: &mut S) -> Result<(), GenerateError> {
	let _ = (index, sink);
	todo!("order entries; for each: graph::from_ir::entry -> serialize -> sink.write; then flush")
}
