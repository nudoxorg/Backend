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

use registry::coordination::{OutboxEntry, SinkKind};
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

/// Supervise the indexing queue worker: restart it with backoff whenever the
/// underlying loop surfaces an error (a poisoned postgres connection, say).
pub(crate) async fn queue_worker<M: EmbeddingModel>(server: Arc<Server<M>>) {
	let indexer = Indexer::new(server);
	loop {
		match indexer.run_worker().await {
			// `run_worker` only returns on failure; log and restart.
			Ok(()) => unreachable!("the worker loop runs until shutdown"),
			Err(error) => {
				tracing::error!(error = %error, "queue worker failed; restarting");
				tokio::time::sleep(SUPERVISOR_BACKOFF).await;
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
			match consume_once(&server, sourced.value, sink).await {
				Ok(0) => {}
				Ok(consumed) => {
					metrics::counter!("outbox_intents_consumed", "sink" => sink.to_string())
						.increment(consumed as u64);
				}
				Err(error) => {
					tracing::warn!(%sink, source = %sourced.source, error = %error, "outbox poll failed");
				}
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
	let watermark = stores.outbox.read_watermark(sink).await.map_err(registry::RegistryError::from)?;
	let entries = stores
		.outbox
		.read_since(sink, watermark, OUTBOX_BATCH)
		.await
		.map_err(registry::RegistryError::from)?;
	for entry in &entries {
		materialize(server, stores, entry).await?;
		stores
			.outbox
			.advance_watermark(sink, entry.id)
			.await
			.map_err(registry::RegistryError::from)?;
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
		.map_err(registry::RegistryError::from)?;

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

/// Keep every source's replica-local package index caught up to postgres.
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
pub(crate) async fn text_index_poller<M: EmbeddingModel>(server: Arc<Server<M>>) {
	let interval = server.config().limits.poll_interval;
	loop {
		for sourced in server.federation().in_precedence() {
			let outbox = &sourced.value.outbox;
			let lag = async {
				let head = outbox.head(SinkKind::Text).await?;
				let consumed = outbox.read_watermark(SinkKind::Text).await?;
				Ok::<i64, registry::error::OutboxError>((head.0 - consumed.0).max(0))
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
