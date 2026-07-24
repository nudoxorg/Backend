use std::path::PathBuf;

use itertools::Itertools;

use crate::{
    build::*,
    entry::{EntryInner, Node, Symbol, Visibility},
    kind::{EntryKind, Kind},
};

use super::*;

/// Build a package exercising every fleshed-out Kind and its builder, then walk
/// it back out to assert the graph wired up as expected.
#[test]
fn builds_and_wires_every_kind() {
    let mut next_id = 0usize;
    let mut id = || {
        next_id += 1;
        next_id
    };

    let pkg = IrPackage::build(PackageId::path("pkg"), sym("root"), |mut root| {
        // A tuple record `Point(i32, i32)` whose fields reference a shared type.
        root.create(id(), sym("Point"), |mut rec| {
            let x = rec.create(id(), sym("0"), |_| {
                Field::builder()
                    .key(FieldKey::Positional(0))
                    .ty(Type::I32)
                    .attributes([])
                    .build()
            });

            let y = rec.create(id(), sym("1"), |_| {
                Field::builder()
                    .key(FieldKey::Positional(1))
                    .ty(Type::I32)
                    .attributes([FieldAttribute::Mutable])
                    .build()
            });

            Record::builder().fields([x, y]).build()
        });

        // A method `fn translate(&mut self, by: i32)`.
        root.create(id(), sym("translate"), |mut func| {
            let by = func.create(id(), sym("by"), |_| {
                Param::builder().ty(Type::I32).attributes([]).build()
            });

            Function::builder()
                .receiver(Receiver::MutRef)
                .input_params([by])
                .output_params([])
                .modifiers([])
                .build()
        });

        // A sum type `enum Shape { Unit }`.
        root.create(id(), sym("Shape"), |mut en| {
            let unit = en.create(id(), sym("Unit"), |_| Variant::builder().fields([]).build());

            Enum::builder().variants([unit]).build()
        });
    });

    let mut counts = std::collections::HashMap::<&str, usize>::new();

    for (_, entry) in pkg.iter() {
        let tag = match entry.kind() {
            EntryInner::Owned(kind) => match kind {
                Kind::Module(_) => "module",
                Kind::Record(_) => "record",
                Kind::Enum(_) => "enum",
                Kind::Variant(_) => "variant",
                Kind::Field(_) => "field",
                Kind::Function(_) => "function",
                Kind::Param(_) => "param",
            },
            EntryInner::Reference(_) => "reference",
        };
        *counts.entry(tag).or_default() += 1;
    }

    assert_eq!(counts["module"], 1, "one root module");
    assert_eq!(counts["record"], 1);
    assert_eq!(counts["field"], 2);
    assert_eq!(counts["function"], 1);
    assert_eq!(counts["param"], 1);
    assert_eq!(counts["enum"], 1);
    assert_eq!(counts["variant"], 1);

    let idx = |id| pkg.info.export_id_to_idx(&id).expect("id not found");

    let root = pkg.info.root_export();

    itertools::assert_equal(
        pkg.iter()
            .sorted_by_key(|&(idx, _)| idx)
            .map(|(_, entry)| entry),
        &[
            entry(sym("root"), n::root([1, 4, 6].map(idx)), Module),
            entry(
                sym("Point"),
                n(root, [2, 3].map(idx)),
                Record::builder()
                    .fields([2, 3].map(idx).map(EntryIndex::typed))
                    .build(),
            ),
            entry(
                sym("0"),
                n::leaf(idx(1)),
                Field::builder()
                    .key(FieldKey::Positional(0))
                    .ty(Type::I32)
                    .build(),
            ),
            entry(
                sym("1"),
                n::leaf(idx(1)),
                Field::builder()
                    .key(FieldKey::Positional(1))
                    .ty(Type::I32)
                    .attributes([FieldAttribute::Mutable])
                    .build(),
            ),
            entry(
                sym("translate"),
                n(root, [idx(5)]),
                Function::builder()
                    .receiver(Receiver::MutRef)
                    .input_params([idx(5).typed()])
                    .build(),
            ),
            entry(
                sym("by"),
                n::leaf(idx(4)),
                Param::builder().ty(Type::I32).build(),
            ),
            entry(
                sym("Shape"),
                n(root, [idx(7)]),
                Enum::builder().variants([idx(7).typed()]).build(),
            ),
            entry(sym("Unit"), n::leaf(idx(6)), Variant::builder().build()),
        ],
    );
}

fn sym(name: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: PathBuf::new(),
        span: 0..0,
    }
}

fn entry<T: EntryKind>(sym: Symbol, node: Node, kind: T) -> Entry {
    Entry::new(sym, node, kind.into_kind())
}

fn n(parent: UntypedEntryIndex, children: impl IntoIterator<Item = UntypedEntryIndex>) -> Node {
    Node::build(parent, children)
}

mod n {
    use super::*;

    pub fn leaf(parent: UntypedEntryIndex) -> Node {
        Node::build(parent, [])
    }

    pub fn root(children: impl IntoIterator<Item = UntypedEntryIndex>) -> Node {
        Node::build(None, children)
    }
}
