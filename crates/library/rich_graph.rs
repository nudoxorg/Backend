//! Versioned rich semantic graph facts shared by every local surface.
//!
//! The graph renderer is deliberately not the owner of this module.  A graph
//! is an immutable, root-pinned projection of compiler and package evidence;
//! desktop, CLI, and MCP receive the same nodes, edges, provenance, and
//! availability states.  Identities are content derived and independent of
//! display labels, while the bounded delta and page operations make refreshes
//! safe for a long-lived client.

use crate::{
    DependencyScope, GraphRelation, PackageDependencyRecord, ProductAdmissionError, ProductText,
    RowId, SemanticConfidence, SemanticGenerationId, SemanticLinkKind, ViewRoot,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

/// Wire and content schema for the rich graph contract.
pub const RICH_GRAPH_SCHEMA_VERSION: u16 = 1;
/// Maximum records changed by one graph delta.
pub const MAX_RICH_GRAPH_DELTA_RECORDS: usize = 1_024;
/// Maximum nodes returned by one graph page.
pub const MAX_RICH_GRAPH_PAGE_ROWS: u16 = 200;
/// Maximum endpoint edges returned by one graph page.
pub const MAX_RICH_GRAPH_PAGE_EDGES: usize = 1_024;

/// A stable graph node identity derived from canonical identity material.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GraphNodeId([u8; 32]);

impl GraphNodeId {
    /// Creates an identity from already admitted bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the fixed-width identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Derives a node identity from one visible row identity.
    #[must_use]
    pub fn for_row(row: RowId) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.rich-ir.graph.node.row.v1\0");
        hasher.update(row.stable_key().as_bytes());
        Self(*hasher.finalize().as_bytes())
    }

    /// Derives a node identity from a compiler declaration identity.
    #[must_use]
    pub fn for_symbol(symbol: crate::SymbolKey) -> Self {
        Self::for_row(RowId::Symbol(symbol))
    }

    /// Derives a node identity from a package reference.
    #[must_use]
    pub fn for_package(package: &crate::PackageReference) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.rich-ir.graph.node.package-reference.v1\0");
        match package {
            crate::PackageReference::Purl(purl) => {
                hasher.update(b"purl\0");
                hasher.update(purl.as_str().as_bytes());
            }
            crate::PackageReference::Local(label) => {
                hasher.update(b"local\0");
                hasher.update(label.as_str().as_bytes());
            }
        }
        Self(*hasher.finalize().as_bytes())
    }

    /// Derives a node identity from an unresolved, authority-qualified package target.
    #[must_use]
    pub fn for_unresolved_package(
        target: &crate::PackageDependencyTarget,
        authority: GraphAuthority,
    ) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.rich-ir.graph.node.package-lineage.v1\0");
        hasher.update(&[graph_authority_tag(authority)]);
        hasher.update(target.ecosystem.as_str().as_bytes());
        hasher.update(&[0]);
        hasher.update(target.name.as_str().as_bytes());
        Self(*hasher.finalize().as_bytes())
    }
}

/// The relation family used by layout, filtering, and product surfaces.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GraphRelationFamily {
    /// Compiler-resolved declaration and occurrence edges.
    Code,
    /// Outgoing package dependency edges.
    Dependency,
    /// Reverse package dependency edges.
    Dependent,
}

/// The typed relationship represented by a rich graph edge.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "family", rename_all = "kebab-case", deny_unknown_fields)]
pub enum GraphEdgeKind {
    /// A compiler semantic relation.
    Code {
        /// Compiler-owned relation kind.
        relation: SemanticLinkKind,
    },
    /// An outgoing package dependency requirement.
    Dependency {
        /// Resolver scope of the requirement.
        scope: DependencyScope,
        /// Whether the dependency is optional for default resolution.
        optional: bool,
    },
    /// A reverse package dependency requirement.
    Dependent {
        /// Resolver scope of the requirement.
        scope: DependencyScope,
        /// Whether the dependency is optional for default resolution.
        optional: bool,
    },
}

impl GraphEdgeKind {
    /// Returns the coarse relation family.
    #[must_use]
    pub const fn family(self) -> GraphRelationFamily {
        match self {
            Self::Code { .. } => GraphRelationFamily::Code,
            Self::Dependency { .. } => GraphRelationFamily::Dependency,
            Self::Dependent { .. } => GraphRelationFamily::Dependent,
        }
    }

