use std::path::PathBuf;

use crate::{
    build::*,
    entry::{EntryInner, Symbol, Visibility},
    id::PackageId,
    kind::Kind,
};

fn sym(name: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: PathBuf::new(),
        span: 0..0,
    }
}

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
            let int = rec.create(id(), sym("i32"), |_| {
                Type::Primitive(Primitive::Integer {
                    signed: true,
                    width: Width::W32,
                })
            });

            let x = rec.create(id(), sym("0"), |_| {
                Field::builder()
                    .key(FieldKey::Positional(0))
                    .ty(int)
                    .attributes([])
                    .build()
            });
            let y = rec.create(id(), sym("1"), |_| {
                Field::builder()
                    .key(FieldKey::Positional(1))
                    .ty(int)
                    .attributes([FieldAttribute::Mutable])
                    .build()
            });

            Record::builder().fields([x, y]).super_types([]).build()
        });

        // A method `fn translate(&mut self, by: i32)`.
        root.create(id(), sym("translate"), |mut func| {
            let ty = func.create(id(), sym("i32"), |_| {
                Type::Primitive(Primitive::Integer {
                    signed: true,
                    width: Width::W32,
                })
            });
            let by = func.create(id(), sym("by"), |_| {
                Param::builder().ty(ty).attributes([]).build()
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
                Kind::Type(_) => "type",
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
    // Two Point fields + one param each declare their own `i32` type entry.
    assert_eq!(counts["type"], 2);
}
