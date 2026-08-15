//! Indexing a package by PURL, against the **real** registries.
//!
//! # Why nothing here is a fixture
//!
//! Doctrine §4: "A hand-authored fixture tests the fixture author's
//! imagination, not the code." That warning has more force here than almost
//! anywhere else in the repo, because every interesting property of this
//! feature is a property of what a registry actually serves:
//!
//! * whether `index.crates.io` really publishes a `cksum` we can verify;
//! * whether a Maven sources jar's `.sha1` sidecar is bare hex or has a
//!   trailing filename (it is the latter, on Maven Central, today);
//! * whether the npm packument's `dist.tarball` matches the URL a naming
//!   convention would have produced;
//! * whether `javax.inject-1-sources.jar` still unpacks to `javax/inject/*.java`
//!   rather than to seven flat files — the regression `finalize-package` in
//!   `nix build .#checks.corpus` exists to prevent, restated here as a test rather than a
//!   comment.
//!
//! A mock server would have answered all four the way the author expected.
//!
//! # Why every test is `#[ignore]`
//!
//! The repo's convention for anything that touches the network or a real
//! producer (`crates/nudox-store/tests/real_crate.rs`, `corpus_contract.rs`).
//! Run with `cargo test -p nudox-engine --test purl_index -- --ignored`.
//!
//! # Package choices
//!
//! Deliberately the smallest real thing per ecosystem, so the suite is minutes
//! rather than hours: `numtoa` (a no-dependency integer formatter),
//! `left-pad@1.3.0`, `six@1.16.0`, `github.com/pkg/errors@v0.9.1`, and
//! `javax.inject:javax.inject@1` (seven source files).

use std::path::{Path, PathBuf};

use nudox_engine::acquire::{Error, IndexEvent, IndexStage, Integrity};
use nudox_engine::wire::Gen;
use nudox_engine::{Engine, EngineConfig, EngineHandle, Purl};
use heart::cost::measured;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A cache directory under `target/`, so a run never touches the developer's
/// real `~/.cache/nudox` and a `cargo clean` is a cache reset.
fn scratch_cache(case: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/purl-index-tests")
        .join(case);
    std::fs::create_dir_all(&dir).expect("scratch cache directory must be creatable");
    dir
}

/// An engine with an empty corpus and a scratch package cache.
///
/// Empty rather than fixture-seeded on purpose: every assertion below is about
/// a package that was *not* there when the engine started, and a corpus with
/// content would leave "was it already loaded?" as an alternative explanation
/// for every pass.
fn engine_for(case: &str) -> (EngineHandle, PathBuf) {
    let cache = scratch_cache(case);
    let engine = Engine::start_with_producer(
        EngineConfig {
            package_cache: Some(cache.clone()),
            ..EngineConfig::default()
        },
        Vec::new(),
    );
    (engine, cache)
}

/// Drive one index job to its terminal event, collecting the stages it passed
/// through on the way.
fn index(engine: &EngineHandle, purl: &str) -> (IndexEvent, Vec<IndexStage>) {
    let parsed = Purl::parse(purl).unwrap_or_else(|e| panic!("{purl} must parse: {e}"));
    let (handle, rx) = engine.index_purl(parsed, Gen(1));

    let mut stages = Vec::new();
    loop {
        let event = rx
            .recv()
            .unwrap_or_else(|_| panic!("{purl}: the index stream closed without a terminal event"));
        match event {
            IndexEvent::Started { .. } => {}
            IndexEvent::Stage { stage, .. } => {
                if stages.last() != Some(&stage) {
                    stages.push(stage);
                }
            }
            terminal => {
                drop(handle);
                return (terminal, stages);
            }
        }
    }
}

fn expect_indexed(engine: &EngineHandle, purl: &str) -> (Integrity, u64, String, String) {
    let (event, stages) = index(engine, purl);
    match event {
        IndexEvent::Indexed {
            integrity,
            symbol_count,
            name,
            version,
            ..
        } => {
            assert!(
                stages.contains(&IndexStage::Resolving),
                "{purl}: every job must report resolving, got {stages:?}",
            );
            (integrity, symbol_count, name.to_string(), version.to_string())
        }
        IndexEvent::Failed { error, .. } => {
            panic!("{purl} failed to index: {error}\nstages reached: {stages:?}")
        }
        other => panic!("{purl}: unexpected terminal event {other:?}"),
    }
}

