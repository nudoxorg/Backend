use super::work::OccurrenceWorkCounters;
use crate::canonical::canonical_edge;
use crate::schema::{
    EdgeSchema, EdgeValueSchema, OccurrenceSchema, encode_edge_schema, encode_edge_value_schema,
    encode_occurrence_schema, encode_occurrence_value_schema,
};
use crate::{
    Edge, EdgeKey, EdgeKind, FacetCoverage, OccurrenceAddress, OccurrenceFact, SemanticError,
};
use backend_version::{PersistentTree, Relation, Schema, StateError, TreeWork};
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

impl backend_flow::CanonicalValue for Edge {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        // Flow arrangements use one generic relation family for all semantic
        // values. Retain both semantic key/value schema contexts before the
        // complete edge preimage so a value from another schema cannot share
        // this arrangement identity, while all key and evidence fields remain
        // part of the preimage.
        out.push(EdgeSchema::DOMAIN);
        out.extend_from_slice(&EdgeSchema::TYPE.to_be_bytes());
        out.push(EdgeSchema::VERSION);
        out.push(EdgeValueSchema::DOMAIN);
        out.extend_from_slice(&EdgeValueSchema::TYPE.to_be_bytes());
        out.push(EdgeValueSchema::VERSION);
        out.extend_from_slice(&canonical_edge(self));
    }

    fn encode_ordered(&self, out: &mut Vec<u8>) {
        // Edge derives lexicographic `Ord` in exactly the field order used by
        // this fixed-width encoding: from, to, kind, multiplicity, support.
        self.encode_canonical(out);
    }
}

/// Private relation schema for retained weighted occurrence rows.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct OccurrenceRows;

impl Relation for OccurrenceRows {
    const DOMAIN: u8 = OccurrenceSchema::DOMAIN;
    const TYPE: u16 = OccurrenceSchema::TYPE | 0x8000;
    type Key = OccurrenceAddress;
    type Value = (OccurrenceFact, i64);

    fn encode_key(key: &Self::Key, out: &mut Vec<u8>) {
        encode_occurrence_schema(key, out);
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        encode_occurrence_value_schema(&value.0, out);
        out.extend_from_slice(&value.1.to_be_bytes());
    }
}

/// Typed edge relation retained by the same persistent object kernel as
/// occurrence rows.  This removes the former bespoke AVL implementation and
/// gives edge roots identical canonical commitments and range traversal.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct EdgeRows;

impl Relation for EdgeRows {
    const DOMAIN: u8 = EdgeSchema::DOMAIN;
    const TYPE: u16 = EdgeSchema::TYPE | 0x8000;
    type Key = EdgeKey;
    type Value = Edge;

    fn encode_key(key: &Self::Key, out: &mut Vec<u8>) {
        encode_edge_schema(key, out);
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        encode_edge_value_schema(value, out);
    }
}

pub(super) type RowState = backend_version::RelationState<OccurrenceRows>;
type EdgeTree = PersistentTree<EdgeRows>;

pub(super) fn map_row_state_error(_error: StateError) -> SemanticError {
    SemanticError::Overflow
}

pub(super) fn map_row_delta_error(_error: backend_version::DeltaError) -> SemanticError {
    SemanticError::Overflow
}

/// Immutable, typed edge arrangement derived from occurrence evidence.
///
/// The arrangement is backed by the shared versioned persistent relation
/// kernel. Cloning an edge set retains its canonical root; an update copies
/// only the affected search paths.
#[derive(Debug)]
pub struct EdgeSet {
    /// Coverage inherited from occurrence authority.
    coverage: FacetCoverage,
    /// Immutable ordered edge tree.  Roots are path-copied on updates.
    pub(super) tree: EdgeTree,
    len: usize,
    /// Compatibility cache for callers requesting the complete sorted slice.
    edges: Arc<OnceLock<Vec<Edge>>>,
}

impl Clone for EdgeSet {
    fn clone(&self) -> Self {
        Self {
            coverage: self.coverage,
            tree: self.tree.clone(),
            len: self.len,
            edges: Arc::clone(&self.edges),
        }
    }
}

impl PartialEq for EdgeSet {
    fn eq(&self, other: &Self) -> bool {
        self.coverage == other.coverage
            && self.tree.root().commitment() == other.tree.root().commitment()
    }
}

impl Eq for EdgeSet {}

impl EdgeSet {
    pub(super) fn from_sorted_edges(
        coverage: FacetCoverage,
        edges: &[(EdgeKey, Edge)],
    ) -> Result<Self, SemanticError> {
        Ok(Self {
            coverage,
            tree: PersistentTree::from_sorted_items(edges).map_err(|_| SemanticError::Overflow)?,
            len: edges.len(),
            edges: Arc::new(OnceLock::new()),
        })
    }

