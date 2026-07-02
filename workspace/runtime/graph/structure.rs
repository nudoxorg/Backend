//! The API surface + structure view over the terminus graph — terminus is the
//! source of truth for how a package's symbols are structured and related.
//!
//! Where the text/vector indexes and the graph disagree about an item's shape,
//! the graph wins: this module reads the authoritative structural tree.

use futures::Stream;
use serde::{Deserialize, Serialize};

use heart::{AccessContext, Generation, GlobalSymbolId, PackageId, Symbol, SymbolKind};

use crate::{error::GraphError, graph::RelationKind};

/// One node in a package's structural tree: the symbol plus how it nests under
/// its parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructureNode {
	/// The symbol at this node.
	pub symbol: Symbol,
	/// Its parent in the structure tree, if any (root modules have none).
	pub parent: Option<GlobalSymbolId>,
	/// How this node relates to its parent (`Member`, `Implements`, ...).
	pub relation: Option<RelationKind>,
	/// The kind, hoisted for cheap filtering without loading the full symbol.
	pub kind: SymbolKind,
}

/// Reads the authoritative structural/API-surface shape of a package out of the
/// graph store.
pub trait StructureView: Send + Sync {
	/// The failure mode of a structural read.
	type Error;

	/// Stream the package's full structural tree (modules → types → members) at
	/// the given generation, access-scoped.
	fn structure(
		&self,
		package: PackageId,
		generation: Generation,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<StructureNode, Self::Error>> + Send;

	/// The direct members of a single symbol (a type's methods/fields, a module's
	/// items), streamed.
	fn members(
		&self,
		symbol: GlobalSymbolId,
		generation: Generation,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<StructureNode, Self::Error>> + Send;

	/// The traits/interfaces a type implements, streamed.
	fn implemented_by(
		&self,
		symbol: GlobalSymbolId,
		generation: Generation,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<GlobalSymbolId, Self::Error>> + Send;
}

impl StructureView for crate::graph::Graph<heart::Live> {
	type Error = GraphError;

	fn structure(
		&self,
		package: PackageId,
		generation: Generation,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<StructureNode, Self::Error>> + Send {
		let _ = (package, generation, scope);
		todo!("WOQL: the package's structural tree at `generation`");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}

	fn members(
		&self,
		symbol: GlobalSymbolId,
		generation: Generation,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<StructureNode, Self::Error>> + Send {
		let _ = (symbol, generation, scope);
		todo!("WOQL: direct members of `symbol`");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}

	fn implemented_by(
		&self,
		symbol: GlobalSymbolId,
		generation: Generation,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<GlobalSymbolId, Self::Error>> + Send {
		let _ = (symbol, generation, scope);
		todo!("WOQL: traits/interfaces implemented by `symbol`");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}
}
