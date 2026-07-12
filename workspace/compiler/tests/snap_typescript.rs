//! Insta snapshot tests for the TypeScript language producer and renderer.
//!
//! ## Structure
//!
//! **Compile-target snapshots** (`snap_compile_*`):
//!   Lower a real TypeScript fixture tree via the deno-doc producer into an
//!   `ir::entry::Index`.  The index is determinised into a
//!   `BTreeMap<String, &Entry>` keyed by path display before snapshotting
//!   with `insta::assert_debug_snapshot!`.
//!
//! **Renderer snapshots** (`snap_render_*`):
//!   Take the same IR and render every renderable entry back to TypeScript
//!   surface via `render_entry`.  Sorted entries are joined into one string
//!   and snapshotted with `insta::assert_snapshot!`.  Two column-width
//!   variants (40 and 100) are provided for the `unions` fixture to capture
//!   line-breaking behaviour on long union signatures.
//!
//! ## Covered edge cases
//!
//! * Exported vs. non-exported (private) declarations (greeter fixture)
//! * Interfaces (TraitDef) with call/index signatures and optional members
//!   (interfaces fixture)
//! * Every TS construct → IR kind mapping:
//!   function, class, interface, type alias, enum, const, let, namespace
//!   (kinds fixture)
//! * Structural / keyof / mapped / conditional types (structural fixture)
//! * Namespaces, classes with private fields `#foo`, default export (toolkit)
//! * `package.json` `types` entry-point resolution (pkg-types fixture)
//! * Generic classes and interfaces, optional params, rest/variadic params,
//!   collection-type wrappers Vec<T>→T[], Option<T>→T|null,
//!   HashMap<K,V>→Record<K,V>, HashSet<T>→Set<T> (generics fixture)
//! * Union, intersection, and inline structural object types (unions fixture)
//! * JSDoc doc comments rendered via `.with_docs(true)` (jsdoc fixture)
//! * Two column widths (40 and 100) on the `classify` long union signature
//!
//! ## Generating / updating snapshots
//!
//! ```sh
//! INSTA_UPDATE=always buck2 test //workspace/compiler:test-snap_typescript
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use compiler::languages::typescript::generate_ir;
use compiler::render::{render_entry, Language, RenderCtx};
use ir::entry::{Index, NudoxPath};
use ir::kind::Entry;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers shared by all tests in this file
// ---------------------------------------------------------------------------

/// Resolve a TypeScript fixture path relative to this crate's manifest dir.
fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typescript")
}

/// Recursively copy a directory tree (`src` → `dest`), creating `dest` first.
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

/// Copy a named fixture into a `TempDir` and run the deno-doc producer on it.
///
/// Returns `(Index, TempDir)` — the `TempDir` must be kept alive for the
/// duration of each test so the temp files are not deleted under the producer.
fn lower(fixture: &str, package: &str) -> (Index, TempDir) {
    let dir = TempDir::new().expect("tempdir creation should succeed");
    copy_tree(&fixture_root().join(fixture), dir.path())
        .unwrap_or_else(|e| panic!("copy_tree({fixture}) failed: {e}"));
    let index = generate_ir(dir.path(), package)
        .unwrap_or_else(|e| panic!("generate_ir({fixture}) failed: {e}"));
    (index, dir)
}

/// Display a [`NudoxPath`] as a stable, sortable string key.
///
/// * `Local(p)`                    → `"p.display()"`
/// * `External { dependency, path }` → `"dependency:path.display()"`
fn path_display(path: &NudoxPath) -> String {
    match path {
        NudoxPath::Local(p) => p.display().to_string(),
        NudoxPath::External { path, dependency } => {
            format!("{dependency}:{}", path.display())
        }
    }
}

/// Collect `Index.entries_by_path` into a deterministic `BTreeMap<String, &Entry>`.
///
/// `FxHashMap` iteration order is nondeterministic; sorting by path display
/// guarantees the snapshot is stable across runs and platforms.
fn sorted_entries(index: &Index) -> BTreeMap<String, &Entry> {
    index
        .entries_by_path
        .iter()
        .map(|(p, e)| (path_display(p), e))
        .collect()
}

/// Render all entries (in sorted path order) into one joined string.
///
/// Entries that have no renderable surface produce the fallback
/// `// <unrendered …>` comment, which is intentionally included so the
/// snapshot documents which entry kinds the renderer skips.
fn render_all(index: &Index, cx: &RenderCtx) -> String {
    sorted_entries(index)
        .values()
        .map(|e| render_entry(e, cx))
        .collect::<Vec<_>>()
        .join("\n\n")
}

