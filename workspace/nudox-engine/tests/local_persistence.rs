//! Red-first specification: **IR must survive a process restart** (P5).
//!
//! # The error class this closes
//!
//! `nudox_engine`'s corpus is an in-memory `BTreeMap` (`store/corpus.rs`) and
//! `PackageView` has no `Serialize`. Nothing about produced IR is written
//! anywhere. Every launch therefore re-runs the producer over every package —
//! the engine's own doc comment says so (`runtime/mod.rs:1046`):
//!
//! > "**Cost**: Linear. Loading four versions runs the producer four times and
//! > holds four `PackageView`s. There is no incremental or shared-storage path
//! > between generations."
//!
//! Meanwhile `DeploymentProfile::embedded()` (`heart/deployment.rs`) already
//! provisions `ir_repo_root` — *"Root of the local libpijul `IrRepository`"* —
//! and `DeploymentKind::Embedded` is documented as *"one process, one compiler,
//! local catalog clone, **offline branches**"*. The store designed to hold
//! exactly this, at exactly this path, is never opened. This is not a missing
//! feature; it is an unconnected wire.
//!
//! # Why these tests delete the source tree
//!
//! The obvious test — "restart and check the package is there" — passes
//! vacuously if the engine simply re-runs the producer. Counting producer
//! invocations would work but needs an injection hook the public API does not
//! offer.
//!
//! Deleting the sources between runs is the honest, hook-free proof: after the
//! source is gone there is nothing left to produce *from*, so a package that
//! still resolves can only have come from the local store. A wall-clock
//! assertion would prove nothing (a small fixture produces fast) and would be
//! flaky under load.
//!
//! **Do not weaken these tests to make them pass.** In particular, do not
//! restore the source tree before the second run.

use std::path::{Path, PathBuf};

use nudox_engine::{Engine, EngineConfig, PackageLoadEvent, PackageSpec, ProducerLanguage};

// ---------------------------------------------------------------------------
// Fixture: a tiny real package on disk
// ---------------------------------------------------------------------------

/// Write a minimal, self-contained Rust crate with three declarations whose
/// names are distinctive enough to assert on.
fn write_fixture_crate(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"persistfix\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .expect("write Cargo.toml");
    std::fs::write(
        root.join("src/lib.rs"),
        r#"
/// A marker function the tests assert on by name.
pub fn persist_marker_alpha() -> u32 { 1 }

/// A second marker, so "some symbols" is distinguishable from "all symbols".
pub fn persist_marker_beta(input: u32) -> u32 { input + 1 }

/// A type, so the assertion is not purely about functions.
pub struct PersistMarkerGamma {
    pub field: u32,
}
"#,
    )
    .expect("write lib.rs");
}

fn spec(root: &Path) -> PackageSpec {
    PackageSpec {
        root: root.to_path_buf(),
        name: "persistfix".to_owned(),
        version: "0.1.0".to_owned(),
        language: ProducerLanguage::Rust,
    }
}

/// An engine config pinned to `data_root` for both its IR store and its package
/// cache, so a test's state is entirely contained in one scratch directory.
fn config_at(data_root: &Path) -> EngineConfig {
    EngineConfig {
        ir_repo_root: Some(data_root.join("ir")),
        package_cache: Some(data_root.join("cache")),
        ..EngineConfig::default()
    }
}

/// Start an engine over `specs`, drain its package stream to completion, and
/// return the names of every package that reached a loaded state.
fn start_and_collect(config: EngineConfig, specs: Vec<PackageSpec>) -> Vec<String> {
    let engine = Engine::start_with_producer(config, specs);
    let rx = engine.packages();
    let mut loaded = Vec::new();
    // Bounded: a hung load must fail the test, not hang the suite.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(PackageLoadEvent::Loaded { package, .. }) => {
                loaded.push(format!("{package:?}"));
                break;
            }
            Ok(PackageLoadEvent::LoadFailed { error, .. }) => {
                panic!("package load failed: {error}");
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    drop(engine);
    loaded
}