    fn encode_key(self, output: &mut Vec<u8>) {
        match self {
            Self::Code { relation } => {
                output.push(0);
                output.push(semantic_link_tag(relation));
            }
            Self::Dependency { scope, optional } => {
                output.push(1);
                output.push(dependency_scope_tag(scope));
                output.push(u8::from(optional));
            }
            Self::Dependent { scope, optional } => {
                output.push(2);
                output.push(dependency_scope_tag(scope));
                output.push(u8::from(optional));
            }
        }
    }
}

/// The authority which supplied a graph fact.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GraphAuthority {
    /// Native compiler or language oracle publication.
    SemanticGeneration,
    /// Authenticated registry metadata.
    RegistryMetadata,
    /// Dependency manifest inside a package archive.
    ArchiveManifest,
    /// Manifest published by a source forge.
    ForgeManifest,
    /// Manifest read from a local project.
    LocalManifest,
}

fn graph_authority_tag(authority: GraphAuthority) -> u8 {
    match authority {
        GraphAuthority::SemanticGeneration => 0,
        GraphAuthority::RegistryMetadata => 1,
        GraphAuthority::ArchiveManifest => 2,
        GraphAuthority::ForgeManifest => 3,
        GraphAuthority::LocalManifest => 4,
    }
}

fn graph_authority_for_dependency(authority: crate::DependencyAuthority) -> GraphAuthority {
    match authority {
        crate::DependencyAuthority::RegistryMetadata => GraphAuthority::RegistryMetadata,
        crate::DependencyAuthority::ArchiveManifest => GraphAuthority::ArchiveManifest,
        crate::DependencyAuthority::ForgeManifest => GraphAuthority::ForgeManifest,
        crate::DependencyAuthority::LocalManifest => GraphAuthority::LocalManifest,
    }
}

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

/// Stable identity for one directed relationship.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GraphEdgeId([u8; 32]);

impl GraphEdgeId {
    /// Creates an identity from already admitted bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the fixed-width identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Derives a stable identity from endpoints and typed relationship.
    #[must_use]
    pub fn derive(from: GraphNodeId, to: GraphNodeId, kind: GraphEdgeKind) -> Self {
        Self::derive_with_detail(from, to, kind, None)
    }

