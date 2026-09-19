//! Relation-node traversal and closure edge validation.

use super::{
    ClosureManifest, ObjectEdge, ObjectId, RelationAdmissionRegistry, StoreError, TypedObject,
};
use backend_version::{CanonicalRelation, RelationState};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn object_edges(
    manifest: &ClosureManifest,
    registry: &RelationAdmissionRegistry,
) -> Result<Vec<ObjectEdge>, StoreError> {
    manifest.admit_with_registry(registry)?;
    let relation_versions = manifest
        .objects()
        .iter()
        .filter_map(|object| {
            registry
                .admit_relation(object.schema(), object.version(), object.bytes())
                .ok()
                .map(|()| ((object.schema(), *object.version()), object.id()))
        })
        .collect::<BTreeMap<_, _>>();
    let mut edges = Vec::new();
    for object in manifest.objects() {
        if registry
            .admit_relation(object.schema(), object.version(), object.bytes())
            .is_err()
        {
            continue;
        }
        let references =
            registry.node_references(object.schema(), object.version(), object.bytes())?;
        let children = references.children;
        let value_references = references.value_references;
        for child in children {
            let target = relation_versions
                .get(&(object.schema(), child))
                .copied()
                .ok_or(StoreError::Corrupt)?;
            edges.push(ObjectEdge::new(object.id(), target));
        }
        for reference in value_references {
            edges.push(ObjectEdge::new(
                object.id(),
                ObjectId::from_bytes(reference),
            ));
        }
    }
    edges.sort_unstable();
    edges.dedup();
    Ok(edges)
}

pub(super) fn relation_node_objects<R: CanonicalRelation>(
    state: &RelationState<R>,
) -> Result<Vec<TypedObject>, StoreError> {
    let mut objects = Vec::new();
    let mut closure = state.node_closure();
    while let Some(node) = closure.try_next().map_err(|_| StoreError::Corrupt)? {
        objects.push(TypedObject::from_checked_state_object_ref(
            node.state_object(),
        )?);
    }
    objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    Ok(objects)
}

pub(super) fn verify_relation_children(
    objects: &[TypedObject],
    registry: &RelationAdmissionRegistry,
) -> Result<(), StoreError> {
    let mut admitted = BTreeSet::new();
    let mut relation_references = Vec::new();
    for object in objects {
        if registry.contains_schema(object.schema()) {
            let references =
                registry.node_references(object.schema(), object.version(), object.bytes())?;
            admitted.insert((object.schema(), *object.version()));
            relation_references.push((object.schema(), references.children));
        } else {
            object.verify_wire_version(registry)?;
        }
    }
    for (schema, children) in relation_references {
        for child in children {
            if !admitted.contains(&(schema, child)) {
                return Err(StoreError::Corrupt);
            }
        }
    }
    Ok(())
}
