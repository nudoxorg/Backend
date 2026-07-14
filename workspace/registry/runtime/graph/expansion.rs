//! Neighbor expansion: walking outward from a starting symbol to build a
//! bounded subgraph, the primitive behind interactive graph exploration.

use std::collections::BTreeSet;

use futures::{Future, Stream, TryFutureExt};
use heart::{SymbolId, Scored};
use serde::{Deserialize, Serialize};

use crate::runtime::{error::GraphError, graph::RelationKind};

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

/// The pure BFS behind [`crate::runtime::graph::Graph::expand`], generic over how a
/// node's outgoing edges are fetched so it is testable without a live graph.
///
/// Honors the bounds exactly: at most `bounds.depth` levels, at most
/// `bounds.breadth` edges taken per expanded node. A node reached twice is
/// reported each time (they are distinct edges) but only expanded once, and
/// each edge's score decays with depth (`1 / depth`) so nearer neighbors rank
/// higher.
pub async fn expand_via<Fetch, Reached, Error>(
	origin: SymbolId,
	bounds: ExpansionBounds,
	mut neighbors: Fetch,
) -> Result<Vec<ExpandedEdge>, Error>
where
	Fetch: FnMut(SymbolId) -> Reached,
	Reached: Future<Output = Result<Vec<(RelationKind, SymbolId)>, Error>>,
{
	let mut visited = BTreeSet::from([origin]);
	let mut frontier = vec![origin];
	let mut edges = Vec::new();
	for depth in 1..=bounds.depth.get() {
		let depth = std::num::NonZeroUsize::new(depth).expect("range starts at 1");
		let score = heart::Score::try_new(1.0 / depth.get() as f32)
			.expect("reciprocal of a positive depth is finite");
		let mut next = Vec::new();
		for via in std::mem::take(&mut frontier) {
			for (relation, target) in neighbors(via).await?.into_iter().take(bounds.breadth.get()) {
				edges.push(ExpandedEdge { target: Scored::new(target, score), via, relation, depth });
				if visited.insert(target) {
					next.push(target);
				}
			}
		}
		if next.is_empty() {
			break;
		}
		frontier = next;
	}
	Ok(edges)
}

impl crate::runtime::graph::Graph<heart::Live> {
	pub fn expand(
		&self,
		origin: SymbolId,
		bounds: ExpansionBounds,
	) -> impl Stream<Item = Result<ExpandedEdge, GraphError>> + Send {
		async move {
			let edges = expand_via(origin, bounds, |node| self.outgoing_edges(node)).await?;
			tracing::debug!(%origin, edges = edges.len(), "expansion completed");
			Ok(futures::stream::iter(edges.into_iter().map(Ok)))
		}
		.try_flatten_stream()
	}
}
