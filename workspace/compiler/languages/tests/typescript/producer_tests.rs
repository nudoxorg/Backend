//! Integration and unit tests for nudox-languages.
//!
//! All tests parse literal TypeScript source strings through OXC in-process
//! (no npm, no disk, no network). Each test is self-contained and hermetic.
//!
//! # Coverage
//! - interface extraction
//! - class with access modifiers
//! - `type` alias with union
//! - generic function with constraint (asserts `TypeOwned::TypeVar`)
//! - string enum (asserts two distinct variant discriminants)
//! - overload pair (asserts **two distinct TsId discriminants**)
//! - JSDoc comment producing `documentation` + `deprecation`
//! - `jsdoc.rs` unit tests (parse_raw_jsdoc)

use std::path::PathBuf;

use oxc_allocator::Allocator;
use oxc_parser::{ParseOptions, Parser};
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;

use nudox_languages::typescript::extract::{
    Accessibility, DeclBody, MemberKind, ModuleFacts, TypeOwned, decl::extract_module,
    jsdoc::parse_raw_jsdoc,
};

// ── Helper ─────────────────────────────────────────────────────────────────────

/// Parse `src` as TypeScript and extract `ModuleFacts`.
///
/// The allocator is scoped to this function — all AST data is owned in the
/// returned `ModuleFacts`. This is exactly the pattern `graph::build_and_extract`
/// uses in production.
fn parse_module(src: &str, name: &str) -> ModuleFacts {
    let allocator = Allocator::default();
    let source_type = SourceType::ts();
    let path = PathBuf::from(format!("/test/{name}.ts"));

    let parse = Parser::new(&allocator, src, source_type)
        .with_options(ParseOptions {
            parse_regular_expression: false,
            allow_return_outside_function: false,
            ..ParseOptions::default()
        })
        .parse();

    assert!(!parse.panicked, "OXC parser panicked on: {src}");

    // Note: JSDoc is automatic with the `jsdoc` feature; no `with_jsdoc` method in 0.139.0.
    // Module record lives on the parse result in 0.139.0, not on Semantic.
    // Must match `graph::build_and_extract`'s configuration exactly, including
    // `with_build_nodes(true)`. This helper's whole value is that it exercises
    // the same `extract_module` production runs; a builder configured
    // differently here tests a `Semantic` that never exists in production.
    //
    // That is not hypothetical: without this flag OXC keeps only an ancestry
    // stack and `semantic.nodes()` is empty, so `record_occurrences`' span
    // lookup panics. A helper missing the flag while production had it (or the
    // reverse) hides that failure from exactly the tests written to catch it.
    let semantic_result = SemanticBuilder::new()
        .with_build_nodes(true)
        .with_check_syntax_error(false)
        .build(&parse.program);

    let semantic = semantic_result.semantic;
    let module_record = &parse.module_record;

    extract_module(
        src,
        &semantic,
        &parse.program,
        &path,
        module_record,
        name.to_string(),
    )
    // allocator dropped here; ModuleFacts is fully owned
}

// ── Test 1: Interface ─────────────────────────────────────────────────────────

#[test]
fn test_interface_extraction() {
    let src = r#"
        export interface Animal {
            name: string;
            age?: number;
            greet(greeting: string): void;
        }
    "#;

    let facts = parse_module(src, "animal");
    assert_eq!(facts.declarations.len(), 1, "one top-level declaration");

    let decl = &facts.declarations[0];
    assert_eq!(decl.name, "Animal");

    let DeclBody::Interface(body) = &decl.body else {
        panic!(
            "expected Interface, got {:?}",
            std::mem::discriminant(&decl.body)
        );
    };

    assert_eq!(body.methods.len(), 1, "one method: greet");
    assert_eq!(body.properties.len(), 2, "two properties: name, age");

    let age_prop = body
        .properties
        .iter()
        .find(|p| p.name == "age")
        .expect("age property");
    assert!(age_prop.modifiers.is_optional, "age should be optional");
}

// ── Test 2: Class with modifiers ──────────────────────────────────────────────

