use std::sync::Arc;

use axum::{Json, Router, extract::{Path, Query, State}, http::StatusCode, routing::{delete, get, post}};
use serde::{Deserialize, Serialize};
use tower_http::trace::TraceLayer;

use crate::{config::PipelineConfig, error::AppError, local_registry::{LocalRegistry, NewPackageRequest, PackageSnapshot}, search::{self, SessionStore}};

#[derive(Clone)]
pub struct AppState {
	pub registry: Arc<LocalRegistry>,
	pub pipeline: PipelineConfig,
	pub sessions: SessionStore,
}

pub fn router(state: AppState) -> Router {
	Router::new()
		.route("/healthz", get(health))
		.route("/search", get(search_docs))
		.route("/terminus_search", get(lookup_symbol))
		.route("/run", get(run_search))
		.route("/expand", get(expand_symbol))
		.route("/session", delete(clear_session))
		.route("/api/packages", get(list_packages).post(add_package))
		.route("/api/packages/{id}", get(get_package))
		.route("/api/packages/{id}/sync", post(sync_package))
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

async fn search_docs(
	State(state): State<AppState>,
	Query(params): Query<SearchQuery>,
) -> Result<Json<search::SearchResponse>, AppError> {
	let response =
		search::semantic_search(&state.pipeline, &params.q, params.limit.unwrap_or(6)).await?;
	Ok(Json(response))
}

async fn lookup_symbol(
	State(state): State<AppState>,
	Query(params): Query<LookupQuery>,
) -> Result<Json<search::LookupResponse>, AppError> {
	let response = search::lookup_symbol(&state.pipeline, &params.q).await?;
	Ok(Json(response))
}

async fn run_search(
	State(state): State<AppState>,
	Query(params): Query<RunSearchQuery>,
) -> Result<Json<search::RunSearchResponse>, AppError> {
	let response = search::run_search(
		&state.pipeline,
		&state.sessions,
		&params.q,
		params.limit.unwrap_or(6),
		params.session.as_deref(),
	)
	.await?;
	Ok(Json(response))
}

async fn expand_symbol(
	State(state): State<AppState>,
	Query(params): Query<ExpandQuery>,
) -> Result<Json<search::ExpandResponse>, AppError> {
	let response = search::expand_symbol(
		&state.pipeline,
		&state.sessions,
		&params.uri,
		params.depth.unwrap_or(2),
		params.breadth.unwrap_or(10),
		params.session.as_deref(),
	)
	.await?;
	Ok(Json(response))
}

async fn clear_session(
	State(state): State<AppState>,
	Query(params): Query<SessionQuery>,
) -> Result<StatusCode, AppError> {
	let session = params.session.trim();
	if session.is_empty() {
		return Err(AppError::Configuration {
			field:   "session",
			message: "value must not be empty".to_owned(),
		});
	}
	state.sessions.clear(session).await;
	Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct HealthResponse {
	status:           &'static str,
	tracked_packages: usize,
}

#[derive(Deserialize)]
struct SearchQuery {
	q:     String,
	limit: Option<usize>,
}

#[derive(Deserialize)]
struct LookupQuery {
	q: String,
}

#[derive(Deserialize)]
struct RunSearchQuery {
	q:       String,
	session: Option<String>,
	limit:   Option<usize>,
}

#[derive(Deserialize)]
struct ExpandQuery {
	uri:     String,
	session: Option<String>,
	depth:   Option<usize>,
	breadth: Option<usize>,
}

#[derive(Deserialize)]
struct SessionQuery {
	session: String,
}
