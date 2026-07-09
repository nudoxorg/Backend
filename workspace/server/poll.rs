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
use runtime::vector::EmbeddingModel;

use crate::coordination::indexing::Indexer;
use crate::error::ServerResult;
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
			match consume_once(sourced.value, sink).await {
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
		materialize(stores, entry).await?;
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
/// KNOWN GAP: full materialization needs to read a package's symbol projection
/// back out of the global index (or decode its IR blob through the compiler),
/// and neither API exists yet — `GlobalStore` only *writes* symbols
/// (`upsert_symbol`) and `runtime::text::Poller` has no public constructor. The
/// durable-cursor plumbing here is real; the per-sink write is a traced no-op
/// until one of those surfaces lands. `save::rebuild` can re-enqueue any
/// generation, so nothing is lost by advancing.
async fn materialize<M: EmbeddingModel>(
	stores: &SourceStores<M>,
	entry: &OutboxEntry,
) -> ServerResult<()> {
	let _ = &stores.text;
	tracing::debug!(
		package = %entry.package,
		sink = %entry.kind,
		sequence = entry.id.0,
		"acknowledged fan-out intent (materialization pending a symbol-projection read API)"
	);
	Ok(())
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
