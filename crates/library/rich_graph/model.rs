//! Rich graph fact payloads, provenance, and deterministic layout input.

use super::identity::graph_authority_for_dependency;
use super::{
    GraphAuthority, GraphEdgeId, GraphEdgeKind, GraphNodeId, GraphRelationFamily, RichGraphError,
};
use crate::{
    GraphRelation, PackageDependencyRecord, ProductText, SemanticConfidence, SemanticGenerationId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;

/// Immutable evidence attached to a node or edge.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphProvenance {
    /// Semantic generation binding, when this fact is compiler-backed.
    pub semantic_generation: Option<SemanticGenerationId>,
    /// Exact semantic generation root used for the fact.
    pub semantic_root: Option<[u8; 32]>,
    /// Authority class which supplied the fact.
    pub authority: GraphAuthority,
    /// Digest of the source image, manifest, or registry row.
    pub source: [u8; 32],
    /// Confidence supplied by a semantic authority, when applicable.
    pub confidence: Option<SemanticConfidence>,
}

impl GraphProvenance {
    /// Creates compiler provenance bound to an exact semantic generation.
    #[must_use]
    pub const fn semantic(
        generation: SemanticGenerationId,
        semantic_root: [u8; 32],
        source: [u8; 32],
        confidence: SemanticConfidence,
    ) -> Self {
        Self {
            semantic_generation: Some(generation),
            semantic_root: Some(semantic_root),
            authority: GraphAuthority::SemanticGeneration,
            source,
            confidence: Some(confidence),
        }
    }

    /// Creates package metadata provenance without inventing a compiler root.
    #[must_use]
    pub const fn package(authority: GraphAuthority, source: [u8; 32]) -> Self {
        Self {
            semantic_generation: None,
            semantic_root: None,
            authority,
            source,
            confidence: None,
        }
    }
}

/// Honest availability of one graph payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "kebab-case")]
pub enum GraphAvailability {
    /// All requested facts were available.
    Ready,
    /// The response was bounded and omitted this many facts.
    Partial {
        /// Number of omitted facts.
        omitted: u32,
    },
    /// The owner announced a fact but its payload is still arriving.
    Loading,
    /// The configured authority cannot answer this fact.
    Unavailable(ProductText),
    /// The authority answered with a typed failure.
    Failed(ProductText),
}

impl GraphAvailability {
    /// Creates a bounded unavailable reason.
    pub fn unavailable(reason: impl Into<String>) -> Result<Self, RichGraphError> {
        Ok(Self::Unavailable(
            ProductText::new(reason).map_err(RichGraphError::Text)?,
        ))
    }

    /// Creates a bounded failure reason.
    pub fn failed(reason: impl Into<String>) -> Result<Self, RichGraphError> {
        Ok(Self::Failed(
            ProductText::new(reason).map_err(RichGraphError::Text)?,
        ))
    }
}

/// One stable rich graph node.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RichGraphNode {
    /// Stable content-derived node identity.
    pub id: GraphNodeId,
    /// Bounded display label.
    pub label: ProductText,
    /// Optional exact coordinate used for navigation.
    pub coordinate: Option<ProductText>,
    /// Stable declaration-kind spelling supplied by the producer.
    pub kind: Option<ProductText>,
    /// Honest payload availability.
    pub availability: GraphAvailability,
    /// Source authority and exact generation evidence.
    pub provenance: Option<GraphProvenance>,
}

impl RichGraphNode {
    /// Creates one graph node.
    #[must_use]
    pub fn new(
        id: GraphNodeId,
        label: impl Into<String>,
        coordinate: Option<String>,
        kind: Option<String>,
        availability: GraphAvailability,
        provenance: Option<GraphProvenance>,
    ) -> Result<Self, RichGraphError> {
        Ok(Self {
            id,
            label: ProductText::new(label).map_err(RichGraphError::Text)?,
            coordinate: coordinate
                .map(ProductText::new)
                .transpose()
                .map_err(RichGraphError::Text)?,
            kind: kind
                .map(ProductText::new)
                .transpose()
                .map_err(RichGraphError::Text)?,
            availability,
            provenance,
        })
    }
}

/// One rich graph edge with evidence and availability.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RichGraphEdge {
    /// Stable endpoint-and-kind identity.
    pub id: GraphEdgeId,
    /// Directed source node.
    pub from: GraphNodeId,
    /// Directed target node.
    pub to: GraphNodeId,
    /// Typed relation family and kind.
    pub kind: GraphEdgeKind,
    /// Honest edge availability.
    pub availability: GraphAvailability,
    /// Source authority and exact generation evidence.
    pub provenance: GraphProvenance,
    /// Package resolver requirement, when this is a dependency edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency_requirement: Option<ProductText>,
}

impl RichGraphEdge {
    /// Creates one edge and derives its stable identity.
    #[must_use]
    pub fn new(
        from: GraphNodeId,
        to: GraphNodeId,
        kind: GraphEdgeKind,
        availability: GraphAvailability,
        provenance: GraphProvenance,
    ) -> Self {
        Self {
            id: GraphEdgeId::derive(from, to, kind),
            from,
            to,
            kind,
            availability,
            provenance,
            dependency_requirement: None,
        }
    }

    fn with_dependency_requirement(
        from: GraphNodeId,
        to: GraphNodeId,
        kind: GraphEdgeKind,
        availability: GraphAvailability,
        provenance: GraphProvenance,
        requirement: ProductText,
    ) -> Self {
        let dependency_requirement = Some(requirement);
        Self {
            id: GraphEdgeId::derive_with_detail(
                from,
                to,
                kind,
                dependency_requirement.as_ref().map(ProductText::as_str),
            ),
            from,
            to,
            kind,
            availability,
            provenance,
            dependency_requirement,
        }
    }

