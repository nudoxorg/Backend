//! The initialization flow: ensure a package is present and fresh before a
//! caller serves against it.

use heart::{PackageId, ResolutionState};
use registry::identity::PackageCoordinates;
use serde::{Deserialize, Serialize};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::error::ServerResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Initialized {
	pub package: PackageId,
	pub state: ResolutionState,
	pub enqueued: bool,
}

impl<M: EmbeddingModel> Server<M> {
	pub async fn ensure_initialized(
		&self,
		coordinates: &PackageCoordinates,
	) -> ServerResult<Initialized> {
		let _ = coordinates;
		todo!("resolve id, check state+freshness, enqueue-if-needed transactionally")
	}
}
