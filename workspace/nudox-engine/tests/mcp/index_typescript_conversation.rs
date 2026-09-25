//! `index` loads a local TypeScript package, and the next turns use it.
//!
//! The corpus starts empty. The only way `Widget` becomes searchable is the
//! index call. A registry dependency is named and not pretended to be loaded.

use std::collections::BTreeMap;

use nudox_engine::mcp::tools::{
    GraphQueryArgs, IndexArgs, PackagesArgs, ReadArgs, SearchSymbolsArgs, SymbolFormat,
};
use nudox_engine::mcp::{MarkdownResult, McpError, NudoxTools, SymbolKeyDto};
use nudox_engine::{Engine, EngineConfig};

const NAME: &str = "widget-index";

fn write_package() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("package");
    std::fs::write(
        dir.path().join("package.json"),
        format!(
            "{{\n  \"name\": \"{NAME}\",\n  \"version\": \"1.2.3\",\n  \"dependencies\": {{ \"left-pad\": \"^1.0.0\" }}\n}}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("index.d.ts"),
        "export class Widget {}\nexport function make(): Widget;\n",
    )
    .unwrap();
    dir
}

fn tools() -> NudoxTools {
    NudoxTools::new(Engine::start_with_producer(EngineConfig::default(), vec![]))
}

#[tokio::test]
async fn indexing_a_local_typescript_package_opens_it_for_search_read_and_graph() {
    let pkg = write_package();
    let tools = tools();
    let root = pkg.path().display().to_string();

    let empty = tools
        .do_index(IndexArgs {
            targets: vec![],
            dependencies: false,
            depth: None,
            wait_seconds: Some(30),
        })
        .await
        .expect_err("an empty target list must be an error");
    match empty {
        McpError::InvalidArgument { argument, .. } => assert_eq!(argument, "targets"),
        other => panic!("expected InvalidArgument on targets, got {other:?}"),
    }

    let missing = tempfile::tempdir().expect("empty dir");
    let bad = tools
        .do_index(IndexArgs {
            targets: vec![missing.path().display().to_string()],
            dependencies: false,
            depth: None,
            wait_seconds: Some(30),
        })
        .await
        .expect("a bad target is a failed row, not an error");
    assert_eq!(bad.indexed.len(), 0, "nothing was indexed: {:?}", bad.indexed);
    assert!(
        bad.failed.iter().any(|fail| fail.target == missing.path().display().to_string()
            && fail.error.contains("manifest")),
        "the failure must name the directory and the missing manifest: {:?}",
        bad.failed
    );

    let indexed = tools
        .do_index(IndexArgs {
            targets: vec![root.clone()],
            dependencies: true,
            depth: Some(1),
            wait_seconds: Some(60),
        })
        .await
        .expect("index");
    assert!(
        indexed.failed.is_empty(),
        "the package must index: {:?}",
        indexed.failed
    );
    let landed = indexed
        .indexed
        .iter()
        .find(|row| row.package == format!("npm:{NAME}"))
        .unwrap_or_else(|| panic!("indexed packages: {:?}", indexed.indexed));
    // A local checkout is not a release. Index reports 0.0.0, the same
    // version a start-up load of this directory would use, so the two paths
    // do not become two generations.
    assert_eq!(landed.version, "0.0.0");
    assert!(
        indexed
            .not_scanned
            .iter()
            .any(|gap| gap.reason.contains("left-pad")),
        "the registry dependency must be named, not silently dropped: {:?}",
        indexed.not_scanned
    );
    assert!(
        indexed
            .indexed
            .iter()
            .all(|row| !row.package.contains("left-pad")),
        "left-pad was not resolved and must not appear as indexed: {:?}",
        indexed.indexed
    );

    let hit = search(&tools, "Widget").await;
    let key = SymbolKeyDto::from_wire(&hit.hit.key);
    assert!(
        key.0.starts_with(&format!("npm:{NAME}#")),
        "Widget must belong to the package index just loaded: {}",
        key.0
    );
    let source = read(&tools, &key).await;
    assert!(
        source.contains("class Widget"),
        "read of the indexed class must show the declaration: {source}"
    );

    let returned = graph(&tools, "Widget", "returnedBy").await;
    let page = returned.to_markdown();
    let names_make = returned
        .rows
        .iter()
        .any(|row| row.cells.iter().any(|cell| cell == "make"));
    assert!(
        names_make,
        "make returns Widget; the edge must name it:\n{page}"
    );
    assert!(
        returned.edge_coverage.is_none(),
        "a page that names make must not also say the edge is empty:\n{page}"
    );

    let listed = tools
        .do_packages(PackagesArgs { package: None })
        .await
        .expect("packages");
    let listed_row = listed
        .packages
        .iter()
        .find(|row| row.lineage == format!("npm:{NAME}"))
        .unwrap_or_else(|| panic!("packages must list what index loaded: {:?}", listed.packages));
    assert!(
        listed_row
            .versions
            .iter()
            .any(|version| version.version == "0.0.0" && version.is_current),
        "packages must show the same generation index reported: {:?}",
        listed_row.versions
    );
}

async fn search(tools: &NudoxTools, name: &str) -> nudox_engine::mcp::tools::SearchHitDoc {
    let result = tools
        .do_unified_search(SearchSymbolsArgs {
            query: name.to_owned(),
            kinds: None,
            packages: Some(vec![format!("npm:{NAME}")]),
            limit: Some(10),
            cursor: None,
        })
        .await
        .unwrap_or_else(|error| panic!("search {name} failed: {error}"));
    let page = result.to_markdown();
    result
        .hits
        .into_iter()
        .find(|hit| {
            hit.hit
                .display_name
                .rsplit(['.', ':'])
                .next()
                .is_some_and(|leaf| leaf == name)
        })
        .unwrap_or_else(|| panic!("search missed {name}:\n{page}"))
}

async fn read(tools: &NudoxTools, key: &SymbolKeyDto) -> String {
    let read = tools
        .do_read(ReadArgs {
            keys: vec![key.clone()].into(),
            format: SymbolFormat::Source,
        })
        .await
        .unwrap_or_else(|error| panic!("read {} failed: {error}", key.0));
    read.symbols[0].source.clone().unwrap_or_default()
}

async fn graph(
    tools: &NudoxTools,
    name: &str,
    edge: &str,
) -> nudox_engine::mcp::tools::QueryResult {
    let mut args = BTreeMap::new();
    args.insert("name".to_owned(), name.to_owned());
    tools
        .do_graph_query(GraphQueryArgs {
            query: format!(
                "{{
                  Symbols {{
                    name @filter(op: \"=\", value: [\"$name\"])
                    {edge} {{ name @output }}
                  }}
                }}"
            ),
            args: Some(args),
            limit: Some(20),
            cursor: None,
        })
        .await
        .unwrap_or_else(|error| panic!("graph {edge} of {name} failed: {error}"))
}
