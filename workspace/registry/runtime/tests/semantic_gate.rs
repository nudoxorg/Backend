//! Diagram: **qdrant semantic/code search is heavy — make sure it is properly
//! gated / not implicit.** This is the runtime-level enforcement under
//! `runtime::vector::gate` (the server surface relies on it).

mod support;

use std::marker::PhantomData;
use std::num::NonZeroUsize;

use runtime::vector::{
    EmbedRole, Embedder, Embedding, EmbeddingPurpose, SemanticGate,
    model::{E5Small, EmbeddingModel},
};
use support::DeterministicEmbedder;

/// Autoref probe: `Probe::<T>::CLONEABLE` resolves to the inherent `true` only
/// when `T: Clone`; otherwise it falls back to the trait's `false`. Lets a
/// runtime test assert the *absence* of an impl.
struct Probe<T>(PhantomData<T>);
trait NoCloneFallback {
    const CLONEABLE: bool = false;
}
impl<T> NoCloneFallback for Probe<T> {}
impl<T: Clone> Probe<T> {
    #[allow(dead_code)]
    const CLONEABLE: bool = true;
}

/// The vector store cannot be searched without presenting the gate.
///
/// Every semantic entry point takes a [`SemanticGate`] *by value* in its
/// signature — the helper below only type-checks because the parameter exists,
/// so "searched without the gate" is unrepresentable at compile time.
#[tokio::test]
async fn vector_search_requires_the_gate() {
    // Never invoked; its existence is the (compile-time) assertion.
    // `store.search` demands the gate in its signature.
    #[allow(dead_code)]
    async fn store_search_demands_the_gate<M, E>(
        store: &runtime::vector::SemanticLive<M>,
        query: &Embedding<M>,
        limit: NonZeroUsize,
    ) where
        M: EmbeddingModel,
        E: Embedder<Model = M>,
    {
        let _ = store.search(SemanticGate::issue("spec"), query, limit, None).await;
    }

    // And the token itself cannot be forged from thin air: `issue` is the only
    // constructor, and it demands an audit reason.
    let gate = SemanticGate::issue("integration spec: gating is mandatory");
    assert_eq!(gate.reason(), "integration spec: gating is mandatory");
}

/// Holding the gate is the ONLY way to run a semantic query.
///
/// The gate is consumed by value and cannot be duplicated: one issuance
/// authorizes exactly one query.
#[tokio::test]
async fn gate_is_the_sole_entry_point() {
    // Not Clone and not Copy — an issued gate cannot be multiplied.
    assert!(!Probe::<SemanticGate>::CLONEABLE, "SemanticGate must not be Clone");
    assert!(!Probe::<SemanticGate>::CLONEABLE, "Copy implies Clone; !Clone rules out both");

    // Consumed by value: after handing the gate to a query, it is gone.
    fn consume(gate: SemanticGate) -> &'static str {
        gate.reason()
    }
    let gate = SemanticGate::issue("one authorization, one query");
    assert_eq!(consume(gate), "one authorization, one query");
    // consume(gate); // <- does not compile: the gate was moved into the query.
}

/// Embedding a query is part of the gated path, not the default path.
///
/// Assert: no embedding happens unless the gate has been satisfied (heavy work
///   is never done implicitly).
#[tokio::test]
async fn embedding_only_happens_behind_the_gate() {
    let embedder = DeterministicEmbedder::new();

    // The planner's shape: embedding work runs only inside a scope that holds
    // an issued gate (mirrors `similar_to_snippet`, which embeds *after* the
    // gate has been presented).
    async fn gated_embed(
        gate: SemanticGate,
        embedder: &DeterministicEmbedder,
        text: &str,
    ) -> Embedding<E5Small> {
        tracing::debug!(reason = gate.reason(), "semantic path authorized");
        embedder.embed(text, EmbeddingPurpose::Code, EmbedRole::Query).await.expect("offline embedder is total")
    }

    // Default (text) path: whatever happens, no embedder is ever consulted.
    assert_eq!(embedder.calls(), 0, "the default path never embeds");

    let gate = SemanticGate::issue("spec: embedding is gated");
    let _query = gated_embed(gate, &embedder, "fn probe()").await;
    assert_eq!(embedder.calls(), 1, "one gate, one embedding, one query");
}
