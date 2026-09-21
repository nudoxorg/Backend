//! Durable composition of the supported advisory authorities.
//!
//! The registry owner owns release bytes. This module owns the other side of
//! that boundary: a versioned, source-qualified security frontier which can
//! be refreshed independently and joined to an exact package/version at
//! acquisition time. A missing frontier remains visible as unknown coverage;
//! it is never converted into a clean result.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use super::{
    AcquisitionDecision, AcquisitionGate, Advisory, AdvisoryCoverage, AdvisoryDelta,
    AdvisoryJournal, AdvisoryJournalError, AdvisoryObservation, AdvisorySource, AdvisorySync,
    FeedFreshness, FreshnessState, GhsaParseError, MalwareCoverage, PackageIdentity, ParseError,
    RustSecParseError, SyncMode,
};

/// A configured authority feed. The bytes are supplied by the composition
/// root so network policy remains outside the portable advisory crate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorityFeed {
    /// Authority which signed/owns these objects.
    pub source: AdvisorySource,
    /// Snapshot or delta semantics of the supplied body.
    pub mode: SyncMode,
    /// Whether a snapshot is complete and may retire absent objects.
    pub complete: bool,
    /// Conditional response and observation evidence.
    pub freshness: FeedFreshness,
    /// Fully parsed source objects.
    pub entries: Vec<Advisory>,
}

impl AuthorityFeed {
    /// Parses a bounded JSON/TOML authority body into one deterministic feed.
    ///
    /// OSV and GHSA accept either one object or a conventional `vulns` /
    /// `advisories` array. RustSec accepts one advisory TOML document; callers
    /// with a directory can use [`Self::from_entries`].
    pub fn parse(
        source: AdvisorySource,
        bytes: &[u8],
        observed_at: u64,
        etag: Option<String>,
        last_modified: Option<String>,
    ) -> Result<Self, AuthorityParseError> {
        let entries = match source {
            AdvisorySource::Osv => parse_osv_feed(bytes, observed_at)?,
            AdvisorySource::RustSec => vec![super::parse_rustsec(bytes, observed_at)?],
            AdvisorySource::Ghsa => parse_ghsa_feed(bytes, observed_at)?,
        };
        Ok(Self {
            source,
            mode: SyncMode::Snapshot,
            complete: true,
            freshness: FeedFreshness {
                etag,
                last_modified,
                observed_at,
                not_modified: false,
            },
            entries,
        })
    }

    /// Builds a feed from already parsed entries, useful for a RustSec tree
    /// and deterministic fixture harnesses.
    #[must_use]
    pub fn from_entries(
        source: AdvisorySource,
        entries: Vec<Advisory>,
        observed_at: u64,
        etag: Option<String>,
        last_modified: Option<String>,
    ) -> Self {
        Self {
            source,
            mode: SyncMode::Snapshot,
            complete: true,
            freshness: FeedFreshness {
                etag,
                last_modified,
                observed_at,
                not_modified: false,
            },
            entries,
        }
    }

    /// Creates a validated conditional `304 Not Modified` response.
    #[must_use]
    pub fn not_modified(
        source: AdvisorySource,
        observed_at: u64,
        etag: Option<String>,
        last_modified: Option<String>,
    ) -> Self {
        Self {
            source,
            mode: SyncMode::Snapshot,
            complete: true,
            freshness: FeedFreshness {
                etag,
                last_modified,
                observed_at,
                not_modified: true,
            },
            entries: Vec::new(),
        }
    }
}

