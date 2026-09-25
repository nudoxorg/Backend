//! The supervised background loops `serve()` runs alongside the HTTP surface:
//! the queue worker driving indexing jobs, one outbox consumer per derived
//! sink, and the replica-local index sync/watermark pollers.
//!
//! Every loop follows the same discipline: it never returns under normal
//! operation, it survives (logs + backs off) transient faults, and it is torn
//! down by task abort at its next await point when `serve()` winds down —
//! every unit of work it performs is idempotent, so an abort mid-tick is safe.

#[allow(unused_imports)]
use crate::server::{registry, vector};
use std::sync::Arc;
use std::time::Duration;

use crate::server::registry::coordination::{OutboxEntry, OutboxOp, SinkKind};
use heart::PackageId;
use registry::vector::{
    EmbedRole, EmbeddingCache, EmbeddingKey, EmbeddingModel, PointId, VectorPoint, VectorStore,
};

use crate::server::coordination::indexing::Indexer;
use crate::server::error::ServerResult;
use crate::server::search::semantic::embedder::HttpEmbedder;
use crate::server::{Server, SourceStores};

/// How long a crashed loop waits before its supervisor restarts it.
const SUPERVISOR_BACKOFF: Duration = Duration::from_secs(5);

/// How many outbox intents a consumer drains per tick.
const OUTBOX_BATCH: usize = 64;

/// How often the storage-reclamation duty runs. Deliberately coarse — GC is a
/// housekeeping sweep, not a hot path; running it hourly keeps its load off the
/// consumers while still bounding outbox growth.
const GC_INTERVAL: Duration = Duration::from_secs(3600);

/// Supervise the indexing queue worker: restart it with backoff whenever the
/// underlying loop surfaces an error (a poisoned postgres connection, say).
///
/// `drain` is the graceful-shutdown signal: once fired the worker stops
/// dequeuing new jobs (in-flight jobs already finish inside `run_worker_until`),
/// `run_worker_until` returns `Ok(())`, and this supervisor exits cleanly rather
/// than restarting — so [`Server::serve`] can wait a bounded drain window for
/// in-flight work to settle before aborting the remaining pollers.
pub(crate) async fn queue_worker<M: EmbeddingModel>(
    server: Arc<Server<M>>,
    drain: tokio_util::sync::CancellationToken,
) {
    let indexer = Indexer::new(Arc::clone(&server));
    loop {
        match indexer.run_worker_until(&drain).await {
            // A clean return means a drain was requested: stop supervising.
            Ok(()) => {
                tracing::info!("queue worker drained; stopping supervisor");
                return;
            }
            Err(error) => {
                tracing::error!(error = %error, "queue worker failed; restarting");
                // Do not sleep past a drain request — re-check promptly on wake.
                tokio::time::sleep(SUPERVISOR_BACKOFF).await;
                if drain.is_cancelled() {
                    return;
                }
            }
        }
    }
}

/// One derived store's consumer: poll every source's outbox from the sink's
/// durable watermark, materialize what was missed, and advance the watermark —
/// the pull half of the transactional-outbox pattern.
pub(crate) async fn outbox_consumer<M: EmbeddingModel>(server: Arc<Server<M>>, sink: SinkKind) {
    let interval = server.config().limits.poll_interval;
    loop {
        for sourced in server.federation().in_precedence() {
            let stores = sourced.value;

            // Per-sink replica safety: claim this source's sink via a postgres
            // advisory lock so exactly one replica drains it. If another replica
            // holds it (or the lock attempt errors), skip this sink this tick and
            // try again next interval — consumers are idempotent, so nothing is
            // lost by yielding a tick.
            let guard = match stores.outbox.try_lock_sink(sink).await {
                Ok(Some(guard)) => guard,
                Ok(None) => {
                    tracing::trace!(%sink, source = %sourced.source, "sink drained by another replica; skipping");
                    continue;
                }
                Err(error) => {
                    tracing::warn!(%sink, source = %sourced.source, error = %error, "sink lock attempt failed");
                    continue;
                }
            };

            match consume_once(&server, stores, sink).await {
                Ok(0) => {}
                Ok(consumed) => {
                    metrics::counter!("outbox_intents_consumed", "sink" => sink.to_string())
                        .increment(consumed as u64);
                }
                Err(error) => {
                    tracing::warn!(%sink, source = %sourced.source, error = %error, "outbox poll failed");
                }
            }

            // Release the advisory lock cleanly so the connection returns to the
            // pool lock-free; a failed unlock is non-fatal (drop frees it too).
            if let Err(error) = guard.release().await {
                tracing::warn!(%sink, source = %sourced.source, error = %error, "sink lock release failed");
            }
        }
        tokio::time::sleep(interval).await;
    }
}

