use nudox_ir_format::{
    FragmentError, FragmentView, PrepareError, PreparedFragment, PrimitiveType, TypeNode,
    TypeNodeFault, WriteError,
};
use nudox_ir_vocab::{EntityId, TypeId};
use std::fmt;

enum TestFailure {
    Prepare(PrepareError),
    Write(WriteError),
    Validate(FragmentError),
}

impl fmt::Debug for TestFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prepare(error) => formatter.debug_tuple("prepare").field(error).finish(),
            Self::Write(error) => formatter.debug_tuple("write").field(error).finish(),
            Self::Validate(error) => formatter.debug_tuple("validate").field(error).finish(),
        }
    }
}

impl From<PrepareError> for TestFailure {
    fn from(error: PrepareError) -> Self {
        Self::Prepare(error)
    }
}

impl From<WriteError> for TestFailure {
    fn from(error: WriteError) -> Self {
        Self::Write(error)
    }
}

impl From<FragmentError> for TestFailure {
    fn from(error: FragmentError) -> Self {
        Self::Validate(error)
    }
}

#[test]
fn zero_one_two_cartesian_consumers_preserve_caller_regions() -> Result<(), TestFailure> {
    let entities = [EntityId::new(7), EntityId::new(11)];
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
            let prepared = PreparedFragment::prepare(&entities[..entity_count], selected_nodes)?;
            let mut output = [0xa5; 128];
            let prefix_len = prepared.output_len();
            let prefix = prepared.write_into(&mut output)?;
            assert_eq!(prefix.len(), prefix_len);
            let view = FragmentView::validate(prefix)?;
            assert_eq!(view.as_ref().as_ptr(), prefix.as_ptr());
            assert!(
                view.entity_ids()
                    .eq(entities[..entity_count].iter().copied())
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
    let entities = [EntityId::new(7), EntityId::new(11)];
    let nodes = [
        TypeNode::Primitive(PrimitiveType::Bool),
        TypeNode::Reference(TypeId::new(0)),
    ];
    let required = PreparedFragment::prepare(&entities, &nodes)?.output_len();
    for available in 0..required {
        let mut output = [0xa5; 128];
        let before = output;
        let result =
            PreparedFragment::prepare(&entities, &nodes)?.write_into(&mut output[..available]);
        assert_eq!(
            result,
            Err(WriteError::OutputTooSmall {
                required,
                available,
            })
        );
        assert_eq!(output, before);
    }
    Ok(())
}
