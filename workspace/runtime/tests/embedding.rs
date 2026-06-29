//! Pipeline part: **embeddings** (`runtime::vector::embedding`).
//!
//! TDD specs for turning source chunks into vectors. Determinism is the
//! contract that makes exact-code search and reproducible indexing possible.

/// The same input yields the same vector.
///
/// Assert: embedding identical code twice produces identical vectors.
#[tokio::test]
async fn same_input_yields_same_vector() {
    todo!("assert deterministic embeddings");
}

/// Different inputs yield different vectors.
#[tokio::test]
async fn different_input_yields_different_vector() {
    todo!("assert distinct inputs -> distinct vectors");
}

/// The same text under different purposes yields different vectors.
///
/// Assert: `EmbeddingPurpose::Code` and `EmbeddingPurpose::Documentation`
///   produce distinct vectors for the same text.
#[tokio::test]
async fn purpose_differentiates_the_vector() {
    todo!("assert purpose changes the embedding");
}

/// Embeddings are non-empty and of the configured dimension.
///
/// Assert: every produced `Embedding` (a non-empty vec) has the model's
///   dimension and at least one element.
#[tokio::test]
async fn embeddings_are_nonempty_and_correctly_sized() {
    todo!("assert non-empty, correctly-dimensioned embeddings");
}
