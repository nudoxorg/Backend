//! Defines explore behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the explore invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Package exploration against the local registry index: durable lexical projections and the registry catalog.
//!
//! Two durable sources answer the explore commands. `library/tantivy` holds the durable lexical
//! projections keyed by segment identity, and `library/catalog.db` holds the registry feed
//! observations a full acquire pipeline recorded. Neither is invented here: a row this module
//! returns carries exactly the facts its source retained, and a fact the source cannot provide is
//! a typed absence rather than a plausible value.

use core::{
    fmt::Write as _,
    future::Future,
    num::NonZeroUsize,
    task::{Context, Poll, Waker},
};
use std::{fs, path::Path, thread};

use heart_identity::{GenerationId, HASH_BYTES};
use interface_documents::Text;
use interface_search::Score;
use server_index_catalog::{FeedIdentity, FeedIdentityFault, TursoCatalog};
use server_index_core::{IndexSnapshot, LexicalOperation, MAX_LEXICAL_ROWS, MAX_SELECTED_SEGMENTS};
use server_index_tantivy::{TantivySegment, TantivySegmentStore, TantivySegmentStoreError};
use server_index_vocabulary::LexicalSegmentId;

use crate::Library;

/// Longest query any surface forwards to [`Library::index_search`].
pub const MAX_EXPLORE_QUERY_BYTES: usize = 128;
/// Longest package name any surface forwards to the registry catalog commands.
pub const MAX_PACKAGE_NAME_BYTES: usize = 128;
/// Page size when a surface does not choose one.
pub const DEFAULT_EXPLORE_LIMIT: usize = 40;
/// Largest page any surface may request from [`Library::index_search`].
pub const MAX_EXPLORE_LIMIT: usize = 100;

/// Directory name beneath [`crate::WorkspaceRoot::tantivy_dir`] that holds published lexical
/// segment projections. It mirrors the durable recipe of `server-index-tantivy`; a reader that
/// finds nothing here reports an empty index rather than guessing at other layouts.
const LEXICAL_PROJECTION_RECIPE: &str = "ntvx-v3";

/// Bounded index-search text, validated on construction.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ExploreQuery(Box<str>);

/// Exact query admission failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExploreQueryError {
    /// Nothing but whitespace was supplied.
    Empty,
    /// The text exceeds the fixed budget.
    TooLong {
        /// Observed bytes.
        observed: usize,
        /// Accepted bytes.
        maximum: usize,
    },
}

impl ExploreQuery {
    /// Admits index-search text.
    ///
    /// # Errors
    ///
    /// Rejects empty (after trimming) or oversized text.
    pub fn new(text: &str) -> Result<Self, ExploreQueryError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(ExploreQueryError::Empty);
        }
        if trimmed.len() > MAX_EXPLORE_QUERY_BYTES {
            return Err(ExploreQueryError::TooLong {
                observed: trimmed.len(),
                maximum: MAX_EXPLORE_QUERY_BYTES,
            });
        }
        Ok(Self(trimmed.into()))
    }

    /// Exact query text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated registry package name.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ExplorePackageName(Box<str>);

/// Exact package-name admission failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExplorePackageNameError {
    /// The name was empty.
    Empty,
    /// The name exceeds the fixed budget.
    TooLong {
        /// Observed bytes.
        observed: usize,
        /// Accepted bytes.
        maximum: usize,
    },
}

impl ExplorePackageName {
    /// Admits a registry package name.
    ///
    /// # Errors
    ///
    /// Rejects an empty or oversized name.
    pub fn new(name: &str) -> Result<Self, ExplorePackageNameError> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(ExplorePackageNameError::Empty);
        }
        if trimmed.len() > MAX_PACKAGE_NAME_BYTES {
            return Err(ExplorePackageNameError::TooLong {
                observed: trimmed.len(),
                maximum: MAX_PACKAGE_NAME_BYTES,
            });
        }
        Ok(Self(trimmed.into()))
    }

    /// Exact package name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Bounded index-search page size.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExploreLimit(NonZeroUsize);

