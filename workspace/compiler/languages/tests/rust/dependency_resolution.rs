//! A `--no-deps` load must be observably different from a full one.
//!
//! # Why this file exists
//!
//! `ra_ap_project_model` answers a failed `cargo metadata` by silently retrying
//! with `--no-deps` and returning *that* as `Ok`, recording the real failure in
//! `ProjectWorkspaceKind::Cargo::error` — an `Option` field nobody is obliged to
//! read. `--no-deps` metadata has no `resolve` section, so every dependency is
//! gone from the crate graph and every Cargo feature, `default` included,
//! evaluates false.
//!
//! Until [`DependencyResolution`] existed, the only trace of that was a stderr
//! line, and the resulting table was structurally indistinguishable from a
//! complete one — which is how a whole corpus of measurements was taken in the
//! degraded mode without anyone noticing.
//!
//! These tests assert the difference is now visible in three ways that a caller
//! cannot skip past: in the type on the loaded workspace, in a refusal to lower,
//! and — once the caller opts in — in the *content* of the table, which really
//! is missing its feature-gated items.

use std::path::{Path, PathBuf};

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, PackageLineageId, PackageName},
    entry::{Symbol, Visibility},
    lower::Lowering,
    package::PackageId,
};
use nudox_languages::rust::{Error, LoadedWorkspace, RustProducer};
use nudox_languages::{PackageSource, Producer, ProducerError};

// ── Fixtures ──────────────────────────────────────────────────────────────────

/// The library both fixtures expose.
///
/// `only_with_extra_feature` is the probe: the producer loads with
/// `CargoFeatures::All`, so a *resolved* workspace activates `extra` and lowers
/// it, while a `--no-deps` workspace has no active features at all and prunes it
/// before the walk ever sees it. Asserting on this symbol — rather than on a
/// count — is what makes the two loads distinguishable by content.
const FIXTURE_LIB_RS: &str = r#"
//! Fixture crate for the dependency-resolution contract tests.

/// Present in every configuration.
pub fn always_present() -> u8 {
    1
}

/// Present only when the `extra` feature is active.
#[cfg(feature = "extra")]
pub fn only_with_extra_feature() -> u8 {
    2
}
"#;

/// A package name that cannot exist on crates.io, so `cargo metadata` must fail
/// under `--offline` while `cargo metadata --no-deps` still succeeds.
///
/// That asymmetry is the exact condition that triggers upstream's fallback, so
/// it is the condition this file has to reproduce.
const UNRESOLVABLE_DEP: &str = "nudox-no-such-crate-exists-anywhere-9e3f1a";

/// Write a standalone cargo package under `CARGO_TARGET_TMPDIR`.
///
/// The trailing empty `[workspace]` table is mandatory, not decoration: the
/// fixture lives under `target/`, which is inside this repository's cargo
/// workspace, and without it `cargo metadata` fails with "current package
/// believes it's in a workspace when it's not" — a *load* failure that would
/// masquerade as the resolution failure this file is trying to observe.
fn write_fixture(name: &str, dependencies: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("create fixture src dir");

    std::fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\n\
             name = \"{name}\"\n\
             version = \"0.1.0\"\n\
             edition = \"2021\"\n\
             \n\
             [lib]\n\
             path = \"src/lib.rs\"\n\
             \n\
             [features]\n\
             default = []\n\
             extra = []\n\
             \n\
             {dependencies}\
             \n\
             [workspace]\n"
        ),
    )
    .expect("write fixture manifest");
    std::fs::write(root.join("src/lib.rs"), FIXTURE_LIB_RS).expect("write fixture lib.rs");
    root
}

// ── Harness ───────────────────────────────────────────────────────────────────

fn source_for(root: &Path, name: &str) -> PackageSource {
    PackageSource::new(root, name, "0.1.0")
}

fn load(src: &PackageSource) -> LoadedWorkspace {
    RustProducer { direct_repo: false }
        .invoke(src)
        .unwrap_or_else(|e| panic!("invoke must succeed even for a degraded workspace: {e}"))
}

