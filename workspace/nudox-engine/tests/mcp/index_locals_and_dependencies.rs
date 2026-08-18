//! `index`: more than one target, local roots, and opt-in dependencies.
//!
//! # What the surface could do before this file
//!
//! `index_package` took exactly one argument that mattered — a `purl` — and a
//! PURL is by construction a *registry* coordinate. That left three things an
//! agent working in a checkout could not do at all:
//!
//! 1. **Index the package it is looking at.** A local root reaches the corpus
//!    only through `Engine::start_with_producer`, which runs once, at process
//!    start, from `NUDOX_PACKAGE_ROOT`. Pointing lindsey at a second checkout
//!    meant editing the environment and restarting the app — and the MCP
//!    client's config with it, since the port moves on restart.
//! 2. **Index several packages in one call.** The corpus has always been a
//!    `BTreeMap<PackageLineageId, _>` with no arity limit
//!    (`store/corpus.rs`); only the tool was singular.
//! 3. **Follow a dependency edge.** Nothing in the engine reads a manifest's
//!    dependency list — `grep -rn 'dependenc' workspace/nudox-engine/src/
//!    packages/` finds only prose. So "open the type this function returns"
//!    dead-ends the moment the type is declared in another package, which in a
//!    monorepo is most of them.
//!
//! # Why dependencies are opt-in and not depth-first-by-default
//!
//! Indexing is seconds-to-minutes per package and a transitive closure is
//! unbounded — a mid-sized npm package can reach several hundred. A tool that
//! did that on every call would be a denial of service against its own corpus,
//! so `dependencies` defaults to off and `depth` defaults to one hop. The
//! caller who wants the closure asks for it and chooses how far.
//!
//! # Why partial success is a first-class result
//!
//! Five targets where the third is misspelled must not discard the two that
//! landed: the expensive work is already paid for, and an agent that gets back
//! one error for the batch cannot tell which target it was about. So the
//! result carries `indexed`, `running` and `failed` side by side rather than
//! being a `Result` over the whole batch.

use std::fs;

use nudox_engine::mcp::tools::{IndexArgs, PackagesArgs};
use nudox_engine::mcp::NudoxTools;
use nudox_engine::{Engine, EngineConfig};

// ---------------------------------------------------------------------------
// Fixtures
//
// Rust for the dependency test: a cargo `path` dependency is an unambiguous,
// hermetic edge — no registry, no network, no lockfile resolution — so the
// test is about dependency *following* rather than about version solving.
// ---------------------------------------------------------------------------

const LIB_RS: &str = r#"
#[derive(Clone, Debug)]
pub struct Widget(pub u32);

pub fn make_widget() -> Widget {
    Widget(1)
}
"#;

const APP_RS: &str = r#"
pub use nudox_index_fixture_lib::Widget;

pub fn build() -> Widget {
    nudox_index_fixture_lib::make_widget()
}
"#;

const APP: &str = "nudox-index-fixture-app";
const LIB: &str = "nudox_index_fixture_lib";

/// A two-crate workspace: `app` declares a `path` dependency on `lib`.
///
/// Returned whole so the `TempDir` outlives every producer run; dropping it
/// mid-lowering deletes the sources the producer is reading.
fn cargo_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create temporary cargo workspace");

    let lib = dir.path().join("lib");
    fs::create_dir_all(lib.join("src")).expect("create lib/src");
    fs::write(
        lib.join("Cargo.toml"),
        format!("[package]\nname = \"{LIB}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n"),
    )
    .expect("write lib manifest");
    fs::write(lib.join("src/lib.rs"), LIB_RS).expect("write lib source");

    let app = dir.path().join("app");
    fs::create_dir_all(app.join("src")).expect("create app/src");
    fs::write(
        app.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{APP}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\n{LIB} = {{ path = \"../lib\" }}\n\n[workspace]\n"
        ),
    )
    .expect("write app manifest");
    fs::write(app.join("src/lib.rs"), APP_RS).expect("write app source");

    dir
}

/// An engine with an empty corpus: every package these tests see arrived
/// through `index`, which is the property under test.
fn empty_engine() -> NudoxTools {
    NudoxTools::new(Engine::start_with_producer(EngineConfig::default(), vec![]))
}

/// The `ecosystem:name` lineages currently resident.
async fn resident(tools: &NudoxTools) -> Vec<String> {
    let loaded = tools
        .do_packages(PackagesArgs { package: None })
        .await
        .expect("listing packages must succeed");
    let mut names: Vec<String> = loaded
        .packages
        .iter()
        .map(|p| p.lineage.clone())
        .collect();
    names.sort();
    names
}

/// Long enough for two real rust-analyzer runs on a two-file crate.
const WAIT: Option<u32> = Some(180);

// ---------------------------------------------------------------------------
// Local roots
// ---------------------------------------------------------------------------

/// A filesystem path is a valid target.
///
/// This is the capability an agent in a checkout needs most and had least:
/// before it, the only way a local package reached the corpus was the
/// process-start environment variable.
#[tokio::test]
async fn a_local_path_is_an_indexable_target() {
    let workspace = cargo_workspace();
    let tools = empty_engine();

    let result = tools
        .do_index(IndexArgs {
            targets: vec![workspace.path().join("lib").display().to_string()],
            dependencies: false,
            depth: None,
            wait_seconds: WAIT,
        })
        .await
        .expect("indexing a local path must not be an error");

    assert!(
        result.failed.is_empty(),
        "indexing a well-formed local package must not fail: {:?}",
        result.failed,
    );
    assert_eq!(
        result.indexed.len(),
        1,
        "one target in, one package out; got indexed={:?} running={:?}",
        result.indexed,
        result.running,
    );
    assert!(
        resident(&tools).await.iter().any(|p| p.contains(LIB)),
        "the indexed package must be visible to every other tool",
    );
}

