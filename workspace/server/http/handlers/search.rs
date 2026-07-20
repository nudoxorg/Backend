//! Read handlers: precise symbol search (default), gated semantic search,
//! package search, usage lookup, and graph expand.
//!
//! Every read handler deserializes the one query algebra
//! ([`heart::query::Query`]) directly — the wire *is* the domain (INDEX-PLAN
//! §9). There is no request DTO wrapper and no handler-side cursor: pagination
//! lives inside the engine, after ranking. Paged responses use the shared
//! [`heart::Page`] (whose items are `heart::Scored<T>` for the ranked
//! surfaces); the streaming `/search` emits its hits as NDJSON so a large
//! result set never materializes client-side in one frame.

use std::sync::Arc;

use axum::{
	Json,
	body::Body,
	extract::{Path, State},
	http::header,
	response::{IntoResponse, Response},
};
use futures::TryStreamExt;
use heart::query::{Query, QueryMode, Target};
use heart::{Page, Score, Scored, Sourced, Symbol, SymbolId};
use registry::runtime::session::{SessionGraph, SessionId, SessionStore};

use registry::runtime::vector::EmbeddingModel;
use crate::Server;
use crate::authz::Principal;
use crate::error::{BadRequestReason, ServerError, ServerResult};
use crate::registry::search::usages::Usage;
use crate::search::query::{
	AbstractQuery, Filter, LiteralQuery, PackageSelector, Query as ExecutionQuery, Search,
};

/// `POST /search` — precise-or-planned symbol search. Streams NDJSON pages of
/// `heart::Scored<Symbol>`.
///
/// The wire query must carry `target: "Symbols"`. `mode: "semantic"` opts into
/// the planned semantic surface (the planner still owns the escalation).
#[tracing::instrument(skip_all, fields(mode = ?query.mode, limit = query.page.limit))]
pub async fn search<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	principal: Principal,
	Json(query): Json<Query>,
) -> ServerResult<Response> {
	let cap = server.authorize_read(&principal, "search.symbols")?;
	if query.target != Target::Symbols {
		return Err(BadRequestReason::MissingField { field: "target must be Symbols for /search" }.into());
	}
	let request = lower_to_symbol_search(&server, query)?;
	let hits: Vec<Scored<Symbol>> = server.search_symbols(&cap, &request).await?.try_collect().await?;
	tracing::debug!(hits = hits.len(), "symbol search served");
	Ok(ndjson(hits))
}

/// `POST /search/semantic` — the explicit semantic opt-in surface. The same
/// pipeline as `/search` with the semantic mode forced on; the planner (and its
/// budget) still owns the final escalation decision.
pub async fn search_semantic<M: EmbeddingModel>(
	state: State<Arc<Server<M>>>,
	principal: Principal,
	Json(mut query): Json<Query>,
) -> ServerResult<Response> {
	query.mode = QueryMode::Semantic;
	search(state, principal, Json(query)).await
}

/// `POST /packages/search` — registry (package) search. Pagination is entirely
/// inside the engine; the handler just forwards the wire query.
#[tracing::instrument(skip_all, fields(limit = query.page.limit))]
pub async fn search_packages<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	principal: Principal,
	Json(query): Json<Query>,
) -> ServerResult<Json<Page<crate::registry::GlobalPackage>>> {
	let cap = server.authorize_read(&principal, "search.packages")?;
	let page = server.search_packages(&cap, &query).await?;
	Ok(Json(page))
}

/// `POST /usages` — every recorded use of one symbol (`target: Usages { of }`),
/// from the reverse `occ` index (INDEX-PLAN §5.5).
///
/// The reverse-position projection is WS5's deliverable; until it lands this
/// returns a typed `501 Not Implemented`, never a silent empty page.
#[tracing::instrument(skip_all, fields(limit = query.page.limit))]
pub async fn usages<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	principal: Principal,
	Json(query): Json<Query>,
) -> ServerResult<Json<Page<Usage>>> {
	let cap = server.authorize_read(&principal, "search.usages")?;
	if !matches!(query.target, Target::Usages { .. }) {
		return Err(BadRequestReason::MissingField {
			field: "target must be Usages { of } for /usages",
		}
		.into());
	}
	let page = server.usages(&cap, &query).await?;
	Ok(Json(page))
}

/// `POST /expand` — walk graph relationships from a hit (the wire query's `text`
/// is the seed symbol's id), merging into the caller's session graph when
/// `session` is set.
#[tracing::instrument(skip_all, fields(seed = %query.text))]
pub async fn expand<M: EmbeddingModel>(
	State(server): State<Arc<Server<M>>>,
	principal: Principal,
	Json(query): Json<Query>,
) -> ServerResult<Json<Page<Symbol>>> {
	let cap = server.authorize_read(&principal, "search.expand")?;
	let seed: uuid::Uuid = query.text.trim().parse().map_err(|e| {
		BadRequestReason::InvalidSymbolId {
			raw: query.text.clone(),
			source: e,
		}
	})?;
	let seed = SymbolId::from_uuid(seed);

	let origin = server.resolve_symbol(&cap, seed).await?.ok_or(ServerError::NotFound)?;
	let hit = Scored::new(origin.value, seed_relevance());
	let mut related = server.expand(&cap, &hit).await?;
	related.truncate((query.page.limit as usize).max(1));

	if let Some(session) = query.session {
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
	principal: Principal,
	Path(id): Path<uuid::Uuid>,
) -> ServerResult<Json<Sourced<Symbol>>> {
	let cap = server.authorize_read(&principal, "search.resolve_symbol")?;
	server
		.resolve_symbol(&cap, SymbolId::from_uuid(id))
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

/// Lower a wire [`Query`] (target `Symbols`) into the internal execution
/// [`Search`]: a `Semantic` mode becomes an [`AbstractQuery`] (which only the
/// planner may escalate); everything else is a validated, operator-escaped
/// literal. The wire `scope` (ecosystems + package stems) becomes the execution
/// [`Filter`]; the wire `page` is carried through as-is.
fn lower_to_symbol_search<M: EmbeddingModel>(
	server: &Server<M>,
	query: Query,
) -> Result<Search<'static>, ServerError> {
	let _ = server; // reserved for future ecosystem-default resolution.
	use strum::IntoEnumIterator;

	let execution_query = if query.mode == QueryMode::Semantic {
		ExecutionQuery::Abstract(AbstractQuery::NaturalLanguage(query.text.clone()))
	} else {
		ExecutionQuery::Literal(LiteralQuery::parse(&query.text).map_err(BadRequestReason::from)?)
	};

	// Package names are ecosystem-scoped; a bare stem is tried against every
	// requested (or, unstated, every known) ecosystem's grammar.
	let candidates: Vec<heart::Language> = if query.scope.ecosystems.is_empty() {
		heart::Language::iter().collect()
	} else {
		query.scope.ecosystems.clone()
	};
	let mut selectors = Vec::new();
	for raw in &query.scope.packages {
		for &ecosystem in &candidates {
			if let Ok(name) = crate::registry::package::PackageName::new(ecosystem, raw.as_str()) {
				selectors.push(PackageSelector { name, version: None });
			}
		}
	}
	if !query.scope.packages.is_empty() && selectors.is_empty() {
		return Err(BadRequestReason::NoValidPackageSelectors.into());
	}

	Ok(Search {
		query: execution_query,
		filter: Filter {
			ecosystems: nonempty::NonEmpty::from_vec(query.scope.ecosystems),
			packages: nonempty::NonEmpty::from_vec(selectors),
		},
		page: query.page,
		_lifetime: std::marker::PhantomData,
	})
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
