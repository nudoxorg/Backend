//! Source-only package catalog observations.
//!
//! Discovery is deliberately separate from [`super::RegistryTransport`]: a
//! discovery event identifies a package coordinate and its standing, but it
//! does not name or fetch an archive. The two source adapters here preserve
//! the different guarantees exposed by the registries. NuGet's catalog is an
//! ordered event stream; crates.io's public crates listing is a mutable,
//! paginated recent-updates view and is therefore always marked windowed.

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use super::{PackageCoordinate, RegistryEcosystem, RegistryEndpoint, admit_registry_coordinate};

/// Upper bound for an opaque source cursor persisted by a local discovery
/// owner. Source cursors are never interpreted by generic storage code.
pub const MAX_DISCOVERY_CURSOR_BYTES: usize = 4096;
/// Upper bound for one source page before it is admitted into the durable
/// discovery journal.
pub const MAX_DISCOVERY_PAGE_ITEMS: usize = 4096;
/// Bound for a source-only package-name snapshot (not a release fact page).
pub const MAX_DISCOVERY_PROJECTS: usize = 2_000_000;

/// Stable identity of the source that made a catalog claim.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct DiscoverySourceIdentity {
    ecosystem: RegistryEcosystem,
    id: [u8; 32],
}

impl DiscoverySourceIdentity {
    /// Builds a discovery identity from an already admitted credential-free
    /// registry endpoint.
    #[must_use]
    pub const fn from_endpoint(endpoint: &RegistryEndpoint) -> Self {
        Self {
            ecosystem: endpoint.ecosystem(),
            id: endpoint.id().as_bytes(),
        }
    }

    /// Ecosystem whose coordinates this source may report.
    #[must_use]
    pub const fn ecosystem(self) -> RegistryEcosystem {
        self.ecosystem
    }

    /// Credential-free source identifier.
    #[must_use]
    pub const fn id(self) -> [u8; 32] {
        self.id
    }
}

/// Opaque cursor owned by one source adapter.
///
/// The byte representation is intentionally not public as text. NuGet stores
/// its catalog timestamp here; the crates.io adapter stores a page hint, which
/// never upgrades its windowed coverage to complete.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DiscoveryCursor(Vec<u8>);

impl DiscoveryCursor {
    /// Admits a source cursor within the storage bound.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, DiscoveryError> {
        let bytes = bytes.into();
        if bytes.len() > MAX_DISCOVERY_CURSOR_BYTES {
            return Err(DiscoveryError::Bounds);
        }
        Ok(Self(bytes))
    }

    /// Returns the opaque source bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// True when the adapter has not committed an initial position.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Revalidates a cursor recovered from durable storage.
    pub fn admit(&self) -> Result<(), DiscoveryError> {
        if self.0.len() > MAX_DISCOVERY_CURSOR_BYTES {
            Err(DiscoveryError::Bounds)
        } else {
            Ok(())
        }
    }
}

/// Time at which the local owner admitted a source observation, in Unix
/// milliseconds. This is independent from a source's event timestamp.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct DiscoveryObservedAt(u64);

impl DiscoveryObservedAt {
    /// Constructs an observation time from the local wall clock.
    #[must_use]
    pub const fn from_unix_millis(value: u64) -> Self {
        Self(value)
    }

    /// Returns the local observation time.
    #[must_use]
    pub const fn as_unix_millis(self) -> u64 {
        self.0
    }
}

/// Completeness promised by the source adapter for this observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryCompleteness {
    /// All source events through the attached cursor were admitted.
    CompleteThroughCursor,
    /// A bounded or mutable source view may omit events or older coordinates.
    Windowed,
    /// This source does not expose the requested discovery surface.
    Unsupported,
    /// The source response was valid but could not be admitted in full.
    Incomplete,
}

/// Current package standing reported by a registry source.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryStanding {
    /// The source currently reports this release as published/listed.
    Published,
    /// The release is published but explicitly yanked or unlisted.
    Yanked,
    /// The source explicitly reports a deletion or withdrawal.
    Withdrawn,
    /// A source tree declares a package recipe, but does not prove a release
    /// was published or built by the binary registry.
    RecipeAvailable,
}

/// Source claim for an optional metadata facet. `Absent` means the source
/// schema does not provide the facet, while `Unknown` means this observation
/// did not establish a value. A known empty list is distinct from both.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum DiscoveryFacet<T> {
    /// Source supplied the value, including a deliberately empty collection.
    Known(T),
    /// This source surface does not report the facet.
    Absent,
    /// The facet may exist, but this response did not establish its value.
    Unknown,
}

impl<T> Default for DiscoveryFacet<T> {
    fn default() -> Self {
        Self::Unknown
    }
}

/// Bounded advisory evidence reported alongside a package release.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryAdvisory {
    /// Source-native advisory identifier, if supplied.
    pub id: String,
    /// Alternate canonical advisory identifiers, if supplied.
    pub aliases: Vec<String>,
    /// Short source-provided advisory description.
    pub summary: DiscoveryFacet<String>,
    /// Source-provided severity text; no normalized score is inferred.
    pub severity: DiscoveryFacet<String>,
    /// Source-provided versions at which the issue is fixed, when available.
    /// The adapter preserves this scope without inferring version ordering.
    #[serde(default)]
    pub fixed_in: DiscoveryFacet<Vec<String>>,
}

/// Optional package and release metadata retained with its source claim.
/// Download counts are represented only when a feed reports a value for this
/// exact release; package totals are not copied onto every version.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryMetadata {
    /// Registry-native alternate names or aliases.
    #[serde(default)]
    pub aliases: DiscoveryFacet<Vec<String>>,
    /// Human-readable package or release description.
    #[serde(default)]
    pub description: DiscoveryFacet<String>,
    /// Source-provided search keywords.
    #[serde(default)]
    pub keywords: DiscoveryFacet<Vec<String>>,
    /// License text, only when the registry itself reports it.
    #[serde(default)]
    pub license: DiscoveryFacet<String>,
    /// Source upload/release time in its original representation.
    #[serde(default)]
    pub published_at: DiscoveryFacet<String>,
    /// Exact-revision deprecation reason where the ecosystem reports it.
    #[serde(default)]
    pub deprecation: DiscoveryFacet<String>,
    /// Exact release yank/unlist flag, kept distinct from other standing.
    #[serde(default)]
    pub yanked: DiscoveryFacet<bool>,
    /// Advisory evidence attached to this exact release by the source.
    #[serde(default)]
    pub advisories: DiscoveryFacet<Vec<DiscoveryAdvisory>>,
    /// Exact per-release download observation; absent is never zero.
    #[serde(default)]
    pub downloads: DiscoveryFacet<u64>,
}

impl Default for DiscoveryMetadata {
    fn default() -> Self {
        Self {
            aliases: DiscoveryFacet::Unknown,
            description: DiscoveryFacet::Unknown,
            keywords: DiscoveryFacet::Unknown,
            license: DiscoveryFacet::Unknown,
            published_at: DiscoveryFacet::Unknown,
            deprecation: DiscoveryFacet::Unknown,
            yanked: DiscoveryFacet::Unknown,
            advisories: DiscoveryFacet::Unknown,
            downloads: DiscoveryFacet::Unknown,
        }
    }
}

impl DiscoveryMetadata {
    /// Checks source text and collection bounds before it enters the journal.
    pub fn admit(&self) -> Result<(), DiscoveryError> {
        fn strings_valid(values: &[String], maximum_items: usize, maximum_bytes: usize) -> bool {
            values.len() <= maximum_items
                && values
                    .iter()
                    .all(|value| value.len() <= maximum_bytes && !value.contains('\0'))
        }
        fn text_valid(value: &DiscoveryFacet<String>, maximum_bytes: usize) -> bool {
            !matches!(value, DiscoveryFacet::Known(text) if text.len() > maximum_bytes || text.contains('\0'))
        }
        let lists_valid = match &self.aliases {
            DiscoveryFacet::Known(values) => strings_valid(values, 64, 1024),
            _ => true,
        } && match &self.keywords {
            DiscoveryFacet::Known(values) => strings_valid(values, 128, 256),
            _ => true,
        };
        let advisories_valid = match &self.advisories {
            DiscoveryFacet::Known(advisories) => {
                advisories.len() <= 128
                    && advisories.iter().all(|advisory| {
                        !advisory.id.is_empty()
                            && advisory.id.len() <= 256
                            && strings_valid(&advisory.aliases, 32, 256)
                            && text_valid(&advisory.summary, 4096)
                            && text_valid(&advisory.severity, 128)
                            && match &advisory.fixed_in {
                                DiscoveryFacet::Known(versions) => {
                                    strings_valid(versions, 128, 256)
                                }
                                _ => true,
                            }
                    })
            }
            _ => true,
        };
        if !lists_valid
            || !advisories_valid
            || !text_valid(&self.description, 16 * 1024)
            || !text_valid(&self.license, 1024)
            || !text_valid(&self.published_at, 128)
            || !text_valid(&self.deprecation, 4096)
        {
            return Err(DiscoveryError::Bounds);
        }
        Ok(())
    }
}

/// One source-specific release claim. It contains no archive URL, digest, or
/// compiler artifact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryFact {
    pub source: DiscoverySourceIdentity,
    pub coordinate: PackageCoordinate,
    pub standing: DiscoveryStanding,
    pub observed_at: DiscoveryObservedAt,
    pub source_event_time: Option<String>,
    pub proof: [u8; 32],
    /// Optional, source-attributed package and version metadata.
    #[serde(default)]
    pub metadata: DiscoveryMetadata,
}

/// A committed unit of discovery work. The caller must persist `facts`,
/// `completeness`, and `next_cursor` together before treating the cursor as
/// advanced.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryBatch {
    pub source: DiscoverySourceIdentity,
    pub previous_cursor: DiscoveryCursor,
    pub next_cursor: DiscoveryCursor,
    /// Source's captured high watermark for this observation.
    pub source_high_watermark: DiscoveryCursor,
    /// Whether `next_cursor` reached the source's captured high watermark.
    /// Mutable windowed feeds leave this false.
    pub caught_up: bool,
    pub observed_at: DiscoveryObservedAt,
    pub completeness: DiscoveryCompleteness,
    pub facts: Vec<DiscoveryFact>,
    /// Package-level deletion events that the durable owner expands against
    /// this source's previously observed releases.
    #[serde(default)]
    pub package_retractions: Vec<DiscoveryPackageRetraction>,
}

impl DiscoveryBatch {
    /// Checks bounds and source/cursor invariants before durable commit.
    pub fn admit(&self) -> Result<(), DiscoveryError> {
        self.previous_cursor.admit()?;
        self.next_cursor.admit()?;
        self.source_high_watermark.admit()?;
        if self.facts.len() > MAX_DISCOVERY_PAGE_ITEMS
            || self.package_retractions.len() > MAX_DISCOVERY_PAGE_ITEMS
            || self.facts.iter().any(|fact| fact.source != self.source)
            || self.facts.iter().any(|fact| {
                admit_registry_coordinate(&fact.coordinate).map_or(true, |coordinate| {
                    coordinate.ecosystem() != self.source.ecosystem()
                }) || fact.metadata.admit().is_err()
            })
            || (self.completeness == DiscoveryCompleteness::CompleteThroughCursor
                && self.next_cursor.is_empty())
            || (self.caught_up && self.next_cursor != self.source_high_watermark)
            || (!self.package_retractions.is_empty()
                && self.source.ecosystem() != RegistryEcosystem::Npm)
            || self.package_retractions.iter().any(|retraction| {
                !valid_package_name(&retraction.package_name)
                    || retraction.source_event_time.is_empty()
                    || retraction.source_event_time.len() > 128
            })
        {
            return Err(DiscoveryError::Protocol);
        }
        Ok(())
    }
}

