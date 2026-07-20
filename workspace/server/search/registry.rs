//! The registry-search surface — finding *packages* (not symbols).

use std::path::Path;
use std::sync::Arc;

use futures::Stream;
use heart::{ContentHash, Cursor, Language, Scored};
use crate::registry::GlobalPackage;
use crate::registry::metadata::Synonyms;
use crate::registry::search::{
	SearchKey, PackageSearchRequest, search_page,
	tantivy::{PackageIndex, SyncWatermark},
};

use heart::PageSpecification;

use crate::error::{BadRequestReason, ServerError, ServerResult};
use crate::search::query::Query;

/// One source's replica-local package index, shared between the query surface
/// and the catalog sync poller. The `tokio::sync::Mutex` exists because
/// [`PackageIndex::sync_from`] mutates (single writer); queries hold the lock
/// only for the in-memory tantivy read.
pub struct PackageSearchIndex {
	index: tokio::sync::Mutex<PackageIndex>,
}

impl PackageSearchIndex {
	/// Open (or create) the replica-local index at `directory`. Hydration
	/// happens per-tick via [`Self::synchronize`], off the catalog outbox feed.
	pub fn open(directory: &Path) -> ServerResult<Self> {
		let index = PackageIndex::open(directory).map_err(crate::registry::RegistryError::from)?;
		Ok(Self { index: tokio::sync::Mutex::new(index) })
	}

	/// Pull one batch of changed packages from the catalog outbox feed into
	/// the local index, advancing the watermark. Idempotent and resumable;
	/// the sync poller's tick.
	pub async fn synchronize(
		&self,
		global: &crate::registry::index::GlobalStore<crate::CatalogEngine>,
		outbox: &crate::registry::coordination::Outbox<crate::CatalogEngine>,
	) -> ServerResult<SyncWatermark> {
		let mut index = self.index.lock().await;
		index.sync_from(global, outbox).await.map_err(|error| crate::registry::RegistryError::from(error).into())
	}

	/// One materialized page of scored packages for a free-text query,
	/// keyset-resumed after `after` when given.
	///
	/// # Ranking
	///
	/// The full five-stage ranking pipeline (BM25 × quality kink + exact/contains
	/// name bonus + diversity pass + representative pull-up + downloads bubble)
	/// runs **once** over the whole over-fetched candidate set, producing a single
	/// total order; each hit carries a strictly-descending rank score derived from
	/// its ordinal in that order (see [`crate::registry::search::collect_ranked_hits`]).
	/// Every page — first or resumed — is a slice of that one order, so the
	/// `(score, id)` keyset cursor advances monotonically with no scoring seam at
	/// the page boundary. There is no first-page/subsequent-page split.
	///
	/// # Ecosystem scope
	///
	/// `ecosystem` is the API-level scope (wins over inline `lang:` tokens in the
	/// structured parse). Pass `None` for an unscoped search across all ecosystems.
	///
	/// # Synonyms
	///
	/// When `synonyms` is `Some`, free terms are expanded after structured parse
	/// so the EXPANDED BM25 tier can fire. `None` skips expansion.
	pub async fn page(
		&self,
		text: &str,
		limit: usize,
		after: Option<&Cursor<SearchKey>>,
		ecosystem: Option<Language>,
		synonyms: Option<&Synonyms>,
	) -> ServerResult<Vec<Scored<GlobalPackage>>> {
		let index = self.index.lock().await;

		// Delegate keyset pagination to the shared registry search_page so
		// production and tests share one resume path.
		let request = PackageSearchRequest {
			text: text.to_owned(),
			ecosystem,
			limit,
			after: after.cloned(),
			semantic: Vec::new(),
		};

		let page = search_page(&index, &request, synonyms)
			.await
			.map_err(crate::registry::RegistryError::from)?;

		Ok(page.items)
	}

	/// The snapshot hash a page's resume cursor is anchored to — derived from
	/// the sync watermark, so a cursor can be recognized as pre-dating a resync.
	pub async fn snapshot(&self) -> ContentHash {
		let index = self.index.lock().await;
		ContentHash::of_bytes(&index.watermark().position.to_le_bytes())
	}

