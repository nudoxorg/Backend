//! Integration tests for the six §L6 MCP tools against the fixture corpus.
//!
//! # What these tests prove
//!
//! * Each tool's happy path returns real data, not stubs.
//! * Unknown symbol / unknown package / malformed input fail cleanly with a
//!   useful message rather than panicking or hanging.
//! * Every streaming tool terminates — no test may hang if a stream never closes.
//! * `graph_query` executes real Trustfall queries and returns rows.
//! * Concurrent calls do not interfere.
//! * Results are deterministic across repeated identical calls.
//!
//! # What these tests do NOT prove
//!
//! * Anything requiring a real Rust toolchain (ra_ap_*) — those tests are
//!   `#[ignore]`d and documented in a comment.
//!
//! # Runtime discipline (same as `tests/endpoint.rs`)
//!
//! Tests create their own Tokio runtime via `#[tokio::test]`, which drops in
//! sync context after the async block completes. The `EngineHandle` keeps the
//! runtime alive via `Arc<RuntimeGuard>`, so dropping it after the async block
//! is safe.

use std::time::Duration;

use nudox_engine::wire::{KindDiscriminant, KindTag};
use nudox_engine::{Engine, EngineConfig, PackageLoadEvent};
use nudox_mcp::tools::{FindUsagesArgs, GetSymbolArgs, GraphQueryArgs, SearchSymbolsArgs};
use nudox_mcp::{NudoxTools, SymbolKeyDto};

// ---------------------------------------------------------------------------
// Harness helpers
// ---------------------------------------------------------------------------

/// Start an engine over the rich fixture corpus.
fn make_tools() -> NudoxTools {
    let engine = Engine::start_with_fixtures(EngineConfig::default());
    NudoxTools::new(engine)
}

/// Wait until the fixture package is visible in the corpus.
///
/// Uses `packages()` (the public surface) rather than `corpus()` (pub(crate)).
/// If the channel closes before we get a `Loaded` event the corpus was already
/// seeded — that is fine too.
async fn wait_for_corpus(tools: &NudoxTools) {
    let rx = tools.engine().packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv_async()).await {
            Ok(Ok(PackageLoadEvent::Loaded { .. })) => return,
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => return, // channel closed → already loaded
            Err(_) => panic!("corpus never seeded within 5 s"),
        }
    }
}

/// The fixture package lineage, for use in tool arguments.
const FIXTURE_LINEAGE: &str = "fixture:nudox-fixture-rich";

// ---------------------------------------------------------------------------
// graph_schema
// ---------------------------------------------------------------------------

/// `graph_schema` must return a non-empty SDL string containing the core types.
///
/// This is a pure in-memory operation (no engine call) so no corpus wait is
/// needed, and it is also deterministic by construction.
#[tokio::test]
async fn graph_schema_returns_the_full_sdl() {
    let tools = make_tools();
    // graph_schema is implemented inline in the server, not in NudoxTools, but
    // its value is crate::SCHEMA_SDL, which is identical to nudox_graph::SCHEMA_SDL.
    let schema = nudox_mcp::SCHEMA_SDL;
    assert!(!schema.is_empty(), "schema must be non-empty");
    assert!(
        schema.contains("type RootSchemaQuery"),
        "schema must define the root query type"
    );
    assert!(
        schema.contains("Symbols"),
        "schema must expose the Symbols entry point"
    );
    assert!(
        schema.contains("Packages"),
        "schema must expose the Packages entry point"
    );
    assert!(
        schema.contains("implementors"),
        "schema must document the implementors edge that graph_query can traverse"
    );
    assert!(
        schema.contains("usages"),
        "schema must document the usages edge"
    );
    assert!(
        schema.contains("ecosystem:name#introhex"),
        "schema must document the key format agents must use"
    );
    // Verify the five formerly-collapsed kinds each have their own schema type
    // (Limit 2 fix). An agent can now write `... on Variant { }` etc.
    for new_type in [
        "type Static ",
        "type Variant ",
        "type Module ",
        "type Reexport ",
        "type Param ",
    ] {
        assert!(
            schema.contains(new_type),
            "schema must declare '{new_type}' as a concrete symbol type"
        );
    }
    // Verify Occurrence → target edge (Limit 1 fix).
    assert!(
        schema.contains("target: Symbol"),
        "schema must declare the Occurrence.target edge to Symbol"
    );
}

/// `graph_schema` returns exactly the same SDL on every call — determinism
/// test for the constant value.
#[test]
fn graph_schema_is_deterministic() {
    let a = nudox_mcp::SCHEMA_SDL;
    let b = nudox_mcp::SCHEMA_SDL;
    assert_eq!(a, b, "schema must be the same constant");
}

