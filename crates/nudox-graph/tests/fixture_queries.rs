//! Integration tests that run named queries against the rich fixture corpus.

use std::sync::Arc;

use nudox_store::{
    corpus::Corpus,
    package::{PackageView, Provenance},
    source::fixtures::{build_rich_view, rich_lineage},
};
use nudox_graph::adapter::CorpusAdapter;
use trustfall::{Schema, TryIntoStruct, execute_query_async};
use nudox_graph::queries::{
    FindSymbolByKeyRow, FindUsagesRow, FindImplementorsRow, ListPackageFunctionsRow,
    SymbolsMentioningTypeRow,
    FIND_SYMBOL_BY_KEY, FIND_USAGES, FIND_IMPLEMENTORS, LIST_PACKAGE_FUNCTIONS,
    SYMBOLS_MENTIONING_TYPE,
};
use futures::StreamExt as _;

fn schema() -> Schema {
    Schema::parse(include_str!("../schema.graphql")).expect("schema must parse")
}

async fn make_corpus() -> Corpus {
    let corpus = Corpus::new();
    let view = build_rich_view();
    let pkg = Arc::new(PackageView::build(view, Provenance::TrustedLocal));
    corpus.insert(pkg).await;
    corpus
}

/// Compute the StableRef key for fixture intro `n` in the rich fixture package.
fn rich_key(n: u8) -> String {
    use nudox_ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};
    let lineage = rich_lineage();
    let intro = IntroId::from_raw([n; 32]);
    StableRef::new(lineage, intro).to_string()
}

// ---------------------------------------------------------------------------
// find_symbol_by_key — intro 6 is the `distance` function
// ---------------------------------------------------------------------------

#[tokio::test]
async fn find_symbol_by_key_distance_function() {
    let corpus = make_corpus().await;
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new(corpus));
    let key = rich_key(6);

    let vars = [("key".to_string(), key.clone())];
    let mut stream = execute_query_async(
        &schema,
        adapter,
        FIND_SYMBOL_BY_KEY,
        vars.into_iter().collect(),
    )
    .expect("query must execute")
    .map(|row| row.expect("no stream error").try_into_struct::<FindSymbolByKeyRow>().expect("deserialize"));

    let row = stream.next().await.expect("at least one row");
    assert_eq!(row.key, key);
    assert_eq!(row.name, "distance");
    assert_eq!(row.kind, "Function");
}

// ---------------------------------------------------------------------------
// list_package_functions — should find at least `distance` and `format_point`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_package_functions_finds_functions() {
    let corpus = make_corpus().await;
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new(corpus));
    let package = rich_lineage().to_string();

    let vars = [("package".to_string(), package)];
    let results: Vec<ListPackageFunctionsRow> = execute_query_async(
        &schema,
        adapter,
        LIST_PACKAGE_FUNCTIONS,
        vars.into_iter().collect(),
    )
    .expect("query must execute")
    .map(|row| row.expect("no error").try_into_struct::<ListPackageFunctionsRow>().expect("deserialize"))
    .collect()
    .await;

    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"distance"), "distance not found in {names:?}");
    assert!(names.contains(&"format_point"), "format_point not found in {names:?}");
    // All rows must have receiverKind
    for row in &results {
        assert!(row.receiver_kind.is_some(), "receiverKind missing for {}", row.name);
    }
}

// ---------------------------------------------------------------------------
// find_usages — format_point (23) calls distance (6)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn find_usages_of_distance() {
    let corpus = make_corpus().await;
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new(corpus));
    let key = rich_key(6); // distance

    let vars = [("key".to_string(), key)];
    let results: Vec<FindUsagesRow> = execute_query_async(
        &schema,
        adapter,
        FIND_USAGES,
        vars.into_iter().collect(),
    )
    .expect("query must execute")
    .map(|row| row.expect("no error").try_into_struct::<FindUsagesRow>().expect("deserialize"))
    .collect()
    .await;

    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"format_point"),
        "format_point should be a user of distance; got {names:?}"
    );
}

// ---------------------------------------------------------------------------
// find_implementors — PointDisplay (9) implements Display (8)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn find_implementors_of_display() {
    let corpus = make_corpus().await;
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new(corpus));
    let key = rich_key(8); // Display trait

    let vars = [("key".to_string(), key)];
    let results: Vec<FindImplementorsRow> = execute_query_async(
        &schema,
        adapter,
        FIND_IMPLEMENTORS,
        vars.into_iter().collect(),
    )
    .expect("query must execute")
    .map(|row| row.expect("no error").try_into_struct::<FindImplementorsRow>().expect("deserialize"))
    .collect()
    .await;

    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"PointDisplay"),
        "PointDisplay should implement Display; got {names:?}"
    );
}

// ---------------------------------------------------------------------------
// symbols_mentioning_type — same as implementors for the mentions edge
// ---------------------------------------------------------------------------

#[tokio::test]
async fn symbols_mentioning_display() {
    let corpus = make_corpus().await;
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new(corpus));
    let key = rich_key(8); // Display

    let vars = [("key".to_string(), key)];
    let results: Vec<SymbolsMentioningTypeRow> = execute_query_async(
        &schema,
        adapter,
        SYMBOLS_MENTIONING_TYPE,
        vars.into_iter().collect(),
    )
    .expect("query must execute")
    .map(|row| row.expect("no error").try_into_struct::<SymbolsMentioningTypeRow>().expect("deserialize"))
    .collect()
    .await;

    // PointDisplay impl mentions Display as its of-trait.
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"PointDisplay"),
        "PointDisplay should mention Display; got {names:?}"
    );
}
