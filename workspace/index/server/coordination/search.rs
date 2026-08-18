//! The search/read flow: route a read request to the right surface and assemble
//! the response.
//!
//! # Unbuffering `/search` (`LOCAL-REMOTE-CONTRACT.md` §3)
//!
//! `search_symbols` used to end with `Ok(futures::stream::iter(hits.into_iter().map(Ok)))`
//! — a fake stream wrapped around a `Vec` that `precise_hits`/`semantic_hits`
//! had already fully materialized by draining each federated source's stream
//! into a `Vec<Vec<_>>` and folding it with `merge_overlay_first`
//! (`IntoIterator<Item = Vec<Scored<T>>>`, which cannot start until every
//! source finishes). Two bugs rode along with that buffering:
//!
//! 1. **Sources were awaited sequentially.** The `for sourced in
//!    self.federation().in_precedence()` loop awaited one source's *entire*
//!    search-and-drain before even starting the next. A federation of two
//!    300ms sources took 600ms, not 300ms.
//! 2. **One source's mid-stream error killed every other source's results.**
//!    `collected.push(hit?)` propagated a single failed item out of
//!    `precise_hits`/`semantic_hits` entirely via `?`, discarding whatever the
//!    *other*, healthy sources had already found.
//!
//! Both are fixed by the same change: each federated source becomes its own
//! [`Answer<Symbols>`] (built by [`precise_source_answer`] /
//! [`semantic_source_answer`], which drain that source's stream into a fresh
//! [`answer_channel`] pair — a per-source item survives even if a later item
//! from the *same* source fails, and a source that fails outright still lets
//! every other source's items stand), all built **concurrently** via
//! [`futures::future::join_all`] rather than a sequential `for` loop, and then
//! folded with [`heart::surface::merge`] — the one federating merge, not a
//! second hand-rolled one — instead of `merge_overlay_first`. `merge` already
//! knows how to turn one source's `Frame::Failed` into a `Frame::Degraded` on
//! the combined answer (contract's whole point: a remote/source failure must
//! not fail an answer other sources already partly satisfied).
//!
//! `merge_overlay_first` itself is untouched and still backs [`expand`] and
//! `coordination::packages::search_packages` — this migration is scoped to
//! `/search` only (S2), not every merge call site.

#[allow(unused_imports)]
use crate::server::registry;
use std::num::NonZeroUsize;

use futures::StreamExt;
use futures::future::join_all;
use heart::stream::WireError;
use heart::surface::{
    Answer, Gen, Residence, Summary, SymbolHit, Symbols, answer_channel, merge_bounded,
};
use heart::{Scored, Sourced, Symbol, SymbolId};

use crate::ecosystem::FilterExt as _;
use crate::server::authz::ReadCap;
use crate::server::error::{BadRequestReason, InternalError, ServerError};
use crate::server::search::planner::Plan;
use crate::server::search::semantic::SemanticSurface;
use crate::server::search::{AbstractQuery, Filter, Query, Search, SymbolCursor};
use crate::server::search::{SearchPlanner, SearchTarget, SymbolStore, merge_overlay_first};
use crate::server::{Server, SourceStores};
use registry::vector::EmbeddingModel;

/// Buffer size for each per-source [`answer_channel`]. A genuine bound now
/// (`answer_channel`'s own doc comment, contract task 9): `precise_source_
/// answer`/`semantic_source_answer` forward items via `item_async`, so a
/// consumer slower than a source parks the forwarding task for room instead
/// of losing results once this buffer fills.
const SOURCE_ANSWER_CAPACITY: usize = 256;

/// How many `symbol_by_id` hydrations [`semantic_source_answer`] runs at once.
/// Was an N+1 run fully in series; this is not "unbounded fan-out" either —
/// bounded so one huge semantic page can't open thousands of catalog reads at
/// a time.
const HYDRATE_CONCURRENCY: usize = 16;

