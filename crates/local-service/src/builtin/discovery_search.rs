//! Rebuildable Tantivy projections for source-only catalog and local declaration search.

use crate::discovery::{DiscoverySearchDocument, DiscoveryStore};
use backend_engine::registry::{
    DiscoveryFacet, DiscoveryMetadata, DiscoverySourceIdentity, RegistryEcosystem,
};
use backend_engine::{
    CommittedViewDelta, DependencyFacts, ForgeAcquisitionResult, ForgeCoordinate, ForgeFact,
    ForgePackageManifest, ForgeSearchRecord, ProductPackageCoordinate, RowChange, RowId, ViewDelta,
    ViewRoot, ViewStateRoot,
};
use backend_library::{Fragment, Row};
use serde::{Deserialize, Serialize};
use std::cell::OnceCell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::ops::Bound;
use std::sync::Arc;
use tantivy::collector::TopDocs;
use tantivy::query::{
    BooleanQuery, ConstScoreQuery, EmptyQuery, EnableScoring, FuzzyTermQuery, Occur, PhraseQuery,
    Query, RangeQuery, RegexQuery, TermQuery, Weight,
};
use tantivy::schema::{FAST, Field, IndexRecordOption, STRING, Schema, TEXT};
use tantivy::{Index, IndexReader, IndexWriter, Order, TantivyDocument, Term};

#[path = "discovery_search/release_projection.rs"]
mod release_projection;
use release_projection::{ReleasePostingField, ReleasePostingPage, VersionedReleaseProjection};

#[cfg(feature = "search-bench")]
#[path = "discovery_search/benchmark.rs"]
pub mod benchmark;

const WRITER_MEMORY_BYTES: usize = 15_000_000;
const SOURCE_PIN_WRITER_MEMORY_BYTES: usize = WRITER_MEMORY_BYTES;
const MAX_QUERY_BYTES: usize = 4096;
const MAX_CURSOR_QUERY_BYTES: usize = 16 * 1024;
const MAX_SEARCH_PAGE_SIZE: usize = 256;
const MAX_INTERNAL_SEARCH_PAGE_SIZE: usize = MAX_SEARCH_PAGE_SIZE + 1;
const MAX_CURSOR_SORT_KEY_BYTES: usize = 32 * 1024;
const MAX_CURSOR_OFFSET: usize = 1_000_000_000;
const BOUNDARY_TOKEN: &str = "catalogboundary";
const SORT_KEY_FIELD: &str = "sort_key";
const MAX_RELEASES_PER_GROUP: usize = 16;
const MAX_LINEAGE_FACET_VALUES: usize = 16_384;

/// Identity for one source-specific discovery claim. A second configured source
/// may publish the same coordinate and remains a distinct search result.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct DiscoverySearchKey {
    pub(crate) source: DiscoverySearchSource,
    pub(crate) coordinate: ProductPackageCoordinate,
    /// Exact canonical package lineage derived from the coordinate. Search
    /// fields fold case independently; grouping preserves source identity.
    pub(crate) lineage: String,
    pub(crate) manifest_path: Option<String>,
}

/// Cross-plane stable package identity. Registry and forge observations retain
/// their source authority; acquired catalog rows use their optional recorded
/// authority without being merged into discovery claims.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub(crate) enum LineageSearchSource {
    Discovery(DiscoverySearchSource),
    Acquired {
        ecosystem: RegistryEcosystem,
        source: Option<[u8; 32]>,
    },
}

impl LineageSearchSource {
    pub(crate) const fn ecosystem(&self) -> RegistryEcosystem {
        match self {
            Self::Discovery(source) => source.ecosystem(),
            Self::Acquired { ecosystem, .. } => *ecosystem,
        }
    }
}

/// One exact, source-specific package lineage, independent of its releases.
/// Tantivy text fields fold case when documents are inserted; this identity
/// retains case when the source's package names are case-sensitive.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LineageKey {
    pub(crate) source: LineageSearchSource,
    pub(crate) ecosystem: RegistryEcosystem,
    #[serde(deserialize_with = "deserialize_lineage")]
    pub(crate) lineage: String,
}

impl LineageKey {
    pub(crate) fn admit(&self) -> Result<(), String> {
        if self.lineage.is_empty()
            || self.lineage.len() > MAX_CURSOR_SORT_KEY_BYTES
            || self.lineage.chars().any(char::is_control)
            || self.source.ecosystem() != self.ecosystem
        {
            return Err("lineage key exceeds its bounds or has inconsistent authority".to_owned());
        }
        if let LineageSearchSource::Discovery(DiscoverySearchSource::Forge(source)) = &self.source
            && (!source.coordinate.identity_is_valid() || source.ecosystem != self.ecosystem)
        {
            return Err("forge lineage has invalid source authority".to_owned());
        }
        Ok(())
    }
}

/// Exact keyset location after one lineage's best matching evidence tier.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiscoverySearchPosition {
    pub(crate) tier: u8,
    pub(crate) key: LineageKey,
}

/// Source kind and identity carried through the shared package-search index.
/// Forge claims stay distinct from registry claims even when both publish the
/// same canonical package/version coordinate.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub(crate) enum DiscoverySearchSource {
    Registry(DiscoverySourceIdentity),
    Forge(ForgeSearchSourceIdentity),
}

impl DiscoverySearchSource {
    pub(crate) const fn ecosystem(&self) -> RegistryEcosystem {
        match self {
            Self::Registry(source) => source.ecosystem(),
            Self::Forge(source) => source.ecosystem,
        }
    }

    pub(crate) fn id(&self) -> [u8; 32] {
        match self {
            Self::Registry(source) => source.id(),
            Self::Forge(source) => source.coordinate.identity(),
        }
    }

    fn registry(&self) -> Option<DiscoverySourceIdentity> {
        match self {
            Self::Registry(source) => Some(*source),
            Self::Forge(_) => None,
        }
    }
}

/// A forge repository/ref identity. The exact revision is retained so that
/// different pinned source revisions remain independently attributable.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub(crate) struct ForgeSearchSourceIdentity {
    pub(crate) coordinate: ForgeCoordinate,
    pub(crate) ecosystem: RegistryEcosystem,
}

/// One immutable package/version claim extracted from an admitted forge tree.
/// Text facets retain their source availability; generic terms carry exact
/// source coordinates, commit IDs, and pin digests for search.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ForgeSearchDocument {
    pub(crate) source: ForgeSearchSourceIdentity,
    pub(crate) coordinate: ProductPackageCoordinate,
    pub(crate) lineage: String,
    pub(crate) manifest_path: Option<String>,
    pub(crate) metadata: DiscoveryMetadata,
    pub(crate) readme: DiscoveryFacet<String>,
    pub(crate) generic_terms: Vec<String>,
}

/// One forge manifest that is addressable only by its pinned source revision.
/// It deliberately has no package coordinate: a commit is source identity,
/// never a release version.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ForgeSourcePinSearchKey {
    pub(crate) source: ForgeCoordinate,
    pub(crate) ecosystem: RegistryEcosystem,
    pub(crate) manifest_path: String,
    pub(crate) resolved_commit: backend_engine::ForgeObjectId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ForgeSourcePinSearchDocument {
    pub(crate) key: ForgeSourcePinSearchKey,
    pub(crate) name: String,
    pub(crate) metadata: DiscoveryMetadata,
    /// Bounded README text is indexed with description terms just like the
    /// package-backed forge projection, while its availability stays separate.
    pub(crate) readme: DiscoveryFacet<String>,
    pub(crate) generic_terms: Vec<String>,
}

impl ForgeSourcePinSearchDocument {
    /// Projects only manifests for which no valid package release PURL was
    /// admitted. The row remains searchable by source, manifest, and commit.
    pub(crate) fn from_search_record(record: &ForgeSearchRecord) -> Result<Vec<Self>, String> {
        let mut documents = Vec::new();
        for manifest in record.manifests.iter() {
            if forge_manifest_package_coordinate(manifest).is_some() {
                continue;
            }
            let package_name = match &manifest.name {
                ForgeFact::Recorded(name) => Some(name.as_str().to_owned()),
                ForgeFact::Unavailable(_) => None,
            };
            let repository_name = record.coordinate.repository().to_owned();
            let name = package_name
                .clone()
                .unwrap_or_else(|| repository_name.clone());
            let mut aliases = vec![repository_name.clone()];
            aliases.push(format!(
                "{}/{}",
                record.coordinate.owner(),
                record.coordinate.repository()
            ));
            if let Some(package_name) = &package_name {
                aliases.push(package_name.clone());
            }
            let mut keywords = vec![manifest.ecosystem.as_str().to_owned()];
            let license = match &record.metadata.license {
                ForgeFact::Recorded(value) => {
                    keywords.push(value.as_str().to_owned());
                    DiscoveryFacet::Known(value.as_str().to_owned())
                }
                ForgeFact::Unavailable(_) => DiscoveryFacet::Unknown,
            };
            if let ForgeFact::Recorded(topics) = &record.metadata.topics {
                keywords.extend(topics.iter().map(|topic| topic.as_str().to_owned()));
            }
            let description = match &record.metadata.description {
                ForgeFact::Recorded(value) => DiscoveryFacet::Known(value.as_str().to_owned()),
                ForgeFact::Unavailable(_) => DiscoveryFacet::Unknown,
            };
            let readme = match &record.metadata.readme {
                ForgeFact::Recorded(value) => DiscoveryFacet::Known(value.as_str().to_owned()),
                ForgeFact::Unavailable(_) => DiscoveryFacet::Unknown,
            };
            let mut generic_terms = vec![
                record.coordinate.canonical(),
                record.coordinate.repository_url().to_owned(),
                record.resolution.commit.as_hex(),
                hex(&record.archive),
                manifest.path.to_string(),
            ];
            if let Some(tree) = &record.resolution.tree {
                generic_terms.push(tree.as_hex());
            }
            if let Some(package_name) = package_name {
                generic_terms.push(package_name);
            }
            if let DependencyFacts::Known(rows) = &manifest.dependencies {
                for row in rows.iter() {
                    keywords.push(row.target.name.as_str().to_owned());
                    generic_terms.push(row.target.requirement.as_str().to_owned());
                    if let Some(resolved) = &row.target.resolved {
                        generic_terms.push(resolved.as_str().to_owned());
                    }
                }
            }
            documents.push(Self {
                key: ForgeSourcePinSearchKey {
                    source: record.coordinate.clone(),
                    ecosystem: manifest.ecosystem,
                    manifest_path: manifest.path.to_string(),
                    resolved_commit: record.resolution.commit.clone(),
                },
                name,
                metadata: DiscoveryMetadata {
                    aliases: DiscoveryFacet::Known(aliases),
                    description,
                    keywords: DiscoveryFacet::Known(keywords),
                    license,
                    ..DiscoveryMetadata::default()
                },
                readme,
                generic_terms,
            });
        }
        Ok(documents)
    }

    fn admit(&self) -> Result<(), String> {
        self.metadata
            .admit()
            .map_err(|error| format!("forge source-pin metadata violates bounds: {error:?}"))?;
        let key = &self.key;
        if !key.source.identity_is_valid()
            || key.manifest_path.is_empty()
            || key.manifest_path.len() > 4096
            || key.manifest_path.contains('\0')
            || self.name.len() > 4096
            || self.name.contains('\0')
            || self.generic_terms.len() > 512
            || self
                .generic_terms
                .iter()
                .any(|term| term.len() > 16 * 1024 || term.contains('\0'))
            || matches!(
                &self.readme,
                DiscoveryFacet::Known(value)
                    if value.len() > 256 * 1024 || value.contains('\0')
            )
        {
            return Err("forge source-pin search document exceeds bounds".to_owned());
        }
        Ok(())
    }
}

impl ForgeSearchDocument {
    /// Creates an indexed forge package claim from pinned source facts. This
    /// constructor supports corpora that have a checked archive URL/ref and
    /// package coordinate but no captured forge API response or archive body.
    /// Such a row remains source-only and every omitted facet stays unknown.
    pub(crate) fn from_pinned_facts(
        source_coordinate: ForgeCoordinate,
        coordinate: ProductPackageCoordinate,
        aliases: DiscoveryFacet<Vec<String>>,
        description: DiscoveryFacet<String>,
        keywords: DiscoveryFacet<Vec<String>>,
        license: DiscoveryFacet<String>,
        readme: DiscoveryFacet<String>,
        generic_terms: Vec<String>,
    ) -> Result<Self, String> {
        let ecosystem = coordinate
            .package_type()
            .registry()
            .unwrap_or(RegistryEcosystem::Cpp);
        let lineage = qualified_lineage(&coordinate)?;
        Ok(Self {
            source: ForgeSearchSourceIdentity {
                coordinate: source_coordinate,
                ecosystem,
            },
            coordinate,
            lineage,
            manifest_path: None,
            metadata: DiscoveryMetadata {
                aliases,
                description,
                keywords,
                license,
                ..DiscoveryMetadata::default()
            },
            readme,
            generic_terms,
        })
    }

    /// Projects only manifest-backed package/version identities from an
    /// admitted forge acquisition result. Manifests without a valid package
    /// coordinate are handled by `ForgeSourcePinSearchDocument` instead.
    pub(crate) fn from_acquisition(result: &ForgeAcquisitionResult) -> Result<Vec<Self>, String> {
        Self::from_search_record(&ForgeSearchRecord {
            coordinate: result.coordinate.clone(),
            resolution: result.resolution.clone(),
            archive: result.archive.to_bytes(),
            metadata: result.metadata.clone(),
            manifests: result.manifests.clone(),
        })
    }

    /// Projects the bounded metadata-only forge catalog snapshot into search
    /// documents without reopening its content-addressed archive.
    pub(crate) fn from_search_record(record: &ForgeSearchRecord) -> Result<Vec<Self>, String> {
        let mut documents = Vec::new();
        for manifest in record.manifests.iter() {
            if let Some(document) = forge_search_document(record, manifest)? {
                documents.push(document);
            }
        }
        Ok(documents)
    }

    fn admit(&self) -> Result<(), String> {
        self.metadata
            .admit()
            .map_err(|error| format!("forge search metadata violates bounds: {error:?}"))?;
        let readme_valid = !matches!(
            &self.readme,
            DiscoveryFacet::Known(value)
                if value.len() > 256 * 1024 || value.contains('\0')
        );
        let terms_valid = self.generic_terms.len() <= 512
            && self
                .generic_terms
                .iter()
                .all(|term| term.len() <= 16 * 1024 && !term.contains('\0'));
        let manifest_path_valid = self.manifest_path.as_ref().map_or(true, |path| {
            !path.is_empty() && path.len() <= 4096 && !path.contains('\0')
        });
        let expected_ecosystem = self
            .coordinate
            .package_type()
            .registry()
            .unwrap_or(RegistryEcosystem::Cpp);
        if !readme_valid
            || !terms_valid
            || !manifest_path_valid
            || !self.source.coordinate.identity_is_valid()
            || self.source.ecosystem != expected_ecosystem
            || qualified_lineage(&self.coordinate)? != self.lineage
        {
            return Err("forge search document exceeds bounds or has an invalid source".to_owned());
        }
        Ok(())
    }
}

/// A bounded page with one best-match evidence tier for each returned key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SearchPage<K> {
    pub(crate) hits: Vec<SearchHit<K>>,
    pub(crate) posting_candidates: usize,
    pub(crate) result_count: SearchResultCount,
    pub(crate) next_cursor: Option<SearchContinuation>,
}

/// Ranked key with the best tier it matched. This evidence can be compared
/// across catalog, local-declaration, and registry-discovery projections.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum SearchMatchEvidence {
    AllDocuments,
    ExactCoordinate,
    ExactName,
    ExactAlias,
    PrefixCoordinate,
    PrefixName,
    PrefixAlias,
    Substring,
    AliasTerms,
    KeywordTerms,
    AdvisoryTerms,
    DescriptionTerms,
    GenericTerms,
    FuzzyName,
    FuzzyAlias,
}

impl SearchMatchEvidence {
    pub(crate) fn rank(self) -> u8 {
        match self {
            Self::AllDocuments => u8::MAX,
            Self::ExactCoordinate => 0,
            Self::ExactName => 1,
            Self::ExactAlias => 2,
            Self::PrefixCoordinate => 3,
            Self::PrefixName => 4,
            Self::PrefixAlias => 5,
            Self::Substring => 6,
            Self::AliasTerms => 7,
            Self::KeywordTerms => 8,
            Self::AdvisoryTerms => 9,
            Self::DescriptionTerms => 10,
            Self::GenericTerms => 11,
            Self::FuzzyName => 12,
            Self::FuzzyAlias => 13,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SearchHit<K> {
    pub(crate) key: K,
    pub(crate) evidence: SearchMatchEvidence,
    pub(crate) standing: SearchStandingEvidence,
    /// Position immediately after this hit. A caller merging several search
    /// planes can persist the exact consumed position even after fetching
    /// ahead from one plane.
    pub(crate) continuation_after: SearchContinuation,
}

/// Last observed release standing, kept typed so historical yanks remain
/// visible without being mistaken for a current availability claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchStandingEvidence {
    Available,
    RecipeAvailable,
    Yanked,
    Withdrawn,
    Absent,
    Unknown,
}

impl Default for SearchStandingEvidence {
    fn default() -> Self {
        Self::Unknown
    }
}

/// Posting-based continuation for the generic projection. It resumes within
/// the last ranked tier and then advances through lower tiers without offsets.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SearchContinuation {
    pub(crate) next_tier: u8,
    #[serde(deserialize_with = "deserialize_cursor_sort_key")]
    pub(crate) after_sort_key: String,
    pub(crate) returned_before: usize,
}

impl SearchContinuation {
    pub(crate) fn admit(&self) -> Result<(), String> {
        let valid_tier =
            self.next_tier == u8::MAX || usize::from(self.next_tier) < SEARCH_TIERS.len();
        if !valid_tier
            || self.after_sort_key.is_empty()
            || self.after_sort_key.len() > MAX_CURSOR_SORT_KEY_BYTES
            || self.returned_before > MAX_CURSOR_OFFSET
        {
            return Err("search continuation exceeds its bounds".to_owned());
        }
        Ok(())
    }
}

fn deserialize_cursor_sort_key<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct CursorSortKeyVisitor;

    impl<'de> serde::de::Visitor<'de> for CursorSortKeyVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                formatter,
                "a search sort key no longer than {MAX_CURSOR_SORT_KEY_BYTES} bytes"
            )
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            if value.len() > MAX_CURSOR_SORT_KEY_BYTES {
                return Err(E::custom("search sort key exceeds its byte limit"));
            }
            Ok(value.to_owned())
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            if value.len() > MAX_CURSOR_SORT_KEY_BYTES {
                return Err(E::custom("search sort key exceeds its byte limit"));
            }
            Ok(value)
        }
    }

    deserializer.deserialize_string(CursorSortKeyVisitor)
}

/// Whether a result count is exact or only a guaranteed lower bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchResultCount {
    Exact(usize),
    AtLeast(usize),
    Unknown,
}

/// Opaque continuation state bound to one structural search revision and
/// normalized request. The UI may serialize it but must return it unchanged.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiscoverySearchCursor {
    pub(crate) snapshot_root: [u8; 32],
    #[serde(deserialize_with = "deserialize_cursor_query")]
    pub(crate) normalized_query: String,
    pub(crate) ecosystem: Option<RegistryEcosystem>,
    pub(crate) after: Option<DiscoverySearchPosition>,
}

impl DiscoverySearchCursor {
    pub(crate) fn admit(&self) -> Result<(), String> {
        if self.normalized_query.len() > MAX_CURSOR_QUERY_BYTES
            || normalize(&self.normalized_query) != self.normalized_query
        {
            return Err("search cursor exceeds its bounds".to_owned());
        }
        if let Some(after) = &self.after {
            if after.tier != u8::MAX && usize::from(after.tier) >= SEARCH_TIERS.len() {
                return Err("search cursor tier is outside the ranking schema".to_owned());
            }
            after.key.admit()?;
            if self
                .ecosystem
                .is_some_and(|ecosystem| ecosystem != after.key.ecosystem)
            {
                return Err("search cursor ecosystem does not match its lineage".to_owned());
            }
        }
        Ok(())
    }
}

fn deserialize_lineage<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct LineageVisitor;

    impl<'de> serde::de::Visitor<'de> for LineageVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                formatter,
                "a canonical lineage no longer than {MAX_CURSOR_SORT_KEY_BYTES} bytes"
            )
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            if value.len() > MAX_CURSOR_SORT_KEY_BYTES {
                return Err(E::custom("lineage exceeds its byte limit"));
            }
            Ok(value.to_owned())
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            if value.len() > MAX_CURSOR_SORT_KEY_BYTES {
                return Err(E::custom("lineage exceeds its byte limit"));
            }
            Ok(value)
        }
    }

    deserializer.deserialize_string(LineageVisitor)
}

fn deserialize_cursor_query<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct CursorQueryVisitor;

    impl<'de> serde::de::Visitor<'de> for CursorQueryVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                formatter,
                "a normalized search query no longer than {MAX_CURSOR_QUERY_BYTES} bytes"
            )
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            if value.len() > MAX_CURSOR_QUERY_BYTES {
                return Err(E::custom("search query exceeds its byte limit"));
            }
            Ok(value.to_owned())
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            if value.len() > MAX_CURSOR_QUERY_BYTES {
                return Err(E::custom("search query exceeds its byte limit"));
            }
            Ok(value)
        }
    }

    deserializer.deserialize_string(CursorQueryVisitor)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiscoverySearchPage<K> {
    pub(crate) hits: Vec<SearchHit<K>>,
    pub(crate) posting_candidates: usize,
    pub(crate) result_count: SearchResultCount,
    pub(crate) next_cursor: Option<SearchContinuation>,
}

pub(super) struct IndexedSearchPage<K> {
    pub(super) hits: Vec<SearchHit<K>>,
    pub(super) posting_candidates: usize,
    pub(super) result_count: SearchResultCount,
    pub(super) next_cursor: Option<SearchContinuation>,
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum SearchTier {
    ExactCoordinate = 0,
    ExactName = 1,
    ExactAlias = 2,
    PrefixCoordinate = 3,
    PrefixName = 4,
    PrefixAlias = 5,
    Substring = 6,
    AliasTerms = 7,
    KeywordTerms = 8,
    AdvisoryTerms = 9,
    DescriptionTerms = 10,
    GenericTerms = 11,
    FuzzyName = 12,
    FuzzyAlias = 13,
}

const SEARCH_TIERS: [SearchTier; 14] = [
    SearchTier::ExactCoordinate,
    SearchTier::ExactName,
    SearchTier::ExactAlias,
    SearchTier::PrefixCoordinate,
    SearchTier::PrefixName,
    SearchTier::PrefixAlias,
    SearchTier::Substring,
    SearchTier::AliasTerms,
    SearchTier::KeywordTerms,
    SearchTier::AdvisoryTerms,
    SearchTier::DescriptionTerms,
    SearchTier::GenericTerms,
    SearchTier::FuzzyName,
    SearchTier::FuzzyAlias,
];

impl SearchTier {
    const fn evidence(self) -> SearchMatchEvidence {
        match self {
            Self::ExactCoordinate => SearchMatchEvidence::ExactCoordinate,
            Self::ExactName => SearchMatchEvidence::ExactName,
            Self::ExactAlias => SearchMatchEvidence::ExactAlias,
            Self::PrefixCoordinate => SearchMatchEvidence::PrefixCoordinate,
            Self::PrefixName => SearchMatchEvidence::PrefixName,
            Self::PrefixAlias => SearchMatchEvidence::PrefixAlias,
            Self::Substring => SearchMatchEvidence::Substring,
            Self::AliasTerms => SearchMatchEvidence::AliasTerms,
            Self::KeywordTerms => SearchMatchEvidence::KeywordTerms,
            Self::AdvisoryTerms => SearchMatchEvidence::AdvisoryTerms,
            Self::DescriptionTerms => SearchMatchEvidence::DescriptionTerms,
            Self::GenericTerms => SearchMatchEvidence::GenericTerms,
            Self::FuzzyName => SearchMatchEvidence::FuzzyName,
            Self::FuzzyAlias => SearchMatchEvidence::FuzzyAlias,
        }
    }
}

/// Query text and an optional canonical ecosystem facet for registry discovery.
/// Facets constrain Tantivy postings before pagination, so a sparse ecosystem
/// does not lose candidates to a page filled by another registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DiscoverySearchRequest<'a> {
    pub(crate) text: &'a str,
    pub(crate) ecosystem: Option<RegistryEcosystem>,
}

/// One source-scoped package lineage with only the release coordinates that
/// matched this query. The caller hydrates each release's mutable facts from
/// the authoritative discovery journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiscoveryPackageSearchGroup {
    pub(crate) key: LineageKey,
    pub(crate) source: DiscoverySearchSource,
    pub(crate) ecosystem: RegistryEcosystem,
    pub(crate) lineage: String,
    pub(crate) evidence: SearchMatchEvidence,
    pub(crate) matched_releases: Vec<DiscoverySearchKey>,
    pub(crate) release_match_scope: ReleaseMatchScope,
    pub(crate) more_releases: bool,
    pub(crate) continuation_after: DiscoverySearchCursor,
}

/// Whether the bounded release facets matched individually or represent a
/// package-level match formed by combining source facts across releases.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReleaseMatchScope {
    ReleaseMatches,
    LineageMetadataOnly,
}

/// Bounded lineage search result. The cursor resumes after the last consumed
/// group rather than imposing a fixed release-candidate ceiling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GroupedSearchPage {
    pub(crate) groups: Vec<DiscoveryPackageSearchGroup>,
    pub(crate) posting_candidates: usize,
    pub(crate) index_documents_visited: usize,
    pub(crate) facet_releases_examined: usize,
    pub(crate) facet_term_keys_scanned: usize,
    pub(crate) result_count: SearchResultCount,
    pub(crate) next_cursor: Option<DiscoverySearchCursor>,
}

#[derive(Clone)]
struct LineageReleaseDocument {
    coordinate: String,
    version: String,
    order_sort_key: String,
    aliases: BTreeSet<String>,
    alias_tokens: BTreeSet<String>,
    keywords: BTreeSet<String>,
    descriptions: BTreeSet<String>,
    advisories: BTreeSet<String>,
    generic: BTreeSet<String>,
    fingerprint: [u8; 32],
}

fn release_search_evidence(
    key: &LineageKey,
    document: &LineageReleaseDocument,
    query: &str,
) -> Option<SearchMatchEvidence> {
    if query.is_empty() {
        return Some(SearchMatchEvidence::AllDocuments);
    }
    let coordinate = normalize(&document.coordinate);
    let name = normalize(lineage_search_name(key.ecosystem, &key.lineage));
    if coordinate == query {
        return Some(SearchMatchEvidence::ExactCoordinate);
    }
    if name == query {
        return Some(SearchMatchEvidence::ExactName);
    }
    if document.aliases.contains(query) {
        return Some(SearchMatchEvidence::ExactAlias);
    }
    if coordinate.starts_with(query) {
        return Some(SearchMatchEvidence::PrefixCoordinate);
    }
    if name.starts_with(query) {
        return Some(SearchMatchEvidence::PrefixName);
    }
    if document
        .aliases
        .iter()
        .any(|alias| alias.starts_with(query))
    {
        return Some(SearchMatchEvidence::PrefixAlias);
    }
    if coordinate.contains(query) || name.contains(query) {
        return Some(SearchMatchEvidence::Substring);
    }
    let tokens = query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect::<BTreeSet<_>>();
    if !tokens.is_empty() && tokens.iter().all(|token| token.chars().count() <= 40) {
        if tokens
            .iter()
            .all(|token| document.alias_tokens.contains(*token))
        {
            return Some(SearchMatchEvidence::AliasTerms);
        }
        if tokens
            .iter()
            .all(|token| document.keywords.contains(*token))
        {
            return Some(SearchMatchEvidence::KeywordTerms);
        }
        if tokens
            .iter()
            .all(|token| document.advisories.contains(*token))
        {
            return Some(SearchMatchEvidence::AdvisoryTerms);
        }
        if tokens
            .iter()
            .all(|token| document.descriptions.contains(*token))
        {
            return Some(SearchMatchEvidence::DescriptionTerms);
        }
        if tokens.iter().all(|token| document.generic.contains(*token)) {
            return Some(SearchMatchEvidence::GenericTerms);
        }
    }
    if fuzzy_term_matches(&name, query) {
        return Some(SearchMatchEvidence::FuzzyName);
    }
    if document
        .aliases
        .iter()
        .any(|alias| fuzzy_term_matches(alias, query))
    {
        return Some(SearchMatchEvidence::FuzzyAlias);
    }
    None
}

fn fuzzy_term_matches(candidate: &str, query: &str) -> bool {
    let candidate = candidate.chars().collect::<Vec<_>>();
    let query = query.chars().collect::<Vec<_>>();
    if !(3..=64).contains(&query.len()) || candidate.len().abs_diff(query.len()) > 1 {
        return false;
    }
    if candidate == query {
        return true;
    }
    if candidate.len() == query.len() {
        let differences = candidate
            .iter()
            .zip(&query)
            .enumerate()
            .filter_map(|(index, (left, right))| (left != right).then_some(index))
            .collect::<Vec<_>>();
        return differences.len() == 1
            || (differences.len() == 2
                && differences[1] == differences[0] + 1
                && candidate[differences[0]] == query[differences[1]]
                && candidate[differences[1]] == query[differences[0]]);
    }
    let (shorter, longer) = if candidate.len() < query.len() {
        (&candidate, &query)
    } else {
        (&query, &candidate)
    };
    let mut short_index = 0;
    let mut long_index = 0;
    let mut skipped = false;
    while short_index < shorter.len() && long_index < longer.len() {
        if shorter[short_index] == longer[long_index] {
            short_index += 1;
            long_index += 1;
        } else if skipped {
            return false;
        } else {
            skipped = true;
            long_index += 1;
        }
    }
    true
}