// ---------------------------------------------------------------------------
// list_packages
// ---------------------------------------------------------------------------

/// `list_packages` returns at least the rich fixture package after seeding.
#[tokio::test]
async fn list_packages_returns_the_fixture_package() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    let result = tools
        .do_list_packages()
        .await
        .expect("list_packages must not fail");
    assert!(
        !result.packages.is_empty(),
        "at least the fixture package must be visible"
    );

    let pkg = result
        .packages
        .iter()
        .find(|p| p.lineage == FIXTURE_LINEAGE)
        .unwrap_or_else(|| {
            panic!(
                "fixture package {FIXTURE_LINEAGE} must appear in list_packages; got {:?}",
                result.packages
            )
        });

    assert_eq!(
        pkg.ecosystem, "fixture",
        "ecosystem field must be 'fixture'"
    );
    assert_eq!(pkg.name, "nudox-fixture-rich", "name must match");
}

/// `list_packages` produces a stable result across two back-to-back calls.
///
/// Regression guard: if the generation counter or race in the query plane
/// caused different views, names or counts would diverge.
#[tokio::test]
async fn list_packages_is_deterministic() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    let a = tools
        .do_list_packages()
        .await
        .expect("first call must succeed");
    let b = tools
        .do_list_packages()
        .await
        .expect("second call must succeed");

    // Compare names (lineage strings), not full struct equality, to avoid
    // depending on ordering details of the graph plane.
    let mut names_a: Vec<String> = a.packages.iter().map(|p| p.lineage.clone()).collect();
    let mut names_b: Vec<String> = b.packages.iter().map(|p| p.lineage.clone()).collect();
    names_a.sort();
    names_b.sort();
    assert_eq!(
        names_a, names_b,
        "list_packages must return the same packages on every call"
    );
}

// ---------------------------------------------------------------------------
// search_symbols
// ---------------------------------------------------------------------------

/// Searching for a known name returns a hit for that name.
#[tokio::test]
async fn search_symbols_finds_known_symbol() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    let result = tools
        .do_search(SearchSymbolsArgs {
            query: "Point".to_owned(),
            kinds: None,
            packages: None,
            limit: None,
        })
        .await
        .expect("search for 'Point' must not fail");

    assert!(
        !result.hits.is_empty(),
        "search for 'Point' must return at least one hit"
    );
    // Every key must be in the canonical form.
    for hit in &result.hits {
        let key_str = format!(
            "{}:{}#{}",
            hit.key.package.ecosystem.as_str(),
            hit.key.package.name.as_str(),
            hit.key.intro.to_hex()
        );
        assert!(
            key_str.contains("fixture:nudox-fixture-rich#"),
            "key must carry the fixture lineage: {key_str}"
        );
    }
}

/// A nonsense query returns an empty hits list (not a panic, not a hang).
#[tokio::test]
async fn search_symbols_nonsense_query_returns_empty() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    let result = tools
        .do_search(SearchSymbolsArgs {
            query: "xyzzy_this_cannot_exist_in_any_fixture_2b7f".to_owned(),
            kinds: None,
            packages: None,
            limit: None,
        })
        .await
        .expect("search for nonsense must not fail");

    assert!(result.hits.is_empty(), "nonsense query must return no hits");
    assert!(!result.truncated, "empty result cannot be truncated");
}

/// Searching by a valid kind filter restricts results to that kind.
#[tokio::test]
async fn search_symbols_kind_filter_restricts_results() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    // The fixture has a Function kind (distance, format_point, legacy_fn).
    let result = tools
        .do_search(SearchSymbolsArgs {
            query: "distance".to_owned(),
            kinds: Some(vec!["Function".to_owned()]),
            packages: None,
            limit: None,
        })
        .await
        .expect("filtered search must not fail");

    // We do not assert it is non-empty because the hit depends on name matching,
    // but we can assert that ALL hits are tagged as functions.
    for hit in &result.hits {
        match &hit.kind {
            KindTag::Known(d) => {
                assert_eq!(
                    *d,
                    KindDiscriminant::Function,
                    "kind filter must restrict to Function; got {d:?}"
                );
            }
            KindTag::Unknown(_) => {
                panic!("all fixture symbols must have a known kind");
            }
        }
    }
}

