//! Lower every real npm-ecosystem fixture in `corpus/manifest.toml` end to
//! end through the actual `Producer` trait (entry discovery -> OXC extraction
//! -> `Lowering` -> seal), and assert on real content.
//!
//! # Why this exists
//!
//! Every other test in this crate (`producer_tests.rs`) parses a literal
//! TypeScript string written by hand. AGENTS-DOCTRINE.md §4 is explicit about
//! what that measures: the fixture author's imagination, not the producer's
//! behaviour on a package nobody here wrote. The Go producer's 47
//! fixture-only tests were 100% green and 0% usable on the first real
//! package it saw. This file is the counterpart: it drives the same
//! `nudox_producer::produce` entry point the store/engine use in production,
//! over the 20 real npm tarballs `corpus/fetch.nu` materializes into
//! `.real-crates/`, and asserts on entries that must be present in each
//! package's real public API — never just `is_ok()`.
//!
//! # Fixture list
//!
//! Mirrors the `ecosystem = "npm"` section of `corpus/manifest.toml` exactly
//! (20 packages, 22 version entries — `lodash` and `zod` each carry two for
//! lineage testing). Kept as a literal list rather than parsed from the TOML
//! at test time so a missing/renamed fixture fails as a clear per-case SKIP
//! instead of a parse error that takes the whole file down with it. If the
//! manifest's npm section changes, update this list to match.
//!
//! # Running
//!
//! ```text
//! cargo test -p nudox-producer-typescript --test real_npm_packages -- --nocapture
//! ```
//!
//! Each case prints a `cost case=…` line (AGENTS-DOCTRINE.md §4) and, on
//! success, an entry count. A missing checkout is a visible per-case SKIP
//! (an environment problem, not a code defect) rather than a failure — run
//! `nu corpus/fetch.nu` first to materialize the corpus.

use std::path::{Path, PathBuf};

use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
use nudox_ir::foreign::Unlinked;
use nudox_producer::{PackageSource, YieldContract, produce};
use nudox_producer_typescript::TypescriptProducer;

/// What a fixture's real public API entitles this sweep to demand of the
/// producer.
///
/// # Why an enum and not a `min_entries: usize`
///
/// The clang sweep (`languages/clang/tests/corpus_sweep.rs`) carries a plain
/// per-package floor because every package in that corpus really does extract.
/// The npm corpus does not: four entries here (`lodash` at both pinned
/// versions, `debug`, and `ws`) are CommonJS with no bundled `.d.ts`, and this
/// producer reads almost nothing out of them — see [`Expect::Stub`] and
/// LIMITATIONS.md L47. A floor is the wrong
/// shape for those, because the only floor they would pass is a floor so low it
/// is the vacuous guard this file just replaced, wearing a number.
///
/// So the degradation is *named* instead, exactly the way
/// [`nudox_producer::YieldContract`] names a producer's own: a `Stub` case
/// asserts an **exact** count, so the day the extractor learns to read these
/// packages the test fails and the claim has to be retracted in the same
/// change that invalidates it. That is [`nudox_producer::ProducerError`]'s
/// `YieldContractOutgrown` reasoning applied one level up, at the corpus.
enum Expect {
    /// The producer analyses this package and must contribute at least this
    /// many declarations *beyond* the root `produce` synthesizes.
    ///
    /// Floors are set below the count measured on 2026-08-07 with enough margin
    /// that ordinary extractor churn does not trip them, and high enough that
    /// falling back to a stub does. They are not the measured numbers: a floor
    /// pinned to today's exact output is a change-detector, not an assertion.
    Declarations(usize),

    /// The producer contributes (almost) nothing for this package, for a reason
    /// that is a real property of the package rather than a defect this sweep
    /// should hide.
    ///
    /// `contributed` is asserted **exactly**, not as a floor. `why` must name
    /// the concrete obstruction, in the [`nudox_producer::DegradedYield`] sense
    /// — specific enough that a reader can check whether it still holds.
    Stub {
        contributed: usize,
        why: &'static str,
    },
}

/// One npm corpus fixture: the manifest package name, the pinned version, the
/// `.real-crates/` directory name `fetch.nu`'s `safe-dir-name` produces for it
/// (`/` -> `__`, joined with `-<version>`), and what the producer owes it.
struct Fixture {
    name: &'static str,
    version: &'static str,
    dir: &'static str,
    expect: Expect,
}

