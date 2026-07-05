//! Neighbor expansion: walking outward from a starting symbol to build a
//! bounded subgraph, the primitive behind interactive graph exploration.

use futures::Stream;
use heart::{SymbolId, Scored};
use serde::{Deserialize, Serialize};

use crate::{error::GraphError, graph::RelationKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpansionBounds {
	pub depth: std::num::NonZeroUsize,
	pub breadth: std::num::NonZeroUsize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpandedEdge {
	pub target:   Scored<SymbolId>,
	pub via:      SymbolId,
	pub relation: RelationKind,
	pub depth:    std::num::NonZeroUsize,
}

impl crate::graph::Graph<heart::Live> {
	pub fn expand(
		&self,
		origin: SymbolId,
		bounds: ExpansionBounds,
	) -> impl Stream<Item = Result<ExpandedEdge, GraphError>> + Send {
		let _ = (origin, bounds);
		todo!("BFS from origin, honoring depth + per-node breadth caps, streaming edges");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}
}