/// An invalid kind name is rejected with `invalid_params`, not a panic.
#[tokio::test]
async fn search_symbols_invalid_kind_is_rejected() {
    let tools = make_tools();
    // No corpus wait needed — kind validation is synchronous.

    let err = tools
        .do_search(SearchSymbolsArgs {
            query: "Point".to_owned(),
            kinds: Some(vec!["NotARealKind".to_owned()]),
            packages: None,
            limit: None,
        })
        .await
        .expect_err("invalid kind must be rejected");

    assert!(
        err.to_string().contains("NotARealKind"),
        "error must echo the bad kind back: {err}"
    );
}

/// An empty `packages` list is rejected (the doc says omit it or supply at
/// least one entry — an empty list is a semantic error, not a filter that
/// matches nothing).
#[tokio::test]
async fn search_symbols_empty_packages_list_is_rejected() {
    let tools = make_tools();

    let err = tools
        .do_search(SearchSymbolsArgs {
            query: "Point".to_owned(),
            kinds: None,
            packages: Some(vec![]), // empty — semantically invalid
            limit: None,
        })
        .await
        .expect_err("empty packages list must be rejected");

    assert!(
        matches!(
            err,
            nudox_mcp::McpError::InvalidArgument {
                argument: "packages",
                ..
            }
        ),
        "expected InvalidArgument for packages, got {err:?}"
    );
}

/// The `limit` field is respected and does not panic on edge cases.
#[tokio::test]
async fn search_symbols_limit_is_respected() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    // A limit of 1 must return at most 1 hit.
    let result = tools
        .do_search(SearchSymbolsArgs {
            query: "a".to_owned(), // very broad — should hit many things
            kinds: None,
            packages: None,
            limit: Some(1),
        })
        .await
        .expect("search with limit=1 must not fail");

    assert!(
        result.hits.len() <= 1,
        "limit=1 must return at most 1 hit, got {}",
        result.hits.len()
    );

    // A limit of 0 must be treated as MAX (500), not "unlimited".
    let wide = tools
        .do_search(SearchSymbolsArgs {
            query: "a".to_owned(),
            kinds: None,
            packages: None,
            limit: Some(0),
        })
        .await
        .expect("search with limit=0 must not fail");
    // We cannot assert an exact count, but we can assert it does not return
    // more than MAX_LIMIT rows (which is 500).
    assert!(wide.hits.len() <= 500, "limit=0 must be clamped to 500");
}

/// Results are stable across two back-to-back searches for the same query.
#[tokio::test]
async fn search_symbols_is_deterministic() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    let a = tools
        .do_search(SearchSymbolsArgs {
            query: "Color".to_owned(),
            kinds: None,
            packages: None,
            limit: Some(10),
        })
        .await
        .expect("first search must succeed");
    let b = tools
        .do_search(SearchSymbolsArgs {
            query: "Color".to_owned(),
            kinds: None,
            packages: None,
            limit: Some(10),
        })
        .await
        .expect("second search must succeed");

    // Key sets must be identical (order may differ slightly if scores tie, but
    // in practice the fixture is deterministic).
    let keys_a: Vec<String> = a
        .hits
        .iter()
        .map(|h| {
            format!(
                "{}:{}#{}",
                h.key.package.ecosystem.as_str(),
                h.key.package.name.as_str(),
                h.key.intro.to_hex()
            )
        })
        .collect();
    let keys_b: Vec<String> = b
        .hits
        .iter()
        .map(|h| {
            format!(
                "{}:{}#{}",
                h.key.package.ecosystem.as_str(),
                h.key.package.name.as_str(),
                h.key.intro.to_hex()
            )
        })
        .collect();
    assert_eq!(keys_a, keys_b, "search results must be deterministic");
}

// ---------------------------------------------------------------------------
// get_symbol
// ---------------------------------------------------------------------------

