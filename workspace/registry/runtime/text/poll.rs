//! Keeping the text index fresh: tantivy polls the catalog outbox for
//! newly-indexed work and pulls it into the local index (rather than the
//! catalog pushing into tantivy). This keeps the replica-local index a pure
//! projection of the durable source, catchable-up after a restart from a
//! persisted watermark.

use std::{
	io::ErrorKind,
	path::{Path, PathBuf},
	time::Duration,
};

use serde::{Deserialize, Serialize};

use heart::Retryable;

use index::engine::VersioningEngine;

use crate::coordination::{Outbox, OutboxSeq, SinkKind};
use crate::index::GlobalStore;
use crate::runtime::{error::TextError, text::index::TextIndex};

/// A durable pointer into the catalog outbox marking how far the local index
/// has been caught up. Persisted alongside the tantivy directory so a
/// restarted replica resumes from where it left off rather than rebuilding
/// from scratch.
///
/// Monotone: it only ever advances, so the poll loop is idempotent and
/// crash-safe (re-processing from a stale watermark just re-upserts, which is a
/// no-op by id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watermark {
	/// The last outbox sequence pulled into the index.
	pub sequence: u64,
}

impl Watermark {
	/// The bottom watermark: nothing consumed yet.
	pub const BOTTOM: Watermark = Watermark { sequence: 0 };
}

/// How many pending outbox intents one poll consumes at most.
const BATCH_LIMIT: usize = 64;

/// The backoff ceiling when transient poll errors stack up.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// The file the watermark persists to, next to the tantivy directory.
const WATERMARK_FILE: &str = "watermark.json";

/// Drives the catalog-outbox → tantivy poll loop: reads Text intents since the
/// [`Watermark`], loads each changed package's serving symbols, upserts them
/// into the local [`TextIndex`], commits, and advances the watermark durably.
pub struct Poller<Engine: VersioningEngine> {
	/// The durable outbox feed the local index is a projection of.
	outbox: Outbox<Engine>,
	/// The symbol source for changed packages.
	global: GlobalStore<Engine>,
	/// The watermark file, derived from the replica's tantivy directory.
	watermark_path: PathBuf,
	/// How long [`run`](Self::run) sleeps between healthy polls.
	interval: Duration,
}

impl<Engine: VersioningEngine + Send + Sync> Poller<Engine> {
	/// Configure a poller feeding the index that lives in `index_directory`,
	/// persisting its watermark alongside it.
	///
	/// The per-intent fan-out materialization in the server upserts a package's
	/// symbols into the same [`TextIndex`] directly (via
	/// [`TextIndex::upsert_batch`], the exact write [`poll_once`](Self::poll_once)
	/// performs), so a Text intent and a poll cycle converge to the same
	/// upsert-by-id state.
	pub fn new(
		outbox: Outbox<Engine>,
		global: GlobalStore<Engine>,
		index_directory: &Path,
		interval: Duration,
	) -> Self {
		Self {
			outbox,
			global,
			watermark_path: index_directory.join(WATERMARK_FILE),
			interval,
		}
	}

	/// Load the persisted watermark for this replica's index (bottom if none).
	pub async fn watermark(&self) -> Result<Watermark, TextError> {
		match tokio::fs::read(&self.watermark_path).await {
			Ok(bytes) => serde_json::from_slice(&bytes).map_err(TextError::Codec),
			Err(error) if error.kind() == ErrorKind::NotFound => Ok(Watermark::BOTTOM),
			Err(error) => Err(TextError::Io(error)),
		}
	}

	/// Durably record an advanced watermark (write-then-rename, so a crash never
	/// leaves a torn file — worst case we replay, which upsert-by-id absorbs).
	async fn record(&self, watermark: Watermark) -> Result<(), TextError> {
		let bytes = serde_json::to_vec(&watermark)
			.expect("a watermark is a plain integer and serializes infallibly");
		let staging = self.watermark_path.with_extension("json.tmp");
		tokio::fs::write(&staging, &bytes).await.map_err(TextError::Io)?;
		tokio::fs::rename(&staging, &self.watermark_path).await.map_err(TextError::Io)?;
		Ok(())
	}

	/// Pull one batch of changes since the current watermark into `index`,
	/// commit, advance and persist the watermark, and return the new watermark
	/// (unchanged if nothing was pending).
	///
	/// The catalog read is cheap and local; the tantivy upsert/commit runs on
	/// a blocking section (see [`TextIndex`]).
	pub async fn poll_once(&self, index: &TextIndex) -> Result<Watermark, TextError> {
		let current = self.watermark().await?;

		let pending = self
			.outbox
			.read_since(
				SinkKind::Text,
				OutboxSeq(i64::try_from(current.sequence).unwrap_or(i64::MAX)),
				BATCH_LIMIT,
			)
			.await
			.map_err(TextError::Poll)?;
		if pending.is_empty() {
			return Ok(current);
		}

		let mut advanced = current.sequence;
		let mut symbols = Vec::new();
		for entry in &pending {
			advanced = advanced.max(entry.id.0.max(0) as u64);
			symbols.extend(
				self.global
					.symbols_for(entry.package)
					.await
					.or_else(|error| match error {
						// A version row can trail its intent; the next intent
						// redelivers it.
						crate::error::IndexError::NotFound { .. } => Ok(Vec::new()),
						other => Err(TextError::Symbols(other)),
					})?,
			);
		}

		// The blocking boundary: tantivy indexing is CPU/disk-bound. On a
		// multi-threaded runtime it moves off the async workers; on a
		// current-thread runtime (tests) it runs inline, which is safe — just
		// not concurrent.
		blocking(|| index.upsert_batch(&symbols))?;

		let watermark = Watermark { sequence: advanced };
		self.record(watermark).await?;
		tracing::info!(
			from = current.sequence,
			to = watermark.sequence,
			intents = pending.len(),
			symbols = symbols.len(),
			"text index caught up"
		);
		Ok(watermark)
	}

	/// Run the poll loop until the returned handle is dropped/cancelled, polling
	/// at the configured interval.
	pub async fn run(&self, index: &TextIndex) -> Result<(), TextError> {
		let mut delay = self.interval;
		loop {
			match self.poll_once(index).await {
				Ok(watermark) => {
					tracing::debug!(sequence = watermark.sequence, "poll cycle complete");
					delay = self.interval;
				}
				// Transient faults back off (doubling, capped) instead of hammering.
				Err(error) if error.is_retryable() => {
					tracing::warn!(%error, backoff = ?delay, "transient poll failure; backing off");
					delay = (delay * 2).min(MAX_BACKOFF);
				}
				Err(error) => return Err(error),
			}
			tokio::time::sleep(delay).await;
		}
	}
}

/// Dispatch CPU/disk-bound work off the async workers when the runtime has
/// blocking capacity, falling back to inline execution on current-thread
/// runtimes (where `block_in_place` would panic).
fn blocking<T>(work: impl FnOnce() -> T) -> T {
	match tokio::runtime::Handle::try_current() {
		Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
			tokio::task::block_in_place(work)
		}
		_ => work(),
	}
}
