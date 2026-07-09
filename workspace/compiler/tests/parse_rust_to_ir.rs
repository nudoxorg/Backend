//! Pipeline part: **Rust source → surface IR** (`compiler::languages::rust`).
//!
//! These specs pin down the shape `cargo rustdoc`'s JSON must be lowered into —
//! an `ir::entry::Index` keyed by `NudoxPath`, with the visibility / method /
//! workspace behaviour the old `producers::parse::rust` path guaranteed.
//!
//! The tests drive the real `cargo rustdoc --output-format json` invocation
//! over the fixtures in `tests/fixtures/rust/`, so they need a JSON-capable
//! (nightly) rustdoc on PATH. Each fixture is copied into a tempdir first so
//! cargo's `target/` never pollutes the repo.

use std::fs;
use std::path::{Path, PathBuf};

use compiler::languages::rust::generate_ir;
use ir::entry::{Index, NudoxPath};
use ir::kind::{Entry, Visibility};
use rustc_hash::FxHashMap as HashMap;
use semver::Version;
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

/// Copy a fixture into a tempdir and lower it.
fn lower(fixture: &str, package: &str, version: &str) -> (Index, HashMap<String, String>, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    copy_tree(&fixture_root().join(fixture), dir.path()).expect("fixture copies");
    let version = Version::parse(version).expect("fixture version parses");
    let (index, sources) =
        generate_ir(dir.path(), package, &version, true).expect("lowering succeeds");
    (index, sources, dir)
}

fn local(path: &str) -> NudoxPath {
    NudoxPath::Local(PathBuf::from(path))
}

fn entry<'i>(index: &'i Index, path: &str) -> &'i Entry {
    index
        .entries_by_path
        .get(&local(path))
        .unwrap_or_else(|| panic!("expected entry at {path}"))
}

fn record<'i>(index: &'i Index, path: &str) -> &'i ir::kind::Symbol<ir::record::Record> {
    match entry(index, path) {
        Entry::RecordType(symbol) => symbol,
        other => panic!("expected RecordType at {path}, got {other}"),
    }
}

// ─── Specs ───────────────────────────────────────────────────────────────────

/// A regular single-crate package lowers every public item into the index.
///
/// Arrange: a `calculator` crate with `add`, `Counter` (+ inherent methods),
///   and a private `internals` module.
/// Act: `languages::rust::generate_ir(root, "calculator", version, ..)`.
/// Assert: the returned `Index.entries_by_path` contains `add` as
///   `Entry::Function`, `Counter` as `Entry::RecordType`, and `root_ids` lists
///   the crate root.
#[test]
fn regular_crate_lowers_public_items() {
    let (index, _sources, _dir) = lower("regular", "calculator", "0.1.0");

    assert!(
        matches!(entry(&index, "calculator::add"), Entry::Function(_)),
        "add lowers as a Function"
    );
    assert!(
        matches!(entry(&index, "calculator::Counter"), Entry::RecordType(_)),
        "Counter lowers as a RecordType"
    );
    assert!(
        index.root_ids.contains(&local("calculator")),
        "the crate root is a root id: {:?}",
        index.root_ids
    );
}

/// Private items are still recorded but carry non-public visibility.
///
/// Assert: `internals::HiddenCounter` is present (the
///   `--document-private-items` pass) rather than dropped, and neither it nor
///   its private parent module is `Visibility::Public`. (`pub(crate)` maps to
///   `Internal`; unmarked items map to `Private`. rustdoc may report either for
///   crate-private modules depending on format version.)
#[test]
fn private_items_retain_private_visibility() {
    let (index, _sources, _dir) = lower("regular", "calculator", "0.1.0");

    let module_visibility = match entry(&index, "calculator::internals") {
        Entry::Module(symbol) => &symbol.visibility,
        other => panic!("internals should be a Module, got {other}"),
    };
    assert!(
        matches!(*module_visibility, Visibility::Internal | Visibility::Private),
        "unmarked module must stay non-public, got {module_visibility:?}"
    );

    let hidden = record(&index, "calculator::internals::HiddenCounter");
    assert!(
        matches!(hidden.visibility, Visibility::Internal | Visibility::Private),
        "pub(crate) struct must stay non-public, got {:?}",
        hidden.visibility
    );
}

