//! Sweeps every provisioned `pypi` corpus package through the full
//! `nudox_producer::produce` pipeline (`invoke` -> `lower` -> `finish` ->
//! yield-contract gate -> `seal`) against real, third-party sdists under
//! `.real-crates/`.
//!
//! # What this file used to assert, and why that was the bug
//!
//! Until now the sweep asserted `table_len == 1` for all 22 entries and
//! reported them as "produced a table" — i.e. it pinned the defect as expected
//! behaviour and counted every one of them as a success. That is the register's
//! L45-cs finding ("26 of 154 corpus entries return Ok with a stub table and
//! are counted as successes") written down as a green test.
//!
//! `table_len == 1` is not a fact about Python packages. It is the count of the
//! root [`nudox_ir::entry::Symbol`] that `produce()` synthesizes *itself*,
//! before `lower` is ever called, for every producer in every language. A
//! producer that does nothing at all yields exactly 1. So the old assertion
//! could not distinguish "we analysed `sqlalchemy`'s 1205 files and found one
//! public symbol" from "we never opened the directory" — and the second is what
//! actually happens.
//!
//! # What it asserts now
//!
//!   1. Every one of the 22 provisioned pypi version entries is present and is
//!      a genuine unpacked sdist (a `setup.py` or `pyproject.toml` at its root
//!      — the actual PyPI source-distribution contract, not just a directory
//!      that happens to exist). Unchanged.
//!   2. Each runs through the real `produce()` pipeline, so the day the
//!      `pyrefly` feature is wired up this test starts measuring real content
//!      with no structural changes. Unchanged.
//!   3. **The emptiness is declared, not merely observed.** `PythonProducer`
//!      returns `YieldContract::RootOnly` under `cfg(not(feature =
//!      "pyrefly"))`, so `produce()` hands back a `Produced` whose `contract`
//!      field carries the degradation and the blocker behind it. The sweep
//!      asserts on *that*, per entry — the typed projection
//!      `Produced::contract.degraded()`, not a count.
//!   4. `table_len == 1` is still asserted, unchanged, as a *secondary*
//!      consistency check: a declared-degraded producer that contributed
//!      anything would already have failed the gate inside `produce()`, so this
//!      now pins agreement between the declaration and the output rather than
//!      standing in for it.
//!   5. The summary counts these as **degraded**, never as successes.
//!
//! And, separately, `the_declaration_is_what_keeps_python_out_of_the_error_path`
//! proves the gate is live rather than vacuous, by mutation (AGENTS-DOCTRINE.md
//! §8: "verify the guard by mutation … a guard nobody has watched fail is a
//! guard nobody has tested").
//!
//! Every entry is measured via `nudox_test_support::measured` per
//! AGENTS-DOCTRINE.md §4, even though the wall-clock numbers this produces are
//! structurally uninteresting (no I/O happens inside `invoke`) — the doctrine
//! requirement is unconditional, not conditioned on the measurement being
//! interesting.
//!
//! # Why `pyrefly` cannot simply be switched on
//!
//! Two independent blockers, both verified against this tree and both named in
//! the producer's own `DegradedYield` blocker string:
//!
//!   * `nudox-producer-python/src/context.rs` — declared by `src/lib.rs` as
//!     `#[cfg(feature = "pyrefly")] pub mod context;` and called by `invoke` —
//!     **is not in the tree**. With the feature on, the crate fails to compile
//!     at the module declaration.
//!   * Adding the git dependency exactly as `Cargo.toml`'s comment prescribes
//!     fails Cargo *dependency resolution*, before any compilation, on a hard
//!     disjoint `blake3` conflict: `pyrefly` pins `=1.8.2`, `workspace/index`'s
//!     `iroh` requires `^1.8.3`. `workspace/driver`'s `blake3 = "^1.8"`
//!     resolves to 1.8.5 and is the first collision Cargo reports; the
//!     `index`/`iroh` one surfaces once that is forced.

use std::path::{Path, PathBuf};

use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
use nudox_producer::{
    PackageSource, Producer, ProducerError, ProducerId, YieldContract, produce,
};
use nudox_producer_python::{PythonProducer, oracle::PythonOracle};

/// One corpus entry: `.real-crates/<dir>`, the PyPI project name, and the
/// ecosystem version string. Mirrors `corpus/manifest.toml`'s 20 `pypi`
/// `[[packages]]` blocks exactly: 20 packages, of which `click` and `pydantic`
/// each carry two versions for lineage testing, giving **22** version entries.
///
/// This count said 21 until now, in three places, while the array below has
/// always had 22 elements. The number is load-bearing — the sweep asserts every
/// entry is accounted for — so an off-by-one here reads as "one fixture is
/// missing" to the next person who checks.
struct Entry {
    dir: &'static str,
    name: &'static str,
    version: &'static str,
}

