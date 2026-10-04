//! Transport-neutral, bounded source discovery facts and progress values.
//!
//! Endpoint admission, source parsers, and network requests remain in the
//! engine and local service. This module owns only the typed fact and batch
//! contract that the durable discovery authority accepts.

use backend_semantic::vocabulary::{PackageUrl as PackageCoordinate, RegistryEcosystem};
use serde::{Deserialize, Serialize, de::Error as _};
use std::{fmt, io};

/// Upper bound for an opaque source cursor persisted by a local discovery
/// owner. Source cursors are never interpreted by generic storage code.
pub const MAX_DISCOVERY_CURSOR_BYTES: usize = 4096;
/// Upper bound for one source page before it is admitted into the durable
/// discovery journal.
pub const MAX_DISCOVERY_PAGE_ITEMS: usize = 4096;
/// Bound for a source-only package-name snapshot (not a release fact page).
pub const MAX_DISCOVERY_PROJECTS: usize = 2_000_000;
/// Maximum canonical JSON batch body admitted before durable transaction work.
pub const MAX_DISCOVERY_BATCH_ENCODED_BYTES: usize = 32 * 1024 * 1024;
/// Maximum retained spelling of one source event timestamp or key.
pub const MAX_DISCOVERY_EVENT_TEXT_BYTES: usize = 512;
/// Maximum retained size of one npm source revision.
pub const MAX_DISCOVERY_REVISION_BYTES: usize = 256;
/// Maximum retained size of one NuGet catalog commit identifier.
pub const MAX_DISCOVERY_COMMIT_ID_BYTES: usize = 256;

/// Stable identity of the source that made a catalog claim.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct DiscoverySourceIdentity {
    ecosystem: RegistryEcosystem,
    id: [u8; 32],
}

impl DiscoverySourceIdentity {
    /// Claims a source identity from its closed ecosystem and stable id.
    ///
    /// This is a value constructor, not proof that a registry endpoint was
    /// authenticated or admitted. Source adapters must derive the id from an
    /// admitted credential-free endpoint before submitting a batch.
    #[must_use]
    pub const fn from_parts(ecosystem: RegistryEcosystem, id: [u8; 32]) -> Self {
        Self { ecosystem, id }
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

/// Normalized UTC time used to order NuGet catalog events.
///
/// The source spelling remains on [`DiscoveryFact::source_event_time`]. This
/// value avoids comparing RFC 3339 strings whose offsets or fractional-second
/// precision differ.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DiscoveryTimestamp {
    unix_seconds: i64,
    nanoseconds: u32,
}

impl<'de> Deserialize<'de> for DiscoveryTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Repr {
            unix_seconds: i64,
            nanoseconds: u32,
        }

        let repr = Repr::deserialize(deserializer)?;
        if repr.nanoseconds >= 1_000_000_000 {
            return Err(D::Error::custom(
                "discovery timestamp nanoseconds out of range",
            ));
        }
        Ok(Self {
            unix_seconds: repr.unix_seconds,
            nanoseconds: repr.nanoseconds,
        })
    }
}

impl DiscoveryTimestamp {
    /// Parses a bounded RFC 3339 timestamp and normalizes it to UTC.
    pub fn parse_rfc3339(value: &str) -> Result<Self, DiscoveryError> {
        let bytes = value.as_bytes();
        if value.len() < 20 || value.len() > 40 || !value.is_ascii() {
            return Err(DiscoveryError::Bounds);
        }
        if bytes[4] != b'-'
            || bytes[7] != b'-'
            || !matches!(bytes[10], b'T' | b't')
            || bytes[13] != b':'
            || bytes[16] != b':'
        {
            return Err(DiscoveryError::Protocol);
        }
        let number = |start: usize, end: usize| -> Result<i64, DiscoveryError> {
            value
                .get(start..end)
                .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()))
                .and_then(|digits| digits.parse::<i64>().ok())
                .ok_or(DiscoveryError::Protocol)
        };
        let year = number(0, 4)?;
        let month = number(5, 7)?;
        let day = number(8, 10)?;
        let hour = number(11, 13)?;
        let minute = number(14, 16)?;
        let second = number(17, 19)?;
        if year == 0
            || !(1..=12).contains(&month)
            || !(0..=23).contains(&hour)
            || !(0..=59).contains(&minute)
            || !(0..=59).contains(&second)
        {
            return Err(DiscoveryError::Protocol);
        }
        let leap_year = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let month_days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap_year => 29,
            2 => 28,
            _ => return Err(DiscoveryError::Protocol),
        };
        if day == 0 || day > month_days {
            return Err(DiscoveryError::Protocol);
        }

        let mut cursor = 19;
        let mut nanoseconds = 0_u32;
        if bytes.get(cursor) == Some(&b'.') {
            cursor += 1;
            let start = cursor;
            while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
                cursor += 1;
            }
            let digits = cursor - start;
            if digits == 0 || digits > 9 {
                return Err(DiscoveryError::Protocol);
            }
            let fraction = value[start..cursor]
                .parse::<u32>()
                .map_err(|_| DiscoveryError::Protocol)?;
            nanoseconds = fraction
                .checked_mul(10_u32.pow(u32::try_from(9 - digits).unwrap_or(0)))
                .ok_or(DiscoveryError::Bounds)?;
        }
        let offset_seconds = match bytes.get(cursor).copied() {
            Some(b'Z' | b'z') if cursor + 1 == bytes.len() => 0_i64,
            Some(sign @ (b'+' | b'-')) if cursor + 6 == bytes.len() => {
                if bytes[cursor + 3] != b':'
                    || !bytes[cursor + 1].is_ascii_digit()
                    || !bytes[cursor + 2].is_ascii_digit()
                    || !bytes[cursor + 4].is_ascii_digit()
                    || !bytes[cursor + 5].is_ascii_digit()
                {
                    return Err(DiscoveryError::Protocol);
                }
                let offset_hour =
                    i64::from(bytes[cursor + 1] - b'0') * 10 + i64::from(bytes[cursor + 2] - b'0');
                let offset_minute =
                    i64::from(bytes[cursor + 4] - b'0') * 10 + i64::from(bytes[cursor + 5] - b'0');
                if offset_hour > 23 || offset_minute > 59 {
                    return Err(DiscoveryError::Protocol);
                }
                let magnitude = offset_hour * 3600 + offset_minute * 60;
                if sign == b'+' { magnitude } else { -magnitude }
            }
            _ => return Err(DiscoveryError::Protocol),
        };

        let days = days_from_civil(year, month, day);
        let local_seconds = days
            .checked_mul(86_400)
            .and_then(|seconds| seconds.checked_add(hour * 3600 + minute * 60 + second))
            .ok_or(DiscoveryError::Bounds)?;
        let unix_seconds = local_seconds
            .checked_sub(offset_seconds)
            .ok_or(DiscoveryError::Bounds)?;
        Ok(Self {
            unix_seconds,
            nanoseconds,
        })
    }

    /// Whole seconds since 1970-01-01T00:00:00Z.
    #[must_use]
    pub const fn unix_seconds(self) -> i64 {
        self.unix_seconds
    }

    /// Nanoseconds within `unix_seconds`.
    #[must_use]
    pub const fn nanoseconds(self) -> u32 {
        self.nanoseconds
    }
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = year - if month <= 2 { 1 } else { 0 };
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Closed provenance/order evidence for a source event.
///
/// Revision spelling remains separately available on the fact. These variants
/// encode only ordering evidence whose source protocol defines it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiscoverySourceEvent {
    /// The source has no event identity, and the ecosystem does not require a
    /// typed feed event. Accepted local progress CAS governs head replacement.
    Unordered,
    /// An explicit point-in-time snapshot for a feed ecosystem that otherwise
    /// has typed event provenance. It carries no upstream ordering claim.
    Snapshot,
    /// An exact NuGet catalog commit. `commit_id` identifies the event but is
    /// opaque and never used to order equal timestamps.
    NugetCatalog {
        timestamp: DiscoveryTimestamp,
        commit_id: String,
    },
    /// An exact npm changes row. Only `sequence` is ordered; revision and row
    /// proof are equality evidence.
    NpmChange {
        sequence: u64,
        revision: Option<String>,
        change_proof: [u8; 32],
    },
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
    /// Alternate canonical identifiers: missing is `Unknown`, null is
    /// `Absent`, and an array (including an empty one) is `Known`.
    pub aliases: DiscoveryFacet<Vec<String>>,
    /// Short source-provided advisory description.
    pub summary: DiscoveryFacet<String>,
    /// Source-provided severity text; no normalized score is inferred.
    pub severity: DiscoveryFacet<String>,
    /// Source-provided versions at which the issue is fixed, when available.
    /// The adapter preserves this scope without inferring version ordering.
    #[serde(default)]
    pub fixed_in: DiscoveryFacet<Vec<String>>,
}

