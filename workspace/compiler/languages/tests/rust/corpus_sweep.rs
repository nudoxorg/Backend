//! Sweeps every provisioned crates.io corpus package through the full
//! `nudox_languages::produce` pipeline (`invoke` -> `lower` -> `finish` ->
//! `seal`) against real, third-party crate checkouts under `result/`.
//!
//! # Why this exists
//!
//! Until this file, `nudox-languages` had no real-package coverage at all.
//! `dependency_resolution.rs` synthesizes two-function fixtures into
//! `CARGO_TARGET_TMPDIR`; `module_census.rs` is the only test that touches a
//! real crate, and it was `#[ignore]`d behind an environment variable that, when
//! unset, made it `return` — so under `--run-ignored all` it reported PASS in
//! 0.014s having asserted nothing. Rust was also the only one of the seven
//! languages in this workspace with no `result/` sweep: go, java, c#,
//! python, typescript and clang all have one.
//!
//! docs/AGENTS-DOCTRINE.md §4 is what that costs: "a hand-authored fixture tests the
//! fixture author's imagination, not the code," and the Go producer's 47 green
//! fixture tests crashed on the first real package it was ever shown. A
//! fixture-only suite's "all our tests pass" is an unmeasured claim.
//!
//! # Shape
//!
//! Follows `go/tests/corpus_sweep.rs` and
//! `typescript/tests/real_npm_packages.rs`: one `#[test]` looping the whole
//! ecosystem list, each entry `heart::cost::measured` individually so
//! every package emits its own `cost case=` line into `perf-report.nu`, no
//! abort on first failure, and a summary assertion at the end carrying every
//! failure's full `std::error::Error` source chain. The corpus table itself
//! lives in `tests/common/mod.rs`, shared with `corpus_manifest.rs`.
//!
//! # Why the whole corpus, at ~1 minute per entry
//!
//! Booting in-process rust-analyzer costs 20-60 s per package and is the
//! dominant term regardless of crate size (`.config/nextest.toml`'s
//! `real-crate-serial` group: "a 34-entry crate and a 14,710-entry crate cost
//! within the same order of magnitude"), so 23 entries is ~20 minutes and
//! narrowing the list is the only real lever. It is not taken, for the reason
//! the go sweep gives: the point of a first sweep is to find every defect in
//! one pass. Sampling would also silently choose *which* defects to find —
//! `serde` and `memchr 2.8.0` are precisely the two checkouts with no vendored
//! sources, `libc` is the one whose API is almost entirely `cfg`-gated, `syn`
//! is the one whose types come out of a declarative macro, and `clap` is the
//! one that declares almost nothing of its own. Every one of those is a
//! different way for the producer to be wrong, and a five-package sample that
//! happened to miss them would report a clean sweep.
//!
//! # Running
//!
//! ```text
//! cargo test -p nudox-languages --test rust_corpus_sweep -- --ignored --nocapture
//! ```

mod common;

use std::time::Duration;

use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
use nudox_ir::foreign::Unlinked;
use nudox_languages::{PackageSource, ProducerError, produce};
use nudox_languages::rust::{RustProducer, Error, error::NothingToDocument};

use common::{ENTRIES, Entry, chain, corpus_root, entry_root};

// ── Per-entry outcome ────────────────────────────────────────────────────────

