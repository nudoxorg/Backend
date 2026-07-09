//! Health/status handlers: liveness, readiness, and the prometheus exposition.

use std::sync::{Arc, OnceLock};

use axum::{
	Json,
	extract::State,
	http::StatusCode,
	response::{IntoResponse, Response},
};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::coordination::health::Health;
use crate::http::dto::HealthDto;

/// The process-wide prometheus handle `/metrics` renders from.
static PROMETHEUS: OnceLock<PrometheusHandle> = OnceLock::new();

/// Install the prometheus recorder behind `/metrics`. Idempotent: a second
/// assembly (tests, restarts within one process) keeps the first handle; a
/// recorder installed elsewhere leaves `/metrics` answering `503` rather than
/// panicking the server up.
pub fn install_prometheus() {
	if PROMETHEUS.get().is_some() {
		return;
	}
	match PrometheusBuilder::new().install_recorder() {
		Ok(handle) => drop(PROMETHEUS.set(handle)),
		Err(error) => {
			tracing::warn!(%error, "prometheus recorder unavailable; /metrics will answer 503");
		}
	}
}

/// `GET /healthz` — liveness: the process is up. Always `200` if reachable.
pub async fn livez() -> StatusCode { StatusCode::OK }

/// `GET /readyz` — readiness: every backing store is reachable/migrated.
/// `200` while the federation serves (degraded overlays are reported, not
/// fatal); `503` once the definitive base is down.
#[tracing::instrument(skip_all)]
pub async fn readyz<M: EmbeddingModel>(State(server): State<Arc<Server<M>>>) -> (StatusCode, Json<HealthDto>) {
	let (status, health) = match server.health().await {
		Health::Ready => (StatusCode::OK, HealthDto { ready: true, degraded: Vec::new() }),
		Health::Degraded(degraded) => {
			tracing::warn!(?degraded, "serving degraded");
			(StatusCode::OK, HealthDto { ready: true, degraded })
		}
		Health::Down => {
			(StatusCode::SERVICE_UNAVAILABLE, HealthDto { ready: false, degraded: Vec::new() })
		}
	};
	(status, Json(health))
}

/// `GET /metrics` — the prometheus exposition text.
pub async fn metrics() -> Response {
	match PROMETHEUS.get() {
		Some(handle) => handle.render().into_response(),
		None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
	}
}
