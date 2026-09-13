//! Transitive reference-index construction and checked closure validation.

use super::{
    BTreeSet, CanonicalValue, CheckpointWriteReport, Debug, DurableCache,
    MAX_REFERENCE_INDEX_DEPTH, ObjectRef, ReferenceIndexInput, object_exists,
};
#[cfg(test)]
use super::{DecodedManifest, WorkScope, store_error};

use super::persist;
use super::persist::persist_object;
#[cfg(test)]
use backend_store::{ClosureManifest, RelationAdmissionRegistry};
use backend_store::{
    FileStore, RawRelation, StoreError, StoredValue, TypedObject, UntrustedObjectId,
};
use backend_version::{PersistentTree, SchemaIdentity};

fn load_reference_index(
    store: &FileStore,
    object_id: [u8; 32],
) -> Result<Option<(TypedObject, ObjectRef)>, StoreError> {
    if !object_exists(store, &object_id)? {
        return Ok(None);
    }
    let object = store.read_object_claim(UntrustedObjectId::from_bytes(object_id))?;
    if object.schema() != SchemaIdentity::of_relation::<RawRelation>() {
        return Err(StoreError::Corrupt);
    }
    let reference = ObjectRef {
        logical: *object.version(),
        object: object_id,
    };
    Ok(Some((object, reference)))
}

#[cfg(test)]
pub(super) fn collect_reference_index_objects(
    store: &FileStore,
    object_id: [u8; 32],
    registry: &RelationAdmissionRegistry,
    scope: &mut WorkScope,
    index_objects: &mut BTreeSet<[u8; 32]>,
    retained_objects: &mut BTreeSet<[u8; 32]>,
) -> Result<u16, StoreError> {
    if !index_objects.insert(object_id) {
        return Err(StoreError::Corrupt);
    }
    if index_objects.len() > usize::from(MAX_REFERENCE_INDEX_DEPTH) + 1 {
        return Err(StoreError::Bounds);
    }
    let object = store.read_object_claim(UntrustedObjectId::from_bytes(object_id))?;
    if object.schema() != SchemaIdentity::of_relation::<RawRelation>() {
        return Err(StoreError::Corrupt);
    }
    scope
        .charge_bytes(object.bytes().len())
        .map_err(store_error)?;
    let single = ClosureManifest::new_with_registry(vec![object.clone()], registry)
        .map_err(|_| StoreError::Corrupt)?;
    let edges = single.object_edges(registry)?;
    let mut depth = 0_u16;
    for edge in edges {
        if edge.from() != object.id() {
            return Err(StoreError::Corrupt);
        }
        let target = *edge.to().as_bytes();
        let target_object = store.read_object(edge.to())?;
        if target_object.schema() == SchemaIdentity::of_relation::<RawRelation>() {
            let child_depth = collect_reference_index_objects(
                store,
                target,
                registry,
                scope,
                index_objects,
                retained_objects,
            )?;
            let edge_depth = child_depth.checked_add(1).ok_or(StoreError::Bounds)?;
            depth = depth.max(edge_depth);
        } else {
            retained_objects.insert(target);
        }
    }
    Ok(depth)
}

pub(super) fn persist_reference_index(
    store: &FileStore,
    objects: &BTreeSet<[u8; 32]>,
    report: &mut CheckpointWriteReport,
) -> Result<(TypedObject, ObjectRef), StoreError> {
    let key = b"flow.trace.refs.v1\0".to_vec();
    let value = StoredValue::new(Vec::new(), 1, objects.iter().copied().collect());
    let tree = PersistentTree::<RawRelation>::from_sorted_items(&[(key, value)])
        .map_err(|_| StoreError::Corrupt)?;
    let object = TypedObject::from_state_root(tree.root().commitment(), tree.root())?;
    persist_object(store, &object, report)?;
    let object_id = *object.id().as_bytes();
    let logical = *object.version();
    Ok((
        object,
        ObjectRef {
            logical,
            object: object_id,
        },
    ))
}

pub(super) fn select_reference_index<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static>(
    input: &ReferenceIndexInput<'_, V>,
    store: &FileStore,
    cache: &mut DurableCache,
    report: &mut CheckpointWriteReport,
) -> Result<(TypedObject, ObjectRef, u16), StoreError> {
    let ReferenceIndexInput {
        arrangement,
        levels,
        history,
        new_objects,
    } = *input;
    let previous_index = cache
        .latest_index
        .and_then(|(root, next_batch, object, depth)| {
            (root == *arrangement.state.root().as_bytes() && next_batch == arrangement.next_batch)
                .then_some((object, depth))
        });
    let selected = if let Some((previous_object, previous_depth)) = previous_index {
        if let Some(previous) = load_reference_index(store, previous_object)? {
            if new_objects.is_empty() {
                (previous.0, previous.1, previous_depth)
            } else if previous_depth < MAX_REFERENCE_INDEX_DEPTH {
                let mut targets = new_objects.clone();
                targets.insert(previous_object);
                let (object, reference) = persist_reference_index(store, &targets, report)?;
                (object, reference, previous_depth + 1)
            } else {
                let retained_objects =
                    persist::collect_current_objects(arrangement, levels, history, store, cache)?;
                let (object, reference) =
                    persist_reference_index(store, &retained_objects, report)?;
                (object, reference, 0)
            }
        } else {
            let retained_objects =
                persist::collect_current_objects(arrangement, levels, history, store, cache)?;
            let (object, reference) = persist_reference_index(store, &retained_objects, report)?;
            (object, reference, 0)
        }
    } else {
        let retained_objects =
            persist::collect_current_objects(arrangement, levels, history, store, cache)?;
        let (object, reference) = persist_reference_index(store, &retained_objects, report)?;
        (object, reference, 0)
    };
    Ok(selected)
}

#[cfg(test)]
pub(super) fn validate_reference_index(
    decoded: &DecodedManifest,
    store: &FileStore,
    scope: &mut WorkScope,
    retained_objects: &BTreeSet<[u8; 32]>,
) -> Result<(), StoreError> {
    let reference_index = store.read_object_claim(UntrustedObjectId::from_bytes(
        decoded.reference_index.object,
    ))?;
    if reference_index.schema() != SchemaIdentity::of_relation::<RawRelation>()
        || reference_index.version() != &decoded.reference_index.logical
    {
        return Err(StoreError::Corrupt);
    }
    let registry = RelationAdmissionRegistry::new();
    let mut index_objects = BTreeSet::new();
    let mut edge_targets = BTreeSet::new();
    let index_depth = collect_reference_index_objects(
        store,
        decoded.reference_index.object,
        &registry,
        scope,
        &mut index_objects,
        &mut edge_targets,
    )?;
    if index_depth != decoded.reference_index_depth || edge_targets != *retained_objects {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}
