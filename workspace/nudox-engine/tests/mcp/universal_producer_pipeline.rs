//! Adversarial end-to-end test of the universal producer → index → MCP path.
//!
//! A fixture `IrView` cannot exercise this path: it already contains sealed
//! IDs, postings and source data, so it would stay green if a real producer
//! dropped body references or if `ProducerSource` forgot to attach a sidecar.
//! This test writes a small but ordinary Rust crate, starts the production
//! engine constructor, and asks its public MCP tool facade the questions an
//! agent actually asks.

use std::fs;
use std::time::Duration;

use nudox_engine::mcp::tools::{FindUsagesArgs, GetSymbolsArgs, SearchSymbolsArgs, SymbolFormat};
use nudox_engine::mcp::{McpError, NudoxTools, SymbolKeyDto};
use nudox_engine::{Engine, EngineConfig, PackageLoadEvent, PackageSpec, ProducerLanguage};

const PACKAGE: &str = "nudox_universal_mcp_fixture";

// `spelling_only` is intentional adversarial input: a text search or an
// untyped tree walk would report it as a user of `Payload`, even though the
// string literal neither names the type in a declaration nor resolves to it in
// a body.  Precise find_usages must exclude it.
const LIB_RS: &str = r#"
#[derive(Clone, Debug)]
pub struct Payload(pub u32);

pub fn target(value: Payload) -> Payload {
    Payload(value.0 + 1)
}

pub fn caller(value: Payload) -> Payload {
    target(value)
}

pub fn spelling_only() -> &'static str {
    "Payload"
}
"#;

fn source_fixture() -> tempfile::TempDir {
    let fixture = tempfile::tempdir().expect("create temporary Rust package");
    fs::create_dir_all(fixture.path().join("src")).expect("create fixture src/");
    fs::write(
        fixture.path().join("Cargo.toml"),
        format!(
            "[package]\nname = \"{PACKAGE}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n"
        ),
    )
    .expect("write fixture manifest");
    fs::write(fixture.path().join("src/lib.rs"), LIB_RS).expect("write fixture source");
    fixture
}

fn key_of(hit: &nudox_engine::HitRow) -> SymbolKeyDto {
    SymbolKeyDto(format!(
        "{}:{}#{}",
        hit.key.package.ecosystem,
        hit.key.package.name,
        hit.key.intro.to_hex()
    ))
}

async fn wait_until_loaded(tools: &NudoxTools) {
    let events = tools.engine().packages();
    match tokio::time::timeout(Duration::from_secs(30), events.recv_async()).await {
        Ok(Ok(PackageLoadEvent::Loaded { .. })) => {}
        Ok(Ok(PackageLoadEvent::LoadFailed { error, .. })) => {
            panic!("the self-contained Rust package must load: {error}")
        }
        Ok(Ok(other)) => panic!("unexpected package event while loading fixture: {other:?}"),
        Ok(Err(error)) => panic!("package event channel closed before fixture loaded: {error}"),
        Err(_) => panic!("production engine did not load real fixture within 30 seconds"),
    }
}

async fn one_function(tools: &NudoxTools, name: &str) -> SymbolKeyDto {
    let result = tools
        .do_search(SearchSymbolsArgs {
            query: name.to_owned(),
            kinds: Some(vec!["Function".to_owned()]),
            packages: Some(vec![format!("cargo:{PACKAGE}")]),
            limit: Some(10),
            cursor: None,
        })
        .await
        .unwrap_or_else(|error| panic!("search for {name} must succeed: {error}"));
    let matches: Vec<_> = result
        .hits
        .iter()
        .filter(|hit| hit.display_name.as_ref() == name)
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "real source must produce exactly one Function named {name}; got {:?}",
        result
            .hits
            .iter()
            .map(|hit| hit.display_name.as_ref())
            .collect::<Vec<_>>()
    );
    key_of(matches[0])
}

async fn one_symbol(tools: &NudoxTools, name: &str, kind: &str) -> SymbolKeyDto {
    let result = tools
        .do_search(SearchSymbolsArgs {
            query: name.to_owned(),
            kinds: Some(vec![kind.to_owned()]),
            packages: Some(vec![format!("cargo:{PACKAGE}")]),
            limit: Some(10),
            cursor: None,
        })
        .await
        .unwrap_or_else(|error| panic!("search for {name} must succeed: {error}"));
    let hit = result
        .hits
        .iter()
        .find(|hit| hit.display_name.as_ref() == name)
        .unwrap_or_else(|| {
            panic!(
                "real source must produce {kind} {name}; got {:?}",
                result
                    .hits
                    .iter()
                    .map(|hit| hit.display_name.as_ref())
                    .collect::<Vec<_>>()
            )
        });
    key_of(hit)
}