/// Bounded source format or cursor admission failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryError {
    /// A source response did not match the documented shape.
    Protocol,
    /// A coordinate or source origin was invalid for this adapter.
    InvalidIdentity,
    /// An input exceeded a configured parser bound.
    Bounds,
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol => formatter.write_str("invalid registry discovery response"),
            Self::InvalidIdentity => formatter.write_str("invalid registry discovery identity"),
            Self::Bounds => formatter.write_str("registry discovery response exceeds bounds"),
        }
    }
}

impl std::error::Error for DiscoveryError {}

/// One page URL and its high-water commit time from the NuGet catalog index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NugetCatalogPageRef {
    pub url: String,
    pub commit_timestamp: String,
    pub commit_id: String,
    pub count: usize,
}

/// Bounded plan obtained from an official NuGet Catalog/3.0.0 index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NugetCatalogPlan {
    pub previous_cursor: DiscoveryCursor,
    /// Cursor to use after every selected page is admitted in full.
    pub next_cursor: DiscoveryCursor,
    pub source_high_watermark: DiscoveryCursor,
    pub pages: Vec<NugetCatalogPageRef>,
    pub completeness: DiscoveryCompleteness,
    /// False when the page budget leaves later catalog pages for a follow-up.
    pub is_caught_up: bool,
}

/// One NuGet catalog item reference extracted from a page. The leaf document
/// is fetched separately for the package identity and listed/deleted state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NugetCatalogLeafRef {
    pub url: String,
    pub commit_timestamp: String,
    pub commit_id: String,
}

/// A NuGet catalog event admitted from its leaf document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NugetCatalogEvent {
    pub coordinate: PackageCoordinate,
    pub standing: DiscoveryStanding,
    pub commit_timestamp: String,
    pub commit_id: String,
    pub proof: [u8; 32],
    pub metadata: DiscoveryMetadata,
}

/// One package version observation decoded from a native catalog response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryReleaseObservation {
    pub coordinate: PackageCoordinate,
    pub standing: DiscoveryStanding,
    pub source_event_time: Option<String>,
    pub proof: [u8; 32],
    pub metadata: DiscoveryMetadata,
}

/// Bounded page from npm's CouchDB-compatible replication changes endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NpmChangesPage {
    pub previous_cursor: DiscoveryCursor,
    pub next_cursor: DiscoveryCursor,
    pub source_high_watermark: DiscoveryCursor,
    /// Number of additional feed items when the source provides it.
    pub pending: u64,
    pub packages: Vec<NpmChangedPackage>,
    pub completeness: DiscoveryCompleteness,
}

/// Current packument carried by one npm change row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NpmChangedPackage {
    pub name: String,
    pub deleted: bool,
    /// True when the replication feed omitted package metadata and a current
    /// packument must be fetched from the public metadata API.
    pub requires_packument: bool,
    /// Exact revision advertised by the change row, when supplied.
    pub revision: Option<String>,
    /// Whether the observed packument exceeded the accepted version window.
    pub is_truncated: bool,
    pub source_event_time: String,
    pub proof: [u8; 32],
    pub releases: Vec<DiscoveryReleaseObservation>,
}

/// Current npm packument fetched separately from the replication feed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NpmPackument {
    pub revision: Option<String>,
    pub releases: Vec<DiscoveryReleaseObservation>,
    pub is_truncated: bool,
}

/// Package-level removal event from npm. The durable owner expands this only
/// to release coordinates already observed from the same source.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryPackageRetraction {
    /// Registry-native package name, including namespace when present.
    pub package_name: String,
    /// Monotonic source sequence for this deletion event.
    pub source_event_time: String,
    /// Hash of the source event row that reported deletion.
    pub proof: [u8; 32],
}

/// Parsed PEP 691 project catalog. PyPI's project list is a mutable snapshot,
/// so it supplies a serial watermark but not an event cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PypiProjectList {
    /// Optional implementation-specific serial; PEP 691 does not require it.
    pub serial: Option<u64>,
    pub projects: Vec<PypiProjectRef>,
    pub is_truncated: bool,
}

/// One source-named PyPI project and its optional per-project serial.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PypiProjectRef {
    pub name: String,
    /// PEP 503 normalized name used for fair deterministic paging.
    pub canonical_name: String,
    pub serial: Option<u64>,
}

/// Bounded exact-project JSON metadata and release history from PyPI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PypiProjectMetadata {
    pub name: String,
    pub latest_version: Option<String>,
    pub releases: Vec<DiscoveryReleaseObservation>,
    pub is_truncated: bool,
}

/// One page of Maven Central's artifact search results. Search is a mutable
/// ranking view, not a change feed, and remains explicitly windowed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MavenSearchPage {
    pub offset: u64,
    pub next_offset: u64,
    pub total_results: u64,
    pub releases: Vec<DiscoveryReleaseObservation>,
    pub completeness: DiscoveryCompleteness,
}

/// Bounded page from index.golang.org's append-oriented module index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoModuleIndexPage {
    pub previous_cursor: DiscoveryCursor,
    pub next_cursor: DiscoveryCursor,
    pub source_high_watermark: DiscoveryCursor,
    pub releases: Vec<DiscoveryReleaseObservation>,
    pub completeness: DiscoveryCompleteness,
}

/// Conan Center recipe entry located in its source forge tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConanRecipeRef {
    pub name: String,
    pub config_path: String,
}

/// Parsed Conan Center Index tree snapshot. It contains recipe source paths,
/// not package binaries, archives, or quality scores.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConanRecipeTree {
    pub tree_sha: String,
    pub truncated: bool,
    pub recipes: Vec<ConanRecipeRef>,
}

/// Parses a bounded npm replication page. Current feeds omit packuments, so
/// the caller retrieves the direct package metadata separately and checks its
/// revision against the one advertised by the change row.
pub fn parse_npm_changes_page(
    bytes: &[u8],
    previous_cursor: &DiscoveryCursor,
    source_high_watermark: &DiscoveryCursor,
    max_changes: usize,
) -> Result<NpmChangesPage, DiscoveryError> {
    if bytes.len() > 32 * 1024 * 1024 || max_changes == 0 || max_changes > MAX_DISCOVERY_PAGE_ITEMS
    {
        return Err(DiscoveryError::Bounds);
    }
    let previous = parse_npm_cursor(previous_cursor)?;
    let source_high = parse_npm_cursor(source_high_watermark)?;
    if source_high < previous {
        return Err(DiscoveryError::Protocol);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| DiscoveryError::Protocol)?;
    let object = value.as_object().ok_or(DiscoveryError::Protocol)?;
    let pending = object
        .get("pending")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let rows = object
        .get("results")
        .or_else(|| object.get("result"))
        .and_then(serde_json::Value::as_array)
        .ok_or(DiscoveryError::Protocol)?;
    if rows.len() > max_changes {
        return Err(DiscoveryError::Bounds);
    }
    let mut packages = Vec::with_capacity(rows.len());
    let mut last_sequence = previous;
    for row in rows {
        let row = row.as_object().ok_or(DiscoveryError::Protocol)?;
        let sequence = json_sequence(row.get("seq").ok_or(DiscoveryError::Protocol)?)?;
        if sequence <= last_sequence || sequence > source_high {
            return Err(DiscoveryError::Protocol);
        }
        last_sequence = sequence;
        let name = row
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or(DiscoveryError::Protocol)?
            .to_owned();
        let deleted = row
            .get("deleted")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if !valid_npm_name(&name) {
            return Err(DiscoveryError::InvalidIdentity);
        }
        let source_event_time = format!("{sequence:020}");
        let proof = *blake3::hash(&serde_json::to_vec(row).map_err(|_| DiscoveryError::Protocol)?)
            .as_bytes();
        let document = row.get("doc").and_then(serde_json::Value::as_object);
        let revision = row
            .get("changes")
            .and_then(serde_json::Value::as_array)
            .and_then(|changes| changes.first())
            .and_then(serde_json::Value::as_object)
            .and_then(|change| change.get("rev"))
            .and_then(serde_json::Value::as_str)
            .filter(|revision| !revision.is_empty() && revision.len() <= 256)
            .map(str::to_owned);
        let (releases, is_truncated) = if deleted {
            (Vec::new(), false)
        } else if let Some(document) = document {
            let doc_name = document
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or(DiscoveryError::Protocol)?;
            if !same_npm_name(doc_name, &name) {
                return Err(DiscoveryError::InvalidIdentity);
            }
            parse_npm_packument_object(document, &name, sequence, MAX_DISCOVERY_PAGE_ITEMS)?
        } else {
            (Vec::new(), false)
        };
        packages.push(NpmChangedPackage {
            name,
            deleted,
            requires_packument: !deleted && document.is_none(),
            revision,
            is_truncated,
            source_event_time,
            proof,
            releases,
        });
    }
    let response_sequence = object
        .get("last_seq")
        .map(json_sequence)
        .transpose()?
        .unwrap_or(last_sequence);
    if response_sequence < last_sequence || response_sequence > source_high {
        return Err(DiscoveryError::Protocol);
    }
    let next_sequence = if packages.is_empty() {
        response_sequence
    } else {
        last_sequence
    };
    let next_cursor = npm_cursor(next_sequence)?;
    let completeness = if packages.iter().any(|package| package.is_truncated) {
        DiscoveryCompleteness::Incomplete
    } else {
        DiscoveryCompleteness::CompleteThroughCursor
    };
    Ok(NpmChangesPage {
        previous_cursor: previous_cursor.clone(),
        next_cursor,
        source_high_watermark: source_high_watermark.clone(),
        pending,
        packages,
        completeness,
    })
}

/// Parses a current packument retrieved from the public metadata endpoint.
pub fn parse_npm_packument_document(
    bytes: &[u8],
    expected_name: &str,
    sequence: u64,
    max_versions: usize,
) -> Result<NpmPackument, DiscoveryError> {
    if bytes.len() > 32 * 1024 * 1024 {
        return Err(DiscoveryError::Bounds);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| DiscoveryError::Protocol)?;
    let object = value.as_object().ok_or(DiscoveryError::Protocol)?;
    let name = required_text(object, "name")?;
    if !same_npm_name(name, expected_name) {
        return Err(DiscoveryError::InvalidIdentity);
    }
    let revision = object
        .get("_rev")
        .and_then(serde_json::Value::as_str)
        .filter(|revision| !revision.is_empty() && revision.len() <= 256)
        .map(str::to_owned);
    let (releases, is_truncated) =
        parse_npm_packument_object(object, expected_name, sequence, max_versions)?;
    Ok(NpmPackument {
        revision,
        releases,
        is_truncated,
    })
}

