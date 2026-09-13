use core::fmt;
use std::sync::Arc;

use crate::{
    NodeError, Relation, StateRoot,
    ids::{append_field, canonical_version_delta_id},
    persistent::PersistentTree,
};

use super::{operations::validate_apply_base, state::RelationState};

/// A single checked before/after map change.
#[derive(Debug, Eq, PartialEq)]
pub struct MapChange<R: Relation> {
    /// Logical key being changed.
    pub key: R::Key,
    /// Value expected at the key before the change.
    pub before: Option<R::Value>,
    /// Value to publish at the key after the change.
    pub after: Option<R::Value>,
}

impl<R: Relation> Clone for MapChange<R> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            before: self.before.clone(),
            after: self.after.clone(),
        }
    }
}

/// Exact base and target roots bound to an ordered delta.
#[derive(Debug, Eq, PartialEq)]
pub struct Delta<R: Relation> {
    pub(crate) base: StateRoot<R>,
    pub(crate) target: StateRoot<R>,
    pub(crate) changes: Vec<MapChange<R>>,
    pub(crate) canonical_changes: Arc<[u8]>,
    pub(crate) id: crate::DeltaId<R>,
}

impl<R: Relation> Clone for Delta<R> {
    fn clone(&self) -> Self {
        Self {
            base: self.base,
            target: self.target,
            changes: self.changes.clone(),
            canonical_changes: self.canonical_changes.clone(),
            id: self.id,
        }
    }
}

impl<R: Relation> Delta<R> {
    pub(crate) fn from_parts(
        base: StateRoot<R>,
        target: StateRoot<R>,
        changes: Vec<MapChange<R>>,
    ) -> Self {
        let canonical_changes = Arc::from(canonical_changes(&changes));
        let id = canonical_version_delta_id::<R>(base, target, &canonical_changes);
        Self {
            base,
            target,
            changes,
            canonical_changes,
            id,
        }
    }

