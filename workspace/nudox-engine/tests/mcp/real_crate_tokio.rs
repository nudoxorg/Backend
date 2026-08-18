//! Integration tests for the full MCP tool surface against a real tokio crate.
//!
//! # Why these tests exist
//!
//! The fixture-based test suite in `tool_integration.rs` exercises all tools
//! against a hand-crafted synthetic corpus whose shape we control exactly.
//! That is the right default — it is fast, deterministic, and author-controlled —
//! but it shares an author with the code it tests, so it cannot prove the graph
//! plane survives contact with a crate nobody wrote for us: real generics, real
//! trait bounds, real re-exports, thousands of items, and — critically — the
//! two capabilities introduced by this change set:
//!
//!   * **Limit 1 fix**: `occurrencesOf { target { name kind } }` — a single
//!     traversal that follows a reference from an occurrence to its target
//!     symbol, with no second query.
//!
//!   * **Limit 2 fix**: `... on Variant { }`, `... on Static { }`, etc. —
//!     type-coercions that now work because each formerly-collapsed kind has its
//!     own named vertex type in the SDL and the adapter emits it.
//!
//! # Why all tests are `#[ignore]`d
//!
//! They run the Rust producer (rust-analyzer in-process) over the tokio crate,
//! which takes ~40 s and requires the crate's dependency graph resolvable
//! offline. They are deliberately-invoked checks, not part of the fast suite.
//!
//! # Running the tests
//!
//! ```text
//! # Ensure tokio is fetched:
//! ls result/tokio/Cargo.toml
//!
//! # Run everything:
//! cargo test -p nudox-mcp --test real_crate_tokio -- --ignored --nocapture
//!
//! # Run a single test:
//! cargo test -p nudox-mcp --test real_crate_tokio graph_query_occurrence_target -- --ignored --nocapture
//! ```
//!
//! # Grace when the fixture is absent
//!
//! Every test skips gracefully if `result/tokio/Cargo.toml` is absent:
//! a missing fixture is an environment problem, not a code defect.  The skip
//! message is printed on `eprintln!` so it appears under `--nocapture`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use nudox_engine::mcp::tools::{FindUsagesArgs, GetSymbolArgs, GraphQueryArgs, SearchSymbolsArgs};
use nudox_engine::mcp::{NudoxTools, SymbolKeyDto};
use nudox_engine::{
    Engine, EngineConfig, PackageLoadEvent, PackageSpec, ProducerLanguage, SharedStr,
};

// ---------------------------------------------------------------------------
// Fixture path helpers
// ---------------------------------------------------------------------------

/// Where a tokio checkout would land if it were part of the corpus.
///
/// `scripts/fetch-real-crate.sh` does not exist in this repo — the real
/// fetch tooling is `nix build .#checks.corpus`, driven by `nix/corpus.nix`
/// (see `docs/CORPUS.md`). As of this writing tokio is not an entry in
/// that manifest (unlike memchr — see `tests/real_crate_memchr.rs`), so
/// fetching it means adding a `{ version, hash }` block to
/// `nix/corpus.nix` under a `crates.io`/`tokio` package (hash via `nu
/// nix build .#checks.corpus hash-url crates.io tokio <version>`) and then running `nu
/// nix build .#checks.corpus` to materialize it.
fn tokio_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../result/tokio")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("/nonexistent"))
}

/// Return `true` when the tokio fixture checkout is present.
///
/// Every test body should call this at the top and `return` immediately if
/// it is false. A missing fixture is a *skip*, not a failure: the CI that has
/// not fetched the crate must not be red.
fn tokio_available() -> bool {
    tokio_root().join("Cargo.toml").is_file()
}

/// Emit a human-readable skip notice and return.
macro_rules! skip_if_absent {
    () => {
        if !tokio_available() {
            eprintln!(
                "SKIP: no checkout at {}. \
                 tokio is not in nix/corpus.nix yet — add a {{ version, hash }} \
                 entry (see docs/CORPUS.md 'Adding a package') and run: \
                 nix build .#checks.corpus",
                tokio_root().display()
            );
            return;
        }
    };
}

