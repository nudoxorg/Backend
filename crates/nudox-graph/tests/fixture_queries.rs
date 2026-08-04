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

// ---------------------------------------------------------------------------
// Occurrence → target edge (Limit 1 fix)
//
// format_point (intro 23) has an Oracle occurrence targeting distance (intro 6).
// The query traverses occurrencesOf { target { name kind } } in one hop.
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
struct OccurrenceTargetRow {
    owner_name: String,
    target_name: String,
    target_kind: String,
}

#[tokio::test]
async fn occurrence_target_edge_resolves_to_symbol() {
    let corpus = make_corpus().await;
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new(corpus));

    // format_point is intro 23; its occurrence targets distance (intro 6).
    let key = rich_key(23); // format_point

    let vars = [("key".to_string(), key)];
    let results: Vec<OccurrenceTargetRow> = execute_query_async(
        &schema,
        adapter,
        r#"{
            Symbols {
                key @filter(op: "=", value: ["$key"])
                name @output(name: "owner_name")
                occurrencesOf {
                    target {
                        name @output(name: "target_name")
                        kind @output(name: "target_kind")
                    }
                }
            }
        }"#,
        vars.into_iter().collect(),
    )
    .expect("query must execute")
    .map(|row| {
        row.expect("no stream error")
            .try_into_struct::<OccurrenceTargetRow>()
            .expect("deserialize")
    })
    .collect()
    .await;

    // format_point has two occurrences: one targets distance, one targets Point.
    // Both targets must be resolved because they live in the same (loaded) package.
    assert!(
        !results.is_empty(),
        "format_point has occurrences with targets in the fixture corpus; got no rows"
    );

    let target_names: Vec<&str> = results.iter().map(|r| r.target_name.as_str()).collect();
    assert!(
        target_names.contains(&"distance"),
        "distance must appear as a target of format_point's occurrences; got {target_names:?}"
    );

    // All returned rows must have a non-empty kind (validates the Symbol edge
    // materialised properly — an empty kind would mean the vertex resolved but
    // property resolution failed silently).
    for row in &results {
        assert!(
            !row.target_kind.is_empty(),
            "target_kind must be non-empty for every resolved occurrence target; row: {row:?}"
        );
    }
}

/// `targetKey` must still be readable alongside `target` — backward-compat guard.
#[tokio::test]
async fn target_key_and_target_edge_coexist() {
    let corpus = make_corpus().await;
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new(corpus));

    // Query both targetKey and target.name in one pass.
    #[derive(Debug, serde::Deserialize)]
    struct Row {
        target_key: String,
        target_name: String,
    }

    let key = rich_key(23); // format_point
    let vars = [("key".to_string(), key)];
    let results: Vec<Row> = execute_query_async(
        &schema,
        adapter,
        r#"{
            Symbols {
                key @filter(op: "=", value: ["$key"])
                occurrencesOf {
                    targetKey @output(name: "target_key")
                    target {
                        name @output(name: "target_name")
                    }
                }
            }
        }"#,
        vars.into_iter().collect(),
    )
    .expect("query must execute")
    .map(|row| row.expect("no error").try_into_struct::<Row>().expect("deserialize"))
    .collect()
    .await;

    assert!(
        !results.is_empty(),
        "format_point must have occurrences; got empty results"
    );

    for row in &results {
        // targetKey must be a non-empty key string.
        assert!(
            !row.target_key.is_empty(),
            "targetKey must be non-empty; row: {row:?}"
        );
        // target.name must also be populated when the target resolves.
        assert!(
            !row.target_name.is_empty(),
            "target.name must be non-empty when the target package is loaded; row: {row:?}"
        );
        // The key must be parseable as ecosystem:name#hex.
        assert!(
            row.target_key.contains('#'),
            "targetKey must be in ecosystem:name#introhex form; got {:?}",
            row.target_key
        );
    }
}

// ---------------------------------------------------------------------------
// Type coercions for formerly-collapsed kinds (Limit 2 fix)
//
// intro(11) is Color::Red — a Variant.
// intro(15) is REGISTRY — a Static.
// intro(1)  is nudox-fixture-rich — a Module.
// intro(22) is reexport_distance — a Reexport.
// intro(7)  is distance.p — a Param.
// ---------------------------------------------------------------------------