/// One feature declaration from a Cargo sparse-index row. `features` and
/// `features2` remain separate because Cargo gives the latter newer feature
/// syntax semantics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CratesSparseFeature {
    pub name: String,
    pub members: Vec<String>,
}

/// One exact dependency declaration from a Cargo sparse-index row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CratesSparseDependency {
    pub name: String,
    pub requirement: String,
    #[serde(default)]
    pub package: DiscoveryFacet<String>,
    #[serde(default)]
    pub features: DiscoveryFacet<Vec<String>>,
    #[serde(default)]
    pub optional: DiscoveryFacet<bool>,
    #[serde(default)]
    pub default_features: DiscoveryFacet<bool>,
    #[serde(default)]
    pub target: DiscoveryFacet<String>,
    #[serde(default)]
    pub kind: DiscoveryFacet<String>,
    #[serde(default)]
    pub registry: DiscoveryFacet<String>,
}

/// Cargo-specific release facts from one sparse-index version record.
/// Missing arrays stay `Unknown`; an explicit empty array is `Known([])`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CratesSparseMetadata {
    pub checksum: String,
    pub schema_version: u32,
    #[serde(default)]
    pub rust_version: DiscoveryFacet<String>,
    #[serde(default)]
    pub links: DiscoveryFacet<String>,
    #[serde(default)]
    pub features: DiscoveryFacet<Vec<CratesSparseFeature>>,
    #[serde(default)]
    pub features2: DiscoveryFacet<Vec<CratesSparseFeature>>,
    #[serde(default)]
    pub dependencies: DiscoveryFacet<Vec<CratesSparseDependency>>,
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
    /// Cargo sparse-index source record, when this release came from Cargo.
    #[serde(default)]
    pub cargo_sparse: DiscoveryFacet<CratesSparseMetadata>,
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
            cargo_sparse: DiscoveryFacet::Unknown,
        }
    }
}