impl ExploreLimit {
    /// The page size used when a surface does not choose one.
    pub const DEFAULT: Self = Self(match NonZeroUsize::new(DEFAULT_EXPLORE_LIMIT) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    });
    /// The largest page any surface may request.
    pub const MAXIMUM: Self = Self(match NonZeroUsize::new(MAX_EXPLORE_LIMIT) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    });

    /// Clamps a requested size into the accepted range; zero becomes the default.
    #[must_use]
    pub const fn clamped(requested: usize) -> Self {
        if requested == 0 {
            return Self::DEFAULT;
        }
        match NonZeroUsize::new(requested) {
            Some(value) if value.get() <= MAX_EXPLORE_LIMIT => Self(value),
            _ => Self::MAXIMUM,
        }
    }

    /// Accepted size.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

impl Default for ExploreLimit {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Exact version spelling the registry feed retained.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PackageVersionText(Box<str>);

impl PackageVersionText {
    /// Wraps one exact registry version spelling.
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self(text.to_owned().into_boxed_str())
    }

    /// Exact version spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The exact SHA-256 archive checksum the registry advertised for one version.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FeedChecksum([u8; 32]);

impl FeedChecksum {
    /// Wraps the catalog's fixed-width checksum value.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The exact checksum bytes for comparison or durable encoding.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    /// The eight-hex abbreviation a row prints, as keys do.
    #[must_use]
    pub fn abbreviation(self) -> String {
        checksum_hex(self).get(..8).unwrap_or_default().to_owned()
    }
}

/// Lower-case hex of the exact checksum, for surfaces that spell it in full.
#[must_use]
pub fn checksum_hex(checksum: FeedChecksum) -> String {
    let mut hex = String::with_capacity(checksum.0.len() * 2);
    for byte in checksum.0 {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Whether the registry snapshot offers this version for materialization.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct VersionActive(bool);

impl VersionActive {
    /// Wraps the registry's active flag for one version.
    #[must_use]
    pub const fn of(active: bool) -> Self {
        Self(active)
    }

    /// Whether the version is active; an inactive version is what registries call yanked.
    #[must_use]
    pub const fn is_active(self) -> bool {
        self.0
    }
}

/// Full-snapshot cycle in which the registry last observed one version row.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Cycle(u64);

impl Cycle {
    /// Wraps one full-snapshot cycle number.
    #[must_use]
    pub const fn of(cycle: u64) -> Self {
        Self(cycle)
    }

    /// Exact cycle number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One version row exactly as the registry feed recorded it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageVersionRow {
    /// Exact version spelling.
    pub version: PackageVersionText,
    /// Exact registry archive checksum.
    pub checksum: FeedChecksum,
    /// Whether the snapshot marks the version active (not yanked).
    pub active: VersionActive,
    /// Snapshot cycle in which the row was observed.
    pub cycle: Cycle,
}

/// Every version row the catalog recorded for one package, in catalog order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageVersionRows {
    /// Rows in the order the catalog returned them: ascending by exact version spelling.
    pub rows: Box<[PackageVersionRow]>,
}

/// One package's latest version and full version history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageProfile {
    /// Highest version by plain byte-wise lexical order over the exact spellings; the catalog
    /// defines no semver ordering, so no other ranking is claimed. `None` only for an empty
    /// history, which the version commands report as [`ExploreError::NotFound`].
    pub latest: Option<PackageVersionRow>,
    /// Full version history.
    pub versions: PackageVersionRows,
}

/// One row of an index search: a matched durable lexical projection entry.
///
/// The durable projection stores one canonical term per row and a fixed-width document identity;
/// the identity carries no package coordinate, so a row names the matched term and nothing else.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexHitRow {
    /// Entity display text the projection retained for the matched document.
    pub document: Text,
    /// Deterministic lexical score of the match.
    pub score: Score,
    /// The exact term that matched, as text.
    pub matched: Box<str>,
}

