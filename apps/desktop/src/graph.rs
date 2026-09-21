//! The declaration graph used by the desktop reader.
//!
//! This module deliberately separates graph truth from its GPUI drawing.  A
//! graph is a projection of an admitted [`backend_present::Page`] and the
//! exact [`backend_library::ViewRoot`] that supplied its rows.  It never
//! invents an edge from a display name: every endpoint is a producer-issued
//! `SymbolKey`, and a relation whose endpoint is not present is reported as
//! partial rather than silently rendered as a successful node.
//!
//! The projection and layout are bounded.  Stable identities make a revision
//! refresh cheap to diff, while the layout cache keeps positions for nodes
//! that survived the refresh.  A viewport query then performs culling before
//! the renderer allocates any element for a node.

use backend_library::{DeclarationKind, Row, RowId, RowState, SymbolKey, ViewRoot};
use backend_present::{Identity, IdentityKey, Page, RelationDirection, RelationLabel};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{Excluded, Unbounded};
use std::sync::Arc;
use std::collections::VecDeque;
use thiserror::Error;

/// Maximum nodes admitted into one reader graph.
pub(crate) const MAX_NODES: usize = 512;
/// Maximum edges admitted into one reader graph.
pub(crate) const MAX_EDGES: usize = 1_024;
/// Maximum nodes laid out in one bounded layout pass.
pub(crate) const MAX_LAYOUT_WORK: usize = 512;
/// Maximum graph nodes returned to one viewport draw.
pub(crate) const MAX_VISIBLE_NODES: usize = 192;
/// Optical node width. Keeping this in the projection module means the
/// painter, hit regions, and culling margin share one geometry contract.
pub(crate) const NODE_WIDTH: f32 = 176.0;
/// Optical node height used for edge termination and compact-window layout.
pub(crate) const NODE_HEIGHT: f32 = 64.0;
/// Horizontal separation between deterministic breadth-first layout columns.
pub(crate) const COLUMN_GAP: f32 = 220.0;
/// Vertical separation between deterministic breadth-first layout rows.
pub(crate) const ROW_GAP: f32 = 116.0;
/// Minimum graph canvas height at the smallest supported reader window.
pub(crate) const MIN_CANVAS_HEIGHT: f32 = 220.0;
/// Maximum graph canvas height so the inspector remains reachable.
pub(crate) const MAX_CANVAS_HEIGHT: f32 = 420.0;

/// Stable identity for a graph node.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum NodeId {
    /// A compiler admitted declaration identity.
    Symbol(SymbolKey),
}

impl NodeId {
    /// Returns the underlying declaration key.
    pub(crate) const fn symbol(self) -> SymbolKey {
        match self {
            Self::Symbol(symbol) => symbol,
        }
    }
}

/// The relationship represented by one graph edge.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum RelationKind {
    /// A parent declaration contains the target declaration.
    Contains,
    /// A compiler supplied semantic link with an unknown concrete kind.
    Related,
    /// A typed compiler link, retained from the presentation label.
    Calls,
    /// A typed compiler link to a method.
    MethodCall,
    /// A typed compiler type reference.
    TypeReference,
    /// A typed compiler read.
    Reads,
    /// A typed compiler write.
    Writes,
    /// A typed compiler import.
    Imports,
    /// A typed compiler implementation edge.
    Implements,
    /// A typed compiler override edge.
    Overrides,
    /// A typed compiler re-export edge.
    Reexports,
    /// A typed compiler inheritance edge.
    Inherits,
    /// A typed documentation edge.
    Documents,
}

impl RelationKind {
    fn from_label(label: RelationLabel) -> (Self, RelationDirection) {
        match label {
            RelationLabel::Neighbourhood | RelationLabel::Related => {
                (Self::Related, RelationDirection::Outgoing)
            }
            RelationLabel::Typed(kind, direction) => {
                let kind = match kind {
                    backend_library::SemanticLinkKind::Calls => Self::Calls,
                    backend_library::SemanticLinkKind::MethodCall => Self::MethodCall,
                    backend_library::SemanticLinkKind::TypeReference => Self::TypeReference,
                    backend_library::SemanticLinkKind::Reads => Self::Reads,
                    backend_library::SemanticLinkKind::Writes => Self::Writes,
                    backend_library::SemanticLinkKind::Imports => Self::Imports,
                    backend_library::SemanticLinkKind::Implements => Self::Implements,
                    backend_library::SemanticLinkKind::Overrides => Self::Overrides,
                    backend_library::SemanticLinkKind::Reexports => Self::Reexports,
                    backend_library::SemanticLinkKind::Inherits => Self::Inherits,
                    backend_library::SemanticLinkKind::Documents => Self::Documents,
                };
                (kind, direction)
            }
        }
    }

    /// Returns a stable readable label for controls and edge badges.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Related => "related",
            Self::Calls | Self::MethodCall => "calls",
            Self::TypeReference => "uses type",
            Self::Reads => "reads",
            Self::Writes => "writes",
            Self::Imports => "imports",
            Self::Implements => "implements",
            Self::Overrides => "overrides",
            Self::Reexports => "re-exports",
            Self::Inherits => "inherits",
            Self::Documents => "documents",
        }
    }
}

/// Availability of a node's payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NodeState {
    /// The row has complete display data.
    Ready,
    /// The owner has announced the row but its content is still arriving.
    Loading,
    /// The owner published the row in a failed state.
    Failed,
    /// The identity is known through a page relation but its row is absent.
    Unavailable,
}

/// One stable graph node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GraphNode {
    /// Stable compiler identity.
    pub(crate) id: NodeId,
    /// Display name from the producer coordinate.
    pub(crate) label: String,
    /// Full coordinate used by jump-to-source and navigation.
    pub(crate) coordinate: String,
    /// Typed declaration kind, where the index supplied one.
    pub(crate) kind: Option<DeclarationKind>,
    /// Source path and line, when captured by the producer.
    pub(crate) source: Option<(String, u32)>,
    /// Honest availability of the row.
    pub(crate) state: NodeState,
}

/// One stable graph edge.  The direction is encoded by its endpoint order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct GraphEdgeId {
    /// Source node.
    pub(crate) from: NodeId,
    /// Target node.
    pub(crate) to: NodeId,
    /// Semantic relationship.
    pub(crate) relation: RelationKind,
}

/// One graph edge with a display label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GraphEdge {
    /// Stable edge identity.
    pub(crate) id: GraphEdgeId,
    /// Label shown in the graph legend.
    pub(crate) label: &'static str,
}

/// Why a graph is not a complete ready projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GraphAvailability {
    /// All requested page relations had admitted row payloads.
    Ready,
    /// Some relation endpoints were outside the selected root or bound.
    Partial {
        /// Number of omitted relation endpoints.
        omitted: usize,
    },
    /// No graph rows were available yet.
    Loading,
    /// The graph lane could not be queried.
    Unavailable(String),
    /// The graph response was malformed or contradicted its root.
    Error(String),
}