impl DiscoveryMetadata {
    /// Checks source text and collection bounds before it enters the journal.
    pub fn admit(&self) -> Result<(), DiscoveryError> {
        fn strings_valid(values: &[String], maximum_items: usize, maximum_bytes: usize) -> bool {
            values.len() <= maximum_items
                && values.iter().all(|value| {
                    !value.is_empty() && value.len() <= maximum_bytes && !value.contains('\0')
                })
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
                            && !advisory.id.bytes().any(|byte| byte.is_ascii_control())
                            && match &advisory.aliases {
                                DiscoveryFacet::Known(values) => strings_valid(values, 32, 256),
                                DiscoveryFacet::Absent | DiscoveryFacet::Unknown => true,
                            }
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
        let cargo_sparse_valid = match &self.cargo_sparse {
            DiscoveryFacet::Known(metadata) => {
                metadata.checksum.len() == 64
                    && metadata
                        .checksum
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
                    && matches!(metadata.schema_version, 1 | 2)
                    && cargo_string_facet_valid(&metadata.rust_version, 128)
                    && cargo_string_facet_valid(&metadata.links, 256)
                    && cargo_sparse_features_valid(&metadata.features)
                    && cargo_sparse_features_valid(&metadata.features2)
                    && cargo_sparse_dependencies_valid(&metadata.dependencies)
            }
            _ => true,
        };
        if !lists_valid
            || !advisories_valid
            || !cargo_sparse_valid
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

fn cargo_string_facet_valid(value: &DiscoveryFacet<String>, maximum_bytes: usize) -> bool {
    !matches!(value, DiscoveryFacet::Known(text) if text.is_empty() || text.len() > maximum_bytes || text.contains('\0'))
}

fn cargo_sparse_features_valid(value: &DiscoveryFacet<Vec<CratesSparseFeature>>) -> bool {
    match value {
        DiscoveryFacet::Known(features) => {
            features.len() <= 4096
                && features.iter().all(|feature| {
                    !feature.name.is_empty()
                        && feature.name.len() <= 256
                        && !feature.name.contains('\0')
                        && feature.members.len() <= 4096
                        && feature.members.iter().all(|member| {
                            !member.is_empty() && member.len() <= 1024 && !member.contains('\0')
                        })
                })
        }
        _ => true,
    }
}

fn cargo_sparse_dependencies_valid(value: &DiscoveryFacet<Vec<CratesSparseDependency>>) -> bool {
    fn strings(value: &DiscoveryFacet<Vec<String>>) -> bool {
        !matches!(value, DiscoveryFacet::Known(values) if values.len() > 256 || values.iter().any(|value| value.is_empty() || value.len() > 1024 || value.contains('\0')))
    }
    fn text(value: &DiscoveryFacet<String>, maximum: usize) -> bool {
        !matches!(value, DiscoveryFacet::Known(value) if value.is_empty() || value.len() > maximum || value.contains('\0'))
    }
    match value {
        DiscoveryFacet::Known(dependencies) => {
            dependencies.len() <= 4096
                && dependencies.iter().all(|dependency| {
                    !dependency.name.is_empty()
                        && dependency.name.len() <= 256
                        && !dependency.name.contains('\0')
                        && !dependency.requirement.is_empty()
                        && dependency.requirement.len() <= 1024
                        && !dependency.requirement.contains('\0')
                        && text(&dependency.package, 256)
                        && strings(&dependency.features)
                        && text(&dependency.target, 1024)
                        && text(&dependency.kind, 32)
                        && text(&dependency.registry, 2048)
                })
        }
        _ => true,
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
    /// Typed source event evidence. Raw source spelling remains separate.
    pub source_event: DiscoverySourceEvent,
    pub source_event_time: Option<String>,
    pub proof: [u8; 32],
    /// Optional, source-attributed package and version metadata.
    #[serde(default)]
    pub metadata: DiscoveryMetadata,
}

/// Version identity for one complete immutable source fact.
///
/// `full_fact_digest` is BLAKE3 over the canonical typed JSON content in field
/// order `(source, coordinate, standing, source_event, source_event_time,
/// proof, metadata)`, prefixed with
/// `backend.registry.discovery.fact-version.typed-json-v1\0`. `observed_at`
/// is excluded: local freshness is a mutable head observation, not fact
/// identity. Source event spelling and every metadata facet are included.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct RegistryFactVersionId {
    pub source: DiscoverySourceIdentity,
    pub coordinate: PackageCoordinate,
    pub full_fact_digest: [u8; 32],
}

impl RegistryFactVersionId {
    /// Builds the version key from one typed fact without retaining a second
    /// canonical payload buffer.
    pub fn from_fact(fact: &DiscoveryFact) -> Result<Self, DiscoveryError> {
        #[derive(Serialize)]
        struct Content<'a> {
            source: DiscoverySourceIdentity,
            coordinate: &'a PackageCoordinate,
            standing: DiscoveryStanding,
            source_event: &'a DiscoverySourceEvent,
            source_event_time: Option<&'a str>,
            proof: &'a [u8; 32],
            metadata: &'a DiscoveryMetadata,
        }

        if fact.coordinate.as_str().len() > MAX_DISCOVERY_BATCH_ENCODED_BYTES {
            return Err(DiscoveryError::Bounds);
        }
        if fact.coordinate.package_type().registry() != Some(fact.source.ecosystem())
            || fact.coordinate.qualifiers().is_some()
            || fact.coordinate.subpath().is_some()
            || !event_admitted(
                fact.source.ecosystem(),
                &fact.source_event,
                fact.source_event_time.as_deref(),
            )
        {
            return Err(DiscoveryError::Protocol);
        }
        fact.metadata.admit()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.registry.discovery.fact-version.typed-json-v1\0");
        let mut writer = HashingWriter {
            hasher: &mut hasher,
            bytes: 0,
            exceeded: false,
        };
        let content = Content {
            source: fact.source,
            coordinate: &fact.coordinate,
            standing: fact.standing,
            source_event: &fact.source_event,
            source_event_time: fact.source_event_time.as_deref(),
            proof: &fact.proof,
            metadata: &fact.metadata,
        };
        match serde_json::to_writer(&mut writer, &content) {
            Ok(()) => {}
            Err(_) if writer.exceeded => return Err(DiscoveryError::Bounds),
            Err(_) => return Err(DiscoveryError::Protocol),
        }
        Ok(Self {
            source: fact.source,
            coordinate: fact.coordinate.clone(),
            full_fact_digest: *hasher.finalize().as_bytes(),
        })
    }

    /// Revalidates a version key recovered from a request or durable index.
    pub fn admit(&self) -> Result<(), RegistryFactReadError> {
        registry_fact_version_valid(self)
            .then_some(())
            .ok_or(RegistryFactReadError::InvalidCursor)
    }
}

/// Bounded summary of Cargo sparse metadata kept outside the large detail
/// payload. Every count retains the source's `Known`, `Absent`, or `Unknown`
/// state, including a known count of zero.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryCargoSparseSummary {
    pub checksum: String,
    pub schema_version: u32,
    pub rust_version: DiscoveryFacet<String>,
    pub links: DiscoveryFacet<String>,
    pub features: DiscoveryFacet<u32>,
    pub features2: DiscoveryFacet<u32>,
    pub dependencies: DiscoveryFacet<u32>,
    /// Length of the canonical typed JSON encoding of this sparse metadata.
    pub detail_encoded_bytes: u32,
    /// BLAKE3 digest of that canonical typed JSON encoding, prefixed by
    /// `backend.registry.discovery.cargo-sparse.typed-json-v1\0`.
    pub detail_digest: [u8; 32],
}

/// Bounded metadata summary returned without loading large collection facets.
/// Collection counts are derived from directory state, while rows remain in
/// the immutable full-fact payload and are available through page reads.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryFactMetadataSummary {
    pub aliases: DiscoveryFacet<u32>,
    pub description: DiscoveryFacet<String>,
    pub keywords: DiscoveryFacet<u32>,
    pub license: DiscoveryFacet<String>,
    pub published_at: DiscoveryFacet<String>,
    pub deprecation: DiscoveryFacet<String>,
    pub yanked: DiscoveryFacet<bool>,
    pub advisories: DiscoveryFacet<u32>,
    pub downloads: DiscoveryFacet<u64>,
    pub cargo_sparse: DiscoveryFacet<DiscoveryCargoSparseSummary>,
}

/// Immutable fact content and bounded summary fields for one retained
/// version. Observation freshness belongs to the mutable selected head and is
/// deliberately absent here. The authority read is
/// `read_fact_core(version: &RegistryFactVersionId) -> Result<DiscoveryFactCore, RegistryFactReadError>`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryFactCore {
    pub version: RegistryFactVersionId,
    pub standing: DiscoveryStanding,
    pub source_event: DiscoverySourceEvent,
    pub source_event_time: Option<String>,
    pub proof: [u8; 32],
    pub metadata: DiscoveryFactMetadataSummary,
    /// Encoded byte length of the immutable payload identified by `version`.
    pub payload_encoded_bytes: u32,
}

