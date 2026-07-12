//! The supervised background loops `serve()` runs alongside the HTTP surface:
//! the queue worker driving indexing jobs, one outbox consumer per derived
//! sink, and the replica-local index sync/watermark pollers.
//!
//! Every loop follows the same discipline: it never returns under normal
//! operation, it survives (logs + backs off) transient faults, and it is torn
//! down by task abort at its next await point when `serve()` winds down —
//! every unit of work it performs is idempotent, so an abort mid-tick is safe.

use std::sync::Arc;
use std::time::Duration;

use crate::registry::coordination::{OutboxEntry, SinkKind};
use runtime::vector::{
	EmbeddingCache, EmbeddingKey, EmbeddingModel, EmbeddingPurpose, SymbolPoint,
};

use crate::coordination::indexing::Indexer;
use crate::error::ServerResult;
use crate::search::semantic::embedder::HttpEmbedder;
use crate::{Server, SourceStores};

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
	drain: sandbox::CancelToken,
) {
	let compiler = server.compiler_client().clone();
	let indexer = Indexer::new(Arc::clone(&server), compiler);
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
	let watermark = stores.outbox.read_watermark(sink).await.map_err(crate::registry::RegistryError::from)?;
	let entries = stores
		.outbox
		.read_since(sink, watermark, OUTBOX_BATCH)
		.await
		.map_err(crate::registry::RegistryError::from)?;
	for entry in &entries {
		materialize(server, stores, entry).await?;
		stores
			.outbox
			.advance_watermark(sink, entry.id)
			.await
			.map_err(crate::registry::RegistryError::from)?;
	}
	Ok(entries.len())
}

/// Materialize one fan-out intent into its derived store.
///
/// Reads the package's symbol projection back out of the global index
/// ([`GlobalStore::symbols_for`] — the symmetric read of the `upsert_symbol`
/// write path) and writes it into the sink named by `entry.kind`:
/// - [`SinkKind::Text`] upserts the symbols into the replica-local tantivy index
///   (upsert-by-id, the same write the text [`Poller`] performs);
/// - [`SinkKind::Vector`] embeds each symbol and upserts a [`SymbolPoint`] into
///   qdrant (keyed by symbol uuid, so a re-embed replaces in place);
/// - [`SinkKind::Graph`] writes the `Symbol` documents into terminus (keyed by
///   `@id`, so a re-insert replaces in place).
///
/// Every write is idempotent (upsert-by-id / replace-by-key), so the outbox is
/// free to re-deliver on crash without duplicating or corrupting the projection.
///
/// [`Poller`]: runtime::text::Poller
/// [`GlobalStore::symbols_for`]: registry::index::GlobalStore::symbols_for
async fn materialize<M: EmbeddingModel>(
	server: &Server<M>,
	stores: &SourceStores<M>,
	entry: &OutboxEntry,
) -> ServerResult<()> {
	// The symbol projection is the shared input to every sink: read it once from
	// the global index (mirrors the `upsert_symbol` write). An intent for a
	// package with no persisted symbols is a well-formed empty materialization.
	let symbols = stores
		.global_store
		.symbols_for(entry.package)
		.await
		.map_err(crate::registry::RegistryError::from)?;

	match entry.kind {
		SinkKind::Text => materialize_text(stores, &symbols)?,
		SinkKind::Vector => {
			materialize_vector(server.embedder(), server.embedding_cache(), stores, &symbols).await?
		}
		SinkKind::Graph => materialize_graph(stores, &symbols).await?,
	}

	tracing::debug!(
		package = %entry.package,
		sink = %entry.kind,
		sequence = entry.id.0,
		symbols = symbols.len(),
		"fan-out intent materialized"
	);
	Ok(())
}

/// Text sink: upsert the package's symbols into the replica-local tantivy index.
/// Upsert-by-id and a single commit — exactly the write the text [`Poller`]
/// performs, so replay is a no-op.
///
/// [`Poller`]: runtime::text::Poller
fn materialize_text<M: EmbeddingModel>(
	stores: &SourceStores<M>,
	symbols: &[heart::Symbol],
) -> ServerResult<()> {
	if symbols.is_empty() {
		return Ok(());
	}
	stores
		.text
		.upsert_batch(symbols)
		.map_err(|error| crate::error::ServerError::Runtime(error.into()))
}