    /// Borrows this checked transition without copying its change columns or
    /// canonical encoding.
    #[must_use]
    pub const fn view(&self) -> DeltaView<'_, R> {
        DeltaView { delta: self }
    }

    /// Binds this transition to the exact state generation it can advance.
    ///
    /// The returned capability carries the state borrow, so an application
    /// path cannot accidentally pair the transition with a different root.
    /// It also checks completeness and every before value before returning.
    ///
    /// # Errors
    ///
    /// Returns [`DeltaError::IncompleteBase`], [`DeltaError::BaseMismatch`],
    /// or [`DeltaError::BeforeMismatch`] when the supplied state cannot be
    /// the exact base of this transition.
    pub fn bind<'a>(&'a self, base: &'a RelationState<R>) -> Result<BoundDelta<'a, R>, DeltaError> {
        validate_apply_base(base, self)?;
        Ok(BoundDelta { base, delta: self })
    }

    /// Returns the exact base relation root.
    #[must_use]
    pub const fn base(&self) -> StateRoot<R> {
        self.base
    }

    /// Returns the exact target relation root.
    #[must_use]
    pub const fn target(&self) -> StateRoot<R> {
        self.target
    }

    /// Returns the canonical transition identity.
    #[must_use]
    pub const fn id(&self) -> crate::DeltaId<R> {
        self.id
    }

    /// Iterates the strictly ordered effective changes.
    pub fn changes(&self) -> impl Iterator<Item = &MapChange<R>> {
        self.changes.iter()
    }

    /// Copies the canonical change encoding used by this transition ID.
    ///
    /// This compatibility accessor allocates a `Vec`; use
    /// [`Self::canonical_changes_bytes`] for validation and borrowing.
    #[must_use]
    pub fn canonical_changes(&self) -> Vec<u8> {
        self.canonical_changes.to_vec()
    }

    /// Borrows the cached canonical change encoding without allocating.
    #[must_use]
    pub fn canonical_changes_bytes(&self) -> &[u8] {
        &self.canonical_changes
    }

    /// Clones the shared canonical change-byte handle without copying bytes.
    #[must_use]
    pub(crate) fn canonical_changes_arc(&self) -> Arc<[u8]> {
        self.canonical_changes.clone()
    }

    /// Revalidates the transition's local canonical invariants.
    ///
    /// This check is used at type-erasure boundaries.  Normal construction
    /// already proves it, but revalidation keeps a workspace descriptor from
    /// relying solely on copied root labels.
    #[must_use]
    pub fn is_canonical(&self) -> bool {
        self.changes
            .windows(2)
            .all(|window| window[0].key < window[1].key)
            && self
                .changes
                .iter()
                .all(|change| change.before != change.after)
            && canonical_version_delta_id::<R>(self.base, self.target, &self.canonical_changes)
                == self.id
    }

    /// Reverses this transition while preserving canonical key order.
    #[must_use]
    pub fn inverse(&self) -> Delta<R> {
        let changes: Vec<_> = self
            .changes
            .iter()
            .map(|change| MapChange {
                key: change.key.clone(),
                before: change.after.clone(),
                after: change.before.clone(),
            })
            .collect();
        let canonical_changes = Arc::from(canonical_changes(&changes));
        Delta {
            base: self.target,
            target: self.base,
            id: canonical_version_delta_id::<R>(self.target, self.base, &canonical_changes),
            changes,
            canonical_changes,
        }
    }

    /// Composes adjacent exact transitions and coalesces overlapping keys.
    ///
    /// # Errors
    ///
    /// Returns [`DeltaError::NonAdjacent`] when the roots do not line up, or
    /// [`DeltaError::CompositionMismatch`] when an overlapping key's first
    /// transition does not produce the value expected by the second.
    pub fn compose(first: &Self, second: &Self) -> Result<Delta<R>, DeltaError> {
        if first.target != second.base {
            return Err(DeltaError::NonAdjacent);
        }
        let mut changes = first.changes.clone();
        for second_change in &second.changes {
            if let Some(first_change) = changes
                .iter_mut()
                .find(|change| change.key == second_change.key)
            {
                if first_change.after != second_change.before {
                    return Err(DeltaError::CompositionMismatch);
                }
                first_change.after.clone_from(&second_change.after);
            } else {
                changes.push(second_change.clone());
            }
        }
        changes.retain(|change| change.before != change.after);
        changes.sort_by(|left, right| left.key.cmp(&right.key));
        let canonical_changes = Arc::from(canonical_changes(&changes));
        Ok(Delta {
            base: first.base,
            target: second.target,
            id: canonical_version_delta_id::<R>(first.base, second.target, &canonical_changes),
            changes,
            canonical_changes,
        })
    }
}

/// Borrowed view of a checked, root bound transition.
#[derive(Clone, Copy, Debug)]
pub struct DeltaView<'a, R: Relation> {
    delta: &'a Delta<R>,
}

impl<'a, R: Relation> DeltaView<'a, R> {
    /// Returns the exact base root covered by the view.
    #[must_use]
    pub const fn base(&self) -> StateRoot<R> {
        self.delta.base
    }

    /// Returns the exact target root covered by the view.
    #[must_use]
    pub const fn target(&self) -> StateRoot<R> {
        self.delta.target
    }

    /// Returns the transition identity.
    #[must_use]
    pub const fn id(&self) -> crate::DeltaId<R> {
        self.delta.id
    }

    /// Iterates changes by reference and preserves canonical key order.
    pub fn changes(&self) -> impl Iterator<Item = &'a MapChange<R>> {
        self.delta.changes.iter()
    }

    /// Borrows the canonical transition body used by the identity.
    #[must_use]
    pub fn canonical_changes(&self) -> &'a [u8] {
        &self.delta.canonical_changes
    }
}

/// A transition whose expected base is carried by its lifetime brand.
#[derive(Clone, Copy, Debug)]
pub struct BoundDelta<'a, R: Relation> {
    base: &'a RelationState<R>,
    delta: &'a Delta<R>,
}

