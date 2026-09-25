//! One engine, four real npm packages, one conversation.
//!
//! axios 1.6.7 depends on follow-redirects, form-data, and proxy-from-env.
//! Each package is loaded from its own root. Later turns use only keys the
//! earlier turns returned. A TypeScript `implementors` query names `subtypes`
//! instead of rendering a bare empty page.
//!
//! The tarballs are not in the repo. Extract them under `/tmp/medium/npm/src`
//! (or point `NUDOX_MEDIUM_NPM` at that directory) and run:
//!
//! ```text
//! cargo test -p nudox-engine --test mcp_medium_npm_conversation -- --ignored
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use heart::cost::estimated_text_tokens;
use nudox_engine::mcp::tools::{
    GraphQueryArgs, PackagesArgs, ReadArgs, RefsArgs, RefsDirection, SearchSymbolsArgs,
    SymbolFormat,
};
use nudox_engine::mcp::{MarkdownResult, NudoxTools, SymbolKeyDto};
use nudox_engine::{Engine, EngineConfig, PackageLoadEvent, PackageSpec, ProducerLanguage};

const ROOT_ENV: &str = "NUDOX_MEDIUM_NPM";
const DEFAULT_ROOT: &str = "/tmp/medium/npm/src";

struct Pkg {
    dir: &'static str,
    name: &'static str,
    version: &'static str,
    canary: &'static str,
    /// Text that must appear in the canary's rendered source.
    source_needle: &'static str,
}

const PACKAGES: &[Pkg] = &[
    Pkg {
        dir: "axios-1.6.7",
        name: "axios",
        version: "1.6.7",
        canary: "AxiosHeaders",
        source_needle: "class AxiosHeaders",
    },
    Pkg {
        dir: "follow-redirects-1.15.6",
        name: "follow-redirects",
        version: "1.15.6",
        canary: "wrap",
        source_needle: "function wrap",
    },
    Pkg {
        dir: "form-data-4.0.0",
        name: "form-data",
        version: "4.0.0",
        canary: "FormData",
        source_needle: "class FormData",
    },
    Pkg {
        dir: "proxy-from-env-1.1.0",
        name: "proxy-from-env",
        version: "1.1.0",
        canary: "getProxyForUrl",
        source_needle: "function getProxyForUrl",
    },
];

fn root() -> PathBuf {
    std::env::var(ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_ROOT))
}

fn lineage(name: &str) -> String {
    format!("npm:{name}")
}

