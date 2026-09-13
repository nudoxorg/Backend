use std::sync::Arc;

use crate::ids::canonical_version_delta_id;
use crate::persistent::{TreeChange, TreeError};
use crate::{NodeError, Relation};

use super::{
    state::RelationState,
    transition::{Delta, DeltaError, DeltaWork, MapChange, PreparedDelta, canonical_changes},
};

/// Prepares a checked delta from a complete base state.
///
/// Effective no-op changes are removed from the canonical transition.  This
/// makes a no-op mutation equivalent to an empty transition and prevents
/// operation history from leaking into the logical delta identity.
///
/// # Errors
///
/// Returns [`DeltaError::IncompleteBase`] for partial input,
/// [`DeltaError::Unsorted`] or [`DeltaError::DuplicateKey`] for malformed
/// ordering, [`DeltaError::BeforeMismatch`] for a stale precondition, or
/// [`DeltaError::Canonical`] when the target cannot be represented.
pub fn prepare_delta<R: Relation>(
    base: &RelationState<R>,
    changes: Vec<MapChange<R>>,
) -> Result<Delta<R>, DeltaError> {
    prepare_delta_with_work(base, changes).map(|(delta, _)| delta)
}

/// Prepares a checked delta and reports immutable tree work counters.
///
/// # Errors
///
/// Returns the same validation errors as [`prepare_delta`].
pub fn prepare_delta_with_work<R: Relation>(
    base: &RelationState<R>,
    changes: Vec<MapChange<R>>,
) -> Result<(Delta<R>, DeltaWork), DeltaError> {
    prepare_delta_with_state(base, changes).map(|(prepared, work)| (prepared.into_delta(), work))
}

/// Prepares a checked delta while retaining its already materialized target
/// tree for a later [`PreparedDelta::commit`] call.
///
/// The returned [`PreparedDelta`] is a local capability: consuming it against
/// the same exact base publishes the retained tree without a second canonical
/// traversal.  Use [`prepare_delta`] when only the semantic wire delta is
/// needed.
///
/// # Errors
///
/// Returns the same validation errors as [`prepare_delta`].
pub fn prepare_delta_with_state<R: Relation>(
    base: &RelationState<R>,
    changes: Vec<MapChange<R>>,
) -> Result<(PreparedDelta<R>, DeltaWork), DeltaError> {
    if !base.coverage.is_delta_eligible() {
        return Err(DeltaError::IncompleteBase);
    }
    let effective = effective_changes(base, changes)?;
    let (tree, work) = materialize_tree_update(base, &effective)?;
    let target = tree.root().commitment();
    let canonical_changes = Arc::from(canonical_changes(&effective));
    let id = canonical_version_delta_id::<R>(base.root(), target, &canonical_changes);
    Ok((
        PreparedDelta {
            delta: Delta {
                base: base.root(),
                target,
                changes: effective,
                canonical_changes,
                id,
            },
            tree,
        },
        work.into(),
    ))
}

/// Applies an already checked path-copy update to a materialized relation
/// state while retaining its coverage witness.
///
/// This is the storage path for indexes whose bytes are being maintained as
/// an implementation detail. It deliberately does not produce a [`Delta`]
/// or any transition capability. A partial witness therefore remains partial
/// and cannot be promoted into authority coverage by updating the tree.
///
/// # Errors
///
/// Returns [`DeltaError`] when the changes are malformed or the canonical
/// path-copy update cannot be constructed.
pub fn prepare_internal_update_with_work<R: Relation>(
    base: &RelationState<R>,
    changes: Vec<MapChange<R>>,
) -> Result<(RelationState<R>, DeltaWork), DeltaError> {
    let effective = effective_changes(base, changes)?;
    let (tree, work) = materialize_tree_update(base, &effective)?;
    let root = tree.root().commitment();
    Ok((
        RelationState {
            tree,
            root,
            coverage: base.coverage,
        },
        work.into(),
    ))
}

