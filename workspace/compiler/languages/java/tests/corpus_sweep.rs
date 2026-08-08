//! Sweeps every provisioned `maven` corpus package through the full
//! `nudox_producer::produce` pipeline (`invoke` -> `lower` -> `finish` ->
//! `seal`) against real, third-party sources-jar checkouts under
//! `.real-crates/`.
//!
//! `producer_tests.rs` proves the pipeline works end to end on one package
//! (Gson 2.11.0, vendored) with deep content assertions. This file proves it
//! (or does not) on all 20 `maven` corpus packages / 22 version entries, per
//! AGENTS-DOCTRINE.md §4's warning that a producer's hand-fixture test suite
//! tells you nothing about real code.
//!
//! This intentionally does not abort on the first failure: the whole point
//! of a first sweep is to find every defect in one pass. Each entry's
//! outcome is recorded and printed; a summary at the end lists successes and
//! failures with the full `std::error::Error` source chain for every
//! failure (never just the terse top-level `Display`).
//!
//! # Why most entries are still expected to fail — the corrected diagnosis
//!
//! This corpus's "5(now 6)/22 lower" number has been diagnosed **twice**
//! before this pass, and both diagnoses were wrong in a specific, checkable
//! way:
//!
//! 1. "`javadoc` is invoked with no classpath at all; add one" — true as far
//!    as it went, but a classpath flag alone cannot lower a package whose
//!    dependency's *source* was never fetched. `JavaProducer::invoke`
//!    (`src/producer.rs`) now *does* pass `-sourcepath` (see that module's
//!    doc comment for the mechanism and its limits) — it fixed exactly one
//!    entry.
//! 2. "The 17 failures are corpus-provisioning gaps — the packages
//!    themselves were never fetched" — also false: all 20 `maven`
//!    `[[packages]]` / 22 version entries in `corpus/manifest.toml` **are**
//!    on disk under `.real-crates/`, hash-verified by `corpus/fetch.nu`.
//!    What is missing is not the 22 corpus packages but *their own*
//!    dependencies — third-party artifacts that were never corpus entries in
//!    the first place, because the maven corpus was deliberately provisioned
//!    as one sources-jar per artifact with no transitive closure (see
//!    `corpus/README.md`).
//!
//! The real, sweep-verified shape: of the 17 non-`known_good` entries, 16
//! need one or more Maven artifacts this corpus does not carry at all
//! (Guava needs `com.google.errorprone`/`org.checkerframework`/
//! `javax.annotation`; JUnit 4 needs `org.hamcrest`; Jackson-databind needs
//! `jackson-core` + `jackson-annotations`; JUnit 5 / Logback additionally hit
//! `module-info.java`'s closed-world module resolution; Lombok's and Vavr's
//! sources-jars ship non-API content — Lombok bundles its own JUnit-4 test
//! tree, Vavr's `API.java` uses `yield` as a bare method name, legal pre-JDK
//! 14 and a **parse**-level error at this JDK's default `--release`, which
//! masks a *second*, real missing-dependency error
//! (`io.vavr.match.annotation`, a separate `io.vavr:vavr-match` artifact)
//! that only surfaces once `NUDOX_JAVA_RELEASE` un-masks the parse error —
//! confirmed by hand-running `javadoc --release 8` against both). None of
//! those 16 are fixable by `-sourcepath` alone, because the missing package
//! is never satisfied by any *other* corpus entry either (checked
//! exhaustively: no failing entry's unresolved import matches a top-level
//! Java package any other corpus artifact actually provides). Exactly one
//! entry — `io.reactivex.rxjava3:rxjava` — needed precisely one artifact
//! (`org.reactivestreams:reactive-streams`, 4 interfaces, confirmed by
//! rerunning `javadoc -Xmaxerrs 100000` and observing every one of its
//! ~2200 errors trace to that single package) and nothing else. That
//! artifact is now a corpus entry (`corpus/manifest.toml`), and rxjava has
//! moved from `ENTRIES`-that-fail into `known_good` below.
//!
//! Fixing the other 16 is real corpus-provisioning work (new
//! `[[packages]]` entries per missing artifact, verified the same way
//! rxjava's was) — legitimately in `corpus/manifest.toml`'s scope, not a
//! `nudox-producer-java` code change, and out of scope for this pass.