/// Drain one batch of intents for `sink` from one source. The watermark only
/// advances *after* an intent materializes, so a crash re-delivers (idempotent
/// by the outbox dedupe key) rather than drops.
async fn consume_once<M: EmbeddingModel>(
    server: &Server<M>,
    stores: &SourceStores<M>,
    sink: SinkKind,
) -> ServerResult<usize> {
    let watermark = stores
        .outbox
        .read_watermark(sink)
        .await
        .map_err(crate::server::registry::RegistryError::from)?;
    let entries = stores
        .outbox
        .read_since(sink, watermark, OUTBOX_BATCH)
        .await
        .map_err(crate::server::registry::RegistryError::from)?;
    for entry in &entries {
        materialize(server, stores, entry).await?;
        stores
            .outbox
            .advance_watermark(sink, entry.id)
            .await
            .map_err(crate::server::registry::RegistryError::from)?;
    }
    Ok(entries.len())
}

/// Materialize one fan-out intent into its derived store.
///
/// Dispatches on `entry.op` first:
///
/// **`Upsert`** — reads the package's symbol projection from the global index
/// and writes it into the sink:
/// - [`SinkKind::Text`] is consumed by [`text_index_poller`] (the replica-local
///   *symbol* tantivy index, `crate::runtime::text::TextIndex`), which reads
///   the same Text-sink outbox under its own file-persisted watermark; this
///   consumer advances only the catalog-side per-sink watermark here (the
///   signal [`crate::server::coordination::search`]-adjacent tests read to
///   confirm the intent was acknowledged);
/// - [`SinkKind::Vector`] embeds each symbol and upserts a [`VectorPoint`] into
///   qdrant (keyed by a symbol-derived point id, so a re-embed replaces in place);
/// - [`SinkKind::Graph`] is a no-op until the IR reverse-position index lands.
///
/// **`Delete`** — removes the package's search projection from the sink.
/// **Blob / CAS data is never touched** — the mirror keeps full history; only
/// search visibility ends. Per sink:
/// - [`SinkKind::Text`] issues a `delete_term` on the replica-local *package*
///   tantivy index, then commits — note this does **not** yet tombstone the
///   symbol-level [`crate::runtime::text::TextIndex`] `text_index_poller` feeds;
///   `TextIndex` currently exposes no delete-by-package primitive, so a withdrawn
///   package's symbols remain precise-searchable until a future symbol-level
///   tombstone lands (tracked, not exercised by any current test);
/// - [`SinkKind::Vector`] deletes all qdrant points whose payload
///   `package` field matches the package uuid;
/// - [`SinkKind::Graph`] nothing to tombstone yet.
///
/// Every write is idempotent (upsert-by-id / replace-by-key / delete-missing-
/// is-ok), so the outbox may re-deliver on crash without corrupting the
/// projection.
///
/// [`GlobalStore::symbols_for`]: registry::index::GlobalStore::symbols_for
async fn materialize<M: EmbeddingModel>(
    server: &Server<M>,
    stores: &SourceStores<M>,
    entry: &OutboxEntry,
) -> ServerResult<()> {
    match entry.op {
        OutboxOp::Upsert => {
            // The symbol projection is the shared input to the vector sink. An
            // intent for a package with no persisted symbols is a well-formed
            // empty materialization. Text-sink upserts are pulled by the
            // package-index poller independently of this consumer.
            let symbols = match entry.kind {
                SinkKind::Vector => stores
                    .global_store
                    .symbols_for(entry.package)
                    .await
                    .map_err(crate::server::registry::RegistryError::from)?,
                SinkKind::Text | SinkKind::Graph => Vec::new(),
            };

            match entry.kind {
                // Package tantivy is kept current by `package_index_poller`,
                // which reads the same Text-sink outbox under its own watermark.
                SinkKind::Text => {}
                SinkKind::Vector => {
                    materialize_vector(
                        server.embedder(),
                        server.embedding_cache(),
                        server.vector_ledger(),
                        stores,
                        &symbols,
                    )
                    .await?
                }
                // The graph sink (catalog `UsageIndex`) is served by the IR
                // reverse-position index, not a symbol-document store. Terminus
                // is removed; in-process IR materialization + reverse-index
                // population is the pending consumer (INDEX-PLAN §5.5 / IP-7), so
                // this intent is a no-op for now rather than writing anywhere.
                SinkKind::Graph => {}
            }

            tracing::debug!(
                package = %entry.package,
                sink = %entry.kind,
                sequence = entry.id.0,
                symbols = symbols.len(),
                "fan-out upsert intent materialized"
            );
        }
        OutboxOp::Delete => {
            // Mirror tombstone: remove this package's search projection.
            // Blob / CAS data is retained — mirror keeps full history.
            match entry.kind {
                SinkKind::Text => delete_package_from_index(stores, entry.package).await?,
                SinkKind::Vector => delete_vector(stores, entry.package).await?,
                // See the upsert arm: the graph/usage plane moved to the IR
                // reverse index (Terminus removed); nothing to tombstone here yet.
                SinkKind::Graph => {}
            }

            tracing::debug!(
                package = %entry.package,
                sink = %entry.kind,
                sequence = entry.id.0,
                "fan-out delete intent materialized (search projection removed; CAS retained)"
            );
        }
    }
    Ok(())
}

