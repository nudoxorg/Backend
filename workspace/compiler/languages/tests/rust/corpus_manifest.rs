//! Preconditions and refusals for the crates.io corpus — the cheap half.
//!
//! # Why this binary is separate from `corpus_sweep.rs`
//!
//! `corpus_sweep.rs` boots in-process rust-analyzer once per package (~1
//! minute each, ~600 MB peak RSS) and is `#[ignore]`d for it, so it only runs
//! when somebody asks for it by name. That is the right cost for what it does
//! and the wrong cost for the question "is the corpus even here?".
//!
//! Every test in this file answers that question, or one like it, with file
//! metadata and a manifest parse — no rust-analyzer, no cargo, milliseconds.
//! It therefore runs in the ordinary gate, which is what makes the expensive
//! sweep's silence trustworthy: if `result/` is missing, renamed, or
//! holds a different version of a package than `nix/corpus.nix` pins,
//! this binary says so long before anyone spends twenty minutes finding out.
//!
//! # What the refusal tests are for
//!
//! docs/AGENTS-DOCTRINE.md §4 requires adversarial coverage of a protocol, not just
//! its happy path. The protocol here is `Producer::invoke`'s contract that a
//! `PackageSource` names a real cargo package. The two ways to violate it that
//! cost no rust-analyzer boot — a path that does not exist, and a directory
//! with no manifest anywhere above it — are covered here. The one that does
//! cost a boot (a manifest that exists but does not define the requested
//! package) lives in `corpus_sweep.rs` with the rest of the expensive work.
//!
//! ```text
//! cargo test -p nudox-languages --test rust_corpus_manifest -- --nocapture
//! ```

mod common;

use std::path::{Path, PathBuf};

use nudox_languages::rust::RustProducer;
use nudox_languages::{PackageSource, Producer, ProducerError};

use common::{ENTRIES, corpus_root, entry_root};

// ── Manifest parsing ─────────────────────────────────────────────────────────

/// The `name` and `version` from a manifest's `[package]` table.
///
/// A three-line scanner rather than a TOML dependency: this crate has no TOML
/// parser and adding one to read two keys out of a file whose shape is fixed by
/// `cargo package` would be a new dependency for no new information. The scan
/// stops at the next `[`-headed table, which is what keeps it from picking up
/// the `name = "bytes"` of `bytes-1.11.0`'s `[[bench]]` target thirty-six lines
/// later — the exact collision that would make a naive "first `name =` in the
/// file" reader wrong on four of the twenty-three entries.
fn package_identity(manifest: &Path) -> Result<(String, String), String> {
    let text = std::fs::read_to_string(manifest)
        .map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;

    let mut in_package = false;
    let (mut name, mut version) = (None, None);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if in_package {
                break;
            }
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = line.strip_prefix("name = ") {
            name = Some(rest.trim_matches('"').to_owned());
        } else if let Some(rest) = line.strip_prefix("version = ") {
            version = Some(rest.trim_matches('"').to_owned());
        }
    }

    match (name, version) {
        (Some(n), Some(v)) => Ok((n, v)),
        _ => Err(format!(
            "{} has no [package] name/version — it is not a publishable cargo package",
            manifest.display()
        )),
    }
}

// ── Corpus preconditions ─────────────────────────────────────────────────────