const ENTRIES: &[Entry] = &[
    Entry { dir: "requests-2.31.0", name: "requests", version: "2.31.0" },
    Entry { dir: "click-8.0.4", name: "click", version: "8.0.4" },
    Entry { dir: "click-8.1.7", name: "click", version: "8.1.7" },
    Entry { dir: "pydantic-1.10.14", name: "pydantic", version: "1.10.14" },
    Entry { dir: "pydantic-2.6.1", name: "pydantic", version: "2.6.1" },
    Entry { dir: "flask-3.0.2", name: "flask", version: "3.0.2" },
    Entry { dir: "attrs-23.2.0", name: "attrs", version: "23.2.0" },
    Entry { dir: "sqlalchemy-2.0.27", name: "sqlalchemy", version: "2.0.27" },
    Entry { dir: "pyyaml-6.0.1", name: "pyyaml", version: "6.0.1" },
    Entry { dir: "python-dateutil-2.8.2", name: "python-dateutil", version: "2.8.2" },
    Entry { dir: "six-1.16.0", name: "six", version: "1.16.0" },
    Entry { dir: "rich-13.7.0", name: "rich", version: "13.7.0" },
    Entry { dir: "typer-0.9.0", name: "typer", version: "0.9.0" },
    Entry { dir: "httpx-0.27.0", name: "httpx", version: "0.27.0" },
    Entry { dir: "black-24.2.0", name: "black", version: "24.2.0" },
    Entry { dir: "more-itertools-10.2.0", name: "more-itertools", version: "10.2.0" },
    Entry { dir: "tenacity-8.2.3", name: "tenacity", version: "8.2.3" },
    Entry { dir: "dataclasses-json-0.6.4", name: "dataclasses-json", version: "0.6.4" },
    Entry { dir: "structlog-24.1.0", name: "structlog", version: "24.1.0" },
    Entry { dir: "jsonschema-4.21.1", name: "jsonschema", version: "4.21.1" },
    Entry { dir: "cattrs-23.2.3", name: "cattrs", version: "23.2.3" },
    Entry { dir: "beautifulsoup4-4.12.3", name: "beautifulsoup4", version: "4.12.3" },
];

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../.real-crates")
        .canonicalize()
        .expect("no .real-crates/ checkout — see corpus/README.md to (re)provision it")
}

/// Walk the full `std::error::Error` source chain. The top-level `Display`
/// on `ProducerError` is deliberately terse (AGENTS-DOCTRINE.md's "hard-won
/// facts": reading only it turns a five-second diagnosis into an hour).
fn chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = e.to_string();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = e.source();
    while let Some(src) = cur {
        out.push_str(" <- ");
        out.push_str(&src.to_string());
        cur = src.source();
    }
    out
}

