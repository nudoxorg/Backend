//! Boundary-local CAS persistence for visible nodes, runs, and level layouts.

use super::{
    Arc, Arrangement, BTreeMap, BTreeSet, BatchRecord, CanonicalValue, CheckpointWriteReport,
    Debug, Delta, DurableCache, HistoryRef, NodeChild, NodeSchema, ObjectRef, PersistedLevels, Run,
    TreeNodeView, object_exists, store_error,
};

use super::codec;
use crate::batch::canonical_bytes;
use backend_store::{FileStore, ObjectWriteReceipt, StoreError, TypedObject};
use backend_version::{ObjectKey, ObjectVersion};

fn account_object(
    receipt: ObjectWriteReceipt,
    report: &mut CheckpointWriteReport,
) -> Result<(), StoreError> {
    if !receipt.created() {
        return Ok(());
    }
    report.objects_written = report
        .objects_written
        .checked_add(1)
        .ok_or(StoreError::Bounds)?;
    report.bytes_written = report
        .bytes_written
        .checked_add(receipt.bytes())
        .ok_or(StoreError::Bounds)?;
    Ok(())
}

pub(super) fn persist_object(
    store: &FileStore,
    object: &TypedObject,
    report: &mut CheckpointWriteReport,
) -> Result<bool, StoreError> {
    let receipt = store.write_object_with_receipt(object)?;
    account_object(receipt, report)?;
    Ok(receipt.created())
}

pub(super) fn persist_node<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static>(
    view: TreeNodeView<'_, crate::ArrangementRelation<V>>,
    store: &FileStore,
    cache: &mut DurableCache,
    report: &mut CheckpointWriteReport,
    new_objects: &mut BTreeSet<[u8; 32]>,
) -> Result<ObjectRef, StoreError> {
    let logical = view.id().to_bytes();
    if let Some(object) = cache.nodes.get(&logical).copied() {
        if object_exists(store, &object)? {
            return Ok(ObjectRef { logical, object });
        }
        cache.nodes.remove(&logical);
    }

    let payload = if let Some(entries) = view.entries() {
        codec::encode_leaf_node(logical, view.level(), entries).map_err(store_error)?
    } else {
        let mut children = Vec::new();
        for child in view.children() {
            let child_ref = persist_node(child, store, cache, report, new_objects)?;
            let first = child.first_key().cloned().ok_or(StoreError::Corrupt)?;
            children.push(NodeChild {
                logical: child_ref.logical,
                object: child_ref.object,
                first,
                row_count: child.row_count(),
            });
        }
        codec::encode_branch_node(logical, view.level(), &children).map_err(store_error)?
    };
    let object = TypedObject::from_value(&ObjectKey::<NodeSchema>::from_value(&payload), &payload);
    persist_object(store, &object, report)?;
    let object_id = *object.id().as_bytes();
    cache.nodes.insert(logical, object_id);
    new_objects.insert(object_id);
    Ok(ObjectRef {
        logical,
        object: object_id,
    })
}

pub(super) fn run_object<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static>(
    run: &Run<V>,
    store: &FileStore,
    cache: &mut DurableCache,
    report: &mut CheckpointWriteReport,
    new_objects: &mut BTreeSet<[u8; 32]>,
) -> Result<ObjectRef, StoreError> {
    let logical = *run.root().as_bytes();
    if let Some(object) = cache.runs.get(&logical).copied() {
        if object_exists(store, &object)? {
            return Ok(ObjectRef { logical, object });
        }
        cache.runs.remove(&logical);
    }
    let rows = run.rows().cloned().collect::<Vec<_>>();
    let payload = canonical_bytes(&rows, [0; 32]);
    let expected = ObjectVersion::<crate::RunSchema>::from_value(&payload);
    if expected.as_bytes() != run.root().as_bytes() {
        return Err(StoreError::Corrupt);
    }
    let object = TypedObject::from_version(
        &ObjectKey::<crate::RunSchema>::from_value(&payload),
        expected,
        &payload,
    )?;
    let created = persist_object(store, &object, report)?;
    if created {
        report.rows_written = report
            .rows_written
            .checked_add(u64::try_from(rows.len()).map_err(|_| StoreError::Bounds)?)
            .ok_or(StoreError::Bounds)?;
    }
    let object_id = *object.id().as_bytes();
    cache.runs.insert(logical, object_id);
    new_objects.insert(object_id);
    Ok(ObjectRef {
        logical,
        object: object_id,
    })
}