fn release_gram_field(width: usize) -> ReleasePostingField {
    match width {
        1 => ReleasePostingField::CoordinateGram1,
        2 => ReleasePostingField::CoordinateGram2,
        _ => ReleasePostingField::CoordinateGram3,
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum LineageFacetField {
    Coordinate,
    AliasValue,
    AliasToken,
    Keyword,
    Description,
    Advisory,
    Generic,
}

struct LineageFacetDelta {
    field: LineageFacetField,
    value: String,
    add: bool,
}

#[derive(Default)]
struct LineageAccumulator {
    coordinates: BTreeMap<String, usize>,
    aliases: BTreeMap<String, usize>,
    alias_tokens: BTreeMap<String, usize>,
    keywords: BTreeMap<String, usize>,
    descriptions: BTreeMap<String, usize>,
    advisories: BTreeMap<String, usize>,
    generic: BTreeMap<String, usize>,
}

impl LineageAccumulator {
    fn apply(&mut self, document: &LineageReleaseDocument, add: bool) -> Vec<LineageFacetDelta> {
        let mut deltas = Vec::new();
        let coordinate = normalize(&document.coordinate);
        adjust_ref_counts(
            &mut self.coordinates,
            std::iter::once(&coordinate),
            add,
            LineageFacetField::Coordinate,
            &mut deltas,
        );
        adjust_ref_counts(
            &mut self.aliases,
            document.aliases.iter(),
            add,
            LineageFacetField::AliasValue,
            &mut deltas,
        );
        adjust_ref_counts(
            &mut self.alias_tokens,
            document.alias_tokens.iter(),
            add,
            LineageFacetField::AliasToken,
            &mut deltas,
        );
        adjust_ref_counts(
            &mut self.keywords,
            document.keywords.iter(),
            add,
            LineageFacetField::Keyword,
            &mut deltas,
        );
        adjust_ref_counts(
            &mut self.descriptions,
            document.descriptions.iter(),
            add,
            LineageFacetField::Description,
            &mut deltas,
        );
        adjust_ref_counts(
            &mut self.advisories,
            document.advisories.iter(),
            add,
            LineageFacetField::Advisory,
            &mut deltas,
        );
        adjust_ref_counts(
            &mut self.generic,
            document.generic.iter(),
            add,
            LineageFacetField::Generic,
            &mut deltas,
        );
        deltas
    }
}

fn adjust_ref_counts<'a>(
    counts: &mut BTreeMap<String, usize>,
    values: impl IntoIterator<Item = &'a String>,
    add: bool,
    field: LineageFacetField,
    deltas: &mut Vec<LineageFacetDelta>,
) {
    for value in values {
        if add {
            let count = counts.entry(value.clone()).or_default();
            if *count == 0 {
                deltas.push(LineageFacetDelta {
                    field,
                    value: value.clone(),
                    add: true,
                });
            }
            *count = (*count).saturating_add(1);
        } else {
            let remove = counts.get_mut(value).is_some_and(|count| {
                if *count <= 1 {
                    *count = 0;
                    true
                } else {
                    *count -= 1;
                    false
                }
            });
            if remove {
                counts.remove(value);
                deltas.push(LineageFacetDelta {
                    field,
                    value: value.clone(),
                    add: false,
                });
            }
        }
    }
}

/// One Tantivy posting per source-specific lineage, backed by the authoritative
/// per-release index for bounded matching release facets.
pub(crate) struct LineageSearchIndex {
    inner: TextSearchIndex<LineageKey>,
    releases: BTreeMap<LineageKey, BTreeMap<String, LineageReleaseDocument>>,
    versioned_releases: BTreeMap<LineageKey, VersionedReleaseProjection>,
    aggregates: BTreeMap<LineageKey, LineageAccumulator>,
    release_to_lineage: BTreeMap<String, LineageKey>,
    coordinate_lineages: BTreeMap<String, BTreeMap<String, (LineageKey, usize)>>,
    facet_postings: BTreeMap<LineageFacetField, BTreeMap<String, BTreeSet<String>>>,
    fingerprint_xor: [u8; 32],
    document_count: usize,
}

impl LineageSearchIndex {
    fn new() -> Result<Self, String> {
        Ok(Self {
            inner: TextSearchIndex::new_with_memory(WRITER_MEMORY_BYTES)?,
            releases: BTreeMap::new(),
            versioned_releases: BTreeMap::new(),
            aggregates: BTreeMap::new(),
            release_to_lineage: BTreeMap::new(),
            coordinate_lineages: BTreeMap::new(),
            facet_postings: BTreeMap::new(),
            fingerprint_xor: [0; 32],
            document_count: 0,
        })
    }

    fn apply_batch(
        &mut self,
        changes: impl IntoIterator<Item = (String, LineageKey, LineageReleaseDocument)>,
    ) -> Result<(), String> {
        let mut affected = BTreeSet::new();
        for (identity, key, document) in changes {
            if let Some(previous_key) = self.release_to_lineage.remove(&identity) {
                let previous = self
                    .releases
                    .get_mut(&previous_key)
                    .and_then(|releases| releases.remove(&identity));
                if let Some(previous) = previous {
                    if let Some(projection) = self.versioned_releases.get_mut(&previous_key) {
                        projection.remove(&identity, &previous, previous_key.ecosystem);
                    }
                    let deltas = self
                        .aggregates
                        .get_mut(&previous_key)
                        .map(|aggregate| aggregate.apply(&previous, false))
                        .unwrap_or_default();
                    self.apply_facet_deltas(&previous_key, deltas);
                    self.adjust_coordinate_lineage(&previous_key, &previous.coordinate, false);
                    self.remove_fingerprint(previous.fingerprint);
                }
                if self
                    .releases
                    .get(&previous_key)
                    .is_some_and(BTreeMap::is_empty)
                {
                    self.releases.remove(&previous_key);
                    self.versioned_releases.remove(&previous_key);
                    self.aggregates.remove(&previous_key);
                }
                affected.insert(previous_key);
            }

            self.add_fingerprint(document.fingerprint);
            self.versioned_releases
                .entry(key.clone())
                .or_default()
                .insert(&identity, &document, key.ecosystem);
            self.adjust_coordinate_lineage(&key, &document.coordinate, true);
            let deltas = self
                .aggregates
                .entry(key.clone())
                .or_default()
                .apply(&document, true);
            self.apply_facet_deltas(&key, deltas);
            let releases = self.releases.entry(key.clone()).or_default();
            releases.insert(identity.clone(), document);
            self.release_to_lineage.insert(identity, key.clone());
            affected.insert(key);
        }

        for key in affected {
            self.refresh_lineage(&key)?;
        }
        Ok(())
    }

    fn remove_batch(&mut self, identities: impl IntoIterator<Item = String>) -> Result<(), String> {
        let mut affected = BTreeSet::new();
        for identity in identities {
            let Some(key) = self.release_to_lineage.remove(&identity) else {
                continue;
            };
            let removed = self
                .releases
                .get_mut(&key)
                .and_then(|releases| releases.remove(&identity));
            if let Some(removed) = removed {
                if let Some(projection) = self.versioned_releases.get_mut(&key) {
                    projection.remove(&identity, &removed, key.ecosystem);
                }
                let deltas = self
                    .aggregates
                    .get_mut(&key)
                    .map(|aggregate| aggregate.apply(&removed, false))
                    .unwrap_or_default();
                self.apply_facet_deltas(&key, deltas);
                self.adjust_coordinate_lineage(&key, &removed.coordinate, false);
                self.remove_fingerprint(removed.fingerprint);
            }
            if self.releases.get(&key).is_some_and(BTreeMap::is_empty) {
                self.releases.remove(&key);
                self.versioned_releases.remove(&key);
                self.aggregates.remove(&key);
            }
            affected.insert(key);
        }
        for key in affected {
            self.refresh_lineage(&key)?;
        }
        Ok(())
    }

    fn versioned_release_page(
        &self,
        key: &LineageKey,
        query: &str,
        limit: usize,
    ) -> Option<ReleasePostingPage> {
        let projection = self.versioned_releases.get(key)?;
        let releases = self.releases.get(key)?;
        Some(projection.top_matches(key, releases, query, limit))
    }

    fn versioned_release_projection_stats(&self) -> (usize, usize) {
        let release_count = self
            .versioned_releases
            .values()
            .map(VersionedReleaseProjection::release_count)
            .sum();
        let estimated_logical_payload_bytes = self
            .versioned_releases
            .values()
            .map(VersionedReleaseProjection::estimated_logical_payload_bytes)
            .sum();
        (release_count, estimated_logical_payload_bytes)
    }

    fn apply_facet_deltas(&mut self, key: &LineageKey, deltas: Vec<LineageFacetDelta>) {
        let sort_key = lineage_sort_key(key);
        for delta in deltas {
            if delta.add {
                self.facet_postings
                    .entry(delta.field)
                    .or_default()
                    .entry(delta.value)
                    .or_default()
                    .insert(sort_key.clone());
                continue;
            }
            let remove_field =
                if let Some(field_postings) = self.facet_postings.get_mut(&delta.field) {
                    let remove_term = if let Some(lineages) = field_postings.get_mut(&delta.value) {
                        lineages.remove(&sort_key);
                        lineages.is_empty()
                    } else {
                        false
                    };
                    if remove_term {
                        field_postings.remove(&delta.value);
                    }
                    field_postings.is_empty()
                } else {
                    false
                };
            if remove_field {
                self.facet_postings.remove(&delta.field);
            }
        }
    }

    fn adjust_coordinate_lineage(&mut self, key: &LineageKey, coordinate: &str, add: bool) {
        let coordinate = normalize(coordinate);
        let sort_key = lineage_sort_key(key);
        if add {
            let entries = self.coordinate_lineages.entry(coordinate).or_default();
            let entry = entries.entry(sort_key).or_insert_with(|| (key.clone(), 0));
            entry.1 = entry.1.saturating_add(1);
            return;
        }
        let mut remove_coordinate = false;
        if let Some(entries) = self.coordinate_lineages.get_mut(&coordinate) {
            let mut remove_lineage = false;
            if let Some((_, count)) = entries.get_mut(&sort_key) {
                *count = count.saturating_sub(1);
                remove_lineage = *count == 0;
            }
            if remove_lineage {
                entries.remove(&sort_key);
            }
            remove_coordinate = entries.is_empty();
        }
        if remove_coordinate {
            self.coordinate_lineages.remove(&coordinate);
        }
    }

    fn exact_coordinate_lineages<'a>(
        &'a self,
        coordinate: &str,
        ecosystem: Option<RegistryEcosystem>,
        after_sort_key: Option<&str>,
        limit: usize,
    ) -> Box<dyn Iterator<Item = (&'a str, &'a LineageKey)> + 'a> {
        let coordinate = normalize(coordinate);
        let Some(entries) = self.coordinate_lineages.get(&coordinate) else {
            return Box::new(std::iter::empty());
        };
        let lower = after_sort_key.map_or_else(
            || Bound::Included(String::new()),
            |after| Bound::Excluded(after.to_owned()),
        );
        Box::new(
            entries
                .range((lower, Bound::Unbounded))
                .filter(move |(_, (key, _))| {
                    ecosystem.is_none_or(|expected| key.ecosystem == expected)
                })
                .take(limit)
                .map(|(sort_key, (key, _))| (sort_key.as_str(), key)),
        )
    }

    fn exact_facet_lineages(
        &self,
        field: LineageFacetField,
        value: &str,
        query: &str,
        expected: SearchMatchEvidence,
        ecosystem: Option<RegistryEcosystem>,
        after_sort_key: Option<&str>,
        limit: usize,
    ) -> (usize, Vec<(String, LineageKey)>) {
        let Some(postings) = self
            .facet_postings
            .get(&field)
            .and_then(|values| values.get(value))
        else {
            return (0, Vec::new());
        };
        let lower = after_sort_key.map_or_else(
            || Bound::Included(String::new()),
            |after| Bound::Excluded(after.to_owned()),
        );
        let mut visited = 0_usize;
        let mut hits = Vec::with_capacity(limit);
        for sort_key in postings.range((lower, Bound::Unbounded)) {
            visited = visited.saturating_add(1);
            let Some(key) = self.inner.keys.get(sort_key) else {
                continue;
            };
            if ecosystem.is_some_and(|expected| key.ecosystem != expected) {
                continue;
            }
            if self.best_evidence_for_lineage(key, query) != Some(expected) {
                continue;
            }
            hits.push((sort_key.clone(), key.clone()));
            if hits.len() == limit {
                break;
            }
        }
        (visited, hits)
    }

    fn token_facet_lineages(
        &self,
        field: LineageFacetField,
        tokens: &[String],
        query: &str,
        expected: SearchMatchEvidence,
        ecosystem: Option<RegistryEcosystem>,
        after_sort_key: Option<&str>,
        limit: usize,
    ) -> (usize, Vec<(String, LineageKey)>) {
        if tokens.is_empty() || limit == 0 {
            return (0, Vec::new());
        }
        let Some(values) = self.facet_postings.get(&field) else {
            return (0, Vec::new());
        };
        let Some(rarest) = tokens
            .iter()
            .filter_map(|token| values.get(token))
            .min_by_key(|postings| postings.len())
        else {
            return (0, Vec::new());
        };
        if tokens.iter().any(|token| !values.contains_key(token)) {
            return (0, Vec::new());
        }
        let lower = after_sort_key.map_or_else(
            || Bound::Included(String::new()),
            |after| Bound::Excluded(after.to_owned()),
        );
        let mut visited = 0_usize;
        let mut hits = Vec::with_capacity(limit);
        for sort_key in rarest.range((lower, Bound::Unbounded)) {
            visited = visited.saturating_add(1);
            let Some(key) = self.inner.keys.get(sort_key) else {
                continue;
            };
            if ecosystem.is_some_and(|expected| key.ecosystem != expected) {
                continue;
            }
            let Some(aggregate) = self.aggregates.get(key) else {
                continue;
            };
            let counts = match field {
                LineageFacetField::AliasToken => &aggregate.alias_tokens,
                LineageFacetField::Keyword => &aggregate.keywords,
                LineageFacetField::Description => &aggregate.descriptions,
                LineageFacetField::Advisory => &aggregate.advisories,
                LineageFacetField::Generic => &aggregate.generic,
                LineageFacetField::Coordinate | LineageFacetField::AliasValue => continue,
            };
            if tokens.iter().all(|token| counts.contains_key(token))
                && self.best_evidence_for_lineage(key, query) == Some(expected)
            {
                hits.push((sort_key.clone(), key.clone()));
                if hits.len() == limit {
                    break;
                }
            }
        }
        (visited, hits)
    }

    fn prefix_facet_lineages(
        &self,
        field: LineageFacetField,
        prefix: &str,
        query: &str,
        expected: SearchMatchEvidence,
        ecosystem: Option<RegistryEcosystem>,
        after_sort_key: Option<&str>,
        limit: usize,
    ) -> (usize, Vec<(String, LineageKey)>) {
        if prefix.is_empty() || limit == 0 {
            return (0, Vec::new());
        }
        let Some(values) = self.facet_postings.get(&field) else {
            return (0, Vec::new());
        };
        let lower = after_sort_key.map_or_else(
            || Bound::Included(String::new()),
            |after| Bound::Excluded(after.to_owned()),
        );
        let mut posting_iters = Vec::new();
        for (value, postings) in values.range(prefix.to_owned()..) {
            if !value.starts_with(prefix) {
                break;
            }
            posting_iters.push(postings.range((lower.clone(), Bound::Unbounded)));
        }

        // Each matching facet term is already sorted by lineage key. Heapify
        // one head per term, then merge only enough posting entries to return
        // the requested distinct lineage keys. This avoids scanning every
        // posting for a common prefix before applying the page limit.
        let mut frontier = Vec::with_capacity(posting_iters.len());
        let mut visited = 0_usize;
        for (iterator_index, postings) in posting_iters.iter_mut().enumerate() {
            if let Some(sort_key) = postings.next() {
                visited = visited.saturating_add(1);
                frontier.push(Reverse((sort_key.clone(), iterator_index)));
            }
        }
        let mut frontier = BinaryHeap::from(frontier);
        let mut selected = Vec::with_capacity(limit);
        let mut last_seen: Option<String> = None;
        while let Some(Reverse((sort_key, iterator_index))) = frontier.pop() {
            if last_seen.as_deref() != Some(sort_key.as_str()) {
                last_seen = Some(sort_key.clone());
                if let Some(key) = self.inner.keys.get(&sort_key)
                    && ecosystem.is_none_or(|expected| key.ecosystem == expected)
                    && self.best_evidence_for_lineage(key, query) == Some(expected)
                {
                    selected.push((sort_key, key.clone()));
                    if selected.len() == limit {
                        break;
                    }
                }
            }
            if let Some(next_sort_key) = posting_iters[iterator_index].next() {
                visited = visited.saturating_add(1);
                frontier.push(Reverse((next_sort_key.clone(), iterator_index)));
            }
        }
        (visited, selected)
    }

    fn best_evidence_for_lineage(
        &self,
        key: &LineageKey,
        query: &str,
    ) -> Option<SearchMatchEvidence> {
        let query = normalize(query);
        if query.is_empty() {
            return Some(SearchMatchEvidence::AllDocuments);
        }
        let aggregate = self.aggregates.get(key)?;
        let name = normalize(lineage_search_name(key.ecosystem, &key.lineage));

        if aggregate.coordinates.contains_key(&query) {
            return Some(SearchMatchEvidence::ExactCoordinate);
        }
        if name == query {
            return Some(SearchMatchEvidence::ExactName);
        }
        if aggregate.aliases.contains_key(&query) {
            return Some(SearchMatchEvidence::ExactAlias);
        }
        if aggregate
            .coordinates
            .range(query.clone()..)
            .next()
            .is_some_and(|(coordinate, _)| coordinate.starts_with(&query))
        {
            return Some(SearchMatchEvidence::PrefixCoordinate);
        }
        if name.starts_with(&query) {
            return Some(SearchMatchEvidence::PrefixName);
        }
        if aggregate
            .aliases
            .range(query.clone()..)
            .next()
            .is_some_and(|(alias, _)| alias.starts_with(&query))
        {
            return Some(SearchMatchEvidence::PrefixAlias);
        }
        if name.contains(&query)
            || aggregate
                .coordinates
                .keys()
                .take(MAX_LINEAGE_FACET_VALUES)
                .any(|coordinate| coordinate.contains(&query))
        {
            return Some(SearchMatchEvidence::Substring);
        }

        let tokens = query
            .split(|character: char| !character.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        if tokens.is_empty() || tokens.iter().any(|token| token.chars().count() > 40) {
            return None;
        }
        if tokens
            .iter()
            .all(|token| aggregate.alias_tokens.contains_key(token))
        {
            return Some(SearchMatchEvidence::AliasTerms);
        }
        if tokens
            .iter()
            .all(|token| aggregate.keywords.contains_key(token))
        {
            return Some(SearchMatchEvidence::KeywordTerms);
        }
        if tokens
            .iter()
            .all(|token| aggregate.advisories.contains_key(token))
        {
            return Some(SearchMatchEvidence::AdvisoryTerms);
        }
        if tokens
            .iter()
            .all(|token| aggregate.descriptions.contains_key(token))
        {
            return Some(SearchMatchEvidence::DescriptionTerms);
        }
        if tokens
            .iter()
            .all(|token| aggregate.generic.contains_key(token))
        {
            return Some(SearchMatchEvidence::GenericTerms);
        }
        None
    }

    fn refresh_lineage(&mut self, key: &LineageKey) -> Result<(), String> {
        let group_sort_key = lineage_sort_key(key);
        if !self.releases.contains_key(key) {
            self.inner.remove_document(&group_sort_key);
            return Ok(());
        }
        let aggregate = self
            .aggregates
            .get(key)
            .ok_or_else(|| "lineage search aggregate is missing".to_owned())?;
        let coordinate_refs = aggregate
            .coordinates
            .keys()
            .take(MAX_LINEAGE_FACET_VALUES)
            .map(String::as_str)
            .collect();
        let aliases = facet_from_counts(&aggregate.aliases);
        let keywords = facet_from_counts(&aggregate.keywords);
        let descriptions = facet_from_counts(&aggregate.descriptions);
        let advisories = facet_from_counts(&aggregate.advisories);
        let generic = aggregate
            .generic
            .keys()
            .take(MAX_LINEAGE_FACET_VALUES)
            .map(String::as_str)
            .collect();
        let search_text = SearchDocumentText {
            coordinate_variants: coordinate_refs,
            aliases,
            keywords,
            descriptions,
            advisories,
            generic,
            substring_generic: false,
            lineage_group: None,
            standing: SearchStandingEvidence::Unknown,
        };
        let name = lineage_search_name(key.ecosystem, &key.lineage);
        if self.inner.identities.contains_key(&group_sort_key) {
            self.inner.replace_document_with_search_text(
                group_sort_key.clone(),
                key.clone(),
                "",
                name,
                search_text,
                key.ecosystem.as_str(),
                group_sort_key,
            )
        } else {
            self.inner.add_document_with_search_text(
                key.clone(),
                "",
                name,
                search_text,
                key.ecosystem.as_str(),
                group_sort_key,
            )
        }
    }

    fn commit(&mut self) -> Result<(), String> {
        self.inner.commit()
    }

    fn structural_fingerprint(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend-lineage-search-structural-root-v1\0");
        hasher.update(&(self.document_count as u64).to_le_bytes());
        hasher.update(&self.fingerprint_xor);
        *hasher.finalize().as_bytes()
    }

    fn add_fingerprint(&mut self, fingerprint: [u8; 32]) {
        for (target, byte) in self.fingerprint_xor.iter_mut().zip(fingerprint) {
            *target ^= byte;
        }
        self.document_count = self.document_count.saturating_add(1);
    }

    fn remove_fingerprint(&mut self, fingerprint: [u8; 32]) {
        for (target, byte) in self.fingerprint_xor.iter_mut().zip(fingerprint) {
            *target ^= byte;
        }
        self.document_count = self.document_count.saturating_sub(1);
    }
}

fn merge_lineage_candidate(
    ranked_hits: &mut BTreeMap<String, SearchHit<LineageKey>>,
    sort_key: String,
    key: LineageKey,
    evidence: SearchMatchEvidence,
) -> bool {
    let continuation_after = SearchContinuation {
        next_tier: evidence.rank(),
        after_sort_key: sort_key.clone(),
        returned_before: 0,
    };
    match ranked_hits.entry(sort_key) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(SearchHit {
                key,
                evidence,
                standing: SearchStandingEvidence::Unknown,
                continuation_after,
            });
            true
        }
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            let current = entry.get_mut();
            if evidence.rank() < current.evidence.rank() {
                current.evidence = evidence;
                current.continuation_after = continuation_after;
            }
            false
        }
    }
}

fn merge_lineage_candidates(
    ranked_hits: &mut BTreeMap<String, SearchHit<LineageKey>>,
    candidates: (usize, Vec<(String, LineageKey)>),
    evidence: SearchMatchEvidence,
    posting_candidates: &mut usize,
    index_documents_visited: &mut usize,
) {
    *posting_candidates = (*posting_candidates).saturating_add(candidates.0);
    for (sort_key, key) in candidates.1 {
        if merge_lineage_candidate(ranked_hits, sort_key, key, evidence) {
            *index_documents_visited = (*index_documents_visited).saturating_add(1);
        }
    }
}

fn versioned_release_facet(
    lineages: &LineageSearchIndex,
    releases: &TextSearchIndex<DiscoverySearchKey>,
    key: &LineageKey,
    query: &str,
    limit: usize,
) -> Result<Option<(Vec<DiscoverySearchKey>, usize, usize, bool)>, String> {
    let Some(page) = lineages.versioned_release_page(key, query, limit) else {
        return Ok(None);
    };
    let mut hits = Vec::with_capacity(page.identities.len());
    for identity in page.identities {
        let sort_key = releases.identities.get(&identity).ok_or_else(|| {
            "versioned release projection references a missing search document".to_owned()
        })?;
        let key = releases
            .keys
            .get(sort_key)
            .ok_or_else(|| "versioned release key is outside the search revision".to_owned())?;
        hits.push(key.clone());
    }
    Ok(Some((
        hits,
        page.posting_entries_examined,
        page.term_keys_scanned,
        page.more,
    )))
}

fn facet_from_counts<'a>(counts: &'a BTreeMap<String, usize>) -> SearchTextFacet<'a> {
    SearchTextFacet::Known(
        counts
            .keys()
            .take(MAX_LINEAGE_FACET_VALUES)
            .map(String::as_str)
            .collect(),
    )
}

/// Search view over discovery claims. Facts and source metadata are hydrated
/// by the caller after this index returns bounded source-coordinate keys.
pub(crate) struct DiscoverySearchIndex {
    inner: TextSearchIndex<DiscoverySearchKey>,
    lineages: LineageSearchIndex,
    source_pins: TextSearchIndex<ForgeSourcePinSearchKey>,
    store_revision: u64,
    revision: u64,
    snapshot_root: [u8; 32],
    forge_fingerprint: [u8; 32],
    forge_release_fingerprint: [u8; 32],
    source_pin_fingerprint: [u8; 32],
    forge_documents: BTreeMap<String, ForgeSearchDocument>,
    source_pin_documents: BTreeMap<String, ForgeSourcePinSearchDocument>,
}

impl DiscoverySearchIndex {
    /// Builds the cold projection from the authoritative journal owner.
    pub(crate) fn open(store: &DiscoveryStore) -> Result<Self, String> {
        Self::open_with_forge(store, &[])
    }

    /// Builds the registry projection and adds source-attributed documents
    /// acquired from code forges to the same Tantivy index.
    pub(crate) fn open_with_forge(
        store: &DiscoveryStore,
        forge_documents: &[ForgeSearchDocument],
    ) -> Result<Self, String> {
        Self::open_with_forge_and_source_pins(store, forge_documents, &[])
    }

    pub(crate) fn open_with_forge_and_source_pins(
        store: &DiscoveryStore,
        forge_documents: &[ForgeSearchDocument],
        source_pin_documents: &[ForgeSourcePinSearchDocument],
    ) -> Result<Self, String> {
        let mut inner = TextSearchIndex::new()?;
        let mut source_pins = TextSearchIndex::new_with_memory(SOURCE_PIN_WRITER_MEMORY_BYTES)?;
        let mut lineage_changes = Vec::new();
        for (source, fact) in store.facts() {
            let lineage = qualified_lineage(&fact.coordinate)?;
            add_discovery_document(
                &mut inner,
                *source,
                fact.coordinate.clone(),
                &lineage,
                &fact.metadata,
            )?;
            let discovery_source = DiscoverySearchSource::Registry(*source);
            let key = DiscoverySearchKey {
                source: discovery_source.clone(),
                coordinate: fact.coordinate.clone(),
                lineage: lineage.clone(),
                manifest_path: None,
            };
            let identity = discovery_sort_key(&discovery_source, fact.coordinate.as_str());
            let lineage_key =
                discovery_lineage_key(&discovery_source, source.ecosystem(), &lineage);
            lineage_changes.push((
                identity,
                lineage_key,
                lineage_release_document(key, &fact.metadata, None, &[]),
            ));
        }
        inner.commit()?;
        source_pins.commit()?;
        let mut lineages = LineageSearchIndex::new()?;
        lineages.apply_batch(lineage_changes)?;
        lineages.commit()?;
        let store_revision = store.search_revision();
        let registry_fingerprint = lineages.structural_fingerprint();
        let mut index = Self {
            inner,
            lineages,
            source_pins,
            store_revision,
            revision: combined_search_revision(registry_fingerprint, [0; 32]),
            snapshot_root: combined_search_snapshot_root(registry_fingerprint, [0; 32]),
            forge_fingerprint: [0; 32],
            forge_release_fingerprint: [0; 32],
            source_pin_fingerprint: [0; 32],
            forge_documents: BTreeMap::new(),
            source_pin_documents: BTreeMap::new(),
        };
        index.sync_forge_documents(forge_documents)?;
        index.sync_source_pin_documents(source_pin_documents)?;
        Ok(index)
    }

    /// Builds the forge-only production projection when registry discovery is
    /// not configured. It shares the same Tantivy document and ranking model.
    pub(crate) fn open_forge_only(forge_documents: &[ForgeSearchDocument]) -> Result<Self, String> {
        Self::open_forge_only_with_source_pins(forge_documents, &[])
    }

    pub(crate) fn open_forge_only_with_source_pins(
        forge_documents: &[ForgeSearchDocument],
        source_pin_documents: &[ForgeSourcePinSearchDocument],
    ) -> Result<Self, String> {
        let mut inner = TextSearchIndex::new()?;
        let mut source_pins = TextSearchIndex::new_with_memory(SOURCE_PIN_WRITER_MEMORY_BYTES)?;
        inner.commit()?;
        source_pins.commit()?;
        let lineages = LineageSearchIndex::new()?;
        let registry_fingerprint = lineages.structural_fingerprint();
        let mut index = Self {
            inner,
            lineages,
            source_pins,
            store_revision: 0,
            revision: combined_search_revision(registry_fingerprint, [0; 32]),
            snapshot_root: combined_search_snapshot_root(registry_fingerprint, [0; 32]),
            forge_fingerprint: [0; 32],
            forge_release_fingerprint: [0; 32],
            source_pin_fingerprint: [0; 32],
            forge_documents: BTreeMap::new(),
            source_pin_documents: BTreeMap::new(),
        };
        index.sync_forge_documents(forge_documents)?;
        index.sync_source_pin_documents(source_pin_documents)?;
        Ok(index)
    }

    /// Synchronizes only new coordinates or changed searchable metadata. A
    /// change to standing, proof, observed time, completeness, or source
    /// freshness stays a cheap reply overlay and leaves searchable postings
    /// untouched.
    pub(crate) fn sync(&mut self, store: &DiscoveryStore) -> Result<(), String> {
        self.sync_registry(store)
    }

    /// Synchronizes registry journal deltas and the supplied current forge
    /// catalog. Forge changes update only their source-scoped documents; a
    /// cold rebuild recovers both source kinds from their authoritative owners.
    pub(crate) fn sync_with_forge(
        &mut self,
        store: &DiscoveryStore,
        forge_documents: &[ForgeSearchDocument],
    ) -> Result<(), String> {
        self.sync_with_forge_and_source_pins(store, forge_documents, &[])
    }

    pub(crate) fn sync_with_forge_and_source_pins(
        &mut self,
        store: &DiscoveryStore,
        forge_documents: &[ForgeSearchDocument],
        source_pin_documents: &[ForgeSourcePinSearchDocument],
    ) -> Result<(), String> {
        self.sync_registry(store)?;
        self.sync_forge_documents(forge_documents)?;
        self.sync_source_pin_documents(source_pin_documents)
    }

    pub(crate) fn sync_forge_only(
        &mut self,
        forge_documents: &[ForgeSearchDocument],
    ) -> Result<(), String> {
        self.sync_forge_only_and_source_pins(forge_documents, &[])
    }

    pub(crate) fn sync_forge_only_and_source_pins(
        &mut self,
        forge_documents: &[ForgeSearchDocument],
        source_pin_documents: &[ForgeSourcePinSearchDocument],
    ) -> Result<(), String> {
        self.sync_forge_documents(forge_documents)?;
        self.sync_source_pin_documents(source_pin_documents)
    }

    fn sync_registry(&mut self, store: &DiscoveryStore) -> Result<(), String> {
        let current_revision = store.search_revision();
        if current_revision == self.store_revision
            && !self.inner.is_dirty()
            && !self.lineages.inner.is_dirty()
        {
            return Ok(());
        }

        if !self.inner.is_dirty()
            && !self.lineages.inner.is_dirty()
            && let Some((revision, changes)) = store.search_changes_since(self.store_revision)
        {
            let mut lineage_changes = Vec::with_capacity(changes.len());
            for change in changes {
                let source = DiscoverySearchSource::Registry(change.source);
                let coordinate_text = change.coordinate.as_str().to_owned();
                let key = DiscoverySearchKey {
                    source: source.clone(),
                    coordinate: change.coordinate.clone(),
                    lineage: change.lineage.clone(),
                    manifest_path: None,
                };
                let identity = discovery_sort_key(&source, &coordinate_text);
                let lineage_key =
                    discovery_lineage_key(&source, source.ecosystem(), &change.lineage);
                lineage_changes.push((
                    identity,
                    lineage_key,
                    lineage_release_document(key, &change.metadata, None, &[]),
                ));
                replace_discovery_document(&mut self.inner, change)?;
            }
            self.inner.commit()?;
            self.lineages.apply_batch(lineage_changes)?;
            self.lineages.commit()?;
            self.store_revision = revision;
            self.refresh_revision();
            return Ok(());
        }

        // The retained delta window was exceeded, or an earlier Tantivy
        // mutation failed. Rebuild from current authoritative journal facts.
        self.inner = rebuild_discovery_search_index(store, &self.forge_documents)?;
        self.lineages = rebuild_lineage_search_index(store, &self.forge_documents)?;
        self.store_revision = current_revision;
        self.refresh_revision();
        Ok(())
    }

    fn sync_forge_documents(&mut self, documents: &[ForgeSearchDocument]) -> Result<(), String> {
        let mut next = BTreeMap::new();
        for document in documents {
            document.admit()?;
            let identity = forge_search_identity(document);
            if next.insert(identity, document.clone()).is_some() {
                return Err(
                    "forge search catalog contains a duplicate package/version identity".to_owned(),
                );
            }
        }

        let removed = self
            .forge_documents
            .keys()
            .filter(|identity| !next.contains_key(*identity))
            .cloned()
            .collect::<Vec<_>>();
        let changed = next
            .iter()
            .filter(|(identity, document)| self.forge_documents.get(*identity) != Some(*document))
            .map(|(identity, document)| (identity.clone(), document.clone()))
            .collect::<Vec<_>>();
        if removed.is_empty() && changed.is_empty() {
            return Ok(());
        }
        let forge_release_fingerprint = forge_documents_fingerprint(&next)?;
        for identity in &removed {
            self.inner.remove_document(identity);
        }
        self.lineages.remove_batch(removed)?;
        let mut lineage_changes = Vec::with_capacity(changed.len());
        for (identity, document) in changed {
            lineage_changes.push(forge_lineage_release(&identity, &document)?);
            add_or_replace_forge_document(&mut self.inner, identity, document)?;
        }
        self.inner.commit()?;
        self.lineages.apply_batch(lineage_changes)?;
        self.lineages.commit()?;
        self.forge_documents = next;
        self.forge_release_fingerprint = forge_release_fingerprint;
        self.refresh_forge_fingerprint();
        self.refresh_revision();
        Ok(())
    }

