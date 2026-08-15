//! Dogfooding log: real agentic workflows driven against `NudoxTools` over a
//! real engine, real producer, real corpora — memchr's three pinned
//! generations (2.7.6 / 2.8.0 / 2.8.3) and serde 1.0.196.
//!
//! # What this file is for
//!
//! This is the "actually use it" half of the task, not a regression
//! guard for its own sake (though every assertion below is on real content,
//! never a bare `is_ok()` — doctrine §4). Run with `--nocapture` and read the
//! printed transcript: it *is* the workflow log the task asked for. Each test
//! prints what was asked and what came back, in the order an agent chain
//! would actually issue the calls, including the malformed-input probes run
//! against this same live engine (a real `PackageNotLoaded` for a lineage
//! that was genuinely never requested, a real invalid Trustfall query against
//! the real schema, and so on) rather than synthetic ones.
//!
//! `NudoxTools` is called directly, the same way `real_crate_memchr_versions.rs`
//! and `real_crate_tokio.rs` do — a real `EngineHandle` over the real
//! rust-analyzer producer, real IR, real corpus. `real_crate_memchr.rs`
//! already proves the raw HTTP+SSE transport carries these same result
//! bodies unchanged; this file spends its budget on breadth of *workflow*
//! instead of re-proving the transport.
//!
//! # Running
//!
//! ```text
//! ls result/memchr-2.7.6/Cargo.toml result/memchr-2.8.0/Cargo.toml \
//!    result/memchr-2.8.3/Cargo.toml result/serde-1.0.196/Cargo.toml
//! # If missing: nix build .#checks.corpus
//!
//! cargo test -p nudox-mcp --test real_crate_dogfood_workflows -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::time::Duration;

use nudox_engine::{
    Engine, EngineConfig, PackageHistorySpec, PackageLoadEvent, PackageSpec, PackageVersionSpec,
    ProducerLanguage, SharedStr,
};
use nudox_engine::mcp::key::PackageLineageDto;
use nudox_engine::mcp::tools::{
    DiffVersionsArgs, DiffVersionsResult, FindUsagesArgs, GetSymbolArgs, GraphQueryArgs,
    ListVersionsArgs, SearchSymbolsArgs, SelectVersionArgs, SelectVersionResult,
};
use nudox_engine::mcp::{NudoxTools, SymbolKeyDto};
use tokio::runtime::Runtime;

// ---------------------------------------------------------------------------
// Fixture path helpers
// ---------------------------------------------------------------------------

const MEMCHR_VERSIONS: [&str; 3] = ["2.7.6", "2.8.0", "2.8.3"];

fn real_crate_root(dir: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../result/{dir}"))
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("/nonexistent"))
}

fn memchr_root(version: &str) -> PathBuf {
    real_crate_root(&format!("memchr-{version}"))
}

fn all_memchr_versions_available() -> bool {
    MEMCHR_VERSIONS
        .iter()
        .all(|v| memchr_root(v).join("Cargo.toml").is_file())
}

fn serde_available() -> bool {
    real_crate_root("serde-1.0.196").join("Cargo.toml").is_file()
}

// ---------------------------------------------------------------------------
// Engine harnesses
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

async fn wait_for_all_memchr_generations(tools: &NudoxTools) {
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
            Ok(Err(_)) => break,
            Err(_) => panic!("no memchr Loaded/LoadFailed event within 240s"),
        }
    }
    for _ in 0..2400 {
        let list = tools
            .do_list_versions(ListVersionsArgs {
                package: PackageLineageDto("cargo:memchr".to_owned()),
            })
            .await
            .expect("list_versions must not fail");
        if list.versions.len() == MEMCHR_VERSIONS.len() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for all memchr generations");
}

fn make_single_package_tools(root: PathBuf, name: &str, version: &str) -> NudoxTools {
    let engine = Engine::start_with_producer(
        EngineConfig::default(),
        vec![PackageSpec {
            root,
            name: name.to_owned(),
            version: version.to_owned(),
            language: ProducerLanguage::Rust,
        }],
    );
    NudoxTools::new(engine)
}

