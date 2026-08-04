//! Insta snapshot tests for the Rust producer and Rust renderer.
//!
//! # Compile-target snapshots
//! Real Rust source fixtures → in-process rust-analyzer producer → `ir::entry::Index`.
//! The full `Debug` of the IR is **not** snapshotted directly because
//! `Index.entries_by_path` is an `FxHashMap` (nondeterministic order) and
//! some payloads carry internal rust-analyzer IDs that vary across runs.
//!
//! Instead we snapshot a **stable projection**:
//!   `BTreeMap<String, EntryStub>` keyed by the NudoxPath display string,
//! where `EntryStub` captures `kind_tag + name + visibility` — the public
//! contract the producer must satisfy — but drops all internal IDs.
//! This is the right tradeoff: we're pinning *what the producer lowered*,
//! not the transient IR guts. Any field that can vary (e.g. `doc_links`
//! containing internal hashes) is excluded from the stub.
//!
//! For traits and records we also include `sealed` / method-count / protocol
//! paths so regressions in those invariants are caught.
//!
//! # Renderer snapshots
//! Selected IR entries (pulled from the same `lower()` helper) are rendered
//! back to Rust surface syntax at two widths (40 and 100 columns) to exercise
//! the line-breaking logic.
//!
//! # How to regenerate
//! ```
//! INSTA_UPDATE=always buck2 test //workspace/compiler:test-snap_rust
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use compiler::languages::rust::generate_ir;
use compiler::render::{render_entry, Language, RenderCtx};
use ir::entry::{Index, NudoxPath};
use ir::kind::{Entry, Visibility};
use rustc_hash::FxHashMap as HashMap;
use semver::Version;
use tempfile::TempDir;

// ── Fixture plumbing (copied from parse_rust_to_ir.rs) ───────────────────────

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