    fn sync_source_pin_documents(
        &mut self,
        documents: &[ForgeSourcePinSearchDocument],
    ) -> Result<(), String> {
        let mut next = BTreeMap::new();
        for document in documents {
            document.admit()?;
            let identity = forge_source_pin_identity(&document.key);
            if next.insert(identity, document.clone()).is_some() {
                return Err("forge source-pin catalog contains a duplicate manifest pin".to_owned());
            }
        }
        let removed = self
            .source_pin_documents
            .keys()
            .filter(|identity| !next.contains_key(*identity))
            .cloned()
            .collect::<Vec<_>>();
        let changed = next
            .iter()
            .filter(|(identity, document)| {
                self.source_pin_documents.get(*identity) != Some(*document)
            })
            .map(|(identity, document)| (identity.clone(), document.clone()))
            .collect::<Vec<_>>();
        let rebuild = self.source_pins.is_dirty();
        if removed.is_empty() && changed.is_empty() && !rebuild {
            return Ok(());
        }
        let source_pin_fingerprint = forge_source_pin_documents_fingerprint(&next)?;
        if rebuild {
            self.source_pins = rebuild_forge_source_pin_index(&next)?;
        } else {
            for identity in &removed {
                self.source_pins.remove_document(identity);
            }
            for (identity, document) in changed {
                add_or_replace_forge_source_pin(&mut self.source_pins, identity, document)?;
            }
            self.source_pins.commit()?;
        }
        self.source_pin_documents = next;
        self.source_pin_fingerprint = source_pin_fingerprint;
        self.refresh_forge_fingerprint();
        self.refresh_revision();
        Ok(())
    }

    fn refresh_forge_fingerprint(&mut self) {
        self.forge_fingerprint = combined_forge_fingerprints(
            self.forge_release_fingerprint,
            self.source_pin_fingerprint,
        );
    }

    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) const fn snapshot_root(&self) -> [u8; 32] {
        self.snapshot_root
    }

    fn refresh_revision(&mut self) {
        self.snapshot_root = combined_search_snapshot_root(
            self.lineages.structural_fingerprint(),
            self.forge_fingerprint,
        );
        self.revision = u64::from_le_bytes(
            self.snapshot_root[..8]
                .try_into()
                .expect("snapshot root has at least eight bytes"),
        );
    }

    /// Returns exact, prefix, and substring matches in deterministic bounded
    /// pages. The returned keys are hydrated from `DiscoveryStore` by callers.
    pub(crate) fn page(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<DiscoverySearchPage<DiscoverySearchKey>, String> {
        self.search(
            DiscoverySearchRequest {
                text: query,
                ecosystem: None,
            },
            limit,
        )
    }

    pub(crate) fn search(
        &self,
        request: DiscoverySearchRequest<'_>,
        limit: usize,
    ) -> Result<DiscoverySearchPage<DiscoverySearchKey>, String> {
        self.search_after(request, limit, None)
    }

    pub(crate) fn search_with_store(
        &self,
        store: &DiscoveryStore,
        request: DiscoverySearchRequest<'_>,
        limit: usize,
    ) -> Result<DiscoverySearchPage<DiscoverySearchKey>, String> {
        self.search_after_with_store(store, request, limit, None)
    }

    pub(crate) fn search_after(
        &self,
        request: DiscoverySearchRequest<'_>,
        limit: usize,
        cursor: Option<&SearchContinuation>,
    ) -> Result<DiscoverySearchPage<DiscoverySearchKey>, String> {
        if request.text.len() > MAX_QUERY_BYTES {
            return Err("catalog search query exceeds its byte limit".to_owned());
        }
        if cursor.is_some_and(|cursor| cursor.admit().is_err()) {
            return Err("release search continuation exceeds its bounds".to_owned());
        }
        let indexed = self.inner.page_filtered(
            request.text,
            limit,
            request.ecosystem.map(RegistryEcosystem::as_str),
            cursor,
        )?;
        let hits = indexed.hits;
        let next_cursor = indexed.next_cursor;
        Ok(DiscoverySearchPage {
            hits,
            posting_candidates: indexed.posting_candidates,
            result_count: indexed.result_count,
            next_cursor,
        })
    }

    /// Searches manifest-backed source pins whose manifests do not establish
    /// a package release coordinate. The continuation is tied to the public
    /// index snapshot by the caller's enclosing cursor.
    pub(crate) fn source_pin_page_after(
        &self,
        query: &str,
        limit: usize,
        cursor: Option<&SearchContinuation>,
    ) -> Result<SearchPage<ForgeSourcePinSearchKey>, String> {
        if query.len() > MAX_QUERY_BYTES {
            return Err("catalog search query exceeds its byte limit".to_owned());
        }
        if cursor.is_some_and(|cursor| cursor.admit().is_err()) {
            return Err("source-pin search continuation exceeds its bounds".to_owned());
        }
        self.source_pins
            .page_filtered(query, limit, None, cursor)
            .map(|page| SearchPage {
                hits: page.hits,
                posting_candidates: page.posting_candidates,
                result_count: page.result_count,
                next_cursor: page.next_cursor,
            })
    }

    /// Adds standing from the authoritative journal after the posting page is
    /// selected. Standing updates therefore stay out of the structural index
    /// revision while callers still receive yanked/withdrawn state explicitly.
    pub(crate) fn search_after_with_store(
        &self,
        store: &DiscoveryStore,
        request: DiscoverySearchRequest<'_>,
        limit: usize,
        cursor: Option<&SearchContinuation>,
    ) -> Result<DiscoverySearchPage<DiscoverySearchKey>, String> {
        let mut page = self.search_after(request, limit, cursor)?;
        for hit in &mut page.hits {
            let Some(source) = hit.key.source.registry() else {
                hit.standing = SearchStandingEvidence::Unknown;
                continue;
            };
            let Some(fact) = store.fact(source, hit.key.coordinate.as_str()) else {
                hit.standing = SearchStandingEvidence::Unknown;
                continue;
            };
            hit.standing = match fact.standing {
                backend_engine::registry::DiscoveryStanding::Published => {
                    SearchStandingEvidence::Available
                }
                backend_engine::registry::DiscoveryStanding::RecipeAvailable => {
                    SearchStandingEvidence::RecipeAvailable
                }
                backend_engine::registry::DiscoveryStanding::Yanked => {
                    SearchStandingEvidence::Yanked
                }
                backend_engine::registry::DiscoveryStanding::Withdrawn => {
                    SearchStandingEvidence::Withdrawn
                }
            };
        }
        Ok(page)
    }

    /// Collapses release hits into source-specific package lineages. The
    /// caller can continue from the returned cursor, so one lineage with a
    /// long history cannot silently hide later package lineages.
    pub(crate) fn search_groups(
        &self,
        request: DiscoverySearchRequest<'_>,
        limit: usize,
    ) -> Result<GroupedSearchPage, String> {
        self.search_groups_after(request, limit, None)
    }

    pub(crate) fn search_groups_after(
        &self,
        request: DiscoverySearchRequest<'_>,
        limit: usize,
        cursor: Option<&DiscoverySearchCursor>,
    ) -> Result<GroupedSearchPage, String> {
        if request.text.len() > MAX_QUERY_BYTES {
            return Err("catalog search query exceeds its byte limit".to_owned());
        }
        let normalized_query = normalize(request.text);
        if cursor.is_some_and(|cursor| {
            cursor.admit().is_err()
                || cursor.snapshot_root != self.snapshot_root
                || cursor.normalized_query != normalized_query
                || cursor.ecosystem != request.ecosystem
        }) {
            return Err(
                "discovery search cursor does not match this catalog revision or request"
                    .to_owned(),
            );
        }
        let limit = limit.min(MAX_SEARCH_PAGE_SIZE);
        if limit == 0 {
            return Ok(GroupedSearchPage {
                groups: Vec::new(),
                posting_candidates: 0,
                index_documents_visited: 0,
                facet_releases_examined: 0,
                facet_term_keys_scanned: 0,
                result_count: SearchResultCount::Unknown,
                next_cursor: None,
            });
        }
        let continuation = cursor
            .and_then(|cursor| cursor.after.as_ref())
            .map(|after| SearchContinuation {
                next_tier: after.tier,
                after_sort_key: lineage_sort_key(&after.key),
                returned_before: 0,
            });
        let prepared_lineage_query = self
            .lineages
            .inner
            .prepare_search_query(normalized_query.clone());
        let prepared_release_query = self.inner.prepare_search_query(normalized_query.clone());
        let empty_release_query = self.inner.prepare_search_query(String::new());
        let page = prepared_lineage_query.page_filtered(
            limit.saturating_add(1),
            request.ecosystem.map(RegistryEcosystem::as_str),
            continuation.as_ref(),
        )?;
        let mut posting_candidates = page.posting_candidates;
        let mut index_documents_visited = page.posting_candidates;
        let mut ranked_hits = BTreeMap::new();
        for hit in page.hits {
            let sort_key = lineage_sort_key(&hit.key);
            ranked_hits.insert(sort_key, hit);
        }
        let cursor_position = cursor.and_then(|cursor| cursor.after.as_ref());
        let tier_is_live = |tier: u8| cursor_position.is_none_or(|position| position.tier <= tier);
        let after_for_tier = |tier: u8| {
            cursor_position
                .filter(|position| position.tier == tier)
                .map(|position| lineage_sort_key(&position.key))
        };

        // The group document only carries the bounded coordinate variant set
        // as Tantivy terms. A direct lookup keeps exact PURLs searchable for
        // every release without growing a lineage document with its history.
        let exact_coordinate_tier = SearchTier::ExactCoordinate as u8;
        if tier_is_live(exact_coordinate_tier) {
            let exact_after = after_for_tier(exact_coordinate_tier);
            let direct = self
                .lineages
                .exact_coordinate_lineages(
                    request.text,
                    request.ecosystem,
                    exact_after.as_deref(),
                    limit.saturating_add(1),
                )
                .map(|(sort_key, key)| (sort_key.to_owned(), key.clone()))
                .collect::<Vec<_>>();
            let visited = direct.len();
            merge_lineage_candidates(
                &mut ranked_hits,
                (visited, direct),
                SearchMatchEvidence::ExactCoordinate,
                &mut posting_candidates,
                &mut index_documents_visited,
            );
        }

        let query_tokens = normalized_query
            .split(|character: char| !character.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let query_tokens_searchable = !query_tokens.is_empty()
            && query_tokens.iter().all(|token| token.chars().count() <= 40);

        for (tier, evidence, field) in [
            (
                SearchTier::ExactAlias as u8,
                SearchMatchEvidence::ExactAlias,
                LineageFacetField::AliasValue,
            ),
            (
                SearchTier::PrefixCoordinate as u8,
                SearchMatchEvidence::PrefixCoordinate,
                LineageFacetField::Coordinate,
            ),
            (
                SearchTier::PrefixAlias as u8,
                SearchMatchEvidence::PrefixAlias,
                LineageFacetField::AliasValue,
            ),
        ] {
            if normalized_query.is_empty() || !tier_is_live(tier) {
                continue;
            }
            let after = after_for_tier(tier);
            let candidates = if evidence == SearchMatchEvidence::ExactAlias {
                self.lineages.exact_facet_lineages(
                    field,
                    &normalized_query,
                    &normalized_query,
                    evidence,
                    request.ecosystem,
                    after.as_deref(),
                    limit.saturating_add(1),
                )
            } else {
                self.lineages.prefix_facet_lineages(
                    field,
                    &normalized_query,
                    &normalized_query,
                    evidence,
                    request.ecosystem,
                    after.as_deref(),
                    limit.saturating_add(1),
                )
            };
            merge_lineage_candidates(
                &mut ranked_hits,
                candidates,
                evidence,
                &mut posting_candidates,
                &mut index_documents_visited,
            );
        }

        if query_tokens_searchable {
            for (tier, evidence, field) in [
                (
                    SearchTier::AliasTerms as u8,
                    SearchMatchEvidence::AliasTerms,
                    LineageFacetField::AliasToken,
                ),
                (
                    SearchTier::KeywordTerms as u8,
                    SearchMatchEvidence::KeywordTerms,
                    LineageFacetField::Keyword,
                ),
                (
                    SearchTier::AdvisoryTerms as u8,
                    SearchMatchEvidence::AdvisoryTerms,
                    LineageFacetField::Advisory,
                ),
                (
                    SearchTier::DescriptionTerms as u8,
                    SearchMatchEvidence::DescriptionTerms,
                    LineageFacetField::Description,
                ),
                (
                    SearchTier::GenericTerms as u8,
                    SearchMatchEvidence::GenericTerms,
                    LineageFacetField::Generic,
                ),
            ] {
                if !tier_is_live(tier) {
                    continue;
                }
                let after = after_for_tier(tier);
                let candidates = self.lineages.token_facet_lineages(
                    field,
                    &query_tokens,
                    &normalized_query,
                    evidence,
                    request.ecosystem,
                    after.as_deref(),
                    limit.saturating_add(1),
                );
                merge_lineage_candidates(
                    &mut ranked_hits,
                    candidates,
                    evidence,
                    &mut posting_candidates,
                    &mut index_documents_visited,
                );
            }
        }

        let mut ranked_hits = ranked_hits.into_iter().collect::<Vec<_>>();
        ranked_hits.sort_by(|(left_key, left), (right_key, right)| {
            left.evidence
                .rank()
                .cmp(&right.evidence.rank())
                .then_with(|| left_key.cmp(right_key))
        });
        let has_more = ranked_hits.len() > limit;
        let selected = ranked_hits
            .into_iter()
            .take(limit)
            .map(|(sort_key, hit)| (hit, sort_key))
            .collect::<Vec<_>>();
        let mut groups = Vec::with_capacity(selected.len());
        let mut facet_releases_examined = 0_usize;
        let mut facet_term_keys_scanned = 0_usize;

        for (hit, _) in selected {
            let LineageSearchSource::Discovery(source) = &hit.key.source else {
                return Err("acquired lineage leaked into the discovery search plane".to_owned());
            };
            let continuation_after = DiscoverySearchCursor {
                snapshot_root: self.snapshot_root,
                normalized_query: normalized_query.clone(),
                ecosystem: request.ecosystem,
                after: Some(DiscoverySearchPosition {
                    tier: hit.evidence.rank(),
                    key: hit.key.clone(),
                }),
            };
            let sort_key = lineage_sort_key(&hit.key);
            let (mut matched_releases, facet_candidates, facet_keys_scanned, mut more_releases) =
                if let Some((hits, examined, keys_scanned, more)) = versioned_release_facet(
                    &self.lineages,
                    &self.inner,
                    &hit.key,
                    &normalized_query,
                    MAX_RELEASES_PER_GROUP,
                )? {
                    (hits, examined, keys_scanned, more)
                } else {
                    let facet_page = prepared_release_query.page_in_group(
                        MAX_RELEASES_PER_GROUP,
                        request.ecosystem.map(RegistryEcosystem::as_str),
                        &sort_key,
                        None,
                    )?;
                    (
                        order_grouped_release_hits(facet_page.hits),
                        facet_page.posting_candidates,
                        0,
                        facet_page.next_cursor.is_some(),
                    )
                };
            posting_candidates = posting_candidates.saturating_add(facet_candidates);
            facet_releases_examined = facet_releases_examined.saturating_add(facet_candidates);
            facet_term_keys_scanned = facet_term_keys_scanned.saturating_add(facet_keys_scanned);
            let release_match_scope = if matched_releases.is_empty() {
                // A lineage can match through the package-level union of facts
                // contributed by different releases. Keep a bounded source
                // facet page visible, with an explicit scope marker.
                if let Some((fallback, examined, keys_scanned, more)) = versioned_release_facet(
                    &self.lineages,
                    &self.inner,
                    &hit.key,
                    "",
                    MAX_RELEASES_PER_GROUP,
                )? {
                    posting_candidates = posting_candidates.saturating_add(examined);
                    facet_releases_examined = facet_releases_examined.saturating_add(examined);
                    facet_term_keys_scanned = facet_term_keys_scanned.saturating_add(keys_scanned);
                    matched_releases = fallback;
                    more_releases = more;
                } else {
                    let fallback = empty_release_query.page_in_group(
                        MAX_RELEASES_PER_GROUP,
                        request.ecosystem.map(RegistryEcosystem::as_str),
                        &sort_key,
                        None,
                    )?;
                    posting_candidates =
                        posting_candidates.saturating_add(fallback.posting_candidates);
                    facet_releases_examined =
                        facet_releases_examined.saturating_add(fallback.posting_candidates);
                    matched_releases = order_grouped_release_hits(fallback.hits);
                    more_releases = fallback.next_cursor.is_some();
                }
                ReleaseMatchScope::LineageMetadataOnly
            } else {
                ReleaseMatchScope::ReleaseMatches
            };
            groups.push(DiscoveryPackageSearchGroup {
                key: hit.key.clone(),
                source: source.clone(),
                ecosystem: hit.key.ecosystem,
                lineage: hit.key.lineage.clone(),
                evidence: hit.evidence,
                matched_releases,
                release_match_scope,
                more_releases,
                continuation_after,
            });
        }

        let next_cursor = if has_more {
            groups.last().map(|group| group.continuation_after.clone())
        } else {
            None
        };
        let result_count = if has_more {
            SearchResultCount::AtLeast(limit.saturating_add(1))
        } else if groups.is_empty() {
            SearchResultCount::Exact(0)
        } else {
            SearchResultCount::Unknown
        };
        Ok(GroupedSearchPage {
            groups,
            posting_candidates,
            index_documents_visited,
            facet_releases_examined,
            facet_term_keys_scanned,
            result_count,
            next_cursor,
        })
    }

    #[cfg(test)]
    pub(crate) fn index_identity(&self) -> usize {
        self.inner.identity()
    }
}

/// Search projection over local declaration rows. It advances by checked row
/// deltas, rebuilds on cold start or a missed base, and maps hits back through
/// `RowId` so queries never traverse every retained row.
#[derive(Default)]
pub(crate) struct LocalDeclarationSearchIndex {
    root: Option<ViewStateRoot>,
    inner: Option<TextSearchIndex<RowId>>,
}

impl LocalDeclarationSearchIndex {
    pub(crate) fn sync(&mut self, view: &ViewRoot) -> Result<(), String> {
        let root = view.root();
        if self.root == Some(root) && self.inner.is_some() {
            return Ok(());
        }
        self.rebuild(view)
    }

    /// Applies a checked row-delta chain to the retained writer. A missing
    /// index stays lazy; a base mismatch invalidates it so the next query
    /// performs one authoritative cold rebuild. Reset deltas rebuild directly
    /// from their checked target root.
    pub(crate) fn apply_committed_deltas(
        &mut self,
        deltas: &[CommittedViewDelta],
    ) -> Result<(), String> {
        if self.inner.is_none() || deltas.is_empty() {
            return Ok(());
        }
        for delta in deltas {
            if self.root != Some(delta.base_root()) {
                self.invalidate();
                return Ok(());
            }
            let result = match delta.delta() {
                ViewDelta::Upsert { row } => self.upsert_row(row),
                ViewDelta::Remove { id } => {
                    self.remove_row(*id);
                    Ok(())
                }
                ViewDelta::Patch { changes } => {
                    let mut result = Ok(());
                    for change in changes.iter() {
                        result = match change {
                            RowChange::Upsert(row) => self.upsert_row(row),
                            RowChange::Remove(id) => {
                                self.remove_row(*id);
                                Ok(())
                            }
                        };
                        if result.is_err() {
                            break;
                        }
                    }
                    result
                }
                ViewDelta::Coverage { .. } => Ok(()),
                ViewDelta::Reset { .. } => self.rebuild(delta.target_view()),
            };
            if let Err(error) = result {
                self.invalidate();
                return Err(error);
            }
            self.root = Some(delta.target_root());
        }
        let commit_result = match self.inner.as_mut() {
            Some(inner) => inner.commit(),
            None => Ok(()),
        };
        if let Err(error) = commit_result {
            self.invalidate();
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn page(&self, query: &str, limit: usize) -> Result<SearchPage<RowId>, String> {
        self.inner
            .as_ref()
            .ok_or_else(|| "local declaration search index was not synchronized".to_owned())?
            .page(query, limit)
    }

    pub(crate) fn page_after(
        &self,
        query: &str,
        limit: usize,
        continuation: Option<&SearchContinuation>,
    ) -> Result<SearchPage<RowId>, String> {
        if let Some(continuation) = continuation {
            continuation.admit()?;
        }
        self.inner
            .as_ref()
            .ok_or_else(|| "local declaration search index was not synchronized".to_owned())?
            .page_with_ecosystem(query, limit, None, continuation)
    }

    pub(crate) const fn revision(&self) -> Option<ViewStateRoot> {
        self.root
    }

    #[cfg(test)]
    pub(crate) fn index_identity(&self) -> Option<usize> {
        self.inner.as_ref().map(TextSearchIndex::identity)
    }

    fn upsert_row(&mut self, row: &Row) -> Result<(), String> {
        let inner = self
            .inner
            .as_mut()
            .ok_or_else(|| "local declaration writer disappeared".to_owned())?;
        let identity = row.id.stable_key();
        if !matches!(row.id, RowId::Symbol(_)) {
            inner.remove_document(&identity);
            return Ok(());
        }
        let name = row.label.rsplit("::").next().unwrap_or(&row.label);
        let content = local_search_content(row);
        inner.replace_document(
            identity,
            row.id,
            row.label.as_str(),
            name,
            content.iter().copied(),
            local_sort_key(row.id, row.label.as_str()),
        )
    }

    fn remove_row(&mut self, id: RowId) {
        if let Some(inner) = self.inner.as_mut() {
            inner.remove_document(&id.stable_key());
        }
    }

    fn rebuild(&mut self, view: &ViewRoot) -> Result<(), String> {
        let mut inner = TextSearchIndex::new()?;
        for row in view
            .row_refs()
            .filter(|row| matches!(row.id, RowId::Symbol(_)))
        {
            let name = row.label.rsplit("::").next().unwrap_or(&row.label);
            let content = local_search_content(row);
            inner.add_document(
                row.id,
                row.label.as_str(),
                name,
                content.iter().copied(),
                local_sort_key(row.id, row.label.as_str()),
            )?;
        }
        inner.commit()?;
        self.root = Some(view.root());
        self.inner = Some(inner);
        Ok(())
    }

    fn invalidate(&mut self) {
        self.root = None;
        self.inner = None;
    }
}

fn local_search_content(row: &Row) -> Vec<&str> {
    let mut content = Vec::new();
    if let Some(signature) = row.signature.as_deref() {
        content.push(signature);
    }
    if let Some(excerpt) = row.excerpt.text() {
        content.push(excerpt);
    }
    for fragment in &row.document {
        match fragment {
            Fragment::Text(value) | Fragment::Code(value) => content.push(value.as_str()),
            Fragment::Link { label, .. } => content.push(label.as_str()),
            Fragment::Break => {}
        }
    }
    content
}

pub(super) struct TextSearchIndex<K> {
    _index: Index,
    writer: IndexWriter,
    reader: IndexReader,
    exact_coordinate: Field,
    exact_name: Field,
    exact_alias: Field,
    ecosystem: Field,
    searchable_content: Field,
    searchable_alias: Field,
    searchable_keyword: Field,
    searchable_description: Field,
    searchable_advisory: Field,
    substring_unigram: Field,
    substring_bigram: Field,
    substring_trigram: Field,
    document_identity: Field,
    lineage_group: Field,
    sort_key: Field,
    pagination_key: Field,
    keys: BTreeMap<String, K>,
    identities: BTreeMap<String, String>,
    standings: BTreeMap<String, SearchStandingEvidence>,
    substring_values: BTreeMap<String, Vec<String>>,
    dirty: bool,
}

/// Normalized query state and lazily built ranked query trees for one index.
/// Grouped search reuses this across its first lineage page and release facets,
/// while exact hits avoid constructing tiers they never reach.
struct PreparedSearchQuery<'index, K> {
    index: &'index TextSearchIndex<K>,
    needle: String,
    tier_queries: [OnceCell<Arc<dyn Query>>; SEARCH_TIERS.len()],
    ranked_tiers: [OnceCell<Arc<dyn Query>>; SEARCH_TIERS.len()],
}

/// Cheaply clones a prepared Tantivy query node into request-specific trees.
#[derive(Clone, Debug)]
struct SharedQuery(Arc<dyn Query>);

impl Query for SharedQuery {
    fn weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        self.0.weight(scoring)
    }

    fn query_terms<'a>(&'a self, visitor: &mut dyn FnMut(&'a Term, bool)) {
        self.0.query_terms(visitor);
    }
}

impl<'index, K: Clone> PreparedSearchQuery<'index, K> {
    fn new(index: &'index TextSearchIndex<K>, needle: String) -> Self {
        Self {
            index,
            needle,
            tier_queries: std::array::from_fn(|_| OnceCell::new()),
            ranked_tiers: std::array::from_fn(|_| OnceCell::new()),
        }
    }

    fn tier_query(&self, tier_index: usize) -> &Arc<dyn Query> {
        self.tier_queries[tier_index].get_or_init(|| {
            Arc::<dyn Query>::from(
                self.index
                    .tier_query(SEARCH_TIERS[tier_index], &self.needle),
            )
        })
    }

    fn ranked_tier_query(&self, tier_index: usize) -> &Arc<dyn Query> {
        self.ranked_tiers[tier_index].get_or_init(|| {
            let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(
                Occur::Must,
                Box::new(SharedQuery(Arc::clone(self.tier_query(tier_index)))),
            )];
            for (earlier_index, earlier_tier) in SEARCH_TIERS[..tier_index].iter().enumerate() {
                if tier_index > SearchTier::Substring as usize
                    && matches!(earlier_tier, SearchTier::Substring)
                {
                    continue;
                }
                clauses.push((
                    Occur::MustNot,
                    Box::new(SharedQuery(Arc::clone(self.tier_query(earlier_index)))),
                ));
            }
            let ranked_query: Box<dyn Query> = Box::new(BooleanQuery::new(clauses));
            Arc::<dyn Query>::from(ranked_query)
        })
    }

    fn page_filtered(
        &self,
        limit: usize,
        ecosystem: Option<&str>,
        continuation: Option<&SearchContinuation>,
    ) -> Result<IndexedSearchPage<K>, String> {
        self.page_filtered_in_group(limit, ecosystem, None, continuation)
    }

    fn page_in_group(
        &self,
        limit: usize,
        ecosystem: Option<&str>,
        lineage_group_sort_key: &str,
        continuation: Option<&SearchContinuation>,
    ) -> Result<SearchPage<K>, String> {
        let indexed = self.page_filtered_in_group(
            limit,
            ecosystem,
            Some(lineage_group_sort_key),
            continuation,
        )?;
        Ok(SearchPage {
            hits: indexed.hits,
            posting_candidates: indexed.posting_candidates,
            result_count: indexed.result_count,
            next_cursor: indexed.next_cursor,
        })
    }

    fn page_filtered_in_group(
        &self,
        limit: usize,
        ecosystem: Option<&str>,
        lineage_group: Option<&str>,
        continuation: Option<&SearchContinuation>,
    ) -> Result<IndexedSearchPage<K>, String> {
        TextSearchIndex::<K>::page_filtered_in_group_prepared(
            self,
            limit,
            ecosystem,
            lineage_group,
            continuation,
        )
    }
}

#[derive(Default)]
pub(super) struct SearchDocumentText<'a> {
    /// Additional exact/prefix coordinate values on a grouped lineage doc.
    /// They are independent index terms, never a concatenated stored value.
    pub(super) coordinate_variants: Vec<&'a str>,
    pub(super) generic: Vec<&'a str>,
    /// Whether legacy free-text content should participate in substring rank.
    /// Typed metadata and forge pins use token matching instead.
    pub(super) substring_generic: bool,
    pub(super) aliases: SearchTextFacet<'a>,
    pub(super) keywords: SearchTextFacet<'a>,
    pub(super) descriptions: SearchTextFacet<'a>,
    pub(super) advisories: SearchTextFacet<'a>,
    /// Source-specific lineage key for bounded per-lineage release facets.
    pub(super) lineage_group: Option<&'a str>,
    pub(super) standing: SearchStandingEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SearchTextFacet<'a> {
    Known(Vec<&'a str>),
    Absent,
    Unknown,
}

impl Default for SearchTextFacet<'_> {
    fn default() -> Self {
        Self::Unknown
    }
}

impl<'a> SearchTextFacet<'a> {
    fn values(&self) -> &[&'a str] {
        match self {
            Self::Known(values) => values,
            Self::Absent | Self::Unknown => &[],
        }
    }
}

fn add_discovery_document(
    index: &mut TextSearchIndex<DiscoverySearchKey>,
    source: DiscoverySourceIdentity,
    coordinate: ProductPackageCoordinate,
    lineage: &str,
    metadata: &DiscoveryMetadata,
) -> Result<(), String> {
    let coordinate_text = coordinate.as_str().to_owned();
    let source = DiscoverySearchSource::Registry(source);
    let key = DiscoverySearchKey {
        source: source.clone(),
        coordinate: coordinate.clone(),
        lineage: lineage.to_owned(),
        manifest_path: None,
    };
    let sort_key = discovery_sort_key(&source, &coordinate_text);
    let lineage_key = discovery_lineage_key(&source, source.ecosystem(), lineage);
    let group_key = lineage_sort_key(&lineage_key);
    let mut search_text = discovery_search_text(metadata);
    search_text.lineage_group = Some(&group_key);
    index.add_document_with_search_text(
        key,
        &coordinate_text,
        lineage_search_name(source.ecosystem(), lineage),
        search_text,
        source.ecosystem().as_str(),
        sort_key,
    )
}

fn replace_discovery_document(
    index: &mut TextSearchIndex<DiscoverySearchKey>,
    document: DiscoverySearchDocument,
) -> Result<(), String> {
    let source = DiscoverySearchSource::Registry(document.source);
    let coordinate_text = document.coordinate.as_str().to_owned();
    let lineage = document.lineage;
    let key = DiscoverySearchKey {
        source: source.clone(),
        coordinate: document.coordinate.clone(),
        lineage: lineage.clone(),
        manifest_path: None,
    };
    let identity = discovery_sort_key(&source, &coordinate_text);
    let lineage_key = discovery_lineage_key(&source, source.ecosystem(), &lineage);
    let group_key = lineage_sort_key(&lineage_key);
    let mut search_text = discovery_search_text(&document.metadata);
    search_text.lineage_group = Some(&group_key);
    index.replace_document_with_search_text(
        identity.clone(),
        key,
        &coordinate_text,
        lineage_search_name(source.ecosystem(), &lineage),
        search_text,
        source.ecosystem().as_str(),
        identity,
    )
}

fn rebuild_discovery_search_index(
    store: &DiscoveryStore,
    forge_documents: &BTreeMap<String, ForgeSearchDocument>,
) -> Result<TextSearchIndex<DiscoverySearchKey>, String> {
    let mut rebuilt = TextSearchIndex::new()?;
    for (source, fact) in store.facts() {
        let lineage = qualified_lineage(&fact.coordinate)?;
        add_discovery_document(
            &mut rebuilt,
            *source,
            fact.coordinate.clone(),
            &lineage,
            &fact.metadata,
        )?;
    }
    for (identity, document) in forge_documents {
        add_or_replace_forge_document(&mut rebuilt, identity.clone(), document.clone())?;
    }
    rebuilt.commit()?;
    Ok(rebuilt)
}

