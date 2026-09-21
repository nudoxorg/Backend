//! Admission checks and errors for root-pinned rich graph data.

use super::{GraphEdgeId, GraphLayoutInput, RICH_GRAPH_SCHEMA_VERSION, RichGraphSnapshot};
use crate::{ProductAdmissionError, ProductText};
use std::collections::BTreeSet;
use std::fmt;

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

impl RichGraphSnapshot {
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
}
