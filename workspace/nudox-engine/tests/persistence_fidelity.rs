//! Red-first specification: **restored IR must equal produced IR** (P5, part 2).
//!
//! # The hole this closes in `local_persistence.rs`
//!
//! Its sibling spec proves IR *survives* a restart — a package still resolves
//! after its sources are deleted. That is necessary and not sufficient: it
//! asserts only `symbol_count > 0`, so an implementation that persists a
//! degraded copy passes it while silently making the product worse.
//!
//! This is not hypothetical. The encoder that persistence goes through,
//! `ir_vcs::raise`, documents its own losses:
//!
//! > "`Inferred`, `QualifiedPath`, or `Unknown` has **no** `TypeWire`
//! > representation at all and raises to `TypeWire::Any` … **This can make two
//! > structurally different declarations (e.g. two `impl`s for different
//! > nominal self-types) raise to identical wire bytes**; the persisted local
//! > copy is a lossy compression of the produced IR, not a lossless mirror."
//!
//! and, under *"What is dropped outright (no wire slot, not even lossy)"*:
//! `Record.super_types`, `Field.key`/`attributes`, `Function.throws`,
//! `Alias.bounds`, `Symbol.doc_links`, and generic-parameter `variance`.
//!
//! Those are inheritance, exception specifications, field attributes, and doc
//! cross-references. A user restarts the app and their type information quietly
//! gets worse — a **user-visible regression disguised as an optimisation**, and
//! precisely the failure a green "it loaded!" test cannot see.
//!
//! The losses are pre-existing properties of `ir_vcs::wire`, not defects the
//! persistence work introduced. But they become *load-bearing* the moment the
//! wire format is used as a storage format, which is what P5 does.
//!
//! # The bar
//!
//! Reuse must be indistinguishable from recomputation. If the local store
//! cannot hold a symbol losslessly it must not claim to — better to reproduce
//! than to serve a quietly degraded copy.
//!
//! # Note on the likely fix
//!
//! `nudox_engine::store::remote::IrSnapshot` already round-trips **semantic**
//! `Entry` values losslessly (`Vec<(IntroId, Entry, Option<IntroId>)>`).
//! `docs/IR-STORAGE-PLAN.md` §5 argues the storage payload should be semantic
//! `Entry`, not the wire form — the wire form exists for libpijul's *patch*
//! layer, and using it as a *storage* layer is what imports its expressiveness
//! ceiling. Per-symbol byte-level diffing over semantic payloads still gives the
//! sharing P5 wants (unchanged symbol ⇒ unchanged bytes ⇒ no rewrite) without
//! the ceiling.
//!
//! **Do not weaken these tests to make them pass.** In particular, do not
//! narrow the fixture to types that happen to survive the round trip.

use std::path::{Path, PathBuf};

use nudox_engine::{Engine, EngineConfig, PackageLoadEvent, PackageSpec, ProducerLanguage};

// ---------------------------------------------------------------------------
// A fixture that deliberately exercises the documented lossy paths
// ---------------------------------------------------------------------------

/// A crate whose declarations hit the exact constructs `raise.rs` says it
/// cannot represent: two `impl`s for *different* nominal self-types (the
/// documented collision case), a trait with super-traits (`Record.super_types`),
/// a generic alias with bounds (`Alias.bounds`), and doc links
/// (`Symbol.doc_links`).
fn write_lossy_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fidelityfix\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .expect("write Cargo.toml");
    std::fs::write(
        root.join("src/lib.rs"),
        r#"
/// A base trait.
pub trait Base {
    /// Required method.
    fn base_method(&self) -> u32;
}

/// A trait with a super-trait — `Record.super_types`, dropped outright by raise.
pub trait Derived: Base {
    /// Another method, linking to [`Base`] — `Symbol.doc_links`, also dropped.
    fn derived_method(&self) -> u32;
}

/// First nominal self-type.
pub struct Alpha {
    pub field: u32,
}

/// Second nominal self-type.
pub struct Beta {
    pub field: u32,
}

// Two impls for DIFFERENT nominal self-types — the documented case that
// "raises to identical wire bytes".
impl Base for Alpha {
    fn base_method(&self) -> u32 { self.field }
}

impl Base for Beta {
    fn base_method(&self) -> u32 { self.field * 2 }
}

/// A generic alias with bounds — `Alias.bounds`, dropped outright.
pub type BoundedAlias<T: Base> = Option<T>;

/// A function whose return type is inferred-ish and generic.
pub fn generic_fn<T: Derived>(input: T) -> impl Base {
    Alpha { field: input.derived_method() }
}
"#,
    )
    .expect("write lib.rs");
}

