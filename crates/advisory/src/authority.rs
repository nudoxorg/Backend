//! Durable composition of the supported advisory authorities.
//!
//! The registry owner owns release bytes. This module owns the other side of
//! that boundary: a versioned, source-qualified security frontier which can
//! be refreshed independently and joined to an exact package/version at
//! acquisition time. A missing frontier remains visible as unknown coverage;
//! it is never converted into a clean result.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use backend_platform::directory::DirectoryCapability;
use serde::{Deserialize, Serialize};

use super::osv_snapshot::{OsvSnapshotError, OsvSnapshotRef};
use super::parse::{parse_ghsa_value, parse_osv_value};
use super::{
    AcquisitionDecision, AcquisitionGate, Advisory, AdvisoryCoverage, AdvisoryDelta,
    AdvisoryJournal, AdvisoryJournalError, AdvisoryObservation, AdvisorySource, AdvisorySync,
    Alias, AliasGraph, AliasGraphError, Checkpoint, FeedFreshness, FreshnessState, GhsaParseError,
    MAX_ADVISORY_BATCH_OBJECTS, MAX_ADVISORY_DOCUMENT_BYTES, MalwareCoverage, OsvFeedScope,
    PackageIdentity, ParseError, RustSecParseError, SyncMode,
};

#[cfg(test)]
use super::OsvEcosystem;

/// Default hard ceiling for one persisted advisory authority JSON state file.
pub const MAX_ADVISORY_AUTHORITY_STATE_BYTES: u64 = 512 * 1024 * 1024;

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
    /// Exact OSV scope selected for this feed. A scoped frontier is complete
    /// only when this matches the source scope selected on the authority.
    pub osv_scope: Option<OsvFeedScope>,
    /// Immutable package-keyed generation for a large complete OSV archive.
    /// When present, `entries` must be empty and the source is read through
    /// its package index at decision time.
    pub osv_snapshot: Option<OsvSnapshotRef>,
    /// Opaque digest binding validators to the configured source location and
    /// selected scope. Raw endpoints and credentials are never persisted.
    pub source_identity: Option<[u8; 32]>,
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
        if bytes.len() > MAX_ADVISORY_DOCUMENT_BYTES {
            return Err(AuthorityParseError::BoundExceeded("document-bytes"));
        }
        let entries = match source {
            AdvisorySource::Osv => parse_osv_feed(bytes, observed_at)?,
            AdvisorySource::RustSec => vec![super::parse_rustsec(bytes, observed_at)?],
            AdvisorySource::Ghsa => parse_ghsa_feed(bytes, observed_at)?,
        };
        let snapshot = etag
            .clone()
            .or_else(|| last_modified.clone())
            .or_else(|| Some(blake3::hash(bytes).to_hex().to_string()));
        let mut entries = entries;
        for advisory in &mut entries {
            advisory.evidence.snapshot = snapshot.clone();
        }
        entries.sort_by(|left, right| left.key.native.cmp(&right.key.native));
        Ok(Self {
            source,
            mode: SyncMode::Snapshot,
            // One RustSec document is one advisory, not the database: it cannot
            // vouch that every other package is clean. The database is a
            // directory tree, admitted through [`Self::from_entries`].
            complete: source != AdvisorySource::RustSec,
            osv_scope: None,
            osv_snapshot: None,
            source_identity: None,
            freshness: FeedFreshness {
                etag,
                last_modified,
                observed_at,
                expires_at: None,
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
        mut entries: Vec<Advisory>,
        observed_at: u64,
        etag: Option<String>,
        last_modified: Option<String>,
    ) -> Self {
        let snapshot = etag.clone().or_else(|| last_modified.clone());
        for advisory in &mut entries {
            if advisory.evidence.snapshot.is_none() {
                advisory.evidence.snapshot = snapshot.clone();
            }
        }
        entries.sort_by(|left, right| left.key.native.cmp(&right.key.native));
        Self {
            source,
            mode: SyncMode::Snapshot,
            complete: true,
            osv_scope: None,
            osv_snapshot: None,
            source_identity: None,
            freshness: FeedFreshness {
                etag,
                last_modified,
                observed_at,
                expires_at: None,
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
            osv_scope: None,
            osv_snapshot: None,
            source_identity: None,
            freshness: FeedFreshness {
                etag,
                last_modified,
                observed_at,
                expires_at: None,
                not_modified: true,
            },
            entries: Vec::new(),
        }
    }

    /// Builds a complete OSV feed backed by a sealed disk generation.
    #[must_use]
    pub fn from_osv_snapshot(
        snapshot: OsvSnapshotRef,
        observed_at: u64,
        etag: Option<String>,
        last_modified: Option<String>,
    ) -> Self {
        Self {
            source: AdvisorySource::Osv,
            mode: SyncMode::Snapshot,
            complete: true,
            osv_scope: Some(snapshot.scope()),
            osv_snapshot: Some(snapshot),
            source_identity: None,
            freshness: FeedFreshness {
                etag,
                last_modified,
                observed_at,
                expires_at: None,
                not_modified: false,
            },
            entries: Vec::new(),
        }
    }
}

fn parse_osv_feed(bytes: &[u8], observed_at: u64) -> Result<Vec<Advisory>, AuthorityParseError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| AuthorityParseError::Osv(ParseError::InvalidJson))?;
    if let Some(values) = value.as_array() {
        if values.len() > MAX_ADVISORY_BATCH_OBJECTS {
            return Err(AuthorityParseError::BoundExceeded("batch-objects"));
        }
        return values
            .iter()
            .map(|value| parse_osv_value(value, observed_at).map_err(AuthorityParseError::Osv))
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
        Some(values) => {
            if values.len() > MAX_ADVISORY_BATCH_OBJECTS {
                return Err(AuthorityParseError::BoundExceeded("batch-objects"));
            }
            values
                .iter()
                .map(|value| parse_osv_value(value, observed_at).map_err(AuthorityParseError::Osv))
                .collect()
        }
        None => Ok(vec![
            parse_osv_value(&value, observed_at).map_err(AuthorityParseError::Osv)?,
        ]),
    }
}

