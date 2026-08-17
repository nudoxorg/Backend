//! The PURL-input path, end to end, through the *tool* surfaces an agent
//! actually calls — not through `EngineHandle` directly.
//!
//! # What `crates/nudox-engine/tests/purl_index.rs` already proves, and what it does not
//!
//! That file is thorough on acquisition (integrity, extraction layout, the
//! five registry-shaped failures) and has one pipeline test that indexes a
//! purl and confirms `EngineHandle::search` finds it. What it cannot prove,
//! because it never constructs one, is that the **tool surface** — `NudoxTools`,
//! the thing an MCP client actually calls — is wired the same way:
//!
//! * `search_symbols` (`do_search`) finds a real symbol from a package that
//!   was not resident when the process started.
//! * `get_symbol` (`do_get_symbol`) opens *the exact key* `search_symbols`
//!   returned, not merely "some symbol".
//! * `graph_query` (`do_graph_query`) reaches the same symbol through
//!   Trustfall, independently of the name-search path.
//! * `index_package` (`do_index_package`), called twice for the same purl,
//!   joins the job the job registry (`crate::index::IndexJobs`) is
//!   responsible for rather than starting a second producer run — the
//!   `joined_existing_job` flag on [`IndexPackageResult::Running`] is the one
//!   place that promise is externally observable, because a terminal
//!   `Indexed` result looks identical whether it was the first caller or the
//!   hundredth.
//! * A failure (`do_index_package` returning `Err`) leaves `list_packages`
//!   exactly as it was — no phantom package for a name the registry rejected —
//!   and the error names what to do next, not just that it failed.
//!
//! # Why every test here is `#[ignore]`
//!
//! Same convention as `purl_index.rs`: these hit real registries
//! (crates.io) over the network. Run with
//! `cargo test -p nudox-mcp --test purl_index_end_to_end -- --ignored`.
//!
//! # Package choice
//!
//! `numtoa` — a no-dependency integer formatter, chosen in `purl_index.rs`
//! for the same reason: small enough that indexing it end to end (fetch,
//! verify, extract, run rust-analyzer) is seconds rather than minutes, and it
//! is real enough that its public trait `NumToA` is a genuine symbol to
//! search for, open, and query — not a fixture invented to make an assertion
//! pass (doctrine §4).

use std::path::PathBuf;

use nudox_engine::{Engine, EngineConfig};
use nudox_engine::mcp::key::PackageLineageDto;
use nudox_engine::mcp::tools::{
    GetSymbolArgs, GraphQueryArgs, IndexPackageArgs, IndexPackageResult, ListVersionsArgs,
    SearchSymbolsArgs,
};
use nudox_engine::mcp::{McpError, NudoxTools, SymbolKeyDto};
use tokio::runtime::Runtime;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A cache directory under `target/`, so a run never touches the developer's
/// real `~/.cache/nudox` and `cargo clean` is a cache reset. Mirrors
/// `purl_index.rs::scratch_cache`.
fn scratch_cache(case: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/purl-index-mcp-tests")
        .join(case);
    std::fs::create_dir_all(&dir).expect("scratch cache directory must be creatable");
    dir
}

/// A fresh `NudoxTools` over an engine with an **empty** corpus and a scratch
/// package cache — empty so every assertion below is genuinely about a
/// package that arrived after start-up, not one that was already resident.
fn make_tools(case: &str) -> (NudoxTools, PathBuf) {
    let cache = scratch_cache(case);
    let engine = Engine::start_with_producer(
        EngineConfig {
            package_cache: Some(cache.clone()),
            ..EngineConfig::default()
        },
        Vec::new(),
    );
    (NudoxTools::new(engine), cache)
}

// ---------------------------------------------------------------------------
// The pipeline: indexed → searchable → openable → graph-reachable
// ---------------------------------------------------------------------------

