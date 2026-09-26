//! Persistent immutable view root and canonical relation state.

use super::{
    Basis, Coverage, CoverageCapability, Lane, Reason, Row, RowId, RowIdentityPreimage, ViewError,
    ViewPageCursor, ViewPageError, ViewRootDescriptor, ViewSnapshotPage,
};
use crate::canonical::{
    Frontier, ViewEntry, ViewEntryKey, ViewMetadata, ViewRecipeId, ViewStateRoot, ViewVersion,
    package_key, symbol_key, view_version_preimage,
};
use backend_version::{
    Coverage as BackendCoverage, CoverageWitness, RelationState, ScopeRoot, UntrustedCoverageScope,
};
use std::cmp::Ordering;
use std::ops::Bound;
use std::sync::{Arc, OnceLock};

mod admit;

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

    #[cfg(test)]
    pub(crate) fn compatibility_rows_are_materialized(&self) -> bool {
        self.rows_cache.get().is_some()
    }

    /// Looks up one row by stable identity without materializing the complete
    /// compatibility slice.
    #[must_use]
    pub fn row(&self, id: RowId) -> Option<Row> {
        self.row_ref(id).cloned()
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

    /// Resolves an opaque symbol selector by membership in this exact view.
    ///
    /// The claimed digest is never promoted directly. The canonical row
    /// slice is ordered by [`RowId`], so the lookup borrows the already typed
    /// key from a matching row in logarithmic time.
    #[must_use]
    pub fn resolve_symbol_commitment(&self, claimed: [u8; 32]) -> Option<crate::SymbolKey> {
        self.rows()
            .binary_search_by(|row| match row.id {
                RowId::Package(_) => Ordering::Less,
                RowId::Symbol(symbol) => symbol.as_bytes().cmp(&claimed),
                RowId::Object(_) => Ordering::Greater,
            })
            .ok()
            .and_then(|index| match self.rows()[index].id {
                RowId::Symbol(symbol) => Some(symbol),
                RowId::Package(_) | RowId::Object(_) => None,
            })
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
            coverage,
            capability: None,
            relation,
        }
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
}
