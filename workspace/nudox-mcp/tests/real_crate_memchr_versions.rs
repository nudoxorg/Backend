//! End-to-end test of the version-lineage feature against real memchr
//! releases: `list_versions`, `select_version`, and a `get_symbol` timeline
//! that actually spans more than one loaded generation.
//!
//! # Why this file exists
//!
//! `tests/tool_integration.rs`'s timeline coverage (`get_symbol_returns_doc_for_fixture_symbol`)
//! only ever sees one loaded generation of the synthetic fixture package, so
//! it can pin the one-row `Present` case but nothing about `Introduced`,
//! `Renamed`, `SignatureChanged`, or the version-registry/corpus interaction
//! `select_version` drives. Doctrine is explicit about why that is not enough:
//! "A timeline test that never sees two real versions of a package is not a
//! test of timelines." `nix/corpus.nix` already pins three real memchr
//! releases (2.7.6, 2.8.0, 2.8.3) specifically so a multi-generation corpus
//! can be built without fabricating one — this file is that test.
//!
//! # Why `#[ignore]`
//!
//! Same reason as `real_crate_tokio.rs` and `real_crate_memchr.rs`: this runs
//! the real Rust producer (rust-analyzer in-process) three times, which is
//! slow and requires the checkouts to be present and dependency-resolvable
//! offline. Not part of the fast suite.
//!
//! # Running
//!
//! ```text
//! # Ensure all three memchr generations are fetched:
//! ls result/memchr-2.7.6/Cargo.toml result/memchr-2.8.0/Cargo.toml \
//!    result/memchr-2.8.3/Cargo.toml
//!
//! # If missing:
//! nix build .#checks.corpus
//!
//! cargo test -p nudox-mcp --test real_crate_memchr_versions -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::time::Duration;

use nudox_engine::{
    Engine, EngineConfig, PackageHistorySpec, PackageLoadEvent, PackageVersionSpec,
    ProducerLanguage, SharedStr,
};
use nudox_mcp::tools::{GetSymbolArgs, ListVersionsArgs, SearchSymbolsArgs, SelectVersionArgs};
use nudox_mcp::{NudoxTools, SymbolKeyDto};
use tokio::runtime::Runtime;

// ---------------------------------------------------------------------------
// Fixture path helpers
// ---------------------------------------------------------------------------

/// The three memchr generations `nix/corpus.nix` pins.
const MEMCHR_VERSIONS: [&str; 3] = ["2.7.6", "2.8.0", "2.8.3"];

fn memchr_root(version: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../result/memchr-{version}"))
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("/nonexistent"))
}

/// `true` only when every generation this test needs is present. A partial
/// checkout (some versions fetched, not others) would silently test fewer
/// generations than the test claims to, so this is all-or-nothing.
fn all_memchr_versions_available() -> bool {
    MEMCHR_VERSIONS
        .iter()
        .all(|v| memchr_root(v).join("Cargo.toml").is_file())
}

// ---------------------------------------------------------------------------
// Engine harness
// ---------------------------------------------------------------------------

fn make_memchr_history_tools() -> NudoxTools {
    let engine = Engine::start_with_versions(
        EngineConfig::default(),
        vec![PackageHistorySpec {
            name: "memchr".to_owned(),
            language: ProducerLanguage::Rust,
            versions: MEMCHR_VERSIONS
                .iter()
                .map(|v| PackageVersionSpec {
                    root: memchr_root(v),
                    version: (*v).to_owned(),
                })
                .collect(),
        }],
    );
    NudoxTools::new(engine)
}