/// The whole promise of the PURL-input feature, through the surfaces an agent
/// actually calls: `index_package` puts a package the corpus never had into
/// it, and afterwards `search_symbols`, `get_symbol` and `graph_query` all
/// see it — independently of each other, and all pointing at the *same*
/// symbol identity.
#[test]
#[ignore = "fetches numtoa from crates.io and runs rust-analyzer over it; run with --ignored"]
fn a_purl_indexed_through_index_package_is_searchable_openable_and_graph_reachable() {
    let runtime = Runtime::new().expect("test runtime must build");
    let case_dir = scratch_cache("pipeline");

    let ((hit_key, doc_key, graph_row_present), cost) = heart::cost::measured(
        "mcp_purl/numtoa/index_search_open_query",
        &case_dir,
        || {
            runtime.block_on(async {
                let (tools, _cache) = make_tools("pipeline");

                // Precondition: nothing named NumToA can be found before indexing.
                let before = tools
                    .do_search(SearchSymbolsArgs {
                        query: "NumToA".to_owned(),
                        kinds: None,
                        packages: None,
                        limit: None,
                        cursor: None,
                    })
                    .await
                    .expect("search_symbols must succeed even over an empty corpus");
                assert!(
                    before.hits.is_empty(),
                    "NumToA must not be findable before the package is indexed; got {:?}",
                    before.hits.iter().map(|h| h.display_name.to_string()).collect::<Vec<_>>()
                );

                // --- index_package -------------------------------------------------
                let indexed = tools
                    .do_index_package(IndexPackageArgs {
                        purl: "pkg:cargo/numtoa@0.2.4".to_owned(),
                        wait_seconds: Some(180),
                    })
                    .await
                    .expect("index_package must succeed for a real, small crate");
                let IndexPackageResult::Indexed {
                    package,
                    version,
                    symbol_count,
                    ..
                } = indexed
                else {
                    panic!("expected the job to finish inside a 180s wait; got {indexed:?}");
                };
                assert_eq!(package, "cargo:numtoa");
                assert_eq!(version, "0.2.4");
                assert!(
                    symbol_count > 1,
                    "numtoa declares a trait and its impls; a 1-symbol package is a root \
                     module and nothing else"
                );

                // --- search_symbols -------------------------------------------------
                let search = tools
                    .do_search(SearchSymbolsArgs {
                        query: "NumToA".to_owned(),
                        kinds: None,
                        packages: None,
                        limit: None,
                        cursor: None,
                    })
                    .await
                    .expect("search_symbols must succeed after indexing");
                let hit = search
                    .hits
                    .iter()
                    .find(|h| &*h.display_name == "NumToA")
                    .unwrap_or_else(|| {
                        panic!(
                            "numtoa's public trait `NumToA` must be searchable after indexing; \
                             got {:?}",
                            search
                                .hits
                                .iter()
                                .map(|h| h.display_name.to_string())
                                .collect::<Vec<_>>()
                        )
                    });
                let hit_key = hit.key.clone();

                // --- get_symbol ------------------------------------------------------
                // Open the *exact* key search_symbols returned, so this asserts get_symbol
                // reaches the same identity rather than merely "some symbol exists".
                let doc = tools
                    .do_get_symbol(GetSymbolArgs {
                        key: SymbolKeyDto::from_wire(&hit_key),
                    })
                    .await
                    .expect("get_symbol must open the key search_symbols just returned");
                let doc_key = doc.head.key.clone();
                assert_eq!(
                    doc_key, hit_key,
                    "get_symbol must open the same symbol identity search_symbols found"
                );

                // --- graph_query -------------------------------------------------------
                // Reach the same symbol via Trustfall, independently of the name-search
                // ranking path.
                let mut args = std::collections::BTreeMap::new();
                args.insert("name".to_owned(), "NumToA".to_owned());
                let query = tools
                    .do_graph_query(GraphQueryArgs {
                        query: r#"{ Symbols { name @filter(op: "=", value: ["$name"]) @output key @output kind @output } }"#
                            .to_owned(),
                        args: Some(args),
                        limit: None,
                        cursor: None,
                    })
                    .await
                    .expect("graph_query must succeed against the resident package");
                let key_col = query
                    .columns
                    .iter()
                    .position(|c| c == "key")
                    .expect("the query requested a key column");
                let expected_key = SymbolKeyDto::from_wire(&hit_key).0;
                let graph_row_present = query
                    .rows
                    .iter()
                    .any(|r| r.cells.get(key_col) == Some(&expected_key));
                assert!(
                    graph_row_present,
                    "graph_query must reach the same symbol key search_symbols and get_symbol \
                     did; expected {expected_key:?}, got rows {:?}",
                    query.rows
                );

                (hit_key, doc_key, graph_row_present)
            })
        },
    );
    eprintln!(
        "mcp_purl/numtoa/index_search_open_query: wall={:.1}s rss={:?}",
        cost.wall.as_secs_f64(),
        cost.peak_rss_bytes
    );
    assert_eq!(hit_key, doc_key);
    assert!(graph_row_present);
    drop(runtime);
}