// ===========================================================================
// Compile-target snapshots
// ===========================================================================

// ---------------------------------------------------------------------------
// greeter: exported vs. private declarations
// ---------------------------------------------------------------------------

/// Snapshot the IR for the `greeter` fixture (export/private lowering).
///
/// Covers:
/// * `export const` → `Entry::Constant` (DEFAULT_GREETING)
/// * `export function` → `Entry::Function` (greet)
/// * `export class` → `Entry::RecordType` (Greeter)
/// * private `const` → `Entry::Constant` with `Visibility::Private`
///   (SECRET_GREETING)
/// * private `class` → `Entry::RecordType` with `Visibility::Private`
///   (WhisperGreeter)
#[test]
fn snap_compile_greeter_ir() {
    let (index, _dir) = lower("greeter", "greeter");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("ts_greeter_ir", sorted);
}

// ---------------------------------------------------------------------------
// interfaces: TraitDef with call/index signatures
// ---------------------------------------------------------------------------

/// Snapshot the IR for the `interfaces` fixture.
///
/// Covers:
/// * `export interface` → `Entry::TraitDef` (Greeter)
/// * Call signature → method named `__call`
/// * Index signature → method named `__index`
/// * Optional interface method (greet) with typed parameter
#[test]
fn snap_compile_interfaces_ir() {
    let (index, _dir) = lower("interfaces", "interfaces");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("ts_interfaces_ir", sorted);
}

// ---------------------------------------------------------------------------
// kinds: exhaustive construct → Entry variant mapping
// ---------------------------------------------------------------------------

/// Snapshot the IR for the `kinds` fixture (all TS construct kinds).
///
/// Covers the full mapping table:
/// * function → Entry::Function (doWork)
/// * class → Entry::RecordType (Widget)
/// * interface → Entry::TraitDef (Printable)
/// * type alias → Entry::TypeAlias (StringOrNumber)
/// * enum → Entry::SumType (Direction)
/// * const → Entry::Constant (MAX_SIZE)
/// * let → Entry::Variable (currentCount)
/// * namespace → Entry::Module (utils)
#[test]
fn snap_compile_kinds_ir() {
    let (index, _dir) = lower("kinds", "kinds");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("ts_kinds_ir", sorted);
}

// ---------------------------------------------------------------------------
// structural: keyof / mapped / conditional types
// ---------------------------------------------------------------------------

/// Snapshot the IR for the `structural` fixture.
///
/// Covers:
/// * `keyof T` → `Type::TypeOperator` (PersonKeys)
/// * Mapped type → `Type::Mapped` (ReadonlyPerson)
/// * Conditional type → `Type::Conditional` (IsString)
/// * `export interface` → `Entry::TraitDef` (Person)
#[test]
fn snap_compile_structural_ir() {
    let (index, _dir) = lower("structural", "structural");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("ts_structural_ir", sorted);
}

// ---------------------------------------------------------------------------
// toolkit: namespaces, private class fields, default export
// ---------------------------------------------------------------------------

/// Snapshot the IR for the `toolkit` fixture.
///
/// Covers:
/// * `export namespace` → `Entry::Module` (toolkit)
/// * `export class` with private `#fields` → `Entry::RecordType` (Builder)
/// * Class methods (push, build) registered as member entries
/// * `export default function` → reachable as `default` / `install`
#[test]
fn snap_compile_toolkit_ir() {
    let (index, _dir) = lower("toolkit", "toolkit");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("ts_toolkit_ir", sorted);
}

// ---------------------------------------------------------------------------
// pkg-types: package.json `types` entry-point resolution
// ---------------------------------------------------------------------------

/// Snapshot the IR for the `pkg-types` fixture.
///
/// Covers:
/// * `package.json` `"types": "mod.ts"` drives entry-point discovery
/// * `export const ENTRY` → `Entry::Constant` or `Entry::Variable`
/// * `export function fromTypesField` → `Entry::Function`
#[test]
fn snap_compile_pkg_types_ir() {
    let (index, _dir) = lower("pkg-types", "pkg-types");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("ts_pkg_types_ir", sorted);
}

// ---------------------------------------------------------------------------
// generics: generic classes/interfaces, optional/rest params, collections
// ---------------------------------------------------------------------------

