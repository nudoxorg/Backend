//! Shape falsifier for recursive semantic products, pooled child lists, and
//! authority-bearing external coordinates.
//! Every typed owner boundary must survive complete product construction.
use compiler_ir_vocabulary::{
    AtomId, ExternalCoordinate, ExternalFragmentId, ListSpan, PooledListError, ProductChildRole,
    ProductChildren, ProductConstructorFault, ProductConstructorTag, ProductId, ProductList,
    ProductListId, ProductRef, SemanticProduct, SemanticProductChild, SemanticProductConstructor,
};

#[test]
fn full_type_shape_preserves_constructor_arity_and_external_authority()
-> Result<(), PooledListError> {
    let first_authority = ExternalFragmentId::from_canonical_bytes(b"dependency-a");
    let second_authority = ExternalFragmentId::from_canonical_bytes(b"dependency-b");
    let first_external = ProductRef::External(ExternalCoordinate::bind(first_authority, 7));
    let second_external = ProductRef::External(ExternalCoordinate::bind(second_authority, 7));
    assert_ne!(first_external, second_external);

    let children = [
        SemanticProductChild {
            target: ProductRef::Local(ProductId::new(0)),
            role: ProductChildRole::ProductMember,
        },
        SemanticProductChild {
            target: first_external,
            role: ProductChildRole::ProductMember,
        },
        SemanticProductChild {
            target: second_external,
            role: ProductChildRole::ProductMember,
        },
    ];
    let span = ListSpan::<ProductChildren>::new(0, 3);
    let product_children = ProductList::try_from_parts(&children, span)?;
    assert_eq!(product_children.as_slice(), children);

    let product = SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    };
    assert_eq!(product.head, AtomId::new(0));

    let function = SemanticProductConstructor::function(2, 1);
    let generic = SemanticProductConstructor::generic(3);
    let union = SemanticProductConstructor::UNION;
    let intersection = SemanticProductConstructor::INTERSECTION;
    assert_eq!(function.validate(3), Ok(()));
    assert_eq!(generic.validate(3), Ok(()));
    assert_eq!(union.validate(3), Ok(()));
    assert_eq!(intersection.validate(3), Ok(()));

    assert_eq!(
        function.validate(2),
        Err(ProductConstructorFault::Arity {
            tag: ProductConstructorTag::Function,
            expected: 3,
            actual: 2,
        })
    );
    assert_eq!(
        SemanticProductConstructor::try_from_parts(99, 0, 0, 0),
        Err(ProductConstructorFault::Tag { actual: 99 })
    );
    Ok(())
}