/// Write Cargo manifests for a fixture (sources are gitignored).
fn scaffold_manifests(fixture: &str, dir: &Path) {
    match fixture {
        "regular" => {
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
        "workspace" => {
            fs::write(
                dir.join("Cargo.toml"),
                r#"[workspace]
members = ["crates/odd-duck"]
resolver = "2"
"#,
            )
            .expect("workspace Cargo.toml");
            fs::write(
                dir.join("crates/odd-duck/Cargo.toml"),
                r#"[package]
name = "odd-duck"
version = "0.3.1"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
            )
            .expect("odd-duck Cargo.toml");
        }
        "binary_workspace" => {
            fs::write(
                dir.join("Cargo.toml"),
                r#"[workspace]
members = ["app", "corelib"]
resolver = "2"
"#,
            )
            .expect("workspace Cargo.toml");
            fs::write(
                dir.join("app/Cargo.toml"),
                r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "app"
path = "src/main.rs"

[dependencies]
corelib = { path = "../corelib" }
"#,
            )
            .expect("app Cargo.toml");
            fs::write(
                dir.join("corelib/Cargo.toml"),
                r#"[package]
name = "corelib"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
            )
            .expect("corelib Cargo.toml");
        }
        other => panic!("unknown fixture for scaffold: {other}"),
    }
}

/// Copy a fixture into a tempdir, scaffold manifests, and lower it to IR.
fn lower(fixture: &str, package: &str, version: &str) -> (Index, HashMap<String, String>, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    copy_tree(&fixture_root().join(fixture), dir.path()).expect("fixture copies");
    scaffold_manifests(fixture, dir.path());
    let version = Version::parse(version).expect("fixture version parses");
    let (index, sources) =
        generate_ir(dir.path(), package, &version, true).expect("lowering succeeds");
    (index, sources, dir)
}

/// Convert a `NudoxPath` to a stable display string for use as a sort key.
fn path_key(p: &NudoxPath) -> String {
    match p {
        NudoxPath::Local(pb) => pb.display().to_string(),
        NudoxPath::External { dependency, path } => {
            format!("ext::{dependency}::{}", path.display())
        }
    }
}

// ── Stable IR projection ──────────────────────────────────────────────────────

/// Compact, stable representation of a single entry for snapshot purposes.
///
/// We deliberately exclude fields that carry internal IDs (e.g. `doc_links`
/// hashes from rust-analyzer) to keep snapshots deterministic across runs.
#[derive(Debug)]
struct EntryStub {
    kind: &'static str,
    name: String,
    visibility: String,
    /// For TraitDef: the `sealed` field.
    sealed: Option<Option<bool>>,
    /// For RecordType: number of inherent methods attached.
    method_count: Option<usize>,
    /// For RecordType: sorted list of implemented-protocol path keys.
    protocols: Option<Vec<String>>,
    /// For Function: deprecation info.
    deprecation: Option<(Option<String>, Option<String>)>,
}

fn vis_str(v: &Visibility) -> String {
    match v {
        Visibility::Public => "pub".into(),
        Visibility::Private => "priv".into(),
        Visibility::Internal => "pub(crate)".into(),
        Visibility::Protected => "protected".into(),
        Visibility::Package => "pub(super)".into(),
    }
}

fn stub_from_entry(e: &Entry) -> EntryStub {
    let kind = e.kind_tag();
    let name = e.name().to_string();
    let vis = vis_str(match e {
        Entry::Module(s) => &s.visibility,
        Entry::RecordType(s) => &s.visibility,
        Entry::TraitDef(s) => &s.visibility,
        Entry::SumType(s) => &s.visibility,
        Entry::Function(s) => &s.visibility,
        Entry::TypeAlias(s) => &s.visibility,
        Entry::UnionType(s) => &s.visibility,
        Entry::TraitImpl(s) => &s.visibility,
        Entry::Constant(s) => &s.visibility,
        Entry::Variable(s) => &s.visibility,
        Entry::Macro(s) => &s.visibility,
        Entry::PrimitiveType(s) => &s.visibility,
        Entry::Field(s) => &s.visibility,
        Entry::Event(s) => &s.visibility,
        Entry::Info(s) => &s.visibility,
    });

    let sealed = match e {
        Entry::TraitDef(s) => Some(s.inner.sealed),
        _ => None,
    };

    let method_count = match e {
        Entry::RecordType(s) => Some(s.inner.methods.as_ref().map(|m| m.len()).unwrap_or(0)),
        _ => None,
    };

    let protocols = match e {
        Entry::RecordType(s) => {
            let mut ps: Vec<String> = s
                .inner
                .implemented_protocols
                .as_ref()
                .into_iter()
                .flatten()
                .map(path_key)
                .collect();
            ps.sort();
            Some(ps)
        }
        _ => None,
    };

    let deprecation = match e {
        Entry::Function(s) => Some(
            s.deprecation
                .as_ref()
                .map(|d| (d.since.clone(), d.note.clone()))
                .unwrap_or((None, None)),
        ),
        _ => None,
    };

    EntryStub { kind, name, visibility: vis, sealed, method_count, protocols, deprecation }
}

/// Build the sorted, stable map that gets snapshotted for IR tests.
fn sorted_stubs(index: &Index) -> BTreeMap<String, EntryStub> {
    index
        .entries_by_path
        .iter()
        .map(|(path, entry)| (path_key(path), stub_from_entry(entry)))
        .collect()
}

/// Grab the entry at `path` (panics if absent).
fn entry<'i>(index: &'i Index, path: &str) -> &'i Entry {
    let key = NudoxPath::Local(PathBuf::from(path));
    index
        .entries_by_path
        .get(&key)
        .unwrap_or_else(|| panic!("no entry at {path}"))
}

// ── Helpers for renderer tests ─────────────────────────────────────────────────

fn render_at(e: &Entry, width: usize) -> String {
    let cx = RenderCtx::new(Language::Rust).with_width(width).with_docs(true);
    render_entry(e, &cx)
}

