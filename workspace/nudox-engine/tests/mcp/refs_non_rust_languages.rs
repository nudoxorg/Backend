//! `refs` must not answer "no callers" when it means "never looked".
//!
//! # The defect
//!
//! `record_occurrence` / `record_foreign_occurrence` are called by exactly one
//! producer in the workspace — the Rust one
//! (`workspace/compiler/languages/src/rust/mod.rs`). TypeScript, Go, Java, C#,
//! Python and C/C++ each lower declarations and stop. Nothing writes an
//! occurrence, so `PackageIndexes::usages_of` finds no posting list and
//! returns `&[]`.
//!
//! `refs` then reports that empty slice as a *result*: `usages: []`,
//! `truncated: false`. There is no error, no warning, and no field that
//! differs from the answer a genuinely-uncalled symbol produces. An agent
//! cannot distinguish the two, and the wrong reading is the confident one —
//! "this function has no callers, it is dead code" — about a function with
//! three callers in the same file.
//!
//! That is the shape doctrine §8 calls a degraded case presented as the good
//! one, and it is the most dangerous possible spelling of it: silence that
//! parses as data.
//!
//! # The two properties, deliberately separated
//!
//! * **Honesty** ([`an_unrecorded_reference_graph_says_so`]) — while a
//!   language's producer records nothing, `refs` must say the graph was never
//!   recorded rather than return a bare empty page. This is the property that
//!   must hold for *every* language, including any added later, and it is what
//!   makes the remaining gaps discoverable instead of silent.
//! * **Capability** ([`typescript_records_the_calls_in_its_own_source`]) — the
//!   TypeScript producer must actually record occurrences. OXC's
//!   `SemanticBuilder` already resolves every identifier to a symbol and its
//!   reference list, which is why this is a lowering gap rather than a missing
//!   analysis.
//!
//! Honesty is not made redundant by the capability fix: it is what keeps the
//! next language's gap from costing another silent-wrong-answer incident.
//!
//! # Hermetic
//!
//! The TypeScript producer parses in-process through OXC — no npm, no network
//! (`workspace/compiler/languages/tests/typescript/producer_tests.rs`'s own
//! note). A `package.json` with no `exports` map plus `src/index.ts` is a
//! complete package as far as entry discovery is concerned.

use std::fs;
use std::time::Duration;

use nudox_engine::mcp::tools::{RefsArgs, RefsDirection, RefsResult, SearchSymbolsArgs};
use nudox_engine::mcp::{NudoxTools, SymbolKeyDto};
use nudox_engine::{Engine, EngineConfig, PackageLoadEvent, PackageSpec, ProducerLanguage};

const PACKAGE: &str = "nudox-refs-fixture";

/// `spellingOnly` is adversarial input, exactly as the Rust pipeline test's
/// `spelling_only` is: a text search would call it a user of `runLoop`. A
/// resolved reference graph must not.
const INDEX_TS: &str = r#"
export interface Task {
    id: string;
}

export function runTool(task: Task): string {
    return task.id;
}

export function runLoop(task: Task): string {
    return runTool(task);
}

export function driver(task: Task): string {
    return runLoop(task);
}

export function spellingOnly(): string {
    return "runLoop";
}
"#;

fn typescript_fixture() -> tempfile::TempDir {
    let fixture = tempfile::tempdir().expect("create temporary TypeScript package");
    fs::create_dir_all(fixture.path().join("src")).expect("create fixture src/");
    // No `exports` map on purpose: that is the classic-resolution case, and it
    // keeps entry discovery on the `src/index.ts` fallback rather than on a
    // resolver path that would make this test about module resolution.
    fs::write(
        fixture.path().join("package.json"),
        format!("{{\n  \"name\": \"{PACKAGE}\",\n  \"version\": \"0.1.0\",\n  \"main\": \"src/index.ts\"\n}}\n"),
    )
    .expect("write fixture manifest");
    fs::write(fixture.path().join("src/index.ts"), INDEX_TS).expect("write fixture source");
    fixture
}

