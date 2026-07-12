//! Pipeline part: **TypeScript source → surface IR** (`compiler::languages::typescript`).
//!
//! Specs for the deno-doc lowering: entry-point / declaration-root discovery and
//! the TS-specific lowerings (exports vs private, namespaces, interfaces,
//! structural types, package.json `types`).

use std::fs;
use std::path::{Path, PathBuf};

use compiler::languages::typescript::generate_ir;
use ir::entry::{Index, NudoxPath};
use ir::kind::{Entry, Visibility};
use ir::ty::Type;
use tempfile::TempDir;

// ─── Fixture plumbing ────────────────────────────────────────────────────────

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typescript")
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
fn lower(fixture: &str, package: &str) -> (Index, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    copy_tree(&fixture_root().join(fixture), dir.path()).expect("fixture copies");
    let index = generate_ir(dir.path(), package).expect("lowering succeeds");
    (index, dir)
}

fn local(path: &str) -> NudoxPath {
    NudoxPath::Local(PathBuf::from(path))
}

fn entry_by_name<'i>(index: &'i Index, name: &str) -> &'i Entry {
    index
        .entries_by_path
        .values()
        .find(|e| e.name() == name)
        .unwrap_or_else(|| {
            let known: Vec<_> = index.entries_by_path.values().map(|e| e.name()).collect();
            panic!("expected entry named {name}, have {known:?}")
        })
}

fn visibility_of(entry: &Entry) -> &Visibility {
    match entry {
        Entry::Module(s) => &s.visibility,
        Entry::RecordType(s) => &s.visibility,
        Entry::Function(s) => &s.visibility,
        Entry::TraitDef(s) => &s.visibility,
        Entry::TraitImpl(s) => &s.visibility,
        Entry::Constant(s) => &s.visibility,
        Entry::Variable(s) => &s.visibility,
        Entry::Macro(s) => &s.visibility,
        Entry::PrimitiveType(s) => &s.visibility,
        Entry::Field(s) => &s.visibility,
        Entry::Event(s) => &s.visibility,
        Entry::Info(s) => &s.visibility,
        Entry::UnionType(s) => &s.visibility,
        Entry::TypeAlias(s) => &s.visibility,
        Entry::SumType(s) => &s.visibility,
    }
}

fn type_alias_type<'i>(index: &'i Index, name: &str) -> &'i Type {
    match entry_by_name(index, name) {
        Entry::TypeAlias(symbol) => &symbol.inner,
        other => panic!("expected TypeAlias {name}, got {other}"),
    }
}

// ─── Specs ───────────────────────────────────────────────────────────────────

/// A regular module lowers exported items and drops private ones.
///
/// Arrange: `greeter.ts` (exported `DEFAULT_GREETING`, `Greeter`, `greet`;
///   private `SECRET_GREETING`, `WhisperGreeter`).
/// Assert: exported items appear; `SECRET_GREETING`/`WhisperGreeter` are
///   `Visibility::Private`.
#[test]
fn regular_module_lowers_exports() {
    let (index, _dir) = lower("greeter", "greeter");

    // `const` declarations must yield Entry::Constant, not Entry::Variable or
    // any other variant.  A wrong mapping here (e.g. a hardcoded placeholder)
    // would produce silent wrong-output data in every downstream consumer.
    assert!(
        matches!(entry_by_name(&index, "DEFAULT_GREETING"), Entry::Constant(_)),
        "const DEFAULT_GREETING must lower as Entry::Constant, not Variable or other"
    );
    assert!(
        matches!(entry_by_name(&index, "Greeter"), Entry::RecordType(_)),
        "class Greeter must lower as Entry::RecordType"
    );
    assert!(
        matches!(entry_by_name(&index, "greet"), Entry::Function(_)),
        "function greet must lower as Entry::Function"
    );

    let secret = entry_by_name(&index, "SECRET_GREETING");
    assert_eq!(
        *visibility_of(secret),
        Visibility::Private,
        "SECRET_GREETING must be Private"
    );

    let whisper = entry_by_name(&index, "WhisperGreeter");
    assert_eq!(
        *visibility_of(whisper),
        Visibility::Private,
        "WhisperGreeter must be Private"
    );
}

/// Namespaces, classes (with private `#fields`), and default exports lower.
///
/// Arrange: `toolkit.ts` (`namespace toolkit`, `Builder` with `#segments`,
///   default-export `install`).
/// Assert: the namespace becomes a `Module`, `Builder` a `RecordType`, and the
///   default export is reachable.
#[test]
fn namespaces_classes_and_default_exports_lower() {
    let (index, _dir) = lower("toolkit", "toolkit");

    assert!(
        matches!(entry_by_name(&index, "toolkit"), Entry::Module(_)),
        "namespace toolkit lowers as a Module"
    );

    assert!(
        matches!(entry_by_name(&index, "Builder"), Entry::RecordType(_)),
        "Builder lowers as a RecordType"
    );

    // Class methods are registered as member Function entries (deno-doc does not
    // always surface private `#fields` as record properties).
    let has_push = index.entries_by_path.values().any(|e| e.name() == "push");
    let has_build = index.entries_by_path.values().any(|e| e.name() == "build");
    assert!(
        has_push && has_build,
        "Builder methods push/build should be present among {:?}",
        index.entries_by_path.values().map(|e| e.name()).collect::<Vec<_>>()
    );

    // Default export is reachable as `default` and/or the function name `install`.
    let has_default = index
        .entries_by_path
        .values()
        .any(|e| e.name() == "default" || e.name() == "install");
    assert!(
        has_default,
        "default export (default/install) must be present among {:?}",
        index.entries_by_path.values().map(|e| e.name()).collect::<Vec<_>>()
    );
}

