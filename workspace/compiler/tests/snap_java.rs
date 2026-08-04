//! Snapshot tests for the **Java** language pipeline.
//!
//! Two groups of snapshots live here:
//!
//!   **Compile-target snapshots** — Java source fixture → doclet oracle jar →
//!   `ir::entry::Index`. Snapshots pin the `Debug` rendering of the produced IR
//!   so any change to the Java lowering immediately shows up as a diff.
//!
//!   **Renderer snapshots** — that same IR rendered back to Java surface syntax
//!   via `compiler::render`. Snapshots pin the pretty-printed string so changes
//!   to the Java backend are caught immediately.
//!
//! # Determinism
//! `Index.entries_by_path` is an `FxHashMap` with non-deterministic iteration
//! order. Before snapshotting we always project into a `BTreeMap<String, &Entry>`
//! keyed by the path display string, giving a stable, sorted view.
//!
//! # Oracle dependency
//! The Java producer invokes `javadoc` with the prebuilt doclet jar
//! (`java-oracle.jar`) that is shipped as a Buck2 resource alongside this
//! binary. Snapshot generation therefore requires:
//!   - A JDK 17+ `javadoc` on `PATH` (provided by the nix devshell).
//!   - The oracle jar to be present (wired by the `//workspace/compiler:test`
//!     Buck target via `resources = { "java-oracle.jar": … }`).
//! Tests are **not** gated behind `#[ignore]` because the Buck test target
//! already provides the oracle jar as a resource. If you are running via
//! `cargo test` outside Buck without a JDK, the tests will fail at the oracle
//! invocation step with an `OracleError::NoJavaSources` or
//! `DocletError::ResourceNotFound` variant — that is expected outside Buck.
//!
//! # Running / accepting snapshots
//! ```sh
//! INSTA_UPDATE=always buck2 test //workspace/compiler:snap_java
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use compiler::compile::producer::LocalForgeContext;
use compiler::languages::java::lower_package;
use compiler::render::{render_entry, Language, RenderCtx};
use ir::entry::{Index, NudoxPath};
use ir::kind::Entry;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Root of Java test fixtures relative to CARGO_MANIFEST_DIR.
fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/java")
}

/// Recursively copy `src` directory tree to `dest`.
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

/// Copy a fixture directory into a TempDir and lower it through the Java
/// producer. Returns `(Index, TempDir)` — keep `TempDir` alive for the
/// duration of the test (it is cleaned on drop).
fn lower(fixture_name: &str) -> (Index, TempDir) {
    let dir = TempDir::new().expect("tempdir creation");
    copy_tree(&fixture_root().join(fixture_name), dir.path())
        .expect("fixture copy should succeed");
    let ctx = LocalForgeContext::default();
    let index = lower_package(&ctx, dir.path())
        .unwrap_or_else(|e| panic!("lower_package failed for fixture {fixture_name:?}: {e}"));
    (index, dir)
}

/// Collect `index.entries_by_path` into a sorted `BTreeMap<String, &Entry>`.
///
/// Keys are path display strings: `NudoxPath::Local(p)` → `p.display()`,
/// `NudoxPath::External { path, dependency }` → `dependency::path`.
/// The `BTreeMap` makes the Debug snapshot deterministic regardless of the
/// underlying `FxHashMap` iteration order.
fn sorted_entries(index: &Index) -> BTreeMap<String, &Entry> {
    index
        .entries_by_path
        .iter()
        .map(|(path, entry)| {
            let key = match path {
                NudoxPath::Local(p) => p.display().to_string(),
                NudoxPath::External { path, dependency } => {
                    format!("{}::{}", dependency, path.display())
                }
            };
            (key, entry)
        })
        .collect()
}

/// Build a Java `RenderCtx` with docs enabled and the given column width.
fn java_ctx(width: usize) -> RenderCtx {
    RenderCtx::new(Language::Java).with_width(width).with_docs(true)
}

/// Render all entries in sorted, deterministic order, joining them with a
/// separator so the whole extraction surfaces in one snapshot.
fn render_all(index: &Index, cx: &RenderCtx) -> String {
    let sorted = sorted_entries(index);
    sorted
        .values()
        .map(|e| render_entry(e, cx))
        .collect::<Vec<_>>()
        .join("\n\n// ---\n\n")
}

// ===========================================================================
// COMPILE-TARGET SNAPSHOTS
// ===========================================================================

// ---------------------------------------------------------------------------
// C1. Classes fixture: abstract base class + concrete subclass
//
// Exercises:
//   - abstract class (Animal): abstract method, public/protected fields,
//     public instance methods, Javadoc doc comments, throws clause
//   - concrete subclass (Dog): extends, field visibility (public/protected/
//     package-private/private), method overloads, overriding, @Deprecated
//     (on learnTrick/internalReset/bark), varargs via bark
// ---------------------------------------------------------------------------