/// One complete answer to an index search.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexSearchPage {
    /// Rows in deterministic rank order.
    pub hits: Box<[IndexHitRow]>,
    /// How much of the published index the search consulted.
    pub coverage: ExploreCoverage,
}

/// Why an index search covered less than everything, or nothing at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExploreUnavailable {
    /// The durable lexical projection directory holds no published segment.
    EmptyIndex,
    /// The durable store reported a fault class named by its own slug.
    StoreFault {
        /// Stable slug naming the fault class.
        slug: &'static str,
    },
    /// The registry catalog database does not exist beneath the workspace root.
    CatalogAbsent,
}

/// How much of the durable index one search actually consulted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExploreCoverage {
    /// Every published segment was opened and searched.
    Complete,
    /// Only part of the published index was searched.
    Partial {
        /// Segments searched.
        searched: usize,
        /// Segments published.
        total: usize,
    },
    /// The search could not run, and says why instead of returning zero rows.
    Unavailable {
        /// Exact reason.
        reason: ExploreUnavailable,
    },
}

/// Exact explore failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExploreError {
    /// The query exceeded the fixed budget.
    QueryTooLong {
        /// Observed bytes.
        observed: usize,
        /// Accepted bytes.
        maximum: usize,
    },
    /// The durable lexical projection could not be opened, reopened, or searched.
    IndexStore {
        /// Bounded description from the store, in the store's own words.
        detail: Box<str>,
    },
    /// The registry catalog database does not exist beneath the workspace root.
    CatalogAbsent,
    /// The registry catalog failed, in the catalog's own words.
    Catalog {
        /// Bounded description from the catalog.
        detail: Box<str>,
    },
    /// The catalog records no version rows for this package.
    NotFound {
        /// Exact name that was looked up, returned rather than dropped.
        package: ExplorePackageName,
    },
    /// The catalog records this package name under more than one ecosystem.
    Ambiguous {
        /// Exact name that was looked up.
        package: ExplorePackageName,
        /// Rows observed for the name.
        observed: usize,
    },
}

impl Library {
    /// Runs one index search over the durable lexical projections.
    ///
    /// The durable store is searched as a shadow projection: published segments are discovered by
    /// their content-addressed directories, reopened, and composed into a snapshot selection whose
    /// identity is derived from those segment identities over one fixed shadow generation, because
    /// no generation proof is durably retained beside the projections in this build. The derived
    /// identity never reaches a reply.
    ///
    /// # Errors
    ///
    /// Returns [`ExploreError::IndexStore`] when a published segment cannot be reopened or the
    /// backend search fails. An empty projection is a state, not an error: it returns
    /// [`ExploreCoverage::Unavailable`] with [`ExploreUnavailable::EmptyIndex`].
    pub fn index_search(
        &self,
        query: &ExploreQuery,
        limit: ExploreLimit,
    ) -> Result<IndexSearchPage, ExploreError> {
        let store = open_index_store(&self.root().tantivy_dir())?;
        let published = published_segments(store.root()).map_err(|error| {
            ExploreError::IndexStore {
                detail: format!("{error}").into_boxed_str(),
            }
        })?;
        if published.is_empty() {
            return Ok(IndexSearchPage {
                hits: Box::new([]),
                coverage: ExploreCoverage::Unavailable {
                    reason: ExploreUnavailable::EmptyIndex,
                },
            });
        }
        let total = published.len();
        let searched = total.min(MAX_SELECTED_SEGMENTS);
        let selection: Vec<LexicalSegmentId> =
            published.iter().copied().take(searched).collect();
        let opened = reopen_segments(&store, &selection)?;
        let hits = search_segments(&store, &opened, query, limit)?;
        let coverage = if searched < total {
            ExploreCoverage::Partial { searched, total }
        } else {
            ExploreCoverage::Complete
        };
        Ok(IndexSearchPage {
            hits,
            coverage,
        })
    }

