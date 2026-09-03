//! Golden laws for C record bodies rendered from the already-built semantic Ir.

use compiler_ir::{
    BuiltinType, ConcreteType, EntityVersion, Ir, IrBuilder, ItemKind, Mutability, TreeItemInput,
    Visibility,
};

fn version(index: u8) -> EntityVersion {
    EntityVersion {
        stable: compiler_ir::StableEntityId::from_raw([index; 16]),
        payload: compiler_ir::PayloadHash::from_raw([index; 16]),
    }
}

fn make_ir() -> Result<Ir, compiler_ir::BuildError> {
    let mut builder = IrBuilder::new();
    let versions = (0..14).map(version).collect::<Vec<_>>();
    let mut tree = builder.reserve_tree(&versions)?;
    let entities = tree.entities();
    let int = tree.intern_concrete(ConcreteType::Builtin(BuiltinType::I32))?;
    let void = tree.intern_concrete(ConcreteType::Builtin(BuiltinType::Void))?;
    let node = tree.intern_concrete(ConcreteType::Nominal(
        entities.get(compiler_ir::TreeEntityId::new(1)).unwrap(),
    ))?;
    let const_node = tree.intern_concrete(ConcreteType::Pointer {
        target: node.erase(),
        mutability: Mutability::Immutable,
    })?;
    let length = tree.intern_atom(b"4")?;
    let array = tree.intern_concrete(ConcreteType::Array {
        element: int.erase(),
        length: Some(length),
    })?;
    let void_pointer = tree.intern_concrete(ConcreteType::Pointer {
        target: void.erase(),
        mutability: Mutability::Mutable,
    })?;
    let parameters = tree.intern_tuple_elements(&[
        compiler_ir::TupleElement {
            label: None,
            ty: int.erase(),
            kind: compiler_ir::TupleElementKind::Required,
        },
        compiler_ir::TupleElement {
            label: None,
            ty: void_pointer.erase(),
            kind: compiler_ir::TupleElementKind::Required,
        },
    ])?;
    let callback = tree.intern_concrete(ConcreteType::Function {
        parameters,
        result: Some(int.erase()),
        variadic: false,
        unsafe_: false,
        abi: None,
    })?;
    let inner = tree.intern_concrete(ConcreteType::Nominal(
        entities.get(compiler_ir::TreeEntityId::new(8)).unwrap(),
    ))?;
    let members = [
        &[][..],
        &[
            compiler_ir::TreeEntityId::new(2),
            compiler_ir::TreeEntityId::new(3),
        ][..],
        &[][..],
        &[][..],
        &[compiler_ir::TreeEntityId::new(5)][..],
        &[][..],
        &[compiler_ir::TreeEntityId::new(7)][..],
        &[][..],
        &[][..],
        &[compiler_ir::TreeEntityId::new(10)][..],
        &[][..],
        &[
            compiler_ir::TreeEntityId::new(12),
            compiler_ir::TreeEntityId::new(13),
        ][..],
        &[][..],
        &[][..],
    ];
    let items = [
        item(b"Empty", ItemKind::Record, None, &members[0], None),
        item(b"Node", ItemKind::Record, None, &members[1], None),
        item(
            b"value",
            ItemKind::Field,
            Some(compiler_ir::TreeEntityId::new(1)),
            &members[2],
            Some(int.erase()),
        ),
        item(
            b"next",
            ItemKind::Field,
            Some(compiler_ir::TreeEntityId::new(1)),
            &members[3],
            Some(const_node.erase()),
        ),
        item(b"ArrayHolder", ItemKind::Record, None, &members[4], None),
        item(
            b"values",
            ItemKind::Field,
            Some(compiler_ir::TreeEntityId::new(4)),
            &members[5],
            Some(array.erase()),
        ),
        item(b"Callback", ItemKind::Record, None, &members[6], None),
        item(
            b"callback",
            ItemKind::Field,
            Some(compiler_ir::TreeEntityId::new(6)),
            &members[7],
            Some(callback.erase()),
        ),
        item(b"Inner", ItemKind::Record, None, &members[8], None),
        item(b"Outer", ItemKind::Record, None, &members[9], None),
        item(
            b"inner",
            ItemKind::Field,
            Some(compiler_ir::TreeEntityId::new(9)),
            &members[10],
            Some(inner.erase()),
        ),
        item(b"Color", ItemKind::Enum, None, &members[11], None),
        item(
            b"RED",
            ItemKind::Variant,
            Some(compiler_ir::TreeEntityId::new(11)),
            &members[12],
            None,
        ),
        item(
            b"GREEN",
            ItemKind::Variant,
            Some(compiler_ir::TreeEntityId::new(11)),
            &members[13],
            None,
        ),
    ];
    tree.commit(&items, &[])?;
    builder.finish()
}

fn item<'a>(
    name: &'a [u8],
    kind: ItemKind,
    parent: Option<compiler_ir::TreeEntityId>,
    members: &'a [compiler_ir::TreeEntityId],
    semantic_type: Option<compiler_ir::TypeId>,
) -> TreeItemInput<'a> {
    TreeItemInput {
        name,
        kind,
        visibility: Visibility::Public,
        parent,
        semantic_type,
        members,
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    }
}

#[test]
fn c_renderings_match_committed_goldens() -> Result<(), compiler_ir::BuildError> {
    let ir = make_ir()?;
    let cases = [
        (&b"Empty"[..], include_str!("goldens/c_empty.golden")),
        (&b"Node"[..], include_str!("goldens/c_two_fields.golden")),
        (&b"ArrayHolder"[..], include_str!("goldens/c_array.golden")),
        (
            &b"Callback"[..],
            include_str!("goldens/c_function_pointer.golden"),
        ),
        (
            &b"Outer"[..],
            include_str!("goldens/c_nested_record.golden"),
        ),
        (&b"Color"[..], include_str!("goldens/c_enum.golden")),
    ];
    for (name, expected) in cases {
        let item = ir.items_named(name).next().unwrap();
        let expected = expected.lines().skip(1).collect::<Vec<_>>().join("\n");
        assert_eq!(
            ir.signature(item.id()).unwrap().to_string(),
            expected,
            "{name:?}"
        );
    }
    Ok(())
}