/// Lower `oracle` into a sealed table.
fn lower(
    oracle: &LoadedWorkspace,
    src: &PackageSource,
) -> Result<PristineIntroTable, ProducerError> {
    let root_sym = Symbol {
        name: src.name.as_str().to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: src.root.clone(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    let mut sink: Lowering<_> = Lowering::new(PackageId::path(src.root()), root_sym);
    RustProducer { direct_repo: false }.lower(oracle, &mut sink)?;
    let package = sink
        .finish()
        .unwrap_or_else(|e| panic!("fixture lowering must be structurally sound: {e}"));
    let lineage = PackageLineageId::new(
        EcosystemId::new("cargo"),
        PackageName::new(src.name.as_str().to_owned()),
    );
    Ok(package.seal(&lineage, &nudox_ir::foreign::Unlinked).table)
}

fn declares(table: &PristineIntroTable, name: &str) -> bool {
    table.iter().any(|(_, entry)| entry.sym().name == name)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// A package whose dependencies resolve reports `Full` and lowers its
/// feature-gated items.
#[test]
fn a_resolved_workspace_reports_full_resolution_and_lowers_feature_gated_items() {
    let root = write_fixture("nudox_fixture_resolved", "");
    let src = source_for(&root, "nudox_fixture_resolved");

    let (table, _cost) = heart::cost::measured("lower/fixture-resolved", &root, || {
        let oracle = load(&src);
        if let Some(fallback) = oracle.dependency_resolution().no_deps() {
            panic!(
                "a dependency-free package must resolve fully; cargo said: {}",
                fallback.cargo_diagnostic()
            );
        }
        assert!(!oracle.dependency_resolution().is_degraded());
        lower(&oracle, &src).expect("a fully resolved workspace must lower without an opt-in")
    });

    assert!(
        declares(&table, "always_present"),
        "the ungated function must be lowered"
    );
    assert!(
        declares(&table, "only_with_extra_feature"),
        "`CargoFeatures::All` must activate `extra`, so its item must be lowered"
    );
}

/// A `--no-deps` load is reported as `NoDeps`, carries the cargo failure, and is
/// refused by `lower` rather than silently producing a partial table.
#[test]
fn a_no_deps_workspace_is_refused_and_carries_the_cargo_failure() {
    let root = write_fixture(
        "nudox_fixture_degraded",
        &format!("[dependencies]\n{UNRESOLVABLE_DEP} = \"9.9.9\"\n"),
    );
    let src = source_for(&root, "nudox_fixture_degraded");

    let (err, _cost) = heart::cost::measured("lower/fixture-degraded", &root, || {
        let oracle = load(&src);

        let diagnostic = oracle
            .dependency_resolution()
            .no_deps()
            .unwrap_or_else(|| {
                panic!(
                    "a package depending on `{UNRESOLVABLE_DEP}` cannot resolve offline, so \
                     the load must be reported as NoDeps"
                )
            })
            .cargo_diagnostic()
            .to_owned();
        assert!(
            diagnostic.contains(UNRESOLVABLE_DEP),
            "the fallback must name the package that failed to resolve so a reader can act \
             on it; got: {diagnostic}"
        );
        assert!(oracle.dependency_resolution().is_degraded());

        lower(&oracle, &src).expect_err("a degraded workspace must not lower by default")
    });

    // The refusal must arrive with its cause chain intact: a caller walking
    // `source` has to reach the cargo diagnostic, not a flattened message.
    let ProducerError::DependenciesUnresolved { ref source, .. } = err else {
        panic!("expected DependenciesUnresolved carrying the producer error, got {err:?}");
    };
    let rust_err = source
        .downcast_ref::<Error>()
        .expect("the source chain must still hold the typed Rust producer error");
    let Error::DependenciesUnresolved { package, cause } = rust_err else {
        panic!("expected DependenciesUnresolved, got {rust_err:?}");
    };
    assert_eq!(package, "nudox_fixture_degraded");
    assert!(
        cause.cargo_diagnostic().contains(UNRESOLVABLE_DEP),
        "the `#[source]` cause must still name the unresolvable package"
    );
}

/// Opting in to a degraded graph yields a table that is measurably incomplete:
/// the ungated item survives, the feature-gated one is gone.
#[test]
fn accepting_a_degraded_graph_yields_a_table_missing_its_feature_gated_items() {
    let root = write_fixture(
        "nudox_fixture_degraded_accepted",
        &format!("[dependencies]\n{UNRESOLVABLE_DEP} = \"9.9.9\"\n"),
    );
    let src = source_for(&root, "nudox_fixture_degraded_accepted");

    let (table, _cost) = heart::cost::measured("lower/fixture-degraded-accepted", &root, || {
        let oracle = load(&src).accept_degraded_dependencies();
        lower(&oracle, &src).expect("an explicitly accepted degraded workspace must lower")
    });

    assert!(
        declares(&table, "always_present"),
        "an accepted degraded lowering is partial, not empty"
    );
    assert!(
        !declares(&table, "only_with_extra_feature"),
        "this is the cost the type exists to make visible: with no `resolve` section every \
         Cargo feature evaluates false, so the `cfg(feature = \"extra\")` item is pruned \
         before the walk sees it"
    );
}