/// Pins the IR from a Java fixture with an abstract base class and concrete
/// subclass. Exercises abstract classes, visibility modifiers, and method
/// declarations.
#[test]
fn snap_compile_classes_ir() {
    let (index, _dir) = lower("classes");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("java_classes_ir", sorted);
}

// ---------------------------------------------------------------------------
// C2. Interfaces fixture: Drawable (default method) + Serializable
//
// Exercises:
//   - interface declarations (TraitDef)
//   - required methods vs default methods (`default void clear(...)`)
//   - Javadoc on interface and individual methods
//   - method with array param (byte[])
// ---------------------------------------------------------------------------

/// Pins the IR from Java interfaces including a `default` method.
#[test]
fn snap_compile_interfaces_ir() {
    let (index, _dir) = lower("interfaces");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("java_interfaces_ir", sorted);
}

// ---------------------------------------------------------------------------
// C3. Enums fixture: Direction (with method) + Status
//
// Exercises:
//   - enum declarations (SumType) with named constants
//   - Javadoc on enum constants
//   - instance method on enum (opposite(), isTerminal())
// ---------------------------------------------------------------------------

/// Pins the IR from Java enums with named constants and instance methods.
#[test]
fn snap_compile_enums_ir() {
    let (index, _dir) = lower("enums");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("java_enums_ir", sorted);
}

// ---------------------------------------------------------------------------
// C4. Generics fixture: Box<T> + SortedPair<T extends Comparable<T>>
//
// Exercises:
//   - unbounded generic class (`class Box<T>`)
//   - bounded generic class (`class SortedPair<T extends Comparable<T>>`)
//   - generic static method (`static <E> List<Box<E>> wrapAll(List<E>)`)
//   - collection type uses: List<T>, Optional<T>
// ---------------------------------------------------------------------------

/// Pins the IR from generic Java classes — unbounded and bounded type params.
#[test]
fn snap_compile_generics_ir() {
    let (index, _dir) = lower("generics");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("java_generics_ir", sorted);
}

// ---------------------------------------------------------------------------
// C5. Records fixture: Point (compact constructor) + NamedPoint<L> (generic)
//
// Exercises:
//   - Java 16+ record declarations
//   - compact constructor with validation
//   - generic record (`record NamedPoint<L>(L label, double x, double y)`)
//   - static factory method on record
// ---------------------------------------------------------------------------

/// Pins the IR from Java records (Java 16+ feature).
#[test]
fn snap_compile_records_ir() {
    let (index, _dir) = lower("records");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("java_records_ir", sorted);
}

// ---------------------------------------------------------------------------
// C6. Sealed interface fixture: Shape (with nested record impls)
//
// Exercises:
//   - sealed interface declaration
//   - permitted subtypes list
//   - nested record types implementing a sealed interface
//   - multiple method signatures on interface
// ---------------------------------------------------------------------------

/// Pins the IR from a sealed Java interface with nested record subtypes.
#[test]
fn snap_compile_sealed_ir() {
    let (index, _dir) = lower("sealed");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("java_sealed_ir", sorted);
}

// ---------------------------------------------------------------------------
// C7. Collections fixture: Registry<V>
//
// Exercises:
//   - HashMap<String,V>, HashSet<String>, List<V>, Map<String,String>,
//     Optional<V> → known-type mappings (Map<K,V>, Set<T>, List<T>,
//     Map<K,V>, Optional<T>)
//   - method with Map<String,V> param
// ---------------------------------------------------------------------------

/// Pins the IR from a class that exercises all major collection type uses:
/// List, Map, Set, Optional.
#[test]
fn snap_compile_collections_ir() {
    let (index, _dir) = lower("collections");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("java_collections_ir", sorted);
}

// ---------------------------------------------------------------------------
// C8. Javadoc fixture: MathUtils (static utility, Javadoc, varargs)
//
// Exercises:
//   - rich Javadoc: @param, @return, @throws, @since, @author, inline {@code}
//   - varargs parameter (`int... values`)
//   - static final utility class pattern (private constructor)
// ---------------------------------------------------------------------------

/// Pins the IR from a static utility class with rich Javadoc and varargs.
#[test]
fn snap_compile_javadoc_ir() {
    let (index, _dir) = lower("javadoc");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("java_javadoc_ir", sorted);
}

// ---------------------------------------------------------------------------
// C9. Deprecated fixture: LegacyApi (@Deprecated class + members)
//
// Exercises:
//   - @Deprecated on the class itself
//   - @Deprecated on a field (static final constant)
//   - @Deprecated on a method
//   - non-deprecated method in same class
// ---------------------------------------------------------------------------

/// Pins the IR from a class with @Deprecated annotations at class, field,
/// and method level.
#[test]
fn snap_compile_deprecated_ir() {
    let (index, _dir) = lower("deprecated");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("java_deprecated_ir", sorted);
}

