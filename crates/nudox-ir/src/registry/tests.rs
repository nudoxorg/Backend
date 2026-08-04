mod resolver {
    use crate as nudox_ir;

    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/resolver.rs"));
}

use std::assert_matches;

use self::resolver::*;

use crate::{index::Ref, test_helpers::*};

#[tokio::test]
async fn build_and_load_simple_package() -> Result<(), ResolverError> {
    let package = package();

    let resolver = ExampleResolver::from_package(package);

    let registry = Registry::new(resolver);

    let root_idx = registry.resolve_id_to_idx(UniqueId::root(PackageId::path("pkg")));

    let root = registry.resolve_entry(root_idx).await?;

    assert_eq!(root.sym(), &sym("root"));
    assert_eq!(root.parent(), None);
    assert_eq!(root.children().len(), 3);
    assert_eq!(root.kind(), &EntryInner::Owned(Kind::Module(Module)));

    let child_idx = registry.resolve_id_to_idx(UniqueId::new(PackageId::path("pkg"), 1));

    let child = registry.resolve_entry(child_idx).await?;

    assert_eq!(child.sym(), &sym("Point"));
    assert_eq!(child.parent(), Some(&Ref::Local(root_idx)));
    assert_eq!(child.children().len(), 2);
    assert_matches!(child.kind(), EntryInner::Owned(Kind::Record(Record { .. })));

    assert_eq!(root.children()[0], Ref::Local(child_idx));

    Ok(())
}

fn package() -> IrPackage<usize> {
    let mut id = id_gen();

    IrPackage::build(PackageId::path("pkg"), sym("root"), |mut root| {
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
    })
}
