//! Pipeline part: **embeddings** (`runtime::vector::embedding`).
//!
//! Tests for turning source chunks into vectors, run against the offline
//! deterministic embedder. Determinism is the contract that makes exact-code
//! search and reproducible indexing possible.

mod support;

use runtime::vector::{
    EmbedRole, Embedder, EmbeddingPurpose,
    model::{E5Small, EmbeddingModel},
};
use support::DeterministicEmbedder;

/// The same input yields the same vector.
///
/// Assert: embedding identical code twice produces identical vectors.
#[tokio::test]
async fn same_input_yields_same_vector() {
    let embedder = DeterministicEmbedder::new();
    let code = "fn checked_add(a: u32, b: u32) -> Option<u32> { a.checked_add(b) }";

    let first = embedder.embed(code, EmbeddingPurpose::Code, EmbedRole::Document).await.expect("embed succeeds");
    let second = embedder.embed(code, EmbeddingPurpose::Code, EmbedRole::Document).await.expect("embed succeeds");
    assert_eq!(first, second, "embedding is a pure function of (text, purpose)");
}

/// Different inputs yield different vectors.
#[tokio::test]
async fn different_input_yields_different_vector() {
    let embedder = DeterministicEmbedder::new();

    let add = embedder.embed("fn add(a: u32, b: u32) -> u32", EmbeddingPurpose::Code, EmbedRole::Document).await;
    let mul = embedder.embed("fn mul(a: u32, b: u32) -> u32", EmbeddingPurpose::Code, EmbedRole::Document).await;
    assert_ne!(
        add.expect("embed succeeds"),
        mul.expect("embed succeeds"),
        "distinct inputs decorrelate"
    );
}

/// The same text under different purposes yields different vectors.
///
/// Assert: `EmbeddingPurpose::Code` and `EmbeddingPurpose::Documentation`
///   produce distinct vectors for the same text.
#[tokio::test]
async fn purpose_differentiates_the_vector() {
    let embedder = DeterministicEmbedder::new();
    let text = "Router routes requests to handlers.";

    let as_code = embedder.embed(text, EmbeddingPurpose::Code, EmbedRole::Document).await.expect("embed succeeds");
    let as_docs =
        embedder.embed(text, EmbeddingPurpose::Documentation, EmbedRole::Document).await.expect("embed succeeds");
    assert_ne!(as_code, as_docs, "purpose is part of the embedding identity");
}

/// Embeddings are non-empty and of the configured dimension.
///
/// Assert: every produced `Embedding` (a non-empty vec) has the model's
///   dimension and at least one element.
#[tokio::test]
async fn embeddings_are_nonempty_and_correctly_sized() {
    let embedder = DeterministicEmbedder::new();

    let batch = embedder
        .embed_batch(&["fn a()", "fn b()", "struct C;"], EmbeddingPurpose::Code, EmbedRole::Document)
        .await
        .expect("batch embed succeeds");
    assert_eq!(batch.len(), 3, "batch results align positionally with inputs");
    for embedding in &batch {
        assert!(!embedding.as_slice().is_empty());
        assert_eq!(embedding.as_slice().len(), E5Small::DIMENSIONS);
    }
}