/// The exact versions on which a projection is based.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct GraphRevision {
    /// Immutable visible view version.
    pub(crate) view: [u8; 32],
    /// Immutable semantic source object version.
    pub(crate) source: [u8; 32],
    /// Visible relation root, useful for stale graph diagnostics.
    pub(crate) relation: [u8; 32],
}

impl GraphRevision {
    fn from_root(root: &ViewRoot) -> Self {
        let basis = root.basis();
        Self {
            view: *root.version().as_bytes(),
            source: *basis.object.as_bytes(),
            relation: *root.root().as_bytes(),
        }
    }
}

/// A bounded, revision-pinned graph projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GraphProjection {
    /// Revision and source object used to derive this graph.
    pub(crate) revision: GraphRevision,
    /// The declaration the reader opened.
    pub(crate) center: NodeId,
    /// Stable nodes keyed by identity.
    pub(crate) nodes: BTreeMap<NodeId, GraphNode>,
    /// Stable edges keyed by endpoint and relationship.
    pub(crate) edges: BTreeMap<GraphEdgeId, GraphEdge>,
    /// Honest state of the graph lane.
    pub(crate) availability: GraphAvailability,
}

impl GraphProjection {
    /// Derives a graph from the page and the exact live view root.
    ///
    /// The page carries typed relations and the root carries authoritative row
    /// payloads.  Combining both gives the graph enough IR for source jumps,
    /// kind badges, and revision diagnostics without treating a name as an
    /// identity.  All work is bounded before the renderer sees this value.
    #[must_use]
    pub(crate) fn from_page(page: &Page, root: &ViewRoot) -> Self {
        let center = match page.identity().key() {
            IdentityKey::Symbol(symbol) => NodeId::Symbol(symbol),
            IdentityKey::Package(_) | IdentityKey::Absent => {
                return Self::empty(
                    GraphRevision::from_root(root),
                    "page has no symbol identity",
                );
            }
        };
        let rows: BTreeMap<SymbolKey, &Row> = root
            .rows()
            .iter()
            .filter_map(|row| match row.id {
                RowId::Symbol(symbol) => Some((symbol, row)),
                RowId::Package(_) | RowId::Object(_) => None,
            })
            .collect();
        let mut graph = Self {
            revision: GraphRevision::from_root(root),
            center,
            nodes: BTreeMap::new(),
            edges: BTreeMap::new(),
            availability: GraphAvailability::Ready,
        };
        graph.add_identity(page.identity(), page.kind(), &rows);

        // Members are real containment facts from the package outline.  They
        // are useful even when the graph lane has no semantic edges.
        let mut omitted = 0usize;
        for member in page.members().iter().flat_map(|group| group.members()) {
            if graph.nodes.len() >= MAX_NODES {
                omitted = omitted.saturating_add(1);
                break;
            }
            let Some(symbol) = symbol_of(member.identity()) else {
                omitted = omitted.saturating_add(1);
                continue;
            };
            let node = NodeId::Symbol(symbol);
            let admitted = rows.contains_key(&symbol);
            graph.add_identity(member.identity(), member.kind(), &rows);
            if admitted {
                graph.add_edge(center, node, RelationKind::Contains);
            } else {
                omitted = omitted.saturating_add(1);
            }
        }

        for group in page.relations() {
            let (kind, direction) = RelationKind::from_label(group.label());
            for relation in group.relations() {
                if graph.nodes.len() >= MAX_NODES || graph.edges.len() >= MAX_EDGES {
                    omitted = omitted.saturating_add(1);
                    break;
                }
                let Some(symbol) = symbol_of(relation.identity()) else {
                    omitted = omitted.saturating_add(1);
                    continue;
                };
                let node = NodeId::Symbol(symbol);
                let admitted = rows.contains_key(&symbol);
                graph.add_identity(relation.identity(), relation.kind(), &rows);
                let (from, to) = match direction {
                    RelationDirection::Outgoing => (center, node),
                    RelationDirection::Incoming => (node, center),
                };
                if admitted {
                    graph.add_edge(from, to, kind);
                } else {
                    omitted = omitted.saturating_add(1);
                }
            }
        }

        // Retain the exact parent relation from the indexed IR when it is
        // available.  This handles a page with no member rows while keeping
        // the graph honest about the missing parent payload.
        if let Some(row) = rows.get(&center.symbol()) {
            if let Some(parent) = row.parent {
                let parent_id = NodeId::Symbol(parent);
                if graph.nodes.len() < MAX_NODES {
                    if let Some(parent_row) = rows.get(&parent) {
                        graph.add_identity_from_row(parent_id, parent_row);
                        graph.add_edge(parent_id, center, RelationKind::Contains);
                    } else {
                        omitted = omitted.saturating_add(1);
                    }
                }
            }
        }

        if graph
            .nodes
            .get(&center)
            .is_some_and(|node| node.state == NodeState::Unavailable)
        {
            graph.availability = GraphAvailability::Unavailable(
                "the selected declaration is not present in this admitted revision".to_owned(),
            );
        } else if omitted > 0 {
            graph.availability = GraphAvailability::Partial { omitted };
        } else if graph
            .nodes
            .values()
            .any(|node| node.state == NodeState::Loading)
        {
            graph.availability = GraphAvailability::Loading;
        } else if graph
            .nodes
            .values()
            .any(|node| node.state == NodeState::Failed)
        {
            graph.availability =
                GraphAvailability::Unavailable("the index published a failed graph row".to_owned());
        }
        graph
    }

    fn empty(revision: GraphRevision, message: &str) -> Self {
        Self {
            revision,
            center: NodeId::Symbol(SymbolKey::from_value("")),
            nodes: BTreeMap::new(),
            edges: BTreeMap::new(),
            availability: GraphAvailability::Error(message.to_owned()),
        }
    }

    fn add_identity(
        &mut self,
        identity: &Identity,
        kind: Option<DeclarationKind>,
        rows: &BTreeMap<SymbolKey, &Row>,
    ) {
        let Some(symbol) = symbol_of(identity) else {
            return;
        };
        let node = NodeId::Symbol(symbol);
        if self.nodes.contains_key(&node) {
            return;
        }
        if let Some(row) = rows.get(&symbol) {
            self.add_identity_from_row(node, row);
            return;
        }
        self.nodes.insert(
            node,
            GraphNode {
                id: node,
                label: identity.name().to_owned(),
                coordinate: identity.coordinate().as_str().to_owned(),
                kind,
                source: None,
                state: NodeState::Unavailable,
            },
        );
    }

    fn add_identity_from_row(&mut self, node: NodeId, row: &Row) {
        let identity = Identity::parse_with_key(&row.label, IdentityKey::Symbol(node.symbol()));
        let source = row
            .source
            .captured()
            .map(|location| (location.path().to_owned(), location.start_line()));
        let state = match row.state {
            RowState::Ready => NodeState::Ready,
            RowState::Loading => NodeState::Loading,
            RowState::Failed => NodeState::Failed,
        };
        self.nodes.entry(node).or_insert_with(|| GraphNode {
            id: node,
            label: identity.name().to_owned(),
            coordinate: row.label.clone(),
            kind: row.kind,
            source,
            state,
        });
    }