impl<M: EmbeddingModel> Server<M> {
    /// Search for symbols. The caller must hold a [`ReadCap`] proving that
    /// authorization has already occurred at the HTTP boundary.
    ///
    /// Returns immediately with an [`Answer<Symbols>`] whose items arrive as
    /// each federated source (and, within the semantic arm, each hydration)
    /// resolves — the caller (the HTTP handler) streams frames onto the wire
    /// as they are emitted rather than waiting for this to finish. A
    /// request-level failure (a malformed cursor, the planner's own
    /// literal/semantic invariant) is still a synchronous `Err`, surfaced as
    /// an HTTP error status *before* the stream begins, same as before this
    /// change — only a *source's* failure, discovered mid-answer, becomes a
    /// [`heart::surface::Frame::Degraded`] on the returned answer instead.
    pub async fn search_symbols<'a>(
        &'a self,
        _cap: &ReadCap,
        request: &'a Search<'a>,
    ) -> Result<Answer<Symbols>, ServerError> {
        // The server does not track cross-request "generations" the way the
        // GUI's search-as-you-type does (`heart::surface::Gen`'s own doc
        // comment: it numbers *requests made by a process*, and this process
        // never supersedes its own in-flight HTTP requests the way a
        // keystroke supersedes the previous one). `Gen` is nonetheless
        // required plumbing on every `Answer`/`Emitter` — a fixed `Gen(0)` is
        // honest about that rather than synthesizing a fake-looking sequence
        // number nothing here would ever advance.
        let generation = Gen(0);
        match self.planner().plan(request) {
            Plan::Precise => self.precise_hits(request, generation).await,
            Plan::Semantic(gate) => self.semantic_hits(gate, request, generation).await,
        }
    }

    /// Resolve a symbol by id. The caller must hold a [`ReadCap`].
    pub async fn resolve_symbol(
        &self,
        _cap: &ReadCap,
        id: SymbolId,
    ) -> Result<Option<Sourced<Symbol>>, ServerError> {
        for sourced in self.federation().in_precedence() {
            if let Some(symbol) = sourced.value.symbol_by_id(id).await? {
                return Ok(Some(sourced.map(|_| symbol)));
            }
        }
        Ok(None)
    }

    /// Expand graph neighbours of a symbol. The caller must hold a [`ReadCap`].
    ///
    /// Left on the pre-S2 `merge_overlay_first` deliberately (contract §3
    /// scopes this migration to `/search`'s hit path only): `expand` returns a
    /// single small, already-bounded `Vec` today (`related_hits` is presently
    /// always empty — see `SymbolStore::related_hits`'s own doc comment — so
    /// there is no buffering cost here worth chasing yet).
    pub async fn expand(
        &self,
        _cap: &ReadCap,
        hit: &Scored<Symbol>,
    ) -> Result<Vec<Scored<Symbol>>, ServerError> {
        let mut groups = Vec::new();
        for sourced in self.federation().in_precedence() {
            groups.push(sourced.value.related_hits(hit).await?);
        }
        Ok(merge_overlay_first(groups, |symbol| symbol.id, usize::MAX))
    }

    /// Query every federated source's usage-query backend for recorded uses of
    /// one symbol. The caller must hold a [`ReadCap`] and supply a query whose
    /// target is `Target::Usages { of }`. Delegates to each source's
    /// process-local, interior-mutable
    /// [`SharedUsageBackend`](crate::server::registry::search::usages::SharedUsageBackend)
    /// (`SourceStores::usage_backend`), mutated in place by that source's own
    /// compile pipeline as packages are indexed.
    ///
    /// Sources are tried in overlay-first precedence order; the first source
    /// that holds a loaded scope for `of`'s package answers (whether the page
    /// is populated or a real, empty one). Only when *every* source reports
    /// [`Error::IndexUnavailable`](crate::server::registry::search::usages::Error::IndexUnavailable)
    /// — no source has this package's IR materialized yet — is the honest
    /// `503` returned; that is distinct from a fake empty page. Any other
    /// error (a malformed `of`) is a client-input problem common to every
    /// source, so it is surfaced immediately rather than masked by continuing
    /// to the next source.
    pub async fn usages(
        &self,
        _cap: &ReadCap,
        query: &heart::query::Query,
    ) -> Result<heart::Page<crate::server::registry::search::usages::Usage>, ServerError> {
        use crate::server::registry::search::usages::Error as UsageError;

        let heart::query::Target::Usages { ref of } = query.target else {
            return Err(crate::server::error::BadRequestReason::MissingField {
                field: "target must be Usages { of } for /usages",
            }
            .into());
        };

        let mut last_unavailable: Option<UsageError> = None;
        for sourced in self.federation().in_precedence() {
            match sourced.value.usage_backend.usages(of, &query.page).await {
                Ok(page) => return Ok(page),
                Err(UsageError::IndexUnavailable) => {
                    last_unavailable = Some(UsageError::IndexUnavailable);
                }
                Err(other) => return Err(other.into()),
            }
        }
        Err(last_unavailable.unwrap_or(UsageError::IndexUnavailable).into())
    }

    fn planner(&self) -> &crate::server::search::SearchPlanner {
        &self.planner
    }

    /// The precise arm: federate every source's `SourceStores::search`
    /// concurrently (each wrapped as its own [`Answer<Symbols>`] by
    /// [`precise_source_answer`]) and fold with [`heart::surface::merge`].
    async fn precise_hits(
        &self,
        request: &Search<'_>,
        generation: Gen,
    ) -> Result<Answer<Symbols>, ServerError> {
        let sourced: Vec<Sourced<&SourceStores<M>>> = self.federation().in_precedence().collect();

        // `join_all` polls every source's future in the same task, so source 2
        // begins the instant source 1 yields rather than after it returns —
        // the fix for the sequential-`for`-loop latency bug (contract §3,
        // `sources_are_polled_concurrently_not_sequentially` at the `heart`
        // level pins the same property one layer down, on `merge` itself).
        let answers = join_all(
            sourced
                .iter()
                .map(|source| precise_source_answer(generation, source.value, request)),
        )
        .await;

        let sources = sourced
            .iter()
            .map(|source| source.source)
            .zip(answers)
            .collect();
        Ok(spawn_merged(
            generation,
            sources,
            NonZeroUsize::new(page_limit(request)).unwrap_or(NonZeroUsize::MIN),
        ))
    }

    /// The gated semantic (qdrant) arm: embed the query once (cache-first),
    /// mint one [`registry::vector::SemanticGate`] per federated source (the
    /// first is the caller's own gate; the rest re-issue under the same audit
    /// reason via [`SearchPlanner::extend_across_federation`]), then federate
    /// every source concurrently and fold with [`heart::surface::merge`].
    async fn semantic_hits(
        &self,
        gate: registry::vector::SemanticGate,
        request: &Search<'_>,
        generation: Gen,
    ) -> Result<Answer<Symbols>, ServerError> {
        let Query::Abstract(query) = &request.query else {
            // The planner never plans a literal query semantically.
            return Err(InternalError::PlannerInvariantSemanticForLiteral.into());
        };
        let limit = NonZeroUsize::new(page_limit(request)).unwrap_or(NonZeroUsize::MIN);
        let after = request
            .page
            .cursor
            .as_deref()
            .map(|token| {
                heart::Cursor::<_, heart::Advisory>::decode(token).map_err(|source| {
                    BadRequestReason::InvalidCursor {
                        token: token.to_owned(),
                        source,
                    }
                })
            })
            .transpose()?;

        let reason = gate.reason();
        let sourced: Vec<Sourced<&SourceStores<M>>> = self.federation().in_precedence().collect();

        // One planner issuance authorizes one user-visible query; fanning it
        // out across the federation re-issues per source under the same
        // audit reason (unchanged from the pre-S2 shape).
        let mut gate = Some(gate);
        let gates: Vec<_> = (0..sourced.len())
            .map(|_| {
                gate.take()
                    .unwrap_or_else(|| SearchPlanner::extend_across_federation(reason))
            })
            .collect();

        let answers = join_all(sourced.iter().zip(gates).map(|(source, gate)| {
            semantic_source_answer(
                generation,
                source.value,
                gate,
                query,
                &request.filter,
                limit,
                after.clone(),
                self,
            )
        }))
        .await;

        let sources = sourced
            .iter()
            .map(|source| source.source)
            .zip(answers)
            .collect();
        Ok(spawn_merged(
            generation,
            sources,
            NonZeroUsize::new(page_limit(request)).unwrap_or(NonZeroUsize::MIN),
        ))
    }
}

