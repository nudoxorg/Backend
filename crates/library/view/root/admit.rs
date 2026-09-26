//! View-root admission and exact transition preparation.

use super::super::{CommittedViewDelta, PreparedViewDelta, ViewDelta};
use super::{Basis, Coverage, CoverageCapability, Row, ViewError, ViewRoot};
use crate::canonical::{Frontier, ViewRecipeId};
use backend_version::{ScopeRoot, prepare_delta_with_state};
use std::sync::{Arc, OnceLock};

impl ViewRoot {
    /// Builds an immutable view root from a producer-admitted coverage
    /// capability and derives its version from complete basis, frontier,
    /// coverage, and visible-row relation content.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::InvalidCoverage`] when the capability does not
    /// cover the source object or when complete coverage is otherwise
    /// unadmitted. Returns [`ViewError::WrongBasis`] for a mismatched frontier
    /// or row basis, [`ViewError::InvalidIdentity`] for a witness whose digest
    /// does not equal its typed row key, and [`ViewError::Duplicate`] for
    /// repeated row identities.
    pub fn new_checked<C: Into<CoverageCapability>>(
        recipe: ViewRecipeId,
        basis: Basis,
        frontier: Frontier,
        rows: Vec<Row>,
        coverage: Vec<Coverage>,
        capability: C,
    ) -> Result<Self, ViewError> {
        Self::build(
            recipe,
            basis,
            frontier,
            rows,
            coverage,
            Some(capability.into()),
        )
    }