    /// Derives an identity while retaining a producer supplied edge detail.
    ///
    /// Dependency requirements are part of the canonical package fact, so two
    /// requirements between the same package pair must not collapse into one
    /// edge merely because their resolver scope is equal.
    #[must_use]
    pub fn derive_with_detail(
        from: GraphNodeId,
        to: GraphNodeId,
        kind: GraphEdgeKind,
        detail: Option<&str>,
    ) -> Self {
        let mut preimage = Vec::with_capacity(1 + 32 + 32 + 3 + detail.map_or(1, str::len));
        preimage.extend_from_slice(b"nudox.rich-ir.graph.edge.v1\0");
        preimage.extend_from_slice(from.as_bytes());
        preimage.extend_from_slice(to.as_bytes());
        kind.encode_key(&mut preimage);
        match detail {
            Some(detail) => {
                preimage.push(1);
                preimage.extend_from_slice(&(detail.len() as u32).to_be_bytes());
                preimage.extend_from_slice(detail.as_bytes());
            }
            None => preimage.push(0),
        }
        Self(*blake3::hash(&preimage).as_bytes())
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
    fn new(
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

    /// Checks all internal ordering, identity, endpoint, and bound laws.
    pub fn admit(&self) -> Result<(), RichGraphError> {
        if self.schema != RICH_GRAPH_SCHEMA_VERSION {
            return Err(RichGraphError::Schema);
        }
        if self.nodes.is_empty() {
            return Err(RichGraphError::NodeBound);
        }
        if !self.nodes.windows(2).all(|pair| pair[0].id < pair[1].id) {
            return Err(RichGraphError::Unordered);
        }
        if !self.edges.windows(2).all(|pair| pair[0].id < pair[1].id) {
            return Err(RichGraphError::Unordered);
        }
        if !self.nodes.iter().any(|node| node.id == self.center) {
            return Err(RichGraphError::CenterMissing);
        }
        let node_ids = self
            .nodes
            .iter()
            .map(|node| node.id)
            .collect::<BTreeSet<_>>();
        for edge in self.edges.iter() {
            if edge.id
                != GraphEdgeId::derive_with_detail(
                    edge.from,
                    edge.to,
                    edge.kind,
                    edge.dependency_requirement
                        .as_ref()
                        .map(ProductText::as_str),
                )
                || !node_ids.contains(&edge.from)
                || !node_ids.contains(&edge.to)
            {
                return Err(RichGraphError::EdgeShape);
            }
        }
        let expected_layout =
            GraphLayoutInput::new(self.revision, self.center, &self.nodes, &self.edges);
        if self.layout != expected_layout {
            return Err(RichGraphError::LayoutMismatch);
        }
        Ok(())
    }

    /// Returns a bounded delta from this snapshot to another snapshot.
    ///
    /// Both snapshots are canonically sorted, so the comparison walks their
    /// packed arrays directly and stops before cloning records beyond the
    /// delta budget.
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

    /// Produces one root-pinned page for a graph request.
    pub fn page(&self, request: &RichGraphRequest) -> Result<RichGraphPage, RichGraphError> {
        self.admit()?;
        if request.revision != self.revision {
            return Err(RichGraphError::StaleRoot);
        }
        if request.center != self.center {
            return Err(RichGraphError::QueryMismatch);
        }
        let start = request.start_offset()?;
        if request.control == GraphControl::Cancel {
            return Ok(RichGraphPage {
                schema: RICH_GRAPH_SCHEMA_VERSION,
                revision: self.revision,
                nodes: Box::new([]),
                edges: Box::new([]),
                terminal: GraphPageTerminal::Cancelled,
            });
        }
        let families = request.families.iter().copied().collect::<BTreeSet<_>>();
        let selected_edges = self
            .edges
            .iter()
            .filter(|edge| families.is_empty() || families.contains(&edge.kind.family()))
            .collect::<Vec<_>>();
        let selected_ids = selected_edges
            .iter()
            .flat_map(|edge| [edge.from, edge.to])
            .chain(std::iter::once(self.center))
            .collect::<BTreeSet<_>>();
        let ids = selected_ids.into_iter().collect::<Vec<_>>();
        if start > ids.len() {
            return Err(RichGraphError::CursorMismatch);
        }
        let end = start
            .saturating_add(usize::from(request.limit))
            .min(ids.len());
        let page_ids = ids[start..end].iter().copied().collect::<BTreeSet<_>>();
        let nodes = self
            .nodes
            .iter()
            .filter(|node| page_ids.contains(&node.id))
            .cloned()
            .collect::<Vec<_>>();
        let edges = selected_edges
            .into_iter()
            .filter(|edge| page_ids.contains(&edge.from) && page_ids.contains(&edge.to))
            .cloned()
            .collect::<Vec<_>>();
        if edges.len() > MAX_RICH_GRAPH_PAGE_EDGES {
            return Err(RichGraphError::EdgeBound);
        }
        let terminal = if end < ids.len() {
            GraphPageTerminal::More(request.next_cursor(end)?)
        } else {
            GraphPageTerminal::Complete
        };
        Ok(RichGraphPage {
            schema: RICH_GRAPH_SCHEMA_VERSION,
            revision: self.revision,
            nodes: nodes.into_boxed_slice(),
            edges: edges.into_boxed_slice(),
            terminal,
        })
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

/// Cooperative execution control for one exact graph request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GraphControl {
    /// Execute or resume the request.
    Continue,
    /// Return a cancelled terminal after exact-root admission.
    Cancel,
}

/// Opaque cursor for rich graph node pages.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RichGraphCursor {
    /// Cursor schema.
    pub schema: u16,
    /// Exact graph revision.
    pub revision: RichGraphRevision,
    /// Query recipe hash.
    pub recipe: [u8; 32],
    /// Number of canonical node ids already consumed.
    pub offset: u32,
}

/// Root-pinned graph request with family selection, pagination, and cancel.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RichGraphRequest {
    /// Exact roots required by this request.
    pub revision: RichGraphRevision,
    /// Center node identity.
    pub center: GraphNodeId,
    /// Families to include; empty means all families.
    pub families: Box<[GraphRelationFamily]>,
    /// Bounded page size.
    pub limit: u16,
    /// Opaque continuation from the previous page.
    pub cursor: Option<RichGraphCursor>,
    /// Cooperative execution control.
    pub control: GraphControl,
}

impl RichGraphRequest {
    /// Creates a first-page request.
    pub fn new(
        revision: RichGraphRevision,
        center: GraphNodeId,
        mut families: Vec<GraphRelationFamily>,
        limit: u16,
    ) -> Result<Self, RichGraphError> {
        if limit == 0 || limit > MAX_RICH_GRAPH_PAGE_ROWS {
            return Err(RichGraphError::PageBound);
        }
        families.sort_unstable();
        families.dedup();
        Ok(Self {
            revision,
            center,
            families: families.into_boxed_slice(),
            limit,
            cursor: None,
            control: GraphControl::Continue,
        })
    }

