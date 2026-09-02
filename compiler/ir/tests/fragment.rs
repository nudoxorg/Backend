//! Exercises the `compiler-ir` tests fragment contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use compiler_ir::{AtomId, EntityId, TypeId};
use compiler_ir::{
    AtomInput, EntityKind, EntityRecord, EntityRecordFault, EntityType, FragmentError,
    FragmentView, PrepareError, PreparedFragment, PrimitiveType, SourceIdentity, TypeNode,
    TypeNodeFault, WriteError,
};
use compiler_vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage};
use heart_identity::{ContentId, SourceFactDomain, ToolchainDomain};
use thiserror::Error;

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
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"format-test-source"),
        byte_len: 18,
    }
}

fn recipe_fact() -> CompileRecipeFact {
    CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source_identity().identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"format-toolchain"),
    )
}

#[test]
fn zero_one_two_cartesian_consumers_preserve_caller_regions() -> Result<(), TestFailure> {
    let entities = [
        EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Constant,
        },
        EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Constant,
        },
    ];
    let expected_entities = [
        EntityType {
            entity: EntityId::new(0),
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Constant,
        },
        EntityType {
            entity: EntityId::new(1),
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Constant,
        },
    ];
    let nodes = [
        TypeNode::Primitive(PrimitiveType::Bool),
        TypeNode::Reference(TypeId::new(0)),
    ];
    let atoms = [AtomInput { bytes: b"alpha" }];

    for entity_count in 0..=2 {
        for type_node_count in 0..=2 {
            let selected_nodes = if type_node_count == 1 {
                &nodes[..1]
            } else {
                &nodes[..type_node_count]
            };
            let selected_atoms = if entity_count == 0 {
                &atoms[..0]
            } else {
                &atoms[..]
            };
            let prepared = match PreparedFragment::prepare(
                source_identity(),
                recipe_fact(),
                &entities[..entity_count],
                selected_nodes,
                selected_atoms,
            ) {
                Ok(prepared) => prepared,
                Err(error) if entity_count > 0 && type_node_count == 0 => {
                    assert_eq!(
                        error,
                        PrepareError::Entity {
                            ordinal: EntityId::new(0),
                            fault: EntityRecordFault::Type(compiler_ir::EntityFault {
                                target: TypeId::new(0),
                                node_count: 0,
                            }),
                        }
                    );
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let mut output = [0xa5; 512];
            let prefix_len = prepared.required_capacity();
            let prefix = prepared.write_into(&mut output)?;
            assert_eq!(prefix.len(), prefix_len);
            let view = FragmentView::validate(prefix)?;
            assert_eq!(view.as_ref().as_ptr(), prefix.as_ptr());
            assert!(
                view.entities()
                    .eq(expected_entities[..entity_count].iter().copied())
            );
            assert!(view.type_nodes().eq(selected_nodes.iter().copied()));
            assert_eq!(view.atoms().count(), selected_atoms.len());
            assert_eq!(&output[prefix_len..], &[0xa5; 512][prefix_len..]);
        }
    }
    Ok(())
}

#[test]
fn semantic_atom_and_source_fact_round_trip() -> Result<(), TestFailure> {
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Function,
    }];
    let nodes = [TypeNode::Primitive(PrimitiveType::I32)];
    let atoms = [AtomInput { bytes: b"alpha" }];
    let source = source_identity();
    let prepared = PreparedFragment::prepare(source, recipe_fact(), &entities, &nodes, &atoms)?;
    let mut output = [0; 256];
    let view = FragmentView::validate(prepared.write_into(&mut output)?)?;
    assert_eq!(view.source, source);
    assert_eq!(
        view.entities().next().map(|entity| entity.name),
        Some(AtomId::new(0))
    );
    assert_eq!(
        view.atoms().next().map(|atom| atom.bytes),
        Some(&b"alpha"[..])
    );
    Ok(())
}

#[test]
fn self_forward_and_backward_type_edges_round_trip() -> Result<(), TestFailure> {
    let nodes = [
        TypeNode::Reference(TypeId::new(0)),
        TypeNode::Reference(TypeId::new(2)),
        TypeNode::Reference(TypeId::new(1)),
    ];
    let prepared = PreparedFragment::prepare(source_identity(), recipe_fact(), &[], &nodes, &[])?;
    let mut output = [0; 256];
    let prefix = prepared.write_into(&mut output)?;
    let view = FragmentView::validate(prefix)?;
    assert!(view.type_nodes().eq(nodes));
    Ok(())
}

#[test]
fn invalid_edge_keeps_source_target_and_node_count() {
    let nodes = [
        TypeNode::Primitive(PrimitiveType::I32),
        TypeNode::Reference(TypeId::new(2)),
    ];
    assert_eq!(
        PreparedFragment::prepare(source_identity(), recipe_fact(), &[], &nodes, &[]).err(),
        Some(PrepareError::TypeNode {
            ordinal: TypeId::new(1),
            fault: TypeNodeFault::Edge {
                target: TypeId::new(2),
                node_count: 2,
            },
        })
    );
}

#[test]
fn every_undersized_output_is_byte_for_byte_unchanged() -> Result<(), TestFailure> {
    let entities = [
        EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Constant,
        },
        EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Constant,
        },
    ];
    let nodes = [
        TypeNode::Primitive(PrimitiveType::Bool),
        TypeNode::Reference(TypeId::new(0)),
    ];
    let atoms = [AtomInput { bytes: b"alpha" }];
    let required =
        PreparedFragment::prepare(source_identity(), recipe_fact(), &entities, &nodes, &atoms)?
            .required_capacity();
    let prepared =
        PreparedFragment::prepare(source_identity(), recipe_fact(), &entities, &nodes, &atoms)?;
    for available in 0..required {
        let mut output = [0xa5; 512];
        let before = output;
        let result = prepared.write_into(&mut output[..available]);
        assert!(matches!(
            result,
            Err(WriteError::OutputTooSmall {
                required: observed_required,
                available: observed_available,
            }) if observed_required == required && observed_available == available
        ));
        assert_eq!(output, before);
    }
    let mut retry_output = [0xa5; 512];
    let retry = prepared.write_into(&mut retry_output[..required])?;
    assert_eq!(retry.len(), required);
    Ok(())
}