/// Inherent methods are attached to their record, and blanket-trait methods are
/// reachable as members.
///
/// Assert: `Counter`'s `Entry::RecordType` exposes its inherent `methods`, and
///   `implemented_protocols` includes the blanket `BlanketView` impl.
#[test]
fn record_methods_and_blanket_impls_are_attached() {
    let (index, _sources, _dir) = lower("regular", "calculator", "0.1.0");

    let counter = record(&index, "calculator::Counter");

    let methods = counter.inner.methods.as_ref().expect("Counter carries methods");
    assert!(
        methods.len() > 3,
        "inherent (new/increment/value) + blanket (view) methods expected, got {}",
        methods.len()
    );

    let protocols =
        counter.inner.implemented_protocols.as_ref().expect("Counter implements protocols");
    assert!(
        protocols.iter().any(|p| matches!(
            p,
            NudoxPath::Local(path) if path.to_string_lossy().ends_with("BlanketView")
        )),
        "the blanket BlanketView impl is attached: {protocols:?}"
    );
}

/// A virtual workspace with a hyphenated crate resolves member crates.
///
/// Arrange: the `crates/odd-duck` workspace fixture (`Widget<T>`, `Behavior`).
/// Assert: the hyphenated crate root canonicalizes, and `Widget`'s two impls
///   both appear.
#[test]
fn workspace_resolves_hyphenated_member_crates() {
    let (index, _sources, _dir) = lower("workspace", "odd-duck", "0.3.1");

    assert!(
        index.root_ids.contains(&local("odd_duck")),
        "hyphenated crate root canonicalizes to odd_duck: {:?}",
        index.root_ids
    );

    let widget = record(&index, "odd_duck::Widget");
    let methods = widget.inner.methods.as_ref().expect("Widget carries methods");
    // Both inherent impl blocks contribute methods (`new` and `echo`). rustdoc
    // may also surface methods from auto/blanket impls, so require at least the
    // two inherent ones rather than an exact count. (`Function` has no name
    // field — identity is in the enclosing Entry, not the method payload.)
    assert!(
        methods.len() >= 2,
        "both impl blocks (new + echo) contribute methods, got {}",
        methods.len()
    );
    // `new` is an associated fn (no receiver); `echo` is a method (has one).
    assert!(
        methods.iter().any(|m| m.receiver.is_none()),
        "Widget::new (static/associated) is present among methods"
    );
    assert!(
        methods.iter().any(|m| m.receiver.is_some()),
        "Widget::echo (instance method) is present among methods"
    );

    assert!(
        matches!(entry(&index, "odd_duck::Behavior"), Entry::TraitDef(_)),
        "Behavior lowers as a TraitDef"
    );
}

/// A binary workspace pulls in its local path-dependency library.
///
/// Assert: `corelib::CoreCounter` is included even though the entry point is the
///   `app` binary (local library deps are documented).
#[test]
fn binary_workspace_includes_local_library_deps() {
    let (index, _sources, _dir) = lower("binary_workspace", "app", "0.1.0");

    assert!(
        matches!(entry(&index, "corelib::CoreCounter"), Entry::RecordType(_)),
        "the local library dependency is documented"
    );
    assert!(
        index.entries_by_path.contains_key(&local("app")),
        "the binary root itself is present"
    );
}

/// External dependency references become `NudoxPath::External`, never `Local`.
///
/// Arrange: `Counter` implements `helper::Marker`, a trait from a path
///   dependency outside the documented set (the offline stand-in for
///   `serde::Serialize`).
/// Assert: the implemented protocol resolves to
///   `External { dependency: "helper", .. }`.
#[test]
fn external_references_are_external_paths() {
    let (index, _sources, _dir) = lower("regular", "calculator", "0.1.0");

    let counter = record(&index, "calculator::Counter");
    let protocols =
        counter.inner.implemented_protocols.as_ref().expect("Counter implements protocols");

    assert!(
        protocols.iter().any(|p| matches!(
            p,
            NudoxPath::External { dependency, .. } if dependency == "helper"
        )),
        "helper::Marker must resolve as an external path: {protocols:?}"
    );
    assert!(
        !protocols.iter().any(|p| matches!(
            p,
            NudoxPath::Local(path) if path.to_string_lossy().contains("Marker")
        )),
        "an external trait must never be minted as Local: {protocols:?}"
    );
}

/// The fq-name → source-text map is produced alongside the index for downstream
/// tree-sitter extraction.
///
/// Assert: `generate_ir` returns a source map whose entry for `add` holds the
///   raw source sliced by its rustdoc line span.
#[test]
fn source_map_is_produced_for_each_function() {
    let (_index, sources, _dir) = lower("regular", "calculator", "0.1.0");

    let add = sources
        .get("calculator::add")
        .unwrap_or_else(|| panic!("source map covers add: {:?}", sources.keys()));
    assert!(add.contains("left + right"), "the mapped text is the function body: {add:?}");
}
