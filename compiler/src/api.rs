use std::sync::Arc;

use axum::{Json, Router, extract::{Path, State}, http::StatusCode, routing::{get, post}};
use serde::Serialize;
use tower_http::trace::TraceLayer;

use crate::{error::AppError, local_registry::{LocalRegistry, NewPackageRequest, PackageSnapshot}};

#[derive(Clone)]
pub struct AppState {
	pub registry: Arc<LocalRegistry>,
}

pub fn router(state: AppState) -> Router {
	Router::new()
		.route("/healthz", get(health))
		.route("/api/packages", get(list_packages).post(add_package))
		.route("/api/packages/:id", get(get_package))
		.route("/api/packages/:id/sync", post(sync_package))
		.layer(TraceLayer::new_for_http())
		.with_state(state)
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
	Json(HealthResponse {
		status:           "ok",
		tracked_packages: state.registry.package_count().await,
	})
}

async fn list_packages(
	State(state): State<AppState>,
) -> Result<Json<Vec<PackageSnapshot>>, AppError> {
	Ok(Json(state.registry.list_packages().await))
}

async fn get_package(
	State(state): State<AppState>,
	Path(id): Path<u64>,
) -> Result<Json<PackageSnapshot>, AppError> {
	Ok(Json(state.registry.get_package(id).await?))
}

async fn add_package(
	State(state): State<AppState>,
	Json(request): Json<NewPackageRequest>,
) -> Result<(StatusCode, Json<PackageSnapshot>), AppError> {
	let package = state.registry.add_package(request).await?;
	Ok((StatusCode::CREATED, Json(package)))
}

async fn sync_package(
	State(state): State<AppState>,
	Path(id): Path<u64>,
) -> Result<Json<PackageSnapshot>, AppError> {
	Ok(Json(state.registry.sync_package(id).await?))
}

#[derive(Serialize)]
struct HealthResponse {
	status:           &'static str,
	tracked_packages: usize,
}
