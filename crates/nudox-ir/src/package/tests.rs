use itertools::Itertools;

use crate::{entry::EntryInner, kinds::*, test_helpers::*};

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
                Kind::Trait(_) => "trait",
                Kind::Impl(_) => "impl",
                Kind::Const(_) => "const",
                Kind::Static(_) => "static",
                Kind::Reexport(_) => "reexport",
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
                    .fields([2, 3].map(idx).map(EntryIndex::typed).map(Ref::Local))
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
                    .input_params([Ref::Local(idx(5).typed())])
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
                Enum::builder()
                    .variants([Ref::Local(idx(7).typed())])
                    .build(),
            ),
            entry(sym("Unit"), n::leaf(idx(6)), Variant::builder().build()),
        ],
    );
}

/// Build entries for the new kinds (Trait, Impl, Const, Static, Reexport) and
/// verify that `Entry::downcast` returns the correct typed handle.
#[test]
fn new_kinds_build_and_downcast() {
    let mut id = id_gen();

    let pkg = IrPackage::build(PackageId::path("pkg"), sym("root"), |mut root| {
        // Trait with one super bound.
        root.create(id(), sym("Display"), |_| {
            Trait::builder().supers([Type::Any]).build()
        });

        // Inherent impl for `u32`.
        root.create(id(), sym("ImplU32"), |_| {
            Impl::builder().self_ty(Type::U32).build()
        });

        // Const declaration.
        root.create(id(), sym("MAX"), |_| Const::builder().ty(Type::U64).build());

        // Static variable.
        root.create(id(), sym("COUNTER"), |_| {
            Static::builder().ty(Type::I32).mutable(true).build()
        });
    });

    // Collect all entries so we can inspect them by name.
    let entries: Vec<_> = pkg.iter().map(|(_, e)| e).collect();

    let display_entry = entries
        .iter()
        .find(|e| e.sym().name == "Display")
        .expect("Display entry not found");

    // downcast to the correct type succeeds.
    assert!(
        display_entry.downcast::<Trait>().is_some(),
        "downcast::<Trait>() should be Some for a Trait entry"
    );

    // downcast to a wrong type returns None (no panic).
    assert!(
        display_entry.downcast::<Record>().is_none(),
        "downcast::<Record>() should be None for a Trait entry"
    );

    // Spot-check body() returns the right data.
    let typed_display = display_entry.downcast::<Trait>().unwrap();
    assert_eq!(typed_display.body().supers.as_ref(), &[Type::Any]);

    // Impl entry.
    let impl_entry = entries
        .iter()
        .find(|e| e.sym().name == "ImplU32")
        .expect("ImplU32 entry not found");
    let typed_impl = impl_entry.downcast::<Impl>().unwrap();
    assert_eq!(typed_impl.body().self_ty, Type::U32);
    assert_eq!(typed_impl.body().of, None);

    // Const entry.
    let const_entry = entries
        .iter()
        .find(|e| e.sym().name == "MAX")
        .expect("MAX entry not found");
    let typed_const = const_entry.downcast::<Const>().unwrap();
    assert_eq!(typed_const.body().ty, Type::U64);

    // Static entry.
    let static_entry = entries
        .iter()
        .find(|e| e.sym().name == "COUNTER")
        .expect("COUNTER entry not found");
    let typed_static = static_entry.downcast::<Static>().unwrap();
    assert_eq!(typed_static.body().ty, Type::I32);
    assert!(typed_static.body().mutable);

    // Discriminant round-trips through as_u16().
    use crate::kind::KindDiscriminant;
    assert_eq!(KindDiscriminant::Trait.as_u16(), 6);
    assert_eq!(KindDiscriminant::Impl.as_u16(), 7);
    assert_eq!(KindDiscriminant::Const.as_u16(), 10);
    assert_eq!(KindDiscriminant::Static.as_u16(), 11);
    assert_eq!(KindDiscriminant::Reexport.as_u16(), 12);
    assert_eq!(KindDiscriminant::Module.as_u16(), 1);
    assert_eq!(KindDiscriminant::Record.as_u16(), 2);
}