fn effective_changes<R: Relation>(
    base: &RelationState<R>,
    changes: Vec<MapChange<R>>,
) -> Result<Vec<MapChange<R>>, DeltaError> {
    let mut last: Option<&R::Key> = None;
    for change in &changes {
        if let Some(previous) = last
            && previous >= &change.key
        {
            return Err(if previous == &change.key {
                DeltaError::DuplicateKey
            } else {
                DeltaError::Unsorted
            });
        }
        last = Some(&change.key);
    }
    let mut effective = Vec::with_capacity(changes.len());
    for change in changes {
        if base.get(&change.key) != change.before.as_ref() {
            return Err(DeltaError::BeforeMismatch);
        }
        if change.before != change.after {
            effective.push(change);
        }
    }
    Ok(effective)
}

fn materialize_tree_update<R: Relation>(
    base: &RelationState<R>,
    changes: &[MapChange<R>],
) -> Result<
    (
        crate::persistent::PersistentTree<R>,
        crate::persistent::TreeWork,
    ),
    DeltaError,
> {
    let tree_changes = changes
        .iter()
        .map(|change| TreeChange {
            key: change.key.clone(),
            after: change.after.clone(),
        })
        .collect::<Vec<_>>();
    base.tree
        .prepare_update(&tree_changes)
        .map(|prepared| {
            let work = prepared.work();
            (prepared.commit(), work)
        })
        .map_err(|error| match error {
            TreeError::Canonical(error) => DeltaError::Canonical(error),
            TreeError::UnsortedOrDuplicate | TreeError::InvalidRoot | TreeError::Overflow => {
                DeltaError::Canonical(NodeError::InvalidBranch)
            }
        })
}

/// Applies a checked delta and verifies the target root.
///
/// # Errors
///
/// Returns [`DeltaError::IncompleteBase`] for a partial state,
/// [`DeltaError::BaseMismatch`] for a stale root,
/// [`DeltaError::BeforeMismatch`] for a stale value precondition, or
/// [`DeltaError::TargetMismatch`] when reconstruction does not produce the
/// exact target root.
pub fn apply_delta<R: Relation>(
    base: &RelationState<R>,
    delta: &Delta<R>,
) -> Result<RelationState<R>, DeltaError> {
    apply_delta_with_work(base, delta).map(|(state, _)| state)
}

/// Applies a checked delta and reports immutable tree work counters.
///
/// # Errors
///
/// Returns the same validation errors as [`apply_delta`].
pub fn apply_delta_with_work<R: Relation>(
    base: &RelationState<R>,
    delta: &Delta<R>,
) -> Result<(RelationState<R>, DeltaWork), DeltaError> {
    validate_apply_base(base, delta)?;
    let tree_changes = delta
        .changes
        .iter()
        .map(|change| TreeChange {
            key: change.key.clone(),
            after: change.after.clone(),
        })
        .collect::<Vec<_>>();
    let (tree, work) = base
        .tree
        .prepare_update(&tree_changes)
        .map(|prepared| {
            let work = prepared.work();
            (prepared.commit(), work)
        })
        .map_err(|error| match error {
            TreeError::Canonical(error) => DeltaError::Canonical(error),
            TreeError::UnsortedOrDuplicate | TreeError::InvalidRoot | TreeError::Overflow => {
                DeltaError::Canonical(NodeError::InvalidBranch)
            }
        })?;
    let root = tree.root().commitment();
    if root != delta.target {
        return Err(DeltaError::TargetMismatch);
    }
    Ok((
        RelationState {
            tree,
            root,
            coverage: base.coverage,
        },
        work.into(),
    ))
}

pub(crate) fn validate_apply_base<R: Relation>(
    base: &RelationState<R>,
    delta: &Delta<R>,
) -> Result<(), DeltaError> {
    if !base.coverage.is_delta_eligible() {
        return Err(DeltaError::IncompleteBase);
    }
    if base.root() != delta.base {
        return Err(DeltaError::BaseMismatch);
    }
    for change in &delta.changes {
        if base.get(&change.key) != change.before.as_ref() {
            return Err(DeltaError::BeforeMismatch);
        }
    }
    Ok(())
}