enum Outcome {
    /// The package lowered and every symbol the corpus table names was found.
    Lowered {
        entries: usize,
        unlinked: usize,
        wall: Duration,
    },
    /// The package lowered, but symbols its own source declares are absent.
    /// Kept apart from `Failed` because it is a lowering defect, not a load
    /// one, and it is the only outcome a count-based test would have missed.
    MissingSymbols {
        entries: usize,
        wall: Duration,
        missing: Vec<String>,
    },
    /// `cargo metadata` could not resolve the dependency graph, so
    /// rust-analyzer loaded `--no-deps` metadata and the producer refused to
    /// walk it. Recorded separately because it is an environment failure with
    /// a known fix (vendor the sources), not a producer defect — and because
    /// the whole reason `DependencyResolution` exists is that this used to be
    /// indistinguishable from a complete lowering.
    Unresolved {
        package: String,
        cargo_diagnostic: String,
        wall: Duration,
    },
    /// Anything else, with its whole cause chain.
    Failed { stage: &'static str, chain: String },
}

// ── One entry ────────────────────────────────────────────────────────────────

fn run_entry(entry: &Entry) -> Outcome {
    let root = entry_root(entry);
    if !root.join("Cargo.toml").is_file() {
        // A missing checkout is a hard failure, not a skip. `corpus_manifest.rs`
        // is the fast test whose job is to report an unprovisioned corpus; by
        // the time anyone has paid for a rust-analyzer boot, "the package was
        // not there" has to be as loud as "the package did not lower".
        return Outcome::Failed {
            stage: "preflight",
            chain: format!(
                "no Cargo.toml at {} — run `nix build .#checks.corpus`",
                root.display()
            ),
        };
    }

    let src = PackageSource::new(&root, entry.name, entry.version);
    let lineage = PackageLineageId::new(
        EcosystemId::new("cargo"),
        PackageName::new(entry.name.to_owned()),
    );

    let case = format!("lower/cargo/{}-{}", entry.name, entry.version);
    let (produced, cost) = heart::cost::measured(&case, &root, || {
        produce(&RustProducer { direct_repo: false }, &src, &lineage, &Unlinked)
    });

    let produced = match produced {
        Ok(p) => p,
        // The typed refusal `DependencyResolution` exists to make impossible to
        // ignore. Unwrapped through both layers — the language-agnostic
        // `ProducerError` and the `Error` it keeps in its `#[source]`
        // slot — because only the inner one carries the `cargo metadata` failure
        // that says *which* dependency did not resolve.
        Err(ProducerError::DependenciesUnresolved { package, source }) => {
            let Some(Error::DependenciesUnresolved { cause, .. }) =
                source.downcast_ref::<Error>()
            else {
                return Outcome::Failed {
                    stage: "dependency resolution (cause chain broken)",
                    chain: format!(
                        "DependenciesUnresolved for `{package}` did not carry a typed \
                         Error in its source slot: {source}"
                    ),
                };
            };
            return Outcome::Unresolved {
                package,
                cargo_diagnostic: cause.cargo_diagnostic().to_owned(),
                wall: cost.wall,
            };
        }
        Err(err) => {
            return Outcome::Failed {
                stage: "produce (invoke/lower/finish/seal)",
                chain: chain(&err),
            };
        }
    };

    // ── Content, not counts (doctrine §4) ────────────────────────────────────
    //
    // Every `Want` names a symbol read out of *this* checkout's source, paired
    // with the IR kind its declaration must produce. Matching both is what
    // makes the assertion survive the two cheap ways to fake it: a table full
    // of correctly-named entries that are all opaque modules, and a table whose
    // `memchr` is the module rather than the function.
    let missing: Vec<String> = entry
        .expect
        .iter()
        .filter(|want| {
            !produced.table.iter().any(|(_, e)| {
                e.sym().name == want.name && e.kind().discriminant() == Some(want.kind)
            })
        })
        .map(|want| format!("{} as {:?}", want.name, want.kind))
        .collect();

    if !missing.is_empty() {
        return Outcome::MissingSymbols {
            entries: produced.table.len(),
            wall: cost.wall,
            missing,
        };
    }

    Outcome::Lowered {
        entries: produced.table.len(),
        unlinked: produced.report.unlinked.len(),
        wall: cost.wall,
    }
}

// ── The sweep ────────────────────────────────────────────────────────────────

/// Every crates.io corpus package lowers, and its lowering contains the symbols
/// its own source declares.
///
/// One test rather than twenty-three so a single run reports the whole sweep as
/// one line-per-package log; each package is still `measured` on its own, so
/// the per-package `cost case=` row survives for `perf-report.nu`.
///
/// Deliberately does not abort on the first failure. A first sweep exists to
/// enumerate defects, not to stop at one, and every outcome — including the
/// environmental `DependenciesUnresolved` ones — is printed with the full cause
/// chain before the summary assertion fires.
#[test]
#[ignore = "drives in-process rust-analyzer over 23 real cargo workspaces (~20 min, ~600 MB RSS per package)"]
fn every_crates_io_corpus_package_lowers_to_the_public_api_its_source_declares() {
    let mut lowered = Vec::new();
    let mut unresolved = Vec::new();
    let mut failures = Vec::new();

    for entry in ENTRIES {
        eprintln!("=== {} ({} {}) ===", entry.dir, entry.name, entry.version);
        match run_entry(entry) {
            Outcome::Lowered {
                entries,
                unlinked,
                wall,
            } => {
                eprintln!(
                    "OK   {}: {entries} entries, {unlinked} unlinked refs, {:.1}s, all {} \
                     expected symbols present",
                    entry.dir,
                    wall.as_secs_f64(),
                    entry.expect.len()
                );
                lowered.push((entry.dir, entries, wall));
            }
            Outcome::MissingSymbols {
                entries,
                wall,
                missing,
            } => {
                eprintln!(
                    "FAIL {}: lowered {entries} entries in {:.1}s but {} declared symbol(s) \
                     are absent: {}",
                    entry.dir,
                    wall.as_secs_f64(),
                    missing.len(),
                    missing.join(", ")
                );
                failures.push((
                    entry.dir,
                    "content",
                    format!(
                        "lowered {entries} entries but these symbols, declared in the \
                         checkout's own source, are absent from the table: {}",
                        missing.join(", ")
                    ),
                ));
            }
            Outcome::Unresolved {
                package,
                cargo_diagnostic,
                wall,
            } => {
                eprintln!(
                    "DEGRADED {}: `{package}` loaded from --no-deps metadata after {:.1}s; \
                     cargo said: {cargo_diagnostic}",
                    entry.dir,
                    wall.as_secs_f64()
                );
                unresolved.push((entry.dir, cargo_diagnostic));
            }
            Outcome::Failed { stage, chain } => {
                eprintln!("FAIL {} at [{stage}]:\n      {chain}", entry.dir);
                failures.push((entry.dir, stage, chain));
            }
        }
    }

    // ── Summary ──────────────────────────────────────────────────────────────
    eprintln!(
        "\n=== crates.io corpus sweep: {} lowered, {} unresolved, {} failed, of {} entries ===",
        lowered.len(),
        unresolved.len(),
        failures.len(),
        ENTRIES.len()
    );
    for (dir, entries, wall) in &lowered {
        eprintln!("  OK        {dir}: {entries} entries in {:.1}s", wall.as_secs_f64());
    }
    for (dir, diagnostic) in &unresolved {
        eprintln!("  DEGRADED  {dir}: {diagnostic}");
    }
    for (dir, stage, chain) in &failures {
        eprintln!("  FAIL      {dir} [{stage}]: {chain}");
    }

    assert!(
        !lowered.is_empty(),
        "not one of the {} crates.io corpus entries under {} lowered — a sweep that measured \
         nothing must not read as a pass",
        ENTRIES.len(),
        corpus_root().display()
    );
    assert!(
        failures.is_empty(),
        "{} of {} crates.io corpus entries failed; see stderr above for each one's full cause \
         chain",
        failures.len(),
        ENTRIES.len()
    );
    assert!(
        unresolved.is_empty(),
        "{} of {} crates.io corpus entries could not resolve their dependency graph offline \
         and were refused rather than lowered degraded. This is an environment defect with a \
         known fix — `cargo vendor` into the checkout plus a `.cargo/config.toml` \
         source-replacement, as `result/memchr-2.8.3` already has — but it is asserted \
         rather than tolerated: a sweep that quietly skips the packages it cannot resolve is \
         reporting on a corpus it chose after seeing the answers.\n  {}",
        unresolved.len(),
        ENTRIES.len(),
        unresolved
            .iter()
            .map(|(dir, why)| format!("{dir}: {why}"))
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

// ── Adversarial ──────────────────────────────────────────────────────────────

/// Asking a real workspace for a package it does not contain is refused, not
/// answered with somebody else's table.
///
/// The load succeeds — `result/memchr-2.8.3` is a perfectly good cargo
/// workspace — and only `lower` can notice that no `hir::Crate` in it answers
/// to the requested name. The failure mode being excluded is the one that
/// `documented_package_names` sets up: when the name is not in the
/// `CargoWorkspace` it logs a `warn!` (silent here — nothing installs a
/// `tracing` subscriber) and returns the requested name anyway, so the refusal
/// has to come from `find_local_crates` finding nothing. A producer that
/// instead fell through to "walk whatever local crates exist" would return
/// memchr's 1 800-odd entries under a package name that does not exist, and the
/// store would cache them under it.
///
/// The assertions walk the whole typed chain rather than reading a message —
/// `ProducerError::NoDeclarationsContributed` -> `Error::NothingToDocument`
/// -> `NothingToDocument::NoCrateForPackages { packages }` — and check that the
/// innermost variant still names the package that was asked for. Each hop is a
/// separate `#[source]` slot, so a future change that flattens any of them into
/// a string (the fate `producer_error_for`'s catch-all `UnsupportedConstruct`
/// arm still imposes on `Load`, `Cancelled` and `LoweringBug`) fails here
/// instead of quietly degrading every caller's diagnostics.
#[test]
#[ignore = "drives in-process rust-analyzer over a real cargo workspace"]
fn a_package_absent_from_the_loaded_workspace_is_refused_rather_than_answered_with_another() {
    let root = corpus_root().join("memchr-2.8.3");
    assert!(
        root.join("Cargo.toml").is_file(),
        "this adversarial case needs the memchr-2.8.3 checkout; run `nix build .#checks.corpus`"
    );

    let absent = "nudox-no-such-crate-in-this-workspace-9e3f1a";
    let src = PackageSource::new(&root, absent, "0.0.0");
    let lineage = PackageLineageId::new(
        EcosystemId::new("cargo"),
        PackageName::new(absent.to_owned()),
    );

    let (result, _cost) =
        heart::cost::measured("lower/cargo/absent-package", &root, || {
            produce(&RustProducer { direct_repo: false }, &src, &lineage, &Unlinked)
        });

    let err = match result {
        Ok(produced) => panic!(
            "lowering a package that is not in the workspace must fail; it produced {} \
             entries instead — which can only be memchr's own API filed under `{absent}`",
            produced.table.len()
        ),
        Err(err) => err,
    };

    let ProducerError::NoDeclarationsContributed { source: Some(source), .. } = &err else {
        panic!(
            "expected NoDeclarationsContributed carrying a cause; got {err:?}"
        );
    };
    let rust_err = source
        .downcast_ref::<Error>()
        .expect("the source chain must still hold the typed Rust producer error");
    let Error::NothingToDocument { package, cause } = rust_err else {
        panic!("expected NothingToDocument, got {rust_err:?}");
    };
    assert_eq!(package, absent);
    let NothingToDocument::NoCrateForPackages { packages } = cause else {
        panic!(
            "the documented set is non-empty here — `documented_package_names` falls back to \
             the requested name when it matches no cargo package — so the refusal must come \
             from the crate graph, not the manifest; got {cause:?}"
        );
    };
    assert!(
        packages.iter().any(|p| p == absent),
        "the innermost cause must still name the package that was asked for; got {packages:?}"
    );
    eprintln!("absent-package refusal chain:\n  {}", chain(&err));
}