/// Snapshot the IR for the `generics` fixture.
///
/// Covers:
/// * Generic class `Box<T>` → `Entry::RecordType` with generic parameters
/// * Generic interface `Stack<T>` → `Entry::TraitDef` with type parameter
/// * Multi-type-param interface `Store<K, V>` → `Entry::TraitDef`
/// * Optional parameter `limit?: number` in function signature
/// * Rest/variadic parameter `...sources: T[][]` in function signature
/// * `T | null` optional member type in interface
#[test]
fn snap_compile_generics_ir() {
    let (index, _dir) = lower("generics", "generics");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("ts_generics_ir", sorted);
}

// ---------------------------------------------------------------------------
// unions: union / intersection / inline-object types
// ---------------------------------------------------------------------------

/// Snapshot the IR for the `unions` fixture.
///
/// Covers:
/// * `type A | B` → `Entry::TypeAlias` (StringOrNumber, MaybeString, Tristate)
/// * `type A & B` intersection → `Entry::TypeAlias` (NamedAndAged)
/// * Inline structural object type in function return position
/// * Long union-typed parameter list (classify) for width-sensitive rendering
#[test]
fn snap_compile_unions_ir() {
    let (index, _dir) = lower("unions", "unions");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("ts_unions_ir", sorted);
}

// ---------------------------------------------------------------------------
// jsdoc: JSDoc doc comments
// ---------------------------------------------------------------------------

/// Snapshot the IR for the `jsdoc` fixture.
///
/// Covers:
/// * Multi-paragraph JSDoc on interfaces and functions carried into `docs`
/// * Optional parameter `precision?: number` in function signature
/// * Nested interface type reference (Coordinate inside Location)
#[test]
fn snap_compile_jsdoc_ir() {
    let (index, _dir) = lower("jsdoc", "jsdoc");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("ts_jsdoc_ir", sorted);
}

// ===========================================================================
// Renderer snapshots
// ===========================================================================

// ---------------------------------------------------------------------------
// greeter: export/private surface — docs on, width 80
// ---------------------------------------------------------------------------

/// Render the `greeter` fixture at 80 columns with docs enabled.
///
/// Exercises:
/// * `export interface …` for classes, `export function …` for free functions
/// * `export const …` (Constant entries are unrendered — produces fallback)
/// * JSDoc comment blocks prepended when `show_docs = true`
/// * Private entries appear with their fallback `// <unrendered …>` tag
#[test]
fn snap_render_greeter_w80() {
    let (index, _dir) = lower("greeter", "greeter");
    let cx = RenderCtx::new(Language::TypeScript)
        .with_width(80)
        .with_docs(true);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("ts_render_greeter_w80", rendered);
}

// ---------------------------------------------------------------------------
// interfaces: TraitDef surface — docs on, width 80
// ---------------------------------------------------------------------------

/// Render the `interfaces` fixture at 80 columns with docs enabled.
///
/// Exercises:
/// * `export interface Greeter { … }` surface with method signatures
/// * `__call` / `__index` synthetic names converted to camelCase
/// * Typed method parameters (`name: string`) appear in the surface
#[test]
fn snap_render_interfaces_w80() {
    let (index, _dir) = lower("interfaces", "interfaces");
    let cx = RenderCtx::new(Language::TypeScript)
        .with_width(80)
        .with_docs(true);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("ts_render_interfaces_w80", rendered);
}

// ---------------------------------------------------------------------------
// kinds: all-construct surface — docs on, width 80
// ---------------------------------------------------------------------------

/// Render the `kinds` fixture at 80 columns with docs enabled.
///
/// Exercises:
/// * `export function doWork(x: number): number;`
/// * `export interface Widget { … }` for class RecordType
/// * `export interface Printable { … }` for interface TraitDef
/// * `type StringOrNumber = …;` for TypeAlias
/// * Discriminated union tag-object surface for SumType (Direction)
/// * Unrendered fallback for Constant / Variable / Module entries
#[test]
fn snap_render_kinds_w80() {
    let (index, _dir) = lower("kinds", "kinds");
    let cx = RenderCtx::new(Language::TypeScript)
        .with_width(80)
        .with_docs(true);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("ts_render_kinds_w80", rendered);
}

// ---------------------------------------------------------------------------
// structural: structural types surface — docs on, width 80
// ---------------------------------------------------------------------------