	/// Remove a package's document from the replica-local tantivy index.
	///
	/// Used by the mirror tombstone path (`OutboxOp::Delete`). The removal is
	/// committed immediately; the sync watermark is not advanced (it tracks
	/// the postgres position, not individual removals). Blob / CAS data is
	/// never touched.
	pub async fn remove(&self, package: heart::PackageId) -> ServerResult<()> {
		let mut index = self.index.lock().await;
		index.remove(package).map_err(|error| crate::registry::RegistryError::from(error).into())
	}
}

/// The package-search adapter in the server's query vocabulary.
pub struct RegistrySearchSurface {
	index: Arc<PackageSearchIndex>,
}

impl RegistrySearchSurface {
	/// Wrap one source's shared package index.
	pub fn new(index: Arc<PackageSearchIndex>) -> Self { Self { index } }

	/// Search packages for `query`, optionally scoped to `ecosystem` and expanded
	/// via `synonyms` when heuristics are loaded on the server.
	pub async fn search(
		&self,
		query: &Query,
		page: &PageSpecification,
		ecosystem: Option<Language>,
		synonyms: Option<&Synonyms>,
	) -> Result<impl Stream<Item = Result<Scored<GlobalPackage>, ServerError>> + Send, ServerError> {
		let after = page
			.cursor
			.as_deref()
			.map(|token| {
				Cursor::<SearchKey>::decode(token)
					.map_err(|source| BadRequestReason::InvalidCursor {
						token: token.to_owned(),
						source,
					})
			})
			.transpose()?;
		let hits = self
			.index
			.page(
				query.text(),
				page.limit as usize,
				after.as_ref(),
				ecosystem,
				synonyms,
			)
			.await?;
		Ok(futures::stream::iter(hits.into_iter().map(Ok)))
	}
}

#[cfg(test)]
mod tests {
	use heart::{Edition, Language, PackageVersion, RegistryOrigin, ResolutionState, Toolchain};
	use crate::registry::{
		GlobalPackage, Package,
		metadata::SearchFacets,
		package::{Coordinates, PackageName},
		search::{PackageSearchRequest, tantivy::PackageIndex},
	};
	use smol_str::SmolStr;

	/// Create a temporary directory for tantivy.
	struct TempDir(std::path::PathBuf);

	impl TempDir {
		fn new(prefix: &str) -> Self {
			let path = std::env::temp_dir()
				.join(format!("server-search-test-{prefix}-{}", uuid::Uuid::new_v4()));
			std::fs::create_dir_all(&path).unwrap();
			Self(path)
		}
		fn path(&self) -> &std::path::Path { &self.0 }
	}

	impl Drop for TempDir {
		fn drop(&mut self) {
			let _ = std::fs::remove_dir_all(&self.0);
		}
	}

	/// Build a Rust package with the given name, quality, and keywords.
	fn make_package(name: &str, quality_ppm: u32, keywords: &[&str]) -> GlobalPackage {
		let coordinates = Coordinates {
			origin: RegistryOrigin::CratesIo,
			name: PackageName::new(Language::Rust, name).expect("fixture name is valid"),
			version: PackageVersion::try_from((Language::Rust, "1.0.0"))
				.expect("fixture version is valid"),
		};
		let toolchain = Toolchain::Rust {
			compiler: semver::Version::new(1, 85, 0),
			edition: Edition::E2024,
		};
		let package = Package { coordinates, toolchain };
		let id = package.id();
		let facets = Some(SearchFacets {
			keywords: keywords.iter().map(|&k| SmolStr::new(k)).collect(),
			quality_ppm,
			..Default::default()
		});
		GlobalPackage { id, package, state: ResolutionState::Unindexed { needed: false }, facets }
	}

	/// Build a searchable tantivy replica with the given records.
	fn build_index(dir: &TempDir, records: &[GlobalPackage]) -> PackageIndex {
		let mut index = PackageIndex::open(dir.path()).expect("tempdir index opens");
		index.absorb(records.iter(), 1).expect("records absorb");
		index
	}

