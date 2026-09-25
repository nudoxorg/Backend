//! Drive the live MCP tools the way an agent does, against real npm checkouts.
//!
//! Ignored: needs `/tmp/npm-corpus` from `.config/scripts/npm-popular-census.py`.
//! Writes `/tmp/npm-corpus/mcp-walk.json`.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use heart::cost::estimated_text_tokens;
use nudox_engine::mcp::render_markdown;
use nudox_engine::mcp::tools::{
    GraphQueryArgs, KeysArg, PackagesArgs, ReadArgs, RefsArgs, RefsDirection, SchemaResult,
    SearchSymbolsArgs, SymbolFormat,
};
use nudox_engine::mcp::{NudoxTools, SymbolKeyDto};
use nudox_engine::{Engine, EngineConfig, PackageLoadEvent, PackageSpec, ProducerLanguage};
use serde_json::{Value, json};

const NAMES: &[&str] = &[
    "react",
    "axios",
    "dayjs",
    "immer",
    "zustand",
    "express",
    "hono",
    "commander",
];

fn corpus() -> PathBuf {
    PathBuf::from(std::env::var("NUDOX_NPM_CORPUS").unwrap_or_else(|_| "/tmp/npm-corpus".into()))
}

fn specs() -> Vec<PackageSpec> {
    let root = corpus();
    let manifest: Vec<Value> =
        serde_json::from_str(&fs::read_to_string(root.join("manifest.json")).expect("manifest"))
            .expect("manifest json");
    let mut specs = Vec::new();
    for want in NAMES {
        let Some(row) = manifest.iter().find(|row| row["name"].as_str() == Some(want)) else {
            continue;
        };
        let dir = root.join(row["dir"].as_str().unwrap());
        if !dir.join("package.json").is_file() {
            eprintln!("skip {want}: no package.json at {}", dir.display());
            continue;
        }
        specs.push(PackageSpec {
            root: dir,
            name: (*want).to_owned(),
            version: row["version"].as_str().unwrap().to_owned(),
            language: ProducerLanguage::TypeScript,
        });
    }
    specs
}

fn key_of(hit: &nudox_engine::mcp::tools::SearchHitDoc) -> SymbolKeyDto {
    SymbolKeyDto(format!(
        "{}:{}#{}",
        hit.hit.key.package.ecosystem, hit.hit.key.package.name, hit.hit.key.intro.to_hex()
    ))
}

async fn wait_loaded(tools: &NudoxTools, expect: usize) -> Vec<Value> {
    let events = tools.engine().packages();
    let deadline = Instant::now() + Duration::from_secs(20 * 60);
    let mut seen = Vec::new();
    while seen.len() < expect && Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(30), events.recv_async()).await {
            Ok(Ok(PackageLoadEvent::Loaded {
                name,
                version,
                symbol_count,
                ..
            })) => {
                eprintln!("loaded {name} symbols={symbol_count}");
                seen.push(json!({"name": name.to_string(), "version": version, "symbol_count": symbol_count, "ok": true}));
            }
            Ok(Ok(PackageLoadEvent::LoadFailed { name, error, .. })) => {
                eprintln!("failed {name}: {error}");
                seen.push(json!({"name": name.to_string(), "ok": false, "error": error.to_string()}));
            }
            Ok(Ok(other)) => eprintln!("event {other:?}"),
            Ok(Err(error)) => {
                seen.push(json!({"ok": false, "error": format!("channel closed: {error}")}));
                break;
            }
            Err(_) => {
                if seen.len() >= expect {
                    break;
                }
            }
        }
    }
    seen
}

fn note(label: &str, markdown: &str) -> Value {
    json!({
        "tool": label,
        "bytes": markdown.len(),
        "estimated_tokens": estimated_text_tokens(markdown),
        "markdown": markdown,
    })
}