    /// Lists every version the registry catalog recorded for one package.
    ///
    /// # Errors
    ///
    /// Returns [`ExploreError::CatalogAbsent`] when `library/catalog.db` does not exist,
    /// [`ExploreError::Catalog`] when the catalog fails, [`ExploreError::NotFound`] when the
    /// catalog holds no rows for the name, and [`ExploreError::Ambiguous`] when the rows span
    /// more than one ecosystem.
    pub fn package_versions(
        &self,
        name: &ExplorePackageName,
    ) -> Result<PackageVersionRows, ExploreError> {
        let rows = feed_rows(self, name)?;
        Ok(PackageVersionRows {
            rows: rows.into_boxed_slice(),
        })
    }

    /// Shows one package's latest version and full version history.
    ///
    /// The latest version is the maximum by plain byte-wise lexical order over the exact version
    /// spellings the catalog retained; the catalog defines no semver ordering, so none is claimed.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Library::package_versions`].
    pub fn package_profile(
        &self,
        name: &ExplorePackageName,
    ) -> Result<PackageProfile, ExploreError> {
        let rows = feed_rows(self, name)?;
        let latest = rows
            .iter()
            .max_by(|left, right| left.version.as_str().cmp(right.version.as_str()))
            .cloned();
        Ok(PackageProfile {
            latest,
            versions: PackageVersionRows {
                rows: rows.into_boxed_slice(),
            },
        })
    }
}

fn feed_rows(
    library: &Library,
    name: &ExplorePackageName,
) -> Result<Vec<PackageVersionRow>, ExploreError> {
    let path = library.root().catalog_db();
    if !path.exists() {
        return Err(ExploreError::CatalogAbsent);
    }
    let spelled = path.to_str().ok_or_else(|| ExploreError::Catalog {
        detail: "the catalog database path is not valid UTF-8".into(),
    })?;
    let feed = FeedIdentity::new(format!("crates.io/sparse/{}", name.as_str())).map_err(
        |fault| ExploreError::Catalog {
            detail: feed_fault_detail(fault),
        },
    )?;
    let catalog = block_on(TursoCatalog::open(spelled)).map_err(|error| ExploreError::Catalog {
        detail: format!("{error}").into_boxed_str(),
    })?;
    let observations = block_on(catalog.feed_observations(&feed)).map_err(|error| {
        ExploreError::Catalog {
            detail: format!("{error}").into_boxed_str(),
        }
    })?;
    let mut rows = Vec::new();
    let mut ecosystems: Vec<&str> = Vec::new();
    for observation in &observations {
        if observation.package != name.as_str() {
            continue;
        }
        if !ecosystems.contains(&observation.ecosystem.as_str()) {
            ecosystems.push(observation.ecosystem.as_str());
        }
        rows.push(PackageVersionRow {
            version: PackageVersionText(observation.version.clone().into_boxed_str()),
            checksum: FeedChecksum::from_bytes(observation.checksum.as_bytes()),
            active: VersionActive(observation.active),
            cycle: Cycle(observation.cycle),
        });
    }
    if rows.is_empty() {
        return Err(ExploreError::NotFound {
            package: name.clone(),
        });
    }
    if ecosystems.len() > 1 {
        return Err(ExploreError::Ambiguous {
            package: name.clone(),
            observed: rows.len(),
        });
    }
    Ok(rows)
}

fn feed_fault_detail(fault: FeedIdentityFault) -> Box<str> {
    let detail = match fault {
        FeedIdentityFault::Empty => "the package name is empty".to_owned(),
        FeedIdentityFault::TooLong => {
            "the package name exceeds the registry feed identity budget".to_owned()
        }
        FeedIdentityFault::InvalidCharacter => {
            "the package name carries a byte the registry feed identity grammar does not admit \
             (lower-case letters, digits, `.`, `-`, `/`, `:`, `_`)"
                .to_owned()
        }
    };
    detail.into_boxed_str()
}

fn open_index_store(root: &Path) -> Result<TantivySegmentStore, ExploreError> {
    TantivySegmentStore::open(root).map_err(|error| ExploreError::IndexStore {
        detail: format!("{error}").into_boxed_str(),
    })
}