    /// Converts an existing compiler sidecar edge into rich graph form.
    #[must_use]
    pub fn from_relation(relation: GraphRelation, provenance: GraphProvenance) -> Self {
        Self::new(
            GraphNodeId::for_row(relation.from),
            GraphNodeId::for_row(relation.to),
            GraphEdgeKind::Code {
                relation: relation.relation,
            },
            GraphAvailability::Ready,
            provenance,
        )
    }

    /// Converts one package dependency record into a directed edge.
    #[must_use]
    pub fn from_dependency(record: &PackageDependencyRecord, family: GraphRelationFamily) -> Self {
        let source = GraphNodeId::for_package(&record.source);
        let target = record.target.resolved.as_ref().map_or_else(
            || {
                GraphNodeId::for_unresolved_package(
                    &record.target,
                    graph_authority_for_dependency(record.evidence.authority),
                )
            },
            GraphNodeId::for_package,
        );
        let (from, to) = match family {
            GraphRelationFamily::Dependent => (target, source),
            _ => (source, target),
        };
        let kind = match family {
            GraphRelationFamily::Dependency => GraphEdgeKind::Dependency {
                scope: record.scope,
                optional: record.optional,
            },
            GraphRelationFamily::Dependent => GraphEdgeKind::Dependent {
                scope: record.scope,
                optional: record.optional,
            },
            GraphRelationFamily::Code => GraphEdgeKind::Dependency {
                scope: record.scope,
                optional: record.optional,
            },
        };
        Self::with_dependency_requirement(
            from,
            to,
            kind,
            GraphAvailability::Ready,
            GraphProvenance::package(
                graph_authority_for_dependency(record.evidence.authority),
                record.evidence.provenance,
            ),
            record.target.requirement.clone(),
        )
    }
}

/// Exact roots that must agree before a graph can be queried or applied.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RichGraphRevision {
    /// Immutable visible view root.
    pub view_root: [u8; 32],
    /// Exact compiler binding identity.
    pub semantic_generation: SemanticGenerationId,
    /// Exact semantic generation root selected by the compiler journal.
    pub semantic_root: [u8; 32],
}

impl RichGraphRevision {
    /// Creates an exact view/compiler revision pair.
    #[must_use]
    pub const fn new(
        view_root: [u8; 32],
        semantic_generation: SemanticGenerationId,
        semantic_root: [u8; 32],
    ) -> Self {
        Self {
            view_root,
            semantic_generation,
            semantic_root,
        }
    }
}

/// A deterministic layout input.  No screen coordinates are part of the
/// contract; renderers can choose geometry while receiving the same ordering.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphLayoutInput {
    /// Revision used to derive the layout seed.
    pub revision: RichGraphRevision,
    /// Center node for breadth-first or radial layouts.
    pub center: GraphNodeId,
    /// Canonically ordered node identities.
    pub nodes: Box<[GraphNodeId]>,
    /// Canonically ordered endpoint identities.
    pub edges: Box<[GraphLayoutEdge]>,
    /// Stable seed for a renderer's deterministic placement.
    pub seed: [u8; 32],
}

/// Endpoint-only layout edge input.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphLayoutEdge {
    /// Stable edge identity.
    pub id: GraphEdgeId,
    /// Source endpoint.
    pub from: GraphNodeId,
    /// Target endpoint.
    pub to: GraphNodeId,
}

impl GraphLayoutInput {
    pub(super) fn new(
        revision: RichGraphRevision,
        center: GraphNodeId,
        nodes: &[RichGraphNode],
        edges: &[RichGraphEdge],
    ) -> Self {
        let node_ids = nodes.iter().map(|node| node.id).collect::<BTreeSet<_>>();
        let nodes = node_ids
            .iter()
            .copied()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let mut layout_edges = edges
            .iter()
            .map(|edge| GraphLayoutEdge {
                id: edge.id,
                from: edge.from,
                to: edge.to,
            })
            .collect::<Vec<_>>();
        layout_edges.sort_unstable();
        let mut preimage = Vec::with_capacity(32 + 32 + 32 * nodes.len());
        preimage.extend_from_slice(&revision.view_root);
        preimage.extend_from_slice(&revision.semantic_generation.to_bytes());
        preimage.extend_from_slice(&revision.semantic_root);
        preimage.extend_from_slice(center.as_bytes());
        for id in &nodes {
            preimage.extend_from_slice(id.as_bytes());
        }
        for edge in &layout_edges {
            preimage.extend_from_slice(edge.id.as_bytes());
            preimage.extend_from_slice(edge.from.as_bytes());
            preimage.extend_from_slice(edge.to.as_bytes());
        }
        let seed = *blake3::hash(&preimage).as_bytes();
        Self {
            revision,
            center,
            nodes,
            edges: layout_edges.into_boxed_slice(),
            seed,
        }
    }
}

/// A root-pinned, immutable rich graph snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RichGraphSnapshot {
    /// Contract schema version.
    pub schema: u16,
    /// Exact visible and semantic roots.
    pub revision: RichGraphRevision,
    /// Node selected by the graph query.
    pub center: GraphNodeId,
    /// Canonically ordered nodes in a shared packed backing array.
    pub nodes: Arc<[RichGraphNode]>,
    /// Canonically ordered edges in a shared packed backing array.
    pub edges: Arc<[RichGraphEdge]>,
    /// Honest completeness of the authoritative result.
    pub availability: GraphAvailability,
    /// Deterministic renderer input derived from the graph facts.
    pub layout: GraphLayoutInput,
}