impl DiscoveryFactCore {
    /// Revalidates the bounded identity and summary returned by an authority.
    pub fn admit(&self) -> Result<(), RegistryFactReadError> {
        self.version.admit()?;
        if self.payload_encoded_bytes == 0
            || !event_admitted(
                self.version.source.ecosystem(),
                &self.source_event,
                self.source_event_time.as_deref(),
            )
            || !metadata_summary_valid(&self.metadata)
        {
            return Err(RegistryFactReadError::Corrupt);
        }
        if usize::try_from(self.payload_encoded_bytes)
            .map_or(true, |bytes| bytes > MAX_DISCOVERY_BATCH_ENCODED_BYTES)
        {
            return Err(RegistryFactReadError::Bounds);
        }
        Ok(())
    }
}

/// Current mutable head selection and its latest accepted local observation
/// receipt. It carries no metadata payload and is resolved together by
/// `read_selected_head(source: DiscoverySourceIdentity, coordinate: &PackageCoordinate) -> Result<Option<DiscoverySelectedHead>, RegistryFactReadError>`
/// in one authority read. An unknown coordinate returns `Ok(None)`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoverySelectedHead {
    pub version: RegistryFactVersionId,
    pub observed_at: DiscoveryObservedAt,
}

impl DiscoverySelectedHead {
    /// Confirms this head was selected for the requested source and
    /// coordinate.
    pub fn admit_for(
        &self,
        source: DiscoverySourceIdentity,
        coordinate: &PackageCoordinate,
    ) -> Result<(), RegistryFactReadError> {
        self.version.admit()?;
        if self.version.source != source || &self.version.coordinate != coordinate {
            return Err(RegistryFactReadError::Corrupt);
        }
        Ok(())
    }
}

/// Closed set of pageable metadata collection facets.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "section", content = "index", rename_all = "snake_case")]
pub enum DiscoveryMetadataSection {
    Aliases,
    Keywords,
    Advisories,
    AdvisoryAliases { advisory_index: u32 },
    AdvisoryFixedIn { advisory_index: u32 },
    CargoFeatures,
    CargoFeatureMembers { feature_index: u32 },
    CargoFeatures2,
    CargoFeatures2Members { feature_index: u32 },
    CargoDependencies,
    CargoDependencyFeatures { dependency_index: u32 },
}

/// Source facet state associated with a metadata page. `Known` with an empty
/// row list is a known empty collection, distinct from `Absent` and `Unknown`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryMetadataFacetState {
    Known,
    Absent,
    Unknown,
}

impl DiscoveryMetadataFacetState {
    /// Maps a typed source facet to its page response state.
    #[must_use]
    pub fn from_facet<T>(facet: &DiscoveryFacet<T>) -> Self {
        match facet {
            DiscoveryFacet::Known(_) => Self::Known,
            DiscoveryFacet::Absent => Self::Absent,
            DiscoveryFacet::Unknown => Self::Unknown,
        }
    }
}

/// Bounded Cargo dependency fields in one dependency row. Its nested feature
/// list is paged separately and represented by the exact count facet here.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryCargoDependencySummary {
    pub name: String,
    pub requirement: String,
    pub package: DiscoveryFacet<String>,
    pub features: DiscoveryFacet<u32>,
    pub optional: DiscoveryFacet<bool>,
    pub default_features: DiscoveryFacet<bool>,
    pub target: DiscoveryFacet<String>,
    pub kind: DiscoveryFacet<String>,
    pub registry: DiscoveryFacet<String>,
}

/// Bounded advisory header row. Alternate identifiers and fixed-version
/// values are fetched from their own sections so JSON escaping never creates
/// an unbounded nested row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryAdvisorySummary {
    pub id: String,
    pub aliases: DiscoveryFacet<u32>,
    pub summary: DiscoveryFacet<String>,
    pub severity: DiscoveryFacet<String>,
    pub fixed_in: DiscoveryFacet<u32>,
}

/// One bounded row from a typed metadata page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DiscoveryMetadataRow {
    Text(String),
    Advisory(DiscoveryAdvisorySummary),
    CargoFeature { name: String, member_count: u32 },
    CargoDependency(DiscoveryCargoDependencySummary),
}

/// Untrusted pagination position bound to one immutable fact version and
/// section. Any valid in-range offset is a valid random-access page request;
/// this cursor is not a capability and has no MAC.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryMetadataPageCursor {
    version: RegistryFactVersionId,
    section: DiscoveryMetadataSection,
    next_index: u32,
}

impl DiscoveryMetadataPageCursor {
    /// Creates a continuation cursor after validating its nonzero row offset.
    pub fn new(
        version: RegistryFactVersionId,
        section: DiscoveryMetadataSection,
        next_index: u32,
    ) -> Result<Self, RegistryFactReadError> {
        if next_index == 0
            || next_index > metadata_section_max_rows(section)
            || !metadata_section_indices_valid(section)
            || !registry_fact_version_valid(&version)
        {
            return Err(RegistryFactReadError::InvalidCursor);
        }
        Ok(Self {
            version,
            section,
            next_index,
        })
    }

    /// Checks that a recovered cursor is still bound to the requested
    /// immutable version and section and that its offset is within the
    /// section's schema maximum. The reader also checks the exact stored row
    /// count before fetching.
    pub fn admit(
        &self,
        version: &RegistryFactVersionId,
        section: DiscoveryMetadataSection,
    ) -> Result<(), RegistryFactReadError> {
        if self.next_index == 0
            || self.next_index > metadata_section_max_rows(section)
            || !metadata_section_indices_valid(section)
            || &self.version != version
            || self.section != section
            || !registry_fact_version_valid(&self.version)
        {
            return Err(RegistryFactReadError::InvalidCursor);
        }
        Ok(())
    }

    /// Version whose immutable payload contains the continuation.
    #[must_use]
    pub fn version(&self) -> &RegistryFactVersionId {
        &self.version
    }

    /// Metadata collection section and, for nested lists, parent row index.
    #[must_use]
    pub const fn section(&self) -> DiscoveryMetadataSection {
        self.section
    }

    /// Zero-based next row offset within the bound section.
    #[must_use]
    pub const fn next_index(&self) -> u32 {
        self.next_index
    }
}