    /// Attaches an opaque continuation cursor.
    #[must_use]
    pub const fn with_cursor(mut self, cursor: RichGraphCursor) -> Self {
        self.cursor = Some(cursor);
        self
    }

    /// Changes this request to a cancellation request.
    #[must_use]
    pub const fn cancelled(mut self) -> Self {
        self.control = GraphControl::Cancel;
        self
    }

    /// Returns the deterministic query recipe hash.
    #[must_use]
    pub fn recipe(&self) -> [u8; 32] {
        let mut preimage = Vec::with_capacity(98 + self.families.len());
        preimage.extend_from_slice(b"nudox.rich-ir.graph.query.v1\0");
        preimage.extend_from_slice(&self.revision.view_root);
        preimage.extend_from_slice(&self.revision.semantic_generation.to_bytes());
        preimage.extend_from_slice(&self.revision.semantic_root);
        preimage.extend_from_slice(self.center.as_bytes());
        preimage.extend_from_slice(&self.limit.to_be_bytes());
        preimage.extend(self.families.iter().map(|family| match family {
            GraphRelationFamily::Code => 0,
            GraphRelationFamily::Dependency => 1,
            GraphRelationFamily::Dependent => 2,
        }));
        *blake3::hash(&preimage).as_bytes()
    }

    /// Validates the continuation and returns its canonical node offset.
    pub fn start_offset(&self) -> Result<usize, RichGraphError> {
        let Some(cursor) = self.cursor else {
            return Ok(0);
        };
        if cursor.schema != RICH_GRAPH_SCHEMA_VERSION
            || cursor.revision != self.revision
            || cursor.recipe != self.recipe()
        {
            return Err(RichGraphError::CursorMismatch);
        }
        usize::try_from(cursor.offset).map_err(|_| RichGraphError::CursorMismatch)
    }

    /// Creates the next opaque cursor for this exact request.
    pub fn next_cursor(&self, offset: usize) -> Result<RichGraphCursor, RichGraphError> {
        let offset = u32::try_from(offset).map_err(|_| RichGraphError::CursorMismatch)?;
        Ok(RichGraphCursor {
            schema: RICH_GRAPH_SCHEMA_VERSION,
            revision: self.revision,
            recipe: self.recipe(),
            offset,
        })
    }
}

/// Terminal state of one rich graph page.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "cursor", rename_all = "kebab-case")]
pub enum GraphPageTerminal {
    /// No more nodes remain.
    Complete,
    /// Another page is available at the same exact roots.
    More(RichGraphCursor),
    /// The request was cooperatively cancelled.
    Cancelled,
}

/// One bounded rich graph page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RichGraphPage {
    /// Contract schema version.
    pub schema: u16,
    /// Exact roots used for the page.
    pub revision: RichGraphRevision,
    /// Nodes in canonical identity order.
    pub nodes: Box<[RichGraphNode]>,
    /// Edges whose endpoints are in this page.
    pub edges: Box<[RichGraphEdge]>,
    /// Explicit completion state.
    pub terminal: GraphPageTerminal,
}