fn parse_osv_feed(bytes: &[u8], observed_at: u64) -> Result<Vec<Advisory>, AuthorityParseError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| AuthorityParseError::Osv(ParseError::InvalidJson))?;
    if let Some(values) = value.as_array() {
        return values
            .iter()
            .map(|value| {
                serde_json::to_vec(value)
                    .ok()
                    .ok_or(AuthorityParseError::Osv(ParseError::InvalidJson))
                    .and_then(|bytes| {
                        super::parse_osv(&bytes, observed_at).map_err(AuthorityParseError::Osv)
                    })
            })
            .collect();
    }
    let Some(object) = value.as_object() else {
        return Err(AuthorityParseError::Osv(ParseError::InvalidJson));
    };
    let values = object
        .get("vulns")
        .or_else(|| object.get("advisories"))
        .and_then(serde_json::Value::as_array);
    match values {
        Some(values) => values
            .iter()
            .map(|value| {
                serde_json::to_vec(value)
                    .ok()
                    .ok_or(AuthorityParseError::Osv(ParseError::InvalidJson))
                    .and_then(|bytes| {
                        super::parse_osv(&bytes, observed_at).map_err(AuthorityParseError::Osv)
                    })
            })
            .collect(),
        None => Ok(vec![
            super::parse_osv(bytes, observed_at).map_err(AuthorityParseError::Osv)?,
        ]),
    }
}

fn parse_ghsa_feed(bytes: &[u8], observed_at: u64) -> Result<Vec<Advisory>, AuthorityParseError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| AuthorityParseError::Ghsa(GhsaParseError::InvalidJson))?;
    if let Some(values) = value.as_array() {
        return values
            .iter()
            .map(|value| {
                serde_json::to_vec(value)
                    .ok()
                    .ok_or(AuthorityParseError::Ghsa(GhsaParseError::InvalidJson))
                    .and_then(|bytes| {
                        super::parse_ghsa_global(&bytes, observed_at)
                            .map_err(AuthorityParseError::Ghsa)
                    })
            })
            .collect();
    }
    let Some(object) = value.as_object() else {
        return Err(AuthorityParseError::Ghsa(GhsaParseError::InvalidJson));
    };
    if let Some(values) = object
        .get("advisories")
        .and_then(serde_json::Value::as_array)
    {
        return values
            .iter()
            .map(|value| {
                serde_json::to_vec(value)
                    .ok()
                    .ok_or(AuthorityParseError::Ghsa(GhsaParseError::InvalidJson))
                    .and_then(|bytes| {
                        super::parse_ghsa_global(&bytes, observed_at)
                            .map_err(AuthorityParseError::Ghsa)
                    })
            })
            .collect();
    }
    Ok(vec![
        super::parse_ghsa_global(bytes, observed_at).map_err(AuthorityParseError::Ghsa)?,
    ])
}

/// Parse failures are source-qualified so one malformed authority cannot be
/// mistaken for a transport failure from another.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorityParseError {
    /// OSV object or batch failed admission.
    Osv(ParseError),
    /// RustSec TOML failed admission.
    RustSec(RustSecParseError),
    /// GHSA object or batch failed admission.
    Ghsa(GhsaParseError),
}

impl std::fmt::Display for AuthorityParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "advisory authority parse failed: {self:?}")
    }
}
impl std::error::Error for AuthorityParseError {}

impl From<ParseError> for AuthorityParseError {
    fn from(value: ParseError) -> Self {
        Self::Osv(value)
    }
}
impl From<RustSecParseError> for AuthorityParseError {
    fn from(value: RustSecParseError) -> Self {
        Self::RustSec(value)
    }
}
impl From<GhsaParseError> for AuthorityParseError {
    fn from(value: GhsaParseError) -> Self {
        Self::Ghsa(value)
    }
}

/// Whether a configured authority is currently usable.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthorityAvailability {
    /// A body was fetched or validated and is usable.
    Available,
    /// The authority could not be reached or admitted during the last refresh.
    Unavailable,
}

/// Durable source frontier metadata. Object bodies remain in the per-source
/// [`AdvisoryJournal`], while this compact record drives conditional refresh
/// and explicit coverage projection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuthorityFrontier {
    /// Source authority.
    pub source: AdvisorySource,
    /// Monotonic source-local journal sequence.
    pub sequence: u64,
    /// Active object/tombstone digest at this frontier.
    pub digest: [u8; 32],
    /// Conditional HTTP validator.
    pub etag: Option<String>,
    /// Conditional HTTP validator.
    pub last_modified: Option<String>,
    /// Local observation time in seconds.
    pub observed_at: u64,
    /// Whether the source snapshot was complete.
    pub complete: bool,
    /// Whether the last refresh was usable.
    pub availability: AuthorityAvailability,
    /// Number of admitted objects in the source journal.
    pub entries: u64,
    /// Whether the body was validated rather than transferred.
    pub not_modified: bool,
}

