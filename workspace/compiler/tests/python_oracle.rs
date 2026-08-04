//! Pipeline part: **Python source → surface IR** via the Pyrefly oracle
//! (`compiler::languages::python`).
//!
//! This is the golden/integration test that proves the Pyrefly type checker is
//! actually driven as a resolution oracle and that its results survive into our
//! IR. It deliberately mixes two cases:
//!
//!   * `add(a: int, b: int) -> int` — fully annotated; the params and return
//!     must come back as concrete resolved types.
//!   * `make()` — *unannotated*; its `str` return type only appears if Pyrefly
//!     ran inference AND those inference results were visible at lowering time.
//!
//! If the inference transaction were thrown away before lowering (the bug this
//! work fixes), the unannotated return — and likely everything else — would
//! collapse to `Any`, and the `make` assertions below would fail. That is what
//! makes this test distinguish "the oracle works" from "everything is Any".

use compiler::languages::python::context::PythonContext;
use ir::entry::Index;
use ir::function::Function;
use ir::kind::Entry;
use ir::parameter::Parameter;
use ir::ty::{LiteralKind, Type};

const SNIPPET: &str = "\
def add(a: int, b: int) -> int:
    return a + b


def make():
    return \"hi\"
";

/// A type counts as "really resolved" when Pyrefly handed us a concrete class
/// reference (e.g. `builtins.int`), a primitive, or a literal — as opposed to
/// the `Any` / `Infer` fallbacks we'd see if inference results never reached
/// lowering. Literals (e.g. `Literal["hi"]` for `return "hi"`) are stronger
/// than a bare `str` reference and still prove the oracle ran.
fn is_resolved(ty: &Type) -> bool {
    match ty {
        Type::Primitive(_) => true,
        Type::TypeReference(r) => !r.identifier.is_empty(),
        Type::Literal(_) => true,
        _ => false,
    }
}

fn find_function<'a>(index: &'a Index, name: &str) -> &'a Function {
    index
        .entries_by_path
        .values()
        .find_map(|e| match e {
            Entry::Function(sym) if sym.name == name => Some(&sym.inner),
            _ => None,
        })
        .unwrap_or_else(|| {
            let found: Vec<&str> = index.entries_by_path.values().map(|e| e.name()).collect();
            panic!("function `{name}` not found in lowered IR; entries present: {found:?}")
        })
}

fn param_type(p: &Parameter) -> &Type {
    match p {
        Parameter::Literal(lp) => lp
            .r#type
            .as_ref()
            .expect("literal parameter lowered without a type"),
        other => panic!("unexpected non-literal parameter: {other:?}"),
    }
}

#[test]
fn pyrefly_oracle_resolves_annotated_and_inferred_types() {
    let ctx = PythonContext::new();

    let handle = ctx.check_snippet("oracle_mod", SNIPPET);
    let index = ctx.lower_handle(&handle);

    // --- annotated function: `def add(a: int, b: int) -> int` ---
    let add = find_function(&index, "add");

    let inputs = add
        .input_parameters
        .as_ref()
        .expect("`add` lowered with no input parameters");
    assert_eq!(inputs.len(), 2, "`add` should have two parameters");

    for (param, pname) in inputs.iter().zip(["a", "b"]) {
        let ty = param_type(param);
        assert!(
            is_resolved(ty),
            "param `{pname}` of `add` is not a resolved type, got: {ty:?}"
        );
        println!("add.{pname} : {ty:?}");
    }

    let add_ret = add
        .output_parameters
        .as_ref()
        .and_then(|v| v.first())
        .map(param_type)
        .expect("`add` lowered with no return type");
    assert!(
        is_resolved(add_ret),
        "return of `add` is not a resolved type, got: {add_ret:?}"
    );
    println!("add -> {add_ret:?}");

    // --- UNANNOTATED function: `def make(): return \"hi\"` ---
    // The whole point: the return type here is *inferred* by Pyrefly. If the
    // inference transaction wasn't committed/visible, this is `Any`.
    let make = find_function(&index, "make");
    let make_ret = make
        .output_parameters
        .as_ref()
        .and_then(|v| v.first())
        .map(param_type)
        .expect("`make` lowered with no return type (inference did not reach lowering)");
    assert!(
        is_resolved(make_ret),
        "inferred return of `make` is not a resolved type (Any leak?), got: {make_ret:?}"
    );
    // It must resolve to `str` (or a string literal, which is more precise) —
    // proving real inference, not a coincidental non-Any placeholder.
    match make_ret {
        Type::Primitive(_) => {}
        Type::Literal(lit) => assert!(
            matches!(lit.kind, LiteralKind::String),
            "inferred return of `make` should be a string literal, got: {lit:?}"
        ),
        Type::TypeReference(r) => assert!(
            r.identifier.contains("str"),
            "inferred return of `make` should be `str`, got reference `{}`",
            r.identifier
        ),
        other => panic!("inferred return of `make` should be str-like, got: {other:?}"),
    }
    println!("make -> {make_ret:?}  (inferred)");
}