/// An interface becomes a `TraitDef`, with call/index signatures as members.
///
/// Assert: `Greeter` lowers to `Entry::TraitDef`; a call signature lowers to a
///   `TraitMethod` named `__call`, an index signature to `__index`.
#[test]
fn interface_becomes_trait_def_with_signatures() {
    let (index, _dir) = lower("interfaces", "interfaces");

    let greeter = match entry_by_name(&index, "Greeter") {
        Entry::TraitDef(symbol) => symbol,
        other => panic!("Greeter should be a TraitDef, got {other}"),
    };

    let methods = greeter
        .inner
        .required_methods
        .as_ref()
        .expect("Greeter carries required methods");

    assert!(
        methods.iter().any(|m| m.name == "__call"),
        "call signature lowers as __call: {:?}",
        methods.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
    assert!(
        methods.iter().any(|m| m.name == "__index"),
        "index signature lowers as __index: {:?}",
        methods.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
    assert!(
        methods.iter().any(|m| m.name == "greet"),
        "ordinary method greet is present"
    );
}

/// Declaration roots are discovered from `package.json` (types/typings/exports).
///
/// Assert: given a package whose `package.json` sets `types: "mod.ts"`, the
///   entry point resolves to `mod.ts` and its exports are lowered.
#[test]
fn declaration_roots_resolved_from_package_json() {
    let (index, _dir) = lower("pkg-types", "pkg-types");

    // Exports live under the module stem of mod.ts → "mod".
    assert!(
        index.entries_by_path.contains_key(&local("mod"))
            || index.entries_by_path.values().any(|e| e.name() == "mod"),
        "mod module root must be present: {:?}",
        index.entries_by_path.keys().collect::<Vec<_>>()
    );
    assert!(
        matches!(entry_by_name(&index, "ENTRY"), Entry::Constant(_) | Entry::Variable(_)),
        "ENTRY export from mod.ts is lowered"
    );
    assert!(
        matches!(entry_by_name(&index, "fromTypesField"), Entry::Function(_)),
        "fromTypesField export from mod.ts is lowered"
    );
}

/// TS structural types (keyof / mapped / conditional) are first-class IR types.
///
/// Assert: a `keyof` lowers to `Type::TypeOperator`, a mapped type to
///   `Type::Mapped`, a conditional to `Type::Conditional`.
#[test]
fn structural_types_are_first_class() {
    let (index, _dir) = lower("structural", "structural");

    assert!(
        matches!(type_alias_type(&index, "PersonKeys"), Type::TypeOperator(_)),
        "keyof lowers to Type::TypeOperator"
    );
    assert!(
        matches!(type_alias_type(&index, "ReadonlyPerson"), Type::Mapped(_)),
        "mapped type lowers to Type::Mapped"
    );
    assert!(
        matches!(type_alias_type(&index, "IsString"), Type::Conditional(_)),
        "conditional type lowers to Type::Conditional"
    );
}

/// Exhaustive TS-construct → `ir::kind::Entry` kind-mapping table.
///
/// This test is the canonical guard against "placeholder SymbolKind" regressions
/// where every symbol silently lands on the same variant.  It covers the full
/// set of TypeScript declaration forms the producer handles:
///
/// | TypeScript construct | Expected Entry variant |
/// |----------------------|------------------------|
/// | `function`           | `Entry::Function`      |
/// | `class`              | `Entry::RecordType`    |
/// | `interface`          | `Entry::TraitDef`      |
/// | `type` alias         | `Entry::TypeAlias`     |
/// | `enum`               | `Entry::SumType`       |
/// | `const`              | `Entry::Constant`      |
/// | `let`                | `Entry::Variable`      |
/// | `namespace`          | `Entry::Module`        |
#[test]
fn ts_kind_mapping_is_correct_for_every_construct() {
    let (index, _dir) = lower("kinds", "kinds");

    assert!(
        matches!(entry_by_name(&index, "doWork"), Entry::Function(_)),
        "function doWork must lower as Entry::Function; got {:?}",
        entry_by_name(&index, "doWork")
    );
    assert!(
        matches!(entry_by_name(&index, "Widget"), Entry::RecordType(_)),
        "class Widget must lower as Entry::RecordType; got {:?}",
        entry_by_name(&index, "Widget")
    );
    assert!(
        matches!(entry_by_name(&index, "Printable"), Entry::TraitDef(_)),
        "interface Printable must lower as Entry::TraitDef; got {:?}",
        entry_by_name(&index, "Printable")
    );
    assert!(
        matches!(entry_by_name(&index, "StringOrNumber"), Entry::TypeAlias(_)),
        "type alias StringOrNumber must lower as Entry::TypeAlias; got {:?}",
        entry_by_name(&index, "StringOrNumber")
    );
    assert!(
        matches!(entry_by_name(&index, "Direction"), Entry::SumType(_)),
        "enum Direction must lower as Entry::SumType; got {:?}",
        entry_by_name(&index, "Direction")
    );
    assert!(
        matches!(entry_by_name(&index, "MAX_SIZE"), Entry::Constant(_)),
        "const MAX_SIZE must lower as Entry::Constant, not Variable or other; got {:?}",
        entry_by_name(&index, "MAX_SIZE")
    );
    assert!(
        matches!(entry_by_name(&index, "currentCount"), Entry::Variable(_)),
        "let currentCount must lower as Entry::Variable, not Constant or other; got {:?}",
        entry_by_name(&index, "currentCount")
    );
    assert!(
        matches!(entry_by_name(&index, "utils"), Entry::Module(_)),
        "namespace utils must lower as Entry::Module; got {:?}",
        entry_by_name(&index, "utils")
    );
}