enum Outcome {
    /// `produce()` returned a table the producer stands behind: it declared
    /// [`YieldContract::Declarations`] and contributed at least one entry of
    /// its own. **No pypi entry reaches this today**, and the sweep says so out
    /// loud rather than folding it together with the case below — the two were
    /// the same `Ok` before this change, which is the whole finding.
    Produced { table_len: usize, unlinked: usize },
    /// `produce()` returned a table whose producer declared up front that it
    /// contributes nothing. Carries the declared blocker so the summary can
    /// print *why*, per entry, instead of a bare count.
    Degraded { table_len: usize, blocker: String },
    /// Fixture is missing or is not a genuine sdist (no `setup.py` /
    /// `pyproject.toml` at its root).
    Preflight { reason: String },
    Fail { stage: &'static str, chain: String },
}

fn run_entry(entry: &Entry) -> Outcome {
    let root = corpus_root().join(entry.dir);
    let has_setup_py = root.join("setup.py").is_file();
    let has_pyproject = root.join("pyproject.toml").is_file();
    if !has_setup_py && !has_pyproject {
        return Outcome::Preflight {
            reason: format!(
                "no setup.py or pyproject.toml at {} — not a genuine sdist",
                root.display()
            ),
        };
    }

    let src = PackageSource::new(&root, entry.name, entry.version);
    let lid = PackageLineageId::new(EcosystemId::new("pypi"), PackageName::new(entry.name));

    let case = format!("pypi-sweep-{}", entry.dir);
    let (produced, _cost) = nudox_test_support::measured(&case, &root, || {
        produce(&PythonProducer, &src, &lid, &nudox_ir::foreign::Unlinked)
    });

    match produced {
        // The classification is *derived from* the value that crossed the seam
        // (`Produced::contract`), never from what this test knows about the
        // build it is compiled into. If someone wires pyrefly up, these entries
        // move to `Produced` on their own.
        Ok(p) => match p.contract.degraded() {
            Some(degraded) => Outcome::Degraded {
                table_len: p.table.len(),
                blocker: degraded.blocker().to_owned(),
            },
            None => Outcome::Produced {
                table_len: p.table.len(),
                unlinked: p.report.unlinked.len(),
            },
        },
        Err(e) => Outcome::Fail {
            stage: "produce (invoke/lower/finish/yield-contract/seal)",
            chain: chain(&e),
        },
    }
}

#[test]
fn every_provisioned_pypi_corpus_package_declares_its_degradation_rather_than_yielding_a_silent_stub()
 {
    let mut failures = Vec::new();
    let mut preflight_failures = Vec::new();
    let mut produced = Vec::new();
    let mut degraded = Vec::new();

    for entry in ENTRIES {
        eprintln!("=== {} ({} @ {}) ===", entry.dir, entry.name, entry.version);
        match run_entry(entry) {
            Outcome::Produced { table_len, unlinked } => {
                eprintln!(
                    "PRODUCED {}: table_len={table_len} unlinked_refs={unlinked}",
                    entry.dir
                );
                produced.push((entry.dir, table_len));
            }
            Outcome::Degraded { table_len, blocker } => {
                eprintln!("DEGRADED {}: table_len={table_len} blocker={blocker}", entry.dir);
                // The blocker must stay specific enough to be checkable. Both
                // facts below are the *current* reasons pyrefly cannot be
                // enabled, and both are stronger than "the feature is off" — a
                // reader told only that would reach for `--features pyrefly`,
                // which fails at module resolution and then at dependency
                // resolution. There is no typed variant to assert instead (the
                // set of blockers is open, deliberately — see `DegradedYield`),
                // so this is the exception AGENTS-DOCTRINE.md §4's
                // "never assert on a message string" rule leaves: the typed
                // half is already asserted by `contract.degraded()` being
                // `Some` in `run_entry`, and this pins that the reason has not
                // decayed into a tautology.
                assert!(
                    blocker.contains("context.rs"),
                    "{}: the declared blocker must name the missing module, since enabling the \
                     feature fails to compile there; got {blocker:?}",
                    entry.dir
                );
                assert!(
                    blocker.contains("blake3"),
                    "{}: the declared blocker must name the dependency-resolution conflict, \
                     since it blocks the fix before compilation; got {blocker:?}",
                    entry.dir
                );
                // Retained verbatim from the pre-inversion version of this
                // test, and deliberately not weakened. Its meaning has changed:
                // it is no longer the *primary* assertion standing in for real
                // analysis, but a consistency check that the declaration and
                // the output agree. `produce()`'s gate would already have
                // failed a declared-degraded producer that contributed
                // anything (`ProducerError::YieldContractOutgrown`), so a
                // `table_len != 1` reaching here would mean the root symbol
                // itself changed shape.
                assert_eq!(
                    table_len, 1,
                    "{}: a degraded producer's table must hold exactly the root entry \
                     `produce()` synthesized, got table_len={table_len}",
                    entry.dir
                );
                degraded.push((entry.dir, blocker));
            }
            Outcome::Preflight { reason } => {
                eprintln!("PREFLIGHT-FAIL {}: {reason}", entry.dir);
                preflight_failures.push((entry.dir, reason));
            }
            Outcome::Fail { stage, chain } => {
                eprintln!("FAIL {} at [{stage}]: {chain}", entry.dir);
                failures.push((entry.dir, stage, chain));
            }
        }
    }

    eprintln!(
        "\n=== pypi corpus sweep summary: {} produced / {} DEGRADED / {} of {} entries ===",
        produced.len(),
        degraded.len(),
        produced.len() + degraded.len(),
        ENTRIES.len()
    );
    eprintln!(
        "NOTE: a DEGRADED entry is NOT a success. Its producer declared up front that it \
         contributes nothing, and `Produced::contract` carries that declaration downstream. \
         These entries must not be counted toward any corpus coverage number."
    );
    for (dir, table_len) in &produced {
        eprintln!("  PRODUCED {dir}: table_len={table_len}");
    }
    for (dir, blocker) in &degraded {
        eprintln!("  DEGRADED {dir}: {blocker}");
    }
    for (dir, reason) in &preflight_failures {
        eprintln!("  PREFLIGHT-FAIL {dir}: {reason}");
    }
    for (dir, stage, chain) in &failures {
        eprintln!("  FAIL {dir} [{stage}]: {chain}");
    }

    assert!(
        preflight_failures.is_empty(),
        "{} of {} pypi corpus entries are not genuine sdists on disk",
        preflight_failures.len(),
        ENTRIES.len()
    );
    assert!(
        failures.is_empty(),
        "{} of {} pypi corpus entries failed to lower through produce(); see stderr above \
         for the full error chain of each",
        failures.len(),
        ENTRIES.len()
    );
    // The inversion, stated as an invariant. While `pyrefly` is off, *every*
    // entry must land in the degraded bucket — none may quietly appear in the
    // produced bucket, which is where they all used to be counted. The day the
    // oracle works this assertion is what forces someone to come back here and
    // retire it, rather than letting a real result be filed under "degraded".
    #[cfg(not(feature = "pyrefly"))]
    {
        assert!(
            produced.is_empty(),
            "{} pypi entries were reported as genuinely produced while the pyrefly oracle is \
             absent: {produced:?}. Either the oracle now works — in which case retire this \
             assertion and `PythonProducer::yield_contract` together — or something is \
             fabricating declarations",
            produced.len()
        );
        assert_eq!(
            degraded.len(),
            ENTRIES.len(),
            "every provisioned pypi entry must declare its degradation; {} did not",
            ENTRIES.len() - degraded.len()
        );
    }
}

/// The gate is live, not vacuous: strip Python's declaration and `produce()`
/// rejects the very same run the sweep above accepts.
///
/// AGENTS-DOCTRINE.md §8 requires verifying a guard by mutation — "break the
/// repair verdict deliberately and confirm the suite goes red. A guard nobody
/// has watched fail is a guard nobody has tested." The sweep above can only
/// show that the *declared* path returns `Ok`; on its own that is equally
/// consistent with a gate that never fires. This produces the mutation as a
/// real type rather than as a comment: `UndeclaredPython` delegates `invoke`
/// and `lower` to the real `PythonProducer` and differs from it in exactly one
/// respect — it does not override `Producer::yield_contract`, so it takes the
/// trait default. That is precisely the state `PythonProducer` was in before
/// this change, and it is the state every one of the 22 corpus entries was
/// counted as a success from.
struct UndeclaredPython;

impl Producer for UndeclaredPython {
    type Id = <PythonProducer as Producer>::Id;
    type Oracle = PythonOracle;

