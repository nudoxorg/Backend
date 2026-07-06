//! The emission pipeline: project an `ir::Index` into graph documents and stream
//! them to a sink.
//!
//! The index is projected to a [`crate::graph::from_ir::GraphCorpus`] via
//! [`crate::graph::from_ir::project`], then serialized to JSON-LD documents and
//! handed to the [`DocumentSink`] in the wave order the graph store's
//! referential integrity expects (GRAPH-ARCHITECTURE.md §5): packages → bare
//! symbols → symbols with links → reified relations → version membership.

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

/// Stream every document of `index`'s graph corpus into `sink`, in the
/// deterministic wave order the corpus upload expects.
pub fn emit<S: DocumentSink>(index: &Index, sink: &mut S) -> Result<(), GenerateError> {
	let _ = (index, sink);
	todo!("graph::from_ir::project -> serialize corpus in wave order -> sink.write; then flush")
}