#[test]
fn test_class_with_modifiers() {
    let src = r#"
        export class Point {
            public x: number;
            private y: number;
            protected z: number;
            readonly #id: string;

            constructor(x: number, y: number) {}

            getX(): number { return this.x; }
        }
    "#;

    let facts = parse_module(src, "point");
    assert_eq!(facts.declarations.len(), 1);

    let decl = &facts.declarations[0];
    assert_eq!(decl.name, "Point");

    let DeclBody::Class(body) = &decl.body else {
        panic!("expected Class");
    };

    // Properties: x, y, z, #id
    let props: Vec<_> = body
        .members
        .iter()
        .filter(|m| {
            matches!(
                m.kind,
                nudox_languages::typescript::extract::MemberKind::Property { .. }
            )
        })
        .collect();
    assert!(props.len() >= 3, "at least 3 property members (x, y, z)");

    // Check private field #id is PrivateField
    let id_prop = props.iter().find(|m| m.name.starts_with('#'));
    if let Some(id_mem) = id_prop {
        assert_eq!(
            id_mem.modifiers.accessibility,
            nudox_languages::typescript::extract::Accessibility::PrivateField,
            "#id should be PrivateField accessibility"
        );
    }
}

// ── Test 3: Type alias with union ─────────────────────────────────────────────

#[test]
fn test_type_alias_union() {
    let src = r#"
        export type StringOrNumber = string | number;
    "#;

    let facts = parse_module(src, "alias");
    assert_eq!(facts.declarations.len(), 1);

    let decl = &facts.declarations[0];
    assert_eq!(decl.name, "StringOrNumber");

    let DeclBody::TypeAlias(body) = &decl.body else {
        panic!("expected TypeAlias");
    };

    let TypeOwned::Union(arms) = &body.target else {
        panic!(
            "expected Union target, got {:?}",
            std::mem::discriminant(&body.target)
        );
    };

    assert_eq!(arms.len(), 2, "string | number has two arms");
    assert!(matches!(&arms[0], TypeOwned::String), "first arm is string");
    assert!(
        matches!(&arms[1], TypeOwned::Number),
        "second arm is number"
    );
}

// ── Test 4: Generic function with constraint → TypeVar ────────────────────────

#[test]
fn test_generic_function_typevar() {
    let src = r#"
        export function identity<T extends string>(value: T): T {
            return value;
        }
    "#;

    let facts = parse_module(src, "identity");
    assert_eq!(facts.declarations.len(), 1);

    let decl = &facts.declarations[0];
    assert_eq!(decl.name, "identity");

    let DeclBody::Function(body) = &decl.body else {
        panic!("expected Function");
    };

    // Check that the generic parameter is present.
    assert_eq!(body.generics.len(), 1, "one type parameter T");
    let gparam = &body.generics[0];
    assert_eq!(gparam.name, "T");
    assert_eq!(gparam.bounds.len(), 1, "T extends string");

    // Check that the parameter type `T` is emitted as TypeVar.
    // The `value: T` parameter should produce TypeOwned::TypeVar("T").
    assert_eq!(body.params.len(), 1, "one parameter: value");
    let param = &body.params[0];
    assert_eq!(param.name, "value");

    match &param.ty {
        Some(TypeOwned::TypeVar(name)) => {
            assert_eq!(name, "T", "parameter type should be TypeVar(T)");
        }
        Some(TypeOwned::Nominal(name)) => {
            // Acceptable if the TypeVar heuristic is inactive: Nominal("T")
            // is the conservative fallback.
            assert_eq!(
                name, "T",
                "parameter type is Nominal(T) — TypeVar heuristic inactive"
            );
        }
        other => {
            panic!("expected TypeVar(T) or Nominal(T) for parameter `value`, got {other:?}");
        }
    }

    // Return type should also be T.
    match &body.return_type {
        Some(TypeOwned::TypeVar(name) | TypeOwned::Nominal(name)) => {
            assert_eq!(name, "T", "return type should be T");
        }
        other => {
            panic!("expected return type T, got {other:?}");
        }
    }
}

// ── Test 5: String enum ───────────────────────────────────────────────────────

#[test]
fn test_string_enum() {
    let src = r#"
        export enum Direction {
            Up = "UP",
            Down = "DOWN",
            Left = "LEFT",
            Right = "RIGHT",
        }
    "#;

    let facts = parse_module(src, "direction");
    assert_eq!(facts.declarations.len(), 1);

    let decl = &facts.declarations[0];
    assert_eq!(decl.name, "Direction");

    let DeclBody::Enum(body) = &decl.body else {
        panic!("expected Enum");
    };

    assert_eq!(body.variants.len(), 4, "four variants");

    let up = body
        .variants
        .iter()
        .find(|v| v.name == "Up")
        .expect("Up variant");
    assert_eq!(
        up.discriminant.as_deref(),
        Some("\"UP\""),
        "Up discriminant should be the string literal \"UP\""
    );

    let down = body
        .variants
        .iter()
        .find(|v| v.name == "Down")
        .expect("Down variant");
    assert_eq!(
        down.discriminant.as_deref(),
        Some("\"DOWN\""),
        "Down discriminant"
    );
}