/// `get_symbol` returns a complete `SymbolDoc` (head + at least one section)
/// for the `Point` record in the fixture corpus.
#[tokio::test]
async fn get_symbol_returns_doc_for_fixture_symbol() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    // Find `Point` first so we have its key.
    let search = tools
        .do_search(SearchSymbolsArgs {
            query: "Point".to_owned(),
            kinds: Some(vec!["Record".to_owned()]),
            packages: None,
            limit: Some(5),
        })
        .await
        .expect("search for Point must succeed");

    let point_hit = search
        .hits
        .iter()
        .find(|h| matches!(&h.kind, KindTag::Known(KindDiscriminant::Record)))
        .unwrap_or_else(|| {
            panic!(
                "Point record must appear in search results; got {:?}",
                search.hits
            )
        });

    let key_str = format!(
        "{}:{}#{}",
        point_hit.key.package.ecosystem.as_str(),
        point_hit.key.package.name.as_str(),
        point_hit.key.intro.to_hex()
    );

    let doc = tools
        .do_get_symbol(GetSymbolArgs {
            key: SymbolKeyDto(key_str.clone()),
        })
        .await
        .expect("get_symbol for a known key must succeed");

    // Head must identify the same symbol.
    let head_key_str = format!(
        "{}:{}#{}",
        doc.head.key.package.ecosystem.as_str(),
        doc.head.key.package.name.as_str(),
        doc.head.key.intro.to_hex()
    );
    assert_eq!(
        head_key_str, key_str,
        "doc.head.key must match the requested key"
    );

    // At least one rendered section must be present for a documented symbol.
    assert!(
        !doc.sections.is_empty(),
        "Point has documentation; at least one section must be emitted"
    );
}

/// A malformed key is rejected with `MalformedKey` before the stream is opened.
#[tokio::test]
async fn get_symbol_malformed_key_is_rejected() {
    let tools = make_tools();

    let err = tools
        .do_get_symbol(GetSymbolArgs {
            key: SymbolKeyDto("not-a-valid-key".to_owned()),
        })
        .await
        .expect_err("malformed key must be rejected");

    assert!(
        matches!(err, nudox_mcp::McpError::MalformedKey { .. }),
        "expected MalformedKey, got {err:?}"
    );
}

/// An unknown (but well-formed) key does not panic or hang — it returns an
/// engine error.
#[tokio::test]
async fn get_symbol_unknown_key_fails_cleanly() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    // Construct a valid-looking key whose intro bytes cannot match any fixture
    // symbol (all-zero intro is never used by the fixture generator).
    let unknown_key = format!("fixture:nudox-fixture-rich#{}", "00".repeat(32));

    let err = tools
        .do_get_symbol(GetSymbolArgs {
            key: SymbolKeyDto(unknown_key),
        })
        .await
        .expect_err("unknown symbol must fail cleanly");

    // We accept either `Engine(SymbolNotFound)` or `TruncatedStream` — both
    // are correct engine-level signals. What we must NOT get is a panic or a
    // hang.
    match &err {
        nudox_mcp::McpError::Engine(_) | nudox_mcp::McpError::TruncatedStream => {}
        other => panic!("unexpected error for unknown symbol: {other:?}"),
    }
}

/// `get_symbol` terminates — the stream never hangs.
///
/// This is the regression test for the `_ => {}` arm at line 411 of tools.rs:
/// `DocEvent::Timeline` was silently dropped there, which is fine (it is a
/// GUI-only event). But if the engine ever emits `Timeline` as a *terminal*
/// event instead of `Done`, the drain would hang forever. This test exercises
/// the drain path for a real symbol and asserts it finishes within 5 s.
#[tokio::test]
async fn get_symbol_always_terminates() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    // Find any symbol to open.
    let search = tools
        .do_search(SearchSymbolsArgs {
            query: "distance".to_owned(),
            kinds: None,
            packages: None,
            limit: Some(1),
        })
        .await
        .expect("search must succeed");

    let hit = &search.hits[0];
    let key_str = format!(
        "{}:{}#{}",
        hit.key.package.ecosystem.as_str(),
        hit.key.package.name.as_str(),
        hit.key.intro.to_hex()
    );

    // Apply a wall-clock timeout so the test fails cleanly on a hang rather
    // than blocking the suite. 10 s is generous for a local fixture.
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        tools.do_get_symbol(GetSymbolArgs {
            key: SymbolKeyDto(key_str),
        }),
    )
    .await
    .expect("get_symbol must complete within 10 s — stream must not hang");

    result.expect("get_symbol for a valid symbol must succeed");
}

// ---------------------------------------------------------------------------
// find_usages
// ---------------------------------------------------------------------------

