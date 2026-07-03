//! Neighbor expansion: walking outward from a starting symbol to build a
//! bounded subgraph, the primitive behind interactive graph exploration.
//!
//! Expansion is always bounded in both **depth** (hops from the origin) and
//! **breadth** (neighbors visited per node), so a pathological hub cannot fan
//! out into an unbounded walk.

use futures::Stream;
use heart::{AccessContext, SymbolId, Scored};
use serde::{Deserialize, Serialize};

use crate::{error::GraphError, graph::RelationKind};

/// The bounds on a neighbor walk. Both are required — an unbounded expansion is
/// unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpansionBounds {
	/// Maximum hops from the origin symbol.
	pub depth: std::num::NonZeroUsize,

	/// Maximum neighbors expanded per node (top-scored kept).
	pub breadth: std::num::NonZeroUsize,
}

/// One edge discovered during expansion: the reached symbol, how it was
/// reached, its hop-distance from the origin, and its relevance score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpandedEdge {
	/// The symbol reached.
	pub target:   Scored<SymbolId>,
	/// The symbol it was reached from.
	pub via:      SymbolId,
	/// The kind of edge traversed.
	pub relation: RelationKind,
	/// Hops from the origin (1 = direct neighbor).
	pub depth:    std::num::NonZeroUsize,
}

impl crate::graph::Graph<heart::Live> {
	/// Walk outward from `origin`, respecting `bounds`, streaming edges as they
	/// are discovered. Access-scoped.
	///
	/// Expands a node's neighbors up to the given bounds, streaming discovered
	/// edges in traversal order.
	pub fn expand(
		&self,
		origin: SymbolId,
		bounds: ExpansionBounds,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<ExpandedEdge, GraphError>> + Send {
		let _ = (origin, bounds, scope);
		todo!("BFS from origin, honoring depth + per-node breadth caps, streaming edges");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}
}
