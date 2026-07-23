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

fn type_param() -> Generic {
    Generic::Type(TypeParam {
        variance: Variance::Invariant,
        kind: TypeKind::Type,
        bounds: Vec::new().into(),
        default: None,
        origin: TypeParamOrigin::Free,
    })
}

fn trait_ref(def: crate::index::EntryIndex<Trait>) -> TraitRef {
    TraitRef {
        def,
        args: Vec::new().into(),
        for_lifetimes: Vec::new().into(),
    }
}

/// Build a package exercising every fleshed-out Kind and its builder — traits
/// and supertraits, generics, impls, consts, statics, aliases, sums, and a rich
/// type or two — then walk it back out and assert every Kind is represented.
#[test]
fn builds_and_wires_every_kind() {
    let mut next_id = 0usize;
    let mut id = || {
        next_id += 1;
        next_id
    };

    let pkg = IrPackage::build(PackageId::path("pkg"), sym("root"), |mut root| {
        // A marker trait, used as a supertrait bound below.
        let sized = root.create(id(), sym("Sized"), |_| {
            Trait::builder()
                .super_traits([])
                .attributes([TraitAttribute::Marker])
                .build()
        });

        // A generic record `Wrapper<T: Sized> { value: T }`.
        let wrapper = root.create(id(), sym("Wrapper"), |mut rec| {
            let t = rec.create(id(), sym("T"), |_| {
                Generic::Type(TypeParam {
                    variance: Variance::Covariant,
                    kind: TypeKind::Type,
                    bounds: vec![Bound::Trait(trait_ref(sized))].into(),
                    default: None,
                    origin: TypeParamOrigin::Free,
                })
            });
            let t_ty = rec.create(id(), sym("T"), |_| Type::Param(t));
            let value = rec.create(id(), sym("value"), |_| {
                Field::builder()
                    .key(FieldKey::Named)
                    .ty(t_ty)
                    .attributes([FieldAttribute::ReadOnly])
                    .build()
            });
            Record::builder()
                .generics(Generics::builder().params([t]).constraints([]).build())
                .fields([value])
                .super_types([])
                .build()
        });

        // A trait `Container: Sized { type Item; fn len(&self) -> usize; }`.
        let container = root.create(id(), sym("Container"), |mut tr| {
            // Required associated type (no default target).
            tr.create(id(), sym("Item"), |_| Alias::builder().bounds([]).build());
            // Required method `len`.
            let usize_ty = tr.create(id(), sym("usize"), |_| {
                Type::Primitive(Primitive::Integer {
                    signed: false,
                    width: Width::Arch,
                })
            });
            let ret = tr.create(id(), sym("out"), |_| {
                Param::builder().ty(usize_ty).attributes([]).build()
            });
            tr.create(id(), sym("len"), |_| {
                Function::builder()
                    .receiver(Receiver::SharedRef)
                    .input_params([])
                    .output_params([ret])
                    .modifiers([])
                    .implemented(false)
                    .build()
            });
            Trait::builder()
                .super_traits([Bound::Trait(trait_ref(sized))])
                .attributes([TraitAttribute::ObjectSafe])
                .build()
        });

        // `impl Container for Wrapper { type Item = i32; }`.
        root.create(id(), sym("impl-Container-Wrapper"), |mut im| {
            let self_ty = im.create(id(), sym("Wrapper"), |_| Type::Named {
                def: wrapper.raw(),
                args: Vec::new().into(),
            });
            let i32_ty = im.create(id(), sym("i32"), |_| {
                Type::Primitive(Primitive::Integer {
                    signed: true,
                    width: Width::W32,
                })
            });
            im.create(id(), sym("Item"), |_| {
                Alias::builder().target(i32_ty).bounds([]).build()
            });
            Impl::builder()
                .trait_ref(trait_ref(container))
                .self_ty(self_ty)
                .attributes([])
                .build()
        });

        // A free constant `const MAX: i32 = 100;`.
        root.create(id(), sym("MAX"), |mut c| {
            let i32_ty = c.create(id(), sym("i32"), |_| {
                Type::Primitive(Primitive::Integer {
                    signed: true,
                    width: Width::W32,
                })
            });
            Const::builder()
                .ty(i32_ty)
                .value(ConstExpr::Int(100))
                .build()
        });

        // A mutable static `static mut COUNTER: i32;`.
        root.create(id(), sym("COUNTER"), |mut s| {
            let i32_ty = s.create(id(), sym("i32"), |_| {
                Type::Primitive(Primitive::Integer {
                    signed: true,
                    width: Width::W32,
                })
            });
            Static::builder().ty(i32_ty).mutable(true).build()
        });

        // A sum type `enum Status { Ok = 0, Err }`.
        root.create(id(), sym("Status"), |mut en| {
            let ok = en.create(id(), sym("Ok"), |_| {
                Variant::builder()
                    .fields([])
                    .discriminant(ConstExpr::Int(0))
                    .build()
            });
            let err = en.create(id(), sym("Err"), |_| Variant::builder().fields([]).build());
            Enum::builder().variants([ok, err]).build()
        });

        // A generic free function `const fn identity<T>(x: T) -> T`.
        root.create(id(), sym("identity"), |mut f| {
            let t = f.create(id(), sym("T"), |_| type_param());
            let x_ty = f.create(id(), sym("T"), |_| Type::Param(t));
            let ret_ty = f.create(id(), sym("T"), |_| Type::Param(t));
            let x = f.create(id(), sym("x"), |_| {
                Param::builder().ty(x_ty).attributes([]).build()
            });
            let ret = f.create(id(), sym("ret"), |_| {
                Param::builder().ty(ret_ty).attributes([]).build()
            });
            Function::builder()
                .generics(Generics::builder().params([t]).constraints([]).build())
                .input_params([x])
                .output_params([ret])
                .modifiers([FnModifier::Const])
                .build()
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
                Kind::Generic(_) => "generic",
                Kind::Trait(_) => "trait",
                Kind::Impl(_) => "impl",
                Kind::Const(_) => "const",
                Kind::Static(_) => "static",
                Kind::Alias(_) => "alias",
                Kind::Type(_) => "type",
            },
            EntryInner::Reference(_) => "reference",
        };
        *counts.entry(tag).or_default() += 1;
    }

    // Every Kind must be represented at least once.
    for kind in [
        "module", "record", "enum", "variant", "field", "function", "param", "generic", "trait",
        "impl", "const", "static", "alias", "type",
    ] {
        assert!(
            counts.get(kind).copied().unwrap_or(0) > 0,
            "missing kind: {kind}"
        );
    }

    assert_eq!(counts["module"], 1, "exactly one root module");
    assert_eq!(counts["trait"], 2, "Sized + Container");
    assert_eq!(counts["variant"], 2, "Ok + Err");
    assert_eq!(
        counts["alias"], 2,
        "Container::Item (required) + impl Item = i32"
    );
}