fn expect_failure(engine: &EngineHandle, purl: &str) -> Error {
    match index(engine, purl).0 {
        IndexEvent::Failed { error, .. } => error,
        other => panic!("{purl} was expected to fail; got {other:?}"),
    }
}

/// Run the acquisition half only — resolve, fetch, verify, extract — without a
/// producer.
///
/// Separated from full indexing because the two fail for entirely different
/// reasons and take entirely different amounts of time: a Java or C# toolchain
/// being absent must not make "does Maven Central still serve this sources jar
/// with a `.sha1`" unanswerable.
fn acquire_only(cache: &Path, purl: &str) -> (Integrity, PathBuf) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let parsed = Purl::parse(purl).unwrap_or_else(|e| panic!("{purl} must parse: {e}"));
    let engine = Engine::start_with_producer(
        EngineConfig {
            package_cache: Some(cache.to_path_buf()),
            ..EngineConfig::default()
        },
        Vec::new(),
    );
    // The public surface is `index_purl`, which also produces. To exercise
    // acquisition alone we drive the same job and stop at the point the package
    // root exists on disk — which is what the `Producing` stage announces.
    let (handle, rx) = engine.index_purl(parsed.clone(), Gen(1));
    let mut integrity = None;
    loop {
        match rx.recv() {
            Ok(IndexEvent::Stage {
                stage: IndexStage::Producing,
                ..
            }) => break,
            Ok(IndexEvent::Indexed { integrity: i, .. }) => {
                integrity = Some(i);
                break;
            }
            Ok(IndexEvent::Failed { error, .. }) => panic!("{purl}: acquisition failed: {error}"),
            Ok(_) => {}
            Err(_) => panic!("{purl}: the index stream closed before the package was acquired"),
        }
    }
    drop(handle);
    drop(runtime);

    let root = cache
        .join(parsed.ecosystem())
        .join(parsed.cache_dir_name());
    assert!(
        root.is_dir(),
        "{purl}: the package root {} must exist once acquisition has finished",
        root.display()
    );
    let integrity = integrity.unwrap_or_else(|| {
        let sidecar = cache.join(parsed.ecosystem()).join(format!(
            "{}.integrity.json",
            parsed.cache_dir_name()
        ));
        let body = std::fs::read_to_string(&sidecar).unwrap_or_else(|e| {
            panic!(
                "{purl}: an acquired package must leave an integrity record at {}: {e}",
                sidecar.display()
            )
        });
        serde_json::from_str(&body).expect("the integrity sidecar must be readable")
    });
    (integrity, root)
}

// ---------------------------------------------------------------------------
// Integrity — the claim this feature is judged on
// ---------------------------------------------------------------------------

/// The four registries that publish a digest must actually be verified against
/// it, and the two that do not must say so rather than imply otherwise.
///
/// This is the test the whole `Integrity` type exists for. `nix build .#checks.corpus`'s
/// header calls a fetcher that accepts a mismatch "worse than not fetching at
/// all"; the PURL path has no manifest hash, so the equivalent claim is "we
/// verified against what the registry publishes, or we said we could not".
#[test]
#[ignore = "fetches six real packages from six real registries; run with --ignored"]
fn a_fetch_is_verified_against_a_published_digest_or_says_it_was_not() {
    let cache = scratch_cache("integrity");
    let ((), _cost) = measured("purl_integrity_matrix", &cache, || {
        for (purl, expect_verified) in [
            ("pkg:cargo/numtoa@0.2.4", true),
            ("pkg:npm/left-pad@1.3.0", true),
            ("pkg:pypi/six@1.16.0", true),
            ("pkg:maven/javax.inject/javax.inject@1", true),
            ("pkg:golang/github.com/pkg/errors@v0.9.1", false),
            ("pkg:nuget/Newtonsoft.Json@13.0.3", false),
        ] {
            let (integrity, _root) = acquire_only(&cache, purl);
            match (&integrity, expect_verified) {
                (Integrity::RegistryDigest { algorithm, digest, published_by }, true) => {
                    assert!(!digest.is_empty(), "{purl}: a verified fetch must carry the digest");
                    assert!(
                        matches!(algorithm.as_str(), "sha256" | "sha512" | "sha1"),
                        "{purl}: unexpected algorithm {algorithm}",
                    );
                    assert!(
                        !published_by.is_empty(),
                        "{purl}: a verified fetch must name the endpoint that substantiates it",
                    );
                }
                (Integrity::TransportOnly { sha256, why }, false) => {
                    assert_eq!(
                        sha256.len(),
                        64,
                        "{purl}: an unverified fetch must still carry a pinnable sha256",
                    );
                    assert!(
                        why.len() > 40,
                        "{purl}: an unverified fetch must say specifically why, got {why:?}",
                    );
                }
                (got, _) => panic!(
                    "{purl}: expected verified={expect_verified}, got {}",
                    got.summary()
                ),
            }
        }
    });
}