fn parse_npm_packument_object(
    object: &serde_json::Map<String, serde_json::Value>,
    package_name: &str,
    sequence: u64,
    max_versions: usize,
) -> Result<(Vec<DiscoveryReleaseObservation>, bool), DiscoveryError> {
    let versions = object
        .get("versions")
        .and_then(serde_json::Value::as_object)
        .ok_or(DiscoveryError::Protocol)?;
    if max_versions == 0 || max_versions > MAX_DISCOVERY_PAGE_ITEMS {
        return Err(DiscoveryError::Bounds);
    }
    let is_truncated = versions.len() > max_versions;
    let description = optional_text_facet(object, "description");
    let keywords = npm_keywords_facet(object);
    let times = object.get("time").and_then(serde_json::Value::as_object);
    let releases = versions
        .iter()
        .take(max_versions)
        .map(|(version, metadata)| {
            let metadata_object = metadata.as_object().ok_or(DiscoveryError::Protocol)?;
            let coordinate = PackageCoordinate::parse(format!("pkg:npm/{package_name}@{version}"))
                .map_err(|_| DiscoveryError::InvalidIdentity)?;
            let deprecation = optional_text_facet(metadata_object, "deprecated");
            let published_at = times
                .and_then(|times| times.get(version))
                .map(|value| value.as_str().map(str::to_owned))
                .map_or(DiscoveryFacet::Unknown, |value| {
                    value.map_or(DiscoveryFacet::Unknown, DiscoveryFacet::Known)
                });
            let mut source_metadata = DiscoveryMetadata::default();
            source_metadata.aliases = DiscoveryFacet::Absent;
            source_metadata.description = description.clone();
            source_metadata.keywords = keywords.clone();
            source_metadata.license = optional_license_facet(metadata_object);
            source_metadata.published_at = published_at;
            source_metadata.deprecation = deprecation;
            source_metadata.yanked = DiscoveryFacet::Absent;
            source_metadata.advisories = DiscoveryFacet::Absent;
            source_metadata.downloads = DiscoveryFacet::Absent;
            source_metadata.admit()?;
            let proof =
                *blake3::hash(&serde_json::to_vec(metadata).map_err(|_| DiscoveryError::Protocol)?)
                    .as_bytes();
            Ok(DiscoveryReleaseObservation {
                coordinate,
                standing: DiscoveryStanding::Published,
                source_event_time: Some(format!("{sequence:020}")),
                proof,
                metadata: source_metadata,
            })
        })
        .collect::<Result<Vec<_>, DiscoveryError>>()?;
    Ok((releases, is_truncated))
}

fn same_npm_name(left: &str, right: &str) -> bool {
    left == right
}

fn npm_keywords_facet(
    object: &serde_json::Map<String, serde_json::Value>,
) -> DiscoveryFacet<Vec<String>> {
    match object.get("keywords") {
        None => DiscoveryFacet::Unknown,
        Some(serde_json::Value::Null) => DiscoveryFacet::Absent,
        Some(serde_json::Value::Array(values)) => DiscoveryFacet::Known(
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect(),
        ),
        Some(serde_json::Value::String(value)) => {
            DiscoveryFacet::Known(value.split_ascii_whitespace().map(str::to_owned).collect())
        }
        Some(_) => DiscoveryFacet::Unknown,
    }
}

/// Parses the bounded project names from a PEP 691 `/simple/` response. A
/// capped list is explicitly marked truncated because the endpoint itself has
/// no portable page cursor.
pub fn parse_pypi_project_list(
    bytes: &[u8],
    max_projects: usize,
) -> Result<PypiProjectList, DiscoveryError> {
    if bytes.len() > 32 * 1024 * 1024 || max_projects == 0 || max_projects > MAX_DISCOVERY_PROJECTS
    {
        return Err(DiscoveryError::Bounds);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| DiscoveryError::Protocol)?;
    let object = value.as_object().ok_or(DiscoveryError::Protocol)?;
    let meta = object
        .get("meta")
        .and_then(serde_json::Value::as_object)
        .ok_or(DiscoveryError::Protocol)?;
    let serial = meta.get("_last-serial").and_then(serde_json::Value::as_u64);
    let projects = object
        .get("projects")
        .and_then(serde_json::Value::as_array)
        .ok_or(DiscoveryError::Protocol)?;
    let is_truncated = projects.len() > max_projects;
    let mut names = Vec::with_capacity(projects.len().min(max_projects));
    let mut seen = BTreeSet::new();
    for project in projects.iter().take(max_projects) {
        let project = project.as_object().ok_or(DiscoveryError::Protocol)?;
        let name = required_text(project, "name")?;
        if !valid_pypi_name(name) {
            return Err(DiscoveryError::InvalidIdentity);
        }
        let canonical = normalize_pypi_name(name);
        if seen.insert(canonical) {
            names.push(PypiProjectRef {
                name: name.to_owned(),
                canonical_name: normalize_pypi_name(name),
                serial: project
                    .get("_last-serial")
                    .and_then(serde_json::Value::as_u64),
            });
        }
    }
    names.sort_by(|left, right| left.canonical_name.cmp(&right.canonical_name));
    Ok(PypiProjectList {
        serial,
        projects: names,
        is_truncated,
    })
}

