//! The API surface + structure view over the terminus graph.

use std::collections::BTreeMap;

use futures::{Stream, TryFutureExt};
use serde::{Deserialize, Serialize};

use heart::{SymbolId, PackageId, Symbol, SymbolKind};

use crate::{error::GraphError, graph::RelationKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructureNode {
	pub symbol: Symbol,
	pub parent: Option<SymbolId>,
	pub relation: Option<RelationKind>,
	pub kind: SymbolKind,
}

/// Join a package's symbols with its parent-edges into [`StructureNode`]s — the
/// pure assembly step behind [`crate::graph::Graph::structure`], factored out so
/// the shaping is testable without a live terminus. `kind` is copied from the
/// graph's symbol record: the graph is the source of truth, so a consumer
/// holding a stale copy from another store reads the authoritative kind here.
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

impl crate::graph::Graph<heart::Live> {
	pub fn structure(
		&self,
		package: PackageId,
	) -> impl Stream<Item = Result<StructureNode, GraphError>> + Send {
		async move {
			let (symbols, parents) =
				futures::try_join!(self.symbols_in_package(package), self.member_parents(package))?;
			tracing::debug!(%package, symbols = symbols.len(), "package structure fetched");
			Ok(futures::stream::iter(assemble(symbols, &parents).into_iter().map(Ok)))
		}
		.try_flatten_stream()
	}

	pub fn members(
		&self,
		symbol: SymbolId,
	) -> impl Stream<Item = Result<StructureNode, GraphError>> + Send {
		async move {
			// Direct members: targets of Member edges out of `symbol`, hydrated
			// into full records via each one's package document.
			let members = self
				.outgoing_edges(symbol)
				.await?
				.into_iter()
				.filter(|(relation, _)| *relation == RelationKind::Member)
				.map(|(_, member)| member)
				.collect::<Vec<_>>();
			let mut nodes = Vec::with_capacity(members.len());
			for member in members {
				if let Some(record) = self.symbol_record(member).await? {
					let kind = record.kind;
					nodes.push(StructureNode {
						symbol: record,
						parent: Some(symbol),
						relation: Some(RelationKind::Member),
						kind,
					});
				}
			}
			Ok(futures::stream::iter(nodes.into_iter().map(Ok)))
		}
		.try_flatten_stream()
	}

	pub fn implemented_by(
		&self,
		symbol: SymbolId,
	) -> impl Stream<Item = Result<SymbolId, GraphError>> + Send {
		async move {
			let targets = self
				.outgoing_edges(symbol)
				.await?
				.into_iter()
				.filter(|(relation, _)| *relation == RelationKind::Implements)
				.map(|(_, target)| target)
				.collect::<Vec<_>>();
			Ok(futures::stream::iter(targets.into_iter().map(Ok)))
		}
		.try_flatten_stream()
	}
}