fn scratch(case: &str) -> PathBuf {
    let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_owned());
    let dir = PathBuf::from(base).join(format!("nudox-persist-{case}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir scratch");
    dir
}

// ---------------------------------------------------------------------------
// The specification
// ---------------------------------------------------------------------------

/// THE test. Produce once, delete the sources, restart — the package must still
/// resolve, which is only possible if its IR was persisted.
#[test]
fn ir_survives_a_restart_after_the_sources_are_deleted() {
    let data_root = scratch("survives");
    let source_root = data_root.join("src-tree");
    write_fixture_crate(&source_root);

    let first = start_and_collect(config_at(&data_root), vec![spec(&source_root)]);
    assert_eq!(
        first.len(),
        1,
        "the first run must actually produce the package"
    );

    // Nothing left to produce from.
    std::fs::remove_dir_all(&source_root).expect("delete the source tree");
    assert!(!source_root.exists(), "sources are gone");

    // The second run is given the SAME spec, pointing at a path that no longer
    // exists. If IR was persisted the engine serves it from the local store; if
    // it was not, there is nothing it can do.
    let second = start_and_collect(config_at(&data_root), vec![spec(&source_root)]);
    assert_eq!(
        second.len(),
        1,
        "IR did not survive the restart — the corpus is still in-memory only, \
         so every launch recompiles the world"
    );
}

/// A restart with **no specs at all** must still repopulate the corpus. The GUI
/// does not re-declare every package it has ever seen at startup; it expects the
/// store to know.
#[test]
fn a_restart_repopulates_the_corpus_without_being_told_what_to_load() {
    let data_root = scratch("repopulate");
    let source_root = data_root.join("src-tree");
    write_fixture_crate(&source_root);

    let first = start_and_collect(config_at(&data_root), vec![spec(&source_root)]);
    assert_eq!(first.len(), 1, "first run produces");

    std::fs::remove_dir_all(&source_root).expect("delete the source tree");

    let second = start_and_collect(config_at(&data_root), Vec::new());
    assert_eq!(
        second.len(),
        1,
        "a restart with no specs must still surface the persisted package"
    );
}

/// Two engines pointed at *different* data roots must not see each other's
/// packages. A store keyed on a path that is ignored would pass the tests above
/// by accident.
#[test]
fn separate_data_roots_are_isolated() {
    let root_a = scratch("iso-a");
    let root_b = scratch("iso-b");
    let source_root = root_a.join("src-tree");
    write_fixture_crate(&source_root);

    let first = start_and_collect(config_at(&root_a), vec![spec(&source_root)]);
    assert_eq!(first.len(), 1, "engine A produces");

    std::fs::remove_dir_all(&source_root).expect("delete the source tree");

    let other = start_and_collect(config_at(&root_b), Vec::new());
    assert!(
        other.is_empty(),
        "a different data root must not see engine A's packages, got {other:?}"
    );
}

/// Producing the *same* package twice must not accumulate duplicate generations
/// unboundedly — re-indexing unchanged sources should be recognised as
/// unchanged, not appended forever.
#[test]
fn reindexing_unchanged_sources_does_not_grow_the_store_without_bound() {
    let data_root = scratch("nogrowth");
    let source_root = data_root.join("src-tree");
    write_fixture_crate(&source_root);
    let ir_root = data_root.join("ir");

    start_and_collect(config_at(&data_root), vec![spec(&source_root)]);
    let after_first = dir_size(&ir_root);
    assert!(after_first > 0, "the IR store must actually hold something");

    for _ in 0..3 {
        start_and_collect(config_at(&data_root), vec![spec(&source_root)]);
    }
    let after_repeats = dir_size(&ir_root);

    assert!(
        after_repeats < after_first * 3,
        "re-indexing identical sources three more times grew the store from {after_first} \
         to {after_repeats} bytes — unchanged input must not be stored again"
    );
}

fn dir_size(path: &Path) -> u64 {
    fn walk(path: &Path, total: &mut u64) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                walk(&entry.path(), total);
            } else {
                *total += meta.len();
            }
        }
    }
    let mut total = 0;
    walk(path, &mut total);
    total
}
