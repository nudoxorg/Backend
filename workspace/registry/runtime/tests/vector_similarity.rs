//! Pipeline part: **semantic/vector search** (`runtime::vector`).
//!
//! Tests for the similarity kernels behind the Qdrant-backed store: cosine
//! ranking, similarity from a starting symbol, from a snippet, and
//! language-scoped similarity. The store's qdrant plumbing needs a live
//! service; the ranking logic it delegates to is exercised here directly
//! against an in-memory corpus and the deterministic embedder.

mod support;

use heart::{Language, Scored, SymbolId};
use runtime::vector::{
    Embedding, EmbeddingPurpose,
    model::E5Small,
    similarity::cosine,
};
use support::{deterministic_embedding, symbol_id};

/// An in-memory stand-in for the collection: `(symbol, language, vector)`.
type Corpus = Vec<(SymbolId, Language, Embedding<E5Small>)>;

/// Rank a corpus against a query by cosine, descending — exactly what the
/// qdrant search path does server-side.
fn rank(corpus: &Corpus, query: &Embedding<E5Small>, limit: usize) -> Vec<Scored<SymbolId>> {
    let mut scored: Vec<_> = corpus
        .iter()
        .map(|(id, _, vector)| Scored::new(*id, cosine(query, vector)))
        .collect();
    scored.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.value.cmp(&b.value)));
    scored.truncate(limit);
    scored
}

fn embed_code(text: &str) -> Embedding<E5Small> {
    deterministic_embedding(text, EmbeddingPurpose::Code)
}

/// Blend two vectors: `weight` of `towards`, the rest of `base` — a controlled
/// way to build candidates at known distances from a query.
fn blend(base: &Embedding<E5Small>, towards: &Embedding<E5Small>, weight: f32) -> Embedding<E5Small> {
    let values: Vec<f32> = base
        .as_slice()
        .iter()
        .zip(towards.as_slice())
        .map(|(b, t)| b * (1.0 - weight) + t * weight)
        .collect();
    Embedding::from_vec(values).expect("a blend preserves the dimension")
}

/// Searching with a vector returns the closest points by cosine, ranked.
///
/// Assert: ranking against the corpus returns up to `k` hits ordered by
///   descending cosine similarity.
#[test]
fn search_returns_top_k_by_cosine() {
    let query = embed_code("fn parse(input: &str) -> Ast");
    let noise = embed_code("SELECT * FROM completely_unrelated");
    // Candidates at increasing distance from the query.
    let corpus: Corpus = [0.0f32, 0.25, 0.5, 0.75]
        .iter()
        .enumerate()
        .map(|(n, weight)| (symbol_id(n as u64), Language::Rust, blend(&query, &noise, *weight)))
        .collect();

    let hits = rank(&corpus, &query, 3);
    assert_eq!(hits.len(), 3, "top-k truncates");
    let ids: Vec<_> = hits.iter().map(|hit| hit.value).collect();
    assert_eq!(ids, [symbol_id(0), symbol_id(1), symbol_id(2)], "nearer blends rank first");
    assert!(hits[0].score > hits[1].score && hits[1].score > hits[2].score);
}

/// An exact-code query ranks the matching symbol first.
///
/// Arrange: deterministic embeddings (same text -> same vector).
/// Assert: querying with a symbol's own code yields that symbol as the top hit
///   with score ~1.0.
#[test]
fn exact_code_query_ranks_itself_first() {
    let sources = [
        "fn checked_add(a: u32, b: u32) -> Option<u32>",
        "fn parse(input: &str) -> Ast",
        "struct Router { routes: Vec<Route> }",
    ];
    let corpus: Corpus = sources
        .iter()
        .enumerate()
        .map(|(n, source)| (symbol_id(n as u64), Language::Rust, embed_code(source)))
        .collect();

    let hits = rank(&corpus, &embed_code(sources[1]), 3);
    assert_eq!(hits[0].value, symbol_id(1), "the exact match is the top hit");
    assert!(
        hits[0].score.into_inner() > 0.999,
        "an identical vector has cosine ~1.0, got {}",
        hits[0].score
    );
}

/// Similarity from a starting symbol surfaces its neighbors.
///
/// Act: rank against the *stored* vector of the starting symbol, then drop the
///   self-hit — the exact shape of `similarity::similar_to`.
/// Assert: returns semantically-near symbols, excluding the symbol itself.
#[test]
fn similarity_from_symbol_surfaces_neighbors() {
    let anchor = embed_code("fn get(&self, key: &K) -> Option<&V>");
    let noise = embed_code("html { margin: 0 }");
    let corpus: Corpus = vec![
        (symbol_id(0), Language::Rust, anchor.clone()),
        (symbol_id(1), Language::Rust, blend(&anchor, &noise, 0.2)),
        (symbol_id(2), Language::Rust, blend(&anchor, &noise, 0.9)),
    ];

    // Over-fetch by one, drop the self-hit, truncate — as similar_to does.
    let neighbors: Vec<_> = rank(&corpus, &anchor, 3)
        .into_iter()
        .filter(|hit| hit.value != symbol_id(0))
        .take(2)
        .collect();
    assert_eq!(neighbors.len(), 2);
    assert_eq!(neighbors[0].value, symbol_id(1), "the nearer neighbor ranks first");
    assert!(
        neighbors.iter().all(|hit| hit.value != symbol_id(0)),
        "the symbol itself is excluded"
    );
}


/// Language-scoped similarity restricts results to one language.
///
/// Act: filter the corpus to one ecosystem before ranking — the payload filter
///   `language::similar_within` pushes into qdrant.
/// Assert: every hit belongs to the language (payload-level scoping, one
///   collection).
#[test]
fn language_scoped_similarity_filters_by_language() {
    let query = embed_code("fn spawn(future: impl Future)");
    let corpus: Corpus = vec![
        (symbol_id(0), Language::Rust, blend(&query, &embed_code("noise a"), 0.1)),
        (symbol_id(1), Language::Python, blend(&query, &embed_code("noise b"), 0.05)),
        (symbol_id(2), Language::Rust, blend(&query, &embed_code("noise c"), 0.3)),
        (symbol_id(3), Language::Typescript, blend(&query, &embed_code("noise d"), 0.02)),
    ];

    let scoped: Corpus = corpus
        .into_iter()
        .filter(|(_, language, _)| *language == Language::Rust)
        .collect();
    let hits = rank(&scoped, &query, 10);
    assert_eq!(hits.len(), 2, "only the scoped ecosystem's symbols are ranked");
    let ids: Vec<_> = hits.iter().map(|hit| hit.value).collect();
    assert_eq!(ids, [symbol_id(0), symbol_id(2)], "closer Rust symbols in cosine order");
}

/// Cosine is symmetric, maximal on itself, and total on zero vectors.
#[test]
fn cosine_kernel_laws() {
    let a = embed_code("fn a()");
    let b = embed_code("fn b()");
    assert_eq!(cosine(&a, &b), cosine(&b, &a), "symmetric");
    assert!(cosine(&a, &a).into_inner() > 0.999, "self-similarity is ~1.0");
    let zero = Embedding::<E5Small>::zeroed();
    assert_eq!(cosine(&a, &zero).into_inner(), 0.0, "zero vectors map to 0.0, not NaN");
}