    /// Derives edge membership/support from a complete occurrence collection.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidSupport`] for zero weights,
    /// [`SemanticError::DuplicateFact`] for repeated occurrence addresses, or
    /// [`SemanticError::Overflow`] when derived support exceeds its schema.
    pub fn from_occurrences(
        coverage: FacetCoverage,
        mut occurrences: Vec<OccurrenceFact>,
    ) -> Result<Self, SemanticError> {
        if coverage.deletion() != crate::Deletion::Live && !occurrences.is_empty() {
            return Err(SemanticError::InvalidCoverageState);
        }
        if occurrences
            .iter()
            .any(|occurrence| occurrence.multiplicity == 0 || occurrence.support == 0)
        {
            return Err(SemanticError::InvalidSupport);
        }
        occurrences.sort();
        if occurrences
            .windows(2)
            .any(|window| window[0].address() == window[1].address())
        {
            return Err(SemanticError::DuplicateFact);
        }
        let mut supports: BTreeMap<EdgeKey, (u32, u32)> = BTreeMap::new();
        for occurrence in occurrences {
            let entry = supports.entry(EdgeKey {
                from: occurrence.source,
                to: occurrence.target,
                kind: EdgeKind::Reference,
            });
            let value = entry.or_insert((0, 0));
            value.0 = value
                .0
                .checked_add(occurrence.multiplicity)
                .ok_or(SemanticError::Overflow)?;
            value.1 = value
                .1
                .checked_add(u32::from(occurrence.support))
                .ok_or(SemanticError::Overflow)?;
        }
        let mut edges = Vec::with_capacity(supports.len());
        for (key, (multiplicity, support)) in supports {
            edges.push(Edge {
                from: key.from,
                to: key.to,
                kind: key.kind,
                multiplicity,
                support: u16::try_from(support).map_err(|_| SemanticError::Overflow)?,
            });
        }
        let keyed = edges
            .into_iter()
            .map(|edge| (edge.key(), edge))
            .collect::<Vec<_>>();
        Self::from_sorted_edges(coverage, &keyed)
    }

    /// Returns the edge for a logical key, if support survives.
    #[must_use]
    pub fn get(&self, key: EdgeKey) -> Option<&Edge> {
        self.tree.get(&key)
    }

    /// Returns the derived edge coverage.
    #[must_use]
    pub const fn coverage(&self) -> FacetCoverage {
        self.coverage
    }

    /// Returns the canonically sorted derived edges.
    #[must_use]
    pub fn edges(&self) -> &[Edge] {
        self.edges.get_or_init(|| {
            let mut edges = Vec::new();
            edges.extend(self.tree.iter().map(|(_, edge)| edge.clone()));
            edges
        })
    }

    /// Traverses only the requested canonical edge-key interval.  The
    /// persistent kernel seeks to the lower bound by one root-to-leaf path,
    /// avoiding a full materialization of the edge arrangement.
    #[must_use = "iterate the selected edge range"]
    pub fn range<B: std::ops::RangeBounds<EdgeKey>>(
        &self,
        bounds: B,
    ) -> impl Iterator<Item = (&EdgeKey, &Edge)> {
        self.tree.range(bounds)
    }

    /// Returns the number of supported logical edges.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns whether no supported logical edge remains.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(super) fn apply_changes(
        &self,
        coverage: FacetCoverage,
        changes: &BTreeMap<EdgeKey, Option<Edge>>,
        work: &mut OccurrenceWorkCounters,
    ) -> Result<Self, SemanticError> {
        if changes.is_empty() {
            return Ok(Self {
                coverage,
                tree: self.tree.clone(),
                len: self.len,
                edges: Arc::clone(&self.edges),
            });
        }
        let tree_changes = changes
            .iter()
            .map(|(key, edge)| backend_version::TreeChange {
                key: *key,
                after: edge.clone(),
            })
            .collect::<Vec<_>>();
        let mut len = self.len;
        for (key, after) in changes {
            let was_present = self.tree.get(key).is_some();
            match (was_present, after.is_some()) {
                (false, true) => len = len.checked_add(1).ok_or(SemanticError::Overflow)?,
                (true, false) => len = len.checked_sub(1).ok_or(SemanticError::Overflow)?,
                _ => {}
            }
        }
        let prepared = self
            .tree
            .prepare_update(&tree_changes)
            .map_err(|_| SemanticError::Overflow)?;
        let tree_work: TreeWork = prepared.work();
        work.edge_probes = work
            .edge_probes
            .checked_add(
                u64::try_from(tree_work.visited_nodes).map_err(|_| SemanticError::Overflow)?,
            )
            .ok_or(SemanticError::Overflow)?;
        work.edge_nodes = work
            .edge_nodes
            .checked_add(
                u64::try_from(tree_work.copied_nodes).map_err(|_| SemanticError::Overflow)?,
            )
            .ok_or(SemanticError::Overflow)?;
        Ok(Self {
            coverage,
            tree: prepared.commit(),
            len,
            edges: Arc::new(OnceLock::new()),
        })
    }

    #[cfg(test)]
    pub(crate) fn root_is_shared_with(&self, other: &Self) -> bool {
        self.tree.root_handle().id() == other.tree.root_handle().id()
    }
}
