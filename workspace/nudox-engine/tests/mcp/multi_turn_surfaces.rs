//! One conversation across the MCP surfaces, on packages the producers actually seal.
//!
//! The load is one engine: a TypeScript package and a header-only C++ package.
//! Later turns use only keys and addresses the earlier turns returned. A page
//! that is empty says why. The schema card stays smaller than the full SDL,
//! and a name search does not spend tokens on unrelated functions.

use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use heart::cost::estimated_text_tokens;
use nudox_engine::mcp::tools::{
    GraphQueryArgs, PackagesArgs, ReadArgs, RefsArgs, RefsDirection, SearchSymbolsArgs,
    SymbolFormat,
};
use nudox_engine::mcp::{MarkdownResult, NudoxTools, SymbolKeyDto};
use nudox_engine::wire::{KindDiscriminant, KindTag};
use nudox_engine::{Engine, EngineConfig, PackageLoadEvent, PackageSpec, ProducerLanguage};

const TS_PACKAGE: &str = "surfaces";
const CPP_PACKAGE: &str = "session-hpp";

fn write_packages() -> (tempfile::TempDir, tempfile::TempDir) {
    let ts = tempfile::tempdir().expect("typescript package");
    fs::write(
        ts.path().join("package.json"),
        format!(
            "{{\n  \"name\": \"{TS_PACKAGE}\",\n  \"version\": \"1.0.0\"\n}}\n"
        ),
    )
    .unwrap();
    fs::write(
        ts.path().join("index.d.ts"),
        "export class B {}\nexport class A extends B {}\nexport class AxiosResponse {}\nexport function f(): AxiosResponse;\nexport interface Marker {}\n",
    )
    .unwrap();
    fs::create_dir(ts.path().join("cjs")).unwrap();
    fs::write(
        ts.path().join("cjs/react.development.js"),
        "exports.useState = function (initialState) { return initialState; };\nvar Router = require('router');\nexports.Router = Router;\n",
    )
    .unwrap();

    let cpp = tempfile::tempdir().expect("header package");
    fs::write(
        cpp.path().join("session.hpp"),
        "#pragma once\nstruct Session { int id; };\n",
    )
    .unwrap();
    (ts, cpp)
}

fn start(ts: &std::path::Path, cpp: &std::path::Path) -> NudoxTools {
    let engine = Engine::start_with_producer(
        EngineConfig::default(),
        vec![
            PackageSpec {
                root: ts.to_path_buf(),
                name: TS_PACKAGE.to_owned(),
                version: "1.0.0".to_owned(),
                language: ProducerLanguage::TypeScript,
            },
            PackageSpec {
                root: cpp.to_path_buf(),
                name: CPP_PACKAGE.to_owned(),
                version: "0.0.0".to_owned(),
                language: ProducerLanguage::Cpp,
            },
        ],
    );
    NudoxTools::new(engine)
}

