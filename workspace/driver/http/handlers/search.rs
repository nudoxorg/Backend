//! Read handlers: symbol search (gated semantic is the live path; precise
//! answers empty after the symbol-tantivy drop), package search, usage lookup,
//! and graph expand.
//!
//! Every read handler deserializes the one query algebra
//! ([`heart::query::Query`]) directly — the wire *is* the domain (INDEX-PLAN
//! §9). There is no request DTO wrapper and no handler-side cursor: pagination
//! lives inside the engine, after ranking. Paged responses use the shared
//! [`heart::Page`] (whose items are `heart::Scored<T>` for the ranked
//! surfaces); the streaming `/search` emits its hits as NDJSON so a large
//! result set never materializes client-side in one frame.

#[allow(unused_imports)]
use crate::registry;
use std::sync::Arc;

use axum::{
    Json,
    body::Body,
    extract::{Path, Query as AxumQuery, State},
    http::{HeaderName, header},
    response::{IntoResponse, Response},
};
use futures::TryStreamExt;
use heart::query::{Query, QueryMode, Target};
use heart::{Page, Score, Scored, Sourced, Symbol, SymbolId};
use index::ecosystem::PackageNameExt as _;
use registry::runtime::session::{SessionGraph, SessionId, SessionStore};
use serde::Deserialize;

use crate::Server;
use crate::authz::Principal;
use crate::error::{BadRequestReason, ServerError, ServerResult};
use crate::registry::search::usages::Usage;
use crate::search::{
    AbstractQuery, Filter, LiteralQuery, PackageSelector, Query as ExecutionQuery, Search,
};
use registry::vector::EmbeddingModel;

/// The response header carrying the request's correlation id.
const QUERY_ID_HEADER: HeaderName = HeaderName::from_static("x-nudox-query-id");

/// Ensure a [`Query`] has a `query_id`, generating a fresh v4 UUID when absent.
/// Returns the id for tracing/metrics correlation.
fn ensure_query_id(query: &mut Query) -> uuid::Uuid {
    *query.query_id.get_or_insert_with(uuid::Uuid::new_v4)
}

/// Attach `x-nudox-query-id` to a response.
fn with_query_id(mut response: Response, query_id: uuid::Uuid) -> Response {
    response.headers_mut().insert(
        QUERY_ID_HEADER,
        http::HeaderValue::from_str(&query_id.to_string())
            .expect("uuid Display is a valid header value"),
    );
    response
}

/// Categorise `count` into a bounded rank bucket for low-cardinality metric labels.
fn rank_bucket(count: usize) -> &'static str {
    match count {
        0 => "zero",
        1 => "top_1",
        2..=5 => "top_5",
        6..=20 => "top_20",
        _ => "beyond",
    }
}

/// Emit observability hangers common to every search surface: retrieval count,
/// zero-result flag (bounded counter, never raw text or unbounded ids), and a
/// structured tracing event keyed on the `query_id`.
fn record_search_metrics(query_id: uuid::Uuid, hits: usize, target: &str) {
    metrics::counter!("search_retrievals", "source" => target.to_owned()).increment(1);
    if hits == 0 {
        metrics::counter!("search_zero_results", "source" => target.to_owned()).increment(1);
    }
    tracing::info!(
        %query_id,
        target,
        hits,
        "search served"
    );
}

#[derive(Debug, Default, Deserialize)]
struct SymbolQuery {
    query_id: Option<uuid::Uuid>,
}