/// Drain one federated source's precise (`SourceStores::search`) stream into
/// its own [`Answer<Symbols>`].
///
/// Items are forwarded to the emitter as they are read from the source's own
/// stream — even though `SourceStores::search` itself eagerly collects into a
/// `Vec` before returning (its own doc comment explains why: a Rust 2024
/// `impl Trait` capture-rule constraint the whole codebase's search paths
/// share), the point of this function is that a *second* source's items no
/// longer have to wait for that collection to finish before `merge` can start
/// forwarding them — `join_all` at the call site starts every source's
/// `search` future in the same poll, and this function's own item-by-item
/// forwarding means a source that errors after some items still leaves those
/// items standing (`Frame::Failed` here becomes `Frame::Degraded` in the
/// merged answer, not a discarded partial result).
async fn precise_source_answer<M: EmbeddingModel>(
    generation: Gen,
    source: &SourceStores<M>,
    request: &Search<'_>,
) -> Answer<Symbols> {
    let (tx, answer) = answer_channel::<Symbols>(SOURCE_ANSWER_CAPACITY, generation);

    let stream = match source.search(request).await {
        Ok(stream) => stream,
        Err(error) => {
            let _ = tx.failed(to_wire_error(&error));
            return answer;
        }
    };
    futures::pin_mut!(stream);

    let mut items = 0u64;
    while let Some(item) = stream.next().await {
        match item {
            Ok(scored) => {
                items += 1;
                // Every federated source here is queried in-process by this
                // very server, so from *this* server's own point of view the
                // row is genuinely `Local` — it is the HTTP handler, one
                // layer up, that re-tags outgoing items `Remote` from the
                // calling client's point of view (contract §1.3; see
                // `http::handlers::search`'s own doc comment on why that
                // retagging belongs there and not here).
                //
                // `SymbolHit::from(Symbol)` is the projection onto the wire
                // item `Symbols::Item` now requires — with `signature: None`,
                // because this source answers from the text index, not IR
                // (see `SymbolHit::from`'s own doc comment for why that is a
                // current source limitation, not a permanent one).
                // Awaited, not the sync `item`: `precise_source_answer` is
                // always driven inside `join_all` within an async request
                // handler with a live reactor (never from a sync `Serve::serve`
                // caller), so it can park for real backpressure instead of
                // dropping a search result a slow HTTP client was merely slow
                // to take.
                if tx
                    .item_async(scored.map(SymbolHit::from), Residence::Local)
                    .await
                    .is_err()
                {
                    // The paired `Answer` (or the merge consuming it) was
                    // dropped — nothing left to receive further frames.
                    return answer;
                }
            }
            Err(error) => {
                let _ = tx.failed(to_wire_error(&error));
                return answer;
            }
        }
    }
    let _ = tx.end(Summary::complete(items));
    answer
}

