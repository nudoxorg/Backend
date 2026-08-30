use nudox_ir_format::{
    DirectoryFault, EntityRecord, FragmentError, FragmentView, PrepareError, PreparedFragment,
    PrimitiveType, TypeNode, TypeNodeFault, WriteError,
};
use nudox_ir_vocab::TypeId;
use thiserror::Error;

const GOLDEN: [u8; 56] = [
    78, 88, 73, 82, 1, 0, 2, 0, 56, 0, 0, 0, 1, 0, 1, 0, 1, 0, 0, 0, 44, 0, 0, 0, 4, 0, 0, 0, 2, 0,
    1, 0, 1, 0, 0, 0, 48, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
const OPTIONAL_GOLDEN: [u8; 73] = [
    78, 88, 73, 82, 1, 0, 3, 0, 73, 0, 0, 0, 1, 0, 1, 0, 1, 0, 0, 0, 60, 0, 0, 0, 4, 0, 0, 0, 2, 0,
    1, 0, 1, 0, 0, 0, 64, 0, 0, 0, 8, 0, 0, 0, 9, 0, 0, 0, 1, 0, 0, 0, 72, 0, 0, 0, 1, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 170,
];

const ENTITY_FLAGS: usize = 14;
const ENTITY_OFFSET: usize = 20;
const ENTITY_BYTE_LENGTH: usize = 24;
const TYPE_KIND: usize = 28;
const TYPE_OFFSET: usize = 36;
const TYPE_RECORD: usize = 48;

#[derive(Debug, Error)]
enum TestFailure {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Write(#[from] WriteError),
    #[error(transparent)]
    Validate(#[from] FragmentError),
}

#[test]
fn prepared_fragment_has_one_exact_independent_golden() -> Result<(), TestFailure> {
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
    }];
    let nodes = [TypeNode::Primitive(PrimitiveType::Bool)];
    let prepared = PreparedFragment::prepare(&entities, &nodes)?;
    let mut output = [0xa5; 64];
    let prefix = prepared.write_into(&mut output)?;
    assert_eq!(prefix, GOLDEN);
    assert_eq!(&output[GOLDEN.len()..], &[0xa5; 8]);
    Ok(())
}

#[test]
fn every_golden_prefix_rejects_with_header_or_geometry_priority() {
    for actual in 0..GOLDEN.len() {
        let expected = if actual < 12 {
            FragmentError::TruncatedHeader {
                required: 12,
                actual,
            }
        } else {
            FragmentError::DeclaredLength {
                declared: GOLDEN.len(),
                actual,
            }
        };
        assert_eq!(
            FragmentView::validate(&GOLDEN[..actual]).err(),
            Some(expected)
        );
    }
}

#[test]
fn header_and_trailing_mutations_preserve_declared_operands() {
    let mut bytes = GOLDEN;
    bytes[0] = 0;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::Magic {
            actual: [0, 88, 73, 82],
        })
    );
    bytes = GOLDEN;
    bytes[4] = 2;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::Schema { actual: 2 })
    );

    let mut trailing = [0; 57];
    trailing[..GOLDEN.len()].copy_from_slice(&GOLDEN);
    assert_eq!(
        FragmentView::validate(&trailing).err(),
        Some(FragmentError::DeclaredLength {
            declared: GOLDEN.len(),
            actual: trailing.len(),
        })
    );
}

#[test]
fn directory_mutations_keep_exact_ordinal_kind_and_operands() {
    let mut bytes = GOLDEN;
    bytes[ENTITY_FLAGS] = 3;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::Directory {
            ordinal: 0,
            fault: DirectoryFault::Flags { kind: 1, actual: 3 },
        })
    );

    bytes = GOLDEN;
    bytes[ENTITY_OFFSET] = 45;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::Directory {
            ordinal: 0,
            fault: DirectoryFault::Offset {
                kind: 1,
                expected: 44,
                actual: 45,
            },
        })
    );

    bytes = GOLDEN;
    bytes[TYPE_OFFSET] = 47;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::Directory {
            ordinal: 1,
            fault: DirectoryFault::Offset {
                kind: 2,
                expected: 48,
                actual: 47,
            },
        })
    );

    bytes = GOLDEN;
    bytes[ENTITY_BYTE_LENGTH] = 5;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::Directory {
            ordinal: 0,
            fault: DirectoryFault::ByteLength {
                kind: 1,
                expected: 4,
                actual: 5,
            },
        })
    );

    bytes = GOLDEN;
    bytes[TYPE_KIND] = 1;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::Directory {
            ordinal: 1,
            fault: DirectoryFault::Order {
                previous: 1,
                actual: 1,
            },
        })
    );

    bytes = GOLDEN;
    bytes[TYPE_KIND] = 3;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::Directory {
            ordinal: 1,
            fault: DirectoryFault::RequiredUnknown { kind: 3 },
        })
    );
}

#[test]
fn optional_unknown_sections_skip_but_required_unknown_sections_fail_closed()
-> Result<(), FragmentError> {
    let view = FragmentView::validate(&OPTIONAL_GOLDEN)?;
    assert_eq!(view.entities().len(), 1);
    assert_eq!(view.type_nodes().len(), 1);
    let mut required = OPTIONAL_GOLDEN;
    required[46] = 1;
    assert_eq!(
        FragmentView::validate(&required).err(),
        Some(FragmentError::Directory {
            ordinal: 2,
            fault: DirectoryFault::RequiredUnknown { kind: 9 },
        })
    );
    Ok(())
}

#[test]
fn type_mutations_keep_node_ordinal_and_first_fault() {
    let mut bytes = GOLDEN;
    bytes[TYPE_RECORD + 1] = 7;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::TypeNode {
            ordinal: TypeId::new(0),
            fault: TypeNodeFault::Reserved { actual: [7, 0, 0] },
        })
    );

    bytes = GOLDEN;
    bytes[TYPE_RECORD] = 9;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::TypeNode {
            ordinal: TypeId::new(0),
            fault: TypeNodeFault::Tag { actual: 9 },
        })
    );

    bytes = GOLDEN;
    bytes[TYPE_RECORD + 4] = 7;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::TypeNode {
            ordinal: TypeId::new(0),
            fault: TypeNodeFault::Primitive { actual: 7 },
        })
    );

    bytes = GOLDEN;
    bytes[ENTITY_FLAGS] = 3;
    bytes[TYPE_RECORD] = 9;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::Directory {
            ordinal: 0,
            fault: DirectoryFault::Flags { kind: 1, actual: 3 },
        })
    );

    bytes = GOLDEN;
    bytes[44] = 1;
    bytes[TYPE_RECORD] = 9;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::Entity {
            ordinal: nudox_ir_vocab::EntityId::new(0),
            fault: nudox_ir_format::EntityFault {
                target: TypeId::new(1),
                node_count: 1,
            },
        })
    );
}

#[test]
fn cardinality_is_not_calibrated_to_the_zero_one_two_matrix() -> Result<(), TestFailure> {
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
    }; 4];
    let nodes = [
        TypeNode::Primitive(PrimitiveType::Bool),
        TypeNode::Reference(TypeId::new(0)),
        TypeNode::Reference(TypeId::new(3)),
        TypeNode::Reference(TypeId::new(1)),
    ];
    let prepared = PreparedFragment::prepare(&entities, &nodes)?;
    let mut output = [0; 128];
    let prefix = prepared.write_into(&mut output)?;
    let view = FragmentView::validate(prefix)?;
    assert_eq!(view.entities().len(), entities.len());
    assert_eq!(view.type_nodes().len(), nodes.len());
    Ok(())
}