/// Parses PyPI's project JSON document without manufacturing a zero download
/// count or treating omitted advisory metadata as a clean security result.
pub fn parse_pypi_project_metadata(
    bytes: &[u8],
    expected_name: &str,
    max_versions: usize,
) -> Result<PypiProjectMetadata, DiscoveryError> {
    if bytes.len() > 32 * 1024 * 1024
        || max_versions == 0
        || max_versions > MAX_DISCOVERY_PAGE_ITEMS
    {
        return Err(DiscoveryError::Bounds);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| DiscoveryError::Protocol)?;
    let object = value.as_object().ok_or(DiscoveryError::Protocol)?;
    let info = object
        .get("info")
        .and_then(serde_json::Value::as_object)
        .ok_or(DiscoveryError::Protocol)?;
    let name = required_text(info, "name")?.to_owned();
    if normalize_pypi_name(&name) != normalize_pypi_name(expected_name) {
        return Err(DiscoveryError::InvalidIdentity);
    }
    let latest_version = info
        .get("version")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let releases = object
        .get("releases")
        .and_then(serde_json::Value::as_object)
        .ok_or(DiscoveryError::Protocol)?;
    let is_truncated = releases.len() > max_versions;
    let package_description = optional_text_facet(info, "summary");
    let package_license = optional_text_facet(info, "license");
    let package_keywords = match info.get("keywords") {
        None => DiscoveryFacet::Unknown,
        Some(serde_json::Value::Null) => DiscoveryFacet::Absent,
        Some(serde_json::Value::String(value)) => DiscoveryFacet::Known(
            value
                .split(',')
                .map(str::trim)
                .filter(|word| !word.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
        Some(_) => DiscoveryFacet::Unknown,
    };
    let latest_advisories = object
        .get("vulnerabilities")
        .and_then(serde_json::Value::as_array)
        .map(|items| parse_pypi_advisories(items))
        .transpose()?;
    let mut observations = Vec::with_capacity(releases.len().min(max_versions));
    for (version, files) in releases.iter().take(max_versions) {
        let files = files.as_array().ok_or(DiscoveryError::Protocol)?;
        if files.is_empty() {
            continue;
        }
        let mut all_yanked = true;
        let mut saw_unyanked = false;
        let mut all_yanked_known = true;
        let mut published_at = None::<String>;
        for file in files {
            let file = file.as_object().ok_or(DiscoveryError::Protocol)?;
            match file.get("yanked").and_then(serde_json::Value::as_bool) {
                Some(true) => {}
                Some(false) => {
                    all_yanked = false;
                    saw_unyanked = true;
                }
                None => {
                    all_yanked = false;
                    all_yanked_known = false;
                }
            }
            if let Some(time) = file
                .get("upload_time_iso_8601")
                .and_then(serde_json::Value::as_str)
                && published_at.as_deref().is_none_or(|old| time > old)
            {
                published_at = Some(time.to_owned());
            }
        }
        let canonical_name = normalize_pypi_name(&name);
        let coordinate = PackageCoordinate::parse(format!("pkg:pypi/{canonical_name}@{version}"))
            .map_err(|_| DiscoveryError::InvalidIdentity)?;
        let yanked = if all_yanked_known {
            DiscoveryFacet::Known(all_yanked)
        } else if saw_unyanked {
            DiscoveryFacet::Known(false)
        } else {
            DiscoveryFacet::Unknown
        };
        let is_latest = latest_version.as_deref() == Some(version.as_str());
        let mut metadata = DiscoveryMetadata::default();
        metadata.aliases = if name != canonical_name {
            DiscoveryFacet::Known(vec![name.clone()])
        } else {
            DiscoveryFacet::Absent
        };
        metadata.description = if is_latest {
            package_description.clone()
        } else {
            DiscoveryFacet::Unknown
        };
        metadata.keywords = if is_latest {
            package_keywords.clone()
        } else {
            DiscoveryFacet::Unknown
        };
        metadata.license = if is_latest {
            package_license.clone()
        } else {
            DiscoveryFacet::Unknown
        };
        metadata.published_at = published_at
            .clone()
            .map_or(DiscoveryFacet::Unknown, DiscoveryFacet::Known);
        metadata.deprecation = DiscoveryFacet::Absent;
        let is_yanked = yanked == DiscoveryFacet::Known(true);
        metadata.yanked = yanked;
        // This endpoint exposes package-level vulnerability evidence, not a
        // complete affected-version interval. Keep its `fixed_in` boundaries
        // intact and leave version ordering to a typed consumer.
        metadata.advisories = latest_advisories
            .clone()
            .map_or(DiscoveryFacet::Unknown, DiscoveryFacet::Known);
        metadata.downloads = DiscoveryFacet::Absent;
        metadata.admit()?;
        let proof =
            *blake3::hash(&serde_json::to_vec(files).map_err(|_| DiscoveryError::Protocol)?)
                .as_bytes();
        observations.push(DiscoveryReleaseObservation {
            coordinate,
            standing: if is_yanked {
                DiscoveryStanding::Yanked
            } else {
                DiscoveryStanding::Published
            },
            source_event_time: published_at,
            proof,
            metadata,
        });
    }
    Ok(PypiProjectMetadata {
        name,
        latest_version,
        releases: observations,
        is_truncated,
    })
}

fn parse_pypi_advisories(
    values: &[serde_json::Value],
) -> Result<Vec<DiscoveryAdvisory>, DiscoveryError> {
    if values.len() > 128 {
        return Err(DiscoveryError::Bounds);
    }
    values
        .iter()
        .map(|value| {
            let object = value.as_object().ok_or(DiscoveryError::Protocol)?;
            let id = required_text(object, "id")?.to_owned();
            let aliases = object
                .get("aliases")
                .and_then(serde_json::Value::as_array)
                .map(|aliases| {
                    aliases
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            Ok(DiscoveryAdvisory {
                id,
                aliases,
                summary: optional_text_facet(object, "details"),
                severity: DiscoveryFacet::Unknown,
                fixed_in: match object.get("fixed_in") {
                    Some(serde_json::Value::Array(values)) => DiscoveryFacet::Known(
                        values
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_owned)
                            .collect(),
                    ),
                    Some(serde_json::Value::Null) => DiscoveryFacet::Absent,
                    Some(_) => DiscoveryFacet::Unknown,
                    None => DiscoveryFacet::Unknown,
                },
            })
        })
        .collect()
}

/// Parses a bounded page from the official Maven Central Solr search API.
/// Maven Central search is mutable and offset-paginated, so its completeness
/// never claims an event cursor or a full registry snapshot.
pub fn parse_maven_search_page(
    bytes: &[u8],
    offset: u64,
    max_results: usize,
) -> Result<MavenSearchPage, DiscoveryError> {
    if bytes.len() > 32 * 1024 * 1024 || max_results == 0 || max_results > MAX_DISCOVERY_PAGE_ITEMS
    {
        return Err(DiscoveryError::Bounds);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| DiscoveryError::Protocol)?;
    let response = value
        .get("response")
        .and_then(serde_json::Value::as_object)
        .ok_or(DiscoveryError::Protocol)?;
    let total_results = required_u64(response, "numFound")?;
    let docs = response
        .get("docs")
        .and_then(serde_json::Value::as_array)
        .ok_or(DiscoveryError::Protocol)?;
    if docs.len() > max_results {
        return Err(DiscoveryError::Bounds);
    }
    let mut releases = Vec::with_capacity(docs.len());
    for document in docs {
        let document = document.as_object().ok_or(DiscoveryError::Protocol)?;
        let group = required_text(document, "g")?;
        let artifact = required_text(document, "a")?;
        let version = document
            .get("latestVersion")
            .and_then(serde_json::Value::as_str)
            .or_else(|| document.get("v").and_then(serde_json::Value::as_str))
            .filter(|version| !version.is_empty())
            .ok_or(DiscoveryError::Protocol)?;
        let coordinate =
            PackageCoordinate::parse(format!("pkg:maven/{group}/{artifact}@{version}"))
                .map_err(|_| DiscoveryError::InvalidIdentity)?;
        let timestamp = document
            .get("timestamp")
            .and_then(serde_json::Value::as_u64)
            .map(|value| value.to_string());
        let mut metadata = DiscoveryMetadata::default();
        metadata.aliases = DiscoveryFacet::Absent;
        metadata.description = DiscoveryFacet::Absent;
        metadata.keywords = DiscoveryFacet::Absent;
        metadata.license = DiscoveryFacet::Unknown;
        metadata.published_at = timestamp
            .clone()
            .map_or(DiscoveryFacet::Unknown, DiscoveryFacet::Known);
        metadata.deprecation = DiscoveryFacet::Absent;
        metadata.yanked = DiscoveryFacet::Unknown;
        metadata.advisories = DiscoveryFacet::Absent;
        metadata.downloads = DiscoveryFacet::Absent;
        let proof =
            *blake3::hash(&serde_json::to_vec(document).map_err(|_| DiscoveryError::Protocol)?)
                .as_bytes();
        releases.push(DiscoveryReleaseObservation {
            coordinate,
            standing: DiscoveryStanding::Published,
            source_event_time: timestamp.map(|timestamp| format!("{timestamp:0>20}")),
            proof,
            metadata,
        });
    }
    let next_offset = offset
        .checked_add(u64::try_from(docs.len()).map_err(|_| DiscoveryError::Bounds)?)
        .ok_or(DiscoveryError::Bounds)?;
    Ok(MavenSearchPage {
        offset,
        next_offset,
        total_results,
        releases,
        completeness: DiscoveryCompleteness::Windowed,
    })
}

/// Parses the line-delimited `index.golang.org/index` feed. `Path` is a plain
/// Go module path and `Timestamp` is the first-cache time; GOPROXY case escapes
/// do not appear in this feed. It is admitted as a bounded recent window,
/// never as a complete registry history.
pub fn parse_go_module_index_page(
    bytes: &[u8],
    previous_cursor: &DiscoveryCursor,
    max_rows: usize,
) -> Result<GoModuleIndexPage, DiscoveryError> {
    if bytes.len() > 16 * 1024 * 1024 || max_rows == 0 || max_rows > MAX_DISCOVERY_PAGE_ITEMS {
        return Err(DiscoveryError::Bounds);
    }
    let previous = if previous_cursor.is_empty() {
        String::new()
    } else {
        std::str::from_utf8(previous_cursor.as_bytes())
            .map_err(|_| DiscoveryError::Protocol)?
            .to_owned()
    };
    let mut releases = Vec::new();
    let mut last_time = previous.clone();
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        if releases.len() >= max_rows {
            return Err(DiscoveryError::Bounds);
        }
        let value: serde_json::Value =
            serde_json::from_slice(line).map_err(|_| DiscoveryError::Protocol)?;
        let object = value.as_object().ok_or(DiscoveryError::Protocol)?;
        let path = required_text(object, "Path")?;
        let version = required_text(object, "Version")?;
        let timestamp = required_text(object, "Timestamp")?;
        if !valid_go_module_path(path) {
            return Err(DiscoveryError::InvalidIdentity);
        }
        if timestamp < last_time.as_str() {
            return Err(DiscoveryError::Protocol);
        }
        let coordinate = PackageCoordinate::parse(format!("pkg:golang/{path}@{version}"))
            .map_err(|_| DiscoveryError::InvalidIdentity)?;
        let mut metadata = DiscoveryMetadata::default();
        metadata.aliases = DiscoveryFacet::Absent;
        metadata.description = DiscoveryFacet::Absent;
        metadata.keywords = DiscoveryFacet::Absent;
        metadata.license = DiscoveryFacet::Absent;
        metadata.published_at = DiscoveryFacet::Known(timestamp.to_owned());
        metadata.deprecation = DiscoveryFacet::Unknown;
        metadata.yanked = DiscoveryFacet::Absent;
        metadata.advisories = DiscoveryFacet::Unknown;
        metadata.downloads = DiscoveryFacet::Absent;
        metadata.admit()?;
        let proof = *blake3::hash(line).as_bytes();
        releases.push(DiscoveryReleaseObservation {
            coordinate,
            standing: DiscoveryStanding::Published,
            source_event_time: Some(timestamp.to_owned()),
            proof,
            metadata,
        });
        last_time = timestamp.to_owned();
    }
    let next_cursor = DiscoveryCursor::new(last_time.into_bytes())?;
    Ok(GoModuleIndexPage {
        previous_cursor: previous_cursor.clone(),
        source_high_watermark: next_cursor.clone(),
        next_cursor,
        releases,
        completeness: DiscoveryCompleteness::Windowed,
    })
}

/// Parses GitHub's recursive tree response for the Conan Center Index. Recipe
/// versions are resolved separately from each recipe's source `config.yml`;
/// the tree itself contains no binaries or build-quality claims.
pub fn parse_conan_recipe_tree(
    bytes: &[u8],
    max_recipes: usize,
) -> Result<ConanRecipeTree, DiscoveryError> {
    if bytes.len() > 32 * 1024 * 1024 || max_recipes == 0 || max_recipes > MAX_DISCOVERY_PAGE_ITEMS
    {
        return Err(DiscoveryError::Bounds);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| DiscoveryError::Protocol)?;
    let object = value.as_object().ok_or(DiscoveryError::Protocol)?;
    let tree_sha = required_text(object, "sha")?.to_owned();
    let truncated = object
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
        .ok_or(DiscoveryError::Protocol)?;
    let entries = object
        .get("tree")
        .and_then(serde_json::Value::as_array)
        .ok_or(DiscoveryError::Protocol)?;
    let mut recipes = BTreeMap::new();
    for entry in entries {
        let entry = entry.as_object().ok_or(DiscoveryError::Protocol)?;
        if entry.get("type").and_then(serde_json::Value::as_str) != Some("blob") {
            continue;
        }
        let path = required_text(entry, "path")?;
        let Some(recipe) = path
            .strip_prefix("recipes/")
            .and_then(|path| path.strip_suffix("/config.yml"))
            .filter(|name| valid_package_name(name) && !name.contains('/'))
        else {
            continue;
        };
        recipes.insert(
            recipe.to_owned(),
            ConanRecipeRef {
                name: recipe.to_owned(),
                config_path: path.to_owned(),
            },
        );
        if recipes.len() > max_recipes {
            break;
        }
    }
    let exceeded = recipes.len() > max_recipes;
    Ok(ConanRecipeTree {
        tree_sha,
        truncated: truncated || exceeded,
        recipes: recipes.into_values().take(max_recipes).collect(),
    })
}

/// Parses the version-to-folder map used by Conan Center Index `config.yml`
/// files. Only this deliberately narrow YAML mapping is admitted; arbitrary
/// recipe code and source archives are never evaluated or fetched.
pub fn parse_conan_recipe_versions(
    bytes: &[u8],
    package_name: &str,
    max_versions: usize,
) -> Result<Vec<DiscoveryReleaseObservation>, DiscoveryError> {
    if bytes.len() > 1024 * 1024
        || !valid_package_name(package_name)
        || max_versions == 0
        || max_versions > MAX_DISCOVERY_PAGE_ITEMS
    {
        return Err(DiscoveryError::Bounds);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| DiscoveryError::Protocol)?;
    let mut in_versions = false;
    let mut versions = BTreeSet::new();
    for line in text.lines() {
        if line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if line == "versions:" {
            in_versions = true;
            continue;
        }
        if !in_versions {
            continue;
        }
        if !line.starts_with("  ") || line.starts_with("   ") {
            if !line.starts_with(' ') {
                in_versions = false;
            }
            continue;
        }
        let entry = line.trim();
        let Some(key) = entry.strip_suffix(':') else {
            continue;
        };
        let version = key
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .or_else(|| {
                key.strip_prefix('\'')
                    .and_then(|value| value.strip_suffix('\''))
            })
            .unwrap_or(key);
        if !valid_version(version) {
            return Err(DiscoveryError::InvalidIdentity);
        }
        versions.insert(version.to_owned());
        if versions.len() > max_versions {
            return Err(DiscoveryError::Bounds);
        }
    }
    if versions.is_empty() {
        return Err(DiscoveryError::Protocol);
    }
    versions
        .into_iter()
        .map(|version| {
            let coordinate = PackageCoordinate::parse(format!(
                "pkg:generic/conan-center/{package_name}@{version}"
            ))
            .map_err(|_| DiscoveryError::InvalidIdentity)?;
            let mut metadata = DiscoveryMetadata::default();
            metadata.aliases = DiscoveryFacet::Absent;
            metadata.description = DiscoveryFacet::Unknown;
            metadata.keywords = DiscoveryFacet::Absent;
            metadata.license = DiscoveryFacet::Unknown;
            metadata.published_at = DiscoveryFacet::Unknown;
            metadata.deprecation = DiscoveryFacet::Unknown;
            metadata.yanked = DiscoveryFacet::Absent;
            metadata.advisories = DiscoveryFacet::Unknown;
            metadata.downloads = DiscoveryFacet::Absent;
            Ok(DiscoveryReleaseObservation {
                coordinate,
                standing: DiscoveryStanding::RecipeAvailable,
                source_event_time: None,
                proof: *blake3::hash(bytes).as_bytes(),
                metadata,
            })
        })
        .collect()
}

fn valid_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+' | b':')
        })
}

