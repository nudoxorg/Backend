//! Typed immutable materialized indexes.
//!
//! Product projections often need a relation whose values are already
//! canonical domain values rather than weighted flow rows.  This module
//! provides the common lifecycle for those indexes: a checked relation state,
//! a root-bound execution frontier, prepared path-copy updates, and cheap
//! immutable leases.  Product crates supply only their relation schema and
//! typed seek adapters.

use crate::{BoundFrontier, FlowError, Frontier};
use backend_version::{
    CoverageWitness, Delta, DeltaError, MapChange, PreparedDelta, Relation, RelationState,
    StateError, StateRoot, TreeNodeHandle,
};
use std::ops::RangeBounds;

/// Structural work paid while preparing one materialized-index update.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MaterializedWork {
    /// Relation values re-encoded for changed neighborhoods.
    pub rows: usize,
    /// Canonical leaves and branches rebuilt.
    pub nodes: usize,
    /// Existing canonical nodes visited while locating changed paths.
    pub visited_nodes: usize,
    /// Existing immutable children retained by the new root.
    pub reused_nodes: usize,
    /// Alias for canonical nodes copied during the update.
    pub copied_nodes: usize,
    /// Canonical bytes emitted by copied nodes.
    pub encoded_bytes: usize,
}

impl From<backend_version::DeltaWork> for MaterializedWork {
    fn from(work: backend_version::DeltaWork) -> Self {
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

fn map_state_error(error: StateError) -> FlowError {
    match error {
        StateError::DuplicateKey | StateError::Canonical(_) => FlowError::InvalidRoot,
    }
}

fn map_delta_error(error: DeltaError) -> FlowError {
    match error {
        DeltaError::IncompleteBase => FlowError::IncompleteFrontier,
        DeltaError::BaseMismatch
        | DeltaError::BeforeMismatch
        | DeltaError::Unsorted
        | DeltaError::DuplicateKey
        | DeltaError::TargetMismatch
        | DeltaError::NonAdjacent
        | DeltaError::CompositionMismatch
        | DeltaError::Canonical(_) => FlowError::InvalidRoot,
    }
}

/// One immutable typed materialized relation.
#[derive(Debug)]
pub struct MaterializedIndex<R: Relation> {
    state: RelationState<R>,
    fence: BoundFrontier<R>,
    len: usize,
}

impl<R: Relation> Clone for MaterializedIndex<R> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            fence: self.fence.clone(),
            len: self.len,
        }
    }
}

impl<R: Relation> MaterializedIndex<R> {
    /// Builds an index from logical entries and binds it to an execution
    /// frontier. Duplicate keys are rejected by the version kernel.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidRoot`] when the canonical relation cannot
    /// be admitted.
    pub fn from_entries(
        entries: impl IntoIterator<Item = (R::Key, R::Value)>,
        coverage: CoverageWitness,
        frontier: Frontier,
    ) -> Result<Self, FlowError> {
        let state = RelationState::from_entries(entries, coverage).map_err(map_state_error)?;
        Ok(Self::from_state(state, frontier))
    }

    /// Wraps an already checked relation state with its execution frontier.
    #[must_use]
    pub fn from_state(state: RelationState<R>, frontier: Frontier) -> Self {
        // The persistent root carries subtree cardinality metadata. Reading
        // the root view is O(1); rebuilding the count by scanning every
        // logical entry defeats structural sharing at this boundary.
        let len = state
            .node_closure()
            .next()
            .map_or(0, backend_version::TreeNodeView::len);
        let fence = BoundFrontier::new(state.root(), frontier);
        Self { state, fence, len }
    }

    /// Returns the exact canonical relation root.
    #[must_use]
    pub const fn root(&self) -> StateRoot<R> {
        self.state.root()
    }

    /// Returns an owning handle to the authenticated logical root node.
    ///
    /// Creating the handle is O(1); unchanged descendants remain shared with
    /// the source index and are not materialized into a row map.
    #[must_use]
    pub fn root_handle(&self) -> TreeNodeHandle<R> {
        self.state.root_handle()
    }

    /// Returns the coverage witness carried by this index.
    #[must_use]
    pub const fn coverage(&self) -> CoverageWitness {
        self.state.coverage()
    }

    /// Returns the root-bound execution frontier.
    #[must_use]
    pub fn fence(&self) -> BoundFrontier<R> {
        self.fence.clone()
    }

    /// Returns the execution frontier bound to this root.
    #[must_use]
    pub fn frontier(&self) -> Frontier {
        self.fence.frontier()
    }

    /// Returns the number of retained logical entries.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns whether this index has no retained logical entries.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Looks up one logical entry by exact key.
    #[must_use]
    pub fn get(&self, key: &R::Key) -> Option<&R::Value> {
        self.state.get(key)
    }

