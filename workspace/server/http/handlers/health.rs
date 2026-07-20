//! Health/status handlers: liveness, readiness, and the prometheus exposition.

use std::sync::Arc;

use axum::{
	Json,
	extract::State,
	http::StatusCode,
	response::{IntoResponse, Response},
};

use registry::vector::EmbeddingModel;
use crate::Server;
use crate::coordination::health::Health;
use crate::http::dto::HealthDto;

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

/// `GET /metrics` — the prometheus exposition text. The recorder (and its
/// render handle) is installed once by `heart::telemetry::init` in `main`, fanned out
/// to the OTLP pipeline; here we just render it. `503` until it is installed
/// or if some other recorder won the global-recorder race.
pub async fn metrics() -> Response {
	match heart::telemetry::render_prometheus() {
		Some(body) => body.into_response(),
		None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
	}
}
