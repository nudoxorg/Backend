//! Cross-version resolution: diffing a package's symbols across two versions so a
//! symbol present in both keeps one stable [`SymbolId`].

use futures::Stream;
use serde::{Deserialize, Serialize};

use heart::{SymbolId, PackageId, Scored};

/// How an old symbol maps onto the next version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
	Stable(SymbolId),
	Moved { previous: SymbolId, next: Scored<SymbolId> },
	Removed(SymbolId),
	Added(SymbolId),
}

impl crate::graph::Graph<heart::Live> {
	pub fn resolve_across(
		&self,
		previous: PackageId,
		next: PackageId,
	) -> impl Stream<Item = Result<Resolution, crate::error::GraphError>> + Send {
		let _ = (previous, next);
		todo!("diff the two versions; map moved/renamed symbols; stream Resolutions");
		#[allow(unreachable_code)]
		futures::stream::empty()
	}
}