/// Vector sink: embed each symbol's fully-qualified name (as code) and upsert a
/// [`SymbolPoint`] into qdrant. The point id is the symbol uuid, so a re-embed
/// replaces the point rather than duplicating it. Embeddings are taken through
/// the shared cache so a re-delivery of unchanged symbols is cheap.
async fn materialize_vector<M: EmbeddingModel>(
	embedder: &HttpEmbedder<M>,
	cache: &EmbeddingCache<M>,
	stores: &SourceStores<M>,
	symbols: &[heart::Symbol],
) -> ServerResult<()> {
	use tower::ServiceExt;

	if symbols.is_empty() {
		return Ok(());
	}

	let mut points = Vec::with_capacity(symbols.len());
	for symbol in symbols {
		let text = symbol.name.fully_qualified.as_str();
		let embedding = cache
			.get_or_embed(
				EmbeddingKey::new(M::id(), text),
				embedder,
				text,
				EmbeddingPurpose::Code,
			)
			.await
			.map_err(|error| crate::error::ServerError::Runtime(error.into()))?;
		points.push(SymbolPoint {
			symbol: symbol.id,
			embedding,
			ecosystem: symbol.ecosystem,
			purpose: EmbeddingPurpose::Code,
		});
	}

	// The uploader is a tower service over `Vec<SymbolPoint>` that chunks to the
	// qdrant batch ceiling internally; drive it once with `oneshot`.
	stores
		.semantics
		.uploader()
		.oneshot(points)
		.await
		.map_err(|error| crate::error::ServerError::Runtime(error.into()))
}

/// Graph sink: write the package's `Symbol` documents into terminus. Keyed by
/// document `@id` (derived from the symbol uuid), so a re-insert replaces in
/// place.
async fn materialize_graph<M: EmbeddingModel>(
	stores: &SourceStores<M>,
	symbols: &[heart::Symbol],
) -> ServerResult<()> {
	if symbols.is_empty() {
		return Ok(());
	}
	stores
		.graph
		.insert_symbols(symbols)
		.await
		.map_err(|error| crate::error::ServerError::Runtime(error.into()))
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

/// Keep every source's replica-local package index caught up to postgres.
#[tracing::instrument(skip_all, name = "package_index_poller")]
pub(crate) async fn package_index_poller<M: EmbeddingModel>(server: Arc<Server<M>>) {
	let interval = server.config().limits.poll_interval;
	loop {
		for sourced in server.federation().in_precedence() {
			if let Err(error) = sourced.value.packages.synchronize().await {
				tracing::warn!(source = %sourced.source, error = %error, "package index sync failed");
			}
		}
		tokio::time::sleep(interval).await;
	}
}

/// Watch each source's text-sink watermark lag (outbox head minus consumed
/// position) and surface it as a gauge, so a stalled text index is visible
/// before users notice stale search.
#[tracing::instrument(skip_all, name = "text_index_poller")]
pub(crate) async fn text_index_poller<M: EmbeddingModel>(server: Arc<Server<M>>) {
	let interval = server.config().limits.poll_interval;
	loop {
		for sourced in server.federation().in_precedence() {
			let outbox = &sourced.value.outbox;
			let lag = async {
				let head = outbox.head(SinkKind::Text).await?;
				let consumed = outbox.read_watermark(SinkKind::Text).await?;
				Ok::<i64, crate::registry::error::OutboxError>((head.0 - consumed.0).max(0))
			};
			match lag.await {
				Ok(pending) => {
					metrics::gauge!("text_index_watermark_lag", "source" => sourced.source.to_string())
						.set(pending as f64);
				}
				Err(error) => {
					tracing::warn!(source = %sourced.source, error = %error, "text watermark poll failed");
				}
			}
		}
		tokio::time::sleep(interval).await;
	}
}