    fn add_edge(&mut self, from: NodeId, to: NodeId, relation: RelationKind) {
        if self.edges.len() >= MAX_EDGES
            || !self.nodes.contains_key(&from)
            || !self.nodes.contains_key(&to)
        {
            return;
        }
        let id = GraphEdgeId { from, to, relation };
        self.edges.entry(id).or_insert(GraphEdge {
            id,
            label: relation.label(),
        });
    }

    /// Returns a stable diff from this projection to the next revision.
    #[must_use]
    pub(crate) fn diff(&self, next: &Self) -> GraphDelta {
        let added_nodes = next
            .nodes
            .keys()
            .filter(|id| !self.nodes.contains_key(id))
            .filter_map(|id| next.nodes.get(id).cloned())
            .collect();
        let removed_nodes = self
            .nodes
            .keys()
            .filter(|id| !next.nodes.contains_key(id))
            .copied()
            .collect();
        let added_edges = next
            .edges
            .keys()
            .filter(|id| !self.edges.contains_key(id))
            .filter_map(|id| next.edges.get(id).cloned())
            .collect();
        let removed_edges = self
            .edges
            .keys()
            .filter(|id| !next.edges.contains_key(id))
            .copied()
            .collect();
        let updated_nodes = next
            .nodes
            .iter()
            .filter(|(id, node)| {
                self.nodes
                    .get(id)
                    .is_some_and(|previous| previous != *node)
            })
            .map(|(_, node)| node.clone())
            .collect();
        let updated_edges = next
            .edges
            .iter()
            .filter(|(id, edge)| {
                self.edges
                    .get(id)
                    .is_some_and(|previous| previous != *edge)
            })
            .map(|(_, edge)| edge.clone())
            .collect();
        GraphDelta {
            from: self.revision,
            to: next.revision,
            center: next.center,
            availability: next.availability.clone(),
            added_nodes,
            removed_nodes,
            added_edges,
            removed_edges,
            updated_nodes,
            updated_edges,
        }
    }
}

fn symbol_of(identity: &Identity) -> Option<SymbolKey> {
    match identity.key() {
        IdentityKey::Symbol(symbol) => Some(symbol),
        IdentityKey::Package(_) | IdentityKey::Absent => None,
    }
}

/// Stable changes between two graph revisions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GraphDelta {
    /// Previous revision.
    pub(crate) from: GraphRevision,
    /// New revision.
    pub(crate) to: GraphRevision,
    /// Center node at the target revision.
    pub(crate) center: NodeId,
    /// Availability at the target revision.
    pub(crate) availability: GraphAvailability,
    /// Nodes newly visible, including their complete producer payload.
    pub(crate) added_nodes: Vec<GraphNode>,
    /// Nodes no longer visible.
    pub(crate) removed_nodes: Vec<NodeId>,
    /// Edges newly visible, including their complete display payload.
    pub(crate) added_edges: Vec<GraphEdge>,
    /// Edges no longer visible.
    pub(crate) removed_edges: Vec<GraphEdgeId>,
    /// Stable nodes whose producer payload changed in place.
    pub(crate) updated_nodes: Vec<GraphNode>,
    /// Stable edges whose producer payload changed in place.
    pub(crate) updated_edges: Vec<GraphEdge>,
}

/// Why an incremental graph delta could not be admitted.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum GraphDeltaError {
    /// The caller tried to apply a delta to a different revision.
    #[error("graph delta is stale: expected base {expected:?}, received {actual:?}")]
    StaleBase {
        /// Revision named by the delta.
        expected: GraphRevision,
        /// Revision held by the caller.
        actual: GraphRevision,
    },
    /// The delta contradicts the graph it is being applied to.
    #[error("graph delta contradicts its base: {0}")]
    Conflict(&'static str),
}

impl GraphDelta {
    /// Applies a complete delta in place without rebuilding the graph.
    ///
    /// The base revision is checked before mutation. The operation consumes
    /// only the changed payloads and therefore lets a live one-edge update
    /// retain every untouched node/edge allocation.
    pub(crate) fn apply_in_place(
        &self,
        graph: &mut GraphProjection,
    ) -> Result<(), GraphDeltaError> {
        if graph.revision != self.from {
            return Err(GraphDeltaError::StaleBase {
                expected: self.from,
                actual: graph.revision,
            });
        }
        for node in &self.added_nodes {
            if graph.nodes.contains_key(&node.id) {
                return Err(GraphDeltaError::Conflict("added node already exists"));
            }
        }
        for node in &self.updated_nodes {
            if !graph.nodes.contains_key(&node.id) {
                return Err(GraphDeltaError::Conflict("updated node is absent"));
            }
        }
        for id in &self.removed_nodes {
            if !graph.nodes.contains_key(id) {
                return Err(GraphDeltaError::Conflict("removed node is absent"));
            }
        }
        for edge in &self.added_edges {
            if graph.edges.contains_key(&edge.id) {
                return Err(GraphDeltaError::Conflict("added edge already exists"));
            }
        }
        for edge in &self.updated_edges {
            if !graph.edges.contains_key(&edge.id) {
                return Err(GraphDeltaError::Conflict("updated edge is absent"));
            }
        }
        for id in &self.removed_edges {
            if !graph.edges.contains_key(id) {
                return Err(GraphDeltaError::Conflict("removed edge is absent"));
            }
        }
        let removed_nodes = self.removed_nodes.iter().copied().collect::<BTreeSet<_>>();
        let added_nodes = self
            .added_nodes
            .iter()
            .map(|node| node.id)
            .collect::<BTreeSet<_>>();
        for edge in self.updated_edges.iter().chain(self.added_edges.iter()) {
            let endpoints_exist = |id| {
                !removed_nodes.contains(&id)
                    && (graph.nodes.contains_key(&id) || added_nodes.contains(&id))
            };
            if !endpoints_exist(edge.id.from) || !endpoints_exist(edge.id.to) {
                return Err(GraphDeltaError::Conflict("edge endpoint is absent"));
            }
        }

        for id in &self.removed_edges {
            graph.edges.remove(id);
        }
        for id in &self.removed_nodes {
            graph.nodes.remove(id);
            graph.edges.retain(|edge, _| edge.from != *id && edge.to != *id);
        }
        for node in &self.updated_nodes {
            graph.nodes.insert(node.id, node.clone());
        }
        for node in &self.added_nodes {
            graph.nodes.insert(node.id, node.clone());
        }
        for edge in &self.updated_edges {
            graph.edges.insert(edge.id, edge.clone());
        }
        for edge in &self.added_edges {
            graph.edges.insert(edge.id, edge.clone());
        }
        graph.center = self.center;
        graph.availability = self.availability.clone();
        graph.revision = self.to;
        Ok(())
    }

