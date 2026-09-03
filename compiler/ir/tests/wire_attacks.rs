//! Exercises the `compiler-ir` tests wire-attacks contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use compiler_ir::{
    AtomFault, AtomInput, DirectoryFault, EntityKind, EntityNameFault, EntityRecord,
    EntityRecordFault, FragmentError, FragmentView, PrepareError, PreparedFragment, PrimitiveType,
    RecipeFactFault, SourceIdentity, SourceIdentityFault, TypeNode, TypeNodeFault, WriteError,
};
use compiler_ir::{AtomId, EntityId, TypeId};
use compiler_vocabulary::{
    CompileRecipeFact, Language, LanguageProfile, NativeTool, RustEdition, Stage,
};
use heart_identity::{ContentId, SourceFactDomain, ToolchainDomain};
use thiserror::Error;

const HEADER_BYTES: usize = 12;
const DIRECTORY_BYTES: usize = 16;
const ENTITY_DIRECTORY: usize = HEADER_BYTES;
const TYPE_DIRECTORY: usize = HEADER_BYTES + DIRECTORY_BYTES;
const ENTITY_FLAGS: usize = ENTITY_DIRECTORY + 2;
const ENTITY_OFFSET: usize = ENTITY_DIRECTORY + 8;
const ENTITY_BYTE_LENGTH: usize = ENTITY_DIRECTORY + 12;
const TYPE_KIND: usize = TYPE_DIRECTORY;
const TYPE_OFFSET: usize = TYPE_DIRECTORY + 8;
const ENTITY_RECORD: usize = HEADER_BYTES + DIRECTORY_BYTES * 6;
const TYPE_RECORD: usize = ENTITY_RECORD + 12;
const ATOM_RECORD: usize = TYPE_RECORD + 8;
const ATOM_BYTES: usize = ATOM_RECORD + 8;
const SOURCE_RECORD: usize = ATOM_BYTES + 5;
const RECIPE_RECORD: usize = SOURCE_RECORD + 36;
const CANONICAL_LENGTH: usize = RECIPE_RECORD + 68;

#[derive(Debug, Error)]
enum TestFailure {
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Write(#[from] WriteError),
    #[error(transparent)]
    Validate(#[from] FragmentError),
}

fn source_identity() -> SourceIdentity {
    SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"golden-source"),
        byte_len: 13,
    }
}

fn recipe_fact() -> CompileRecipeFact {
    CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source_identity().identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"golden-toolchain"),
    )
}

fn canonical() -> Result<[u8; CANONICAL_LENGTH], TestFailure> {
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Constant,
    }];
    let nodes = [TypeNode::Primitive(PrimitiveType::Bool)];
    let atoms = [AtomInput { bytes: b"alpha" }];
    let prepared =
        PreparedFragment::prepare(source_identity(), recipe_fact(), &entities, &nodes, &atoms)?;
    assert_eq!(prepared.required_capacity(), CANONICAL_LENGTH);
    let mut output = [0; CANONICAL_LENGTH];
    prepared.write_into(&mut output)?;
    Ok(output)
}

#[test]
fn canonical_fixture_has_exact_closed_layout_and_semantic_lanes() -> Result<(), TestFailure> {
    let bytes = canonical()?;
    assert_eq!(
        &bytes[..HEADER_BYTES],
        &[78, 88, 73, 82, 2, 0, 6, 0, 245, 0, 0, 0]
    );
    assert_eq!(
        &bytes[ENTITY_DIRECTORY..ENTITY_DIRECTORY + DIRECTORY_BYTES],
        &[1, 0, 1, 0, 1, 0, 0, 0, 108, 0, 0, 0, 12, 0, 0, 0]
    );
    assert_eq!(
        &bytes[TYPE_DIRECTORY..TYPE_DIRECTORY + DIRECTORY_BYTES],
        &[2, 0, 1, 0, 1, 0, 0, 0, 120, 0, 0, 0, 8, 0, 0, 0]
    );
    assert_eq!(
        &bytes[ENTITY_RECORD..ENTITY_RECORD + 12],
        &[0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0]
    );
    assert_eq!(&bytes[TYPE_RECORD..TYPE_RECORD + 8], &[0; 8]);
    assert_eq!(
        &bytes[ATOM_RECORD..ATOM_RECORD + 8],
        &[0, 0, 0, 0, 5, 0, 0, 0]
    );
    assert_eq!(&bytes[ATOM_BYTES..SOURCE_RECORD], b"alpha");
    assert_eq!(&bytes[SOURCE_RECORD..SOURCE_RECORD + 4], &[13, 0, 0, 0]);
    assert_eq!(
        &bytes[SOURCE_RECORD + 4..RECIPE_RECORD],
        source_identity().identity.as_ref().as_slice()
    );
    Ok(())
}

#[test]
fn every_canonical_prefix_rejects_with_header_or_geometry_priority() -> Result<(), TestFailure> {
    let bytes = canonical()?;
    for actual in 0..bytes.len() {
        let expected = if actual < HEADER_BYTES {
            FragmentError::TruncatedHeader {
                required: HEADER_BYTES,
                actual,
            }
        } else {
            FragmentError::DeclaredLength {
                declared: bytes.len(),
                actual,
            }
        };
        assert_eq!(
            FragmentView::validate(&bytes[..actual]).err(),
            Some(expected)
        );
    }
    Ok(())
}