impl<'a, R: Relation> BoundDelta<'a, R> {
    /// Returns the checked transition view.
    #[must_use]
    pub const fn view(self) -> DeltaView<'a, R> {
        DeltaView { delta: self.delta }
    }

    /// Applies the transition to its branded base state.
    ///
    /// # Errors
    ///
    /// Returns a checked transition error if the branded state has become
    /// stale or the path copy cannot reproduce the target root.
    pub fn apply(self) -> Result<RelationState<R>, DeltaError> {
        super::operations::apply_delta(self.base, self.delta)
    }

    /// Applies the transition and reports path-copy work.
    ///
    /// # Errors
    ///
    /// Returns a checked transition error if the branded state has become
    /// stale or the path copy cannot reproduce the target root.
    pub fn apply_with_work(self) -> Result<(RelationState<R>, DeltaWork), DeltaError> {
        super::operations::apply_delta_with_work(self.base, self.delta)
    }
}

/// A prepared delta retaining its already materialized target tree.
///
/// This capability is intended for local prepare-then-commit paths.  Its
/// semantic [`Delta`] remains available for journaling or wire encoding, but
/// consuming the capability publishes the retained persistent tree instead
/// of rebuilding it.  A decoded or otherwise independently admitted
/// [`Delta`] must continue through [`crate::apply_delta`].
#[derive(Debug)]
pub struct PreparedDelta<R: Relation> {
    pub(crate) delta: Delta<R>,
    pub(crate) tree: PersistentTree<R>,
}

impl<R: Relation> PartialEq for PreparedDelta<R> {
    fn eq(&self, other: &Self) -> bool {
        self.delta.base == other.delta.base
            && self.delta.target == other.delta.target
            && self.delta.id == other.delta.id
            && self.delta.changes.len() == other.delta.changes.len()
            && self
                .delta
                .changes
                .iter()
                .zip(&other.delta.changes)
                .all(|(left, right)| {
                    left.key == right.key
                        && left.before == right.before
                        && left.after == right.after
                })
    }
}

impl<R: Relation> Eq for PreparedDelta<R> {}

impl<R: Relation> PreparedDelta<R> {
    /// Returns the semantic delta retained by this prepared capability.
    #[must_use]
    pub const fn delta(&self) -> &Delta<R> {
        &self.delta
    }

    /// Consumes the capability and returns its semantic delta.
    #[must_use]
    pub fn into_delta(self) -> Delta<R> {
        self.delta
    }

    /// Publishes the retained target tree against the exact base state.
    ///
    /// The returned state reuses the tree computed during preparation.  This
    /// operation is O(1) apart from checking the exact base and before-value
    /// fences; it performs no canonical rebuild.
    ///
    /// # Errors
    ///
    /// Returns the same base, coverage, and before-value errors as
    /// [`crate::apply_delta`].
    pub fn commit(self, base: &RelationState<R>) -> Result<RelationState<R>, DeltaError> {
        self.commit_with_work(base).map(|(state, _)| state)
    }

    /// Publishes the retained target tree and reports work performed at
    /// commit time.  A successful commit reports zero tree work because all
    /// canonical work was completed by preparation.
    ///
    /// # Errors
    ///
    /// Returns the same base, coverage, and before-value errors as
    /// [`crate::apply_delta`].
    pub fn commit_with_work(
        self,
        base: &RelationState<R>,
    ) -> Result<(RelationState<R>, DeltaWork), DeltaError> {
        validate_apply_base(base, &self.delta)?;
        let target = self.delta.target;
        debug_assert_eq!(self.tree.root().commitment(), target);
        Ok((
            RelationState {
                tree: self.tree,
                root: target,
                coverage: base.coverage,
            },
            DeltaWork::default(),
        ))
    }
}

/// Computes a relation delta identity directly from exact roots and ordered
/// before/after changes.
///
/// This helper hashes only the supplied change sequence and does not inspect
/// or rebuild either relation state.  Callers crossing a wire boundary must
/// validate strict key ordering and before/after semantics before relying on
/// the returned identity.
#[must_use]
pub fn canonical_delta_id<R: Relation>(
    base: StateRoot<R>,
    target: StateRoot<R>,
    changes: &[MapChange<R>],
) -> crate::DeltaId<R> {
    let bytes = canonical_changes(changes);
    canonical_version_delta_id::<R>(base, target, &bytes)
}

