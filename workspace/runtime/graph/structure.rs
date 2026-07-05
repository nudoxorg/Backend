//! The API surface + structure view over the terminus graph.

use futures::Stream;
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

impl crate::graph::Graph<heart::Live> {
	pub fn structure(
		&self,
		package: PackageId,
	) -> impl Stream<Item = Result<StructureNode, GraphError>> + Send {
		let _ = package;
		todo!("WOQL: the package's structural tree");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}

	pub fn members(
		&self,
		symbol: SymbolId,
	) -> impl Stream<Item = Result<StructureNode, GraphError>> + Send {
		let _ = symbol;
		todo!("WOQL: direct members of `symbol`");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}

	pub fn implemented_by(
		&self,
		symbol: SymbolId,
	) -> impl Stream<Item = Result<SymbolId, GraphError>> + Send {
		let _ = symbol;
		todo!("WOQL: traits/interfaces implemented by `symbol`");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}
}