fn rebuild_lineage_search_index(
    store: &DiscoveryStore,
    forge_documents: &BTreeMap<String, ForgeSearchDocument>,
) -> Result<LineageSearchIndex, String> {
    let mut rebuilt = LineageSearchIndex::new()?;
    let mut changes = Vec::new();
    for (source, fact) in store.facts() {
        let lineage = qualified_lineage(&fact.coordinate)?;
        let source = DiscoverySearchSource::Registry(*source);
        let key = DiscoverySearchKey {
            source: source.clone(),
            coordinate: fact.coordinate.clone(),
            lineage: lineage.clone(),
            manifest_path: None,
        };
        let identity = discovery_sort_key(&source, fact.coordinate.as_str());
        let lineage_key = discovery_lineage_key(&source, source.ecosystem(), &lineage);
        changes.push((
            identity,
            lineage_key,
            lineage_release_document(key, &fact.metadata, None, &[]),
        ));
    }
    for (identity, document) in forge_documents {
        changes.push(forge_lineage_release(identity, document)?);
    }
    rebuilt.apply_batch(changes)?;
    rebuilt.commit()?;
    Ok(rebuilt)
}

fn forge_lineage_release(
    identity: &str,
    document: &ForgeSearchDocument,
) -> Result<(String, LineageKey, LineageReleaseDocument), String> {
    document.admit()?;
    let source = DiscoverySearchSource::Forge(document.source.clone());
    let key = DiscoverySearchKey {
        source: source.clone(),
        coordinate: document.coordinate.clone(),
        lineage: document.lineage.clone(),
        manifest_path: document.manifest_path.clone(),
    };
    let lineage_key = discovery_lineage_key(&source, source.ecosystem(), &document.lineage);
    let readme = match &document.readme {
        DiscoveryFacet::Known(readme) => Some(readme.as_str()),
        DiscoveryFacet::Absent | DiscoveryFacet::Unknown => None,
    };
    Ok((
        identity.to_owned(),
        lineage_key,
        lineage_release_document(key, &document.metadata, readme, &document.generic_terms),
    ))
}

fn forge_search_identity(document: &ForgeSearchDocument) -> String {
    format!("forge:{}", forge_document_sort_key(document))
}

fn forge_documents_fingerprint(
    documents: &BTreeMap<String, ForgeSearchDocument>,
) -> Result<[u8; 32], String> {
    if documents.is_empty() {
        return Ok([0; 32]);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend-discovery-forge-search-documents-v1\0");
    hasher.update(&(documents.len() as u64).to_le_bytes());
    for (identity, document) in documents {
        hash_revision_part(&mut hasher, b"document");
        hash_revision_part(&mut hasher, identity.as_bytes());
        hash_revision_part(
            &mut hasher,
            document.source.coordinate.canonical().as_bytes(),
        );
        hash_revision_part(&mut hasher, document.source.ecosystem.as_str().as_bytes());
        hash_revision_part(&mut hasher, document.coordinate.as_str().as_bytes());
        hash_revision_part(&mut hasher, document.lineage.as_bytes());
        hash_revision_part(
            &mut hasher,
            document
                .manifest_path
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        );
        let metadata = serde_json::to_vec(&document.metadata)
            .map_err(|error| format!("encode forge metadata revision: {error}"))?;
        hash_revision_part(&mut hasher, &metadata);
        let readme = serde_json::to_vec(&document.readme)
            .map_err(|error| format!("encode forge readme revision: {error}"))?;
        hash_revision_part(&mut hasher, &readme);
        hasher.update(&(document.generic_terms.len() as u64).to_le_bytes());
        for term in &document.generic_terms {
            hash_revision_part(&mut hasher, term.as_bytes());
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

fn forge_source_pin_identity(key: &ForgeSourcePinSearchKey) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend-discovery-forge-source-pin-key-v1\0");
    hash_revision_part(&mut hasher, key.source.canonical().as_bytes());
    hash_revision_part(&mut hasher, key.ecosystem.as_str().as_bytes());
    hash_revision_part(&mut hasher, key.manifest_path.as_bytes());
    hash_revision_part(&mut hasher, key.resolved_commit.as_hex().as_bytes());
    format!("forge-source-pin:{}", hasher.finalize().to_hex())
}

pub(crate) fn forge_source_pin_sort_key(key: &ForgeSourcePinSearchKey) -> String {
    forge_source_pin_identity(key)
}

fn forge_source_pin_documents_fingerprint(
    documents: &BTreeMap<String, ForgeSourcePinSearchDocument>,
) -> Result<[u8; 32], String> {
    if documents.is_empty() {
        return Ok([0; 32]);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend-discovery-forge-source-pins-v1\0");
    hasher.update(&(documents.len() as u64).to_le_bytes());
    for (identity, document) in documents {
        hash_revision_part(&mut hasher, identity.as_bytes());
        hash_revision_part(&mut hasher, document.name.as_bytes());
        let metadata = serde_json::to_vec(&document.metadata)
            .map_err(|error| format!("encode forge source-pin metadata revision: {error}"))?;
        hash_revision_part(&mut hasher, &metadata);
        let readme = serde_json::to_vec(&document.readme)
            .map_err(|error| format!("encode forge source-pin README revision: {error}"))?;
        hash_revision_part(&mut hasher, &readme);
        hasher.update(&(document.generic_terms.len() as u64).to_le_bytes());
        for term in &document.generic_terms {
            hash_revision_part(&mut hasher, term.as_bytes());
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

fn combined_forge_fingerprints(
    release_fingerprint: [u8; 32],
    source_pin_fingerprint: [u8; 32],
) -> [u8; 32] {
    if release_fingerprint == [0; 32] && source_pin_fingerprint == [0; 32] {
        return [0; 32];
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend-discovery-forge-catalog-v1\0");
    hasher.update(&release_fingerprint);
    hasher.update(&source_pin_fingerprint);
    *hasher.finalize().as_bytes()
}

fn hash_revision_part(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn combined_search_revision(registry_fingerprint: [u8; 32], forge_fingerprint: [u8; 32]) -> u64 {
    let root = combined_search_snapshot_root(registry_fingerprint, forge_fingerprint);
    u64::from_le_bytes(root[..8].try_into().expect("snapshot root is fixed-width"))
}

fn combined_search_snapshot_root(
    registry_fingerprint: [u8; 32],
    forge_fingerprint: [u8; 32],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend-discovery-search-revision-v2\0");
    hasher.update(&registry_fingerprint);
    hasher.update(&forge_fingerprint);
    *hasher.finalize().as_bytes()
}

fn forge_document_sort_key(document: &ForgeSearchDocument) -> String {
    format!(
        "{}:{}",
        discovery_sort_key(
            &DiscoverySearchSource::Forge(document.source.clone()),
            document.coordinate.as_str()
        ),
        document.manifest_path.as_deref().unwrap_or_default()
    )
}

fn add_or_replace_forge_document(
    index: &mut TextSearchIndex<DiscoverySearchKey>,
    identity: String,
    document: ForgeSearchDocument,
) -> Result<(), String> {
    let sort_key = forge_document_sort_key(&document);
    let source = DiscoverySearchSource::Forge(document.source.clone());
    let coordinate_text = document.coordinate.as_str().to_owned();
    let key = DiscoverySearchKey {
        source: source.clone(),
        coordinate: document.coordinate.clone(),
        lineage: document.lineage.clone(),
        manifest_path: document.manifest_path.clone(),
    };
    let mut search_text = discovery_search_text(&document.metadata);
    if let DiscoveryFacet::Known(readme) = &document.readme {
        let mut descriptions = search_text.descriptions.values().to_vec();
        descriptions.push(readme);
        search_text.descriptions = SearchTextFacet::Known(descriptions);
    }
    search_text
        .generic
        .extend(document.generic_terms.iter().map(String::as_str));
    let lineage_key = discovery_lineage_key(&source, source.ecosystem(), &document.lineage);
    let group_key = lineage_sort_key(&lineage_key);
    search_text.lineage_group = Some(&group_key);
    index.replace_document_with_search_text(
        identity,
        key,
        &coordinate_text,
        lineage_search_name(source.ecosystem(), &document.lineage),
        search_text,
        source.ecosystem().as_str(),
        sort_key,
    )
}

fn add_or_replace_forge_source_pin(
    index: &mut TextSearchIndex<ForgeSourcePinSearchKey>,
    identity: String,
    document: ForgeSourcePinSearchDocument,
) -> Result<(), String> {
    document.admit()?;
    let sort_key = identity.clone();
    let mut search_text = discovery_search_text(&document.metadata);
    if let DiscoveryFacet::Known(readme) = &document.readme {
        let mut descriptions = search_text.descriptions.values().to_vec();
        descriptions.push(readme);
        search_text.descriptions = SearchTextFacet::Known(descriptions);
    }
    search_text
        .generic
        .extend(document.generic_terms.iter().map(String::as_str));
    index.replace_document_with_search_text(
        identity,
        document.key.clone(),
        &document.key.source.canonical(),
        &document.name,
        search_text,
        document.key.ecosystem.as_str(),
        sort_key,
    )
}

fn rebuild_forge_source_pin_index(
    documents: &BTreeMap<String, ForgeSourcePinSearchDocument>,
) -> Result<TextSearchIndex<ForgeSourcePinSearchKey>, String> {
    let mut index = TextSearchIndex::new_with_memory(SOURCE_PIN_WRITER_MEMORY_BYTES)?;
    for (identity, document) in documents {
        add_or_replace_forge_source_pin(&mut index, identity.clone(), document.clone())?;
    }
    index.commit()?;
    Ok(index)
}

fn forge_search_document(
    result: &ForgeSearchRecord,
    manifest: &ForgePackageManifest,
) -> Result<Option<ForgeSearchDocument>, String> {
    let Some(coordinate) = forge_manifest_package_coordinate(manifest) else {
        return Ok(None);
    };
    let version = match &manifest.version {
        ForgeFact::Recorded(value) => value.as_str().to_owned(),
        ForgeFact::Unavailable(_) => return Ok(None),
    };
    let name = match &manifest.name {
        ForgeFact::Recorded(value) => value.as_str(),
        ForgeFact::Unavailable(_) => return Ok(None),
    };
    let lineage = match backend_engine::registry::admit_registry_coordinate(&coordinate) {
        Ok(coordinate) => coordinate.qualified_name().as_str().to_owned(),
        Err(_) => return Ok(None),
    };

    let mut aliases = vec![result.coordinate.repository().to_owned()];
    aliases.push(format!(
        "{}/{}",
        result.coordinate.owner(),
        result.coordinate.repository()
    ));
    let mut keywords = vec![manifest.ecosystem.as_str().to_owned()];
    let license = match &result.metadata.license {
        ForgeFact::Recorded(value) => {
            keywords.push(value.as_str().to_owned());
            DiscoveryFacet::Known(value.as_str().to_owned())
        }
        ForgeFact::Unavailable(_) => DiscoveryFacet::Unknown,
    };
    if let ForgeFact::Recorded(topics) = &result.metadata.topics {
        keywords.extend(topics.iter().map(|topic| topic.as_str().to_owned()));
    }
    let mut generic_terms = vec![
        result.coordinate.canonical(),
        result.coordinate.repository_url().to_owned(),
        result.resolution.commit.as_hex(),
        hex(&result.archive),
        manifest.path.to_string(),
    ];
    if let Some(tree) = &result.resolution.tree {
        generic_terms.push(tree.as_hex());
    }
    let description = match &result.metadata.description {
        ForgeFact::Recorded(value) => DiscoveryFacet::Known(value.as_str().to_owned()),
        ForgeFact::Unavailable(_) => DiscoveryFacet::Unknown,
    };
    let readme = match &result.metadata.readme {
        ForgeFact::Recorded(value) => DiscoveryFacet::Known(value.as_str().to_owned()),
        ForgeFact::Unavailable(_) => DiscoveryFacet::Unknown,
    };
    match &manifest.dependencies {
        DependencyFacts::Known(rows) => {
            for row in rows.iter() {
                keywords.push(row.target.name.as_str().to_owned());
                generic_terms.push(row.target.requirement.as_str().to_owned());
                if let Some(resolved) = &row.target.resolved {
                    generic_terms.push(resolved.as_str().to_owned());
                }
            }
        }
        DependencyFacts::Unknown(_) | DependencyFacts::Unavailable(_) => {}
    }
    if let ForgeFact::Recorded(name) = &manifest.name {
        aliases.retain(|alias| alias != name.as_str());
    }

    let metadata = DiscoveryMetadata {
        aliases: DiscoveryFacet::Known(aliases),
        description,
        keywords: DiscoveryFacet::Known(keywords),
        license,
        ..DiscoveryMetadata::default()
    };
    generic_terms.push(version);
    Ok(Some(ForgeSearchDocument {
        source: ForgeSearchSourceIdentity {
            coordinate: result.coordinate.clone(),
            ecosystem: manifest.ecosystem,
        },
        coordinate,
        lineage,
        manifest_path: Some(manifest.path.to_string()),
        metadata,
        readme,
        generic_terms,
    }))
}

fn forge_manifest_package_coordinate(
    manifest: &ForgePackageManifest,
) -> Option<ProductPackageCoordinate> {
    let (ForgeFact::Recorded(name), ForgeFact::Recorded(version)) =
        (&manifest.name, &manifest.version)
    else {
        return None;
    };
    let package_name = if manifest.ecosystem == RegistryEcosystem::Maven {
        name.as_str().replace(':', "/")
    } else {
        name.as_str().to_owned()
    };
    ProductPackageCoordinate::parse(format!(
        "pkg:{}/{}@{}",
        manifest.ecosystem.package_type().as_str(),
        package_name,
        version.as_str()
    ))
    .ok()
}

fn discovery_search_text(metadata: &DiscoveryMetadata) -> SearchDocumentText<'_> {
    fn append_known_list<'a>(target: &mut Vec<&'a str>, facet: &'a DiscoveryFacet<Vec<String>>) {
        if let DiscoveryFacet::Known(values) = facet {
            target.extend(
                values
                    .iter()
                    .map(String::as_str)
                    .filter(|value| !value.is_empty()),
            );
        }
    }

    fn append_known_text<'a>(target: &mut Vec<&'a str>, facet: &'a DiscoveryFacet<String>) {
        if let DiscoveryFacet::Known(value) = facet
            && !value.is_empty()
        {
            target.push(value);
        }
    }

    let mut aliases = Vec::new();
    let mut keywords = Vec::new();
    let mut descriptions = Vec::new();
    let mut advisories_text = Vec::new();
    append_known_list(&mut aliases, &metadata.aliases);
    append_known_list(&mut keywords, &metadata.keywords);
    append_known_text(&mut descriptions, &metadata.description);
    if let DiscoveryFacet::Known(advisories) = &metadata.advisories {
        for advisory in advisories {
            if !advisory.id.is_empty() {
                advisories_text.push(advisory.id.as_str());
            }
            advisories_text.extend(
                advisory
                    .aliases
                    .iter()
                    .map(String::as_str)
                    .filter(|value| !value.is_empty()),
            );
            append_known_list(&mut advisories_text, &advisory.fixed_in);
            append_known_text(&mut advisories_text, &advisory.summary);
            append_known_text(&mut advisories_text, &advisory.severity);
        }
    }
    SearchDocumentText {
        coordinate_variants: Vec::new(),
        generic: Vec::new(),
        substring_generic: false,
        aliases: search_text_facet(&metadata.aliases, aliases),
        keywords: search_text_facet(&metadata.keywords, keywords),
        descriptions: search_text_facet(&metadata.description, descriptions),
        advisories: search_text_facet(&metadata.advisories, advisories_text),
        lineage_group: None,
        standing: SearchStandingEvidence::Unknown,
    }
}

fn search_text_facet<'a, T>(
    facet: &'a DiscoveryFacet<T>,
    values: Vec<&'a str>,
) -> SearchTextFacet<'a> {
    match facet {
        DiscoveryFacet::Known(_) => SearchTextFacet::Known(values),
        DiscoveryFacet::Absent => SearchTextFacet::Absent,
        DiscoveryFacet::Unknown => SearchTextFacet::Unknown,
    }
}

fn lineage_release_document(
    key: DiscoverySearchKey,
    metadata: &DiscoveryMetadata,
    readme: Option<&str>,
    generic_terms: &[String],
) -> LineageReleaseDocument {
    let search_text = discovery_search_text(metadata);
    let aliases: BTreeSet<String> = search_text
        .aliases
        .values()
        .iter()
        .map(|value| normalize(value))
        .filter(|value| !value.is_empty())
        .collect();
    let alias_tokens = token_set(aliases.iter().map(String::as_str));
    let keywords = token_set(search_text.keywords.values().iter().copied());
    let mut descriptions = token_set(search_text.descriptions.values().iter().copied());
    if let Some(readme) = readme {
        descriptions.extend(token_set(std::iter::once(readme)));
    }
    let advisories = token_set(search_text.advisories.values().iter().copied());
    let mut generic = token_set(generic_terms.iter().map(String::as_str));
    generic.extend(token_set(search_text.generic.iter().copied()));
    let source = key.source.clone();
    let ecosystem = source.ecosystem();
    let lineage = key.lineage.clone();
    let release_identity = discovery_release_identity(&key);
    let lineage_key = discovery_lineage_key(&source, ecosystem, &lineage);
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"backend-lineage-search-document-v1\0");
    hash_revision_part(&mut fingerprint, release_identity.as_bytes());
    hash_revision_part(&mut fingerprint, lineage_sort_key(&lineage_key).as_bytes());
    hash_revision_part(&mut fingerprint, key.coordinate.as_str().as_bytes());
    for (tag, values) in [
        (b"alias".as_slice(), &aliases),
        (b"keyword".as_slice(), &keywords),
        (b"description".as_slice(), &descriptions),
        (b"advisory".as_slice(), &advisories),
        (b"generic".as_slice(), &generic),
    ] {
        hash_revision_part(&mut fingerprint, tag);
        for value in values {
            hash_revision_part(&mut fingerprint, value.as_bytes());
        }
    }
    LineageReleaseDocument {
        coordinate: key.coordinate.as_str().to_owned(),
        version: key.coordinate.version().to_owned(),
        order_sort_key: discovery_sort_key(&key.source, key.coordinate.as_str()),
        aliases,
        alias_tokens,
        keywords,
        descriptions,
        advisories,
        generic,
        fingerprint: *fingerprint.finalize().as_bytes(),
    }
}

fn discovery_release_identity(key: &DiscoverySearchKey) -> String {
    let coordinate = key.coordinate.as_str();
    let sort_key = discovery_sort_key(&key.source, coordinate);
    match &key.source {
        DiscoverySearchSource::Registry(_) => sort_key,
        DiscoverySearchSource::Forge(_) => format!(
            "forge:{sort_key}:{}",
            key.manifest_path.as_deref().unwrap_or_default()
        ),
    }
}

fn token_set<'a>(values: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    for value in values {
        let normalized = normalize(value);
        tokens.extend(
            normalized
                .split(|character: char| !character.is_alphanumeric())
                .filter(|token| !token.is_empty())
                .map(str::to_owned),
        );
    }
    tokens
}

fn discovery_lineage_key(
    source: &DiscoverySearchSource,
    ecosystem: RegistryEcosystem,
    lineage: &str,
) -> LineageKey {
    LineageKey {
        source: LineageSearchSource::Discovery(source.clone()),
        ecosystem,
        lineage: lineage.to_owned(),
    }
}

/// Deterministic stored sort key shared by discovery and acquired projections.
pub(crate) fn lineage_sort_key(key: &LineageKey) -> String {
    let (source_kind, source_id, authority_marker) = match &key.source {
        LineageSearchSource::Discovery(DiscoverySearchSource::Registry(source)) => {
            ("0", source.id(), "")
        }
        LineageSearchSource::Discovery(DiscoverySearchSource::Forge(source)) => {
            ("1", source.coordinate.identity(), "")
        }
        LineageSearchSource::Acquired { source, .. } => (
            "2",
            source.unwrap_or([0; 32]),
            if source.is_some() { "1" } else { "0" },
        ),
    };
    format!(
        "{source_kind}\u{1f}{}\u{1f}{}\u{1f}{authority_marker}\u{1f}{}:{}",
        key.ecosystem.as_str(),
        hex(&source_id),
        key.lineage.len(),
        key.lineage,
    )
}

impl<K: Clone> TextSearchIndex<K> {
    pub(super) fn new() -> Result<Self, String> {
        Self::new_with_memory(WRITER_MEMORY_BYTES)
    }

    pub(super) fn new_with_memory(writer_memory_bytes: usize) -> Result<Self, String> {
        let mut schema = Schema::builder();
        let exact_coordinate = schema.add_text_field("exact_coordinate", STRING);
        let exact_name = schema.add_text_field("exact_name", STRING);
        let exact_alias = schema.add_text_field("exact_alias", STRING);
        let ecosystem = schema.add_text_field("ecosystem", STRING);
        let searchable_content = schema.add_text_field("searchable_content", TEXT);
        let searchable_alias = schema.add_text_field("searchable_alias", TEXT);
        let searchable_keyword = schema.add_text_field("searchable_keyword", TEXT);
        let searchable_description = schema.add_text_field("searchable_description", TEXT);
        let searchable_advisory = schema.add_text_field("searchable_advisory", TEXT);
        let lineage_group = schema.add_text_field("lineage_group", STRING);
        let substring_unigram = schema.add_text_field("substring_unigram", TEXT);
        let substring_bigram = schema.add_text_field("substring_bigram", TEXT);
        let substring_trigram = schema.add_text_field("substring_trigram", TEXT);
        let document_identity = schema.add_text_field("document_identity", STRING);
        let sort_key = schema.add_text_field(SORT_KEY_FIELD, FAST);
        let pagination_key = schema.add_text_field("pagination_key", STRING);
        let index = Index::create_in_ram(schema.build());
        let writer = index
            .writer_with_num_threads(1, writer_memory_bytes)
            .map_err(|error| error.to_string())?;
        let reader = index.reader().map_err(|error| error.to_string())?;
        Ok(Self {
            _index: index,
            writer,
            reader,
            exact_coordinate,
            exact_name,
            exact_alias,
            ecosystem,
            searchable_content,
            searchable_alias,
            searchable_keyword,
            searchable_description,
            searchable_advisory,
            lineage_group,
            substring_unigram,
            substring_bigram,
            substring_trigram,
            document_identity,
            sort_key,
            pagination_key,
            keys: BTreeMap::new(),
            identities: BTreeMap::new(),
            standings: BTreeMap::new(),
            substring_values: BTreeMap::new(),
            dirty: false,
        })
    }

    pub(super) fn add_document<'a>(
        &mut self,
        key: K,
        coordinate: &str,
        name: &str,
        content: impl IntoIterator<Item = &'a str>,
        sort_key: String,
    ) -> Result<(), String> {
        let search_text = SearchDocumentText {
            generic: content.into_iter().collect(),
            substring_generic: true,
            ..SearchDocumentText::default()
        };
        self.insert_document(
            sort_key.clone(),
            key,
            coordinate,
            name,
            search_text,
            None,
            sort_key,
        )
    }

    pub(super) fn add_document_with_ecosystem<'a>(
        &mut self,
        key: K,
        coordinate: &str,
        name: &str,
        content: impl IntoIterator<Item = &'a str>,
        ecosystem: &str,
        sort_key: String,
    ) -> Result<(), String> {
        let search_text = SearchDocumentText {
            generic: content.into_iter().collect(),
            substring_generic: true,
            ..SearchDocumentText::default()
        };
        self.insert_document(
            sort_key.clone(),
            key,
            coordinate,
            name,
            search_text,
            Some(ecosystem),
            sort_key,
        )
    }

    pub(super) fn replace_document<'a>(
        &mut self,
        identity: String,
        key: K,
        coordinate: &str,
        name: &str,
        content: impl IntoIterator<Item = &'a str>,
        sort_key: String,
    ) -> Result<(), String> {
        let search_text = SearchDocumentText {
            generic: content.into_iter().collect(),
            substring_generic: true,
            ..SearchDocumentText::default()
        };
        self.remove_document(&identity);
        self.insert_document(identity, key, coordinate, name, search_text, None, sort_key)
    }

    pub(super) fn replace_document_with_ecosystem<'a>(
        &mut self,
        identity: String,
        key: K,
        coordinate: &str,
        name: &str,
        content: impl IntoIterator<Item = &'a str>,
        ecosystem: &str,
        sort_key: String,
    ) -> Result<(), String> {
        let search_text = SearchDocumentText {
            generic: content.into_iter().collect(),
            substring_generic: true,
            ..SearchDocumentText::default()
        };
        self.remove_document(&identity);
        self.insert_document(
            identity,
            key,
            coordinate,
            name,
            search_text,
            Some(ecosystem),
            sort_key,
        )
    }

    pub(super) fn add_document_with_search_text(
        &mut self,
        key: K,
        coordinate: &str,
        name: &str,
        search_text: SearchDocumentText<'_>,
        ecosystem: &str,
        sort_key: String,
    ) -> Result<(), String> {
        self.insert_document(
            sort_key.clone(),
            key,
            coordinate,
            name,
            search_text,
            Some(ecosystem),
            sort_key,
        )
    }

    pub(super) fn replace_document_with_search_text(
        &mut self,
        identity: String,
        key: K,
        coordinate: &str,
        name: &str,
        search_text: SearchDocumentText<'_>,
        ecosystem: &str,
        sort_key: String,
    ) -> Result<(), String> {
        self.remove_document(&identity);
        self.insert_document(
            identity,
            key,
            coordinate,
            name,
            search_text,
            Some(ecosystem),
            sort_key,
        )
    }

    fn insert_document<'a>(
        &mut self,
        identity: String,
        key: K,
        coordinate: &str,
        name: &str,
        search_text: SearchDocumentText<'a>,
        ecosystem: Option<&str>,
        sort_key: String,
    ) -> Result<(), String> {
        if self.keys.contains_key(&sort_key) {
            return Err("duplicate stable sort key in Tantivy projection".to_owned());
        }
        if self.identities.contains_key(&identity) {
            return Err("duplicate document identity in Tantivy projection".to_owned());
        }
        let coordinate = normalize(coordinate);
        let name = normalize(name);
        let standing = search_text.standing;
        let generic = search_text.generic;
        let mut substring_fields = vec![coordinate.as_str()];
        substring_fields.extend(search_text.coordinate_variants.iter().copied());
        substring_fields.push(name.as_str());
        if search_text.substring_generic {
            substring_fields.extend(generic.iter().copied());
        }
        let mut substring_values = substring_fields
            .iter()
            .map(|field| normalize(field))
            .collect::<Vec<_>>();
        let mut document = TantivyDocument::default();
        document.add_text(self.exact_coordinate, coordinate.as_str());
        for coordinate in &search_text.coordinate_variants {
            let coordinate = normalize(coordinate);
            if !coordinate.is_empty() {
                document.add_text(self.exact_coordinate, coordinate.as_str());
            }
        }
        document.add_text(self.exact_name, name.as_str());
        for alias in search_text.aliases.values() {
            let alias = normalize(alias);
            if !alias.is_empty() {
                document.add_text(self.exact_alias, alias.as_str());
            }
            document.add_text(self.searchable_alias, alias.as_str());
        }
        if let Some(ecosystem) = ecosystem {
            document.add_text(self.ecosystem, ecosystem);
        }
        if let Some(lineage_group) = search_text.lineage_group {
            document.add_text(self.lineage_group, lineage_group);
        }
        for field in &generic {
            document.add_text(self.searchable_content, field);
        }
        for field in search_text.keywords.values() {
            document.add_text(self.searchable_keyword, field);
        }
        for field in search_text.descriptions.values() {
            document.add_text(self.searchable_description, field);
        }
        for field in search_text.advisories.values() {
            document.add_text(self.searchable_advisory, field);
        }
        document.add_text(self.document_identity, identity.as_str());
        document.add_text(self.sort_key, sort_key.as_str());
        document.add_text(self.pagination_key, sort_key.as_str());
        document.add_text(
            self.substring_unigram,
            combined_gram_stream::<1>(substring_values.iter().map(String::as_str)).as_str(),
        );
        document.add_text(
            self.substring_bigram,
            combined_gram_stream::<2>(substring_values.iter().map(String::as_str)).as_str(),
        );
        document.add_text(
            self.substring_trigram,
            combined_gram_stream::<3>(substring_values.iter().map(String::as_str)).as_str(),
        );
        substring_values.retain(|field| !field.is_empty());
        self.dirty = true;
        self.writer
            .add_document(document)
            .map_err(|error| error.to_string())?;
        self.identities.insert(identity, sort_key.clone());
        self.standings.insert(sort_key.clone(), standing);
        self.substring_values
            .insert(sort_key.clone(), substring_values);
        self.keys.insert(sort_key, key);
        Ok(())
    }

    fn remove_document(&mut self, identity: &str) {
        let Some(sort_key) = self.identities.remove(identity) else {
            return;
        };
        self.writer
            .delete_term(Term::from_field_text(self.document_identity, identity));
        self.keys.remove(&sort_key);
        self.standings.remove(&sort_key);
        self.substring_values.remove(&sort_key);
        self.dirty = true;
    }

    fn contains_substring(&self, sort_key: &str, needle: &str) -> bool {
        self.substring_values
            .get(sort_key)
            .is_some_and(|values| values.iter().any(|value| value.contains(needle)))
    }

    pub(super) fn commit(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        self.writer.commit().map_err(|error| error.to_string())?;
        self.reader.reload().map_err(|error| error.to_string())?;
        self.dirty = false;
        Ok(())
    }

    fn prepare_search_query(&self, needle: String) -> PreparedSearchQuery<'_, K> {
        PreparedSearchQuery::new(self, needle)
    }

    pub(super) fn page(&self, query: &str, limit: usize) -> Result<SearchPage<K>, String> {
        self.page_with_ecosystem(query, limit, None, None)
    }

    pub(super) fn page_with_ecosystem(
        &self,
        query: &str,
        limit: usize,
        ecosystem: Option<&str>,
        continuation: Option<&SearchContinuation>,
    ) -> Result<SearchPage<K>, String> {
        let indexed = self.page_filtered(query, limit, ecosystem, continuation)?;
        Ok(SearchPage {
            hits: indexed.hits,
            posting_candidates: indexed.posting_candidates,
            result_count: indexed.result_count,
            next_cursor: indexed.next_cursor,
        })
    }

    pub(super) fn page_filtered(
        &self,
        query: &str,
        limit: usize,
        ecosystem: Option<&str>,
        continuation: Option<&SearchContinuation>,
    ) -> Result<IndexedSearchPage<K>, String> {
        self.page_filtered_in_group(query, limit, ecosystem, None, continuation)
    }

    pub(super) fn page_in_group(
        &self,
        query: &str,
        limit: usize,
        ecosystem: Option<&str>,
        lineage_group: &str,
        continuation: Option<&SearchContinuation>,
    ) -> Result<SearchPage<K>, String> {
        if query.len() > MAX_QUERY_BYTES {
            return Err("catalog search query exceeds its byte limit".to_owned());
        }
        self.prepare_search_query(normalize(query)).page_in_group(
            limit,
            ecosystem,
            lineage_group,
            continuation,
        )
    }

    pub(super) fn page_filtered_in_group(
        &self,
        query: &str,
        limit: usize,
        ecosystem: Option<&str>,
        lineage_group: Option<&str>,
        continuation: Option<&SearchContinuation>,
    ) -> Result<IndexedSearchPage<K>, String> {
        if query.len() > MAX_QUERY_BYTES {
            return Err("catalog search query exceeds its byte limit".to_owned());
        }
        let prepared = self.prepare_search_query(normalize(query));
        prepared.page_filtered_in_group(limit, ecosystem, lineage_group, continuation)
    }

    fn page_filtered_in_group_prepared(
        prepared: &PreparedSearchQuery<'_, K>,
        limit: usize,
        ecosystem: Option<&str>,
        lineage_group: Option<&str>,
        continuation: Option<&SearchContinuation>,
    ) -> Result<IndexedSearchPage<K>, String> {
        let index = prepared.index;
        let limit = limit.min(MAX_INTERNAL_SEARCH_PAGE_SIZE);
        if limit == 0 {
            return Ok(IndexedSearchPage {
                hits: Vec::new(),
                posting_candidates: 0,
                result_count: SearchResultCount::Unknown,
                next_cursor: None,
            });
        }
        if index.keys.is_empty() {
            return Ok(IndexedSearchPage {
                hits: Vec::new(),
                posting_candidates: 0,
                result_count: SearchResultCount::Exact(0),
                next_cursor: None,
            });
        }
        let needle = prepared.needle.as_str();
        if needle.is_empty() {
            if ecosystem.is_some() || lineage_group.is_some() {
                let after = continuation.map(|value| value.after_sort_key.as_str());
                let mut clauses = Vec::new();
                if let Some(ecosystem) = ecosystem {
                    clauses.push((Occur::Must, exact_term_query(index.ecosystem, ecosystem)));
                }
                if let Some(lineage_group) = lineage_group {
                    clauses.push((
                        Occur::Must,
                        exact_term_query(index.lineage_group, lineage_group),
                    ));
                }
                if let Some(after_sort_key) = after {
                    clauses.push((
                        Occur::Must,
                        Box::new(RangeQuery::new(
                            Bound::Excluded(Term::from_field_text(
                                index.pagination_key,
                                after_sort_key,
                            )),
                            Bound::Unbounded,
                        )),
                    ));
                }
                let (posting_candidates, selected) = index.collect_category(
                    Box::new(BooleanQuery::new(clauses)),
                    limit.saturating_add(1),
                    u8::MAX,
                    after,
                )?;
                return Ok(make_indexed_page(
                    selected,
                    limit,
                    posting_candidates,
                    continuation.map_or(0, |value| value.returned_before),
                ));
            }
            let after = continuation.map(|value| value.after_sort_key.as_str());
            let lower = after.map_or(Bound::Unbounded, |value| Bound::Excluded(value.to_owned()));
            let mut selected = index
                .keys
                .range((lower, Bound::Unbounded))
                .take(limit.saturating_add(1))
                .map(|(sort_key, key)| {
                    (
                        u8::MAX,
                        sort_key.clone(),
                        key.clone(),
                        index.standings.get(sort_key).copied().unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>();
            let has_more = selected.len() > limit;
            let next_cursor = if has_more {
                selected.truncate(limit);
                selected
                    .last()
                    .map(|(_, sort_key, _, _)| SearchContinuation {
                        next_tier: u8::MAX,
                        after_sort_key: sort_key.clone(),
                        returned_before: continuation
                            .map_or(0, |value| value.returned_before)
                            .saturating_add(selected.len()),
                    })
            } else {
                None
            };
            return Ok(IndexedSearchPage {
                hits: selected
                    .into_iter()
                    .enumerate()
                    .map(|(index, (_, sort_key, key, standing))| SearchHit {
                        key,
                        evidence: SearchMatchEvidence::AllDocuments,
                        standing,
                        continuation_after: SearchContinuation {
                            next_tier: u8::MAX,
                            after_sort_key: sort_key,
                            returned_before: continuation
                                .map_or(0, |value| value.returned_before)
                                .saturating_add(index)
                                .saturating_add(1),
                        },
                    })
                    .collect(),
                posting_candidates: 0,
                result_count: SearchResultCount::Exact(index.keys.len()),
                next_cursor,
            });
        }
        let mut posting_candidates = 0_usize;
        let mut selected = Vec::with_capacity(limit.saturating_add(1));
        let first_tier = continuation.map_or(0, |value| usize::from(value.next_tier));
        if first_tier >= SEARCH_TIERS.len() {
            return Err("search continuation tier is outside the ranking schema".to_owned());
        }
        for tier_index in first_tier..SEARCH_TIERS.len() {
            let mut after_sort_key = continuation
                .filter(|value| usize::from(value.next_tier) == tier_index)
                .map(|value| value.after_sort_key.clone());
            loop {
                let remaining = limit.saturating_add(1).saturating_sub(selected.len());
                if remaining == 0 {
                    break;
                }
                let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(
                    Occur::Must,
                    Box::new(SharedQuery(Arc::clone(
                        prepared.ranked_tier_query(tier_index),
                    ))),
                )];
                if let Some(ecosystem) = ecosystem {
                    clauses.push((Occur::Must, exact_term_query(index.ecosystem, ecosystem)));
                }
                if let Some(lineage_group) = lineage_group {
                    clauses.push((
                        Occur::Must,
                        exact_term_query(index.lineage_group, lineage_group),
                    ));
                }
                if let Some(after_sort_key) = after_sort_key.as_deref() {
                    clauses.push((
                        Occur::Must,
                        Box::new(RangeQuery::new(
                            Bound::Excluded(Term::from_field_text(
                                index.pagination_key,
                                after_sort_key,
                            )),
                            Bound::Unbounded,
                        )),
                    ));
                }
                let query: Box<dyn Query> = Box::new(BooleanQuery::new(clauses));
                let (candidates, fetched) = index.collect_category(
                    query,
                    remaining,
                    tier_index as u8,
                    after_sort_key.as_deref(),
                )?;
                posting_candidates = posting_candidates.saturating_add(candidates);
                let fetched_count = fetched.len();
                let last_fetched = fetched.last().map(|(_, sort_key, _, _)| sort_key.clone());
                if tier_index > SearchTier::Substring as usize {
                    selected.extend(
                        fetched.into_iter().filter(|(_, sort_key, _, _)| {
                            !index.contains_substring(sort_key, needle)
                        }),
                    );
                    if selected.len() >= limit.saturating_add(1) || fetched_count < remaining {
                        break;
                    }
                    let Some(last_fetched) = last_fetched else {
                        break;
                    };
                    after_sort_key = Some(last_fetched);
                    continue;
                }
                selected.extend(fetched);
                break;
            }
            if selected.len() >= limit.saturating_add(1) {
                break;
            }
        }
        Ok(make_indexed_page(
            selected,
            limit,
            posting_candidates,
            continuation.map_or(0, |value| value.returned_before),
        ))
    }

    fn tier_query(&self, tier: SearchTier, needle: &str) -> Box<dyn Query> {
        match tier {
            SearchTier::ExactCoordinate => exact_term_query(self.exact_coordinate, needle),
            SearchTier::ExactName => exact_term_query(self.exact_name, needle),
            SearchTier::ExactAlias => exact_term_query(self.exact_alias, needle),
            SearchTier::PrefixCoordinate => prefix_query(self.exact_coordinate, needle),
            SearchTier::PrefixName => prefix_query(self.exact_name, needle),
            SearchTier::PrefixAlias => prefix_query(self.exact_alias, needle),
            SearchTier::Substring => substring_query(
                self.substring_unigram,
                self.substring_bigram,
                self.substring_trigram,
                needle,
            ),
            SearchTier::AliasTerms => searchable_content_query(self.searchable_alias, needle),
            SearchTier::KeywordTerms => searchable_content_query(self.searchable_keyword, needle),
            SearchTier::AdvisoryTerms => searchable_content_query(self.searchable_advisory, needle),
            SearchTier::DescriptionTerms => {
                searchable_content_query(self.searchable_description, needle)
            }
            SearchTier::GenericTerms => searchable_content_query(self.searchable_content, needle),
            SearchTier::FuzzyName => fuzzy_name_query(self.exact_name, needle),
            SearchTier::FuzzyAlias => fuzzy_name_query(self.exact_alias, needle),
        }
    }

    fn collect_category(
        &self,
        query: Box<dyn Query>,
        limit: usize,
        tier: u8,
        after_sort_key: Option<&str>,
    ) -> Result<(usize, Vec<(u8, String, K, SearchStandingEvidence)>), String> {
        if limit == 0 {
            return Ok((0, Vec::new()));
        }
        let fetch = limit.min(self.keys.len());
        let searcher = self.reader.searcher();
        let results = searcher
            .search(
                &query,
                &TopDocs::with_limit(fetch).order_by_string_fast_field(SORT_KEY_FIELD, Order::Asc),
            )
            .map_err(|error| error.to_string())?;
        let posting_candidates = results.len();
        let mut selected = Vec::with_capacity(posting_candidates);
        for (sort_key, _) in results {
            let sort_key = sort_key
                .ok_or_else(|| "Tantivy discovery document has no stable sort key".to_owned())?;
            let key = self
                .keys
                .get(&sort_key)
                .ok_or_else(|| "Tantivy discovery sort key is outside its revision".to_owned())?;
            if after_sort_key.is_none_or(|after| sort_key.as_str() > after) {
                selected.push((
                    tier,
                    sort_key.clone(),
                    key.clone(),
                    self.standings.get(&sort_key).copied().unwrap_or_default(),
                ));
            }
        }
        Ok((posting_candidates, selected))
    }

    fn is_dirty(&self) -> bool {
        self.dirty
    }

    #[cfg(test)]
    fn identity(&self) -> usize {
        std::ptr::from_ref(&self._index) as usize
    }
}

fn make_indexed_page<K>(
    mut selected: Vec<(u8, String, K, SearchStandingEvidence)>,
    limit: usize,
    posting_candidates: usize,
    returned_before: usize,
) -> IndexedSearchPage<K> {
    let has_lookahead = selected.len() > limit;
    let next_cursor = if has_lookahead {
        selected.truncate(limit);
        selected
            .last()
            .map(|(next_tier, sort_key, _, _)| SearchContinuation {
                next_tier: *next_tier,
                after_sort_key: sort_key.clone(),
                returned_before: returned_before.saturating_add(selected.len()),
            })
    } else {
        None
    };
    let result_count = if has_lookahead {
        SearchResultCount::AtLeast(returned_before.saturating_add(limit.saturating_add(1)))
    } else {
        SearchResultCount::Exact(returned_before.saturating_add(selected.len()))
    };
    IndexedSearchPage {
        hits: selected
            .into_iter()
            .enumerate()
            .map(|(index, (tier, sort_key, key, standing))| SearchHit {
                key,
                evidence: search_evidence(tier),
                standing,
                continuation_after: SearchContinuation {
                    next_tier: tier,
                    after_sort_key: sort_key,
                    returned_before: returned_before.saturating_add(index).saturating_add(1),
                },
            })
            .collect(),
        posting_candidates,
        result_count,
        next_cursor,
    }
}

pub(crate) fn search_evidence(tier: u8) -> SearchMatchEvidence {
    match tier {
        0 => SearchMatchEvidence::ExactCoordinate,
        1 => SearchMatchEvidence::ExactName,
        2 => SearchMatchEvidence::ExactAlias,
        3 => SearchMatchEvidence::PrefixCoordinate,
        4 => SearchMatchEvidence::PrefixName,
        5 => SearchMatchEvidence::PrefixAlias,
        6 => SearchMatchEvidence::Substring,
        7 => SearchMatchEvidence::AliasTerms,
        8 => SearchMatchEvidence::KeywordTerms,
        9 => SearchMatchEvidence::AdvisoryTerms,
        10 => SearchMatchEvidence::DescriptionTerms,
        11 => SearchMatchEvidence::GenericTerms,
        12 => SearchMatchEvidence::FuzzyName,
        13 => SearchMatchEvidence::FuzzyAlias,
        _ => SearchMatchEvidence::AllDocuments,
    }
}

fn exact_term_query(field: Field, value: &str) -> Box<dyn Query> {
    Box::new(ConstScoreQuery::new(
        Box::new(TermQuery::new(
            Term::from_field_text(field, value),
            IndexRecordOption::Basic,
        )),
        1.0,
    ))
}

fn prefix_query(field: Field, value: &str) -> Box<dyn Query> {
    // Tantivy 0.26 removed PrefixQuery. RegexQuery walks the term dictionary
    // through an FST automaton, so this remains prefix/postings based instead
    // of scanning every document. The literal prefix is escaped because
    // coordinates and names may contain regex metacharacters.
    let pattern = regex_prefix_pattern(value);
    let prefix: Box<dyn Query> = match RegexQuery::from_pattern(&pattern, field) {
        Ok(query) => Box::new(query),
        Err(_) => Box::new(EmptyQuery),
    };
    Box::new(ConstScoreQuery::new(prefix, 1.0))
}

fn fuzzy_name_query(field: Field, value: &str) -> Box<dyn Query> {
    let characters = value.chars().count();
    if !(3..=64).contains(&characters) {
        return Box::new(EmptyQuery);
    }
    Box::new(ConstScoreQuery::new(
        Box::new(FuzzyTermQuery::new(
            Term::from_field_text(field, value),
            1,
            true,
        )),
        1.0,
    ))
}

fn searchable_content_query(field: Field, value: &str) -> Box<dyn Query> {
    let normalized = normalize(value);
    let tokens = normalized
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if tokens.is_empty() || tokens.iter().any(|token| token.chars().count() > 40) {
        return Box::new(EmptyQuery);
    }
    let clauses = tokens
        .into_iter()
        .map(|token| {
            (
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(field, token),
                    IndexRecordOption::Basic,
                )) as Box<dyn Query>,
            )
        })
        .collect();
    Box::new(ConstScoreQuery::new(
        Box::new(BooleanQuery::new(clauses)),
        1.0,
    ))
}