/// One bounded page of a pageable metadata collection. `start_index` is zero
/// for the first page and equals the supplied cursor offset for later pages.
/// The authority read is
/// `read_metadata_page(version: &RegistryFactVersionId, section: DiscoveryMetadataSection, cursor: Option<&DiscoveryMetadataPageCursor>, max_rows: u8, max_encoded_bytes: u32) -> Result<DiscoveryMetadataPage, RegistryFactReadError>`;
/// limits above [`MAX_DISCOVERY_METADATA_PAGE_ROWS`] or
/// [`MAX_DISCOVERY_METADATA_PAGE_ENCODED_BYTES`] are rejected.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryMetadataPage {
    pub version: RegistryFactVersionId,
    pub section: DiscoveryMetadataSection,
    pub start_index: u32,
    pub facet_state: DiscoveryMetadataFacetState,
    /// Exact number of rows in this source facet or nested collection.
    pub total_rows: u32,
    pub rows: Vec<DiscoveryMetadataRow>,
    pub next_cursor: Option<DiscoveryMetadataPageCursor>,
}

impl DiscoveryMetadataPage {
    /// Validates page shape and the exact encoded response envelope before it
    /// is returned to a caller.
    pub fn admit(&self, max_encoded_bytes: u32) -> Result<(), RegistryFactReadError> {
        if max_encoded_bytes == 0
            || max_encoded_bytes > MAX_DISCOVERY_METADATA_PAGE_ENCODED_BYTES
            || self.rows.len() > usize::from(MAX_DISCOVERY_METADATA_PAGE_ROWS)
            || !metadata_rows_match_section(self.section, &self.rows)
        {
            return Err(RegistryFactReadError::Bounds);
        }
        if !registry_fact_version_valid(&self.version)
            || !metadata_section_indices_valid(self.section)
            || self.start_index > metadata_section_max_rows(self.section)
            || self.total_rows > metadata_section_max_rows(self.section)
            || self
                .start_index
                .checked_add(
                    u32::try_from(self.rows.len()).map_err(|_| RegistryFactReadError::Bounds)?,
                )
                .is_none_or(|end| end > self.total_rows)
        {
            return Err(RegistryFactReadError::InvalidCursor);
        }
        if self.facet_state != DiscoveryMetadataFacetState::Known
            && (self.start_index != 0
                || self.total_rows != 0
                || !self.rows.is_empty()
                || self.next_cursor.is_some())
        {
            return Err(RegistryFactReadError::InvalidCursor);
        }
        let page_end = self
            .start_index
            .checked_add(u32::try_from(self.rows.len()).map_err(|_| RegistryFactReadError::Bounds)?)
            .ok_or(RegistryFactReadError::Bounds)?;
        if self.facet_state == DiscoveryMetadataFacetState::Known
            && self.next_cursor.is_some() != (page_end < self.total_rows)
        {
            return Err(RegistryFactReadError::InvalidCursor);
        }
        if let Some(cursor) = &self.next_cursor {
            if self.rows.is_empty()
                || self.facet_state != DiscoveryMetadataFacetState::Known
                || &cursor.version != &self.version
                || cursor.section != self.section
                || cursor.next_index != page_end
                || cursor.next_index == 0
            {
                return Err(RegistryFactReadError::InvalidCursor);
            }
        }
        let mut writer = CountingWriter {
            bytes: 0,
            exceeded: false,
        };
        match serde_json::to_writer(&mut writer, self) {
            Ok(()) if writer.bytes <= usize::try_from(max_encoded_bytes).unwrap_or(0) => Ok(()),
            Ok(()) => Err(RegistryFactReadError::Bounds),
            Err(_) if writer.exceeded => Err(RegistryFactReadError::Bounds),
            Err(_) => Err(RegistryFactReadError::Corrupt),
        }
    }
}

fn metadata_section_max_rows(section: DiscoveryMetadataSection) -> u32 {
    match section {
        DiscoveryMetadataSection::Aliases => 64,
        DiscoveryMetadataSection::Keywords => 128,
        DiscoveryMetadataSection::Advisories | DiscoveryMetadataSection::AdvisoryFixedIn { .. } => {
            128
        }
        DiscoveryMetadataSection::AdvisoryAliases { .. } => 32,
        DiscoveryMetadataSection::CargoFeatures
        | DiscoveryMetadataSection::CargoFeatureMembers { .. }
        | DiscoveryMetadataSection::CargoFeatures2
        | DiscoveryMetadataSection::CargoFeatures2Members { .. }
        | DiscoveryMetadataSection::CargoDependencies => 4096,
        DiscoveryMetadataSection::CargoDependencyFeatures { .. } => 256,
    }
}

fn metadata_section_indices_valid(section: DiscoveryMetadataSection) -> bool {
    match section {
        DiscoveryMetadataSection::AdvisoryAliases { advisory_index }
        | DiscoveryMetadataSection::AdvisoryFixedIn { advisory_index } => advisory_index < 128,
        DiscoveryMetadataSection::CargoFeatureMembers { feature_index }
        | DiscoveryMetadataSection::CargoFeatures2Members { feature_index } => feature_index < 4096,
        DiscoveryMetadataSection::CargoDependencyFeatures { dependency_index } => {
            dependency_index < 4096
        }
        DiscoveryMetadataSection::Aliases
        | DiscoveryMetadataSection::Keywords
        | DiscoveryMetadataSection::Advisories
        | DiscoveryMetadataSection::CargoFeatures
        | DiscoveryMetadataSection::CargoFeatures2
        | DiscoveryMetadataSection::CargoDependencies => true,
    }
}

fn registry_fact_version_valid(version: &RegistryFactVersionId) -> bool {
    version.coordinate.as_str().len() <= 1024
        && version.coordinate.package_type().registry() == Some(version.source.ecosystem())
        && version.coordinate.qualifiers().is_none()
        && version.coordinate.subpath().is_none()
}

