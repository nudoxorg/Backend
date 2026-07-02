//! Neighbor expansion: walking outward from a starting symbol to build a bounded
//! subgraph, the primitive behind interactive graph exploration.
//!
//! Expansion is always bounded in both **depth** (hops from the origin) and
//! **breadth** (neighbors visited per node), so a pathological hub cannot fan
//! out into an unbounded walk.

use futures::Stream;
use serde::{Deserialize, Serialize};

use heart::{AccessContext, Generation, GlobalSymbolId, Scored};

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

/// One edge discovered during expansion: the reached symbol, how it was reached,
/// its hop-distance from the origin, and its relevance score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpandedEdge {
	/// The symbol reached.
	pub target: Scored<GlobalSymbolId>,
	/// The symbol it was reached from.
	pub via: GlobalSymbolId,
	/// The kind of edge traversed.
	pub relation: RelationKind,
	/// Hops from the origin (1 = direct neighbor).
	pub depth: usize,
}

/// Expands a node's neighbors up to the given bounds, streaming discovered edges
/// in traversal order.
pub trait Expand: Send + Sync {
	/// The failure mode of an expansion.
	type Error;

	/// Walk outward from `origin`, respecting `bounds`, streaming edges as they
	/// are discovered. Access-scoped and generation-pinned.
	fn expand(
		&self,
		origin: GlobalSymbolId,
		bounds: ExpansionBounds,
		generation: Generation,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<ExpandedEdge, Self::Error>> + Send;
}

impl Expand for crate::graph::Graph<heart::Live> {
	type Error = GraphError;

	fn expand(
		&self,
		origin: GlobalSymbolId,
		bounds: ExpansionBounds,
		generation: Generation,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<ExpandedEdge, Self::Error>> + Send {
		let _ = (origin, bounds, generation, scope);
		todo!("BFS from origin, honoring depth + per-node breadth caps, streaming edges");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}
}