/// The three `Expect::Stub` reasons, written once so the three fixtures that
/// share the same real obstruction cannot drift into three different accounts
/// of it.
const NO_TYPE_DECLARATIONS: &str =
    "ships no `.d.ts` and declares no `types`/`typings` in package.json — a pure \
     CommonJS tarball whose API escapes only through `module.exports` at runtime, \
     so there is no top-level declaration for the extractor to read. Verified \
     against the 2026-08-07 checkout under `.real-crates/`.";

/// The 22 npm version entries from `corpus/manifest.toml`, in manifest order.
const FIXTURES: &[Fixture] = &[
    // lodash publishes its types as the separate `@types/lodash` package;
    // `lodash.js` itself is a UMD bundle whose ~300 functions are assigned
    // inside one closure. `.real-crates/lodash-4.17.21/` contains no `.d.ts`.
    Fixture { name: "lodash", version: "4.17.21", dir: "lodash-4.17.21",
              expect: Expect::Stub { contributed: 1, why: NO_TYPE_DECLARATIONS } },
    Fixture { name: "lodash", version: "4.17.20", dir: "lodash-4.17.20",
              expect: Expect::Stub { contributed: 1, why: NO_TYPE_DECLARATIONS } },
    Fixture { name: "zod", version: "3.22.4", dir: "zod-3.22.4",
              expect: Expect::Declarations(800) },
    Fixture { name: "zod", version: "3.23.8", dir: "zod-3.23.8",
              expect: Expect::Declarations(800) },
    Fixture { name: "type-fest", version: "4.10.2", dir: "type-fest-4.10.2",
              expect: Expect::Declarations(300) },
    Fixture { name: "chalk", version: "5.3.0", dir: "chalk-5.3.0",
              expect: Expect::Declarations(80) },
    Fixture { name: "commander", version: "12.0.0", dir: "commander-12.0.0",
              expect: Expect::Declarations(250) },
    Fixture { name: "axios", version: "1.6.7", dir: "axios-1.6.7",
              expect: Expect::Declarations(400) },
    Fixture { name: "date-fns", version: "3.3.1", dir: "date-fns-3.3.1",
              expect: Expect::Declarations(1500) },
    Fixture { name: "rxjs", version: "7.8.1", dir: "rxjs-7.8.1",
              expect: Expect::Declarations(1500) },
    Fixture { name: "immer", version: "10.0.3", dir: "immer-10.0.3",
              expect: Expect::Declarations(40) },
    Fixture { name: "uuid", version: "9.0.1", dir: "uuid-9.0.1",
              expect: Expect::Declarations(30) },
    // `main` is `./src/index.js`; the tarball is four `.js` files and a README.
    Fixture { name: "debug", version: "4.3.4", dir: "debug-4.3.4",
              expect: Expect::Stub { contributed: 1, why: NO_TYPE_DECLARATIONS } },
    Fixture { name: "left-pad", version: "1.3.0", dir: "left-pad-1.3.0",
              expect: Expect::Declarations(4) },
    Fixture { name: "yup", version: "1.4.0", dir: "yup-1.4.0",
              expect: Expect::Declarations(250) },
    // `exports.require` is plain CommonJS `index.js`; no `.d.ts` in the tarball.
    Fixture { name: "ws", version: "8.16.0", dir: "ws-8.16.0",
              expect: Expect::Stub { contributed: 2, why: NO_TYPE_DECLARATIONS } },
    Fixture { name: "@types/node", version: "20.11.0", dir: "@types__node-20.11.0",
              expect: Expect::Declarations(9000) },
    Fixture { name: "fp-ts", version: "2.16.5", dir: "fp-ts-2.16.5",
              expect: Expect::Declarations(5000) },
    Fixture { name: "class-validator", version: "0.14.1", dir: "class-validator-0.14.1",
              expect: Expect::Declarations(800) },
    Fixture { name: "reflect-metadata", version: "0.2.1", dir: "reflect-metadata-0.2.1",
              expect: Expect::Declarations(40) },
    Fixture { name: "p-limit", version: "5.0.0", dir: "p-limit-5.0.0",
              expect: Expect::Declarations(3) },
    Fixture { name: "dayjs", version: "1.11.10", dir: "dayjs-1.11.10",
              expect: Expect::Declarations(80) },
];

/// Root of the corpus checkout directory, resolved relative to this crate's
/// manifest so the test works regardless of the invoking shell's cwd.
fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../.real-crates")
}

fn fixture_root(dir: &str) -> PathBuf {
    corpus_root().join(dir)
}

