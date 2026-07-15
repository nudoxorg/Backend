//! Snapshot tests for the **C#** language pipeline.
//!
//! Two groups of snapshots live here:
//!
//!   **Compile-target snapshots** — C# source fixture → Roslyn oracle →
//!   `ir::entry::Index`. Snapshots pin the `Debug` rendering of the produced
//!   IR so any change to the C# lowering immediately shows up as a diff.
//!
//!   **Renderer snapshots** — that same IR rendered back to C# surface syntax
//!   via `compiler::render`. Snapshots pin the pretty-printed string so
//!   changes to the C# backend are caught immediately.
//!
//! # Determinism
//! `Index.entries_by_path` is an `FxHashMap` with non-deterministic iteration
//! order. Before snapshotting we always project into a `BTreeMap<String, &Entry>`
//! keyed by the path display string, giving a stable, sorted view.
//!
//! # Oracle dependency
//! The C# producer invokes `dotnet` with the prebuilt Roslyn oracle
//! (`csharp-oracle/oracle.dll`) that is shipped as a Buck2 resource alongside
//! this binary. Snapshot generation therefore requires:
//!   - A .NET 10 SDK `dotnet` on `PATH` (provided by the nix devshell; see
//!     `flake.nix` `csharp.dotnet` config entry).
//!   - The oracle publish directory to be present (wired by the
//!     `//workspace/compiler:snap_csharp` Buck target via
//!     `resources = { "csharp-oracle": … }`).
//!   - `DOTNET_CLI_HOME` set to a writable directory (sandbox gotcha — same
//!     class of fix as the Go `GOCACHE` issue; the Buck test target wires
//!     this automatically; see CSHARP-PLAN §1.2).
//!
//! Tests are **not** gated behind `#[ignore]` because the Buck test target
//! already provides the oracle as a resource. If you are running via
//! `cargo test` outside Buck without a .NET SDK, the tests will fail at the
//! oracle invocation step with an `OracleError` variant — that is expected
//! outside Buck.
//!
//! # Running / accepting snapshots
//! ```sh
//! INSTA_UPDATE=always buck2 test //workspace/compiler:snap_csharp
//! ```
//!
//! # NOTE: no `.snap` files are committed here.
//! Run the command above with `INSTA_UPDATE=always` once the .NET SDK and
//! oracle build are available to generate and accept the initial snapshots.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use compiler::compile::producer::LocalForgeContext;
use compiler::languages::csharp::lower_package;
use compiler::render::{render_entry, Language, RenderCtx};
use ir::entry::{Index, NudoxPath};
use ir::kind::Entry;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Root of C# test fixtures relative to CARGO_MANIFEST_DIR.
fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/csharp")
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