/// Build a Record with a type generic parameter and an Impl with a where-clause
/// predicate, then downcast each and assert that the generics/wheres round-trip
/// through `body()`.
#[test]
fn generics_and_where_clauses_round_trip() {
    let mut id = id_gen();

    let pkg = IrPackage::build(PackageId::path("pkg"), sym("root"), |mut root| {
        // A record `struct Container<T: Any>`.
        root.create(id(), sym("Container"), |_| {
            Record::builder()
                .generics([GenericParam::Type {
                    name: "T".to_owned(),
                    bounds: [Type::Any].into(),
                    default: None,
                }])
                .build()
        });

        // An impl block `impl<T> Trait for T where T: Any`.
        root.create(id(), sym("ImplGeneric"), |_| {
            Impl::builder()
                .self_ty(Type::SelfType)
                .wheres([WherePred {
                    target: Type::SelfType,
                    bounds: [Type::Any].into(),
                }])
                .build()
        });
    });

    let entries: Vec<_> = pkg.iter().map(|(_, e)| e).collect();

    // Check Record generics.
    let container = entries
        .iter()
        .find(|e| e.sym().name == "Container")
        .expect("Container entry not found")
        .downcast::<Record>()
        .expect("downcast::<Record>() failed");
    assert_eq!(container.body().generics.len(), 1);
    assert_eq!(container.body().generics[0], GenericParam::Type {
        name: "T".to_owned(),
        bounds: [Type::Any].into(),
        default: None,
    });
    assert!(container.body().wheres.is_empty());

    // Check Impl where-clause.
    let impl_entry = entries
        .iter()
        .find(|e| e.sym().name == "ImplGeneric")
        .expect("ImplGeneric entry not found")
        .downcast::<Impl>()
        .expect("downcast::<Impl>() failed");
    assert!(impl_entry.body().generics.is_empty());
    assert_eq!(impl_entry.body().wheres.len(), 1);
    assert_eq!(impl_entry.body().wheres[0], WherePred {
        target: Type::SelfType,
        bounds: [Type::Any].into(),
    });
}

/// Verify form/flags metadata on Record, Impl, Trait, and Variant.
#[test]
fn form_and_flags_metadata() {
    let mut id = id_gen();

    let pkg = IrPackage::build(PackageId::path("pkg"), sym("root"), |mut root| {
        // A tuple record.
        root.create(id(), sym("TuplePoint"), |_| {
            Record::builder().form(RecordForm::Tuple).build()
        });

        // A negative blanket impl.
        root.create(id(), sym("NegBlanket"), |_| {
            Impl::builder()
                .flags(ImplFlags {
                    negative: true,
                    blanket: true,
                })
                .self_ty(Type::Any)
                .build()
        });

        // An unsafe auto trait.
        root.create(id(), sym("UnsafeAuto"), |_| {
            Trait::builder()
                .flags(TraitFlags {
                    is_unsafe: true,
                    is_auto: true,
                    sealed: false,
                })
                .build()
        });

        // A struct-form variant.
        root.create(id(), sym("StructVariant"), |_| {
            Variant::builder().form(VariantForm::Struct).build()
        });
    });

    let entries: Vec<_> = pkg.iter().map(|(_, e)| e).collect();

    // Tuple record.
    let rec = entries
        .iter()
        .find(|e| e.sym().name == "TuplePoint")
        .expect("TuplePoint not found")
        .downcast::<Record>()
        .expect("downcast::<Record>() failed");
    assert_eq!(rec.body().form, RecordForm::Tuple);

    // Negative blanket impl.
    let impl_entry = entries
        .iter()
        .find(|e| e.sym().name == "NegBlanket")
        .expect("NegBlanket not found")
        .downcast::<Impl>()
        .expect("downcast::<Impl>() failed");
    assert_eq!(impl_entry.body().flags, ImplFlags {
        negative: true,
        blanket: true
    });

    // Unsafe auto trait.
    let trait_entry = entries
        .iter()
        .find(|e| e.sym().name == "UnsafeAuto")
        .expect("UnsafeAuto not found")
        .downcast::<Trait>()
        .expect("downcast::<Trait>() failed");
    assert_eq!(trait_entry.body().flags, TraitFlags {
        is_unsafe: true,
        is_auto: true,
        sealed: false
    });

    // Struct-form variant.
    let variant_entry = entries
        .iter()
        .find(|e| e.sym().name == "StructVariant")
        .expect("StructVariant not found")
        .downcast::<Variant>()
        .expect("downcast::<Variant>() failed");
    assert_eq!(variant_entry.body().form, VariantForm::Struct);
}