    /// Builds an immutable view root whose coverage is explicitly incomplete.
    ///
    /// This constructor cannot create a `Coverage::Complete` lane.  A
    /// producer that has complete evidence must call [`Self::new_checked`].
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::InvalidCoverage`] for complete or invalid partial
    /// coverage, [`ViewError::WrongBasis`] for a mismatched frontier or row,
    /// [`ViewError::InvalidIdentity`] for a witness whose digest does not
    /// equal its typed row key, and [`ViewError::Duplicate`] for repeated row
    /// identities.
    pub fn new_incomplete(
        recipe: ViewRecipeId,
        basis: Basis,
        frontier: Frontier,
        rows: Vec<Row>,
        coverage: Vec<Coverage>,
    ) -> Result<Self, ViewError> {
        Self::build(recipe, basis, frontier, rows, coverage, None)
    }

    fn build(
        recipe: ViewRecipeId,
        basis: Basis,
        frontier: Frontier,
        mut rows: Vec<Row>,
        coverage: Vec<Coverage>,
        capability: Option<CoverageCapability>,
    ) -> Result<Self, ViewError> {
        if coverage.iter().any(|value| value.is_complete()) && capability.is_none() {
            return Err(ViewError::InvalidCoverage);
        }
        if let Some(capability) = capability.as_ref()
            && capability.scope_root() != ScopeRoot::from_bytes(basis.object.to_bytes())
        {
            return Err(ViewError::InvalidCoverage);
        }
        rows.sort_by_key(|row| row.id);
        if frontier.branch != basis.branch
            || frontier.log != basis.log
            || frontier.schema != basis.schema
            || frontier.root != basis.root
        {
            return Err(ViewError::WrongBasis);
        }
        if !coverage.iter().copied().all(Coverage::is_valid) {
            return Err(ViewError::InvalidCoverage);
        }
        if rows.iter().any(|row| !super::row_identity_matches(row)) {
            return Err(ViewError::InvalidIdentity);
        }
        if rows.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(ViewError::Duplicate);
        }
        if rows.iter().any(|row| row.basis != basis) {
            return Err(ViewError::WrongBasis);
        }
        let coverage = coverage.into_boxed_slice();
        let relation =
            super::relation_state_from_parts(basis, frontier, rows, &coverage, capability.clone())?;
        let root = relation.root();
        let version = super::version_for_view(recipe, basis, frontier, root, &coverage);
        Ok(Self {
            recipe,
            version,
            root,
            basis,
            frontier,
            rows_cache: Arc::new(OnceLock::new()),
            coverage,
            capability,
            relation,
        })
    }

    /// Builds an empty immutable view at one source frontier using producer
    /// evidence for the source scope.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::InvalidCoverage`] when the producer capability is
    /// for another source object.
    pub fn empty_checked<C: Into<CoverageCapability>>(
        recipe: ViewRecipeId,
        basis: Basis,
        frontier: Frontier,
        capability: C,
    ) -> Result<Self, ViewError> {
        Self::new_checked(
            recipe,
            basis,
            frontier,
            Vec::new(),
            vec![Coverage::Complete],
            capability,
        )
    }

    /// Returns whether every row, relation root, version, and frontier is
    /// bound to this view's recipe and source basis.
    #[must_use]
    pub fn is_coherent(&self) -> bool {
        self.frontier.branch == self.basis.branch
            && self.frontier.log == self.basis.log
            && self.frontier.schema == self.basis.schema
            && self.frontier.root == self.basis.root
            && self.coverage.iter().copied().all(Coverage::is_valid)
            && (!self.coverage.iter().copied().any(Coverage::is_complete)
                || self.capability.is_some())
            && self.capability.as_ref().is_none_or(|capability| {
                capability.scope_root() == ScopeRoot::from_bytes(self.basis.object.to_bytes())
            })
            && self.relation.root() == self.root
            && super::relation_metadata_matches(self)
            && self.version
                == super::version_for_view(
                    self.recipe,
                    self.basis,
                    self.frontier,
                    self.root,
                    &self.coverage,
                )
    }

    /// Prepares one exact transition from this root. The target recipe and
    /// snapshot version are derived from the base recipe and resulting values.
    ///
    /// # Errors
    ///
    /// Returns a [`ViewError`] when the base is incoherent, the capability is
    /// for another source, the transition exceeds the sequence bound, or the
    /// resulting relation delta cannot be admitted.
    pub fn prepare<C: Into<CoverageCapability>>(
        &self,
        delta: ViewDelta,
        capability: C,
    ) -> Result<PreparedViewDelta, ViewError> {
        if !self.is_coherent() {
            return Err(ViewError::IncoherentBase);
        }
        let capability = capability.into();
        if capability.scope_root() != ScopeRoot::from_bytes(self.basis.object.to_bytes()) {
            return Err(ViewError::InvalidCoverage);
        }
        let coverage = super::super::transition::validate_delta(self, &delta, &capability)?;
        let target_frontier = Frontier::new(
            self.frontier.branch,
            self.frontier.log,
            self.frontier.schema,
            self.frontier.root,
            self.frontier
                .sequence
                .checked_add(1)
                .ok_or(ViewError::Unbounded)?,
        );
        let changes = super::super::transition::relation_changes_for_delta(
            self,
            &delta,
            target_frontier,
            &coverage,
        );
        let (relation, _work) = prepare_delta_with_state(&self.relation, changes)
            .map_err(|_| ViewError::InvalidRelationDelta)?;
        let target_root = relation.delta().target();
        let target_version = super::version_for_view(
            self.recipe,
            self.basis,
            target_frontier,
            target_root,
            &coverage,
        );
        let relation_changes = relation
            .delta()
            .canonical_changes_bytes()
            .to_vec()
            .into_boxed_slice();
        if self.relation.root() != self.root || relation.delta().base() != self.root {
            return Err(ViewError::InvalidRelationDelta);
        }
        Ok(PreparedViewDelta {
            base_recipe: self.recipe,
            target_recipe: self.recipe,
            base_version: self.version,
            target_version,
            base_root: self.root,
            target_root,
            source: self.basis,
            base_frontier: self.frontier,
            target_frontier,
            coverage,
            capability,
            delta,
            relation,
            relation_changes,
        })
    }

    /// Consumes this base root and a prepared transition, returning the
    /// committed root and its exact transition receipt.
    ///
    /// # Errors
    ///
    /// Returns a [`ViewError`] when the prepared transition does not match the
    /// consumed base root.
    pub fn commit(
        self,
        prepared: PreparedViewDelta,
    ) -> Result<(Self, CommittedViewDelta), ViewError> {
        let result = prepared.commit(&self)?;
        Ok(result)
    }
}
