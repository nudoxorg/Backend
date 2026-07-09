//! Diagram (indexing): **runs computer · generate blob information.**
//!
//! Specs for `compiler::generate` — producing the three resolutions and the
//! canonical code/treesitter hash that becomes a package's postgres identity.
//!
//! Drives the real pipeline over the Rust `regular` fixture (package
//! `calculator@0.1.0`). The fixture is copied into a tempdir first so cargo's
//! `target/` never pollutes the repo. Needs a JSON-capable (nightly) rustdoc
//! on PATH — same requirement as `parse_rust_to_ir`.

use std::fs;
use std::path::{Path, PathBuf};

use compiler::error::GenerateError;
use compiler::generate::linked_data::{DocumentSink, emit};
use compiler::generate::{self, PackageInput};
use compiler::graph::link::PackageCtx;
use heart::{ContentHash, Edition, Language, PackageVersion, RegistryOrigin, Toolchain};
use registry::identity::PackageCoordinates;
use registry::package::PackageName;
use serde_json::Value;
use tempfile::TempDir;

// ─── Fixture plumbing ────────────────────────────────────────────────────────

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust")
}

fn copy_tree(src: &Path, dest: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Copy the regular Rust fixture into a tempdir and build a PackageInput.
///
/// Manifests are written here (gitignore only tracks `*.rs`).
fn package_input(dir: &TempDir) -> PackageInput {
    copy_tree(&fixture_root().join("regular"), dir.path()).expect("fixture copies");
    fs::write(
        dir.path().join("Cargo.toml"),
        r#"[package]
name = "calculator"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"

[dependencies]
helper = { path = "helper" }
"#,
    )
    .expect("root Cargo.toml");
    fs::write(
        dir.path().join("helper/Cargo.toml"),
        r#"[package]
name = "helper"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("helper Cargo.toml");
    PackageInput {
        coordinates: PackageCoordinates {
            origin: RegistryOrigin::CratesIo,
            name: PackageName::new(Language::Rust, "calculator").expect("valid package name"),
            version: PackageVersion::try_from((Language::Rust, "0.1.0"))
                .expect("valid package version"),
        },
        toolchain: Toolchain::Rust {
            compiler: semver::Version::new(1, 85, 0),
            edition: Edition::E2021,
        },
        root: dir.path().to_path_buf(),
    }
}

fn generate_regular() -> (generate::GeneratedPackage, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let input = package_input(&dir);
    let generated = generate::generate(&input).expect("generation succeeds");
    (generated, dir)
}

/// Fold per-file digests the same way `BlobInfo::assemble` does.
fn fold_snapshot(archive: &generate::SourceArchive) -> ContentHash {
    let mut hasher = ContentHash::builder();
    for file in &archive.files {
        let path = file.path.to_string_lossy();
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(file.hash.as_bytes());
    }
    hasher.finalize()
}

#[derive(Default)]
struct Collector {
    docs: Vec<Value>,
    flushed: bool,
}

impl DocumentSink for Collector {
    fn write(&mut self, document: Value) -> Result<(), GenerateError> {
        self.docs.push(document);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), GenerateError> {
        self.flushed = true;
        Ok(())
    }
}

// ─── Specs ───────────────────────────────────────────────────────────────────

/// Generation produces all three resolutions for a package.
///
/// Assert: the CST (`generate::cst`), API surface (`generate::surface`), and
///   tarred source (`generate::source_archive`) are all produced.
#[test]
fn generates_cst_surface_and_archive() {
    let (generated, _dir) = generate_regular();

    assert!(
        !generated.surface.entries_by_path.is_empty(),
        "surface must contain lowered entries"
    );
    assert!(
        !generated.archive.files.is_empty(),
        "archive must list at least the fixture source files"
    );
    // The regular fixture has `.rs` sources; CST extraction for Rust must
    // produce a well-formed (non-empty) set of per-file resolutions.
    assert!(
        !generated.cst.files.is_empty(),
        "CST for the regular Rust fixture must not be empty once treesitter works"
    );
}

/// Generation emits linked data for the graph store.
///
/// Assert: `generate::linked_data` produces the Entry + Kind documents for
///   terminus.
#[test]
fn generates_linked_data_documents() {
    let (generated, _dir) = generate_regular();

    let mut sink = Collector::default();
    let ctx = PackageCtx {
        language: "rust".into(),
        package: "calculator".into(),
        version: Some("0.1.0".into()),
    };
    emit(&generated.surface, ctx, &mut sink).expect("linked-data emission succeeds");
    assert!(sink.flushed, "emit must flush the sink once at the end");

    let packages: Vec<&Value> = sink.docs.iter().filter(|d| d["@type"] == "Package").collect();
    assert!(!packages.is_empty(), "Package document(s) must be emitted: {:?}", sink.docs);

    let symbols: Vec<&Value> = sink.docs.iter().filter(|d| d["@type"] == "Symbol").collect();
    assert!(
        !symbols.is_empty(),
        "Symbol documents must be emitted from the surface Index"
    );
}

/// Blob info carries the canonical code/treesitter hash.
///
/// Assert: `generate::blob_info` computes a stable hash of the
///   code/treesitter representation (the value postgres records as identity).
#[test]
fn blob_info_carries_canonical_hash() {
    let (generated, _dir) = generate_regular();

    assert_ne!(
        *generated.blob_info.snapshot.as_bytes(),
        [0u8; 32],
        "snapshot hash must be non-zero for a non-empty package"
    );
    assert_eq!(
        generated.snapshot, generated.blob_info.snapshot,
        "GeneratedPackage.snapshot must match blob_info.snapshot"
    );

    let recomputed = fold_snapshot(&generated.archive);
    assert_eq!(
        generated.blob_info.snapshot, recomputed,
        "blob_info.snapshot must match the identity fold over archive digests"
    );
}

/// Identical input yields an identical hash (reproducibility).
///
/// Assert: regenerating from the same source produces the same hash.
///
/// `generate::source_archive` skips toolchain build dirs (`target/`), so
/// rustdoc's side effects during surface lowering never pollute the snapshot.
#[test]
fn hash_is_reproducible() {
    let dir_a = TempDir::new().expect("tempdir a");
    let dir_b = TempDir::new().expect("tempdir b");
    let input_a = package_input(&dir_a);
    let input_b = package_input(&dir_b);

    let first = generate::generate(&input_a).expect("first generation");
    let second = generate::generate(&input_b).expect("second generation");

    assert_eq!(
        first.blob_info.snapshot, second.blob_info.snapshot,
        "identical source trees must produce identical snapshot hashes"
    );

    // Per-file digests must also match pairwise (stronger than the fold).
    let files_a: Vec<_> = first
        .archive
        .files
        .iter()
        .map(|f| (f.path.clone(), f.hash))
        .collect();
    let files_b: Vec<_> = second
        .archive
        .files
        .iter()
        .map(|f| (f.path.clone(), f.hash))
        .collect();
    assert_eq!(files_a, files_b, "per-file source digests must be reproducible");
}