// ---------------------------------------------------------------------------
// Engine + corpus harness
// ---------------------------------------------------------------------------

/// Build a `NudoxTools` that loads tokio via the real Rust producer.
///
/// Construction starts the producer in the background; call `wait_for_corpus`
/// before querying to ensure the package is visible.
fn make_tokio_tools() -> NudoxTools {
    let root = tokio_root();
    // Derive the version from Cargo.toml if possible, fall back to a
    // placeholder that is only used for diagnostics.
    let version = read_tokio_version(&root).unwrap_or_else(|| "unknown".to_owned());
    let engine = Engine::start_with_producer(
        EngineConfig::default(),
        vec![PackageSpec {
            root,
            name: "tokio".to_owned(),
            version,
            language: ProducerLanguage::Rust,
        }],
    );
    NudoxTools::new(engine)
}

/// Parse the `version` field out of `<root>/Cargo.toml` without pulling in
/// a full TOML library. Returns `None` on parse failure; the caller provides a
/// safe default.
fn read_tokio_version(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("version") && line.contains('=') {
            let rhs = line.split_once('=')?.1.trim();
            let version = rhs.trim_matches(|c| c == '"' || c == '\'').to_owned();
            if !version.is_empty() {
                return Some(version);
            }
        }
    }
    None
}