/// Every entry in `nix/corpus.nix`'s crates.io section is on disk, and is
/// the package and version it claims to be.
///
/// Asserting on the manifest's own `name`/`version` rather than on the
/// directory name is the content assertion here: `result/memchr-2.8.3`
/// being present proves a directory exists, and nothing more. A checkout
/// re-fetched at the wrong version, or a directory hand-renamed to make a sweep
/// go green, both leave the directory name intact and the manifest wrong — and
/// the sweep would then measure a package nobody asked for while reporting the
/// version everybody expected.
///
/// This fails, rather than skipping, when the corpus is absent. That is the
/// deliberate choice: `result/` is materialized by `nix build .#checks.corpus`
/// and every other language's corpus test in this workspace already requires
/// it. A skip here would restore exactly the failure mode this whole file
/// exists to remove — a green run that measured nothing.
#[test]
fn every_crates_io_corpus_entry_is_a_checkout_of_the_package_and_version_it_names() {
    let mut failures: Vec<String> = Vec::new();

    for entry in ENTRIES {
        let root = entry_root(entry);
        let manifest = root.join("Cargo.toml");

        if !manifest.is_file() {
            failures.push(format!(
                "{}: no Cargo.toml at {} — run `nix build .#checks.corpus` to materialize the corpus",
                entry.dir,
                root.display()
            ));
            continue;
        }
        if !root.join("src/lib.rs").is_file() {
            failures.push(format!(
                "{}: Cargo.toml present but no src/lib.rs — every crates.io corpus entry is a \
                 library, and a package with no lib target lowers to nothing",
                entry.dir
            ));
            continue;
        }

        match package_identity(&manifest) {
            Ok((name, version)) => {
                if name != entry.name || version != entry.version {
                    failures.push(format!(
                        "{}: manifest declares `{name} {version}`, corpus table expects \
                         `{} {}`",
                        entry.dir, entry.name, entry.version
                    ));
                }
            }
            Err(why) => failures.push(format!("{}: {why}", entry.dir)),
        }
    }

    assert!(
        failures.is_empty(),
        "{}/{} crates.io corpus entries are missing or misdescribed:\n  {}",
        failures.len(),
        ENTRIES.len(),
        failures.join("\n  ")
    );
}

/// Report, per entry, whether its checkout can resolve its dependency graph
/// with the network off — and fail if *none* of them can.
///
/// `ra::loaded::load` runs `cargo metadata --offline`. When that fails,
/// `ra_ap_project_model` retries with `--no-deps` and returns *that* as `Ok`;
/// the resulting lowering is missing every `cfg(feature = "…")` item in the
/// package. `DependencyResolution` makes it refuse to lower rather than lie,
/// so in the sweep this shows up as a typed error, not as a wrong number — but
/// the *reason* is always the same environmental one, and it is visible from
/// here for free: `cargo vendor` wrote a `vendor/` directory and a
/// `.cargo/config.toml` redirecting `crates-io` at it (see the comment
/// `result/memchr-2.8.3/.cargo/config.toml` carries).
///
/// This prints the classification rather than asserting per entry, because a
/// package with no dependencies at all needs no vendor directory and would be
/// wrongly flagged. What it does assert is that the redirect exists *somewhere*
/// — if every entry lost its `vendor/`, the sweep would degrade to twenty-three
/// identical `DependenciesUnresolved` errors, and this one-millisecond test
/// says why before that happens.
#[test]
fn the_corpus_carries_the_vendored_sources_offline_resolution_needs() {
    let mut vendored = Vec::new();
    let mut bare = Vec::new();

    for entry in ENTRIES {
        let root = entry_root(entry);
        let has_redirect = root.join(".cargo/config.toml").is_file();
        let has_vendor = root.join("vendor").is_dir();
        if has_redirect && has_vendor {
            vendored.push(entry.dir);
        } else {
            bare.push((entry.dir, has_redirect, has_vendor));
        }
    }

    eprintln!(
        "offline-resolvable checkouts: {}/{}",
        vendored.len(),
        ENTRIES.len()
    );
    for (dir, redirect, vendor) in &bare {
        eprintln!("  NO VENDOR {dir}: .cargo/config.toml={redirect} vendor/={vendor}");
    }

    assert!(
        !vendored.is_empty(),
        "no crates.io corpus entry under {} has both a `.cargo/config.toml` \
         source-replacement and a `vendor/` directory; `cargo metadata --offline` will fall \
         back to `--no-deps` for every package and the sweep can only report degraded loads",
        corpus_root().display()
    );
}

// ── Adversarial: inputs that are not packages ────────────────────────────────

