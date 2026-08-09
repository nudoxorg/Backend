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
//! # The ceiling under current policy: 19/22, and the last three are language
//! and toolchain walls, not missing artifacts
//!
//! This corpus's lowering count has been diagnosed **four** times before this
//! pass, at 5, 6, 14 and 14 again. Every one of those diagnoses was
//! directionally useful and wrong in at least one checkable way; the pattern
//! that keeps recurring is that a stated blocker turns out to be the *first*
//! blocker, and fixing it reveals the real one. Three of the four "structural,
//! unfixable" pins this file carried before this pass were not structural at
//! all:
//!
//! 1. **h2 and kafka-clients were pinned on closure size.** Download size is
//!    not a reason to skip a package, and neither pin survived contact with
//!    the actual provisioning. h2 is now green on the full Lucene + OSGi +
//!    both-servlet-generations + JTS closure, taken from h2's own GitHub
//!    `pom.xml` at tag `version-2.2.224` because its *published* POM declares
//!    zero dependencies. kafka-clients' whole closure
//!    (zstd-jni, lz4-java, snappy-java, jose4j, opentelemetry-proto,
//!    protobuf-java, Jackson) is likewise provisioned, taken from Kafka's
//!    `gradle/dependencies.gradle` at tag `3.7.0` because its POM declares 4
//!    runtime-scope dependencies and omits jose4j and opentelemetry-proto
//!    entirely — and that closure did resolve. kafka is still pinned, but on
//!    a completely different and much smaller thing: exactly one unresolvable
//!    symbol inside jackson-databind, described in its test below. Every
//!    entry is named in `corpus/manifest.toml` with the file and import it
//!    resolves.
//! 2. **httpclient5 was pinned on `org.conscrypt` being unobtainable as
//!    source.** It is obtainable. The earlier pass checked
//!    `conscrypt-openjdk-uber`, whose sources jar is genuinely missing the
//!    generated `NativeConstants.java`; plain `org.conscrypt:conscrypt-openjdk`
//!    ships it (134 `.java` entries against the uber jar's 133, and that one
//!    file is the whole difference). httpclient5 is green.
//! 3. **The JPMS three — logback-classic, junit-jupiter-api, assertj-core —
//!    were pinned on a policy tradeoff that has now been taken deliberately.**
//!    All three are green. See "The JPMS exception" below for exactly what was
//!    conceded and what was not.
//!
//! The three that remain — `lombok`, `retrofit`, `kafka-clients` — are each
//! pinned on something no `[[packages]]` entry, `[[jpms_modules]]` entry, or
//! compiler flag can supply, and each pin test below asserts that *specific*
//! cause rather than "some error", so a silent drift to a different cause (or
//! a silent fix) is caught rather than rotting unnoticed.
//!
//! # The JPMS exception, and its exact boundary
//!
//! `logback-classic`, `junit-jupiter-api` and `assertj-core` all failed
//! because `javac`'s module system has positions that a sources jar cannot
//! fill. `corpus/README.md`'s maven row says the corpus fetches sources jars
//! because "asking for anything else would hand the producer bytecode instead
//! of Java". That rule is about what gets **lowered**, and it is still
//! absolute: every `.java` file this producer ever reads comes from a sources
//! jar.
//!
//! What was conceded is narrower: `corpus/manifest.toml` gained a
//! `[[jpms_modules]]` array — six compiled jars, fetched only so `javac` can
//! *resolve a module descriptor*, copied unextracted into
//! `.real-crates/.module-path/`, and passed to nothing but `--module-path`.
//! `JavaProducer::sourcepath_entries` skips the dot-directory, and
//! `discover_java_sources` would find zero `.java` files in it regardless, so
//! a binary jar cannot become lowering input by accident — that is a
//! structural property of where the bytes live, not a convention. The array
//! being separate from `[[packages]]` is what makes it so.
//!
//! Three distinct situations forced it, and every module that could stay
//! source did (`apiguardian-api`, `junit-platform-commons`, `jakarta.mail-api`,
//! `jakarta.activation-api` and `jakarta.servlet-api:6.0.0` are all ordinary
//! sources-jar `[[packages]]` entries resolved through `--module-source-path`):
//!
//! * **No `module-info.java` in the sources jar at all** — `org.slf4j` and
//!   `org.opentest4j`. Only their compiled jars carry the descriptor
//!   (slf4j-api's under the multi-release path
//!   `META-INF/versions/9/module-info.class`, opentest4j's plainly at the jar
//!   root). Verified with `unzip -l` on both classifiers of both artifacts.
//! * **A source `module-info.java` that `requires` an automatic module** —
//!   `ch.qos.logback.core`. Its sources jar *does* carry a real
//!   `module-info.java`, and that is the problem: it says `requires static
//!   janino;` and `requires static commons.compiler;`. Those are automatic
//!   module names, derived from jar filenames, so by construction no source
//!   artifact can ever satisfy them — compiling logback-core from source dies
//!   on `module not found: janino` no matter what else is provisioned.
//!   Resolving logback-core as a compiled module does not follow its
//!   `requires static` edges, and logback-classic then compiles clean.
//! * **A target with no `module-info.java` of its own** — `assertj-core`.
//!   `javac` refuses `-sourcepath` and `--module-source-path` in the same
//!   invocation ("cannot specify both --source-path and --module-source-path"),
//!   and assertj-core needs `-sourcepath` for byte-buddy, hamcrest and
//!   opentest4j. So its four JUnit-Jupiter soft-assertion files can only reach
//!   `org.junit.jupiter.api.extension` and `org.junit.platform.commons.support`
//!   through `--module-path` plus `--add-modules ALL-MODULE-PATH`. This is the
//!   one place where a corpus package's own source resolves *dependency* types
//!   out of bytecode; assertj-core's own 782 files are still lowered from
//!   source alone, and junit-jupiter-api still lowers from its own sources.
//!
//! # Two producer capabilities this pass added, both off by default
//!
//! * **`--module-source-path`.** A target carrying its own `module-info.java`
//!   is compiled by `javac` as a named module, and a named module cannot read
//!   `-sourcepath` packages at all — it reads its `requires` closure, and
//!   fails before looking at ordinary source if any of it is missing. The
//!   producer now switches invocation shape on exactly that fact. `gson` and
//!   `jakarta.validation-api` were already green and are both modular; they
//!   were re-verified under the new shape before it was adopted (their
//!   `requires` lists name only system modules) and are asserted in
//!   `known_good` below.
//! * **`NUDOX_JAVA_ADD_EXPORTS`.** `--add-exports java.base/sun.security.x509=ALL-UNNAMED`,
//!   set for `httpclient5` alone. Conscrypt's `Platform.java` imports
//!   `sun.security.x509.AlgorithmId` and conscrypt's own Gradle build only
//!   gets away with it by compiling at `sourceCompatibility 1.7`, before the
//!   module system enforced exports. No artifact can supply a package that
//!   lives inside `java.base`, and `--release 8` makes it *worse*, not better
//!   (`ct.sym` hides internal packages entirely rather than merely
//!   unexporting them).
//!
//! Both are per-entry env knobs rather than always-on flags for the same
//! reason `NUDOX_JAVA_RELEASE` is: a globally more permissive compile
//! environment turns "the corpus lowers" into a claim about our flags instead
//! of a claim about the code.
//!
//! # Carried forward from previous passes (still load-bearing)
//!
//! * **Package-name collisions on `-sourcepath`.** Provisioning *both*
//!   `jackson-core`/`jackson-annotations` 2.15.4 and 2.16.1 side by side
//!   (matching jackson-databind's own two versions) was tried and produces
//!   cross-version `cannot find symbol` contamination. One copy of 2.16.1
//!   satisfies both jackson-databind versions.
//! * **Hamcrest is genuinely two different artifacts here, not one.** junit4
//!   needs pre-2.x `hamcrest-core:1.3` (it uses `org.hamcrest.Factory`,
//!   deleted in the 2.x unification); mockito-core needs unified
//!   `hamcrest:2.2`. Both coexist on every package's `-sourcepath`.
//! * **A POM's declared scope is a hint, not ground truth.** `jackson-core`
//!   imports `ch.randelshofer.fastdoubleparser` with a POM that declares zero
//!   dependencies (Maven Shade relocates it at build time; the sources jar
//!   keeps the original import). `byte-buddy` imports
//!   `edu.umd.cs.findbugs.annotations.*` and `byte-buddy-agent` imports
//!   `com.sun.jna.*` — both `<scope>provided</scope>`. h2's servlet/Lucene/OSGi
//!   closure and kafka's entire closure are invisible from their POMs.
//! * **`NUDOX_JAVA_RELEASE` is dangerous as a blanket fix.** Forcing
//!   `--release 8` corpus-wide (tried directly, not assumed) breaks gson and
//!   jakarta.validation-api (`modules are not supported in -source 8`), guava
//!   (`package sun.misc does not exist`), and mockito-core (`private interface
//!   methods are not supported in -source 8`). It is applied to exactly one
//!   entry (vavr) — see `Entry::release` and `ScopedJavaEnv`. It also cannot
//!   be combined with the module system at all: `javac` rejects
//!   `--module-path` with `target 8`, which is why
//!   `JavaProducer::module_system_available` suppresses it below Java 9. vavr
//!   regressed out of this sweep the one time that was overlooked.
//!
//! # The three pins
//!
//! `lombok`, `retrofit` and `kafka-clients` each have their own `#[test]`
//! below, in the style of `nudox-producer-csharp`'s
//! `system_text_json_hits_the_known_per_tfm_file_duplication_limitation`
//! (`workspace/compiler/languages/csharp/tests/nuget_corpus.rs`): a doc
//! comment naming the specific root cause and a **falsification trigger** —
//! the exact condition under which the pin should be revisited — and a body
//! that asserts the specific known failure substring. Five more `#[test]`s
//! guard the packages that moved *out* of the pinned set this pass, asserting
//! the mechanism that fixed each one rather than just its presence in
//! `known_good`.

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
///
/// `release`, when set, is applied as `NUDOX_JAVA_RELEASE` for *only* this
/// entry's two `invoke()` calls (see `run_entry`), not process-wide — vavr
/// is the one entry that needs `--release 8/9` to unmask its real, second
/// error (see its `KNOWN_CONTENT` comment below). This is NOT a knob to
/// reach for casually: forcing it broke gson and jakarta.validation-api
/// (`modules are not supported in -source 8` — both keep a real
/// `module-info.java`), guava (`package sun.misc does not exist` —
/// `--release` swaps in the `ct.sym` cross-compilation view, which hides
/// JDK-internal packages that are reachable without it), and mockito-core
/// (`private interface methods are not supported in -source 8`) when tried
/// against every other entry during this pass — confirmed by re-running the
/// whole set with it forced on. That is also why lombok — the other
/// package with a real parse-level masking error, per this file's module
/// doc — does NOT get a `release` override here: unmasking it does not lead
/// to a fix, see its pinned test below.
struct Entry {
    dir: &'static str,
    name: &'static str,
    version: &'static str,
    release: Option<&'static str>,
    /// `NUDOX_JAVA_ADD_EXPORTS` for this entry only: a comma-separated list
    /// of `<module>/<package>` specs, each becoming
    /// `--add-exports <module>/<package>=ALL-UNNAMED`. `httpclient5` is the
    /// one entry that needs it — see
    /// `httpclient5_resolves_conscrypt_from_the_non_uber_sources_jar`.
    add_exports: Option<&'static str>,
    /// `NUDOX_JAVA_ADD_MODULES` for this entry only. Needed by a target with
    /// no `module-info.java` of its own that must still read a compiled
    /// module off `--module-path`; `assertj-core` is the corpus's only case.
    add_modules: Option<&'static str>,
}