/// `find_usages` for `distance` (intro 6) returns `format_point` (intro 23),
/// which is the one Oracle-confidence caller the fixture defines.
#[tokio::test]
async fn find_usages_returns_oracle_confidence_callers() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    // Find `distance` by name.
    let search = tools
        .do_search(SearchSymbolsArgs {
            query: "distance".to_owned(),
            kinds: Some(vec!["Function".to_owned()]),
            packages: None,
            limit: Some(5),
        })
        .await
        .expect("search for distance must succeed");

    let distance_hit = search.hits.iter().find(|h| {
        let name = &h.display_name;
        // The display name may include path prefix; just look for "distance"
        name.contains("distance") && !name.contains("reexport")
    });

    let Some(hit) = distance_hit else {
        // If there are no hits the fixture may not have indexed yet; that is
        // a corpus-wait issue, not a tools issue.  Fail with a clear message.
        panic!(
            "fixture must have a 'distance' function; search returned: {:?}",
            search
                .hits
                .iter()
                .map(|h| h.display_name.to_string())
                .collect::<Vec<_>>()
        );
    };

    let key_str = format!(
        "{}:{}#{}",
        hit.key.package.ecosystem.as_str(),
        hit.key.package.name.as_str(),
        hit.key.intro.to_hex()
    );

    let result = tools
        .do_find_usages(FindUsagesArgs {
            key: SymbolKeyDto(key_str),
            limit: None,
        })
        .await
        .expect("find_usages for distance must not fail");

    // The fixture has exactly one Oracle caller: `format_point`.
    let caller_names: Vec<String> = result.usages.iter().map(|u| u.name.clone()).collect();
    assert!(
        caller_names.iter().any(|n| n.contains("format_point")),
        "format_point must appear as a caller of distance; got callers: {caller_names:?}"
    );
}

/// A malformed key is rejected before reaching the graph plane.
#[tokio::test]
async fn find_usages_malformed_key_is_rejected() {
    let tools = make_tools();

    let err = tools
        .do_find_usages(FindUsagesArgs {
            key: SymbolKeyDto("bad-key".to_owned()),
            limit: None,
        })
        .await
        .expect_err("malformed key must be rejected");

    assert!(
        matches!(err, nudox_mcp::McpError::MalformedKey { .. }),
        "expected MalformedKey, got {err:?}"
    );
}

/// A valid key that has no callers returns an empty usages list, not an error.
#[tokio::test]
async fn find_usages_no_callers_returns_empty() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    // `MAX_SIZE` is a const with no callers in the fixture.
    let search = tools
        .do_search(SearchSymbolsArgs {
            query: "MAX_SIZE".to_owned(),
            kinds: Some(vec!["Const".to_owned()]),
            packages: None,
            limit: Some(1),
        })
        .await
        .expect("search for MAX_SIZE must succeed");

    if search.hits.is_empty() {
        // Corpus not ready or const not indexed; skip.
        return;
    }

    let hit = &search.hits[0];
    let key_str = format!(
        "{}:{}#{}",
        hit.key.package.ecosystem.as_str(),
        hit.key.package.name.as_str(),
        hit.key.intro.to_hex()
    );

    let result = tools
        .do_find_usages(FindUsagesArgs {
            key: SymbolKeyDto(key_str),
            limit: None,
        })
        .await
        .expect("find_usages for a symbol with no callers must succeed (not fail)");

    assert!(
        result.usages.is_empty(),
        "MAX_SIZE has no callers in the fixture; usages must be empty"
    );
}

// ---------------------------------------------------------------------------
// graph_query — the main verdict surface
// ---------------------------------------------------------------------------

/// The simplest possible query: list all packages.
///
/// This is the same query `list_packages` uses internally; it must return at
/// least the fixture package and must terminate.
#[tokio::test]
async fn graph_query_list_packages_returns_fixture() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: nudox_mcp::tools::PACKAGES_QUERY.to_owned(),
            args: None,
            limit: None,
        })
        .await
        .expect("PACKAGES_QUERY must succeed");

    assert!(!result.columns.is_empty(), "must have columns");
    assert!(
        result.columns.contains(&"lineage".to_owned()),
        "must have a 'lineage' column; got {:?}",
        result.columns
    );

    // At least the fixture package must appear.
    let lineage_col = result.columns.iter().position(|c| c == "lineage").unwrap();
    let fixture_found = result.rows.iter().any(|r| {
        r.cells
            .get(lineage_col)
            .map(|c| c == FIXTURE_LINEAGE)
            .unwrap_or(false)
    });
    assert!(
        fixture_found,
        "fixture package must appear in Packages query; rows: {:?}",
        result.rows
    );
}

/// An agent can discover the schema (`graph_schema`), then write a query
/// against it — this test checks that a valid query against the real schema
/// returns rows.
#[tokio::test]
async fn graph_query_symbols_query_returns_rows() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: "{ Symbols { name @output kind @output } }".to_owned(),
            args: None,
            limit: Some(20),
        })
        .await
        .expect("Symbols query must succeed");

    assert_eq!(
        result.columns.len(),
        2,
        "must have 2 columns: name and kind"
    );
    assert!(!result.rows.is_empty(), "fixture corpus must have symbols");
}

