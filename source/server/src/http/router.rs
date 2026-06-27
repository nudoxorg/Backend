use axum::{Json, Router, extract::{Path, Query, State}, http::StatusCode, routing::{delete, get, post}};
use tower_http::trace::TraceLayer;

use super::{AppState, error::{AppError, ConfigError, IngestError}};
use super::dto::{ExpandQuery, HealthResponse, LookupQuery, RunSearchQuery, SearchQuery, SessionQuery, SymbolMatchResponse};
use crate::{registry::{AddPackageOutcome, NewPackageRequest, PackageSnapshot}, search};

pub fn router(state: AppState) -> Router {
	Router::new()
		.route("/healthz", get(health))
		.route("/text-search", get(text_search))
		.route("/search", get(search_docs))
		.route("/terminus_search", get(lookup_symbol))
		.route("/run", get(run_search))
		.route("/expand", get(expand_symbol))
		.route("/session", delete(clear_session))
		.route("/symbol-search", post(symbol_search))
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

async fn text_search(
	State(state): State<AppState>,
	Query(params): Query<SearchQuery>,
) -> Result<Json<search::SearchResponse>, AppError> {
	let index = state
		.targets
		.text_index
		.as_ref()
		.ok_or_else(|| AppError::TextSearchNotConfigured)?;
	let limit = params.limit.unwrap_or(6);
	let hits = index.search(&params.q, limit)?;
	let results = hits
		.into_iter()
		.map(|h| search::SearchResult {
			uri:         h.uri,
			score:       h.score,
			collection:  h.package.clone(),
			fq_name:     h.fq_name,
			language:    Some(h.language),
			package:     Some(h.package),
			version:     h.version,
			symbol_kind: h.symbol_kind,
		})
		.collect();
	Ok(Json(search::SearchResponse { query: params.q, results }))
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
	match state.registry.add_package(request).await? {
		AddPackageOutcome::Created(package) => Ok((StatusCode::ACCEPTED, Json(package))),
		AddPackageOutcome::Existing(package) => Ok((StatusCode::OK, Json(package))),
	}
}

async fn sync_package(
	State(state): State<AppState>,
	Path(id): Path<u64>,
) -> Result<(StatusCode, Json<PackageSnapshot>), AppError> {
	Ok((StatusCode::ACCEPTED, Json(state.registry.sync_package(id).await?)))
}

async fn search_docs(
	State(state): State<AppState>,
	Query(params): Query<SearchQuery>,
) -> Result<Json<search::SearchResponse>, AppError> {
	let response =
		search::semantic_search(&state.pipeline, &params.q, params.limit.unwrap_or(6)).await?;
	Ok(Json(response))
}

enum LookupMode<'a> {
	ByUri(&'a str),
	ByContext { symbol: &'a str, language: &'a str, package: Option<&'a str> },
}

impl<'a> LookupMode<'a> {
	fn from_params(params: &'a LookupQuery) -> Result<Self, AppError> {
		if let Some(uri) = params.q.as_deref().filter(|q| !q.trim().is_empty()) {
			return Ok(Self::ByUri(uri));
		}
		match (params.symbol.as_deref(), params.language.as_deref()) {
			(Some(symbol), Some(language)) => Ok(Self::ByContext {
				symbol,
				language,
				package: params.package.as_deref(),
			}),
			_ => Err(AppError::Config(ConfigError::MissingQueryParams)),
		}
	}
}

async fn lookup_symbol(
	State(state): State<AppState>,
	Query(params): Query<LookupQuery>,
) -> Result<Json<search::LookupResponse>, AppError> {
	let response = match LookupMode::from_params(&params)? {
		LookupMode::ByUri(uri) => search::lookup_symbol(&state.pipeline, uri).await?,
		LookupMode::ByContext { symbol, language, package } => {
			search::lookup_symbol_with_context(&state.pipeline, symbol, language, package).await?
		}
	};
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
		return Err(AppError::Config(ConfigError::EmptySession));
	}
	state.sessions.clear(session).await;
	Ok(StatusCode::NO_CONTENT)
}

async fn symbol_search(
	State(state): State<AppState>,
	Json(query): Json<nudox_core::SymbolQuery>,
) -> Result<Json<Vec<SymbolMatchResponse>>, AppError> {
	if query.body_query.is_some() {
		return Err(AppError::NotImplemented {
			message: "body_query requires embeddings which are not yet configured; \
			          omit body_query and use name_pattern, scope, or kind instead"
				.to_owned(),
		});
	}
	let orch =
		state.targets.orchestrator.as_ref().ok_or_else(|| AppError::SymbolSearchNotConfigured)?;
	let matches = orch.search(&query).await.map_err(|e| {
		AppError::Ingest(IngestError::Pipeline {
			details: format!("orchestrator search failed: {}", e),
		})
	})?;
	Ok(Json(matches.into_iter().map(SymbolMatchResponse::from).collect()))
}