/// Render every renderable entry in the index (sorted by path key) and join with
/// a horizontal rule so a single snapshot covers the full surface.
fn render_all_sorted(index: &Index, width: usize) -> String {
    let mut pairs: Vec<(String, String)> = index
        .entries_by_path
        .iter()
        .filter_map(|(path, e)| {
            // Only snapshot entry kinds the Rust renderer handles directly.
            match e {
                Entry::RecordType(_)
                | Entry::SumType(_)
                | Entry::TraitDef(_)
                | Entry::Function(_)
                | Entry::TypeAlias(_)
                | Entry::UnionType(_) => Some((path_key(path), render_at(e, width))),
                _ => None,
            }
        })
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    pairs
        .into_iter()
        .map(|(k, v)| format!("// {k}\n{v}"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

// ── Compile-target snapshot tests ─────────────────────────────────────────────

/// Snapshot the stable IR projection for the `regular` fixture (`calculator`).
///
/// Covers: structs with inherent methods, sealed/open traits, deprecated
/// functions, external path references, private items, blanket impls.
#[test]
fn snap_regular_ir() {
    let (index, _sources, _dir) = lower("regular", "calculator", "0.1.0");
    let stubs = sorted_stubs(&index);
    insta::assert_debug_snapshot!("rust_regular_ir", stubs);
}

/// Snapshot the stable IR projection for the `workspace` fixture (`odd-duck`).
///
/// Covers: hyphenated crate names, generic structs (`Widget<T>`), enums
/// (`Mode`), trait definitions (`Behavior`), multiple impl blocks.
#[test]
fn snap_workspace_ir() {
    let (index, _sources, _dir) = lower("workspace", "odd-duck", "0.3.1");
    let stubs = sorted_stubs(&index);
    insta::assert_debug_snapshot!("rust_workspace_ir", stubs);
}

/// Snapshot the stable IR projection for the `binary_workspace` fixture.
///
/// Covers: binary crate entry point, local library dependency (`corelib`)
/// pulled in automatically.
#[test]
fn snap_binary_workspace_ir() {
    let (index, _sources, _dir) = lower("binary_workspace", "app", "0.1.0");
    let stubs = sorted_stubs(&index);
    insta::assert_debug_snapshot!("rust_binary_workspace_ir", stubs);
}

// ── Renderer snapshot tests ────────────────────────────────────────────────────

/// Render every renderable entry from the `regular` fixture at 80 columns
/// (default width, docs enabled).
///
/// Covers in one sweep: struct with private field + inherent methods, sealed
/// trait, open trait, deprecated function, BlanketView trait, and `add`.
#[test]
fn snap_regular_render_w80() {
    let (index, _sources, _dir) = lower("regular", "calculator", "0.1.0");
    let rendered = render_all_sorted(&index, 80);
    insta::assert_snapshot!("rust_regular_render_w80", rendered);
}

/// Render every renderable entry from the `workspace` fixture at 80 columns.
///
/// Covers: generic struct `Widget<T>`, enum `Mode` (unit variants), trait
/// `Behavior` with required method.
#[test]
fn snap_workspace_render_w80() {
    let (index, _sources, _dir) = lower("workspace", "odd-duck", "0.3.1");
    let rendered = render_all_sorted(&index, 80);
    insta::assert_snapshot!("rust_workspace_render_w80", rendered);
}

/// Render `Widget<T>` (a generic struct with a type-parametric field) at a
/// narrow 40-column budget to exercise line-breaking in generic declarations.
#[test]
fn snap_widget_render_narrow_w40() {
    let (index, _sources, _dir) = lower("workspace", "odd-duck", "0.3.1");
    let e = entry(&index, "odd_duck::Widget");
    let rendered = render_at(e, 40);
    insta::assert_snapshot!("rust_widget_render_w40", rendered);
}

/// Render `Widget<T>` at a wide 100-column budget — should stay on one line.
#[test]
fn snap_widget_render_wide_w100() {
    let (index, _sources, _dir) = lower("workspace", "odd-duck", "0.3.1");
    let e = entry(&index, "odd_duck::Widget");
    let rendered = render_at(e, 100);
    insta::assert_snapshot!("rust_widget_render_w100", rendered);
}

/// Render the deprecated `legacy_add` function — must include the
/// `#[deprecated(since = ..., note = ...)]` annotation in the output.
#[test]
fn snap_deprecated_function_render() {
    let (index, _sources, _dir) = lower("regular", "calculator", "0.1.0");
    let e = entry(&index, "calculator::legacy_add");
    let rendered = render_at(e, 80);
    insta::assert_snapshot!("rust_deprecated_fn_render", rendered);
}

/// Render `SealedTrait` (a trait with a private-module supertrait).
///
/// The renderer outputs it as a plain `trait` definition (sealedness is an
/// IR-level concept, not surface syntax); this snapshot pins that behaviour.
#[test]
fn snap_sealed_trait_render() {
    let (index, _sources, _dir) = lower("regular", "calculator", "0.1.0");
    let e = entry(&index, "calculator::SealedTrait");
    let rendered = render_at(e, 80);
    insta::assert_snapshot!("rust_sealed_trait_render", rendered);
}

/// Render `OpenTrait` (a trait with a public supertrait).
#[test]
fn snap_open_trait_render() {
    let (index, _sources, _dir) = lower("regular", "calculator", "0.1.0");
    let e = entry(&index, "calculator::OpenTrait");
    let rendered = render_at(e, 80);
    insta::assert_snapshot!("rust_open_trait_render", rendered);
}

/// Render `BlanketView` (a trait with an associated type and a provided method
/// `view`).
#[test]
fn snap_blanket_view_trait_render() {
    let (index, _sources, _dir) = lower("regular", "calculator", "0.1.0");
    let e = entry(&index, "calculator::BlanketView");
    let rendered = render_at(e, 80);
    insta::assert_snapshot!("rust_blanket_view_render", rendered);
}

/// Render `Counter` (a struct with private fields, inherent methods, and
/// external trait impl).  Width 80, docs enabled.
#[test]
fn snap_counter_render_w80() {
    let (index, _sources, _dir) = lower("regular", "calculator", "0.1.0");
    let e = entry(&index, "calculator::Counter");
    let rendered = render_at(e, 80);
    insta::assert_snapshot!("rust_counter_render_w80", rendered);
}

/// Render `Counter` at 40 columns to exercise field-list breaking.
#[test]
fn snap_counter_render_w40() {
    let (index, _sources, _dir) = lower("regular", "calculator", "0.1.0");
    let e = entry(&index, "calculator::Counter");
    let rendered = render_at(e, 40);
    insta::assert_snapshot!("rust_counter_render_w40", rendered);
}

/// Render `corelib::CoreCounter` from the binary_workspace fixture.
#[test]
fn snap_core_counter_render() {
    let (index, _sources, _dir) = lower("binary_workspace", "app", "0.1.0");
    let e = entry(&index, "corelib::CoreCounter");
    let rendered = render_at(e, 80);
    insta::assert_snapshot!("rust_core_counter_render", rendered);
}

/// Render `Behavior` trait from the workspace fixture.
#[test]
fn snap_behavior_trait_render() {
    let (index, _sources, _dir) = lower("workspace", "odd-duck", "0.3.1");
    let e = entry(&index, "odd_duck::Behavior");
    let rendered = render_at(e, 80);
    insta::assert_snapshot!("rust_behavior_trait_render", rendered);
}

/// Render the `Mode` enum (unit-only variants) from the workspace fixture.
#[test]
fn snap_mode_enum_render() {
    let (index, _sources, _dir) = lower("workspace", "odd-duck", "0.3.1");
    let e = entry(&index, "odd_duck::Mode");
    let rendered = render_at(e, 80);
    insta::assert_snapshot!("rust_mode_enum_render", rendered);
}
