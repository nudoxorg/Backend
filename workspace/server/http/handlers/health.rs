//! Health/status handlers: liveness, readiness, and per-package parse status.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::http::dto::HealthDto;

/// `GET /healthz` — liveness: the process is up. Always `200` if reachable.
pub async fn livez() -> StatusCode { StatusCode::OK }

/// `GET /readyz` — readiness: every backing store is reachable/migrated.
pub async fn readyz<M: EmbeddingModel>(State(server): State<Arc<Server<M>>>) -> (StatusCode, Json<HealthDto>) {
	let _ = server;
	todo!("Server::health -> HealthDto + 200/503")
}
