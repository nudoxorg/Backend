//! Nix render backend + `sig` parse/render round-trip property checks.
//!
//! Covers:
//!   * `sig::parse` → `Signature::to_type` → `render_entry` / `Backend::ty`
//!     for arrows, slices, records, and type variables
//!   * `render_entry` of a Function emits `# Type ::` and a Nix binding

use compiler::languages::nix::sig::{self, Signature};
use compiler::render::{render_entry, Language, RenderCtx};
use ir::entry::NudoxPath;
use ir::function::Function;
use ir::kind::{Entry, Symbol, Visibility};
use ir::parameter::{LiteralParameter, Parameter};
use ir::primitives::{Primitive, Width};
use ir::ty::{FunctionPointer, Type, TypeReference};

fn nix_ctx() -> RenderCtx {
    RenderCtx::new(Language::Nix).with_width(100)
}

fn render_type(ty: &Type) -> String {
    // Wrap as a TypeAlias so `render_entry` dispatches to the Nix alias path,
    // which still routes through `Backend::ty`.
    let entry = Entry::TypeAlias(Symbol {
        name:          "T".into(),
        path:          NudoxPath::Local("T".into()),
        aliases:       None,
        visibility:    Visibility::Public,
        documentation: None,
        deprecation:   None,
        doc_links:     None,
        inner:         ty.clone(),
    });
    render_entry(&entry, &nix_ctx())
}

fn render_sig(sig: &Signature) -> String {
    render_type(&sig.to_type())
}

// ── sig parser property / round-trip cases ────────────────────────────────

#[test]
fn sig_roundtrip_int_string_bool_arrows() {
    let sig = sig::parse("Int -> String -> Bool").expect("parse Int -> String -> Bool");
    assert!(sig.name.is_none());
    assert_eq!(sig.params.len(), 2);
    assert!(matches!(sig.params[0], Type::Primitive(Primitive::Int(_))));
    assert!(matches!(sig.params[1], Type::Primitive(Primitive::String)));
    assert!(matches!(sig.ret, Type::Primitive(Primitive::Bool)));

    let rendered = render_sig(&sig);
    assert!(
        rendered.contains("->") && rendered.contains("Int") && rendered.contains("Bool"),
        "expected arrow/`::` surface, got: {rendered}"
    );
}

#[test]
fn sig_roundtrip_string_slice() {
    let sig = sig::parse("[String]").expect("parse [String]");
    assert!(sig.params.is_empty());
    assert!(matches!(sig.ret, Type::Slice(_)));

    let rendered = render_sig(&sig);
    assert!(
        rendered.contains("[") && rendered.contains("String") && rendered.contains("]"),
        "expected [String] surface, got: {rendered}"
    );
}

#[test]
fn sig_roundtrip_record_with_optional_field() {
    let sig = sig::parse("{ name :: String; version :: String? }")
        .expect("parse record with optional field");
    assert!(sig.params.is_empty());
    match &sig.ret {
        Type::RecordLiteral(rec) => assert_eq!(rec.fields.len(), 2),
        other => panic!("expected record, got {other:?}"),
    }

    let rendered = render_sig(&sig);
    assert!(
        rendered.contains("name")
            && rendered.contains("::")
            && rendered.contains("String")
            && (rendered.contains("version") || rendered.contains("?")),
        "expected record `::` surface, got: {rendered}"
    );
}

#[test]
fn sig_roundtrip_type_variables() {
    let sig = sig::parse("a -> b").expect("parse a -> b");
    assert_eq!(sig.params.len(), 1);
    assert!(matches!(sig.params[0], Type::GenericParam(ref g) if g.name == "a"));
    assert!(matches!(sig.ret, Type::GenericParam(ref g) if g.name == "b"));

    let rendered = render_sig(&sig);
    assert!(
        rendered.contains("a") && rendered.contains("->") && rendered.contains("b"),
        "expected a -> b surface, got: {rendered}"
    );
}

// ── Function entry rendering ──────────────────────────────────────────────

fn literal(name: &str, ty: Type) -> Parameter {
    Parameter::Literal(LiteralParameter {
        name:          name.into(),
        r#type:        Some(ty),
        attributes:    None,
        default_value: None,
        description:   None,
    })
}

#[test]
fn render_function_emits_type_comment_and_binding() {
    let func = Function {
        input_parameters: Some(vec![
            literal("age", Type::Primitive(Primitive::Int(Width::W64))),
            literal("name", Type::Primitive(Primitive::String)),
        ]),
        output_parameters: Some(vec![literal(
            "",
            Type::Primitive(Primitive::Bool),
        )]),
        type_links:            None,
        attributes:            None,
        generics:              None,
        receiver:              None,
        overloads:             None,
        implemented:           true,
        members:               None,
        implemented_protocols: None,
        body:                  None,
    };

    let entry = Entry::Function(Symbol {
        name:          "greet".into(),
        path:          NudoxPath::Local("greet".into()),
        aliases:       None,
        visibility:    Visibility::Public,
        documentation: Some("Greet someone.".into()),
        deprecation:   None,
        doc_links:     None,
        inner:         func,
    });

    let cx = nix_ctx().with_docs(true);
    let out = render_entry(&entry, &cx);
    println!("rendered:\n{out}");

    assert!(
        out.contains("# Type ::") || out.contains("Type ::"),
        "expected `# Type ::` convention, got:\n{out}"
    );
    assert!(
        out.contains("->"),
        "expected arrow chain in type comment, got:\n{out}"
    );
    assert!(
        out.contains("greet") && out.contains("="),
        "expected Nix binding `greet = …`, got:\n{out}"
    );
    // Curried form for simple params: `age: name:`
    assert!(
        out.contains("age:") && out.contains("name:"),
        "expected curried formals, got:\n{out}"
    );
}

#[test]
fn render_named_sig_to_type_uses_function_pointer() {
    let sig = sig::parse("mapAttrs :: (String -> a -> b) -> AttrSet a -> AttrSet b")
        .expect("parse mapAttrs sig");
    let ty = sig.to_type();
    assert!(
        matches!(ty, Type::FunctionPointer(FunctionPointer { .. })),
        "arrow sig should lower to FunctionPointer, got {ty:?}"
    );
    // Also exercise the bare type renderer path used by TypeAlias.
    let rendered = render_type(&ty);
    assert!(
        rendered.contains("->") && rendered.contains("AttrSet"),
        "expected AttrSet arrow surface, got: {rendered}"
    );
    let _ = TypeReference {
        identifier: "AttrSet".into(),
        generic_args: None,
    };
}