/// Drain one federated source's semantic (qdrant) arm into its own
/// [`Answer<Symbols>`]: embed-and-match under `gate`, then hydrate the
/// returned ids into full [`Symbol`] rows.
///
/// # The N+1 this closes
///
/// The pre-S2 shape awaited `symbol_by_id` once per hit, inside the same
/// `while let Some(hit) = identities.next().await` loop that read the
/// semantic match itself — every hydration serialized behind the previous
/// one even though they are independent catalog reads. Here the ids are read
/// first (already bounded by `limit`, so this is not a new unbounded buffer),
/// then hydrated through `buffer_unordered(HYDRATE_CONCURRENCY)`, which runs
/// up to [`HYDRATE_CONCURRENCY`] catalog reads at once and forwards each
/// resolved row to the emitter as soon as it is ready — not in id order,
/// which is fine: the `merge` this answer feeds already gives up a single
/// total order in exchange for progressive delivery (`heart::surface::merge`'s
/// own doc comment, "PROGRESSIVE").
#[allow(clippy::too_many_arguments)]
async fn semantic_source_answer<M: EmbeddingModel>(
    generation: Gen,
    source: &SourceStores<M>,
    gate: registry::vector::SemanticGate,
    query: &AbstractQuery,
    filter: &Filter,
    limit: NonZeroUsize,
    after: Option<SymbolCursor>,
    server: &Server<M>,
) -> Answer<Symbols> {
    let (tx, answer) = answer_channel::<Symbols>(SOURCE_ANSWER_CAPACITY, generation);

    let surface = SemanticSurface::new(&source.semantics, &server.embedder, &server.embedding_cache);
    let identities = match surface.search(gate, query, filter, limit, after).await {
        Ok(identities) => identities,
        Err(error) => {
            let _ = tx.failed(to_wire_error(&error));
            return answer;
        }
    };
    futures::pin_mut!(identities);

    let mut scored_ids = Vec::new();
    while let Some(scored_identity) = identities.next().await {
        match scored_identity {
            Ok(scored_identity) => scored_ids.push(scored_identity),
            Err(error) => {
                let _ = tx.failed(to_wire_error(&error));
                return answer;
            }
        }
    }

    let mut hydrated = futures::stream::iter(scored_ids)
        .map(|scored_identity| async move {
            let symbol = source.symbol_by_id(scored_identity.value).await;
            (scored_identity.score, symbol)
        })
        .buffer_unordered(HYDRATE_CONCURRENCY);

    let mut items = 0u64;
    while let Some((score, symbol)) = hydrated.next().await {
        match symbol {
            Ok(Some(symbol)) if filter.admits(&symbol) => {
                items += 1;
                // See `precise_source_answer`'s matching comment: this arm
                // projects onto `SymbolHit` the same way, with the same
                // `signature: None` (qdrant hydration has no IR either).
                // See `precise_source_answer`'s matching comment: this is
                // driven inside `join_all` on the request-handling task's
                // reactor, so it awaits room instead of dropping a hydrated
                // hit.
                if tx
                    .item_async(Scored::new(SymbolHit::from(symbol), score), Residence::Local)
                    .await
                    .is_err()
                {
                    return answer;
                }
            }
            Ok(_) => {}
            Err(error) => {
                let _ = tx.failed(to_wire_error(&error));
                return answer;
            }
        }
    }
    let _ = tx.end(Summary::complete(items));
    answer
}