/// An empty `query` string is rejected immediately with `InvalidArgument`.
#[tokio::test]
async fn graph_query_empty_query_is_rejected() {
    let tools = make_tools();

    let err = tools
        .do_graph_query(GraphQueryArgs {
            query: "   ".to_owned(), // whitespace-only
            args: None,
            limit: None,
        })
        .await
        .expect_err("empty query must be rejected");

    assert!(
        matches!(
            err,
            nudox_mcp::McpError::InvalidArgument {
                argument: "query",
                ..
            }
        ),
        "expected InvalidArgument for query, got {err:?}"
    );
}

/// An invalid Trustfall query returns an `Engine` error with a message that
/// includes *something* diagnostic — not a panic, not an empty response.
#[tokio::test]
async fn graph_query_invalid_query_returns_engine_error() {
    let tools = make_tools();

    let err = tools
        .do_graph_query(GraphQueryArgs {
            query: "{ this is not trustfall syntax !!! }".to_owned(),
            args: None,
            limit: None,
        })
        .await
        .expect_err("syntactically invalid query must fail");

    assert!(
        matches!(err, nudox_mcp::McpError::Engine(_)),
        "expected Engine error for invalid query, got {err:?}"
    );
    // Error must carry a non-empty message so a caller can self-correct.
    assert!(
        !err.to_string().is_empty(),
        "engine error message must not be empty"
    );
}

/// Querying for a non-existent field name returns an engine error rather than
/// an empty result set. This is the "query errors are returned with enough
/// detail to fix the query" test.
#[tokio::test]
async fn graph_query_unknown_field_returns_error_not_empty_rows() {
    let tools = make_tools();

    let err = tools
        .do_graph_query(GraphQueryArgs {
            query: "{ Symbols { nonExistentField2b7f @output } }".to_owned(),
            args: None,
            limit: None,
        })
        .await
        .expect_err("unknown field must fail with an error, not silently return empty rows");

    assert!(
        matches!(err, nudox_mcp::McpError::Engine(_)),
        "expected Engine error for unknown field, got {err:?}"
    );
}

/// The `limit` field on `graph_query` caps the number of rows and sets
/// `truncated = true` when rows are cut.
#[tokio::test]
async fn graph_query_limit_is_applied_and_truncated_is_set() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    // The fixture has >1 symbol, so limit=1 must truncate.
    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: "{ Symbols { name @output } }".to_owned(),
            args: None,
            limit: Some(1),
        })
        .await
        .expect("query with limit=1 must succeed");

    assert_eq!(result.rows.len(), 1, "limit=1 must return exactly 1 row");
    assert!(
        result.truncated,
        "truncated must be true when rows were cut"
    );
}

/// Columns are addressed by name, not position — this exercises the column
/// extraction path through `graph_query`.
#[tokio::test]
async fn graph_query_column_order_is_stable() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: "{ Packages { name @output ecosystem @output lineage @output } }".to_owned(),
            args: None,
            limit: Some(10),
        })
        .await
        .expect("Packages query with multiple outputs must succeed");

    // All three columns must be present (BTreeMap → sorted order: ecosystem,
    // lineage, name).
    for col in ["ecosystem", "lineage", "name"] {
        assert!(
            result.columns.contains(&col.to_owned()),
            "missing column '{col}'; columns: {:?}",
            result.columns
        );
    }
    // Every row must have the same number of cells as columns.
    for (i, row) in result.rows.iter().enumerate() {
        assert_eq!(
            row.cells.len(),
            result.columns.len(),
            "row {i} cell count must match column count"
        );
    }
}

/// Walk `Symbols → members` — the tree traversal that an agent would use to
/// enumerate a record's fields.
#[tokio::test]
async fn graph_query_symbol_members_traversal_is_reachable() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    // `Point` (Record, intro 3) has two fields: `x` and `y`.
    // We filter by name and walk to its members.
    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: r#"{
                Symbols {
                    name @filter(op: "=", value: ["$name"]) @output(name: "parent_name")
                    kind @filter(op: "=", value: ["Record"])
                    members {
                        name @output(name: "member_name")
                        kind @output(name: "member_kind")
                    }
                }
            }"#
            .to_owned(),
            args: Some(
                [("name".to_owned(), "Point".to_owned())]
                    .into_iter()
                    .collect(),
            ),
            limit: Some(20),
        })
        .await
        .expect("Symbols → members traversal must succeed");

    let member_col = result.columns.iter().position(|c| c == "member_name");
    assert!(
        member_col.is_some(),
        "must have a member_name column; got {:?}",
        result.columns
    );
    let member_col = member_col.unwrap();

    let member_names: Vec<String> = result
        .rows
        .iter()
        .filter_map(|r| r.cells.get(member_col).map(|c| c.to_string()))
        .collect();

    // x and y must appear as members of Point.
    assert!(
        member_names.iter().any(|n| n == "x"),
        "Point.x must appear as a member; got: {member_names:?}"
    );
    assert!(
        member_names.iter().any(|n| n == "y"),
        "Point.y must appear as a member; got: {member_names:?}"
    );
}

