//! Pipeline part: **import filtering** in `item::lower_module` (Phase 3 #1).
//!
//! A module re-exports every name it can resolve, including names pulled in via
//! `from enum import Enum`. Without filtering, that import would leak out as a
//! bogus `Enum` entry. This test proves imports whose *definition* lives in
//! another module are dropped, while locally-defined classes survive.

use compiler::languages::python::context::PythonContext;
use ir::entry::Index;
use ir::kind::Entry;

const SNIPPET: &str = "\
from enum import Enum
from dataclasses import dataclass


class Local:
    value: int


@dataclass
class AlsoLocal:
    name: str
";

fn names(index: &Index) -> Vec<String> {
    index
        .entries_by_path
        .values()
        .map(|e| e.name().to_string())
        .collect()
}

fn has_entry(index: &Index, name: &str) -> bool {
    index.entries_by_path.values().any(|e| e.name() == name)
}

#[test]
fn imports_are_filtered_locals_are_kept() {
    let ctx = PythonContext::new();
    let handle = ctx.check_snippet("imports_mod", SNIPPET);
    let index = ctx.lower_handle(&handle);
    println!("entries: {:?}", names(&index));

    // The `from enum import Enum` re-export must NOT appear as an entry.
    assert!(
        !has_entry(&index, "Enum"),
        "imported `Enum` leaked into the IR; entries: {:?}",
        names(&index)
    );
    // Nor should the imported `dataclass` decorator function.
    assert!(
        !has_entry(&index, "dataclass"),
        "imported `dataclass` leaked into the IR; entries: {:?}",
        names(&index)
    );

    // The locally-defined classes MUST be present.
    assert!(
        matches!(
            index.entries_by_path.values().find(|e| e.name() == "Local"),
            Some(Entry::RecordType(_))
        ),
        "local class `Local` missing or mis-lowered; entries: {:?}",
        names(&index)
    );
    assert!(
        has_entry(&index, "AlsoLocal"),
        "local class `AlsoLocal` missing; entries: {:?}",
        names(&index)
    );
}
