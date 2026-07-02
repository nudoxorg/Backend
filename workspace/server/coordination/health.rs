//! Health + parse-status coordination.
//!
//! Aggregates per-backend readiness and a package's parse status (from the
//! registry's postgres-backed health + queue) into the projections the health
//! endpoint and load-balancer consult.

use heart::{BackendKind, ResolutionState, package::PackageId};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::error::ServerResult;

/// The server's aggregate readiness.
#[derive(Debug, Clone)]
pub enum Health {
	/// All backends reachable and migrated.
	Ready,
	/// Serving, but one or more backends are degraded.
	Degraded(Vec<BackendKind>),
	/// Not serving.
	Down,
}

impl<M: EmbeddingModel> Server<M> {
	/// Probe every backing store of **every federated source** concurrently and
	/// fold into one aggregate readiness. A degraded overlay is distinguished from
	/// a degraded definitive base by the caller if needed (via [`Self::federation`]).
	pub async fn health(&self) -> Health {
		for (source_id, _role, _stores) in self.federation().in_precedence() {
			let _ = source_id; // probe each source's stores with tokio::join!, collect Probes
		}
		todo!("join! each source's probes; registry::health::aggregate; fold into Ready/Degraded/Down")
	}

	/// The parse status of a single package.
	pub async fn parse_status(&self, package: PackageId) -> ServerResult<Option<ResolutionState>> {
		let _ = package;
		todo!("read ResolutionState from the global index")
	}
}