use std::path::{Path, PathBuf};

use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
use nudox_producer::{PackageSource, Producer, produce};
use nudox_producer_java::JavaProducer;

/// One corpus entry: `.real-crates/<dir>`, the `groupId:artifactId` name
/// used both as the on-disk name-selector-equivalent and the
/// `PackageLineageId`, and the ecosystem version string. Mirrors
/// `corpus/manifest.toml`'s 20 `maven` `[[packages]]` blocks / 22 version
/// entries exactly (`guava` and `jackson-databind` each carry two versions
/// for lineage testing).
struct Entry {
    dir: &'static str,
    name: &'static str,
    version: &'static str,
}

const ENTRIES: &[Entry] = &[
    Entry { dir: "com.google.code.gson__gson-2.10.1", name: "com.google.code.gson:gson", version: "2.10.1" },
    Entry { dir: "com.google.guava__guava-33.0.0-jre", name: "com.google.guava:guava", version: "33.0.0-jre" },
    Entry { dir: "com.google.guava__guava-32.1.3-jre", name: "com.google.guava:guava", version: "32.1.3-jre" },
    Entry {
        dir: "org.apache.commons__commons-lang3-3.14.0",
        name: "org.apache.commons:commons-lang3",
        version: "3.14.0",
    },
    Entry { dir: "junit__junit-4.13.2", name: "junit:junit", version: "4.13.2" },
    Entry {
        dir: "org.junit.jupiter__junit-jupiter-api-5.10.2",
        name: "org.junit.jupiter:junit-jupiter-api",
        version: "5.10.2",
    },
    Entry {
        dir: "com.fasterxml.jackson.core__jackson-databind-2.16.1",
        name: "com.fasterxml.jackson.core:jackson-databind",
        version: "2.16.1",
    },
    Entry {
        dir: "com.fasterxml.jackson.core__jackson-databind-2.15.4",
        name: "com.fasterxml.jackson.core:jackson-databind",
        version: "2.15.4",
    },
    Entry { dir: "org.projectlombok__lombok-1.18.30", name: "org.projectlombok:lombok", version: "1.18.30" },
    Entry { dir: "io.reactivex.rxjava3__rxjava-3.1.8", name: "io.reactivex.rxjava3:rxjava", version: "3.1.8" },
    Entry { dir: "org.slf4j__slf4j-api-2.0.12", name: "org.slf4j:slf4j-api", version: "2.0.12" },
    Entry {
        dir: "ch.qos.logback__logback-classic-1.4.14",
        name: "ch.qos.logback:logback-classic",
        version: "1.4.14",
    },
    Entry {
        dir: "com.squareup.retrofit2__retrofit-2.9.0",
        name: "com.squareup.retrofit2:retrofit",
        version: "2.9.0",
    },
    Entry {
        dir: "org.apache.httpcomponents.client5__httpclient5-5.3.1",
        name: "org.apache.httpcomponents.client5:httpclient5",
        version: "5.3.1",
    },
    Entry { dir: "com.google.dagger__dagger-2.51", name: "com.google.dagger:dagger", version: "2.51" },
    Entry {
        dir: "org.mapstruct__mapstruct-1.5.5.Final",
        name: "org.mapstruct:mapstruct",
        version: "1.5.5.Final",
    },
    Entry { dir: "io.vavr__vavr-0.10.4", name: "io.vavr:vavr", version: "0.10.4" },
    Entry { dir: "org.assertj__assertj-core-3.25.3", name: "org.assertj:assertj-core", version: "3.25.3" },
    Entry { dir: "com.h2database__h2-2.2.224", name: "com.h2database:h2", version: "2.2.224" },
    Entry { dir: "org.mockito__mockito-core-5.10.0", name: "org.mockito:mockito-core", version: "5.10.0" },
    Entry {
        dir: "jakarta.validation__jakarta.validation-api-3.0.2",
        name: "jakarta.validation:jakarta.validation-api",
        version: "3.0.2",
    },
    Entry {
        dir: "org.apache.kafka__kafka-clients-3.7.0",
        name: "org.apache.kafka:kafka-clients",
        version: "3.7.0",
    },
];

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../.real-crates")
        .canonicalize()
        .expect("no .real-crates/ checkout — see corpus/README.md to (re)provision it")
}