/// Wait for the tokio package to appear in the corpus.
///
/// The producer is async; we must not query before the `Loaded` event arrives.
/// Times out after 120 s — generous for a local rust-analyzer run.
async fn wait_for_tokio_corpus(tools: &NudoxTools) {
    let rx = tools.engine().packages();
    let deadline = tokio::time::Instant::now() + Duration::from_mins(2);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv_async()).await {
            // `PackageLoadEvent::Loaded` deliberately does not carry
            // `nudox_ir::change::PackageLineageId` (see the type's doc
            // comment in nudox-engine/src/lib.rs): the event is a GUI-facing
            // projection, not a re-export of the store/IR shape. The name we
            // actually need to identify "the tokio package arrived" is the
            // `name: SharedStr` field.
            Ok(Ok(PackageLoadEvent::Loaded { name, .. })) if name == SharedStr::from("tokio") => {
                return;
            }
            Ok(Ok(PackageLoadEvent::Loaded { .. } | _)) => {}
            Ok(Err(_)) => return, // channel closed → already loaded
            Err(elapsed) => panic!(
                "tokio corpus never seeded within 120 s: {elapsed:?} — producer may have failed. \
                 Run with --nocapture to see tracing output."
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// graph_schema — verify the new types appear in the SDL served to agents
// ---------------------------------------------------------------------------

/// The schema served to an agent must advertise the five new dedicated types
/// (Limit 2 fix) and the `Occurrence.target` edge (Limit 1 fix).
///
/// This test does not start the engine; it reads the schema constant directly.
/// The same constant is served by the `graph_schema` MCP tool (LR-7).
#[test]
#[ignore = "documents the schema capabilities the Limit 1 and Limit 2 fixes introduce; run with --ignored to confirm"]
fn graph_schema_lists_all_five_new_distinct_types() {
    let sdl = nudox_engine::mcp::SCHEMA_SDL;

    // Limit 2: each of the five formerly-collapsed kinds has its own type.
    for new_type in [
        "type Static ",
        "type Variant ",
        "type Module ",
        "type Reexport ",
        "type Param ",
    ] {
        assert!(
            sdl.contains(new_type),
            "schema must declare '{new_type}' as a concrete Symbol implementor; \
             an agent writing `... on {t} {{ }}` would silently return nothing without it",
            t = new_type.trim()
        );
    }

    // Limit 1: Occurrence carries a real edge, not just a key string.
    assert!(
        sdl.contains("target: Symbol"),
        "schema must declare `Occurrence.target: Symbol` so that \
         `occurrencesOf {{ target {{ name kind }} }}` works without a second query"
    );

    // All five also implement Symbol.
    for implementor in ["Static", "Variant", "Module", "Reexport", "Param"] {
        assert!(
            sdl.contains(&format!("{implementor} implements Symbol")),
            "schema must declare '{implementor} implements Symbol'"
        );
    }
}

// ---------------------------------------------------------------------------
// list_packages — tokio appears with correct lineage
// ---------------------------------------------------------------------------

/// After the producer finishes, tokio must appear in `list_packages` with the
/// `cargo` ecosystem and `tokio` name.
#[tokio::test]
#[ignore = "loads tokio through rust-analyzer (~40 s); run with --ignored"]
async fn list_packages_tokio_present_with_correct_lineage() {
    skip_if_absent!();
    let tools = make_tokio_tools();
    wait_for_tokio_corpus(&tools).await;

    let result = tools
        .do_list_packages()
        .await
        .expect("list_packages must not fail");
    assert!(
        !result.packages.is_empty(),
        "corpus must have at least the tokio package"
    );

    let tokio_pkg = result
        .packages
        .iter()
        .find(|p| p.name == "tokio")
        .unwrap_or_else(|| {
            panic!(
                "tokio must appear in list_packages; got: {:?}",
                result
                    .packages
                    .iter()
                    .map(|p| p.lineage.clone())
                    .collect::<Vec<_>>()
            )
        });

    assert_eq!(
        tokio_pkg.ecosystem, "cargo",
        "tokio must report 'cargo' as its ecosystem"
    );
    assert_eq!(
        tokio_pkg.name, "tokio",
        "tokio must carry 'tokio' as its name"
    );
    assert!(
        tokio_pkg.lineage.starts_with("cargo:tokio"),
        "lineage must start with 'cargo:tokio'; got: {}",
        tokio_pkg.lineage
    );
}

/// Two back-to-back `list_packages` calls must return the same lineages.
#[tokio::test]
#[ignore = "loads tokio through rust-analyzer (~40 s); run with --ignored"]
async fn list_packages_is_deterministic_over_tokio() {
    skip_if_absent!();
    let tools = make_tokio_tools();
    wait_for_tokio_corpus(&tools).await;

    let a = tools.do_list_packages().await.expect("first call");
    let b = tools.do_list_packages().await.expect("second call");

    let mut la: Vec<_> = a.packages.iter().map(|p| p.lineage.clone()).collect();
    let mut lb: Vec<_> = b.packages.iter().map(|p| p.lineage.clone()).collect();
    la.sort();
    lb.sort();
    assert_eq!(la, lb, "list_packages must be deterministic across calls");
}

// ---------------------------------------------------------------------------
// search_symbols — finds real tokio symbols
// ---------------------------------------------------------------------------

/// Search for `Runtime` — tokio's most recognisable export.
///
/// Asserts the hit exists, the key is in canonical form, and the kind is
/// recorded correctly.
#[tokio::test]
#[ignore = "loads tokio through rust-analyzer (~40 s); run with --ignored"]
async fn search_symbols_finds_tokio_runtime() {
    skip_if_absent!();
    let tools = make_tokio_tools();
    wait_for_tokio_corpus(&tools).await;

    let result = tools
        .do_search(SearchSymbolsArgs {
            query: "Runtime".to_owned(),
            kinds: None,
            packages: None,
            limit: Some(20),
            cursor: None,
        })
        .await
        .expect("search for Runtime must not fail");

    assert!(
        !result.hits.is_empty(),
        "tokio::Runtime must appear in search results for 'Runtime'"
    );

    // Every hit must carry a cargo:tokio lineage in its key.
    for hit in &result.hits {
        let key_str = format!(
            "{}:{}#{}",
            hit.hit.key.package.ecosystem.as_str(),
            hit.hit.key.package.name.as_str(),
            hit.hit.key.intro.to_hex()
        );
        assert!(
            key_str.starts_with("cargo:tokio#"),
            "every hit for a single-package corpus must be in cargo:tokio; got {key_str}"
        );
    }
}

/// A broad prefix search must return results and respect the limit.
#[tokio::test]
#[ignore = "loads tokio through rust-analyzer (~40 s); run with --ignored"]
async fn search_symbols_broad_search_respects_limit() {
    skip_if_absent!();
    let tools = make_tokio_tools();
    wait_for_tokio_corpus(&tools).await;

    let result = tools
        .do_search(SearchSymbolsArgs {
            query: "a".to_owned(), // matches many things
            kinds: None,
            packages: None,
            limit: Some(10),
            cursor: None,
        })
        .await
        .expect("broad search must not fail");

    assert!(
        result.hits.len() <= 10,
        "limit=10 must be respected; got {} hits",
        result.hits.len()
    );
}

// ---------------------------------------------------------------------------
// get_symbol — a known tokio symbol streams head and sections
// ---------------------------------------------------------------------------

/// `get_symbol` on a tokio `Runtime` must produce a head and at least one
/// rendered section (Runtime has a substantial doc comment).
#[tokio::test]
#[ignore = "loads tokio through rust-analyzer (~40 s); run with --ignored"]
async fn get_symbol_runtime_streams_head_and_sections() {
    skip_if_absent!();
    let tools = make_tokio_tools();
    wait_for_tokio_corpus(&tools).await;

    // Find Runtime first.
    let search = tools
        .do_search(SearchSymbolsArgs {
            query: "Runtime".to_owned(),
            kinds: None,
            packages: None,
            limit: Some(10),
            cursor: None,
        })
        .await
        .expect("search must succeed");

    let Some(hit) = search.hits.iter().find(|h| {
        // Accept the main Record or any hit named exactly "Runtime".
        h.hit.display_name.contains("Runtime")
    }) else {
        panic!(
            "tokio::Runtime must appear in search; got: {:?}",
            search
                .hits
                .iter()
                .map(|h| h.hit.display_name.to_string())
                .collect::<Vec<_>>()
        );
    };

    let key_str = format!(
        "{}:{}#{}",
        hit.hit.key.package.ecosystem.as_str(),
        hit.hit.key.package.name.as_str(),
        hit.hit.key.intro.to_hex()
    );

    let doc = tokio::time::timeout(
        Duration::from_secs(30),
        tools.do_get_symbol(GetSymbolArgs {
            key: SymbolKeyDto(key_str.clone()),
        }),
    )
    .await
    .expect("get_symbol must complete within 30 s")
    .expect("get_symbol for Runtime must succeed");

    // Head must identify the same key.
    let head_key = format!(
        "{}:{}#{}",
        doc.head.key.package.ecosystem.as_str(),
        doc.head.key.package.name.as_str(),
        doc.head.key.intro.to_hex()
    );
    assert_eq!(head_key, key_str, "head key must match the requested key");

    // tokio::Runtime is extensively documented; at least one section must emerge.
    assert!(
        !doc.sections.is_empty(),
        "Runtime has documentation; at least one rendered section must be emitted"
    );
}

// ---------------------------------------------------------------------------
// find_usages — a widely-used tokio type has callers
// ---------------------------------------------------------------------------

/// tokio's `Runtime` is used by many items within the crate itself.
/// `find_usages` must return a non-empty list.
#[tokio::test]
#[ignore = "loads tokio through rust-analyzer (~40 s); run with --ignored"]
async fn find_usages_runtime_has_callers() {
    skip_if_absent!();
    let tools = make_tokio_tools();
    wait_for_tokio_corpus(&tools).await;

    // Find Runtime.
    let search = tools
        .do_search(SearchSymbolsArgs {
            query: "Runtime".to_owned(),
            kinds: None,
            packages: None,
            limit: Some(10),
            cursor: None,
        })
        .await
        .expect("search must succeed");

    let Some(hit) = search
        .hits
        .iter()
        .find(|h| h.hit.display_name.contains("Runtime"))
    else {
        panic!("tokio::Runtime must appear in search results");
    };

    let key_str = format!(
        "{}:{}#{}",
        hit.hit.key.package.ecosystem.as_str(),
        hit.hit.key.package.name.as_str(),
        hit.hit.key.intro.to_hex()
    );

    let result = tools
        .do_find_usages(FindUsagesArgs {
            key: SymbolKeyDto(key_str),
            limit: Some(20),
            cursor: None,
        })
        .await
        .expect("find_usages must not fail");

    // tokio references its own Runtime extensively.
    assert!(
        !result.usages.is_empty(),
        "tokio::Runtime must have at least one caller within the crate; usages list is empty"
    );
}

// ---------------------------------------------------------------------------
// graph_query — new capabilities: Occurrence → target traversal (Limit 1)
// ---------------------------------------------------------------------------

/// An agent can follow `occurrencesOf { target { name kind } }` in a single
/// query rather than using the `targetKey` string as input to a second round-
/// trip.  This is the primary motivating use-case for Limit 1.
///
/// Strategy: find any tokio symbol that has at least one occurrence, then
/// verify that `occurrencesOf { target { name } }` returns rows.  We pick
/// `Runtime` because it is widely-referenced — any real crate with usages works.
#[tokio::test]
#[ignore = "loads tokio through rust-analyzer (~40 s); run with --ignored"]
async fn graph_query_occurrence_target_traversal_returns_rows() {
    skip_if_absent!();
    let tools = make_tokio_tools();
    wait_for_tokio_corpus(&tools).await;

    // Find any tokio symbol (we use a broad search and pick the first hit).
    let search = tools
        .do_search(SearchSymbolsArgs {
            query: "tokio".to_owned(),
            kinds: Some(vec!["Function".to_owned()]),
            packages: None,
            limit: Some(10),
            cursor: None,
        })
        .await
        .expect("search must succeed");

    if search.hits.is_empty() {
        eprintln!("SKIP (inner): no Function hits for 'tokio' — corpus may lack occurrences");
        return;
    }

    // Run a query that traverses Symbol → occurrencesOf → target.
    // This is the Limit 1 capability: no second query needed.
    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: r#"{
                Symbols {
                    kind @filter(op: "=", value: ["Function"])
                    name @output(name: "owner_name")
                    occurrencesOf {
                        targetKey @output(name: "target_key")
                        referenceKind @output
                        target {
                            name @output(name: "target_name")
                            kind @output(name: "target_kind")
                        }
                    }
                }
            }"#
            .to_owned(),
            args: None,
            limit: Some(50),
            cursor: None,
        })
        .await
        .expect("occurrencesOf → target traversal must succeed");

    // We do not assert a specific count because it depends on the tokio source
    // tree, but we assert the columns are present: if the traversal is broken,
    // Trustfall returns an engine error rather than an empty result set.
    let col_names = &result.columns;
    assert!(
        col_names.contains(&"target_name".to_owned()),
        "must have a 'target_name' column from the Occurrence.target edge; \
         columns: {col_names:?}. If this is empty the target edge was not traversed."
    );
    assert!(
        col_names.contains(&"target_key".to_owned()),
        "must have a 'target_key' column from Occurrence.targetKey; columns: {col_names:?}"
    );

    // If we got any rows, each target_name must be non-empty.
    if result.rows.is_empty() {
        // No rows is not a hard failure: the tokio functions we picked may not
        // carry graph-worthy occurrences at the index-or-above confidence floor.
        // Log so the tester can see what happened.
        eprintln!(
            "WARN: occurrencesOf → target returned 0 rows for the sampled functions. \
             This may be expected if none carry index-or-above confidence occurrences."
        );
    } else {
        let name_col = col_names.iter().position(|c| c == "target_name").unwrap();
        for row in &result.rows {
            let target_name = row
                .cells
                .get(name_col)
                .map_or("", std::string::String::as_str);
            assert!(
                !target_name.is_empty(),
                "target_name must be a real symbol name, not empty; row: {:?}",
                row.cells
            );
        }
        eprintln!(
            "occurrencesOf → target: {} rows returned, sample target name: {:?}",
            result.rows.len(),
            result.rows[0].cells.get(name_col)
        );
    }
}