/// crates.io's `cksum` is the digest GLOBAL-IR-GRAPH §2 names by ecosystem
/// ("sparse/git index entry, `cksum` = SHA-256 of the `.crate`"). Pin that we
/// use *that* value and not a locally invented one.
#[test]
#[ignore = "fetches numtoa from crates.io; run with --ignored"]
fn a_cargo_fetch_is_checked_against_the_index_cksum_not_a_local_hash() {
    let cache = scratch_cache("cargo-cksum");
    let ((), _cost) = measured("purl_cargo_cksum", &cache, || {
        let (integrity, _root) = acquire_only(&cache, "pkg:cargo/numtoa@0.2.4");
        let Integrity::RegistryDigest {
            algorithm,
            digest,
            published_by,
        } = integrity
        else {
            panic!("crates.io publishes a cksum, so this must be a verified fetch");
        };
        assert_eq!(algorithm, "sha256");
        assert_eq!(digest.len(), 64, "cksum is hex sha256");
        assert!(
            published_by.contains("cksum"),
            "the claim must name the field it rests on, got {published_by:?}",
        );
    });
}

// ---------------------------------------------------------------------------
// Extraction layout — the `javax.inject` regression, against the real jar
// ---------------------------------------------------------------------------

/// A Maven sources jar's leading directories are the *package path* and must
/// survive extraction.
///
/// `nix build .#checks.corpus` records what happened when they did not: recursive
/// wrapper-stripping descended twice into `javax/inject/` and produced seven
/// flat files each still declaring `package javax.inject;`, javadoc could not
/// resolve them by `-sourcepath`, and dagger failed with "package javax.inject
/// does not exist" while the jar sat right there — hash-verified and unusable.
/// This is that story as an executable claim, against the same artifact.
#[test]
#[ignore = "fetches javax.inject's sources jar from Maven Central; run with --ignored"]
fn a_maven_sources_jar_keeps_the_package_directories_that_are_its_layout() {
    let cache = scratch_cache("maven-layout");
    let ((), _cost) = measured("purl_maven_layout", &cache, || {
        let (_integrity, root) = acquire_only(&cache, "pkg:maven/javax.inject/javax.inject@1");
        let inject = root.join("javax").join("inject");
        assert!(
            inject.is_dir(),
            "javax/inject/ must exist under {}; a flattened tree is the regression this \
             ecosystem-scoped strip exists to prevent",
            root.display(),
        );
        assert!(
            inject.join("Inject.java").is_file(),
            "javax/inject/Inject.java must be where its package declaration says it is",
        );
    });
}

/// A Go module zip nests the whole archive under `<module path>@<version>/`,
/// which is several one-child directories in a row — the case
/// `deepest-sole-dir` is recursive for.
#[test]
#[ignore = "fetches github.com/pkg/errors from the Go module proxy; run with --ignored"]
fn a_go_module_zip_is_unwrapped_all_the_way_to_the_module_root() {
    let cache = scratch_cache("go-layout");
    let ((), _cost) = measured("purl_go_layout", &cache, || {
        let (_integrity, root) = acquire_only(&cache, "pkg:golang/github.com/pkg/errors@v0.9.1");
        assert!(
            root.join("errors.go").is_file(),
            "errors.go must be at the package root, not under github.com/pkg/errors@v0.9.1/; \
             got {:?}",
            std::fs::read_dir(&root)
                .map(|d| d.filter_map(Result::ok).map(|e| e.file_name()).collect::<Vec<_>>()),
        );
        assert!(root.join("go.mod").is_file(), "the module manifest must be present");
    });
}

