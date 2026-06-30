//! Diagram: **qdrant semantic/code search is heavy — make sure it is properly
//! gated / not implicit.** This is the runtime-level enforcement under
//! `runtime::vector::gate` (the server surface relies on it).
//!
//! TDD specs (`todo!()`).

/// The vector store cannot be searched without presenting the gate.
///
/// Assert: calling the semantic search path without the gate token is a
///   compile-time or hard runtime refusal — never a silent run.
#[tokio::test]
async fn vector_search_requires_the_gate() {
    todo!("assert the gate is mandatory for vector search");
}

/// Holding the gate is the ONLY way to run a semantic query.
///
/// Assert: with the gate, a vector query runs; the gate is the sole entry point.
#[tokio::test]
async fn gate_is_the_sole_entry_point() {
    todo!("assert the gate is the only semantic entry point");
}

/// Embedding a query is part of the gated path, not the default path.
///
/// Assert: no embedding happens unless the gate has been satisfied (heavy work
///   is never done implicitly).
#[tokio::test]
async fn embedding_only_happens_behind_the_gate() {
    todo!("assert embeddings are computed only behind the gate");
}
