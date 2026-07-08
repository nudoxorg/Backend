//! Pipeline part: **docstrings → IR** in `item`/`docstring` (Phase 3 #2).
//!
//! Proves that a function's Google-style docstring lands on `Symbol.documentation`
//! (summary) and that a documented parameter's description lands on the matching
//! `LiteralParameter.description`.

use compiler::languages::python::context::PythonContext;
use ir::entry::Index;
use ir::function::Function;
use ir::kind::{Entry, Symbol};
use ir::parameter::Parameter;

const SNIPPET: &str = "\
def greet(name, count):
    \"\"\"Greet a person.

    A slightly longer description of the
    greeting behaviour.

    Args:
        name: who to greet.
        count: how many times to repeat.

    Returns:
        the assembled greeting.
    \"\"\"
    return name * count
";

fn function_symbol<'a>(index: &'a Index, name: &str) -> &'a Symbol<Function> {
    index
        .entries_by_path
        .values()
        .find_map(|e| match e {
            Entry::Function(sym) if sym.name == name => Some(sym),
            _ => None,
        })
        .unwrap_or_else(|| panic!("function `{name}` not found in lowered IR"))
}

fn param_description<'a>(func: &'a Function, name: &str) -> Option<&'a str> {
    func.input_parameters.as_ref()?.iter().find_map(|p| match p {
        Parameter::Literal(lp) if lp.name == name => lp.description.as_deref(),
        _ => None,
    })
}

#[test]
fn function_docstring_and_param_descriptions_are_populated() {
    let ctx = PythonContext::new();
    let handle = ctx.check_snippet("docstrings_mod", SNIPPET);
    let index = ctx.lower_handle(&handle);

    let greet = function_symbol(&index, "greet");

    // --- summary lands on Symbol.documentation ---
    let doc = greet
        .documentation
        .as_deref()
        .expect("`greet` lowered with no documentation");
    println!("greet documentation: {doc:?}");
    assert!(
        doc.contains("Greet a person."),
        "documentation should contain the summary, got: {doc:?}"
    );

    // --- per-parameter descriptions land on LiteralParameter.description ---
    let name_desc =
        param_description(&greet.inner, "name").expect("param `name` lowered with no description");
    println!("greet.name description: {name_desc:?}");
    assert!(
        name_desc.contains("who to greet"),
        "param `name` description wrong, got: {name_desc:?}"
    );

    let count_desc = param_description(&greet.inner, "count")
        .expect("param `count` lowered with no description");
    assert!(
        count_desc.contains("how many times"),
        "param `count` description wrong, got: {count_desc:?}"
    );
}
