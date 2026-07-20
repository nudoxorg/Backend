//! The route table.
//!
//! Splits a **read plane** (search, expand, sessions, health) from a
//! **write/admin plane** (add/get/sync packages) so the two can carry different
//! middleware (body limits, auth, rate limits) and a read never shares a code
//! path with ingest. Both planes share the `Arc<Server<M>>` application state.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
	Router,
	error_handling::HandleErrorLayer,
	extract::{DefaultBodyLimit, MatchedPath, Request},
	http::StatusCode,
	middleware::{self, Next},
	response::Response,
	routing::{get, post},
};
use tower_http::classify::ServerErrorsFailureClass;
use tower_http::trace::TraceLayer;
use tracing::Span;

use registry::vector::EmbeddingModel;
use crate::config::Limits;
use crate::Server;
use crate::http::handlers::{admin, compiled, depshards, health, indexing, rerank as rerank_handler, search};

/// The largest read-plane request body: search/expand requests are JSON control
/// messages plus at most a pasted code snippet.
const READ_PLANE_BODY_CEILING: usize = 2 * 1024 * 1024;

/// The largest write/admin-plane request body. The admin surface accepts only
/// small coordinate JSON, so its ceiling sits *below* the read plane's — the
/// configured `max_request_bytes` can tighten it further but never widen it.
const WRITE_PLANE_BODY_CEILING: usize = 64 * 1024;

/// The body ceiling for `POST /v1/compiled/lookup` (SMOLVM-PLAN §5.1).
///
/// The plan specifies "≤ 256 KiB". Worst case: 1 024 keys × 64 hex bytes +
/// JSON punctuation ≈ 68 KiB; 256 KiB is the stated contract.
const COMPILED_LOOKUP_BODY_CEILING: usize = 256 * 1024;

/// Build the full application router over a shared server handle.
///
/// Layers cross-cutting middleware via `tower`/axum: request tracing on both
/// planes (via [`tower_http::trace::TraceLayer`] — OBSERVABILITY-PLAN.md §6:
/// every route now gets one server span carrying `http.route`,
/// `http.request.method`, and `http.response.status_code` automatically,
/// replacing the previous hand-rolled `trace_request` middleware), per-plane
/// body limits (stricter on the write plane), and a request timeout on the
/// admin mutations. Existing per-handler `#[tracing::instrument]` spans
/// (e.g. `health::readyz`) nest under this request span unchanged — this
/// layer only adds the outer span, it does not replace inner instrumentation.
pub fn router<M: EmbeddingModel>(server: Arc<Server<M>>) -> Router {
	let limits = &server.config().limits;
	Router::new()
		.merge(read_plane().layer(DefaultBodyLimit::max(READ_PLANE_BODY_CEILING)))
		.merge(write_plane(limits))
		.merge(admin_plane(limits))
		.merge(compiled_plane())
		.merge(vector_plane())
		// RED metrics via `route_layer` (not `layer`): it runs *after* route
		// matching, so `MatchedPath` is populated and the `http_route` label is
		// the low-cardinality template (`/symbols/:id`) rather than the raw path
		// — the difference between a bounded metric and a per-id series
		// explosion. It also skips unmatched 404s, the desired RED denominator.
		// The trace layer below stays `.layer` so it still spans unmatched
		// requests.
		.route_layer(middleware::from_fn(record_http_metrics))
		.layer(request_trace_layer())
		.with_state(server)
}

/// RED (Rate / Errors / Duration) HTTP metrics, emitted through the `metrics`
/// facade so they reach both the Prometheus `/metrics` exposition (scraped by
/// VictoriaMetrics as `job="backend"`) and the OTLP pipeline via the fan-out
/// recorder in `workspace/telemetry`.
///
/// Metric + label names are the contract the nixos-side Grafana dashboard and
/// vmalert rules query, so they are fixed here:
///   - counter `http_requests`   → renders `http_requests_total` (the exporter
///     appends `_total`); labels `http_route`, `method`, `status`.
///   - histogram `http_request_duration_seconds` → renders
///     `http_request_duration_seconds_bucket`/`_sum`/`_count` (the telemetry
///     recorder configures `_seconds` buckets); same labels.
///
/// `http_route` comes from `MatchedPath` (the route template) — see the
/// `route_layer` note at the call site for why that is available here.
async fn record_http_metrics(request: Request, next: Next) -> Response {
	let method = request.method().as_str().to_owned();
	let route = request
		.extensions()
		.get::<MatchedPath>()
		.map(|matched| matched.as_str().to_owned())
		.unwrap_or_else(|| request.uri().path().to_owned());
	let started = Instant::now();

	let response = next.run(request).await;

	let status = response.status().as_u16().to_string();
	metrics::counter!(
		"http_requests",
		"http_route" => route.clone(),
		"method" => method.clone(),
		"status" => status.clone(),
	)
	.increment(1);
	metrics::histogram!(
		"http_request_duration_seconds",
		"http_route" => route,
		"method" => method,
		"status" => status,
	)
	.record(started.elapsed().as_secs_f64());

	response
}

