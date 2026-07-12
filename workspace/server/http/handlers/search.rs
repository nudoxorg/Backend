//! Read handlers: precise search (default), gated semantic search, package
//! search, and graph expand. Paged responses use the shared [`heart::Page`]
//! (whose items are `heart::Scored<T>`); the streaming `/search` emits those
//! hits as NDJSON so a large result set never materializes client-side in one
//! frame.

use std::sync::Arc;

use axum::{
	Json,
	body::Body,
	extract::{Path, State},
	http::header,
	response::{IntoResponse, Response},
};
use futures::TryStreamExt;
use heart::{Cursor, Page, Score, Scored, Sourced, Symbol, SymbolId};
use registry::search::SearchKey;
use runtime::session::{SessionGraph, SessionId, SessionStore};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::error::{BadRequestReason, ServerError, ServerResult};
use crate::http::dto::SearchRequestDto;
use crate::search::merge_overlay_first;
use crate::search::query::{LiteralQuery, Pagination, Query};
use crate::search::registry::RegistrySearchSurface;

/// `POST /search` — precise-or-planned symbol search. Streams NDJSON pages of
/// `heart::Scored<Symbol>`.
#[tracing::instrument(skip_all, fields(semantic = req.semantic, limit = %req.limit))]
pub async fn search<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	Json(req): Json<SearchRequestDto>,
) -> ServerResult<Response> {
	let request = req.into_search()?;
	let hits: Vec<Scored<Symbol>> = server.search_symbols(&request).await?.try_collect().await?;
	tracing::debug!(hits = hits.len(), "symbol search served");
	Ok(ndjson(hits))
}

/// `POST /search/semantic` — the explicit semantic opt-in surface. The same
/// pipeline as `/search` with the semantic flag forced on; the planner (and its
/// budget) still owns the final escalation decision.
pub async fn search_semantic<M: EmbeddingModel>(
	state: State<Arc<Server<M>>>,
	Json(mut req): Json<SearchRequestDto>,
) -> ServerResult<Response> {
	req.semantic = true;
	search(state, Json(req)).await
}

/// `POST /packages/search` — registry (package) search.
#[tracing::instrument(skip_all, fields(limit = %req.limit))]
pub async fn search_packages<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	Json(req): Json<SearchRequestDto>,
) -> ServerResult<Json<Page<registry::GlobalPackage>>> {
	server.authorize("search.packages")?;
	let limit = req.limit.get() as usize;
	let query =
		Query::Literal(LiteralQuery::parse(&req.query).map_err(BadRequestReason::from)?);
	let page = Pagination { limit: req.limit, after: req.cursor };

	let mut groups = Vec::new();
	for sourced in server.federation().in_precedence() {
		let surface = RegistrySearchSurface::new(Arc::clone(&sourced.value.packages));
		let hits = surface.search(&query, &page).await?;
		groups.push(hits.try_collect().await?);
	}
	let items = merge_overlay_first(groups, |package| package.id, limit);

	// A full page may have more behind it; the resume token is keyset-anchored
	// at the definitive base's index snapshot.
	let snapshot = server.base().packages.snapshot().await;
	let next = (items.len() == limit)
		.then(|| items.last())
		.flatten()
		.map(|last| Cursor::<SearchKey>::new((last.score, last.value.id), snapshot).encode());
	tracing::debug!(hits = items.len(), "package search served");
	Ok(Json(Page { items, next }))
}

/// `POST /expand` — walk graph relationships from a hit (the request's `query`
/// is the seed symbol's id), merging into the caller's session graph when a
/// `session` is named.
#[tracing::instrument(skip_all, fields(seed = %req.query))]
pub async fn expand<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	Json(req): Json<SearchRequestDto>,
) -> ServerResult<Json<Page<Symbol>>> {
	let seed: uuid::Uuid = req.query.trim().parse().map_err(|e| {
		BadRequestReason::InvalidSymbolId {
			raw: req.query.clone(),
			source: e,
		}
	})?;
	let seed = SymbolId::from_uuid(seed);

	let origin = server.resolve_symbol(seed).await?.ok_or(ServerError::NotFound)?;
	let hit = Scored::new(origin.value, seed_relevance());
	let mut related = server.expand(&hit).await?;
	related.truncate(req.limit.get() as usize);

	if let Some(session) = req.session {
		let mut delta = SessionGraph::empty();
		delta.nodes.insert(seed);
		delta.nodes.extend(related.iter().map(|neighbour| neighbour.value.id));
		server
			.sessions()
			.merge_into(SessionId::from_uuid(session), delta)
			.await
			.map_err(|error| ServerError::Runtime(error.into()))?;
	}
	tracing::debug!(related = related.len(), "expansion served");
	Ok(Json(Page { items: related, next: None }))
}

/// `GET /symbols/:id` — resolve one symbol by its durable id, tagged with the
/// federation source that answered.
#[tracing::instrument(skip_all, fields(symbol = %id))]
pub async fn get_symbol<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	Path(id): Path<uuid::Uuid>,
) -> ServerResult<Json<Sourced<Symbol>>> {
	server
		.resolve_symbol(SymbolId::from_uuid(id))
		.await?
		.map(Json)
		.ok_or(ServerError::NotFound)
}

/// `GET /sessions/:id` — the caller's accumulated exploration graph (created
/// empty on first touch, hydrated from disk when persisted).
#[tracing::instrument(skip_all, fields(session = %id))]
pub async fn get_session<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	Path(id): Path<uuid::Uuid>,
) -> ServerResult<Json<SessionGraph>> {
	let graph = server
		.sessions()
		.open(SessionId::from_uuid(id))
		.await
		.map_err(|error| ServerError::Runtime(error.into()))?;
	Ok(Json(SessionGraph::clone(&graph)))
}

/// The seed hit's own relevance for an expansion: certain.
fn seed_relevance() -> Score {
	Score::try_new(1.0).expect("one is finite")
}

/// Serialize results as NDJSON — one JSON object per line, streamed — under
/// `application/x-ndjson`.
fn ndjson<T: serde::Serialize + Send + 'static>(items: Vec<T>) -> Response {
	let lines = futures::stream::iter(items.into_iter().map(|item| {
		serde_json::to_vec(&item).map(|mut line| {
			line.push(b'\n');
			bytes::Bytes::from(line)
		})
	}));
	([(header::CONTENT_TYPE, "application/x-ndjson")], Body::from_stream(lines)).into_response()
}