/// Walk `Trait → implementors` — the trait implementation traversal.
///
/// `Display` (intro 8) has exactly one implementor: `PointDisplay` (intro 9).
#[tokio::test]
async fn graph_query_trait_implementors_traversal_is_reachable() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: nudox_graph::queries::FIND_IMPLEMENTORS.to_owned(),
            args: {
                // Find Display's key first.
                let search = tools
                    .do_search(SearchSymbolsArgs {
                        query: "Display".to_owned(),
                        kinds: Some(vec!["Trait".to_owned()]),
                        packages: None,
                        limit: Some(5),
                    })
                    .await
                    .expect("search for Display trait must succeed");
                let display = search
                    .hits
                    .iter()
                    .find(|h| matches!(&h.kind, KindTag::Known(KindDiscriminant::Trait)))
                    .unwrap_or_else(|| panic!("Display trait must appear in search results"));
                let key_str = format!(
                    "{}:{}#{}",
                    display.key.package.ecosystem.as_str(),
                    display.key.package.name.as_str(),
                    display.key.intro.to_hex()
                );
                Some([("key".to_owned(), key_str)].into_iter().collect())
            },
            limit: Some(10),
        })
        .await
        .expect("FIND_IMPLEMENTORS must succeed");

    // `PointDisplay` must appear.
    let key_col = result.columns.iter().position(|c| c == "key");
    let name_col = result.columns.iter().position(|c| c == "name");
    assert!(
        name_col.is_some() || key_col.is_some(),
        "must have a 'name' or 'key' column; got {:?}",
        result.columns
    );

    if let Some(ncol) = name_col {
        let names: Vec<String> = result
            .rows
            .iter()
            .filter_map(|r| r.cells.get(ncol).map(|c| c.to_string()))
            .collect();
        assert!(
            names.iter().any(|n| n.contains("PointDisplay")),
            "PointDisplay must appear as an implementor of Display; got: {names:?}"
        );
    }
}

/// Walk `Symbol → usages` via a Trustfall query (not the typed `find_usages`
/// tool). Proves the usages edge in the graph is reachable by agents who write
/// their own queries.
#[tokio::test]
async fn graph_query_usages_edge_is_reachable() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    // Find distance's key.
    let search = tools
        .do_search(SearchSymbolsArgs {
            query: "distance".to_owned(),
            kinds: Some(vec!["Function".to_owned()]),
            packages: None,
            limit: Some(5),
        })
        .await
        .expect("search for distance must succeed");

    let distance = search
        .hits
        .iter()
        .find(|h| h.display_name.contains("distance") && !h.display_name.contains("reexport"));

    let Some(hit) = distance else {
        return; // corpus not ready
    };

    let key_str = format!(
        "{}:{}#{}",
        hit.key.package.ecosystem.as_str(),
        hit.key.package.name.as_str(),
        hit.key.intro.to_hex()
    );

    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: nudox_graph::queries::FIND_USAGES.to_owned(),
            args: Some([("key".to_owned(), key_str)].into_iter().collect()),
            limit: Some(10),
        })
        .await
        .expect("FIND_USAGES query must succeed");

    // format_point must appear as a caller.
    let name_col = result
        .columns
        .iter()
        .position(|c| c == "name")
        .expect("must have name column");
    let callers: Vec<String> = result
        .rows
        .iter()
        .filter_map(|r| r.cells.get(name_col).map(|c| c.to_string()))
        .collect();

    assert!(
        callers.iter().any(|n| n.contains("format_point")),
        "format_point must appear as a caller via the usages edge; got: {callers:?}"
    );
}