/// Persisted collection of configured OSV, RustSec, and GHSA authorities.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdvisoryAuthority {
    /// Maximum age accepted as fresh evidence.
    pub max_age_secs: u64,
    /// Whether resolver calls are made under the offline product policy.
    #[serde(default)]
    offline: bool,
    /// Sources enabled by the current process composition. Old cached source
    /// bodies remain auditable but do not silently become active again.
    #[serde(default)]
    configured: BTreeSet<AdvisorySource>,
    /// Per-source object journals.
    journals: BTreeMap<AdvisorySource, AdvisoryJournal>,
    /// Per-source durable frontier records.
    frontiers: BTreeMap<AdvisorySource, AuthorityFrontier>,
}

impl AdvisoryAuthority {
    /// Creates a new empty authority set. No configured source means unknown
    /// coverage and therefore a warning at the product boundary.
    #[must_use]
    pub fn new(max_age_secs: u64) -> Self {
        Self {
            max_age_secs,
            offline: false,
            configured: BTreeSet::new(),
            journals: BTreeMap::new(),
            frontiers: BTreeMap::new(),
        }
    }

    /// Selects the source set which participates in coverage decisions.
    /// Cached bodies for removed sources are retained for audit/replay.
    pub fn configure_sources(&mut self, sources: impl IntoIterator<Item = AdvisorySource>) {
        self.configured = sources.into_iter().collect();
    }

    /// Updates the freshness window selected by the current process without
    /// rewriting source bodies.
    pub const fn set_max_age_secs(&mut self, max_age_secs: u64) {
        self.max_age_secs = max_age_secs;
    }

    /// Sets the offline bit used by the immutable resolver seam.
    pub const fn set_offline(&mut self, offline: bool) {
        self.offline = offline;
    }