/// Whether `name` is a real, non-empty JavaScript/TypeScript identifier rather
/// than a path fragment, a file name, or a placeholder.
///
/// The doc comment on the sweep below has promised this check since the file
/// was written; it is implemented here rather than as `!name.contains('/')`
/// because "no path separator" is the weakest possible reading of it. A leading
/// alphabetic/`_`/`$` followed only by identifier characters excludes `/` and
/// `\` by construction, and also excludes the shapes a stubbed extractor
/// actually emits — `""`, `"index.d.ts"`, `"src/index"`, `"<anonymous>"`,
/// `"0"` — none of which `!contains('/')` would have caught.
fn is_real_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_alphabetic() || first == '_' || first == '$' => {
            chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        }
        _ => false,
    }
}

/// What one fixture's lowering actually yielded, in the terms the sweep asserts
/// on.
struct Lowered {
    /// Declarations the producer contributed **beyond** the root `produce`
    /// synthesizes before `lower` is ever called.
    ///
    /// Derived from the entries themselves — an entry the producer declared has
    /// a parent, and `seal` leaves exactly one parentless entry, the
    /// synthesized root (`ir/model/src/package/seal.rs`'s
    /// `seal_materializes_the_tree` pins that) — never as `len() - 1`, which
    /// would be arithmetic over a representation this test does not own.
    contributed: usize,
    /// How many contributed entries carry a real identifier name.
    identifiers: usize,
    /// The first few contributed names, so a failure says *what* was found
    /// instead of only that the count was wrong.
    sample: Vec<String>,
    /// The contract `produce` held the producer to for this package.
    contract: YieldContract,
}

/// Lower one fixture through the real `Producer` pipeline, or return the full
/// error chain as `Err`.
///
/// Walks `std::error::Error::source` per AGENTS-DOCTRINE.md §8 ("print the
/// whole `#[source]` chain") — this crate's `ProducerError` wraps
/// `discover_entry_points`/`build_and_extract` failures behind
/// `ProducerError::OracleSpawn { reason: io::Error::other(e), .. }`, and the
/// terse top-level `Display` alone ("workspace load failed"-style) does not
/// name the file or the real cause.
fn lower(root: &Path, name: &str, version: &str) -> Result<Lowered, String> {
    let src = PackageSource::new(root, name, version);
    let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new(name));

    produce(&TypescriptProducer::new(), &src, &lineage, &Unlinked)
        .map(|produced| {
            let table = &produced.table;
            let declared: Vec<&str> = table
                .iter()
                .filter(|(id, _)| table.parent_of(*id).is_some())
                .map(|(_, e)| e.sym().name.as_str())
                .collect();

            Lowered {
                contributed: declared.len(),
                identifiers: declared
                    .iter()
                    .filter(|n| is_real_identifier(n))
                    .count(),
                sample: declared.iter().take(6).map(|n| (*n).to_owned()).collect(),
                contract: produced.contract.clone(),
            }
        })
        .map_err(|err| {
            let mut chain = format!("{err}");
            let mut cursor: &dyn std::error::Error = &err;
            while let Some(source) = std::error::Error::source(cursor) {
                chain.push_str(&format!("\n  caused by: {source}"));
                cursor = source;
            }
            chain
        })
}