/// Fold per-source answers (supplied, like `merge_overlay_first` before it, in
/// federation **precedence order** — overlays first, then the definitive base)
/// with the one federating merge, and spawn the [`heart::surface::MergePump`]
/// that drives it.
///
/// The pump has to be *driven* by something — [`heart::surface::merge`]'s own
/// doc comment is explicit that it returns the pump as a plain future rather
/// than spawning it internally so callers with no Tokio reactor (the GUI) can
/// hand it to their own executor. The server *does* have Tokio, so `tokio::
/// spawn` here is the direct analogue of `RemoteClient::serve`'s own
/// `tokio::spawn(pump::<S>(...))` for the same shape one layer further out.
///
/// # Why `merge_bounded` and not `merge`
///
/// Each source already caps its own contribution at the request's page limit,
/// so an unbounded fold over `N` sources would deliver up to `N × limit` items —
/// a caller asking for a page of 30 would receive 90. Giving up the *global
/// sort* for progressive delivery was a deliberate trade; silently multiplying
/// the *page size* was not. `merge_bounded` counts distinct keys (a supersede
/// replaces a row rather than adding one, so it is exempt) and reports
/// `Completeness::Partial` when the budget cuts the answer short.
fn spawn_merged(
    generation: Gen,
    sources: Vec<(heart::SourceId, Answer<Symbols>)>,
    limit: NonZeroUsize,
) -> Answer<Symbols> {
    let (merged, pump) = merge_bounded::<Symbols>(generation, sources, limit);
    tokio::spawn(pump);
    merged
}

/// Classify a [`ServerError`] into the [`WireError`] class a
/// [`heart::surface::Frame::Failed`]/[`heart::surface::Degradation`] carries,
/// reusing the same status-code dichotomy [`ServerError::status`] already
/// computes (mirrors `heart::client::remote::classify_status_error`'s
/// 4xx/backend split on the client side of the same wire).
fn to_wire_error(error: &ServerError) -> WireError {
    let status = error.status();
    if status.is_client_error() {
        WireError::BadRequest(error.to_string())
    } else if status == http::StatusCode::SERVICE_UNAVAILABLE {
        WireError::Backend(error.to_string())
    } else {
        WireError::Internal(error.to_string())
    }
}

/// The requested page size, widened to the merge vocabulary.
///
/// # Why there is no longer a post-merge `.truncate(limit)`
///
/// `merge_overlay_first` used to truncate the *whole* folded set to `limit`
/// after sorting by score — which required materializing every source's
/// results first, exactly the buffering this change removes. Each source
/// already caps its own contribution at `limit` (`SourceStores::search`'s own
/// `NonZeroUsize::new(request.page.limit as usize)`), so a federation of `N`
/// sources may now emit up to `N × limit` items rather than a single
/// re-truncated `limit`. This is the direct, documented consequence of giving
/// up a global sort for progressive delivery (`heart::surface::merge`'s
/// "PROGRESSIVE" doc comment: you cannot have items-stream-as-known,
/// globally-sorted output, *and* overlay-override all at once) — a client
/// that wants a hard cap across sources applies it after receiving, the same
/// way it already tolerates arrival order not being score order.
fn page_limit(request: &Search<'_>) -> usize {
    request.page.limit as usize
}