    /// Opens a durable authority state file, recovering to an empty frontier
    /// only when it does not exist.
    pub fn open(path: impl AsRef<Path>, max_age_secs: u64) -> Result<Self, AuthorityStorageError> {
        let path = path.as_ref();
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(AuthorityStorageError::Decode),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::new(max_age_secs))
            }
            Err(error) => Err(AuthorityStorageError::Io(error)),
        }
    }

    /// Atomically persists the current source frontiers and journals.
    pub fn persist(&self, path: impl AsRef<Path>) -> Result<(), AuthorityStorageError> {
        let path = path.as_ref();
        let parent = path.parent().ok_or(AuthorityStorageError::NoParent)?;
        fs::create_dir_all(parent).map_err(AuthorityStorageError::Io)?;
        let bytes = serde_json::to_vec(self).map_err(AuthorityStorageError::Encode)?;
        let temporary = path.with_extension("json.tmp");
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(AuthorityStorageError::Io)?;
        file.write_all(&bytes).map_err(AuthorityStorageError::Io)?;
        file.sync_all().map_err(AuthorityStorageError::Io)?;
        drop(file);
        fs::rename(temporary, path).map_err(AuthorityStorageError::Io)
    }

    /// Applies one source body transactionally and advances its frontier.
    pub fn apply(
        &mut self,
        feed: AuthorityFeed,
    ) -> Result<&AuthorityFrontier, AuthorityApplyError> {
        let source = feed.source;
        self.configured.insert(source);
        let mut journal = self.journals.get(&source).cloned().unwrap_or_default();
        if feed.freshness.not_modified && journal.checkpoint().is_none() {
            return Err(AuthorityApplyError::NotModifiedWithoutFrontier(source));
        }
        let entries = feed
            .entries
            .into_iter()
            .map(AdvisoryDelta::Upsert)
            .collect();
        let checkpoint = journal
            .apply(AdvisorySync {
                mode: feed.mode,
                complete: feed.complete,
                entries,
                freshness: feed.freshness.clone(),
            })
            .map_err(AuthorityApplyError::Journal)?
            .clone();
        let frontier = AuthorityFrontier {
            source,
            sequence: checkpoint.sequence,
            digest: checkpoint.digest,
            etag: checkpoint.freshness.etag.clone(),
            last_modified: checkpoint.freshness.last_modified.clone(),
            observed_at: checkpoint.freshness.observed_at,
            complete: feed.complete,
            availability: AuthorityAvailability::Available,
            entries: u64::try_from(journal.iter().count()).unwrap_or(u64::MAX),
            not_modified: checkpoint.freshness.not_modified,
        };
        self.journals.insert(source, journal);
        self.frontiers.insert(source, frontier);
        self.frontiers
            .get(&source)
            .ok_or(AuthorityApplyError::Invariant)
    }

    /// Records an unreachable source without destroying the last usable cache.
    pub fn mark_unavailable(
        &mut self,
        source: AdvisorySource,
        observed_at: u64,
    ) -> &AuthorityFrontier {
        let previous = self.frontiers.get(&source);
        let frontier = AuthorityFrontier {
            source,
            sequence: previous.map_or(0, |value| value.sequence),
            digest: previous.map_or([0; 32], |value| value.digest),
            etag: previous.and_then(|value| value.etag.clone()),
            last_modified: previous.and_then(|value| value.last_modified.clone()),
            // An outage is a new availability fact, not new advisory evidence. Keep the
            // previous observation time so cached evidence becomes stale on its original clock.
            observed_at: previous.map_or(observed_at, |value| value.observed_at),
            complete: previous.is_some_and(|value| value.complete),
            availability: AuthorityAvailability::Unavailable,
            entries: previous.map_or(0, |value| value.entries),
            not_modified: false,
        };
        self.frontiers.insert(source, frontier);
        self.frontiers.get(&source).expect("inserted frontier")
    }

    /// Reads one source's current conditional validators.
    #[must_use]
    pub fn frontier(&self, source: AdvisorySource) -> Option<&AuthorityFrontier> {
        self.frontiers.get(&source)
    }

    /// Iterates configured source frontiers in stable authority order.
    pub fn frontiers(&self) -> impl Iterator<Item = &AuthorityFrontier> {
        self.configured
            .iter()
            .filter_map(|source| self.frontiers.get(source))
    }

    /// Resolves an exact package/version against all configured source
    /// frontiers and keeps yanked/unlisted facts independent from advisories.
    #[must_use]
    pub fn observe(
        &self,
        package: &PackageIdentity,
        version: &str,
        yanked: bool,
        unlisted: bool,
        now: u64,
        offline: bool,
    ) -> AdvisoryObservation {
        if self.configured.is_empty() {
            return AdvisoryObservation {
                advisories: Box::new([]),
                coverage: AdvisoryCoverage::Unknown,
                freshness: FreshnessState::Unknown,
                offline,
                yanked,
                unlisted,
                malware: MalwareCoverage::NotCovered,
            };
        }
        let mut advisories = Vec::new();
        let mut complete = true;
        let mut partial = false;
        let mut unavailable = false;
        let mut missing = false;
        let mut stale = false;
        let mut not_modified = true;
        for source in &self.configured {
            let Some(frontier) = self.frontiers.get(source) else {
                complete = false;
                partial = true;
                missing = true;
                continue;
            };
            if frontier.availability == AuthorityAvailability::Unavailable {
                unavailable = true;
            }
            if !frontier.complete {
                complete = false;
                partial = true;
            }
            if now.saturating_sub(frontier.observed_at) > self.max_age_secs {
                stale = true;
            }
            not_modified &= frontier.not_modified;
            if let Some(journal) = self.journals.get(source) {
                advisories.extend(
                    AcquisitionGate::matching(
                        journal.iter().filter(|a| !a.is_withdrawn()),
                        package,
                        version,
                    )
                    .into_iter()
                    .cloned(),
                );
            }
        }
        advisories.sort_by_key(|advisory| advisory.key.canonical.clone());
        advisories.dedup_by_key(|advisory| advisory.key.canonical.clone());
        let coverage = if unavailable {
            AdvisoryCoverage::Unavailable
        } else if complete {
            AdvisoryCoverage::Complete
        } else if partial {
            AdvisoryCoverage::Partial
        } else {
            AdvisoryCoverage::Unknown
        };
        let freshness = if missing {
            FreshnessState::Unknown
        } else if stale {
            FreshnessState::Stale
        } else if not_modified {
            FreshnessState::NotModified
        } else {
            FreshnessState::Fresh
        };
        // GHSA's global feed carries an explicit malware statement for every object. Other
        // authorities remain advisory-only, so a complete OSV/RustSec frontier never masquerades
        // as malware coverage.
        let malware = if self.configured.contains(&AdvisorySource::Ghsa)
            && self
                .frontiers
                .get(&AdvisorySource::Ghsa)
                .is_some_and(|frontier| {
                    frontier.complete && frontier.availability == AuthorityAvailability::Available
                }) {
            MalwareCoverage::Covered
        } else {
            MalwareCoverage::NotCovered
        };
        AdvisoryObservation {
            advisories: advisories.into_boxed_slice(),
            coverage,
            freshness,
            offline,
            yanked,
            unlisted,
            malware,
        }
    }

    /// Compact policy projection used by callers which want to validate a
    /// refresh before exposing it to a product surface.
    #[must_use]
    pub fn decide(
        &self,
        package: &PackageIdentity,
        version: &str,
        gate: AcquisitionGate,
        now: u64,
        offline: bool,
    ) -> (AdvisoryObservation, AcquisitionDecision) {
        let observation = self.observe(package, version, false, false, now, offline);
        let decision = gate.decide(&observation);
        (observation, decision)
    }

    /// Returns every admitted object in source order for diagnostics and
    /// deterministic fixture inspection.
    pub fn iter(&self) -> impl Iterator<Item = &Advisory> {
        self.journals.values().flat_map(AdvisoryJournal::iter)
    }
}

