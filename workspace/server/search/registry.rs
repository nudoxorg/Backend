//! The registry-search surface — finding *packages* (not symbols), backed by
//! `registry::search` (replica-local tantivy polled from postgres).

use futures::Stream;
use heart::{AccessContext, Scored};
use registry::GlobalPackage;

use crate::error::ServerError;
use crate::search::query::{Page, Query};

/// The read-side entry point for package discovery.
pub struct RegistrySearchSurface {
	// TODO: borrowed handle to registry::search::RegistrySearch.
}

impl RegistrySearchSurface {
	/// Search for packages, streaming scored [`GlobalPackage`] hits, keyset-paged
	/// and access-scoped.
	pub async fn search(
		&self,
		query: &Query,
		page: &Page,
		scope: &AccessContext,
	) -> Result<impl Stream<Item = Result<Scored<GlobalPackage>, ServerError>> + Send, ServerError> {
		let _ = (query, page, scope);
		todo!("delegate to registry::search::RegistrySearch, map errors into ServerError");
		#[allow(unreachable_code)]
		Ok(futures::stream::empty())
	}
}