/// Walk the full `std::error::Error` source chain. `ProducerError`'s
/// top-level `Display` is deliberately terse (AGENTS-DOCTRINE.md's
/// "hard-won facts": reading only it turns a five-second diagnosis into an
/// hour).
fn chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = e.to_string();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = e.source();
    while let Some(src) = cur {
        out.push_str("\n  caused by: ");
        out.push_str(&src.to_string());
        cur = src.source();
    }
    out
}

enum Outcome {
    Ok {
        table_len: usize,
        oracle_types: usize,
        oracle_members: usize,
        unlinked: usize,
        /// Every lowered symbol's name, owned so `run_entry` can drop the
        /// full `Produced` value. Used to assert on *named* content (a type
        /// that really exists in the package's source) rather than a count —
        /// see `KNOWN_CONTENT` below and AGENTS-DOCTRINE.md §4.
        symbol_names: Vec<String>,
    },
    Fail {
        stage: &'static str,
        chain: String,
    },
}

fn run_entry(entry: &Entry) -> Outcome {
    let root = corpus_root().join(entry.dir);
    if !root.is_dir() {
        return Outcome::Fail {
            stage: "preflight",
            chain: format!("no directory at {}", root.display()),
        };
    }

    let src = PackageSource::new(&root, entry.name, entry.version);

    // Independent oracle floor: a SEPARATE `invoke()` call from the one
    // `produce()` makes internally (mirrors the Go corpus sweep's use of the
    // inherent `invoke_oracle` alongside `produce()`), so the reported decl
    // count cannot be silently inflated or deflated by whatever `produce()`
    // itself happens to do. Measured on its own (not folded into the
    // `produce()` case below) so that entries which fail here — most of
    // this corpus, per this file's module doc — still emit a real `cost
    // case=` benchmark line for the actual subprocess work `javadoc` did,
    // rather than going dark just because the pipeline stopped early.
    let oracle_case = format!("java-sweep-oracle-{}", entry.dir);
    let (oracle_result, _oracle_cost) =
        nudox_test_support::measured(&oracle_case, &root, || JavaProducer::new().invoke(&src));
    let extraction = match oracle_result {
        Ok(e) => e,
        Err(e) => {
            return Outcome::Fail {
                stage: "oracle (independent verification run)",
                chain: chain(&e),
            };
        }
    };
    let oracle_types = extraction.types.len();
    let oracle_members: usize = extraction
        .types
        .iter()
        .map(|t| {
            t.fields.len() + t.methods.len() + t.constructors.len() + t.enum_constants.len()
        })
        .sum();

    let lid = PackageLineageId::new(EcosystemId::new("maven"), PackageName::new(entry.name));
    let case = format!("java-sweep-{}", entry.dir);
    let (produced, _cost) =
        nudox_test_support::measured(&case, &root, || produce(&JavaProducer::new(), &src, &lid, &nudox_ir::foreign::Unlinked));

    match produced {
        Ok(p) => {
            let symbol_names = p.table.iter().map(|(_, e)| e.sym().name.clone()).collect();
            Outcome::Ok {
                table_len: p.table.len(),
                oracle_types,
                oracle_members,
                unlinked: p.report.unlinked.len(),
                symbol_names,
            }
        }
        Err(e) => Outcome::Fail {
            stage: "produce (invoke/lower/finish/seal)",
            chain: chain(&e),
        },
    }
}

/// Per-entry named-content floor: at least one symbol name that genuinely
/// appears in that package's own source, keyed by `Entry::dir`. This is the
/// AGENTS-DOCTRINE.md §4 bar ("a symbol that really exists"), which the
/// oracle-count comparison below cannot provide on its own — a producer that
/// silently renamed or fabricated symbols could still satisfy a count-only
/// check. `io.reactivex.rxjava3:rxjava` is the package this pass newly
/// fixed (see this file's module doc for the `-sourcepath` mechanism and
/// why it fixed exactly this one entry); `Flowable`, `Observable`, `Single`,
/// and `Completable` are `io.reactivex.rxjava3.core`'s well-known top-level
/// reactive types, each a real, separate `.java` file in that checkout.
const KNOWN_CONTENT: &[(&str, &[&str])] = &[(
    "io.reactivex.rxjava3__rxjava-3.1.8",
    &["Flowable", "Observable", "Single", "Completable"],
)];