impl AdvisoryResolver for AdvisoryAuthority {
    fn observe(
        &self,
        package: &PackageIdentity,
        version: &str,
        yanked: bool,
        unlisted: bool,
    ) -> AdvisoryObservation {
        self.observe(
            package,
            version,
            yanked,
            unlisted,
            unix_seconds(),
            self.offline,
        )
    }
}

/// Portable resolver seam consumed by the registry owner.
pub trait AdvisoryResolver: Send + Sync {
    /// Resolves one exact release against an immutable authority snapshot.
    fn observe(
        &self,
        package: &PackageIdentity,
        version: &str,
        yanked: bool,
        unlisted: bool,
    ) -> AdvisoryObservation;
}

/// Durable authority storage failure.
#[derive(Debug)]
pub enum AuthorityStorageError {
    /// Filesystem operation failed.
    Io(std::io::Error),
    /// Persisted state was not valid JSON.
    Decode(serde_json::Error),
    /// State could not be encoded.
    Encode(serde_json::Error),
    /// Target path has no parent directory.
    NoParent,
}
impl std::fmt::Display for AuthorityStorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "advisory authority storage failed: {self:?}")
    }
}
impl std::error::Error for AuthorityStorageError {}

/// Authority state could not admit one complete source transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorityApplyError {
    /// A conditional validator was accepted before a body existed.
    NotModifiedWithoutFrontier(AdvisorySource),
    /// The underlying copy-on-write journal rejected a semantic conflict.
    Journal(AdvisoryJournalError),
    /// Internal map insertion invariant failed.
    Invariant,
}

