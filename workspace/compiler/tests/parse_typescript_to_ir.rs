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
use ir::record::{Field, FieldKey};
use ir::ty::{LiteralKind, Type};
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
        Entry::TypeAlias(symbol) => &symbol.inner.target,
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

// ─── "exceed deno" regression suite ─────────────────────────────────────────
//
// Each test below pins an IR capability that deno_doc could not express.
// The fixture lives in `tests/fixtures/typescript/exceed/mod.ts`.

/// String literal type preserves the exact value `"foo"`.
///
/// deno_doc could not express this: it collapsed all string literal types to
/// the bare `string` keyword, discarding the value entirely.
#[test]
fn literal_string_type_preserves_value() {
    let (index, _dir) = lower("exceed", "exceed");

    match type_alias_type(&index, "Lit") {
        Type::Literal(lit) => {
            assert_eq!(
                lit.kind,
                LiteralKind::String,
                "Lit: expected LiteralKind::String, got {:?}",
                lit.kind
            );
            assert_eq!(
                lit.value, "\"foo\"",
                "Lit: expected value == \"\\\"foo\\\"\", got {:?}",
                lit.value
            );
        }
        other => panic!(
            "type Lit should lower to Type::Literal, got {other:?}\n\
             (deno_doc could not express this — it collapsed to bare `string`)"
        ),
    }
}

/// Numeric literal type preserves the exact value `42`.
///
/// deno_doc could not express this: it collapsed all numeric literal types to
/// the bare `number` keyword, discarding the value entirely.
#[test]
fn literal_number_type_preserves_value() {
    let (index, _dir) = lower("exceed", "exceed");

    match type_alias_type(&index, "Num") {
        Type::Literal(lit) => {
            assert_eq!(
                lit.kind,
                LiteralKind::Number,
                "Num: expected LiteralKind::Number, got {:?}",
                lit.kind
            );
            assert_eq!(
                lit.value, "42",
                "Num: expected value == \"42\", got {:?}",
                lit.value
            );
        }
        other => panic!(
            "type Num should lower to Type::Literal, got {other:?}\n\
             (deno_doc could not express this — it collapsed to bare `number`)"
        ),
    }
}

/// Boolean literal type preserves the exact value `true`.
///
/// deno_doc could not express this: it collapsed all boolean literal types to
/// the bare `boolean` keyword, discarding the value entirely.
#[test]
fn literal_boolean_type_preserves_value() {
    let (index, _dir) = lower("exceed", "exceed");

    match type_alias_type(&index, "Flag") {
        Type::Literal(lit) => {
            assert_eq!(
                lit.kind,
                LiteralKind::Boolean,
                "Flag: expected LiteralKind::Boolean, got {:?}",
                lit.kind
            );
            assert_eq!(
                lit.value, "true",
                "Flag: expected value == \"true\", got {:?}",
                lit.value
            );
        }
        other => panic!(
            "type Flag should lower to Type::Literal, got {other:?}\n\
             (deno_doc could not express this — it collapsed to bare `boolean`)"
        ),
    }
}

/// Template-literal type lowers to `Type::TemplateLiteral`, not `Primitive(String)`.
///
/// deno_doc could not express this: it flattened all template-literal types to
/// the plain `string` primitive, losing the structural interpolation.
#[test]
fn template_literal_type_is_first_class() {
    let (index, _dir) = lower("exceed", "exceed");

    match type_alias_type(&index, "Tmpl") {
        Type::TemplateLiteral(tl) => {
            // `id-${string}` → quasis = ["id-", ""], types = [Primitive(String)]
            assert!(
                !tl.quasis.is_empty(),
                "Tmpl: TemplateLiteralType must carry at least one quasi, got {:?}",
                tl.quasis
            );
            assert!(
                !tl.types.is_empty(),
                "Tmpl: TemplateLiteralType must carry at least one interpolated type, got {:?}",
                tl.types
            );
        }
        other => panic!(
            "type Tmpl should lower to Type::TemplateLiteral, got {other:?}\n\
             (deno_doc could not express this — it flattened to `string`)"
        ),
    }
}

/// `typeof base` lowers to `Type::TypeQuery` carrying the binding name.
///
/// deno_doc could not express this: it stringified `typeof x` to a plain
/// `TypeReference` whose identifier happened to spell out the source text,
/// losing the `typeof` semantics.
#[test]
fn typeof_query_is_first_class() {
    let (index, _dir) = lower("exceed", "exceed");

    match type_alias_type(&index, "Q") {
        Type::TypeQuery(tq) => {
            assert_eq!(
                tq.name, "base",
                "Q: TypeQuery name should be \"base\", got {:?}",
                tq.name
            );
        }
        other => panic!(
            "type Q should lower to Type::TypeQuery, got {other:?}\n\
             (deno_doc could not express this — it emitted a fake TypeReference)"
        ),
    }
}

/// Named-tuple labels are preserved in `Type::NamedTuple`.
///
/// deno_doc could not express this: it dropped tuple element labels and emitted
/// a plain `Type::Tuple`, making `[first: string, second: number]` and
/// `[string, number]` indistinguishable.
#[test]
fn named_tuple_labels_are_preserved() {
    let (index, _dir) = lower("exceed", "exceed");

    match type_alias_type(&index, "Pair") {
        Type::NamedTuple(members) => {
            assert_eq!(
                members.len(),
                2,
                "Pair: expected 2 named-tuple members, got {} — members: {:?}",
                members.len(),
                members.iter().map(|m| &m.label).collect::<Vec<_>>()
            );
            let labels: Vec<Option<&str>> =
                members.iter().map(|m| m.label.as_deref()).collect();
            assert_eq!(
                labels,
                vec![Some("first"), Some("second")],
                "Pair: expected labels [Some(\"first\"), Some(\"second\")], got {:?}",
                labels
            );
        }
        other => panic!(
            "type Pair should lower to Type::NamedTuple, got {other:?}\n\
             (deno_doc could not express this — it dropped labels into a plain Tuple)"
        ),
    }
}