/// Several targets in one call.
#[tokio::test]
async fn several_targets_are_indexed_in_one_call() {
    let workspace = cargo_workspace();
    let tools = empty_engine();

    let result = tools
        .do_index(IndexArgs {
            targets: vec![
                workspace.path().join("lib").display().to_string(),
                workspace.path().join("app").display().to_string(),
            ],
            dependencies: false,
            depth: None,
            wait_seconds: WAIT,
        })
        .await
        .expect("indexing two local paths must not be an error");

    assert!(
        result.failed.is_empty(),
        "neither target is malformed: {:?}",
        result.failed,
    );
    assert_eq!(
        result.indexed.len(),
        2,
        "both targets must land; got indexed={:?} running={:?}",
        result.indexed,
        result.running,
    );

    let names = resident(&tools).await;
    assert!(
        names.iter().any(|p| p.contains(LIB)) && names.iter().any(|p| p.contains(APP)),
        "both packages must be resident together: {names:?}",
    );
}

/// One bad target must not discard the good ones.
///
/// The whole point of reporting `indexed` and `failed` side by side: a batch
/// that returned a single `Err` would throw away work that had already
/// completed, and would not say which target was at fault.
#[tokio::test]
async fn a_bad_target_does_not_discard_the_good_ones() {
    let workspace = cargo_workspace();
    let tools = empty_engine();

    let result = tools
        .do_index(IndexArgs {
            targets: vec![
                workspace.path().join("lib").display().to_string(),
                "/nonexistent/path/that/is/not/a/package".to_owned(),
            ],
            dependencies: false,
            depth: None,
            wait_seconds: WAIT,
        })
        .await
        .expect("a partly-bad batch is a result, not an error");

    assert_eq!(
        result.indexed.len(),
        1,
        "the good target must still have landed: {:?}",
        result.indexed,
    );
    assert_eq!(
        result.failed.len(),
        1,
        "the bad target must be reported, individually: {:?}",
        result.failed,
    );
    assert!(
        result.failed[0].target.contains("nonexistent"),
        "the failure must name WHICH target failed, or the agent cannot act \
         on it: {:?}",
        result.failed[0],
    );
}

// ---------------------------------------------------------------------------
// Dependencies
// ---------------------------------------------------------------------------

/// Off by default: indexing `app` alone must not drag `lib` in.
///
/// Asserted explicitly because "opt-in" is only meaningful if the default is
/// observable. A default that quietly indexed the closure would turn a
/// one-package request into an unbounded one.
#[tokio::test]
async fn dependencies_are_not_indexed_unless_asked_for() {
    let workspace = cargo_workspace();
    let tools = empty_engine();

    tools
        .do_index(IndexArgs {
            targets: vec![workspace.path().join("app").display().to_string()],
            dependencies: false,
            depth: None,
            wait_seconds: WAIT,
        })
        .await
        .expect("indexing the app must not be an error");

    let names = resident(&tools).await;
    assert!(
        names.iter().any(|p| p.contains(APP)),
        "the named target must be resident: {names:?}",
    );
    assert!(
        !names.iter().any(|p| p.contains(LIB)),
        "`dependencies: false` must index exactly what was named: {names:?}",
    );
}

/// Opted in, the declared dependency is indexed too.
#[tokio::test]
async fn opting_in_indexes_the_declared_dependencies() {
    let workspace = cargo_workspace();
    let tools = empty_engine();

    let result = tools
        .do_index(IndexArgs {
            targets: vec![workspace.path().join("app").display().to_string()],
            dependencies: true,
            depth: None,
            wait_seconds: WAIT,
        })
        .await
        .expect("indexing with dependencies must not be an error");

    assert!(
        result.failed.is_empty(),
        "a cargo `path` dependency resolves on the filesystem with no \
         registry involved; nothing here should fail: {:?}",
        result.failed,
    );

    let names = resident(&tools).await;
    assert!(
        names.iter().any(|p| p.contains(APP)),
        "the named target must still be resident: {names:?}",
    );
    assert!(
        names.iter().any(|p| p.contains(LIB)),
        "`app`'s manifest declares `{LIB}` as a path dependency; opting in \
         must index it: {names:?}",
    );
}

/// The dependency rows must be distinguishable from the requested ones.
///
/// An agent that asked for one package and got three back needs to know which
/// one it named — otherwise it cannot tell a dependency it happened to
/// acquire from the package it is actually working on.
#[tokio::test]
async fn dependency_rows_say_they_were_not_requested_directly() {
    let workspace = cargo_workspace();
    let tools = empty_engine();

    let result = tools
        .do_index(IndexArgs {
            targets: vec![workspace.path().join("app").display().to_string()],
            dependencies: true,
            depth: None,
            wait_seconds: WAIT,
        })
        .await
        .expect("indexing with dependencies must not be an error");

    let requested: Vec<_> = result.indexed.iter().filter(|p| p.requested).collect();
    let pulled_in: Vec<_> = result.indexed.iter().filter(|p| !p.requested).collect();

    assert_eq!(
        requested.len(),
        1,
        "exactly one package was named by the caller: {:?}",
        result.indexed,
    );
    assert!(
        requested[0].package.contains(APP),
        "the requested row must be the target the caller named: {:?}",
        requested[0],
    );
    assert!(
        pulled_in.iter().any(|p| p.package.contains(LIB)),
        "the dependency must be present and marked as not directly \
         requested: {:?}",
        result.indexed,
    );
}
