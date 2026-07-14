//! The API surface + structure view over the terminus graph.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use heart::{SymbolId, Symbol, SymbolKind};

use crate::runtime::graph::RelationKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructureNode {
	pub symbol: Symbol,
	pub parent: Option<SymbolId>,
	pub relation: Option<RelationKind>,
	pub kind: SymbolKind,
}

/// Join a package's symbols with its parent-edges into [`StructureNode`]s.
/// `kind` is copied from the graph's symbol record: the graph is the source of
/// truth, so a consumer holding a stale copy from another store reads the
/// authoritative kind here.
pub fn assemble(
	symbols: Vec<Symbol>,
	parents: &BTreeMap<SymbolId, (RelationKind, SymbolId)>,
) -> Vec<StructureNode> {
	symbols
		.into_iter()
		.map(|symbol| {
			let (relation, parent) = match parents.get(&symbol.id) {
				Some((relation, parent)) => (Some(*relation), Some(*parent)),
				None => (None, None),
			};
			let kind = symbol.kind;
			StructureNode { symbol, parent, relation, kind }
		})
		.collect()
}