/// Parses NuGet's ordered catalog index and selects pages newer than the
/// durable `(timestamp, commitId)` cursor. The first poll starts from the oldest retained
/// page. If the page budget leaves more work, `is_caught_up` stays false;
/// `next_cursor` still describes gap-free coverage through the last selected
/// page.
pub fn parse_nuget_catalog_index(
    bytes: &[u8],
    catalog_origin: &str,
    previous_cursor: &DiscoveryCursor,
    max_pages: usize,
) -> Result<NugetCatalogPlan, DiscoveryError> {
    previous_cursor.admit()?;
    if max_pages == 0 || max_pages > MAX_DISCOVERY_PAGE_ITEMS || bytes.len() > 16 * 1024 * 1024 {
        return Err(DiscoveryError::Bounds);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| DiscoveryError::Protocol)?;
    let object = value.as_object().ok_or(DiscoveryError::Protocol)?;
    let source_high = required_text(object, "commitTimeStamp")?;
    let source_commit_id = required_text(object, "commitId")?;
    let source_high_cursor = nuget_cursor(source_high, source_commit_id)?;
    let declared_count = object
        .get("count")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(DiscoveryError::Protocol)?;
    let source_pages = object
        .get("items")
        .and_then(serde_json::Value::as_array)
        .ok_or(DiscoveryError::Protocol)?;
    if declared_count != source_pages.len() {
        return Err(DiscoveryError::Protocol);
    }
    let mut pages = object
        .get("items")
        .and_then(serde_json::Value::as_array)
        .ok_or(DiscoveryError::Protocol)?
        .iter()
        .map(|item| {
            let item = item.as_object().ok_or(DiscoveryError::Protocol)?;
            let timestamp = required_text(item, "commitTimeStamp")?.to_owned();
            let commit_id = required_text(item, "commitId")?.to_owned();
            let url = required_text(item, "@id")?.to_owned();
            let _cursor = nuget_cursor(&timestamp, &commit_id)?;
            let count = item
                .get("count")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or(DiscoveryError::Protocol)?;
            if count > MAX_DISCOVERY_PAGE_ITEMS || !same_catalog_path(catalog_origin, &url) {
                return Err(DiscoveryError::InvalidIdentity);
            }
            Ok(NugetCatalogPageRef {
                url,
                commit_timestamp: timestamp,
                commit_id,
                count,
            })
        })
        .collect::<Result<Vec<_>, DiscoveryError>>()?;
    if pages.len() > MAX_DISCOVERY_PAGE_ITEMS {
        return Err(DiscoveryError::Bounds);
    }
    pages.sort_by(|left, right| {
        left.commit_timestamp
            .cmp(&right.commit_timestamp)
            .then_with(|| left.commit_id.cmp(&right.commit_id))
    });
    let previous = parse_nuget_cursor(previous_cursor)?;
    let high = parse_nuget_cursor(&source_high_cursor)?.ok_or(DiscoveryError::Protocol)?;
    if previous.as_ref().is_some_and(|previous| high < *previous) {
        return Err(DiscoveryError::Protocol);
    }
    pages.retain(|page| {
        previous.as_ref().is_none_or(|previous| {
            (page.commit_timestamp.as_str(), page.commit_id.as_str())
                > (previous.0.as_str(), previous.1.as_str())
        }) && (page.commit_timestamp.as_str(), page.commit_id.as_str())
            <= (high.0.as_str(), high.1.as_str())
    });
    pages.truncate(max_pages);
    let next = match pages.last() {
        Some(page) => nuget_cursor(&page.commit_timestamp, &page.commit_id)?,
        None if previous_cursor.is_empty() => source_high_cursor.clone(),
        None => previous_cursor.clone(),
    };
    // A page budget can leave the source head behind, but the selected pages
    // are chronological and gap-free through this cursor.
    let completeness = DiscoveryCompleteness::CompleteThroughCursor;
    let source_high_watermark = source_high_cursor;
    let is_caught_up = next == source_high_watermark;
    Ok(NugetCatalogPlan {
        previous_cursor: previous_cursor.clone(),
        next_cursor: next,
        source_high_watermark,
        pages,
        completeness,
        is_caught_up,
    })
}

/// Parses a NuGet catalog page into ordered leaf references. The page itself
/// must match the index reference and stay within the captured source head.
pub fn parse_nuget_catalog_page(
    bytes: &[u8],
    catalog_origin: &str,
    previous_cursor: &DiscoveryCursor,
    source_high_watermark: &DiscoveryCursor,
    expected_commit_id: &str,
    max_items: usize,
) -> Result<Vec<NugetCatalogLeafRef>, DiscoveryError> {
    previous_cursor.admit()?;
    source_high_watermark.admit()?;
    if max_items == 0 || max_items > MAX_DISCOVERY_PAGE_ITEMS || bytes.len() > 32 * 1024 * 1024 {
        return Err(DiscoveryError::Bounds);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| DiscoveryError::Protocol)?;
    let page = value.as_object().ok_or(DiscoveryError::Protocol)?;
    let page_timestamp = required_text(page, "commitTimeStamp")?;
    let page_commit_id = required_text(page, "commitId")?;
    if page_commit_id != expected_commit_id {
        return Err(DiscoveryError::Protocol);
    }
    let previous = parse_nuget_cursor(previous_cursor)?;
    let high = parse_nuget_cursor(source_high_watermark)?.ok_or(DiscoveryError::Protocol)?;
    let page_key = (page_timestamp, page_commit_id);
    if previous
        .as_ref()
        .is_some_and(|previous| page_key <= (previous.0.as_str(), previous.1.as_str()))
        || page_key > (high.0.as_str(), high.1.as_str())
    {
        return Err(DiscoveryError::Protocol);
    }
    let declared_count = page
        .get("count")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(DiscoveryError::Protocol)?;
    let rows = page
        .get("items")
        .and_then(serde_json::Value::as_array)
        .ok_or(DiscoveryError::Protocol)?;
    if rows.len() > max_items {
        return Err(DiscoveryError::Bounds);
    }
    if rows.len() != declared_count {
        return Err(DiscoveryError::Protocol);
    }
    let mut leaves = rows
        .iter()
        .map(|item| {
            let item = item.as_object().ok_or(DiscoveryError::Protocol)?;
            let timestamp = required_text(item, "commitTimeStamp")?.to_owned();
            let url = required_text(item, "@id")?.to_owned();
            let commit_id = required_text(item, "commitId")?.to_owned();
            let _cursor = nuget_cursor(&timestamp, &commit_id)?;
            if !same_catalog_path(catalog_origin, &url) || timestamp.as_str() > page_timestamp {
                return Err(DiscoveryError::InvalidIdentity);
            }
            Ok(NugetCatalogLeafRef {
                url,
                commit_timestamp: timestamp,
                commit_id,
            })
        })
        .collect::<Result<Vec<_>, DiscoveryError>>()?;
    leaves.retain(|leaf| leaf.commit_timestamp.as_str() <= page_timestamp);
    leaves.sort_by(|left, right| {
        left.commit_timestamp
            .cmp(&right.commit_timestamp)
            .then_with(|| left.commit_id.cmp(&right.commit_id))
    });
    if leaves.len() > max_items {
        return Err(DiscoveryError::Bounds);
    }
    Ok(leaves)
}

/// Parses a NuGet catalog leaf. A deleted item is `Withdrawn`, an unlisted
/// `PackageDetails` item is `Yanked`, and a listed package is `Published`.
pub fn parse_nuget_catalog_leaf(bytes: &[u8]) -> Result<NugetCatalogEvent, DiscoveryError> {
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(DiscoveryError::Bounds);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| DiscoveryError::Protocol)?;
    let object = value.as_object().ok_or(DiscoveryError::Protocol)?;
    let package_id = required_text(object, "nuget:id")?;
    let version = required_text(object, "nuget:version")?;
    let timestamp = required_text(object, "commitTimeStamp")?.to_owned();
    let commit_id = required_text(object, "commitId")?.to_owned();
    let _cursor = nuget_cursor(&timestamp, &commit_id)?;
    let event_type = object.get("@type").ok_or(DiscoveryError::Protocol)?;
    let is_deleted = type_contains(event_type, "PackageDelete");
    let listed = object
        .get("listed")
        .or_else(|| object.get("nuget:listed"))
        .and_then(serde_json::Value::as_bool);
    let standing = if is_deleted {
        DiscoveryStanding::Withdrawn
    } else if listed == Some(false) {
        DiscoveryStanding::Yanked
    } else if listed == Some(true) {
        DiscoveryStanding::Published
    } else {
        return Err(DiscoveryError::Protocol);
    };
    let coordinate = PackageCoordinate::parse(format!("pkg:nuget/{package_id}@{version}"))
        .map_err(|_| DiscoveryError::InvalidIdentity)?;
    let proof = *blake3::hash(bytes).as_bytes();
    let mut metadata = DiscoveryMetadata::default();
    metadata.aliases = DiscoveryFacet::Absent;
    metadata.description = optional_text_facet(object, "description");
    metadata.keywords = match object.get("tags") {
        Some(serde_json::Value::Array(tags)) => DiscoveryFacet::Known(
            tags.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect(),
        ),
        Some(serde_json::Value::String(tags)) => {
            DiscoveryFacet::Known(tags.split_ascii_whitespace().map(str::to_owned).collect())
        }
        Some(serde_json::Value::Null) => DiscoveryFacet::Absent,
        Some(_) => DiscoveryFacet::Unknown,
        None => DiscoveryFacet::Unknown,
    };
    metadata.license = optional_text_facet(object, "licenseExpression");
    metadata.published_at = optional_text_facet(object, "published");
    metadata.deprecation = DiscoveryFacet::Absent;
    metadata.yanked = match standing {
        DiscoveryStanding::Yanked => DiscoveryFacet::Known(true),
        DiscoveryStanding::Published => DiscoveryFacet::Known(false),
        DiscoveryStanding::Withdrawn | DiscoveryStanding::RecipeAvailable => {
            DiscoveryFacet::Unknown
        }
    };
    metadata.advisories = DiscoveryFacet::Unknown;
    metadata.downloads = DiscoveryFacet::Absent;
    metadata.admit()?;
    Ok(NugetCatalogEvent {
        coordinate,
        standing,
        commit_timestamp: timestamp,
        commit_id,
        proof,
        metadata,
    })
}

/// One newest-version row from the crates.io recent-updates listing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CratesRecentRelease {
    pub coordinate: PackageCoordinate,
    pub updated_at: String,
    pub proof: [u8; 32],
    pub metadata: DiscoveryMetadata,
}

/// Parsed crates.io API page. This view is always `Windowed`: page offsets can
/// move while crates.io receives updates and it is not an event cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CratesRecentPage {
    pub requested_page: u32,
    pub total_pages: Option<u32>,
    pub next_cursor: DiscoveryCursor,
    pub completeness: DiscoveryCompleteness,
    pub releases: Vec<CratesRecentRelease>,
}

/// Exact release standing observed in one Cargo sparse-index package file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CratesSparseRelease {
    pub coordinate: PackageCoordinate,
    pub standing: DiscoveryStanding,
    pub proof: [u8; 32],
    pub metadata: DiscoveryMetadata,
}

/// A complete current version/yank snapshot for one known crates.io package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CratesSparsePackage {
    pub package_name: String,
    pub releases: Vec<CratesSparseRelease>,
}

/// Returns the Cargo sparse-index path for a validated crate name.
pub fn crates_sparse_index_path(name: &str) -> Result<String, DiscoveryError> {
    if name.is_empty()
        || name.len() > 256
        || !name
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'_' | b'-'))
    {
        return Err(DiscoveryError::InvalidIdentity);
    }
    let normalized = name.to_ascii_lowercase();
    let path = match normalized.len() {
        1 => format!("1/{normalized}"),
        2 => format!("2/{normalized}"),
        3 => format!("3/{}/{}", &normalized[..1], normalized),
        _ => format!("{}/{}/{}", &normalized[..2], &normalized[2..4], normalized),
    };
    Ok(path)
}