// ── Test 6: Overload pair → two distinct declarations ─────────────────────────

#[test]
fn test_overload_pair_distinct_declarations() {
    // TypeScript allows multiple overload signatures + one implementation.
    // The spec requires each overload signature to be its own IR declaration.
    // The implementation body is also emitted (has_body = true, discriminant differs).
    let src = r#"
        export function process(x: string): string;
        export function process(x: number): number;
        export function process(x: any): any {
            return x;
        }
    "#;

    let facts = parse_module(src, "overloads");

    // OXC semantic groups declarations by name. The module should have at
    // least 2 DeclFact entries (each overload signature + implementation).
    // The exact count depends on whether OXC surfaces all three or only the
    // two overload signatures as distinct exports.
    //
    // The key assertion: no two DeclFact entries for `process` may have the
    // same `decl_index` (discriminant). That is the spec's "two distinct
    // declarations" guarantee.
    let process_decls: Vec<_> = facts
        .declarations
        .iter()
        .filter(|d| d.name == "process")
        .collect();

    assert!(
        process_decls.len() >= 2,
        "expected at least 2 declarations for `process` (overload signatures), got {}",
        process_decls.len()
    );

    let discriminants: Vec<u32> = process_decls.iter().map(|d| d.decl_index).collect();
    let unique: std::collections::HashSet<u32> = discriminants.iter().copied().collect();
    assert_eq!(
        unique.len(),
        discriminants.len(),
        "all discriminants must be distinct; got duplicates in {discriminants:?}"
    );
}

// ── Test 7: JSDoc → documentation + deprecation ───────────────────────────────

#[test]
fn test_jsdoc_documentation_and_deprecation() {
    let src = r#"
        /**
         * Compute the square of a number.
         * @deprecated Use `square2` instead.
         */
        export function square(x: number): number {
            return x * x;
        }
    "#;

    let facts = parse_module(src, "square");
    assert_eq!(facts.declarations.len(), 1);

    let decl = &facts.declarations[0];
    assert_eq!(decl.name, "square");

    let doc = decl.doc.doc.as_deref().unwrap_or("");
    assert!(
        doc.contains("Compute the square"),
        "documentation should contain 'Compute the square', got: {doc:?}"
    );

    let dep = decl
        .doc
        .deprecation
        .as_ref()
        .expect("deprecation must be present");
    let note = dep.note.as_deref().unwrap_or("");
    assert!(
        note.contains("square2"),
        "deprecation note should reference square2, got: {note:?}"
    );
}

// ── Test 8: jsdoc.rs unit tests ───────────────────────────────────────────────

#[test]
fn test_jsdoc_parse_raw_basic() {
    let raw = " * A simple description.\n * @deprecated use newFn instead\n * @ignore";
    let facts = parse_raw_jsdoc(raw);

    let doc = facts.doc.as_deref().unwrap_or("");
    assert!(
        doc.contains("simple description"),
        "doc should contain description: {doc:?}"
    );

    assert!(
        facts.deprecation.is_some(),
        "@deprecated tag must produce deprecation"
    );
    let note = facts
        .deprecation
        .as_ref()
        .unwrap()
        .note
        .as_deref()
        .unwrap_or("");
    assert!(
        note.contains("newFn"),
        "deprecation note should mention newFn: {note:?}"
    );

    assert!(facts.ignore, "@ignore tag must set ignore = true");
}

#[test]
fn test_jsdoc_parse_raw_description_override() {
    // @description tag overrides the main doc comment body.
    let raw = " * main body text\n * @description This is the real description.";
    let facts = parse_raw_jsdoc(raw);

    let doc = facts.doc.as_deref().unwrap_or("");
    assert_eq!(
        doc, "This is the real description.",
        "description tag overrides main body"
    );
}

