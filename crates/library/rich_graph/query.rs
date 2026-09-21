//! Root-pinned cursor pagination and cooperative cancellation.

use super::{
    GraphNodeId, GraphRelationFamily, MAX_RICH_GRAPH_PAGE_EDGES, MAX_RICH_GRAPH_PAGE_ROWS,
    RICH_GRAPH_SCHEMA_VERSION, RichGraphError, RichGraphRevision, RichGraphSnapshot,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

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

impl RichGraphSnapshot {
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