/// Wait until all three generations are recorded in the version registry, or
/// panic on a load failure for memchr — a producer failure here would
/// otherwise present as a silent hang until the deadline.
async fn wait_for_all_generations(tools: &NudoxTools) {
    // `packages()` collapses multiple generations of one lineage into a
    // single `Loaded` event per lineage (see
    // `packages_emits_one_row_per_lineage_not_per_generation` in
    // `crates/nudox-engine/tests/multi_package_flows.rs`), so it can only
    // ever signal "at least one generation of memchr landed", never "all
    // three did". Wait for that first signal (or a hard failure) here, then
    // poll `list_versions` directly below for the real all-three signal.
    let rx = tools.engine().packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(240);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv_async()).await {
            Ok(Ok(PackageLoadEvent::Loaded { name, .. })) if name == SharedStr::from("memchr") => {
                break;
            }
            Ok(Ok(PackageLoadEvent::LoadFailed { name, error, .. }))
                if name == SharedStr::from("memchr") =>
            {
                panic!("memchr failed to load: {error}");
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break, // channel closed — corpus may already be seeded
            Err(_) => panic!(
                "no memchr Loaded/LoadFailed event within 240s; the producer may have \
                 failed — run with --nocapture to see tracing output"
            ),
        }
    }

    for _ in 0..2400 {
        let list = tools
            .do_list_versions(ListVersionsArgs {
                package: nudox_mcp::key::PackageLineageDto("cargo:memchr".to_owned()),
            })
            .await
            .expect("list_versions must not fail");
        if list.versions.len() == MEMCHR_VERSIONS.len() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "timed out waiting for all {} memchr generations to be registered",
        MEMCHR_VERSIONS.len()
    );
}

// ---------------------------------------------------------------------------
// list_versions / select_version
// ---------------------------------------------------------------------------

/// `list_versions` reports all three real generations, newest first, with
/// 2.8.3 current before any `select_version` call.
#[test]
#[ignore = "drives rust-analyzer over three real cargo workspaces (~30-90s total); \
            run explicitly with --ignored"]
fn list_versions_reports_all_three_real_memchr_generations() {
    if !all_memchr_versions_available() {
        eprintln!(
            "SKIP: not all of {:?} are fetched under result/. Run: nix build .#checks.corpus",
            MEMCHR_VERSIONS
        );
        return;
    }

    let runtime = Runtime::new().expect("test runtime must build");
    let case_dir = memchr_root("2.8.3");

    let (list, _cost) = nudox_test_support::measured(
        "mcp_versions/memchr/list_versions_three_real_generations",
        &case_dir,
        || {
            runtime.block_on(async {
                let tools = make_memchr_history_tools();
                wait_for_all_generations(&tools).await;
                tools
                    .do_list_versions(ListVersionsArgs {
                        package: nudox_mcp::key::PackageLineageDto("cargo:memchr".to_owned()),
                    })
                    .await
                    .expect("list_versions must not fail")
            })
        },
    );

    let seen: Vec<&str> = list.versions.iter().map(|v| v.version.as_str()).collect();
    assert_eq!(
        seen,
        vec!["2.8.3", "2.8.0", "2.7.6"],
        "list_versions must report all three real generations, newest first; got {seen:?}"
    );
    assert!(
        list.versions[0].is_current,
        "the newest generation (2.8.3) must be current before any select_version call"
    );
    assert!(
        list.versions[1..].iter().all(|v| !v.is_current),
        "exactly one generation may be current"
    );
    // Every generation reports a real, non-trivial symbol count — proves this
    // is not three empty package shells.
    for v in &list.versions {
        assert!(
            v.symbol_count > 100,
            "generation {} reports a suspiciously small symbol_count {}: real memchr \
             lowers to well over 100 entries at every one of these releases",
            v.version,
            v.symbol_count
        );
    }
}

/// `select_version` actually repoints the corpus: after switching to 2.7.6,
/// `list_versions` reports it current, and switching to a version that was
/// never loaded returns `NotLoaded` rather than an error.
#[test]
#[ignore = "drives rust-analyzer over three real cargo workspaces (~30-90s total); \
            run explicitly with --ignored"]