fn parse_ghsa_feed(bytes: &[u8], observed_at: u64) -> Result<Vec<Advisory>, AuthorityParseError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| AuthorityParseError::Ghsa(GhsaParseError::InvalidJson))?;
    if let Some(values) = value.as_array() {
        if values.len() > MAX_ADVISORY_BATCH_OBJECTS {
            return Err(AuthorityParseError::BoundExceeded("batch-objects"));
        }
        return values
            .iter()
            .map(|value| parse_ghsa_value(value, observed_at).map_err(AuthorityParseError::Ghsa))
            .collect();
    }
    let Some(object) = value.as_object() else {
        return Err(AuthorityParseError::Ghsa(GhsaParseError::InvalidJson));
    };
    if let Some(values) = object
        .get("advisories")
        .and_then(serde_json::Value::as_array)
    {
        if values.len() > MAX_ADVISORY_BATCH_OBJECTS {
            return Err(AuthorityParseError::BoundExceeded("batch-objects"));
        }
        return values
            .iter()
            .map(|value| parse_ghsa_value(value, observed_at).map_err(AuthorityParseError::Ghsa))
            .collect();
    }
    Ok(vec![
        parse_ghsa_value(&value, observed_at).map_err(AuthorityParseError::Ghsa)?,
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
    /// A feed exceeded a parser bound before allocation/admission.
    BoundExceeded(&'static str),
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
    /// OSV database partition represented by this frontier. Other authorities
    /// do not use this scope.
    #[serde(default)]
    pub osv_scope: Option<OsvFeedScope>,
    /// Opaque location/scope identity used to prevent cross-source 304 reuse.
    #[serde(default)]
    pub source_identity: Option<[u8; 32]>,
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
    /// Source-provided freshness deadline, when available.
    #[serde(default)]
    pub expires_at: Option<u64>,
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
    /// Process-local serialization ceiling; derived from source policy and
    /// never trusted from a persisted state file.
    #[serde(skip)]
    maximum_state_bytes: u64,
    /// Process-selected endpoint identities. Persisted frontiers are compared
    /// against these after composition so an old endpoint cannot establish
    /// current coverage before an explicit refresh.
    #[serde(skip)]
    selected_source_identities: BTreeMap<AdvisorySource, [u8; 32]>,
    /// Whether resolver calls are made under the offline product policy.
    #[serde(default)]
    offline: bool,
    /// Sources enabled by the current process composition. Old cached source
    /// bodies remain auditable but do not silently become active again.
    #[serde(default)]
    configured: BTreeSet<AdvisorySource>,
    /// Selected OSV partition. `None` means an old or unselected source whose
    /// coverage must remain incomplete until a scoped snapshot is admitted.
    #[serde(default)]
    osv_scope: Option<OsvFeedScope>,
    /// Selected immutable OSV package index for a large streamed archive.
    #[serde(default)]
    osv_snapshot: Option<OsvSnapshotRef>,
    /// Previous durable OSV generation retained as a rollback/audit point.
    #[serde(default)]
    osv_previous_snapshot: Option<OsvSnapshotRef>,
    /// Cross-authority alias graph.  Per-source journals retain their own source-local graph for
    /// transactional admission; this graph prevents OSV/RustSec/GHSA feeds from silently
    /// disagreeing about one shared CVE/GHSA identity.
    #[serde(default)]
    aliases: AliasGraph,
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
            maximum_state_bytes: MAX_ADVISORY_AUTHORITY_STATE_BYTES,
            selected_source_identities: BTreeMap::new(),
            offline: false,
            configured: BTreeSet::new(),
            osv_scope: Some(OsvFeedScope::All),
            osv_snapshot: None,
            osv_previous_snapshot: None,
            aliases: AliasGraph::default(),
            journals: BTreeMap::new(),
            frontiers: BTreeMap::new(),
        }
    }

    /// Selects the source set which participates in coverage decisions.
    /// Cached bodies for removed sources are retained for audit/replay.
    pub fn configure_sources(&mut self, sources: impl IntoIterator<Item = AdvisorySource>) {
        self.configured = sources.into_iter().collect();
    }

    /// Binds configured authorities to the opaque identities of their current
    /// endpoint and scope. Old cached facts remain positive evidence, but do
    /// not provide clean coverage for a newly selected source.
    pub fn configure_source_identities(
        &mut self,
        identities: impl IntoIterator<Item = (AdvisorySource, [u8; 32])>,
    ) {
        self.selected_source_identities = identities.into_iter().collect();
    }

    /// Selects the OSV dataset whose coverage is admitted by this authority.
    /// A changed selection invalidates the old OSV frontier until that exact
    /// partition has been refreshed.
    pub const fn configure_osv_scope(&mut self, scope: Option<OsvFeedScope>) {
        self.osv_scope = scope;
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
        Self::open_with_limit(path, max_age_secs, MAX_ADVISORY_AUTHORITY_STATE_BYTES)
    }

    /// Opens a durable authority state under the caller's state-file byte
    /// ceiling. The limit is checked before allocation and one byte beyond it
    /// is read to close the stat/read race.
    pub fn open_with_limit(
        path: impl AsRef<Path>,
        max_age_secs: u64,
        maximum_state_bytes: u64,
    ) -> Result<Self, AuthorityStorageError> {
        let path = path.as_ref();
        let parent = path
            .parent()
            .filter(|value| !value.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(AuthorityStorageError::InvalidStateFile)?;
        let directory = match DirectoryCapability::open(parent) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut authority = Self::new(max_age_secs);
                authority.maximum_state_bytes = maximum_state_bytes;
                authority.prune_unselected_snapshot_generations(path)?;
                return Ok(authority);
            }
            Err(error) => return Err(AuthorityStorageError::Io(error)),
        };
        let file = match directory.open_file_read(name) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut authority = Self::new(max_age_secs);
                authority.maximum_state_bytes = maximum_state_bytes;
                authority.prune_unselected_snapshot_generations(path)?;
                return Ok(authority);
            }
            Err(error) => return Err(AuthorityStorageError::Io(error)),
        };
        let opened_metadata = file.metadata().map_err(AuthorityStorageError::Io)?;
        if !opened_metadata.is_file() {
            return Err(AuthorityStorageError::InvalidStateFile);
        }
        if opened_metadata.len() > maximum_state_bytes {
            return Err(AuthorityStorageError::BoundExceeded);
        }
        let mut bytes = Vec::new();
        file.take(maximum_state_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(AuthorityStorageError::Io)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum_state_bytes {
            return Err(AuthorityStorageError::BoundExceeded);
        }
        let mut authority: Self =
            serde_json::from_slice(&bytes).map_err(AuthorityStorageError::Decode)?;
        authority
            .rebuild_aliases()
            .map_err(AuthorityStorageError::AliasConflict)?;
        let selected_snapshot_unavailable = authority
            .osv_snapshot
            .as_mut()
            .is_some_and(|snapshot| snapshot.attach_root(osv_snapshot_root(path)).is_err());
        if selected_snapshot_unavailable {
            authority.osv_snapshot = None;
            if let Some(frontier) = authority.frontiers.get_mut(&AdvisorySource::Osv) {
                frontier.availability = AuthorityAvailability::Unavailable;
            }
        }
        let previous_snapshot_unavailable = authority
            .osv_previous_snapshot
            .as_mut()
            .is_some_and(|snapshot| snapshot.attach_root(osv_snapshot_root(path)).is_err());
        if previous_snapshot_unavailable {
            authority.osv_previous_snapshot = None;
        }
        authority.max_age_secs = max_age_secs;
        authority.maximum_state_bytes = maximum_state_bytes;
        authority.prune_unselected_snapshot_generations(path)?;
        Ok(authority)
    }

    fn prune_unselected_snapshot_generations(
        &self,
        authority_path: &Path,
    ) -> Result<(), AuthorityStorageError> {
        let mut retained = BTreeSet::new();
        if let Some(snapshot) = self.osv_snapshot.as_ref() {
            retained.insert(snapshot.generation_id().to_owned());
        }
        if let Some(snapshot) = self.osv_previous_snapshot.as_ref() {
            retained.insert(snapshot.generation_id().to_owned());
        }
        OsvSnapshotRef::prune_unreferenced_generations(
            osv_snapshot_root(authority_path),
            &retained,
        )
        .map_err(AuthorityStorageError::Snapshot)?;
        Ok(())
    }

    /// Returns the durable root for immutable OSV generations adjacent to an
    /// advisory authority journal.
    #[must_use]
    pub fn osv_snapshot_root(path: impl AsRef<Path>) -> PathBuf {
        osv_snapshot_root(path.as_ref())
    }

    /// Atomically persists the current source frontiers and journals.
    pub fn persist(&self, path: impl AsRef<Path>) -> Result<(), AuthorityStorageError> {
        let path = path.as_ref();
        let parent = path.parent().ok_or(AuthorityStorageError::NoParent)?;
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        fs::create_dir_all(parent).map_err(AuthorityStorageError::Io)?;
        let directory = DirectoryCapability::open(parent).map_err(AuthorityStorageError::Io)?;
        // A fixed sibling name turns a crash left behind by a previous process into a permanent
        // persistence outage.  A process-local nonce keeps concurrent writers independent while
        // the final rename remains the single atomic publication point.
        static PERSIST_NONCE: AtomicU64 = AtomicU64::new(0);
        let temporary = format!(
            ".{}.{}.{}.tmp",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("advisory-authority"),
            std::process::id(),
            PERSIST_NONCE.fetch_add(1, Ordering::Relaxed)
        );
        let mut file = directory
            .create_file_exclusive(&temporary)
            .map_err(AuthorityStorageError::Io)?;
        let (encoded, exceeded, write_error) = {
            let mut writer = BoundedStateWriter {
                file: &mut file,
                written: 0,
                maximum: self.maximum_state_bytes,
                exceeded: false,
                write_error: None,
            };
            let encoded = serde_json::to_writer(&mut writer, self);
            (encoded, writer.exceeded, writer.write_error)
        };
        if exceeded {
            drop(file);
            let _ = directory.remove_file(&temporary);
            return Err(AuthorityStorageError::BoundExceeded);
        }
        if let Some(kind) = write_error {
            drop(file);
            let _ = directory.remove_file(&temporary);
            return Err(AuthorityStorageError::Io(std::io::Error::new(
                kind,
                "authority state write failed",
            )));
        }
        if let Err(error) = encoded {
            drop(file);
            let _ = directory.remove_file(&temporary);
            return Err(AuthorityStorageError::Encode(error));
        }
        if let Err(error) = file.sync_all() {
            drop(file);
            let _ = directory.remove_file(&temporary);
            return Err(AuthorityStorageError::Io(error));
        }
        drop(file);
        let destination = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(AuthorityStorageError::InvalidStateFile)?;
        if let Err(error) = directory.rename(&temporary, destination, true) {
            let _ = directory.remove_file(&temporary);
            return Err(AuthorityStorageError::Io(error));
        }
        // The file is durable before the rename; syncing the directory makes the name update
        // durable as well on filesystems which otherwise allow a power loss between the two.
        if let Err(error) = directory.sync_all()
            && !matches!(
                error.kind(),
                std::io::ErrorKind::Unsupported | std::io::ErrorKind::PermissionDenied
            )
        {
            return Err(AuthorityStorageError::Io(error));
        }
        if let Some(snapshot) = self.osv_snapshot.as_ref() {
            snapshot
                .commit_persisted()
                .map_err(AuthorityStorageError::Snapshot)?;
        }
        if let Some(snapshot) = self.osv_previous_snapshot.as_ref() {
            snapshot
                .commit_persisted()
                .map_err(AuthorityStorageError::Snapshot)?;
        }
        if self.osv_snapshot.is_some() || self.osv_previous_snapshot.is_some() {
            let mut keep = BTreeSet::new();
            if let Some(snapshot) = self.osv_snapshot.as_ref() {
                keep.insert(snapshot.generation_id().to_owned());
            }
            if let Some(snapshot) = self.osv_previous_snapshot.as_ref() {
                keep.insert(snapshot.generation_id().to_owned());
            }
            let snapshot_root = self
                .osv_snapshot
                .as_ref()
                .and_then(OsvSnapshotRef::root_path)
                .or_else(|| {
                    self.osv_previous_snapshot
                        .as_ref()
                        .and_then(OsvSnapshotRef::root_path)
                })
                .map(Path::to_path_buf)
                .unwrap_or_else(|| osv_snapshot_root(path));
            OsvSnapshotRef::prune_unreferenced_generations(snapshot_root, &keep)
                .map_err(AuthorityStorageError::Snapshot)?;
        }
        Ok(())
    }

    /// Applies one source body transactionally and advances its frontier.
    pub fn apply(
        &mut self,
        feed: AuthorityFeed,
    ) -> Result<&AuthorityFrontier, AuthorityApplyError> {
        let source = feed.source;
        let feed_osv_scope = feed.osv_scope;
        let feed_source_identity = feed.source_identity;
        if self
            .selected_source_identities
            .get(&source)
            .is_some_and(|identity| feed_source_identity != Some(*identity))
        {
            return Err(AuthorityApplyError::ConfiguredSourceMismatch(source));
        }
        if feed.freshness.not_modified
            && feed_source_identity.is_some()
            && self
                .frontiers
                .get(&source)
                .is_some_and(|frontier| frontier.source_identity != feed_source_identity)
        {
            return Err(AuthorityApplyError::NotModifiedSourceMismatch(source));
        }
        let supplied_snapshot = feed.osv_snapshot.is_some();
        // Journals and package generations are source-local. When a registry
        // endpoint or selected OSV partition changes, an incomplete response
        // from the new source cannot inherit positive rows from the old one.
        // Keep the old immutable generation as the previous audit/rollback
        // point, but stop presenting it as evidence from the newly selected
        // source.
        let source_identity_changed = self.frontiers.get(&source).is_some_and(|frontier| {
            frontier.source_identity != feed_source_identity
                || (frontier.source_identity.is_none()
                    && source == AdvisorySource::Osv
                    && feed_osv_scope.is_some_and(|scope| frontier.osv_scope != Some(scope)))
        });
        let external_snapshot = feed.osv_snapshot.or_else(|| {
            (source == AdvisorySource::Osv && feed.freshness.not_modified)
                .then(|| self.osv_snapshot.clone())
                .flatten()
        });
        if supplied_snapshot && (source != AdvisorySource::Osv || !feed.entries.is_empty()) {
            return Err(AuthorityApplyError::UnexpectedSnapshot);
        }
        if external_snapshot
            .as_ref()
            .is_some_and(|snapshot| !snapshot.is_verified())
        {
            return Err(AuthorityApplyError::SnapshotUnavailable);
        }
        for advisory in &feed.entries {
            if advisory.key.native.source != source {
                return Err(AuthorityApplyError::SourceMismatch {
                    expected: source,
                    actual: advisory.key.native.source,
                });
            }
        }
        let mut aliases = self.aliases.clone();
        for advisory in &feed.entries {
            aliases
                .admit_identity(advisory)
                .map_err(AuthorityApplyError::AliasConflict)?;
        }
        let mut journal = if source_identity_changed {
            AdvisoryJournal::default()
        } else {
            self.journals.get(&source).cloned().unwrap_or_default()
        };
        if feed.freshness.not_modified
            && journal.checkpoint().is_none()
            && external_snapshot.is_none()
        {
            return Err(AuthorityApplyError::NotModifiedWithoutFrontier(source));
        }
        let previous_frontier = self.frontiers.get(&source);
        let complete = if feed.freshness.not_modified {
            previous_frontier.is_some_and(|frontier| frontier.complete)
        } else if let Some(snapshot) = external_snapshot.as_ref() {
            feed.complete
                && feed_osv_scope == Some(snapshot.scope())
                && feed_osv_scope == self.osv_scope
        } else if source == AdvisorySource::Osv {
            feed.complete && feed_osv_scope.is_some() && feed_osv_scope == self.osv_scope
        } else {
            feed.complete
        };
        let osv_scope = if source == AdvisorySource::Osv {
            if feed.freshness.not_modified {
                previous_frontier.and_then(|frontier| frontier.osv_scope)
            } else {
                feed_osv_scope
            }
        } else {
            None
        };
        let (checkpoint, active_entries) =
            if let Some(snapshot) = external_snapshot.as_ref() {
                let sequence = previous_frontier
                    .map_or(0, |frontier| frontier.sequence)
                    .checked_add(1)
                    .ok_or(AuthorityApplyError::Journal(
                        AdvisoryJournalError::SequenceOverflow,
                    ))?;
                let digest = if feed.freshness.not_modified {
                    previous_frontier
                        .map_or_else(|| snapshot.frontier_digest(), |frontier| frontier.digest)
                } else {
                    snapshot.frontier_digest()
                };
                let checkpoint = Checkpoint {
                    sequence,
                    digest,
                    freshness: feed.freshness.clone(),
                };
                if supplied_snapshot {
                    self.journals.remove(&source);
                    if self.osv_snapshot.as_ref().is_some_and(|selected| {
                        selected.generation_id() != snapshot.generation_id()
                    }) && let Some(previous) = self.osv_snapshot.take()
                    {
                        self.osv_previous_snapshot = Some(previous);
                    }
                }
                self.osv_snapshot = Some(snapshot.clone());
                (checkpoint, snapshot.advisory_objects())
            } else {
                if source_identity_changed
                    && source == AdvisorySource::Osv
                    && !feed.freshness.not_modified
                    && let Some(previous) = self.osv_snapshot.take()
                {
                    self.osv_previous_snapshot = Some(previous);
                }
                if source == AdvisorySource::Osv
                    && feed.complete
                    && !feed.freshness.not_modified
                    && feed_osv_scope == self.osv_scope
                {
                    if let Some(previous) = self.osv_snapshot.take() {
                        self.osv_previous_snapshot = Some(previous);
                    }
                }
                let entries = feed
                    .entries
                    .into_iter()
                    .map(AdvisoryDelta::Upsert)
                    .collect();
                let checkpoint = journal
                    .apply(AdvisorySync {
                        mode: feed.mode,
                        complete,
                        entries,
                        freshness: feed.freshness.clone(),
                    })
                    .map_err(AuthorityApplyError::Journal)?
                    .clone();
                (
                    checkpoint,
                    u64::try_from(journal.iter().count()).unwrap_or(u64::MAX),
                )
            };
        let frontier = AuthorityFrontier {
            source,
            osv_scope,
            source_identity: feed_source_identity,
            sequence: checkpoint.sequence,
            digest: checkpoint.digest,
            etag: checkpoint.freshness.etag.clone(),
            last_modified: checkpoint.freshness.last_modified.clone(),
            observed_at: checkpoint.freshness.observed_at,
            expires_at: if feed.freshness.not_modified {
                feed.freshness
                    .expires_at
                    .or_else(|| previous_frontier.and_then(|frontier| frontier.expires_at))
            } else {
                feed.freshness.expires_at
            },
            complete,
            availability: AuthorityAvailability::Available,
            entries: active_entries,
            not_modified: checkpoint.freshness.not_modified,
        };
        if !supplied_snapshot {
            self.journals.insert(source, journal);
        }
        self.frontiers.insert(source, frontier);
        self.aliases = aliases;
        self.configured.insert(source);
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
        // A failed first fetch is still a configured source.  Exposing that distinction lets
        // policy report `unavailable` instead of the much less actionable `unknown` state.
        self.configured.insert(source);
        let previous = self.frontiers.get(&source);
        let frontier = AuthorityFrontier {
            source,
            osv_scope: previous
                .and_then(|value| value.osv_scope)
                .or(self.osv_scope.filter(|_| source == AdvisorySource::Osv)),
            source_identity: previous.and_then(|value| value.source_identity),
            sequence: previous.map_or(0, |value| value.sequence),
            digest: previous.map_or([0; 32], |value| value.digest),
            etag: previous.and_then(|value| value.etag.clone()),
            last_modified: previous.and_then(|value| value.last_modified.clone()),
            // An outage is a new availability fact, not new advisory evidence. Keep the
            // previous observation time so cached evidence becomes stale on its original clock.
            observed_at: previous.map_or(observed_at, |value| value.observed_at),
            expires_at: previous.and_then(|value| value.expires_at),
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
            let identity_mismatch = self
                .selected_source_identities
                .get(source)
                .is_some_and(|identity| frontier.source_identity != Some(*identity));
            if identity_mismatch {
                complete = false;
                partial = true;
                missing = true;
            }
            let mut scope_matches = true;
            if *source == AdvisorySource::Osv {
                let selected_scope = match (self.osv_scope, frontier.osv_scope) {
                    (Some(configured), Some(observed)) if configured == observed => {
                        Some(Some(configured))
                    }
                    (None, None) => Some(None),
                    _ => None,
                };
                match selected_scope {
                    Some(Some(OsvFeedScope::All))
                        if matches!(
                            package.ecosystem.as_str(),
                            "cargo" | "npm" | "pypi" | "maven" | "nuget" | "go"
                        ) => {}
                    Some(Some(OsvFeedScope::All)) => {
                        scope_matches = false;
                        complete = false;
                        partial = true;
                    }
                    Some(Some(OsvFeedScope::Ecosystem(ecosystem)))
                        if package.ecosystem == ecosystem.package_ecosystem() => {}
                    Some(Some(OsvFeedScope::Ecosystem(_))) | None => {
                        scope_matches = false;
                        complete = false;
                        partial = true;
                    }
                    Some(None) => {
                        // An unselected JSON source can contribute positive
                        // matches, but it cannot establish absence across any
                        // ecosystem.
                        complete = false;
                        partial = true;
                    }
                }
            }
            // Positive rows remain useful when the configured identity still
            // proves they came from the selected endpoint/scope, even when an
            // individual response covers only part of that source. If identity
            // is unknown and a known scope conflicts, withhold the rows rather
            // than presenting old-source evidence under the new selection.
            let selected_identity_proven = self
                .selected_source_identities
                .get(source)
                .is_some_and(|identity| frontier.source_identity == Some(*identity));
            let may_use_positive_facts = !identity_mismatch
                && (selected_identity_proven || scope_matches || frontier.osv_scope.is_none());
            let mut selected_osv_advisories = (*source == AdvisorySource::Osv
                && may_use_positive_facts
                && self.osv_snapshot.is_some())
            .then(BTreeMap::new);
            if frontier.availability == AuthorityAvailability::Unavailable {
                unavailable = true;
            }
            if !frontier.complete {
                complete = false;
                partial = true;
            }
            let policy_expiry = frontier.observed_at.saturating_add(self.max_age_secs);
            let expires_at = frontier
                .expires_at
                .map_or(policy_expiry, |source| source.min(policy_expiry));
            if now >= expires_at {
                stale = true;
            }
            not_modified &= frontier.not_modified;
            if *source == AdvisorySource::Osv
                && may_use_positive_facts
                && let Some(snapshot) = self.osv_snapshot.as_ref()
            {
                match snapshot.matching(package, version) {
                    Ok((matches, unresolved)) => {
                        complete &= !unresolved;
                        partial |= unresolved;
                        let (matches, alias_conflict) =
                            reconcile_snapshot_aliases(&self.aliases, matches);
                        if alias_conflict {
                            unavailable = true;
                            complete = false;
                            partial = true;
                            missing = true;
                        }
                        if let Some(selected) = selected_osv_advisories.as_mut() {
                            selected.extend(
                                matches
                                    .into_iter()
                                    .map(|advisory| (advisory.key.canonical.clone(), advisory)),
                            );
                        } else {
                            advisories.extend(matches);
                        }
                    }
                    Err(_) => {
                        unavailable = true;
                        complete = false;
                        partial = true;
                        missing = true;
                    }
                }
            }
            if may_use_positive_facts && let Some(journal) = self.journals.get(source) {
                let (matches, unresolved) = AcquisitionGate::matching_with_coverage(
                    journal.iter().filter(|a| !a.is_withdrawn()),
                    package,
                    version,
                );
                complete &= !unresolved;
                partial |= unresolved;
                if let Some(selected) = selected_osv_advisories.as_mut() {
                    selected.extend(
                        matches
                            .into_iter()
                            .cloned()
                            .map(|advisory| (advisory.key.canonical.clone(), advisory)),
                    );
                } else {
                    advisories.extend(matches.into_iter().cloned());
                }
            }
            if let Some(selected) = selected_osv_advisories {
                advisories.extend(selected.into_values());
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

    /// Returns resident in-memory objects for diagnostics and deterministic
    /// fixture inspection. Large OSV snapshots remain package-keyed on disk.
    pub fn iter(&self) -> impl Iterator<Item = &Advisory> {
        self.journals.values().flat_map(AdvisoryJournal::iter)
    }

    fn rebuild_aliases(&mut self) -> Result<(), AliasGraphError> {
        // Keep identity edges for withdrawn/snapshot-omitted objects.  They are part of the
        // durable tombstone history: dropping them on a cold start would let a later feed reuse
        // an old alias as an unrelated root.  Older state files may not have the field at all,
        // so the `serde(default)` graph is augmented from the active journals below.
        let mut aliases = self.aliases.clone();
        for advisory in self.iter() {
            aliases.admit_identity(advisory)?;
        }
        self.aliases = aliases;
        Ok(())
    }
}

/// Resolves aliases from selected disk-backed advisories against the resident
/// cross-source graph and one bounded query-local graph. This preserves the
/// normal canonical IDs without cloning the potentially large durable graph
/// for every package lookup.
fn reconcile_snapshot_aliases(
    existing: &AliasGraph,
    advisories: Vec<Advisory>,
) -> (Vec<Advisory>, bool) {
    let mut query_aliases = AliasGraph::default();
    let mut output = Vec::with_capacity(advisories.len());
    let mut conflict = false;
    for mut advisory in advisories {
        let native = advisory.key.native.id.clone();
        let mut roots = BTreeSet::new();
        for alias in advisory
            .aliases
            .iter()
            .map(|alias| alias.value.as_str())
            .chain(std::iter::once(native.as_str()))
        {
            if let Some(root) = existing.resolve(alias) {
                roots.insert(root.0);
            }
        }
        if roots.len() > 1 {
            conflict = true;
            advisory.key.canonical = super::CanonicalAdvisoryId(native);
            output.push(advisory);
            continue;
        }
        if let Some(root) = roots.first() {
            let mut aliases = advisory.aliases.into_vec();
            aliases.push(Alias {
                value: root.clone(),
                source: AdvisorySource::Osv,
            });
            aliases.sort();
            aliases.dedup();
            advisory.aliases = aliases.into_boxed_slice();
        }
        match query_aliases.admit_identity(&advisory) {
            Ok(canonical) => advisory.key.canonical = canonical,
            Err(_) => {
                conflict = true;
                advisory.key.canonical = super::CanonicalAdvisoryId(native);
            }
        }
        output.push(advisory);
    }
    (output, conflict)
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
    /// Immutable OSV generation validation or retention failed.
    Snapshot(OsvSnapshotError),
    /// Persisted state was not valid JSON.
    Decode(serde_json::Error),
    /// State could not be encoded.
    Encode(serde_json::Error),
    /// Target path has no parent directory.
    NoParent,
    /// Persisted source objects disagree about a shared alias.
    AliasConflict(AliasGraphError),
    /// Persisted authority state exceeded the configured byte ceiling.
    BoundExceeded,
    /// Authority state path is not a regular non-symlink file.
    InvalidStateFile,
}
impl std::fmt::Display for AuthorityStorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "advisory authority storage failed: {self:?}")
    }
}
impl std::error::Error for AuthorityStorageError {}