/// Render the `structural` fixture at 80 columns with docs enabled.
///
/// Exercises:
/// * `type PersonKeys = …;` for keyof TypeOperator alias
/// * `type ReadonlyPerson = …;` for mapped type alias (renders as `unknown`)
/// * `type IsString<T> = …;` for conditional type alias (renders as `unknown`)
/// * `export interface Person { … }` for the base record
#[test]
fn snap_render_structural_w80() {
    let (index, _dir) = lower("structural", "structural");
    let cx = RenderCtx::new(Language::TypeScript)
        .with_width(80)
        .with_docs(true);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("ts_render_structural_w80", rendered);
}

// ---------------------------------------------------------------------------
// toolkit: namespaces + default export — docs on, width 80
// ---------------------------------------------------------------------------

/// Render the `toolkit` fixture at 80 columns with docs enabled.
///
/// Exercises:
/// * `export interface Builder { … }` with method signatures
/// * Namespace module fallback (`// <unrendered module>`)
/// * Default-exported function surface
#[test]
fn snap_render_toolkit_w80() {
    let (index, _dir) = lower("toolkit", "toolkit");
    let cx = RenderCtx::new(Language::TypeScript)
        .with_width(80)
        .with_docs(true);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("ts_render_toolkit_w80", rendered);
}

// ---------------------------------------------------------------------------
// generics: generic type params + collections — docs on, width 80
// ---------------------------------------------------------------------------

/// Render the `generics` fixture at 80 columns with docs enabled.
///
/// Exercises:
/// * `export interface Box<T> { … }` generic type parameter in interface
/// * `export interface Stack<T> { … }` with `T | null` optional return
/// * `export interface Store<K, V> { … }` two type params
/// * `export function process<T>(items: T[], limit?: number): [T[], number];`
///   — optional param and tuple return type
/// * `export function merge<T>(...sources: T[][]): T[];` — rest param and
///   array-of-array type
#[test]
fn snap_render_generics_w80() {
    let (index, _dir) = lower("generics", "generics");
    let cx = RenderCtx::new(Language::TypeScript)
        .with_width(80)
        .with_docs(true);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("ts_render_generics_w80", rendered);
}

// ---------------------------------------------------------------------------
// unions: union / intersection types — narrow (40 cols) and wide (100 cols)
// ---------------------------------------------------------------------------

/// Render the `unions` fixture at a narrow 40-column budget.
///
/// The long `classify` signature (`string | number | boolean | null |
/// undefined` parameter) should break across multiple lines at 40 cols.
#[test]
fn snap_render_unions_w40() {
    let (index, _dir) = lower("unions", "unions");
    let cx = RenderCtx::new(Language::TypeScript)
        .with_width(40)
        .with_docs(false);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("ts_render_unions_w40", rendered);
}

/// Render the `unions` fixture at a wide 100-column budget.
///
/// At 100 columns the same `classify` signature should fit inline; the pair
/// (40 / 100) together prove the printer respects the column budget.
#[test]
fn snap_render_unions_w100() {
    let (index, _dir) = lower("unions", "unions");
    let cx = RenderCtx::new(Language::TypeScript)
        .with_width(100)
        .with_docs(false);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("ts_render_unions_w100", rendered);
}

// ---------------------------------------------------------------------------
// jsdoc: doc-comment rendering — width 80 with and without docs
// ---------------------------------------------------------------------------

/// Render the `jsdoc` fixture at 80 columns with docs enabled.
///
/// Exercises:
/// * Multi-paragraph `/** … */` JSDoc blocks prepended to interfaces and
///   functions
/// * Optional parameter `precision?: number` rendered as `precision?: number`
/// * Nested type reference `Coordinate` appears as a plain type name
#[test]
fn snap_render_jsdoc_w80_with_docs() {
    let (index, _dir) = lower("jsdoc", "jsdoc");
    let cx = RenderCtx::new(Language::TypeScript)
        .with_width(80)
        .with_docs(true);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("ts_render_jsdoc_w80_with_docs", rendered);
}

/// Render the `jsdoc` fixture at 80 columns with docs disabled.
///
/// The diff between this and `snap_render_jsdoc_w80_with_docs` captures
/// exactly what the `/** … */` doc-comment prepend adds, making regressions
/// in the documentation rendering path easy to spot.
#[test]
fn snap_render_jsdoc_w80_no_docs() {
    let (index, _dir) = lower("jsdoc", "jsdoc");
    let cx = RenderCtx::new(Language::TypeScript)
        .with_width(80)
        .with_docs(false);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("ts_render_jsdoc_w80_no_docs", rendered);
}