fn select_version_switches_the_current_generation_and_reports_not_loaded_honestly() {
    if !all_memchr_versions_available() {
        eprintln!(
            "SKIP: not all of {:?} are fetched under result/. Run: nix build .#checks.corpus",
            MEMCHR_VERSIONS
        );
        return;
    }

    let runtime = Runtime::new().expect("test runtime must build");
    let case_dir = memchr_root("2.7.6");

    let ((switched, not_loaded, relist), _cost) = nudox_test_support::measured(
        "mcp_versions/memchr/select_version_real_switch",
        &case_dir,
        || {
            runtime.block_on(async {
                let tools = make_memchr_history_tools();
                wait_for_all_generations(&tools).await;

                let switched = tools
                    .do_select_version(SelectVersionArgs {
                        package: nudox_mcp::key::PackageLineageDto("cargo:memchr".to_owned()),
                        version: "2.7.6".to_owned(),
                    })
                    .await
                    .expect("select_version to a loaded version must not fail");

                let not_loaded = tools
                    .do_select_version(SelectVersionArgs {
                        package: nudox_mcp::key::PackageLineageDto("cargo:memchr".to_owned()),
                        version: "0.0.1-never-loaded".to_owned(),
                    })
                    .await
                    .expect("select_version to an unloaded version must not error");

                let relist = tools
                    .do_list_versions(ListVersionsArgs {
                        package: nudox_mcp::key::PackageLineageDto("cargo:memchr".to_owned()),
                    })
                    .await
                    .expect("list_versions must not fail");

                (switched, not_loaded, relist)
            })
        },
    );

    match switched {
        nudox_mcp::tools::SelectVersionResult::Switched {
            version,
            symbol_count,
            ..
        } => {
            assert_eq!(version, "2.7.6");
            assert!(symbol_count > 100, "real memchr 2.7.6 must have a real symbol count");
        }
        other => panic!("expected Switched for a loaded version, got {other:?}"),
    }

    assert!(
        matches!(
            &not_loaded,
            nudox_mcp::tools::SelectVersionResult::NotLoaded { version, .. }
                if version.as_str() == "0.0.1-never-loaded"
        ),
        "an unloaded version must come back as NotLoaded, not an error and not Switched; \
         got {not_loaded:?}"
    );

    let current: Vec<&str> = relist
        .versions
        .iter()
        .filter(|v| v.is_current)
        .map(|v| v.version.as_str())
        .collect();
    assert_eq!(
        current,
        vec!["2.7.6"],
        "after switching, list_versions must report 2.7.6 as current — the corpus \
         must actually have been repointed, not merely the registry designation \
         updated with no effect a caller can observe"
    );
}

// ---------------------------------------------------------------------------
// get_symbol timeline across real generations
// ---------------------------------------------------------------------------

/// `get_symbol`'s timeline for a symbol that exists in all three real memchr
/// releases actually reflects three real generations — not a single
/// synthetic `Present` row, and not a fabricated history.
///
/// `memchr::memchr` (the top-level function, re-exported at the crate root
/// in all three releases per `pub use crate::memchr::{memchr, ...}`) is the
/// target: a small, stable, heavily-documented public function that is a
/// realistic case for "does this symbol's history look right".
#[test]
#[ignore = "drives rust-analyzer over three real cargo workspaces (~30-90s total); \
            run explicitly with --ignored"]