#[test]
fn every_provisioned_maven_corpus_package_lowers_through_the_real_producer() {
    let mut failures = Vec::new();
    let mut successes = Vec::new();

    for entry in ENTRIES {
        eprintln!("=== {} ({} @ {}) ===", entry.dir, entry.name, entry.version);
        match run_entry(entry) {
            Outcome::Ok { table_len, oracle_types, oracle_members, unlinked, symbol_names } => {
                eprintln!(
                    "OK  {}: table_len={table_len} oracle_types={oracle_types} \
                     oracle_members={oracle_members} unlinked_refs={unlinked}",
                    entry.dir
                );
                // Real-content floor, not a bare non-zero check: the finished
                // table must hold at least as many live entries as the oracle
                // independently reported top-level types — the table also
                // nests fields/methods/enum-constants under those, so it can
                // only be >=; anything less means lowering silently dropped
                // declared symbols.
                assert!(
                    table_len >= oracle_types,
                    "{}: table has fewer live entries ({table_len}) than the oracle's own \
                     independently-counted types ({oracle_types}) — lowering dropped symbols",
                    entry.dir
                );
                assert!(oracle_types > 0, "{}: oracle reported zero types — not real content", entry.dir);
                if let Some((_, expected)) = KNOWN_CONTENT.iter().find(|(dir, _)| *dir == entry.dir) {
                    for name in *expected {
                        assert!(
                            symbol_names.iter().any(|n| n == name),
                            "{}: expected real symbol {name:?} not found among {} lowered names \
                             — content floor failed, not just a count",
                            entry.dir,
                            symbol_names.len()
                        );
                    }
                }
                successes.push((entry.dir, table_len, oracle_members));
            }
            Outcome::Fail { stage, chain } => {
                eprintln!("FAIL {} at [{stage}]: {chain}", entry.dir);
                failures.push((entry.dir, stage, chain));
            }
        }
    }

    eprintln!(
        "\n=== maven corpus sweep summary: {}/{} succeeded ===",
        successes.len(),
        ENTRIES.len()
    );
    for (dir, table_len, oracle_members) in &successes {
        eprintln!("  OK   {dir}: {table_len} live entries (oracle reported {oracle_members} members)");
    }
    for (dir, stage, chain) in &failures {
        eprintln!("  FAIL {dir} [{stage}]: {chain}");
    }

    // Still diagnostic, not a pass/fail gate over ALL 22 — re-examined for
    // this pass, per the task's instruction to check whether that stance is
    // still right once packages actually start working, and it is: the
    // remaining 16 failures (see this file's module doc) each need a
    // specific *other* Maven artifact's source that is not, and was never
    // meant to be, a corpus entry — `failures.is_empty()` would still turn
    // an honest, per-package-diagnosed limitation into a red CI run for
    // reasons that have nothing to do with a regression in this producer.
    // What DOES change here: `known_good` is no longer just "verified to
    // resolve offline", it is "verified to resolve, and — where a fix
    // landed in this pass — verified on named content, not just success".
    // rxjava moved into this list this pass; see `KNOWN_CONTENT` above for
    // its content floor and this file's module doc for why `-sourcepath`
    // fixed exactly this one entry and no others.
    let known_good = [
        "com.google.code.gson__gson-2.10.1",
        "org.apache.commons__commons-lang3-3.14.0",
        "org.slf4j__slf4j-api-2.0.12",
        "org.mapstruct__mapstruct-1.5.5.Final",
        "jakarta.validation__jakarta.validation-api-3.0.2",
        "io.reactivex.rxjava3__rxjava-3.1.8",
    ];
    for dir in known_good {
        assert!(
            successes.iter().any(|(d, _, _)| *d == dir),
            "{dir} was previously verified to resolve offline through this producer and must \
             keep succeeding; see failures list above for what broke it"
        );
    }
}
