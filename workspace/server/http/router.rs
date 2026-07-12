//! The route table.
//!
//! Splits a **read plane** (search, expand, sessions, health) from a
//! **write/admin plane** (add/get/sync packages) so the two can carry different
//! middleware (body limits, auth, rate limits) and a read never shares a code
//! path with ingest. Both planes share the `Arc<Server<M>>` application state.

use std::sync::Arc;
use std::time::Instant;

use axum::{
	Router,
	error_handling::HandleErrorLayer,
	extract::{DefaultBodyLimit, Request},
	http::StatusCode,
	middleware::{self, Next},
	response::Response,
	routing::{get, post},
};

use runtime::vector::EmbeddingModel;
use crate::config::Limits;
use crate::Server;
use crate::http::handlers::{admin, health, indexing, search};

/// The largest read-plane request body: search/expand requests are JSON control
/// messages plus at most a pasted code snippet.
const READ_PLANE_BODY_CEILING: usize = 2 * 1024 * 1024;

/// The largest write/admin-plane request body. The admin surface accepts only
/// small coordinate JSON, so its ceiling sits *below* the read plane's — the
/// configured `max_request_bytes` can tighten it further but never widen it.
const WRITE_PLANE_BODY_CEILING: usize = 64 * 1024;

/// Build the full application router over a shared server handle.
///
/// Layers cross-cutting middleware via `tower`/axum: request tracing on both
/// planes, per-plane body limits (stricter on the write plane), and a request
/// timeout on the admin mutations. The vendored `tower-http` build carries no
/// middleware features, so tracing rides a lean `axum::middleware::from_fn`.
pub fn router<M: EmbeddingModel>(server: Arc<Server<M>>) -> Router {
	let limits = &server.config().limits;
	Router::new()
		.merge(read_plane().layer(DefaultBodyLimit::max(READ_PLANE_BODY_CEILING)))
		.merge(write_plane(limits))
		.merge(admin_plane(limits))
		.layer(middleware::from_fn(trace_request))
		.with_state(server)
}

/// The read plane: `/search`, `/search/semantic`, `/packages/search`,
/// `/symbols/:id`, `/expand`, `/sessions/:id`.
fn read_plane<M: EmbeddingModel>() -> Router<Arc<Server<M>>> {
	Router::new()
		.route("/search", post(search::search))
		.route("/search/semantic", post(search::search_semantic))
		.route("/packages/search", post(search::search_packages))
		.route("/expand", post(search::expand))
		.route("/symbols/:id", get(search::get_symbol))
		.route("/sessions/:id", get(search::get_session))
}

/// The write/admin plane: `POST /packages` (add/index), `GET /packages/:id`,
/// `POST /packages/:id/sync`, plus the operational surface (`/healthz`,
/// `/readyz`, `/metrics`). Mutations carry the strictest body limit and the
/// configured upload timeout; the operational probes stay unbounded so a slow
/// backend can never mask its own readiness report.
fn write_plane<M: EmbeddingModel>(limits: &Limits) -> Router<Arc<Server<M>>> {
	let body_ceiling =
		usize::try_from(limits.max_request_bytes).unwrap_or(usize::MAX).min(WRITE_PLANE_BODY_CEILING);
	let mutations = Router::new()
		.route("/packages", post(indexing::add_package))
		.route("/packages/:id", get(indexing::get_package))
		.route("/packages/:id/sync", post(indexing::sync_package))
		.layer(
			tower::ServiceBuilder::new()
				.layer(HandleErrorLayer::new(admission_timed_out))
				.layer(tower::timeout::TimeoutLayer::new(limits.upload_timeout)),
		)
		.layer(DefaultBodyLimit::max(body_ceiling));
	let operations = Router::new()
		.route("/healthz", get(health::livez))
		.route("/readyz", get(health::readyz))
		.route("/metrics", get(health::metrics));
	mutations.merge(operations)
}

/// The admin plane: privileged operations behind [`AdminPrincipal`] extraction.
/// Both routes carry the same body ceiling and upload timeout as write mutations.
fn admin_plane<M: EmbeddingModel>(limits: &Limits) -> Router<Arc<Server<M>>> {
	let body_ceiling =
		usize::try_from(limits.max_request_bytes).unwrap_or(usize::MAX).min(WRITE_PLANE_BODY_CEILING);
	Router::new()
		.route("/admin/packages/:id/verify", post(admin::verify_package))
		.route("/admin/packages/:id/rebuild", post(admin::rebuild_package))
		.layer(
			tower::ServiceBuilder::new()
				.layer(HandleErrorLayer::new(admission_timed_out))
				.layer(tower::timeout::TimeoutLayer::new(limits.upload_timeout)),
		)
		.layer(DefaultBodyLimit::max(body_ceiling))
}

/// The timeout layer's error projection: an admin mutation that outlived
/// `limits.upload_timeout` answers `504` rather than hanging the client.
async fn admission_timed_out(error: tower::BoxError) -> (StatusCode, String) {
	(StatusCode::GATEWAY_TIMEOUT, format!("request timed out: {error}"))
}

/// Cross-cutting request tracing: method, path, status, latency — on every
/// route of both planes. Bodies and headers (which may carry secrets) are
/// never logged.
async fn trace_request(request: Request, next: Next) -> Response {
	let method = request.method().clone();
	let path = request.uri().path().to_owned();
	let started = Instant::now();
	let response = next.run(request).await;
	tracing::info!(
		%method,
		path = %path,
		status = response.status().as_u16(),
		elapsed_ms = started.elapsed().as_millis() as u64,
		"request served"
	);
	response
}
