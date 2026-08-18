//! Health + parse-status coordination.
//!
//! Aggregates per-backend readiness and a package's parse status (from the
//! registry's postgres-backed health + queue) into the projections the health
//! endpoint and load-balancer consult.

#[allow(unused_imports)]
use crate::server::registry;
use crate::server::registry::error::IndexError;
use heart::{BackendKind, PackageId, Probeable, ResolutionState};

use crate::server::error::ServerResult;
use crate::server::{Server, SourceStores};
use registry::vector::EmbeddingModel;

/// The server's aggregate readiness.
#[derive(Debug, Clone)]
pub enum Health {
    /// All backends reachable and migrated.
    Ready,
    /// Serving, but one or more backends are degraded.
    Degraded(Vec<BackendKind>),
    /// Not serving; carries the backends that took it down.
    ///
    /// The list is the whole reason this is not a unit variant. `/readyz`
    /// previously answered a `Down` verdict with `{"ready":false,
    /// "degraded":[]}` — "not ready, and nothing is wrong" — because the
    /// impaired set [`Server::health`] had already computed was discarded at
    /// the point it became most useful. An operator reading a 503 needs to
    /// know *which* store is down at least as much as one reading a 200.
    Down(Vec<BackendKind>),
}

impl<M: EmbeddingModel> Server<M> {
    /// Probe every backing store of **every federated source** concurrently and
    /// fold into one aggregate readiness. A degraded overlay is distinguished from
    /// a degraded definitive base by the caller if needed (via [`Self::federation`]).
    pub async fn health(&self) -> Health {
        let mut degraded = Vec::new();
        for sourced in self.federation().in_precedence() {
            let probes = probe_source(sourced.value).await;
            match crate::server::registry::health::aggregate(&probes) {
                crate::server::registry::health::Health::Ready => {}
                crate::server::registry::health::Health::Degraded(backends) => {
                    // A degraded source degrades the federation.
                    degraded.extend(backends);
                }
                crate::server::registry::health::Health::Down => {
                    if sourced.role == heart::SourceRole::Definitive {
                        // The base is required; without it the server cannot serve.
                        // Report which backends took it down: a 503 whose body
                        // names no impaired store sends the reader to guess.
                        let mut down: Vec<BackendKind> = probes
                            .iter()
                            .filter(|probe| !probe.healthy)
                            .map(|probe| probe.backend)
                            .collect();
                        down.sort_by_key(|backend| format!("{backend}"));
                        down.dedup();
                        return Health::Down(down);
                    }
                    // A dead overlay is shed, not fatal: every backend it fronts is
                    // reported degraded so operators see it.
                    degraded.extend(probes.iter().map(|probe| probe.backend));
                }
            }
        }
        degraded.sort_by_key(|backend| format!("{backend}"));
        degraded.dedup();
        if degraded.is_empty() {
            Health::Ready
        } else {
            Health::Degraded(degraded)
        }
    }

    /// The parse status of a single package.
    pub async fn parse_status(&self, package: PackageId) -> ServerResult<Option<ResolutionState>> {
        match self.global_store().get_state(package).await {
            Ok(state) => Ok(Some(state)),
            Err(IndexError::NotFound { .. }) => Ok(None),
            Err(error) => Err(crate::server::registry::RegistryError::from(error).into()),
        }
    }
}

/// Probe one source's backends concurrently via [`Probeable`].
///
/// # Why the process-local text index is probed too
///
/// It used to be excluded, on the reasoning that it "is pure process-local
/// state (opened at assemble); it is not a remote backend". That is true about
/// where it lives and wrong about what readiness means. `/readyz` answers "can
/// this process serve requests", not "are my remote dependencies up", and the
/// symbol-search index is the store every `/search` reads. It is also the store
/// most likely to vanish underneath a *running* process, because it is the only
/// one that lives in a plain directory: on macOS `SourceConfig::data_directory`
/// defaults under `$TMPDIR` (`/var/folders/...`), which the OS periodically
/// purges.
///
/// Measured on macOS, 2026-08-16, against a live `nudox-serve` with `itoa`
/// ingested: deleting the data directory made every `POST /search` answer
/// `503 text index engine error` while `/readyz` kept answering
/// `{"ready":true,"degraded":[]}`. A load balancer would have gone on routing
/// to a replica that could not answer a single query. `TextIndex` had
/// implemented [`Probeable`] the whole time and nothing ever called it — the
/// dead-impl tell (docs/LIMITATIONS.md L35).
///
/// The probe deliberately does **not** treat an *empty* index as unhealthy: a
/// server that has ingested nothing yet is genuinely ready to accept ingest,
/// and zero hits from an empty corpus is an honest answer rather than a
/// degraded one. What it catches is the index erroring, which is what a purged
/// or corrupt directory actually does.
async fn probe_source<M: EmbeddingModel>(stores: &SourceStores<M>) -> Vec<heart::Probe> {
    let (catalog, object_store, qdrant, text) = tokio::join!(
        stores.global_store.probe(),
        stores.blobs.probe(),
        stores.semantics.probe(),
        stores.text.probe(),
    );
    vec![catalog, object_store, qdrant, text]
}