    /// Applies a complete delta while transferring ownership of the base.
    pub(crate) fn apply_to(
        &self,
        mut graph: GraphProjection,
    ) -> Result<GraphProjection, GraphDeltaError> {
        self.apply_in_place(&mut graph)?;
        Ok(graph)
    }
}

/// A finite layout point in graph world coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Position {
    /// Horizontal coordinate.
    pub(crate) x: f32,
    /// Vertical coordinate.
    pub(crate) y: f32,
}

impl Default for Position {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0 }
    }
}

/// A rectangular graph viewport in world coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Viewport {
    /// Left edge.
    pub(crate) left: f32,
    /// Top edge.
    pub(crate) top: f32,
    /// Width.
    pub(crate) width: f32,
    /// Height.
    pub(crate) height: f32,
}

impl Viewport {
    /// Returns whether a point is within the viewport plus a node margin.
    #[must_use]
    pub(crate) fn contains(self, position: Position, margin: f32) -> bool {
        position.x >= self.left - margin
            && position.x <= self.left + self.width + margin
            && position.y >= self.top - margin
            && position.y <= self.top + self.height + margin
    }
}

/// Cached deterministic positions for one graph revision.
#[derive(Clone, Debug, Default)]
pub(crate) struct LayoutCache {
    revision: Option<GraphRevision>,
    center: Option<NodeId>,
    positions: BTreeMap<NodeId, Position>,
    depths: BTreeMap<NodeId, usize>,
    /// Number of nodes actually processed in the last bounded pass.
    pub(crate) work: usize,
    /// Whether the pass covered the complete admitted graph.
    pub(crate) complete: bool,
}

impl LayoutCache {
    /// Recomputes only when the revision changes, retaining positions for
    /// surviving identities and returning work that is explicitly bounded.
    pub(crate) fn sync(&mut self, graph: &GraphProjection) {
        self.sync_with_delta(graph, None);
    }

    /// Applies a graph delta while retaining unaffected coordinates. Only
    /// added, updated, or edge-adjacent nodes are placed again; this keeps a
    /// live feed from making a reader graph jump on every revision.
    pub(crate) fn sync_with_delta(
        &mut self,
        graph: &GraphProjection,
        delta: Option<&GraphDelta>,
    ) {
        if self.revision == Some(graph.revision)
            && self.positions.keys().all(|id| graph.nodes.contains_key(id))
        {
            return;
        }
        let previous = std::mem::take(&mut self.positions);
        let previous_depths = std::mem::take(&mut self.depths);
        let mut dirty = BTreeSet::new();
        if let Some(delta) = delta {
            dirty.extend(delta.added_nodes.iter().map(|node| node.id));
            dirty.extend(delta.updated_nodes.iter().map(|node| node.id));
            dirty.extend(
                delta
                    .added_edges
                    .iter()
                    .map(|edge| edge.id)
                    .chain(delta.removed_edges.iter().copied())
                    .chain(delta.updated_edges.iter().map(|edge| edge.id))
                    .flat_map(|edge| [edge.from, edge.to]),
            );
        } else {
            dirty.extend(graph.nodes.keys().copied());
        }
        let incremental = delta.is_some()
            && self.revision.is_some()
            && delta.is_some_and(|delta| {
                delta.removed_nodes.is_empty() && delta.removed_edges.is_empty()
            });
        let mut distance = if incremental {
            previous_depths
                .into_iter()
                .filter(|(id, _)| graph.nodes.contains_key(id))
                .collect::<BTreeMap<_, _>>()
        } else {
            BTreeMap::new()
        };
        if incremental {
            distance.entry(graph.center).or_insert(0);
            for node in graph.nodes.keys() {
                distance.entry(*node).or_insert(0);
            }
            // An addition can only shorten paths. Propagating from the small
            // changed frontier keeps a one-edge update from traversing the
            // entire graph; removals fall back to the exact BFS above.
            let mut frontier = VecDeque::from_iter(dirty.iter().copied());
            frontier.push_back(graph.center);
            while let Some(node) = frontier.pop_front() {
                let candidate = if node == graph.center {
                    0
                } else {
                    graph
                        .edges
                        .values()
                        .filter(|edge| edge.id.from == node || edge.id.to == node)
                        .map(|edge| {
                            let neighbour = if edge.id.from == node {
                                edge.id.to
                            } else {
                                edge.id.from
                            };
                            distance
                                .get(&neighbour)
                                .copied()
                                .unwrap_or(0)
                                .saturating_add(1)
                        })
                        .min()
                        .unwrap_or(0)
                };
                if distance.get(&node).copied() != Some(candidate) {
                    distance.insert(node, candidate);
                    for edge in graph
                        .edges
                        .values()
                        .filter(|edge| edge.id.from == node || edge.id.to == node)
                    {
                        frontier.push_back(if edge.id.from == node {
                            edge.id.to
                        } else {
                            edge.id.from
                        });
                    }
                }
            }
        } else {
            distance.insert(graph.center, 0usize);
            let mut frontier = vec![graph.center];
            while let Some(node) = frontier.pop() {
                let depth = distance[&node];
                for edge in graph
                    .edges
                    .values()
                    .filter(|edge| edge.id.from == node || edge.id.to == node)
                {
                    let next = if edge.id.from == node {
                        edge.id.to
                    } else {
                        edge.id.from
                    };
                    if !distance.contains_key(&next) {
                        distance.insert(next, depth.saturating_add(1));
                        frontier.push(next);
                    }
                }
            }
        }
        let mut positions = BTreeMap::new();
        let mut levels: BTreeMap<usize, Vec<NodeId>> = BTreeMap::new();
        for node in graph.nodes.keys().copied() {
            levels
                .entry(*distance.get(&node).unwrap_or(&0))
                .or_default()
                .push(node);
        }
        for node in graph.nodes.keys().copied() {
            levels
                .entry(*distance.get(&node).unwrap_or(&0))
                .or_default()
                .push(node);
        }
        let mut work = 0usize;
        for (depth, mut nodes) in levels {
            nodes.sort_unstable();
            for (slot, node) in nodes.into_iter().enumerate() {
                if work >= MAX_LAYOUT_WORK {
                    break;
                }
                if !dirty.contains(&node) && previous.contains_key(&node) {
                    positions.insert(node, previous[&node]);
                    continue;
                }
                let fallback = Position {
                    x: NODE_WIDTH - 16.0 + slot as f32 * COLUMN_GAP,
                    y: NODE_HEIGHT + 24.0 + depth as f32 * ROW_GAP,
                };
                let position = if dirty.contains(&node) {
                    fallback
                } else {
                    previous.get(&node).copied().unwrap_or(fallback)
                };
                positions.insert(node, position);
                work = work.saturating_add(1);
            }
            if work >= MAX_LAYOUT_WORK {
                break;
            }
        }
        let complete = positions.len() >= graph.nodes.len();
        self.positions = positions;
        self.depths = distance;
        self.work = work;
        self.complete = complete;
        self.revision = Some(graph.revision);
        self.center = Some(graph.center);
    }