async fn wait_until_both_loaded(tools: &NudoxTools) {
    let events = tools.engine().packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut loaded = 0;
    while loaded < 2 {
        match tokio::time::timeout_at(deadline, events.recv_async()).await {
            Ok(Ok(PackageLoadEvent::Loaded { .. })) => loaded += 1,
            Ok(Ok(PackageLoadEvent::LoadFailed { name, error, .. })) => {
                panic!("{name} failed to load: {error}")
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => panic!("package channel closed after {loaded} loads: {error}"),
            Err(_) => panic!("timed out after {loaded} loads"),
        }
    }
}

fn npm() -> String {
    format!("npm:{TS_PACKAGE}")
}

async fn search_exact<'a>(
    tools: &NudoxTools,
    query: &str,
    kinds: Option<Vec<String>>,
) -> Vec<nudox_engine::mcp::tools::SearchHitDoc> {
    let result = tools
        .do_unified_search(SearchSymbolsArgs {
            query: query.to_owned(),
            kinds,
            packages: Some(vec![npm()]),
            limit: Some(20),
            cursor: None,
        })
        .await
        .unwrap_or_else(|error| panic!("search {query} failed: {error}"));
    let page = result.to_markdown();
    let tokens = estimated_text_tokens(&page);
    assert!(
        tokens < 1_500,
        "search page for {query} is {tokens} tokens; a name query must stay small:\n{page}"
    );
    assert!(
        page.contains("~sem:unavailable("),
        "search without an embed model must say so, not pretend to rank: {page}"
    );
    assert!(
        result
            .hits
            .iter()
            .all(|hit| hit.hit.display_name.eq_ignore_ascii_case(query)),
        "a name query must not pad the page with other symbols of the same kind: {}",
        result
            .hits
            .iter()
            .map(|hit| hit.hit.display_name.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    result.hits
}

#[tokio::test]
async fn one_conversation_across_packages_search_read_refs_and_graph() {
    let (ts, cpp) = write_packages();
    let tools = start(ts.path(), cpp.path());
    wait_until_both_loaded(&tools).await;

    let listed = tools
        .do_packages(PackagesArgs { package: None })
        .await
        .expect("packages");
    let names: Vec<_> = listed.packages.iter().map(|pkg| pkg.lineage.clone()).collect();
    assert!(
        names.iter().any(|lineage| lineage == &npm()),
        "packages must list the typescript package, got {names:?}"
    );
    assert!(
        names.iter().any(|lineage| lineage == &format!("cpp:{CPP_PACKAGE}")),
        "packages must list the header package, got {names:?}"
    );
    let session = tools
        .do_unified_search(SearchSymbolsArgs {
            query: "Session".to_owned(),
            kinds: Some(vec!["Record".to_owned()]),
            packages: Some(vec![format!("cpp:{CPP_PACKAGE}")]),
            limit: Some(10),
            cursor: None,
        })
        .await
        .expect("search Session");
    assert!(
        session
            .hits
            .iter()
            .any(|hit| &*hit.hit.display_name == "Session"),
        "the header-only struct must be searchable, got {:?}",
        session
            .hits
            .iter()
            .map(|hit| hit.hit.display_name.to_string())
            .collect::<Vec<_>>()
    );

    let use_state = search_exact(&tools, "useState", Some(vec!["Function".to_owned()])).await;
    assert_eq!(
        use_state.len(),
        1,
        "kinds Function must not return unrelated functions; hits: {:?}",
        use_state
            .iter()
            .map(|hit| hit.hit.display_name.to_string())
            .collect::<Vec<_>>()
    );
    let key = SymbolKeyDto::from_wire(&use_state[0].hit.key);
    let again = search_exact(&tools, "useState", Some(vec!["Function".to_owned()])).await;
    assert_eq!(
        again[0].hit.key, use_state[0].hit.key,
        "the next search turn must return the same key"
    );

    let read = tools
        .do_read(ReadArgs {
            keys: vec![key.clone()].into(),
            format: SymbolFormat::Source,
        })
        .await
        .expect("read useState");
    let symbol = &read.symbols[0];
    let source = symbol.source.as_deref().unwrap_or("");
    assert!(
        source.contains("initialState"),
        "read must return the function body, not only a name: {source}"
    );
    assert!(
        source.len() < 400,
        "read of one function must not dump the file: {source}"
    );
    let address = use_state[0].address.clone().expect("useState has an address");
    assert!(
        address.contains("::"),
        "the address search hands to the next turn contains the path separator: {address}"
    );
    let by_address = tools
        .do_read(ReadArgs {
            keys: vec![SymbolKeyDto(address)].into(),
            format: SymbolFormat::Source,
        })
        .await
        .expect("read the address from the previous turn");
    assert_eq!(
        by_address.symbols[0].key, symbol.key,
        "the address and the key must be the same declaration"
    );

    let f_hits = search_exact(&tools, "f", Some(vec!["Function".to_owned()])).await;
    assert_eq!(f_hits.len(), 1, "f must be the one function of that name");
    let f_key = SymbolKeyDto::from_wire(&f_hits[0].hit.key);
    let refs = tools
        .do_refs(RefsArgs {
            key: f_key,
            direction: RefsDirection::Out,
            limit: Some(20),
            cursor: None,
        })
        .await
        .expect("refs out");
    let refs_page = refs.to_markdown();
    match &refs {
        nudox_engine::mcp::tools::RefsResult::Out {
            occurrences,
            coverage,
            ..
        } => {
            let names_axios = occurrences.iter().any(|row| {
                row.target.as_ref().is_some_and(|target| {
                    target.signature.contains("AxiosResponse")
                        || target.path.as_deref().is_some_and(|path| path.contains("AxiosResponse"))
                }) || row.target_key.contains("AxiosResponse")
            });
            assert!(
                names_axios,
                "refs of f must name AxiosResponse, not an empty complete page:\n{refs_page}\n{occurrences:?}"
            );
            assert!(
                coverage.is_none(),
                "a recorded reference must not also carry a not-recorded note:\n{refs_page}"
            );
        }
        other => panic!("refs out must be the out direction, got {other:?}"),
    }

    let routers = search_exact(&tools, "Router", None).await;
    assert!(
        routers
            .iter()
            .any(|hit| hit.hit.kind == KindTag::Known(KindDiscriminant::Reexport)),
        "Router must be a reexport, not a const; hits: {:?}",
        routers
            .iter()
            .map(|hit| format!("{} {:?}", hit.hit.display_name, hit.hit.kind))
            .collect::<Vec<_>>()
    );

    let subtypes = graph(&tools, "B", "subtypes").await;
    assert_relation(&subtypes, "A", "subtypes");
    let subtypes_again = graph(&tools, "B", "subtypes").await;
    assert_eq!(
        subtypes.to_markdown(),
        subtypes_again.to_markdown(),
        "the same graph query on the next turn must be the same page"
    );
    let returned = graph(&tools, "AxiosResponse", "returnedBy").await;
    assert_relation(&returned, "f", "returnedBy");
    let returned_again = graph(&tools, "AxiosResponse", "returnedBy").await;
    assert_eq!(returned.to_markdown(), returned_again.to_markdown());
    for (page, edge) in [(&subtypes, "subtypes"), (&returned, "returnedBy")] {
        let tokens = estimated_text_tokens(&page.to_markdown());
        assert!(
            tokens < 800,
            "{edge} page is {tokens} tokens; one edge of one symbol must stay small:\n{}",
            page.to_markdown()
        );
    }

    let child_hits = tools
        .do_unified_search(SearchSymbolsArgs {
            query: "A".to_owned(),
            kinds: Some(vec!["Record".to_owned()]),
            packages: Some(vec![npm()]),
            limit: Some(20),
            cursor: None,
        })
        .await
        .expect("search the subtype named by the graph turn");
    let child_page = child_hits.to_markdown();
    let child_tokens = estimated_text_tokens(&child_page);
    assert!(
        child_tokens < 1_500,
        "search of the graph result is {child_tokens} tokens:\n{child_page}"
    );
    let child = child_hits
        .hits
        .iter()
        .find(|hit| &*hit.hit.display_name == "A")
        .expect("the subtype A from the previous turn must be a search hit");
    let child_key = SymbolKeyDto::from_wire(&child.hit.key);
    let child_read = tools
        .do_read(ReadArgs {
            keys: vec![child_key].into(),
            format: SymbolFormat::Source,
        })
        .await
        .expect("read the subtype from the graph turn");
    let child_source = child_read.symbols[0].source.as_deref().unwrap_or("");
    assert!(
        child_source.contains("extends B"),
        "reading the graph result must show the relation, not only the name: {child_source}"
    );
    assert!(child_source.len() < 400, "one class must not dump the file: {child_source}");

    let refs_again = tools
        .do_refs(RefsArgs {
            key: SymbolKeyDto::from_wire(&f_hits[0].hit.key),
            direction: RefsDirection::Out,
            limit: Some(20),
            cursor: None,
        })
        .await
        .expect("refs out again");
    assert_eq!(
        refs_page,
        refs_again.to_markdown(),
        "refs of the same key on the next turn must be the same page"
    );
    let refs_tokens = estimated_text_tokens(&refs_page);
    assert!(
        refs_tokens < 800,
        "refs of one function is {refs_tokens} tokens:\n{refs_page}"
    );

    let mut args = BTreeMap::new();
    args.insert("name".to_owned(), "Marker".to_owned());
    let implementors = tools
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
            limit: Some(20),
            cursor: None,
        })
        .await
        .expect("implementors is a Trait edge and must parse");
    let implementors_page = implementors.to_markdown();
    assert!(
        implementors_page.contains("~graph:edge_empty(implementors")
            && implementors_page.contains("subtypes"),
        "TypeScript implementors is Rust-only and must point at subtypes:\n{implementors_page}"
    );

    let card = nudox_engine::mcp::SCHEMA_CARD;
    assert!(
        card.contains("subtypes") && card.contains("returnedBy"),
        "the schema card must name the edges this conversation queried"
    );
    let card_tokens = estimated_text_tokens(card);
    let sdl_tokens = estimated_text_tokens(nudox_engine::mcp::SCHEMA_SDL);
    assert!(
        card_tokens * 4 < sdl_tokens,
        "the schema card ({card_tokens} tokens) must stay much smaller than the full SDL ({sdl_tokens})"
    );
}

async fn graph(tools: &NudoxTools, name: &str, edge: &str) -> nudox_engine::mcp::tools::QueryResult {
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

fn assert_relation(result: &nudox_engine::mcp::tools::QueryResult, child: &str, edge: &str) {
    let page = result.to_markdown();
    let named = result
        .rows
        .iter()
        .any(|row| row.cells.iter().any(|value| value == child));
    if named {
        assert!(
            result.edge_coverage.is_none(),
            "a page with rows must not also carry an empty-edge note:\n{page}"
        );
        return;
    }
    assert!(
        page.contains("~graph:edge_empty(") && page.contains(edge),
        "{edge} did not name {child} and did not say why the page is empty:\n{page}"
    );
}