#[tokio::test]
#[ignore]
async fn agent_walks_real_typescript_packages() {
    let specs = specs();
    assert!(!specs.is_empty(), "corpus must contain the walk packages");
    let expect = specs.len();
    let engine = Engine::start_with_producer(EngineConfig::default(), specs);
    let tools = NudoxTools::new(engine);
    let loads = wait_loaded(&tools, expect).await;

    let mut steps: Vec<Value> = Vec::new();

    match tools.do_packages(PackagesArgs { package: None }).await {
        Ok(result) => steps.push(note("packages", &render_markdown(&result))),
        Err(error) => steps.push(json!({"tool": "packages", "error": error.to_string()})),
    }

    let card = SchemaResult {
        schema: nudox_engine::mcp::SCHEMA_CARD.to_owned(),
        full: false,
    };
    steps.push(note("schema", &render_markdown(&card)));
    let full = SchemaResult {
        schema: nudox_engine::mcp::SCHEMA_SDL.to_owned(),
        full: true,
    };
    let full_md = render_markdown(&full);
    steps.push(json!({
        "tool": "schema_full",
        "bytes": full_md.len(),
        "estimated_tokens": estimated_text_tokens(&full_md),
    }));

    let searches = [
        ("axios request", "request", Some(vec!["npm:axios".into()]), None),
        ("react useState", "useState", Some(vec!["npm:react".into()]), None),
        ("dayjs", "dayjs", Some(vec!["npm:dayjs".into()]), None),
        ("express Router", "Router", Some(vec!["npm:express".into()]), Some(vec!["Record".into(), "Function".into()])),
        ("param request", "request", None, Some(vec!["Param".into()])),
    ];
    let mut first_key: Option<SymbolKeyDto> = None;
    let mut first_address: Option<String> = None;
    for (label, query, packages, kinds) in searches {
        match tools
            .do_unified_search(SearchSymbolsArgs {
                query: query.into(),
                kinds,
                packages,
                limit: Some(8),
                cursor: None,
            })
            .await
        {
            Ok(result) => {
                if first_key.is_none() {
                    if let Some(hit) = result.hits.first() {
                        first_key = Some(key_of(hit));
                        first_address = hit.address.clone();
                    }
                }
                let preview: Vec<_> = result
                    .hits
                    .iter()
                    .take(5)
                    .map(|hit| {
                        json!({
                            "name": hit.hit.display_name.to_string(),
                            "kind": format!("{:?}", hit.hit.kind),
                            "address": hit.address,
                            "semantic": hit.semantic,
                        })
                    })
                    .collect();
                let mut row = note(&format!("search:{label}"), &render_markdown(&result));
                row["hits"] = json!(preview);
                steps.push(row);
            }
            Err(error) => steps.push(json!({"tool": format!("search:{label}"), "error": error.to_string()})),
        }
    }

    if let Some(key) = first_key.clone() {
        match tools
            .do_read(ReadArgs {
                keys: KeysArg(vec![key.clone()]),
                format: SymbolFormat::Source,
            })
            .await
        {
            Ok(result) => steps.push(note("read:key", &render_markdown(&result))),
            Err(error) => steps.push(json!({"tool": "read:key", "error": error.to_string()})),
        }
        for (label, direction) in [("refs:in", RefsDirection::In), ("refs:out", RefsDirection::Out)] {
            match tools
                .do_refs(RefsArgs {
                    key: key.clone(),
                    direction,
                    limit: Some(15),
                    cursor: None,
                })
                .await
            {
                Ok(result) => steps.push(note(label, &render_markdown(&result))),
                Err(error) => steps.push(json!({"tool": label, "error": error.to_string()})),
            }
        }
    }
    if let Some(address) = first_address.clone() {
        match tools
            .do_read(ReadArgs {
                keys: KeysArg(vec![SymbolKeyDto(address.clone())]),
                format: SymbolFormat::Signature,
            })
            .await
        {
            Ok(result) => steps.push(note("read:address", &render_markdown(&result))),
            Err(error) => steps.push(json!({"tool": "read:address", "error": error.to_string()})),
        }
    }

    let graphs = [
        (
            "graph:packages",
            "{ Packages { name @output ecosystem @output } }",
            None,
        ),
        (
            "graph:axios-functions",
            "{ Symbols { ... on Function { name @filter(op: \"=\", value: [\"$name\"]) key @output path @output isPublic @output } } }",
            Some("request"),
        ),
        (
            "graph:implementors",
            "{ Symbols { ... on Trait { name @filter(op: \"=\", value: [\"$name\"]) implementors { name @output key @output } } } }",
            Some("AxiosAdapter"),
        ),
        (
            "graph:returnedBy",
            "{ Symbols { name @filter(op: \"=\", value: [\"$name\"]) returnedBy { ... on Function { name @output key @output } } } }",
            Some("AxiosResponse"),
        ),
    ];
    for (label, query, name) in graphs {
        let args = name.map(|n| {
            let mut map = std::collections::BTreeMap::new();
            map.insert("name".into(), n.into());
            map
        });
        match tools
            .do_graph_query(GraphQueryArgs {
                query: query.into(),
                args,
                limit: Some(20),
                cursor: None,
            })
            .await
        {
            Ok(result) => {
                let mut row = note(label, &render_markdown(&result));
                row["row_count"] = json!(result.rows.len());
                row["edge_coverage"] = json!(result.edge_coverage.as_ref().map(|n| n.reason.clone()));
                steps.push(row);
            }
            Err(error) => steps.push(json!({"tool": label, "error": error.to_string()})),
        }
    }

    let report = json!({"loads": loads, "steps": steps, "address": first_address});
    let path = corpus().join("mcp-walk.json");
    fs::write(&path, serde_json::to_string_pretty(&report).unwrap()).expect("write walk");
    eprintln!("wrote {}", path.display());
    assert!(loads.iter().any(|row| row["ok"] == true), "at least one package must load");
}