/// Computes a transition identity after checking its canonical change shape.
///
/// New wire adapters should use this entry point instead of hashing an
/// arbitrary change slice. The compatibility [`canonical_delta_id`] helper
/// remains available for adapters that validate their envelope separately.
///
/// # Errors
///
/// Returns [`DeltaError::Unsorted`] or [`DeltaError::DuplicateKey`] for an
/// unordered sequence, and [`DeltaError::CompositionMismatch`] for an
/// effective no-op change that should have been omitted.
pub fn checked_canonical_delta_id<R: Relation>(
    base: StateRoot<R>,
    target: StateRoot<R>,
    changes: &[MapChange<R>],
) -> Result<crate::DeltaId<R>, DeltaError> {
    let mut previous = None;
    for change in changes {
        if let Some(previous_key) = previous
            && previous_key >= &change.key
        {
            return Err(if previous_key == &change.key {
                DeltaError::DuplicateKey
            } else {
                DeltaError::Unsorted
            });
        }
        if change.before == change.after {
            return Err(DeltaError::CompositionMismatch);
        }
        previous = Some(&change.key);
    }
    Ok(canonical_delta_id(base, target, changes))
}

pub(crate) fn canonical_changes<R: Relation>(changes: &[MapChange<R>]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for change in changes {
        bytes.push(0x03);
        append_field(&mut bytes, |out| R::encode_key(&change.key, out));
        match (&change.before, &change.after) {
            (None, None) => bytes.push(0),
            (Some(before), None) => {
                bytes.push(1);
                append_field(&mut bytes, |out| R::encode_value(before, out));
            }
            (None, Some(after)) => {
                bytes.push(2);
                append_field(&mut bytes, |out| R::encode_value(after, out));
            }
            (Some(before), Some(after)) => {
                bytes.push(3);
                append_field(&mut bytes, |out| R::encode_value(before, out));
                append_field(&mut bytes, |out| R::encode_value(after, out));
            }
        }
    }
    bytes
}

/// Failure while preparing, applying, or composing a delta.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeltaError {
    /// The base state root did not match the delta's exact base.
    BaseMismatch,
    /// A change's before-value did not match the visible state.
    BeforeMismatch,
    /// Changes were not strictly ordered.
    Unsorted,
    /// Two changes targeted the same key.
    DuplicateKey,
    /// The reconstructed state did not match the exact target root.
    TargetMismatch,
    /// The source state was not complete authority coverage.
    IncompleteBase,
    /// The two transitions did not share an exact intermediate root.
    NonAdjacent,
    /// Overlapping changes disagreed about the intermediate value.
    CompositionMismatch,
    /// Canonical state construction rejected a resulting node.
    Canonical(NodeError),
}

impl fmt::Display for DeltaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid relation delta: {self:?}")
    }
}

impl std::error::Error for DeltaError {}

/// Counts the immutable canonical tree work performed by one update.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeltaWork {
    /// Leaf rows re-encoded for changed neighborhoods.
    pub rows: usize,
    /// Canonical leaves and branches rebuilt.
    pub nodes: usize,
    /// Existing canonical nodes visited while locating the update.
    pub visited_nodes: usize,
    /// Existing immutable child nodes retained by the new root.
    pub reused_nodes: usize,
    /// Alias for canonical nodes copied during the update.
    pub copied_nodes: usize,
    /// Canonical bytes emitted by copied nodes.
    pub encoded_bytes: usize,
}

impl From<crate::persistent::TreeWork> for DeltaWork {
    fn from(work: crate::persistent::TreeWork) -> Self {
        Self {
            rows: work.rows,
            nodes: work.nodes,
            visited_nodes: work.visited_nodes,
            reused_nodes: work.reused_nodes,
            copied_nodes: work.copied_nodes,
            encoded_bytes: work.encoded_bytes,
        }
    }
}