fn spec(root: &Path) -> PackageSpec {
    PackageSpec {
        root: root.to_path_buf(),
        name: "fidelityfix".to_owned(),
        version: "0.1.0".to_owned(),
        language: ProducerLanguage::Rust,
    }
}

fn config_at(data_root: &Path) -> EngineConfig {
    EngineConfig {
        ir_repo_root: Some(data_root.join("ir")),
        package_cache: Some(data_root.join("cache")),
        ..EngineConfig::default()
    }
}

/// What one run of the engine observed about the package: the symbol count and
/// the root, both taken straight off the load event the GUI itself consumes.
#[derive(Debug, Clone, PartialEq)]
struct Observed {
    symbol_count: u64,
    has_root: bool,
}

fn run_once(config: EngineConfig, specs: Vec<PackageSpec>) -> Option<Observed> {
    let engine = Engine::start_with_producer(config, specs);
    let rx = engine.packages();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    let mut observed = None;
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(PackageLoadEvent::Loaded {
                symbol_count, root, ..
            }) => {
                observed = Some(Observed {
                    symbol_count,
                    has_root: root.is_some(),
                });
                break;
            }
            Ok(PackageLoadEvent::LoadFailed { error, .. }) => {
                panic!("package load failed: {error}")
            }
            Ok(_) => continue,
            Err(flume::RecvTimeoutError::Timeout) => continue,
            Err(flume::RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(engine);
    observed
}

fn scratch(case: &str) -> PathBuf {
    let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_owned());
    let dir = PathBuf::from(base).join(format!("nudox-fidelity-{case}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir scratch");
    dir
}

// ---------------------------------------------------------------------------
// The specification
// ---------------------------------------------------------------------------

/// THE test. A restored package must present exactly what a freshly-produced
/// one does. Anything less means the store is serving a degraded copy and the
/// user's type information silently got worse across a restart.
#[test]
fn a_restored_package_is_indistinguishable_from_a_produced_one() {
    let data_root = scratch("indistinguishable");
    let source_root = data_root.join("src-tree");
    write_lossy_fixture(&source_root);

    let produced = run_once(config_at(&data_root), vec![spec(&source_root)])
        .expect("the first run must produce the package");

    // Second run, same sources present — this isolates *storage* fidelity from
    // the separate question of whether the store is consulted at all.
    let restored = run_once(config_at(&data_root), vec![spec(&source_root)])
        .expect("the second run must resolve the package");

    assert_eq!(
        restored, produced,
        "a package restored from the local store must be indistinguishable from \
         a freshly-produced one. A lower symbol count means declarations were \
         dropped or collided on the way through the storage encoding — reuse \
         that silently degrades the product is worse than recomputing."
    );
}

/// The stronger form: with the sources gone, the restored package must *still*
/// match what production originally yielded. This is the case a user actually
/// hits — reopen the app, sources long since moved or cleaned.
#[test]
fn fidelity_holds_when_the_sources_are_gone() {
    let data_root = scratch("sources-gone");
    let source_root = data_root.join("src-tree");
    write_lossy_fixture(&source_root);

    let produced = run_once(config_at(&data_root), vec![spec(&source_root)])
        .expect("the first run must produce the package");

    std::fs::remove_dir_all(&source_root).expect("delete the source tree");

    let restored = run_once(config_at(&data_root), vec![spec(&source_root)])
        .expect("the restored package must resolve without sources");

    assert_eq!(
        restored, produced,
        "restoring without sources must not be lossier than restoring with them"
    );
}

/// Two `impl`s for different nominal self-types must remain two declarations.
/// `raise.rs` documents that they can "raise to identical wire bytes"; if a
/// content-addressed store then treats them as one, a declaration is lost
/// outright rather than merely flattened.
#[test]
fn distinct_impls_do_not_collide_in_the_store() {
    let data_root = scratch("impl-collision");
    let source_root = data_root.join("src-tree");
    write_lossy_fixture(&source_root);

    let produced = run_once(config_at(&data_root), vec![spec(&source_root)])
        .expect("first run produces");
    std::fs::remove_dir_all(&source_root).expect("delete the source tree");
    let restored = run_once(config_at(&data_root), vec![spec(&source_root)])
        .expect("second run restores");

    assert_eq!(
        restored.symbol_count, produced.symbol_count,
        "the fixture declares two impls for two different nominal self-types; a \
         restored count below the produced count means they collided into one \
         entry in the store"
    );
}
