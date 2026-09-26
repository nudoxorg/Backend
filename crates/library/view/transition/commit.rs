//! Committed view delta methods.

use super::*;

impl CommittedViewDelta {
    /// Returns the checked relation transition identity.
    #[must_use]
    pub const fn id(&self) -> crate::ViewDeltaId {
        self.id
    }

    /// Returns the recipe identity before this transition.
    #[must_use]
    pub const fn base_recipe(&self) -> ViewRecipeId {
        self.base_recipe
    }

    /// Returns the recipe identity after this transition.
    #[must_use]
    pub const fn target_recipe(&self) -> ViewRecipeId {
        self.target_recipe
    }

    /// Returns the view version before this transition.
    #[must_use]
    pub const fn base_version(&self) -> ViewVersion {
        self.base_version
    }

    /// Returns the view version after this transition.
    #[must_use]
    pub const fn target_version(&self) -> ViewVersion {
        self.target_version
    }

    /// Returns the visible relation root before this transition.
    #[must_use]
    pub const fn base_root(&self) -> ViewStateRoot {
        self.base_root
    }

    /// Returns the visible relation root after this transition.
    #[must_use]
    pub const fn target_root(&self) -> ViewStateRoot {
        self.target_root
    }

    /// Returns the source basis shared by the transition.
    #[must_use]
    pub const fn source(&self) -> Basis {
        self.source
    }

    /// Returns the target frontier.
    #[must_use]
    pub const fn frontier(&self) -> Frontier {
        self.frontier
    }

    /// Returns target lane coverage.
    #[must_use]
    pub fn coverage(&self) -> &[Coverage] {
        &self.coverage
    }

    /// Returns the exact checked change description carried by this
    /// transition.  The description is independent of the retained base and
    /// target handles, so compact journal codecs can persist one row delta
    /// without serializing either complete view root.
    #[must_use]
    pub const fn delta(&self) -> &ViewDelta {
        &self.delta
    }

    /// Returns the number of visible rows affected by this transition.
    #[must_use]
    pub fn changed_row_count(&self) -> usize {
        match &self.delta {
            ViewDelta::Upsert { .. } | ViewDelta::Remove { .. } => 1,
            ViewDelta::Patch { changes } => changes.len(),
            ViewDelta::Coverage { .. } => 0,
            ViewDelta::Reset { root } => self
                .base
                .rows()
                .iter()
                .zip(root.rows().iter())
                .filter(|(before, after)| before != after)
                .count()
                .saturating_add(self.base.rows().len().abs_diff(root.rows().len())),
        }
    }

    /// Returns the exact canonical relation-change bytes used to derive the
    /// transition identity. Producers may include these bytes in a wire delta
    /// certificate; receivers re-admit the ID against the checked roots.
    #[must_use]
    pub fn canonical_changes(&self) -> &[u8] {
        &self.relation_changes
    }

    /// Returns the exact checked base view retained by this transition.
    ///
    /// Producers use this snapshot when constructing a certified event. The
    /// returned reference is immutable and shares the transition's retained
    /// canonical tree, so exposing it does not create a second mutable owner.
    #[must_use]
    pub fn base_view(&self) -> &ViewRoot {
        self.base.as_ref()
    }

    /// Returns the exact checked target view retained by this transition.
    #[must_use]
    pub fn target_view(&self) -> &ViewRoot {
        self.next.as_ref()
    }

    pub(crate) fn validate(&self) -> Result<(), ViewError> {
        if !self.base.is_coherent()
            || !self.next.is_coherent()
            || self.source != self.base.basis
            || self.base_recipe != self.base.recipe
            || self.base_version != self.base.version
            || self.base_root != self.base.root
            || self.target_recipe != self.next.recipe
            || self.target_version != self.next.version
            || self.target_root != self.next.root
            || self.frontier != self.next.frontier
            || self.coverage.as_ref() != self.next.coverage.as_ref()
        {
            return Err(ViewError::InvalidRelationDelta);
        }
        if self.base.relation.root() != self.base_root
            || self.next.relation.root() != self.target_root
        {
            return Err(ViewError::InvalidRelationDelta);
        }
        if crate::admit_delta_transition(
            &encode_id(self.id.as_bytes()),
            self.base_root,
            self.target_root,
            &self.relation_changes,
        )
        .is_err()
        {
            return Err(ViewError::InvalidRelationDelta);
        }
        if !delta_matches_target_rows(&self.base, &self.next, &self.delta) {
            return Err(ViewError::InvalidRelationDelta);
        }
        Ok(())
    }

    /// Applies this committed transition by consuming both the receipt and
    /// exact base root.
    ///
    /// # Errors
    ///
    /// Returns a [`ViewError`] when the receipt is forged, internally
    /// inconsistent, or supplied with a different base root.
    pub fn apply_to(self, base: &ViewRoot) -> Result<ViewRoot, ViewError> {
        self.validate()?;
        if base.recipe != self.base_recipe {
            return Err(ViewError::WrongViewIdentity);
        }
        if base.version != self.base_version || base.root != self.base_root {
            return Err(ViewError::WrongBase);
        }
        if base.basis != self.source || base.frontier != self.base.frontier {
            return Err(ViewError::WrongBasis);
        }
        Ok((*self.next).clone())
    }

    /// Returns the exact base snapshot retained for transport validation.
    pub(crate) fn base_for_wire(&self) -> &ViewRoot {
        self.base.as_ref()
    }

    /// Returns the exact target snapshot retained for transport validation.
    pub(crate) fn target_for_wire(&self) -> &ViewRoot {
        self.next.as_ref()
    }

    /// Returns the target visible relation root.
    #[must_use]
    pub const fn target(&self) -> ViewStateRoot {
        self.target_root
    }

    /// Returns the target source frontier.
    #[must_use]
    pub const fn target_frontier(&self) -> Frontier {
        self.frontier
    }

    /// Returns the flow execution frontier bound to the committed target root.
    #[must_use]
    pub fn flow_frontier(&self) -> backend_flow::BoundFrontier<crate::ViewRelation> {
        backend_flow::BoundFrontier::new(self.target_root, self.frontier.flow())
    }

    /// Returns the producer-admitted source scope used by this transition.
    #[must_use]
    pub fn coverage_scope(&self) -> ScopeRoot {
        self.capability.scope_root()
    }
}
