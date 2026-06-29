//! Pipeline part: **Rust source → surface IR** (`compiler::languages::rust`).
//!
//! These are TDD specs: each body is `todo!()` until the lowering lands. They
//! pin down the shape `cargo rustdoc`'s JSON must be lowered into — an
//! `ir::entry::Index` keyed by `NudoxPath`, with the visibility / method /
//! workspace behaviour the old `producers::parse::rust` path guaranteed.

/// A regular single-crate package lowers every public item into the index.
///
/// Arrange: a `calculator` crate with `add`, `Counter` (+ inherent methods),
///   and a private `internals` module.
/// Act: `languages::rust::generate_ir(pkg, version, workspace)`.
/// Assert: the returned `Index.entries_by_path` contains `add` as
///   `Entry::Function`, `Counter` as `Entry::RecordType`, and `root_ids` lists
///   the crate root.
#[test]
fn regular_crate_lowers_public_items() {
    todo!("lower the `calculator` fixture and assert its entries/root_ids");
}

/// Private items are still recorded but carry `Visibility::Private`.
///
/// Assert: `internals::HiddenCounter` is present with `Visibility::Private`
///   (the `--document-private-items` pass), not dropped.
#[test]
fn private_items_retain_private_visibility() {
    todo!("assert HiddenCounter is present and Private");
}

/// Inherent methods are attached to their record, and blanket-trait methods are
/// reachable as members.
///
/// Assert: `Counter`'s `Entry::RecordType` exposes its inherent `methods`, and
///   `implemented_protocols` includes the blanket `BlanketView` impl.
#[test]
fn record_methods_and_blanket_impls_are_attached() {
    todo!("assert Counter.methods and implemented_protocols");
}

/// A virtual workspace with a hyphenated crate resolves member crates.
///
/// Arrange: the `crates/odd-duck` workspace fixture (`Widget<T>`, `Behavior`).
/// Assert: the hyphenated crate root canonicalizes, and `Widget`'s two impls
///   both appear.
#[test]
fn workspace_resolves_hyphenated_member_crates() {
    todo!("lower the workspace fixture and assert odd-duck entries");
}

/// A binary workspace pulls in its local path-dependency library.
///
/// Assert: `corelib::CoreCounter` is included even though the entry point is the
///   `app` binary (local library deps are documented).
#[test]
fn binary_workspace_includes_local_library_deps() {
    todo!("assert corelib entries are present from the binary workspace");
}

/// External dependency references become `NudoxPath::External`, never `Local`.
///
/// Assert: a parameter typed `serde::Serialize` resolves to
///   `External { dependency: "serde", .. }`.
#[test]
fn external_references_are_external_paths() {
    todo!("assert external type refs use NudoxPath::External");
}

/// The fq-name → source-text map is produced alongside the index for downstream
/// tree-sitter extraction.
///
/// Assert: `generate_ir` returns a `(Index, HashMap<String,String>)` whose map
///   has the raw source for `add` sliced by its rustdoc line span.
#[test]
fn source_map_is_produced_for_each_function() {
    todo!("assert the returned source map covers `add`");
}