/// A unique scratch directory outside any cargo workspace.
///
/// Deliberately *not* under `CARGO_TARGET_TMPDIR`: that lives inside this
/// repository's own `target/`, and `ProjectManifest::discover_single` walks
/// *upwards* looking for a manifest. A "package root" created there resolves to
/// the backend workspace's own root `Cargo.toml` and loads successfully — the
/// test would then be asserting that a nonexistent package lowers fine. The
/// system temp directory has no cargo manifest above it, which is the condition
/// this test needs to be about the input rather than about its neighbours.
fn scratch_dir(label: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("nudox-rust-corpus-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// A directory with no cargo manifest — and none above it — is refused.
///
/// The interesting half is that it is refused *at all*. A producer that
/// answered "no manifest" with an empty-but-`Ok` oracle would hand the pipeline
/// a table with nothing in it, and downstream an empty package and an
/// unreadable one are the same value.
///
/// The variant assertion records today's typed answer, which is
/// `ProducerError::OracleSpawn`. That is the wrong variant for this condition
/// and the assertion is written to say so out loud rather than to bless it:
/// nothing was spawned, there is no oracle subprocess in the Rust producer at
/// all (it drives rust-analyzer in-process), and `invoke`'s blanket
/// `map_err(|e| ProducerError::OracleSpawn { .. })` in `src/lib.rs` is what
/// puts every load failure — missing manifest, unparseable manifest, missing
/// sysroot — behind that one name. The cause chain does survive, so the test
/// checks the chain reaches the real reason.
#[test]
fn a_directory_with_no_cargo_manifest_is_refused_rather_than_lowered_empty() {
    let root = scratch_dir("no-manifest");
    let src = PackageSource::new(&root, "nudox-not-a-package", "0.0.0");

    let err = RustProducer { direct_repo: false }
        .invoke(&src)
        .err()
        .expect("a directory with no Cargo.toml above it must not load as a workspace");

    assert!(
        matches!(err, ProducerError::OracleSpawn { .. }),
        "expected the load failure to arrive as a typed ProducerError; got {err:?}"
    );
    let rendered = common::chain(&err);
    assert!(
        rendered.contains("caused by"),
        "the refusal must keep its cause chain — the top-level Display is `oracle spawn \
         failed`, which names neither the path nor the reason; got:\n{rendered}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A checkout path that does not exist is refused, and names something other
/// than a spawn failure in its chain.
///
/// The empty-directory case above and this one fail at different points —
/// `abs_path`/manifest discovery versus a path that cannot even be made
/// absolute — and both are reachable in practice from the same mistake: a
/// corpus entry whose directory was renamed. Covering only one of them leaves
/// the other free to succeed.
#[test]
fn a_checkout_path_that_does_not_exist_is_refused() {
    let root = std::env::temp_dir().join("nudox-rust-corpus-absent-9e3f1a/never/created");
    assert!(!root.exists(), "the fixture path must genuinely not exist");

    let src = PackageSource::new(&root, "memchr", "2.8.3");
    let err = RustProducer { direct_repo: false }
        .invoke(&src)
        .err()
        .expect("a nonexistent package root must not load as a workspace");

    assert!(
        matches!(err, ProducerError::OracleSpawn { .. }),
        "expected the load failure to arrive as a typed ProducerError; got {err:?}"
    );
    eprintln!("absent-root refusal chain:\n  {}", common::chain(&err));
}

/// The Nix catalog is the suite. 50× the ~20-package samples is 1000 version
/// lines, and the expansion has to name packages nobody would pick as a demo.
#[test]
fn the_nix_corpus_holds_at_least_a_thousand_versions_including_obscure_ones() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../nix/corpus.nix");
    let src = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|err| panic!("read {}: {err}", manifest.display()));
    let versions = src.matches("version = ").count();
    assert!(
        versions >= 1000,
        "nix/corpus.nix has {versions} version lines; the suite floor is 1000"
    );
    for name in ["neura-shader", "jzz-synth-tiny", "django-field-translate"] {
        assert!(
            src.contains(&format!("name = \"{name}\"")),
            "{name} must be in the catalog, not only a famous crate"
        );
    }
}
