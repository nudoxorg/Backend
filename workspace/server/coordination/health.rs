//! Health + parse-status coordination.
//!
//! Aggregates per-backend readiness and a package's parse status (from the
//! registry's postgres-backed health + queue) into the projections the health
//! endpoint and load-balancer consult.

use heart::{BackendKind, PackageId, Probeable, ResolutionState};
use crate::registry::error::IndexError;

use registry::runtime::vector::EmbeddingModel;
use crate::error::ServerResult;
use crate::{Server, SourceStores};

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
		let mut degraded = Vec::new();
		for sourced in self.federation().in_precedence() {
			let probes = probe_source(sourced.value).await;
			match crate::registry::health::aggregate(&probes) {
				crate::registry::health::Health::Ready => {}
				crate::registry::health::Health::Degraded(backends) => {
					// A degraded source degrades the federation.
					degraded.extend(backends);
				}
				crate::registry::health::Health::Down => {
					if sourced.role == heart::SourceRole::Definitive {
						// The base is required; without it the server cannot serve.
						return Health::Down;
					}
					// A dead overlay is shed, not fatal: every backend it fronts is
					// reported degraded so operators see it.
					degraded.extend(probes.iter().map(|probe| probe.backend));
				}
			}
		}
		degraded.sort_by_key(|backend| format!("{backend}"));
		degraded.dedup();
		if degraded.is_empty() { Health::Ready } else { Health::Degraded(degraded) }
	}

	/// The parse status of a single package.
	pub async fn parse_status(&self, package: PackageId) -> ServerResult<Option<ResolutionState>> {
		match self.global_store().get_state(package).await {
			Ok(state) => Ok(Some(state)),
			Err(IndexError::NotFound { .. }) => Ok(None),
			Err(error) => Err(crate::registry::RegistryError::from(error).into()),
		}
	}
}

/// Probe one source's five backends concurrently via [`Probeable`].
async fn probe_source<M: EmbeddingModel>(stores: &SourceStores<M>) -> Vec<heart::Probe> {
	let (catalog, object_store, terminus, qdrant, tantivy) = tokio::join!(
		stores.global_store.probe(),
		stores.blobs.probe(),
		stores.graph.probe(),
		stores.semantics.probe(),
		stores.text.probe(),
	);
	vec![catalog, object_store, terminus, qdrant, tantivy]
}