    const ID: ProducerId = ProducerId("python-undeclared-mutant/1");
    const LANGUAGE: nudox_ir::body::Language = <PythonProducer as Producer>::LANGUAGE;

    fn invoke(&self, src: &PackageSource) -> Result<Self::Oracle, ProducerError> {
        PythonProducer.invoke(src)
    }

    // No `yield_contract` override — that is the mutation.

    fn lower(
        &self,
        oracle: &Self::Oracle,
        out: &mut nudox_ir::lower::Lowering<Self::Id>,
    ) -> Result<(), ProducerError> {
        PythonProducer.lower(oracle, out)
    }
}

#[test]
#[cfg(not(feature = "pyrefly"))]
fn the_declaration_is_what_keeps_python_out_of_the_error_path() {
    let entry = &ENTRIES[0];
    let root = corpus_root().join(entry.dir);
    let src = PackageSource::new(&root, entry.name, entry.version);
    let lid = PackageLineageId::new(EcosystemId::new("pypi"), PackageName::new(entry.name));

    // Same package, same `invoke`, same `lower`, same pipeline as the sweep.
    let case = format!("pypi-undeclared-mutant-{}", entry.dir);
    let (result, _cost) = nudox_test_support::measured(&case, &root, || {
        produce(&UndeclaredPython, &src, &lid, &nudox_ir::foreign::Unlinked)
    });

    match result {
        Err(ProducerError::NoDeclarationsContributed {
            package,
            producer,
            source,
        }) => {
            eprintln!(
                "mutant rejected as expected: package={package} producer={producer} \
                 source={source:?}"
            );
            assert_eq!(package, entry.name);
            assert_eq!(producer, UndeclaredPython::ID);
        }
        Err(other) => panic!(
            "expected ProducerError::NoDeclarationsContributed for a producer that contributes \
             nothing and declares nothing, got {other:?}"
        ),
        Ok(p) => panic!(
            "the yield-contract gate did not fire: a producer that reads no source and \
             declares no degradation sealed a table of {} entries and was reported as a \
             success — this is exactly the L45-cs defect, unrepaired",
            p.table.len()
        ),
    }

    // And the real producer, on the identical input, is accepted *because* it
    // declares. The pair is the point: acceptance is earned by the
    // declaration, not by the gate being asleep.
    let honest = produce(&PythonProducer, &src, &lid, &nudox_ir::foreign::Unlinked)
        .expect("the declared-degraded producer is accepted");
    assert!(
        matches!(honest.contract, YieldContract::RootOnly(_)),
        "and it is accepted as degraded, not as a success"
    );
}