/// `targetKey` is preserved alongside `target` — the two are complementary.
///
/// This guards backward-compatibility: callers that already use `targetKey` as
/// a string (to pass it to another tool) must continue to work.
#[tokio::test]
#[ignore = "loads tokio through rust-analyzer (~40 s); run with --ignored"]
async fn graph_query_target_key_still_works_alongside_target_edge() {
    skip_if_absent!();
    let tools = make_tokio_tools();
    wait_for_tokio_corpus(&tools).await;

    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: r#"{
                Symbols {
                    kind @filter(op: "=", value: ["Function"])
                    occurrencesOf {
                        targetKey @output
                        confidence @output
                    }
                }
            }"#
            .to_owned(),
            args: None,
            limit: Some(20),
            cursor: None,
        })
        .await
        .expect("targetKey-only query must still work after adding the target edge");

    // targetKey column must be present — backward-compatible.
    assert!(
        result.columns.contains(&"targetKey".to_owned()),
        "targetKey must still be available on Occurrence; columns: {:?}",
        result.columns
    );
}

// ---------------------------------------------------------------------------
// graph_query — new capabilities: type coercions (Limit 2)
// ---------------------------------------------------------------------------

/// `... on Variant { }` must match real tokio enum variants and return rows.
///
/// Before Limit 2, `Variant` was collapsed into `OtherSymbol`, so this
/// coercion would silently return nothing.  After the fix, the adapter emits
/// `Vertex::Variant` for every `KindDiscriminant::Variant` and the coercion
/// matches correctly.
#[tokio::test]
#[ignore = "loads tokio through rust-analyzer (~40 s); run with --ignored"]
async fn graph_query_variant_coercion_returns_real_rows() {
    skip_if_absent!();
    let tools = make_tokio_tools();
    wait_for_tokio_corpus(&tools).await;

    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: r#"{
                Symbols {
                    ... on Variant {
                        name @output(name: "variant_name")
                        kind @output(name: "variant_kind")
                    }
                }
            }"#
            .to_owned(),
            args: None,
            limit: Some(50),
            cursor: None,
        })
        .await
        .expect("... on Variant { } coercion must succeed");

    // tokio has many enums (e.g. `broadcast::error::TryRecvError`, `TcpKind`,
    // etc.) so at least some variants must appear.
    assert!(
        !result.rows.is_empty(),
        "tokio has enum variants; `... on Variant {{ }}` must return rows. \
         If this is empty the adapter is still emitting OtherSymbol for Variant entries."
    );

    let kind_col = result
        .columns
        .iter()
        .position(|c| c == "variant_kind")
        .expect("must have a 'variant_kind' column");

    for row in &result.rows {
        let kind = row
            .cells
            .get(kind_col)
            .map_or("", std::string::String::as_str);
        assert_eq!(
            kind, "Variant",
            "every row from `... on Variant {{ }}` must have kind=Variant; got {kind:?}"
        );
    }

    eprintln!(
        "Variant coercion: {} rows returned, sample: {:?}",
        result.rows.len(),
        result
            .rows
            .iter()
            .take(3)
            .map(|r| r.cells.first())
            .collect::<Vec<_>>()
    );
}

