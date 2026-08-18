//! A load whose build scripts did not run must be observably different from one
//! whose did — and, for the shape crates.io actually publishes, must not
//! happen.
//!
//! # Why this file exists
//!
//! `ProjectWorkspace::run_build_scripts` answers a `cargo check` that exited
//! non-zero with `Ok(scripts)`, parking the diagnostics in `scripts.error()` —
//! an `Option<&str>` nobody is obliged to read, the exact shape of the
//! `ProjectWorkspaceKind::Cargo::error` field that `dependency_resolution.rs`
//! exists for. Until [`BuildScriptExecution`] existed, the only trace was a
//! `warn!` into a process with no `tracing` subscriber.
//!
//! What that costs is worse than an incomplete table. When a
//! `cargo:rustc-cfg=foo` never arrives, `#[cfg(foo)]` items are pruned **and**
//! the `#[cfg(not(foo))]` fallback the author wrote for the other case is
//! lowered in their place, as though it were the package's public structure.
//! The output is not a smaller description of the crate; it is a description of
//! a different crate, and nothing about it looks wrong. `log 0.4.17` lost
//! `set_logger`/`set_boxed_logger` and gained a private `AtomicUsize` shim this
//! way. See docs/LIMITATIONS.md L50.
//!
//! # Why the fixtures are synthesized rather than real checkouts
//!
//! The corpus sweep (`corpus_sweep.rs`) covers the real packages and costs ~13
//! minutes (783.84 s measured 2026-08-07). These four cases isolate the same
//! mechanism in 62.10 s for the whole file, and —
//! for `a_phantom_test_target_does_not_stop_the_build_script` — reproduce the
//! *published tarball* shape exactly: a `[[test]]` entry pointing at a file
//! that `cargo package`'s `exclude` left out. That is the L50 trigger in full,
//! and nothing about it needs a real crate.

use std::path::{Path, PathBuf};

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, PackageLineageId, PackageName},
    entry::{Symbol, Visibility},
    lower::Lowering,
    package::PackageId,
};
use nudox_languages::{
    PackageSource, Producer, ProducerError,
    rust::{BuildScriptExecution, BuildScriptFailure, Error, LoadedWorkspace, RustProducer},
};

// ── Fixtures
// ──────────────────────────────────────────────────────────────────

/// The library every fixture here exposes.
///
/// The pair is the whole point. Asserting only that `gated_by_build_script`
/// appears would be satisfied by a producer that lowers everything
/// unconditionally; asserting that `FallbackShimNoReaderShouldEverSee` is
/// *absent* is what pins the difference between "the cfg was true" and "the cfg
/// was never evaluated". Both are needed, because L50's damage was precisely
/// that the second item was published as public API.
const FIXTURE_LIB_RS: &str = r#"
//! Fixture crate for the build-script cfg contract tests.

/// Present in every configuration.
pub fn always_present() -> u8 {
    1
}

/// Present only when the build script's `cargo:rustc-cfg` reached the graph.
#[cfg(nudox_probe)]
pub fn gated_by_build_script() -> u8 {
    2
}

/// The other side of the same gate: a private fallback that must never be
/// lowered as public structure.
#[cfg(not(nudox_probe))]
pub struct FallbackShimNoReaderShouldEverSee;
"#;

/// A build script that emits the cfg the library is gated on.
const BUILD_RS_OK: &str = r#"
fn main() {
    println!("cargo::rustc-check-cfg=cfg(nudox_probe)");
    println!("cargo:rustc-cfg=nudox_probe");
}
"#;

/// A build script that fails before emitting anything.
///
/// `exit(1)` rather than `panic!` so the failure is cargo's ("process didn't
/// exit successfully") and not a Rust backtrace, which is what a real broken
/// build script most often looks like.
const BUILD_RS_FAILING: &str = r#"
fn main() {
    eprintln!("nudox-fixture-build-script-refused-to-run");
    std::process::exit(1);
}
"#;

