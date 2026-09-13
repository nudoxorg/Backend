//! Batch contracts and admission facades.

use super::{BatchRoot, BatchSchema, batch_root, canonical_bytes, consolidate_rows};
use crate::{
    ArrangementRelation, ArrangementRoot, BoundFrontier, CanonicalValue, CoverageWitness, Delta,
    FlowError, Frontier, Microbatch, PreparedOutput, RowRef,
};
use std::fmt::Debug;

/// Contract for operators that prepare owned output and lend it to readers.
pub trait IncrementalOperator {
    /// Input payload type.
    type Input;
    /// Output payload type.
    type Output: Clone + Ord + Debug + Eq + CanonicalValue + 'static;
    /// Lending output batch type.
    type Batch<'a>: Iterator<Item = RowRef<'a, Self::Output>>
    where
        Self: 'a;

    /// Prepares a scoped output transition.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError`] when the input transition or output arithmetic
    /// is invalid.
    fn prepare<'a>(
        &'a mut self,
        input: &'a Microbatch<Self::Input>,
        base: ArrangementRoot<Self::Output>,
        target: ArrangementRoot<Self::Output>,
        frontier: Frontier,
        coverage: CoverageWitness,
    ) -> Result<PreparedOutput<Self::Output>, FlowError>;

    /// Borrows prepared rows for the duration of the output borrow.
    fn borrow<'a>(&'a self, output: &'a PreparedOutput<Self::Output>) -> Self::Batch<'a>;
}

/// A GAT lending cursor. The item borrow cannot outlive the cursor borrow.
pub trait LendingCursor {
    /// Payload yielded by this cursor.
    type Value;
    /// Borrowed row type.
    type Item<'a>: 'a
    where
        Self: 'a;

    /// Borrows the next row for exactly the current cursor borrow.
    fn next(&mut self) -> Option<Self::Item<'_>>;
    /// Returns the number of rows not yet borrowed.
    fn remaining(&self) -> usize;
}

/// A source whose batches are lent from immutable backing storage.
pub trait BatchSource {
    /// Batch type branded to the source borrow.
    type Batch<'a>: LendingCursor
    where
        Self: 'a;

    /// Returns the next available batch.
    fn next_batch(&mut self) -> Option<Self::Batch<'_>>;
}

/// A verified batch signature callback.
pub trait BatchVerifier {
    /// Verifier-specific error.
    type Error;
    /// Verifies canonical bytes and their parent binding.
    ///
    /// # Errors
    ///
    /// Returns the verifier-specific error when the signature is not valid.
    fn verify(&self, bytes: &[u8], parent: &[u8], signature: &[u8]) -> Result<(), Self::Error>;
}

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> PreparedOutput<V> {
    /// Borrows the checked, consolidated output rows retained by this
    /// preparation.
    #[must_use]
    pub fn deltas(&self) -> &[Delta<V>] {
        &self.deltas
    }

    /// Returns the base root and frontier as one bound publication fence.
    #[must_use]
    pub fn base_fence(&self) -> BoundFrontier<ArrangementRelation<V>> {
        BoundFrontier::new(self.base, self.frontier.clone())
    }

    /// Returns the target root and frontier as one bound publication fence.
    #[must_use]
    pub fn target_fence(&self) -> BoundFrontier<ArrangementRelation<V>> {
        BoundFrontier::new(self.target, self.frontier.clone())
    }

    /// Admits a complete output transition with checked roots and frontier.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::IncompleteFrontier`] when coverage is partial or
    /// a change is outside the declared frontier. Propagates checked
    /// consolidation failures from the supplied changes.
    pub fn admit(
        base: ArrangementRoot<V>,
        target: ArrangementRoot<V>,
        frontier: Frontier,
        coverage: CoverageWitness,
        deltas: Vec<Delta<V>>,
    ) -> Result<Self, FlowError> {
        if !coverage.state().is_complete()
            || deltas.iter().any(|delta| !frontier.covers(delta.time))
        {
            return Err(FlowError::IncompleteFrontier);
        }
        Ok(Self {
            base,
            target,
            frontier,
            coverage,
            deltas: consolidate_rows(deltas)?,
        })
    }
}

/// A canonical, signed weighted batch.
#[derive(Clone, Debug)]
pub struct SignedBatch<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> {
    /// Canonical content digest.
    pub digest: BatchRoot,
    /// Parent root binding.
    pub parent: ArrangementRoot<V>,
    /// Sorted and consolidated rows.
    pub deltas: Vec<Delta<V>>,
    /// Authority signature.
    pub signature: Vec<u8>,
}

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> SignedBatch<V> {
    /// Canonicalizes, checks, and hashes a weighted batch.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::ZeroDiff`] for a zero update or
    /// [`FlowError::Overflow`] when consolidation overflows.
    pub fn canonicalize(
        deltas: Vec<Delta<V>>,
        parent: ArrangementRoot<V>,
        signature: Vec<u8>,
    ) -> Result<Self, FlowError> {
        let deltas = consolidate_rows(deltas)?;
        let digest = batch_root(&deltas, parent.to_bytes());
        Ok(Self {
            digest,
            parent,
            deltas,
            signature,
        })
    }

    /// Verifies both the content digest and an external signature.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidBatch`] when the content digest or the
    /// verifier's signature does not match.
    pub fn verify<T: BatchVerifier>(&self, verifier: &T) -> Result<(), FlowError> {
        // Serialize once for both content authentication and the external
        // verifier. Rebuilding the same canonical byte stream twice made
        // verification unnecessarily linear in the retained payload.
        let bytes = canonical_bytes(&self.deltas, self.parent.to_bytes());
        if self.digest != backend_version::ObjectVersion::<BatchSchema>::from_value(&bytes) {
            return Err(FlowError::InvalidBatch);
        }
        verifier
            .verify(&bytes, self.parent.as_bytes(), &self.signature)
            .map_err(|_| FlowError::InvalidBatch)
    }
}