// ---------------------------------------------------------------------------
// Joining a running job, not duplicating the package
// ---------------------------------------------------------------------------

/// Two `index_package` calls for the same purl must join one job. This is
/// what `crate::index::IndexJobs` exists for (see its module docs): without
/// it, a second poll runs the producer a second time and races the first into
/// `VersionRegistry::record`.
///
/// The only place "joined" is externally observable is
/// [`IndexPackageResult::Running::joined_existing_job`] — a terminal `Indexed`
/// result looks the same whichever caller triggered the work, so this test
/// forces both calls to land while the job is still running (`wait_seconds:
/// 0`, an immediate poll) before letting a third call actually wait for
/// completion.
#[test]
#[ignore = "fetches numtoa from crates.io and runs rust-analyzer over it; run with --ignored"]
fn indexing_the_same_purl_twice_joins_the_running_job_rather_than_duplicating_the_package() {
    let runtime = Runtime::new().expect("test runtime must build");
    let case_dir = scratch_cache("join");

    let ((first_joined, second_joined, final_symbol_count, version_count), cost) =
        heart::cost::measured("mcp_purl/numtoa/join_running_job", &case_dir, || {
            runtime.block_on(async {
                let (tools, _cache) = make_tools("join");
                let purl = "pkg:cargo/numtoa@0.2.4".to_owned();

                // First call: nothing is running yet, so this one must start the job.
                // wait_seconds: 0 means "poll once, return immediately" — a real
                // network fetch + rust-analyzer run cannot finish in zero time, so this
                // is reliably `Running`.
                let first = tools
                    .do_index_package(IndexPackageArgs {
                        purl: purl.clone(),
                        wait_seconds: Some(0),
                    })
                    .await
                    .expect("index_package must not fail merely because the deadline is 0");
                let first_joined = match first {
                    IndexPackageResult::Running {
                        joined_existing_job,
                        ..
                    } => joined_existing_job,
                    IndexPackageResult::Indexed { .. } => {
                        panic!(
                            "a 0-second wait must not observe a finished index — numtoa cannot \
                             be fetched and lowered in zero time; the job registry may not be \
                             starting the work asynchronously"
                        )
                    }
                };

                // Second call, same purl, also an immediate poll: must attach to the
                // job the first call started, not begin a second producer run.
                let second = tools
                    .do_index_package(IndexPackageArgs {
                        purl: purl.clone(),
                        wait_seconds: Some(0),
                    })
                    .await
                    .expect("the joining call must not fail");
                let second_joined = match second {
                    IndexPackageResult::Running {
                        joined_existing_job,
                        ..
                    } => joined_existing_job,
                    IndexPackageResult::Indexed { .. } => {
                        panic!("still should not be finished after a second 0-second poll")
                    }
                };

                // Now actually wait for it, and confirm the corpus ends up with exactly
                // one generation — a blind `Corpus::insert` racing two producer runs
                // would be the failure mode this test exists to catch.
                let finished = tools
                    .do_index_package(IndexPackageArgs {
                        purl,
                        wait_seconds: Some(180),
                    })
                    .await
                    .expect("index_package must eventually succeed");
                let IndexPackageResult::Indexed { symbol_count, .. } = finished else {
                    panic!("expected the job to finish inside a 180s wait; got {finished:?}");
                };

                let versions = tools
                    .do_list_versions(ListVersionsArgs {
                        package: PackageLineageDto("cargo:numtoa".to_owned()),
                    })
                    .await
                    .expect("list_versions must succeed for a package that is now resident");

                (first_joined, second_joined, symbol_count, versions.versions.len())
            })
        });
    eprintln!(
        "mcp_purl/numtoa/join_running_job: wall={:.1}s rss={:?}",
        cost.wall.as_secs_f64(),
        cost.peak_rss_bytes
    );

    assert!(!first_joined, "the first caller must have started the job, not joined one");
    assert!(
        second_joined,
        "the second caller, polling the same purl while it is still running, must join the \
         first call's job rather than starting a second producer run"
    );
    assert!(final_symbol_count > 1, "numtoa declares a trait and its impls");
    assert_eq!(
        version_count, 1,
        "one purl indexed three times (twice polling, once to completion) must still be one \
         generation, not three"
    );
    drop(runtime);
}