fn store_detail(error: &TantivySegmentStoreError) -> Box<str> {
    format!("{error}").into_boxed_str()
}

/// Discovers the published lexical segments beneath one durable projection root.
fn published_segments(root: &Path) -> std::io::Result<Vec<LexicalSegmentId>> {
    let mut ids = Vec::new();
    for entry in fs::read_dir(root.join(LEXICAL_PROJECTION_RECIPE))? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(id) = segment_identity(name) else {
            continue;
        };
        ids.push(id);
    }
    ids.sort();
    Ok(ids)
}

/// Decodes one content-addressed segment directory name into its identity.
fn segment_identity(name: &str) -> Option<LexicalSegmentId> {
    if name.len() != HASH_BYTES * 2 {
        return None;
    }
    let mut bytes = [0_u8; HASH_BYTES];
    for (index, cell) in bytes.iter_mut().enumerate() {
        let cell_text = name.get(index * 2..index * 2 + 2)?;
        *cell = u8::from_str_radix(cell_text, 16).ok()?;
    }
    LexicalSegmentId::try_from(bytes).ok()
}

fn reopen_segments(
    store: &TantivySegmentStore,
    ids: &[LexicalSegmentId],
) -> Result<Vec<TantivySegment>, ExploreError> {
    ids.iter()
        .map(|id| store.reopen(*id))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ExploreError::IndexStore {
            detail: store_detail(&error),
        })
}

fn search_segments(
    store: &TantivySegmentStore,
    opened: &[TantivySegment],
    query: &ExploreQuery,
    limit: ExploreLimit,
) -> Result<Box<[IndexHitRow]>, ExploreError> {
    let ids = segment_ids(opened);
    let selection = IndexSnapshot::new(shadow_generation(), &[], &ids).map_err(|error| {
        ExploreError::IndexStore {
            detail: format!("{error}").into_boxed_str(),
        }
    })?;
    let segments: Vec<&TantivySegment> = opened.iter().collect();
    let snapshot = store.compose(selection, &segments).map_err(|error| {
        ExploreError::IndexStore {
            detail: store_detail(&error),
        }
    })?;
    let bytes = query.as_str().as_bytes();
    let operations = [LexicalOperation::new(bytes), LexicalOperation::prefix(bytes)];
    let requested = limit.get();
    let mut output: Vec<Option<_>> = vec![None; requested];
    let mut scratch: Vec<Option<_>> = vec![None; requested.saturating_mul(2)];
    let mut candidates: Vec<Option<_>> =
        vec![None; MAX_LEXICAL_ROWS.saturating_mul(opened.len())];
    let written = snapshot
        .search_terms(&operations, requested, &mut output, &mut scratch, &mut candidates)
        .map_err(|error| ExploreError::IndexStore {
            detail: store_detail(&error),
        })?;
    let mut hits = Vec::with_capacity(written);
    for hit in output.into_iter().flatten().take(written) {
        let term = core::str::from_utf8(hit.term()).map_err(|_| ExploreError::IndexStore {
            detail: "a matched lexical term is not valid UTF-8, so the durable projection \
                     disagrees with its source"
                .into(),
        })?;
        hits.push(IndexHitRow {
            document: Text::new(term),
            score: Score(u32::from(hit.score())),
            matched: Box::<str>::from(term),
        });
    }
    Ok(hits.into_boxed_slice())
}

fn segment_ids(opened: &[TantivySegment]) -> Vec<LexicalSegmentId> {
    opened.iter().map(TantivySegment::id).collect()
}

/// The fixed generation every shadow selection is derived from. Its only property that matters is
/// that it is one constant, so the same published segments derive the same selection identity on
/// every reader; no canonical generation proof exists for these projections.
const SHADOW_GENERATION_INPUT: &[u8] = b"interface-library.index-search.shadow-selection.v1";

fn shadow_generation() -> GenerationId {
    GenerationId::from_canonical_bytes(SHADOW_GENERATION_INPUT)
}