    /// Iterates retained entries in canonical key order.
    pub fn iter(&self) -> impl Iterator<Item = (&R::Key, &R::Value)> {
        self.state.iter()
    }

    /// Seeks one inclusive/exclusive key range without visiting keys before
    /// its lower bound.
    pub fn range<B: RangeBounds<R::Key>>(
        &self,
        bounds: B,
    ) -> impl Iterator<Item = (&R::Key, &R::Value)> {
        self.state.range(bounds)
    }

    /// Advances the execution fence while retaining the exact relation root.
    /// This is the zero-change path for a view update that changes only
    /// source progress or metadata; it never rebuilds or re-encodes rows.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::FrontierRegressed`] when the supplied frontier is
    /// behind the index's current execution frontier.
    pub fn advance(&self, frontier: Frontier) -> Result<Self, FlowError> {
        if !frontier.advances_from(&self.frontier()) {
            return Err(FlowError::FrontierRegressed);
        }
        Ok(Self {
            state: self.state.clone(),
            fence: BoundFrontier::new(self.root(), frontier),
            len: self.len,
        })
    }

    /// Prepares a checked path-copy update and binds its target to a new
    /// execution frontier. The base index remains unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::FrontierRegressed`] for a target frontier that
    /// does not advance this index, [`FlowError::IncompleteFrontier`] for an
    /// index without complete backend coverage, or [`FlowError::InvalidRoot`]
    /// for a stale/malformed change set.
    pub fn prepare(
        &self,
        changes: Vec<MapChange<R>>,
        frontier: Frontier,
    ) -> Result<(PreparedMaterializedIndex<R>, MaterializedWork), FlowError> {
        if !frontier.advances_from(&self.frontier()) {
            return Err(FlowError::FrontierRegressed);
        }
        let (prepared, work) = backend_version::prepare_delta_with_state(&self.state, changes)
            .map_err(map_delta_error)?;
        let target_len = prepared.delta().changes().try_fold(
            self.len,
            |len, change| -> Result<usize, FlowError> {
                match (&change.before, &change.after) {
                    (None, Some(_)) => len.checked_add(1).ok_or(FlowError::Overflow),
                    (Some(_), None) => len.checked_sub(1).ok_or(FlowError::InvalidRoot),
                    _ => Ok(len),
                }
            },
        )?;
        Ok((
            PreparedMaterializedIndex {
                base_root: self.root(),
                base_frontier: self.frontier(),
                target_frontier: frontier,
                target_len,
                prepared,
            },
            work.into(),
        ))
    }

    /// Retains an immutable lease over this exact index root.
    #[must_use]
    pub fn lease(&self) -> MaterializedIndexLease<R> {
        MaterializedIndexLease {
            index: self.clone(),
        }
    }
}

/// A prepared typed index update retaining its path-copied target tree.
#[derive(Debug)]
pub struct PreparedMaterializedIndex<R: Relation> {
    base_root: StateRoot<R>,
    base_frontier: Frontier,
    target_frontier: Frontier,
    target_len: usize,
    prepared: PreparedDelta<R>,
}

impl<R: Relation> PreparedMaterializedIndex<R> {
    /// Returns the checked relation delta retained by this preparation.
    #[must_use]
    pub const fn delta(&self) -> &Delta<R> {
        self.prepared.delta()
    }

    /// Returns the root-bound target execution frontier.
    #[must_use]
    pub fn target_fence(&self) -> BoundFrontier<R> {
        BoundFrontier::new(self.delta().target(), self.target_frontier.clone())
    }

    /// Consumes this update against its exact base index.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidRoot`] if the base root or frontier does
    /// not match, or if a retained before-value fence fails.
    pub fn commit(
        self,
        base: &MaterializedIndex<R>,
    ) -> Result<(MaterializedIndex<R>, Delta<R>), FlowError> {
        if base.root() != self.base_root || base.frontier() != self.base_frontier {
            return Err(FlowError::InvalidRoot);
        }
        let delta = self.prepared.delta().clone();
        let state = self.prepared.commit(&base.state).map_err(map_delta_error)?;
        let index = MaterializedIndex {
            fence: BoundFrontier::new(state.root(), self.target_frontier),
            state,
            len: self.target_len,
        };
        Ok((index, delta))
    }
}

/// A cheap immutable retention lease for one materialized index root.
#[derive(Debug)]
pub struct MaterializedIndexLease<R: Relation> {
    index: MaterializedIndex<R>,
}

impl<R: Relation> Clone for MaterializedIndexLease<R> {
    fn clone(&self) -> Self {
        Self {
            index: self.index.clone(),
        }
    }
}

