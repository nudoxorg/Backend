//! Compiled-output lookup handler: `POST /v1/compiled/lookup`.
//!
//! Implements SMOLVM-PLAN §5 (SV-6): the no-double-compile JobKey handshake.
//! Clients call this endpoint **after sealing** (the JobKey is only derivable
//! once the sealed input set is pinned) and **before booting a VM**, to learn
//! which JobKeys the fleet has already compiled. A hit returns the Pijul channel
//! and tip; the client pulls those changes over iroh and verification happens at
//! import.
//!
//! # Auth gate
//!
//! Requires a [`Principal`] + read capability (`compiled.lookup`). Under the
//! current allow-all policy every authenticated request is granted.
//!
//! # Body limit
//!
//! ≤ 256 KiB, enforced via a per-route [`DefaultBodyLimit`] override in the
//! router (SMOLVM-PLAN §5.1). Worst-case: 1024 keys × 64 hex bytes + JSON
//! framing ≈ 68 KiB; 256 KiB matches the plan's stated ceiling.
//!
//! # Metrics
//!
//! - `compile_lookup_hits` → renders `compile_lookup_hits_total`
//! - `compile_lookup_misses` → renders `compile_lookup_misses_total`

#[allow(unused_imports)]
use crate::server::registry;
use std::sync::Arc;

use axum::{Json, extract::State};
use registry::compiled::CompiledStore;
use registry::vector::EmbeddingModel;

use crate::server::Server;
use crate::server::authz::Principal;
use crate::server::error::ServerResult;
use crate::server::http::dto::{CompiledLookupEntry, CompiledLookupRequest, CompiledLookupResponse};

/// Names passed to the metrics facade. The Prometheus exporter appends
/// `_total` when rendering counters, so these must not include that suffix.
pub(crate) const COMPILE_LOOKUP_HITS_METRIC: &str = "compile_lookup_hits";
pub(crate) const COMPILE_LOOKUP_MISSES_METRIC: &str = "compile_lookup_misses";

/// `POST /v1/compiled/lookup` — batch JobKey → hit/miss + VCS change-set ref.
#[tracing::instrument(skip_all, fields(key_count = req.job_keys.len()))]
pub async fn compiled_lookup<M: EmbeddingModel>(
    State(server): State<Arc<Server<M>>>,
    principal: Principal,
    Json(req): Json<CompiledLookupRequest>,
) -> ServerResult<Json<CompiledLookupResponse>> {
    let _cap = server.authorize_read(&principal, "compiled.lookup")?;

    req.validate()?;

    let store = server.compiled_store();
    let mut results = Vec::with_capacity(req.job_keys.len());

    for key in &req.job_keys {
        let raw = key.clone().into_bytes();
        match store.lookup(&raw).await {
            Ok(registry::compiled::LookupResult::Hit(hit)) => {
                metrics::counter!(COMPILE_LOOKUP_HITS_METRIC).increment(1);
                tracing::debug!(job_key = %key, "compiled lookup: hit");
                results.push(CompiledLookupEntry::hit(key, &hit));
            }
            Ok(registry::compiled::LookupResult::Miss) => {
                metrics::counter!(COMPILE_LOOKUP_MISSES_METRIC).increment(1);
                tracing::debug!(job_key = %key, "compiled lookup: miss");
                results.push(CompiledLookupEntry::miss(key));
            }
            Err(error) => {
                // A single-key backend failure should not abort the whole batch.
                // Downgrade to miss + log; client always falls back to local compile.
                metrics::counter!(COMPILE_LOOKUP_MISSES_METRIC).increment(1);
                tracing::error!(%error, job_key = %key, "compiled store error; treating as miss");
                results.push(CompiledLookupEntry::miss(key));
            }
        }
    }

    tracing::info!(
        key_count = req.job_keys.len(),
        hits = results.iter().filter(|r| r.hit).count(),
        "compiled lookup served"
    );
    Ok(Json(CompiledLookupResponse { results }))
}

#[cfg(test)]
mod tests {
    use super::{COMPILE_LOOKUP_HITS_METRIC, COMPILE_LOOKUP_MISSES_METRIC};

    #[test]
    fn compiled_lookup_counter_names_leave_prometheus_suffixing_to_exporter() {
        assert_eq!(COMPILE_LOOKUP_HITS_METRIC, "compile_lookup_hits");
        assert_eq!(COMPILE_LOOKUP_MISSES_METRIC, "compile_lookup_misses");
        assert!(!COMPILE_LOOKUP_HITS_METRIC.ends_with("_total"));
        assert!(!COMPILE_LOOKUP_MISSES_METRIC.ends_with("_total"));
    }
}