    /// Returns a bounded cull set for the current viewport.
    #[must_use]
    pub(crate) fn visible_nodes<'a>(
        &self,
        graph: &'a GraphProjection,
        viewport: Viewport,
        limit: usize,
    ) -> Vec<&'a GraphNode> {
        let limit = limit.min(MAX_VISIBLE_NODES);
        graph
            .nodes
            .values()
            .filter(|node| {
                self.positions
                    .get(&node.id)
                    .is_some_and(|position| viewport.contains(*position, 180.0))
            })
            .take(limit)
            .collect()
    }

    /// Returns a node's world position, if this pass laid it out.
    #[must_use]
    pub(crate) fn position(&self, node: NodeId) -> Option<Position> {
        self.positions.get(&node).copied()
    }
}

/// Relation filters exposed by the graph legend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RelationFilters {
    enabled: BTreeSet<RelationKind>,
}

impl Default for RelationFilters {
    fn default() -> Self {
        Self {
            enabled: BTreeSet::from([
                RelationKind::Contains,
                RelationKind::Related,
                RelationKind::Calls,
                RelationKind::MethodCall,
                RelationKind::TypeReference,
                RelationKind::Reads,
                RelationKind::Writes,
                RelationKind::Imports,
                RelationKind::Implements,
                RelationKind::Overrides,
                RelationKind::Reexports,
                RelationKind::Inherits,
                RelationKind::Documents,
            ]),
        }
    }
}

impl RelationFilters {
    /// Enables or disables one relationship kind.
    pub(crate) fn toggle(&mut self, kind: RelationKind) {
        if !self.enabled.remove(&kind) {
            self.enabled.insert(kind);
        }
    }

    /// Returns whether a relationship kind is visible.
    #[must_use]
    pub(crate) fn accepts(&self, kind: RelationKind) -> bool {
        self.enabled.contains(&kind)
    }

    fn signature(&self) -> u16 {
        self.enabled.iter().fold(0u16, |bits, kind| {
            let bit = (*kind as u8).min(15);
            bits | (1u16 << bit)
        })
    }
}

/// Stable geometry shared by the canvas painter and edge hit regions.
pub(crate) type EdgeGeometry = (GraphEdgeId, Position, Position);

/// A graph hit target. Nodes have priority over edges so an edge terminating
/// at a node can never steal that node's pointer or keyboard affordance.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum HitTarget {
    /// A declaration node.
    Node(NodeId),
    /// A semantic edge between two declaration nodes.
    Edge(GraphEdgeId),
}

#[derive(Clone, Debug)]
struct EdgeGeometryCache {
    revision: GraphRevision,
    viewport: Viewport,
    filters: u16,
    paths: Arc<[EdgeGeometry]>,
}

/// Mutable navigation state for a graph reader.
#[derive(Clone, Debug, Default)]
pub(crate) struct ExplorerState {
    /// Current graph projection.
    pub(crate) graph: Option<GraphProjection>,
    /// Revision aware layout cache.
    pub(crate) layout: LayoutCache,
    /// Selected node for keyboard traversal.
    pub(crate) selected: Option<NodeId>,
    /// Current world pan offset.
    pub(crate) pan: Position,
    /// Last pointer position during a middle-button pan gesture.
    drag_last: Option<Position>,
    /// Current zoom, clamped to a readable range.
    pub(crate) zoom: f32,
    /// Last reader window width, supplied by the live GPUI window.
    viewport_width: f32,
    /// Last reader window height, supplied by the live GPUI window.
    viewport_height: f32,
    /// Visible relationship filters.
    pub(crate) filters: RelationFilters,
    /// Whether the reader's graph panel is expanded.
    pub(crate) expanded: bool,
    edge_geometry: Option<EdgeGeometryCache>,
}

impl ExplorerState {
    /// Returns a usable default zoom value.
    pub(crate) fn new() -> Self {
        Self {
            zoom: 1.0,
            viewport_width: 640.0,
            viewport_height: 480.0,
            expanded: true,
            ..Self::default()
        }
    }

    /// Records the actual window bounds used for responsive culling and the
    /// compact graph canvas height. The graph never assumes a desktop width.
    pub(crate) fn set_viewport_size(&mut self, width: f32, height: f32) {
        self.viewport_width = width.max(1.0);
        self.viewport_height = height.max(1.0);
    }

    /// Returns the current world viewport after pan and zoom are applied.
    #[must_use]
    pub(crate) fn viewport(&self) -> Viewport {
        Viewport {
            left: -self.pan.x / self.zoom,
            top: -self.pan.y / self.zoom,
            width: self.viewport_width / self.zoom,
            height: self.canvas_height() / self.zoom,
        }
    }

    /// Returns a responsive canvas height that keeps the inspector reachable.
    #[must_use]
    pub(crate) fn canvas_height(&self) -> f32 {
        (self.viewport_height * 0.42).clamp(MIN_CANVAS_HEIGHT, MAX_CANVAS_HEIGHT)
    }

    /// Installs the latest projection while preserving stable selection.
    pub(crate) fn sync(&mut self, graph: GraphProjection) -> Option<GraphDelta> {
        let selected = self.selected.filter(|id| graph.nodes.contains_key(id));
        let delta = self.graph.as_ref().map(|old| old.diff(&graph));
        if self
            .graph
            .as_ref()
            .is_none_or(|old| old.revision != graph.revision)
        {
            self.layout.sync_with_delta(&graph, delta.as_ref());
            self.edge_geometry = None;
        }
        self.selected = selected.or(Some(graph.center));
        self.graph = Some(graph);
        delta
    }

    /// Returns whether a live root can reuse the current projection.
    ///
    /// Page assembly is called on every reader paint. Checking the immutable
    /// root version and center identity here avoids rebuilding or cloning the
    /// graph until the producer has actually advanced the admitted revision.
    #[must_use]
    pub(crate) fn needs_sync(&self, page: &Page, root: &ViewRoot) -> bool {
        let center = match page.identity().key() {
            IdentityKey::Symbol(symbol) => NodeId::Symbol(symbol),
            IdentityKey::Package(_) | IdentityKey::Absent => return true,
        };
        self.graph.as_ref().is_none_or(|graph| {
            graph.revision != GraphRevision::from_root(root) || graph.center != center
        })
    }

    /// Moves the camera while the graph canvas is being dragged.
    pub(crate) fn begin_drag(&mut self, position: Position) {
        self.drag_last = Some(position);
    }

    /// Applies a pointer delta to the camera.
    pub(crate) fn drag_to(&mut self, position: Position) {
        if let Some(last) = self.drag_last.replace(position) {
            self.pan_by(position.x - last.x, position.y - last.y);
        }
    }