/// `... on Variant { }` must match Color::Red/Green/Blue and return rows.
///
/// Before the fix, Variant entries were emitted as OtherSymbol and the
/// coercion would silently return nothing.
#[tokio::test]
async fn variant_coercion_matches_color_variants() {
    let corpus = make_corpus().await;
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new(corpus));

    #[derive(Debug, serde::Deserialize)]
    struct Row {
        name: String,
        kind: String,
    }

    let results: Vec<Row> = execute_query_async(
        &schema,
        adapter,
        r#"{
            Symbols {
                ... on Variant {
                    name @output
                    kind @output
                }
            }
        }"#,
        std::collections::BTreeMap::new(),
    )
    .expect("query must execute")
    .map(|row| row.expect("no error").try_into_struct::<Row>().expect("deserialize"))
    .collect()
    .await;

    // The fixture has Red, Green, Blue as Variant entries.
    assert!(
        !results.is_empty(),
        "... on Variant {{ }} must return Color::Red/Green/Blue; got empty results. \
         If empty, the adapter is still emitting OtherSymbol for Variant."
    );

    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    for expected in ["Red", "Green", "Blue"] {
        assert!(
            names.contains(&expected),
            "{expected} must appear in Variant coercion results; got {names:?}"
        );
    }

    // Every row must have kind=Variant.
    for row in &results {
        assert_eq!(
            row.kind, "Variant",
            "every row from ... on Variant must have kind=Variant; got {:?}",
            row.kind
        );
    }
}

/// `... on Static { }` must match REGISTRY and return rows.
#[tokio::test]
async fn static_coercion_matches_registry() {
    let corpus = make_corpus().await;
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new(corpus));

    #[derive(Debug, serde::Deserialize)]
    struct Row {
        name: String,
    }

    let results: Vec<Row> = execute_query_async(
        &schema,
        adapter,
        r#"{
            Symbols {
                ... on Static {
                    name @output
                }
            }
        }"#,
        std::collections::BTreeMap::new(),
    )
    .expect("query must execute")
    .map(|row| row.expect("no error").try_into_struct::<Row>().expect("deserialize"))
    .collect()
    .await;

    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"REGISTRY"),
        "REGISTRY (kind=Static) must appear in ... on Static coercion; got {names:?}. \
         If empty, the adapter is still emitting OtherSymbol for Static."
    );
}

/// `... on Module { }` must match the root module and the fmt submodule.
#[tokio::test]
async fn module_coercion_matches_modules() {
    let corpus = make_corpus().await;
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new(corpus));

    #[derive(Debug, serde::Deserialize)]
    struct Row {
        name: String,
    }

    let results: Vec<Row> = execute_query_async(
        &schema,
        adapter,
        r#"{
            Symbols {
                ... on Module {
                    name @output
                }
            }
        }"#,
        std::collections::BTreeMap::new(),
    )
    .expect("query must execute")
    .map(|row| row.expect("no error").try_into_struct::<Row>().expect("deserialize"))
    .collect()
    .await;

    assert!(
        !results.is_empty(),
        "... on Module {{ }} must return at least the root and fmt modules; got empty. \
         If empty, the adapter is still emitting OtherSymbol for Module."
    );
}

/// `... on Reexport { }` must match reexport_distance.
#[tokio::test]
async fn reexport_coercion_matches_reexports() {
    let corpus = make_corpus().await;
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new(corpus));

    #[derive(Debug, serde::Deserialize)]
    struct Row {
        name: String,
    }

    let results: Vec<Row> = execute_query_async(
        &schema,
        adapter,
        r#"{
            Symbols {
                ... on Reexport {
                    name @output
                }
            }
        }"#,
        std::collections::BTreeMap::new(),
    )
    .expect("query must execute")
    .map(|row| row.expect("no error").try_into_struct::<Row>().expect("deserialize"))
    .collect()
    .await;

    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"reexport_distance"),
        "reexport_distance must appear in ... on Reexport coercion; got {names:?}. \
         If empty, the adapter is still emitting OtherSymbol for Reexport."
    );
}

/// `... on Param { }` must match distance.p.
#[tokio::test]
async fn param_coercion_matches_params() {
    let corpus = make_corpus().await;
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new(corpus));

    #[derive(Debug, serde::Deserialize)]
    struct Row {
        name: String,
    }

    let results: Vec<Row> = execute_query_async(
        &schema,
        adapter,
        r#"{
            Symbols {
                ... on Param {
                    name @output
                }
            }
        }"#,
        std::collections::BTreeMap::new(),
    )
    .expect("query must execute")
    .map(|row| row.expect("no error").try_into_struct::<Row>().expect("deserialize"))
    .collect()
    .await;

    // The fixture has `p` (intro 7) as a Param child of `distance`.
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"p"),
        "distance.p (kind=Param) must appear in ... on Param coercion; got {names:?}. \
         If empty, the adapter is still emitting OtherSymbol for Param."
    );
}
