//! Real corpus/package reference regressions through the production MCP path.
//!
//! The language integration tests prove that each producer can lower real
//! declarations.  These checks ask the stronger question: after
//! `ProducerSource` has sealed and indexed those declarations, does the MCP
//! surface expose a real caller -> callee occurrence?  Every source named
//! below is provisioned corpus material; the clang case only copies its real
//! single-header source into a temporary translation unit so the oracle can
//! visit header declarations without mutating the read-only corpus.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use nudox_engine::mcp::{
    NudoxTools, SymbolKeyDto,
    tools::{RefsArgs, RefsDirection, RefsResult, SearchSymbolsArgs},
};
use nudox_engine::{Engine, EngineConfig, PackageLoadEvent, PackageSpec, ProducerLanguage};

fn corpus_root() -> PathBuf {
    std::env::var_os("NUDOX_CORPUS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join("result")
        })
}

fn key(hit: &nudox_engine::mcp::tools::SearchHitDoc) -> SymbolKeyDto {
    SymbolKeyDto(format!(
        "{}:{}#{}",
        hit.hit.key.package.ecosystem,
        hit.hit.key.package.name,
        hit.hit.key.intro.to_hex()
    ))
}

async fn loaded(tools: &NudoxTools, label: &str) {
    match tokio::time::timeout(
        Duration::from_secs(300),
        tools.engine().packages().recv_async(),
    )
    .await
    {
        Ok(Ok(PackageLoadEvent::Loaded { .. })) => {}
        Ok(Ok(event)) => panic!("{label} did not load: {event:?}"),
        Ok(Err(error)) => panic!("{label} load stream closed: {error}"),
        Err(_) => panic!("{label} did not load within five minutes"),
    }
}

async fn symbols(tools: &NudoxTools, package: &str, name: &str) -> Vec<SymbolKeyDto> {
    let result = tools
        .do_search(SearchSymbolsArgs {
            query: name.to_owned(),
            kinds: Some(vec!["Function".to_owned()]),
            packages: Some(vec![package.to_owned()]),
            limit: Some(100),
            cursor: None,
        })
        .await
        .unwrap_or_else(|error| panic!("search for {package}::{name} failed: {error}"));
    result
        .hits
        .iter()
        .filter(|hit| {
            let display = &*hit.hit.display_name;
            display == name || display.ends_with(&format!(".{name}"))
        })
        .map(key)
        .collect()
}

async fn assert_edge(tools: &NudoxTools, package: &str, caller_name: &str, callee_name: &str) {
    let callers = symbols(tools, package, caller_name).await;
    let callees = symbols(tools, package, callee_name).await;
    assert!(
        !callers.is_empty(),
        "{package}: real caller {caller_name} was not lowered"
    );
    assert!(
        !callees.is_empty(),
        "{package}: real callee {callee_name} was not lowered"
    );
    let callee_keys: std::collections::HashSet<&str> =
        callees.iter().map(|key| key.0.as_str()).collect();

    let mut seen_out = false;
    for caller in callers {
        let outgoing = tools
            .do_refs(RefsArgs {
                key: caller.clone(),
                direction: RefsDirection::Out,
                limit: Some(100),
                cursor: None,
            })
            .await
            .unwrap_or_else(|error| panic!("{package}: refs(out) failed: {error}"));
        if let RefsResult::Out { occurrences, .. } = outgoing {
            seen_out |= occurrences
                .iter()
                .any(|occurrence| callee_keys.contains(occurrence.target_key.as_str()));
        }
    }

    assert!(
        seen_out,
        "{package}: {caller_name} has no sealed occurrence targeting {callee_name}"
    );
}

fn start(root: PathBuf, name: &str, version: &str, language: ProducerLanguage) -> NudoxTools {
    NudoxTools::new(Engine::start_with_producer(
        EngineConfig::default(),
        vec![PackageSpec {
            root,
            name: name.to_owned(),
            version: version.to_owned(),
            language,
        }],
    ))
}

#[tokio::test]
async fn real_go_zap_refs_expose_atomic_level_factory_to_default() {
    let tools = start(
        corpus_root().join("go.uber.org__zap-v1.28.0"),
        "go.uber.org/zap",
        "v1.28.0",
        ProducerLanguage::Go,
    );
    loaded(&tools, "go.uber.org/zap").await;
    assert_edge(
        &tools,
        "go:go.uber.org/zap",
        "NewAtomicLevelAt",
        "NewAtomicLevel",
    )
    .await;
}

#[tokio::test]
async fn real_gson_refs_expose_delegate_accessor_to_delegate() {
    let tools = start(
        corpus_root().join("com.google.code.gson__gson-2.10.1"),
        "com.google.code.gson:gson",
        "2.10.1",
        ProducerLanguage::Java,
    );
    loaded(&tools, "com.google.code.gson:gson").await;
    assert_edge(
        &tools,
        "maven:com.google.code.gson:gson",
        "getSerializationDelegate",
        "delegate",
    )
    .await;
}

#[tokio::test]
async fn real_requests_refs_expose_session_get_to_request() {
    let tools = start(
        corpus_root().join("requests-2.31.0"),
        "requests",
        "2.31.0",
        ProducerLanguage::Python,
    );
    loaded(&tools, "requests").await;
    assert_edge(&tools, "pypi:requests", "get", "request").await;
}

#[tokio::test]
async fn real_polly_refs_expose_builder_to_component() {
    let root = corpus_root()
        .join("Polly.Source-8.3.0")
        .join("Polly-8.3.0")
        .join("src")
        .join("Polly.Core");
    let tools = start(root, "Polly.Source", "8.3.0", ProducerLanguage::CSharp);
    loaded(&tools, "Polly source").await;
    assert_edge(
        &tools,
        "nuget:Polly.Source",
        "Build",
        "BuildPipelineComponent",
    )
    .await;
}

#[tokio::test]
async fn real_nlohmann_refs_expose_dump_to_is_object() {
    let source_root = corpus_root().join("nlohmann-json-v3.11.3");
    let fixture = tempfile::tempdir().expect("create clang corpus probe directory");
    let include = fixture.path().join("single_include/nlohmann");
    std::fs::create_dir_all(&include).expect("create nlohmann include directory");
    std::fs::copy(
        source_root.join("single_include/nlohmann/json.hpp"),
        include.join("json.hpp"),
    )
    .expect("copy real nlohmann json.hpp");
    std::fs::copy(include.join("json.hpp"), include.join("json.cpp"))
        .expect("copy real header as C++ TU");
    let db = serde_json::json!([{
        "directory": fixture.path(),
        "file": "single_include/nlohmann/json.cpp",
        "arguments": ["c++", "-Isingle_include", "-std=c++17", "-x", "c++", "-c"]
    }]);
    std::fs::write(
        fixture.path().join("compile_commands.json"),
        serde_json::to_vec_pretty(&db).expect("serialize compile commands"),
    )
    .expect("write compile_commands.json");
    let tools = start(
        fixture.path().to_path_buf(),
        "nlohmann-json",
        "v3.11.3",
        ProducerLanguage::Cpp,
    );
    loaded(&tools, "nlohmann-json").await;
    assert_edge(&tools, "cpp:nlohmann-json", "merge_patch", "is_object").await;
}
