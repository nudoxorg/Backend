#![cfg(feature = "server")]
//! Pipeline part: **read-only search surface** (`index::server::search::SearchTarget`).
//!
//! TDD specs for the abstract+literal query surface the server exposes. Precise
//! (literal) symbol search no longer has a backend after the symbol-tantivy
//! drop; these specs pin the planner routing and the empty precise surface.

mod server_common;

use heart::SymbolKind;
use heart::client::query::{
    AbstractQuery, ExecutionQuery as Query, Filter, PackageSelector, Search,
};
use index::ecosystem::{FilterExt as _, PackageNameExt as _, PackageSelectorExt as _};
use index::server::search::SearchPlanner;
use index::server::search::planner::Plan;

/// A literal query plans to the precise surface (no semantic gate).
///
/// Assert: the planner never escalates a literal to qdrant.
#[tokio::test]
async fn literal_query_plans_precise() {
    let request = server_common::literal_search("Deserialize", 8);
    assert!(
        matches!(request.query, Query::Literal(_)),
        "fixture builds a literal query"
    );
    assert!(
        matches!(SearchPlanner::new().plan(&request), Plan::Precise),
        "a literal query must never be planned semantically"
    );
}

/// A natural-language query goes through the semantic (qdrant) store.
///
/// Act: plan `Query::Abstract(AbstractQuery::NaturalLanguage(..))`.
/// Assert: the planner routes it to the semantic surface — a gate is minted,
///   which is the sole entry to the embed-and-match path.
#[tokio::test]
async fn natural_language_query_is_semantic() {
    let planner = SearchPlanner::new();
    let request = abstract_search(AbstractQuery::NaturalLanguage(
        "how do I deserialize a struct from JSON".into(),
    ));
    assert!(
        matches!(planner.plan(&request), Plan::Semantic(_)),
        "a natural-language query under budget must plan semantically"
    );
}

/// A code-snippet query is treated as an abstract semantic query.
#[tokio::test]
async fn code_snippet_query_is_semantic() {
    let planner = SearchPlanner::new();
    let request = abstract_search(AbstractQuery::CodeSnippet {
        ecosystem: Some(heart::Language::Rust),
        code: "serde_json::from_str::<Config>(&text)?".into(),
    });
    assert!(
        matches!(planner.plan(&request), Plan::Semantic(_)),
        "a code-snippet query under budget must plan semantically"
    );
}

/// A scope filter only admits matching symbols (unit check on Filter).
#[tokio::test]
async fn scope_filter_restricts_results() {
    let serde = server_common::package_id("serde");
    let tokio_pkg = server_common::package_id("tokio");
    let wanted = server_common::rust_symbol(
        serde,
        "Deserialize",
        "serde::Deserialize",
        SymbolKind::Trait,
    );
    let unwanted = server_common::rust_symbol(
        tokio_pkg,
        "Deserialize",
        "tokio::Deserialize",
        SymbolKind::Trait,
    );

    let filter = Filter {
        ecosystems: nonempty::NonEmpty::from_vec(vec![heart::Language::Rust]),
        packages: nonempty::NonEmpty::from_vec(vec![PackageSelector {
            name: index::server::registry::package::PackageName::new(
                heart::Language::Rust,
                "serde",
            )
            .expect("fixture names are valid"),
            version: None,
        }]),
    };

    assert!(
        filter.admits(&wanted),
        "serde-rooted symbol survives package scope"
    );
    assert!(
        !filter.admits(&unwanted),
        "tokio-rooted symbol is filtered out"
    );
}

/// An abstract request over an unbounded filter and a page of eight.
fn abstract_search(query: AbstractQuery) -> Search<'static> {
    Search {
        query: Query::Abstract(query),
        filter: Filter::default(),
        page: server_common::page(8),
        _lifetime: std::marker::PhantomData,
    }
}
