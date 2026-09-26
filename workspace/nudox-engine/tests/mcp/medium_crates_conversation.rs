//! serde 1.0.229 and serde_core 1.0.229 in one engine conversation.
//!
//! serde re-exports `Serialize` from serde_core. Both packages have to seal,
//! and the serde hit has to name serde_core rather than an undeclared local.
//! Later turns reuse the key the first search returned.
//!
//! ```text
//! cargo test -p nudox-engine --test mcp_medium_crates_conversation -- --ignored
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use heart::cost::estimated_text_tokens;
use nudox_engine::mcp::tools::{
    GraphQueryArgs, PackagesArgs, ReadArgs, SearchSymbolsArgs, SymbolFormat,
};
use nudox_engine::mcp::{MarkdownResult, NudoxTools, SymbolKeyDto};
use nudox_engine::{Engine, EngineConfig, PackageLoadEvent, PackageSpec, ProducerLanguage};

const ROOT_ENV: &str = "NUDOX_MEDIUM_CRATES";
const DEFAULT_ROOT: &str = "/tmp/medium/crates";

#[tokio::test]
#[ignore = "extract serde 1.0.229 and serde_core 1.0.229 under /tmp/medium/crates"]
async fn serde_and_serde_core_are_one_conversation() {
    let root = std::env::var(ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_ROOT));
    let specs = [
        ("serde-1.0.229", "serde"),
        ("serde_core-1.0.229", "serde_core"),
    ]
    .map(|(dir, name)| PackageSpec {
        root: root.join(dir),
        name: name.to_owned(),
        version: "1.0.229".to_owned(),
        language: ProducerLanguage::Rust,
    });
    for spec in &specs {
        assert!(
            spec.root.join("Cargo.toml").is_file(),
            "missing {} — extract the crate under {}",
            spec.root.display(),
            root.display()
        );
    }

    let tools = NudoxTools::new(Engine::start_with_producer(
        EngineConfig::default(),
        specs.to_vec(),
    ));
    wait_until_loaded(&tools, specs.len()).await;

    let listed = tools
        .do_packages(PackagesArgs { package: None })
        .await
        .expect("packages");
    for name in ["serde", "serde_core"] {
        let lineage = format!("cargo:{name}");
        let row = listed
            .packages
            .iter()
            .find(|row| row.lineage == lineage)
            .unwrap_or_else(|| panic!("packages did not list {lineage}"));
        assert!(
            row.versions
                .iter()
                .any(|version| version.version == "1.0.229" && version.is_current),
            "{lineage} is not current at 1.0.229"
        );
    }

    let core = search_named(&tools, "Serialize", "cargo:serde_core")
        .await
        .into_iter()
        .next()
        .expect("serde_core Serialize");
    let core_key = SymbolKeyDto::from_wire(&core.hit.key);
    assert!(
        core_key.0.starts_with("cargo:serde_core#"),
        "Serialize in serde_core resolved elsewhere: {}",
        core_key.0
    );
    let core_read = read_source(&tools, &core_key).await;
    assert!(
        core_read.contains("trait Serialize"),
        "serde_core Serialize must be the trait, got: {core_read}"
    );
    assert!(
        estimated_text_tokens(&core_read) < 1_500,
        "one trait read is too large:\n{core_read}"
    );

    let messy = format!(" `{}` ", core_key.0.replace(':', "::").replace('#', ":"));
    let messy_read = tools
        .do_read(ReadArgs {
            keys: vec![SymbolKeyDto(messy.clone())].into(),
            format: SymbolFormat::Source,
        })
        .await
        .unwrap_or_else(|error| panic!("messy key {messy} failed: {error}"));
    assert_eq!(
        messy_read.symbols[0].key, 
        tools
            .do_read(ReadArgs {
                keys: vec![core_key.clone()].into(),
                format: SymbolFormat::Source,
            })
            .await
            .expect("reread")
            .symbols[0]
            .key,
        "paste noise must not change which declaration is read"
    );

    let again = search_named(&tools, "Serialize", "cargo:serde_core")
        .await
        .into_iter()
        .next()
        .expect("serde_core Serialize again");
    assert_eq!(
        SymbolKeyDto::from_wire(&again.hit.key).0,
        core_key.0,
        "the next search turn must return the same Serialize key"
    );

    let facades = search_named(&tools, "Serialize", "cargo:serde").await;
    let mut rendered = String::new();
    for hit in &facades {
        let symbol = tools
            .do_read(ReadArgs {
                keys: vec![SymbolKeyDto::from_wire(&hit.hit.key)].into(),
                format: SymbolFormat::Source,
            })
            .await
            .expect("read serde Serialize");
        let doc = symbol.symbols.first().expect("read returned no symbol");
        rendered.push_str(doc.signature.as_deref().unwrap_or(""));
        rendered.push('\n');
        rendered.push_str(doc.source.as_deref().unwrap_or(""));
        rendered.push('\n');
    }
    assert!(
        rendered.contains("serde_core"),
        "serde's Serialize re-export must name serde_core, not an undeclared local:\n{rendered}"
    );

    let page = implementors(&tools, "Serialize").await;
    let text = page.to_markdown();
    let named = page.rows.iter().any(|row| row.cells.iter().any(|cell| !cell.is_empty()));
    if named {
        assert!(
            page.edge_coverage.is_none(),
            "a page with implementors must not also say the edge is empty:\n{text}"
        );
    } else {
        assert!(
            text.contains("~graph:edge_empty(implementors"),
            "Serialize's implementors page must say why it is empty:\n{text}"
        );
    }
    assert!(
        estimated_text_tokens(&text) < 1_500,
        "one implementors page is too large:\n{text}"
    );
}

