use std::fs;
use std::time::Duration;

use nudox_engine::mcp::NudoxTools;
use nudox_engine::mcp::tools::SearchSymbolsArgs;
use nudox_engine::{Engine, EngineConfig, PackageLoadEvent, PackageSpec, ProducerLanguage};

const PACKAGE: &str = "nudox-engine-ts-fixture";

#[tokio::test]
async fn typescript_producer_loads_named_symbol_through_engine() {
    let fixture = tempfile::tempdir().expect("create TypeScript fixture");
    fs::write(
        fixture.path().join("package.json"),
        r#"{"name":"nudox-engine-ts-fixture","version":"0.1.0","main":"index.ts"}"#,
    )
    .expect("write package manifest");
    fs::write(
        fixture.path().join("index.ts"),
        "export function greet(name: string): string { return `hello ${name}`; }\n",
    )
    .expect("write TypeScript source");

    let tools = NudoxTools::new(Engine::start_with_producer(
        EngineConfig::default(),
        vec![PackageSpec {
            root: fixture.path().to_path_buf(),
            name: PACKAGE.to_owned(),
            version: "0.1.0".to_owned(),
            language: ProducerLanguage::TypeScript,
        }],
    ));

    let event = tokio::time::timeout(
        Duration::from_secs(10),
        tools.engine().packages().recv_async(),
    )
    .await
    .expect("engine load timed out")
    .expect("package event channel closed");
    assert!(
        matches!(event, PackageLoadEvent::Loaded { .. }),
        "fixture must load through the TypeScript producer: {event:?}"
    );

    let result = tools
        .do_search(SearchSymbolsArgs {
            query: "greet".to_owned(),
            kinds: Some(vec!["Function".to_owned()]),
            packages: Some(vec![format!("npm:{PACKAGE}")]),
            limit: Some(10),
            cursor: None,
        })
        .await
        .expect("search through the loaded engine must succeed");

    assert!(
        result
            .hits
            .iter()
            .any(|hit| &*hit.hit.display_name == "greet"),
        "engine/store path must expose named TypeScript symbol `greet`; got {:?}",
        result
            .hits
            .iter()
            .map(|hit| &*hit.hit.display_name)
            .collect::<Vec<_>>()
    );
}
