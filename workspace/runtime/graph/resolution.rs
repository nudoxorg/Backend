//! Cross-version resolution: diffing a package's symbols across two versions so a
//! symbol present in both keeps one stable [`GlobalSymbolId`].
//!
//! [`GlobalSymbolId`] is derived from the instance token + entry URI, so a
//! symbol whose path is unchanged is *already* stable across versions. Real
//! resolution handles the harder cases: a renamed/moved symbol that is "the same
//! thing", and mapping an old occurrence onto its new identity so references
//! survive a version bump.

use futures::Stream;
use serde::{Deserialize, Serialize};

use heart::{AccessContext, Generation, GlobalSymbolId, PackageId, Scored};

/// How an old symbol maps onto the next version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
	/// The symbol is unchanged (same id in both versions).
	Stable(GlobalSymbolId),
	/// The symbol moved/renamed; the old id maps to a new one (scored by match
	/// confidence).
	Moved {
		/// Its identity in the previous version.
		previous: GlobalSymbolId,
		/// Its identity (and match confidence) in the next version.
		next: Scored<GlobalSymbolId>,
	},
	/// The symbol was removed in the next version.
	Removed(GlobalSymbolId),
	/// The symbol is new in the next version.
	Added(GlobalSymbolId),
}

/// Resolves symbol identities across a version diff of one package.
pub trait ResolveAcross: Send + Sync {
	/// The failure mode of a resolution.
	type Error;

	/// Diff `package` between two generations, streaming a [`Resolution`] per
	/// affected symbol.
	fn resolve_across(
		&self,
		package: PackageId,
		previous: Generation,
		next: Generation,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<Resolution, Self::Error>> + Send;
}

impl ResolveAcross for crate::graph::Graph<heart::Live> {
	type Error = crate::error::GraphError;

	fn resolve_across(
		&self,
		package: PackageId,
		previous: Generation,
		next: Generation,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<Resolution, Self::Error>> + Send {
		let _ = (package, previous, next, scope);
		todo!("diff the two generations; map moved/renamed symbols; stream Resolutions");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}
}
