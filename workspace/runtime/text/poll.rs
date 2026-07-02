//! Keeping the text index fresh: tantivy polls postgres for newly-indexed work
//! and pulls it into the local index (rather than postgres pushing into
//! tantivy). This keeps the replica-local index a pure projection of the durable
//! source, catchable-up after a restart from a persisted watermark.

use serde::{Deserialize, Serialize};

use heart::Generation;

use crate::{error::TextError, text::index::TextIndex};

/// A durable pointer into postgres marking how far the local index has been
/// caught up. Persisted alongside the tantivy directory so a restarted replica
/// resumes from where it left off rather than rebuilding from scratch.
///
/// Monotone: it only ever advances, so the poll loop is idempotent and
/// crash-safe (re-processing from a stale watermark just re-upserts, which is a
/// no-op by id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watermark {
	/// The last postgres change sequence pulled into the index.
	pub sequence: u64,
	/// The generation the most-recently-pulled batch reflected, for skew checks.
	pub generation: Generation,
}

/// Drives the postgres → tantivy poll loop: reads rows changed since the
/// [`Watermark`], upserts them into the local [`TextIndex`], commits, and
/// advances the watermark durably.
pub struct Poller {
	// sqlx::PgPool (source), the target TextIndex, the poll interval, and the
	// persisted watermark location; kept opaque so the transport can change.
}

impl Poller {
	/// Load the persisted watermark for this replica's index (bottom if none).
	pub async fn watermark(&self) -> Result<Watermark, TextError> {
		todo!("read the persisted watermark next to the tantivy dir")
	}

	/// Pull one batch of changes since the current watermark into `index`,
	/// commit, advance and persist the watermark, and return the new watermark
	/// (unchanged if nothing was pending).
	///
	/// The postgres read is async; the tantivy upsert/commit runs on
	/// spawn_blocking (see [`TextIndex`]).
	pub async fn poll_once(&self, index: &TextIndex) -> Result<Watermark, TextError> {
		let _ = index;
		todo!("SELECT rows > watermark; index.upsert_batch on spawn_blocking; commit; advance watermark")
	}

	/// Run the poll loop until the returned handle is dropped/cancelled, polling
	/// at the configured interval.
	pub async fn run(&self, index: &TextIndex) -> Result<(), TextError> {
		let _ = index;
		todo!("poll_once then sleep(interval) in a loop, with backoff on transient errors")
	}
}