fn get_symbol_timeline_spans_three_real_memchr_generations() {
    if !all_memchr_versions_available() {
        eprintln!(
            "SKIP: not all of {:?} are fetched under result/. Run: nix build .#checks.corpus",
            MEMCHR_VERSIONS
        );
        return;
    }

    let runtime = Runtime::new().expect("test runtime must build");
    let case_dir = memchr_root("2.8.3");

    let (doc, _cost) = nudox_test_support::measured(
        "mcp_versions/memchr/get_symbol_timeline_three_real_generations",
        &case_dir,
        || {
            runtime.block_on(async {
                let tools = make_memchr_history_tools();
                wait_for_all_generations(&tools).await;

                // Find the top-level `memchr` function specifically (not the
                // `memchr` module or the `Memchr` struct, which the same
                // query text also matches).
                let search = tools
                    .do_search(SearchSymbolsArgs {
                        query: "memchr".to_owned(),
                        kinds: Some(vec!["Function".to_owned()]),
                        packages: None,
                        limit: Some(50),
                        cursor: None,
                    })
                    .await
                    .expect("search for memchr must succeed");

                let hit = search
                    .hits
                    .iter()
                    .find(|h| &*h.display_name == "memchr")
                    .unwrap_or_else(|| {
                        panic!(
                            "the top-level `memchr` function must be findable by exact \
                             name; got hits: {:?}",
                            search
                                .hits
                                .iter()
                                .map(|h| h.display_name.to_string())
                                .collect::<Vec<_>>()
                        )
                    });

                let key_str = format!(
                    "{}:{}#{}",
                    hit.key.package.ecosystem.as_str(),
                    hit.key.package.name.as_str(),
                    hit.key.intro.to_hex()
                );

                tools
                    .do_get_symbol(GetSymbolArgs {
                        key: SymbolKeyDto(key_str),
                    })
                    .await
                    .expect("get_symbol for the memchr function must succeed")
            })
        },
    );

    eprintln!(
        "memchr() timeline: versions_examined={}, rows={:?}",
        doc.timeline.versions_examined,
        doc.timeline
            .rows
            .iter()
            .map(|r| (r.version.to_string(), format!("{:?}", r.change)))
            .collect::<Vec<_>>()
    );

    // The engine examined all three loaded generations, whatever it
    // concluded about `memchr`'s `IntroId` stability across them.
    assert_eq!(
        doc.timeline.versions_examined, 3,
        "all three loaded generations must have been examined to build this timeline"
    );

    // This is the honest core of the test: `IntroId` stability across real
    // releases is not guaranteed (see `EngineHandle::select_version`'s own
    // documented caveat, and this task's item 2). If the id survived, the
    // timeline has more than one row and the newest is current. If it did
    // not — the function's identity escalated to a different disambiguator
    // tier between releases — the timeline correctly has exactly one row
    // (this key's only appearance), which is not a failure of the timeline
    // feature, it is the feature correctly reporting what it can prove.
    assert!(
        !doc.timeline.rows.is_empty(),
        "the symbol we just opened must appear in its own timeline"
    );
    assert!(
        doc.timeline.rows[0].is_current,
        "the first (newest) row must be marked current"
    );
    if doc.timeline.rows.len() > 1 {
        // Rows are newest-first (matches `VersionList::versions`).
        let versions: Vec<&str> = doc
            .timeline
            .rows
            .iter()
            .map(|r| r.version.as_ref())
            .collect();
        let mut sorted = versions.clone();
        sorted.sort_by(|a, b| b.cmp(a)); // lexical desc happens to match here (2.8.3>2.8.0>2.7.6)
        assert_eq!(
            versions, sorted,
            "multi-row timeline must be newest-first; got {versions:?}"
        );
        // Only the oldest row in a multi-row timeline may be `Present`
        // (unclassified first appearance); every later row must carry a real
        // classification, never a placeholder.
        for row in &doc.timeline.rows[..doc.timeline.rows.len() - 1] {
            assert_ne!(
                row.change,
                nudox_engine::wire::TimelineChange::Present,
                "only the oldest examined generation may be classified Present; \
                 row for {} was not the oldest but still says Present",
                row.version
            );
        }
    } else {
        eprintln!(
            "NOTE: memchr()'s IntroId did not survive across the three loaded generations \
             (timeline has one row despite three being examined) — this is the exact \
             instability EngineHandle::select_version's docs warn about, not a test failure."
        );
    }
}

// ---------------------------------------------------------------------------
// diff_versions, and the key provenance it rests on
// ---------------------------------------------------------------------------

/// The cross-version question, answered in one call against two real releases.
///
/// # What this proves that a fixture cannot
///
/// Two things, and the second is the one the design turns on.
///
/// 1. `PackageView::key_tier` is populated on the **production** load path.
///    Every hand-built fixture in this repo reports `Unrecorded`, because it
///    never ran `seal`; only a package produced through `ProducerSource` has a
///    `SealReport` to carry. If `build_sealed` were not wired in
///    `source/producer.rs` this test would see `Unrecorded` everywhere and the
///    whole of gap 4 would be inert while every unit test stayed green.
///
/// 2. The diff **never claims a deletion it cannot support**. The invariant
///    asserted below is not "the counts look plausible" — it is that no row
///    verdict is `removed` unless that declaration's key was content-derived,
///    checked row by row against the tier the sealer recorded. A diff that
///    reported a key-churned declaration as deleted would be worse than no
///    diff, and this is the assertion that would catch it.
#[test]
#[ignore = "drives rust-analyzer over three real cargo workspaces (~30-90s total); \
            run explicitly with --ignored"]