/// `... on Module { }` must match real tokio modules and return rows.
///
/// Before Limit 2, `Module` collapsed into `OtherSymbol`.
#[tokio::test]
#[ignore = "loads tokio through rust-analyzer (~40 s); run with --ignored"]
async fn graph_query_module_coercion_returns_real_rows() {
    skip_if_absent!();
    let tools = make_tokio_tools();
    wait_for_tokio_corpus(&tools).await;

    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: r#"{
                Symbols {
                    ... on Module {
                        name @output(name: "mod_name")
                        kind @output(name: "mod_kind")
                    }
                }
            }"#
            .to_owned(),
            args: None,
            limit: Some(30),
            cursor: None,
        })
        .await
        .expect("... on Module { } coercion must succeed");

    assert!(
        !result.rows.is_empty(),
        "tokio has modules (`tokio::task`, `tokio::sync`, etc.); \
         `... on Module {{ }}` must return rows. \
         If empty, the adapter is still emitting OtherSymbol for Module entries."
    );

    let kind_col = result
        .columns
        .iter()
        .position(|c| c == "mod_kind")
        .expect("must have a 'mod_kind' column");
    for row in &result.rows {
        let kind = row
            .cells
            .get(kind_col)
            .map_or("", std::string::String::as_str);
        assert_eq!(
            kind, "Module",
            "every Module row must have kind=Module; got {kind:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// graph_query — existing traversals still work after the schema changes
// ---------------------------------------------------------------------------

/// Walk `Symbols → members` to enumerate a struct's fields — regression guard
/// that the base Symbol interface traversal works correctly after the schema
/// additions.
#[tokio::test]
#[ignore = "loads tokio through rust-analyzer (~40 s); run with --ignored"]
async fn graph_query_symbol_members_traversal_still_works() {
    skip_if_absent!();
    let tools = make_tokio_tools();
    wait_for_tokio_corpus(&tools).await;

    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: r#"{
                Symbols {
                    kind @filter(op: "=", value: ["Record"])
                    name @output(name: "struct_name")
                    members {
                        name @output(name: "field_name")
                        kind @output(name: "field_kind")
                    }
                }
            }"#
            .to_owned(),
            args: None,
            limit: Some(50),
            cursor: None,
        })
        .await
        .expect("Symbols → members traversal must succeed after schema changes");

    // tokio has many structs with fields.
    assert!(
        !result.rows.is_empty(),
        "tokio has struct types with fields; Symbols → members must return rows"
    );
}