/// Write a standalone cargo package under `CARGO_TARGET_TMPDIR`.
///
/// The trailing empty `[workspace]` table is mandatory, not decoration: the
/// fixture lives under `target/`, which is inside this repository's cargo
/// workspace, and without it `cargo metadata` fails with "current package
/// believes it's in a workspace when it's not" — a *load* failure that would
/// masquerade as the build-script failure this file is trying to observe.
fn write_fixture(name: &str, build_rs: &str, extra_manifest: &str) -> PathBuf {
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
             build = \"build.rs\"\n\
             \n\
             [lib]\n\
             path = \"src/lib.rs\"\n\
             \n\
             {extra_manifest}\
             \n\
             [workspace]\n"
        ),
    )
    .expect("write fixture manifest");
    std::fs::write(root.join("build.rs"), build_rs).expect("write fixture build.rs");
    std::fs::write(root.join("src/lib.rs"), FIXTURE_LIB_RS).expect("write fixture lib.rs");
    root
}

// ── Harness
// ───────────────────────────────────────────────────────────────────

fn source_for(root: &Path, name: &str) -> PackageSource {
    PackageSource::new(root, name, "0.1.0")
}

fn load(src: &PackageSource) -> LoadedWorkspace {
    RustProducer { direct_repo: false }
        .invoke(src)
        .unwrap_or_else(|e| panic!("invoke must succeed even for a degraded workspace: {e}"))
}

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

/// A workspace whose build script runs reports `Ran` and lowers the item behind
/// the cfg it emitted — not the fallback on the other side of that cfg.
#[test]
fn a_working_build_script_reports_ran_and_lowers_its_cfg_gated_item() {
    let root = write_fixture("nudox_fixture_build_script_ok", BUILD_RS_OK, "");
    let src = source_for(&root, "nudox_fixture_build_script_ok");

    let (table, _cost) = heart::cost::measured("lower/fixture-build-script-ok", &root, || {
        let oracle = load(&src);
        assert!(
            matches!(oracle.build_script_execution(), BuildScriptExecution::Ran),
            "a package with a working build.rs must record that its build scripts ran; got {:?}",
            oracle.build_script_execution()
        );
        assert!(!oracle.completeness().is_degraded());
        lower(&oracle, &src).expect("a complete load must lower without an opt-in")
    });

    assert!(
        declares(&table, "always_present"),
        "the ungated fn must be lowered"
    );
    assert!(
        declares(&table, "gated_by_build_script"),
        "`cargo:rustc-cfg=nudox_probe` reached the crate graph, so the item behind it is real \
         public API and must be lowered"
    );
    assert!(
        !declares(&table, "FallbackShimNoReaderShouldEverSee"),
        "the `#[cfg(not(nudox_probe))]` fallback is not part of this crate on this target; \
         lowering it is L50's signature damage, not merely a missing item"
    );
}

