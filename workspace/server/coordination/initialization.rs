//! The initialization flow: ensure a package is present and fresh before a
//! caller serves against it.
//!
//! Idempotent by construction — identity is the deterministic [`PackageId`], so
//! a duplicate request returns the existing state rather than re-enqueuing.

use heart::{AccessContext, PackageCoordinates, ResolutionState, package::PackageId};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::error::ServerResult;

/// The outcome of ensuring a package is initialized.
pub struct Initialized {
	/// The deterministic id the request resolved to.
	pub package: PackageId,
	/// Its current lifecycle state.
	pub state: ResolutionState,
	/// Whether this call enqueued new work (vs finding it already present).
	pub enqueued: bool,
}

impl<M: EmbeddingModel> Server<M> {
	/// Ensure a package is indexed and fresh:
	/// 1. mint its [`PackageId`] from coordinates;
	/// 2. read its state from the global index;
	/// 3. if absent or stale (freshness check), enqueue an indexing job in the
	///    same transaction that records the state transition;
	/// 4. return the current state (never blocks on completion).
	pub async fn ensure_initialized(
		&self,
		coordinates: &PackageCoordinates,
		ctx: &AccessContext,
	) -> ServerResult<Initialized> {
		let _ = (coordinates, ctx);
		todo!("resolve id, check state+freshness, enqueue-if-needed transactionally")
	}
}