#[test]
fn header_and_directory_mutations_keep_exact_operands() -> Result<(), TestFailure> {
    let golden = canonical()?;
    let mut bytes = golden;
    bytes[0] = 0;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::Magic {
            actual: [0, 88, 73, 82],
        })
    );
    bytes = golden;
    bytes[4] = 3;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
            Some(FragmentError::Schema { actual: 3 })
    );
    bytes = golden;
    bytes[ENTITY_FLAGS] = 3;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::Directory {
            ordinal: 0,
            fault: DirectoryFault::Flags { kind: 1, actual: 3 },
        })
    );
    bytes = golden;
    bytes[ENTITY_OFFSET] = 109;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::Directory {
            ordinal: 0,
            fault: DirectoryFault::Offset {
                kind: 1,
                expected: ENTITY_RECORD,
                actual: 109,
            },
        })
    );
    bytes = golden;
    bytes[TYPE_OFFSET] = 119;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::Directory {
            ordinal: 1,
            fault: DirectoryFault::Offset {
                kind: 2,
                expected: TYPE_RECORD,
                actual: 119,
            },
        })
    );
    bytes = golden;
    bytes[ENTITY_BYTE_LENGTH] = 13;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::Directory {
            ordinal: 0,
            fault: DirectoryFault::ByteLength {
                kind: 1,
                expected: 12,
                actual: 13,
            },
        })
    );
    bytes = golden;
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
    Ok(())
}

#[test]
fn semantic_and_authority_mutations_fail_before_a_borrowed_view() -> Result<(), TestFailure> {
    let golden = canonical()?;
    let mut bytes = golden;
    bytes[ENTITY_RECORD + 4] = 1;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::EntityRecord {
            ordinal: EntityId::new(0),
            fault: EntityRecordFault::Name(EntityNameFault {
                target: AtomId::new(1),
                atom_count: 1,
            }),
        })
    );
    bytes = golden;
    bytes[ENTITY_RECORD + 8] = 13;
    assert!(matches!(
        FragmentView::validate(&bytes),
        Err(FragmentError::EntityRecord {
            fault: EntityRecordFault::Kind { actual: 13 },
            ..
        })
    ));
    bytes = golden;
    bytes[ATOM_RECORD] = 1;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::Atom {
            fault: AtomFault::Range {
                ordinal: AtomId::new(0),
                start: 1,
                length: 5,
                byte_count: 5,
            },
        })
    );
    bytes = golden;
    bytes[SOURCE_RECORD + 4] = 0;
    assert!(matches!(
        FragmentView::validate(&bytes),
        Err(FragmentError::SourceIdentity {
            fault: SourceIdentityFault::Authority(_),
        })
    ));
    bytes = golden;
    bytes[RECIPE_RECORD] = u8::from(Language::Python);
    assert!(matches!(
        FragmentView::validate(&bytes),
        Err(FragmentError::RecipeFact {
            fault: RecipeFactFault::IdentityRelation { expected, observed },
        }) if expected != observed
    ));
    Ok(())
}

#[test]
fn type_mutations_keep_node_ordinal_and_first_fault() -> Result<(), TestFailure> {
    let golden = canonical()?;
    let mut bytes = golden;
    bytes[TYPE_RECORD + 1] = 7;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::TypeNode {
            ordinal: TypeId::new(0),
            fault: TypeNodeFault::Reserved { actual: [7, 0, 0] },
        })
    );
    bytes = golden;
    bytes[TYPE_RECORD] = 9;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::TypeNode {
            ordinal: TypeId::new(0),
            fault: TypeNodeFault::Tag { actual: 9 },
        })
    );
    bytes = golden;
    bytes[TYPE_RECORD + 4] = 7;
    assert_eq!(
        FragmentView::validate(&bytes).err(),
        Some(FragmentError::TypeNode {
            ordinal: TypeId::new(0),
            fault: TypeNodeFault::Primitive { actual: 7 },
        })
    );
    Ok(())
}

#[test]
fn cardinality_is_not_calibrated_to_the_zero_one_two_matrix() -> Result<(), TestFailure> {
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Record,
    }; 4];
    let nodes = [
        TypeNode::Primitive(PrimitiveType::Bool),
        TypeNode::Reference(TypeId::new(0)),
        TypeNode::Reference(TypeId::new(3)),
        TypeNode::Reference(TypeId::new(1)),
    ];
    let atoms = [AtomInput { bytes: b"quad" }];
    let prepared =
        PreparedFragment::prepare(source_identity(), recipe_fact(), &entities, &nodes, &atoms)?;
    let mut output = [0; 512];
    let prefix = prepared.write_into(&mut output)?;
    let view = FragmentView::validate(prefix)?;
    assert_eq!(view.entities().len(), entities.len());
    assert_eq!(view.type_nodes().len(), nodes.len());
    assert_eq!(view.atoms().len(), atoms.len());
    Ok(())
}