/// The published-tarball shape — a `[[test]]` target whose file `cargo package`
/// excluded — does not stop the build script from running.
///
/// This is the L50 regression test. `ra_ap_project_model` 0.0.341 passes
/// `--all-targets` to the build-script `cargo check` unconditionally whenever
/// the toolchain supports `--compile-time-deps`, which makes cargo resolve
/// targets whose sources are not in the tarball and abort at target resolution
/// — before a single build script runs.
/// `ra::loaded::narrowed_build_script_config` removes the flag, which is why
/// this passes.
///
/// Verified by mutation: restoring the flag (passing the unmodified
/// `cargo_config` to `run_build_scripts`) turns this red with
/// `Error::BuildScriptsFailed`, and `corpus_sweep` red on
/// `log 0.4.17` and `nom 5.1.3`.
#[test]
fn a_phantom_test_target_does_not_stop_the_build_script() {
    let root = write_fixture(
        "nudox_fixture_phantom_target",
        BUILD_RS_OK,
        // No `tests/phantom.rs` is ever written. This is not a broken fixture:
        // it is byte-for-byte the situation `result/log-0.4.17` is in,
        // whose generated manifest declares `[[test]] name = "filters"` while
        // `exclude` kept `tests/` out of the published tarball.
        "[[test]]\nname = \"phantom\"\npath = \"tests/phantom.rs\"\n",
    );
    let src = source_for(&root, "nudox_fixture_phantom_target");

    let (table, _cost) = heart::cost::measured("lower/fixture-phantom-target", &root, || {
        let oracle = load(&src);
        assert!(
            matches!(oracle.build_script_execution(), BuildScriptExecution::Ran),
            "a target the tarball omits is not a build-script failure: nothing this engine \
                 documents lives in a `[[test]]` target, so the build-script `cargo check` must \
                 not be asked to resolve one. Got {:?}",
            oracle.build_script_execution()
        );
        lower(&oracle, &src).expect("a complete load must lower without an opt-in")
    });

    assert!(
        declares(&table, "gated_by_build_script"),
        "this is the L50 defect in one assertion: with `--all-targets` the build script never \
         ran, so this item was absent from the table and nothing said so"
    );
    assert!(
        !declares(&table, "FallbackShimNoReaderShouldEverSee"),
        "and this is the half that made L50 worse than a missing item: the fallback shim was \
         lowered as public structure in its place"
    );
}

/// A build script that fails is refused, with the cargo diagnostic reachable by
/// walking the `#[source]` chain.
#[test]
fn a_failed_build_script_is_refused_and_carries_the_cargo_diagnostic() {
    let root = write_fixture("nudox_fixture_build_script_failed", BUILD_RS_FAILING, "");
    let src = source_for(&root, "nudox_fixture_build_script_failed");

    let (err, _cost) = heart::cost::measured("lower/fixture-build-script-failed", &root, || {
        let oracle = load(&src);

        let failure = oracle
            .build_script_execution()
            .failure()
            .unwrap_or_else(|| {
                panic!(
                    "a build.rs that exits 1 must be recorded as a failure, not as `Ran`; \
                         got {:?}",
                    oracle.build_script_execution()
                )
            })
            .clone();
        let BuildScriptFailure::CargoRefusedTheWorkspace { .. } = failure else {
            panic!(
                "cargo ran and rejected the workspace, so the failure must be \
                     `CargoRefusedTheWorkspace` — `NotRun` means we could not start cargo at \
                     all and would send a reader to check their PATH. Got {failure:?}"
            );
        };
        assert!(
            failure
                .diagnostic()
                .is_some_and(|d| d.contains("nudox-fixture-build-script-refused-to-run")),
            "the diagnostic must carry what the build script itself printed, or the reader \
                 learns only that something failed; got {:?}",
            failure.diagnostic()
        );
        assert!(oracle.completeness().is_degraded());

        lower(&oracle, &src).expect_err("a failed build script must not lower by default")
    });

    // The refusal must arrive with its cause chain intact: a caller walking
    // `source` has to reach the cargo diagnostic, not a flattened message.
    let ProducerError::BuildScriptsFailed { ref source, .. } = err else {
        panic!("expected BuildScriptsFailed carrying the producer error, got {err:?}");
    };
    let rust_err = source
        .downcast_ref::<Error>()
        .expect("the source chain must still hold the typed Rust producer error");
    let Error::BuildScriptsFailed { package, cause } = rust_err else {
        panic!("expected BuildScriptsFailed, got {rust_err:?}");
    };
    assert_eq!(package, "nudox_fixture_build_script_failed");
    assert!(
        cause
            .diagnostic()
            .is_some_and(|d| d.contains("nudox-fixture-build-script-refused-to-run")),
        "the `#[source]` cause must still carry the build script's own output"
    );
}