struct BoundedStateWriter<'a> {
    file: &'a mut fs::File,
    written: u64,
    maximum: u64,
    exceeded: bool,
    write_error: Option<std::io::ErrorKind>,
}

impl Write for BoundedStateWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let length = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        if self
            .written
            .checked_add(length)
            .is_none_or(|next| next > self.maximum)
        {
            self.exceeded = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "advisory authority state exceeds its byte limit",
            ));
        }
        match self.file.write(buffer) {
            Ok(written) => {
                self.written = self
                    .written
                    .saturating_add(u64::try_from(written).unwrap_or(u64::MAX));
                Ok(written)
            }
            Err(error) => {
                self.write_error = Some(error.kind());
                Err(error)
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.file.flush() {
            Ok(()) => Ok(()),
            Err(error) => {
                self.write_error = Some(error.kind());
                Err(error)
            }
        }
    }
}

/// Authority state could not admit one complete source transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorityApplyError {
    /// The supplied source body was not fetched from the currently selected
    /// endpoint and scope.
    ConfiguredSourceMismatch(AdvisorySource),
    /// A conditional validator was accepted before a body existed.
    NotModifiedWithoutFrontier(AdvisorySource),
    /// A conditional response cannot reuse a frontier from another configured
    /// endpoint or source scope.
    NotModifiedSourceMismatch(AdvisorySource),
    /// A feed was labeled as one source but carried a native identity from another.
    SourceMismatch {
        /// Source selected by the transport/configuration.
        expected: AdvisorySource,
        /// Source carried by the parsed object.
        actual: AdvisorySource,
    },
    /// An alias connected two incompatible source claims.
    AliasConflict(AliasGraphError),
    /// The underlying copy-on-write journal rejected a semantic conflict.
    Journal(AdvisoryJournalError),
    /// Internal map insertion invariant failed.
    Invariant,
    /// A disk-backed snapshot was attached to a different source or carried
    /// unbounded in-memory entries at the same time.
    UnexpectedSnapshot,
    /// A disk-backed snapshot was not fully verified before selection.
    SnapshotUnavailable,
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

