//! Transport-neutral, bounded source discovery facts and progress values.
//!
//! Endpoint admission, source parsers, and network requests remain in the
//! engine and local service. This module owns only the typed fact and batch
//! contract that the durable discovery authority accepts.

use backend_semantic::vocabulary::{PackageUrl as PackageCoordinate, RegistryEcosystem};
use serde::{Deserialize, Serialize};
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
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct DiscoveryTimestamp {
    unix_seconds: i64,
    nanoseconds: u32,
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
    /// The feed does not provide a trustworthy ordered event identity.
    Unordered,
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
        if self
            .facts
            .len()
            .saturating_add(self.package_retractions.len())
            > MAX_DISCOVERY_PAGE_ITEMS
            || self.facts.iter().any(|fact| fact.source != self.source)
            || self.facts.iter().any(|fact| {
                PackageCoordinate::parse(fact.coordinate.as_str().to_owned()).is_err()
                    || fact.coordinate.package_type().registry() != Some(self.source.ecosystem())
                    || fact.coordinate.qualifiers().is_some()
                    || fact.coordinate.subpath().is_some()
                    || fact.metadata.admit().is_err()
                    || !event_admitted(
                        self.source.ecosystem(),
                        &fact.source_event,
                        fact.source_event_time.as_deref(),
                    )
            })
            || (self.completeness == DiscoveryCompleteness::CompleteThroughCursor
                && self.next_cursor.is_empty())
            || (self.caught_up && self.next_cursor != self.source_high_watermark)
            || (!self.package_retractions.is_empty()
                && self.source.ecosystem() != RegistryEcosystem::Npm)
            || self.package_retractions.iter().any(|retraction| {
                !valid_package_name(&retraction.package_name)
                    || !event_admitted(
                        self.source.ecosystem(),
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
        (_, DiscoverySourceEvent::Unordered) => true,
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