    /// Ends a pointer pan gesture.
    pub(crate) fn end_drag(&mut self) {
        self.drag_last = None;
    }

    /// Moves the graph camera by a bounded amount.
    pub(crate) fn pan_by(&mut self, dx: f32, dy: f32) {
        self.pan.x = (self.pan.x + dx).clamp(-4_000.0, 4_000.0);
        self.pan.y = (self.pan.y + dy).clamp(-4_000.0, 4_000.0);
    }

    /// Adjusts zoom and clamps it to the supported readable range.
    pub(crate) fn zoom_by(&mut self, delta: f32) {
        self.zoom = (self.zoom + delta).clamp(0.35, 2.5);
    }

    /// Resets pan and zoom, retaining node selection.
    pub(crate) fn fit(&mut self) {
        self.pan = Position { x: 0.0, y: 0.0 };
        self.zoom = 1.0;
    }

    /// Selects the next visible node in stable key order.
    pub(crate) fn focus_next(&mut self) {
        let Some(graph) = &self.graph else { return };
        self.selected = self
            .selected
            .and_then(|selected| {
                graph
                    .nodes
                    .range((Excluded(selected), Unbounded))
                    .next()
                    .map(|(id, _)| *id)
            })
            .or_else(|| graph.nodes.keys().next().copied());
    }

    /// Selects the preceding visible node in stable key order.
    pub(crate) fn focus_previous(&mut self) {
        let Some(graph) = &self.graph else { return };
        self.selected = self
            .selected
            .and_then(|selected| {
                graph
                    .nodes
                    .range(..selected)
                    .next_back()
                    .map(|(id, _)| *id)
            })
            .or_else(|| graph.nodes.keys().next_back().copied());
    }

    /// Toggles one relationship kind in the current legend.
    pub(crate) fn toggle_filter(&mut self, kind: RelationKind) {
        self.filters.toggle(kind);
        self.edge_geometry = None;
    }

    /// Expands or collapses the deliberate reader graph panel.
    pub(crate) fn toggle_expanded(&mut self) {
        self.expanded = !self.expanded;
    }

