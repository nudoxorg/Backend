//! Persistent immutable view root and canonical relation state.

use super::{
    Basis, CommittedViewDelta, Coverage, CoverageCapability, Lane, PreparedViewDelta, Reason, Row,
    RowId, RowIdentityPreimage, ViewDelta, ViewError, ViewPageCursor, ViewPageError,
    ViewRootDescriptor, ViewSnapshotPage,
};
use crate::canonical::{
    Frontier, PackageKey, ViewEntry, ViewEntryKey, ViewMetadata, ViewRecipeId, ViewStateRoot,
    ViewVersion, package_key, symbol_key, view_version_preimage,
};
use backend_version::{
    Coverage as BackendCoverage, CoverageWitness, RelationState, ScopeRoot, UntrustedCoverageScope,
    prepare_delta_with_state,
};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::{Arc, OnceLock};

/// Maximum number of rows returned by one snapshot hydration page.
///
/// A reset describes the complete root in constant space and hydrates rows in
/// these small bounded pages.  Keeping this limit in the library contract
/// means a local daemon and every process client reject the same oversized
/// page before allocating a second copy of a view.
pub const MAX_SNAPSHOT_PAGE_ROWS: usize = 256;

/// Immutable root of a UI or protocol view.
#[derive(Clone, Debug)]
pub struct ViewRoot {
    /// Stable view recipe identity shared by every version of this view.
    pub(crate) recipe: ViewRecipeId,
    /// Content identity of this complete view snapshot.
    pub(crate) version: ViewVersion,
    /// Canonical visible row relation root.
    pub(crate) root: ViewStateRoot,
    /// Exact source basis for all rows.
    pub(crate) basis: Basis,
    /// Source branch/log/schema/sequence frontier.
    pub(crate) frontier: Frontier,
    /// Lazy compatibility materialization of canonical relation rows.
    pub(super) rows_cache: Arc<OnceLock<Arc<[Row]>>>,
    /// First and last label in each package, built without cloning row documents.
    pub(super) package_labels: Arc<OnceLock<PackageLabelIndex>>,
    /// First row identity for each exact label, built without cloning row bodies.
    pub(super) label_ids: Arc<OnceLock<BTreeMap<String, RowId>>>,
    /// Coverage for each requested lane.
    pub(crate) coverage: Box<[Coverage]>,
    /// Producer-admitted witness for complete source coverage.
    ///
    /// A view may retain incomplete coverage without a witness.  Complete
    /// coverage always carries the capability that admitted its producer
    /// scope; keeping it on the root prevents a later caller from replacing
    /// the witness with a tautological root comparison.
    pub(super) capability: Option<CoverageCapability>,
    /// Persistent canonical relation state for this exact root. Backend
    /// updates path-copy this state and retain untouched canonical nodes.
    pub(super) relation: RelationState<crate::ViewRelation>,
}

#[derive(Clone, Debug, Default)]
struct PackageLabelMaps {
    first: BTreeMap<String, RowId>,
    last: BTreeMap<String, RowId>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct PackageLabelIndex {
    by_package: BTreeMap<PackageKey, PackageLabelMaps>,
}

impl PartialEq for ViewRoot {
    fn eq(&self, other: &Self) -> bool {
        self.recipe == other.recipe
            && self.version == other.version
            && self.root == other.root
            && self.basis == other.basis
            && self.frontier == other.frontier
            && self.coverage == other.coverage
            && self.capability == other.capability
            && self.relation == other.relation
    }
}

impl Eq for ViewRoot {}

impl ViewRoot {
    /// Returns the stable recipe identity shared by all versions.
    #[must_use]
    pub const fn recipe(&self) -> ViewRecipeId {
        self.recipe
    }

    /// Returns the immutable content version.
    #[must_use]
    pub const fn version(&self) -> ViewVersion {
        self.version
    }

    /// Returns the canonical visible relation root.
    #[must_use]
    pub const fn root(&self) -> ViewStateRoot {
        self.root
    }

    /// Returns the complete source basis.
    #[must_use]
    pub const fn basis(&self) -> Basis {
        self.basis
    }

    /// Returns the source branch/log/schema/sequence frontier.
    #[must_use]
    pub const fn frontier(&self) -> Frontier {
        self.frontier
    }

    /// Returns a constant-size descriptor for this immutable root.
    ///
    /// The descriptor retains identity, source basis, frontier, coverage, and
    /// row count while leaving canonical row payloads in the persistent tree.
    /// It is the metadata portion of a paged reset response.
    #[must_use]
    pub fn descriptor(&self) -> ViewRootDescriptor {
        ViewRootDescriptor {
            recipe: self.recipe,
            version: self.version,
            root: self.root,
            basis: self.basis,
            frontier: self.frontier,
            coverage: self.coverage.clone(),
            capability: self.capability.clone(),
            row_count: self.row_count(),
        }
    }