fn metadata_rows_match_section(
    section: DiscoveryMetadataSection,
    rows: &[DiscoveryMetadataRow],
) -> bool {
    rows.iter().all(|row| match (section, row) {
        (DiscoveryMetadataSection::Advisories, DiscoveryMetadataRow::Advisory(advisory)) => {
            !advisory.id.is_empty()
                && advisory.id.len() <= 256
                && !advisory.id.bytes().any(|byte| byte.is_ascii_control())
                && facet_count_valid(&advisory.aliases, 32)
                && facet_text_valid(&advisory.summary, 4096)
                && facet_text_valid(&advisory.severity, 128)
                && facet_count_valid(&advisory.fixed_in, 128)
        }
        (
            DiscoveryMetadataSection::CargoFeatures | DiscoveryMetadataSection::CargoFeatures2,
            DiscoveryMetadataRow::CargoFeature { name, member_count },
        ) => !name.is_empty() && name.len() <= 256 && !name.contains('\0') && *member_count <= 4096,
        (
            DiscoveryMetadataSection::CargoDependencies,
            DiscoveryMetadataRow::CargoDependency(dependency),
        ) => {
            !dependency.name.is_empty()
                && dependency.name.len() <= 256
                && !dependency.name.contains('\0')
                && !dependency.requirement.is_empty()
                && dependency.requirement.len() <= 1024
                && !dependency.requirement.contains('\0')
                && facet_text_valid(&dependency.package, 256)
                && facet_count_valid(&dependency.features, 256)
                && facet_text_valid(&dependency.target, 1024)
                && facet_text_valid(&dependency.kind, 32)
                && facet_text_valid(&dependency.registry, 2048)
        }
        (DiscoveryMetadataSection::Aliases, DiscoveryMetadataRow::Text(value)) => {
            text_row_valid(value, 1024)
        }
        (
            DiscoveryMetadataSection::Keywords
            | DiscoveryMetadataSection::AdvisoryAliases { .. }
            | DiscoveryMetadataSection::AdvisoryFixedIn { .. },
            DiscoveryMetadataRow::Text(value),
        ) => text_row_valid(value, 256),
        (
            DiscoveryMetadataSection::CargoFeatureMembers { .. }
            | DiscoveryMetadataSection::CargoFeatures2Members { .. }
            | DiscoveryMetadataSection::CargoDependencyFeatures { .. },
            DiscoveryMetadataRow::Text(value),
        ) => text_row_valid(value, 1024),
        _ => false,
    })
}

fn text_row_valid(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= maximum_bytes && !value.contains('\0')
}

fn facet_count_valid(value: &DiscoveryFacet<u32>, maximum: u32) -> bool {
    !matches!(value, DiscoveryFacet::Known(count) if *count > maximum)
}

fn facet_text_valid(value: &DiscoveryFacet<String>, maximum_bytes: usize) -> bool {
    !matches!(value, DiscoveryFacet::Known(text) if text.is_empty() || text.len() > maximum_bytes || text.contains('\0'))
}

fn metadata_summary_valid(metadata: &DiscoveryFactMetadataSummary) -> bool {
    facet_count_valid(&metadata.aliases, 64)
        && facet_text_valid(&metadata.description, 16 * 1024)
        && facet_count_valid(&metadata.keywords, 128)
        && facet_text_valid(&metadata.license, 1024)
        && facet_text_valid(&metadata.published_at, 128)
        && facet_text_valid(&metadata.deprecation, 4096)
        && facet_count_valid(&metadata.advisories, 128)
        && match &metadata.cargo_sparse {
            DiscoveryFacet::Known(sparse) => {
                sparse.checksum.len() == 64
                    && sparse.checksum.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && matches!(sparse.schema_version, 1 | 2)
                    && facet_text_valid(&sparse.rust_version, 128)
                    && facet_text_valid(&sparse.links, 256)
                    && facet_count_valid(&sparse.features, 4096)
                    && facet_count_valid(&sparse.features2, 4096)
                    && facet_count_valid(&sparse.dependencies, 4096)
                    && sparse.detail_encoded_bytes > 0
                    && usize::try_from(sparse.detail_encoded_bytes)
                        .is_ok_and(|bytes| bytes <= MAX_DISCOVERY_BATCH_ENCODED_BYTES)
            }
            DiscoveryFacet::Absent | DiscoveryFacet::Unknown => true,
        }
}

/// Typed outcome when an immutable fact version or metadata page cannot be
/// read. `Pruned` is returned only when a durable tombstone proves explicit
/// pruning; it is distinct from a version that never existed. `Busy` means a
/// transient authority/snapshot contention and `Unavailable` means the
/// authority is not configured or enabled.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryFactReadError {
    NotFound,
    Pruned,
    Busy,
    Unavailable,
    InvalidCursor,
    Bounds,
    Corrupt,
}

/// Maximum rows returned by one immutable metadata page.
pub const MAX_DISCOVERY_METADATA_PAGE_ROWS: u8 = 32;
/// Maximum encoded bytes returned by one immutable metadata page.
pub const MAX_DISCOVERY_METADATA_PAGE_ENCODED_BYTES: u32 = 512 * 1024;

struct HashingWriter<'a> {
    hasher: &'a mut blake3::Hasher,
    bytes: usize,
    exceeded: bool,
}

impl io::Write for HashingWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(bytes) = self.bytes.checked_add(buffer.len()) else {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "fact digest bound",
            ));
        };
        if bytes > MAX_DISCOVERY_BATCH_ENCODED_BYTES {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "fact digest bound",
            ));
        }
        self.bytes = bytes;
        self.hasher.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A committed unit of discovery work. The caller must persist `facts`,
/// `completeness`, and `next_cursor` together before treating the cursor as
/// advanced.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryBatch {
    pub source: DiscoverySourceIdentity,
    /// Local per-source sequence captured before this batch's network work.
    /// This is a CAS fence; source cursors and upstream watermarks remain
    /// opaque adapter hints and never substitute for this local sequence.
    pub expected_base_sequence: u64,
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

/// Parsed source work before it is fenced to the local progress sequence that
/// was captured before the feed request began. Parser code must not invent a
/// sequence or make a source observation look like an admitted CAS batch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiscoveryBatchDraft {
    pub source: DiscoverySourceIdentity,
    pub previous_cursor: DiscoveryCursor,
    pub next_cursor: DiscoveryCursor,
    pub source_high_watermark: DiscoveryCursor,
    pub caught_up: bool,
    pub observed_at: DiscoveryObservedAt,
    pub completeness: DiscoveryCompleteness,
    pub facts: Vec<DiscoveryFact>,
    pub package_retractions: Vec<DiscoveryPackageRetraction>,
}

impl DiscoveryBatchDraft {
    /// Checks typed source content and bounds before a captured local sequence
    /// is attached. The final sequence-fenced batch is admitted again before
    /// durable submission.
    pub fn admit(&self) -> Result<(), DiscoveryError> {
        validate_batch_parts(
            self.source,
            &self.previous_cursor,
            &self.next_cursor,
            &self.source_high_watermark,
            self.caught_up,
            self.completeness,
            &self.facts,
            &self.package_retractions,
        )?;
        let mut writer = CountingWriter {
            bytes: 0,
            exceeded: false,
        };
        match serde_json::to_writer(&mut writer, self) {
            Ok(()) => {
                // The final CAS sequence field can add at most twenty decimal
                // digits plus fixed JSON syntax.
                if writer
                    .bytes
                    .checked_add(32)
                    .is_none_or(|bytes| bytes > MAX_DISCOVERY_BATCH_ENCODED_BYTES)
                {
                    Err(DiscoveryError::Bounds)
                } else {
                    Ok(())
                }
            }
            Err(_) if writer.exceeded => Err(DiscoveryError::Bounds),
            Err(_) => Err(DiscoveryError::Protocol),
        }
    }

