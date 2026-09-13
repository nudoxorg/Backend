//! Coverage witnesses for the flow-owned arrangement relation.
//!
//! Arrangement storage is a closed local relation. It can participate in the
//! version delta algebra without claiming authority provenance. An upstream
//! authority must provide `CoverageWitness::Complete` before publication.

use backend_version::{ClosedRelationScope, CoverageWitness, ScopeRoot};

const FLOW_SCOPE: ScopeRoot = ScopeRoot::from_u64(0x666c_6f77_2e76_3221);

pub(crate) fn arrangement_coverage() -> CoverageWitness {
    CoverageWitness::closed_relation(ClosedRelationScope::from_scope_root(FLOW_SCOPE))
}

use super::row::{ArrangementRoot, CanonicalValue, Delta};
use super::time::Frontier;
use backend_version::Relation;
use std::fmt;

/// A progress frontier bound to the exact relation state it describes.
#[derive(Debug, Eq, PartialEq)]
pub struct BoundFrontier<R: Relation> {
    root: backend_version::StateRoot<R>,
    frontier: Frontier,
}

impl<R: Relation> Clone for BoundFrontier<R> {
    fn clone(&self) -> Self {
        Self {
            root: self.root,
            frontier: self.frontier.clone(),
        }
    }
}

impl<R: Relation> BoundFrontier<R> {
    /// Binds a relation root to an execution frontier.
    #[must_use]
    pub fn new(root: backend_version::StateRoot<R>, frontier: Frontier) -> Self {
        Self { root, frontier }
    }

    /// Returns the exact relation root.
    #[must_use]
    pub const fn root(&self) -> backend_version::StateRoot<R> {
        self.root
    }

    /// Returns the bound execution frontier.
    #[must_use]
    pub fn frontier(&self) -> Frontier {
        self.frontier.clone()
    }
}

/// A prepared output retaining its exact source and target roots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedOutput<V: Clone + Ord + fmt::Debug + Eq + CanonicalValue + 'static> {
    /// Expected source root.
    pub base: ArrangementRoot<V>,
    /// Result source root.
    pub target: ArrangementRoot<V>,
    pub(crate) frontier: Frontier,
    pub(crate) coverage: CoverageWitness,
    pub(crate) deltas: Vec<Delta<V>>,
}