    /// Returns the number of visible rows without materializing the row
    /// compatibility slice.
    #[must_use]
    pub fn row_count(&self) -> u64 {
        self.relation.root_handle().summary().len.saturating_sub(1) as u64
    }

    /// Reads one bounded page directly from the retained canonical tree.
    ///
    /// The cursor is bound to this root's recipe/version/root and advances by
    /// stable row identity.  The range seek touches one root-to-leaf path and
    /// clones at most `credit + 1` rows, with the extra row used only to detect
    /// a continuation.
    ///
    /// # Errors
    ///
    /// Returns [`ViewPageError`] when credit is outside the shared bound, the
    /// cursor names another root, or its anchor is absent.
    pub fn page(
        &self,
        cursor: ViewPageCursor,
        credit: usize,
    ) -> Result<ViewSnapshotPage, ViewPageError> {
        if credit == 0 || credit > MAX_SNAPSHOT_PAGE_ROWS {
            return Err(ViewPageError::InvalidCredit);
        }
        if cursor.recipe() != self.recipe
            || cursor.version() != self.version
            || cursor.root() != self.root
        {
            return Err(ViewPageError::CursorMismatch);
        }
        let start = match cursor.after() {
            Some(after) => {
                if !matches!(
                    self.relation.get(&ViewEntryKey::Row(after)),
                    Some(ViewEntry::Row(_))
                ) {
                    return Err(ViewPageError::MissingAnchor);
                }
                Bound::Excluded(ViewEntryKey::Row(after))
            }
            None => Bound::Excluded(ViewEntryKey::Metadata),
        };
        let mut rows = self
            .relation
            .range((start, Bound::Unbounded))
            .filter_map(|(key, value)| match (key, value) {
                (ViewEntryKey::Row(_), ViewEntry::Row(row)) => Some(row.clone()),
                _ => None,
            })
            .take(credit.checked_add(1).ok_or(ViewPageError::Overflow)?)
            .collect::<Vec<_>>();
        let next = if rows.len() > credit {
            let last = rows
                .get(credit.saturating_sub(1))
                .map(|row| row.id)
                .ok_or(ViewPageError::Overflow)?;
            rows.truncate(credit);
            Some(ViewPageCursor::from_after(self, last))
        } else {
            None
        };
        rows.truncate(credit);
        Ok(ViewSnapshotPage {
            cursor,
            rows: rows.into_boxed_slice(),
            next,
            total_rows: self.row_count(),
        })
    }

    /// Returns the flow execution frontier bound to this exact relation root.
    ///
    /// Arrangement consumers use this value to coordinate incremental work;
    /// the root binding prevents a progress report from being detached from
    /// the view state it describes.
    #[must_use]
    pub fn flow_frontier(&self) -> backend_flow::BoundFrontier<crate::ViewRelation> {
        backend_flow::BoundFrontier::new(self.root, self.frontier.flow())
    }

    /// Borrows canonical rows without filling the owned row cache.
    ///
    /// Republishing a frontier uses this to compare stored hashes. The owned
    /// cache remains available through [`Self::rows`] for callers that need a
    /// slice.
    #[must_use]
    pub fn row_refs(&self) -> impl Iterator<Item = &Row> {
        self.relation
            .iter()
            .filter_map(|(key, value)| match (key, value) {
                (ViewEntryKey::Row(_), ViewEntry::Row(row)) => Some(row),
                _ => None,
            })
    }

