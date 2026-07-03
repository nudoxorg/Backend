//! Cross-version resolution: diffing a package's symbols across two versions so a
//! symbol present in both keeps one stable [`SymbolId`].
//!
//! [`SymbolId`] is derived from the instance token + entry URI, so a
//! symbol whose path is unchanged is *already* stable across versions. Real
//! resolution handles the harder cases: a renamed/moved symbol that is "the same
//! thing", and mapping an old occurrence onto its new identity so references
//! survive a version bump.

use futures::Stream;
use serde::{Deserialize, Serialize};

use heart::{AccessContext, SymbolId, PackageId, Scored};

/// How an old symbol maps onto the next version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
	/// The symbol is unchanged (same id in both versions).
	Stable(SymbolId),
	/// The symbol moved/renamed; the old id maps to a new one (scored by match
	/// confidence).
	Moved {
		/// Its identity in the previous version.
		previous: SymbolId,
		/// Its identity (and match confidence) in the next version.
		next: Scored<SymbolId>,
	},
	/// The symbol was removed in the next version.
	Removed(SymbolId),
	/// The symbol is new in the next version.
	Added(SymbolId),
}

/// Resolves symbol identities across a version diff of one package.
impl crate::graph::Graph<heart::Live> {
	/// Diff a package across a version bump — from `previous` to `next` (each a
	/// distinct [`PackageId`], since the version is part of the coordinates) —
	/// streaming a [`Resolution`] per affected symbol.
	pub fn resolve_across(
		&self,
		previous: PackageId,
		next: PackageId,
		scope: &AccessContext,
	) -> impl Stream<Item = Result<Resolution, crate::error::GraphError>> + Send {
		let _ = (previous, next, scope);
		todo!("diff the two versions; map moved/renamed symbols; stream Resolutions");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}
}
