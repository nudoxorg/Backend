//! Checked view transitions and bounded snapshot values.

use super::{Basis, Coverage, CoverageCapability, Freshness, Row, RowId, ViewRoot};
use crate::Cursor;
use crate::canonical::{
    Frontier, ViewEntry, ViewEntryKey, ViewMetadata, ViewRecipeId, ViewStateRoot, ViewVersion,
    encode_id,
};
use backend_version::{MapChange, PreparedDelta, ScopeRoot};
use std::sync::{Arc, OnceLock};

/// Maximum row changes carried by one atomic hot transition.
pub const MAX_VIEW_PATCH_ROWS: usize = 256;

/// One stable-row operation inside an atomic view patch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowChange {
    /// Insert or replace one row.
    Upsert(Box<Row>),
    /// Remove one row by stable identity.
    Remove(RowId),
}

impl RowChange {
    /// Returns the stable identity selected by this operation.
    #[must_use]
    pub const fn id(&self) -> RowId {
        match self {
            Self::Upsert(row) => row.id,
            Self::Remove(id) => *id,
        }
    }
}

/// Typed update to a view root. This is only a change description; it has no
/// authority to mutate a root until [`ViewRoot::prepare`] validates it.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewDelta {
    /// Replace the complete view with another coherent content projection.
    Reset {
        /// Complete replacement root. Its recipe must match the base recipe;
        /// its target version is re-derived during preparation.
        root: Box<ViewRoot>,
    },
    /// Insert or replace one stable row.
    Upsert {
        /// Row to insert or replace.
        row: Row,
    },
    /// Remove one stable row.
    Remove {
        /// Stable row identity.
        id: RowId,
    },
    /// Atomically apply a sorted, duplicate-free set of independent row changes.
    Patch {
        /// Row changes in stable identity order.
        changes: Arc<[RowChange]>,
    },
    /// Add one lane coverage report.
    Coverage {
        /// Coverage report.
        coverage: Coverage,
    },
}

/// A checked delta prepared against one exact view root.
#[derive(Debug)]
pub struct PreparedViewDelta {
    pub(super) base_recipe: ViewRecipeId,
    pub(super) target_recipe: ViewRecipeId,
    pub(super) base_version: ViewVersion,
    pub(super) target_version: ViewVersion,
    pub(super) base_root: ViewStateRoot,
    pub(super) target_root: ViewStateRoot,
    pub(super) source: Basis,
    pub(super) base_frontier: Frontier,
    pub(super) target_frontier: Frontier,
    pub(super) coverage: Box<[Coverage]>,
    pub(super) delta: ViewDelta,
    pub(super) relation: PreparedDelta<crate::ViewRelation>,
    pub(super) relation_changes: Box<[u8]>,
    pub(super) capability: CoverageCapability,
}

impl PartialEq for PreparedViewDelta {
    fn eq(&self, other: &Self) -> bool {
        self.base_recipe == other.base_recipe
            && self.target_recipe == other.target_recipe
            && self.base_version == other.base_version
            && self.target_version == other.target_version
            && self.base_root == other.base_root
            && self.target_root == other.target_root
            && self.source == other.source
            && self.base_frontier == other.base_frontier
            && self.target_frontier == other.target_frontier
            && self.coverage == other.coverage
            && self.delta == other.delta
            && self.relation == other.relation
            && self.relation_changes == other.relation_changes
            && self.capability == other.capability
    }
}

impl Eq for PreparedViewDelta {}

impl PreparedViewDelta {
    /// Returns the stable recipe expected by this transition.
    #[must_use]
    pub const fn base_view(&self) -> ViewRecipeId {
        self.base_recipe
    }

    /// Returns the stable recipe produced by this transition.
    #[must_use]
    pub const fn target_view(&self) -> ViewRecipeId {
        self.target_recipe
    }

    /// Returns the view version expected by this transition.
    #[must_use]
    pub const fn base_version(&self) -> ViewVersion {
        self.base_version
    }

