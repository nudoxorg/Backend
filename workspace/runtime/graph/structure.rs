//! The API surface + structure view over the terminus graph — terminus is the
//! source of truth for how a package's symbols are structured and related.
//!
//! Where the text/vector indexes and the graph disagree about an item's shape,
//! the graph wins: this module reads the authoritative structural tree.

use futures::Stream;
use serde::{Deserialize, Serialize};

use heart::{AccessContext, SymbolId, PackageId, Symbol, SymbolKind};

use crate::{error::GraphError, graph::RelationKind};

/// One node in a package's structural tree: the symbol plus how it nests under
/// its parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructureNode {
	/// The symbol at this node.
	pub symbol: Symbol,
	/// Its parent in the structure tree, if any (root modules have none).
	pub parent: Option<SymbolId>,
	/// How this node relates to its parent (`Member`, `Implements`, ...).
	pub relation: Option<RelationKind>,
	/// The kind, hoisted for cheap filtering without loading the full symbol.
	pub kind: SymbolKind,
}

/// Reads the authoritative structural/API-surface shape of a package out of the
/// graph store.
impl crate::graph::Graph<heart::Live> {
	/// Stream the package's full structural tree (modules → types → members),
	/// access-scoped.
	pub fn structure(
		&self,
		package: PackageId,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<StructureNode, GraphError>> + Send {
		let _ = (package, scope);
		todo!("WOQL: the package's structural tree");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}

	/// The direct members of a single symbol (a type's methods/fields, a module's
	/// items), streamed.
	pub fn members(
		&self,
		symbol: SymbolId,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<StructureNode, GraphError>> + Send {
		let _ = (symbol, scope);
		todo!("WOQL: direct members of `symbol`");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}

	/// The traits/interfaces a type implements, streamed.
	pub fn implemented_by(
		&self,
		symbol: SymbolId,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<SymbolId, GraphError>> + Send {
		let _ = (symbol, scope);
		todo!("WOQL: traits/interfaces implemented by `symbol`");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}
}
