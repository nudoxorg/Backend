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
use crate::server::registry;
use std::sync::Arc;

use crate::ecosystem::PackageNameExt as _;
use axum::{
    Json,
    body::Body,
    extract::{Path, Query as AxumQuery, State},
    http::{HeaderName, header},
    response::{IntoResponse, Response},
};
use futures::StreamExt as _;
use heart::query::{Query, QueryMode, Target};
use heart::surface::{Answer, Frame, GenerationId, Located, Residence, Symbols};
use heart::{PackageHit, Page, Score, Scored, SymbolId};
use registry::runtime::session::{SessionGraph, SessionId, SessionStore};
use serde::Deserialize;

use crate::server::Server;
use crate::server::authz::Principal;
use crate::server::error::{BadRequestReason, ServerError, ServerResult};
use crate::server::registry::search::usages::Usage;
use crate::server::search::{
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
pub struct SymbolQuery {
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
    // No `.try_collect()` here — that call used to drain this exact stream
    // into a `Vec` before a single byte reached the client (the buffer point
    // `LOCAL-REMOTE-CONTRACT.md` §0.2/§3(a) names explicitly). `search_symbols`
    // now returns an `Answer<Symbols>` whose frames are written to the
    // response body as `coordination::search`'s federated sources (and
    // `heart::surface::merge`) actually produce them.
    let answer = server.search_symbols(&cap, &request).await?;
    tracing::debug!(%query_id, "symbol search streaming");
    Ok(with_query_id(symbol_answer_response(answer, query_id), query_id))
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
    // Project the internal `GlobalPackage` into the shared wire `PackageHit`
    // before serialising. This is the *single* conversion point: the client
    // decodes `Page<PackageHit>`, so any drift between what the server sends and
    // what the client expects is a compile error right here, not a runtime
    // `Value` index-panic in the GUI.
    let projected = Page {
        items: page
            .items
            .into_iter()
            .map(|scored| scored.map(project_package_hit))
            .collect(),
        next: page.next,
    };
    Ok(with_query_id(Json(projected).into_response(), query_id))
}

/// Lower one internal `GlobalPackage` to the shared wire [`PackageHit`].
///
/// The coordinates / id / state are already `heart` vocabulary and move across
/// untouched; only the rich `SearchFacets` are projected down to the lean
/// ranking-visible signals a client renders.
fn project_package_hit(package: crate::server::registry::GlobalPackage) -> PackageHit {
    let (quality_ppm, description, downloads) = match &package.facets {
        Some(facets) => (
            Some(facets.quality_ppm),
            facets.description.clone(),
            facets.downloads,
        ),
        None => (None, None, None),
    };
    PackageHit {
        id: package.id,
        coordinates: package.package.coordinates,
        state: package.state,
        quality_ppm,
        description,
        downloads,
    }
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
            if let Ok(name) =
                crate::server::registry::package::PackageName::new(ecosystem, raw.as_str())
            {
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

/// The remote-corpus generation this handler stamps onto every outgoing item.
///
/// No real corpus-generation identifier (`heart::query::AsOf`/
/// `CatalogCommitHash`, or a future catalog watermark) is threaded through to
/// this handler yet — [`heart::surface::Residence::Remote`]'s `generation`
/// exists to let a client tell "these two items are the same underlying data,
/// fetched at different times" from "these are from different corpus
/// snapshots entirely" (contract §1.3), and this handler cannot make that
/// distinction today. `0` is a deliberately inert placeholder, not a derived
/// one: synthesizing something that merely *looks* meaningful (hashing the
/// query id, say) would be worse, because a future reader would have to
/// discover by inspection that it carries no real information instead of
/// being told so here. Wiring a real generation through is follow-up work
/// (`LOCAL-REMOTE-CONTRACT.md` S3/S4), not part of this change.
const PLACEHOLDER_REMOTE_GENERATION: GenerationId = GenerationId(0);

/// Stream a symbol [`Answer`] onto the wire as NDJSON [`Frame<Symbols>`]
/// lines, writing each frame to the response body **the moment it is
/// produced** rather than after the whole answer is known.
///
/// This is the buffer point `LOCAL-REMOTE-CONTRACT.md` §0.2/§3(a) names
/// explicitly: the old `hit_stream(hits: Vec<H>)` wrapped an
/// already-fully-materialized `Vec` in `futures::stream::iter`, so the bytes
/// chunked but nothing ever arrived earlier than the last one. Piping
/// [`Answer::into_stream`] straight into [`Body::from_stream`] means a hit
/// [`coordination::search`](crate::server::coordination) already emitted can
/// reach the client while a slower federated source is still being awaited —
/// the property `search_symbols` now exists to provide.
///
/// Every [`Frame::Item`] is re-tagged [`Residence::Remote`] here, regardless
/// of what `coordination::search` set it to (`Residence::Local` — correct
/// from *that* code's point of view, since every federated source it reads is
/// queried in-process by this very server). A caller reaching this handler
/// over HTTP is, by definition, not this process: every row it receives needs
/// the network to be re-served, and [`Residence`] exists precisely so a
/// client can tell that apart from data it already has on disk (contract
/// §1.3). The terminal frame's completeness/count is unaffected by the
/// retag — only [`Frame::Item`] carries a residence to rewrite.
///
/// Also where `search`'s observability hangers move to: the old handler
/// called [`record_search_metrics`] once, synchronously, with the final `Vec`
/// length. There is no such synchronous count anymore — the hit total is only
/// known once [`Frame::End`] itself arrives — so metrics are recorded from
/// *inside* the frame-mapping closure, the moment that terminal frame is
/// observed.
fn symbol_answer_response(answer: Answer<Symbols>, query_id: uuid::Uuid) -> Response {
    let lines = answer.into_stream().map(move |frame| {
        let frame = match frame {
            Frame::Item(located) => Frame::Item(Located::new(
                located.value,
                Residence::Remote {
                    generation: PLACEHOLDER_REMOTE_GENERATION,
                },
            )),
            other => other,
        };
        if let Frame::End(summary) = &frame {
            record_search_metrics(query_id, summary.items as usize, "symbols");
        }
        serde_json::to_vec(&frame).map(|mut line| {
            line.push(b'\n');
            bytes::Bytes::from(line)
        })
    });
    (
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(lines),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::{rank_bucket, symbol_answer_response};
    use heart::surface::{Gen, Residence, SymbolHit, Summary, answer_channel};
    use heart::{Scored, Score, SymbolKind};
    use http_body_util::BodyExt;

    #[test]
    fn rank_buckets_are_bounded() {
        assert_eq!(rank_bucket(0), "zero");
        assert_eq!(rank_bucket(1), "top_1");
        assert_eq!(rank_bucket(5), "top_5");
        assert_eq!(rank_bucket(20), "top_20");
        assert_eq!(rank_bucket(21), "beyond");
    }

    /// A bare `SymbolHit` fixture — `signature: None`, matching what this
    /// handler's own real sources ever produce (see `SymbolHit::from`'s doc
    /// comment: no federated source here has IR).
    fn sample_symbol(tag: u8) -> SymbolHit {
        SymbolHit {
            package: heart::identity::PackageId::from_uuid(uuid::Uuid::from_bytes([tag; 16])),
            path: format!("crate::sym{tag}").into(),
            display_name: format!("sym{tag}").into(),
            ecosystem: heart::Language::Rust,
            kind: SymbolKind::Function,
            signature: None,
            reference: None,
        }
    }

    /// The direct successor of the old `hit_stream_emits_incremental_ndjson_
    /// frames_not_one_static_body` — same property (the response body arrives
    /// as several distinct chunks, not one static blob), now pinned against
    /// `symbol_answer_response`/`Answer<Symbols>` instead of a `Vec` wrapped in
    /// `futures::stream::iter`. This is the test that would fail if a future
    /// change reintroduced a `.try_collect()` before this function: collecting
    /// the whole answer first would still produce a well-formed body, but as a
    /// single chunk arriving only once every frame was already known.
    #[tokio::test]
    async fn symbol_answer_response_emits_incremental_ndjson_frames_not_one_static_body() {
        let (tx, answer) = answer_channel::<heart::surface::Symbols>(8, Gen(1));
        tx.item(
            Scored::new(sample_symbol(1), Score::try_new(0.9).unwrap()),
            Residence::Local,
        )
        .expect("emit");
        tx.item(
            Scored::new(sample_symbol(2), Score::try_new(0.5).unwrap()),
            Residence::Local,
        )
        .expect("emit");
        tx.end(Summary::complete(2)).expect("end");

        let response = symbol_answer_response(answer, uuid::Uuid::new_v4());
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/x-ndjson"
        );

        let mut body = response.into_body();
        let mut chunks = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.expect("stream body must not fail");
            if let Ok(data) = frame.into_data() {
                chunks.push(data);
            }
        }

        assert!(
            chunks.len() >= 3,
            "two items plus the terminal frame must be delivered as incremental body chunks, \
             not one static response"
        );
        let joined = chunks
            .iter()
            .flat_map(|chunk| chunk.iter().copied())
            .collect::<Vec<_>>();
        // `bytecount::count` is the clippy-preferred way to do this over a
        // large buffer; this one is a few dozen bytes of test fixture, so a
        // manual `fold` (rather than pulling in a new dependency for a single
        // assertion) is the right amount of ceremony.
        let newlines = joined.iter().fold(0u32, |count, &byte| {
            count + u32::from(byte == b'\n')
        });
        assert_eq!(
            newlines, 3,
            "the streaming body must contain one NDJSON line per item plus End"
        );

        // Every item frame must be re-tagged Remote (never the Local residence
        // the answer was built with) — see `symbol_answer_response`'s own doc
        // comment on why the retag happens at this layer.
        let text = String::from_utf8(joined).expect("ndjson body is utf8");
        let item_lines: Vec<&str> = text
            .lines()
            .filter(|line| line.starts_with(r#"{"item":"#))
            .collect();
        assert_eq!(item_lines.len(), 2, "both items must round-trip as item frames");
        for line in item_lines {
            assert!(
                line.contains(r#""residence":{"remote":{"generation":0}}"#),
                "item frame must carry the placeholder Remote residence, got {line}"
            );
        }
    }
}