    /// Returns the view version produced by this transition.
    #[must_use]
    pub const fn target_version(&self) -> ViewVersion {
        self.target_version
    }

    /// Returns the canonical visible row root this transition expects.
    #[must_use]
    pub const fn base_root(&self) -> ViewStateRoot {
        self.base_root
    }

    /// Returns the canonical visible row root this transition produces.
    #[must_use]
    pub const fn target_root(&self) -> ViewStateRoot {
        self.target_root
    }

    /// Returns the checked relation transition identity.
    #[must_use]
    pub fn id(&self) -> crate::ViewDeltaId {
        self.relation.delta().id()
    }

    /// Returns the exact canonical relation changes retained by the backend
    /// preparation.
    #[must_use]
    pub fn canonical_changes(&self) -> &[u8] {
        &self.relation_changes
    }

    /// Returns the producer-admitted source scope used by this transition.
    #[must_use]
    pub fn coverage_scope(&self) -> ScopeRoot {
        self.capability.scope_root()
    }

    /// Consumes this preparation and the exact base root to publish a
    /// committed transition.
    ///
    /// # Errors
    ///
    /// Returns a [`ViewError`] when the supplied base differs in recipe,
    /// version, root, basis, frontier, or target content.
    pub fn commit(self, base: &ViewRoot) -> Result<(ViewRoot, CommittedViewDelta), ViewError> {
        if base.recipe != self.base_recipe {
            return Err(ViewError::WrongViewIdentity);
        }
        if base.version != self.base_version || base.root != self.base_root {
            return Err(ViewError::WrongBase);
        }
        if base.basis != self.source {
            return Err(ViewError::WrongBasis);
        }
        if base.frontier != self.base_frontier {
            return Err(ViewError::WrongFrontier);
        }
        let relation_id = self.relation.delta().id();
        let relation_changes = self.relation_changes;
        let target_relation = self
            .relation
            .commit(&base.relation)
            .map_err(|_| ViewError::InvalidRelationDelta)?;
        if target_relation.root() != self.target_root {
            return Err(ViewError::WrongTarget);
        }
        let next = ViewRoot {
            recipe: self.target_recipe,
            version: self.target_version,
            root: self.target_root,
            basis: self.source,
            frontier: self.target_frontier,
            rows_cache: Arc::new(OnceLock::new()),
            coverage: self.coverage.clone(),
            capability: Some(self.capability.clone()),
            relation: target_relation,
        };
        if !next.is_coherent() {
            return Err(ViewError::WrongTarget);
        }
        let committed = CommittedViewDelta {
            base_recipe: self.base_recipe,
            target_recipe: self.target_recipe,
            base_version: self.base_version,
            target_version: self.target_version,
            base_root: self.base_root,
            target_root: self.target_root,
            source: self.source,
            frontier: self.target_frontier,
            coverage: self.coverage,
            delta: self.delta,
            capability: self.capability.clone(),
            id: relation_id,
            relation_changes,
            base: Arc::new(base.clone()),
            next: Arc::new(next.clone()),
        };
        Ok((next, committed))
    }
}

/// A prepared transition that has been committed against its exact base.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedViewDelta {
    /// Stable view recipe identity before the transition.
    pub(crate) base_recipe: ViewRecipeId,
    /// Stable view recipe identity after the transition.
    pub(crate) target_recipe: ViewRecipeId,
    /// View version before the transition.
    pub(crate) base_version: ViewVersion,
    /// View version after the transition.
    pub(crate) target_version: ViewVersion,
    /// Row relation root before the transition.
    pub(crate) base_root: ViewStateRoot,
    /// Row relation root after the transition.
    pub(crate) target_root: ViewStateRoot,
    /// Source basis shared by the transition.
    pub(crate) source: Basis,
    /// Source frontier after the transition.
    pub(crate) frontier: Frontier,
    /// Coverage after the transition.
    pub(crate) coverage: Box<[Coverage]>,
    /// Original checked change payload.
    pub(crate) delta: ViewDelta,
    capability: CoverageCapability,
    /// Idempotency identity for replay/deduplication.
    pub(crate) id: crate::ViewDeltaId,
    /// Canonical ordered relation changes retained as transition evidence.
    relation_changes: Box<[u8]>,
    base: Arc<ViewRoot>,
    next: Arc<ViewRoot>,
}

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

