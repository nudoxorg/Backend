use nudox_ir_format::{
    EntityFault, EntityRecord, EntityType, FragmentError, FragmentView, PrepareError,
    PreparedFragment, PrimitiveType, TypeNode, TypeNodeFault, WriteError,
};
use nudox_ir_vocab::{EntityId, TypeId};
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

#[test]
fn zero_one_two_cartesian_consumers_preserve_caller_regions() -> Result<(), TestFailure> {
    let entities = [
        EntityRecord {
            semantic_type: TypeId::new(0),
        },
        EntityRecord {
            semantic_type: TypeId::new(0),
        },
    ];
    let expected_entities = [
        EntityType {
            entity: EntityId::new(0),
            semantic_type: TypeId::new(0),
        },
        EntityType {
            entity: EntityId::new(1),
            semantic_type: TypeId::new(0),
        },
    ];
    let nodes = [
        TypeNode::Primitive(PrimitiveType::Bool),
        TypeNode::Reference(TypeId::new(0)),
    ];

    for entity_count in 0..=2 {
        for type_node_count in 0..=2 {
            let selected_nodes = if type_node_count == 1 {
                &nodes[..1]
            } else {
                &nodes[..type_node_count]
            };
            let prepared =
                match PreparedFragment::prepare(&entities[..entity_count], selected_nodes) {
                    Ok(prepared) => prepared,
                    Err(error) if entity_count > 0 && type_node_count == 0 => {
                        assert_eq!(
                            error,
                            PrepareError::Entity {
                                ordinal: EntityId::new(0),
                                fault: EntityFault {
                                    target: TypeId::new(0),
                                    node_count: 0,
                                },
                            }
                        );
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                };
            let mut output = [0xa5; 128];
            let prefix_len = prepared.output_len();
            let prefix = prepared.write_into(&mut output)?;
            assert_eq!(prefix.len(), prefix_len);
            let view = FragmentView::validate(prefix)?;
            assert_eq!(view.as_ref().as_ptr(), prefix.as_ptr());
            assert!(
                view.entities()
                    .eq(expected_entities[..entity_count].iter().copied())
            );
            assert!(view.type_nodes().eq(selected_nodes.iter().copied()));
            assert_eq!(&output[prefix_len..], &[0xa5; 128][prefix_len..]);
        }
    }
    Ok(())
}

#[test]
fn self_forward_and_backward_type_edges_round_trip() -> Result<(), TestFailure> {
    let nodes = [
        TypeNode::Reference(TypeId::new(0)),
        TypeNode::Reference(TypeId::new(2)),
        TypeNode::Reference(TypeId::new(1)),
    ];
    let prepared = PreparedFragment::prepare(&[], &nodes)?;
    let mut output = [0; 128];
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
        PreparedFragment::prepare(&[], &nodes).err(),
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
        },
        EntityRecord {
            semantic_type: TypeId::new(0),
        },
    ];
    let nodes = [
        TypeNode::Primitive(PrimitiveType::Bool),
        TypeNode::Reference(TypeId::new(0)),
    ];
    let required = PreparedFragment::prepare(&entities, &nodes)?.output_len();
    let prepared = PreparedFragment::prepare(&entities, &nodes)?;
    for available in 0..required {
        let mut output = [0xa5; 128];
        let before = output;
        let result = prepared.write_into(&mut output[..available]);
        assert_eq!(
            result,
            Err(WriteError::OutputTooSmall {
                required,
                available,
            })
        );
        assert_eq!(output, before);
    }
    let mut retry_output = [0xa5; 128];
    let retry = prepared.write_into(&mut retry_output[..required])?;
    assert_eq!(retry.len(), required);
    Ok(())
}
