//! End-to-end MCP output and token-budget matrix.
//!
//! This target intentionally calls the public `NudoxMcpServer` methods rather
//! than `NudoxTools` directly. That keeps the engine, admission gate, typed
//! result projection, and canonical Markdown formatter in the measured path.
//!
//! The matrix covers two deliberately different package contexts:
//!
//! * `fixture:nudox-fixture-rich` — relationships, docs, occurrences, and every
//!   kind of symbol;
//! * `fixture:nudox-fixture-perf` — 10,000 repetitive functions, useful for
//!   pagination and output-growth checks.
//!
//! Every successful text response emits a `cost case=... output_bytes=...
//! output_tokens=...` line. `heart::cost::estimated_text_tokens` is a stable
//! relative estimate, not a claim about one model's tokenizer. The same log
//! format is consumed by `.config/scripts/perf-report.nu`.

use std::{future::Future, path::Path, time::Duration};

use heart::cost::{estimated_text_tokens, measured_text};
use nudox_engine::{
    Engine, EngineConfig,
    wire::KindTag,
    mcp::{
        AccountGate, NudoxMcpServer, PACKAGE_URI_PREFIX, SCHEMA_URI, SymbolKeyDto,
        key::PackageLineageDto,
        render_markdown,
        tools::{
            DiffVersionsArgs, FindUsagesArgs, GetOccurrencesArgs, GetSymbolArgs, GetSymbolsArgs,
            GraphQueryArgs, GraphSchemaArgs, IndexPackageArgs, ListPackagesArgs, ListVersionsArgs,
            SearchSymbolsArgs, SelectVersionArgs, SemanticHitRow, SemanticSearchArgs,
            SemanticSearchResult, SemanticStatus, SymbolFormat,
        },
    },
};
use rmcp::{
    ErrorData,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, ReadResourceResult, ResourceContents},
};
use serde::Serialize;
use tokio::runtime::Runtime;

const RICH: &str = "fixture:nudox-fixture-rich";
const PERF: &str = "fixture:nudox-fixture-perf";

fn harness() -> (Runtime, NudoxMcpServer) {
    let runtime = Runtime::new().expect("test runtime must build");
    let engine = Engine::start_with_fixture_set(
        EngineConfig::default(),
        nudox_engine::store::source::fixtures::FixtureSet::Both,
    );
    let server = NudoxMcpServer::new(
        engine,
        AccountGate::unmetered("token budget integration matrix"),
    );
    (runtime, server)
}