fn osv_snapshot_root(authority_path: &Path) -> PathBuf {
    let parent = authority_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    parent.join("advisory-snapshots")
}

/// Reads a bounded local authority body. Directory walking is intentionally
/// left to the local-service composition root, where its I/O policy belongs.
pub fn read_feed(path: impl AsRef<Path>, maximum: usize) -> Result<Vec<u8>, AuthorityStorageError> {
    let metadata = fs::metadata(path.as_ref()).map_err(AuthorityStorageError::Io)?;
    let maximum = u64::try_from(maximum).unwrap_or(u64::MAX);
    if metadata.len() > maximum {
        return Err(AuthorityStorageError::Io(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            "advisory source exceeds configured bound",
        )));
    }
    // Metadata can change after the stat.  Read one byte beyond the bound so a concurrent
    // writer cannot turn an oversized source into an unbounded allocation.
    let file = fs::File::open(path).map_err(AuthorityStorageError::Io)?;
    let mut bytes = Vec::new();
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(AuthorityStorageError::Io)?;
    if bytes.len() as u64 > maximum {
        return Err(AuthorityStorageError::Io(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            "advisory source exceeds configured bound",
        )));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CanonicalAdvisoryId;

    fn osv() -> Vec<u8> {
        br#"{"schema_version":"1.3.1","id":"OSV-AUTH-1","modified":"2026-01-02T00:00:00Z","affected":[{"package":{"ecosystem":"Cargo","name":"demo"},"ranges":[{"type":"SEMVER","events":[{"introduced":"0"},{"fixed":"2.0.0"}]}]}]}"#.to_vec()
    }

    fn osv_for(id: &str, ecosystem: &str) -> Vec<u8> {
        format!(
            r#"{{"schema_version":"1.3.1","id":"{id}","modified":"2026-01-02T00:00:00Z","affected":[{{"package":{{"ecosystem":"{ecosystem}","name":"demo"}},"ranges":[{{"type":"SEMVER","events":[{{"introduced":"0"}},{{"fixed":"2.0.0"}}]}}]}}]}}"#
        )
        .into_bytes()
    }

    #[test]
    fn source_batches_are_durable_and_exact_versioned() {
        let mut feed =
            AuthorityFeed::parse(AdvisorySource::Osv, &osv(), 10, Some("a".into()), None)
                .expect("OSV fixture");
        feed.osv_scope = Some(OsvFeedScope::All);
        let mut authority = AdvisoryAuthority::new(100);
        authority.configure_osv_scope(Some(OsvFeedScope::All));
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
    fn streamed_snapshot_matches_reuse_the_existing_alias_engine() {
        let mut left =
            super::super::parse_osv(&osv_for("OSV-LEFT", "Cargo"), 10).expect("left OSV object");
        let mut right =
            super::super::parse_osv(&osv_for("OSV-RIGHT", "Cargo"), 10).expect("right OSV object");
        left.aliases = Box::new([Alias {
            value: "CVE-2026-1234".into(),
            source: AdvisorySource::Osv,
        }]);
        right.aliases = Box::new([Alias {
            value: "CVE-2026-1234".into(),
            source: AdvisorySource::Osv,
        }]);
        let (joined, conflict) =
            reconcile_snapshot_aliases(&AliasGraph::default(), vec![left, right]);
        assert!(!conflict);
        assert_eq!(joined.len(), 2);
        assert_eq!(joined[0].key.canonical, joined[1].key.canonical);
        assert_eq!(joined[0].key.canonical.0, "OSV-LEFT");
    }

    #[test]
    fn incomplete_osv_refresh_overlays_disk_snapshot_positive_facts() {
        let directory = tempfile::tempdir().expect("snapshot root");
        let scope = OsvFeedScope::Ecosystem(OsvEcosystem::Cargo);
        let mut old = super::super::parse_osv(&osv(), 10).expect("old OSV object");
        old.summary = Some("older selected advisory".into());
        let mut builder = super::super::OsvSnapshotBuilder::create(
            directory.path(),
            scope,
            10,
            10,
            10,
            1024 * 1024,
        )
        .expect("snapshot builder");
        builder.push(&old).expect("stage old advisory");
        let snapshot = builder
            .finish(*blake3::hash(b"complete OSV export").as_bytes())
            .expect("seal old snapshot");
        let mut authority = AdvisoryAuthority::new(100);
        authority.configure_sources([AdvisorySource::Osv]);
        authority.configure_osv_scope(Some(scope));
        authority
            .apply(AuthorityFeed::from_osv_snapshot(snapshot, 10, None, None))
            .expect("admit complete snapshot");

        let mut refreshed = super::super::parse_osv(&osv(), 11).expect("partial OSV object");
        refreshed.summary = Some("new partial positive advisory".into());
        let mut partial =
            AuthorityFeed::from_entries(AdvisorySource::Osv, vec![refreshed], 11, None, None);
        partial.complete = false;
        partial.osv_scope = Some(scope);
        authority.apply(partial).expect("admit partial page");

        let package = super::super::normalize_package("cargo", "demo").expect("package");
        let observation = authority.observe(&package, "1.0.0", false, false, 11, false);
        assert_eq!(observation.coverage, AdvisoryCoverage::Partial);
        assert_eq!(observation.advisories.len(), 1);
        assert_eq!(
            observation.advisories[0].summary.as_deref(),
            Some("new partial positive advisory")
        );
    }

    #[test]
    fn snapshot_retention_keeps_selected_previous_and_live_readers_only() {
        fn stage(root: &Path, scope: OsvFeedScope, id: &str) -> (AuthorityFeed, String) {
            let advisory = super::super::parse_osv(&osv_for(id, "Cargo"), 10).expect("OSV object");
            let mut builder =
                super::super::OsvSnapshotBuilder::create(root, scope, 10, 10, 10, 1024 * 1024)
                    .expect("snapshot builder");
            builder.push(&advisory).expect("stage advisory");
            let snapshot = builder
                .finish(*blake3::hash(id.as_bytes()).as_bytes())
                .expect("seal snapshot");
            let generation = snapshot.generation_id().to_owned();
            (
                AuthorityFeed::from_osv_snapshot(snapshot, 10, None, None),
                generation,
            )
        }

        let directory = tempfile::tempdir().expect("authority directory");
        let authority_path = directory.path().join("authority.json");
        let snapshot_root = AdvisoryAuthority::osv_snapshot_root(&authority_path);
        let scope = OsvFeedScope::Ecosystem(OsvEcosystem::Cargo);
        let mut authority = AdvisoryAuthority::new(100);
        authority.configure_sources([AdvisorySource::Osv]);
        authority.configure_osv_scope(Some(scope));

        let (first_feed, first) = stage(&snapshot_root, scope, "OSV-RETENTION-1");
        authority.apply(first_feed).expect("select first snapshot");
        authority
            .persist(&authority_path)
            .expect("persist first selection");

        let (second_feed, second) = stage(&snapshot_root, scope, "OSV-RETENTION-2");
        authority
            .apply(second_feed)
            .expect("select second snapshot");
        authority
            .persist(&authority_path)
            .expect("persist second selection");
        assert_eq!(
            authority
                .osv_previous_snapshot
                .as_ref()
                .map(OsvSnapshotRef::generation_id),
            Some(first.as_str())
        );
        let reopened = AdvisoryAuthority::open(&authority_path, 100)
            .expect("reopen current and previous generations");
        assert_eq!(
            reopened
                .osv_previous_snapshot
                .as_ref()
                .map(OsvSnapshotRef::generation_id),
            Some(first.as_str())
        );
        drop(reopened);

        let live_reader = authority.osv_previous_snapshot.clone();
        let (third_feed, third) = stage(&snapshot_root, scope, "OSV-RETENTION-3");
        authority.apply(third_feed).expect("select third snapshot");
        authority
            .persist(&authority_path)
            .expect("persist third selection");
        assert!(snapshot_root.join(&first).is_dir());
        assert!(snapshot_root.join(&second).is_dir());
        assert!(snapshot_root.join(&third).is_dir());

        drop(live_reader);
        authority
            .persist(&authority_path)
            .expect("collect after live reader closes");
        assert!(!snapshot_root.join(first).exists());
        assert!(snapshot_root.join(second).is_dir());
        assert!(snapshot_root.join(third).is_dir());
    }

    #[test]
    fn active_snapshot_builder_holds_owner_lease_against_collection() {
        let directory = tempfile::tempdir().expect("snapshot root");
        let builder = super::super::OsvSnapshotBuilder::create(
            directory.path(),
            OsvFeedScope::Ecosystem(OsvEcosystem::Cargo),
            10,
            10,
            10,
            1024 * 1024,
        )
        .expect("snapshot builder");
        assert_eq!(
            OsvSnapshotRef::prune_unreferenced_generations(directory.path(), &BTreeSet::new())
                .expect("bounded collection attempt"),
            false
        );
        assert!(matches!(
            super::super::OsvSnapshotBuilder::create(
                directory.path(),
                OsvFeedScope::Ecosystem(OsvEcosystem::Cargo),
                10,
                10,
                10,
                1024 * 1024,
            ),
            Err(OsvSnapshotError::Busy)
        ));
        drop(builder);
    }

    #[test]
    fn attached_osv_index_mutation_cannot_change_a_lookup_to_clean() {
        let directory = tempfile::tempdir().expect("snapshot root");
        let scope = OsvFeedScope::Ecosystem(OsvEcosystem::Cargo);
        let advisory = super::super::parse_osv(&osv(), 10).expect("OSV object");
        let mut builder = super::super::OsvSnapshotBuilder::create(
            directory.path(),
            scope,
            10,
            10,
            10,
            1024 * 1024,
        )
        .expect("snapshot builder");
        builder.push(&advisory).expect("stage advisory");
        let snapshot = builder
            .finish(*blake3::hash(b"index mutation regression").as_bytes())
            .expect("seal snapshot");
        let generation = snapshot.generation_id().to_owned();
        let mut authority = AdvisoryAuthority::new(100);
        authority.configure_sources([AdvisorySource::Osv]);
        authority.configure_osv_scope(Some(scope));
        authority
            .apply(AuthorityFeed::from_osv_snapshot(snapshot, 10, None, None))
            .expect("select snapshot");

        let index_path = directory.path().join(generation).join("package-index.bin");
        let mut bytes = fs::read(&index_path).expect("read package index");
        bytes[0] ^= 1;
        fs::write(index_path, bytes).expect("mutate on-disk package index");

        let package = super::super::normalize_package("cargo", "demo").expect("identity");
        let observation = authority.observe(&package, "1.0.0", false, false, 10, false);
        assert_eq!(observation.coverage, AdvisoryCoverage::Complete);
        assert_eq!(observation.advisories.len(), 1);
    }

    #[test]
    fn selected_osv_ecosystem_never_claims_global_clean_coverage() {
        let mut authority = AdvisoryAuthority::new(100);
        authority.configure_sources([AdvisorySource::Osv]);
        authority.configure_osv_scope(Some(OsvFeedScope::Ecosystem(OsvEcosystem::Cargo)));
        let mut cargo_feed = AuthorityFeed::parse(
            AdvisorySource::Osv,
            &osv_for("OSV-CARGO-1", "Cargo"),
            10,
            None,
            None,
        )
        .expect("Cargo OSV feed");
        cargo_feed.osv_scope = Some(OsvFeedScope::Ecosystem(OsvEcosystem::Cargo));
        authority.apply(cargo_feed).expect("admit Cargo OSV feed");

        let cargo = super::super::normalize_package("cargo", "demo").expect("Cargo identity");
        let cargo_observation = authority.observe(&cargo, "1.0.0", false, false, 10, false);
        assert_eq!(cargo_observation.coverage, AdvisoryCoverage::Complete);
        assert_eq!(cargo_observation.advisories.len(), 1);

        let pypi = super::super::normalize_package("pypi", "demo").expect("PyPI identity");
        let outside_scope = authority.observe(&pypi, "1.0.0", false, false, 10, false);
        assert_eq!(outside_scope.coverage, AdvisoryCoverage::Partial);
        assert!(outside_scope.advisories.is_empty());

        authority.configure_osv_scope(Some(OsvFeedScope::Ecosystem(OsvEcosystem::Pypi)));
        let old_frontier = authority.observe(&cargo, "1.0.0", false, false, 10, false);
        assert_eq!(old_frontier.coverage, AdvisoryCoverage::Partial);
        assert!(old_frontier.advisories.is_empty());
        let not_yet_refreshed = authority.observe(&pypi, "1.0.0", false, false, 10, false);
        assert_eq!(not_yet_refreshed.coverage, AdvisoryCoverage::Partial);
        assert!(not_yet_refreshed.advisories.is_empty());

        let mut pypi_feed = AuthorityFeed::parse(
            AdvisorySource::Osv,
            &osv_for("OSV-PYPI-1", "PyPI"),
            11,
            None,
            None,
        )
        .expect("PyPI OSV feed");
        pypi_feed.osv_scope = Some(OsvFeedScope::Ecosystem(OsvEcosystem::Pypi));
        authority.apply(pypi_feed).expect("admit PyPI OSV feed");
        let pypi_observation = authority.observe(&pypi, "1.0.0", false, false, 11, false);
        assert_eq!(pypi_observation.coverage, AdvisoryCoverage::Complete);
        assert_eq!(pypi_observation.advisories.len(), 1);
    }

    #[test]
    fn osv_feed_scope_must_match_the_selected_source_policy() {
        let mut feed = AuthorityFeed::from_entries(AdvisorySource::Osv, Vec::new(), 10, None, None);
        feed.osv_scope = Some(OsvFeedScope::Ecosystem(OsvEcosystem::Cargo));
        let mut authority = AdvisoryAuthority::new(100);
        authority.configure_sources([AdvisorySource::Osv]);
        authority.configure_osv_scope(Some(OsvFeedScope::Ecosystem(OsvEcosystem::Pypi)));
        let frontier = authority.apply(feed).expect("record mismatched feed");
        assert!(!frontier.complete);
        let pypi = super::super::normalize_package("pypi", "demo").expect("PyPI identity");
        assert_eq!(
            authority
                .observe(&pypi, "1.0.0", false, false, 10, false)
                .coverage,
            AdvisoryCoverage::Partial
        );
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
                    expires_at: None,
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
    fn conditional_response_cannot_reuse_another_endpoint_frontier() {
        let mut authority = AdvisoryAuthority::new(100);
        let mut body =
            AuthorityFeed::parse(AdvisorySource::Osv, &osv(), 10, Some("a".into()), None)
                .expect("OSV body");
        body.source_identity = Some([1; 32]);
        body.osv_scope = Some(OsvFeedScope::All);
        authority.apply(body).expect("admit endpoint A body");
        authority.configure_sources([AdvisorySource::Osv]);
        authority.configure_osv_scope(Some(OsvFeedScope::All));
        authority.configure_source_identities([(AdvisorySource::Osv, [2; 32])]);
        let mut stale_body =
            AuthorityFeed::parse(AdvisorySource::Osv, &osv(), 11, Some("b".into()), None)
                .expect("other endpoint body");
        stale_body.osv_scope = Some(OsvFeedScope::All);
        stale_body.source_identity = Some([1; 32]);
        assert_eq!(
            authority
                .apply(stale_body)
                .expect_err("reject stale source body"),
            AuthorityApplyError::ConfiguredSourceMismatch(AdvisorySource::Osv)
        );
        let package = super::super::normalize_package("cargo", "demo").expect("package");
        let endpoint_b = authority.observe(&package, "1.0.0", false, false, 10, false);
        assert_eq!(endpoint_b.coverage, AdvisoryCoverage::Partial);
        assert!(
            endpoint_b.advisories.is_empty(),
            "endpoint A facts must not be attributed to selected endpoint B"
        );

        let mut endpoint_b =
            AuthorityFeed::not_modified(AdvisorySource::Osv, 11, Some("a".into()), None);
        endpoint_b.source_identity = Some([2; 32]);
        assert_eq!(
            authority
                .apply(endpoint_b)
                .expect_err("reject cross-endpoint 304"),
            AuthorityApplyError::NotModifiedSourceMismatch(AdvisorySource::Osv)
        );

        let mut endpoint_a =
            AuthorityFeed::not_modified(AdvisorySource::Osv, 12, Some("a".into()), None);
        endpoint_a.source_identity = Some([1; 32]);
        authority.configure_source_identities([(AdvisorySource::Osv, [1; 32])]);
        authority
            .apply(endpoint_a)
            .expect("matching endpoint can reuse its cached body");
        authority.configure_source_identities([(AdvisorySource::Osv, [1; 32])]);
        assert_eq!(
            authority
                .frontier(AdvisorySource::Osv)
                .expect("frontier")
                .source_identity,
            Some([1; 32])
        );
    }

    #[test]
    fn partial_osv_refresh_from_new_endpoint_cannot_relabel_old_snapshot() {
        let directory = tempfile::tempdir().expect("snapshot root");
        let scope_a = OsvFeedScope::All;
        let scope_b = OsvFeedScope::Ecosystem(OsvEcosystem::Cargo);
        let old = super::super::parse_osv(&osv(), 10).expect("old OSV object");
        let mut builder = super::super::OsvSnapshotBuilder::create(
            directory.path(),
            scope_a,
            10,
            10,
            10,
            1024 * 1024,
        )
        .expect("snapshot builder");
        builder.push(&old).expect("stage old advisory");
        let snapshot = builder
            .finish(*blake3::hash(b"endpoint A complete export").as_bytes())
            .expect("seal old snapshot");

        let mut authority = AdvisoryAuthority::new(100);
        authority.configure_sources([AdvisorySource::Osv]);
        authority.configure_osv_scope(Some(scope_a));
        authority.configure_source_identities([(AdvisorySource::Osv, [1; 32])]);
        let mut feed = AuthorityFeed::from_osv_snapshot(snapshot, 10, None, None);
        feed.source_identity = Some([1; 32]);
        authority.apply(feed).expect("admit endpoint A snapshot");

        authority.configure_osv_scope(Some(scope_b));
        authority.configure_source_identities([(AdvisorySource::Osv, [2; 32])]);
        let mut partial = AuthorityFeed::parse(AdvisorySource::Osv, b"[]", 11, None, None)
            .expect("endpoint B partial page");
        partial.complete = false;
        partial.osv_scope = Some(scope_b);
        partial.source_identity = Some([2; 32]);
        authority
            .apply(partial)
            .expect("admit endpoint B partial page");

        let package = super::super::normalize_package("cargo", "demo").expect("package");
        let observation = authority.observe(&package, "1.0.0", false, false, 11, false);
        assert_eq!(observation.coverage, AdvisoryCoverage::Partial);
        assert!(
            observation.advisories.is_empty(),
            "endpoint A rows must not be presented as endpoint B facts"
        );
        assert!(authority.osv_snapshot.is_none());
        assert!(authority.osv_previous_snapshot.is_some());
    }

    #[test]
    fn first_refresh_outage_is_unavailable_not_unknown() {
        let mut authority = AdvisoryAuthority::new(100);
        authority.mark_unavailable(AdvisorySource::Osv, 1);
        let package = super::super::normalize_package("cargo", "demo").expect("identity");
        let observation = authority.observe(&package, "1.0.0", false, false, 1, false);
        assert_eq!(observation.coverage, AdvisoryCoverage::Unavailable);
        assert_eq!(observation.freshness, FreshnessState::Fresh);
    }

    #[test]
    fn authority_state_load_and_store_respect_the_configured_byte_ceiling() {
        let directory = tempfile::tempdir().expect("authority directory");
        let path = directory.path().join("authority.json");
        let authority =
            AdvisoryAuthority::open_with_limit(&path, 10, 3).expect("missing authority is empty");
        assert!(matches!(
            authority.persist(&path),
            Err(AuthorityStorageError::BoundExceeded)
        ));
        assert!(!path.exists(), "oversized state was not published");

        fs::write(&path, b"1234").expect("write oversized authority");
        assert!(matches!(
            AdvisoryAuthority::open_with_limit(&path, 10, 3),
            Err(AuthorityStorageError::BoundExceeded)
        ));

        fs::write(&path, b"").expect("write empty corrupt authority");
        assert!(matches!(
            AdvisoryAuthority::open_with_limit(&path, 10, 3),
            Err(AuthorityStorageError::Decode(_))
        ));
    }

    #[test]
    fn missing_authority_prunes_complete_generation_left_before_selection() {
        let directory = tempfile::tempdir().expect("authority directory");
        let authority_path = directory.path().join("authority.json");
        let snapshot_root = AdvisoryAuthority::osv_snapshot_root(&authority_path);
        let scope = OsvFeedScope::Ecosystem(OsvEcosystem::Cargo);
        let advisory = super::super::parse_osv(&osv(), 10).expect("OSV object");
        let mut builder = super::super::OsvSnapshotBuilder::create(
            &snapshot_root,
            scope,
            10,
            10,
            10,
            1024 * 1024,
        )
        .expect("snapshot builder");
        builder.push(&advisory).expect("stage advisory");
        let snapshot = builder
            .finish(*blake3::hash(b"finished before authority selection").as_bytes())
            .expect("seal generation");
        let orphan = snapshot_root.join(snapshot.generation_id());
        drop(snapshot);
        assert!(
            orphan.is_dir(),
            "complete generation exists before recovery"
        );

        AdvisoryAuthority::open(&authority_path, 100).expect("recover empty authority");
        assert!(
            !orphan.exists(),
            "unselected complete generation is reclaimed before the next quota calculation"
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

    #[test]
    fn source_expiry_is_honored_and_capped_by_configured_max_age() {
        let mut source_limited =
            AuthorityFeed::parse(AdvisorySource::Osv, &osv(), 10, None, None).expect("OSV fixture");
        source_limited.freshness.expires_at = Some(20);
        let mut authority = AdvisoryAuthority::new(100);
        authority.apply(source_limited).expect("admit source");
        let package = super::super::normalize_package("cargo", "demo").expect("identity");
        assert_eq!(
            authority
                .observe(&package, "1.0.0", false, false, 19, false)
                .freshness,
            FreshnessState::Fresh
        );
        authority.mark_unavailable(AdvisorySource::Osv, 19);
        assert_eq!(
            authority
                .observe(&package, "1.0.0", false, false, 20, false)
                .coverage,
            AdvisoryCoverage::Unavailable
        );
        assert_eq!(
            authority
                .observe(&package, "1.0.0", false, false, 20, false)
                .freshness,
            FreshnessState::Stale
        );

        let mut policy_limited = AuthorityFeed::parse(AdvisorySource::Osv, &osv(), 30, None, None)
            .expect("second OSV fixture");
        policy_limited.freshness.expires_at = Some(300);
        authority.set_max_age_secs(5);
        authority.apply(policy_limited).expect("refresh source");
        assert_eq!(
            authority
                .observe(&package, "1.0.0", false, false, 34, false)
                .freshness,
            FreshnessState::Fresh
        );
        assert_eq!(
            authority
                .observe(&package, "1.0.0", false, false, 35, false)
                .freshness,
            FreshnessState::Stale
        );
    }

    #[test]
    fn cold_persist_keeps_tombstones_and_alias_identity() {
        let path = std::env::temp_dir().join(format!(
            "nudox-advisory-authority-{}-{}.json",
            std::process::id(),
            1_u64
        ));
        let mut authority = AdvisoryAuthority::new(100);
        authority.configure_osv_scope(Some(OsvFeedScope::All));
        let mut initial =
            AuthorityFeed::parse(AdvisorySource::Osv, &osv(), 10, Some("a".into()), None)
                .expect("OSV fixture");
        initial.osv_scope = Some(OsvFeedScope::All);
        authority.apply(initial).expect("admit source");
        let mut empty = AuthorityFeed::from_entries(
            AdvisorySource::Osv,
            Vec::new(),
            11,
            Some("b".into()),
            None,
        );
        empty.osv_scope = Some(OsvFeedScope::All);
        authority.apply(empty).expect("complete empty snapshot");
        authority.persist(&path).expect("persist");

        let restored = AdvisoryAuthority::open(&path, 7).expect("cold open");
        assert_eq!(restored.max_age_secs, 7);
        assert_eq!(
            restored.aliases.resolve("OSV-AUTH-1"),
            Some(CanonicalAdvisoryId("OSV-AUTH-1".to_owned()))
        );
        let key = CanonicalAdvisoryId("OSV-AUTH-1".to_owned());
        assert_eq!(
            restored
                .journals
                .get(&AdvisorySource::Osv)
                .and_then(|journal| journal.tombstone_reason(&key)),
            Some("snapshot-omitted")
        );
        let _ = fs::remove_file(path);
    }
}
