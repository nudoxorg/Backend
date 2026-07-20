//! Property tests pinning the [`Merge`] join-semilattice laws for
//! [`SessionGraph`]. Session merge is meant to be idempotent, commutative, and
//! associative (it is set-union under the hood); these tests make that a checked
//! invariant rather than a doc-comment promise.

use heart::SymbolId;
use proptest::prelude::*;
use runtime::session::{Edge, SessionGraph};
use runtime::session::RelationKind;
use uuid::Uuid;

/// Build a `SessionGraph` from raw ids so proptest can generate arbitrary graphs
/// without depending on the (todo) identity-minting pipeline.
fn graph_from(nodes: Vec<u64>, edges: Vec<(u64, u8, u64)>) -> SessionGraph {
	let sym = |n: u64| SymbolId::from_uuid(Uuid::from_u128(n as u128));
	let kind = |k: u8| match k % 4 {
		0 => RelationKind::Member,
		1 => RelationKind::Reference,
		2 => RelationKind::Occurrence,
		_ => RelationKind::Implements,
	};
	SessionGraph {
		nodes: nodes.into_iter().map(sym).collect(),
		edges: edges
			.into_iter()
			.map(|(f, k, t)| Edge { from: sym(f), kind: kind(k), to: sym(t) })
			.collect(),
	}
}

prop_compose! {
	fn any_graph()(
		nodes in prop::collection::vec(0u64..16, 0..8),
		edges in prop::collection::vec((0u64..16, 0u8..4, 0u64..16), 0..8),
	) -> SessionGraph {
		graph_from(nodes, edges)
	}
}

proptest! {
	/// Idempotent: merging a graph with itself changes nothing.
	#[test]
	fn merge_is_idempotent(g in any_graph()) {
		let mut a = g.clone();
		a.merge(g.clone());
		prop_assert_eq!(a, g);
	}

	/// Commutative: merge order does not matter.
	#[test]
	fn merge_is_commutative(x in any_graph(), y in any_graph()) {
		let mut xy = x.clone();
		xy.merge(y.clone());
		let mut yx = y;
		yx.merge(x);
		prop_assert_eq!(xy, yx);
	}

	/// Associative: grouping does not matter.
	#[test]
	fn merge_is_associative(x in any_graph(), y in any_graph(), z in any_graph()) {
		let mut left = x.clone();
		left.merge(y.clone());
		left.merge(z.clone());

		let mut yz = y;
		yz.merge(z);
		let mut right = x;
		right.merge(yz);

		prop_assert_eq!(left, right);
	}

	/// The empty graph is the identity element (bottom of the lattice).
	#[test]
	fn empty_is_identity(g in any_graph()) {
		let mut a = g.clone();
		a.merge(SessionGraph::empty());
		prop_assert_eq!(a, g);
	}
}