/// Errors raised by graph admission, reconstruction, and query pinning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RichGraphError {
    /// A public text fact exceeded the product text bound.
    Text(ProductAdmissionError),
    /// Unsupported graph schema.
    Schema,
    /// The graph did not contain any node.
    NodeBound,
    /// A page or edge payload exceeded its fixed bound.
    EdgeBound,
    /// Delta record count exceeded its fixed bound.
    DeltaBound,
    /// Page size exceeded its fixed bound.
    PageBound,
    /// A center node was absent.
    CenterMissing,
    /// A node or edge identity was repeated.
    Duplicate,
    /// One identity was reused for different payloads.
    IdentityCollision,
    /// A relation endpoint or derived edge identity was invalid.
    EdgeShape,
    /// Canonical ordering was not preserved.
    Unordered,
    /// Deterministic layout input disagreed with graph facts.
    LayoutMismatch,
    /// A query did not match the graph center or family recipe.
    QueryMismatch,
    /// A continuation belonged to another root or query.
    CursorMismatch,
    /// A delta was based on an older exact graph root.
    StaleRoot,
}

impl fmt::Display for RichGraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Text(error) => return write!(formatter, "rich graph text: {error}"),
            Self::Schema => "unsupported rich graph schema",
            Self::NodeBound => "rich graph node set is empty",
            Self::EdgeBound => "rich graph page edge bound exceeded",
            Self::DeltaBound => "rich graph delta bound exceeded",
            Self::PageBound => "rich graph page bound exceeded",
            Self::CenterMissing => "rich graph center is absent",
            Self::Duplicate => "rich graph contains a duplicate identity",
            Self::IdentityCollision => "rich graph identity payload collision",
            Self::EdgeShape => "rich graph edge shape is invalid",
            Self::Unordered => "rich graph records are not canonically ordered",
            Self::LayoutMismatch => "rich graph layout input is not deterministic",
            Self::QueryMismatch => "rich graph query does not match the snapshot",
            Self::CursorMismatch => "rich graph cursor does not match the query",
            Self::StaleRoot => "rich graph request or delta is pinned to a stale root",
        })
    }
}

impl std::error::Error for RichGraphError {}

fn semantic_link_tag(kind: SemanticLinkKind) -> u8 {
    match kind {
        SemanticLinkKind::Calls => 0,
        SemanticLinkKind::MethodCall => 1,
        SemanticLinkKind::TypeReference => 2,
        SemanticLinkKind::Reads => 3,
        SemanticLinkKind::Writes => 4,
        SemanticLinkKind::Imports => 5,
        SemanticLinkKind::Implements => 6,
        SemanticLinkKind::Overrides => 7,
        SemanticLinkKind::Reexports => 8,
        SemanticLinkKind::Inherits => 9,
        SemanticLinkKind::Documents => 10,
    }
}