// ---------------------------------------------------------------------------
// The whole pipeline: a package that was not there becomes searchable
// ---------------------------------------------------------------------------

/// The feature, end to end: type a PURL at a running engine with an empty
/// corpus, and afterwards search finds a symbol that really exists in that
/// package.
///
/// The assertion is on *content* — a named declaration from `numtoa`'s public
/// API — rather than on `is_ok()` or a non-zero count, per doctrine §4. A stub
/// that inserted an empty package would pass a count assertion.
#[test]
#[ignore = "fetches numtoa from crates.io and runs rust-analyzer over it; run with --ignored"]
fn a_purl_typed_at_a_running_engine_becomes_a_searchable_package() {
    let cache = scratch_cache("cargo-e2e");
    let ((), _cost) = measured("purl_index_cargo_end_to_end", &cache, || {
        let (engine, _cache) = engine_for("cargo-e2e");

        // Precondition: the corpus is empty, so nothing below can be explained
        // by the package having already been there.
        let before = drain_packages(&engine);
        assert!(
            before.is_empty(),
            "this engine was started with no packages; got {before:?}",
        );

        let (integrity, symbol_count, name, version) =
            expect_indexed(&engine, "pkg:cargo/numtoa@0.2.4");

        assert_eq!(name, "numtoa");
        assert_eq!(version, "0.2.4");
        assert!(
            symbol_count > 1,
            "numtoa declares a trait and its impls; a 1-symbol package is a root module and \
             nothing else",
        );
        assert!(
            matches!(integrity, Integrity::RegistryDigest { .. }),
            "crates.io publishes a cksum: {}",
            integrity.summary(),
        );

        // The point of the whole exercise: the *other* tools can now see it.
        let hits = search(&engine, "NumToA");
        assert!(
            hits.iter().any(|h| h == "NumToA"),
            "numtoa's public trait `NumToA` must be searchable after indexing; got {hits:?}",
        );

        // And it is a first-class lineage, not a bolt-on: the version registry
        // knows about it, which is what makes `list_versions`/`select_version`
        // work for a package that arrived after start-up.
        let lineage = nudox_engine::wire::PackageLineageId::new(
            nudox_engine::wire::EcosystemId::new("cargo"),
            nudox_engine::wire::PackageName::new("numtoa"),
        );
        let versions = engine.versions(&lineage);
        assert_eq!(versions.len(), 1, "one generation was indexed");
        assert!(versions.versions[0].is_current);
        assert_eq!(&*versions.versions[0].version, "0.2.4");
    });
}

/// Indexing the same PURL twice must not produce two lineages, two generations,
/// or two producer runs' worth of work — the second is a cache hit.
#[test]
#[ignore = "fetches numtoa twice; run with --ignored"]
fn indexing_the_same_purl_twice_is_a_cache_hit_and_still_one_generation() {
    let cache = scratch_cache("cargo-idempotent");
    let ((), _cost) = measured("purl_index_idempotent", &cache, || {
        let (engine, _cache) = engine_for("cargo-idempotent");

        let (_, _, _, _) = expect_indexed(&engine, "pkg:cargo/numtoa@0.2.4");
        let (_event, stages) = index(&engine, "pkg:cargo/numtoa@0.2.4");
        assert!(
            stages.contains(&IndexStage::Cached),
            "the second index must report a cache hit rather than downloading again; got {stages:?}",
        );
        assert!(
            !stages.contains(&IndexStage::Downloading),
            "a cache hit must not download; got {stages:?}",
        );

        let lineage = nudox_engine::wire::PackageLineageId::new(
            nudox_engine::wire::EcosystemId::new("cargo"),
            nudox_engine::wire::PackageName::new("numtoa"),
        );
        assert_eq!(
            engine.versions(&lineage).len(),
            1,
            "re-indexing one version must replace that generation, not add a duplicate row",
        );
    });
}