/// Class `Rich` surfaces the TC39 `accessor y` as a field with the `accessor` decorator.
///
/// deno_doc could not express this: it had no knowledge of the TC39
/// `accessor` keyword and silently skipped `AccessorProperty` nodes.
#[test]
fn class_accessor_property_is_surfaced() {
    let (index, _dir) = lower("exceed", "exceed");

    let rich = match entry_by_name(&index, "Rich") {
        Entry::RecordType(sym) => sym,
        other => panic!("expected Entry::RecordType for Rich, got {other:?}"),
    };

    let accessor_field = rich.inner.fields.iter().find(|f| {
        if let Field::Known(kf) = f {
            if let FieldKey::Ident(ref name) = kf.key {
                return name == "y";
            }
        }
        false
    });
    let kf = match accessor_field {
        Some(Field::Known(kf)) => kf,
        _ => panic!(
            "Rich: expected a field named `y` (accessor) in fields {:?}\n\
             (deno_doc could not express this — AccessorProperty nodes were silently skipped)",
            rich.inner
                .fields
                .iter()
                .filter_map(|f| {
                    if let Field::Known(kf) = f {
                        if let FieldKey::Ident(ref s) = kf.key { Some(s.clone()) } else { None }
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        ),
    };
    assert!(
        kf.attributes.decorators.iter().any(|d| d == "accessor"),
        "Rich.y: field must carry the \"accessor\" decorator, got {:?}",
        kf.attributes.decorators
    );
}

/// Class `Rich` emits a synthetic `__static` entry for its static-initialiser block.
///
/// deno_doc could not express this: `static { }` blocks have no type-level
/// surface and were silently dropped, making them invisible in the IR.
#[test]
fn class_static_block_is_synthesised() {
    let (index, _dir) = lower("exceed", "exceed");

    // The synthetic entry is named `__static` and lives as an Entry::Function
    // in the index (member of Rich).
    let has_static = index
        .entries_by_path
        .values()
        .any(|e| e.name() == "__static" && matches!(e, Entry::Function(_)));

    assert!(
        has_static,
        "Rich: a synthetic Entry::Function named `__static` must be present for the static block;\
         found entries: {:?}\n\
         (deno_doc could not express this — static {{ }} blocks were silently dropped)",
        index
            .entries_by_path
            .values()
            .map(|e| e.name())
            .collect::<Vec<_>>()
    );
}

/// Constructor-body `this.z = 1` is synthesised as a field on class `Rich`.
///
/// deno_doc could not express this: only `PropertyDefinition` nodes were
/// scanned for fields; bare `this.x = …` assignments in the constructor body
/// were invisible to the type extractor.
#[test]
fn constructor_this_assignment_synthesises_field() {
    let (index, _dir) = lower("exceed", "exceed");

    let rich = match entry_by_name(&index, "Rich") {
        Entry::RecordType(sym) => sym,
        other => panic!("expected Entry::RecordType for Rich, got {other:?}"),
    };

    let has_z = rich.inner.fields.iter().any(|f| {
        if let Field::Known(kf) = f {
            if let FieldKey::Ident(ref name) = kf.key {
                return name == "z";
            }
        }
        false
    });
    assert!(
        has_z,
        "Rich: synthesised field `z` (from `this.z = 1` in constructor) must be present;\
         actual fields: {:?}\n\
         (deno_doc could not express this — constructor-body this-assignments were invisible)",
        rich.inner
            .fields
            .iter()
            .filter_map(|f| {
                if let Field::Known(kf) = f {
                    if let FieldKey::Ident(ref s) = kf.key { Some(s.clone()) } else { None }
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    );
}

/// `get x()` getter is lowered as an `Entry::Function` member named `x` on `Rich`.
///
/// deno_doc surfaced getters inconsistently; the OXC producer treats them as
/// regular `MethodDefinition` entries with kind `Get`, lowered to `Entry::Function`.
#[test]
fn class_getter_is_lowered_as_function() {
    let (index, _dir) = lower("exceed", "exceed");

    // The getter `x` should be present as a member entry (local_path = ["Rich", "x"]).
    let has_getter = index
        .entries_by_path
        .values()
        .any(|e| e.name() == "x" && matches!(e, Entry::Function(_)));

    assert!(
        has_getter,
        "Rich: getter `x` must lower to Entry::Function;\
         found function entries: {:?}",
        index
            .entries_by_path
            .values()
            .filter(|e| matches!(e, Entry::Function(_)))
            .map(|e| e.name())
            .collect::<Vec<_>>()
    );
}

/// `export default function run()` surfaces as a named entry (`run` or `default`).
///
/// deno_doc could not express this: default-exported named function declarations
/// were silently dropped — neither `run` nor `default` appeared in its output.
#[test]
fn default_export_function_is_present() {
    let (index, _dir) = lower("exceed", "exceed");

    let has_default_fn = index
        .entries_by_path
        .values()
        .any(|e| (e.name() == "run" || e.name() == "default") && matches!(e, Entry::Function(_)));

    assert!(
        has_default_fn,
        "exceed: default-exported function `run` must be present as Entry::Function \
         under name \"run\" or \"default\"; found: {:?}\n\
         (deno_doc could not express this — default-exported declarations were silently dropped)",
        index
            .entries_by_path
            .values()
            .map(|e| e.name())
            .collect::<Vec<_>>()
    );
}