    /// Attaches the local sequence captured before network work and performs
    /// final admission of the exact durable batch.
    pub fn admit_for_sequence(
        self,
        expected_base_sequence: u64,
    ) -> Result<DiscoveryBatch, DiscoveryError> {
        let batch = DiscoveryBatch {
            source: self.source,
            expected_base_sequence,
            previous_cursor: self.previous_cursor,
            next_cursor: self.next_cursor,
            source_high_watermark: self.source_high_watermark,
            caught_up: self.caught_up,
            observed_at: self.observed_at,
            completeness: self.completeness,
            facts: self.facts,
            package_retractions: self.package_retractions,
        };
        batch.admit()?;
        Ok(batch)
    }
}

impl DiscoveryBatch {
    /// Checks bounds and source/cursor invariants before durable commit.
    pub fn admit(&self) -> Result<(), DiscoveryError> {
        validate_batch_parts(
            self.source,
            &self.previous_cursor,
            &self.next_cursor,
            &self.source_high_watermark,
            self.caught_up,
            self.completeness,
            &self.facts,
            &self.package_retractions,
        )?;
        self.encoded_len_bounded()?;
        Ok(())
    }

    /// Counts the canonical typed batch encoding without retaining an encoded
    /// copy. Durable owners call this before cloning or transaction assembly.
    pub fn encoded_len_bounded(&self) -> Result<usize, DiscoveryError> {
        let mut writer = CountingWriter {
            bytes: 0,
            exceeded: false,
        };
        match serde_json::to_writer(&mut writer, self) {
            Ok(()) => Ok(writer.bytes),
            Err(_) if writer.exceeded => Err(DiscoveryError::Bounds),
            Err(_) => Err(DiscoveryError::Protocol),
        }
    }
}

fn validate_batch_parts(
    source: DiscoverySourceIdentity,
    previous_cursor: &DiscoveryCursor,
    next_cursor: &DiscoveryCursor,
    source_high_watermark: &DiscoveryCursor,
    caught_up: bool,
    completeness: DiscoveryCompleteness,
    facts: &[DiscoveryFact],
    package_retractions: &[DiscoveryPackageRetraction],
) -> Result<(), DiscoveryError> {
    previous_cursor.admit()?;
    next_cursor.admit()?;
    source_high_watermark.admit()?;
    if facts.len().saturating_add(package_retractions.len()) > MAX_DISCOVERY_PAGE_ITEMS
        || facts.iter().any(|fact| fact.source != source)
        || facts.iter().any(|fact| {
            PackageCoordinate::parse(fact.coordinate.as_str().to_owned()).is_err()
                || fact.coordinate.package_type().registry() != Some(source.ecosystem())
                || fact.coordinate.qualifiers().is_some()
                || fact.coordinate.subpath().is_some()
                || fact.metadata.admit().is_err()
                || !event_admitted(
                    source.ecosystem(),
                    &fact.source_event,
                    fact.source_event_time.as_deref(),
                )
        })
        || (completeness == DiscoveryCompleteness::CompleteThroughCursor && next_cursor.is_empty())
        || (caught_up && next_cursor != source_high_watermark)
        || (!package_retractions.is_empty() && source.ecosystem() != RegistryEcosystem::Npm)
        || package_retractions.iter().any(|retraction| {
            !valid_package_name(&retraction.package_name)
                || !event_admitted(
                    source.ecosystem(),
                    &retraction.source_event,
                    Some(&retraction.source_event_time),
                )
                || !matches!(
                    &retraction.source_event,
                    DiscoverySourceEvent::NpmChange { change_proof, .. }
                        if *change_proof == retraction.proof
                )
        })
    {
        return Err(DiscoveryError::Protocol);
    }
    Ok(())
}

struct CountingWriter {
    bytes: usize,
    exceeded: bool,
}

impl io::Write for CountingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(bytes) = self.bytes.checked_add(buffer.len()) else {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "discovery batch bound",
            ));
        };
        if bytes > MAX_DISCOVERY_BATCH_ENCODED_BYTES {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "discovery batch bound",
            ));
        }
        self.bytes = bytes;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn event_admitted(
    ecosystem: RegistryEcosystem,
    event: &DiscoverySourceEvent,
    source_spelling: Option<&str>,
) -> bool {
    if source_spelling
        .is_some_and(|value| value.len() > MAX_DISCOVERY_EVENT_TEXT_BYTES || value.contains('\0'))
    {
        return false;
    }
    match (ecosystem, event) {
        (RegistryEcosystem::Npm | RegistryEcosystem::Nuget, DiscoverySourceEvent::Unordered) => {
            false
        }
        (_, DiscoverySourceEvent::Unordered | DiscoverySourceEvent::Snapshot) => true,
        (
            RegistryEcosystem::Nuget,
            DiscoverySourceEvent::NugetCatalog {
                timestamp,
                commit_id,
            },
        ) => {
            commit_id.len() <= MAX_DISCOVERY_COMMIT_ID_BYTES
                && !commit_id.is_empty()
                && !commit_id.bytes().any(|byte| byte.is_ascii_control())
                && source_spelling.is_some_and(|spelling| {
                    DiscoveryTimestamp::parse_rfc3339(spelling).ok() == Some(*timestamp)
                })
        }
        (
            RegistryEcosystem::Npm,
            DiscoverySourceEvent::NpmChange {
                sequence, revision, ..
            },
        ) => {
            *sequence > 0
                && revision.as_ref().is_none_or(|value| {
                    !value.is_empty()
                        && value.len() <= MAX_DISCOVERY_REVISION_BYTES
                        && !value.bytes().any(|byte| byte.is_ascii_control())
                })
                && source_spelling.is_some_and(|spelling| {
                    spelling.len() == 20
                        && spelling.bytes().all(|byte| byte.is_ascii_digit())
                        && spelling.parse::<u64>().ok() == Some(*sequence)
                })
        }
        _ => false,
    }
}