/// Query/view snapshot with explicit freshness and continuation cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewSnapshot {
    /// Immutable view root.
    pub root: ViewRoot,
    /// Freshness relative to the request.
    pub freshness: Freshness,
    /// Continuation cursor, if more bounded rows are available.
    pub next: Option<Cursor>,
    /// Typed semantic edges when this snapshot came from the compiler graph
    /// authority. Ordinary snapshots leave this absent, preserving their
    /// existing wire shape and meaning.
    pub graph_relations: Option<Box<[crate::GraphRelation]>>,
}

/// Rejected update reason at the view boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewError {
    /// The prepared transition names another row relation root.
    WrongBase,
    /// A row or replacement root names another source basis.
    WrongBasis,
    /// The transition names another view recipe identity.
    WrongViewIdentity,
    /// The source branch/log/schema/sequence changed while preparing.
    WrongFrontier,
    /// The base root or its rows are not coherent.
    IncoherentBase,
    /// A replacement would introduce two rows with one stable identity.
    Duplicate,
    /// Coverage or row growth exceeded the bounded view contract.
    Unbounded,
    /// The generated relation transition could not be prepared.
    InvalidRelationDelta,
    /// The target version or root did not match prepared content.
    WrongTarget,
    /// The exact relation scope could not be admitted as complete.
    InvalidCoverage,
}

pub(super) fn relation_changes_for_delta(
    base: &ViewRoot,
    delta: &ViewDelta,
    target_frontier: Frontier,
    target_coverage: &[Coverage],
) -> Vec<MapChange<crate::ViewRelation>> {
    let mut changes: Vec<MapChange<crate::ViewRelation>> = Vec::with_capacity(3);
    let metadata_key = ViewEntryKey::Metadata;
    let metadata_before = base.relation.get(&metadata_key).cloned();
    let metadata_after = Some(ViewEntry::Metadata(ViewMetadata::new(
        base.basis,
        target_frontier,
        target_coverage,
    )));
    if metadata_before != metadata_after {
        changes.push(MapChange {
            key: metadata_key,
            before: metadata_before,
            after: metadata_after,
        });
    }
    match delta {
        ViewDelta::Upsert { row } => {
            let key = ViewEntryKey::Row(row.id);
            let before = base.relation.get(&key).cloned();
            let after = Some(ViewEntry::Row(row.clone()));
            if before != after {
                changes.push(MapChange { key, before, after });
            }
        }
        ViewDelta::Remove { id } => {
            let key = ViewEntryKey::Row(*id);
            let before = base.relation.get(&key).cloned();
            if before.is_some() {
                changes.push(MapChange {
                    key,
                    before,
                    after: None,
                });
            }
        }
        ViewDelta::Patch { changes: patch } => {
            changes.reserve(patch.len());
            for change in patch.iter() {
                let key = ViewEntryKey::Row(change.id());
                let before = base.relation.get(&key).cloned();
                let after = match change {
                    RowChange::Upsert(row) => Some(ViewEntry::Row(row.as_ref().clone())),
                    RowChange::Remove(_) => None,
                };
                if before != after {
                    changes.push(MapChange { key, before, after });
                }
            }
        }
        ViewDelta::Coverage { .. } => {}
        ViewDelta::Reset { root } => {
            // A reset can replace an arbitrary number of rows. It is the
            // explicit cold-load operation; incremental upsert/remove paths
            // above retain the direct changed-key guarantee.
            let mut keys = std::collections::BTreeSet::new();
            keys.extend(
                base.relation
                    .iter()
                    .filter_map(|(key, _)| matches!(key, ViewEntryKey::Row(_)).then_some(key)),
            );
            keys.extend(
                root.relation
                    .iter()
                    .filter_map(|(key, _)| matches!(key, ViewEntryKey::Row(_)).then_some(key)),
            );
            for key in keys {
                let before = base.relation.get(&key).cloned();
                let after = root.relation.get(&key).cloned();
                if before != after {
                    changes.push(MapChange { key, before, after });
                }
            }
        }
    }
    changes.sort_by_key(|change| change.key);
    changes
}