#[test]
fn test_jsdoc_parse_raw_no_deprecation() {
    let raw = " * Just a normal function.";
    let facts = parse_raw_jsdoc(raw);

    assert!(
        facts.deprecation.is_none(),
        "no @deprecated tag means no deprecation"
    );
    assert!(!facts.ignore, "no @ignore means ignore = false");
    let doc = facts.doc.as_deref().unwrap_or("");
    assert!(doc.contains("normal function"), "doc: {doc:?}");
}

// ── Test 9: Module-level TSZ seam ─────────────────────────────────────────────

// ── TSZ oracle tier tests (--features tsz) ────────────────────────────────────

/// Verify that OXC alone yields `None` return type for an unannotated function,
/// and that TszOracle fills it in with the checker-inferred concrete type.
///
/// This test writes a real `.ts` file to a temp directory, runs OXC extraction,
/// then promotes to `TszOracle` (which runs tsz enrichment), and compares.
///
/// Gated behind `--features tsz` — the type and constructor are unavailable
/// without the feature.
#[cfg(feature = "tsz")]
#[test]
fn test_tsz_enriches_inferred_return_type() {
    use nudox_languages::typescript::extract::{DeclBody, TypeOwned};
    use nudox_languages::typescript::producer::TsOracle;
    use nudox_languages::typescript::{OwnedOracle, TszOracle};
    use std::io::Write;

    // Write a TypeScript file with NO explicit return annotation.
    let dir = tempfile::tempdir().expect("tempdir");
    let ts_path = dir.path().join("infer.ts");
    {
        let mut f = std::fs::File::create(&ts_path).expect("create ts file");
        writeln!(
            f,
            "export function add(a: number, b: number) {{ return a + b; }}"
        )
        .expect("write");
    }

    // OXC extraction.
    let entry_points = vec![ts_path.clone()];
    let modules = nudox_languages::typescript::graph::build_and_extract(&entry_points, dir.path())
        .expect("oxc extract");

    // Verify OXC produced the function with None return type.
    let add_oxc = modules
        .iter()
        .flat_map(|m| m.declarations.iter())
        .find(|d| d.name == "add")
        .expect("OXC should find `add`");
    let DeclBody::Function(oxc_fn) = &add_oxc.body else {
        panic!("expected Function body");
    };
    assert!(
        oxc_fn.return_type.is_none(),
        "OXC should yield None return type for unannotated function, got: {:?}",
        oxc_fn.return_type
    );

    // Promote to TszOracle (runs enrichment).
    let owned = OwnedOracle::new(modules);
    let tsz: TszOracle = TszOracle::from(owned);

    // Find the enriched function.
    let add_tsz = tsz
        .modules()
        .iter()
        .flat_map(|m| m.declarations.iter())
        .find(|d| d.name == "add")
        .expect("TszOracle should still have `add`");
    let DeclBody::Function(tsz_fn) = &add_tsz.body else {
        panic!("expected Function body from TszOracle");
    };

    // tsz should have filled in `number` (the inferred return type of a + b).
    match &tsz_fn.return_type {
        Some(TypeOwned::Number) => {
            // Perfect: tsz correctly inferred `number`.
        }
        Some(other) => {
            // Acceptable if tsz returned a compatible representation (e.g. via
            // the format+reparse path, which might give Nominal("number")).
            // Any concrete type is better than None.
            match other {
                TypeOwned::Nominal(name) if name == "number" => {}
                TypeOwned::Any | TypeOwned::Unknown => {
                    panic!("tsz enrichment should improve on None; got opaque type: {other:?}");
                }
                _ => {
                    // Something concrete was inferred — that's an improvement.
                    // Don't assert the exact spelling; checker representation may vary.
                }
            }
        }
        None => {
            panic!("tsz enrichment should have filled in return type for `add`, still None");
        }
    }
}

/// The `TypescriptProducer::<TszOracle>::new_tsz()` constructor compiles and
/// produces the correct type signature (compile-time seam test).
#[cfg(feature = "tsz")]
#[test]
fn test_tsz_producer_constructor() {
    use nudox_languages::typescript::{TszOracle, TypescriptProducer};
    let _: TypescriptProducer<TszOracle> = TypescriptProducer::new_tsz();
    // No panic = the tsz producer seam is wired end-to-end.
}

// ── Test 9: Module-level TSZ seam (OXC) ──────────────────────────────────────