fn regex_prefix_pattern(value: &str) -> String {
    let mut pattern = String::with_capacity(value.len().saturating_mul(2) + 2);
    for character in value.chars() {
        if matches!(
            character,
            '\\' | '.' | '^' | '$' | '|' | '?' | '*' | '+' | '(' | ')' | '[' | ']' | '{' | '}'
        ) {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push_str(".*");
    pattern
}

fn substring_query(unigram: Field, bigram: Field, trigram: Field, query: &str) -> Box<dyn Query> {
    let characters = query.chars().collect::<Vec<_>>();
    let (field, width) = match characters.len() {
        0 => return Box::new(EmptyQuery),
        1 => (unigram, 1),
        2 => (bigram, 2),
        _ => (trigram, 3),
    };
    let terms = characters
        .windows(width)
        .map(|window| Term::from_field_text(field, &gram_token(window)))
        .collect::<Vec<_>>();
    if width == 1 || terms.len() == 1 {
        Box::new(TermQuery::new(terms[0].clone(), IndexRecordOption::Basic))
    } else {
        Box::new(PhraseQuery::new(terms))
    }
}

fn combined_gram_stream<'a, const WIDTH: usize>(
    fields: impl IntoIterator<Item = &'a str>,
) -> String {
    let mut stream = String::new();
    for field in fields {
        let mut window = ['\0'; WIDTH];
        let mut seen = 0_usize;
        let mut field_emitted = false;
        for character in field.chars() {
            window.rotate_left(1);
            window[WIDTH - 1] = character;
            if seen < WIDTH {
                seen += 1;
            }
            if seen >= WIDTH {
                if !field_emitted {
                    if !stream.is_empty() {
                        stream.push(' ');
                        stream.push_str(BOUNDARY_TOKEN);
                    }
                    field_emitted = true;
                }
                if !stream.is_empty() {
                    stream.push(' ');
                }
                append_gram_token(&mut stream, &window);
            }
        }
    }
    stream
}

fn gram_tokens(value: &str, width: usize) -> Vec<String> {
    value
        .chars()
        .collect::<Vec<_>>()
        .windows(width)
        .map(gram_token)
        .collect()
}

fn gram_token(characters: &[char]) -> String {
    let mut token = String::with_capacity(1 + characters.len() * 6);
    append_gram_token(&mut token, characters);
    token
}

fn append_gram_token(token: &mut String, characters: &[char]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    token.push('g');
    for character in characters {
        let code_point = u32::from(*character);
        for shift in [20, 16, 12, 8, 4, 0] {
            token.push(char::from(HEX[((code_point >> shift) & 0x0f) as usize]));
        }
    }
}

fn normalize(value: &str) -> String {
    value.chars().flat_map(char::to_lowercase).collect()
}

fn qualified_lineage(coordinate: &ProductPackageCoordinate) -> Result<String, String> {
    if coordinate.package_type().registry().is_none() {
        return Ok(coordinate.namespace().map_or_else(
            || coordinate.name().to_owned(),
            |namespace| format!("{namespace}:{}", coordinate.name()),
        ));
    }
    backend_engine::registry::admit_registry_coordinate(coordinate)
        .map(|coordinate| coordinate.qualified_name().as_str().to_owned())
        .map_err(|error| format!("invalid package coordinate in search projection: {error:?}"))
}

fn lineage_search_name(ecosystem: RegistryEcosystem, lineage: &str) -> &str {
    match ecosystem {
        RegistryEcosystem::Npm | RegistryEcosystem::Golang => {
            lineage.rsplit('/').next().unwrap_or(lineage)
        }
        RegistryEcosystem::Maven => lineage.rsplit(':').next().unwrap_or(lineage),
        RegistryEcosystem::Cpp => lineage.rsplit([':', '/']).next().unwrap_or(lineage),
        RegistryEcosystem::Cargo | RegistryEcosystem::Pypi | RegistryEcosystem::Nuget => lineage,
    }
}

/// Keeps release matches in evidence order, then favors the newest release
/// when one lineage has several hits at the same evidence tier. This only
/// changes the presentation order of the bounded release facet; group cursors
/// and their stable lineage keys remain unchanged.
fn order_grouped_release_hits(hits: Vec<SearchHit<DiscoverySearchKey>>) -> Vec<DiscoverySearchKey> {
    let Some(ecosystem) = hits.first().map(|hit| hit.key.source.ecosystem()) else {
        return Vec::new();
    };
    let versions = hits
        .iter()
        .map(|hit| {
            super::product_state::normalized_version_key(ecosystem, hit.key.coordinate.version())
        })
        .collect::<Option<Vec<_>>>();
    let Some(versions) = versions else {
        // Preserve the index's existing deterministic order if the ecosystem
        // version grammar cannot prove an ordering for every displayed hit.
        return hits.into_iter().map(|hit| hit.key).collect();
    };

    let mut ranked = hits.into_iter().zip(versions).collect::<Vec<_>>();
    ranked.sort_by(|(left, left_version), (right, right_version)| {
        left.evidence
            .rank()
            .cmp(&right.evidence.rank())
            .then_with(|| right_version.cmp(left_version))
            .then_with(|| {
                discovery_sort_key(&left.key.source, left.key.coordinate.as_str()).cmp(
                    &discovery_sort_key(&right.key.source, right.key.coordinate.as_str()),
                )
            })
    });
    ranked.into_iter().map(|(hit, _)| hit.key).collect()
}

fn discovery_sort_key(source: &DiscoverySearchSource, coordinate: &str) -> String {
    let source_id = source.id();
    format!(
        "{}\u{1f}{}:{}:{}\u{1f}{}:{}",
        normalize(coordinate),
        source.ecosystem().package_type().as_str(),
        match source {
            DiscoverySearchSource::Registry(_) => "registry",
            DiscoverySearchSource::Forge(_) => "forge",
        },
        hex(&source_id),
        coordinate.len(),
        coordinate,
    )
}

fn local_sort_key(id: RowId, coordinate: &str) -> String {
    format!("{}\u{1f}{}", normalize(coordinate), id.stable_key())
}

fn hex(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_engine::registry::{
        DiscoveryAdvisory, DiscoveryBatch, DiscoveryCompleteness, DiscoveryCursor, DiscoveryFact,
        DiscoveryObservedAt, DiscoveryStanding, RegistryEcosystem, RegistryEndpoint,
    };
    use std::collections::BTreeSet;

    fn source(ecosystem: RegistryEcosystem, endpoint: &str) -> DiscoverySourceIdentity {
        let endpoint = RegistryEndpoint::new(ecosystem, endpoint).expect("endpoint");
        DiscoverySourceIdentity::from_endpoint(&endpoint)
    }

    fn key(source: DiscoverySourceIdentity, coordinate: &str) -> DiscoverySearchKey {
        let coordinate = ProductPackageCoordinate::parse(coordinate).expect("coordinate");
        let lineage = qualified_lineage(&coordinate).expect("qualified lineage");
        DiscoverySearchKey {
            source: DiscoverySearchSource::Registry(source),
            coordinate,
            lineage,
            manifest_path: None,
        }
    }

    fn hit_keys<K>(hits: Vec<SearchHit<K>>) -> Vec<K> {
        hits.into_iter().map(|hit| hit.key).collect()
    }

    fn oracle<K: Clone>(
        entries: &[(K, String, String, String)],
        query: &str,
        limit: usize,
    ) -> Vec<K> {
        let needle = normalize(query);
        let mut matches = entries
            .iter()
            .filter_map(|(key, coordinate, name, sort_key)| {
                let coordinate = normalize(coordinate);
                let name = normalize(name);
                let rank = if needle.is_empty() {
                    5
                } else if coordinate == needle {
                    0
                } else if name == needle {
                    1
                } else if coordinate.starts_with(&needle) {
                    2
                } else if name.starts_with(&needle) {
                    3
                } else if coordinate.contains(&needle) || name.contains(&needle) {
                    4
                } else if needle.chars().count() >= 3 && edit_distance_at_most_one(&name, &needle) {
                    5
                } else {
                    return None;
                };
                Some((rank, sort_key, key))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        matches
            .into_iter()
            .take(limit)
            .map(|(_, _, key)| key.clone())
            .collect()
    }

    fn edit_distance_at_most_one(left: &str, right: &str) -> bool {
        let left = left.chars().collect::<Vec<_>>();
        let right = right.chars().collect::<Vec<_>>();
        if left.len().abs_diff(right.len()) > 1 {
            return false;
        }
        let mut left_index = 0;
        let mut right_index = 0;
        let mut edits = 0;
        while left_index < left.len() && right_index < right.len() {
            if left[left_index] == right[right_index] {
                left_index += 1;
                right_index += 1;
                continue;
            }
            edits += 1;
            if edits > 1 {
                return false;
            }
            if left.len() == right.len() {
                if left_index + 1 < left.len()
                    && right_index + 1 < right.len()
                    && left[left_index] == right[right_index + 1]
                    && left[left_index + 1] == right[right_index]
                {
                    left_index += 2;
                    right_index += 2;
                } else {
                    left_index += 1;
                    right_index += 1;
                }
            } else if left.len() > right.len() {
                left_index += 1;
            } else {
                right_index += 1;
            }
        }
        if left_index < left.len() || right_index < right.len() {
            edits += 1;
        }
        edits <= 1
    }

    fn facet_list_matches(facet: &DiscoveryFacet<Vec<String>>, query: &str) -> bool {
        match facet {
            DiscoveryFacet::Known(values) => {
                values_contain_terms(values.iter().map(String::as_str), query)
            }
            DiscoveryFacet::Absent | DiscoveryFacet::Unknown => false,
        }
    }

    fn facet_text_matches(facet: &DiscoveryFacet<String>, query: &str) -> bool {
        match facet {
            DiscoveryFacet::Known(value) => {
                values_contain_terms(std::iter::once(value.as_str()), query)
            }
            DiscoveryFacet::Absent | DiscoveryFacet::Unknown => false,
        }
    }

    fn values_contain_terms<'a>(values: impl Iterator<Item = &'a str>, query: &str) -> bool {
        let terms = query
            .split(|character: char| !character.is_alphanumeric())
            .filter(|term| !term.is_empty())
            .map(normalize)
            .collect::<Vec<_>>();
        if terms.is_empty() {
            return false;
        }
        let words = values
            .flat_map(|value| {
                value
                    .split(|character: char| !character.is_alphanumeric())
                    .filter(|word| !word.is_empty())
                    .map(normalize)
                    .collect::<Vec<_>>()
            })
            .collect::<BTreeSet<_>>();
        terms.iter().all(|term| words.contains(term))
    }

    fn typed_oracle(
        entries: &[(DiscoverySearchKey, DiscoveryMetadata)],
        query: &str,
        ecosystem: Option<RegistryEcosystem>,
        limit: usize,
    ) -> Vec<DiscoverySearchKey> {
        let needle = normalize(query);
        let mut matches = entries
            .iter()
            .filter_map(|(key, metadata)| {
                if ecosystem.is_some_and(|expected| key.source.ecosystem() != expected) {
                    return None;
                }
                let coordinate = normalize(key.coordinate.as_str());
                let name = normalize(lineage_search_name(key.source.ecosystem(), &key.lineage));
                let aliases = match &metadata.aliases {
                    DiscoveryFacet::Known(values) => {
                        values.iter().map(|value| normalize(value)).collect()
                    }
                    DiscoveryFacet::Absent | DiscoveryFacet::Unknown => Vec::new(),
                };
                let advisory_values = match &metadata.advisories {
                    DiscoveryFacet::Known(advisories) => advisories
                        .iter()
                        .flat_map(|advisory| {
                            std::iter::once(advisory.id.as_str())
                                .chain(advisory.aliases.iter().map(String::as_str))
                                .chain(known_list_text(&advisory.fixed_in))
                                .chain(known_text(&advisory.summary))
                                .chain(known_text(&advisory.severity))
                        })
                        .collect::<Vec<_>>(),
                    DiscoveryFacet::Absent | DiscoveryFacet::Unknown => Vec::new(),
                };
                let rank = if needle.is_empty() {
                    15
                } else if coordinate == needle {
                    0
                } else if name == needle {
                    1
                } else if aliases.iter().any(|alias| alias == &needle) {
                    2
                } else if coordinate.starts_with(&needle) {
                    3
                } else if name.starts_with(&needle) {
                    4
                } else if aliases.iter().any(|alias| alias.starts_with(&needle)) {
                    5
                } else if coordinate.contains(&needle) || name.contains(&needle) {
                    6
                } else if facet_list_matches(&metadata.aliases, query) {
                    7
                } else if facet_list_matches(&metadata.keywords, query) {
                    8
                } else if values_contain_terms(advisory_values.into_iter(), query) {
                    9
                } else if facet_text_matches(&metadata.description, query) {
                    10
                } else if needle.chars().count() >= 3 && edit_distance_at_most_one(&name, &needle) {
                    12
                } else if needle.chars().count() >= 3
                    && aliases
                        .iter()
                        .any(|alias| edit_distance_at_most_one(alias, &needle))
                {
                    13
                } else {
                    return None;
                };
                Some((
                    rank,
                    discovery_sort_key(&key.source, key.coordinate.as_str()),
                    key,
                ))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        matches
            .into_iter()
            .take(limit)
            .map(|(_, _, key)| key.clone())
            .collect()
    }

    fn typed_oracle_rank(
        key: &DiscoverySearchKey,
        metadata: &DiscoveryMetadata,
        query: &str,
    ) -> Option<u8> {
        let needle = normalize(query);
        if needle.is_empty() {
            return Some(u8::MAX);
        }
        let coordinate = normalize(key.coordinate.as_str());
        let name = normalize(lineage_search_name(key.source.ecosystem(), &key.lineage));
        let aliases = match &metadata.aliases {
            DiscoveryFacet::Known(values) => values.iter().map(|value| normalize(value)).collect(),
            DiscoveryFacet::Absent | DiscoveryFacet::Unknown => Vec::new(),
        };
        let advisory_values = match &metadata.advisories {
            DiscoveryFacet::Known(advisories) => advisories
                .iter()
                .flat_map(|advisory| {
                    std::iter::once(advisory.id.as_str())
                        .chain(advisory.aliases.iter().map(String::as_str))
                        .chain(known_list_text(&advisory.fixed_in))
                        .chain(known_text(&advisory.summary))
                        .chain(known_text(&advisory.severity))
                })
                .collect::<Vec<_>>(),
            DiscoveryFacet::Absent | DiscoveryFacet::Unknown => Vec::new(),
        };
        if coordinate == needle {
            Some(0)
        } else if name == needle {
            Some(1)
        } else if aliases.iter().any(|alias| alias == &needle) {
            Some(2)
        } else if coordinate.starts_with(&needle) {
            Some(3)
        } else if name.starts_with(&needle) {
            Some(4)
        } else if aliases.iter().any(|alias| alias.starts_with(&needle)) {
            Some(5)
        } else if coordinate.contains(&needle) || name.contains(&needle) {
            Some(6)
        } else if facet_list_matches(&metadata.aliases, query) {
            Some(7)
        } else if facet_list_matches(&metadata.keywords, query) {
            Some(8)
        } else if values_contain_terms(advisory_values.into_iter(), query) {
            Some(9)
        } else if facet_text_matches(&metadata.description, query) {
            Some(10)
        } else if needle.chars().count() >= 3 && edit_distance_at_most_one(&name, &needle) {
            Some(12)
        } else if needle.chars().count() >= 3
            && aliases
                .iter()
                .any(|alias| edit_distance_at_most_one(alias, &needle))
        {
            Some(13)
        } else {
            None
        }
    }

    fn typed_group_oracle(
        entries: &[(DiscoverySearchKey, DiscoveryMetadata)],
        query: &str,
        ecosystem: Option<RegistryEcosystem>,
        limit: usize,
    ) -> Vec<(LineageKey, u8, Vec<DiscoverySearchKey>, bool)> {
        let mut grouped =
            BTreeMap::<LineageKey, Vec<(DiscoverySearchKey, DiscoveryMetadata)>>::new();
        for (key, metadata) in entries {
            if ecosystem.is_some_and(|expected| key.source.ecosystem() != expected) {
                continue;
            }
            let group = discovery_lineage_key(&key.source, key.source.ecosystem(), &key.lineage);
            grouped
                .entry(group)
                .or_default()
                .push((key.clone(), metadata.clone()));
        }
        let mut groups = grouped
            .into_iter()
            .filter_map(|(key, mut releases)| {
                let evidence = typed_group_oracle_rank(&key, &releases, query)?;
                let mut matching = releases
                    .iter()
                    .filter_map(|(release, metadata)| {
                        typed_oracle_rank(release, metadata, query)
                            .map(|rank| (rank, release.clone()))
                    })
                    .collect::<Vec<_>>();
                // When package-level terms match only after unioning metadata
                // across releases, retain a bounded representative page from
                // the whole lineage rather than silently dropping the group.
                if matching.is_empty() {
                    matching.extend(releases.drain(..).map(|(release, _)| (u8::MAX, release)));
                }
                if let Some(versions) = matching
                    .iter()
                    .map(|(_, release)| typed_oracle_semver(release.coordinate.version()))
                    .collect::<Option<Vec<_>>>()
                {
                    let mut ranked = matching.into_iter().zip(versions).collect::<Vec<_>>();
                    ranked.sort_by(
                        |((left_rank, left), left_version),
                         ((right_rank, right), right_version)| {
                            left_rank
                                .cmp(right_rank)
                                .then_with(|| right_version.cmp(left_version))
                                .then_with(|| {
                                    discovery_sort_key(&left.source, left.coordinate.as_str()).cmp(
                                        &discovery_sort_key(
                                            &right.source,
                                            right.coordinate.as_str(),
                                        ),
                                    )
                                })
                        },
                    );
                    matching = ranked
                        .into_iter()
                        .map(|((rank, release), _)| (rank, release))
                        .collect();
                } else {
                    matching.sort_by(|left, right| {
                        left.0.cmp(&right.0).then_with(|| {
                            discovery_sort_key(&left.1.source, left.1.coordinate.as_str()).cmp(
                                &discovery_sort_key(&right.1.source, right.1.coordinate.as_str()),
                            )
                        })
                    });
                }
                let more = matching.len() > MAX_RELEASES_PER_GROUP;
                let keys = matching
                    .into_iter()
                    .take(MAX_RELEASES_PER_GROUP)
                    .map(|(_, key)| key)
                    .collect();
                Some((key, evidence, keys, more))
            })
            .collect::<Vec<_>>();
        groups.sort_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then_with(|| lineage_sort_key(&left.0).cmp(&lineage_sort_key(&right.0)))
        });
        groups.into_iter().take(limit).collect()
    }

    /// Deliberately small, independent oracle for the numeric SemVer fixtures
    /// below. Production release ordering uses the shared ecosystem parser.
    fn typed_oracle_semver(version: &str) -> Option<Vec<u64>> {
        let version = version.strip_prefix('v').unwrap_or(version);
        if version.contains('-') || version.contains('+') {
            return None;
        }
        version
            .split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .ok()
    }

    fn typed_group_oracle_rank(
        key: &LineageKey,
        releases: &[(DiscoverySearchKey, DiscoveryMetadata)],
        query: &str,
    ) -> Option<u8> {
        let needle = normalize(query);
        if needle.is_empty() {
            return Some(u8::MAX);
        }
        let coordinates = releases
            .iter()
            .map(|(release, _)| normalize(release.coordinate.as_str()))
            .collect::<Vec<_>>();
        let name = normalize(lineage_search_name(key.ecosystem, &key.lineage));
        let mut aliases = Vec::new();
        let mut keywords = Vec::new();
        let mut descriptions = Vec::new();
        let mut advisories = Vec::new();
        for (_, metadata) in releases {
            if let DiscoveryFacet::Known(values) = &metadata.aliases {
                aliases.extend(values.iter().map(|value| normalize(value)));
            }
            if let DiscoveryFacet::Known(values) = &metadata.keywords {
                keywords.extend(values.iter().map(|value| value.as_str()));
            }
            if let DiscoveryFacet::Known(value) = &metadata.description {
                descriptions.push(value.as_str());
            }
            if let DiscoveryFacet::Known(values) = &metadata.advisories {
                for advisory in values {
                    advisories.push(advisory.id.as_str());
                    advisories.extend(advisory.aliases.iter().map(String::as_str));
                    advisories.extend(known_list_text(&advisory.fixed_in));
                    advisories.extend(known_text(&advisory.summary));
                    advisories.extend(known_text(&advisory.severity));
                }
            }
        }
        if coordinates.iter().any(|coordinate| coordinate == &needle) {
            Some(0)
        } else if name == needle {
            Some(1)
        } else if aliases.iter().any(|alias| alias == &needle) {
            Some(2)
        } else if coordinates
            .iter()
            .any(|coordinate| coordinate.starts_with(&needle))
        {
            Some(3)
        } else if name.starts_with(&needle) {
            Some(4)
        } else if aliases.iter().any(|alias| alias.starts_with(&needle)) {
            Some(5)
        } else if coordinates
            .iter()
            .any(|coordinate| coordinate.contains(&needle))
            || name.contains(&needle)
        {
            Some(6)
        } else if values_contain_terms(aliases.iter().map(String::as_str), query) {
            Some(7)
        } else if values_contain_terms(keywords.into_iter(), query) {
            Some(8)
        } else if values_contain_terms(advisories.into_iter(), query) {
            Some(9)
        } else if values_contain_terms(descriptions.into_iter(), query) {
            Some(10)
        } else if needle.chars().count() >= 3 && edit_distance_at_most_one(&name, &needle) {
            Some(12)
        } else if needle.chars().count() >= 3
            && aliases
                .iter()
                .any(|alias| edit_distance_at_most_one(alias, &needle))
        {
            Some(13)
        } else {
            None
        }
    }

    fn known_text(facet: &DiscoveryFacet<String>) -> Option<&str> {
        match facet {
            DiscoveryFacet::Known(value) if !value.is_empty() => Some(value.as_str()),
            DiscoveryFacet::Known(_) | DiscoveryFacet::Absent | DiscoveryFacet::Unknown => None,
        }
    }

    fn known_list_text<'a>(
        facet: &'a DiscoveryFacet<Vec<String>>,
    ) -> Box<dyn Iterator<Item = &'a str> + 'a> {
        match facet {
            DiscoveryFacet::Known(values) => Box::new(values.iter().map(String::as_str)),
            DiscoveryFacet::Absent | DiscoveryFacet::Unknown => Box::new(std::iter::empty()),
        }
    }

    fn discovery_entries(
        entries: &[DiscoverySearchKey],
    ) -> Vec<(DiscoverySearchKey, String, String, String)> {
        entries
            .iter()
            .cloned()
            .map(|key| {
                let coordinate = key.coordinate.as_str().to_owned();
                let name = lineage_search_name(key.source.ecosystem(), &key.lineage).to_owned();
                let sort_key = discovery_sort_key(&key.source, &coordinate);
                (key, coordinate, name, sort_key)
            })
            .collect()
    }

    fn build_discovery_index(
        entries: &[DiscoverySearchKey],
    ) -> TextSearchIndex<DiscoverySearchKey> {
        let mut index = TextSearchIndex::new().expect("index");
        for key in entries {
            let coordinate = key.coordinate.as_str();
            let lineage_key =
                discovery_lineage_key(&key.source, key.source.ecosystem(), &key.lineage);
            let group_key = lineage_sort_key(&lineage_key);
            let mut search_text = SearchDocumentText::default();
            search_text.lineage_group = Some(&group_key);
            index
                .add_document_with_search_text(
                    key.clone(),
                    coordinate,
                    lineage_search_name(key.source.ecosystem(), &key.lineage),
                    search_text,
                    key.source.ecosystem().as_str(),
                    discovery_sort_key(&key.source, coordinate),
                )
                .expect("add discovery document");
        }
        index.commit().expect("commit discovery index");
        index
    }

    fn build_discovery_search_projection(entries: &[DiscoverySearchKey]) -> DiscoverySearchIndex {
        let metadata = entries
            .iter()
            .cloned()
            .map(|key| (key, DiscoveryMetadata::default()))
            .collect::<Vec<_>>();
        build_discovery_projection_with_metadata(&metadata)
    }

    fn build_discovery_projection_with_metadata(
        entries: &[(DiscoverySearchKey, DiscoveryMetadata)],
    ) -> DiscoverySearchIndex {
        let mut inner = TextSearchIndex::new().expect("release index");
        let mut lineages = LineageSearchIndex::new().expect("lineage index");
        let mut changes = Vec::new();
        for (key, metadata) in entries {
            let coordinate = key.coordinate.as_str();
            let lineage_key =
                discovery_lineage_key(&key.source, key.source.ecosystem(), &key.lineage);
            let group_key = lineage_sort_key(&lineage_key);
            let mut search_text = discovery_search_text(metadata);
            search_text.lineage_group = Some(&group_key);
            inner
                .add_document_with_search_text(
                    key.clone(),
                    coordinate,
                    lineage_search_name(key.source.ecosystem(), &key.lineage),
                    search_text,
                    key.source.ecosystem().as_str(),
                    discovery_sort_key(&key.source, coordinate),
                )
                .expect("add release document");
            changes.push((
                discovery_sort_key(&key.source, coordinate),
                lineage_key,
                lineage_release_document(key.clone(), metadata, None, &[]),
            ));
        }
        inner.commit().expect("commit release index");
        lineages
            .apply_batch(changes)
            .expect("add lineage documents");
        lineages.commit().expect("commit lineage index");
        let registry_fingerprint = lineages.structural_fingerprint();
        let snapshot_root = combined_search_snapshot_root(registry_fingerprint, [0; 32]);
        let mut source_pins = TextSearchIndex::new().expect("source-pin index");
        source_pins.commit().expect("commit source-pin index");
        DiscoverySearchIndex {
            inner,
            lineages,
            source_pins,
            store_revision: 0,
            revision: u64::from_le_bytes(snapshot_root[..8].try_into().expect("fixed root")),
            snapshot_root,
            forge_fingerprint: [0; 32],
            forge_release_fingerprint: [0; 32],
            source_pin_fingerprint: [0; 32],
            forge_documents: BTreeMap::new(),
            source_pin_documents: BTreeMap::new(),
        }
    }

    fn discovery_batch(
        source: DiscoverySourceIdentity,
        previous_cursor: &str,
        next_cursor: &str,
        observed_at: u64,
        facts: &[(&str, DiscoveryStanding, &str, u8)],
    ) -> DiscoveryBatch {
        let previous_cursor =
            DiscoveryCursor::new(previous_cursor.as_bytes().to_vec()).expect("previous cursor");
        let next_cursor =
            DiscoveryCursor::new(next_cursor.as_bytes().to_vec()).expect("next cursor");
        DiscoveryBatch {
            source,
            previous_cursor,
            next_cursor: next_cursor.clone(),
            source_high_watermark: next_cursor,
            caught_up: true,
            observed_at: DiscoveryObservedAt::from_unix_millis(observed_at),
            completeness: DiscoveryCompleteness::CompleteThroughCursor,
            facts: facts
                .iter()
                .map(|(coordinate, standing, event_time, proof)| DiscoveryFact {
                    source,
                    coordinate: ProductPackageCoordinate::parse(*coordinate).expect("coordinate"),
                    standing: *standing,
                    observed_at: DiscoveryObservedAt::from_unix_millis(observed_at),
                    source_event_time: Some((*event_time).to_owned()),
                    proof: [*proof; 32],
                    metadata: DiscoveryMetadata::default(),
                })
                .collect(),
            package_retractions: Vec::new(),
        }
    }

    fn temp_journal_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "backend-discovery-search-{}-{}.journal",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    fn forge_document(
        source: &str,
        package: &str,
        aliases: DiscoveryFacet<Vec<String>>,
        description: DiscoveryFacet<String>,
        keywords: DiscoveryFacet<Vec<String>>,
        license: DiscoveryFacet<String>,
        readme: DiscoveryFacet<String>,
        generic_terms: Vec<String>,
    ) -> ForgeSearchDocument {
        ForgeSearchDocument::from_pinned_facts(
            ForgeCoordinate::parse(source).expect("forge source coordinate"),
            ProductPackageCoordinate::parse(package).expect("package coordinate"),
            aliases,
            description,
            keywords,
            license,
            readme,
            generic_terms,
        )
        .expect("forge search document")
    }

    fn percentile_micros(samples: &mut [std::time::Duration], percentile: usize) -> u128 {
        samples.sort_unstable();
        let index = samples.len().saturating_sub(1).saturating_mul(percentile) / 100;
        samples[index].as_micros()
    }

    #[cfg(unix)]
    fn resident_set_kib() -> Option<u64> {
        let pid = std::process::id().to_string();
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", pid.as_str()])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()?.trim().parse().ok()
    }

    #[cfg(not(unix))]
    fn resident_set_kib() -> Option<u64> {
        None
    }

    #[test]
    fn forge_source_pin_search_indexes_only_its_bounded_recorded_readme() {
        let coordinate = ForgeCoordinate::parse("https://github.com/acme/readme-pin@branch:main")
            .expect("source pin coordinate");
        let commit =
            backend_engine::ForgeObjectId::parse("0123456789012345678901234567890123456789")
                .expect("commit");
        let tree = backend_engine::ForgeObjectId::parse("1123456789012345678901234567890123456789")
            .expect("tree");
        let resolution = backend_engine::ForgeResolution::for_coordinate(
            &coordinate,
            commit,
            Some(tree),
            "main-readme-validator",
        )
        .expect("bound resolution");
        let mut metadata = backend_engine::ForgeRepositoryMetadata::unavailable(
            "acme",
            backend_engine::ForgeUnavailableReason::AuthorityOmitted,
        );
        metadata.description = ForgeFact::Recorded(
            backend_library::ProductText::new("Short repository summary").expect("description"),
        );
        metadata.readme = ForgeFact::Recorded(
            backend_library::ProductText::new("quartzveilpinreadmeonly").expect("bounded README"),
        );
        let record = ForgeSearchRecord {
            coordinate: coordinate.clone(),
            resolution,
            archive: [7; 32],
            metadata,
            manifests: vec![ForgePackageManifest {
                path: "Cargo.toml".into(),
                ecosystem: RegistryEcosystem::Cargo,
                name: ForgeFact::Recorded(
                    backend_library::ProductText::new("readme-pin-demo").expect("package name"),
                ),
                version: ForgeFact::Unavailable(
                    backend_engine::ForgeUnavailableReason::AuthorityOmitted,
                ),
                dependencies: DependencyFacts::Unknown(backend_library::ProductText::from_static(
                    "source-only fixture",
                )),
            }]
            .into_boxed_slice(),
        };
        assert!(forge_manifest_package_coordinate(&record.manifests[0]).is_none());
        assert!(matches!(
            &record.metadata.description,
            ForgeFact::Recorded(value)
                if !value.as_str().contains("quartzveilpinreadmeonly")
        ));

        let documents = ForgeSourcePinSearchDocument::from_search_record(&record)
            .expect("source-pin projection");
        assert_eq!(documents.len(), 1);
        assert_eq!(
            documents[0].readme,
            DiscoveryFacet::Known("quartzveilpinreadmeonly".to_owned())
        );
        let index = DiscoverySearchIndex::open_forge_only_with_source_pins(&[], &documents)
            .expect("forge-only source-pin index");
        let page = index
            .source_pin_page_after("quartzveilpinreadmeonly", 8, None)
            .expect("README source-pin search");
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].key, documents[0].key);

        let mut without_readme = record;
        without_readme.metadata.readme =
            ForgeFact::Unavailable(backend_engine::ForgeUnavailableReason::AuthorityOmitted);
        let unknown_readme = ForgeSourcePinSearchDocument::from_search_record(&without_readme)
            .expect("unavailable README projection");
        assert_eq!(unknown_readme[0].readme, DiscoveryFacet::Unknown);
        let mut oversized = unknown_readme[0].clone();
        oversized.readme = DiscoveryFacet::Known("x".repeat(256 * 1024 + 1));
        assert!(
            oversized.admit().is_err(),
            "oversized README must be rejected"
        );
    }

    #[test]
    fn discovery_search_matches_independent_ranked_oracle_and_keeps_source_claims_distinct() {
        let cargo = source(RegistryEcosystem::Cargo, "https://index.crates.io");
        let cargo_mirror = source(RegistryEcosystem::Cargo, "https://mirror.example.test");
        let npm = source(RegistryEcosystem::Npm, "https://registry.npmjs.org");
        let entries = vec![
            key(cargo, "pkg:cargo/serde@1.0.0"),
            key(cargo, "pkg:cargo/serde@2.0.0"),
            key(cargo, "pkg:cargo/serde-json@1.0.0"),
            key(cargo_mirror, "pkg:cargo/serde@1.0.0"),
            key(npm, "pkg:npm/serde@2.0.0"),
            key(cargo, "pkg:cargo/other@1.0.0"),
        ];
        let searchable = discovery_entries(&entries);
        let index = build_discovery_index(&entries);
        for query in [
            "pkg:cargo/serde@1.0.0",
            "serde",
            "SER",
            "rde",
            "sered",
            "missing",
            "",
        ] {
            for limit in [0, 1, 2, 4, 16] {
                let actual = hit_keys(index.page(query, limit).expect("search page").hits);
                assert_eq!(actual, oracle(&searchable, query, limit), "{query} {limit}");
            }
        }
        let exact = hit_keys(index.page("pkg:cargo/serde@1.0.0", 8).expect("exact").hits);
        assert_eq!(exact.len(), 2, "one coordinate claimed by two sources");
        assert_ne!(exact[0].source, exact[1].source);

        let typo = hit_keys(index.page("sered", 8).expect("one-edit typo").hits);
        assert_eq!(typo, oracle(&searchable, "sered", 8));
        assert!(!typo.is_empty(), "adjacent transposition should find serde");

        let cargo_only = index
            .page_filtered("serde", 8, Some(RegistryEcosystem::Cargo.as_str()), None)
            .expect("Cargo facet");
        assert!(
            cargo_only
                .hits
                .iter()
                .all(|hit| { hit.key.source.ecosystem() == RegistryEcosystem::Cargo })
        );
        assert_eq!(cargo_only.hits.len(), 4);
        let pypi_only = index
            .page_filtered("serde", 8, Some(RegistryEcosystem::Pypi.as_str()), None)
            .expect("PyPI facet");
        assert!(pypi_only.hits.is_empty());

        let first_blank = index
            .page_with_ecosystem("", 2, Some(RegistryEcosystem::Cargo.as_str()), None)
            .expect("first blank filtered page");
        assert_eq!(first_blank.hits.len(), 2);
        assert!(matches!(
            first_blank.result_count,
            SearchResultCount::AtLeast(3)
        ));
        let expected_cargo_documents = entries
            .iter()
            .filter(|key| key.source.ecosystem() == RegistryEcosystem::Cargo)
            .count();
        assert_eq!(expected_cargo_documents, 5);
        let second_blank = index
            .page_with_ecosystem(
                "",
                2,
                Some(RegistryEcosystem::Cargo.as_str()),
                first_blank.next_cursor.as_ref(),
            )
            .expect("second blank filtered page");
        assert_eq!(second_blank.hits.len(), 2);
        assert_eq!(
            second_blank.result_count,
            SearchResultCount::AtLeast(expected_cargo_documents)
        );
        let third_blank = index
            .page_with_ecosystem(
                "",
                2,
                Some(RegistryEcosystem::Cargo.as_str()),
                second_blank.next_cursor.as_ref(),
            )
            .expect("final blank filtered page");
        assert_eq!(third_blank.hits.len(), 1);
        assert_eq!(
            third_blank.result_count,
            SearchResultCount::Exact(expected_cargo_documents)
        );
        assert!(third_blank.next_cursor.is_none());
        assert!(first_blank.hits.iter().all(|hit| {
            hit.evidence == SearchMatchEvidence::AllDocuments
                && hit.key.source.ecosystem() == RegistryEcosystem::Cargo
        }));

        let grouped_index = build_discovery_search_projection(&entries);
        let grouped = grouped_index
            .search_groups(
                DiscoverySearchRequest {
                    text: "serde",
                    ecosystem: Some(RegistryEcosystem::Cargo),
                },
                8,
            )
            .expect("grouped Cargo page");
        assert_eq!(grouped.groups.len(), 3);
        let serde_group = grouped
            .groups
            .iter()
            .find(|group| {
                group.source == DiscoverySearchSource::Registry(cargo) && group.lineage == "serde"
            })
            .expect("Cargo serde lineage");
        assert_eq!(serde_group.matched_releases.len(), 2);
        assert_eq!(serde_group.ecosystem, RegistryEcosystem::Cargo);
    }

    #[test]
    fn lineage_search_matches_asymmetric_source_and_release_facet_oracle() {
        let cargo = source(RegistryEcosystem::Cargo, "https://index.crates.io");
        let mirror = source(RegistryEcosystem::Cargo, "https://mirror.example.test");
        let npm = source(RegistryEcosystem::Npm, "https://registry.npmjs.org");
        let entries = vec![
            (
                key(cargo, "pkg:cargo/serde@1.0.0"),
                DiscoveryMetadata {
                    aliases: DiscoveryFacet::Known(vec!["serde-json".to_owned()]),
                    keywords: DiscoveryFacet::Known(vec!["serialization".to_owned()]),
                    description: DiscoveryFacet::Known("typed serialization toolkit".to_owned()),
                    ..DiscoveryMetadata::default()
                },
            ),
            (
                key(cargo, "pkg:cargo/serde@2.0.0"),
                DiscoveryMetadata {
                    aliases: DiscoveryFacet::Absent,
                    description: DiscoveryFacet::Known("typed serialization library".to_owned()),
                    advisories: DiscoveryFacet::Known(vec![DiscoveryAdvisory {
                        id: "GHSA-1234-abcd".to_owned(),
                        aliases: vec!["CVE-2026-4242".to_owned()],
                        summary: DiscoveryFacet::Known("unsafe parser".to_owned()),
                        severity: DiscoveryFacet::Known("high".to_owned()),
                        fixed_in: DiscoveryFacet::Known(vec!["3.0.0".to_owned()]),
                    }]),
                    ..DiscoveryMetadata::default()
                },
            ),
            (
                key(mirror, "pkg:cargo/serde@1.0.0"),
                DiscoveryMetadata {
                    aliases: DiscoveryFacet::Known(vec!["json adapter".to_owned()]),
                    description: DiscoveryFacet::Unknown,
                    ..DiscoveryMetadata::default()
                },
            ),
            (
                key(npm, "pkg:npm/@acme/serde@3.0.0"),
                DiscoveryMetadata {
                    aliases: DiscoveryFacet::Known(vec!["serde-js".to_owned()]),
                    description: DiscoveryFacet::Known("JavaScript serialization".to_owned()),
                    ..DiscoveryMetadata::default()
                },
            ),
            (
                key(npm, "pkg:npm/serde-tools@1.0.0"),
                DiscoveryMetadata {
                    keywords: DiscoveryFacet::Known(vec!["serde helper".to_owned()]),
                    ..DiscoveryMetadata::default()
                },
            ),
            (
                key(cargo, "pkg:cargo/fused@1.0.0"),
                DiscoveryMetadata {
                    aliases: DiscoveryFacet::Known(vec!["split".to_owned()]),
                    ..DiscoveryMetadata::default()
                },
            ),
            (
                key(cargo, "pkg:cargo/fused@2.0.0"),
                DiscoveryMetadata {
                    aliases: DiscoveryFacet::Known(vec!["alias".to_owned()]),
                    ..DiscoveryMetadata::default()
                },
            ),
        ];
        let index = build_discovery_projection_with_metadata(&entries);

        for (query, ecosystem) in [
            ("pkg:cargo/serde@1.0.0", None),
            ("serde", None),
            ("json", Some(RegistryEcosystem::Cargo)),
            ("GHSA-1234-abcd", None),
            (
                "typed serialization toolkit",
                Some(RegistryEcosystem::Cargo),
            ),
            ("sered", Some(RegistryEcosystem::Cargo)),
            ("", Some(RegistryEcosystem::Npm)),
            ("split alias", Some(RegistryEcosystem::Cargo)),
        ] {
            let expected = typed_group_oracle(&entries, query, ecosystem, 32);
            let actual = index
                .search_groups(
                    DiscoverySearchRequest {
                        text: query,
                        ecosystem,
                    },
                    32,
                )
                .expect("lineage search page");
            if query == "split alias" {
                assert_eq!(actual.groups.len(), 1);
                assert_eq!(
                    actual.groups[0].release_match_scope,
                    ReleaseMatchScope::LineageMetadataOnly
                );
                assert_eq!(actual.groups[0].matched_releases.len(), 2);
            } else if query == "GHSA-1234-abcd" {
                assert!(actual.groups.iter().all(|group| {
                    group.release_match_scope == ReleaseMatchScope::ReleaseMatches
                }));
            }
            let actual = actual
                .groups
                .into_iter()
                .map(|group| {
                    (
                        group.key,
                        group.evidence.rank(),
                        group.matched_releases,
                        group.more_releases,
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "query={query:?} ecosystem={ecosystem:?}");
        }
    }

    #[test]
    fn prepared_group_search_matches_oracle_on_cold_warm_and_continued_pages() {
        let cargo = source(RegistryEcosystem::Cargo, "https://index.crates.io");
        let mirror = source(RegistryEcosystem::Cargo, "https://mirror.example.test");
        let npm = source(RegistryEcosystem::Npm, "https://registry.npmjs.org");
        let entries = vec![
            (
                key(cargo, "pkg:cargo/serde@1.0.0"),
                DiscoveryMetadata {
                    aliases: DiscoveryFacet::Known(vec!["serde-json".to_owned()]),
                    ..DiscoveryMetadata::default()
                },
            ),
            (
                key(cargo, "pkg:cargo/serde@2.0.0"),
                DiscoveryMetadata {
                    keywords: DiscoveryFacet::Known(vec!["serialization".to_owned()]),
                    ..DiscoveryMetadata::default()
                },
            ),
            (
                key(mirror, "pkg:cargo/serde@1.0.0"),
                DiscoveryMetadata {
                    aliases: DiscoveryFacet::Known(vec!["serde-adapter".to_owned()]),
                    ..DiscoveryMetadata::default()
                },
            ),
            (
                key(npm, "pkg:npm/@acme/serde@3.0.0"),
                DiscoveryMetadata {
                    description: DiscoveryFacet::Known("JavaScript serialization".to_owned()),
                    ..DiscoveryMetadata::default()
                },
            ),
            (
                key(npm, "pkg:npm/serde-tools@1.0.0"),
                DiscoveryMetadata {
                    keywords: DiscoveryFacet::Known(vec!["serde helper".to_owned()]),
                    ..DiscoveryMetadata::default()
                },
            ),
        ];
        let index = build_discovery_projection_with_metadata(&entries);
        let request = || DiscoverySearchRequest {
            text: "serde",
            ecosystem: None,
        };
        let expected = typed_group_oracle(&entries, "serde", None, 32);
        let project = |groups: Vec<DiscoveryPackageSearchGroup>| {
            groups
                .into_iter()
                .map(|group| {
                    (
                        group.key,
                        group.evidence.rank(),
                        group.matched_releases,
                        group.more_releases,
                    )
                })
                .collect::<Vec<_>>()
        };

        // The first call exercises query preparation against the freshly
        // committed index; the next call exercises the same warm request.
        let cold = index
            .search_groups_after(request(), 1, None)
            .expect("cold grouped page");
        let warm = index
            .search_groups_after(request(), 1, None)
            .expect("warm grouped page");
        assert_eq!(project(warm.groups), project(cold.groups.clone()));

        let mut actual = project(cold.groups);
        let mut cursor = cold.next_cursor;
        while let Some(after) = cursor {
            let page = index
                .search_groups_after(request(), 1, Some(&after))
                .expect("continued grouped page");
            actual.extend(project(page.groups));
            cursor = page.next_cursor;
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn grouped_search_pages_past_many_releases_and_bounds_release_expansion() {
        let cargo = source(RegistryEcosystem::Cargo, "https://index.crates.io");
        let mut entries = (0..320)
            .map(|patch| key(cargo, &format!("pkg:cargo/widget@1.0.{patch}")))
            .collect::<Vec<_>>();
        entries.push(key(cargo, "pkg:cargo/widget-addon@1.0.0"));
        let index = build_discovery_search_projection(&entries);
        let request = DiscoverySearchRequest {
            text: "widget",
            ecosystem: Some(RegistryEcosystem::Cargo),
        };

        let first = index
            .search_groups_after(request, 1, None)
            .expect("first lineage page");
        assert_eq!(first.groups.len(), 1);
        assert_eq!(first.groups[0].lineage, "widget");
        assert_eq!(
            first.groups[0].matched_releases.len(),
            MAX_RELEASES_PER_GROUP
        );
        assert_eq!(
            first.groups[0]
                .matched_releases
                .iter()
                .map(|key| key.coordinate.version().to_owned())
                .collect::<Vec<_>>(),
            (304..320)
                .rev()
                .map(|patch| format!("1.0.{patch}"))
                .collect::<Vec<_>>(),
            "the bounded facet must start at the globally newest typed versions"
        );
        assert_eq!(
            first.facet_releases_examined, 17,
            "only the top 16 plus one lookahead release posting should be examined"
        );
        assert!(first.groups[0].more_releases);
        assert!(first.next_cursor.is_some());
        assert!(matches!(first.result_count, SearchResultCount::AtLeast(2)));

        let cursor = first.next_cursor.expect("continuation after widget");
        let encoded = serde_json::to_vec(&cursor).expect("serialize cursor");
        let cursor: DiscoverySearchCursor =
            serde_json::from_slice(&encoded).expect("deserialize cursor");
        cursor.admit().expect("admitted cursor");
        let second = index
            .search_groups_after(request, 1, Some(&cursor))
            .expect("next lineage page");
        assert_eq!(second.groups.len(), 1);
        assert_eq!(second.groups[0].lineage, "widget-addon");
        assert!(second.next_cursor.is_none());
        assert_eq!(second.result_count, SearchResultCount::Unknown);
    }

    #[test]
    fn grouped_search_finds_late_facet_terms_without_scanning_unrelated_lineages() {
        const RELEASES: usize = 129;
        const KEYWORDS_PER_RELEASE: usize = 128;
        const UNRELATED_LINEAGES: usize = 64;
        let cargo = source(RegistryEcosystem::Cargo, "https://index.crates.io");
        let query = format!("facetoverrun{:05}", RELEASES * KEYWORDS_PER_RELEASE - 1);
        let mut target_entries = (0..RELEASES)
            .map(|release| {
                let keywords = (0..KEYWORDS_PER_RELEASE)
                    .map(|ordinal| {
                        format!(
                            "facetoverrun{:05}",
                            release * KEYWORDS_PER_RELEASE + ordinal
                        )
                    })
                    .collect();
                (
                    key(cargo, &format!("pkg:cargo/widget@1.0.{release}")),
                    DiscoveryMetadata {
                        keywords: DiscoveryFacet::Known(keywords),
                        ..DiscoveryMetadata::default()
                    },
                )
            })
            .collect::<Vec<_>>();
        let raw_hits = target_entries
            .iter()
            .filter(|(_, metadata)| match &metadata.keywords {
                DiscoveryFacet::Known(values) => values.iter().any(|value| value == &query),
                DiscoveryFacet::Absent | DiscoveryFacet::Unknown => false,
            })
            .count();
        assert_eq!(raw_hits, 1, "independent raw-metadata oracle");

        let target_index = build_discovery_projection_with_metadata(&target_entries);
        let request = DiscoverySearchRequest {
            text: &query,
            ecosystem: Some(RegistryEcosystem::Cargo),
        };
        let target_page = target_index
            .search_groups(request, 8)
            .expect("late keyword lineage page");
        assert_eq!(target_page.groups.len(), raw_hits);
        assert_eq!(
            target_page.groups[0].evidence,
            SearchMatchEvidence::KeywordTerms
        );
        assert_eq!(target_page.groups[0].matched_releases.len(), 1);
        assert_eq!(
            target_page.groups[0].matched_releases[0]
                .coordinate
                .as_str(),
            "pkg:cargo/widget@1.0.128"
        );
        assert_eq!(target_page.index_documents_visited, 1);
        drop(target_index);

        for lineage in 0..UNRELATED_LINEAGES {
            let keywords = (0..KEYWORDS_PER_RELEASE)
                .map(|ordinal| format!("unrelated{lineage:03}term{ordinal:03}"))
                .collect();
            target_entries.push((
                key(cargo, &format!("pkg:cargo/unrelated-{lineage}@1.0.0")),
                DiscoveryMetadata {
                    keywords: DiscoveryFacet::Known(keywords),
                    ..DiscoveryMetadata::default()
                },
            ));
        }
        let unrelated_index = build_discovery_projection_with_metadata(&target_entries);
        let unrelated_page = unrelated_index
            .search_groups(request, 8)
            .expect("late keyword with unrelated lineages");
        assert_eq!(unrelated_page.groups.len(), raw_hits);
        assert_eq!(unrelated_page.groups[0].lineage, "widget");
        assert_eq!(
            unrelated_page.index_documents_visited,
            target_page.index_documents_visited
        );
        drop(unrelated_index);

        let mut removed_entries = target_entries
            .into_iter()
            .take(RELEASES)
            .collect::<Vec<_>>();
        let last = removed_entries.last_mut().expect("last target release");
        let DiscoveryFacet::Known(values) = &mut last.1.keywords else {
            panic!("fixture keywords must be known");
        };
        values.retain(|value| value != &query);
        let negative_raw_hits = removed_entries
            .iter()
            .filter(|(_, metadata)| match &metadata.keywords {
                DiscoveryFacet::Known(values) => values.iter().any(|value| value == &query),
                DiscoveryFacet::Absent | DiscoveryFacet::Unknown => false,
            })
            .count();
        assert_eq!(negative_raw_hits, 0, "independent removal oracle");
        let removed_index = build_discovery_projection_with_metadata(&removed_entries);
        let removed_page = removed_index
            .search_groups(request, 8)
            .expect("removed late keyword lineage page");
        assert!(removed_page.groups.is_empty());
        assert_eq!(removed_page.result_count, SearchResultCount::Exact(0));
    }

    #[test]
    fn mixed_case_coordinate_facets_normalize_on_add_query_and_remove() {
        let source_identity = source(RegistryEcosystem::Cargo, "https://index.crates.io");
        let source = DiscoverySearchSource::Registry(source_identity);
        let lineage = discovery_lineage_key(&source, RegistryEcosystem::Cargo, "widget");
        let identity = "mixed-case-coordinate-release".to_owned();
        let document = LineageReleaseDocument {
            coordinate: "PKG:CARGO/Widget@1.0.0".to_owned(),
            version: "1.0.0".to_owned(),
            order_sort_key: identity.clone(),
            aliases: BTreeSet::new(),
            alias_tokens: BTreeSet::new(),
            keywords: BTreeSet::new(),
            descriptions: BTreeSet::new(),
            advisories: BTreeSet::new(),
            generic: BTreeSet::new(),
            fingerprint: [7; 32],
        };
        let mut index = LineageSearchIndex::new().expect("lineage index");
        index
            .apply_batch([(identity.clone(), lineage.clone(), document)])
            .expect("add mixed-case coordinate");

        let query = "PKG:CARGO/WIDGET@1.0.";
        let (visited, hits) = index.prefix_facet_lineages(
            LineageFacetField::Coordinate,
            &normalize(query),
            &normalize(query),
            SearchMatchEvidence::PrefixCoordinate,
            Some(RegistryEcosystem::Cargo),
            None,
            2,
        );
        assert_eq!(visited, 1);
        assert_eq!(hits, [(lineage_sort_key(&lineage), lineage.clone())]);
        assert_eq!(
            index
                .exact_coordinate_lineages(
                    "PKG:CARGO/WIDGET@1.0.0",
                    Some(RegistryEcosystem::Cargo),
                    None,
                    2,
                )
                .map(|(_, key)| key.clone())
                .collect::<Vec<_>>(),
            [lineage.clone()]
        );

        index.remove_batch([identity]).expect("remove coordinate");
        assert!(
            index
                .exact_coordinate_lineages(
                    "PKG:CARGO/WIDGET@1.0.0",
                    Some(RegistryEcosystem::Cargo),
                    None,
                    2,
                )
                .next()
                .is_none()
        );
        assert!(
            index
                .prefix_facet_lineages(
                    LineageFacetField::Coordinate,
                    &normalize(query),
                    &normalize(query),
                    SearchMatchEvidence::PrefixCoordinate,
                    Some(RegistryEcosystem::Cargo),
                    None,
                    2,
                )
                .1
                .is_empty()
        );
    }

    #[test]
    fn grouped_search_keeps_one_lookahead_at_the_maximum_public_page_size() {
        let cargo = source(RegistryEcosystem::Cargo, "https://index.crates.io");
        let entries = (0..MAX_SEARCH_PAGE_SIZE + 1)
            .map(|index| key(cargo, &format!("pkg:cargo/widget-{index:03}@1.0.0")))
            .collect::<Vec<_>>();
        let index = build_discovery_search_projection(&entries);
        let request = DiscoverySearchRequest {
            text: "widget",
            ecosystem: Some(RegistryEcosystem::Cargo),
        };

        let first = index
            .search_groups_after(request, MAX_SEARCH_PAGE_SIZE, None)
            .expect("maximum-size first page");
        assert_eq!(first.groups.len(), MAX_SEARCH_PAGE_SIZE);
        assert!(first.next_cursor.is_some());
        assert!(matches!(
            first.result_count,
            SearchResultCount::AtLeast(value) if value >= MAX_SEARCH_PAGE_SIZE + 1
        ));

        let second = index
            .search_groups_after(request, MAX_SEARCH_PAGE_SIZE, first.next_cursor.as_ref())
            .expect("maximum-size continuation");
        assert_eq!(second.groups.len(), 1);
        assert!(second.next_cursor.is_none());
        assert_ne!(first.groups[0].key, second.groups[0].key);
    }

    #[test]
    #[ignore = "manual 16k-release exact-coordinate coverage diagnostic"]
    fn exact_coordinate_lookup_covers_releases_beyond_lineage_field_bound() {
        let cargo = source(RegistryEcosystem::Cargo, "https://index.crates.io");
        let entries = (0..MAX_LINEAGE_FACET_VALUES + 32)
            .map(|minor| key(cargo, &format!("pkg:cargo/widget@1.{minor}.0")))
            .collect::<Vec<_>>();
        let mut coordinates = entries
            .iter()
            .map(|entry| entry.coordinate.as_str().to_owned())
            .collect::<Vec<_>>();
        coordinates.sort();
        let target = coordinates[MAX_LINEAGE_FACET_VALUES + 8].clone();
        let index = build_discovery_search_projection(&entries);
        let page = index
            .search_groups(
                DiscoverySearchRequest {
                    text: &target,
                    ecosystem: Some(RegistryEcosystem::Cargo),
                },
                8,
            )
            .expect("exact PURL lookup beyond bounded lineage terms");
        assert_eq!(page.groups.len(), 1);
        assert_eq!(
            page.groups[0].evidence,
            SearchMatchEvidence::ExactCoordinate
        );
        assert_eq!(page.groups[0].matched_releases.len(), 1);
        assert_eq!(
            page.groups[0].matched_releases[0].coordinate.as_str(),
            target
        );
        assert_eq!(
            page.groups[0].release_match_scope,
            ReleaseMatchScope::ReleaseMatches
        );
    }

    #[test]
    #[ignore = "manual 100k/1m package release scale and deep keyset diagnostic"]
    fn lineage_projection_scale_diagnostic() {
        use tantivy::{Directory as _, HasLen as _};

        let release_count = std::env::var("DISCOVERY_SEARCH_SCALE_RELEASES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(100_000);
        assert!((100_000..=1_000_000).contains(&release_count));
        const RELEASES_PER_LINEAGE: usize = 10;
        let lineage_count = release_count.div_ceil(RELEASES_PER_LINEAGE);
        let source = source(RegistryEcosystem::Cargo, "https://index.crates.io");
        let rss_before = resident_set_kib();
        let build_started = std::time::Instant::now();
        let mut release_index = TextSearchIndex::new().expect("release index");
        let mut lineages = LineageSearchIndex::new().expect("lineage index");
        for start in (0..release_count).step_by(20_000) {
            let end = start.saturating_add(20_000).min(release_count);
            let mut changes = Vec::with_capacity(end - start);
            for ordinal in start..end {
                let package = ordinal / RELEASES_PER_LINEAGE;
                let version = ordinal % RELEASES_PER_LINEAGE;
                let coordinate_text = format!("pkg:cargo/pkg-{package:06}@1.0.{version:04}");
                let key = key(source, &coordinate_text);
                let lineage_key =
                    discovery_lineage_key(&key.source, RegistryEcosystem::Cargo, &key.lineage);
                let group_key = lineage_sort_key(&lineage_key);
                let mut search_text = SearchDocumentText::default();
                search_text.lineage_group = Some(&group_key);
                release_index
                    .add_document_with_search_text(
                        key.clone(),
                        &coordinate_text,
                        lineage_search_name(RegistryEcosystem::Cargo, &key.lineage),
                        search_text,
                        RegistryEcosystem::Cargo.as_str(),
                        discovery_sort_key(&key.source, &coordinate_text),
                    )
                    .expect("add release");
                changes.push((
                    discovery_sort_key(&key.source, &coordinate_text),
                    lineage_key,
                    lineage_release_document(key, &DiscoveryMetadata::default(), None, &[]),
                ));
            }
            lineages.apply_batch(changes).expect("apply lineages");
        }
        release_index.commit().expect("commit release index");
        lineages.commit().expect("commit lineage index");
        let registry_fingerprint = lineages.structural_fingerprint();
        let snapshot_root = combined_search_snapshot_root(registry_fingerprint, [0; 32]);
        let mut source_pins = TextSearchIndex::new().expect("source-pin index");
        source_pins.commit().expect("commit source-pin index");
        let search = DiscoverySearchIndex {
            inner: release_index,
            lineages,
            source_pins,
            store_revision: 0,
            revision: u64::from_le_bytes(snapshot_root[..8].try_into().expect("fixed root")),
            snapshot_root,
            forge_fingerprint: [0; 32],
            forge_release_fingerprint: [0; 32],
            source_pin_fingerprint: [0; 32],
            forge_documents: BTreeMap::new(),
            source_pin_documents: BTreeMap::new(),
        };
        let build_millis = build_started.elapsed().as_millis();
        let index_bytes = [
            &search.inner._index,
            &search.lineages.inner._index,
            &search.source_pins._index,
        ]
        .into_iter()
        .flat_map(|index| {
            index
                .directory()
                .list_managed_files()
                .into_iter()
                .filter_map(|path| index.directory().open_read(&path).ok())
                .map(|slice| u64::try_from(slice.len()).unwrap_or(u64::MAX))
                .collect::<Vec<_>>()
        })
        .fold(0_u64, u64::saturating_add);
        let (versioned_release_count, versioned_release_estimated_payload_bytes) =
            search.lineages.versioned_release_projection_stats();
        let versioned_release_estimated_payload_bytes_per_release = if versioned_release_count == 0
        {
            0
        } else {
            versioned_release_estimated_payload_bytes / versioned_release_count
        };
        let rss_after = resident_set_kib();

        let deep_lineage = format!("pkg-{:06}", lineage_count * 9 / 10);
        let deep_key = discovery_lineage_key(
            &DiscoverySearchSource::Registry(source),
            RegistryEcosystem::Cargo,
            &deep_lineage,
        );
        let request = DiscoverySearchRequest {
            text: "pkg-",
            ecosystem: Some(RegistryEcosystem::Cargo),
        };
        let cursor = DiscoverySearchCursor {
            snapshot_root,
            normalized_query: normalize(request.text),
            ecosystem: request.ecosystem,
            after: Some(DiscoverySearchPosition {
                tier: SearchMatchEvidence::PrefixName.rank(),
                key: deep_key,
            }),
        };
        let cursor_bytes = serde_json::to_vec(&cursor).expect("encode bounded keyset cursor");
        assert!(cursor_bytes.len() < 2048, "cursor grew with result prefix");
        let mut warm_samples = Vec::with_capacity(51);
        let mut last_page_counters = None;
        for _ in 0..51 {
            let started = std::time::Instant::now();
            let page = search
                .search_groups_after(request, 32, Some(&cursor))
                .expect("deep keyset page");
            warm_samples.push(started.elapsed());
            assert_eq!(page.groups.len(), 32);
            assert!(page.index_documents_visited <= 33);
            assert!(page.facet_releases_examined <= 32 * RELEASES_PER_LINEAGE);
            assert!(page.groups.iter().all(|group| {
                lineage_sort_key(&group.key)
                    > lineage_sort_key(&cursor.after.as_ref().expect("deep position").key)
            }));
            last_page_counters = Some((
                page.index_documents_visited,
                page.facet_releases_examined,
                page.facet_term_keys_scanned,
            ));
        }
        let p50_micros = percentile_micros(&mut warm_samples, 50);
        let p95_micros = percentile_micros(&mut warm_samples, 95);
        eprintln!(
            "discovery scale: releases={release_count} lineages={lineage_count} cold_projection_build_ms={build_millis} tantivy_index_bytes={index_bytes} versioned_release_estimated_logical_payload_bytes={versioned_release_estimated_payload_bytes} versioned_release_estimated_logical_payload_bytes_per_release={versioned_release_estimated_payload_bytes_per_release} versioned_release_count={versioned_release_count} rss_before_kib={rss_before:?} rss_after_kib={rss_after:?} deep_page_p50_us={p50_micros} deep_page_p95_us={p95_micros} page_counters=(tantivy_lineages_visited,release_posting_entries_examined,release_term_keys_scanned)={last_page_counters:?} cursor_bytes={}",
            cursor_bytes.len()
        );
    }

    #[test]
    fn grouped_cursor_skips_a_prior_lineage_matching_at_a_weaker_tier() {
        let cargo = source(RegistryEcosystem::Cargo, "https://index.crates.io");
        let entries = [
            ("pkg:cargo/alpha-widget@1.0.0", true, false),
            ("pkg:cargo/zeta@1.0.0", true, false),
            ("pkg:cargo/alpha-widget@2.0.0", false, true),
        ]
        .into_iter()
        .map(|(coordinate_text, alias, keyword)| {
            let key = key(cargo, coordinate_text);
            let metadata = DiscoveryMetadata {
                aliases: if alias {
                    DiscoveryFacet::Known(vec!["serde".to_owned()])
                } else {
                    DiscoveryFacet::Unknown
                },
                keywords: if keyword {
                    DiscoveryFacet::Known(vec!["serde".to_owned()])
                } else {
                    DiscoveryFacet::Unknown
                },
                ..DiscoveryMetadata::default()
            };
            (key, metadata)
        })
        .collect::<Vec<_>>();
        let index = build_discovery_projection_with_metadata(&entries);
        let request = DiscoverySearchRequest {
            text: "serde",
            ecosystem: Some(RegistryEcosystem::Cargo),
        };
        let first = index
            .search_groups_after(request, 1, None)
            .expect("first group page");
        assert_eq!(first.groups[0].lineage, "alpha-widget");
        let cursor = first.next_cursor.expect("second group cursor");
        let second = index
            .search_groups_after(request, 1, Some(&cursor))
            .expect("second group page");
        assert_eq!(second.groups.len(), 1);
        assert_eq!(second.groups[0].lineage, "zeta");
        assert!(
            second
                .groups
                .iter()
                .all(|group| group.lineage != "alpha-widget")
        );
        assert_eq!(second.result_count, SearchResultCount::Unknown);
    }

    #[test]
    fn search_cursor_rejects_oversized_sort_key_during_deserialization() {
        let oversized = "x".repeat(MAX_CURSOR_SORT_KEY_BYTES + 1);
        let encoded = serde_json::json!({
            "next_tier": 0,
            "after_sort_key": oversized,
            "returned_before": 0
        })
        .to_string();
        assert!(serde_json::from_str::<SearchContinuation>(&encoded).is_err());
    }

    #[test]
    fn structural_search_revision_is_stable_for_reopened_registry_and_forge_rows() {
        let path = temp_journal_path();
        let source = source(RegistryEcosystem::Cargo, "https://index.crates.io");
        let forge = forge_document(
            "https://github.com/acme/serde-fork@tag:v1.0.0",
            "pkg:cargo/serde-fork@1.0.0",
            DiscoveryFacet::Known(vec!["serde fork".to_owned()]),
            DiscoveryFacet::Known("forked serialization support".to_owned()),
            DiscoveryFacet::Unknown,
            DiscoveryFacet::Unknown,
            DiscoveryFacet::Unknown,
            vec!["commit-deadbeef".to_owned()],
        );
        let store = {
            let mut store = DiscoveryStore::open(path.clone()).expect("open journal");
            store
                .commit(discovery_batch(
                    source,
                    "",
                    "cursor-1",
                    100,
                    &[(
                        "pkg:cargo/serde@1.0.0",
                        DiscoveryStanding::Published,
                        "2026-09-28",
                        7,
                    )],
                ))
                .expect("commit registry document");
            store
        };
        let first = DiscoverySearchIndex::open_with_forge(&store, &[forge.clone()])
            .expect("first projection");
        let revision = first.revision();
        let root = first.snapshot_root();
        drop(first);
        drop(store);

        let reopened = DiscoveryStore::open(path.clone()).expect("reopen journal");
        let rebuilt =
            DiscoverySearchIndex::open_with_forge(&reopened, &[forge]).expect("rebuild projection");
        assert_eq!(rebuilt.revision(), revision);
        assert_eq!(rebuilt.snapshot_root(), root);
        drop(rebuilt);
        drop(reopened);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("journal.lock"));
    }

    #[test]
    fn cold_reopen_preserves_case_sensitive_go_coordinates_and_lineages() {
        let path = temp_journal_path();
        let source = source(RegistryEcosystem::Golang, "https://proxy.golang.org");
        let upper_coordinate = "pkg:golang/github.com/Sirupsen/logrus@v1.4.1";
        let lower_coordinate = "pkg:golang/github.com/sirupsen/logrus@v1.4.1";
        let mut store = DiscoveryStore::open(path.clone()).expect("open journal");
        store
            .commit(discovery_batch(
                source,
                "",
                "go-window-1",
                100,
                &[(
                    upper_coordinate,
                    DiscoveryStanding::Published,
                    "2026-09-01",
                    1,
                )],
            ))
            .expect("commit first case-sensitive Go module");
        let mut warm = DiscoverySearchIndex::open(&store).expect("initial warm projection");
        store
            .commit(discovery_batch(
                source,
                "go-window-1",
                "go-window-2",
                200,
                &[(
                    lower_coordinate,
                    DiscoveryStanding::Published,
                    "2026-09-02",
                    2,
                )],
            ))
            .expect("commit second case-sensitive Go module");
        warm.sync(&store).expect("incremental warm update");

        let request = DiscoverySearchRequest {
            text: "logrus",
            ecosystem: Some(RegistryEcosystem::Golang),
        };
        let expected_coordinates = [upper_coordinate.to_owned(), lower_coordinate.to_owned()];
        let release_coordinates = |index: &DiscoverySearchIndex| {
            let mut coordinates = index
                .search(request, 8)
                .expect("release search")
                .hits
                .into_iter()
                .map(|hit| hit.key.coordinate.as_str().to_owned())
                .collect::<Vec<_>>();
            coordinates.sort();
            coordinates
        };
        assert_eq!(release_coordinates(&warm), expected_coordinates);
        let warm_groups = warm.search_groups(request, 8).expect("warm grouped search");
        assert_eq!(warm_groups.groups.len(), 2);

        drop(warm);
        drop(store);
        let reopened = DiscoveryStore::open(path.clone()).expect("reopen journal");
        let cold = DiscoverySearchIndex::open(&reopened).expect("cold projection rebuild");
        assert_eq!(release_coordinates(&cold), expected_coordinates);

        let cold_groups = cold.search_groups(request, 8).expect("cold grouped search");
        let mut lineages = cold_groups
            .groups
            .iter()
            .map(|group| group.lineage.as_str())
            .collect::<Vec<_>>();
        lineages.sort_unstable();
        assert_eq!(
            lineages,
            ["github.com/Sirupsen/logrus", "github.com/sirupsen/logrus"]
        );

        let first_page = cold.search_groups(request, 1).expect("first grouped page");
        let first_lineage = first_page.groups[0].lineage.clone();
        let cursor = first_page.next_cursor.expect("second lineage continuation");
        let encoded = serde_json::to_vec(&cursor).expect("encode continuation");
        let decoded: DiscoverySearchCursor =
            serde_json::from_slice(&encoded).expect("decode continuation");
        let second_page = cold
            .search_groups_after(request, 1, Some(&decoded))
            .expect("second grouped page");
        assert_eq!(second_page.groups.len(), 1);
        assert_ne!(second_page.groups[0].lineage, first_lineage);
        assert!(second_page.next_cursor.is_none());

        drop(cold);
        drop(reopened);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("journal.lock"));
    }

    #[test]
    fn cold_reopen_orders_grouped_releases_by_semantic_version() {
        let path = temp_journal_path();
        let source = source(RegistryEcosystem::Cargo, "https://index.crates.io");
        let mut rows = (0..320)
            .map(|patch| {
                (
                    format!("pkg:cargo/order-demo@1.0.{patch}"),
                    DiscoveryStanding::Published,
                    "2026-09-01".to_owned(),
                    (patch % 251) as u8,
                )
            })
            .collect::<Vec<_>>();
        rows.push((
            "pkg:cargo/order-demo-addon@1.0.0".to_owned(),
            DiscoveryStanding::Published,
            "2026-09-01".to_owned(),
            252,
        ));
        let rows = rows
            .iter()
            .map(|(coordinate, standing, event_time, proof)| {
                (coordinate.as_str(), *standing, event_time.as_str(), *proof)
            })
            .collect::<Vec<_>>();
        let mut store = DiscoveryStore::open(path.clone()).expect("open journal");
        store
            .commit(discovery_batch(source, "", "order-window-1", 100, &rows))
            .expect("commit release history");
        let mut warm = DiscoverySearchIndex::open(&store).expect("warm projection");
        let request = DiscoverySearchRequest {
            text: "order-demo",
            ecosystem: Some(RegistryEcosystem::Cargo),
        };
        let release_page = |index: &DiscoverySearchIndex| {
            let page = index
                .search_groups_after(request, 1, None)
                .expect("group search");
            assert_eq!(page.groups.len(), 1);
            let versions = page.groups[0]
                .matched_releases
                .iter()
                .map(|key| key.coordinate.version().to_owned())
                .collect::<Vec<_>>();
            (
                versions,
                page.groups[0].more_releases,
                page.facet_releases_examined,
                page.next_cursor,
            )
        };
        let expected = (304..320)
            .rev()
            .map(|patch| format!("1.0.{patch}"))
            .collect::<Vec<_>>();
        let (initial_order, initial_more, initial_examined, _) = release_page(&warm);
        assert_eq!(initial_order, expected);
        assert!(initial_more);
        assert_eq!(initial_examined, 17);

        let yanked_coordinate = "pkg:cargo/order-demo@1.0.320";
        store
            .commit(discovery_batch(
                source,
                "order-window-1",
                "order-window-2",
                200,
                &[(
                    yanked_coordinate,
                    DiscoveryStanding::Yanked,
                    "2026-09-02",
                    253,
                )],
            ))
            .expect("commit newest yanked release");
        warm.sync(&store)
            .expect("incrementally sync newest release");
        let (updated_order, updated_more, updated_examined, updated_cursor) = release_page(&warm);
        assert_eq!(
            updated_order,
            (305..321)
                .rev()
                .map(|patch| format!("1.0.{patch}"))
                .collect::<Vec<_>>()
        );
        assert!(updated_more);
        assert_eq!(updated_examined, 17);
        let updated_cursor = updated_cursor.expect("cursor continues after first lineage");
        let encoded_cursor = serde_json::to_vec(&updated_cursor).expect("encode group cursor");
        let updated_cursor: DiscoverySearchCursor =
            serde_json::from_slice(&encoded_cursor).expect("decode group cursor");

        let yanked_search = warm
            .search_with_store(
                &store,
                DiscoverySearchRequest {
                    text: yanked_coordinate,
                    ecosystem: Some(RegistryEcosystem::Cargo),
                },
                1,
            )
            .expect("read current standing overlay");
        assert_eq!(yanked_search.hits.len(), 1);
        assert_eq!(
            yanked_search.hits[0].standing,
            SearchStandingEvidence::Yanked
        );

        drop(warm);
        drop(store);
        let reopened = DiscoveryStore::open(path.clone()).expect("reopen journal");
        let cold = DiscoverySearchIndex::open(&reopened).expect("cold projection");
        let (cold_order, cold_more, cold_examined, cold_cursor) = release_page(&cold);
        assert_eq!(
            cold_order,
            (305..321)
                .rev()
                .map(|patch| format!("1.0.{patch}"))
                .collect::<Vec<_>>()
        );
        assert!(cold_more);
        assert_eq!(cold_examined, 17);
        assert_eq!(cold_cursor, Some(updated_cursor.clone()));
        let second_group = cold
            .search_groups_after(request, 1, Some(&updated_cursor))
            .expect("continue after cold reopen");
        assert_eq!(second_group.groups.len(), 1);
        assert_eq!(second_group.groups[0].lineage, "order-demo-addon");
        assert!(second_group.next_cursor.is_none());
        let cold_yanked = cold
            .search_with_store(
                &reopened,
                DiscoverySearchRequest {
                    text: yanked_coordinate,
                    ecosystem: Some(RegistryEcosystem::Cargo),
                },
                1,
            )
            .expect("reopen current standing overlay");
        assert_eq!(cold_yanked.hits[0].standing, SearchStandingEvidence::Yanked);

        drop(cold);
        drop(reopened);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("journal.lock"));
        let mut lock_path = path.as_os_str().to_owned();
        lock_path.push(".lock");
        let _ = std::fs::remove_file(std::path::PathBuf::from(lock_path));
    }

    #[test]
    fn lineage_snapshot_root_binds_content_and_ignores_insertion_order() {
        let cargo = source(RegistryEcosystem::Cargo, "https://index.crates.io");
        let entries = vec![
            key(cargo, "pkg:cargo/alpha@1.0.0"),
            key(cargo, "pkg:cargo/beta@1.0.0"),
        ];
        let forward = build_discovery_search_projection(&entries);
        let reversed =
            build_discovery_search_projection(&entries.iter().cloned().rev().collect::<Vec<_>>());
        let different = build_discovery_search_projection(&[entries[0].clone()]);

        assert_eq!(forward.snapshot_root(), reversed.snapshot_root());
        assert_ne!(forward.snapshot_root(), different.snapshot_root());
        let request = DiscoverySearchRequest {
            text: "pkg:cargo",
            ecosystem: Some(RegistryEcosystem::Cargo),
        };
        let cursor = forward
            .search_groups(request, 1)
            .expect("first catalog page")
            .next_cursor
            .expect("continuation");
        assert!(
            different
                .search_groups_after(request, 1, Some(&cursor))
                .is_err()
        );
    }

    #[test]
    fn forge_only_search_indexes_source_facts_incrementally_and_rebuilds_stably() {
        let path = temp_journal_path();
        let store = DiscoveryStore::open(path.clone()).expect("empty discovery journal");
        let v1 = forge_document(
            "https://github.com/acme/micro-utils@tag:v1.0.0",
            "pkg:cargo/micro-utils@1.0.0",
            DiscoveryFacet::Known(vec!["micro toolkit".to_owned()]),
            DiscoveryFacet::Known("safe asynchronous primitives".to_owned()),
            DiscoveryFacet::Known(vec!["tokio".to_owned(), "Rust".to_owned()]),
            DiscoveryFacet::Known("MIT".to_owned()),
            DiscoveryFacet::Known("reentrant event loop beacon".to_owned()),
            vec![
                "https://github.com/acme/micro-utils/archive/refs/tags/v1.0.0.tar.gz".to_owned(),
                "deadbeefcafe".to_owned(),
                "sha256-archive-pin-one".to_owned(),
            ],
        );
        assert_eq!(v1.metadata.downloads, DiscoveryFacet::Unknown);
        assert_eq!(v1.metadata.yanked, DiscoveryFacet::Unknown);
        assert_eq!(v1.metadata.advisories, DiscoveryFacet::Unknown);
        let mut index = DiscoverySearchIndex::open_with_forge(&store, &[v1.clone()])
            .expect("build combined projection");
        let initial_revision = index.revision();
        let index_identity = index.index_identity();
        let search = |index: &DiscoverySearchIndex, query: &str| {
            index
                .search(
                    DiscoverySearchRequest {
                        text: query,
                        ecosystem: None,
                    },
                    16,
                )
                .expect("search")
        };

        let exact = search(&index, "pkg:cargo/micro-utils@1.0.0");
        assert_eq!(exact.hits.len(), 1);
        assert_eq!(
            exact.hits[0].key.coordinate.as_str(),
            "pkg:cargo/micro-utils@1.0.0"
        );
        assert!(matches!(
            exact.hits[0].key.source,
            DiscoverySearchSource::Forge(_)
        ));
        assert_eq!(exact.hits[0].evidence, SearchMatchEvidence::ExactCoordinate);
        assert_eq!(
            search(&index, "micro toolkit").hits[0].evidence,
            SearchMatchEvidence::ExactAlias
        );
        assert_eq!(
            search(&index, "tokio").hits[0].evidence,
            SearchMatchEvidence::KeywordTerms
        );
        assert_eq!(
            search(&index, "reentrant event loop").hits[0].evidence,
            SearchMatchEvidence::DescriptionTerms
        );
        assert_eq!(
            search(&index, "deadbeefcafe").hits[0].evidence,
            SearchMatchEvidence::GenericTerms
        );

        let v2 = forge_document(
            "https://github.com/acme/micro-utils@tag:v2.0.0",
            "pkg:cargo/micro-utils@2.0.0",
            DiscoveryFacet::Known(vec!["micro toolkit".to_owned()]),
            DiscoveryFacet::Unknown,
            DiscoveryFacet::Known(vec!["tokio".to_owned(), "Rust".to_owned()]),
            DiscoveryFacet::Known("MIT OR Apache-2.0".to_owned()),
            DiscoveryFacet::Known("allocation-free scheduler beacon".to_owned()),
            vec![
                "https://github.com/acme/micro-utils/archive/refs/tags/v2.0.0.tar.gz".to_owned(),
                "cafebabedead".to_owned(),
                "sha256-archive-pin-two".to_owned(),
            ],
        );
        index
            .sync_with_forge(&store, &[v1.clone(), v2.clone()])
            .expect("apply forge version delta");
        assert_eq!(index.index_identity(), index_identity);
        assert_eq!(search(&index, "allocation-free scheduler").hits.len(), 1);

        let expected = search(&index, "micro toolkit")
            .hits
            .into_iter()
            .map(|hit| (hit.key, hit.evidence))
            .collect::<Vec<_>>();
        let expected_revision = index.revision();
        drop(index);
        drop(store);
        let reopened = DiscoveryStore::open(path.clone()).expect("reopen journal");
        let rebuilt = DiscoverySearchIndex::open_with_forge(&reopened, &[v1.clone(), v2.clone()])
            .expect("rebuild combined projection");
        assert_eq!(rebuilt.revision(), expected_revision);
        assert_ne!(rebuilt.revision(), initial_revision);
        let actual = search(&rebuilt, "micro toolkit")
            .hits
            .into_iter()
            .map(|hit| (hit.key, hit.evidence))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        assert_eq!(
            search(&rebuilt, "pkg:cargo/micro-utils@2.0.0").hits[0]
                .key
                .source,
            DiscoverySearchSource::Forge(ForgeSearchSourceIdentity {
                coordinate: ForgeCoordinate::parse(
                    "https://github.com/acme/micro-utils@tag:v2.0.0"
                )
                .expect("forge source coordinate"),
                ecosystem: RegistryEcosystem::Cargo,
            })
        );
        let mut changed_v2 = ForgeSearchDocument::from_pinned_facts(
            ForgeCoordinate::parse("https://github.com/acme/micro-utils@tag:v2.0.0")
                .expect("forge source coordinate"),
            ProductPackageCoordinate::parse("pkg:cargo/micro-utils@2.0.0")
                .expect("package coordinate"),
            DiscoveryFacet::Known(vec!["micro toolkit".to_owned()]),
            DiscoveryFacet::Known("changed searchable summary".to_owned()),
            DiscoveryFacet::Known(vec!["tokio".to_owned(), "Rust".to_owned()]),
            DiscoveryFacet::Known("MIT OR Apache-2.0".to_owned()),
            DiscoveryFacet::Known("allocation-free scheduler beacon".to_owned()),
            vec!["changed archive pin".to_owned()],
        )
        .expect("changed forge document");
        changed_v2.metadata.advisories = DiscoveryFacet::Known(Vec::new());
        let changed = DiscoverySearchIndex::open_with_forge(&reopened, &[v1.clone(), changed_v2])
            .expect("open changed forge metadata");
        assert_ne!(changed.revision(), rebuilt.revision());
        drop(changed);
        drop(rebuilt);
        drop(reopened);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("journal.lock"));
    }

    #[test]
    fn streaming_grams_keep_the_existing_unicode_and_field_boundary_grammar() {
        fn reference(fields: &[String], width: usize) -> String {
            let mut terms = Vec::new();
            for field in fields {
                if !terms.is_empty() {
                    terms.push(BOUNDARY_TOKEN.to_owned());
                }
                let characters = field.chars().collect::<Vec<_>>();
                for window in characters.windows(width) {
                    let mut term = String::from("g");
                    for character in window {
                        term.push_str(&format!("{:06x}", u32::from(*character)));
                    }
                    terms.push(term);
                }
            }
            terms.join(" ")
        }

        for fields in [
            vec!["", "a", "", "ab", "", "abc"],
            vec!["Café Straße", "İstanbul", "emoji 🚀 launch"],
            vec!["👩🏽‍💻", "e\u{301}", "\u{0}", ""],
        ] {
            let normalized = fields.into_iter().map(normalize).collect::<Vec<_>>();
            let values = || normalized.iter().map(String::as_str);
            assert_eq!(
                combined_gram_stream::<1>(values()),
                reference(&normalized, 1)
            );
            assert_eq!(
                combined_gram_stream::<2>(values()),
                reference(&normalized, 2)
            );
            assert_eq!(
                combined_gram_stream::<3>(values()),
                reference(&normalized, 3)
            );
        }
    }

    #[test]
    fn search_grams_match_unicode_phrases_without_cross_field_matches() {
        let mut index = TextSearchIndex::new().expect("index");
        index
            .add_document(
                1_u8,
                "pkg:local/coffee",
                "Café Straße",
                ["emoji 🚀 launch"].into_iter(),
                "pkg:local/coffee\u{1f}1".to_owned(),
            )
            .expect("add row");
        index
            .add_document(
                2_u8,
                "pkg:local/other",
                "Coffee",
                ["́"].into_iter(),
                "pkg:local/other\u{1f}2".to_owned(),
            )
            .expect("add second row");
        index.commit().expect("commit");
        assert_eq!(hit_keys(index.page("CAFÉ", 4).expect("accent").hits), [1]);
        assert_eq!(hit_keys(index.page("ßE", 4).expect("sharp s").hits), [1]);
        assert_eq!(hit_keys(index.page("🚀", 4).expect("emoji").hits), [1]);
        assert!(index.page("é", 4).expect("cross-field").hits.is_empty());
    }

    #[test]
    fn searchable_metadata_matches_all_query_terms_without_phrase_order() {
        let mut index = TextSearchIndex::new().expect("index");
        index
            .add_document_with_ecosystem(
                1_u8,
                "pkg:cargo/async-client@1.0.0",
                "async-client",
                ["asyncio networking", "HTTP client for Python"].into_iter(),
                "cargo",
                "cargo\u{1f}async-client".to_owned(),
            )
            .expect("add metadata result");
        index
            .add_document_with_ecosystem(
                2_u8,
                "pkg:cargo/http-server@1.0.0",
                "http-server",
                ["HTTP test server"].into_iter(),
                "cargo",
                "cargo\u{1f}http-server".to_owned(),
            )
            .expect("add unrelated result");
        index.commit().expect("commit");

        assert_eq!(
            hit_keys(
                index
                    .page("client asyncio", 8)
                    .expect("metadata terms")
                    .hits
            ),
            [1]
        );
        assert_eq!(
            hit_keys(
                index
                    .page("python HTTP", 8)
                    .expect("description terms")
                    .hits
            ),
            [1]
        );
        assert!(
            index
                .page("python server", 8)
                .expect("mixed terms")
                .hits
                .is_empty()
        );
    }

    #[test]
    fn ranked_pages_resume_across_tiers_without_offset_scans_or_duplicate_hits() {
        let mut index = TextSearchIndex::new().expect("index");
        let mut expected_rows = Vec::new();
        let mut next_id = 0_usize;
        for name in ["needle", "needle"] {
            let coordinate = format!("pkg:local/exact-{next_id:03}");
            let sort_key = format!("{}\u{1f}{next_id:03}", normalize(&coordinate));
            index
                .add_document(
                    next_id,
                    &coordinate,
                    name,
                    std::iter::empty(),
                    sort_key.clone(),
                )
                .expect("add exact-name row");
            expected_rows.push((next_id, coordinate, name.to_owned(), sort_key));
            next_id += 1;
        }
        for _ in 0..80 {
            let name = format!("needle-client-{next_id:03}");
            let coordinate = format!("pkg:local/{name}");
            let sort_key = format!("{}\u{1f}{next_id:03}", normalize(&coordinate));
            index
                .add_document(
                    next_id,
                    &coordinate,
                    &name,
                    std::iter::empty(),
                    sort_key.clone(),
                )
                .expect("add prefix row");
            expected_rows.push((next_id, coordinate, name, sort_key));
            next_id += 1;
        }
        for _ in 0..60 {
            let name = format!("x-needle-token-{next_id:03}");
            let coordinate = format!("pkg:local/{name}");
            let sort_key = format!("{}\u{1f}{next_id:03}", normalize(&coordinate));
            index
                .add_document(
                    next_id,
                    &coordinate,
                    &name,
                    std::iter::empty(),
                    sort_key.clone(),
                )
                .expect("add substring row");
            expected_rows.push((next_id, coordinate, name, sort_key));
            next_id += 1;
        }
        for _ in 0..3 {
            let name = "needel".to_owned();
            let coordinate = format!("pkg:local/needel-{next_id:03}");
            let sort_key = format!("{}\u{1f}{next_id:03}", normalize(&coordinate));
            index
                .add_document(
                    next_id,
                    &coordinate,
                    &name,
                    std::iter::empty(),
                    sort_key.clone(),
                )
                .expect("add typo row");
            expected_rows.push((next_id, coordinate, name, sort_key));
            next_id += 1;
        }
        index.commit().expect("commit");

        let expected = oracle(&expected_rows, "needle", usize::MAX);
        let mut actual = Vec::new();
        let mut continuation = None;
        let mut page_count = 0;
        loop {
            let page = index
                .page_with_ecosystem("needle", 17, None, continuation.as_ref())
                .expect("page");
            page_count += 1;
            assert!(page.posting_candidates <= 18);
            actual.extend(page.hits.into_iter().map(|hit| hit.key));
            if let Some(cursor) = page.next_cursor {
                if page_count == 1 {
                    assert!(matches!(page.result_count, SearchResultCount::AtLeast(18)));
                }
                continuation = Some(cursor);
            } else {
                assert_eq!(page.result_count, SearchResultCount::Exact(expected.len()));
                break;
            }
        }
        assert!(page_count > 1, "fixture should exercise continuation");
        assert_eq!(actual, expected);
        assert_eq!(actual.iter().collect::<BTreeSet<_>>().len(), actual.len());
    }

    #[test]
    fn exact_lookahead_prepares_only_the_exact_search_tier() {
        let mut index = TextSearchIndex::new().expect("index");
        for ordinal in 0..2 {
            index
                .add_document(
                    ordinal,
                    "pkg:local/needle",
                    "needle",
                    std::iter::empty(),
                    format!("pkg:local/needle\u{1f}{ordinal:03}"),
                )
                .expect("add exact-coordinate row");
        }
        index.commit().expect("commit");

        let prepared = index.prepare_search_query(normalize("pkg:local/needle"));
        let page = prepared
            .page_filtered(1, None, None)
            .expect("exact search page");
        assert_eq!(page.hits.len(), 1);
        assert!(page.next_cursor.is_some());
        assert_eq!(
            prepared
                .tier_queries
                .iter()
                .filter(|query| query.get().is_some())
                .count(),
            1
        );
        assert_eq!(
            prepared
                .ranked_tiers
                .iter()
                .filter(|query| query.get().is_some())
                .count(),
            1
        );
        assert!(prepared.tier_queries[0].get().is_some());
        assert!(prepared.ranked_tiers[0].get().is_some());
    }

    #[test]
    fn typed_registry_facets_rank_alias_keyword_advisory_and_description_without_inventing_unknowns()
     {
        let cargo = source(RegistryEcosystem::Cargo, "https://index.crates.io");
        let npm = source(RegistryEcosystem::Npm, "https://registry.npmjs.org");
        let pypi = source(RegistryEcosystem::Pypi, "https://pypi.org/simple");
        let nuget = source(
            RegistryEcosystem::Nuget,
            "https://api.nuget.org/v3/index.json",
        );

        let cargo_key = key(cargo, "pkg:cargo/hyper-transport@0.4.0");
        let mut cargo_metadata = DiscoveryMetadata::default();
        cargo_metadata.aliases = DiscoveryFacet::Known(vec!["net-client".to_owned()]);
        cargo_metadata.keywords = DiscoveryFacet::Known(vec![
            "asynchronous networking".to_owned(),
            "http transport".to_owned(),
        ]);
        cargo_metadata.description =
            DiscoveryFacet::Known("A lightweight protocol adapter".to_owned());
        cargo_metadata.license = DiscoveryFacet::Known("MIT".to_owned());

        let npm_key = key(npm, "pkg:npm/%40example/net-client@2.1.0");
        let mut npm_metadata = DiscoveryMetadata::default();
        npm_metadata.aliases = DiscoveryFacet::Known(vec!["wire-client".to_owned()]);
        npm_metadata.keywords = DiscoveryFacet::Known(vec!["networking".to_owned()]);
        npm_metadata.description =
            DiscoveryFacet::Known("A JavaScript client component".to_owned());

        let pypi_key = key(pypi, "pkg:pypi/http-bridge@1.2.0");
        let mut pypi_metadata = DiscoveryMetadata::default();
        pypi_metadata.aliases = DiscoveryFacet::Absent;
        pypi_metadata.keywords = DiscoveryFacet::Known(vec!["python http".to_owned()]);
        pypi_metadata.description =
            DiscoveryFacet::Known("Python client for HTTP services".to_owned());
        pypi_metadata.advisories = DiscoveryFacet::Known(vec![DiscoveryAdvisory {
            id: "GHSA-abcd-1234".to_owned(),
            aliases: vec!["CVE-2026-12345".to_owned()],
            summary: DiscoveryFacet::Known("Remote request exposure".to_owned()),
            severity: DiscoveryFacet::Known("high".to_owned()),
            fixed_in: DiscoveryFacet::Unknown,
        }]);

        // Keep the package name unrelated to the query so this checks unknown
        // facets rather than a one-edit fuzzy match on the coordinate name.
        let nuget_key = key(nuget, "pkg:nuget/unlisted-package@1.0.0");
        let mut nuget_metadata = DiscoveryMetadata::default();
        nuget_metadata.aliases = DiscoveryFacet::Unknown;
        nuget_metadata.keywords = DiscoveryFacet::Absent;
        nuget_metadata.description = DiscoveryFacet::Unknown;

        let entries = vec![
            (cargo_key.clone(), cargo_metadata.clone()),
            (npm_key.clone(), npm_metadata.clone()),
            (pypi_key.clone(), pypi_metadata.clone()),
            (nuget_key.clone(), nuget_metadata.clone()),
        ];
        let mut index = TextSearchIndex::new().expect("index");
        for (key, metadata) in &entries {
            let coordinate = key.coordinate.as_str();
            let lineage = lineage_search_name(key.source.ecosystem(), &key.lineage);
            index
                .add_document_with_search_text(
                    key.clone(),
                    coordinate,
                    lineage,
                    discovery_search_text(metadata),
                    key.source.ecosystem().as_str(),
                    discovery_sort_key(&key.source, coordinate),
                )
                .expect("index discovery metadata");
        }
        index.commit().expect("commit");

        for (query, ecosystem) in [
            ("net-client", None),
            ("wire-cli", None),
            ("asynchronous networking", None),
            ("GHSA-abcd-1234", None),
            ("python client", None),
            ("unknown metadata", None),
            ("python http", Some(RegistryEcosystem::Pypi)),
            ("python http", Some(RegistryEcosystem::Cargo)),
        ] {
            let actual = index
                .page_filtered(query, 16, ecosystem.map(RegistryEcosystem::as_str), None)
                .expect("typed metadata query")
                .hits
                .into_iter()
                .map(|hit| hit.key)
                .collect::<Vec<_>>();
            assert_eq!(
                actual,
                typed_oracle(&entries, query, ecosystem, 16),
                "query={query:?} ecosystem={ecosystem:?}"
            );
        }
        assert_eq!(
            hit_keys(index.page("net-client", 8).expect("alias query").hits),
            [npm_key.clone(), cargo_key]
        );
        assert_eq!(
            hit_keys(index.page("wire-cli", 8).expect("alias prefix typo").hits),
            [npm_key]
        );
        assert_eq!(
            hit_keys(index.page("GHSA-abcd-1234", 8).expect("advisory id").hits),
            [pypi_key]
        );
        assert!(
            index
                .page("unknown metadata", 8)
                .expect("unknown omitted")
                .hits
                .is_empty()
        );
        assert!(
            index
                .page("absent keyword", 8)
                .expect("absent omitted")
                .hits
                .is_empty()
        );
    }

    #[test]
    fn local_document_replacements_and_deletes_match_the_linear_oracle_and_cold_build() {
        let mut index = TextSearchIndex::new().expect("index");
        let first_identity = "symbol:one".to_owned();
        let second_identity = "symbol:two".to_owned();
        let first_sort = "pkg:local/oldneedle\u{1f}symbol:one".to_owned();
        let second_sort = "pkg:local/otherneedle\u{1f}symbol:two".to_owned();
        index
            .replace_document(
                first_identity.clone(),
                "one".to_owned(),
                "pkg:local/oldneedle",
                "oldneedle",
                std::iter::empty(),
                first_sort.clone(),
            )
            .expect("insert first row");
        index
            .replace_document(
                second_identity.clone(),
                "two".to_owned(),
                "pkg:local/otherneedle",
                "otherneedle",
                std::iter::empty(),
                second_sort.clone(),
            )
            .expect("insert second row");
        index.commit().expect("initial commit");
        let initial = vec![
            (
                "one".to_owned(),
                "pkg:local/oldneedle".to_owned(),
                "oldneedle".to_owned(),
                first_sort,
            ),
            (
                "two".to_owned(),
                "pkg:local/otherneedle".to_owned(),
                "otherneedle".to_owned(),
                second_sort,
            ),
        ];
        assert_eq!(
            hit_keys(index.page("oldneedle", 8).expect("old query").hits),
            oracle(&initial, "oldneedle", 8)
        );

        let replacement_sort = "pkg:local/newneedle\u{1f}symbol:one".to_owned();
        index
            .replace_document(
                first_identity,
                "one".to_owned(),
                "pkg:local/newneedle",
                "newneedle",
                std::iter::empty(),
                replacement_sort.clone(),
            )
            .expect("replace first row");
        index.remove_document(&second_identity);
        index.commit().expect("delta commit");
        let current = vec![(
            "one".to_owned(),
            "pkg:local/newneedle".to_owned(),
            "newneedle".to_owned(),
            replacement_sort.clone(),
        )];
        for query in ["oldneedle", "newneedle", "pkg:local", ""] {
            assert_eq!(
                hit_keys(index.page(query, 8).expect("incremental query").hits),
                oracle(&current, query, 8),
                "{query}"
            );
        }

        let mut cold = TextSearchIndex::new().expect("cold index");
        cold.replace_document(
            "symbol:one".to_owned(),
            "one".to_owned(),
            "pkg:local/newneedle",
            "newneedle",
            std::iter::empty(),
            replacement_sort,
        )
        .expect("cold insert");
        cold.commit().expect("cold commit");
        assert_eq!(
            hit_keys(index.page("newneedle", 8).expect("incremental result").hits),
            hit_keys(cold.page("newneedle", 8).expect("cold result").hits)
        );
    }

    #[test]
    fn local_view_delta_chain_updates_one_writer_and_matches_cold_rebuild() {
        let (base, _) = super::super::initial_view().expect("initial view");
        let capability = super::super::test_builtin_view_capability().expect("view capability");
        let first_id = RowId::Symbol(backend_engine::symbol_key("fixture::before"));
        let removed_id = RowId::Symbol(backend_engine::symbol_key("fixture::removed"));
        let first = Row::new(first_id, base.basis(), "fixture::before");
        let removed = Row::new(removed_id, base.basis(), "fixture::removed");
        let mut local = LocalDeclarationSearchIndex::default();
        local.sync(&base).expect("initial local projection");
        let index_identity = local.index_identity().expect("initial index");

        let prepared = base
            .prepare(ViewDelta::Upsert { row: first }, capability.clone())
            .expect("prepare initial row");
        let (view_one, first_delta) = base.clone().commit(prepared).expect("commit initial row");
        let mut changes = vec![
            RowChange::Upsert(Box::new(Row::new(
                first_id,
                view_one.basis(),
                "fixture::replacement",
            ))),
            RowChange::Upsert(Box::new(removed)),
        ];
        changes.sort_by_key(RowChange::id);
        let patch = ViewDelta::Patch {
            changes: changes.into(),
        };
        let prepared = view_one
            .prepare(patch, capability)
            .expect("prepare update patch");
        let (view_two, second_delta) = view_one.commit(prepared).expect("commit update patch");
        let removal = ViewDelta::Remove { id: removed_id };
        let prepared = view_two
            .prepare(
                removal,
                super::super::test_builtin_view_capability().expect("removal capability"),
            )
            .expect("prepare removal");
        let (view_three, third_delta) = view_two.commit(prepared).expect("commit removal");

        local
            .apply_committed_deltas(&[first_delta, second_delta, third_delta])
            .expect("apply committed row deltas");
        assert_eq!(local.index_identity(), Some(index_identity));
        assert!(local.page("before", 8).expect("old label").hits.is_empty());
        assert_eq!(
            hit_keys(local.page("replacement", 8).expect("updated label").hits),
            [first_id]
        );
        assert!(
            local
                .page("removed", 8)
                .expect("removed label")
                .hits
                .is_empty()
        );

        let mut cold = LocalDeclarationSearchIndex::default();
        cold.sync(&view_three).expect("cold projection");
        for query in ["before", "replacement", "removed", "fixture", ""] {
            assert_eq!(
                hit_keys(local.page(query, 16).expect("incremental page").hits),
                hit_keys(cold.page(query, 16).expect("cold page").hits),
                "{query}"
            );
        }

        let unrelated_row = Row::new(
            RowId::Symbol(backend_engine::symbol_key("fixture::unrelated")),
            base.basis(),
            "fixture::unrelated",
        );
        let prepared = base
            .prepare(
                ViewDelta::Upsert { row: unrelated_row },
                super::super::test_builtin_view_capability().expect("fallback capability"),
            )
            .expect("prepare unrelated delta");
        let (_, unrelated_delta) = base.commit(prepared).expect("commit unrelated delta");
        local
            .apply_committed_deltas(&[unrelated_delta])
            .expect("mismatched delta invalidates cache");
        assert_eq!(local.index_identity(), None);
        local.sync(&view_three).expect("fallback cold rebuild");
        assert_eq!(
            hit_keys(local.page("replacement", 8).expect("fallback result").hits),
            [first_id]
        );
    }

    #[test]
    fn rare_query_collects_page_bounded_hits_with_ten_thousand_unrelated_rows() {
        const ROWS: usize = 10_000;
        let source = source(RegistryEcosystem::Cargo, "https://index.crates.io");
        let mut entries = (0..ROWS)
            .map(|index| key(source, &format!("pkg:cargo/unrelated-{index:05}@1.0.0")))
            .collect::<Vec<_>>();
        let target = key(source, "pkg:cargo/zzq9needle@1.0.0");
        entries.push(target.clone());
        let searchable = discovery_entries(&entries);
        let index = build_discovery_index(&entries);
        let page = index.page("zzq9needle", 8).expect("rare query");
        assert_eq!(
            hit_keys(page.hits.clone()),
            oracle(&searchable, "zzq9needle", 8)
        );
        assert_eq!(hit_keys(page.hits), [target]);
        assert_eq!(page.posting_candidates, 1);
    }

    #[test]
    fn discovery_projection_reuses_structure_for_fact_refresh_and_rebuilds_after_reopen() {
        let source = source(
            RegistryEcosystem::Nuget,
            "https://api.nuget.org/v3/index.json",
        );
        let path = temp_journal_path();
        let first_coordinate = "pkg:nuget/Widget@1.0.0";
        let second_coordinate = "pkg:nuget/Widget.Tools@2.0.0";
        let mut store = DiscoveryStore::open(path.clone()).expect("open journal");
        let mut initial_batch = discovery_batch(
            source,
            "",
            "2026-09-01T00:00:00Z",
            100,
            &[(
                first_coordinate,
                DiscoveryStanding::Published,
                "2026-09-01T00:00:00Z",
                1,
            )],
        );
        initial_batch.facts[0].metadata.aliases =
            DiscoveryFacet::Known(vec!["legacy-widget".to_owned()]);
        store.commit(initial_batch).expect("commit initial fact");

        let mut index = DiscoverySearchIndex::open(&store).expect("cold build");
        let index_identity = index.index_identity();
        let first_key = key(source, first_coordinate);
        assert_eq!(
            hit_keys(index.page("widget", 8).expect("initial page").hits),
            [first_key.clone()]
        );
        assert_eq!(
            hit_keys(index.page("legacy-widget", 8).expect("initial alias").hits),
            [first_key.clone()]
        );
        let initial_revision = store.search_revision();

        let mut standing_refresh = discovery_batch(
            source,
            "2026-09-01T00:00:00Z",
            "2026-09-02T00:00:00Z",
            200,
            &[(
                first_coordinate,
                DiscoveryStanding::Yanked,
                "2026-09-02T00:00:00Z",
                2,
            )],
        );
        standing_refresh.facts[0].metadata.aliases =
            DiscoveryFacet::Known(vec!["legacy-widget".to_owned()]);
        store
            .commit(standing_refresh)
            .expect("refresh existing fact");
        assert_eq!(store.search_revision(), initial_revision);
        index.sync(&store).expect("metadata-only sync");
        assert_eq!(index.index_identity(), index_identity);
        assert_eq!(
            store
                .fact(source, first_coordinate)
                .expect("hydrated fact")
                .standing,
            DiscoveryStanding::Yanked,
            "search keys stay stable while current fact data is hydrated separately"
        );
        let current_yanked = index
            .search_with_store(
                &store,
                DiscoverySearchRequest {
                    text: "widget",
                    ecosystem: Some(RegistryEcosystem::Nuget),
                },
                8,
            )
            .expect("current standing overlay");
        assert_eq!(
            current_yanked.hits[0].standing,
            SearchStandingEvidence::Yanked
        );

        let mut structural_update = discovery_batch(
            source,
            "2026-09-02T00:00:00Z",
            "2026-09-03T00:00:00Z",
            300,
            &[
                (
                    first_coordinate,
                    DiscoveryStanding::Yanked,
                    "2026-09-03T00:00:00Z",
                    4,
                ),
                (
                    second_coordinate,
                    DiscoveryStanding::Published,
                    "2026-09-03T00:00:00Z",
                    3,
                ),
            ],
        );
        structural_update.facts[0].metadata.aliases =
            DiscoveryFacet::Known(vec!["current-widget".to_owned()]);
        store
            .commit(structural_update)
            .expect("replace searchable alias and add release");
        index.sync(&store).expect("incremental sync");
        assert_eq!(index.index_identity(), index_identity);
        assert!(
            index
                .page("legacy-widget", 8)
                .expect("old alias")
                .hits
                .is_empty()
        );
        assert_eq!(
            hit_keys(
                index
                    .page("current-widget", 8)
                    .expect("replacement alias")
                    .hits
            ),
            [first_key.clone()]
        );
        let second_key = key(source, second_coordinate);
        assert_eq!(
            index
                .page("widget.tools", 8)
                .expect("incremental page")
                .hits
                .into_iter()
                .map(|hit| hit.key)
                .collect::<Vec<_>>(),
            [second_key.clone()]
        );

        drop(index);
        drop(store);
        let reopened = DiscoveryStore::open(path.clone()).expect("reopen journal");
        assert!(reopened.is_historical(source));
        assert_eq!(
            reopened
                .fact(source, first_coordinate)
                .expect("historical fact")
                .standing,
            DiscoveryStanding::Yanked,
            "cold reopen preserves the last source value while freshness is historical"
        );
        let rebuilt = DiscoverySearchIndex::open(&reopened).expect("rebuild projection");
        let historical_reply = rebuilt
            .search_with_store(
                &reopened,
                DiscoverySearchRequest {
                    text: "widget",
                    ecosystem: Some(RegistryEcosystem::Nuget),
                },
                8,
            )
            .expect("historical search reply");
        assert!(historical_reply.hits.iter().any(|hit| {
            hit.key.coordinate.as_str() == first_coordinate
                && hit.standing == SearchStandingEvidence::Yanked
        }));
        assert!(historical_reply.hits.iter().any(|hit| {
            hit.key.coordinate.as_str() == second_coordinate
                && hit.standing == SearchStandingEvidence::Available
        }));
        assert_eq!(
            hit_keys(
                rebuilt
                    .page("current-widget", 8)
                    .expect("reopened metadata")
                    .hits
            ),
            [key(source, first_coordinate)]
        );
        assert_eq!(
            rebuilt
                .search(
                    DiscoverySearchRequest {
                        text: "widget",
                        ecosystem: Some(RegistryEcosystem::Nuget),
                    },
                    8,
                )
                .expect("filtered reopen query")
                .hits
                .into_iter()
                .map(|hit| hit.key)
                .collect::<Vec<_>>(),
            [
                key(source, first_coordinate),
                key(source, second_coordinate)
            ]
        );
        assert!(
            rebuilt
                .search(
                    DiscoverySearchRequest {
                        text: "widget",
                        ecosystem: Some(RegistryEcosystem::Cargo),
                    },
                    8,
                )
                .expect("empty ecosystem facet")
                .hits
                .is_empty()
        );
        let searchable = discovery_entries(&[first_key, second_key]);
        assert_eq!(
            hit_keys(rebuilt.page("widget", 8).expect("reopened page").hits),
            oracle(&searchable, "widget", 8)
        );
        drop(rebuilt);
        drop(reopened);
        let _ = std::fs::remove_file(&path);
        let mut lock_path = path.as_os_str().to_owned();
        lock_path.push(".lock");
        let _ = std::fs::remove_file(std::path::PathBuf::from(lock_path));
    }

    #[test]
    #[ignore = "manual diagnostic; run with --ignored --nocapture to measure the 10k search projection"]
    fn manual_10k_discovery_search_benchmark_reports_rss_and_warm_vs_linear_p50_p95() {
        const ROWS: usize = 10_000;
        const SAMPLES: usize = 100;
        let source = source(RegistryEcosystem::Cargo, "https://index.crates.io");
        let mut entries = (0..ROWS)
            .map(|index| key(source, &format!("pkg:cargo/unrelated-{index:05}@1.0.0")))
            .collect::<Vec<_>>();
        let target = key(source, "pkg:cargo/zzq9needle@1.0.0");
        entries.push(target);
        let searchable = discovery_entries(&entries);
        let rss_before = resident_set_kib();
        let build_started = std::time::Instant::now();
        let index = build_discovery_index(&entries);
        let build_millis = build_started.elapsed().as_millis();
        let rss_after = resident_set_kib();
        let rss_delta_kib = rss_before
            .zip(rss_after)
            .map(|(before, after)| after.saturating_sub(before));

        let expected = oracle(&searchable, "zzq9needle", 8);
        let mut index_samples = Vec::with_capacity(SAMPLES);
        let mut linear_samples = Vec::with_capacity(SAMPLES);
        let mut posting_candidates = 0;
        for _ in 0..SAMPLES {
            let started = std::time::Instant::now();
            let page = index.page("zzq9needle", 8).expect("indexed query");
            index_samples.push(started.elapsed());
            posting_candidates = page.posting_candidates;
            assert_eq!(hit_keys(page.hits), expected);

            let started = std::time::Instant::now();
            let linear = oracle(&searchable, "zzq9needle", 8);
            linear_samples.push(started.elapsed());
            assert_eq!(linear, expected);
        }

        let index_p50 = percentile_micros(&mut index_samples, 50);
        let index_p95 = percentile_micros(&mut index_samples, 95);
        let linear_p50 = percentile_micros(&mut linear_samples, 50);
        let linear_p95 = percentile_micros(&mut linear_samples, 95);
        eprintln!(
            "10k discovery search: build={build_millis}ms rss_delta_kib={rss_delta_kib:?} posting_candidates={posting_candidates} indexed_warm_p50={index_p50}us indexed_warm_p95={index_p95}us linear_p50={linear_p50}us linear_p95={linear_p95}us"
        );
    }

    #[test]
    #[ignore = "manual live-owner latency diagnostic; set DISCOVERY_SEARCH_OWNER_JOURNAL to a cloned owner's registry-discovery/catalog.journal"]
    fn manual_live_owner_group_search_reports_cold_open_and_warm_owner_p50_p95() {
        const WARMUPS: usize = 3;
        const SAMPLES: usize = 21;
        let journal = std::env::var_os("DISCOVERY_SEARCH_OWNER_JOURNAL")
            .expect("set DISCOVERY_SEARCH_OWNER_JOURNAL to a cloned owner's catalog journal");
        let store = DiscoveryStore::open(std::path::PathBuf::from(journal))
            .expect("open cloned live-owner discovery journal");
        let cold_started = std::time::Instant::now();
        let index = DiscoverySearchIndex::open(&store).expect("rebuild owner search projection");
        let cold_open_millis = cold_started.elapsed().as_millis();
        let request = DiscoverySearchRequest {
            text: "serde",
            ecosystem: Some(RegistryEcosystem::Cargo),
        };
        for _ in 0..WARMUPS {
            index
                .search_groups(request, 20)
                .expect("warm owner grouped search");
        }
        let mut samples = Vec::with_capacity(SAMPLES);
        let mut last_page = None;
        for _ in 0..SAMPLES {
            let started = std::time::Instant::now();
            let page = index
                .search_groups(request, 20)
                .expect("measured owner grouped search");
            samples.push(started.elapsed());
            last_page = Some((
                page.groups.len(),
                page.posting_candidates,
                page.index_documents_visited,
                page.facet_releases_examined,
            ));
        }
        let p50_micros = percentile_micros(&mut samples, 50);
        let p95_micros = percentile_micros(&mut samples, 95);
        assert!(
            last_page.is_some_and(|(groups, candidates, _, _)| { groups > 0 && candidates > 0 })
        );
        eprintln!(
            "live owner grouped search: cold_open={cold_open_millis}ms warm_owner_p50={p50_micros}us warm_owner_p95={p95_micros}us query={} ecosystem={:?} limit=20 samples={SAMPLES} page_counters={last_page:?}",
            request.text, request.ecosystem,
        );
    }

    #[test]
    #[ignore = "manual diagnostic; run with --ignored --nocapture to measure incremental local-row mutation cost"]
    fn manual_10k_local_index_reports_delta_mutation_and_cold_rebuild_cost() {
        const ROWS: usize = 10_000;
        const CHANGED: usize = 100;
        let rss_before = resident_set_kib();
        let build_started = std::time::Instant::now();
        let mut index = TextSearchIndex::new().expect("index");
        let mut rows = Vec::with_capacity(ROWS);
        for ordinal in 0..ROWS {
            let identity = format!("symbol:{ordinal:05}");
            let coordinate = format!("pkg:local/unrelated-{ordinal:05}");
            let name = format!("unrelated-{ordinal:05}");
            let sort_key = local_test_sort_key(&coordinate, &identity);
            rows.push((
                identity.clone(),
                coordinate.clone(),
                name.clone(),
                sort_key.clone(),
            ));
            index
                .replace_document(
                    identity.clone(),
                    identity,
                    &coordinate,
                    &name,
                    std::iter::empty(),
                    sort_key,
                )
                .expect("insert row");
        }
        index.commit().expect("initial commit");
        let build_millis = build_started.elapsed().as_millis();
        let rss_after_build = resident_set_kib();

        let mutation_started = std::time::Instant::now();
        for ordinal in 0..CHANGED {
            let identity = format!("symbol:{ordinal:05}");
            let coordinate = format!("pkg:local/replaced-{ordinal:05}");
            let name = format!("replaced-{ordinal:05}");
            let sort_key = local_test_sort_key(&coordinate, &identity);
            rows[ordinal] = (
                identity.clone(),
                coordinate.clone(),
                name.clone(),
                sort_key.clone(),
            );
            index
                .replace_document(
                    identity.clone(),
                    identity,
                    &coordinate,
                    &name,
                    std::iter::empty(),
                    sort_key,
                )
                .expect("replace row");
        }
        for ordinal in CHANGED..CHANGED * 2 {
            let identity = format!("symbol:{ordinal:05}");
            index.remove_document(&identity);
            rows[ordinal] = (String::new(), String::new(), String::new(), String::new());
        }
        index.commit().expect("delta commit");
        let mutation_millis = mutation_started.elapsed().as_millis();
        let rss_after_delta = resident_set_kib();

        let cold_started = std::time::Instant::now();
        let mut cold = TextSearchIndex::new().expect("cold index");
        for (identity, coordinate, name, sort_key) in rows.iter().filter(|row| !row.0.is_empty()) {
            cold.replace_document(
                identity.clone(),
                identity.clone(),
                coordinate,
                name,
                std::iter::empty(),
                sort_key.clone(),
            )
            .expect("cold row");
        }
        cold.commit().expect("cold commit");
        let cold_rebuild_millis = cold_started.elapsed().as_millis();
        for query in ["replaced-00050", "unrelated-05000", "replaced-00150"] {
            assert_eq!(
                hit_keys(index.page(query, 16).expect("incremental query").hits),
                hit_keys(cold.page(query, 16).expect("cold query").hits)
            );
        }
        let rss_build_delta_kib = rss_before
            .zip(rss_after_build)
            .map(|(before, after)| after.saturating_sub(before));
        let rss_mutation_delta_kib = rss_after_build
            .zip(rss_after_delta)
            .map(|(before, after)| after.saturating_sub(before));
        eprintln!(
            "10k local index: build={build_millis}ms delta_{CHANGED}_upserts_{CHANGED}_deletes={mutation_millis}ms cold_rebuild_after_delta={cold_rebuild_millis}ms rss_delta_build_kib={rss_build_delta_kib:?} rss_delta_mutation_kib={rss_mutation_delta_kib:?} writer_budget_bytes={WRITER_MEMORY_BYTES}"
        );
    }

    fn local_test_sort_key(coordinate: &str, identity: &str) -> String {
        format!("{}\u{1f}{identity}", normalize(coordinate))
    }
}