impl std::fmt::Display for AuthorityApplyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "advisory authority apply failed: {self:?}")
    }
}
impl std::error::Error for AuthorityApplyError {}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Reads a bounded local authority body. Directory walking is intentionally
/// left to the local-service composition root, where its I/O policy belongs.
pub fn read_feed(path: impl AsRef<Path>, maximum: usize) -> Result<Vec<u8>, AuthorityStorageError> {
    let metadata = fs::metadata(path.as_ref()).map_err(AuthorityStorageError::Io)?;
    if metadata.len() > u64::try_from(maximum).unwrap_or(u64::MAX) {
        return Err(AuthorityStorageError::Io(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            "advisory source exceeds configured bound",
        )));
    }
    fs::read(path).map_err(AuthorityStorageError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn osv() -> Vec<u8> {
        br#"{"schema_version":"1.3.1","id":"OSV-AUTH-1","modified":"2026-01-02T00:00:00Z","affected":[{"package":{"ecosystem":"Cargo","name":"demo"},"ranges":[{"type":"SEMVER","events":[{"introduced":"0"},{"fixed":"2.0.0"}]}]}]}"#.to_vec()
    }

    #[test]
    fn source_batches_are_durable_and_exact_versioned() {
        let feed = AuthorityFeed::parse(AdvisorySource::Osv, &osv(), 10, Some("a".into()), None)
            .expect("OSV fixture");
        let mut authority = AdvisoryAuthority::new(100);
        authority.apply(feed).expect("admit source");
        let package = super::super::normalize_package("cargo", "demo").expect("identity");
        let observation = authority.observe(&package, "1.0.0", false, false, 10, false);
        assert_eq!(observation.coverage, AdvisoryCoverage::Complete);
        assert_eq!(observation.advisories.len(), 1);
        let clean = authority.observe(&package, "2.0.0", false, false, 10, false);
        assert!(clean.advisories.is_empty());
        assert_eq!(clean.coverage, AdvisoryCoverage::Complete);
    }

    #[test]
    fn withdrawal_remains_auditable_but_stops_matching() {
        let feed =
            AuthorityFeed::parse(AdvisorySource::Osv, &osv(), 10, None, None).expect("OSV fixture");
        let mut authority = AdvisoryAuthority::new(100);
        authority.apply(feed).expect("admit source");
        let key = authority
            .iter()
            .next()
            .expect("advisory")
            .key
            .canonical
            .clone();
        let evidence = authority.iter().next().expect("evidence").evidence.clone();
        let journal = authority
            .journals
            .get_mut(&AdvisorySource::Osv)
            .expect("journal");
        journal
            .apply(AdvisorySync {
                mode: SyncMode::Delta,
                complete: true,
                entries: vec![AdvisoryDelta::Withdraw {
                    key,
                    withdrawn: "2026-02-01T00:00:00Z".into(),
                    evidence,
                }],
                freshness: FeedFreshness {
                    etag: None,
                    last_modified: None,
                    observed_at: 11,
                    not_modified: false,
                },
            })
            .expect("withdraw");
        let package = super::super::normalize_package("cargo", "demo").expect("identity");
        assert!(
            authority
                .observe(&package, "1.0.0", false, false, 11, false)
                .advisories
                .is_empty()
        );
        assert_eq!(authority.iter().count(), 1);
    }

    #[test]
    fn no_authority_is_unknown_and_304_before_genesis_is_rejected() {
        let authority = AdvisoryAuthority::new(100);
        let package = super::super::normalize_package("cargo", "demo").expect("identity");
        assert_eq!(
            authority
                .observe(&package, "1.0.0", false, false, 1, false)
                .coverage,
            AdvisoryCoverage::Unknown
        );
        let mut authority = AdvisoryAuthority::new(100);
        let error = authority
            .apply(AuthorityFeed::not_modified(
                AdvisorySource::Osv,
                1,
                None,
                None,
            ))
            .expect_err("304 without body");
        assert_eq!(
            error,
            AuthorityApplyError::NotModifiedWithoutFrontier(AdvisorySource::Osv)
        );
    }

    #[test]
    fn restart_preserves_frontier_and_unavailable_is_explicit() {
        let feed =
            AuthorityFeed::parse(AdvisorySource::Osv, &osv(), 10, Some("etag-1".into()), None)
                .expect("OSV fixture");
        let mut authority = AdvisoryAuthority::new(5);
        authority.apply(feed).expect("admit source");
        authority.mark_unavailable(AdvisorySource::Osv, 20);
        let bytes = serde_json::to_vec(&authority).expect("encode authority");
        let restored: AdvisoryAuthority = serde_json::from_slice(&bytes).expect("decode authority");
        let package = super::super::normalize_package("cargo", "demo").expect("identity");
        let observation = restored.observe(&package, "1.0.0", false, false, 30, true);
        assert_eq!(observation.coverage, AdvisoryCoverage::Unavailable);
        assert_eq!(observation.freshness, FreshnessState::Stale);
        assert_eq!(
            restored
                .frontier(AdvisorySource::Osv)
                .and_then(|frontier| frontier.etag.as_deref()),
            Some("etag-1")
        );
    }
}