/// The `TraceLayer` shared by every route. `make_span_with` opens one span per
/// request, carrying the OTel HTTP semantic-convention field names
/// (`http.route` prefers the *matched* route template over the raw path, so
/// e.g. `/symbols/:id` groups instead of fragmenting per id — the grouping
/// `tracing_opentelemetry` needs to keep span cardinality sane).
/// `on_response`/`on_failure` fill in the status code and latency on that same
/// span once the response is known, so the eventual OTel span carries all the
/// fields the plan asks for on one record rather than scattered log events.
/// Bodies/headers are never logged (no `on_body_chunk` customization),
/// preserving the previous `trace_request`'s guarantee.
#[allow(clippy::type_complexity)]
fn request_trace_layer() -> TraceLayer<
	tower_http::classify::SharedClassifier<tower_http::classify::ServerErrorsAsFailures>,
	impl Fn(&Request) -> Span + Clone,
	tower_http::trace::DefaultOnRequest,
	impl Fn(&Response, Duration, &Span) + Clone,
	tower_http::trace::DefaultOnBodyChunk,
	tower_http::trace::DefaultOnEos,
	impl Fn(ServerErrorsFailureClass, Duration, &Span) + Clone,
> {
	TraceLayer::new_for_http()
		.make_span_with(|request: &Request| {
			let route = request
				.extensions()
				.get::<MatchedPath>()
				.map(MatchedPath::as_str)
				.unwrap_or_else(|| request.uri().path());
			tracing::info_span!(
				"http.server.request",
				"http.route" = %route,
				"http.request.method" = %request.method(),
				"http.response.status_code" = tracing::field::Empty,
				"otel.name" = %format!("{} {}", request.method(), route),
				"otel.kind" = "server",
			)
		})
		.on_response(|response: &Response, latency: Duration, span: &Span| {
			span.record("http.response.status_code", response.status().as_u16());
			tracing::info!(
				parent: span,
				status = response.status().as_u16(),
				elapsed_ms = latency.as_millis() as u64,
				"request served"
			);
		})
		.on_failure(|error: ServerErrorsFailureClass, latency: Duration, span: &Span| {
			tracing::warn!(
				parent: span,
				?error,
				elapsed_ms = latency.as_millis() as u64,
				"request failed"
			);
		})
}

/// The read plane: `/search`, `/search/semantic`, `/packages/search`,
/// `/usages`, `/symbols/:id`, `/expand`, `/sessions/:id`.
fn read_plane<M: EmbeddingModel>() -> Router<Arc<Server<M>>> {
	Router::new()
		.route("/search", post(search::search))
		.route("/search/semantic", post(search::search_semantic))
		.route("/packages/search", post(search::search_packages))
		.route("/usages", post(search::usages))
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

/// The compiled-output plane: `POST /v1/compiled/lookup` (SMOLVM-PLAN §5, SV-6).
///
/// Carries a dedicated body limit of [`COMPILED_LOOKUP_BODY_CEILING`] (256 KiB)
/// per the plan's "control plane, ≤ 256 KiB" statement. No write mutations here;
/// this is a read gate on fleet-produced artifacts.
///
/// Auth: [`Principal`] + `compiled.lookup` read capability. Under the current
/// allow-all policy every request is granted; future enforcement will require a
/// `DownloadGrant`-equivalent token (SV-7).
fn compiled_plane<M: EmbeddingModel>() -> Router<Arc<Server<M>>> {
	Router::new()
		.route("/v1/compiled/lookup", post(compiled::compiled_lookup))
		.layer(DefaultBodyLimit::max(COMPILED_LOOKUP_BODY_CEILING))
}

/// The body ceiling for `POST /v1/rerank` (256 documents × ~512 bytes ≈ 128 KiB
/// plus JSON framing; 256 KiB is a generous ceiling).
const RERANK_BODY_CEILING: usize = 256 * 1024;

/// The vector-plane routes: dep-shard manifest and the rerank surface
/// (09-vector §20.3, §20.8).
///
/// - `GET /v1/depshards/{package}/{version}/manifest` — edgepack manifest
///   (status pending/ready/failed, artifact CAS key, RAM estimate).
/// - `POST /v1/rerank` — cross-encoder reranking for Deep mode queries
///   (§20.8); timeout → explicit `rerank_unavailable` (§20.9).
fn vector_plane<M: EmbeddingModel>() -> Router<Arc<Server<M>>> {
	Router::new()
		.route(
			"/v1/depshards/:package/:version/manifest",
			get(depshards::get_manifest),
		)
		.route("/v1/rerank", post(rerank_handler::rerank))
		.layer(DefaultBodyLimit::max(RERANK_BODY_CEILING))
}

/// The timeout layer's error projection: an admin mutation that outlived
/// `limits.upload_timeout` answers `504` rather than hanging the client.
async fn admission_timed_out(error: tower::BoxError) -> (StatusCode, String) {
	(StatusCode::GATEWAY_TIMEOUT, format!("request timed out: {error}"))
}