    /// Returns stable rows in canonical order.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        self.rows_cache
            .get_or_init(|| {
                self.relation
                    .iter()
                    .filter_map(|(key, value)| match (key, value) {
                        (ViewEntryKey::Row(_), ViewEntry::Row(row)) => Some(row.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
                    .into()
            })
            .as_ref()
    }

    /// Streams canonical rows without populating the compatibility cache.
    ///
    /// Internal index builders use this borrowed traversal during cold load,
    /// keeping the persistent relation as the sole retained row owner.
    pub(crate) fn iter_rows(&self) -> impl Iterator<Item = &Row> {
        self.relation
            .iter()
            .filter_map(|(key, value)| match (key, value) {
                (ViewEntryKey::Row(_), ViewEntry::Row(row)) => Some(row),
                _ => None,
            })
    }

    /// Reports whether [`Self::rows`] has filled the owned compatibility slice.
    ///
    /// Borrowed publication walks [`Self::row_refs`] and leaves this false.
    #[must_use]
    pub fn compatibility_rows_are_materialized(&self) -> bool {
        self.rows_cache.get().is_some()
    }

    /// Looks up one row by stable identity without materializing the complete
    /// compatibility slice.
    #[must_use]
    pub fn row(&self, id: RowId) -> Option<Row> {
        self.row_ref(id).cloned()
    }

    /// Borrows the first row with this exact label.
    ///
    /// The index is filled from borrowed rows and does not clone row documents
    /// into the compatibility cache. A repeated label keeps the earliest row.
    #[must_use]
    pub fn row_by_label(&self, label: &str) -> Option<&Row> {
        let id = *self.label_ids().get(label)?;
        self.row_ref(id)
    }

    fn label_ids(&self) -> &BTreeMap<String, RowId> {
        self.label_ids.get_or_init(|| {
            let mut index = BTreeMap::new();
            for row in self.row_refs() {
                index.entry(row.label.clone()).or_insert(row.id);
            }
            index
        })
    }

    /// Borrows one row by stable identity without cloning its retained text
    /// or populating the complete compatibility slice.
    #[must_use]
    pub fn row_ref(&self, id: RowId) -> Option<&Row> {
        match self.relation.get(&ViewEntryKey::Row(id)) {
            Some(ViewEntry::Row(row)) => Some(row),
            _ => None,
        }
    }

    /// Returns the earliest row in relation order with this package and label.
    ///
    /// The index is built from borrowed rows, so the compatibility slice stays
    /// empty. Duplicate labels keep that first row.
    #[must_use]
    pub fn first_package_label(&self, package: PackageKey, label: &str) -> Option<RowId> {
        self.package_label_index()
            .by_package
            .get(&package)
            .and_then(|maps| maps.first.get(label).copied())
    }

    /// Returns the latest row in relation order with this package and label.
    ///
    /// Call-graph coordinate maps keep this later row when labels collide.
    #[must_use]
    pub fn last_package_label(&self, package: PackageKey, label: &str) -> Option<RowId> {
        self.package_label_index()
            .by_package
            .get(&package)
            .and_then(|maps| maps.last.get(label).copied())
    }

    fn package_label_index(&self) -> &PackageLabelIndex {
        self.package_labels.get_or_init(|| {
            let mut index = PackageLabelIndex::default();
            for row in self.row_refs() {
                let Some(package) = row.package else {
                    continue;
                };
                let maps = index.by_package.entry(package).or_default();
                maps.first.entry(row.label.clone()).or_insert(row.id);
                maps.last.insert(row.label.clone(), row.id);
            }
            index
        })
    }

    /// Resolves an opaque symbol selector by membership in this exact view.
    ///
    /// The claimed digest is never promoted directly. Symbol rows sit in
    /// [`RowId`] order inside the relation, so the lookup seeks one
    /// root-to-leaf path and borrows the typed key from the stored row. The
    /// compatibility row slice stays untouched.
    #[must_use]
    pub fn resolve_symbol_commitment(&self, claimed: [u8; 32]) -> Option<crate::SymbolKey> {
        let (_key, value) = self.relation.find_by(|key| match key {
            ViewEntryKey::Metadata => Ordering::Less,
            ViewEntryKey::Row(RowId::Package(_)) => Ordering::Less,
            ViewEntryKey::Row(RowId::Symbol(symbol)) => symbol.as_bytes().cmp(&claimed),
            ViewEntryKey::Row(RowId::Object(_)) => Ordering::Greater,
        })?;
        match value {
            ViewEntry::Row(row) => match row.id {
                RowId::Symbol(symbol) if symbol.as_bytes() == &claimed => Some(symbol),
                RowId::Package(_) | RowId::Symbol(_) | RowId::Object(_) => None,
            },
            ViewEntry::Metadata(_) => None,
        }
    }

    /// Looks up only the canonical display label for one row.
    ///
    /// Producers use this narrow lookup while certifying a bounded page's
    /// ordinary package/parent references. Rows with an explicit identity
    /// witness use [`Self::row_identity_preimage`] instead. Both lookups keep
    /// claims independently checkable without materializing the complete
    /// compatibility row slice.
    #[must_use]
    pub fn row_label(&self, id: RowId) -> Option<String> {
        self.row_ref(id).map(|row| row.label.clone())
    }

    /// Returns the explicit canonical identity preimage for one row, when
    /// the producer attached one.  This is intentionally separate from
    /// [`Self::row_label`]: labels are presentation text and cannot certify
    /// occurrence-disambiguated identities.
    #[must_use]
    pub fn row_identity_preimage(&self, id: RowId) -> Option<RowIdentityPreimage> {
        self.row_ref(id)
            .and_then(|row| row.identity_preimage().cloned())
    }

    /// Returns the canonical relation-node bytes whose commitment is
    /// [`Self::root`].  A producer can place these bytes in a
    /// [`crate::WireClaim::Root`] certificate for a standalone transport
    /// receiver to rehash independently.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError::InvalidRelationDelta`] when the retained rows and
    /// metadata no longer admit a canonical relation node.
    pub fn canonical_relation_bytes(&self) -> Result<Vec<u8>, ViewError> {
        Ok(self.relation.materialize().canonical_bytes().to_vec())
    }

    /// Returns a conservative serialized-size bound without materializing
    /// the compatibility row slice.  Transports use this before converting a
    /// root into a wire DTO so an oversized producer result is rejected while
    /// its persistent relation remains lazy.
    #[must_use]
    pub fn encoded_size_bound(&self) -> usize {
        let mut raw = 4096usize;
        for (_, entry) in self.relation.iter() {
            raw = raw.saturating_add(match entry {
                ViewEntry::Metadata(_) => 512,
                ViewEntry::Row(row) => row_encoded_size(row),
            });
        }
        raw.saturating_mul(4)
    }

    /// Returns lane coverage reports.
    #[must_use]
    pub fn coverage(&self) -> &[Coverage] {
        &self.coverage
    }

    /// Returns the producer capability retained by this root, when its
    /// coverage was admitted as complete.
    #[must_use]
    pub fn capability(&self) -> Option<CoverageCapability> {
        self.capability.clone()
    }

    /// Returns the checked backend witness carried by the authoritative
    /// relation. Secondary flow materializations reuse this witness instead
    /// of inventing an independent coverage claim.
    #[must_use]
    pub(crate) const fn relation_coverage(&self) -> CoverageWitness {
        self.relation.coverage()
    }

    /// Builds the fixed unavailable root used before a producer has admitted
    /// any complete source coverage.
    ///
    /// This constructor only records an unavailable lane and therefore cannot
    /// mint a complete-coverage capability. Callers that have arbitrary rows
    /// or coverage reports must use [`Self::new_incomplete`] so those values
    /// are checked before publication.
    pub(crate) fn unavailable(
        recipe: ViewRecipeId,
        basis: Basis,
        frontier: Frontier,
        lane: Lane,
        reason: Reason,
    ) -> Self {
        let coverage: Box<[Coverage]> =
            vec![Coverage::Unavailable { lane, reason }].into_boxed_slice();
        let relation = RelationState::empty(CoverageWitness::Unavailable(
            UntrustedCoverageScope::from_scope_root(ScopeRoot::from_bytes(basis.object.to_bytes())),
        ));
        let root = relation.root();
        let version = version_for_view(recipe, basis, frontier, root, &coverage);
        Self {
            recipe,
            version,
            root,
            basis,
            frontier,
            rows_cache: Arc::new(OnceLock::new()),
            package_labels: Arc::new(OnceLock::new()),
            label_ids: Arc::new(OnceLock::new()),
            coverage,
            capability: None,
            relation,
        }
    }

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
        if rows.iter().any(|row| !row_identity_matches(row)) {
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
            relation_state_from_parts(basis, frontier, rows, &coverage, capability.clone())?;
        let root = relation.root();
        let version = version_for_view(recipe, basis, frontier, root, &coverage);
        Ok(Self {
            recipe,
            version,
            root,
            basis,
            frontier,
            rows_cache: Arc::new(OnceLock::new()),
            package_labels: Arc::new(OnceLock::new()),
            label_ids: Arc::new(OnceLock::new()),
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
            && relation_metadata_matches(self)
            && self.version
                == version_for_view(
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
        let coverage = super::transition::validate_delta(self, &delta, &capability)?;
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
        let changes =
            super::transition::relation_changes_for_delta(self, &delta, target_frontier, &coverage);
        let (relation, _work) = prepare_delta_with_state(&self.relation, changes)
            .map_err(|_| ViewError::InvalidRelationDelta)?;
        let target_root = relation.delta().target();
        let target_version = version_for_view(
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

fn relation_state_from_parts(
    basis: Basis,
    frontier: Frontier,
    rows: impl IntoIterator<Item = Row>,
    coverage: &[Coverage],
    capability: Option<CoverageCapability>,
) -> Result<RelationState<crate::ViewRelation>, ViewError> {
    let witness = capability.map_or_else(
        || {
            let kind = coverage
                .first()
                .copied()
                .map_or(BackendCoverage::Partial, |value| match value {
                    Coverage::Complete | Coverage::Partial { .. } => BackendCoverage::Partial,
                    Coverage::Unavailable { .. } => BackendCoverage::Unavailable,
                });
            match kind {
                BackendCoverage::Unavailable => {
                    CoverageWitness::Unavailable(UntrustedCoverageScope::from_scope_root(
                        ScopeRoot::from_bytes(basis.object.to_bytes()),
                    ))
                }
                BackendCoverage::Partial | BackendCoverage::Complete => {
                    CoverageWitness::Partial(UntrustedCoverageScope::from_scope_root(
                        ScopeRoot::from_bytes(basis.object.to_bytes()),
                    ))
                }
                BackendCoverage::Closed => {
                    CoverageWitness::Closed(backend_version::ClosedRelationScope::from_scope_root(
                        ScopeRoot::from_bytes(basis.object.to_bytes()),
                    ))
                }
                BackendCoverage::Unsupported => {
                    CoverageWitness::Unsupported(UntrustedCoverageScope::from_scope_root(
                        ScopeRoot::from_bytes(basis.object.to_bytes()),
                    ))
                }
            }
        },
        CoverageCapability::witness,
    );
    let metadata = ViewEntry::Metadata(ViewMetadata::new(basis, frontier, coverage));
    let entries = std::iter::once((ViewEntryKey::Metadata, metadata)).chain(
        rows.into_iter()
            .map(|row| (ViewEntryKey::Row(row.id), ViewEntry::Row(row))),
    );
    RelationState::from_entries(entries, witness).map_err(|_| ViewError::Duplicate)
}

pub(super) fn relation_metadata_matches(view: &ViewRoot) -> bool {
    let expected =
        ViewEntry::Metadata(ViewMetadata::new(view.basis, view.frontier, &view.coverage));
    match view.relation.get(&ViewEntryKey::Metadata) {
        Some(actual) => actual == &expected,
        None => {
            // The structurally infallible unavailable root is intentionally
            // an empty partial relation. It cannot be advanced or claim
            // complete coverage, and therefore has no metadata entry to
            // admit as a visible result.
            view.capability.is_none()
                && view
                    .coverage
                    .iter()
                    .all(|coverage| matches!(coverage, Coverage::Unavailable { .. }))
                && view.relation.iter().next().is_none()
        }
    }
}

fn version_for_view(
    recipe: ViewRecipeId,
    basis: Basis,
    frontier: Frontier,
    root: ViewStateRoot,
    coverage: &[Coverage],
) -> ViewVersion {
    crate::canonical::view_version(&view_version_preimage(
        recipe, basis, frontier, root, coverage,
    ))
}

fn row_encoded_size(row: &Row) -> usize {
    let mut size = 512usize
        .saturating_add(row.label.len())
        .saturating_add(row.signature.as_deref().map_or(0, str::len));
    size = size.saturating_add(
        row.identity_preimage()
            .map_or(0, |preimage| preimage.as_str().len()),
    );
    size = size.saturating_add(row.facts.text_bytes());
    for fragment in &row.document {
        size = size.saturating_add(match fragment {
            super::Fragment::Text(value) | super::Fragment::Code(value) => {
                64usize.saturating_add(value.len())
            }
            super::Fragment::Link { label, .. } => 96usize.saturating_add(label.len()),
            super::Fragment::Break => 8,
        });
    }
    size
}

/// Checks the optional row identity witness against the typed stable key.
/// Ordinary rows keep the compact no-sidecar representation; rows carrying a
/// witness must prove the exact digest before entering a relation root.
pub(super) fn row_identity_matches(row: &Row) -> bool {
    match (row.id, row.identity_preimage()) {
        (RowId::Package(package), Some(preimage)) => package_key(preimage.as_str()) == package,
        (RowId::Symbol(symbol), Some(preimage)) => symbol_key(preimage.as_str()) == symbol,
        (RowId::Object(_), Some(_)) => false,
        (_, None) => true,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::canonical::{object_version, symbol_key, view_key, view_state_root};
    use crate::{AuthorityScopeClaim, ScopeRoot};

    fn capability(object: crate::SemanticObject) -> CoverageCapability {
        let declared = AuthorityScopeClaim::from_object_version(object);
        let scope = ScopeRoot::from_bytes(object.to_bytes());
        let observation = crate::admit_producer_observation(
            crate::UntrustedProducerObservation::new(
                *scope.as_bytes(),
                scope,
                *scope.as_bytes(),
                scope.as_bytes().to_vec(),
            ),
            &TestCoverageVerifier,
        )
        .expect("producer observation");
        CoverageCapability::from_authorized_with_evidence(
            crate::admit_complete_scope(declared, observation).expect("coverage capability"),
            scope.as_bytes().to_vec(),
        )
        .expect("coverage evidence")
    }

    struct TestCoverageVerifier;

    impl crate::ProducerObservationVerifier for TestCoverageVerifier {
        type Error = &'static str;

        fn verify(
            &self,
            observation: &crate::UntrustedProducerObservation,
        ) -> Result<crate::ProducerObservationClaims, Self::Error> {
            if observation.producer_identity() == *observation.scope_root().as_bytes()
                && observation.context() == *observation.scope_root().as_bytes()
                && observation.evidence() == observation.scope_root().as_bytes()
            {
                Ok(crate::ProducerObservationClaims::new(
                    observation.producer_identity(),
                    observation.scope_root(),
                    observation.context(),
                    *blake3::hash(observation.evidence()).as_bytes(),
                ))
            } else {
                Err("invalid test producer observation")
            }
        }
    }

    fn root_with_rows(count: usize) -> ViewRoot {
        let source = view_state_root(&[]);
        let object = object_version(b"paged-view-source");
        let basis = Basis::new(source, object);
        let rows = (0..count)
            .map(|index| {
                let id = symbol_key(&format!("pkg::{index:08}"));
                Row::new(RowId::Symbol(id), basis, format!("s{index}"))
                    .with_document(Vec::<crate::Fragment>::new())
            })
            .collect();
        ViewRoot::new_checked(
            view_key(b"paged-view"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, source, 0),
            rows,
            vec![Coverage::Complete],
            capability(object),
        )
        .expect("paged view root")
    }

    #[test]
    fn package_labels_keep_first_and_last_without_materializing_rows() {
        let source = view_state_root(&[]);
        let object = object_version(b"package-labels");
        let basis = Basis::new(source, object);
        let package = package_key("pkg:alpha");
        let early = symbol_key("early-label");
        let late = symbol_key("late-label");
        let (first_key, last_key) = if RowId::Symbol(early) < RowId::Symbol(late) {
            (early, late)
        } else {
            (late, early)
        };
        let label = "pkg:alpha::src/lib.rs:1::draw";
        let body = "x".repeat(4096);
        let mut rows = vec![
            Row::in_package(RowId::Symbol(first_key), basis, package, label),
            Row::in_package(RowId::Symbol(last_key), basis, package, label),
        ];
        let sibling = package_key("pkg:beta");
        for index in 0..32 {
            let name = format!("sibling-{index}");
            rows.push(
                Row::in_package(RowId::Symbol(symbol_key(&name)), basis, sibling, name)
                    .with_document(vec![crate::Fragment::Text(body.clone())]),
            );
        }
        let root = ViewRoot::new_checked(
            view_key(b"package-labels"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, source, 0),
            rows,
            vec![Coverage::Complete],
            capability(object),
        )
        .expect("view");
        assert!(root.rows_cache.get().is_none());
        assert_eq!(
            root.first_package_label(package, label),
            Some(RowId::Symbol(first_key))
        );
        assert_eq!(
            root.last_package_label(package, label),
            Some(RowId::Symbol(last_key))
        );
        assert!(root.rows_cache.get().is_none());
        assert!(root.first_package_label(package, "missing").is_none());
        assert!(root.last_package_label(sibling, label).is_none());
    }

    #[test]
    fn page_seek_is_root_bound_and_never_materializes_the_compatibility_slice() {
        let root = root_with_rows(513);
        assert_eq!(root.row_count(), 513);
        assert!(root.rows_cache.get().is_none());
        let first = root
            .page(ViewPageCursor::first(&root), 128)
            .expect("first page");
        assert_eq!(first.rows().len(), 128);
        assert!(first.next().is_some());
        assert!(root.rows_cache.get().is_none());
        let next = root
            .page(first.next().expect("continuation"), 128)
            .expect("next page");
        assert_eq!(next.rows().len(), 128);
        assert!(root.rows_cache.get().is_none());
        assert_eq!(first.rows().last().map(|row| row.id), next.cursor().after());
        assert_eq!(
            root.page(
                ViewPageCursor {
                    recipe: view_key(b"other"),
                    ..first.cursor()
                },
                128,
            ),
            Err(ViewPageError::CursorMismatch)
        );
    }

    #[test]
    fn row_refs_borrow_without_filling_the_owned_cache() {
        let root = root_with_rows(4);
        assert_eq!(root.row_refs().count(), 4);
        assert!(root.rows_cache.get().is_none());
        assert_eq!(root.rows().len(), 4);
        assert!(root.rows_cache.get().is_some());
    }

    fn heavy_root(count: usize) -> ViewRoot {
        let source = view_state_root(&[]);
        let object = object_version(b"label-index");
        let basis = Basis::new(source, object);
        let body = "x".repeat(256);
        let rows = (0..count)
            .map(|index| {
                let label = format!("pkg::item-{index:04}");
                Row::new(RowId::Symbol(symbol_key(&label)), basis, label)
                    .with_document(vec![crate::Fragment::Text(body.clone())])
            })
            .collect();
        ViewRoot::new_checked(
            view_key(b"label-index"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, source, 0),
            rows,
            vec![Coverage::Complete],
            capability(object),
        )
        .expect("label index view")
    }

    #[test]
    fn label_index_keeps_the_earliest_row_and_skips_the_owned_cache() {
        let source = view_state_root(&[]);
        let object = object_version(b"duplicate-label");
        let basis = Basis::new(source, object);
        let first = Row::new(RowId::Symbol(symbol_key("first")), basis, "same");
        let second = Row::new(RowId::Symbol(symbol_key("second")), basis, "same");
        let root = ViewRoot::new_checked(
            view_key(b"duplicate-label"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, source, 0),
            vec![first, second],
            vec![Coverage::Complete],
            capability(object),
        )
        .expect("duplicate labels");
        let indexed = root.row_by_label("same").expect("label");
        let scanned = root
            .rows()
            .iter()
            .find(|row| row.label == "same")
            .expect("scan");
        assert_eq!(indexed.id, scanned.id);
        assert!(root.row_by_label("missing").is_none());
        let borrowed = ViewRoot::new_checked(
            view_key(b"duplicate-label-borrowed"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, source, 1),
            vec![
                Row::new(RowId::Symbol(symbol_key("only")), basis, "only"),
            ],
            vec![Coverage::Complete],
            capability(object),
        )
        .expect("borrowed label");
        assert!(borrowed.row_by_label("only").is_some());
        assert!(borrowed.rows_cache.get().is_none());
    }

    #[test]
    #[allow(clippy::print_stdout)]
    fn label_lookup_skips_cloned_row_bodies() {
        const ROWS: usize = 4096;
        const SAMPLES: usize = 16;
        let needle = format!("pkg::item-{:04}", ROWS - 1);
        let cold_owned: Vec<_> = (0..SAMPLES).map(|_| heavy_root(ROWS)).collect();
        let cold_indexed: Vec<_> = (0..SAMPLES).map(|_| heavy_root(ROWS)).collect();
        let warm_owned = heavy_root(ROWS);
        let warm_indexed = heavy_root(ROWS);
        assert_eq!(
            warm_owned
                .rows()
                .iter()
                .find(|row| row.label == needle)
                .map(|row| row.id),
            warm_indexed.row_by_label(&needle).map(|row| row.id)
        );
        let mut owned_cold = Vec::with_capacity(SAMPLES);
        let mut indexed_cold = Vec::with_capacity(SAMPLES);
        let mut owned_warm = Vec::with_capacity(SAMPLES);
        let mut indexed_warm = Vec::with_capacity(SAMPLES);
        for _ in 0..4 {
            std::hint::black_box(warm_owned.rows().iter().find(|row| row.label == needle));
            std::hint::black_box(warm_indexed.row_by_label(&needle));
        }
        for index in 0..SAMPLES {
            let started = std::time::Instant::now();
            std::hint::black_box(
                cold_owned[index]
                    .rows()
                    .iter()
                    .find(|row| row.label == needle),
            );
            owned_cold.push(started.elapsed().as_nanos());
            let started = std::time::Instant::now();
            std::hint::black_box(cold_indexed[index].row_by_label(&needle));
            indexed_cold.push(started.elapsed().as_nanos());
            let started = std::time::Instant::now();
            std::hint::black_box(warm_owned.rows().iter().find(|row| row.label == needle));
            owned_warm.push(started.elapsed().as_nanos());
            let started = std::time::Instant::now();
            std::hint::black_box(warm_indexed.row_by_label(&needle));
            indexed_warm.push(started.elapsed().as_nanos());
        }
        owned_cold.sort_unstable();
        indexed_cold.sort_unstable();
        owned_warm.sort_unstable();
        indexed_warm.sort_unstable();
        let owned_cold_median = owned_cold[SAMPLES / 2];
        let indexed_cold_median = indexed_cold[SAMPLES / 2];
        let owned_warm_median = owned_warm[SAMPLES / 2];
        let indexed_warm_median = indexed_warm[SAMPLES / 2];
        println!(
            "view_label_index rows={ROWS} cold_owned_median_ns={owned_cold_median} \
             cold_index_median_ns={indexed_cold_median} warm_owned_median_ns={owned_warm_median} \
             warm_index_median_ns={indexed_warm_median}"
        );
        assert!(
            indexed_cold_median < owned_cold_median,
            "cold index {indexed_cold_median} owned {owned_cold_median}"
        );
        assert!(
            indexed_warm_median < owned_warm_median,
            "warm index {indexed_warm_median} owned {owned_warm_median}"
        );
    }

    #[test]
    fn one_hundred_thousand_row_reset_hydrates_bounded_pages_and_rechecks_root() {
        let root = root_with_rows(100_000);
        let descriptor = root.descriptor();
        assert_eq!(descriptor.row_count(), 100_000);
        assert!(root.rows_cache.get().is_none());
        let mut cursor = ViewPageCursor::first(&root);
        let mut hydrated = Vec::with_capacity(
            usize::try_from(descriptor.row_count()).expect("test row count fits usize"),
        );
        let mut pages = 0usize;
        loop {
            let page = root.page(cursor, 256).expect("bounded page");
            assert!(page.rows().len() <= MAX_SNAPSHOT_PAGE_ROWS);
            hydrated.extend_from_slice(page.rows());
            pages += 1;
            match page.next() {
                Some(next) => cursor = next,
                None => break,
            }
        }
        assert_eq!(pages, 391);
        assert_eq!(hydrated.len(), 100_000);
        assert!(root.rows_cache.get().is_none());
        let restored = descriptor
            .admit_rows(hydrated)
            .expect("admit hydrated root");
        assert_eq!(restored.root(), root.root());
        assert_eq!(restored.version(), root.version());
    }

    #[test]
    fn reset_descriptor_rejects_missing_or_mutated_pages() {
        let root = root_with_rows(8);
        let descriptor = root.descriptor();
        let mut page = root.page(ViewPageCursor::first(&root), 8).expect("page");
        let mut rows = page.rows().to_vec();
        rows.pop();
        assert_eq!(
            descriptor.clone().admit_rows(rows),
            Err(ViewError::WrongTarget)
        );
        rows = page.rows().to_vec();
        rows[0].label.push_str(" tampered");
        assert_eq!(descriptor.admit_rows(rows), Err(ViewError::WrongTarget));
        page = root.page(ViewPageCursor::first(&root), 8).expect("page");
        assert!(page.next().is_none());
    }

    fn slice_symbol_commitment(rows: &[Row], claimed: [u8; 32]) -> Option<crate::SymbolKey> {
        rows.binary_search_by(|row| match row.id {
            RowId::Package(_) => Ordering::Less,
            RowId::Symbol(symbol) => symbol.as_bytes().cmp(&claimed),
            RowId::Object(_) => Ordering::Greater,
        })
        .ok()
        .and_then(|index| match rows[index].id {
            RowId::Symbol(symbol) => Some(symbol),
            RowId::Package(_) | RowId::Object(_) => None,
        })
    }

    #[test]
    fn symbol_commitment_seek_matches_a_cloned_row_scan() {
        const ROWS: usize = 4096;
        const SAMPLES: usize = 9;
        let source = view_state_root(&[]);
        let object = object_version(b"symbol-commitment-source");
        let basis = Basis::new(source, object);
        let package = package_key("commitment-package");
        let body = "d".repeat(4096);
        let mut built = Vec::with_capacity(ROWS + 1);
        built.push(Row::new(
            RowId::Package(package),
            basis,
            "commitment-package",
        ));
        for index in 0..ROWS {
            let id = symbol_key(&format!("pkg::{index:08}"));
            built.push(
                Row::new(RowId::Symbol(id), basis, format!("s{index}"))
                    .with_document(vec![crate::Fragment::Text(body.clone())]),
            );
        }
        let root = ViewRoot::new_checked(
            view_key(b"symbol-commitment"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, source, 0),
            built,
            vec![Coverage::Complete],
            capability(object),
        )
        .expect("symbol commitment view");
        assert!(!root.compatibility_rows_are_materialized());
        let mut symbols: Vec<_> = root
            .iter_rows()
            .filter_map(|row| match row.id {
                RowId::Symbol(symbol) => Some(symbol),
                RowId::Package(_) | RowId::Object(_) => None,
            })
            .collect();
        symbols.sort_unstable();
        let claimed = symbols[symbols.len() / 2].to_bytes();
        let mut borrowed = [0_u128; SAMPLES];
        let mut owned = [0_u128; SAMPLES];
        for sample in 0..SAMPLES {
            let started = std::time::Instant::now();
            let hit = root.resolve_symbol_commitment(claimed);
            borrowed[sample] = started.elapsed().as_nanos();
            std::hint::black_box(hit);
            let started = std::time::Instant::now();
            let cloned: Vec<_> = root.iter_rows().cloned().collect();
            let hit = slice_symbol_commitment(&cloned, claimed);
            owned[sample] = started.elapsed().as_nanos();
            std::hint::black_box(hit);
        }
        assert!(!root.compatibility_rows_are_materialized());
        let cloned: Vec<_> = root.iter_rows().cloned().collect();
        for symbol in &symbols {
            let bytes = symbol.to_bytes();
            assert_eq!(
                root.resolve_symbol_commitment(bytes),
                slice_symbol_commitment(&cloned, bytes)
            );
            assert_eq!(root.resolve_symbol_commitment(bytes), Some(*symbol));
        }
        let mut missing = claimed;
        missing[31] ^= 0xff;
        assert_eq!(
            root.resolve_symbol_commitment(missing),
            slice_symbol_commitment(&cloned, missing)
        );
        assert_eq!(
            root.resolve_symbol_commitment(package.to_bytes()),
            slice_symbol_commitment(&cloned, package.to_bytes())
        );
        assert_eq!(
            root.resolve_symbol_commitment(claimed),
            slice_symbol_commitment(root.rows(), claimed)
        );
        borrowed.sort_unstable();
        owned.sort_unstable();
        let borrowed_median = borrowed[SAMPLES / 2];
        let owned_median = owned[SAMPLES / 2];
        eprintln!(
            "symbol_commitment_seek rows={ROWS} owned_median_ns={owned_median} \
             borrowed_median_ns={borrowed_median}"
        );
        assert!(
            borrowed_median.saturating_mul(32) < owned_median,
            "borrowed seek {borrowed_median} ns was not 32× faster than cloning every row \
             {owned_median} ns"
        );
    }
}