pub(super) fn delta_run_object<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static>(
    rows: &[Delta<V>],
    store: &FileStore,
    cache: &mut DurableCache,
    report: &mut CheckpointWriteReport,
    new_objects: &mut BTreeSet<[u8; 32]>,
) -> Result<ObjectRef, StoreError> {
    let payload = canonical_bytes(rows, [0; 32]);
    let expected = ObjectVersion::<crate::RunSchema>::from_value(&payload);
    let logical = *expected.as_bytes();
    if let Some(object_id) = cache.runs.get(&logical).copied() {
        if object_exists(store, &object_id)? {
            return Ok(ObjectRef {
                logical,
                object: object_id,
            });
        }
        cache.runs.remove(&logical);
    }
    let object = TypedObject::from_version(
        &ObjectKey::<crate::RunSchema>::from_value(&payload),
        expected,
        &payload,
    )?;
    let created = persist_object(store, &object, report)?;
    if created {
        report.rows_written = report
            .rows_written
            .checked_add(u64::try_from(rows.len()).map_err(|_| StoreError::Bounds)?)
            .ok_or(StoreError::Bounds)?;
    }
    let object_id = *object.id().as_bytes();
    cache.runs.insert(logical, object_id);
    new_objects.insert(object_id);
    Ok(ObjectRef {
        logical,
        object: object_id,
    })
}

pub(super) fn collect_current_objects<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static>(
    arrangement: &Arrangement<V>,
    levels: &[Vec<ObjectRef>],
    history: &[HistoryRef],
    store: &FileStore,
    cache: &DurableCache,
) -> Result<BTreeSet<[u8; 32]>, StoreError> {
    let mut objects = BTreeSet::new();
    let mut closure = arrangement.state.node_closure();
    while let Some(view) = closure.try_next().map_err(|_| StoreError::Bounds)? {
        let logical = view.id().to_bytes();
        let object = cache
            .nodes
            .get(&logical)
            .copied()
            .ok_or(StoreError::Corrupt)?;
        if !object_exists(store, &object)? {
            return Err(StoreError::Corrupt);
        }
        objects.insert(object);
    }
    if closure.error().is_some() {
        return Err(StoreError::Bounds);
    }
    for level in levels {
        objects.extend(level.iter().map(|reference| reference.object));
    }
    objects.extend(history.iter().map(|record| record.run.object));
    Ok(objects)
}

pub(super) fn prune_history_cache<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static>(
    arrangement: &Arrangement<V>,
    cache: &mut DurableCache,
) {
    if let (Some(oldest), Some(newest)) = (
        arrangement.history.first().map(|record| record.sequence),
        arrangement.history.last().map(|record| record.sequence),
    ) {
        cache
            .history
            .retain(|sequence, _| *sequence >= oldest && *sequence <= newest);
    } else {
        cache.history.clear();
    }
}

pub(super) fn persist_levels<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static>(
    levels: &[Vec<Arc<Run<V>>>],
    store: &FileStore,
    cache: &mut DurableCache,
    report: &mut CheckpointWriteReport,
    new_objects: &mut BTreeSet<[u8; 32]>,
) -> Result<PersistedLevels, StoreError> {
    let mut persisted = Vec::with_capacity(levels.len());
    let mut run_refs = BTreeMap::<[u8; 32], ObjectRef>::new();
    for level in levels {
        let mut refs = Vec::with_capacity(level.len());
        for run in level {
            let reference = run_object(run, store, cache, report, new_objects)?;
            run_refs.insert(reference.logical, reference);
            refs.push(reference);
        }
        persisted.push(refs);
    }
    Ok((persisted, run_refs))
}

pub(super) fn persist_history<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static>(
    history: &[BatchRecord<V>],
    run_refs: &BTreeMap<[u8; 32], ObjectRef>,
    store: &FileStore,
    cache: &mut DurableCache,
    report: &mut CheckpointWriteReport,
    new_objects: &mut BTreeSet<[u8; 32]>,
) -> Result<Vec<HistoryRef>, StoreError> {
    let mut persisted = Vec::with_capacity(history.len());
    for record in history {
        let reference = if let Some((logical, object)) =
            cache.history.get(&record.sequence).copied()
        {
            if object_exists(store, &object)? {
                ObjectRef { logical, object }
            } else {
                cache.history.remove(&record.sequence);
                let payload = canonical_bytes(&record.deltas, [0; 32]);
                let logical = *ObjectVersion::<crate::RunSchema>::from_value(&payload).as_bytes();
                if let Some(reference) = run_refs.get(&logical).copied() {
                    reference
                } else {
                    delta_run_object(&record.deltas, store, cache, report, new_objects)?
                }
            }
        } else {
            let payload = canonical_bytes(&record.deltas, [0; 32]);
            let logical = *ObjectVersion::<crate::RunSchema>::from_value(&payload).as_bytes();
            if let Some(reference) = run_refs.get(&logical).copied() {
                reference
            } else {
                delta_run_object(&record.deltas, store, cache, report, new_objects)?
            }
        };
        cache
            .history
            .insert(record.sequence, (reference.logical, reference.object));
        persisted.push(HistoryRef {
            sequence: record.sequence,
            before: *record.before.as_bytes(),
            after: *record.after.as_bytes(),
            bytes: record.bytes,
            run: reference,
        });
    }
    Ok(persisted)
}