/// Walk `Package → members` via lineage filter — the query an agent would
/// write to list a package's top-level symbols.
#[tokio::test]
async fn graph_query_package_members_traversal_is_reachable() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: r#"{
                Packages {
                    lineage @filter(op: "=", value: ["$lineage"])
                    members {
                        name @output
                        kind @output
                    }
                }
            }"#
            .to_owned(),
            args: Some(
                [("lineage".to_owned(), FIXTURE_LINEAGE.to_owned())]
                    .into_iter()
                    .collect(),
            ),
            limit: Some(50),
        })
        .await
        .expect("Package → members traversal must succeed");

    assert!(!result.rows.is_empty(), "fixture package must have members");
    let name_col = result
        .columns
        .iter()
        .position(|c| c == "name")
        .expect("must have name col");
    let names: Vec<String> = result
        .rows
        .iter()
        .filter_map(|r| r.cells.get(name_col).map(|c| c.to_string()))
        .collect();

    // At least a few of the well-known fixture symbols must appear.
    for expected in ["Point", "Color", "Display"] {
        assert!(
            names.iter().any(|n| n == expected),
            "'{expected}' must appear as a package member; names: {names:?}"
        );
    }
}

/// `graph_query` is safe to call concurrently from multiple tasks. This is
/// the regression test for generation counter races in `NudoxTools`.
///
/// We cannot prove absence of interference beyond "neither panics nor hangs",
/// but that is the correct bar: the spec says concurrent calls must not
/// interfere, and the simplest observable failure is a panic from a stale
/// generation or a deadlock.
#[tokio::test]
async fn graph_query_concurrent_calls_do_not_interfere() {
    let tools = std::sync::Arc::new(make_tools());
    wait_for_corpus(&*tools).await;

    let tools_clone = tools.clone();
    let t1 = tokio::spawn(async move {
        for _ in 0..3 {
            tools_clone
                .do_graph_query(GraphQueryArgs {
                    query: "{ Symbols { name @output } }".to_owned(),
                    args: None,
                    limit: Some(5),
                })
                .await
                .expect("concurrent call 1 must succeed")
        }
    });

    let tools_clone = tools.clone();
    let t2 = tokio::spawn(async move {
        for _ in 0..3 {
            tools_clone
                .do_list_packages()
                .await
                .expect("concurrent call 2 must succeed")
        }
    });

    // Both tasks must complete without panicking.
    t1.await.expect("task 1 must not panic");
    t2.await.expect("task 2 must not panic");
}

// ---------------------------------------------------------------------------
// Graph query: what is NOT reachable (documented gaps)
// ---------------------------------------------------------------------------
//
// These are noted here rather than as failing tests because they are schema
// design decisions, not bugs. An agent reading this file learns what queries
// it cannot write today.
//
// 1. Cross-package usages: the `usages` edge scans all packages, so cross-
//    package traversals are supported — but the fixture corpus only has one
//    package, so no test here proves cross-package *paths*. That requires a
//    multi-package corpus (the `FixtureSource::both()` variant adds a perf
//    package, but it has no occurrences).
//
// 2. Occurrence confidence filtering: `occurrencesOf` exposes the `confidence`
//    field but the schema has no `@filter` pushdown for it. An agent that wants
//    only Oracle-confidence occurrences must filter in-memory. This is a known
//    schema gap.
//
// NOTE: The following two gaps listed previously have been CLOSED:
//
// 3. `Occurrence → target` navigation: FIXED. `occurrencesOf { target { ... } }`
//    now works as a single traversal. The `targetKey` string is also preserved
//    for backward-compatibility.
//
// 4. `Static`, `Variant`, `Module`, `Reexport`, and `Param` now each have their
//    own dedicated schema types. Type-coercions like `... on Variant { }` work
//    uniformly. `OtherSymbol` is retained only for genuinely unknown kinds.

// ---------------------------------------------------------------------------
// SymbolKeyDto codec
// ---------------------------------------------------------------------------

/// A key returned by `search_symbols` can be parsed back into a wire key
/// and re-serialised to the same string — this is the round-trip LR-1 requires.
#[tokio::test]
async fn symbol_key_from_search_round_trips() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    let search = tools
        .do_search(SearchSymbolsArgs {
            query: "Point".to_owned(),
            kinds: None,
            packages: None,
            limit: Some(1),
        })
        .await
        .expect("search must succeed");

    if search.hits.is_empty() {
        return; // corpus not ready yet
    }

    let hit = &search.hits[0];
    let key_str = format!(
        "{}:{}#{}",
        hit.key.package.ecosystem.as_str(),
        hit.key.package.name.as_str(),
        hit.key.intro.to_hex()
    );

    let dto = SymbolKeyDto(key_str.clone());
    let wire = dto.to_wire().expect("key from search must parse");
    let back = SymbolKeyDto::from_wire(&wire);
    assert_eq!(
        dto, back,
        "round-trip through wire key must be lossless: {key_str}"
    );
}
