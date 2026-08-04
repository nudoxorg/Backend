//! Pipeline part: **`@overload` grouping → IR** in `function`/`item` (Phase 3 #3).
//!
//! Pyrefly pre-merges every `@overload`-decorated signature into a single
//! `Type::Overload`. This test proves that surfaces as ONE `Function` entry
//! whose `overloads` field carries the branch signatures.

use compiler::languages::python::context::PythonContext;
use ir::entry::Index;
use ir::function::Function;
use ir::kind::Entry;

const SNIPPET: &str = "\
from typing import overload


@overload
def f(x: int) -> int: ...
@overload
def f(x: str) -> str: ...
def f(x):
    return x
";

fn functions_named<'a>(index: &'a Index, name: &str) -> Vec<&'a Function> {
    index
        .entries_by_path
        .values()
        .filter_map(|e| match e {
            Entry::Function(sym) if sym.name == name => Some(&sym.inner),
            _ => None,
        })
        .collect()
}

#[test]
fn overloads_collapse_to_one_function_with_branches() {
    let ctx = PythonContext::new();
    let handle = ctx.check_snippet("overloads_mod", SNIPPET);
    let index = ctx.lower_handle(&handle);
    let entries: Vec<&str> = index.entries_by_path.values().map(|e| e.name()).collect();
    println!("entries: {entries:?}");

    // The imported `overload` decorator must not leak as an entry.
    assert!(
        !entries.contains(&"overload"),
        "imported `overload` leaked into the IR; entries: {entries:?}"
    );

    // Exactly one `f` entry.
    let fns = functions_named(&index, "f");
    assert_eq!(
        fns.len(),
        1,
        "expected a single `f` Function entry, got {}",
        fns.len()
    );

    // It carries the overload branches.
    let overloads = fns[0]
        .overloads
        .as_ref()
        .expect("`f` lowered without `overloads`");
    println!("f overload branches: {}", overloads.len());
    assert!(
        overloads.len() >= 2,
        "expected at least 2 overload branches, got {}",
        overloads.len()
    );
}