/// Copy a fixture directory into a TempDir and lower it through the C#
/// producer. Returns `(Index, TempDir)` — keep `TempDir` alive for the
/// duration of the test (it is cleaned on drop).
fn lower(fixture_name: &str) -> (Index, TempDir) {
    let dir = TempDir::new().expect("tempdir creation");
    copy_tree(&fixture_root().join(fixture_name), dir.path())
        .expect("fixture copy should succeed");
    let ctx = LocalForgeContext::default();
    let index = lower_package(&ctx, dir.path()).unwrap_or_else(|e| {
        // Print the full thiserror chain — top-level OracleExtractFailed alone
        // hides SpawnDotnetFailed / PublishError / etc. (same pattern as snap_go).
        use std::error::Error;
        let mut chain = format!("lower_package failed for fixture {fixture_name:?}: {e}");
        let mut src = e.source();
        while let Some(s) = src {
            chain.push_str(&format!("\n  caused by: {s}"));
            src = s.source();
        }
        panic!("{chain}");
    });
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

/// Build a C# `RenderCtx` with docs enabled and the given column width.
fn csharp_ctx(width: usize) -> RenderCtx {
    RenderCtx::new(Language::CSharp).with_width(width).with_docs(true)
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
// C1. Classes fixture: abstract Animal base + sealed Dog subclass
//
// Exercises:
//   - abstract class (Animal): abstract method, protected field,
//     virtual override, XML doc comments
//   - sealed concrete subclass (Dog): params[] varargs, optional parameter,
//     private helper method
// ---------------------------------------------------------------------------

/// Pins the IR from a C# fixture with an abstract base class and a sealed
/// concrete subclass. Exercises abstract classes, sealed classes, visibility
/// modifiers, and method declarations.
#[test]
fn snap_compile_classes_ir() {
    let (index, _dir) = lower("classes");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_classes_ir", sorted);
}

// ---------------------------------------------------------------------------
// C2. Structs fixture: readonly Point3D + mutable ValueRange
//
// Exercises:
//   - readonly struct (Point3D): IEquatable<T>, readonly members, static field
//   - mutable struct (ValueRange): non-readonly members, exception in ctor
// ---------------------------------------------------------------------------

/// Pins the IR from C# structs: a readonly value type and a mutable one.
#[test]
fn snap_compile_structs_ir() {
    let (index, _dir) = lower("structs");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_structs_ir", sorted);
}

// ---------------------------------------------------------------------------
// C3. Records fixture: positional Point + generic NamedPoint<L>
//
// Exercises:
//   - positional record (Point): primary constructor, compiler-synthesised
//     members, static factory with exception
//   - generic record (NamedPoint<L>): unbounded type parameter
// ---------------------------------------------------------------------------

/// Pins the IR from C# positional records — plain and generic variants.
#[test]
fn snap_compile_records_ir() {
    let (index, _dir) = lower("records");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_records_ir", sorted);
}

// ---------------------------------------------------------------------------
// C4. Interfaces fixture: IDrawable (DIM + static abstract) + IRepository<T,TKey>
//
// Exercises:
//   - default interface members (DIM): `Clear` and `ShapeKind` with bodies
//   - static abstract member: `ScaleTo(double, double)`
//   - generic interface with multiple type parameters and constraints
// ---------------------------------------------------------------------------

/// Pins the IR from C# interfaces including default members and a static
/// abstract member.
#[test]
fn snap_compile_interfaces_ir() {
    let (index, _dir) = lower("interfaces");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_interfaces_ir", sorted);
}

// ---------------------------------------------------------------------------
// C5. Enums fixture: Direction (: byte underlying) + FilePermissions ([Flags])
//     + JobStatus (plain)
//
// Exercises:
//   - enum with explicit underlying type `: byte`
//   - `[Flags]` bit-flag enum with composite members
//   - plain enum without an underlying-type annotation
// ---------------------------------------------------------------------------

/// Pins the IR from C# enums covering underlying types, [Flags], and plain
/// enumerations.
#[test]
fn snap_compile_enums_ir() {
    let (index, _dir) = lower("enums");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_enums_ir", sorted);
}

// ---------------------------------------------------------------------------
// C6. Delegates fixture: Transformer<in TIn, out TOut>, CompletionCallback,
//     BinaryPredicate<T>, RefAction<T>
//
// Exercises:
//   - generic delegate with covariant/contravariant type parameters
//   - delegate with nullable parameter
//   - delegate with `ref` parameter modifier
// ---------------------------------------------------------------------------

/// Pins the IR from C# delegate declarations including variance annotations
/// and ref parameters.
#[test]
fn snap_compile_delegates_ir() {
    let (index, _dir) = lower("delegates");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_delegates_ir", sorted);
}

// ---------------------------------------------------------------------------
// C7. Generics fixture: Box<T> (notnull), SortedPair<T : IComparable<T>>,
//     GenericConstraints (all constraint forms + allows ref struct),
//     IProducer<out T>, IConsumer<in T>
//
// Exercises:
//   - `notnull`, `class`, `new()`, `unmanaged`, `IComparable<T>`, `INumber<T>`
//   - C# 13 `allows ref struct` constraint
//   - covariant `out` and contravariant `in` type parameters
// ---------------------------------------------------------------------------

/// Pins the IR from generic C# types covering every constraint form and
/// variance annotations.
#[test]
fn snap_compile_generics_ir() {
    let (index, _dir) = lower("generics");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_generics_ir", sorted);
}

// ---------------------------------------------------------------------------
// C8. Properties fixture: Product (required/init/asymmetric accessors)
//     + KeyedStore<TValue> (indexer)
//
// Exercises:
//   - `required` init-only property
//   - `private set`, `internal set`, `protected set` asymmetric accessors
//   - computed property (getter only)
//   - indexer with `get`/`set`
// ---------------------------------------------------------------------------

/// Pins the IR from C# properties including asymmetric accessors, init-only,
/// required members, and an indexer.
#[test]
fn snap_compile_properties_ir() {
    let (index, _dir) = lower("properties");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_properties_ir", sorted);
}

// ---------------------------------------------------------------------------
// C9. Events fixture: StatusService (EventHandler<T>, Action<T>),
//     ExplicitEventHost (custom add/remove)
//
// Exercises:
//   - standard EventHandler-pattern events
//   - bare Action delegate events
//   - events with explicit add/remove accessors (lock-based)
// ---------------------------------------------------------------------------

/// Pins the IR from C# event declarations including standard and
/// explicit-accessor events.
#[test]
fn snap_compile_events_ir() {
    let (index, _dir) = lower("events");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_events_ir", sorted);
}

// ---------------------------------------------------------------------------
// C10. Operators fixture: Money struct (arithmetic + comparison + conversions
//      + checked operators)
//
// Exercises:
//   - binary arithmetic operators (`+`, `-`, unary `-`)
//   - `checked` arithmetic operators (C# 11)
//   - comparison operators (`==`, `!=`, `<`, `>`, `<=`, `>=`)
//   - `implicit` conversion from decimal
//   - `explicit` conversion to decimal
//   - `explicit checked` conversion to int (C# 11)
// ---------------------------------------------------------------------------

/// Pins the IR from a C# struct exercising the full operator suite including
/// checked operators and implicit/explicit conversions.
#[test]
fn snap_compile_operators_ir() {
    let (index, _dir) = lower("operators");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_operators_ir", sorted);
}

// ---------------------------------------------------------------------------
// C11. Extensions fixture: StringExtensions (classic) + C# 14 extension block
//
// Exercises:
//   - classic static extension class (Truncate, IsBlank, ToTitleCase)
//   - C# 14 `extension(string)` block (CountOccurrences, Repeat, TypeLabel)
// ---------------------------------------------------------------------------

/// Pins the IR from C# extension methods in both the classic static-class form
/// and the C# 14 extension-block form.
#[test]
fn snap_compile_extensions_ir() {
    let (index, _dir) = lower("extensions");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_extensions_ir", sorted);
}

// ---------------------------------------------------------------------------
// C12. Nullability fixture: AnnotatedStore (#nullable enable) + ObliviousStore
//      (#nullable disable)
//
// Exercises:
//   - fully annotated nullable reference types (string?)
//   - nullable value types (int?)
//   - oblivious nullability context (no annotation metadata)
// ---------------------------------------------------------------------------

/// Pins the IR from a C# file with mixed nullability contexts (annotated and
/// oblivious).
#[test]
fn snap_compile_nullability_ir() {
    let (index, _dir) = lower("nullability");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_nullability_ir", sorted);
}

// ---------------------------------------------------------------------------
// C13. XmlDoc fixture: XmlDocShowcase (all tags) + XmlDocShowcase<T>
//      + XmlDocChild (<inheritdoc/> and cref forms)
//
// Exercises:
//   - summary, remarks, param, typeparam, returns, value, exception
//   - example + code blocks
//   - c, see cref, see langword, paramref, list (bullet)
//   - inheritdoc/ (plain) and inheritdoc cref=
// ---------------------------------------------------------------------------

/// Pins the IR from a C# class exercising every XML documentation tag plus
/// inheritdoc resolution.
#[test]
fn snap_compile_xmldoc_ir() {
    let (index, _dir) = lower("xmldoc");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_xmldoc_ir", sorted);
}

// ---------------------------------------------------------------------------
// C14. Deprecated fixture: LegacyApi ([Obsolete] class + hard-error member)
//      + MixedDeprecation (field, method with DiagnosticId)
//
// Exercises:
//   - [Obsolete] on the class itself (non-error)
//   - [Obsolete(error: true)] on a method (hard-error deprecation)
//   - [Obsolete] with no message on a field
//   - [Obsolete] with DiagnosticId + UrlFormat
// ---------------------------------------------------------------------------

/// Pins the IR from C# [Obsolete] annotations at class, field, and method
/// level including hard-error and DiagnosticId forms.
#[test]
fn snap_compile_deprecated_ir() {
    let (index, _dir) = lower("deprecated");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_deprecated_ir", sorted);
}

// ---------------------------------------------------------------------------
// C15. Visibility fixture: all six C# access levels + explicit interface impl
//
// Exercises:
//   - public → Visibility::Public
//   - protected internal → Visibility::Protected (+ doc note)
//   - protected → Visibility::Protected
//   - internal → Visibility::Internal
//   - private protected → Visibility::Package (+ doc note)
//   - private → Visibility::Private
//   - explicit interface implementation → Visibility::Public (callable via iface)
// ---------------------------------------------------------------------------

/// Pins the IR from a class that declares all six C# access levels plus an
/// explicit interface implementation.
#[test]
fn snap_compile_visibility_ir() {
    let (index, _dir) = lower("visibility");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_visibility_ir", sorted);
}

// ---------------------------------------------------------------------------
// C16. Unsafe fixture: MemoryUtils (int*, delegate*<int,int>) + FixedBuffer
//
// Exercises:
//   - raw pointer parameters (`int*`, `byte*`)
//   - unmanaged function pointer (`delegate*<int, int>`)
//   - `fixed` buffer in a struct
// ---------------------------------------------------------------------------

/// Pins the IR from C# unsafe code — raw pointers and unmanaged function
/// pointers.
#[test]
fn snap_compile_unsafe_ir() {
    let (index, _dir) = lower("unsafe");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_unsafe_ir", sorted);
}

// ---------------------------------------------------------------------------
// C17. AsyncAwait fixture: DataClient (async Task<T?>) + Sequences (yield,
//      IAsyncEnumerable)
//
// Exercises:
//   - async method returning Task<string?>
//   - async method returning Task<string> (non-nullable)
//   - iterator method with `yield return` (IEnumerable<long>)
//   - async iterator with `yield return` (IAsyncEnumerable<int>)
// ---------------------------------------------------------------------------

/// Pins the IR from async/await and iterator patterns.
#[test]
fn snap_compile_asyncawait_ir() {
    let (index, _dir) = lower("asyncawait");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("csharp_asyncawait_ir", sorted);
}

// ===========================================================================
// RENDERER SNAPSHOTS
// ===========================================================================

// ---------------------------------------------------------------------------
// R1. Classes fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the C# surface rendered from the classes IR at 80 columns.
#[test]
fn snap_render_classes_w80() {
    let (index, _dir) = lower("classes");
    let cx = csharp_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_classes_w80", rendered);
}

// ---------------------------------------------------------------------------
// R2. Structs fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the C# surface rendered from the structs IR at 80 columns.
#[test]
fn snap_render_structs_w80() {
    let (index, _dir) = lower("structs");
    let cx = csharp_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_structs_w80", rendered);
}

// ---------------------------------------------------------------------------
// R3. Records fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the C# surface rendered from the records IR at 80 columns.
#[test]
fn snap_render_records_w80() {
    let (index, _dir) = lower("records");
    let cx = csharp_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_records_w80", rendered);
}

// ---------------------------------------------------------------------------
// R4. Interfaces fixture rendered at two widths (40 and 100)
//
// IDrawable and IRepository<T,TKey> have long signatures with constraints;
// at width 40 the printer must break across lines, at 100 they inline.
// ---------------------------------------------------------------------------

/// Pins C# interface rendering at 40 columns — forces type-constraint line
/// breaks.
#[test]
fn snap_render_interfaces_w40() {
    let (index, _dir) = lower("interfaces");
    let cx = csharp_ctx(40);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_interfaces_w40", rendered);
}

/// Pins C# interface rendering at 100 columns — inlines most constructs.
#[test]
fn snap_render_interfaces_w100() {
    let (index, _dir) = lower("interfaces");
    let cx = csharp_ctx(100);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_interfaces_w100", rendered);
}

// ---------------------------------------------------------------------------
// R5. Enums fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the C# surface rendered from enum declarations.
/// [Flags] enums should render with the attribute and composite members.
#[test]
fn snap_render_enums_w80() {
    let (index, _dir) = lower("enums");
    let cx = csharp_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_enums_w80", rendered);
}

// ---------------------------------------------------------------------------
// R6. Delegates fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the C# surface from delegate declarations.
#[test]
fn snap_render_delegates_w80() {
    let (index, _dir) = lower("delegates");
    let cx = csharp_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_delegates_w80", rendered);
}

// ---------------------------------------------------------------------------
// R7. Generics fixture rendered at two widths (40 and 100)
//
// Constraint clauses (`where T : INumber<T>, allows ref struct`) are verbose;
// at width 40 they break; at width 100 they inline.
// ---------------------------------------------------------------------------

/// Pins generic C# surface at 40 columns — forces constraint line breaks.
#[test]
fn snap_render_generics_w40() {
    let (index, _dir) = lower("generics");
    let cx = csharp_ctx(40);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_generics_w40", rendered);
}

/// Pins generic C# surface at 100 columns — inlines most constructs.
#[test]
fn snap_render_generics_w100() {
    let (index, _dir) = lower("generics");
    let cx = csharp_ctx(100);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_generics_w100", rendered);
}

// ---------------------------------------------------------------------------
// R8. Properties fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the C# surface from properties — asymmetric accessors, init,
/// required, and indexer should all appear.
#[test]
fn snap_render_properties_w80() {
    let (index, _dir) = lower("properties");
    let cx = csharp_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_properties_w80", rendered);
}

// ---------------------------------------------------------------------------
// R9. Events fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the C# surface from event declarations.
#[test]
fn snap_render_events_w80() {
    let (index, _dir) = lower("events");
    let cx = csharp_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_events_w80", rendered);
}

// ---------------------------------------------------------------------------
// R10. Operators fixture rendered at two widths (40 and 100)
//
// The Money struct has many operators; at 40 columns the signature list
// wraps aggressively, useful for testing the line-break logic.
// ---------------------------------------------------------------------------

/// Pins operator C# rendering at 40 columns.
#[test]
fn snap_render_operators_w40() {
    let (index, _dir) = lower("operators");
    let cx = csharp_ctx(40);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_operators_w40", rendered);
}

/// Pins operator C# rendering at 100 columns.
#[test]
fn snap_render_operators_w100() {
    let (index, _dir) = lower("operators");
    let cx = csharp_ctx(100);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_operators_w100", rendered);
}

// ---------------------------------------------------------------------------
// R11. Extensions fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the C# surface from both classic and C# 14 extension methods.
#[test]
fn snap_render_extensions_w80() {
    let (index, _dir) = lower("extensions");
    let cx = csharp_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_extensions_w80", rendered);
}

// ---------------------------------------------------------------------------
// R12. Nullability fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the C# surface from the nullability fixture; annotated types should
/// carry `?` markers, oblivious types should render without.
#[test]
fn snap_render_nullability_w80() {
    let (index, _dir) = lower("nullability");
    let cx = csharp_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_nullability_w80", rendered);
}

// ---------------------------------------------------------------------------
// R13. XmlDoc fixture rendered at width 80 — docs enabled
// ---------------------------------------------------------------------------

/// Pins the C# surface for XmlDocShowcase with `/// <summary>` comments
/// rendered (`with_docs(true)`) above each declaration.
#[test]
fn snap_render_xmldoc_w80() {
    let (index, _dir) = lower("xmldoc");
    let cx = csharp_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_xmldoc_w80", rendered);
}

// ---------------------------------------------------------------------------
// R14. Deprecated fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the C# surface from the deprecated fixture.
/// [Obsolete] entries should render with an `[Obsolete]` attribute line.
#[test]
fn snap_render_deprecated_w80() {
    let (index, _dir) = lower("deprecated");
    let cx = csharp_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_deprecated_w80", rendered);
}

// ---------------------------------------------------------------------------
// R15. Visibility fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the C# surface showing all six visibility keywords and the explicit
/// interface implementation.
#[test]
fn snap_render_visibility_w80() {
    let (index, _dir) = lower("visibility");
    let cx = csharp_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_visibility_w80", rendered);
}

// ---------------------------------------------------------------------------
// R16. Unsafe fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the C# surface from unsafe code — pointer and function-pointer types.
#[test]
fn snap_render_unsafe_w80() {
    let (index, _dir) = lower("unsafe");
    let cx = csharp_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_unsafe_w80", rendered);
}

// ---------------------------------------------------------------------------
// R17. AsyncAwait fixture rendered at width 80
// ---------------------------------------------------------------------------

/// Pins the C# surface from async/await and iterator patterns.
#[test]
fn snap_render_asyncawait_w80() {
    let (index, _dir) = lower("asyncawait");
    let cx = csharp_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("csharp_render_asyncawait_w80", rendered);
}
