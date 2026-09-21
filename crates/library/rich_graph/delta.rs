//! Bounded graph deltas and exact-root reconstruction.

use super::{
    GraphAvailability, GraphEdgeId, GraphLayoutInput, GraphNodeId, MAX_RICH_GRAPH_DELTA_RECORDS,
    RICH_GRAPH_SCHEMA_VERSION, RichGraphEdge, RichGraphError, RichGraphNode, RichGraphRevision,
    RichGraphSnapshot,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A bounded set of changes between two graph roots.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RichGraphDelta {
    /// Base graph revision required by this delta.
    pub base: RichGraphRevision,
    /// Target graph revision produced by this delta.
    pub target: RichGraphRevision,
    /// Target center node.
    pub center: GraphNodeId,
    /// Newly present nodes.
    pub added_nodes: Box<[RichGraphNode]>,
    /// Changed node payloads.
    pub updated_nodes: Box<[RichGraphNode]>,
    /// Removed node identities.
    pub removed_nodes: Box<[GraphNodeId]>,
    /// Newly present edges.
    pub added_edges: Box<[RichGraphEdge]>,
    /// Changed edge payloads.
    pub updated_edges: Box<[RichGraphEdge]>,
    /// Removed edge identities.
    pub removed_edges: Box<[GraphEdgeId]>,
    /// Target availability state.
    pub availability: GraphAvailability,
}

fn diff_records<T, K, F>(
    before: &[T],
    after: &[T],
    key: F,
) -> Result<(Vec<T>, Vec<T>, Vec<K>), RichGraphError>
where
    T: Clone + Eq,
    K: Copy + Ord,
    F: Fn(&T) -> K,
{
    let mut added = Vec::new();
    let mut updated = Vec::new();
    let mut removed = Vec::new();
    let mut before_index = 0;
    let mut after_index = 0;
    while before_index < before.len() || after_index < after.len() {
        match (before.get(before_index), after.get(after_index)) {
            (None, None) => break,
            (None, Some(after_value)) => {
                if added.len() + updated.len() + removed.len() >= MAX_RICH_GRAPH_DELTA_RECORDS {
                    return Err(RichGraphError::DeltaBound);
                }
                added.push(after_value.clone());
                after_index += 1;
            }
            (Some(before_value), None) => {
                if added.len() + updated.len() + removed.len() >= MAX_RICH_GRAPH_DELTA_RECORDS {
                    return Err(RichGraphError::DeltaBound);
                }
                removed.push(key(before_value));
                before_index += 1;
            }
            (Some(before_value), Some(after_value)) => {
                match key(before_value).cmp(&key(after_value)) {
                    std::cmp::Ordering::Less => {
                        if added.len() + updated.len() + removed.len()
                            >= MAX_RICH_GRAPH_DELTA_RECORDS
                        {
                            return Err(RichGraphError::DeltaBound);
                        }
                        removed.push(key(before_value));
                        before_index += 1;
                    }
                    std::cmp::Ordering::Equal => {
                        if before_value != after_value {
                            if added.len() + updated.len() + removed.len()
                                >= MAX_RICH_GRAPH_DELTA_RECORDS
                            {
                                return Err(RichGraphError::DeltaBound);
                            }
                            updated.push(after_value.clone());
                        }
                        before_index += 1;
                        after_index += 1;
                    }
                    std::cmp::Ordering::Greater => {
                        if added.len() + updated.len() + removed.len()
                            >= MAX_RICH_GRAPH_DELTA_RECORDS
                        {
                            return Err(RichGraphError::DeltaBound);
                        }
                        added.push(after_value.clone());
                        after_index += 1;
                    }
                }
            }
        }
        if added.len() + updated.len() + removed.len() > MAX_RICH_GRAPH_DELTA_RECORDS {
            return Err(RichGraphError::DeltaBound);
        }
    }
    Ok((added, updated, removed))
}

impl RichGraphDelta {
    /// Checks the fixed work bound before a client applies this delta.
    pub fn admit(&self) -> Result<(), RichGraphError> {
        let total = self.added_nodes.len()
            + self.updated_nodes.len()
            + self.removed_nodes.len()
            + self.added_edges.len()
            + self.updated_edges.len()
            + self.removed_edges.len();
        if total > MAX_RICH_GRAPH_DELTA_RECORDS {
            return Err(RichGraphError::DeltaBound);
        }
        if !self
            .added_nodes
            .windows(2)
            .all(|pair| pair[0].id < pair[1].id)
            || !self
                .updated_nodes
                .windows(2)
                .all(|pair| pair[0].id < pair[1].id)
            || !self.removed_nodes.windows(2).all(|pair| pair[0] < pair[1])
            || !self
                .added_edges
                .windows(2)
                .all(|pair| pair[0].id < pair[1].id)
            || !self
                .updated_edges
                .windows(2)
                .all(|pair| pair[0].id < pair[1].id)
            || !self.removed_edges.windows(2).all(|pair| pair[0] < pair[1])
        {
            return Err(RichGraphError::Unordered);
        }
        Ok(())
    }
}

impl RichGraphSnapshot {
    pub fn diff(&self, next: &Self) -> Result<RichGraphDelta, RichGraphError> {
        self.admit()?;
        next.admit()?;
        let (added_nodes, updated_nodes, removed_nodes) =
            diff_records(&self.nodes, &next.nodes, |node| node.id)?;
        let (added_edges, updated_edges, removed_edges) =
            diff_records(&self.edges, &next.edges, |edge| edge.id)?;
        Ok(RichGraphDelta {
            base: self.revision,
            target: next.revision,
            center: next.center,
            added_nodes: added_nodes.into_boxed_slice(),
            updated_nodes: updated_nodes.into_boxed_slice(),
            removed_nodes: removed_nodes.into_boxed_slice(),
            added_edges: added_edges.into_boxed_slice(),
            updated_edges: updated_edges.into_boxed_slice(),
            removed_edges: removed_edges.into_boxed_slice(),
            availability: next.availability.clone(),
        })
    }

    /// Applies a delta only when its base is this exact graph revision.
    pub fn apply_delta(&self, delta: &RichGraphDelta) -> Result<Self, RichGraphError> {
        self.admit()?;
        if delta.base != self.revision {
            return Err(RichGraphError::StaleRoot);
        }
        delta.admit()?;
        let mut nodes = self
            .nodes
            .iter()
            .cloned()
            .map(|node| (node.id, node))
            .collect::<BTreeMap<_, _>>();
        let mut edges = self
            .edges
            .iter()
            .cloned()
            .map(|edge| (edge.id, edge))
            .collect::<BTreeMap<_, _>>();
        for id in &delta.removed_nodes {
            nodes.remove(id);
            edges.retain(|_, edge| edge.from != *id && edge.to != *id);
        }
        for id in &delta.removed_edges {
            edges.remove(id);
        }
        for node in delta.updated_nodes.iter().chain(delta.added_nodes.iter()) {
            nodes.insert(node.id, node.clone());
        }
        for edge in delta.updated_edges.iter().chain(delta.added_edges.iter()) {
            edges.insert(edge.id, edge.clone());
        }
        let nodes = nodes.into_values().collect::<Vec<_>>();
        let edges = edges.into_values().collect::<Vec<_>>();
        let snapshot = Self {
            schema: RICH_GRAPH_SCHEMA_VERSION,
            revision: delta.target,
            center: delta.center,
            layout: GraphLayoutInput::new(delta.target, delta.center, &nodes, &edges),
            nodes: Arc::from(nodes.into_boxed_slice()),
            edges: Arc::from(edges.into_boxed_slice()),
            availability: delta.availability.clone(),
        };
        snapshot.admit()?;
        Ok(snapshot)
    }
}