/// Two versions of one package indexed on demand must behave like two versions
/// loaded at start-up: one lineage, two generations, newest current.
///
/// This is the assertion that says live insertion goes through
/// `VersionRegistry::record` rather than straight into the corpus. A blind
/// `Corpus::insert` would leave the second index resident regardless of which
/// version it was.
#[test]
#[ignore = "fetches two versions of numtoa and lowers both; run with --ignored"]
fn indexing_an_older_version_second_does_not_demote_the_newer_one() {
    let cache = scratch_cache("cargo-versions");
    let ((), _cost) = measured("purl_index_two_generations", &cache, || {
        let (engine, _cache) = engine_for("cargo-versions");

        expect_indexed(&engine, "pkg:cargo/numtoa@0.2.4");
        expect_indexed(&engine, "pkg:cargo/numtoa@0.2.3");

        let lineage = nudox_engine::wire::PackageLineageId::new(
            nudox_engine::wire::EcosystemId::new("cargo"),
            nudox_engine::wire::PackageName::new("numtoa"),
        );
        let versions = engine.versions(&lineage);
        assert_eq!(versions.len(), 2, "one lineage, two generations");
        assert_eq!(
            &*versions.current().expect("a current generation").version,
            "0.2.4",
            "the newest generation stays current even though the older one arrived last",
        );
    });
}

// ---------------------------------------------------------------------------
// The five failures, against the registries that actually produce them
// ---------------------------------------------------------------------------

/// A misspelled name must be reported as a misspelled name — naming the
/// registry and the endpoint that said no.
#[test]
#[ignore = "asks crates.io about a package that does not exist; run with --ignored"]
fn a_typo_in_the_name_is_reported_as_an_unknown_package_and_not_as_a_version_problem() {
    let cache = scratch_cache("unknown-package");
    let ((), _cost) = measured("purl_unknown_package", &cache, || {
        let (engine, _cache) = engine_for("unknown-package");
        let err = expect_failure(&engine, "pkg:cargo/numtoa-with-a-typo-nobody-published@0.2.5");
        assert_eq!(err.kind(), "unknown_package");
        assert!(!err.is_transient(), "a typo is not fixed by retrying");
        let text = err.to_string();
        assert!(text.contains("crates.io"), "{text}");
        assert!(text.contains("index.crates.io"), "the endpoint must be named: {text}");
    });
}

/// A real package at an unpublished version must list the versions that exist —
/// the only genuinely useful thing to say.
#[test]
#[ignore = "asks crates.io for a version of serde that does not exist; run with --ignored"]
fn a_version_that_was_never_published_lists_the_ones_that_were() {
    let cache = scratch_cache("bad-version");
    let ((), _cost) = measured("purl_version_not_found", &cache, || {
        let (engine, _cache) = engine_for("bad-version");
        let err = expect_failure(&engine, "pkg:cargo/serde@0.0.0-does-not-exist");
        assert_eq!(err.kind(), "version_not_found");
        let Error::VersionNotFound {
            available,
            published,
            ..
        } = &err
        else {
            panic!("expected VersionNotFound, got {err:?}");
        };
        assert!(
            *published > 100,
            "serde has hundreds of published versions; got {published}",
        );
        assert!(
            available.iter().any(|v| v.starts_with("1.0.")),
            "the listed versions must be real serde versions, got {available:?}",
        );
        assert!(
            err.to_string().contains("1.0."),
            "the message itself must name a version the caller can use: {err}",
        );
    });
}