	/// The fused ranking pipeline promotes the exact-name match above a
	/// higher-BM25 non-exact match and above a high-quality unrelated crate.
	///
	/// Corpus:
	/// - "tokio-extended": non-exact, slightly more text overlap with "tokio"
	/// - "tokio": exact name match
	/// - "serde": very high quality but completely unrelated name
	///
	/// Query: "tokio"
	/// Expected: "tokio" appears before "serde" (exact bonus must beat quality signal)
	#[tokio::test]
	async fn fused_ranking_exact_name_beats_higher_bm25() {
		let dir = TempDir::new("exact-name");
		let tokio_ext = make_package("tokio-extended", 600_000, &["async", "tokio", "runtime"]);
		let tokio_exact = make_package("tokio", 500_000, &["async", "runtime"]);
		let unrelated = make_package("serde", 900_000, &["serialization", "json"]);

		let records = vec![tokio_ext, tokio_exact, unrelated];
		let index = build_index(&dir, &records);

		let query = PackageSearchRequest {
			text: "tokio".to_owned(),
			ecosystem: None,
			limit: 10,
			after: None,
			semantic: Vec::new(),
		};

		let page = crate::registry::search::search_page(&index, &query, None)
			.await
			.expect("query executes");

		let names: Vec<&str> = page
			.items
			.iter()
			.map(|s| s.value.package.coordinates.name.original())
			.collect();

		// Exact match must be first.
		assert!(
			names.first().copied() == Some("tokio"),
			"exact-name match 'tokio' must rank first; got order: {names:?}"
		);

		// Unrelated high-quality crate must not outrank the exact match.
		let tokio_pos = names.iter().position(|&n| n == "tokio").unwrap_or(usize::MAX);
		let serde_pos = names.iter().position(|&n| n == "serde").unwrap_or(usize::MAX);
		assert!(
			tokio_pos < serde_pos,
			"exact-match 'tokio' (pos {tokio_pos}) must outrank unrelated 'serde' (pos {serde_pos})"
		);
	}

	/// Multi-parent packages (same (name, version) via different origins) collapse
	/// to a single result through the multi_parent::merge de-dup stage.
	#[tokio::test]
	async fn fused_ranking_deduplicates_multi_parent() {
		let dir = TempDir::new("dedup");
		let primary = make_package("serde", 800_000, &["serialization"]);
		// Mirror: same logical (name, version), different id (mirrors a different
		// registry origin in production; here the id is derived from a distinct
		// coordinates key produced by changing origin).
		let mirror_coords = Coordinates {
			origin: RegistryOrigin::Custom {
				name: "mirror.example".into(),
				url: url::Url::parse("https://mirror.example").expect("fixture url"),
			},
			name: PackageName::new(Language::Rust, "serde").expect("fixture name"),
			version: PackageVersion::try_from((Language::Rust, "1.0.0")).expect("fixture version"),
		};
		let mirror_pkg = Package {
			coordinates: mirror_coords,
			toolchain: Toolchain::Rust {
				compiler: semver::Version::new(1, 85, 0),
				edition: Edition::E2024,
			},
		};
		let mirror_id = mirror_pkg.id();
		let mirror = GlobalPackage {
			id: mirror_id,
			package: mirror_pkg,
			state: ResolutionState::Unindexed { needed: false },
			facets: primary.facets.clone(),
		};
		assert_ne!(primary.id, mirror.id, "precondition: distinct ids");

		let records = vec![primary, mirror];
		let index = build_index(&dir, &records);

		let query = PackageSearchRequest {
			text: "serde".to_owned(),
			ecosystem: None,
			limit: 10,
			after: None,
			semantic: Vec::new(),
		};

		let page = crate::registry::search::search_page(&index, &query, None)
			.await
			.expect("query executes");

		assert_eq!(
			page.items.len(),
			1,
			"multi-parent duplicates must collapse to one result; got {} items",
			page.items.len()
		);
	}

	/// The full five-stage pipeline must not panic on a realistic corpus size
	/// (diversity pass activates at 25+ items, representative pull-up at 7+).
	#[tokio::test]
	async fn fused_ranking_pipeline_runs_without_panic_on_real_corpus() {
		let dir = TempDir::new("pipeline");
		let records: Vec<GlobalPackage> = (0..30_u32)
			.map(|i| make_package(&format!("crate-{i:02}"), i * 30_000, &["async"]))
			.collect();
		let index = build_index(&dir, &records);

		let query = PackageSearchRequest {
			text: "crate".to_owned(),
			ecosystem: None,
			limit: 10,
			after: None,
			semantic: Vec::new(),
		};

		let page = crate::registry::search::search_page(&index, &query, None)
			.await
			.expect("full ranking pipeline must not panic");

		assert!(!page.items.is_empty(), "results must be non-empty");
		assert!(page.items.len() <= 10, "limit must be respected");
	}
}