fn wait_for_packages(runtime: &Runtime, server: &NudoxMcpServer) {
    runtime.block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let result = server
                .tools()
                .do_list_packages()
                .await
                .expect("fixture package listing must work while seeding");
            if result.packages.len() == 2 {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "both fixture packages must become visible; got {:?}",
                result.packages
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
}

fn text_result(result: CallToolResult) -> String {
    let block = result
        .content
        .into_iter()
        .next()
        .expect("successful MCP tool calls must return one text block");
    match block {
        ContentBlock::Text(text) => text.text,
        other => panic!("successful MCP tool call returned non-text content: {other:?}"),
    }
}

fn measure_call<F>(runtime: &Runtime, case: &str, future: F) -> String
where
    F: Future<Output = Result<CallToolResult, ErrorData>>,
{
    let (text, _cost) = measured_text(case, Path::new(env!("CARGO_MANIFEST_DIR")), || {
        text_result(
            runtime
                .block_on(future)
                .unwrap_or_else(|error| panic!("{case} returned MCP error: {error:?}")),
        )
    });
    assert_markdown(case, &text);
    assert!(
        estimated_text_tokens(&text) > 0,
        "{case} must have a non-empty token estimate"
    );
    text
}

fn measure_structured_call<F, T>(runtime: &Runtime, case: &str, future: F) -> String
where
    F: Future<Output = Result<T, ErrorData>>,
    T: Serialize,
{
    let (wire, _cost) = measured_text(case, Path::new(env!("CARGO_MANIFEST_DIR")), || {
        let result = runtime
            .block_on(future)
            .unwrap_or_else(|error| panic!("{case} returned MCP error: {error:?}"));
        serde_json::to_string(&result).expect("MCP resource result must serialize")
    });
    assert!(estimated_text_tokens(&wire) > 0);
    wire
}

fn measure_resource_text<F>(runtime: &Runtime, case: &str, future: F) -> String
where
    F: Future<Output = Result<ReadResourceResult, ErrorData>>,
{
    let (text, _cost) = measured_text(case, Path::new(env!("CARGO_MANIFEST_DIR")), || {
        let result = runtime
            .block_on(future)
            .unwrap_or_else(|error| panic!("{case} returned MCP error: {error:?}"));
        match result
            .contents
            .into_iter()
            .next()
            .expect("resource read must return one content block")
        {
            ResourceContents::TextResourceContents { text, .. } => text,
            ResourceContents::BlobResourceContents { .. } => {
                panic!("fixture resource unexpectedly returned a binary blob")
            }
            _ => panic!("resource returned an unknown content variant"),
        }
    });
    assert!(estimated_text_tokens(&text) > 0);
    text
}

fn assert_markdown(case: &str, text: &str) {
    let trimmed = text.trim_start();
    assert!(
        trimmed.starts_with("## ") || trimmed.starts_with("# "),
        "{case} must begin with a Markdown heading, got: {trimmed:?}"
    );
    assert!(
        !trimmed.starts_with("{\""),
        "{case} must not expose a JSON object as its agent-facing format"
    );
    assert!(
        !text.contains("\"hits\":") && !text.contains("\"packages\":"),
        "{case} must not leak typed JSON field wrappers"
    );
}

fn assert_compact<T: serde::Serialize>(case: &str, markdown: &str, typed: &T) {
    let json = serde_json::to_string(typed).expect("typed MCP result must serialize");
    let markdown_tokens = estimated_text_tokens(markdown);
    let json_tokens = estimated_text_tokens(&json);
    assert!(
        markdown_tokens < json_tokens,
        "{case} Markdown must use fewer estimated agent tokens than typed JSON (markdown={} JSON={}; bytes markdown={} JSON={})",
        markdown_tokens,
        json_tokens,
        markdown.len(),
        json.len()
    );
}

fn search_key(
    runtime: &Runtime,
    server: &NudoxMcpServer,
    query: &str,
    package: &str,
) -> SymbolKeyDto {
    runtime.block_on(async {
        let result = server
            .tools()
            .do_search(SearchSymbolsArgs {
                query: query.to_owned(),
                kinds: None,
                packages: Some(vec![package.to_owned()]),
                limit: Some(16),
                cursor: None,
            })
            .await
            .unwrap_or_else(|error| panic!("search for {query:?} failed: {error:?}"));
        let hit = result
            .hits
            .first()
            .unwrap_or_else(|| panic!("search for {query:?} returned no hits"));
        SymbolKeyDto::from_wire(&hit.key)
    })
}

#[test]
fn every_mcp_facet_has_compact_markdown_measurements_across_contexts() {
    let (runtime, server) = harness();
    wait_for_packages(&runtime, &server);

    // Discovery and package context.
    let packages_args = Parameters(ListPackagesArgs::default());
    let packages = measure_call(
        &runtime,
        "mcp/both/list_packages",
        server.list_packages(packages_args),
    );
    assert!(packages.contains(RICH));
    assert!(packages.contains(PERF));

    // Resources are the other MCP protocol surface: list metadata is
    // intentionally measured as its structured wire form, while the schema
    // and package resources are measured as the text an agent actually reads.
    let resource_list = measure_structured_call(
        &runtime,
        "mcp/both/resources/list/wire",
        server.list_resources_snapshot(),
    );
    assert!(resource_list.contains(SCHEMA_URI));
    assert!(resource_list.contains(&format!("{PACKAGE_URI_PREFIX}{RICH}")));
    assert!(resource_list.contains(&format!("{PACKAGE_URI_PREFIX}{PERF}")));
    let schema_resource = measure_resource_text(
        &runtime,
        "mcp/both/resources/read/schema",
        server.read_resource_uri(SCHEMA_URI),
    );
    assert!(schema_resource.contains("type RootSchemaQuery"));
    let package_resource = measure_resource_text(
        &runtime,
        "mcp/both/resources/read/rich_package",
        server.read_resource_uri(format!("{PACKAGE_URI_PREFIX}{RICH}")),
    );
    assert_markdown("mcp/both/resources/read/rich_package", &package_resource);
    assert!(package_resource.contains(RICH));

    // Resolve stable keys once, exactly as an agent would: search first, then
    // pass the returned keys through all relationship and source facets.
    let point_key = search_key(&runtime, &server, "Point", RICH);
    let distance_key = search_key(&runtime, &server, "distance", RICH);
    let format_key = search_key(&runtime, &server, "format_point", RICH);
    let perf_key = search_key(&runtime, &server, "fn_9999", PERF);

    // Structural search in a rich package: compare the actual server response
    // with its typed result and repeat it to pin deterministic ordering.
    let rich_search_args = SearchSymbolsArgs {
        query: "Point".to_owned(),
        kinds: None,
        packages: Some(vec![RICH.to_owned()]),
        limit: Some(16),
        cursor: None,
    };
    let rich_typed = runtime
        .block_on(server.tools().do_search(rich_search_args.clone()))
        .expect("rich search must work");
    let rich_search = measure_call(
        &runtime,
        "mcp/both/rich/search_symbols",
        server.search_symbols(Parameters(rich_search_args.clone())),
    );
    assert!(rich_search.contains("Point"));
    assert!(rich_search.contains("| declaration |"));
    assert!(!rich_search.contains("| kind |"));
    assert!(!rich_search.contains("| trust |"));
    assert_compact("mcp/both/rich/search_symbols", &rich_search, &rich_typed);
    let rich_repeat = runtime
        .block_on(server.search_symbols(Parameters(rich_search_args)))
        .map(text_result)
        .expect("repeat rich search must work");
    assert_eq!(
        rich_search, rich_repeat,
        "search Markdown must be deterministic"
    );

    // The large package supplies a genuinely different context and forces the
    // pagination cursor path instead of only testing a small happy page.
    let perf_first_args = SearchSymbolsArgs {
        query: "fn_".to_owned(),
        kinds: Some(vec!["Function".to_owned()]),
        packages: Some(vec![PERF.to_owned()]),
        limit: Some(3),
        cursor: None,
    };
    let perf_first_typed = runtime
        .block_on(server.tools().do_search(perf_first_args.clone()))
        .expect("perf search must work");
    let perf_first = measure_call(
        &runtime,
        "mcp/both/perf/search_symbols/page_1",
        server.search_symbols(Parameters(perf_first_args)),
    );
    assert!(
        perf_first.contains("next:"),
        "perf page must expose its cursor"
    );
    assert!(perf_first_typed.next_cursor.is_some());
    let perf_second = measure_call(
        &runtime,
        "mcp/both/perf/search_symbols/page_2",
        server.search_symbols(Parameters(SearchSymbolsArgs {
            query: "fn_".to_owned(),
            kinds: Some(vec!["Function".to_owned()]),
            packages: Some(vec![PERF.to_owned()]),
            limit: Some(3),
            cursor: perf_first_typed.next_cursor,
        })),
    );
    assert_ne!(
        perf_first, perf_second,
        "pagination must advance the result"
    );

    // Semantic search is still an honest, compact result when this default
    // build has no embedder. This assertion prevents a missing model from
    // being represented as an empty successful search.
    let semantic = measure_call(
        &runtime,
        "mcp/both/rich/semantic_search/unavailable",
        server.semantic_search(Parameters(SemanticSearchArgs {
            query: "format values as readable text".to_owned(),
            kinds: None,
            packages: Some(vec![RICH.to_owned()]),
            limit: Some(4),
            cursor: None,
        })),
    );
    assert!(semantic.contains("status: unavailable"));

    // The live fixture build intentionally has no embedder, so also measure a
    // ready semantic projection directly. This pins the output contract for
    // the normal model-backed path without making the benchmark download a
    // model or turn model availability into a test precondition.
    let semantic_ready = SemanticSearchResult {
        query: "format points".to_owned(),
        status: SemanticStatus::Ready,
        hits: vec![SemanticHitRow {
            key: point_key.clone(),
            display_name: "Point".to_owned(),
            signature: "pub struct Point".to_owned(),
            kind: KindTag::Unknown(1),
            score: 0.91,
            documentation: Some("A point in two-dimensional space.".to_owned()),
        }],
        truncated: false,
        next_cursor: None,
    };
    let semantic_ready = measure_text_variant(
        &runtime,
        "mcp/both/rich/semantic_search/ready_projection",
        &semantic_ready,
    );
    assert!(semantic_ready.contains("| declaration |"));
    assert!(semantic_ready.contains("pub struct Point"));
    assert!(semantic_ready.contains("A point in two-dimensional space."));

    // Source-first and batched signature contexts.
    let symbol = measure_call(
        &runtime,
        "mcp/both/rich/get_symbol/source",
        server.get_symbol(Parameters(GetSymbolArgs {
            key: point_key.clone(),
        })),
    );
    assert!(symbol.contains(&point_key.0));
    assert!(symbol.contains("Point"));

    let batch_keys = vec![
        point_key.clone(),
        distance_key.clone(),
        format_key.clone(),
        perf_key.clone(),
    ];
    let batch_args = GetSymbolsArgs {
        keys: batch_keys,
        format: SymbolFormat::Signature,
    };
    let batch_typed = runtime
        .block_on(server.tools().do_get_symbols(batch_args.clone()))
        .expect("mixed rich/perf batch must work");
    let batch = measure_call(
        &runtime,
        "mcp/both/mixed/get_symbols/signature",
        server.get_symbols(Parameters(batch_args)),
    );
    assert!(batch.contains(RICH));
    assert!(batch.contains(PERF));
    assert!(batch.contains("```"));
    assert!(!batch.contains("| symbol |"));
    assert!(!batch.contains("visibility:"));
    assert_compact("mcp/both/mixed/get_symbols/signature", &batch, &batch_typed);

    // Relationship facets: reverse usages and owner-relative exact
    // occurrences are distinct questions and both must retain their keys.
    let usages = measure_call(
        &runtime,
        "mcp/both/rich/find_usages",
        server.find_usages(Parameters(FindUsagesArgs {
            key: distance_key,
            limit: Some(8),
            cursor: None,
        })),
    );
    assert!(usages.contains("format_point"));
    assert!(usages.contains("declaration"));
    assert!(usages.contains("fn format_point"));

    let occurrences = measure_call(
        &runtime,
        "mcp/both/rich/get_occurrences",
        server.get_occurrences(Parameters(GetOccurrencesArgs {
            key: format_key,
            limit: Some(8),
            cursor: None,
        })),
    );
    assert!(occurrences.contains("status: complete"));
    assert!(occurrences.contains("owner-relative bytes"));
    assert!(occurrences.contains("fn format_point"));

    // Version and diff surfaces exercise the honest not-loaded state without
    // needing a real package download in the benchmark.
    let versions = measure_call(
        &runtime,
        "mcp/both/rich/list_versions",
        server.list_versions(Parameters(ListVersionsArgs {
            package: PackageLineageDto(RICH.to_owned()),
        })),
    );
    assert!(versions.contains("0.1.0"));

    let selected = measure_call(
        &runtime,
        "mcp/both/rich/select_version/not_loaded",
        server.select_version(Parameters(SelectVersionArgs {
            package: PackageLineageDto(RICH.to_owned()),
            version: "9.9.9".to_owned(),
        })),
    );
    assert!(selected.contains("not loaded"));

    let diff = measure_call(
        &runtime,
        "mcp/both/rich/diff_versions/not_loaded",
        server.diff_versions(Parameters(DiffVersionsArgs {
            package: PackageLineageDto(RICH.to_owned()),
            from_version: "0.1.0".to_owned(),
            to_version: "9.9.9".to_owned(),
            limit: Some(8),
            cursor: None,
        })),
    );
    assert!(diff.contains("Diff unavailable"));

    // Graph facets: the schema is the exact source for the query immediately
    // below it, and the query output remains a relationship table.
    let schema = measure_call(
        &runtime,
        "mcp/both/graph_schema",
        server.graph_schema(Parameters(GraphSchemaArgs::default())),
    );
    assert!(schema.contains("type RootSchemaQuery"));

    let graph = measure_call(
        &runtime,
        "mcp/both/mixed/graph_query",
        server.graph_query(Parameters(GraphQueryArgs {
            query: "{ Symbols { key @output signature @output kind @output name @output path @output } }"
                .to_owned(),
            args: None,
            limit: Some(12),
            cursor: None,
        })),
    );
    assert!(graph.contains("```text"));
    assert!(graph.contains("// key:"));
    assert!(graph.contains("// kind: Function"));
    assert!(graph.contains("// name: fn_"));
    assert!(graph.contains("// path:"));
    assert!(!graph.contains("| declaration |"));
    assert!(graph.contains("fn fn_"));

    // Indexing is the one network-facing facet. Keep this test hermetic by
    // exercising its malformed-input error through the server and cover the
    // long-running state with the same canonical formatter used by the live
    // result. The latter is important: progress output is agent context too.
    let index_error = runtime
        .block_on(server.index_package(Parameters(IndexPackageArgs {
            purl: "not-a-package-url".to_owned(),
            wait_seconds: Some(0),
        })))
        .expect_err("malformed package URL must be rejected");
    let index_error_debug = format!("{index_error:?}");
    assert!(
        index_error_debug.contains("Index") || index_error_debug.contains("purl"),
        "index error must retain its typed cause: {index_error_debug}"
    );

    let running = nudox_engine::mcp::tools::IndexPackageResult::Running {
        purl: "pkg:cargo/example@1.2.3".to_owned(),
        stage: "downloading".to_owned(),
        received_bytes: Some(512),
        total_bytes: Some(1024),
        elapsed_seconds: 3,
        joined_existing_job: true,
    };
    let running = measure_text_variant(&runtime, "mcp/both/index_package/running", &running);
    assert!(running.contains("Index running"));
    assert!(running.contains("512/1024 B"));
}

fn measure_text_variant<T: nudox_engine::mcp::MarkdownResult>(
    _runtime: &Runtime,
    case: &str,
    result: &T,
) -> String {
    let (text, _cost) = measured_text(case, Path::new(env!("CARGO_MANIFEST_DIR")), || {
        render_markdown(result)
    });
    assert_markdown(case, &text);
    text
}
