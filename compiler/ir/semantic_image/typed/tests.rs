//! Unrun hostile falsifiers for canonical typed-pool planning.

use crate::{
    BorrowedTree, BuiltinType, ConcreteType, CorePayloadHash,
    DeclarationFamilyId, EntityAuthorityFacts, EntityVersion, FactAvailability,
    Ir, IrBuilder, ItemKind, ParentageAuthority, TreeItemInput,
    VariantFingerprint, Visibility,
};

use super::*;

fn version(value: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([value; 16]),
        variant: VariantFingerprint::from_raw([value.wrapping_add(1); 16]),
        core_payload: CorePayloadHash::from_raw([value.wrapping_add(2); 16]),
    }
}

fn typed_image(reverse: bool) -> Result<Ir, crate::BuildError> {
    let mut builder = IrBuilder::new();
    let (string, boolean) = if reverse {
        let boolean = builder.intern_concrete(ConcreteType::Builtin(BuiltinType::Bool))?;
        let string = builder.intern_concrete(ConcreteType::Builtin(BuiltinType::String))?;
        (string.erase(), boolean.erase())
    } else {
        let string = builder.intern_concrete(ConcreteType::Builtin(BuiltinType::String))?;
        let boolean = builder.intern_concrete(ConcreteType::Builtin(BuiltinType::Bool))?;
        (string.erase(), boolean.erase())
    };
    builder.intern_types(&[string, boolean])?;
    let authority = EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        members: FactAvailability::Captured,
        visibility: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    };
    let item = TreeItemInput {
        name: b"typed",
        kind: ItemKind::TypeAlias,
        visibility: Visibility::Private,
        authority,
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    };
    let versions = [version(9)];
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &[item],
        links: &[],
    })?;
    builder.finish()
}

fn canonical_shape(plan: &TypedDependencyPlan<'_>) -> Vec<(TypedPlanDomain, TypedFingerprint)> {
    plan.scratch
        .canonical_nodes
        .iter()
        .copied()
        .map(|node| {
            let slot = plan.slot(node).expect("planner nodes have validated slots");
            (node.domain(), plan.scratch.fingerprints[slot])
        })
        .collect()
}

#[test]
fn typed_dependency_plan_is_insertion_order_invariant() -> Result<(), crate::BuildError> {
    let first = typed_image(false)?;
    let reverse = typed_image(true)?;
    let first = TypedDependencyPlan::build(&first).expect("first typed plan is admitted");
    let reverse = TypedDependencyPlan::build(&reverse).expect("reverse typed plan is admitted");
    assert_eq!(canonical_shape(&first), canonical_shape(&reverse));
    Ok(())
}

#[test]
fn anonymous_cycle_is_a_typed_terminal_not_a_synthetic_type() -> Result<(), crate::BuildError> {
    let ir = typed_image(false)?;
    let mut plan = TypedDependencyPlan::build(&ir).expect("baseline typed plan is admitted");
    let node = plan
        .scratch
        .nodes
        .iter()
        .copied()
        .find(|node| matches!(node, TypedPlanNode::Type(_)))
        .expect("fixture has one type row");
    plan.scratch.edges.clear();
    plan.scratch.edges.push(TypedPlanEdge {
        role: TypedEdgeRole::TypeTag,
        target: TypedPlanTarget::Node(node),
    });
    plan.scratch.edge_offsets.clear();
    plan.scratch.edge_offsets.push(0);
    for _ in 0..plan.scratch.nodes.len() {
        plan.scratch.edge_offsets.push(1);
    }
    plan.scratch.states.fill(0);
    plan.scratch.stack.clear();
    plan.scratch.postorder.clear();
    assert!(matches!(
        plan.build_postorder(),
        Err(TypedPlanFault::AnonymousCycle { from, to }) if from == node && to == node
    ));
    Ok(())
}

#[test]
fn equal_digest_fallback_compares_structure_without_aliasing() -> Result<(), crate::BuildError> {
    let ir = typed_image(false)?;
    let mut plan = TypedDependencyPlan::build(&ir).expect("baseline typed plan is admitted");
    plan.scratch
        .fingerprints
        .fill(TypedFingerprint::from_raw([7; 32]));
    plan.scratch.canonical_nodes.clear();
    plan.scratch.canonical_slots.fill(u32::MAX);
    plan.order_canonical_nodes()
        .expect("distinct rows with a forced digest collision compare exactly");
    let types = plan
        .scratch
        .nodes
        .iter()
        .copied()
        .filter(|node| matches!(node, TypedPlanNode::Type(_)))
        .collect::<Vec<_>>();
    assert_eq!(types.len(), 2);
    let first = plan.slot(types[0]).expect("validated first type slot");
    let second = plan.slot(types[1]).expect("validated second type slot");
    assert_ne!(plan.scratch.canonical_slots[first], plan.scratch.canonical_slots[second]);
    Ok(())
}

#[test]
fn identical_raw_typed_rows_are_rejected_after_exact_comparison() -> Result<(), crate::BuildError> {
    let ir = typed_image(false)?;
    let mut plan = TypedDependencyPlan::build(&ir).expect("baseline typed plan is admitted");
    let types = plan
        .scratch
        .nodes
        .iter()
        .copied()
        .filter(|node| matches!(node, TypedPlanNode::Type(_)))
        .collect::<Vec<_>>();
    assert_eq!(types.len(), 2);
    let (first_start, first_end) = plan.edge_range(types[0]).expect("first type range");
    let (second_start, second_end) = plan.edge_range(types[1]).expect("second type range");
    assert_eq!(first_end - first_start, second_end - second_start);
    let first_edges = plan.scratch.edges[first_start..first_end].to_vec();
    plan.scratch.edges[second_start..second_end].copy_from_slice(&first_edges);
    plan.scratch
        .fingerprints
        .fill(TypedFingerprint::from_raw([9; 32]));
    plan.scratch.canonical_nodes.clear();
    assert!(matches!(
        plan.order_canonical_nodes(),
        Err(TypedPlanError::Typed(TypedPlanFault::DuplicateCanonicalKey { .. }))
    ));
    Ok(())
}