/// Parses the newline-delimited JSON version records served from
/// `https://index.crates.io/<sparse-index-path>` for one known crate. This
/// certifies current versions and yank flags only for that package.
pub fn parse_crates_sparse_package(
    bytes: &[u8],
    expected_name: &str,
    max_versions: usize,
) -> Result<CratesSparsePackage, DiscoveryError> {
    let normalized_name = expected_name.to_ascii_lowercase();
    let _ = crates_sparse_index_path(expected_name)?;
    if bytes.len() > 16 * 1024 * 1024
        || max_versions == 0
        || max_versions > MAX_DISCOVERY_PAGE_ITEMS
    {
        return Err(DiscoveryError::Bounds);
    }
    let mut releases = Vec::new();
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        if releases.len() >= max_versions {
            return Err(DiscoveryError::Bounds);
        }
        let value: serde_json::Value =
            serde_json::from_slice(line).map_err(|_| DiscoveryError::Protocol)?;
        let record = value.as_object().ok_or(DiscoveryError::Protocol)?;
        let name = required_text(record, "name")?;
        let version = required_text(record, "vers")?;
        if !name.eq_ignore_ascii_case(&normalized_name) {
            return Err(DiscoveryError::InvalidIdentity);
        }
        let yanked = record
            .get("yanked")
            .and_then(serde_json::Value::as_bool)
            .ok_or(DiscoveryError::Protocol)?;
        let coordinate = PackageCoordinate::parse(format!("pkg:cargo/{name}@{version}"))
            .map_err(|_| DiscoveryError::InvalidIdentity)?;
        let mut metadata = DiscoveryMetadata::default();
        metadata.aliases = DiscoveryFacet::Absent;
        metadata.description = DiscoveryFacet::Absent;
        metadata.keywords = DiscoveryFacet::Absent;
        metadata.license = DiscoveryFacet::Unknown;
        metadata.published_at = DiscoveryFacet::Unknown;
        metadata.deprecation = DiscoveryFacet::Absent;
        metadata.yanked = DiscoveryFacet::Known(yanked);
        metadata.advisories = DiscoveryFacet::Unknown;
        metadata.downloads = DiscoveryFacet::Absent;
        releases.push(CratesSparseRelease {
            coordinate,
            standing: if yanked {
                DiscoveryStanding::Yanked
            } else {
                DiscoveryStanding::Published
            },
            proof: *blake3::hash(line).as_bytes(),
            metadata,
        });
    }
    if releases.is_empty() {
        return Err(DiscoveryError::Protocol);
    }
    Ok(CratesSparsePackage {
        package_name: normalized_name,
        releases,
    })
}

/// Parses the public `GET /api/v1/crates?page=…&per_page=…&sort=recent-updates`
/// response. The result describes only each crate's `newest_version`; callers
/// that need complete version or yank facts must request its sparse-index file
/// using the package name.
pub fn parse_crates_recent_page(
    bytes: &[u8],
    requested_page: u32,
    page_size: usize,
    max_rows: usize,
) -> Result<CratesRecentPage, DiscoveryError> {
    if requested_page == 0
        || page_size == 0
        || page_size > MAX_DISCOVERY_PAGE_ITEMS
        || max_rows == 0
        || max_rows > page_size
        || bytes.len() > 16 * 1024 * 1024
    {
        return Err(DiscoveryError::Bounds);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| DiscoveryError::Protocol)?;
    let object = value.as_object().ok_or(DiscoveryError::Protocol)?;
    let rows = object
        .get("crates")
        .and_then(serde_json::Value::as_array)
        .ok_or(DiscoveryError::Protocol)?;
    if rows.len() > max_rows {
        return Err(DiscoveryError::Bounds);
    }
    let total = object
        .get("meta")
        .and_then(serde_json::Value::as_object)
        .and_then(|meta| meta.get("total"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let total_pages = total.map(|count| {
        let count = count as usize;
        count.div_ceil(page_size).max(1).min(u32::MAX as usize) as u32
    });
    let next_page = if total_pages.is_some_and(|total| requested_page >= total) {
        1
    } else {
        requested_page.saturating_add(1)
    };
    let next_cursor = DiscoveryCursor::new(next_page.to_be_bytes().to_vec())?;
    let mut releases = Vec::with_capacity(rows.len());
    for row in rows {
        let row = row.as_object().ok_or(DiscoveryError::Protocol)?;
        let name = required_text(row, "name")?;
        let version = row
            .get("newest_version")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                row.get("max_stable_version")
                    .and_then(serde_json::Value::as_str)
            })
            .ok_or(DiscoveryError::Protocol)?;
        let updated_at = required_text(row, "updated_at")?.to_owned();
        let coordinate = PackageCoordinate::parse(format!("pkg:cargo/{name}@{version}"))
            .map_err(|_| DiscoveryError::InvalidIdentity)?;
        let proof = *blake3::hash(
            serde_json::to_vec(row)
                .map_err(|_| DiscoveryError::Protocol)?
                .as_slice(),
        )
        .as_bytes();
        let mut metadata = DiscoveryMetadata::default();
        metadata.aliases = DiscoveryFacet::Absent;
        metadata.description = optional_text_facet(row, "description");
        metadata.keywords = match row.get("keywords") {
            Some(serde_json::Value::Array(keywords)) => DiscoveryFacet::Known(
                keywords
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            ),
            Some(serde_json::Value::Null) => DiscoveryFacet::Absent,
            Some(_) => DiscoveryFacet::Unknown,
            None => DiscoveryFacet::Unknown,
        };
        metadata.license = DiscoveryFacet::Unknown;
        metadata.published_at = DiscoveryFacet::Known(updated_at.clone());
        metadata.deprecation = DiscoveryFacet::Absent;
        metadata.yanked = DiscoveryFacet::Unknown;
        metadata.advisories = DiscoveryFacet::Unknown;
        metadata.downloads = DiscoveryFacet::Absent;
        metadata.admit()?;
        releases.push(CratesRecentRelease {
            coordinate,
            updated_at,
            proof,
            metadata,
        });
    }
    Ok(CratesRecentPage {
        requested_page,
        total_pages,
        next_cursor,
        completeness: DiscoveryCompleteness::Windowed,
        releases,
    })
}

fn required_text<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a str, DiscoveryError> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(DiscoveryError::Protocol)
}

fn required_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<u64, DiscoveryError> {
    object
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or(DiscoveryError::Protocol)
}

fn parse_npm_cursor(cursor: &DiscoveryCursor) -> Result<u64, DiscoveryError> {
    if cursor.is_empty() {
        return Ok(0);
    }
    let value = std::str::from_utf8(cursor.as_bytes()).map_err(|_| DiscoveryError::Protocol)?;
    value
        .strip_prefix("npm-v1:")
        .unwrap_or(value)
        .parse::<u64>()
        .map_err(|_| DiscoveryError::Protocol)
}

fn npm_cursor(sequence: u64) -> Result<DiscoveryCursor, DiscoveryError> {
    DiscoveryCursor::new(format!("npm-v1:{sequence:020}").into_bytes())
}

fn json_sequence(value: &serde_json::Value) -> Result<u64, DiscoveryError> {
    match value {
        serde_json::Value::Number(number) => number.as_u64().ok_or(DiscoveryError::Protocol),
        serde_json::Value::String(value) if !value.is_empty() => {
            value.parse::<u64>().map_err(|_| DiscoveryError::Protocol)
        }
        _ => Err(DiscoveryError::Protocol),
    }
}

fn valid_npm_name(value: &str) -> bool {
    if value.is_empty() || value.len() > 214 || !value.is_ascii() {
        return false;
    }
    let (scope, package) = match value.strip_prefix('@') {
        Some(scoped) => match scoped.split_once('/') {
            Some((scope, package)) if !scope.is_empty() && !package.is_empty() => {
                (Some(scope), package)
            }
            _ => return false,
        },
        None => (None, value),
    };
    let valid_component = |part: &str| {
        part != "."
            && part != ".."
            && part.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.')
            })
    };
    scope.is_none_or(valid_component) && valid_component(package)
}

fn optional_text_facet(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> DiscoveryFacet<String> {
    match object.get(field) {
        None => DiscoveryFacet::Unknown,
        Some(serde_json::Value::Null) => DiscoveryFacet::Absent,
        Some(serde_json::Value::String(value)) => DiscoveryFacet::Known(value.clone()),
        Some(_) => DiscoveryFacet::Unknown,
    }
}

fn optional_license_facet(
    object: &serde_json::Map<String, serde_json::Value>,
) -> DiscoveryFacet<String> {
    match object.get("license") {
        None => DiscoveryFacet::Unknown,
        Some(serde_json::Value::Null) => DiscoveryFacet::Absent,
        Some(serde_json::Value::String(value)) => DiscoveryFacet::Known(value.clone()),
        Some(serde_json::Value::Object(license)) => license
            .get("type")
            .and_then(serde_json::Value::as_str)
            .map(|value| DiscoveryFacet::Known(value.to_owned()))
            .unwrap_or(DiscoveryFacet::Unknown),
        Some(_) => DiscoveryFacet::Unknown,
    }
}

fn valid_pypi_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn normalize_pypi_name(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut separator = false;
    for character in value.chars() {
        if matches!(character, '-' | '_' | '.') {
            if !separator {
                normalized.push('-');
            }
            separator = true;
        } else {
            normalized.extend(character.to_lowercase());
            separator = false;
        }
    }
    normalized
}

fn valid_package_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/' | b'@' | b':')
        })
}

fn valid_go_module_path(value: &str) -> bool {
    if value.is_empty() || value.len() > 512 || !value.is_ascii() {
        return false;
    }
    let mut components = value.split('/');
    let Some(domain) = components.next() else {
        return false;
    };
    if !domain.contains('.')
        || domain.starts_with('-')
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain.contains("..")
        || !domain.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        })
    {
        return false;
    }
    components.all(|component| {
        !component.is_empty()
            && !component.starts_with('.')
            && !component.ends_with('.')
            && !component.contains("..")
            && component.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
            })
    })
}

fn type_contains(value: &serde_json::Value, suffix: &str) -> bool {
    let matches = |value: &str| value.rsplit(':').next().is_some_and(|kind| kind == suffix);
    value.as_str().is_some_and(matches)
        || value
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item.as_str().is_some_and(matches)))
}

fn same_origin(origin: &str, url: &str) -> bool {
    let Some(origin_end) = origin.find("://").map(|scheme| {
        origin[scheme + 3..]
            .find('/')
            .map_or(origin.len(), |slash| scheme + 3 + slash)
    }) else {
        return false;
    };
    let source_origin = &origin[..origin_end];
    let Some(url_end) = url.find("://").map(|scheme| {
        url[scheme + 3..]
            .find('/')
            .map_or(url.len(), |slash| scheme + 3 + slash)
    }) else {
        return false;
    };
    url[..url_end].eq_ignore_ascii_case(source_origin)
}

const NUGET_CURSOR_SEPARATOR: char = '\u{1f}';

fn nuget_cursor(timestamp: &str, commit_id: &str) -> Result<DiscoveryCursor, DiscoveryError> {
    if timestamp.is_empty()
        || timestamp.len() > 128
        || timestamp.contains(NUGET_CURSOR_SEPARATOR)
        || timestamp.bytes().any(|byte| byte.is_ascii_control())
        || commit_id.is_empty()
        || commit_id.len() > 256
        || commit_id.contains(NUGET_CURSOR_SEPARATOR)
        || commit_id.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(DiscoveryError::Protocol);
    }
    DiscoveryCursor::new(format!("{timestamp}{NUGET_CURSOR_SEPARATOR}{commit_id}").into_bytes())
}

