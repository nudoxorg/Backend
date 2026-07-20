//! Pipeline part: **read-only search surface** (`driver::search::SearchTarget`).
//!
//! TDD specs for the abstract+literal query surface the server exposes over the
//! runtime stores. The precise arm is exercised for real over a replica-local
//! tantivy index (the exact surface a `SourceStores` target delegates to); the
//! semantic arm is asserted at the planner, the only place it can begin.

mod common;

use futures::StreamExt;
use heart::SymbolKind;
use driver::search::SearchPlanner;
use driver::search::planner::Plan;
use driver::search::query::{AbstractQuery, Filter, PackageSelector, Query, Search};
use driver::search::symbols::SymbolTextSurface;

/// A literal query searches the precise (tantivy) index.
///
/// Act: search the literal surface with `Query::Literal(..)`.
/// Assert: returns a stream of scored items matching the literal.
#[tokio::test]
async fn literal_query_hits_the_text_index() {
    let replica = common::TempDir::new("literal-hits");
    let package = common::package_id("serde");
    let symbol = common::rust_symbol(package, "Deserialize", "serde::Deserialize", SymbolKind::Trait);
    let index = common::populated_text_index(replica.path(), std::slice::from_ref(&symbol));

    let request = common::literal_search("Deserialize", 8);
    let Query::Literal(literal) = &request.query else { unreachable!("built literal") };
    let surface = SymbolTextSurface::over(&index);
    let hits = surface
        .search(literal, &request.page)
        .await
        .expect("a committed index answers literal queries");
    let hits: Vec<_> = hits.collect().await;

    let first = hits
        .first()
        .expect("the ingested symbol is found")
        .as_ref()
        .expect("the stream yields hits, not errors");
    assert_eq!(first.value.id, symbol.id, "the scored hit is the ingested symbol");
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

/// A scope filter restricts results by language and package.
///
/// Assert: a `Filter` with `language`/`package` set only yields matching items.
#[tokio::test]
async fn scope_filter_restricts_results() {
    let replica = common::TempDir::new("scope-filter");
    let serde = common::package_id("serde");
    let tokio_pkg = common::package_id("tokio");
    let wanted = common::rust_symbol(serde, "Deserialize", "serde::Deserialize", SymbolKind::Trait);
    let unwanted = common::rust_symbol(tokio_pkg, "Deserialize", "tokio::Deserialize", SymbolKind::Trait);
    let index = common::populated_text_index(replica.path(), &[wanted.clone(), unwanted]);

    let mut request = common::literal_search("Deserialize", 8);
    request.filter = Filter {
        ecosystems: nonempty::NonEmpty::from_vec(vec![heart::Language::Rust]),
        packages: nonempty::NonEmpty::from_vec(vec![PackageSelector {
            name: driver::registry::package::PackageName::new(heart::Language::Rust, "serde")
                .expect("fixture names are valid"),
            version: None,
        }]),
    };
    let Query::Literal(literal) = &request.query else { unreachable!("built literal") };
    let hits: Vec<_> = SymbolTextSurface::over(&index)
        .search(literal, &request.page)
        .await
        .expect("literal search succeeds")
        .collect()
        .await;
    let admitted: Vec<_> = hits
        .into_iter()
        .map(|hit| hit.expect("the stream yields hits, not errors"))
        .filter(|hit| request.filter.admits(&hit.value))
        .collect();

    assert_eq!(admitted.len(), 1, "only the serde-rooted symbol survives the package scope");
    assert_eq!(admitted[0].value.id, wanted.id);
}

/// Pagination bounds the number of results via limit/offset.
#[tokio::test]
async fn pagination_bounds_results() {
    let replica = common::TempDir::new("pagination");
    let package = common::package_id("serde");
    let symbols: Vec<_> = (0..5)
        .map(|index| {
            common::rust_symbol(
                package,
                "answer",
                &format!("serde::module{index}::answer"),
                SymbolKind::Function,
            )
        })
        .collect();
    let index = common::populated_text_index(replica.path(), &symbols);

    let request = common::literal_search("answer", 2);
    let Query::Literal(literal) = &request.query else { unreachable!("built literal") };
    let hits: Vec<_> = SymbolTextSurface::over(&index)
        .search(literal, &request.page)
        .await
        .expect("paged search succeeds")
        .collect()
        .await;

    assert_eq!(hits.len(), 2, "the page limit bounds the result count");
}

/// `get_by_id` fetches a single item by its durable global id.
///
/// Assert: returns `Some(item)` for a known id, `None` otherwise.
#[tokio::test]
async fn get_by_id_fetches_one() {
    let replica = common::TempDir::new("get-by-id");
    let package = common::package_id("serde");
    let symbol = common::rust_symbol(package, "Serialize", "serde::Serialize", SymbolKind::Trait);
    let index = common::populated_text_index(replica.path(), std::slice::from_ref(&symbol));
    let surface = SymbolTextSurface::over(&index);

    let found = surface.find(symbol.id).await.expect("identity lookup succeeds");
    assert_eq!(found.as_ref().map(|found| found.id), Some(symbol.id), "a known id resolves");

    let missing = surface.find(common::absent_symbol_id()).await.expect("lookup succeeds");
    assert!(missing.is_none(), "an unknown id resolves to None, not an error");
}

/// An abstract request over an unbounded filter and a page of eight.
fn abstract_search(query: AbstractQuery) -> Search<'static> {
    Search {
        query: Query::Abstract(query),
        filter: Filter::default(),
        page: common::page(8),
        _lifetime: std::marker::PhantomData,
    }
}
