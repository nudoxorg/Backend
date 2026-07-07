//! The registry-search surface — finding *packages* (not symbols).

use std::path::Path;
use std::sync::Arc;

use futures::Stream;
use heart::{ContentHash, Cursor, Score, Scored};
use registry::GlobalPackage;
use registry::search::{SearchKey, tantivy::{PackageIndex, SyncWatermark}};

use crate::error::{ServerError, ServerResult};
use crate::search::query::{Pagination, Query};

/// One source's replica-local package index, shared between the query surface
/// and the postgres sync poller. The `tokio::sync::Mutex` exists because
/// [`PackageIndex::sync_from`] mutates (single writer); queries hold the lock
/// only for the in-memory tantivy read.
pub struct PackageSearchIndex {
	index: tokio::sync::Mutex<PackageIndex>,
	pool: sqlx::PgPool,
}

impl PackageSearchIndex {
	/// Open (or create) the replica-local index at `directory`, hydrating from
	/// the given postgres pool.
	pub fn open(directory: &Path, pool: sqlx::PgPool) -> ServerResult<Self> {
		let index = PackageIndex::open(directory).map_err(registry::RegistryError::from)?;
		Ok(Self { index: tokio::sync::Mutex::new(index), pool })
	}

	/// Pull one batch of changed packages from postgres into the local index,
	/// advancing the watermark. Idempotent and resumable; the sync poller's tick.
	pub async fn synchronize(&self) -> ServerResult<SyncWatermark> {
		let mut index = self.index.lock().await;
		index.sync_from(&self.pool).await.map_err(|error| registry::RegistryError::from(error).into())
	}

	/// One materialized page of scored packages for a free-text query,
	/// keyset-resumed after `after` when given.
	pub async fn page(
		&self,
		text: &str,
		limit: usize,
		after: Option<&Cursor<SearchKey>>,
	) -> ServerResult<Vec<Scored<GlobalPackage>>> {
		let index = self.index.lock().await;

		// The index yields top-k only; over-fetch so a resumed page still fills
		// after dropping everything at-or-before the cursor position.
		let fetch = limit.saturating_add(1).saturating_mul(4).max(64);
		let matched = index.query(text, fetch).map_err(registry::RegistryError::from)?;

		let ranked = matched.into_iter().filter_map(|(package, raw_score)| {
			// A non-finite tantivy score is unrepresentable in the shared Score;
			// drop the hit rather than corrupt the ranking.
			Score::try_new(raw_score).ok().map(|score| (package, score))
		});
		let resumed: Vec<(heart::PackageId, Score)> = match after {
			None => ranked.collect(),
			Some(cursor) => {
				let (last_score, last_package) = cursor.after;
				// Keyset: strictly after the cursor position in (score desc, id asc)
				// order, so a vanished cursor row cannot swallow a neighbour.
				ranked
					.filter(|(package, score)| {
						*score < last_score || (*score == last_score && *package > last_package)
					})
					.collect()
			}
		};

		let selected: Vec<_> = resumed.into_iter().take(limit).collect();
		let hydrated = index
			.hydrate(&selected.iter().map(|(package, _)| *package).collect::<Vec<_>>())
			.await
			.map_err(registry::RegistryError::from)?;

		// Hydration order is the store's business; re-align on the id.
		let mut records: std::collections::HashMap<_, _> =
			hydrated.into_iter().map(|package| (package.id, package)).collect();
		Ok(selected
			.into_iter()
			.filter_map(|(package, score)| {
				records.remove(&package).map(|record| Scored::new(record, score))
			})
			.collect())
	}

	/// The snapshot hash a page's resume cursor is anchored to — derived from
	/// the sync watermark, so a cursor can be recognized as pre-dating a resync.
	pub async fn snapshot(&self) -> ContentHash {
		let index = self.index.lock().await;
		ContentHash::of_bytes(&index.watermark().position.to_le_bytes())
	}
}

/// The package-search adapter in the server's query vocabulary.
pub struct RegistrySearchSurface {
	index: Arc<PackageSearchIndex>,
}

impl RegistrySearchSurface {
	/// Wrap one source's shared package index.
	pub fn new(index: Arc<PackageSearchIndex>) -> Self { Self { index } }

	pub async fn search(
		&self,
		query: &Query,
		page: &Pagination,
	) -> Result<impl Stream<Item = Result<Scored<GlobalPackage>, ServerError>> + Send, ServerError> {
		let after = page
			.after
			.as_deref()
			.map(|token| {
				Cursor::<SearchKey>::decode(token)
					.map_err(|error| ServerError::BadRequest(format!("invalid cursor: {error}")))
			})
			.transpose()?;
		let hits = self
			.index
			.page(query.text(), page.limit.get() as usize, after.as_ref())
			.await?;
		Ok(futures::stream::iter(hits.into_iter().map(Ok)))
	}
}
