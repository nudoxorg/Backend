//! Public evidence and bounded-work values for lazy persisted trees.

use crate::{
    CanonicalNode, CanonicalRelation, CanonicalRootAdmissionError, CheckedCanonicalRoot, MapChange,
    StateRoot, UntrustedId, admit_canonical_root_claim,
};

/// Explicit work performed by one lazy update.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LazyTreeWork {
    /// Number of authenticated nodes fetched from the loader.
    pub loaded_nodes: usize,
    /// Number of canonical nodes emitted into the changed frontier.
    pub rebuilt_nodes: usize,
    /// Number of extra nodes created by split propagation.
    pub split_nodes: usize,
    /// Number of logical entries removed.
    pub removed_entries: usize,
    /// Total bytes in the changed frontier.
    pub emitted_bytes: usize,
}

/// A checked target root produced by a lazy path copy.
#[derive(Debug)]
pub struct LazyPreparedUpdate<R: CanonicalRelation> {
    pub(super) base: StateRoot<R>,
    pub(super) target: CheckedCanonicalRoot<R>,
    pub(super) changed: Vec<CanonicalNode<R>>,
    pub(super) work: LazyTreeWork,
    pub(super) changes: Vec<MapChange<R>>,
}

/// A bounded page read from a lazily opened canonical relation.
#[derive(Debug)]
pub struct LazyTreePage<R: CanonicalRelation> {
    pub(super) entries: Vec<(R::Key, R::Value)>,
    pub(super) next: Option<R::Key>,
}

impl<R: CanonicalRelation> LazyTreePage<R> {
    /// Returns the owned rows in canonical key order.
    #[must_use]
    pub fn entries(&self) -> &[(R::Key, R::Value)] {
        &self.entries
    }

    /// Returns the last emitted key when another page remains.
    #[must_use]
    pub fn next(&self) -> Option<&R::Key> {
        self.next.as_ref()
    }
}

/// Owned proof that one persisted relation root has passed canonical admission.
#[derive(Debug)]
pub struct PersistedTreeRoot<R: CanonicalRelation> {
    pub(super) evidence: CheckedCanonicalRoot<R>,
}

impl<R: CanonicalRelation> Clone for PersistedTreeRoot<R> {
    fn clone(&self) -> Self {
        Self {
            evidence: self.evidence.clone(),
        }
    }
}

impl<R: CanonicalRelation> PersistedTreeRoot<R> {
    /// Reuses a canonical root already admitted by a typed node loader.
    #[must_use]
    pub const fn from_checked(evidence: CheckedCanonicalRoot<R>) -> Self {
        Self { evidence }
    }

    /// Admits canonical root bytes against an untrusted relation-root claim.
    ///
    /// # Errors
    /// Returns a canonical grammar, schema, or digest admission error.
    pub fn admit(claim: UntrustedId<R>, bytes: &[u8]) -> Result<Self, CanonicalRootAdmissionError> {
        Ok(Self {
            evidence: admit_canonical_root_claim(claim, bytes)?,
        })
    }

    /// Returns the admitted typed state root.
    #[must_use]
    pub const fn root(&self) -> StateRoot<R> {
        self.evidence.root()
    }

    /// Returns the schema identity proven by the relation marker.
    #[must_use]
    pub const fn schema(&self) -> crate::SchemaIdentity {
        crate::SchemaIdentity::new(R::DOMAIN, R::TYPE, R::VERSION)
    }

    /// Returns the admitted canonical node evidence.
    #[must_use]
    pub const fn evidence(&self) -> &CheckedCanonicalRoot<R> {
        &self.evidence
    }
}

impl<R: CanonicalRelation> LazyPreparedUpdate<R> {
    /// Returns the exact root opened before the update.
    #[must_use]
    pub const fn base(&self) -> StateRoot<R> {
        self.base
    }

    /// Returns the checked target root descriptor.
    #[must_use]
    pub const fn target(&self) -> &CheckedCanonicalRoot<R> {
        &self.target
    }

    /// Consumes the update and returns its checked target descriptor.
    #[must_use]
    pub fn into_target(self) -> CheckedCanonicalRoot<R> {
        self.target
    }

    /// Returns an owned persisted-root handoff for storage publication.
    #[must_use]
    pub fn target_root(&self) -> PersistedTreeRoot<R> {
        PersistedTreeRoot {
            evidence: self.target.clone(),
        }
    }

    /// Clones the exact single-key transition prepared by this path copy.
    #[must_use]
    pub fn delta(&self) -> crate::delta::Delta<R> {
        crate::delta::Delta::from_parts(self.base, self.target.root(), self.changes.clone())
    }

    /// Consumes this path-copy update and returns its exact typed delta.
    #[must_use]
    pub fn into_delta(self) -> crate::delta::Delta<R> {
        crate::delta::Delta::from_parts(self.base, self.target.root(), self.changes)
    }

    /// Borrows newly encoded nodes in child-to-root order for store CAS.
    #[must_use]
    pub fn changed_nodes(&self) -> &[CanonicalNode<R>] {
        &self.changed
    }

    /// Returns bounded loader, rebuild, split, and byte counters.
    #[must_use]
    pub const fn work(&self) -> LazyTreeWork {
        self.work
    }
}
