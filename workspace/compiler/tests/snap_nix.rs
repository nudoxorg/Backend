//! Insta snapshot tests for the Nix language producer and renderer.
//!
//! ## Structure
//!
//! **Compile-target snapshots** (`snap_compile_*`):
//!   Lower a real Nix fixture tree → Nix producer → `ir::entry::Index`.
//!   The index is determinised into a `BTreeMap<String, &Entry>` keyed by
//!   path display before snapshotting with `insta::assert_debug_snapshot!`.
//!
//! **Renderer snapshots** (`snap_render_*`):
//!   Take the same IR and render every entry back to Nix surface via
//!   `render_entry`. Sorted entries are joined into one string and snapshotted
//!   with `insta::assert_snapshot!`.  At least one test runs at two column
//!   widths (40 and 100) to capture line-breaking behaviour.
//!
//! ## Covered edge cases
//!
//! * Untyped function (no `::` sig comment)
//! * Simple arrow chain: `Int -> Bool`, `String -> [String] -> String`
//! * Slice type parameter: `[String]`
//! * Closed record parameter: `{ name :: String; age :: Int }`
//! * Open record (rest `...`): `{ name :: String; ... }`
//! * Type variables: `a -> b`, `a -> (a -> b) -> b`
//! * Higher-order HOF: `(String -> a -> b) -> AttrSet a -> AttrSet b`
//! * Nullable / optional field: `String?`
//! * Curried multi-arg functions
//! * `inherit` aliases (attrset re-export)
//! * Attrset-valued bindings / nested namespace modules
//! * Doc comments recovered into markdown (`.with_docs(true)`)
//! * Builtins-style synthetic functions (no typed params)
//! * Two column widths (40 vs 100) for a long signature
//!
//! ## Generating / updating snapshots
//!
//! ```sh
//! INSTA_UPDATE=always buck2 test //workspace/compiler:test-snap_nix
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use compiler::languages::nix::lower_package;
use compiler::render::{render_entry, Language, RenderCtx};
use ir::entry::{Index, NudoxPath};
use ir::kind::Entry;

// ---------------------------------------------------------------------------
// Helpers shared by compile + render tests
// ---------------------------------------------------------------------------

/// Resolve a fixture path relative to this crate's manifest directory.
fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/nix")
        .join(rel)
}

/// Display a [`NudoxPath`] as a stable, sortable string.
///
/// * `Local(p)` → `p.display()`
/// * `External { dependency, path }` → `"dependency:path.display()"`
fn path_display(path: &NudoxPath) -> String {
    match path {
        NudoxPath::Local(p) => p.display().to_string(),
        NudoxPath::External { path, dependency } => {
            format!("{dependency}:{}", path.display())
        }
    }
}

/// Collect `&Index.entries_by_path` into a deterministic `BTreeMap<String, &Entry>`.
///
/// `FxHashMap` iteration order is nondeterministic; sorting by path display
/// guarantees that the snapshot is stable across runs and platforms.
fn sorted_entries<'a>(index: &'a Index) -> BTreeMap<String, &'a Entry> {
    index
        .entries_by_path
        .iter()
        .map(|(p, e)| (path_display(p), e))
        .collect()
}

/// Lower a fixture tree, panicking with a readable message on failure.
fn lower(fixture_rel: &str) -> Index {
    let root = fixture(fixture_rel);
    assert!(
        root.exists(),
        "fixture directory missing: {}",
        root.display()
    );
    lower_package(Path::new(&root))
        .unwrap_or_else(|e| panic!("lower_package({fixture_rel}) failed: {e}"))
}

/// Render all entries in sorted order into a single string.
///
/// Each entry is rendered with `render_entry`, and entries are joined by
/// `\n\n` so the snapshot reads like a Nix source file.
fn render_all(index: &Index, cx: &RenderCtx) -> String {
    sorted_entries(index)
        .values()
        .map(|e| render_entry(e, cx))
        .collect::<Vec<_>>()
        .join("\n\n")
}

// ---------------------------------------------------------------------------
// Compile-target snapshot: lib-style fixture
// ---------------------------------------------------------------------------

/// Snapshot the full IR (as Debug) for the existing lib-style fixture.
///
/// Covers: mapAttrs (HOF with generic type vars), concatStringsSep (arrow +
/// slice), makeWrapper (record formals), attrsets.mapAttrs (inherit alias),
/// plus docs and return types.
#[test]
fn snap_compile_libstyle_ir() {
    let index = lower("lib-style");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("nix_libstyle_ir", sorted);
}

// ---------------------------------------------------------------------------
// Compile-target snapshot: typed-fns fixture
// ---------------------------------------------------------------------------

/// Snapshot the full IR for the typed-fns fixture.
///
/// Covers: untyped function, simple arrows, slice parameter, closed record,
/// open record (`...`), type variables, HOF, nullable field, multi-arg curried
/// functions.
#[test]
fn snap_compile_typed_fns_ir() {
    let index = lower("typed-fns");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("nix_typed_fns_ir", sorted);
}