pub(super) fn validate_delta(
    base: &ViewRoot,
    delta: &ViewDelta,
    capability: &CoverageCapability,
) -> Result<Box<[Coverage]>, ViewError> {
    let coverage = match delta {
        ViewDelta::Reset { root } => {
            if root.recipe != base.recipe {
                return Err(ViewError::WrongViewIdentity);
            }
            if root.basis != base.basis
                || root.frontier.branch != base.frontier.branch
                || root.frontier.log != base.frontier.log
                || root.frontier.schema != base.frontier.schema
                || root.frontier.root != base.frontier.root
            {
                return Err(ViewError::WrongBasis);
            }
            if !root.is_coherent() {
                return Err(ViewError::IncoherentBase);
            }
            root.coverage.clone()
        }
        ViewDelta::Upsert { row } => {
            if row.basis != base.basis {
                return Err(ViewError::WrongBasis);
            }
            base.coverage.clone()
        }
        ViewDelta::Remove { .. } => base.coverage.clone(),
        ViewDelta::Patch { changes } => {
            if changes.is_empty()
                || changes.len() > MAX_VIEW_PATCH_ROWS
                || changes.windows(2).any(|pair| pair[0].id() >= pair[1].id())
            {
                return Err(ViewError::Unbounded);
            }
            if changes
                .iter()
                .any(|change| matches!(change, RowChange::Upsert(row) if row.basis != base.basis))
            {
                return Err(ViewError::WrongBasis);
            }
            base.coverage.clone()
        }
        ViewDelta::Coverage { coverage } => {
            if !coverage.is_valid() || base.coverage.len() >= 64 {
                return Err(if coverage.is_valid() {
                    ViewError::Unbounded
                } else {
                    ViewError::InvalidCoverage
                });
            }
            let mut values = base.coverage.to_vec();
            values.push(*coverage);
            values.into_boxed_slice()
        }
    };
    if coverage.iter().any(|value| value.is_complete())
        && capability.scope_root() != ScopeRoot::from_bytes(base.basis.object.to_bytes())
    {
        return Err(ViewError::InvalidCoverage);
    }
    Ok(coverage)
}

fn delta_matches_target_rows(base: &ViewRoot, target: &ViewRoot, delta: &ViewDelta) -> bool {
    if !super::root::relation_metadata_matches(target) {
        return false;
    }
    match delta {
        ViewDelta::Upsert { row } => target.row(row.id).as_ref() == Some(row),
        ViewDelta::Remove { id } => target.row(*id).is_none(),
        ViewDelta::Patch { changes } => changes.iter().all(|change| match change {
            RowChange::Upsert(row) => target.row(row.id).as_ref() == Some(row.as_ref()),
            RowChange::Remove(id) => target.row(*id).is_none(),
        }),
        // Coverage changes only replace the metadata entry. The checked
        // target root and metadata binding above prove that the row tree was
        // retained; walking every row here would defeat O(1) validation.
        ViewDelta::Coverage { .. } => {
            base.relation.get(&ViewEntryKey::Metadata).is_some()
                && target.relation.get(&ViewEntryKey::Metadata).is_some()
        }
        ViewDelta::Reset { root } => target
            .relation
            .iter()
            .filter(|(key, _)| matches!(key, ViewEntryKey::Row(_)))
            .eq(root
                .relation
                .iter()
                .filter(|(key, _)| matches!(key, ViewEntryKey::Row(_)))),
    }
}