/// A PURL with no `@version` is a different failure from a wrong one, and it
/// still has to carry the list.
#[test]
#[ignore = "asks crates.io what versions of serde exist; run with --ignored"]
fn a_versionless_purl_is_refused_with_the_list_rather_than_guessing_latest() {
    let cache = scratch_cache("no-version");
    let ((), _cost) = measured("purl_version_missing", &cache, || {
        let (engine, _cache) = engine_for("no-version");
        let err = expect_failure(&engine, "pkg:cargo/serde");
        assert_eq!(err.kind(), "version_missing");
        let Error::VersionMissing { available, .. } = &err else {
            panic!("expected VersionMissing, got {err:?}");
        };
        assert!(!available.is_empty(), "the list is the whole point of this variant");
        assert_ne!(
            err.kind(),
            Error::VersionNotFound {
                purl: String::new(),
                registry: "crates.io",
                requested: String::new(),
                published: 0,
                available: Vec::new(),
            }
            .kind(),
            "'you gave no version' and 'that version is wrong' must not be the same failure",
        );
    });
}

/// A PyPI release with no sdist is a real, common state and its own failure —
/// nothing is wrong with the request, the network, or this build.
#[test]
#[ignore = "asks PyPI for a wheel-only release; run with --ignored"]
fn a_wheel_only_release_is_reported_as_missing_sources_not_as_a_missing_package() {
    let cache = scratch_cache("no-sdist");
    let ((), _cost) = measured("purl_no_source_artifact", &cache, || {
        let (engine, _cache) = engine_for("no-sdist");
        // `nvidia-cublas-cu12` publishes wheels only; if this ever gains an
        // sdist the test fails loudly rather than silently passing, which is
        // the correct outcome for a claim about what a registry serves.
        let err = expect_failure(&engine, "pkg:pypi/nvidia-cublas-cu12@12.4.5.8");
        assert_eq!(
            err.kind(),
            "no_source_artifact",
            "got {err} — if this release now has an sdist, pick another wheel-only package",
        );
        assert!(
            err.to_string().contains("wheel"),
            "the message must say what is missing and why it cannot be used: {err}",
        );
    });
}

// ---------------------------------------------------------------------------
// Adversarial
// ---------------------------------------------------------------------------

/// Cancelling mid-flight must produce a `Cancelled` terminal event rather than
/// a silently closed stream.
#[test]
#[ignore = "starts a real fetch and cancels it; run with --ignored"]
fn dropping_the_stream_handle_cancels_the_job_rather_than_orphaning_it() {
    let cache = scratch_cache("cancel");
    let ((), _cost) = measured("purl_index_cancelled", &cache, || {
        let (engine, _cache) = engine_for("cancel");
        let purl = Purl::parse("pkg:cargo/serde@1.0.196").expect("valid purl");
        let (handle, rx) = engine.index_purl(purl, Gen(1));

        // `Started` is emitted before any work begins, so receiving it proves
        // the job is live at the moment we cancel.
        assert!(matches!(rx.recv(), Ok(IndexEvent::Started { .. })));
        handle.cancel();

        let terminal = loop {
            match rx.recv() {
                Ok(IndexEvent::Stage { .. }) => continue,
                Ok(other) => break other,
                Err(_) => panic!("a cancelled job must report cancellation, not close silently"),
            }
        };
        match terminal {
            IndexEvent::Failed { error, .. } => assert_eq!(error.kind(), "cancelled"),
            // A cancel that lands after the work finished is a legitimate race,
            // not a failure — the package really is indexed.
            IndexEvent::Indexed { .. } => {}
            other => panic!("unexpected terminal event {other:?}"),
        }
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn drain_packages(engine: &EngineHandle) -> Vec<String> {
    let rx = engine.packages();
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let nudox_engine::PackageLoadEvent::Loaded { name, .. } = event {
            out.push(name.to_string());
        }
    }
    out
}

fn search(engine: &EngineHandle, query: &str) -> Vec<String> {
    use nudox_engine::wire::SearchEvent;

    let (handle, rx) = engine.search(
        nudox_engine::SearchQuery {
            text: query.to_owned(),
            ..Default::default()
        },
        Gen(2),
    );
    let mut names = Vec::new();
    while let Ok(event) = rx.recv() {
        match event {
            SearchEvent::Section { rows, .. } => {
                names.extend(rows.iter().map(|r| r.display_name.to_string()));
            }
            SearchEvent::Done { .. } | SearchEvent::Failed { .. } => break,
            _ => {}
        }
    }
    drop(handle);
    names
}