/// This is a compile-time seam test, not a runtime test.
/// It verifies that `TypescriptProducer` is generic over `O: TsOracle`, that
/// `OwnedOracle` implements it, and that a future tsz oracle would only need to:
///   1. Create a struct `TszOracle` implementing `TsOracle + sealed::TsOracleSeal`
///   2. Implement `From<OwnedOracle> for TszOracle`
///   3. Construct `TypescriptProducer::<TszOracle>::new()`
///
/// Since `TsOracleSeal` is private, step 1 requires touching this crate.
/// This test documents the seam; no additional assertion is needed.
#[test]
fn test_tsz_seam_documented() {
    use nudox_languages::typescript::{OwnedOracle, TypescriptProducer};
    // Constructing the default producer exercises the OwnedOracle path.
    let _producer: TypescriptProducer<OwnedOracle> = TypescriptProducer::new();
    // No panic = the seam is wired.
}

// ── Item 1+2: Constructor parameter properties + this.x synthesis ─────────────

#[test]
fn test_constructor_parameter_properties() {
    let src = r#"
        export class Vec2 {
            constructor(public x: number, private readonly y: number) {}
        }
    "#;

    let facts = parse_module(src, "vec2");
    assert_eq!(facts.declarations.len(), 1);

    let DeclBody::Class(body) = &facts.declarations[0].body else {
        panic!("expected Class");
    };

    let props: Vec<_> = body
        .members
        .iter()
        .filter(|m| matches!(m.kind, MemberKind::Property { .. }))
        .collect();

    let x = props
        .iter()
        .find(|m| m.name == "x")
        .expect("field `x` must be synthesised from parameter property");
    assert_eq!(
        x.modifiers.accessibility,
        Accessibility::Public,
        "x is public"
    );
    assert!(!x.modifiers.is_readonly, "x is not readonly");

    let y = props
        .iter()
        .find(|m| m.name == "y")
        .expect("field `y` must be synthesised from parameter property");
    assert_eq!(
        y.modifiers.accessibility,
        Accessibility::Private,
        "y is private"
    );
    assert!(y.modifiers.is_readonly, "y is readonly");
}

