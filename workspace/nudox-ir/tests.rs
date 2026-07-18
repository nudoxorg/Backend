//! Crate-level integration test for the IR **data model**.
//!
//! Change/patch semantics (record, dependencies, apply, unrecord) belong to
//! libpijul via `nudox-ir-vcs` and are exercised there. Here we only prove the
//! producer path: `EntryBuilder` → `seal_payloads` → a `PristineIntroTable`
//! container that the archive can seal.

use crate::apply::PristineIntroTable;
use crate::builder::EntryBuilder;
use crate::symbol::{ByteSpan, Visibility};
use nudox_change::{EcosystemId, PackageLineageId, PackageName};

fn pkg() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("mylib"))
}

#[test]
fn build_seal_populate_container() {
    let mut builder = EntryBuilder::new();
    let root = builder.add_module("root", Visibility::Public, "src/lib.rs", ByteSpan::ZERO, None, None);
    builder.add_function(
        "do_thing",
        Visibility::Public,
        "src/lib.rs",
        ByteSpan::new(10, 40),
        Some(root),
        None,
        Vec::new(),
        Vec::new(),
    );

    let payloads = builder.seal_payloads(&pkg());
    assert_eq!(payloads.len(), 2);

    // The module (a root) must precede/parent the function.
    let module_intro = payloads
        .iter()
        .find(|(_, p, parent)| matches!(p.kind, crate::wire::KindWire::Module(_)) && parent.is_none())
        .map(|(i, _, _)| *i)
        .expect("a root module");

    let mut table = PristineIntroTable::new();
    for (intro, payload, parent) in payloads {
        table.insert_live(intro, payload, parent);
    }
    assert_eq!(table.len(), 2);
    assert!(table.is_live(module_intro));

    // The function's parent resolves to the module.
    let function_parent = table
        .live_entries()
        .find(|(_, p)| matches!(p.kind, crate::wire::KindWire::Function(_)))
        .and_then(|(i, _)| table.parent_of(i));
    assert_eq!(function_parent, Some(module_intro));
}
