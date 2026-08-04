//! Diagram rule: **qdrant semantic search is heavy and must be explicitly gated
//! — never implicit.** The default plan is precise (literal); semantic only
//! runs under an issued gate.
//!
//! Specs for `driver::search::planner` + `registry::vector::SemanticGate`. The
//! gate itself is type-enforced (`Semantic::search` consumes a `SemanticGate`
//! by value — there is no ungated entry point to compile against), so these
//! specs pin the planner: the one place a gate may be minted.

#[allow(unused_imports)]
use driver::{registry};
mod common;

use std::time::Duration;

use driver::search::SearchPlanner;
use driver::search::planner::Plan;
use driver::search::query::{AbstractQuery, Filter, Query, Search};

/// A plain search request uses the precise surface, not qdrant.
///
/// Act: plan a default search (no semantic opt-in ⇒ a literal query).
/// Assert: the plan is precise — no gate is minted, so the qdrant/embedder
///   path is unreachable for this request.
#[tokio::test]
async fn default_search_is_text_not_semantic() {
    let planner = SearchPlanner::new();
    let request = common::literal_search("Deserialize", 8);
    assert!(
        matches!(planner.plan(&request), Plan::Precise),
        "a literal (default) query must never be planned semantically"
    );
}

/// Semantic search requires an explicit opt-in / gate token.
///
/// Act: request semantic search when the planner will not authorize it
///   (a zero-budget planner — the policy says no).
/// Assert: no gate is issued; the request degrades to the precise surface
///   rather than reaching qdrant.
#[tokio::test]
async fn semantic_search_without_gate_is_rejected() {
    let never_semantic = SearchPlanner::with_quota(0, Duration::from_secs(60));
    let request = natural_language_search("what parses TOML into a struct");
    assert!(
        matches!(never_semantic.plan(&request), Plan::Precise),
        "without an authorization the semantic surface must be unreachable"
    );
}

/// Semantic search runs only once the gate is satisfied.
///
/// Act: plan an abstract query under budget, then spend the gate against the
///   live semantic surface when the backend stack is opted in.
/// Assert: the plan carries a gate; with backends, the gated query embeds and
///   answers (an empty corpus answering cleanly is the contract).
#[tokio::test]
async fn semantic_search_with_gate_runs() {
    let planner = SearchPlanner::new();
    let request = natural_language_search("deserialize json into a struct");
    let Plan::Semantic(gate) = planner.plan(&request) else {
        panic!("an abstract query under budget must be planned semantically");
    };
    assert!(
        gate.reason().contains("natural-language"),
        "the gate records why the expensive path was chosen"
    );

    // The spend half needs live qdrant + an embedder — opt-in only.
    let Some((server, _data)) = common::assembled_server("semantic_search_with_gate_runs").await
    else {
        return;
    };
    let cap = common::read_cap(&server);
    let hits: Vec<_> = {
        use futures::StreamExt;
        let stream = server
            .search_symbols(&cap, &request)
            .await
            .expect("a gated semantic search over an empty corpus answers cleanly");
        stream.collect().await
    };
    assert!(
        hits.into_iter().all(|hit| hit.is_ok()),
        "an empty corpus yields no errors, only an empty page"
    );
}

/// The semantic path never runs implicitly as a fallback of text search.
///
/// Assert: a literal query stays precise even when the semantic budget is
///   wide open and even when repeated — escalation only ever flows
///   abstract→precise (degrade), never precise→abstract.
#[tokio::test]
async fn text_search_does_not_implicitly_escalate_to_semantic() {
    let planner = SearchPlanner::new();
    let request = common::literal_search("nonexistent_symbol_zzz", 8);
    for _ in 0..3 {
        assert!(
            matches!(planner.plan(&request), Plan::Precise),
            "an empty text result must not silently escalate to a semantic query"
        );
    }

    // The degrade direction is real: once the budget is spent, abstract
    // queries fall back to precise — the only sanctioned crossing.
    let one_shot = SearchPlanner::with_quota(1, Duration::from_secs(60));
    let abstract_request = natural_language_search("streaming parser");
    assert!(matches!(one_shot.plan(&abstract_request), Plan::Semantic(_)));
    assert!(
        matches!(one_shot.plan(&abstract_request), Plan::Precise),
        "an exhausted budget degrades abstract queries to the precise surface"
    );
}

/// An abstract natural-language request over an unbounded filter.
fn natural_language_search(text: &str) -> Search<'static> {
    Search {
        query: Query::Abstract(AbstractQuery::NaturalLanguage(text.into())),
        filter: Filter::default(),
        page: common::page(8),
        _lifetime: std::marker::PhantomData,
    }
}