/// Every present npm fixture lowers to a non-trivial, well-formed IR table.
///
/// One test, not 22, so a single `cargo test` run reports the whole corpus
/// sweep in one line-per-case log instead of 22 separate harness entries —
/// but every case is measured individually via `nudox_test_support::measured`
/// so the `cost case=…` line (and thus the per-package number) survives.
///
/// Assertions per fixture (never `is_ok()`, per §4):
/// * the producer's [`YieldContract`] is not a declared degradation — an
///   inert producer must not be counted here as a documented package;
/// * the count of declarations it contributed *beyond the synthesized root*
///   meets the fixture's [`Expect`] — a floor for the packages it really
///   reads, an exact count for the four it does not;
/// * at least one contributed entry's name is a real identifier
///   (`is_real_identifier`) — the content assertion this doc comment promised
///   from the day the file was written and the body did not implement.
///
/// # What the count used to be, and why it could not fail
///
/// This sweep previously failed only on `entry_count == 0`, taken from
/// `produced.table.len()`. That branch was unreachable: [`produce`] builds the
/// root [`nudox_ir::entry::Symbol`] itself and hands it to `Lowering::new`
/// before `lower` is called, so *every* successful run seals a table of at
/// least one entry and the count is never zero. A producer that read no bytes
/// at all scored 1 and passed. The replacement counts only what the producer
/// contributed, which is 0 in exactly that case — and which [`produce`] now
/// rejects outright as `ProducerError::NoDeclarationsContributed`, making the
/// old guard unreachable from a second direction as well.
///
/// Any fixture whose checkout is missing is skipped with a printed reason
/// (environment problem, not a code defect) rather than failing the whole
/// sweep, and the final assertion fails loudly if *none* of the 22 were
/// actually exercised — a directory rename that silently skipped every case
/// must not read as a green run.
#[test]
fn every_real_npm_fixture_contributes_the_declarations_it_is_pinned_to() {
    let mut ran = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for fixture in FIXTURES {
        let root = fixture_root(fixture.dir);
        if !root.join("package.json").is_file() {
            eprintln!(
                "SKIP: no checkout at {} (run `nu corpus/fetch.nu` to materialize the corpus)",
                root.display()
            );
            continue;
        }

        // Counted here, not on the success path: `ran` is "fixtures this run
        // actually exercised", and it is the denominator of the summary below.
        // Incrementing it only on `Ok` made a fixture that both lowered and
        // then failed an assertion appear in numerator and denominator
        // separately — the summary read "2/24" over a 22-entry corpus.
        ran += 1;

        let case = format!("lower/npm/{}-{}", fixture.name, fixture.version);
        let (result, cost) = nudox_test_support::measured(&case, &root, || {
            lower(&root, fixture.name, fixture.version)
        });

        let lowered = match result {
            Ok(lowered) => lowered,
            Err(chain) => {
                failures.push(format!("{}-{}: {chain}", fixture.name, fixture.version));
                continue;
            }
        };

        let Lowered {
            contributed,
            identifiers,
            sample,
            contract,
        } = lowered;
        eprintln!(
            "OK: {}-{} contributed {contributed} declarations ({identifiers} named by a \
             real identifier) in {:.2}s; first names: {sample:?}",
            fixture.name,
            fixture.version,
            cost.wall.as_secs_f64()
        );

        // A producer that declares itself inert must not be tallied as a
        // documented package. `produce` already rejects a *contradiction*
        // between the declaration and the output; what it cannot know is that
        // this corpus expects real analysis, which is this sweep's to say.
        if let Some(degraded) = contract.degraded() {
            failures.push(format!(
                "{}-{}: the producer declares `YieldContract::RootOnly` — this corpus \
                 exists to measure real extraction, not a declared degradation: {degraded}",
                fixture.name, fixture.version
            ));
            continue;
        }

        match fixture.expect {
            Expect::Declarations(floor) => {
                if contributed < floor {
                    failures.push(format!(
                        "{}-{}: contributed {contributed} declarations beyond the \
                         synthesized root, expected at least {floor} — this is not real \
                         extraction. First names: {sample:?}",
                        fixture.name, fixture.version
                    ));
                    continue;
                }
            }
            Expect::Stub {
                contributed: pinned,
                why,
            } => {
                // Asserted as equality on purpose: see `Expect::Stub`. If the
                // extractor learns to read these packages this fails, and the
                // fixture has to be promoted to `Expect::Declarations` in the
                // same change — the claim cannot outlive the obstruction.
                if contributed != pinned {
                    failures.push(format!(
                        "{}-{}: pinned as a stub at {pinned} contributed declarations \
                         because it {why} — got {contributed}. If the extractor now reads \
                         this package, promote the fixture to \
                         `Expect::Declarations`; if it regressed, that is a real defect. \
                         First names: {sample:?}",
                        fixture.name, fixture.version
                    ));
                    continue;
                }
            }
        }

        // The content assertion (AGENTS-DOCTRINE.md §4): a count is satisfied by
        // a table of placeholders, a real exported name is not.
        if identifiers == 0 {
            failures.push(format!(
                "{}-{}: none of its {contributed} contributed entries is named by a real \
                 identifier — a table of path fragments or placeholders is not a lowered \
                 public API. First names: {sample:?}",
                fixture.name, fixture.version
            ));
        }
    }

    assert!(
        ran > 0,
        "no npm fixtures were found under {} — run `nu corpus/fetch.nu` first",
        corpus_root().display()
    );

    assert!(
        failures.is_empty(),
        "{}/{ran} exercised npm fixtures did not contribute the declarations they are \
         pinned to:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