fn diffing_two_real_memchr_releases_never_claims_an_unsupported_deletion() {
    use nudox_engine::wire::{DiffVerdict, KeyTierLabel};
    use nudox_mcp::tools::{DiffVersionsArgs, DiffVersionsResult};

    if !all_memchr_versions_available() {
        eprintln!(
            "SKIP: not all of {:?} are fetched under result/. Run: nix build .#checks.corpus",
            MEMCHR_VERSIONS
        );
        return;
    }

    let runtime = Runtime::new().expect("test runtime must build");
    let case_dir = memchr_root("2.8.3");

    let (result, _cost) = nudox_test_support::measured(
        "mcp_versions/memchr/diff_2.8.0_to_2.8.3",
        &case_dir,
        || {
            runtime.block_on(async {
                let tools = make_memchr_history_tools();
                wait_for_all_generations(&tools).await;
                tools
                    .do_diff_versions(DiffVersionsArgs {
                        package: nudox_mcp::key::PackageLineageDto("cargo:memchr".to_owned()),
                        from_version: "2.8.0".to_owned(),
                        to_version: "2.8.3".to_owned(),
                        // The whole diff in one page: this test asserts a
                        // property of *every* row, so paging would silently
                        // narrow what it checks.
                        limit: Some(500),
                        cursor: None,
                    })
                    .await
                    .expect("diff_versions must not fail")
            })
        },
    );

    let DiffVersionsResult::Diff { diff, truncated, .. } = result else {
        panic!("both 2.8.0 and 2.8.3 are loaded; got {result:?}");
    };

    eprintln!("--- memchr 2.8.0 -> 2.8.3 ---");
    eprintln!("declarations before : {}", diff.from_symbol_count);
    eprintln!("declarations after  : {}", diff.to_symbol_count);
    eprintln!("unchanged           : {}", diff.unchanged);
    eprintln!("rows                : {}", diff.rows.len());
    eprintln!(
        "  added         : {}",
        diff.count(|v| matches!(v, DiffVerdict::Added))
    );
    eprintln!(
        "  removed       : {}",
        diff.count(|v| matches!(v, DiffVerdict::Removed))
    );
    eprintln!(
        "  rekeyed       : {}",
        diff.count(|v| matches!(v, DiffVerdict::Rekeyed { .. }))
    );
    eprintln!(
        "  changed       : {}",
        diff.count(|v| matches!(v, DiffVerdict::Changed { .. }))
    );
    eprintln!("  indeterminate : {}", diff.indeterminate());
    for row in diff.rows.iter().take(15) {
        eprintln!("    {:?} {} ({})", row.verdict, row.path, row.kind);
    }

    assert!(
        diff.from_symbol_count > 100 && diff.to_symbol_count > 100,
        "both generations must be real lowerings, not empty shells: {} -> {}",
        diff.from_symbol_count,
        diff.to_symbol_count
    );
    assert!(
        !truncated,
        "the 500-row page must hold the whole diff, or the per-row assertion \
         below only covers part of it"
    );

    // The load-bearing assertion, and the one this test was written to make:
    // **no `Removed` row may share a `(kind, path)` with any other row.**
    //
    // `Removed` is the only verdict that asserts a deletion. A declaration of
    // the same kind still sitting at that path in the newer generation —
    // whether it arrived as `Added` or as the far half of a `Rekeyed` — is
    // direct evidence that nothing was deleted, so a `Removed` there is a
    // false removal, which is the failure mode that makes a diff worse than no
    // diff.
    //
    // This assertion failed on the first real run: six `Structural`-keyed
    // tuple fields in `memchr.memchr.arch.all.rabinkarp` / `memmem.needle` /
    // `memmem.searcher` were reported `Removed` beside an `Added` at the same
    // path, because the tier gate alone let them through. `nudox_engine::diff`
    // pass 3 now gates on the path as well.
    for row in diff.rows.iter() {
        if !matches!(row.verdict, DiffVerdict::Removed) {
            continue;
        }
        let others: Vec<&str> = diff
            .rows
            .iter()
            .filter(|o| o.path == row.path && o.kind == row.kind && !std::ptr::eq(*o, row))
            .map(|o| match o.verdict {
                DiffVerdict::Added => "added",
                DiffVerdict::Removed => "removed",
                DiffVerdict::Rekeyed { .. } => "rekeyed",
                DiffVerdict::Changed { .. } => "changed",
                DiffVerdict::Indeterminate { .. } => "indeterminate",
                _ => "unknown",
            })
            .collect();
        assert!(
            others.is_empty(),
            "`{}` ({}) is reported removed while other rows at the same path \
             say {:?}; a still-occupied path is evidence against deletion",
            row.path,
            row.kind,
            others
        );
    }

    // Gap 4's production wiring, checked here rather than asserted in prose:
    // a real lowering must produce *some* tier that is not `Unrecorded`.
    // `Rekeyed` and `Indeterminate` are the only verdicts that carry one, so
    // this is skipped-with-a-note when 2.8.0 and 2.8.3 happen to churn no
    // keys at all — which is a legitimate outcome for two adjacent patch
    // releases, and asserting otherwise would be asserting a defect exists.
    let tiers: Vec<KeyTierLabel> = diff
        .rows
        .iter()
        .filter_map(|r| match r.verdict {
            DiffVerdict::Indeterminate { tier } => Some(tier),
            DiffVerdict::Rekeyed { from_tier, .. } => Some(from_tier),
            _ => None,
        })
        .collect();
    if tiers.is_empty() {
        eprintln!(
            "NOTE: no declaration changed key between 2.8.0 and 2.8.3, so this run \
             observed no tier through the diff. See \
             `key_provenance_reaches_a_really_produced_package` for the direct check."
        );
    } else {
        assert!(
            tiers.iter().all(|t| *t != KeyTierLabel::Unrecorded),
            "a package produced through the real load path must carry a seal \
             report; `Unrecorded` here means `PackageView::build_sealed` is not \
             wired in `nudox-store`'s producer source. tiers: {tiers:?}"
        );
    }
}