fn start(fixture: &tempfile::TempDir) -> NudoxTools {
    let engine = Engine::start_with_producer(
        EngineConfig::default(),
        vec![PackageSpec {
            root: fixture.path().to_path_buf(),
            name: PACKAGE.to_owned(),
            version: "0.1.0".to_owned(),
            language: ProducerLanguage::TypeScript,
        }],
    );
    NudoxTools::new(engine)
}

async fn wait_until_loaded(tools: &NudoxTools) {
    let events = tools.engine().packages();
    match tokio::time::timeout(Duration::from_mins(1), events.recv_async()).await {
        Ok(Ok(PackageLoadEvent::Loaded { .. })) => {}
        Ok(Ok(PackageLoadEvent::LoadFailed { error, .. })) => {
            panic!("the self-contained TypeScript package must load: {error}")
        }
        Ok(Ok(other)) => panic!("unexpected package event while loading fixture: {other:?}"),
        Ok(Err(error)) => panic!("package event channel closed before fixture loaded: {error}"),
        Err(elapsed) => panic!("engine did not load the TypeScript fixture within 60 seconds: {elapsed:?}"),
    }
}

/// The one function named `name`, as a key.
async fn one_function(tools: &NudoxTools, name: &str) -> SymbolKeyDto {
    let result = tools
        .do_search(SearchSymbolsArgs {
            query: name.to_owned(),
            kinds: Some(vec!["Function".to_owned()]),
            packages: Some(vec![format!("npm:{PACKAGE}")]),
            limit: Some(20),
            cursor: None,
        })
        .await
        .unwrap_or_else(|error| panic!("search for {name} must succeed: {error}"));

    let matches: Vec<_> = result
        .hits
        .iter()
        .filter(|hit| &*hit.hit.display_name == name)
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "the TypeScript producer must lower exactly one Function named {name}; got {:?}",
        result
            .hits
            .iter()
            .map(|hit| &*hit.hit.display_name)
            .collect::<Vec<_>>(),
    );
    SymbolKeyDto::from_wire(&matches[0].hit.key)
}

// ---------------------------------------------------------------------------
// Honesty
// ---------------------------------------------------------------------------

/// An empty `refs` page must carry *why* it is empty when the reason is that
/// nothing was ever recorded.
///
/// # The contract this pins
///
/// `RefsResult::In` gains a `coverage: Option<ReferenceCoverage>`, following
/// the shape `SearchResult::semantic` already uses for exactly this situation:
/// omitted in the ordinary case so it costs nothing, present when the answer
/// is not authoritative.
///
/// ```ignore
/// pub enum ReferenceCoverage {
///     /// This package's producer recorded a reference graph.
///     Recorded,
///     /// It did not, so an empty page means "not indexed", not "no callers".
///     NotRecorded { language: String },
/// }
/// ```
///
/// The assertion below is deliberately written against *observable* behaviour
/// rather than the variant name: whatever the fix is called, an agent holding
/// this response must be able to tell the two situations apart without
/// knowing which producer ran.
#[tokio::test]
async fn an_unrecorded_reference_graph_says_so() {
    let fixture = typescript_fixture();
    let tools = start(&fixture);
    wait_until_loaded(&tools).await;

    let run_loop = one_function(&tools, "runLoop").await;
    let result = tools
        .do_refs(RefsArgs {
            key: run_loop.clone(),
            direction: RefsDirection::In,
            limit: Some(50),
            cursor: None,
        })
        .await
        .expect("refs must not error on a resolvable key");

    let RefsResult::In {
        usages, coverage, ..
    } = &result
    else {
        panic!("direction: In must produce RefsResult::In");
    };

    // Either the graph is recorded and `runLoop`'s caller is in the page, or
    // it is not recorded and the response says so. What must never happen —
    // and is what happens today — is an empty page with nothing to
    // distinguish it from a genuinely uncalled symbol.
    if usages.is_empty() {
        let note = coverage.as_ref().unwrap_or_else(|| {
            panic!(
                "refs returned an empty page for a function with a caller in \
                 the same file, and nothing on the response says the \
                 TypeScript producer never recorded a reference graph. An \
                 agent reading this concludes `runLoop` is dead code."
            )
        });
        let rendered = format!("{note:?}").to_ascii_lowercase();
        assert!(
            rendered.contains("record"),
            "the coverage note must name the situation (nothing recorded), \
             so the reader knows to stop trusting the page: got {note:?}",
        );
    }
}