fn valid_package_name(value: &str) -> bool {
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

#[cfg(test)]
mod source_discovery_tests {
    use super::*;

    fn fact() -> DiscoveryFact {
        let source = DiscoverySourceIdentity::from_parts(RegistryEcosystem::Cargo, [7; 32]);
        DiscoveryFact {
            source,
            coordinate: PackageCoordinate::parse("pkg:cargo/example@1.2.3").expect("coordinate"),
            standing: DiscoveryStanding::Published,
            observed_at: DiscoveryObservedAt::from_unix_millis(10),
            source_event: DiscoverySourceEvent::Unordered,
            source_event_time: None,
            proof: [8; 32],
            metadata: DiscoveryMetadata::default(),
        }
    }

    #[test]
    fn version_identity_excludes_freshness_and_covers_all_fact_content() {
        let original = fact();
        let original_id = RegistryFactVersionId::from_fact(&original).expect("version id");
        let mut refreshed = original.clone();
        refreshed.observed_at = DiscoveryObservedAt::from_unix_millis(1);
        assert_eq!(
            RegistryFactVersionId::from_fact(&refreshed).expect("fresh version id"),
            original_id
        );

        refreshed.metadata.description = DiscoveryFacet::Known("different".to_owned());
        assert_ne!(
            RegistryFactVersionId::from_fact(&refreshed).expect("changed version id"),
            original_id
        );
    }

    #[test]
    fn event_ordering_timestamps_are_normalized_and_deserialization_is_validated() {
        let utc = DiscoveryTimestamp::parse_rfc3339("2026-10-01T12:30:00.100000000Z")
            .expect("UTC timestamp");
        let offset = DiscoveryTimestamp::parse_rfc3339("2026-10-01T13:30:00.1+01:00")
            .expect("offset timestamp");
        assert_eq!(utc, offset);
        assert!(
            serde_json::from_str::<DiscoveryTimestamp>(
                r#"{"unix_seconds":0,"nanoseconds":1000000000}"#
            )
            .is_err()
        );
    }

    #[test]
    fn npm_and_nuget_require_typed_events_or_explicit_snapshots() {
        for (ecosystem, coordinate) in [
            (RegistryEcosystem::Npm, "pkg:npm/example@1.0.0"),
            (RegistryEcosystem::Nuget, "pkg:nuget/Example@1.0.0"),
        ] {
            let source = DiscoverySourceIdentity::from_parts(ecosystem, [9; 32]);
            let make_draft = |event| DiscoveryBatchDraft {
                source,
                previous_cursor: DiscoveryCursor::default(),
                next_cursor: DiscoveryCursor::default(),
                source_high_watermark: DiscoveryCursor::default(),
                caught_up: false,
                observed_at: DiscoveryObservedAt::from_unix_millis(10),
                completeness: DiscoveryCompleteness::Windowed,
                facts: vec![DiscoveryFact {
                    source,
                    coordinate: PackageCoordinate::parse(coordinate).expect("coordinate"),
                    standing: DiscoveryStanding::Published,
                    observed_at: DiscoveryObservedAt::from_unix_millis(10),
                    source_event: event,
                    source_event_time: None,
                    proof: [4; 32],
                    metadata: DiscoveryMetadata::default(),
                }],
                package_retractions: Vec::new(),
            };

            assert!(make_draft(DiscoverySourceEvent::Unordered).admit().is_err());
            assert!(make_draft(DiscoverySourceEvent::Snapshot).admit().is_ok());
        }
    }

    #[test]
    fn metadata_cursors_bind_version_section_and_exact_page_continuation() {
        let first = RegistryFactVersionId::from_fact(&fact()).expect("first version");
        let mut changed_fact = fact();
        changed_fact.metadata.description = DiscoveryFacet::Known("changed".to_owned());
        let second = RegistryFactVersionId::from_fact(&changed_fact).expect("second version");
        let section = DiscoveryMetadataSection::Aliases;

        let continuation = DiscoveryMetadataPageCursor::new(first.clone(), section, 2)
            .expect("valid continuation");
        assert_eq!(continuation.admit(&first, section), Ok(()));
        assert_eq!(
            continuation.admit(&second, section),
            Err(RegistryFactReadError::InvalidCursor)
        );
        assert_eq!(
            continuation.admit(&first, DiscoveryMetadataSection::Keywords),
            Err(RegistryFactReadError::InvalidCursor)
        );
        assert!(DiscoveryMetadataPageCursor::new(first.clone(), section, 0).is_err());
        assert!(DiscoveryMetadataPageCursor::new(first.clone(), section, 65).is_err());

        // A caller may deliberately seek to another valid row offset; the
        // cursor is an untrusted pagination position, not an authorization token.
        let random_access =
            DiscoveryMetadataPageCursor::new(first.clone(), section, 1).expect("valid offset");
        assert_eq!(random_access.admit(&first, section), Ok(()));

        let page = DiscoveryMetadataPage {
            version: first.clone(),
            section,
            start_index: 0,
            facet_state: DiscoveryMetadataFacetState::Known,
            total_rows: 3,
            rows: vec![
                DiscoveryMetadataRow::Text("one".to_owned()),
                DiscoveryMetadataRow::Text("two".to_owned()),
            ],
            next_cursor: Some(continuation),
        };
        assert_eq!(
            page.admit(MAX_DISCOVERY_METADATA_PAGE_ENCODED_BYTES),
            Ok(())
        );

        let mut tampered_page = page;
        tampered_page.next_cursor = Some(random_access);
        assert_eq!(
            tampered_page.admit(MAX_DISCOVERY_METADATA_PAGE_ENCODED_BYTES),
            Err(RegistryFactReadError::InvalidCursor)
        );
    }

    #[test]
    fn metadata_pages_preserve_unknown_absent_and_known_empty() {
        let version = RegistryFactVersionId::from_fact(&fact()).expect("version");
        for state in [
            DiscoveryMetadataFacetState::Unknown,
            DiscoveryMetadataFacetState::Absent,
            DiscoveryMetadataFacetState::Known,
        ] {
            let page = DiscoveryMetadataPage {
                version: version.clone(),
                section: DiscoveryMetadataSection::Aliases,
                start_index: 0,
                facet_state: state,
                total_rows: 0,
                rows: Vec::new(),
                next_cursor: None,
            };
            assert_eq!(
                page.admit(MAX_DISCOVERY_METADATA_PAGE_ENCODED_BYTES),
                Ok(())
            );
        }
    }
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
/// Package-level removal event from npm. The durable owner expands this only
/// to release coordinates already observed from the same source.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryPackageRetraction {
    /// Registry-native package name, including namespace when present.
    pub package_name: String,
    /// Monotonic source sequence for this deletion event.
    pub source_event_time: String,
    /// Exact source change event that withdrew this package's releases.
    pub source_event: DiscoverySourceEvent,
    /// Hash of the source event row that reported deletion.
    pub proof: [u8; 32],
}
