//! Graph construction from admitted nodes and compiler view relations.

use super::{
    GraphAvailability, GraphEdgeId, GraphLayoutInput, GraphNodeId, GraphProvenance,
    RICH_GRAPH_SCHEMA_VERSION, RichGraphEdge, RichGraphError, RichGraphNode, RichGraphRevision,
    RichGraphSnapshot,
};
use crate::{GraphRelation, ProductText, RowId, SemanticConfidence, ViewRoot};
use std::collections::BTreeMap;
use std::sync::Arc;

impl RichGraphSnapshot {
    /// Builds a graph from a view root and typed compiler relation sidecar.
    pub fn from_view(
        root: &ViewRoot,
        center: RowId,
        relations: &[GraphRelation],
        revision: RichGraphRevision,
    ) -> Result<Self, RichGraphError> {
        if revision.view_root != root.root().to_bytes() {
            return Err(RichGraphError::StaleRoot);
        }
        let provenance = GraphProvenance::semantic(
            revision.semantic_generation,
            revision.semantic_root,
            revision.semantic_root,
            SemanticConfidence::Compiler,
        );
        let mut builder = RichGraphBuilder::new(revision, GraphNodeId::for_row(center));
        for row in root.rows() {
            let availability = match row.state {
                crate::RowState::Ready => GraphAvailability::Ready,
                crate::RowState::Loading => GraphAvailability::Loading,
                crate::RowState::Failed => GraphAvailability::failed("row failed")?,
            };
            builder.add_node(RichGraphNode::new(
                GraphNodeId::for_row(row.id),
                row.label.clone(),
                Some(row.label.clone()),
                row.kind.map(|kind| format!("{kind:?}")),
                availability,
                Some(provenance),
            )?)?;
        }
        for relation in relations {
            builder.add_edge(RichGraphEdge::from_relation(*relation, provenance))?;
        }
        builder.finish()
    }
}

/// Builder enforcing graph node/edge and identity bounds before publication.
pub struct RichGraphBuilder {
    revision: RichGraphRevision,
    center: GraphNodeId,
    nodes: BTreeMap<GraphNodeId, RichGraphNode>,
    edges: BTreeMap<GraphEdgeId, RichGraphEdge>,
}

impl RichGraphBuilder {
    /// Creates an empty bounded graph builder.
    #[must_use]
    pub fn new(revision: RichGraphRevision, center: GraphNodeId) -> Self {
        Self {
            revision,
            center,
            nodes: BTreeMap::new(),
            edges: BTreeMap::new(),
        }
    }

    /// Adds a node, rejecting duplicate identities with different payloads.
    pub fn add_node(&mut self, node: RichGraphNode) -> Result<(), RichGraphError> {
        if let Some(previous) = self.nodes.get(&node.id) {
            return if previous == &node {
                Err(RichGraphError::Duplicate)
            } else {
                Err(RichGraphError::IdentityCollision)
            };
        }
        self.nodes.insert(node.id, node);
        Ok(())
    }

    /// Adds an edge, rejecting duplicate identities and unknown endpoints.
    pub fn add_edge(&mut self, edge: RichGraphEdge) -> Result<(), RichGraphError> {
        if !self.nodes.contains_key(&edge.from) || !self.nodes.contains_key(&edge.to) {
            return Err(RichGraphError::EdgeShape);
        }
        if edge.id
            != GraphEdgeId::derive_with_detail(
                edge.from,
                edge.to,
                edge.kind,
                edge.dependency_requirement
                    .as_ref()
                    .map(ProductText::as_str),
            )
        {
            return Err(RichGraphError::EdgeShape);
        }
        if self.edges.contains_key(&edge.id) {
            return Err(RichGraphError::Duplicate);
        }
        self.edges.insert(edge.id, edge);
        Ok(())
    }

    /// Publishes the checked immutable graph snapshot.
    pub fn finish(self) -> Result<RichGraphSnapshot, RichGraphError> {
        if !self.nodes.contains_key(&self.center) {
            return Err(RichGraphError::CenterMissing);
        }
        let nodes = self.nodes.into_values().collect::<Vec<_>>();
        let edges = self.edges.into_values().collect::<Vec<_>>();
        let snapshot = RichGraphSnapshot {
            schema: RICH_GRAPH_SCHEMA_VERSION,
            revision: self.revision,
            center: self.center,
            layout: GraphLayoutInput::new(self.revision, self.center, &nodes, &edges),
            nodes: Arc::from(nodes.into_boxed_slice()),
            edges: Arc::from(edges.into_boxed_slice()),
            availability: GraphAvailability::Ready,
        };
        snapshot.admit()?;
        Ok(snapshot)
    }
}