#[tokio::test]
async fn real_rust_source_reaches_precise_usages_and_compact_batched_mcp() {
    let fixture = source_fixture();
    let engine = Engine::start_with_producer(
        EngineConfig::default(),
        vec![PackageSpec {
            root: fixture.path().to_path_buf(),
            name: PACKAGE.to_owned(),
            version: "0.1.0".to_owned(),
            language: ProducerLanguage::Rust,
        }],
    );
    let tools = NudoxTools::new(engine);
    wait_until_loaded(&tools).await;

    let target = one_function(&tools, "target").await;
    let caller = one_function(&tools, "caller").await;
    let payload = one_symbol(&tools, "Payload", "Record").await;

    // 1. A semantic function call must become a normal usage posting through
    // ProducerSource, with no Rust-specific post-seal helper in the test.
    let target_usages = tools
        .do_find_usages(FindUsagesArgs {
            key: target.clone(),
            limit: Some(20),
            cursor: None,
        })
        .await
        .expect("find_usages(target) must succeed");
    assert!(
        target_usages.usages.iter().any(|usage| usage.key == caller),
        "the real `caller` function must be a precise usage of `target`; got {:?}",
        target_usages
            .usages
            .iter()
            .map(|usage| (&usage.name, &usage.path))
            .collect::<Vec<_>>()
    );

    // 2. Signature type references use the same postings as body calls.  Both
    // functions take and return Payload, so both must appear even if a frontend
    // has no body-occurrence implementation; a string literal must not.
    let payload_usages = tools
        .do_find_usages(FindUsagesArgs {
            key: payload.clone(),
            limit: Some(20),
            cursor: None,
        })
        .await
        .expect("find_usages(Payload) must succeed");
    let payload_users: Vec<&str> = payload_usages
        .usages
        .iter()
        .map(|usage| usage.name.as_str())
        .collect();
    assert!(
        payload_users.contains(&"target"),
        "Payload must include target's declaration type refs: {payload_users:?}"
    );
    assert!(
        payload_users.contains(&"caller"),
        "Payload must include caller's declaration type refs: {payload_users:?}"
    );
    assert!(
        !payload_users.contains(&"spelling_only"),
        "a string literal is not a resolved reference to Payload: {payload_users:?}"
    );

    // 3. The compact batch projection preserves request order, carries exact
    // bodies, and does not force the tool to serialise the GUI document model.
    let batch = tools
        .do_get_symbols(GetSymbolsArgs {
            keys: vec![caller.clone(), caller.clone(), target.clone()],
            format: SymbolFormat::Source,
        })
        .await
        .expect("batched compact lookup must succeed");
    assert_eq!(
        batch.symbols.len(),
        3,
        "one compact result per requested key"
    );
    assert_eq!(
        batch.symbols[0].key, caller,
        "batch must preserve caller first"
    );
    assert_eq!(
        batch.symbols[1].key, caller,
        "duplicate keys must preserve their input position"
    );
    assert_eq!(
        batch.symbols[0], batch.symbols[1],
        "duplicate keys must reuse one canonical compact projection"
    );
    assert_eq!(
        batch.symbols[2].key, target,
        "batch must preserve target third"
    );
    assert!(
        batch.symbols[0]
            .references
            .iter()
            .any(|reference| reference.target == payload),
        "compact formatting must retain Payload as a first-class resolved signature link: {:?}",
        batch.symbols[0].references
    );
    assert!(
        batch.symbols[0].source.as_deref().is_some_and(
            |source| source.contains("target(value)") && source.trim_end().ends_with('}')
        ),
        "caller compact source must include the exact multi-line function body: {:?}",
        batch.symbols[0].source
    );
    assert!(
        batch.symbols.iter().all(|symbol| symbol.signature.is_none()),
        "source mode must not duplicate signatures already present in exact source"
    );
    assert!(
        batch.symbols[2]
            .source
            .as_deref()
            .is_some_and(|source| source.contains("Payload(value.0 + 1)")
                && source.trim_end().ends_with('}')),
        "target compact source must include its body: {:?}",
        batch.symbols[2].source
    );

    let signatures = tools
        .do_get_symbols(GetSymbolsArgs {
            keys: vec![caller, target],
            format: SymbolFormat::Signature,
        })
        .await
        .expect("signature-only batch must succeed");
    assert!(
        signatures
            .symbols
            .iter()
            .all(|symbol| symbol.source.is_none()),
        "signature format must omit source rather than paying for bodies"
    );
    assert!(
        signatures
            .symbols
            .iter()
            .all(|symbol| symbol.signature.is_some()),
        "signature format must carry the compact rendered signature"
    );

    // 4. The public batch boundary rejects both degenerate and unbounded
    // requests before opening any symbol streams.
    for keys in [Vec::new(), vec![target_usages.usages[0].key.clone(); 33]] {
        let error = tools
            .do_get_symbols(GetSymbolsArgs {
                keys,
                format: SymbolFormat::Source,
            })
            .await
            .expect_err("invalid batch cardinality must be rejected");
        assert!(
            matches!(
                error,
                McpError::InvalidArgument {
                    argument: "keys",
                    ..
                }
            ),
            "batch cardinality must fail as an input error, got {error:?}"
        );
    }
}