/// `POST /search` — precise-or-planned symbol search. Streams NDJSON pages of
/// `heart::Scored<Symbol>`.
///
/// The wire query must carry `target: "Symbols"`. `mode: "semantic"` opts into
/// the planned semantic surface (the planner still owns the escalation).
#[tracing::instrument(skip_all, fields(mode = ?query.mode, limit = query.page.limit))]
pub async fn search<M: EmbeddingModel>(
    State(server): State<Arc<Server<M>>>,
    principal: Principal,
    Json(mut query): Json<Query>,
) -> ServerResult<Response> {
    let cap = server.authorize_read(&principal, "search.symbols")?;
    if query.target != Target::Symbols {
        return Err(BadRequestReason::MissingField {
            field: "target must be Symbols for /search",
        }
        .into());
    }
    let query_id = ensure_query_id(&mut query);
    let request = lower_to_symbol_search(&server, query)?;
    let hits: Vec<Scored<Symbol>> = server
        .search_symbols(&cap, &request)
        .await?
        .try_collect()
        .await?;
    tracing::debug!(%query_id, hits = hits.len(), "symbol search served");
    record_search_metrics(query_id, hits.len(), "symbols");
    Ok(with_query_id(ndjson(hits), query_id))
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
    Json(mut query): Json<Query>,
) -> ServerResult<Response> {
    let cap = server.authorize_read(&principal, "search.packages")?;
    let query_id = ensure_query_id(&mut query);
    let page = server.search_packages(&cap, &query).await?;
    let hits = page.items.len();
    tracing::debug!(%query_id, hits, "package search served");
    record_search_metrics(query_id, hits, "packages");
    Ok(with_query_id(Json(page).into_response(), query_id))
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
    Json(mut query): Json<Query>,
) -> ServerResult<Response> {
    let cap = server.authorize_read(&principal, "search.usages")?;
    if !matches!(query.target, Target::Usages { .. }) {
        return Err(BadRequestReason::MissingField {
            field: "target must be Usages { of } for /usages",
        }
        .into());
    }
    let query_id = ensure_query_id(&mut query);
    let page = server.usages(&cap, &query).await?;
    let hits = page.items.len();
    tracing::debug!(%query_id, hits, "usage lookup served");
    record_search_metrics(query_id, hits, "usages");
    Ok(with_query_id(Json(page).into_response(), query_id))
}

/// `POST /expand` — walk graph relationships from a hit (the wire query's `text`
/// is the seed symbol's id), merging into the caller's session graph when
/// `session` is set.
#[tracing::instrument(skip_all, fields(seed = %query.text))]
pub async fn expand<M: EmbeddingModel>(
    State(server): State<Arc<Server<M>>>,
    principal: Principal,
    Json(mut query): Json<Query>,
) -> ServerResult<Response> {
    let cap = server.authorize_read(&principal, "search.expand")?;
    let query_id = ensure_query_id(&mut query);
    let seed: uuid::Uuid =
        query
            .text
            .trim()
            .parse()
            .map_err(|e| BadRequestReason::InvalidSymbolId {
                raw: query.text.clone(),
                source: e,
            })?;
    let seed = SymbolId::from_uuid(seed);

    let origin = server
        .resolve_symbol(&cap, seed)
        .await?
        .ok_or(ServerError::NotFound)?;
    let hit = Scored::new(origin.value, seed_relevance());
    let mut related = server.expand(&cap, &hit).await?;
    related.truncate((query.page.limit as usize).max(1));

    if let Some(session) = query.session {
        let mut delta = SessionGraph::empty();
        delta.nodes.insert(seed);
        delta
            .nodes
            .extend(related.iter().map(|neighbour| neighbour.value.id));
        server
            .sessions()
            .merge_into(SessionId::from_uuid(session), delta)
            .await
            .map_err(|error| ServerError::Runtime(error.into()))?;
    }
    tracing::debug!(related = related.len(), "expansion served");
    let hits = related.len();
    record_search_metrics(query_id, hits, "expand");
    Ok(with_query_id(
        Json(Page {
            items: related,
            next: None,
        })
        .into_response(),
        query_id,
    ))
}

/// `GET /symbols/:id` — resolve one symbol by its durable id, tagged with the
/// federation source that answered.
#[tracing::instrument(skip_all, fields(symbol = %id))]
pub async fn get_symbol<M: EmbeddingModel>(
    State(server): State<Arc<Server<M>>>,
    principal: Principal,
    Path(id): Path<uuid::Uuid>,
    AxumQuery(params): AxumQuery<SymbolQuery>,
) -> ServerResult<Response> {
    let cap = server.authorize_read(&principal, "search.resolve_symbol")?;
    let response = server
        .resolve_symbol(&cap, SymbolId::from_uuid(id))
        .await?
        .map(|symbol| Json(symbol).into_response())
        .ok_or(ServerError::NotFound)?;
    Ok(match params.query_id {
        Some(query_id) => with_query_id(response, query_id),
        None => response,
    })
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
                selectors.push(PackageSelector {
                    name,
                    version: None,
                });
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

#[cfg(test)]
mod tests {
    use super::rank_bucket;

    #[test]
    fn rank_buckets_are_bounded() {
        assert_eq!(rank_bucket(0), "zero");
        assert_eq!(rank_bucket(1), "top_1");
        assert_eq!(rank_bucket(5), "top_5");
        assert_eq!(rank_bucket(20), "top_20");
        assert_eq!(rank_bucket(21), "beyond");
    }
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
    (
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(lines),
    )
        .into_response()
}