// ---------------------------------------------------------------------------
// A failed index leaves the corpus unchanged
// ---------------------------------------------------------------------------

/// A package name the registry rejects must fail `index_package` with an
/// error that names what to do next, and must leave `list_packages` exactly
/// as empty as it was before the call — no phantom lineage for a package that
/// was never actually indexed.
#[test]
#[ignore = "asks crates.io about a package that does not exist; run with --ignored"]
fn a_failed_index_leaves_the_corpus_unchanged_and_the_error_names_what_to_do() {
    let runtime = Runtime::new().expect("test runtime must build");
    let case_dir = scratch_cache("failure");

    let ((kind, help, message, packages_before, packages_after), cost) =
        heart::cost::measured("mcp_purl/unknown_package/failure_leaves_corpus_alone", &case_dir, || {
            runtime.block_on(async {
                let (tools, _cache) = make_tools("failure");

                let packages_before = tools
                    .do_list_packages()
                    .await
                    .expect("list_packages must succeed over an empty corpus")
                    .packages;

                let err = tools
                    .do_index_package(IndexPackageArgs {
                        purl: "pkg:cargo/numtoa-with-a-typo-nobody-published@0.2.5".to_owned(),
                        wait_seconds: Some(30),
                    })
                    .await
                    .expect_err("a package crates.io has never heard of must fail, not succeed");

                let (kind, help, message) = match &err {
                    McpError::Index { error } => {
                        (error.kind().to_owned(), error.help().map(str::to_owned), error.to_string())
                    }
                    other => panic!("expected McpError::Index, got {other:?}"),
                };

                let packages_after = tools
                    .do_list_packages()
                    .await
                    .expect("list_packages must succeed after a failed index")
                    .packages;

                (kind, help, message, packages_before, packages_after)
            })
        });
    eprintln!(
        "mcp_purl/unknown_package/failure_leaves_corpus_alone: wall={:.1}s rss={:?}",
        cost.wall.as_secs_f64(),
        cost.peak_rss_bytes
    );

    assert_eq!(kind, "unknown_package", "a typo is an unknown-package failure, not any other kind");
    assert!(
        help.as_deref().is_some_and(|h| h.len() > 20),
        "the error must name what to do next, not just that it failed; got {help:?}"
    );
    assert!(message.contains("crates.io"), "the message must name the registry: {message}");
    assert_eq!(
        packages_before, packages_after,
        "a failed index must not leave a phantom package behind: before={packages_before:?} \
         after={packages_after:?}"
    );
    assert!(packages_after.is_empty(), "this engine started with an empty corpus and nothing indexed");
    drop(runtime);
}