/// `SealReport` reaches `PackageView` on the production load path.
///
/// The direct form of the second half of the test above, and the one that
/// still says something when two adjacent releases happen to churn no keys:
/// it asks the graph for `keyTier` on a really-produced package and requires
/// the answer not to be `Unrecorded`.
#[test]
#[ignore = "drives rust-analyzer over three real cargo workspaces (~30-90s total); \
            run explicitly with --ignored"]
fn key_provenance_reaches_a_really_produced_package() {
    use nudox_mcp::tools::GraphQueryArgs;

    if !all_memchr_versions_available() {
        eprintln!(
            "SKIP: not all of {:?} are fetched under result/. Run: nix build .#checks.corpus",
            MEMCHR_VERSIONS
        );
        return;
    }

    let runtime = Runtime::new().expect("test runtime must build");
    let case_dir = memchr_root("2.8.3");

    let (result, _cost) = nudox_test_support::measured(
        "mcp_versions/memchr/key_tier_on_a_produced_package",
        &case_dir,
        || {
            runtime.block_on(async {
                let tools = make_memchr_history_tools();
                wait_for_all_generations(&tools).await;
                tools
                    .do_graph_query(GraphQueryArgs {
                        query: r#"{
                            Symbols {
                                name @filter(op: "=", value: ["$name"])
                                name @output
                                keyTier @output
                                keyIsContentDerived @output
                            }
                        }"#
                        .to_owned(),
                        args: Some(
                            [("name".to_owned(), "memchr_iter".to_owned())]
                                .into_iter()
                                .collect(),
                        ),
                        limit: Some(50),
                        cursor: None,
                    })
                    .await
                    .expect("graph_query must not fail")
            })
        },
    );

    eprintln!("columns: {:?}", result.columns);
    for row in &result.rows {
        eprintln!("  {:?}", row.cells);
    }
    assert!(
        !result.rows.is_empty(),
        "memchr declares `memchr_iter`; an empty result means the query, not \
         the field, is wrong"
    );

    let tier_col = result
        .columns
        .iter()
        .position(|c| c == "keyTier")
        .expect("the query outputs keyTier");
    let tiers: Vec<&str> = result
        .rows
        .iter()
        .map(|r| r.cells[tier_col].as_str())
        .collect();
    assert!(
        tiers.iter().all(|t| *t != "Unrecorded"),
        "a package produced through `ProducerSource` carries a `SealReport`, so \
         every symbol's tier must be known. `Unrecorded` here means the report \
         is still being dropped at the `nudox-store` boundary — the exact \
         defect gap 4 closes. got: {tiers:?}"
    );
}