async fn search_named(
    tools: &NudoxTools,
    query: &str,
    package: &str,
) -> Vec<nudox_engine::mcp::tools::SearchHitDoc> {
    let result = tools
        .do_unified_search(SearchSymbolsArgs {
            query: query.to_owned(),
            kinds: Some(vec!["Trait".to_owned(), "Reexport".to_owned()]),
            packages: Some(vec![package.to_owned()]),
            limit: Some(10),
            cursor: None,
        })
        .await
        .unwrap_or_else(|error| panic!("search {query} in {package} failed: {error}"));
    let page = result.to_markdown();
    assert!(
        page.contains("~sem:unavailable("),
        "search without an embed model must say so: {page}"
    );
    assert!(
        estimated_text_tokens(&page) < 2_000,
        "search page for {query} is too large:\n{page}"
    );
    let hits: Vec<_> = result
        .hits
        .into_iter()
        .filter(|hit| {
            hit.hit
                .display_name
                .rsplit(['.', ':'])
                .next()
                .is_some_and(|leaf| leaf.eq_ignore_ascii_case(query))
        })
        .collect();
    assert!(
        !hits.is_empty(),
        "search {query} in {package} missed that name:\n{page}"
    );
    hits
}

async fn read_source(tools: &NudoxTools, key: &SymbolKeyDto) -> String {
    let read = tools
        .do_read(ReadArgs {
            keys: vec![key.clone()].into(),
            format: SymbolFormat::Source,
        })
        .await
        .unwrap_or_else(|error| panic!("read {} failed: {error}", key.0));
    read.symbols[0].source.clone().unwrap_or_default()
}

async fn implementors(
    tools: &NudoxTools,
    name: &str,
) -> nudox_engine::mcp::tools::QueryResult {
    let mut args = BTreeMap::new();
    args.insert("name".to_owned(), name.to_owned());
    tools
        .do_graph_query(GraphQueryArgs {
            query: "{
                  Symbols {
                    ... on Trait {
                      name @filter(op: \"=\", value: [\"$name\"])
                      implementors { name @output }
                    }
                  }
                }"
            .to_owned(),
            args: Some(args),
            limit: Some(10),
            cursor: None,
        })
        .await
        .unwrap_or_else(|error| panic!("implementors of {name} failed: {error}"))
}

async fn wait_until_loaded(tools: &NudoxTools, expected: usize) {
    let events = tools.engine().packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    let mut loaded = 0;
    while loaded < expected {
        match tokio::time::timeout_at(deadline, events.recv_async()).await {
            Ok(Ok(PackageLoadEvent::Loaded { .. })) => loaded += 1,
            Ok(Ok(PackageLoadEvent::LoadFailed { name, error, .. })) => {
                panic!("{name} failed to load: {error}")
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => panic!("package channel closed after {loaded} loads: {error}"),
            Err(_) => panic!("timed out after {loaded} of {expected} loads"),
        }
    }
}