/// The same must hold for `direction: out`.
///
/// Occurrences are the *other* half of the same posting data, and a fix that
/// only annotated the `in` direction would leave `refs(direction: "out")`
/// reporting "this body references nothing" about a body full of calls.
#[tokio::test]
async fn the_out_direction_is_equally_honest() {
    let fixture = typescript_fixture();
    let tools = start(&fixture);
    wait_until_loaded(&tools).await;

    let run_loop = one_function(&tools, "runLoop").await;
    let result = tools
        .do_refs(RefsArgs {
            key: run_loop,
            direction: RefsDirection::Out,
            limit: Some(50),
            cursor: None,
        })
        .await
        .expect("refs must not error on a resolvable key");

    let RefsResult::Out {
        occurrences,
        coverage,
        ..
    } = &result
    else {
        panic!("direction: Out must produce RefsResult::Out");
    };

    if occurrences.is_empty() {
        assert!(
            coverage.is_some(),
            "`runLoop`'s body calls `runTool`; an empty occurrence page must \
             say the graph was never recorded rather than imply the body is \
             empty",
        );
    }
}

// ---------------------------------------------------------------------------
// Capability
// ---------------------------------------------------------------------------

/// The TypeScript producer must record the calls that are in its own source.
///
/// This is the property the user hit: `runLoop` has a caller in the same file,
/// and `refs` returned nothing. OXC's `SemanticBuilder` — already run by
/// `graph::build_and_extract` — resolves every identifier to a symbol and
/// holds its reference list, so this is a gap in lowering, not in analysis.
#[tokio::test]
async fn typescript_records_the_calls_in_its_own_source() {
    let fixture = typescript_fixture();
    let tools = start(&fixture);
    wait_until_loaded(&tools).await;

    let run_loop = one_function(&tools, "runLoop").await;
    let spelling_only = one_function(&tools, "spellingOnly").await;

    let result = tools
        .do_refs(RefsArgs {
            key: run_loop,
            direction: RefsDirection::In,
            limit: Some(50),
            cursor: None,
        })
        .await
        .expect("refs must not error on a resolvable key");

    let RefsResult::In { usages, .. } = &result else {
        panic!("direction: In must produce RefsResult::In");
    };

    let callers: Vec<&str> = usages.iter().map(|u| u.name.as_str()).collect();
    assert!(
        callers.contains(&"driver"),
        "`driver` calls `runLoop` in the same file and must appear as a \
         usage; got {callers:?}",
    );
    assert!(
        !usages.iter().any(|u| u.key == spelling_only),
        "`spellingOnly` only contains the *string* \"runLoop\"; a resolved \
         reference graph must not report it as a caller — that is the \
         difference between this and grep. got {callers:?}",
    );
}

/// The outbound half of the same fact: `runLoop`'s body references `runTool`.
#[tokio::test]
async fn typescript_records_what_a_body_calls() {
    let fixture = typescript_fixture();
    let tools = start(&fixture);
    wait_until_loaded(&tools).await;

    let run_loop = one_function(&tools, "runLoop").await;
    let run_tool = one_function(&tools, "runTool").await;

    let result = tools
        .do_refs(RefsArgs {
            key: run_loop,
            direction: RefsDirection::Out,
            limit: Some(50),
            cursor: None,
        })
        .await
        .expect("refs must not error on a resolvable key");

    let RefsResult::Out { occurrences, .. } = &result else {
        panic!("direction: Out must produce RefsResult::Out");
    };

    assert!(
        occurrences.iter().any(|o| o.target_key == run_tool.0),
        "`runLoop` calls `runTool`; its outbound occurrences must include it. \
         got targets {:?}",
        occurrences
            .iter()
            .map(|o| o.target_key.as_str())
            .collect::<Vec<_>>(),
    );
}
