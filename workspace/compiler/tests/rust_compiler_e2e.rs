//! End-to-end: **rust compiler → final registry blob**.
//!
//! A regular cargo package, a little scaffold (copy fixture + write the
//! manifests gitignore strips), then `generate` + `BlobBuilder` — the same
//! hand-off the indexer uses. Assert the final blob is as expected.
//!
//! ```text
//!   cargo package (fixture)  →  generate  →  BlobManifest + cas sections
//! ```
//!
//! Needs a JSON-capable rustdoc on PATH. Under buck2:
//! `buck2 test //workspace/compiler:test-rust_compiler_e2e`.

use std::fs;
use std::path::{Path, PathBuf};

use compiler::generate::{self, PackageInput};
use heart::{ContentHash, Edition, Language, PackageVersion, RegistryOrigin, Toolchain};
use ir::entry::{Index, NudoxPath};
use ir::kind::Entry;
use registry::blob::{BlobBuilder, FileReferences, ReferenceSet};
use registry::identity::PackageCoordinates;
use registry::package::PackageName;
use registry::Package;
use tempfile::TempDir;

// ─── Scaffold ────────────────────────────────────────────────────────────────

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

/// Materialize the `regular` fixture as a real cargo package in `dir`.
///
/// Sources live under `tests/fixtures/rust/regular/`; manifests are written
/// here because the repo gitignore only tracks `*.rs`.
fn scaffold_regular(dir: &Path) {
    copy_tree(&fixture_root().join("regular"), dir).expect("fixture copies");

    fs::write(
        dir.join("Cargo.toml"),
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
        dir.join("helper/Cargo.toml"),
        r#"[package]
name = "helper"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("helper Cargo.toml");
}

fn package_input(root: &Path) -> PackageInput {
    PackageInput {
        coordinates: PackageCoordinates {
            origin: RegistryOrigin::CratesIo,
            name: PackageName::new(Language::Rust, "calculator").expect("valid name"),
            version: PackageVersion::try_from((Language::Rust, "0.1.0")).expect("valid version"),
        },
        toolchain: Toolchain::Rust {
            compiler: semver::Version::new(1, 85, 0),
            edition: Edition::E2021,
        },
        root: root.to_path_buf(),
    }
}

fn local(path: &str) -> NudoxPath {
    NudoxPath::Local(PathBuf::from(path))
}

fn path_key(path: &NudoxPath) -> String {
    match path {
        NudoxPath::Local(p) => p.to_string_lossy().into_owned(),
        NudoxPath::External { path, dependency } => {
            format!("{dependency}::{}", path.to_string_lossy())
        }
    }
}

/// JSON-friendly projection of an [`Index`] for the IR blob section.
fn ir_wire_from_index(index: &Index) -> serde_json::Value {
    let entries: serde_json::Map<String, serde_json::Value> = index
        .entries_by_path
        .iter()
        .map(|(path, entry)| {
            let kind = match entry {
                Entry::Function(_) => "Function",
                Entry::RecordType(_) => "RecordType",
                Entry::Module(_) => "Module",
                Entry::TraitDef(_) => "TraitDef",
                Entry::TraitImpl(_) => "TraitImpl",
                Entry::Constant(_) => "Constant",
                Entry::TypeAlias(_) => "TypeAlias",
                Entry::UnionType(_) => "UnionType",
                Entry::SumType(_) => "SumType",
                Entry::Macro(_) => "Macro",
                Entry::Variable(_) => "Variable",
                Entry::PrimitiveType(_) => "PrimitiveType",
                Entry::Field(_) => "Field",
                Entry::Event(_) => "Event",
                Entry::Info(_) => "Info",
            };
            (
                path_key(path),
                serde_json::json!({ "kind": kind, "name": entry.name() }),
            )
        })
        .collect();
    serde_json::json!({
        "root_ids": index.root_ids.iter().map(path_key).collect::<Vec<_>>(),
        "entries": entries,
    })
}

/// Assemble the registry blob the indexer would emit from a `GeneratedPackage`.
fn assemble_blob(
    package: &Package,
    generated: &generate::GeneratedPackage,
    root: &Path,
) -> (registry::BlobManifest, Vec<registry::blob::creation::PendingSection>) {
    let mut builder = BlobBuilder::new(package.id(), package.toolchain.clone());

    for file in &generated.archive.files {
        let bytes = fs::read(root.join(&file.path))
            .unwrap_or_else(|e| panic!("read {}: {e}", file.path.display()));
        assert_eq!(
            ContentHash::of_bytes(&bytes),
            file.hash,
            "archive digest vs disk for {}",
            file.path.display()
        );
        builder
            .push_file(
                smol_str::SmolStr::from(file.path.to_string_lossy().as_ref()),
                bytes::Bytes::from(bytes),
            )
            .expect("unique path");
    }

    // Wire form for the IR section: path keys as strings (serde_json cannot use
    // the NudoxPath enum as a map key). Round-trip checks read this shape.
    let ir_wire = ir_wire_from_index(&generated.surface);
    builder
        .set_ir(bytes::Bytes::from(
            serde_json::to_vec(&ir_wire).expect("IR wire serializes"),
        ))
        .expect("IR once");

    let references = ReferenceSet {
        by_file: generated
            .cst
            .files
            .iter()
            .map(|f| FileReferences {
                path: smol_str::SmolStr::from(f.path.to_string_lossy().as_ref()),
                references: f.references.clone(),
            })
            .collect(),
    };
    builder.set_references(&references).expect("refs once");
    builder.finalize().expect("complete blob")
}

// ─── Spec ────────────────────────────────────────────────────────────────────

/// Scaffold a cargo package, run the rust compiler, assert the final blob.
#[test]
fn rust_compiler_produces_expected_blob() {
    let dir = TempDir::new().expect("tempdir");
    scaffold_regular(dir.path());

    let input = package_input(dir.path());
    let package = Package {
        coordinates: input.coordinates.clone(),
        toolchain: input.toolchain.clone(),
    };

    let generated = generate::generate(&input).expect("generation succeeds");

    // Surface: the public items the fixture declares.
    assert!(
        matches!(
            generated.surface.entries_by_path.get(&local("calculator::add")),
            Some(Entry::Function(_))
        ),
        "add lowers as a Function"
    );
    assert!(
        matches!(
            generated.surface.entries_by_path.get(&local("calculator::Counter")),
            Some(Entry::RecordType(_))
        ),
        "Counter lowers as a RecordType"
    );
    assert!(
        generated.surface.root_ids.contains(&local("calculator")),
        "crate root is a root id"
    );

    // Source archive skips rustdoc's target/.
    let paths: Vec<_> = generated
        .archive
        .files
        .iter()
        .map(|f| f.path.to_string_lossy().into_owned())
        .collect();
    assert!(paths.iter().any(|p| p.ends_with("Cargo.toml")), "has Cargo.toml: {paths:?}");
    assert!(paths.iter().any(|p| p.ends_with("src/lib.rs")), "has src/lib.rs: {paths:?}");
    assert!(paths.iter().all(|p| !p.contains("target/")), "no target/: {paths:?}");

    // Compiler snapshot is the sorted fold of per-file digests.
    let mut fold = ContentHash::builder();
    for file in &generated.archive.files {
        let path = file.path.to_string_lossy();
        fold.update(&(path.len() as u64).to_le_bytes());
        fold.update(path.as_bytes());
        fold.update(file.hash.as_bytes());
    }
    assert_eq!(generated.blob_info.snapshot, fold.finalize());
    assert_eq!(generated.snapshot, generated.blob_info.snapshot);

    // Final registry blob: files + IR + references, every section integrity-checked.
    let (manifest, sections) = assemble_blob(&package, &generated, dir.path());
    manifest.validate().expect("manifest well-formed");
    assert_eq!(manifest.package, package.id());
    assert_eq!(manifest.files.len(), generated.archive.files.len());

    for entry in manifest.files.iter() {
        let section = sections
            .iter()
            .find(|s| s.hash == entry.hash)
            .unwrap_or_else(|| panic!("missing section for {}", entry.path));
        assert_eq!(ContentHash::of_bytes(&section.bytes), entry.hash);
        assert_eq!(section.bytes.len() as u64, entry.size);
    }

    let ir_section = sections
        .iter()
        .find(|s| s.hash == manifest.ir_ref)
        .expect("IR section");
    let ir: serde_json::Value =
        serde_json::from_slice(&ir_section.bytes).expect("IR section is JSON");
    assert_eq!(ir["entries"]["calculator::add"]["kind"], "Function");
    assert_eq!(ir["entries"]["calculator::Counter"]["kind"], "RecordType");
    assert!(
        ir["root_ids"]
            .as_array()
            .expect("root_ids array")
            .iter()
            .any(|v| v.as_str() == Some("calculator")),
        "blob IR retains crate root"
    );

    let refs_section = sections
        .iter()
        .find(|s| s.hash == manifest.references_ref)
        .expect("references section");
    ReferenceSet::decode(&refs_section.bytes).expect("references decode");
    if !generated.cst.files.is_empty() {
        let refs = ReferenceSet::decode(&refs_section.bytes).unwrap();
        assert_eq!(refs.by_file.len(), generated.cst.files.len());
    }
}
