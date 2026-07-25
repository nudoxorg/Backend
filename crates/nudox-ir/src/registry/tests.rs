mod resolver {
    use crate as nudox_ir;

    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/resolver.rs"));
}

use self::resolver::*;

use crate::test_helpers::*;

#[tokio::test]
async fn build_and_load_simple_package() -> Result<(), ResolverError> {
    let package = package();

    let resolver = ExampleResolver::from_package(package);

    let registry = Registry::new(resolver);

    let root = registry.resolve_id_to_idx(UniqueId::root(PackageId::path("pkg")));

    let root = registry.resolve_entry(root).await?;

    assert_eq!(root.sym(), &sym("root"));
    assert_eq!(root.parent(), None);
    assert_eq!(root.children().len(), 3);
    assert_eq!(root.kind(), &EntryInner::Owned(Kind::Module(Module)));

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
