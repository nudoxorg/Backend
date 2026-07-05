//! The registry-search surface — finding *packages* (not symbols).

use futures::Stream;
use heart::Scored;
use registry::GlobalPackage;

use crate::error::ServerError;
use crate::search::query::{Pagination, Query};

pub struct RegistrySearchSurface {}

impl RegistrySearchSurface {
	pub async fn search(
		&self,
		query: &Query,
		page: &Pagination,
	) -> Result<impl Stream<Item = Result<Scored<GlobalPackage>, ServerError>> + Send, ServerError> {
		let _ = (query, page);
		todo!("delegate to registry::search::RegistrySearch, map errors into ServerError");
		#[allow(unreachable_code)]
		Ok(futures::stream::empty())
	}
}