// ---------------------------------------------------------------------------
// C10. Visibility fixture: all four Java access modifiers
//
// Exercises:
//   - public → Visibility::Public
//   - protected → Visibility::Protected
//   - package-private (no keyword) → Visibility::Package
//   - private → Visibility::Private
// ---------------------------------------------------------------------------

/// Pins the IR from a class that declares all four Java access levels.
#[test]
fn snap_compile_visibility_ir() {
    let (index, _dir) = lower("visibility");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("java_visibility_ir", sorted);
}

// ===========================================================================
// RENDERER SNAPSHOTS
// ===========================================================================

// ---------------------------------------------------------------------------
// R1. Classes fixture rendered at width 80 (default)
// ---------------------------------------------------------------------------

/// Pins the Java surface rendered from the classes IR at 80 columns.
#[test]
fn snap_render_classes_w80() {
    let (index, _dir) = lower("classes");
    let cx = java_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("java_render_classes_w80", rendered);
}

// ---------------------------------------------------------------------------
// R2. Interfaces fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the Java surface rendered from the interfaces IR at 80 columns.
/// Should emit `interface Drawable { ... }` with method stubs.
#[test]
fn snap_render_interfaces_w80() {
    let (index, _dir) = lower("interfaces");
    let cx = java_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("java_render_interfaces_w80", rendered);
}

// ---------------------------------------------------------------------------
// R3. Enums fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the Java surface rendered from the enums IR.
/// Enum→SumType renders as sealed interface + record variants.
#[test]
fn snap_render_enums_w80() {
    let (index, _dir) = lower("enums");
    let cx = java_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("java_render_enums_w80", rendered);
}

// ---------------------------------------------------------------------------
// R4. Generics fixture rendered at two widths (40 and 100)
//
// The bounded generic `SortedPair<T extends Comparable<T>>` has a long
// signature; at width 40 the printer must break across lines, at width 100
// it inlines. This pair exercises the line-breaking behavior explicitly.
// ---------------------------------------------------------------------------

/// Pins generic Java surface at 40 columns — forces line breaks in type params.
#[test]
fn snap_render_generics_w40() {
    let (index, _dir) = lower("generics");
    let cx = java_ctx(40);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("java_render_generics_w40", rendered);
}

/// Pins generic Java surface at 100 columns — inlines most constructs.
#[test]
fn snap_render_generics_w100() {
    let (index, _dir) = lower("generics");
    let cx = java_ctx(100);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("java_render_generics_w100", rendered);
}

// ---------------------------------------------------------------------------
// R5. Records fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the Java surface rendered from record declarations.
#[test]
fn snap_render_records_w80() {
    let (index, _dir) = lower("records");
    let cx = java_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("java_render_records_w80", rendered);
}

// ---------------------------------------------------------------------------
// R6. Sealed interface fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the Java surface from the sealed interface; should emit
/// `sealed interface Shape permits ...` and one `record` per variant.
#[test]
fn snap_render_sealed_w80() {
    let (index, _dir) = lower("sealed");
    let cx = java_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("java_render_sealed_w80", rendered);
}

// ---------------------------------------------------------------------------
// R7. Collections fixture rendered at two widths
//
// Registry<V>'s method signatures use List<V>, Map<String,V>, Set<String>,
// Optional<V>. At width 40 argument lists must break; at 100 they inline.
// ---------------------------------------------------------------------------

/// Pins collection-type Java rendering at 40 columns.
#[test]
fn snap_render_collections_w40() {
    let (index, _dir) = lower("collections");
    let cx = java_ctx(40);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("java_render_collections_w40", rendered);
}

/// Pins collection-type Java rendering at 100 columns.
#[test]
fn snap_render_collections_w100() {
    let (index, _dir) = lower("collections");
    let cx = java_ctx(100);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("java_render_collections_w100", rendered);
}

// ---------------------------------------------------------------------------
// R8. Javadoc fixture rendered at width 80 — docs enabled
// ---------------------------------------------------------------------------

/// Pins the Java surface for MathUtils with Javadoc comments rendered
/// (`with_docs(true)`) as `/** ... */` blocks above each declaration.
#[test]
fn snap_render_javadoc_w80() {
    let (index, _dir) = lower("javadoc");
    let cx = java_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("java_render_javadoc_w80", rendered);
}

// ---------------------------------------------------------------------------
// R9. Deprecated fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the Java surface from the deprecated fixture.
/// @Deprecated entries should render with `@Deprecated` marker lines.
#[test]
fn snap_render_deprecated_w80() {
    let (index, _dir) = lower("deprecated");
    let cx = java_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("java_render_deprecated_w80", rendered);
}

// ---------------------------------------------------------------------------
// R10. Visibility fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the Java surface showing all four visibility keywords (public,
/// protected, package-private/no-prefix, private).
#[test]
fn snap_render_visibility_w80() {
    let (index, _dir) = lower("visibility");
    let cx = java_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("java_render_visibility_w80", rendered);
}