fn parse_nuget_cursor(
    cursor: &DiscoveryCursor,
) -> Result<Option<(String, String)>, DiscoveryError> {
    if cursor.is_empty() {
        return Ok(None);
    }
    let text = std::str::from_utf8(cursor.as_bytes()).map_err(|_| DiscoveryError::Protocol)?;
    let (timestamp, commit_id) = text
        .split_once(NUGET_CURSOR_SEPARATOR)
        // Timestamp-only cursors were emitted by the first journal format;
        // treating their commit id as empty safely replays equal-time pages.
        .unwrap_or((text, ""));
    if timestamp.is_empty()
        || timestamp.len() > 128
        || timestamp.bytes().any(|byte| byte.is_ascii_control())
        || commit_id.len() > 256
        || commit_id.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(DiscoveryError::Protocol);
    }
    Ok(Some((timestamp.to_owned(), commit_id.to_owned())))
}

fn same_catalog_path(catalog_index: &str, candidate: &str) -> bool {
    if !same_origin(catalog_index, candidate) {
        return false;
    }
    fn path(value: &str) -> Option<&str> {
        let scheme_end = value.find("://")? + 3;
        let authority_end = scheme_end + value[scheme_end..].find('/')?;
        let path = value[authority_end..]
            .split(['?', '#'])
            .next()
            .unwrap_or_default();
        Some(if path.is_empty() { "/" } else { path })
    }
    let (Some(index_path), Some(candidate_path)) = (path(catalog_index), path(candidate)) else {
        return false;
    };
    let Some((base, _)) = index_path.rsplit_once('/') else {
        return candidate_path.starts_with('/');
    };
    let base = if base.is_empty() { "/" } else { base };
    let prefix = if base == "/" {
        base
    } else {
        // Require a segment boundary so `/catalog0-evil` is outside the feed.
        if !candidate_path.starts_with(base) {
            return false;
        }
        &candidate_path[..base.len()]
    };
    let within_prefix = candidate_path.starts_with(prefix)
        && (prefix == "/"
            || candidate_path.len() == prefix.len()
            || candidate_path.as_bytes().get(prefix.len()) == Some(&b'/'));
    let encoded_path = candidate_path.to_ascii_lowercase();
    within_prefix
        && !candidate_path.contains('\\')
        && !encoded_path.contains("%2e")
        && !encoded_path.contains("%2f")
        && !encoded_path.contains("%5c")
        && candidate_path
            .split('/')
            .all(|component| component != "." && component != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(value: &str) -> DiscoveryCursor {
        DiscoveryCursor::new(value.as_bytes().to_vec()).expect("bounded cursor")
    }

    #[test]
    fn nuget_catalog_page_and_leaf_preserve_unlist_and_delete() {
        let origin = "http://127.0.0.1:8080/v3/catalog0/index.json";
        let plan = parse_nuget_catalog_index(
            br#"{"commitId":"tip","commitTimeStamp":"2026-09-01T00:00:00Z","count":1,"items":[{"@id":"http://127.0.0.1:8080/v3/catalog0/page2.json","commitId":"tip","commitTimeStamp":"2026-09-01T00:00:00Z","count":2}]}"#,
            origin,
            &cursor("2026-08-31T00:00:00Z"),
            10,
        )
        .expect("catalog index");
        assert_eq!(plan.pages.len(), 1);
        assert_eq!(
            plan.completeness,
            DiscoveryCompleteness::CompleteThroughCursor
        );
        assert!(plan.is_caught_up);
        let refs = parse_nuget_catalog_page(
            br#"{"commitId":"tip","commitTimeStamp":"2026-09-01T00:00:00Z","count":2,"items":[{"@id":"http://127.0.0.1:8080/v3/catalog0/data/a.json","commitTimeStamp":"2026-09-01T00:00:00Z","commitId":"one"},{"@id":"http://127.0.0.1:8080/v3/catalog0/data/b.json","commitTimeStamp":"2026-09-01T00:00:00Z","commitId":"two"}]}"#,
            origin,
            &cursor("2026-08-31T00:00:00Z"),
            &plan.source_high_watermark,
            &plan.pages[0].commit_id,
            10,
        )
        .expect("catalog page");
        assert_eq!(refs.len(), 2);
        let unlisted = parse_nuget_catalog_leaf(
            br#"{"@type":"nuget:PackageDetails","commitTimeStamp":"2026-09-01T00:00:00Z","commitId":"one","nuget:id":"Widget","nuget:version":"1.2.3","listed":false}"#,
        )
        .expect("unlisted package leaf");
        assert_eq!(unlisted.standing, DiscoveryStanding::Yanked);
        assert_eq!(unlisted.coordinate.as_str(), "pkg:nuget/Widget@1.2.3");
        let deleted = parse_nuget_catalog_leaf(
            br#"{"@type":["Catalog","nuget:PackageDelete"],"commitTimeStamp":"2026-09-01T00:00:00Z","commitId":"two","nuget:id":"Widget","nuget:version":"1.2.3"}"#,
        )
        .expect("deleted package leaf");
        assert_eq!(deleted.standing, DiscoveryStanding::Withdrawn);
    }

    #[test]
    fn nuget_catalog_refuses_cross_origin_page_and_marks_cold_bootstrap_windowed() {
        let origin = "https://api.nuget.org/v3/catalog0/index.json";
        let index = br#"{"commitId":"tip","commitTimeStamp":"2026-09-02T00:00:00Z","count":1,"items":[{"@id":"https://attacker.invalid/page.json","commitId":"tip","commitTimeStamp":"2026-09-02T00:00:00Z","count":1}]}"#;
        assert_eq!(
            parse_nuget_catalog_index(index, origin, &DiscoveryCursor::default(), 10),
            Err(DiscoveryError::InvalidIdentity)
        );
        let index = br#"{"commitId":"tip","commitTimeStamp":"2026-09-02T00:00:00Z","count":2,"items":[{"@id":"https://api.nuget.org/v3/catalog0/page0.json","commitId":"older","commitTimeStamp":"2026-09-01T00:00:00Z","count":1},{"@id":"https://api.nuget.org/v3/catalog0/page1.json","commitId":"tip","commitTimeStamp":"2026-09-02T00:00:00Z","count":1}]}"#;
        let plan = parse_nuget_catalog_index(index, origin, &DiscoveryCursor::default(), 1)
            .expect("bounded initial catalog");
        assert_eq!(plan.pages.len(), 1);
        assert_eq!(
            plan.completeness,
            DiscoveryCompleteness::CompleteThroughCursor
        );
        assert!(!plan.is_caught_up);
    }

    #[test]
    fn nuget_catalog_cursor_orders_equal_timestamps_and_rejects_sibling_paths() {
        let origin = "https://api.nuget.org/v3/catalog0/index.json";
        let timestamp = "2026-09-02T00:00:00Z";
        let previous = nuget_cursor(timestamp, "a").expect("previous event cursor");
        let index = br#"{"commitId":"z","commitTimeStamp":"2026-09-02T00:00:00Z","count":2,"items":[{"@id":"https://api.nuget.org/v3/catalog0/page-a.json","commitId":"a","commitTimeStamp":"2026-09-02T00:00:00Z","count":1},{"@id":"https://api.nuget.org/v3/catalog0/page-z.json","commitId":"z","commitTimeStamp":"2026-09-02T00:00:00Z","count":1}] }"#;
        let plan = parse_nuget_catalog_index(index, origin, &previous, 10)
            .expect("total ordered page cursor");
        assert_eq!(plan.pages.len(), 1);
        assert_eq!(plan.pages[0].commit_id, "z");
        assert!(plan.is_caught_up);
        let sibling_path = br#"{"commitId":"z","commitTimeStamp":"2026-09-02T00:00:00Z","count":1,"items":[{"@id":"https://api.nuget.org/v3/catalog0-evil/page.json","commitId":"z","commitTimeStamp":"2026-09-02T00:00:00Z","count":1}]}"#;
        assert_eq!(
            parse_nuget_catalog_index(sibling_path, origin, &DiscoveryCursor::default(), 10),
            Err(DiscoveryError::InvalidIdentity)
        );
        let same_origin_other_path = br#"{"commitId":"z","commitTimeStamp":"2026-09-02T00:00:00Z","count":1,"items":[{"@id":"https://api.nuget.org/v3/other/page.json","commitId":"z","commitTimeStamp":"2026-09-02T00:00:00Z","count":1}]}"#;
        assert_eq!(
            parse_nuget_catalog_index(
                same_origin_other_path,
                origin,
                &DiscoveryCursor::default(),
                10
            ),
            Err(DiscoveryError::InvalidIdentity)
        );
    }

    #[test]
    fn crates_io_recent_updates_are_never_reported_as_a_complete_feed() {
        let page = parse_crates_recent_page(
            br#"{"crates":[{"name":"serde","newest_version":"1.0.0","max_stable_version":"1.0.0","updated_at":"2026-09-01T12:00:00Z"}],"meta":{"total":101}}"#,
            1,
            100,
            100,
        )
        .expect("crates listing");
        assert_eq!(page.completeness, DiscoveryCompleteness::Windowed);
        assert_eq!(page.total_pages, Some(2));
        assert_eq!(page.next_cursor.as_bytes(), &2_u32.to_be_bytes());
        assert_eq!(
            page.releases[0].coordinate.as_str(),
            "pkg:cargo/serde@1.0.0"
        );
    }

    #[test]
    fn cargo_sparse_index_paths_and_yanks_are_complete_for_one_name() {
        assert_eq!(crates_sparse_index_path("a"), Ok("1/a".to_owned()));
        assert_eq!(crates_sparse_index_path("ab"), Ok("2/ab".to_owned()));
        assert_eq!(crates_sparse_index_path("abc"), Ok("3/a/abc".to_owned()));
        assert_eq!(
            crates_sparse_index_path("abcd"),
            Ok("ab/cd/abcd".to_owned())
        );
        assert_eq!(
            crates_sparse_index_path("Serde"),
            Ok("se/rd/serde".to_owned())
        );
        let sparse_index_fixture = br#"{"name":"serde","vers":"1.0.94","deps":[{"name":"serde_derive","req":"^1.0","features":[],"optional":true,"default_features":true,"target":null,"kind":"normal"},{"name":"serde_derive","req":"^1.0","features":[],"optional":false,"default_features":true,"target":null,"kind":"dev"}],"cksum":"076a696fdea89c19d3baed462576b8f6d663064414b5c793642da8dfeb99475b","features":{"alloc":["unstable"],"default":["std"],"derive":["serde_derive"],"rc":[],"std":[],"unstable":[]},"yanked":false,"pubtime":"2019-06-27T17:55:32Z"}
{"name":"serde","vers":"1.0.95","deps":[{"name":"serde_derive","req":"^1.0","features":[],"optional":true,"default_features":true,"target":null,"kind":"normal"},{"name":"serde_derive","req":"^1.0","features":[],"optional":false,"default_features":true,"target":null,"kind":"dev"}],"cksum":"e47a9fd6b2d2d2330b19b0b3e5248a170a5acd6356fd88c7bb30362ef9c70567","features":{"alloc":[],"default":["std"],"derive":["serde_derive"],"rc":[],"std":[],"unstable":[]},"yanked":true,"pubtime":"2019-07-16T17:25:50Z"}
"#;
        let package = parse_crates_sparse_package(sparse_index_fixture, "serde", 10)
            .expect("Cargo sparse package");
        assert_eq!(package.releases.len(), 2);
        assert_eq!(
            package.releases[0].coordinate.as_str(),
            "pkg:cargo/serde@1.0.94"
        );
        assert_eq!(package.releases[0].standing, DiscoveryStanding::Published);
        assert_eq!(package.releases[1].standing, DiscoveryStanding::Yanked);
        assert_eq!(
            package.releases[0].metadata.yanked,
            DiscoveryFacet::Known(false)
        );
        assert_eq!(
            package.releases[1].metadata.yanked,
            DiscoveryFacet::Known(true)
        );
        assert_eq!(
            package.releases[0].metadata.downloads,
            DiscoveryFacet::Absent
        );
        let mutated_identity = std::str::from_utf8(sparse_index_fixture)
            .expect("fixture is UTF-8")
            .replacen("\"name\":\"serde\"", "\"name\":\"other\"", 1);
        assert_eq!(
            parse_crates_sparse_package(mutated_identity.as_bytes(), "serde", 10),
            Err(DiscoveryError::InvalidIdentity)
        );
    }

    #[test]
    fn go_index_preserves_module_path_case_and_windowed_coverage() {
        let page = parse_go_module_index_page(
            br#"{"Path":"golang.org/x/text","Version":"v0.3.0","Timestamp":"2019-04-10T19:08:52.997264Z"}
{"Path":"github.com/FiloSottile/mkcert","Version":"v1.3.0","Timestamp":"2019-04-10T20:30:31.325463Z"}
"#,
            &DiscoveryCursor::default(),
            2,
        )
        .expect("Go module index fixture");
        let independent_expected = vec![
            "pkg:golang/golang.org/x/text@v0.3.0",
            "pkg:golang/github.com/FiloSottile/mkcert@v1.3.0",
        ];
        assert_eq!(
            page.releases
                .iter()
                .map(|release| release.coordinate.as_str())
                .collect::<Vec<_>>(),
            independent_expected
        );
        assert_eq!(page.completeness, DiscoveryCompleteness::Windowed);
        assert_eq!(
            page.releases[1].metadata.published_at,
            DiscoveryFacet::Known("2019-04-10T20:30:31.325463Z".to_owned())
        );
        assert_eq!(page.releases[1].metadata.yanked, DiscoveryFacet::Absent);
        assert_eq!(
            page.releases[1].metadata.deprecation,
            DiscoveryFacet::Unknown
        );
        assert_eq!(page.next_cursor.as_bytes(), b"2019-04-10T20:30:31.325463Z");
        let unsafe_path = br#"{"Path":"github.com/example/../escape","Version":"v1.0.0","Timestamp":"2026-09-02T00:00:00Z"}"#;
        assert_eq!(
            parse_go_module_index_page(unsafe_path, &DiscoveryCursor::default(), 1),
            Err(DiscoveryError::InvalidIdentity)
        );
        let proxy_escaped_path = br#"{"Path":"github.com/!azure/azure-sdk-for-go","Version":"v1.2.3","Timestamp":"2026-09-02T00:00:00Z"}"#;
        assert_eq!(
            parse_go_module_index_page(proxy_escaped_path, &DiscoveryCursor::default(), 1),
            Err(DiscoveryError::InvalidIdentity)
        );
    }

    #[test]
    fn npm_changes_fetch_current_packument_and_keep_partial_metadata_typed() {
        let high = npm_cursor(3).expect("captured update sequence");
        let page = parse_npm_changes_page(
            br#"{"results":[{"seq":1,"id":"zod","changes":[{"rev":"7-a1b2"}]}],"last_seq":1,"pending":2}"#,
            &DiscoveryCursor::default(),
            &high,
            10,
        )
        .expect("npm changes page");
        assert_eq!(page.next_cursor, npm_cursor(1).expect("event cursor"));
        assert_eq!(
            page.completeness,
            DiscoveryCompleteness::CompleteThroughCursor
        );
        assert!(page.packages[0].requires_packument);
        assert_eq!(page.packages[0].revision.as_deref(), Some("7-a1b2"));
        let packument = parse_npm_packument_document(
            br#"{"name":"zod","_rev":"7-a1b2","description":"Schema validation","keywords":["schema","typescript"],"versions":{"1.0.0":{"license":"MIT"},"2.0.0":{"deprecated":"replaced"}},"time":{"1.0.0":"2025-01-01T00:00:00.000Z","2.0.0":"2026-01-01T00:00:00.000Z"}}"#,
            "zod",
            1,
            1,
        )
        .expect("current packument");
        assert_eq!(packument.revision.as_deref(), Some("7-a1b2"));
        assert!(packument.is_truncated);
        assert_eq!(packument.releases.len(), 1);
        assert_eq!(
            packument.releases[0].coordinate.as_str(),
            "pkg:npm/zod@1.0.0"
        );
        assert_eq!(
            packument.releases[0].metadata.license,
            DiscoveryFacet::Known("MIT".to_owned())
        );
        assert_eq!(
            packument.releases[0].metadata.description,
            DiscoveryFacet::Known("Schema validation".to_owned())
        );
        let future =
            br#"{"results":[{"seq":4,"id":"zod","changes":[{"rev":"8-c3d4"}]}],"last_seq":4}"#;
        assert_eq!(
            parse_npm_changes_page(future, &DiscoveryCursor::default(), &high, 10),
            Err(DiscoveryError::Protocol)
        );
    }

    #[test]
    fn pep691_and_pypi_json_preserve_yanks_and_fixed_version_advisory_scope() {
        let projects = parse_pypi_project_list(
            br#"{"meta":{},"projects":[{"name":"Django"},{"name":"my_pkg"}]}"#,
            10,
        )
        .expect("PEP 691 project set without optional serial");
        assert_eq!(projects.serial, None);
        assert_eq!(projects.projects[0].canonical_name, "django");
        assert_eq!(projects.projects[1].canonical_name, "my-pkg");
        let project = parse_pypi_project_metadata(
            br#"{"info":{"name":"my_pkg","version":"2.0","summary":"Small parser","license":"MIT","keywords":"parse, json"},"vulnerabilities":[{"id":"GHSA-abcd-1234","aliases":["CVE-2026-12345"],"details":"Input validation issue","fixed_in":["2.0"]}],"releases":{"1.0":[{"yanked":true,"upload_time_iso_8601":"2025-01-01T00:00:00Z"}],"2.0":[{"yanked":false,"upload_time_iso_8601":"2026-01-01T00:00:00Z"}]}}"#,
            "MY-PKG",
            10,
        )
        .expect("PyPI project JSON");
        assert_eq!(project.latest_version.as_deref(), Some("2.0"));
        assert_eq!(project.releases.len(), 2);
        let old = project
            .releases
            .iter()
            .find(|release| release.coordinate.as_str() == "pkg:pypi/my-pkg@1.0")
            .expect("older release");
        assert_eq!(old.standing, DiscoveryStanding::Yanked);
        let latest = project
            .releases
            .iter()
            .find(|release| release.coordinate.as_str() == "pkg:pypi/my-pkg@2.0")
            .expect("latest release");
        assert_eq!(latest.standing, DiscoveryStanding::Published);
        assert_eq!(
            latest.metadata.description,
            DiscoveryFacet::Known("Small parser".to_owned())
        );
        let DiscoveryFacet::Known(advisories) = &latest.metadata.advisories else {
            panic!("the source vulnerability list must remain known");
        };
        assert_eq!(advisories[0].id, "GHSA-abcd-1234");
        assert_eq!(
            advisories[0].fixed_in,
            DiscoveryFacet::Known(vec!["2.0".to_owned()])
        );
    }

    #[test]
    fn maven_and_conan_fixtures_distinguish_release_from_recipe_evidence() {
        let page = parse_maven_search_page(
            br#"{"response":{"numFound":500,"docs":[{"g":"com.acme","a":"codec","latestVersion":"2.1","timestamp":1234},{"g":"org.demo","a":"tool","v":"1.0","timestamp":1230}]}}"#,
            0,
            2,
        )
        .expect("Maven Central search snapshot");
        assert_eq!(page.total_results, 500);
        assert_eq!(page.next_offset, 2);
        assert_eq!(page.completeness, DiscoveryCompleteness::Windowed);
        assert_eq!(
            page.releases[0].coordinate.as_str(),
            "pkg:maven/com.acme/codec@2.1"
        );
        assert_eq!(
            page.releases[0].metadata.published_at,
            DiscoveryFacet::Known("1234".to_owned())
        );
        assert_eq!(page.releases[0].metadata.downloads, DiscoveryFacet::Absent);

        let tree = parse_conan_recipe_tree(
            br#"{"sha":"abc123","truncated":false,"tree":[{"path":"recipes/zlib/config.yml","type":"blob"}]}"#,
            10,
        )
        .expect("Conan Center recipe tree");
        assert_eq!(tree.tree_sha, "abc123");
        assert_eq!(tree.recipes[0].config_path, "recipes/zlib/config.yml");
        let releases = parse_conan_recipe_versions(
            b"versions:\n  1.2.13:\n    folder: all\n  1.3.0:\n    folder: all\n",
            "zlib",
            10,
        )
        .expect("Conan recipe versions");
        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].standing, DiscoveryStanding::RecipeAvailable);
        assert_eq!(releases[0].metadata.published_at, DiscoveryFacet::Unknown);
        assert_eq!(releases[0].metadata.downloads, DiscoveryFacet::Absent);
        assert!(
            parse_conan_recipe_versions(
                b"versions:\n  bad/version:\n    folder: all\n",
                "zlib",
                10
            )
            .is_err()
        );
    }

    #[test]
    fn nuget_batch_requires_source_matched_facts_and_bounded_cursors() {
        let endpoint =
            RegistryEndpoint::new(RegistryEcosystem::Nuget, "https://nuget.example.test")
                .expect("source endpoint");
        let source = DiscoverySourceIdentity::from_endpoint(&endpoint);
        let coordinate = PackageCoordinate::parse("pkg:nuget/Widget@1.0.0").expect("coordinate");
        let batch = DiscoveryBatch {
            source,
            previous_cursor: cursor("2026-09-01T00:00:00Z"),
            next_cursor: cursor("2026-09-02T00:00:00Z"),
            source_high_watermark: cursor("2026-09-02T00:00:00Z"),
            caught_up: true,
            observed_at: DiscoveryObservedAt::from_unix_millis(10),
            completeness: DiscoveryCompleteness::CompleteThroughCursor,
            facts: vec![DiscoveryFact {
                source,
                coordinate,
                standing: DiscoveryStanding::Published,
                observed_at: DiscoveryObservedAt::from_unix_millis(10),
                source_event_time: None,
                proof: [1; 32],
                metadata: DiscoveryMetadata::default(),
            }],
            package_retractions: Vec::new(),
        };
        assert_eq!(batch.admit(), Ok(()));
        assert_eq!(
            DiscoveryCursor::new(vec![0; MAX_DISCOVERY_CURSOR_BYTES + 1]),
            Err(DiscoveryError::Bounds)
        );
    }
}