/// Vector sink: embed each symbol's fully-qualified name (as code) and upsert a
/// [`VectorPoint`] into the remote (qdrant) store. The point id is derived
/// deterministically from the symbol id ([`PointId::from_symbol`]), so a re-embed
/// replaces the point rather than duplicating it. Each point carries the
/// filterable payload (`language`/`package`/`kind`) plus its `symbol_id`, so
/// search can recover the [`heart::SymbolId`] from a hit (the derived point id is
/// not itself the symbol uuid). Embeddings are taken through the shared cache so
/// a re-delivery of unchanged symbols is cheap.
async fn materialize_vector<M: EmbeddingModel>(
    embedder: &HttpEmbedder<M>,
    cache: &EmbeddingCache<M>,
    ledger: &std::sync::Mutex<crate::frontier::vector::UpsertLedger>,
    stores: &SourceStores<M>,
    symbols: &[heart::Symbol],
) -> ServerResult<()> {
    if symbols.is_empty() {
        return Ok(());
    }

    let mut staged = Vec::with_capacity(symbols.len());
    for symbol in symbols {
        let text = symbol.name.fully_qualified.as_str();
        let embedding = cache
            .get_or_embed(
                EmbeddingKey::new(M::id(), EmbedRole::Document, text),
                embedder,
                text,
            )
            .await
            .map_err(|error| crate::server::error::ServerError::from(error))?;
        let fingerprint = crate::frontier::vector::PointId {
            package: smol_str::SmolStr::new(symbol.package.as_uuid().to_string()),
            intro_hex: smol_str::SmolStr::new(symbol.id.as_uuid().to_string()),
            content_hash: blake3::hash(embedding_bytes(embedding.as_slice())).into(),
        };
        staged.push((
            fingerprint,
            VectorPoint {
                id: PointId::from_symbol(&symbol.id),
                vector: embedding,
                payload: crate::server::bakery::symbol_payload(symbol),
            },
        ));
    }

    let pending: Vec<_> = {
        let guard = ledger.lock().unwrap_or_else(|poison| poison.into_inner());
        staged
            .into_iter()
            .filter(|(fingerprint, _)| guard.needs_write(fingerprint))
            .collect()
    };
    if pending.is_empty() {
        return Ok(());
    }

    let points: Vec<_> = pending.iter().map(|(_, point)| point.clone()).collect();
    stores
        .semantics
        .upsert(points)
        .await
        .map_err(|error| crate::server::error::ServerError::from(error))?;

    let mut guard = ledger.lock().unwrap_or_else(|poison| poison.into_inner());
    guard.commit(
        &pending
            .iter()
            .map(|(fingerprint, _)| fingerprint.clone())
            .collect::<Vec<_>>(),
    );
    Ok(())
}