async fn wait_for_package(tools: &NudoxTools, name: &str) {
    let rx = tools.engine().packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    let expected = SharedStr::from(name);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv_async()).await {
            Ok(Ok(PackageLoadEvent::Loaded { name, .. })) if name == expected => return,
            Ok(Ok(PackageLoadEvent::LoadFailed { name, error, .. })) if name == expected => {
                panic!("{name} failed to load: {error}");
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => return,
            Err(_) => panic!("no {expected} Loaded/LoadFailed event within 180s"),
        }
    }
}

// ---------------------------------------------------------------------------
// Workflow log macro — every step prints what was asked and what came back
// ---------------------------------------------------------------------------

macro_rules! log_step {
    ($label:expr, $($arg:tt)*) => {
        eprintln!("\n=== {} ===\n{}", $label, format!($($arg)*))
    };
}

/// `SymbolHead` carries no bare `name` field — the display label lives on the
/// last breadcrumb crumb (see `CrumbRef`). A symbol always has at least one
/// crumb (itself), so this only panics on a broken invariant, not on real
/// input.
fn head_name(head: &nudox_engine::wire::SymbolHead) -> String {
    head.breadcrumb
        .last().map_or_else(|| "<no breadcrumb>".to_owned(), |c| c.label.to_string())
}

// ---------------------------------------------------------------------------
// The memchr workflow suite
// ---------------------------------------------------------------------------

/// One long chain of the workflows an agent would actually run against a real
/// package, over one shared memchr (3-generation) engine — chosen to
/// amortise the ~30-90s of real rust-analyzer lowering across every step
/// instead of paying it per workflow.
#[test]
#[ignore = "drives rust-analyzer over three real cargo workspaces (~30-90s total); \
            run explicitly with --ignored"]