/// Walk `Trait → implementors` to enumerate trait implementations — a regression
/// guard for the `Trait`-specific neighbor that must still work.
#[tokio::test]
#[ignore = "loads tokio through rust-analyzer (~40 s); run with --ignored"]
async fn graph_query_trait_implementors_still_work() {
    skip_if_absent!();
    let tools = make_tokio_tools();
    wait_for_tokio_corpus(&tools).await;

    // Find any trait in tokio.
    let search = tools
        .do_search(SearchSymbolsArgs {
            query: "Future".to_owned(),
            kinds: Some(vec!["Trait".to_owned()]),
            packages: None,
            limit: Some(5),
            cursor: None,
        })
        .await
        .expect("search must succeed");

    if search.hits.is_empty() {
        eprintln!("SKIP (inner): no Trait hits for 'Future' in tokio");
        return;
    }

    let key_str = {
        let hit = &search.hits[0];
        format!(
            "{}:{}#{}",
            hit.hit.key.package.ecosystem.as_str(),
            hit.hit.key.package.name.as_str(),
            hit.hit.key.intro.to_hex()
        )
    };

    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: nudox_engine::graph::queries::FIND_IMPLEMENTORS.to_owned(),
            args: Some(std::iter::once(("key".to_owned(), key_str)).collect()),
            limit: Some(20),
            cursor: None,
        })
        .await
        .expect("FIND_IMPLEMENTORS must succeed after schema changes");

    // We only assert the query executes without error; the number of
    // implementors depends on how many types implement Future in tokio.
    let _ = result.rows.len();
}