#[tokio::test]
#[ignore = "extract axios 1.6.7 and its dependencies under /tmp/medium/npm/src"]
async fn axios_and_its_dependencies_are_one_conversation() {
    let root = root();
    let specs: Vec<PackageSpec> = PACKAGES
        .iter()
        .map(|pkg| PackageSpec {
            root: root.join(pkg.dir),
            name: pkg.name.to_owned(),
            version: pkg.version.to_owned(),
            language: ProducerLanguage::TypeScript,
        })
        .collect();
    for spec in &specs {
        assert!(
            spec.root.join("package.json").is_file(),
            "missing {} — extract the package under {}",
            spec.root.display(),
            root.display()
        );
    }

    let tools = NudoxTools::new(Engine::start_with_producer(EngineConfig::default(), specs));
    wait_until_loaded(&tools, PACKAGES.len()).await;

    let listed = tools
        .do_packages(PackagesArgs { package: None })
        .await
        .expect("packages");
    for pkg in PACKAGES {
        let row = listed
            .packages
            .iter()
            .find(|row| row.lineage == lineage(pkg.name))
            .unwrap_or_else(|| panic!("packages did not list {}", lineage(pkg.name)));
        assert!(
            row.versions
                .iter()
                .any(|version| version.version == pkg.version && version.is_current),
            "{name} is not current at {version}",
            name = pkg.name,
            version = pkg.version
        );
    }

    let mut keys = Vec::new();
    for pkg in PACKAGES {
        let hits = search_canary(&tools, pkg.canary, &lineage(pkg.name)).await;
        let mut found = None;
        for hit in &hits {
            let key = SymbolKeyDto::from_wire(&hit.hit.key);
            assert!(
                key.0.starts_with(&format!("{}#", lineage(pkg.name))),
                "{} resolved in the wrong package: {}",
                pkg.canary,
                key.0
            );
            let read = tools
                .do_read(ReadArgs {
                    keys: vec![key.clone()].into(),
                    format: SymbolFormat::Source,
                })
                .await
                .unwrap_or_else(|error| panic!("read {} failed: {error}", pkg.canary));
            let source = read.symbols[0].source.as_deref().unwrap_or("");
            if source.contains(pkg.source_needle) {
                found = Some((key, read.symbols[0].key.clone(), source.to_owned()));
                break;
            }
        }
        let (key, symbol_key, source) = found.unwrap_or_else(|| {
            panic!(
                "none of the {} hits for {} rendered {}",
                hits.len(),
                pkg.canary,
                pkg.source_needle
            )
        });
        let tokens = estimated_text_tokens(&source);
        assert!(
            tokens < 1_200,
            "read of {} is {tokens} tokens; one declaration must not dump the package:\n{source}",
            pkg.canary
        );
        let messy = format!(" `{}` ", key.0.replace(':', "::").replace('#', ":"));
        let by_messy = tools
            .do_read(ReadArgs {
                keys: vec![SymbolKeyDto(messy.clone())].into(),
                format: SymbolFormat::Source,
            })
            .await
            .unwrap_or_else(|error| panic!("messy key {messy} failed: {error}"));
        assert_eq!(
            by_messy.symbols[0].key, symbol_key,
            "paste noise must not change which declaration is read"
        );
        keys.push(key);
    }

    let again = search_canary(&tools, "AxiosHeaders", &lineage("axios")).await;
    assert!(
        again
            .iter()
            .any(|hit| SymbolKeyDto::from_wire(&hit.hit.key).0 == keys[0].0),
        "the next search turn must return the same AxiosHeaders key"
    );

    let refs = tools
        .do_refs(RefsArgs {
            key: keys[0].clone(),
            direction: RefsDirection::Out,
            limit: Some(20),
            cursor: None,
        })
        .await
        .expect("refs out of AxiosHeaders");
    let refs_page = refs.to_markdown();
    let refs_again = tools
        .do_refs(RefsArgs {
            key: keys[0].clone(),
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
    assert!(
        refs_page.contains("RawAxiosHeaders") && refs_page.contains("AxiosHeaderValue"),
        "refs of AxiosHeaders must name the types it uses:\n{refs_page}"
    );
    assert!(
        !refs_page.contains("~refs:not_recorded"),
        "a page that names targets must not also say references are unrecorded:\n{refs_page}"
    );
    let refs_tokens = estimated_text_tokens(&refs_page);
    assert!(
        refs_tokens < 1_500,
        "refs of one symbol is {refs_tokens} tokens:\n{refs_page}"
    );

    let mut args = BTreeMap::new();
    args.insert("name".to_owned(), "AxiosResponse".to_owned());
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
        .expect("implementors of AxiosResponse");
    let page = implementors.to_markdown();
    assert!(
        page.contains("~graph:edge_empty(implementors") && page.contains("subtypes"),
        "a TypeScript interface has no Rust implementors; the page must point at subtypes:\n{page}"
    );
    let tokens = estimated_text_tokens(&page);
    assert!(
        tokens < 800,
        "one empty edge is {tokens} tokens:\n{page}"
    );
}

fn names_canary(display: &str, canary: &str) -> bool {
    display.eq_ignore_ascii_case(canary)
        || display
            .rsplit(['.', ':'])
            .next()
            .is_some_and(|leaf| leaf.eq_ignore_ascii_case(canary))
}

async fn search_canary(
    tools: &NudoxTools,
    query: &str,
    package: &str,
) -> Vec<nudox_engine::mcp::tools::SearchHitDoc> {
    let result = tools
        .do_unified_search(SearchSymbolsArgs {
            query: query.to_owned(),
            kinds: None,
            packages: Some(vec![package.to_owned()]),
            limit: Some(20),
            cursor: None,
        })
        .await
        .unwrap_or_else(|error| panic!("search {query} in {package} failed: {error}"));
    let page = result.to_markdown();
    let tokens = estimated_text_tokens(&page);
    assert!(
        tokens < 2_000,
        "search page for {query} is {tokens} tokens:\n{page}"
    );
    assert!(
        page.contains("~sem:unavailable("),
        "search without an embed model must say so: {page}"
    );
    let hits: Vec<_> = result
        .hits
        .into_iter()
        .filter(|hit| names_canary(&hit.hit.display_name, query))
        .collect();
    assert!(
        !hits.is_empty(),
        "search {query} in {package} did not return that name:\n{page}"
    );
    hits
}

async fn wait_until_loaded(tools: &NudoxTools, expected: usize) {
    let events = tools.engine().packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
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