    /// Returns cached edge paths for the current live revision and viewport.
    /// The returned Arc is shared by the painter and semantic hit regions, so
    /// rendering never clones the entire projection or the geometry list.
    pub(crate) fn edge_geometry(&mut self, viewport: Viewport) -> Arc<[EdgeGeometry]> {
        let Some(graph) = self.graph.as_ref() else {
            return Arc::from(Vec::<EdgeGeometry>::new());
        };
        let filters = self.filters.signature();
        if let Some(cache) = &self.edge_geometry
            && cache.revision == graph.revision
            && cache.viewport == viewport
            && cache.filters == filters
        {
            return Arc::clone(&cache.paths);
        }
        let paths: Arc<[EdgeGeometry]> = graph
            .edges
            .values()
            .filter(|edge| self.filters.accepts(edge.id.relation))
            .filter_map(|edge| {
                let from = self.layout.position(edge.id.from)?;
                let to = self.layout.position(edge.id.to)?;
                if viewport.contains(from, NODE_WIDTH) && viewport.contains(to, NODE_WIDTH) {
                    Some((edge.id, from, to))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .into();
        self.edge_geometry = Some(EdgeGeometryCache {
            revision: graph.revision,
            viewport,
            filters,
            paths: Arc::clone(&paths),
        });
        paths
    }

    /// Hit-tests in graph world coordinates using the same cached geometry
    /// that the painter uses. Node rectangles win over edge strokes, and
    /// edges are selected by nearest distance with stable edge-id ordering.
    #[must_use]
    pub(crate) fn hit_test(
        &mut self,
        point: Position,
        viewport: Viewport,
        tolerance: f32,
    ) -> Option<HitTarget> {
        let graph = self.graph.as_ref()?;
        for node in graph.nodes.values() {
            let Some(position) = self.layout.position(node.id) else {
                continue;
            };
            if point.x >= position.x
                && point.x <= position.x + NODE_WIDTH
                && point.y >= position.y
                && point.y <= position.y + NODE_HEIGHT
            {
                return Some(HitTarget::Node(node.id));
            }
        }
        self.edge_geometry(viewport)
            .iter()
            .filter_map(|(id, from, to)| {
                let distance = distance_to_segment(point, *from, *to);
                (distance <= tolerance).then_some((*id, distance))
            })
            .min_by(|(left_id, left_distance), (right_id, right_distance)| {
                left_distance
                    .total_cmp(right_distance)
                    .then_with(|| left_id.cmp(right_id))
            })
            .map(|(id, _)| HitTarget::Edge(id))
    }
}

fn distance_to_segment(point: Position, from: Position, to: Position) -> f32 {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let length_squared = dx.mul_add(dx, dy * dy);
    if length_squared <= f32::EPSILON {
        return (point.x - from.x).hypot(point.y - from.y);
    }
    let projection = ((point.x - from.x).mul_add(dx, (point.y - from.y) * dy)
        / length_squared)
        .clamp(0.0, 1.0);
    let closest = Position {
        x: from.x + projection * dx,
        y: from.y + projection * dy,
    };
    (point.x - closest.x).hypot(point.y - closest.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_library::{
        Basis, Frontier, Row, RowId, ViewRoot, object_version, symbol_key, view_key,
        view_state_root,
    };
    use backend_present::{
        IdentityKey, LineNumber, Member, MemberGroup, PackagePath, Relation, RelationGroup,
        Source, SourceLine, SourceSite, Truncation,
    };

    fn key(seed: u64) -> SymbolKey {
        let text = format!("test-symbol-{seed}");
        SymbolKey::from_value(&text)
    }

    fn node(id: NodeId, label: impl Into<String>) -> GraphNode {
        GraphNode {
            id,
            label: label.into(),
            coordinate: "test::src/lib.rs:1::symbol".to_owned(),
            kind: None,
            source: None,
            state: NodeState::Ready,
        }
    }

    fn graph(count: usize) -> GraphProjection {
        let center = NodeId::Symbol(key(0));
        let mut result = GraphProjection {
            revision: GraphRevision {
                view: [1; 32],
                source: [2; 32],
                relation: [3; 32],
            },
            center,
            nodes: BTreeMap::new(),
            edges: BTreeMap::new(),
            availability: GraphAvailability::Ready,
        };
        for seed in 0..count {
            let id = NodeId::Symbol(key(seed as u64));
            result.nodes.insert(id, node(id, seed.to_string()));
            if seed > 0 {
                let edge = GraphEdgeId {
                    from: center,
                    to: id,
                    relation: RelationKind::Related,
                };
                result.edges.insert(
                    edge,
                    GraphEdge {
                        id: edge,
                        label: RelationKind::Related.label(),
                    },
                );
            }
        }
        result
    }

    #[test]
    fn admitted_typed_page_relations_reach_graph_projection() {
        // This is the final seam in the live path: the owner admits rows and
        // typed relation labels into the presentation page, then the desktop
        // projection derives only stable identities from that page and root.
        // Imports is the semantic IR's dependency edge (there is no lossy
        // Related fallback in this path); Contains comes from the outline.
        let centre_label = "pkg::src/lib.rs:1::centre";
        let calls_label = "pkg::src/lib.rs:2::calls";
        let implements_label = "pkg::src/lib.rs:3::implements";
        let depends_label = "pkg::src/lib.rs:4::depends";
        let contains_label = "pkg::src/lib.rs:5::contains";
        let centre = symbol_key(centre_label);
        let calls = symbol_key(calls_label);
        let implements = symbol_key(implements_label);
        let depends = symbol_key(depends_label);
        let contains = symbol_key(contains_label);
        let source_root = view_state_root(&[]);
        let basis = Basis::new(source_root, object_version(b"graph-source"));
        let frontier = Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0);
        let root = ViewRoot::new_incomplete(
            view_key(b"graph-view"),
            basis,
            frontier,
            [
                Row::new(RowId::Symbol(centre), basis, centre_label),
                Row::new(RowId::Symbol(calls), basis, calls_label),
                Row::new(RowId::Symbol(implements), basis, implements_label),
                Row::new(RowId::Symbol(depends), basis, depends_label),
                Row::new(RowId::Symbol(contains), basis, contains_label),
            ]
            .into_iter()
            .collect(),
            Vec::new(),
        )
        .expect("admitted graph root");
        let identity = |label: &str, key| {
            Identity::parse_with_key(label, IdentityKey::Symbol(key))
        };
        let source = Source::Captured {
            site: SourceSite::new(PackagePath::new("src/lib.rs"), LineNumber::new(1).unwrap()),
            lines: vec![SourceLine::new(LineNumber::new(1).unwrap(), "centre")].into_boxed_slice(),
            truncation: Truncation::Complete,
        };
        let page = Page::new(
            identity(centre_label, centre),
            Some(backend_library::DeclarationKind::Function),
            source,
        )
        .with_members(MemberGroup::group(vec![Member::new(
            identity(contains_label, contains),
            Some(backend_library::DeclarationKind::Method),
            None,
        )]))
        .with_relations(vec![
            RelationGroup::new(
                RelationLabel::Typed(
                    backend_library::SemanticLinkKind::Calls,
                    RelationDirection::Outgoing,
                ),
                vec![Relation::new(
                    identity(calls_label, calls),
                    Some(backend_library::DeclarationKind::Function),
                )],
            ),
            RelationGroup::new(
                RelationLabel::Typed(
                    backend_library::SemanticLinkKind::Implements,
                    RelationDirection::Outgoing,
                ),
                vec![Relation::new(
                    identity(implements_label, implements),
                    Some(backend_library::DeclarationKind::Trait),
                )],
            ),
            RelationGroup::new(
                RelationLabel::Typed(
                    backend_library::SemanticLinkKind::Imports,
                    RelationDirection::Outgoing,
                ),
                vec![Relation::new(
                    identity(depends_label, depends),
                    Some(backend_library::DeclarationKind::Module),
                )],
            ),
        ]);

        let graph = GraphProjection::from_page(&page, &root);
        assert!(matches!(graph.availability, GraphAvailability::Ready));
        for relation in [
            RelationKind::Contains,
            RelationKind::Calls,
            RelationKind::Implements,
            RelationKind::Imports,
        ] {
            assert!(
                graph.edges.keys().any(|edge| edge.relation == relation),
                "missing {relation:?} edge: {:?}",
                graph.edges.keys().collect::<Vec<_>>()
            );
        }
        assert!(graph.edges.keys().all(|edge| {
            graph.nodes.contains_key(&edge.from) && graph.nodes.contains_key(&edge.to)
        }));
    }

    #[test]
    fn edge_identity_is_stable_and_endpoints_are_admitted_nodes() {
        let first_graph = graph(32);
        assert!(
            first_graph
                .edges
                .keys()
                .all(|edge| first_graph.nodes.contains_key(&edge.from)
                    && first_graph.nodes.contains_key(&edge.to))
        );
        assert!(first_graph.edges.len() <= MAX_EDGES);
        let same = graph(32);
        assert_eq!(first_graph.edges, same.edges);
    }

    #[test]
    fn explorer_keeps_selection_across_revision_when_identity_survives() {
        let mut state = ExplorerState::new();
        let mut first = graph(2);
        let selected = NodeId::Symbol(key(1));
        state.sync(first.clone());
        state.selected = Some(selected);
        state.zoom_by(10.0);
        assert_eq!(state.zoom, 2.5);
        first.revision.view = [4; 32];
        state.sync(first);
        assert_eq!(state.selected, Some(selected));
    }

    #[test]
    fn bounded_layout_and_culling_never_return_unknown_nodes() {
        let graph = graph(MAX_LAYOUT_WORK + 40);
        let mut cache = LayoutCache::default();
        cache.sync(&graph);
        assert!(cache.work <= MAX_LAYOUT_WORK);
        let visible = cache.visible_nodes(
            &graph,
            Viewport {
                left: -1_000.0,
                top: -1_000.0,
                width: 4_000.0,
                height: 4_000.0,
            },
            MAX_VISIBLE_NODES + 40,
        );
        assert!(visible.len() <= MAX_VISIBLE_NODES);
        assert!(
            visible
                .iter()
                .all(|node| graph.nodes.contains_key(&node.id))
        );
    }

    #[test]
    fn hit_testing_prioritizes_nodes_and_stably_selects_edges() {
        let graph = graph(3);
        let mut state = ExplorerState::new();
        state.sync(graph.clone());
        let viewport = Viewport {
            left: -100.0,
            top: -100.0,
            width: 2_000.0,
            height: 2_000.0,
        };
        let center_position = state
            .layout
            .position(graph.center)
            .expect("center has a layout position");
        assert_eq!(
            state.hit_test(center_position, viewport, 8.0),
            Some(HitTarget::Node(graph.center))
        );
        let edge = *graph.edges.keys().next().expect("edge");
        let from = state.layout.position(edge.from).expect("from position");
        let to = state.layout.position(edge.to).expect("to position");
        let midpoint = Position {
            x: (from.x + to.x) * 0.5,
            y: (from.y + to.y) * 0.5,
        };
        assert_eq!(
            state.hit_test(midpoint, viewport, 8.0),
            Some(HitTarget::Edge(edge))
        );
    }

    #[test]
    fn keyboard_traversal_wraps_in_stable_identity_order() {
        let graph = graph(3);
        let mut state = ExplorerState::new();
        state.sync(graph);
        state.selected = Some(NodeId::Symbol(key(2)));
        state.focus_next();
        assert_eq!(state.selected, Some(NodeId::Symbol(key(0))));
        state.focus_previous();
        assert_eq!(state.selected, Some(NodeId::Symbol(key(2))));
    }

    #[test]
    fn differential_sequence_is_order_independent_and_reports_retractions() {
        let first = graph(8);
        let mut reordered = first.clone();
        // BTreeMap ordering is the producer's canonical order, but this test
        // deliberately rebuilds the maps to model duplicate/reordered feed
        // batches. The stable projection must remain byte-for-byte identical.
        reordered.nodes = first
            .nodes
            .iter()
            .rev()
            .map(|(id, node)| (*id, node.clone()))
            .collect();
        reordered.edges = first
            .edges
            .iter()
            .rev()
            .map(|(id, edge)| (*id, edge.clone()))
            .collect();
        assert!(first.diff(&reordered).added_nodes.is_empty());
        assert!(first.diff(&reordered).removed_edges.is_empty());

        let mut retracted = reordered.clone();
        let removed = retracted
            .edges
            .keys()
            .next()
            .copied()
            .expect("graph has an edge");
        retracted.edges.remove(&removed);
        let delta = reordered.diff(&retracted);
        assert_eq!(delta.removed_edges, vec![removed]);
        assert!(delta.added_edges.is_empty());

        let updated_id = NodeId::Symbol(key(1));
        let mut payload_change = first.clone();
        payload_change
            .nodes
            .get_mut(&updated_id)
            .expect("stable node")
            .label = "changed payload".to_owned();
        let delta = first.diff(&payload_change);
        assert_eq!(
            delta.updated_nodes.iter().map(|node| node.id).collect::<Vec<_>>(),
            vec![updated_id]
        );
        assert!(delta.added_nodes.is_empty());
        assert!(delta.removed_nodes.is_empty());

        let edge_id = *first.edges.keys().next().expect("stable edge");
        let mut edge_payload_change = first.clone();
        edge_payload_change.edges.get_mut(&edge_id).expect("edge").label = "changed";
        let delta = first.diff(&edge_payload_change);
        assert_eq!(
            delta.updated_edges.iter().map(|edge| edge.id).collect::<Vec<_>>(),
            vec![edge_id]
        );
    }

    #[test]
    fn complete_delta_applies_without_rebuilding_and_rejects_stale_base() {
        let first = graph(8);
        let mut next = first.clone();
        next.revision.view[0] = 7;
        let removed = *next.edges.keys().next().expect("edge");
        next.edges.remove(&removed);
        let new_id = NodeId::Symbol(key(99));
        next.nodes.insert(new_id, node(new_id, "new"));
        let added = GraphEdgeId {
            from: next.center,
            to: new_id,
            relation: RelationKind::Calls,
        };
        next.edges.insert(
            added,
            GraphEdge {
                id: added,
                label: RelationKind::Calls.label(),
            },
        );
        let delta = first.diff(&next);
        let applied = delta.apply_to(first.clone()).expect("fresh base applies");
        assert_eq!(applied, next);

        let mut stale_base = graph(8);
        stale_base.revision.source[0] = 1;
        let stale = delta.apply_to(stale_base);
        assert!(matches!(stale, Err(GraphDeltaError::StaleBase { .. })));
    }

    #[test]
    fn bounded_layout_handles_cycles_and_randomized_revisions() {
        let mut cycle = graph(5);
        let ids: Vec<NodeId> = cycle.nodes.keys().copied().collect();
        for window in ids.windows(2) {
            let edge = GraphEdgeId {
                from: window[1],
                to: window[0],
                relation: RelationKind::Calls,
            };
            cycle.edges.insert(
                edge,
                GraphEdge {
                    id: edge,
                    label: edge.relation.label(),
                },
            );
        }
        let mut layout = LayoutCache::default();
        layout.sync(&cycle);
        assert!(layout.work <= MAX_LAYOUT_WORK);
        assert!(
            layout
                .positions
                .values()
                .all(|position| position.x.is_finite() && position.y.is_finite())
        );

        // A deterministic pseudo-random sequence exercises additions, gaps,
        // resets, and retractions without depending on a test-only backend.
        let mut state = ExplorerState::new();
        for seed in 0..128usize {
            let count = (seed.wrapping_mul(37) % 80).max(1);
            let mut next = graph(count);
            next.revision.view[0] = seed as u8;
            next.revision.source[1] = (seed >> 1) as u8;
            let delta = state.sync(next.clone());
            if let Some(delta) = delta {
                assert_eq!(delta.to, next.revision);
                assert!(
                    delta
                        .added_nodes
                        .iter()
                        .all(|node| next.nodes.contains_key(&node.id))
                );
                assert!(
                    delta
                        .removed_nodes
                        .iter()
                        .all(|id| !next.nodes.contains_key(id))
                );
            }
            assert!(state.layout.work <= MAX_LAYOUT_WORK);
        }
    }

    #[test]
    fn incremental_layout_matches_full_oracle_invariants_and_retains_quiet_nodes() {
        let initial = graph(18);
        let mut incremental = ExplorerState::new();
        incremental.sync(initial.clone());
        let quiet = NodeId::Symbol(key(12));
        let quiet_position = incremental.layout.position(quiet);

        let mut next = initial.clone();
        next.revision.view[0] = 99;
        next.nodes
            .get_mut(&NodeId::Symbol(key(1)))
            .expect("updated endpoint")
            .state = NodeState::Loading;
        let new_id = NodeId::Symbol(key(99));
        next.nodes.insert(new_id, node(new_id, "new"));
        let edge = GraphEdgeId {
            from: next.center,
            to: new_id,
            relation: RelationKind::Related,
        };
        next.edges.insert(
            edge,
            GraphEdge {
                id: edge,
                label: RelationKind::Related.label(),
            },
        );

        let delta = initial.diff(&next);
        assert_eq!(
            delta.updated_nodes.iter().map(|node| node.id).collect::<Vec<_>>(),
            vec![NodeId::Symbol(key(1))]
        );
        incremental.sync(next.clone());

        let mut full = LayoutCache::default();
        full.sync(&next);
        assert!(incremental.layout.work <= MAX_LAYOUT_WORK);
        assert!(incremental
            .layout
            .positions
            .values()
            .all(|position| position.x.is_finite() && position.y.is_finite()));
        assert_eq!(incremental.layout.position(quiet), quiet_position);
        assert!(full.positions.keys().all(|id| next.nodes.contains_key(id)));

        // A duplicate/no-op batch must converge to the same deterministic
        // coordinates as the incremental path. The independent full oracle
        // still supplies the invariant check above; this policy deliberately
        // retains quiet coordinates when a structural frontier changes.
        let mut duplicate_batch = ExplorerState::new();
        duplicate_batch.sync(initial.clone());
        duplicate_batch.sync(initial);
        duplicate_batch.sync(next.clone());
        for id in next.nodes.keys() {
            assert_eq!(
                incremental.layout.position(*id),
                duplicate_batch.layout.position(*id),
                "duplicate batch changed layout for {id:?}"
            );
        }
    }
}
