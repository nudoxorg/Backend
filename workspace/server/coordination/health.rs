//! Health + parse-status coordination.
//!
//! Aggregates per-backend readiness and a package's parse status (from the
//! registry's postgres-backed health + queue) into the projections the health
//! endpoint and load-balancer consult.

use std::num::NonZeroUsize;
use std::time::Instant;

use futures::StreamExt;
use heart::{BackendKind, PackageId, ResolutionState};
use registry::error::IndexError;
use registry::health::Probe;

use runtime::vector::EmbeddingModel;
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
			match registry::health::aggregate(&probes) {
				registry::health::Health::Ready => {}
				registry::health::Health::Degraded(backends) => {
					// A degraded source degrades the federation.
					degraded.extend(backends);
				}
				registry::health::Health::Down => {
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
			Err(IndexError::NotFound(_)) => Ok(None),
			Err(error) => Err(registry::RegistryError::from(error).into()),
		}
	}
}

/// Probe one source's five backends concurrently.
///
/// The stores expose no dedicated ping (no [`registry::health::Probeable`]
/// impls yet), so each probe is the cheapest read the store's API affords —
/// where "the thing does not exist" is *proof of reachability*, it counts as
/// healthy.
async fn probe_source<M: EmbeddingModel>(stores: &SourceStores<M>) -> Vec<Probe> {
	let (postgres, object_store, terminus, qdrant, tantivy) = tokio::join!(
		probe_postgres(stores),
		probe_object_store(stores),
		probe_terminus(stores),
		probe_qdrant(stores),
		probe_tantivy(stores),
	);
	vec![postgres, object_store, terminus, qdrant, tantivy]
}

/// Time a probe body and shape its verdict.
async fn timed(
	backend: BackendKind,
	body: impl Future<Output = Result<(), String>>,
) -> Probe {
	let started = Instant::now();
	let outcome = body.await;
	Probe {
		backend,
		healthy: outcome.is_ok(),
		latency_ms: u32::try_from(started.elapsed().as_millis()).ok(),
		detail: outcome.err(),
	}
}

async fn probe_postgres<M: EmbeddingModel>(stores: &SourceStores<M>) -> Probe {
	timed(BackendKind::Postgres, async {
		match stores.global_store.get_state(PackageId::from_uuid(uuid::Uuid::nil())).await {
			Ok(_) | Err(IndexError::NotFound(_)) => Ok(()),
			Err(error) => Err(error.to_string()),
		}
	})
	.await
}

async fn probe_object_store<M: EmbeddingModel>(stores: &SourceStores<M>) -> Probe {
	timed(BackendKind::ObjectStore, async {
		let sentinel = heart::ContentHash::of_bytes(b"nudox readiness sentinel");
		match stores.blobs.get_section(sentinel).await {
			Ok(_) | Err(registry::StoreError::NotFound(_)) => Ok(()),
			Err(error) => Err(error.to_string()),
		}
	})
	.await
}

async fn probe_terminus<M: EmbeddingModel>(stores: &SourceStores<M>) -> Probe {
	use runtime::error::GraphError;
	use runtime::graph::GraphStore;

	timed(BackendKind::Terminus, async {
		let nobody = heart::SymbolId::from_uuid(uuid::Uuid::nil());
		match stores.graph.are_related(nobody, nobody).await {
			Ok(_) | Err(GraphError::NotFound) | Err(GraphError::Query { .. }) => Ok(()),
			Err(error) => Err(error.to_string()),
		}
	})
	.await
}

async fn probe_qdrant<M: EmbeddingModel>(stores: &SourceStores<M>) -> Probe {
	use runtime::error::VectorError;

	timed(BackendKind::Qdrant, async {
		// A one-hit zero-vector query: cheap, and gated like every semantic read
		// (the token still comes from the planner module, the one minting home).
		let gate = crate::search::SearchPlanner::extend_across_federation("readiness probe");
		let query = runtime::vector::Embedding::<M>::zeroed();
		match stores.semantics.search(gate, &query, NonZeroUsize::MIN, None).await {
			Ok(_) => Ok(()),
			Err(error @ (VectorError::Connect(_) | VectorError::Transport(_))) => {
				Err(error.to_string())
			}
			Err(_) => Ok(()),
		}
	})
	.await
}

async fn probe_tantivy<M: EmbeddingModel>(stores: &SourceStores<M>) -> Probe {
	timed(BackendKind::Tantivy, async {
		let query = runtime::text::TextQuery::new("readiness");
		let hits = stores.text.search(&query, NonZeroUsize::MIN, None);
		futures::pin_mut!(hits);
		match hits.next().await {
			None | Some(Ok(_)) => Ok(()),
			Some(Err(error)) => Err(error.to_string()),
		}
	})
	.await
}
