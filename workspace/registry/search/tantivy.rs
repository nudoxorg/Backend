//! The tantivy abstraction over registry search.
//!
//! Postgres is the source of truth for what packages exist; this is a
//! **replica-local** tantivy index that mirrors a searchable projection of it.
//! Each replica keeps its own index on local disk and **polls postgres from a
//! watermark** to catch up — the index is derived, disposable, and rebuildable,
//! never authoritative. Search reads never touch postgres on the hot path.

use heart::{access::AccessContext, content::Generation, package::PackageId};
use tantivy::{Index, IndexReader};

use crate::{GlobalPackage, error::SearchError};

/// The last postgres position a replica has folded into its local index — the
/// watermark it resumes syncing from. Anchored to a [`Generation`] so a page
/// served against a newer sync can be detected as skewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SyncWatermark {
	/// The postgres logical sequence (e.g. an `updated_at` cursor or txid) last
	/// consumed.
	pub position: i64,

	/// The index generation this watermark reflects.
	pub generation: Generation,
}

/// A replica-local tantivy index over the searchable package projection.
pub struct PackageIndex {
	index: Index,
	reader: IndexReader,
	watermark: SyncWatermark,
}

impl PackageIndex {
	/// Open (or create) the replica-local index at `path`.
	pub fn open(path: &std::path::Path) -> Result<Self, SearchError> {
		let _ = path;
		todo!("build/load the schema (name, ecosystem, description, keywords), open reader")
	}

	/// The tantivy schema fields the package projection indexes. Defined once so
	/// writer and reader agree.
	pub fn schema() -> tantivy::schema::Schema {
		todo!("text: name/description/keywords; facet: ecosystem/visibility; stored: package_id")
	}

	/// Poll postgres from the current watermark and fold new/changed packages
	/// into the local index, advancing the watermark. Idempotent and resumable.
	/// `// tantivy commit + fsync run on spawn_blocking`.
	pub async fn sync_from(&mut self, pool: &sqlx::PgPool) -> Result<SyncWatermark, SearchError> {
		let _ = (&self.index, &mut self.watermark, pool);
		todo!("SELECT changed rows > watermark, upsert docs, commit, advance watermark")
	}

	/// Execute a text query, returning the matching package ids with their raw
	/// tantivy scores (ranking + access-filter happen a layer up). `// tantivy
	/// search runs on spawn_blocking`.
	pub fn query(
		&self,
		ctx: &AccessContext,
		text: &str,
		limit: usize,
	) -> Result<Vec<(PackageId, f32)>, SearchError> {
		let _ = (&self.reader, ctx, text, limit);
		todo!("parse query, collect top-k with scores, project to package ids")
	}

	/// Hydrate matched package ids into full [`GlobalPackage`] records (from the
	/// stored projection or a postgres batch-get).
	pub async fn hydrate(&self, ids: &[PackageId]) -> Result<Vec<GlobalPackage>, SearchError> {
		let _ = ids;
		todo!("batch-load GlobalPackage records for the matched ids")
	}

	/// The current sync watermark.
	pub fn watermark(&self) -> SyncWatermark { self.watermark }
}