impl<R: Relation> MaterializedIndexLease<R> {
    /// Returns the exact root retained by this lease.
    #[must_use]
    pub const fn root(&self) -> StateRoot<R> {
        self.index.root()
    }

    /// Borrows the retained index for bounded reads.
    #[must_use]
    pub const fn index(&self) -> &MaterializedIndex<R> {
        &self.index
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{Epoch, Time};
    use backend_version::{
        AuthorityScopeClaim, MapChange, ObjectVersion, ProducerObservationClaims,
        ProducerObservationVerifier, Schema, ScopeRoot, UntrustedProducerObservation,
        admit_complete_scope, admit_producer_observation,
    };

    struct TestScope;

    struct TestScopeVerifier(ScopeRoot);

    impl ProducerObservationVerifier for TestScopeVerifier {
        type Error = &'static str;

        fn verify(
            &self,
            observation: &UntrustedProducerObservation,
        ) -> Result<ProducerObservationClaims, Self::Error> {
            if observation.producer_identity() == [0x71; 32]
                && observation.scope_root() == self.0
                && observation.context() == *self.0.as_bytes()
                && observation.evidence() == self.0.as_bytes()
            {
                Ok(ProducerObservationClaims::new(
                    [0x71; 32],
                    self.0,
                    *self.0.as_bytes(),
                    *blake3::hash(self.0.as_bytes()).as_bytes(),
                ))
            } else {
                Err("materialized producer observation mismatch")
            }
        }
    }

    impl Schema for TestScope {
        const DOMAIN: u8 = 0x71;
        const TYPE: u16 = 1;
        type Value = [u8];

        fn encode(value: &Self::Value, out: &mut Vec<u8>) {
            out.extend_from_slice(value);
        }
    }

    #[derive(Debug)]
    struct TestRelation;

    impl Relation for TestRelation {
        const DOMAIN: u8 = 0x72;
        const TYPE: u16 = 1;
        type Key = u64;
        type Value = u64;

        fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
            out.extend_from_slice(&value.to_be_bytes());
        }

        fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
            out.extend_from_slice(&value.to_be_bytes());
        }
    }

    fn complete_coverage() -> CoverageWitness {
        let object = ObjectVersion::<TestScope>::from_value(b"materialized-scope");
        let declared = AuthorityScopeClaim::from_object_version(object);
        let observed = ScopeRoot::from_bytes(object.to_bytes());
        let verifier = TestScopeVerifier(observed);
        let admitted = admit_producer_observation(
            UntrustedProducerObservation::new(
                [0x71; 32],
                observed,
                *observed.as_bytes(),
                observed.as_bytes().to_vec(),
            ),
            &verifier,
        )
        .expect("producer observation");
        admit_complete_scope(declared, admitted)
            .map(CoverageWitness::Complete)
            .expect("complete producer witness")
    }

    fn frontier(epoch: u64) -> Frontier {
        Frontier::new(Time::new(Epoch(epoch), 0))
    }

    #[test]
    fn one_row_update_uses_shared_path_copy_and_lease() {
        let index = MaterializedIndex::<TestRelation>::from_entries(
            (0..10_000).map(|key| (key, key)),
            complete_coverage(),
            frontier(0),
        )
        .expect("materialized index");
        let before_root = index.root();
        let lease = index.lease();
        let (prepared, work) = index
            .prepare(
                vec![MapChange {
                    key: 5_000,
                    before: Some(5_000),
                    after: Some(5_001),
                }],
                frontier(1),
            )
            .expect("one row preparation");
        assert!(work.visited_nodes < 64, "unexpected broad seek: {work:?}");
        assert!(work.copied_nodes < 64, "unexpected broad copy: {work:?}");
        let (next, delta) = prepared.commit(&index).expect("one row commit");
        assert_eq!(delta.base(), before_root);
        assert_eq!(index.root(), lease.root());
        assert_eq!(next.get(&5_000), Some(&5_001));
        assert_eq!(next.len(), index.len());
        assert_eq!(next.frontier(), frontier(1));
        assert_eq!(next.lease().root(), next.root());
    }

    #[test]
    fn metadata_progress_advances_the_fence_without_rebuilding_relation_nodes() {
        let index = MaterializedIndex::<TestRelation>::from_entries(
            [(1, 10), (2, 20)],
            complete_coverage(),
            frontier(3),
        )
        .expect("materialized index");
        let root = index.root();
        let advanced = index.advance(frontier(4)).expect("advance fence");
        assert_eq!(advanced.root(), root);
        assert_eq!(advanced.frontier(), frontier(4));
        assert_eq!(advanced.get(&1), Some(&10));
    }
}