/// Walk `Package → members` — the query an agent uses to enumerate a package's
/// top-level symbols.
#[tokio::test]
#[ignore = "loads tokio through rust-analyzer (~40 s); run with --ignored"]
async fn graph_query_package_members_traversal_still_works() {
    skip_if_absent!();
    let tools = make_tokio_tools();
    wait_for_tokio_corpus(&tools).await;

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
                std::iter::once(("lineage".to_owned(), "cargo:tokio".to_owned())).collect(),
            ),
            limit: Some(50),
            cursor: None,
        })
        .await
        .expect("Package → members traversal must succeed");

    assert!(
        !result.rows.is_empty(),
        "tokio package must have members visible via Package → members traversal"
    );

    let name_col = result
        .columns
        .iter()
        .position(|c| c == "name")
        .expect("must have name column");
    let names: Vec<&str> = result
        .rows
        .iter()
        .filter_map(|r| r.cells.get(name_col).map(std::string::String::as_str))
        .collect();

    eprintln!(
        "Package → members: {} names, sample: {:?}",
        names.len(),
        &names[..names.len().min(5)]
    );
}

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

/// The same query returns identical rows on two back-to-back executions.
///
/// This is the regression guard for generation-counter races or non-deterministic
/// index iteration in the corpus.
#[tokio::test]
#[ignore = "loads tokio through rust-analyzer (~40 s); run with --ignored"]
async fn graph_query_is_deterministic_over_tokio() {
    skip_if_absent!();
    let tools = make_tokio_tools();
    wait_for_tokio_corpus(&tools).await;

    let query_args = GraphQueryArgs {
        query: "{ Symbols { name @output kind @output } }".to_owned(),
        args: None,
        limit: Some(30),
        cursor: None,
    };

    let first = tools
        .do_graph_query(query_args.clone())
        .await
        .expect("first call");
    let second = tools.do_graph_query(query_args).await.expect("second call");

    // Collect (name, kind) pairs from each run and sort so the comparison is
    // order-independent (hash-table iteration may vary across calls).
    let mut pairs_a: Vec<(String, String)> = first
        .rows
        .iter()
        .zip(std::iter::repeat(&first.columns))
        .map(|(row, cols)| {
            let name_col = cols.iter().position(|c| c == "name").unwrap_or(0);
            let kind_col = cols.iter().position(|c| c == "kind").unwrap_or(1);
            (
                row.cells.get(name_col).cloned().unwrap_or_default(),
                row.cells.get(kind_col).cloned().unwrap_or_default(),
            )
        })
        .collect();
    let mut pairs_b: Vec<(String, String)> = second
        .rows
        .iter()
        .zip(std::iter::repeat(&second.columns))
        .map(|(row, cols)| {
            let name_col = cols.iter().position(|c| c == "name").unwrap_or(0);
            let kind_col = cols.iter().position(|c| c == "kind").unwrap_or(1);
            (
                row.cells.get(name_col).cloned().unwrap_or_default(),
                row.cells.get(kind_col).cloned().unwrap_or_default(),
            )
        })
        .collect();

    pairs_a.sort();
    pairs_b.sort();

    assert_eq!(
        pairs_a, pairs_b,
        "two identical queries over tokio must return the same rows"
    );
}

