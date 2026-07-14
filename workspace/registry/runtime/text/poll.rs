//! Keeping the text index fresh: tantivy polls postgres for newly-indexed work
//! and pulls it into the local index (rather than postgres pushing into
//! tantivy). This keeps the replica-local index a pure projection of the durable
//! source, catchable-up after a restart from a persisted watermark.

use std::{
	io::ErrorKind,
	path::{Path, PathBuf},
	time::Duration,
};

use serde::{Deserialize, Serialize};
use sqlx::Row;

use heart::Retryable;

use crate::{error::{RowDecodeError, TextError}, text::index::TextIndex};

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
}

impl Watermark {
	/// The bottom watermark: nothing consumed yet.
	pub const BOTTOM: Watermark = Watermark { sequence: 0 };
}

/// How many pending `outbox` intents one poll consumes at most.
const BATCH_LIMIT: i64 = 64;

/// The backoff ceiling when transient poll errors stack up.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// The file the watermark persists to, next to the tantivy directory.
const WATERMARK_FILE: &str = "watermark.json";

/// Pending change intents for the text sink, newest last:
/// `(outbox sequence, package id)`.
const PENDING_SQL: &str = "SELECT seq, package_id FROM outbox \
	WHERE sink_kind = 'text' AND seq > $1 ORDER BY seq ASC LIMIT $2";

/// The serving projection of every symbol in the given packages, joined with
/// the owning package's ecosystem token.
const SYMBOLS_SQL: &str = "SELECT s.id, s.package_id, s.fq_name, s.kind, p.language \
	FROM symbols s JOIN packages p ON p.id = s.package_id \
	WHERE s.package_id = ANY($1)";

/// Drives the postgres → tantivy poll loop: reads rows changed since the
/// [`Watermark`], upserts them into the local [`TextIndex`], commits, and
/// advances the watermark durably.
pub struct Poller {
	/// The durable source the local index is a projection of.
	pool: sqlx::PgPool,
	/// The watermark file, derived from the replica's tantivy directory.
	watermark_path: PathBuf,
	/// How long [`run`](Self::run) sleeps between healthy polls.
	interval: Duration,
}

impl Poller {
	/// Configure a poller feeding the index that lives in `index_directory`,
	/// persisting its watermark alongside it.
	///
	/// This is the public constructor the server builds a text catch-up loop from
	/// (it owns the postgres pool + the replica's tantivy directory). The
	/// per-intent fan-out materialization in the server upserts a package's
	/// symbols into the same [`TextIndex`] directly (via
	/// [`TextIndex::upsert_batch`], the exact write [`poll_once`](Self::poll_once)
	/// performs), so a Text intent and a poll cycle converge to the same
	/// upsert-by-id state.
	pub fn new(pool: sqlx::PgPool, index_directory: &Path, interval: Duration) -> Self {
		Self { pool, watermark_path: index_directory.join(WATERMARK_FILE), interval }
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
	/// The postgres read is async; the tantivy upsert/commit runs on
	/// spawn_blocking (see [`TextIndex`]).
	pub async fn poll_once(&self, index: &TextIndex) -> Result<Watermark, TextError> {
		let current = self.watermark().await?;

		let pending = sqlx::query(PENDING_SQL)
			.bind(i64::try_from(current.sequence).unwrap_or(i64::MAX))
			.bind(BATCH_LIMIT)
			.fetch_all(&self.pool)
			.await
			.map_err(TextError::Poll)?;
		if pending.is_empty() {
			return Ok(current);
		}

		let mut advanced = current.sequence;
		let mut packages = Vec::with_capacity(pending.len());
		for row in &pending {
			let sequence: i64 = row.try_get("seq").map_err(TextError::Poll)?;
			advanced = advanced.max(sequence.max(0) as u64);
			packages.push(row.try_get::<heart::Guid, _>("package_id").map_err(TextError::Poll)?);
		}

		let rows = sqlx::query(SYMBOLS_SQL)
			.bind(&packages)
			.fetch_all(&self.pool)
			.await
			.map_err(TextError::Poll)?;
		let symbols = rows.iter().map(symbol_from_row).collect::<Result<Vec<_>, _>>()?;

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
			packages = packages.len(),
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

/// Decode one joined `symbols × packages` row into the [`heart::Symbol`] the
/// index serves. The plain name is the last path segment of the stored
/// fully-qualified name (postgres does not carry it separately).
fn symbol_from_row(row: &sqlx::postgres::PgRow) -> Result<heart::Symbol, TextError> {
	let id: heart::Guid = row.try_get("id").map_err(TextError::Poll)?;
	let package: heart::Guid = row.try_get("package_id").map_err(TextError::Poll)?;
	let fq_name: String = row.try_get("fq_name").map_err(TextError::Poll)?;
	let kind: String = row.try_get("kind").map_err(TextError::Poll)?;
	let language: String = row.try_get("language").map_err(TextError::Poll)?;

	Ok(heart::Symbol {
		id: heart::SymbolId::from_uuid(id),
		package: heart::PackageId::from_uuid(package),
		ecosystem: language
			.parse()
			.map_err(|_| TextError::Row(RowDecodeError::UnknownEcosystem { raw: language }))?,
		name: heart::Name {
			plain: plain_name(&fq_name).into(),
			fully_qualified: fq_name.as_str().into(),
		},
		kind: crate::text::index::parse_kind(&kind)
			.ok_or_else(|| TextError::Row(RowDecodeError::UnknownSymbolKind { raw: kind }))?,
	})
}

/// The leaf identifier of a fully-qualified name, across ecosystem separators
/// (`::` for Rust, `.` for Python/TypeScript, `/` defensively).
fn plain_name(fully_qualified: &str) -> &str {
	fully_qualified
		.rsplit(['/', '.', ':'])
		.find(|segment| !segment.is_empty())
		.unwrap_or(fully_qualified)
}