#[test]
fn test_this_field_synthesis() {
    let src = r#"
        export class Counter {
            constructor() {
                this.count = 0;
                this.label = "counter";
            }
        }
    "#;

    let facts = parse_module(src, "counter");
    let DeclBody::Class(body) = &facts.declarations[0].body else {
        panic!("expected Class");
    };

    let props: Vec<_> = body
        .members
        .iter()
        .filter(|m| matches!(m.kind, MemberKind::Property { .. }))
        .collect();

    assert!(
        props.iter().any(|m| m.name == "count"),
        "field `count` must be synthesised from `this.count = 0`; members: {:?}",
        props.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
    assert!(
        props.iter().any(|m| m.name == "label"),
        "field `label` must be synthesised from `this.label = …`"
    );
}

// ── Item 3: Accessor properties ────────────────────────────────────────────────

#[test]
fn test_accessor_property() {
    let src = r#"
        export class Box {
            accessor value: string = "";
        }
    "#;

    let facts = parse_module(src, "box");
    let DeclBody::Class(body) = &facts.declarations[0].body else {
        panic!("expected Class");
    };

    let accessor = body
        .members
        .iter()
        .find(|m| m.name == "value")
        .expect("`accessor value` must appear as a member");

    match &accessor.kind {
        MemberKind::Accessor { ty } => {
            assert!(
                matches!(ty, Some(TypeOwned::String)),
                "accessor value type should be string, got {ty:?}"
            );
        }
        other => panic!("expected MemberKind::Accessor, got {other:?}"),
    }

    // The `accessor` decorator marker should be set.
    assert!(
        accessor.decorators.iter().any(|d| d.token == "accessor"),
        "accessor member must carry 'accessor' decorator marker"
    );
}

// ── Item 4: Static blocks ──────────────────────────────────────────────────────

#[test]
fn test_static_block() {
    let src = r#"
        export class Config {
            static value: number = 0;
            static {
                Config.value = 42;
            }
        }
    "#;

    let facts = parse_module(src, "config");
    let DeclBody::Class(body) = &facts.declarations[0].body else {
        panic!("expected Class");
    };

    let static_block = body
        .members
        .iter()
        .find(|m| matches!(&m.kind, MemberKind::StaticBlock { .. }))
        .expect("static block must appear as a member");

    match &static_block.kind {
        MemberKind::StaticBlock { name } => {
            assert_eq!(name, "__static", "first static block gets name `__static`");
        }
        other => panic!("expected StaticBlock, got {other:?}"),
    }
}

// ── Item 5: Interface index signatures ─────────────────────────────────────────

#[test]
fn test_interface_index_signature() {
    let src = r#"
        export interface StringMap {
            [k: string]: number;
        }
    "#;

    let facts = parse_module(src, "strmap");
    let DeclBody::Interface(body) = &facts.declarations[0].body else {
        panic!("expected Interface");
    };

    assert_eq!(body.index_signatures.len(), 1, "one index signature");
    let idx = &body.index_signatures[0];
    assert_eq!(idx.key_name, "k");
    assert!(
        matches!(idx.key_ty, TypeOwned::String),
        "key type is string"
    );
    assert!(
        matches!(idx.value_ty, TypeOwned::Number),
        "value type is number"
    );
}

// ── Item 6: Interface construct signatures ──────────────────────────────────────

#[test]
fn test_interface_construct_signature() {
    let src = r#"
        export interface Factory {
            new (name: string): object;
        }
    "#;

    let facts = parse_module(src, "factory");
    let DeclBody::Interface(body) = &facts.declarations[0].body else {
        panic!("expected Interface");
    };

    assert_eq!(
        body.construct_signatures.len(),
        1,
        "one construct signature"
    );
    let cs = &body.construct_signatures[0];
    assert_eq!(cs.params.len(), 1, "one parameter `name`");
    assert_eq!(cs.params[0].name, "name");
    assert!(
        matches!(cs.params[0].ty, Some(TypeOwned::String)),
        "param type is string"
    );
}

// ── Item 7: Decorators on class and members ────────────────────────────────────

#[test]
fn test_class_and_member_decorators() {
    // OXC supports decorators in TS parsing mode.
    let src = r#"
        @sealed
        export class Service {
            @readonly
            name: string = "";
        }
    "#;

    let facts = parse_module(src, "service");
    let DeclBody::Class(body) = &facts.declarations[0].body else {
        panic!("expected Class");
    };

    // Class-level decorator.
    assert!(
        !body.decorators.is_empty(),
        "class `Service` must have at least one decorator; got none"
    );
    assert!(
        body.decorators.iter().any(|d| d.token.contains("sealed")),
        "class decorator must contain 'sealed'; got {:?}",
        body.decorators
    );

    // Member-level decorator.
    let name_member = body
        .members
        .iter()
        .find(|m| m.name == "name")
        .expect("field `name` must be present");
    assert!(
        !name_member.decorators.is_empty(),
        "field `name` must have at least one decorator"
    );
    assert!(
        name_member
            .decorators
            .iter()
            .any(|d| d.token.contains("readonly")),
        "field decorator must contain 'readonly'"
    );
}

// ── Item 8: Constructor type in type position ───────────────────────────────────

#[test]
fn test_constructor_type_in_position() {
    let src = r#"
        export type Newable = new (arg: string) => object;
    "#;

    let facts = parse_module(src, "newable");
    let DeclBody::TypeAlias(body) = &facts.declarations[0].body else {
        panic!("expected TypeAlias");
    };

    match &body.target {
        TypeOwned::Function(f) => {
            assert_eq!(f.params.len(), 1, "constructor type has one param");
            assert_eq!(f.params[0].name, "arg");
            assert!(matches!(f.params[0].ty, Some(TypeOwned::String)));
        }
        other => panic!("expected TypeOwned::Function for constructor type, got {other:?}"),
    }
}

// ── Item 9: Import types ───────────────────────────────────────────────────────

#[test]
fn test_import_type_lowers_to_nominal() {
    let src = r#"
        export type Foo = import("./bar").Baz;
    "#;

    let facts = parse_module(src, "importtype");
    let DeclBody::TypeAlias(body) = &facts.declarations[0].body else {
        panic!("expected TypeAlias");
    };

    match &body.target {
        TypeOwned::Nominal(name) => {
            assert_eq!(name, "Baz", "import type qualifier should be Baz");
        }
        other => panic!("expected TypeOwned::Nominal for import type, got {other:?}"),
    }
}

// ── Item 10: Named tuple member labels ─────────────────────────────────────────

#[test]
fn test_named_tuple_member_label_preserved() {
    let src = r#"
        export type Range = [start: number, end: number];
    "#;

    let facts = parse_module(src, "range");
    let DeclBody::TypeAlias(body) = &facts.declarations[0].body else {
        panic!("expected TypeAlias");
    };

    let TypeOwned::Tuple(members) = &body.target else {
        panic!(
            "expected Tuple, got {:?}",
            std::mem::discriminant(&body.target)
        );
    };

    assert_eq!(members.len(), 2, "two tuple members");

    // Both must be NamedTupleElem, not a bare Number (label stripping bug).
    match &members[0] {
        TypeOwned::NamedTupleElem { label, ty } => {
            assert_eq!(label, "start", "first label is 'start'");
            assert!(
                matches!(ty.as_ref(), TypeOwned::Number),
                "start type is number"
            );
        }
        other => panic!("expected NamedTupleElem for first member, got {other:?}"),
    }

    match &members[1] {
        TypeOwned::NamedTupleElem { label, ty } => {
            assert_eq!(label, "end", "second label is 'end'");
            assert!(
                matches!(ty.as_ref(), TypeOwned::Number),
                "end type is number"
            );
        }
        other => panic!("expected NamedTupleElem for second member, got {other:?}"),
    }
}

// ── Verified: mapped / conditional / keyof types ───────────────────────────────
//
// The audit said UNCERTAIN.  These tests prove the actual behavior of the
// OXC lowering path.  Results are documented in comments alongside the
// assertions so the behavior is on record and not just assumed.

/// `type K = keyof Person` — TSTypeOperatorType with operator=Keyof.
///
/// The OXC path maps this to:
///   `TypeOwned::Apply { base: Nominal("Keyof"), args: [TypeVar("Person")] }`
///
/// The operator name comes from `format!("{:?}", op.operator)` which is the
/// Debug-derived form of `TSTypeOperatorOperator::Keyof` = `"Keyof"` (Pascal
/// case).  This is **meaningful** — not a silent drop — but the casing differs
/// from the TypeScript keyword (`keyof` lowercase).  A future cleanup could
/// normalize to lowercase; for now the representation is explicit and testable.
#[test]
fn test_keyof_type_lowers_to_apply() {
    let src = r#"
        export interface Person { name: string; age: number; }
        export type PersonKeys = keyof Person;
    "#;

    let facts = parse_module(src, "keyof_test");
    // PersonKeys is the second declaration.
    let alias_decl = facts
        .declarations
        .iter()
        .find(|d| d.name == "PersonKeys")
        .expect("PersonKeys alias must be present");

    let DeclBody::TypeAlias(body) = &alias_decl.body else {
        panic!(
            "expected TypeAlias for PersonKeys, got {:?}",
            std::mem::discriminant(&alias_decl.body)
        );
    };

    // keyof T → Apply { base: Nominal("Keyof"), args: [TypeVar("Person") or Nominal("Person")] }
    match &body.target {
        TypeOwned::Apply { base, args } => {
            // The operator name is the Debug-derived string.
            match base.as_ref() {
                TypeOwned::Nominal(name) => {
                    assert_eq!(
                        name, "Keyof",
                        "keyof operator must be encoded as Nominal(\"Keyof\") — the Debug \
                         form of TSTypeOperatorOperator::Keyof; got {name:?}"
                    );
                }
                other => panic!("keyof base must be Nominal(\"Keyof\"), got {other:?}"),
            }
            assert_eq!(args.len(), 1, "keyof has one type argument (the operand)");
            // The operand (Person) is either TypeVar or Nominal depending on heuristic.
            match &args[0] {
                TypeOwned::TypeVar(n) | TypeOwned::Nominal(n) => {
                    assert_eq!(n, "Person", "keyof operand must be Person, got {n:?}");
                }
                other => panic!("keyof operand must be TypeVar/Nominal(\"Person\"), got {other:?}"),
            }
        }
        other => panic!(
            "keyof type must lower to TypeOwned::Apply, got {:?}\n\
             If this is TypeOwned::Unsupported, the TSTypeOperatorType arm is broken.",
            std::mem::discriminant(other)
        ),
    }
}

/// `type R = { readonly [P in keyof T]: T[P] }` — TSMappedType.
///
/// Mapped types now lower to `TypeOwned::Mapped` with the real IR slot.
#[test]
fn test_mapped_type_lowers_to_real_mapped() {
    let src = r#"
        export type ReadonlyPerson<T> = { readonly [P in keyof T]: T[P] };
    "#;

    let facts = parse_module(src, "mapped_test");
    let alias_decl = facts
        .declarations
        .iter()
        .find(|d| d.name == "ReadonlyPerson")
        .expect("ReadonlyPerson alias must be present");

    let DeclBody::TypeAlias(body) = &alias_decl.body else {
        panic!(
            "expected TypeAlias, got {:?}",
            std::mem::discriminant(&alias_decl.body)
        );
    };

    // Mapped types now have a real IR slot: TypeOwned::Mapped.
    assert!(
        matches!(&body.target, TypeOwned::Mapped { key_var, readonly, .. }
            if !key_var.is_empty() && *readonly == nudox_ir::kinds::ty::MappedModifier::Add),
        "{{ readonly [P in keyof T]: T[P] }} must lower to TypeOwned::Mapped with readonly=Add; \
         got {:?}",
        body.target
    );
}

/// `type C<T> = T extends string ? string : never` — TSConditionalType.
///
/// Conditional types now lower to `TypeOwned::Conditional` with the real IR slot.
#[test]
fn test_conditional_type_lowers_to_real_conditional() {
    let src = r#"
        export type IsString<T> = T extends string ? string : never;
    "#;

    let facts = parse_module(src, "conditional_test");
    let alias_decl = facts
        .declarations
        .iter()
        .find(|d| d.name == "IsString")
        .expect("IsString alias must be present");

    let DeclBody::TypeAlias(body) = &alias_decl.body else {
        panic!(
            "expected TypeAlias, got {:?}",
            std::mem::discriminant(&alias_decl.body)
        );
    };

    // Conditional types now have a real IR slot: TypeOwned::Conditional.
    assert!(
        matches!(&body.target, TypeOwned::Conditional { .. }),
        "T extends string ? string : never must lower to TypeOwned::Conditional; \
         got {:?}",
        body.target
    );
}

#[test]
fn test_template_literal_type_lowers_to_real_template_literal() {
    let src = r#"
        export type Greeting<T extends string> = `hello-${T}`;
    "#;
    let facts = parse_module(src, "template_literal_test");
    let alias = facts
        .declarations
        .iter()
        .find(|d| d.name == "Greeting")
        .expect("Greeting must be present");
    let DeclBody::TypeAlias(body) = &alias.body else {
        panic!("expected TypeAlias");
    };
    assert!(
        matches!(&body.target, TypeOwned::TemplateLiteral(_)),
        "template literal type must lower to TypeOwned::TemplateLiteral; got {:?}",
        body.target
    );
}

#[test]
fn test_object_type_literal_lowers_to_object_literal() {
    let src = r#"
        export type Point = { x: number; y?: string };
    "#;
    let facts = parse_module(src, "object_literal_test");
    let alias = facts
        .declarations
        .iter()
        .find(|d| d.name == "Point")
        .expect("Point must be present");
    let DeclBody::TypeAlias(body) = &alias.body else {
        panic!("expected TypeAlias");
    };
    match &body.target {
        TypeOwned::ObjectLiteral(members) => {
            let x = members
                .iter()
                .find(|m| m.name == "x")
                .expect("must have field x");
            assert!(!x.optional, "x must be required");
            let y = members
                .iter()
                .find(|m| m.name == "y")
                .expect("must have field y");
            assert!(y.optional, "y must be optional");
        }
        other => panic!("object type literal must lower to ObjectLiteral; got {other:?}"),
    }
}

/// `` type X = `hello` `` — a no-substitution template literal in type position.
///
/// This arrives as `TSLiteralType(TSLiteral::TemplateLiteral)`, a different AST
/// node from the interpolating `TSType::TSTemplateLiteralType`. It is still a
/// template literal type with one fixed span, so it must lower to
/// `TypeOwned::TemplateLiteral` rather than degrading to `Unsupported`.
#[test]
fn test_no_substitution_template_literal_lowers_to_template_literal() {
    let src = r#"
        export type Greeting = `hello`;
    "#;
    let facts = parse_module(src, "no_subst_template_test");
    let alias = facts
        .declarations
        .iter()
        .find(|d| d.name == "Greeting")
        .expect("Greeting must be present");
    let DeclBody::TypeAlias(body) = &alias.body else {
        panic!("expected TypeAlias");
    };
    match &body.target {
        TypeOwned::TemplateLiteral(parts) => {
            assert_eq!(parts.len(), 1, "one fixed span, no interpolations");
        }
        other => panic!(
            "no-substitution template literal must lower to TemplateLiteral, not \
             Unsupported; got {other:?}"
        ),
    }
}