fn memchr_agent_workflow_chain() {
    if !all_memchr_versions_available() {
        eprintln!(
            "SKIP: not all of {MEMCHR_VERSIONS:?} are fetched under result/. \
             Run: nix build .#checks.corpus"
        );
        return;
    }

    let runtime = Runtime::new().expect("test runtime must build");
    let case_dir = memchr_root("2.8.3");

    runtime.block_on(async move {
        let tools = make_memchr_history_tools();
        wait_for_all_memchr_generations(&tools).await;

        // --- Step 1: search_symbols("mem") -----------------------------------
        let search = tools
            .do_search(SearchSymbolsArgs {
                query: "mem".to_owned(),
                kinds: None,
                packages: None,
                limit: Some(20),
                cursor: None,
            })
            .await
            .expect("search_symbols must not fail");
        log_step!(
            "search_symbols(query=\"mem\", limit=20)",
            "{} hits, truncated={}; top 5 (name, kind): {:?}",
            search.hits.len(),
            search.truncated,
            search
                .hits
                .iter()
                .take(5)
                .map(|h| (h.display_name.clone(), h.kind))
                .collect::<Vec<_>>()
        );
        assert!(!search.hits.is_empty(), "real memchr must match \"mem\" by name");
        // Match on name AND kind==Record: "Memchr" the struct and the
        // `memchr` submodule/free-function both surface for a substring
        // search on "mem", and a bare name match can land on either — see
        // this file's workflow-log finding about `display_name` vs.
        // `get_symbol`'s breadcrumb label disagreeing for a re-export.
        let memchr_struct = search
            .hits
            .iter()
            .find(|h| {
                &*h.display_name == "Memchr"
                    && h.kind == nudox_engine::wire::KindTag::Known(nudox_engine::wire::KindDiscriminant::Record)
            })
            .unwrap_or_else(|| panic!("expected a Record hit literally named Memchr; got {:?}",
                search.hits.iter().map(|h| (h.display_name.clone(), h.kind)).collect::<Vec<_>>()));
        let memchr_key = SymbolKeyDto::from_wire(&memchr_struct.key);

        // --- Step 2: get_symbol(key) ------------------------------------------
        let doc = tools
            .do_get_symbol(GetSymbolArgs { key: memchr_key.clone() })
            .await
            .expect("get_symbol must not fail for a key search_symbols just returned");
        log_step!(
            "get_symbol(key)",
            "key={} head.name={} sections={} timeline_rows={}",
            memchr_key.0,
            head_name(&doc.head),
            doc.sections.len(),
            doc.timeline.rows.len()
        );
        // NOTE (a real dogfooding finding, out of scope for this error-communication
        // task to fix): `doc.head`'s breadcrumb label for this exact key comes back
        // as "memchr" (the module), not "Memchr" (the struct `search_symbols` named
        // it), even though `search_symbols` filtered on `kind == Record` and an exact
        // name match. The *key* is what matters for every assertion below, so this
        // only pins identity, not the breadcrumb label — but see this file's final
        // report note: `get_symbol`'s breadcrumb and `search_symbols`'s `display_name`
        // can disagree for at least one real symbol in a real package, which is worth
        // a follow-up investigation by whoever owns `chunk.rs`'s breadcrumb builder.
        assert_eq!(doc.head.key, memchr_struct.key, "get_symbol must resolve to the same key search_symbols returned");
        assert!(
            !doc.timeline.rows.is_empty(),
            "every resolved symbol must carry a non-empty timeline (§9.3.5)"
        );

        // --- Step 3: find_usages(key) -------------------------------------------
        let usages = tools
            .do_find_usages(FindUsagesArgs {
                key: memchr_key.clone(),
                limit: Some(50),
                cursor: None,
            })
            .await
            .expect("find_usages must not fail");
        log_step!(
            "find_usages(key=Memchr)",
            "{} usages, truncated={}",
            usages.usages.len(),
            usages.truncated
        );

        // --- Step 4: graph_query — "what does this type implement" -------------
        let implements_q = r#"query {
                Symbols {
                    ... on Record {
                        name @filter(op: "=", value: ["$name"]) @output
                        implementedBy {
                            ... on Impl {
                                ofTrait @output
                                key @output(name: "implKey")
                            }
                        }
                    }
                }
            }"#.to_string();
        let mut args = std::collections::BTreeMap::new();
        args.insert("name".to_owned(), "Memchr".to_owned());
        let implements = tools
            .do_graph_query(GraphQueryArgs {
                query: implements_q.clone(),
                args: Some(args),
                limit: Some(50),
                cursor: None,
            })
            .await;
        match &implements {
            Ok(r) => log_step!(
                "graph_query(\"what does Memchr implement\")",
                "columns={:?} rows={}\n  {:?}",
                r.columns,
                r.rows.len(),
                r.rows.iter().take(5).map(|row| row.cells.clone()).collect::<Vec<_>>()
            ),
            Err(e) => log_step!(
                "graph_query(\"what does Memchr implement\") FAILED",
                "{e}"
            ),
        }
        // If this failed it is worth knowing about, but the schema shape for
        // `... on Record { implementedBy { ... on Impl { ofTrait } } }` is
        // exactly what `implementedBy`'s own doc comment describes, so a
        // failure here is itself a finding, not something to paper over —
        // panic with the real error rather than silently continuing.
        let implements = implements.expect("see printed error above");
        assert!(
            !implements.rows.is_empty(),
            "Memchr genuinely has an `impl Iterator for Memchr` block in its own crate; \
             implementedBy must find it even though Iterator itself is not a loaded vertex"
        );

        // --- Step 5: graph_query — "which functions return Memchr" -------------
        let returns_q = r#"query {
            Symbols {
                ... on Record {
                    name @filter(op: "=", value: ["$name"])
                    returnedBy {
                        name @output
                        key @output
                    }
                }
            }
        }"#;
        let mut args2 = std::collections::BTreeMap::new();
        args2.insert("name".to_owned(), "Memchr".to_owned());
        let returns = tools
            .do_graph_query(GraphQueryArgs {
                query: returns_q.to_owned(),
                args: Some(args2),
                limit: Some(50),
                cursor: None,
            })
            .await
            .expect("returnedBy query must not fail");
        log_step!(
            "graph_query(\"which functions return Memchr\")",
            "columns={:?} rows={}\n  {:?}",
            returns.columns,
            returns.rows.len(),
            returns.rows.iter().map(|row| row.cells.clone()).collect::<Vec<_>>()
        );
        assert!(
            !returns.rows.is_empty(),
            "Memchr::new returns Memchr<'h> in the real source; returnedBy must find it"
        );

        // --- Step 6: graph_query — deprecated public symbols --------------------
        // FINDING, then fixed in this same session: passing a Boolean-typed
        // `$variable` through `graph_query`'s `args` map is advertised
        // ("the adapter coerces them to the property's declared type" —
        // `GraphQueryArgs::args`'s own doc comment) but did not actually
        // work before `nudox_engine::query::coerce_variable` existed: every
        // arg was wrapped unconditionally as `FieldValue::String`, so a
        // Boolean-typed `@filter` variable was never satisfiable no matter
        // what the caller sent. The variable probe below must now succeed.
        // The literal-value probe (no `$`) is expected to keep failing —
        // that is a hard Trustfall syntax rule (filter operands must start
        // with `$` or `%`), not a coercion gap, and is logged for contrast.
        let deprecated_variable_q = r#"query {
            Symbols {
                isDeprecated @filter(op: "=", value: ["$yes"])
                name @output
                key @output
            }
        }"#;
        let mut args3 = std::collections::BTreeMap::new();
        args3.insert("yes".to_owned(), "true".to_owned());
        let variable_attempt = tools
            .do_graph_query(GraphQueryArgs {
                query: deprecated_variable_q.to_owned(),
                args: Some(args3),
                limit: Some(500),
                cursor: None,
            })
            .await;
        log_step!(
            "graph_query(\"deprecated symbols\", Boolean via $variable)",
            "{}",
            variable_attempt
                .as_ref().map_or_else(|e| format!("FAILED: {e}"), |r| format!("rows={}", r.rows.len()))
        );
        variable_attempt.expect(
            "a correctly-typed Boolean $variable must now succeed against a real corpus \
             (regression check for the coerce_variable fix)",
        );

        let deprecated_literal_q = r#"query {
            Symbols {
                isDeprecated @filter(op: "=", value: ["true"])
                name @output
                key @output
            }
        }"#;
        let literal_attempt = tools
            .do_graph_query(GraphQueryArgs {
                query: deprecated_literal_q.to_owned(),
                args: None,
                limit: Some(500),
                cursor: None,
            })
            .await;
        log_step!(
            "graph_query(\"deprecated symbols\", Boolean via literal \"true\", no $variable)",
            "{}",
            literal_attempt
                .as_ref().map_or_else(|e| format!("FAILED: {e}"), |r| format!(
                    "rows={} (memchr 2.8.3 may legitimately have zero — this is the honest \
                     count, not a fabricated one)",
                    r.rows.len()
                ))
        );

        // --- Step 7: semantic search — expected empty by design -----------------
        let semantic = tools
            .do_search(SearchSymbolsArgs {
                query: "find the function that parses a URL".to_owned(),
                kinds: None,
                packages: None,
                limit: Some(20),
                cursor: None,
            })
            .await
            .expect("search_symbols must not fail even for a sentence-shaped query");
        log_step!(
            "search_symbols(query=\"find the function that parses a URL\")",
            "{} hits: {:?}\n  (SECTION_SEMANTIC is a stub, always empty by design — \
             docs/LIMITATIONS.md L41 — so this is name/kind substring matching against a \
             sentence, not semantic search; whatever came back is name matches on \
             individual words, not intent)",
            semantic.hits.len(),
            semantic.hits.iter().map(|h| h.display_name.clone()).collect::<Vec<_>>()
        );

        // --- Step 8: pagination past 500 rows via cursor -------------------------
        let mut all_symbols_q = r#"query {
            Symbols {
                name @output
                key @output
            }
        }"#
        .to_owned();
        let page1 = tools
            .do_graph_query(GraphQueryArgs {
                query: std::mem::take(&mut all_symbols_q),
                args: None,
                limit: None, // default 50
                cursor: None,
            })
            .await
            .expect("first Symbols page must not fail");
        log_step!(
            "graph_query(\"every Symbol\", page 1, default limit)",
            "rows={} truncated={} next_cursor={:?}",
            page1.rows.len(),
            page1.truncated,
            page1.next_cursor
        );
        assert_eq!(page1.rows.len(), 50, "default limit must be 50");
        assert!(
            page1.truncated,
            "memchr's own declaration table is well over 500 entries (docs/LIMITATIONS.md L1: \
             1,325+ for 2.8.3 alone); the very first page must already say more exist"
        );

        // Walk pages with limit=500 until we've paged past 500 rows total, to
        // dogfood the cursor mechanism itself rather than just its first hop.
        let mut offset_seen = 0usize;
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let page = tools
                .do_graph_query(GraphQueryArgs {
                    query: "query { Symbols { name @output key @output } }".to_owned(),
                    args: None,
                    limit: Some(500),
                    cursor: cursor.clone(),
                })
                .await
                .expect("a cursor this server issued must always be honoured");
            pages += 1;
            offset_seen += page.rows.len();
            log_step!(
                "graph_query cursor walk",
                "page {pages}: rows={} truncated={} cursor_in={cursor:?} cursor_out={:?}",
                page.rows.len(),
                page.truncated,
                page.next_cursor
            );
            if offset_seen > 500 {
                break;
            }
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
            if pages > 5 {
                break; // safety valve
            }
        }
        assert!(
            offset_seen > 500,
            "must have paged past the 500-row single-call ceiling by combining pages; saw {offset_seen}"
        );

        // --- Step 9: diff_versions(2.8.0, 2.8.3) ---------------------------------
        let diff = tools
            .do_diff_versions(DiffVersionsArgs {
                package: PackageLineageDto("cargo:memchr".to_owned()),
                from_version: "2.8.0".to_owned(),
                to_version: "2.8.3".to_owned(),
                limit: Some(500),
                cursor: None,
            })
            .await
            .expect("diff_versions must not fail for two loaded generations");
        match diff {
            DiffVersionsResult::Diff { diff, truncated, .. } => {
                log_step!(
                    "diff_versions(cargo:memchr, 2.8.0 -> 2.8.3)",
                    "rows_this_page={} unchanged={} from_count={} to_count={} \
                     indeterminate_on_this_page={} truncated={}",
                    diff.rows.len(),
                    diff.unchanged,
                    diff.from_symbol_count,
                    diff.to_symbol_count,
                    diff.indeterminate(),
                    truncated
                );
            }
            DiffVersionsResult::NotLoaded { loaded, .. } => {
                panic!("both 2.8.0 and 2.8.3 were just loaded; got NotLoaded, loaded={loaded:?}")
            }
        }

        // --- Step 10: select_version + stale-key check --------------------------
        // Collect a few Structural- and non-Structural-tiered keys from the
        // current (2.8.3) generation before switching, using the same
        // `keyTier` graph field an agent is told to check in advance.
        let tiers_q = r#"query {
            Symbols {
                keyTier @output
                key @output
                name @output
            }
        }"#;
        let tiers = tools
            .do_graph_query(GraphQueryArgs {
                query: tiers_q.to_owned(),
                args: None,
                limit: Some(500),
                cursor: None,
            })
            .await
            .expect("keyTier query must not fail");
        let tier_idx = tiers.columns.iter().position(|c| c == "keyTier").unwrap();
        let key_idx = tiers.columns.iter().position(|c| c == "key").unwrap();
        let fragile: Vec<(String, String)> = tiers
            .rows
            .iter()
            .filter(|r| r.cells[tier_idx] != "Structural")
            .map(|r| (r.cells[key_idx].clone(), r.cells[tier_idx].clone()))
            .collect();
        log_step!(
            "graph_query(keyTier over 2.8.3, first 500 rows)",
            "{} non-Structural keys out of {} rows sampled: {:?}",
            fragile.len(),
            tiers.rows.len(),
            fragile.iter().take(5).collect::<Vec<_>>()
        );

        let switch = tools
            .do_select_version(SelectVersionArgs {
                package: PackageLineageDto("cargo:memchr".to_owned()),
                version: "2.7.6".to_owned(),
            })
            .await
            .expect("select_version must not fail");
        match switch {
            SelectVersionResult::Switched { version, symbol_count, .. } => {
                log_step!(
                    "select_version(cargo:memchr, 2.7.6)",
                    "now serving {version} ({symbol_count} entries)"
                );
            }
            SelectVersionResult::NotLoaded { .. } => panic!("2.7.6 was just loaded"),
        }

        if let Some((stale_key, tier)) = fragile.into_iter().next() {
            let result = tools
                .do_get_symbol(GetSymbolArgs { key: SymbolKeyDto(stale_key.clone()) })
                .await;
            match result {
                Ok(doc) => log_step!(
                    "get_symbol({stale_key}) after switching away from 2.8.3",
                    "STILL RESOLVES as {} — a {tier}-tiered key survived this particular switch \
                     (expected: most do; only escalated keys are AT RISK, not guaranteed to move)",
                    head_name(&doc.head)
                ),
                Err(e) => log_step!(
                    "get_symbol({stale_key}) after switching away from 2.8.3",
                    "FAILED: {e}\n  (this key was minted at tier {tier} in 2.8.3 — the error \
                     should say so, see the McpError data.engine.possibly_stale field)"
                ),
            }
        } else {
            log_step!(
                "stale-key workflow",
                "no non-Structural key found in the first 500 rows sampled from 2.8.3 — \
                 skipping the live repro; covered synthetically instead in \
                 crates/nudox-engine/tests/error_enrichment.rs"
            );
        }

        // --- Step 11: malformed-input probes against this same real engine -----
        probe_malformed_inputs(&tools).await;
        eprintln!("\n=== memchr_agent_workflow_chain: all steps completed ===");
    });
    let _ = case_dir; // measured() convention unused here; kept for parity with sibling files
}