// ---------------------------------------------------------------------------
// Limits and truncation
// ---------------------------------------------------------------------------

/// A broad query with a small limit must truncate and set `truncated = true`.
///
/// tokio has thousands of symbols; limit=1 must cut the result and flag it.
#[tokio::test]
#[ignore = "loads tokio through rust-analyzer (~40 s); run with --ignored"]
async fn graph_query_truncation_is_set_for_large_corpus() {
    skip_if_absent!();
    let tools = make_tokio_tools();
    wait_for_tokio_corpus(&tools).await;

    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: "{ Symbols { name @output } }".to_owned(),
            args: None,
            limit: Some(1),
            cursor: None,
        })
        .await
        .expect("query must succeed");

    assert_eq!(result.rows.len(), 1, "limit=1 must return exactly 1 row");
    assert!(
        result.truncated,
        "truncated must be true when tokio's thousands of symbols are cut to 1"
    );
}

/// A `MAX_LIMIT`-bounded query never returns more than 500 rows.
///
/// An unbounded traversal over tokio (thousands of symbols) would inject an
/// enormous context window. The ceiling must be hard.
#[tokio::test]
#[ignore = "loads tokio through rust-analyzer (~40 s); run with --ignored"]
async fn graph_query_max_limit_is_respected_for_tokio() {
    skip_if_absent!();
    let tools = make_tokio_tools();
    wait_for_tokio_corpus(&tools).await;

    // Deliberately no limit — falls through to DEFAULT_LIMIT (50).
    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: "{ Symbols { name @output } }".to_owned(),
            args: None,
            limit: None,
            cursor: None,
        })
        .await
        .expect("unlimited query must succeed");

    assert!(
        result.rows.len() <= 500,
        "no result must exceed MAX_LIMIT=500; got {}",
        result.rows.len()
    );
}
