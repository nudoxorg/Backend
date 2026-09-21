//! Versioned rich semantic graph facts shared by every local surface.
//!
//! The graph renderer is deliberately not the owner of this module. A graph
//! is an immutable, root-pinned projection of compiler and package evidence;
//! desktop, CLI, and MCP receive the same nodes, edges, provenance, and
//! availability states. Identities are content derived and independent of
//! display labels, while bounded delta and page operations make refreshes
//! safe for a long-lived client.

mod builder;
mod delta;
mod identity;
mod model;
mod query;
mod validation;

#[cfg(test)]
mod tests;

/// Wire and content schema for the rich graph contract.
pub const RICH_GRAPH_SCHEMA_VERSION: u16 = 1;
/// Maximum records changed by one graph delta.
pub const MAX_RICH_GRAPH_DELTA_RECORDS: usize = 1_024;
/// Maximum nodes returned by one graph page.
pub const MAX_RICH_GRAPH_PAGE_ROWS: u16 = 200;
/// Maximum endpoint edges returned by one graph page.
pub const MAX_RICH_GRAPH_PAGE_EDGES: usize = 1_024;

pub use builder::RichGraphBuilder;
pub use delta::RichGraphDelta;
pub use identity::{GraphAuthority, GraphEdgeId, GraphEdgeKind, GraphNodeId, GraphRelationFamily};
pub use model::{
    GraphAvailability, GraphLayoutEdge, GraphLayoutInput, GraphProvenance, RichGraphEdge,
    RichGraphNode, RichGraphRevision, RichGraphSnapshot,
};
pub use query::{
    GraphControl, GraphPageTerminal, RichGraphCursor, RichGraphPage, RichGraphRequest,
};
pub use validation::RichGraphError;