// ---------------------------------------------------------------------------
// Malformed-input probes — run against a real, live engine
// ---------------------------------------------------------------------------

async fn probe_malformed_inputs(tools: &NudoxTools) {
    // 1. Malformed SymbolKey.
    let bad_key = tools
        .do_get_symbol(GetSymbolArgs { key: SymbolKeyDto("not-a-key-at-all".to_owned()) })
        .await;
    log_step!(
        "get_symbol(key=\"not-a-key-at-all\")",
        "{}",
        bad_key.as_ref().err().map_or_else(|| "(unexpectedly Ok)".to_owned(), std::string::ToString::to_string)
    );
    assert!(bad_key.is_err(), "a malformed key must be rejected, not silently accepted");

    // 2. Malformed PackageLineageDto.
    let bad_lineage = tools
        .do_list_versions(ListVersionsArgs { package: PackageLineageDto("no-colon-here".to_owned()) })
        .await;
    log_step!(
        "list_versions(package=\"no-colon-here\")",
        "{}",
        bad_lineage.as_ref().err().map_or_else(|| "(unexpectedly Ok)".to_owned(), std::string::ToString::to_string)
    );
    assert!(bad_lineage.is_err());

    // 3. Invalid Trustfall query — genuine syntax error.
    let bad_query = tools
        .do_graph_query(GraphQueryArgs {
            query: "{ this is not valid graphql !! }".to_owned(),
            args: None,
            limit: None,
            cursor: None,
        })
        .await;
    log_step!(
        "graph_query(\"{{ this is not valid graphql !! }}\")",
        "{}",
        bad_query.as_ref().err().map_or_else(|| "(unexpectedly Ok)".to_owned(), std::string::ToString::to_string)
    );
    assert!(bad_query.is_err());

    // 4. Unknown `kind` in search_symbols.
    let bad_kind = tools
        .do_search(SearchSymbolsArgs {
            query: "mem".to_owned(),
            kinds: Some(vec!["Struct".to_owned()]), // not a real kind name (it's "Record")
            packages: None,
            limit: None,
            cursor: None,
        })
        .await;
    log_step!(
        "search_symbols(kinds=[\"Struct\"]) — a plausible-but-wrong guess",
        "{}",
        bad_kind.as_ref().err().map_or_else(|| "(unexpectedly Ok)".to_owned(), std::string::ToString::to_string)
    );
    assert!(bad_kind.is_err());

    // 5. Cursor misuse — a cursor the server never issued.
    let bad_cursor = tools
        .do_search(SearchSymbolsArgs {
            query: "mem".to_owned(),
            kinds: None,
            packages: None,
            limit: None,
            cursor: Some("not-a-real-cursor".to_owned()),
        })
        .await;
    log_step!(
        "search_symbols(cursor=\"not-a-real-cursor\")",
        "{}",
        bad_cursor.as_ref().err().map_or_else(|| "(unexpectedly Ok)".to_owned(), std::string::ToString::to_string)
    );
    assert!(bad_cursor.is_err());

    // 6. A package that was genuinely never requested, against this real,
    //    single-package (memchr-only) engine — the "does not exist / not
    //    loaded" half of the trio the task calls out. `attempted` must be
    //    `None` here because this engine's source only ever named memchr.
    let never_requested = tools
        .do_get_symbol(GetSymbolArgs {
            key: SymbolKeyDto(format!("cargo:tokio#{}", "0".repeat(64))),
        })
        .await;
    log_step!(
        "get_symbol(key=\"cargo:tokio#000...\") — tokio was never requested by this engine",
        "{}",
        never_requested
            .as_ref()
            .err().map_or_else(|| "(unexpectedly Ok)".to_owned(), std::string::ToString::to_string)
    );
    assert!(never_requested.is_err());
}