/// Drives one catalog future to completion on the calling thread.
///
/// Spin policy: the future is polled in a loop against [`Waker::noop`], so nothing can wake it
/// but this loop. Every future reached here is one short local Turso statement, and the callers
/// are the CLI, the MCP server, and the GUI engine thread — a plain worker thread, never the
/// interactive main thread — so instead of adding an executor dependency the loop yields to the
/// scheduler between pending polls, which bounds the cost of a slow device without burning a core.
fn block_on<Output>(future: impl Future<Output = Output>) -> Output {
    let mut future = core::pin::pin!(future);
    let waker = Waker::noop();
    loop {
        match future.as_mut().poll(&mut Context::from_waker(waker)) {
            Poll::Ready(output) => return output,
            Poll::Pending => thread::yield_now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_and_names_reject_empty_and_oversized_text() {
        assert_eq!(ExploreQuery::new("   "), Err(ExploreQueryError::Empty));
        assert_eq!(
            ExploreQuery::new(&"a".repeat(MAX_EXPLORE_QUERY_BYTES + 1)),
            Err(ExploreQueryError::TooLong {
                observed: MAX_EXPLORE_QUERY_BYTES + 1,
                maximum: MAX_EXPLORE_QUERY_BYTES,
            })
        );
        let admitted = ExploreQuery::new("serde").unwrap_or_else(|error| {
            panic!("a bounded query is admitted: {error:?}");
        });
        assert_eq!(admitted.as_str(), "serde");
        assert_eq!(
            ExplorePackageName::new(""),
            Err(ExplorePackageNameError::Empty)
        );
        assert!(
            ExplorePackageName::new(&"a".repeat(MAX_PACKAGE_NAME_BYTES + 1)).is_err(),
            "an oversized name is refused"
        );
    }

    #[test]
    fn limits_clamp_into_the_closed_range() {
        assert_eq!(ExploreLimit::clamped(0), ExploreLimit::DEFAULT);
        assert_eq!(ExploreLimit::clamped(0).get(), DEFAULT_EXPLORE_LIMIT);
        assert_eq!(
            ExploreLimit::clamped(MAX_EXPLORE_LIMIT + 1),
            ExploreLimit::MAXIMUM
        );
        assert_eq!(ExploreLimit::clamped(9).get(), 9);
    }

    #[test]
    fn checksums_abbreviate_like_keys_and_spell_in_full() {
        let checksum = FeedChecksum::from_bytes([0xab; 32]);
        assert_eq!(checksum.abbreviation(), "abababab");
        assert_eq!(checksum_hex(checksum).len(), 64);
        assert!(checksum_hex(checksum).starts_with("ab"));
    }

    #[test]
    fn segment_directory_names_decode_and_reject_strangers() {
        let identity = LexicalSegmentId::from_canonical_bytes(b"fixture-segment");
        let mut hex = String::with_capacity(HASH_BYTES * 2);
        for byte in identity.as_ref() {
            let _ = write!(hex, "{byte:02x}");
        }
        assert_eq!(segment_identity(&hex), Some(identity));
        assert!(segment_identity("ntvx").is_none(), "a recipe name is not an id");
        let mut corrupted = hex.clone();
        corrupted.replace_range(0..2, "zz");
        assert!(
            segment_identity(&corrupted).is_none(),
            "a non-hex name is not an id"
        );
    }

    #[test]
    fn profile_latest_is_the_lexical_maximum_not_a_semver_claim() {
        let row = |version: &str| PackageVersionRow {
            version: PackageVersionText(version.into()),
            checksum: FeedChecksum::from_bytes([0; 32]),
            active: VersionActive(true),
            cycle: Cycle(1),
        };
        let rows = vec![row("0.100.0"), row("0.9.0")];
        let latest = rows
            .iter()
            .max_by(|left, right| left.version.as_str().cmp(right.version.as_str()))
            .cloned();
        assert_eq!(
            latest.map(|row| row.version.as_str().to_owned()),
            Some("0.9.0".to_owned()),
            "byte-wise lexical order ranks 9 above 100; no semver ordering is claimed"
        );
    }
}