const ENTRIES: &[Entry] = &[
    Entry { dir: "com.google.code.gson__gson-2.10.1", name: "com.google.code.gson:gson", version: "2.10.1", release: None, add_exports: None, add_modules: None },
    Entry { dir: "com.google.guava__guava-33.0.0-jre", name: "com.google.guava:guava", version: "33.0.0-jre", release: None, add_exports: None, add_modules: None },
    Entry { dir: "com.google.guava__guava-32.1.3-jre", name: "com.google.guava:guava", version: "32.1.3-jre", release: None, add_exports: None, add_modules: None },
    Entry {
        dir: "org.apache.commons__commons-lang3-3.14.0",
        name: "org.apache.commons:commons-lang3",
        version: "3.14.0",
        release: None,
        add_exports: None,
        add_modules: None,
    },
    Entry { dir: "junit__junit-4.13.2", name: "junit:junit", version: "4.13.2", release: None, add_exports: None, add_modules: None },
    Entry {
        dir: "org.junit.jupiter__junit-jupiter-api-5.10.2",
        name: "org.junit.jupiter:junit-jupiter-api",
        version: "5.10.2",
        release: None,
        add_exports: None,
        add_modules: None,
    },
    Entry {
        dir: "com.fasterxml.jackson.core__jackson-databind-2.16.1",
        name: "com.fasterxml.jackson.core:jackson-databind",
        version: "2.16.1",
        release: None,
        add_exports: None,
        add_modules: None,
    },
    Entry {
        dir: "com.fasterxml.jackson.core__jackson-databind-2.15.4",
        name: "com.fasterxml.jackson.core:jackson-databind",
        version: "2.15.4",
        release: None,
        add_exports: None,
        add_modules: None,
    },
    Entry { dir: "org.projectlombok__lombok-1.18.30", name: "org.projectlombok:lombok", version: "1.18.30", release: None, add_exports: None, add_modules: None },
    Entry { dir: "io.reactivex.rxjava3__rxjava-3.1.8", name: "io.reactivex.rxjava3:rxjava", version: "3.1.8", release: None, add_exports: None, add_modules: None },
    Entry { dir: "org.slf4j__slf4j-api-2.0.12", name: "org.slf4j:slf4j-api", version: "2.0.12", release: None, add_exports: None, add_modules: None },
    Entry {
        dir: "ch.qos.logback__logback-classic-1.4.14",
        name: "ch.qos.logback:logback-classic",
        version: "1.4.14",
        release: None,
        add_exports: None,
        add_modules: None,
    },
    Entry {
        dir: "com.squareup.retrofit2__retrofit-2.9.0",
        name: "com.squareup.retrofit2:retrofit",
        version: "2.9.0",
        release: None,
        add_exports: None,
        add_modules: None,
    },
    Entry {
        dir: "org.apache.httpcomponents.client5__httpclient5-5.3.1",
        name: "org.apache.httpcomponents.client5:httpclient5",
        version: "5.3.1",
        release: None,
        // conscrypt's `Platform.java` imports `sun.security.x509.AlgorithmId`.
        // See `httpclient5_resolves_conscrypt_from_the_non_uber_sources_jar`
        // for why neither an artifact nor `--release` can substitute.
        add_exports: Some("java.base/sun.security.x509"),
        add_modules: None,
    },
    Entry { dir: "com.google.dagger__dagger-2.51", name: "com.google.dagger:dagger", version: "2.51", release: None, add_exports: None, add_modules: None },
    Entry {
        dir: "org.mapstruct__mapstruct-1.5.5.Final",
        name: "org.mapstruct:mapstruct",
        version: "1.5.5.Final",
        release: None,
        add_exports: None,
        add_modules: None,
    },
    Entry { dir: "io.vavr__vavr-0.10.4", name: "io.vavr:vavr", version: "0.10.4", release: Some("8"), add_exports: None, add_modules: None },
    Entry {
        dir: "org.assertj__assertj-core-3.25.3",
        name: "org.assertj:assertj-core",
        version: "3.25.3",
        release: None,
        add_exports: None,
        // assertj-core has no `module-info.java`, so its 4 JUnit-Jupiter
        // soft-assertion files can only see `org.junit.jupiter.api.extension`
        // and `org.junit.platform.commons.support` if those modules are pulled
        // into the unnamed module's root set explicitly. See
        // `assertj_core_reads_the_junit_chain_off_the_module_path`.
        add_modules: Some("ALL-MODULE-PATH"),
    },
    Entry { dir: "com.h2database__h2-2.2.224", name: "com.h2database:h2", version: "2.2.224", release: None, add_exports: None, add_modules: None },
    Entry { dir: "org.mockito__mockito-core-5.10.0", name: "org.mockito:mockito-core", version: "5.10.0", release: None, add_exports: None, add_modules: None },
    Entry {
        dir: "jakarta.validation__jakarta.validation-api-3.0.2",
        name: "jakarta.validation:jakarta.validation-api",
        version: "3.0.2",
        release: None,
        add_exports: None,
        add_modules: None,
    },
    Entry {
        dir: "org.apache.kafka__kafka-clients-3.7.0",
        name: "org.apache.kafka:kafka-clients",
        version: "3.7.0",
        release: None,
        add_exports: None,
        add_modules: None,
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

/// Serializes every `javadoc` invocation in this binary, because the
/// per-entry knobs below are process-global environment variables and Cargo
/// runs the `#[test]`s in one binary **in parallel by default**.
///
/// This is not defensive tidiness. Before the lock existed, a plain `cargo
/// test -p nudox-producer-java` failed three tests that a `--test-threads=1`
/// run passed: `httpclient5`'s `--add-exports` was clobbered by a
/// concurrently-starting entry that sets no exports, and `assertj-core` lost
/// its `--add-modules` the same way. Relying on the caller to pass
/// `--test-threads=1` would have left a suite that is green or red depending
/// on an invocation flag, which is worse than either outcome.
///
/// Poison is deliberately ignored: a panicking test leaves the *environment*
/// restored (the guards' `Drop` still runs during unwind), so the only state
/// the mutex protects is already consistent, and honouring poison would turn
/// one real failure into N spurious ones.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Scopes one environment variable to exactly the lifetime of one
/// `run_entry` call, restoring whatever was there before (unset, if nothing
/// was) on drop — so a per-entry knob never leaks into a later entry's
/// invocation. `set_var`/`remove_var` are process-global and normally unsound
/// to call from a multi-threaded test binary; what makes it sound here is
/// [`ENV_LOCK`], which every construction path goes through.
struct ScopedJavaEnv {
    key: &'static str,
    previous: Option<String>,
}

impl ScopedJavaEnv {
    fn set(key: &'static str, value: Option<&str>) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: see struct doc — single-threaded within this binary.
        unsafe {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        ScopedJavaEnv { key, previous }
    }
}

impl Drop for ScopedJavaEnv {
    fn drop(&mut self) {
        // SAFETY: see struct doc — single-threaded within this binary.
        unsafe {
            match &self.previous {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// All three per-entry knobs plus the [`ENV_LOCK`] that makes setting them
/// safe, as one value.
///
/// Field order is drop order: `knobs` restores the previous environment
/// first, and only then is the lock released, so a waiting test never
/// observes a half-restored environment.
struct ScopedEnv {
    #[allow(dead_code)]
    knobs: [ScopedJavaEnv; 3],
    #[allow(dead_code)]
    lock: std::sync::MutexGuard<'static, ()>,
}

/// Locks, then applies all three per-entry knobs in `Entry` field order.
fn scoped_env(entry: &Entry) -> ScopedEnv {
    // Poison is ignored on purpose — see `ENV_LOCK`'s doc comment.
    let lock = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    ScopedEnv {
        knobs: [
            ScopedJavaEnv::set("NUDOX_JAVA_RELEASE", entry.release),
            ScopedJavaEnv::set("NUDOX_JAVA_ADD_EXPORTS", entry.add_exports),
            ScopedJavaEnv::set("NUDOX_JAVA_ADD_MODULES", entry.add_modules),
        ],
        lock,
    }
}

/// The same guard for a caller that drives `JavaProducer::invoke` directly
/// rather than through [`run_entry`] — currently only the lombok pin, which
/// needs to observe *two* different `--release` levels on one checkout.
fn scoped_release(release: Option<&'static str>) -> ScopedEnv {
    scoped_env(&Entry {
        dir: "",
        name: "",
        version: "",
        release,
        add_exports: None,
        add_modules: None,
    })
}

fn run_entry(entry: &Entry) -> Outcome {
    let root = corpus_root().join(entry.dir);
    if !root.is_dir() {
        return Outcome::Fail {
            stage: "preflight",
            chain: format!("no directory at {}", root.display()),
        };
    }

    let _env_guard = scoped_env(entry);
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
const KNOWN_CONTENT: &[(&str, &[&str])] = &[
    (
        "io.reactivex.rxjava3__rxjava-3.1.8",
        &["Flowable", "Observable", "Single", "Completable"],
    ),
    // guava: real, separate top-level types confirmed on disk at
    // com/google/common/{base,collect,util/concurrent}/*.java.
    (
        "com.google.guava__guava-33.0.0-jre",
        &["Preconditions", "Strings", "ImmutableMap", "AbstractFuture"],
    ),
    (
        "com.google.guava__guava-32.1.3-jre",
        &["Preconditions", "Strings", "ImmutableMap", "AbstractFuture"],
    ),
    // junit4: org/junit/{Assert,Test}.java are the package's best-known
    // public API; JUnitMatchers is the file whose org.hamcrest.core.CombinableMatcher
    // import was junit4's actual blocker (see manifest.toml's hamcrest-core comment).
    ("junit__junit-4.13.2", &["Assert", "Test", "JUnitMatchers"]),
    // jackson-databind: ObjectMapper is the package's flagship public class,
    // confirmed on disk at com/fasterxml/jackson/databind/ObjectMapper.java
    // in both version checkouts; AnnotationIntrospector was the file whose
    // `JsonTypeInfo.Value`-typed method was this pass's actual jackson-annotations
    // resolution check.
    (
        "com.fasterxml.jackson.core__jackson-databind-2.16.1",
        &["ObjectMapper", "AnnotationIntrospector"],
    ),
    (
        "com.fasterxml.jackson.core__jackson-databind-2.15.4",
        &["ObjectMapper"],
    ),
    // dagger: real top-level annotation types, each its own file directly
    // under dagger/.
    (
        "com.google.dagger__dagger-2.51",
        &["Component", "Reusable", "Provides", "BindsOptionalOf"],
    ),
    // vavr: API.java is the file whose masked `yield`-identifier parse error
    // and io.vavr.match.annotation dependency this entry's `release`/vavr-match
    // fix (see Entry doc comment and manifest.toml) targets directly; Value
    // and control.{Option,Try} are vavr's other well-known top-level types,
    // confirmed on disk.
    ("io.vavr__vavr-0.10.4", &["API", "Value", "Option", "Try"]),
    // mockito-core: real top-level public API types, each its own file
    // directly under org/mockito/.
    (
        "org.mockito__mockito-core-5.10.0",
        &["Mockito", "MockSettings", "Spy", "InOrder"],
    ),
    // ── the five that moved out of the pinned set this pass ──────────────
    //
    // h2: `Driver` is org/h2/Driver.java, the JDBC entry point. The other
    // three are deliberately the files that were *unreachable* before this
    // pass — one per newly-provisioned closure, so the content floor fails if
    // any of them silently drops back out rather than only if h2 stops
    // lowering altogether: `FullTextLucene` (org/h2/fulltext/, the Lucene
    // integration), `OsgiDataSourceFactory` (org/h2/util/, the OSGi one), and
    // `ValueGeometry` (org/h2/value/, the JTS one).
    (
        "com.h2database__h2-2.2.224",
        &["Driver", "FullTextLucene", "OsgiDataSourceFactory", "ValueGeometry"],
    ),
    // junit-jupiter-api: `Assertions`, `Assumptions` and `Test` are the
    // package's best-known public API, each its own file under
    // org/junit/jupiter/api/. Their presence is what proves the modular
    // compilation actually produced content rather than merely exiting zero.
    (
        "org.junit.jupiter__junit-jupiter-api-5.10.2",
        &["Assertions", "Assumptions", "Test"],
    ),
    // logback-classic: `Logger` and `LoggerContext` are the two types every
    // logback consumer touches; `PatternLayout` and `Level` are separate real
    // files in the same directory. All four sit directly under
    // ch/qos/logback/classic/.
    (
        "ch.qos.logback__logback-classic-1.4.14",
        &["Logger", "LoggerContext", "PatternLayout", "Level"],
    ),
    // httpclient5: `HttpClients` and `CloseableHttpClient` are its public
    // entry points (org/apache/hc/client5/http/impl/classic/), and
    // `ConscryptClientTlsStrategy` (org/apache/hc/client5/http/ssl/) is
    // specifically the file whose single `org.conscrypt.Conscrypt` import was
    // this package's last blocker — naming it here means the conscrypt fix
    // cannot silently rot into "compiles because that file got skipped".
    (
        "org.apache.httpcomponents.client5__httpclient5-5.3.1",
        &["HttpClients", "CloseableHttpClient", "ConscryptClientTlsStrategy"],
    ),
    // assertj-core: `Assertions` and `AbstractAssert` are its core public API;
    // `SoftAssertions` and `JUnitJupiterSoftAssertions` are both real files
    // under org/assertj/core/api/, and the latter is one of the four
    // JUnit-Jupiter-integration files that the `--module-path` exception
    // exists for. If those four ever stop compiling, this entry fails on the
    // name rather than passing on a slightly smaller count.
    (
        "org.assertj__assertj-core-3.25.3",
        &["Assertions", "AbstractAssert", "SoftAssertions", "JUnitJupiterSoftAssertions"],
    ),
];

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

    // Still diagnostic, not a pass/fail gate over ALL 22. Five packages moved
    // into `known_good` this pass — h2, httpclient5, logback-classic,
    // junit-jupiter-api and assertj-core — bringing the total to 19/22. The
    // remaining 3 are each individually root-caused in their own `#[test]`
    // below and blocked on something outside this corpus's reach: a language
    // boundary (retrofit's Kotlin-only imports), a `javac` option
    // contradiction (lombok), and a `-sourcepath` type-lookup rule
    // (kafka-clients). `failures.is_empty()` would turn 3 honest,
    // per-package-diagnosed limitations into a red CI run for reasons that
    // have nothing to do with a regression in this producer.
    //
    // Every name below has been observed green through the real pipeline, and
    // this list is also the regression tripwire for the two changes this pass
    // made that have corpus-*wide* blast radius. Both are checked here by
    // name, which is the only reason they could be made at all:
    //
    //   * The modular invocation shape — `--module-source-path` instead of
    //     `-sourcepath` for any target carrying its own `module-info.java` —
    //     changes how `gson` and `jakarta.validation-api` compile even though
    //     neither needed fixing. Both were re-verified in isolation first,
    //     and both are asserted here.
    //   * The 21 new artifacts all join every *non*-modular target's
    //     `-sourcepath`, a package-name namespace shared by the whole corpus.
    //     Lucene alone adds ~1600 files under `org/apache/lucene/**` and
    //     `org/tartarus/**`. Nothing regressed, but nothing would have said so
    //     without this list.
    //
    // `jakarta.servlet-api` is deliberately provisioned twice, 5.0.0 and
    // 6.0.0, one for each mechanism (see `corpus/manifest.toml`). They cannot
    // collide: the 6.0.0 checkout carries a `module-info.java`, so it is
    // excluded from every `-sourcepath`, and h2 resolves `jakarta.servlet`
    // against exactly one directory, the 5.0.0 one.
    let known_good = [
        "com.google.code.gson__gson-2.10.1",
        "org.apache.commons__commons-lang3-3.14.0",
        "org.slf4j__slf4j-api-2.0.12",
        "org.mapstruct__mapstruct-1.5.5.Final",
        "jakarta.validation__jakarta.validation-api-3.0.2",
        "io.reactivex.rxjava3__rxjava-3.1.8",
        "com.google.guava__guava-33.0.0-jre",
        "com.google.guava__guava-32.1.3-jre",
        "junit__junit-4.13.2",
        "com.fasterxml.jackson.core__jackson-databind-2.16.1",
        "com.fasterxml.jackson.core__jackson-databind-2.15.4",
        "com.google.dagger__dagger-2.51",
        "io.vavr__vavr-0.10.4",
        "org.mockito__mockito-core-5.10.0",
        // Moved in this pass.
        "com.h2database__h2-2.2.224",
        "org.apache.httpcomponents.client5__httpclient5-5.3.1",
        "ch.qos.logback__logback-classic-1.4.14",
        "org.junit.jupiter__junit-jupiter-api-5.10.2",
        "org.assertj__assertj-core-3.25.3",
    ];
    for dir in known_good {
        assert!(
            successes.iter().any(|(d, _, _)| *d == dir),
            "{dir} was previously verified to resolve offline through this producer and must \
             keep succeeding; see failures list above for what broke it"
        );
    }
}

/// Looks up one `ENTRIES` row by directory name — shared by every pin test
/// below so each one exercises the exact same `Entry` (and `release`
/// setting) the main sweep above does, not a hand-rolled duplicate.
fn entry(dir: &str) -> &'static Entry {
    ENTRIES.iter().find(|e| e.dir == dir).unwrap_or_else(|| panic!("no ENTRIES row for {dir}"))
}

/// Asserts one entry lowered, and returns its lowered symbol names.
///
/// The five "this is how it got fixed" guards below all have the same shape:
/// the sweep above already proves the package lowers, so what each one adds is
/// a named, mechanism-specific reason it lowers — the thing that would
/// otherwise be reconstructed from scratch the next time it breaks.
fn expect_lowered(dir: &str) -> Vec<String> {
    match run_entry(entry(dir)) {
        Outcome::Ok { symbol_names, .. } => symbol_names,
        Outcome::Fail { stage, chain } => {
            panic!("{dir} no longer lowers, failed at [{stage}]: {chain}")
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Fixed this pass. Each guard names the mechanism, not just the outcome.
// ───────────────────────────────────────────────────────────────────────────

/// `com.h2database:h2:2.2.224` was pinned as "Lucene/OSGi/servlet closure too
/// large" (~5.9 MB). Download size is not a reason to skip a package, and the
/// closure turned out to be entirely ordinary once actually fetched: JTS,
/// three Lucene artifacts, both OSGi artifacts, and both servlet generations,
/// all sources jars, all `role = "dependency"` entries in
/// `corpus/manifest.toml`.
///
/// The part worth keeping is *where the versions came from*. h2's published
/// Maven POM declares **zero** dependencies — it is a shaded release POM, and
/// resolving against it produces exactly the "h2 has no dependencies" answer
/// that made this look unprovisionable. Every version here comes from h2's
/// own `h2/pom.xml` at GitHub tag `version-2.2.224` instead.
///
/// One real trap, and the reason this test asserts on
/// `JakartaWebServlet`/`WebServlet` specifically: h2 imports *both*
/// `jakarta.servlet.*` and `javax.servlet.*`, and the obvious
/// `jakarta.servlet-api:6.0.0` is the wrong artifact twice over. h2's pom
/// pins 5.0.0, and 6.0.0's sources jar carries a `module-info.java`, which
/// `JavaProducer::sourcepath_entries` excludes from every package's
/// `-sourcepath` — so provisioning 6.0.0 would have downloaded a real,
/// hash-verified artifact that could never resolve a single h2 import.
#[test]
fn h2_lowers_with_its_undeclared_lucene_osgi_and_dual_servlet_closure() {
    let names = expect_lowered("com.h2database__h2-2.2.224");
    for (name, why) in [
        ("FullTextLucene", "the Lucene closure"),
        ("OsgiDataSourceFactory", "the OSGi closure"),
        ("ValueGeometry", "the JTS closure"),
        ("WebServlet", "javax.servlet"),
        ("JakartaWebServlet", "jakarta.servlet 5.0.0"),
    ] {
        assert!(
            names.iter().any(|n| n == name),
            "h2 lowered without {name}, the file that proves {why} resolved — if h2 now \
             lowers with a smaller closure, shrink the manifest deliberately rather than \
             letting this rot"
        );
    }
}

/// `org.apache.httpcomponents.client5:httpclient5:5.3.1` was pinned as
/// blocked on `org.conscrypt`, on the grounds that conscrypt's published
/// sources jar is missing `NativeConstants.java` — a file conscrypt's native
/// build generates from C headers and never checks in. That is true of
/// `org.conscrypt:conscrypt-openjdk-uber`, which is the artifact the earlier
/// pass checked. It is **not** true of plain
/// `org.conscrypt:conscrypt-openjdk`, whose sources jar ships
/// `org/conscrypt/NativeConstants.java`: 134 `.java` entries against the uber
/// jar's 133, and that one file is the entire difference.
///
/// A second, independent blocker sat behind it, which is why this test
/// asserts a flag as well as a symbol. Conscrypt's `Platform.java` imports
/// `sun.security.x509.AlgorithmId`; conscrypt's own Gradle build compiles at
/// `sourceCompatibility 1.7`, i.e. before the module system enforced exports,
/// so it never has to say so. No Maven artifact can supply a package inside
/// `java.base`, and `--release 8` makes it strictly worse — `ct.sym` removes
/// `sun.security.x509` from the visible platform entirely rather than merely
/// leaving it unexported. `--add-exports java.base/sun.security.x509=ALL-UNNAMED`,
/// scoped to this entry via `Entry::add_exports`, is the only thing that
/// works.
///
/// `org.brotli:dec` was also missing (declared `<optional>true</optional>` in
/// httpclient5's POM) and is now provisioned.
#[test]
fn httpclient5_resolves_conscrypt_from_the_non_uber_sources_jar() {
    let e = entry("org.apache.httpcomponents.client5__httpclient5-5.3.1");
    assert_eq!(
        e.add_exports,
        Some("java.base/sun.security.x509"),
        "httpclient5's conscrypt dependency needs this export; dropping it silently \
         re-breaks the package"
    );

    let names = expect_lowered(e.dir);
    assert!(
        names.iter().any(|n| n == "ConscryptClientTlsStrategy"),
        "httpclient5 lowered without ConscryptClientTlsStrategy — the one file whose \
         org.conscrypt.Conscrypt import this entry's whole provisioning exists for"
    );
}

/// `ch.qos.logback:logback-classic:1.4.14` was pinned on `org.slf4j` being a
/// JPMS module with no `module-info.java` in its sources jar. That was the
/// visible blocker, not the whole one: its `module-info.java` also requires
/// `ch.qos.logback.core`, `jakarta.servlet` and `jakarta.mail`, and
/// `requires static` is optional only at *runtime* — at compile time every
/// one of them has to resolve or `javac` stops before reading a single
/// ordinary source file.
///
/// Three of those four are satisfied from source, through
/// `--module-source-path`: `jakarta.mail-api` and its own transitive
/// `jakarta.activation-api`, plus `jakarta.servlet-api:6.0.0` (the lowest
/// version whose *sources* jar carries a `module-info.java`; h2's 5.0.0 does
/// not, which is why both are provisioned).
///
/// `ch.qos.logback.core` cannot be, and the reason is the interesting one.
/// logback-core's sources jar *does* carry a real `module-info.java` — and it
/// says `requires static janino;` and `requires static commons.compiler;`.
/// Those are **automatic** module names, synthesised by `javac` from jar
/// filenames, so no source artifact can ever satisfy them; compiling
/// logback-core from source fails with `module not found: janino` regardless
/// of what else is provisioned (reproduced directly before reaching for the
/// jar). Resolving logback-core as a *compiled* module does not follow its
/// `requires static` edges at all, and logback-classic then compiles clean.
/// So logback-core and slf4j-api are `[[jpms_modules]]` entries — compiled,
/// unextracted, `--module-path`-only.
///
/// Falsification trigger: slf4j-api publishing a sources `module-info.java`,
/// or logback-core dropping its automatic-module `requires`, would let both
/// move back to source.
#[test]
fn logback_classic_lowers_via_module_source_path_plus_two_compiled_descriptors() {
    let names = expect_lowered("ch.qos.logback__logback-classic-1.4.14");
    assert!(
        names.iter().any(|n| n == "Logger") && names.iter().any(|n| n == "LoggerContext"),
        "logback-classic lowered without Logger/LoggerContext"
    );

    let module_path = corpus_root().join(".module-path");
    for jar in ["slf4j-api-2.0.12.jar", "logback-core-1.4.14.jar"] {
        assert!(
            module_path.join(jar).is_file(),
            "{jar} missing from {} — logback-classic resolves org.slf4j and \
             ch.qos.logback.core through it and nothing else can; re-run `nu corpus/fetch.nu`",
            module_path.display()
        );
    }
    assert!(
        !module_path.join("slf4j-api-2.0.12.jar").is_dir(),
        "a [[jpms_modules]] entry must stay an unextracted jar — if it is ever a directory \
         of sources, it has become reachable from -sourcepath and the exception has leaked"
    );
}

/// `org.junit.jupiter:junit-jupiter-api:5.10.2` was pinned on
/// `org.opentest4j`'s sources jar carrying no `module-info.java`. That much
/// was right; what the pin also claimed — that `org.apiguardian:apiguardian-api`
/// and `org.junit.platform:junit-platform-commons` were "both now corpus
/// entries" — was not: neither was in `corpus/manifest.toml`, and all three
/// modules were unresolved. Both are entries now, and both stay **source**:
/// their sources jars do carry real `module-info.java` files, so
/// `--module-source-path` resolves them and only `opentest4j` needs a
/// compiled descriptor.
///
/// That 2-of-3-from-source split is the point of this test. It is what keeps
/// the compiled-jar exception at "the descriptor genuinely does not exist as
/// source" rather than "modules are easier as jars".
///
/// Falsification trigger: opentest4j publishing a `module-info.java` in its
/// sources classifier would let its `[[jpms_modules]]` entry be deleted
/// outright.
#[test]
fn junit_jupiter_api_lowers_with_two_source_modules_and_one_compiled_one() {
    let names = expect_lowered("org.junit.jupiter__junit-jupiter-api-5.10.2");
    assert!(
        names.iter().any(|n| n == "Assertions"),
        "junit-jupiter-api lowered without Assertions"
    );

    let root = corpus_root();
    for dir in [
        "org.apiguardian__apiguardian-api-1.1.2",
        "org.junit.platform__junit-platform-commons-1.10.2",
    ] {
        assert!(
            root.join(dir).join("module-info.java").is_file(),
            "{dir} must be a SOURCE module (its sources jar carries module-info.java) — if it \
             ever needs the compiled jar instead, that is a real widening of the \
             [[jpms_modules]] exception and belongs in the manifest's rationale"
        );
    }
    assert!(
        root.join(".module-path").join("opentest4j-1.3.0.jar").is_file(),
        "opentest4j's compiled descriptor is the one junit-jupiter-api cannot get from source"
    );
}

/// `org.assertj:assertj-core:3.25.3` was pinned as sharing junit-jupiter-api's
/// blocker with "no independent fix". Fixing junit-jupiter-api turned out
/// *not* to fix it, because assertj-core's problem is a different one that
/// happens to involve the same modules.
///
/// assertj-core has no `module-info.java` of its own, so it compiles into the
/// unnamed module and needs `-sourcepath` for byte-buddy, hamcrest and
/// opentest4j — 778 of its 782 files resolve that way. The other 4
/// (`JUnitJupiterBDDSoftAssertions`, `JUnitJupiterSoftAssertions`,
/// `SoftAssertionsExtension`, `SoftlyExtension`) import
/// `org.junit.jupiter.api.extension` and `org.junit.platform.commons.support`,
/// which live in module-bearing checkouts that `-sourcepath` deliberately
/// excludes. `--module-source-path` is not an alternative: `javac` rejects it
/// in the same invocation as `-sourcepath` ("cannot specify both
/// --source-path and --module-source-path"), so a non-modular target can
/// never resolve a modular dependency from source. Two further attempts
/// failed for recorded reasons — putting junit-jupiter-api's *source*
/// checkout on `-sourcepath` makes `javac` enter its `module-info.java` and
/// demand the modules anyway (`cannot access module-info`), and naming those
/// modules explicitly in `--add-modules` collapses the default root set, so
/// assertj then cannot see `java.logging`/`java.lang.management`/`javax.xml.parsers`.
///
/// What works is `--module-path` plus `--add-modules ALL-MODULE-PATH`, with
/// compiled descriptors for the whole JUnit chain. This is the widest point
/// of the compiled-jar exception and the only place in the corpus where a
/// package's own source resolves *dependency* types out of bytecode: those 4
/// files' JUnit types come from jars. assertj-core's own 782 files are still
/// lowered from source alone, and junit-jupiter-api still lowers from its own
/// sources, so no bytecode reaches the IR.
///
/// Falsification trigger: `javac` ever permitting `-sourcepath` alongside
/// `--module-source-path`, or assertj-core splitting its JUnit-Jupiter
/// integration into a separate artifact, would remove the need entirely.
#[test]
fn assertj_core_reads_the_junit_chain_off_the_module_path() {
    let e = entry("org.assertj__assertj-core-3.25.3");
    assert_eq!(
        e.add_modules,
        Some("ALL-MODULE-PATH"),
        "without --add-modules nothing on --module-path enters the unnamed module's root \
         set, and assertj-core's 4 JUnit-Jupiter files stop resolving"
    );

    let names = expect_lowered(e.dir);
    for name in ["Assertions", "JUnitJupiterSoftAssertions", "SoftAssertionsExtension"] {
        assert!(
            names.iter().any(|n| n == name),
            "assertj-core lowered without {name} — the JUnit-Jupiter-integration files are \
             exactly what the module-path exception exists for, so their absence is a \
             silent regression, not a smaller success"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Still pinned. Three packages, three different walls.
// ───────────────────────────────────────────────────────────────────────────

/// `com.squareup.retrofit2:retrofit:2.9.0` — pinned on a **language**
/// boundary, not a missing artifact and not (as previously recorded) Android.
///
/// Three of retrofit's own main-source files import Kotlin-only types:
/// `BuiltInConverters.java` imports `kotlin.Unit`, and `HttpServiceMethod.java`
/// and `RequestFactory.java` import `kotlin.coroutines.Continuation`. The only
/// artifact that publishes those types is `org.jetbrains.kotlin:kotlin-stdlib`,
/// whose sources jar is Kotlin: `kotlin/Unit.kt`, not `kotlin/Unit.java` (142
/// `.kt` files against 28 `.java`, checked with `unzip -l`). This producer's
/// oracle is `javadoc`, which cannot parse `.kt` under any flag. There is no
/// arrangement of corpus entries that fixes this — the blocker is that a Java
/// library's public API depends on Kotlin declarations.
///
/// Everything in front of that was verified rather than assumed, because the
/// previous pin named the wrong cause. Compiling all of retrofit against a
/// scratch sourcepath containing okhttp 3.14.9 and okio 1.17.2 (both pure
/// Java at these versions — okhttp only becomes Kotlin at 4.x, so the earlier
/// "okhttp is unobtainable" reasoning does not apply) plus the whole corpus
/// leaves exactly four unresolved things: the Kotlin imports above,
/// `org.codehaus.mojo.animal_sniffer` (trivially obtainable),
/// `sun.security.x509` in okhttp's `Platform.java` (solvable with the same
/// `--add-exports` httpclient5 uses), and `android.os`/`android.util` in
/// okhttp's `AndroidPlatform.java`/`Android10Platform.java`. Only the Kotlin
/// one is unfixable — which is why okhttp, okio and animal-sniffer are **not**
/// provisioned: they would add three artifacts to every package's
/// `-sourcepath` and still leave retrofit red.
///
/// On the Android question specifically, since it is the reason previously
/// recorded: `com.google.android:android` tops out at 4.1.1.4 (API 16, 2012),
/// and its sources jar is a machine-generated stub whose every method body is
/// `throw new RuntimeException("Stub!")`. Shipping it would put fabricated
/// implementations in the corpus to satisfy a package that would stay red
/// anyway — the thing AGENTS-DOCTRINE.md §6 exists to prevent, even though
/// the bytes are genuinely published. It is the wrong call independently of
/// whether it would work.
///
/// Falsification trigger: retrofit dropping its Kotlin imports (retrofit 3.x
/// went the other way and is now Kotlin-first), or this producer gaining a
/// Kotlin front end.
#[test]
fn retrofit_blocked_on_kotlin_only_types_in_its_own_source() {
    let root = corpus_root().join("com.squareup.retrofit2__retrofit-2.9.0");
    for (file, import) in [
        ("retrofit2/BuiltInConverters.java", "import kotlin.Unit;"),
        ("retrofit2/HttpServiceMethod.java", "import kotlin.coroutines.Continuation;"),
        ("retrofit2/RequestFactory.java", "import kotlin.coroutines.Continuation;"),
    ] {
        let text = std::fs::read_to_string(root.join(file))
            .unwrap_or_else(|e| panic!("read {file}: {e}"));
        assert!(
            text.contains(import),
            "{file} no longer has `{import}` — if retrofit dropped its Kotlin dependency, \
             provision okhttp/okio/animal-sniffer and move it into known_good instead of \
             leaving this test to rot"
        );
    }

    match run_entry(entry("com.squareup.retrofit2__retrofit-2.9.0")) {
        Outcome::Fail { chain, .. } => {
            assert!(
                chain.contains("okhttp3") || chain.contains("kotlin"),
                "expected retrofit's known okhttp3/kotlin blocker, got: {chain}"
            );
        }
        Outcome::Ok { .. } => panic!(
            "retrofit now lowers — move it into known_good in this file instead of leaving \
             this test to rot"
        ),
    }
}

/// `org.projectlombok:lombok:1.18.30` — pinned on two independent structural
/// facts, either of which alone is fatal. Neither is "the closure is large",
/// which is how this was previously recorded.
///
/// **1. A `javac` option contradiction with no way out.** `lombok/var.java`
/// and `lombok/experimental/var.java` both declare `public @interface var`,
/// which is illegal from Java 10 on (`'var' not allowed here`), so lombok
/// only parses at `--release 8` or `9`. But 503 of its errors at that level
/// are `com.sun.tools.javac.*` — JDK-internal compiler API, part of the
/// `jdk.compiler` module and not exported. The one flag that could make those
/// visible is `--add-exports`, and `javac` refuses to accept it together with
/// `--release`, in two different ways depending on the level:
///
/// ```text
/// --release 8: error: option --add-exports not allowed with target 8
/// --release 9: error: exporting a package from system module jdk.compiler
///                     is not allowed with --release
/// ```
///
/// Both messages were reproduced directly. The flag lombok needs to parse and
/// the flag lombok needs to resolve are mutually exclusive, so no combination
/// of corpus entries and compiler options can lower this package.
///
/// **2. lombok's published sources jar is incomplete.** 92 of its 363 source
/// files `import lombok.spi.Provides`, and `lombok/spi/` does not exist
/// anywhere in `lombok-1.18.30-sources.jar` (checked with `unzip -l`: zero
/// entries under `spi/`). It is generated by lombok's own bootstrap build. No
/// Maven artifact can supply a package inside lombok's own namespace that
/// lombok itself does not publish.
///
/// The rest of the unmasked closure — `org.eclipse.jdt.internal.compiler.*`,
/// `org.eclipse.core.*`, `com.zwitserloot.cmdreader`, `lombok.patcher`,
/// `org.objectweb.asm`, `org.apache.tools.ant` — is real but beside the
/// point: those are ordinary artifacts that could be provisioned if the two
/// facts above did not already close the door.
///
/// Falsification trigger: lombok dropping its `@interface var` (it is
/// deprecated in favour of the language feature), or publishing `lombok.spi`
/// in its sources jar. Both would have to happen.
#[test]
fn lombok_blocked_on_a_release_flag_contradiction_and_an_incomplete_sources_jar() {
    let root = corpus_root().join("org.projectlombok__lombok-1.18.30");

    // Fact 2 first, because it is a property of the checkout rather than of a
    // compile: `lombok.spi` is imported and absent.
    assert!(
        !root.join("lombok").join("spi").exists(),
        "lombok/spi/ now exists in the sources jar — one of the two pins is gone; re-check \
         the other before assuming lombok is still blocked"
    );

    let src = PackageSource::new(&root, "org.projectlombok:lombok", "1.18.30");

    // Fact 1, half one: at the default release, only the parse error shows.
    // Each half takes `ENV_LOCK` in its own block — the mutex is not
    // reentrant, so holding both guards at once would deadlock against this
    // very test.
    let default_chain = {
        let _guard = scoped_release(None);
        let default_err = JavaProducer::new().invoke(&src).expect_err(
            "lombok should still fail at the default --release — if it now lowers cleanly, \
             move it into known_good in this file instead of leaving this test to rot",
        );
        chain(&default_err)
    };
    assert!(
        default_chain.contains("'var' not allowed here"),
        "expected the known default-release 'var' parse error, got: {default_chain}"
    );

    // Fact 1, half two: unmask it and the JDK-internal compiler API appears —
    // the thing `--add-exports` cannot be combined with `--release` to reach.
    let _release_guard = scoped_release(Some("8"));
    let unmasked_err = JavaProducer::new().invoke(&src).expect_err(
        "lombok should still fail at --release 8 — if it now lowers cleanly, this pin is \
         stale: set Entry::release to Some(\"8\") and move lombok into known_good",
    );
    let unmasked_chain = chain(&unmasked_err);
    assert!(
        unmasked_chain.contains("com.sun.tools.javac")
            || unmasked_chain.contains("lombok.spi")
            || unmasked_chain.contains("org.eclipse.jdt.internal.compiler"),
        "expected the known JDK-internal / lombok.spi / Eclipse closure once unmasked, got: \
         {unmasked_chain}"
    );
}

/// `org.apache.kafka:kafka-clients:3.7.0` — pinned on a `javac` `-sourcepath`
/// lookup rule, after its actual dependency closure was provisioned and did
/// resolve. This is a much smaller and much more specific failure than the
/// "closure too large and undeclared" it replaces, and it is not kafka's
/// fault at all.
///
/// Kafka's closure was the least discoverable in this corpus and is now fully
/// provisioned: zstd-jni, lz4-java, snappy-java, jose4j, opentelemetry-proto
/// and its own protobuf-java, plus Jackson. Kafka's POM declares 4
/// runtime-scope dependencies and omits jose4j and opentelemetry-proto
/// entirely; the versions come from `gradle/dependencies.gradle` at GitHub tag
/// `3.7.0`. **Maintenance note, and the real cost of this entry:** a version
/// bump must re-derive that list from the new tag's Gradle files plus a fresh
/// import grep. A POM-only check will miss half of it. See
/// `corpus/manifest.toml`'s kafka section header.
///
/// What remains is one error, and it is inside a *dependency*:
///
/// ```text
/// jackson-databind/.../util/internal/PrivateMaxEntriesMap.java:861:
///   error: cannot find symbol: class Linked
/// ```
///
/// `Linked` is a package-private **secondary** top-level type — declared
/// inside `LinkedDeque.java`, not in a `Linked.java` of its own. `javac`
/// resolves types found via `-sourcepath` by filename, so it looks for
/// `internal/Linked.java`, does not find it, and fails. Within
/// jackson-databind's own compile this never happens, because every file is
/// on the command line; from any *other* package it is unreachable, and
/// `ObjectMapper`'s attribution transitively requires it (`ObjectMapper` →
/// `TypeFactory` → `LRUMap` → `PrivateMaxEntriesMap`).
///
/// This is not a duplicate-version artifact of jackson-databind being
/// provisioned at both 2.15.4 and 2.16.1 — it reproduces with a single
/// version on the sourcepath, checked directly. It is a general property:
/// *any* corpus package that reaches `ObjectMapper` through `-sourcepath`
/// hits it. Kafka is currently the only one that does.
///
/// Deliberately not worked around. Passing jackson's sources on the command
/// line would make them specified elements and lower jackson's symbols into
/// kafka's table; putting jackson-databind on `--module-path` would use a
/// compiled jar for something that is not a JPMS descriptor gap, which is
/// exactly the boundary `[[jpms_modules]]` draws.
///
/// Falsification trigger: jackson-databind moving `Linked` into its own file,
/// or this producer gaining a way to hand `javac` a dependency's full source
/// set without those files becoming lowering input.
#[test]
fn kafka_clients_blocked_on_a_secondary_type_javac_cannot_find_on_sourcepath() {
    match run_entry(entry("org.apache.kafka__kafka-clients-3.7.0")) {
        Outcome::Fail { chain, .. } => {
            assert!(
                chain.contains("Linked") && chain.contains("PrivateMaxEntriesMap"),
                "expected the known jackson-databind secondary-type lookup failure. Anything \
                 else means kafka's closure regressed — the provisioned artifacts are \
                 zstd-jni, lz4-java, snappy-java, jose4j, opentelemetry-proto and \
                 protobuf-java, and a version bump must re-derive them from Kafka's Gradle \
                 files, never its POM. Got: {chain}"
            );
        }
        Outcome::Ok { .. } => panic!(
            "kafka-clients now lowers — if jackson-databind moved `Linked` into its own \
             file, add a KNOWN_CONTENT row and move kafka into known_good instead of \
             leaving this test to rot"
        ),
    }
}