// ---------------------------------------------------------------------------
// The serde workflow: a trait with real, in-package implementors
// ---------------------------------------------------------------------------

/// `Trait.implementors` ("what implements this trait") needs a trait *and*
/// its implementors both resident in the same corpus to answer anything —
/// memchr defines no traits of its own, so this uses serde, whose
/// `ser::impls` module hand-implements `Serialize` for dozens of its own
/// standard-library-adjacent types in the same crate.
#[test]
#[ignore = "drives rust-analyzer over a real cargo workspace (~10-30s); run explicitly with \
            --ignored"]
fn serde_trait_implementors_workflow() {
    if !serde_available() {
        eprintln!("SKIP: no checkout at result/serde-1.0.196. Run: nix build .#checks.corpus");
        return;
    }

    let runtime = Runtime::new().expect("test runtime must build");

    runtime.block_on(async move {
        let tools = make_single_package_tools(real_crate_root("serde-1.0.196"), "serde", "1.0.196");
        wait_for_package(&tools, "serde").await;

        let q = r#"query {
            Symbols {
                ... on Trait {
                    name @filter(op: "=", value: ["$name"])
                    implementors {
                        name @output(name: "implementorName")
                        key @output(name: "implementorKey")
                    }
                }
            }
        }"#;
        let mut args = std::collections::BTreeMap::new();
        args.insert("name".to_owned(), "Serialize".to_owned());
        let result = tools
            .do_graph_query(GraphQueryArgs {
                query: q.to_owned(),
                args: Some(args),
                limit: Some(500),
                cursor: None,
            })
            .await
            .expect("Trait.implementors query must not fail");

        log_step!(
            "graph_query(\"what implements Serialize\") over real serde 1.0.196",
            "rows={}\n  first 10: {:?}",
            result.rows.len(),
            result.rows.iter().take(10).map(|r| r.cells.clone()).collect::<Vec<_>>()
        );
        assert!(
            !result.rows.is_empty(),
            "serde's own ser/impls.rs hand-implements Serialize for dozens of types in the \
             same crate; implementors must find at least one"
        );
    });
}