/// Opting in to a load with no build-script cfgs yields a table that is not
/// merely incomplete but *counterfeit* — and that is exactly what the type
/// exists to make visible before anyone publishes it.
#[test]
fn accepting_a_failed_build_script_yields_a_table_describing_a_different_crate() {
    let root = write_fixture(
        "nudox_fixture_build_script_failed_accepted",
        BUILD_RS_FAILING,
        "",
    );
    let src = source_for(&root, "nudox_fixture_build_script_failed_accepted");

    let (table, _cost) =
        heart::cost::measured("lower/fixture-build-script-failed-accepted", &root, || {
            let oracle = load(&src).accept_missing_build_script_cfgs();
            lower(&oracle, &src).expect("an explicitly accepted degraded workspace must lower")
        });

    assert!(
        declares(&table, "always_present"),
        "an accepted degraded lowering is wrong, not empty"
    );
    assert!(
        !declares(&table, "gated_by_build_script"),
        "with no `cargo:rustc-cfg=nudox_probe` the gated item evaluates false and is pruned \
         before the walk sees it"
    );
    assert!(
        declares(&table, "FallbackShimNoReaderShouldEverSee"),
        "and the other side of the gate is lowered in its place. This assertion is the reason \
         `BuildScriptsFailed` is a refusal rather than a warning: the degraded table is not a \
         subset of the real one, it contains items the real one does not"
    );
}

/// L1 performance contract: repeated loads of an unchanged package reuse the
/// Cargo workspace model, while build scripts and HIR loading remain per-load.
/// This is deliberately structural — the samples are printed for diagnosis,
/// while the assertions pin the safe cache boundary without depending on speed.
#[test]
fn repeated_load_reuses_workspace_model_but_not_build_scripts_or_hir() {
    let first_root = write_fixture("nudox_fixture_l1_cached", BUILD_RS_OK, "");
    let first = source_for(&first_root, "nudox_fixture_l1_cached");
    let second = source_for(&first_root, "nudox_fixture_l1_cached");

    let first_oracle = load(&first);
    let second_oracle = load(&second);
    let first_profile = first_oracle.load_profile();
    let second_profile = second_oracle.load_profile();

    let expected = [
        "manifest_discover",
        "workspace_load",
        "build_scripts",
        "load_workspace",
    ];
    let phase_names = |profile: &nudox_languages::rust::LoadProfile| {
        profile.phases().map(|(name, _)| name).collect::<Vec<_>>()
    };
    assert_eq!(phase_names(first_profile), expected.to_vec());
    assert_eq!(
        phase_names(second_profile),
        ["manifest_discover", "build_scripts", "load_workspace"].to_vec()
    );

    // These are the fixed costs that would be tempting to hide behind an
    // unsafe cross-load workspace/build-script cache.  Keep their repetition
    // visible until a cache has explicit invalidation semantics.
    assert_eq!(first_profile.phase_count("workspace_load"), 1);
    assert_eq!(second_profile.phase_count("workspace_load"), 0);
    assert_eq!(first_profile.phase_count("build_scripts"), 1);
    assert_eq!(second_profile.phase_count("build_scripts"), 1);
    assert_eq!(first_profile.phase_count("load_workspace"), 1);
    assert_eq!(second_profile.phase_count("load_workspace"), 1);

    // Salsa caches are demand-driven by the documented walk; priming every
    // crate in each workspace would turn this repeated fixed cost into a
    // larger one without changing the package shape.
    assert_eq!(first_profile.cache_prefill_count(), 0);
    assert_eq!(second_profile.cache_prefill_count(), 0);
    assert_eq!(first_profile.workspace_cache_hit_count(), 0);
    assert_eq!(second_profile.workspace_cache_hit_count(), 1);

    eprintln!(
        "l1 load profiles: first={:?} second={:?}",
        first_profile.phases().collect::<Vec<_>>(),
        second_profile.phases().collect::<Vec<_>>()
    );
}