fn dependency_scope_tag(scope: DependencyScope) -> u8 {
    match scope {
        DependencyScope::Runtime => 0,
        DependencyScope::Optional => 1,
        DependencyScope::Development => 2,
        DependencyScope::Build => 3,
        DependencyScope::Peer => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GraphRelation, RegistryEcosystem, SemanticLinkKind, symbol_key, view_state_root};

    fn revision(seed: u8) -> RichGraphRevision {
        RichGraphRevision::new(
            [seed; 32],
            SemanticGenerationId::new([seed.saturating_add(1); 32]),
            [seed.saturating_add(2); 32],
        )
    }

    fn graph(revision: RichGraphRevision, extra: usize) -> RichGraphSnapshot {
        let center = GraphNodeId::for_symbol(symbol_key("pkg::centre"));
        let mut builder = RichGraphBuilder::new(revision, center);
        builder
            .add_node(
                RichGraphNode::new(
                    center,
                    "centre",
                    Some("pkg::centre".to_owned()),
                    Some("Function".to_owned()),
                    GraphAvailability::Ready,
                    None,
                )
                .expect("centre node"),
            )
            .expect("centre");
        for index in 0..extra {
            let symbol = symbol_key(&format!("pkg::target{index}"));
            let target = GraphNodeId::for_symbol(symbol);
            builder
                .add_node(
                    RichGraphNode::new(
                        target,
                        format!("target{index}"),
                        None,
                        None,
                        GraphAvailability::Ready,
                        None,
                    )
                    .expect("target node"),
                )
                .expect("target");
            builder
                .add_edge(RichGraphEdge::new(
                    center,
                    target,
                    GraphEdgeKind::Code {
                        relation: SemanticLinkKind::Calls,
                    },
                    GraphAvailability::Ready,
                    GraphProvenance::semantic(
                        revision.semantic_generation,
                        revision.semantic_root,
                        [index as u8; 32],
                        SemanticConfidence::Compiler,
                    ),
                ))
                .expect("edge");
        }
        builder.finish().expect("graph")
    }

    #[test]
    fn ids_are_stable_and_relation_families_are_distinct() {
        let symbol = symbol_key("pkg::centre");
        assert_eq!(
            GraphNodeId::for_symbol(symbol),
            GraphNodeId::for_symbol(symbol)
        );
        let cargo_target =
            crate::PackageDependencyTarget::new(RegistryEcosystem::Cargo, "core", "*", None)
                .expect("cargo lineage");
        let npm_target =
            crate::PackageDependencyTarget::new(RegistryEcosystem::Npm, "core", "*", None)
                .expect("npm lineage");
        assert_ne!(
            GraphNodeId::for_unresolved_package(&cargo_target, GraphAuthority::RegistryMetadata),
            GraphNodeId::for_unresolved_package(&npm_target, GraphAuthority::RegistryMetadata)
        );
        assert_ne!(
            GraphNodeId::for_unresolved_package(&cargo_target, GraphAuthority::RegistryMetadata),
            GraphNodeId::for_unresolved_package(&cargo_target, GraphAuthority::LocalManifest)
        );
        let package = crate::PackageReference::parse("pkg:cargo/demo@1.0.0").expect("package");
        let local = crate::PackageReference::parse("demo").expect("local package");
        assert_ne!(
            GraphNodeId::for_package(&package),
            GraphNodeId::for_package(&local)
        );
        assert_ne!(
            GraphEdgeId::derive(
                GraphNodeId::from_bytes([1; 32]),
                GraphNodeId::from_bytes([2; 32]),
                GraphEdgeKind::Dependency {
                    scope: DependencyScope::Runtime,
                    optional: false,
                }
            ),
            GraphEdgeId::derive(
                GraphNodeId::from_bytes([1; 32]),
                GraphNodeId::from_bytes([2; 32]),
                GraphEdgeKind::Dependent {
                    scope: DependencyScope::Runtime,
                    optional: false,
                }
            )
        );
    }

    #[test]
    fn graph_reconstruction_and_delta_are_inverse_laws() {
        let first = graph(revision(1), 3);
        let second = graph(revision(2), 4);
        let delta = first.diff(&second).expect("delta");
        assert_eq!(first.apply_delta(&delta).expect("apply"), second);
        assert_eq!(second.diff(&first).expect("reverse").target, first.revision);
        assert_eq!(first.apply_delta(&delta).expect("repeat source"), second);
    }

    #[test]
    fn stale_roots_and_foreign_cursors_are_rejected() {
        let snapshot = graph(revision(4), 4);
        let center = snapshot.center;
        let request = RichGraphRequest::new(
            snapshot.revision,
            center,
            vec![GraphRelationFamily::Code],
            2,
        )
        .expect("request");
        let page = snapshot.page(&request).expect("first page");
        let GraphPageTerminal::More(cursor) = page.terminal else {
            panic!("expected a continuation");
        };
        let foreign = RichGraphRequest::new(revision(9), center, Vec::new(), 2)
            .expect("foreign")
            .with_cursor(cursor);
        assert_eq!(snapshot.page(&foreign), Err(RichGraphError::StaleRoot));
        let stale = RichGraphRequest::new(snapshot.revision, center, Vec::new(), 2)
            .expect("stale query")
            .with_cursor(RichGraphCursor {
                revision: snapshot.revision,
                recipe: [8; 32],
                ..cursor
            });
        assert_eq!(snapshot.page(&stale), Err(RichGraphError::CursorMismatch));
    }

    #[test]
    fn cancellation_is_bounded_and_preserves_exact_revision() {
        let snapshot = graph(revision(8), 40);
        let request = RichGraphRequest::new(
            snapshot.revision,
            snapshot.center,
            Vec::new(),
            MAX_RICH_GRAPH_PAGE_ROWS,
        )
        .expect("request")
        .cancelled();
        let page = snapshot.page(&request).expect("cancel");
        assert_eq!(page.terminal, GraphPageTerminal::Cancelled);
        assert!(page.nodes.is_empty());
        assert_eq!(page.revision, snapshot.revision);
    }

    #[test]
    fn high_fanout_graphs_remain_page_bounded() {
        let snapshot = graph(revision(10), 1_400);
        let cloned = snapshot.clone();
        assert!(Arc::ptr_eq(&snapshot.nodes, &cloned.nodes));
        assert!(Arc::ptr_eq(&snapshot.edges, &cloned.edges));
        let same_fanout = graph(revision(11), 1_400);
        assert_eq!(
            snapshot
                .nodes
                .iter()
                .map(|node| node.id)
                .collect::<Vec<_>>(),
            same_fanout
                .nodes
                .iter()
                .map(|node| node.id)
                .collect::<Vec<_>>()
        );
        assert_eq!(snapshot.diff(&same_fanout), Err(RichGraphError::DeltaBound));
        let mut request = RichGraphRequest::new(
            snapshot.revision,
            snapshot.center,
            vec![GraphRelationFamily::Code],
            37,
        )
        .expect("request");
        let mut seen = BTreeSet::new();
        loop {
            let page = snapshot.page(&request).expect("bounded page");
            assert!(page.nodes.len() <= 37);
            seen.extend(page.nodes.iter().map(|node| node.id));
            match page.terminal {
                GraphPageTerminal::Complete => break,
                GraphPageTerminal::More(cursor) => {
                    request = request.clone().with_cursor(cursor);
                }
                GraphPageTerminal::Cancelled => panic!("unexpected cancellation"),
            }
        }
        assert_eq!(seen.len(), snapshot.nodes.len());
    }

    #[test]
    fn package_edges_keep_requirement_and_reverse_dependent_direction() {
        let source = crate::PackageReference::parse("pkg:cargo/demo@1.0.0").expect("source");
        let target =
            crate::PackageDependencyTarget::new(RegistryEcosystem::Cargo, "serde", "^1", None)
                .expect("target");
        let record = PackageDependencyRecord::new(
            source.clone(),
            target,
            DependencyScope::Runtime,
            false,
            crate::DependencyEvidence {
                authority: crate::DependencyAuthority::RegistryMetadata,
                frontier: [1; 32],
                provenance: [2; 32],
            },
        );
        let dependency = RichGraphEdge::from_dependency(&record, GraphRelationFamily::Dependency);
        let dependent = RichGraphEdge::from_dependency(&record, GraphRelationFamily::Dependent);
        assert_eq!(
            dependency
                .dependency_requirement
                .as_ref()
                .map(ProductText::as_str),
            Some("^1")
        );
        assert_eq!(dependency.from, dependent.to);
        assert_eq!(dependency.to, dependent.from);
        assert!(dependency.provenance.semantic_root.is_none());
    }

    #[test]
    fn route_serialization_keeps_generation_root_and_layout_seed() {
        let snapshot = graph(revision(12), 2);
        let encoded = serde_json::to_vec(&snapshot).expect("encode");
        let decoded: RichGraphSnapshot = serde_json::from_slice(&encoded).expect("decode");
        assert_eq!(decoded, snapshot);
        decoded.admit().expect("admitted roundtrip");
        assert_eq!(decoded.revision.semantic_root, [14; 32]);
        assert_eq!(decoded.layout.seed, snapshot.layout.seed);
    }

    #[test]
    fn from_view_rejects_relation_endpoint_outside_root() {
        let root = ViewRoot::new_incomplete(
            crate::view_key(b"graph"),
            crate::Basis::with_context(
                view_state_root(&[]),
                crate::object_version(b"source"),
                crate::branch_key("branch"),
                crate::log_key("log"),
                1,
            ),
            crate::Frontier::new(
                crate::branch_key("branch"),
                crate::log_key("log"),
                1,
                view_state_root(&[]),
                0,
            ),
            vec![],
            vec![],
        )
        .expect("root");
        let centre = symbol_key("pkg::centre");
        let target = symbol_key("pkg::target");
        let result = RichGraphSnapshot::from_view(
            &root,
            RowId::Symbol(centre),
            &[GraphRelation::new(
                RowId::Symbol(centre),
                RowId::Symbol(target),
                SemanticLinkKind::Calls,
            )],
            revision(1),
        );
        assert!(result.is_err());
    }
}