fn embedding_bytes(values: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

// ─────────────────────────────────────────────────────────────────────────────
// Delete helpers — mirror tombstone path (OutboxOp::Delete).
//
// Each removes a package's search projection from one sink. CAS / blob data is
// never touched — the mirror retains full history; only search visibility ends.
// ─────────────────────────────────────────────────────────────────────────────

/// Text sink: remove the package document from the replica-local package index.
async fn delete_package_from_index<M: EmbeddingModel>(
    stores: &SourceStores<M>,
    package: PackageId,
) -> ServerResult<()> {
    stores.packages.remove(package).await
}

/// Vector sink: delete all qdrant points for this package.
///
/// Delegates to [`RemoteStore::delete_by_package`], a filter-delete over the
/// `package` payload key every point carries (set by `symbol_payload` in
/// `materialize_vector`). The operation is idempotent — deleting already-absent
/// points is a no-op.
///
/// [`RemoteStore::delete_by_package`]: vector::remote::store::RemoteStore::delete_by_package
async fn delete_vector<M: EmbeddingModel>(
    stores: &SourceStores<M>,
    package: PackageId,
) -> ServerResult<()> {
    stores
        .semantics
        .delete_by_package(&package.as_uuid().to_string())
        .await
        .map_err(|error| crate::server::error::ServerError::from(error))
}

/// The storage-reclamation duty: a coarse periodic sweep that reclaims what is
/// provably no longer needed. Like the other fan-out loops it never returns
/// under normal operation, survives transient faults by logging and retrying at
/// the next tick, and does only idempotent work (a `DELETE` of already-consumed
/// rows), so an abort mid-sweep is safe.
///
/// **What it reclaims today — outbox rows below every sink's watermark.** Each
/// source's outbox accumulates one row per `(package, generation, sink)`; once
/// *every* derived sink has consumed a row it can never be re-delivered, so it
/// is dead weight. [`registry::coordination::Outbox::gc_consumed`] deletes every
/// row at or below the minimum watermark across all sinks — a conservative floor
/// that is `0` (deletes nothing) unless every sink has acked, so a lagging or
/// not-yet-created sink is never outrun.
///
/// **What it does NOT reclaim yet — orphaned CAS blobs.** Deleting a `cas/{hash}`
/// blob safely requires proving no live pointer or generation references it, and
/// the reference set is not currently enumerable from one place:
/// - `ptr/{package-id}` objects point at the *manifest* hash, and each manifest
///   in turn references its section hashes — so a live blob set is the transitive
///   closure over every package's current manifest, not a single table;
/// - `parse_status.content_hash` and `symbols.generation` name generations, but
///   there is no index from a blob hash back to "is any live manifest still
///   referencing it", and object stores offer no atomic "list-then-delete under a
///   reference lock", so a naive mark-and-sweep races an in-flight `put_manifest`
///   (which writes the blob before repointing `ptr/`) and could delete a blob a
///   concurrent emit is about to reference.
///
/// A correct blob GC therefore needs either (a) a generation-count / refcount
/// side table maintained transactionally with `put_manifest`, or (b) a
/// stop-the-world mark phase that first snapshots every `ptr/` → manifest →
/// section closure and only sweeps blobs older than that snapshot's start. Both
/// are additive follow-ups; until one lands, deleting blobs is not provably safe,
/// so this loop performs the outbox purge only.
///
/// The blob-enumeration primitive the closure above needs — [`Store::list_cas`]
/// — now exists (Phase 4f), so this loop emits an observability gauge of the
/// stored CAS object count. Reclamation itself stays deferred: enumeration is
/// necessary but not sufficient, and the missing half is still the
/// transactional refcount / snapshot-fence, not the ability to list.
///
/// TODO(blob-gc, DAEMON-PLAN §5-ops): add CAS blob reclamation once a
/// transactional blob-reference count (or a snapshot-fenced mark-sweep) exists;
/// see the closure/race notes above for exactly why the naive list-and-delete is
/// unsafe. The `list_cas()` gauge below is the read-only, race-free half.
#[tracing::instrument(skip_all, name = "cas_gc")]
pub(crate) async fn cas_gc<M: EmbeddingModel>(server: Arc<Server<M>>) {
    loop {
        for sourced in server.federation().in_precedence() {
            match sourced.value.outbox.gc_consumed().await {
                Ok(0) => {}
                Ok(reclaimed) => {
                    metrics::counter!("outbox_rows_reclaimed", "source" => sourced.source.to_string())
						.increment(reclaimed);
                    tracing::info!(
                        source = %sourced.source,
                        reclaimed,
                        "reclaimed consumed outbox rows"
                    );
                }
                Err(error) => {
                    tracing::warn!(source = %sourced.source, error = %error, "outbox GC failed");
                }
            }

            // Read-only observability: how many content-addressed blobs the store
            // holds. Safe under concurrent writes (it never deletes); a growing gap
            // between this and referenced generations is the signal a real GC is
            // eventually needed. Deletion remains deferred per the doc above.
            match sourced.value.blobs.list_cas().await {
                Ok(stored) => {
                    metrics::gauge!("cas_blobs_stored", "source" => sourced.source.to_string())
                        .set(stored.len() as f64);
                }
                Err(error) => {
                    tracing::warn!(source = %sourced.source, error = %error, "cas blob enumeration failed");
                }
            }
        }
        tokio::time::sleep(GC_INTERVAL).await;
    }
}

/// Keep every source's replica-local package index caught up to the catalog.
#[tracing::instrument(skip_all, name = "package_index_poller")]
pub(crate) async fn package_index_poller<M: EmbeddingModel>(server: Arc<Server<M>>) {
    let interval = server.config().limits.poll_interval;
    loop {
        for sourced in server.federation().in_precedence() {
            if let Err(error) = sourced
                .value
                .packages
                .synchronize(&sourced.value.global_store, &sourced.value.outbox)
                .await
            {
                tracing::warn!(source = %sourced.source, error = %error, "package index sync failed");
            }
        }
        tokio::time::sleep(interval).await;
    }
}

/// Keep every source's replica-local *symbol* text index (the precise-search
/// surface, [`crate::runtime::text::TextIndex`]) caught up to the catalog's
/// Text-sink outbox intents.
///
/// Distinct from [`package_index_poller`] above, which projects *packages*
/// into a separate replica-local tantivy index — the two are independent
/// projections over the same outbox sink, each with its own watermark file.
/// Each tick reuses the per-source [`crate::runtime::text::Poller`] built at
/// assembly (`Driver::connect_source`) rather than reconstructing one, since
/// it is not `Clone`.
#[tracing::instrument(skip_all, name = "text_index_poller")]
pub(crate) async fn text_index_poller<M: EmbeddingModel>(server: Arc<Server<M>>) {
    let interval = server.config().limits.poll_interval;
    loop {
        for sourced in server.federation().in_precedence() {
            let stores = sourced.value;
            if let Err(error) = stores.text_poller.poll_once(&stores.text).await {
                tracing::warn!(source = %sourced.source, error = %error, "text index poll failed");
            }
        }
        tokio::time::sleep(interval).await;
    }
}

/// How often to recompute corpus dependents + per-eco popularity percentiles.
/// Heavy-ish full scan; keep well below hot poll intervals.
const PACKAGE_SIGNALS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

/// Periodic sweep: reverse-dependency counts + per-ecosystem popularity CDF.
/// Writes only when values change (touch discipline so tantivy does not full-resync).
#[tracing::instrument(skip_all, name = "package_signals_poller")]
pub(crate) async fn package_signals_poller<M: EmbeddingModel>(server: Arc<Server<M>>) {
    // Stagger first run slightly so it does not pile onto boot with index sync.
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    loop {
        for sourced in server.federation().in_precedence() {
            let store = &sourced.value.global_store;
            match store.refresh_dependents().await {
                Ok(n) => {
                    if n > 0 {
                        tracing::info!(
                            source = %sourced.source,
                            updated = n,
                            "dependents sweep wrote updates"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        source = %sourced.source,
                        error = %error,
                        "dependents sweep failed"
                    );
                }
            }
            match store.refresh_popularity_percentiles().await {
                Ok(n) => {
                    if n > 0 {
                        tracing::info!(
                            source = %sourced.source,
                            updated = n,
                            "popularity percentile sweep wrote updates"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        source = %sourced.source,
                        error = %error,
                        "popularity percentile sweep failed"
                    );
                }
            }
        }
        tokio::time::sleep(PACKAGE_SIGNALS_INTERVAL).await;
    }
}