// ---------------------------------------------------------------------------
// Compile-target snapshot: attrset-vals fixture
// ---------------------------------------------------------------------------

/// Snapshot the full IR for the attrset-vals fixture.
///
/// Covers: nested attrset module (`strings`), `strings.trim` / `strings.pad`
/// functions with typed sigs, `utils.trim` inherit alias, `defaultWidth`
/// constant, plus the synthetic builtins module.
#[test]
fn snap_compile_attrset_vals_ir() {
    let index = lower("attrset-vals");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("nix_attrset_vals_ir", sorted);
}

// ---------------------------------------------------------------------------
// Compile-target snapshot: mini-flake fixture
// ---------------------------------------------------------------------------

/// Snapshot the full IR for the mini-flake fixture (flake.nix with outputs).
///
/// Covers: flake root module, `lib.double` (RFC-145 doc + typed sig),
/// `packages.*.hello` derivation constant, `nixosModules.default`, and
/// the synthetic builtins.
#[test]
fn snap_compile_mini_flake_ir() {
    let index = lower("mini-flake");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("nix_mini_flake_ir", sorted);
}

// ---------------------------------------------------------------------------
// Renderer snapshot: lib-style at width 80 (default)
// ---------------------------------------------------------------------------

/// Snapshot the rendered Nix surface for the lib-style fixture at 80 columns,
/// with docs enabled.
///
/// Exercises: HOF type comment, RFC-145 block doc-comment (`/** … */`),
/// curried form (`mapAttrs = f: set: …`), attrset-pattern form (`makeWrapper =
/// { name, version ? null, ... }:`), and inherit alias rendering.
#[test]
fn snap_render_libstyle_w80() {
    let index = lower("lib-style");
    let cx = RenderCtx::new(Language::Nix).with_width(80).with_docs(true);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("nix_render_libstyle_w80", rendered);
}

// ---------------------------------------------------------------------------
// Renderer snapshot: typed-fns at width 40 (narrow) and 100 (wide)
// ---------------------------------------------------------------------------

/// Snapshot the typed-fns fixture rendered at a narrow 40-column budget.
///
/// Covers line-breaking of long signatures (HOF, record patterns) so the
/// snapshot captures how the Wadler-Lindig printer reflows a long `::` type
/// comment and attrset-pattern formals.
#[test]
fn snap_render_typed_fns_w40() {
    let index = lower("typed-fns");
    let cx = RenderCtx::new(Language::Nix).with_width(40).with_docs(true);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("nix_render_typed_fns_w40", rendered);
}

/// Snapshot the typed-fns fixture rendered at a wide 100-column budget.
///
/// At 100 columns most signatures and formals fit on a single line, so the
/// two-width pair (40 / 100) together prove the printer respects the budget.
#[test]
fn snap_render_typed_fns_w100() {
    let index = lower("typed-fns");
    let cx = RenderCtx::new(Language::Nix).with_width(100).with_docs(true);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("nix_render_typed_fns_w100", rendered);
}

// ---------------------------------------------------------------------------
// Renderer snapshot: attrset-vals — nested modules + inherit alias
// ---------------------------------------------------------------------------

/// Snapshot the attrset-vals fixture at 80 columns with docs.
///
/// Exercises: `strings` namespace Module entry (renders as an unrendered
/// module tag — covered by the `// <unrendered module>` fallback), `trim` and
/// `pad` functions with typed sigs, `utils.trim` inherit alias, `defaultWidth`
/// constant, and synthetic builtins (untyped function stubs).
#[test]
fn snap_render_attrset_vals_w80() {
    let index = lower("attrset-vals");
    let cx = RenderCtx::new(Language::Nix).with_width(80).with_docs(true);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("nix_render_attrset_vals_w80", rendered);
}

// ---------------------------------------------------------------------------
// Renderer snapshot: mini-flake — flake root + lib.double
// ---------------------------------------------------------------------------

/// Snapshot the mini-flake fixture at 80 columns with docs.
///
/// Exercises: flake root Module (unrendered), `lib.double` typed function
/// with RFC-145 doc, derivation constants, and synthetic builtins.
#[test]
fn snap_render_mini_flake_w80() {
    let index = lower("mini-flake");
    let cx = RenderCtx::new(Language::Nix).with_width(80).with_docs(true);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("nix_render_mini_flake_w80", rendered);
}

// ---------------------------------------------------------------------------
// Renderer snapshot: docs disabled — compare lib-style with/without docs
// ---------------------------------------------------------------------------

/// Snapshot lib-style rendered without doc comments (`.with_docs(false)`).
///
/// The diff between this and `snap_render_libstyle_w80` captures exactly what
/// the `/** … */` doc-comment prepend adds, making regressions in the
/// documentation path easy to spot.
#[test]
fn snap_render_libstyle_no_docs() {
    let index = lower("lib-style");
    let cx = RenderCtx::new(Language::Nix).with_width(80).with_docs(false);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("nix_render_libstyle_no_docs", rendered);
}
