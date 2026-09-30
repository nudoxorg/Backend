//! Immutable, package-keyed storage for large OSV snapshots.
//!
//! The archive parser streams one advisory at a time into a private staging
//! directory. A complete generation is published only after its package files
//! and binary lookup index have been flushed and checksummed. Runtime lookup
//! reads one package's bounded posting file instead of reconstructing the
//! global advisory map in memory.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use backend_platform::directory::DirectoryCapability;

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};

use crate::{
    AcquisitionGate, Advisory, AdvisoryCategory, AdvisorySchema, AdvisorySource, AffectedRange,
    Alias, Evidence, MalwareCoverage, OsvEcosystem, OsvFeedScope, PackageIdentity, Reference,
    Severity,
};

const SNAPSHOT_SCHEMA: u16 = 1;
const PACKAGE_INDEX_RECORD_BYTES: u64 = 80;
const MAX_OPEN_PACKAGE_FILES: usize = 32;
const MAX_OSV_SNAPSHOT_ROW_BYTES: usize = 16 * 1024 * 1024;
const MAX_OSV_QUERY_RECORDS: u64 = 100_000;
const MAX_OSV_QUERY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_OSV_STORAGE_ENTRIES: usize = 4_000_000;
const OSV_ROOT_LOCK: &str = ".osv-root.lock";
const OSV_LEASE_DIRECTORY: &str = "leases";
const OSV_GENERATION_LOCK: &str = "generation.lock";

/// Upper bound on source objects retained by one staged snapshot.
pub const MAX_OSV_SNAPSHOT_OBJECTS: u64 = 1_000_000;
/// Upper bound on normalized package identities in one staged snapshot.
pub const MAX_OSV_SNAPSHOT_PACKAGES: u64 = 500_000;
/// Upper bound on package/advisory postings written by one staged snapshot.
pub const MAX_OSV_SNAPSHOT_PACKAGE_ROWS: u64 = 2_000_000;
/// Hard ceiling for all staged, selected, and historical OSV generation bytes
/// under one local snapshot root, including indexes and manifests.
pub const MAX_OSV_SNAPSHOT_STORAGE_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// Quota category reached while staging or reading a large source snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OsvSnapshotLimit {
    /// Source archive contained too many JSON advisory objects.
    Objects,
    /// Source archive affected too many distinct package identities.
    Packages,
    /// Source archive produced too many package/advisory postings.
    PackageRows,
    /// Derived local snapshot exceeded its configured byte budget.
    StoredBytes,
    /// One package lookup exceeded its bounded output budget.
    QueryBytes,
    /// One package lookup contained too many advisory rows.
    QueryRows,
}

/// Safe local storage or source-quota failure for an OSV snapshot.
#[derive(Debug)]
pub enum OsvSnapshotError {
    /// Local filesystem operation failed.
    Io(io::Error),
    /// One advisory object could not be encoded or decoded.
    Encoding(serde_json::Error),
    /// A source object violated the admitted identity or snapshot format.
    Invalid(&'static str),
    /// A bounded source or query reached an explicit quota.
    Limit(OsvSnapshotLimit),
    /// Another process currently owns staging/retention for this root.
    Busy,
}

impl std::fmt::Display for OsvSnapshotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(_) => formatter.write_str("OSV snapshot local storage failed"),
            Self::Encoding(_) => formatter.write_str("OSV snapshot record encoding failed"),
            Self::Invalid(reason) => write!(formatter, "OSV snapshot is invalid: {reason}"),
            Self::Limit(limit) => write!(formatter, "OSV snapshot limit reached: {limit:?}"),
            Self::Busy => formatter.write_str("OSV snapshot root is busy"),
        }
    }
}

impl std::error::Error for OsvSnapshotError {}

impl From<io::Error> for OsvSnapshotError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for OsvSnapshotError {
    fn from(error: serde_json::Error) -> Self {
        Self::Encoding(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct OsvSnapshotManifest {
    schema: u16,
    generation: String,
    scope: OsvFeedScope,
    source_digest: [u8; 32],
    package_index_digest: [u8; 32],
    advisory_objects: u64,
    package_rows: u64,
    package_count: u64,
    stored_bytes: u64,
}

/// Immutable selected OSV generation retained by an advisory authority.
///
/// `directory` and `index_verified` are process-local and never serialized.
/// A cold opener reconstructs and verifies them from the advisory journal's
/// parent directory and the immutable generation identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LeaseMode {
    Shared,
    Exclusive,
    Released,
}

#[derive(Debug)]
struct LeaseState {
    file: File,
    mode: LeaseMode,
}

/// Kernel-managed lease on either the snapshot owner lock or one generation.
/// Clones share the same handle so dropping any one reader cannot release a
/// lock another reader is relying on.
#[derive(Debug)]
struct FileLease(Mutex<LeaseState>);

impl FileLease {
    fn open(
        directory: &DirectoryCapability,
        name: &str,
        create: bool,
        mode: LeaseMode,
        wait: bool,
    ) -> Result<Option<Self>, OsvSnapshotError> {
        let file = directory.open_private_file_read_write(name, create)?;
        let locked = match (mode, wait) {
            (LeaseMode::Shared, true) => {
                FileExt::lock_shared(&file)?;
                true
            }
            (LeaseMode::Exclusive, true) => {
                FileExt::lock_exclusive(&file)?;
                true
            }
            (LeaseMode::Shared, false) => FileExt::try_lock_shared(&file)?,
            (LeaseMode::Exclusive, false) => FileExt::try_lock_exclusive(&file)?,
            (LeaseMode::Released, _) => {
                return Err(OsvSnapshotError::Invalid("released lease acquisition"));
            }
        };
        if !locked {
            return Ok(None);
        }
        Ok(Some(Self(Mutex::new(LeaseState { file, mode }))))
    }

    fn downgrade_to_shared(&self) -> Result<(), OsvSnapshotError> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| OsvSnapshotError::Invalid("snapshot lease mutex"))?;
        if state.mode == LeaseMode::Exclusive {
            // The root owner lease prevents collection while this lock changes
            // modes, so the short unlock/relock transition is race-free.
            FileExt::unlock(&state.file)?;
            FileExt::lock_shared(&state.file)?;
            state.mode = LeaseMode::Shared;
        }
        Ok(())
    }

    fn release(&self) -> Result<(), OsvSnapshotError> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| OsvSnapshotError::Invalid("snapshot lease mutex"))?;
        if state.mode != LeaseMode::Released {
            FileExt::unlock(&state.file)?;
            state.mode = LeaseMode::Released;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OsvSnapshotRef {
    generation: String,
    scope: OsvFeedScope,
    source_digest: [u8; 32],
    package_index_digest: [u8; 32],
    advisory_objects: u64,
    package_rows: u64,
    package_count: u64,
    stored_bytes: u64,
    #[serde(skip)]
    directory: PathBuf,
    #[serde(skip)]
    root_directory: Option<DirectoryCapability>,
    #[serde(skip)]
    generation_directory: Option<DirectoryCapability>,
    #[serde(skip)]
    index_verified: bool,
    /// Shared lease held by live readers, or exclusive until selection is
    /// durably committed.
    #[serde(skip)]
    lease: Option<Arc<FileLease>>,
    /// Serializes staging against quota accounting and generation collection
    /// until the authority journal has durably selected this generation.
    #[serde(skip)]
    pending_root_lease: Option<Arc<FileLease>>,
    /// Verified, sorted records retained in memory so a later on-disk index
    /// mutation cannot turn a lookup into false clean coverage.
    #[serde(skip)]
    index_records: Option<Arc<Vec<[u8; PACKAGE_INDEX_RECORD_BYTES as usize]>>>,
}

impl PartialEq for OsvSnapshotRef {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self.scope == other.scope
            && self.source_digest == other.source_digest
            && self.package_index_digest == other.package_index_digest
            && self.advisory_objects == other.advisory_objects
            && self.package_rows == other.package_rows
            && self.package_count == other.package_count
            && self.stored_bytes == other.stored_bytes
    }
}

impl Eq for OsvSnapshotRef {}

impl OsvSnapshotRef {
    /// Selected source scope represented by this immutable generation.
    #[must_use]
    pub const fn scope(&self) -> OsvFeedScope {
        self.scope
    }

    /// Compressed source-body digest retained as provenance.
    #[must_use]
    pub const fn source_digest(&self) -> [u8; 32] {
        self.source_digest
    }

    /// Number of distinct parsed advisory objects represented by this snapshot.
    #[must_use]
    pub const fn advisory_objects(&self) -> u64 {
        self.advisory_objects
    }

    /// Number of package-specific advisory rows in the selected snapshot.
    #[must_use]
    pub const fn package_rows(&self) -> u64 {
        self.package_rows
    }

    pub(crate) const fn is_verified(&self) -> bool {
        self.index_verified
    }

    pub(crate) fn frontier_digest(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.osv.frontier.v1\0");
        hasher.update(&self.source_digest);
        hasher.update(&self.package_index_digest);
        hasher.update(&[as_u8(self.scope)]);
        *hasher.finalize().as_bytes()
    }

    /// Verifies the manifest and complete package index, then binds this
    /// reference to the local immutable generation directory.
    pub fn attach_root(&mut self, root: impl AsRef<Path>) -> Result<(), OsvSnapshotError> {
        let root = root.as_ref();
        if self.generation.len() != 64
            || !self.generation.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(OsvSnapshotError::Invalid("generation identity"));
        }
        let root_directory = DirectoryCapability::open(root)?;
        root_directory.validate_private()?;
        let root_lease = FileLease::open(
            &root_directory,
            OSV_ROOT_LOCK,
            true,
            LeaseMode::Shared,
            false,
        )?
        .ok_or(OsvSnapshotError::Busy)?;
        let generation_directory = root_directory.open_private_dir(&self.generation)?;
        let leases_directory = generation_directory.open_private_dir(OSV_LEASE_DIRECTORY)?;
        match leases_directory.create_file_exclusive(OSV_GENERATION_LOCK) {
            Ok(file) => file.sync_all()?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(OsvSnapshotError::Io(error)),
        }
        let lease = Arc::new(
            FileLease::open(
                &leases_directory,
                OSV_GENERATION_LOCK,
                false,
                LeaseMode::Shared,
                true,
            )?
            .ok_or(OsvSnapshotError::Invalid("generation lease unavailable"))?,
        );
        let attached =
            self.attach_root_with_lease(root, root_directory, generation_directory, lease);
        drop(root_lease);
        attached
    }

    fn attach_root_with_lease(
        &mut self,
        root: &Path,
        root_directory: DirectoryCapability,
        generation_directory: DirectoryCapability,
        lease: Arc<FileLease>,
    ) -> Result<(), OsvSnapshotError> {
        root_directory.validate_private()?;
        let _package_directory = generation_directory.open_private_dir("packages")?;
        let leases_directory = generation_directory.open_private_dir(OSV_LEASE_DIRECTORY)?;
        let lease_file = leases_directory.open_private_file(OSV_GENERATION_LOCK)?;
        if lease_file.metadata()?.len() != 0 {
            return Err(OsvSnapshotError::Invalid("generation lease file"));
        }
        let manifest = read_manifest(&generation_directory)?;
        if manifest.schema != SNAPSHOT_SCHEMA
            || manifest.generation != self.generation
            || manifest.scope != self.scope
            || manifest.source_digest != self.source_digest
            || manifest.package_index_digest != self.package_index_digest
            || manifest.advisory_objects != self.advisory_objects
            || manifest.package_rows != self.package_rows
            || manifest.package_count != self.package_count
            || manifest.stored_bytes != self.stored_bytes
        {
            return Err(OsvSnapshotError::Invalid(
                "manifest does not match authority",
            ));
        }
        if self.advisory_objects == 0
            || self.advisory_objects > MAX_OSV_SNAPSHOT_OBJECTS
            || self.package_count == 0
            || self.package_count > MAX_OSV_SNAPSHOT_PACKAGES
            || self.package_rows == 0
            || self.package_rows > MAX_OSV_SNAPSHOT_PACKAGE_ROWS
            || self.stored_bytes == 0
            || self.stored_bytes > 64 * 1024 * 1024 * 1024
        {
            return Err(OsvSnapshotError::Invalid(
                "snapshot manifest exceeds limits",
            ));
        }
        let index_file = generation_directory.open_private_file("package-index.bin")?;
        let expected_len = self
            .package_count
            .checked_mul(PACKAGE_INDEX_RECORD_BYTES)
            .ok_or(OsvSnapshotError::Invalid("package index length overflow"))?;
        if index_file.metadata()?.len() != expected_len {
            return Err(OsvSnapshotError::Invalid("package index length"));
        }
        let index_records = validate_package_index(
            index_file,
            self.package_index_digest,
            self.package_count,
            self.package_rows,
            self.stored_bytes,
        )?;
        self.directory = root.join(&self.generation);
        self.root_directory = Some(root_directory);
        self.generation_directory = Some(generation_directory);
        self.index_verified = true;
        self.lease = Some(lease);
        self.index_records = Some(Arc::new(index_records));
        Ok(())
    }

    pub(crate) fn generation_id(&self) -> &str {
        &self.generation
    }

    pub(crate) fn root_path(&self) -> Option<&Path> {
        self.directory.parent()
    }

    /// Releases the root writer lease only after the durable authority file
    /// names this generation and downgrades its generation lock to a reader.
    pub(crate) fn commit_persisted(&self) -> Result<(), OsvSnapshotError> {
        if let Some(lease) = self.lease.as_ref() {
            lease.downgrade_to_shared()?;
        }
        if let Some(root_lease) = self.pending_root_lease.as_ref() {
            root_lease.release()?;
        }
        Ok(())
    }

    /// Removes orphan staging directories and unreferenced immutable
    /// generations while holding the cross-process owner lock. Active readers
    /// are protected by per-generation shared kernel leases.
    pub(crate) fn prune_unreferenced_generations(
        root: impl AsRef<Path>,
        retained: &BTreeSet<String>,
    ) -> Result<bool, OsvSnapshotError> {
        prune_unreferenced_generations(root.as_ref(), retained)
    }

    /// Reads and validates only the selected package's postings, then returns
    /// exact-version matches and whether any matching range was unresolved.
    pub(crate) fn matching(
        &self,
        package: &PackageIdentity,
        version: &str,
    ) -> Result<(Vec<Advisory>, bool), OsvSnapshotError> {
        if !self.index_verified {
            return Err(OsvSnapshotError::Invalid("package index was not verified"));
        }
        let key = package_key(package);
        let Some(entry) = self.find_package(&key)? else {
            return Ok((Vec::new(), false));
        };
        if entry.rows > MAX_OSV_QUERY_RECORDS {
            return Err(OsvSnapshotError::Limit(OsvSnapshotLimit::QueryRows));
        }
        if entry.bytes > MAX_OSV_QUERY_BYTES {
            return Err(OsvSnapshotError::Limit(OsvSnapshotLimit::QueryBytes));
        }
        let generation_directory =
            self.generation_directory
                .as_ref()
                .ok_or(OsvSnapshotError::Invalid(
                    "snapshot directory was not attached",
                ))?;
        let package_directory = generation_directory.open_private_dir("packages")?;
        let file = package_directory.open_private_file(&format!("{}.jsonl", hex(&key)))?;
        let opened_metadata = file.metadata()?;
        if opened_metadata.len() != entry.bytes {
            return Err(OsvSnapshotError::Invalid("package posting file changed"));
        }
        let mut reader = BufReader::new(file);
        let mut hasher = blake3::Hasher::new();
        let mut line = Vec::with_capacity(4096);
        let mut row_count = 0_u64;
        let mut matches = Vec::new();
        let mut unresolved = false;
        let mut matched_bytes = 0_u64;
        while read_bounded_line(&mut reader, &mut line, MAX_OSV_SNAPSHOT_ROW_BYTES)? {
            hasher.update(&line);
            row_count = row_count
                .checked_add(1)
                .ok_or(OsvSnapshotError::Invalid("package row count overflow"))?;
            let advisory: Advisory = serde_json::from_slice(trim_line(&line))?;
            if advisory.key.native.source != AdvisorySource::Osv
                || advisory.affected.len() != 1
                || advisory.affected[0].package.ecosystem != package.ecosystem
                || advisory.affected[0].package.name != package.name
            {
                return Err(OsvSnapshotError::Invalid("package row identity"));
            }
            let (selected, row_unresolved) = AcquisitionGate::matching_with_coverage(
                std::iter::once(&advisory),
                package,
                version,
            );
            unresolved |= row_unresolved;
            if !selected.is_empty() {
                matched_bytes = matched_bytes
                    .checked_add(u64::try_from(line.len()).unwrap_or(u64::MAX))
                    .ok_or(OsvSnapshotError::Limit(OsvSnapshotLimit::QueryBytes))?;
                if matched_bytes > MAX_OSV_QUERY_BYTES
                    || matches.len() as u64 >= MAX_OSV_QUERY_RECORDS
                {
                    return Err(OsvSnapshotError::Limit(
                        if matched_bytes > MAX_OSV_QUERY_BYTES {
                            OsvSnapshotLimit::QueryBytes
                        } else {
                            OsvSnapshotLimit::QueryRows
                        },
                    ));
                }
                matches.push(advisory);
            }
            line.clear();
        }
        if row_count != entry.rows || *hasher.finalize().as_bytes() != entry.digest {
            return Err(OsvSnapshotError::Invalid("package posting digest"));
        }
        matches.sort_by(|left, right| left.key.canonical.cmp(&right.key.canonical));
        Ok((matches, unresolved))
    }

    fn find_package(&self, key: &[u8; 32]) -> Result<Option<PackageIndexEntry>, OsvSnapshotError> {
        let records = self
            .index_records
            .as_ref()
            .ok_or(OsvSnapshotError::Invalid("verified package index missing"))?;
        let mut low = 0_usize;
        let mut high = records.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let buffer = records
                .get(middle)
                .ok_or(OsvSnapshotError::Invalid("package index lookup bounds"))?;
            match buffer[..32].cmp(key) {
                std::cmp::Ordering::Less => low = middle.saturating_add(1),
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => {
                    return Ok(Some(PackageIndexEntry {
                        digest: buffer[32..64]
                            .try_into()
                            .map_err(|_| OsvSnapshotError::Invalid("package digest width"))?,
                        rows: u64::from_be_bytes(
                            buffer[64..72]
                                .try_into()
                                .map_err(|_| OsvSnapshotError::Invalid("package row width"))?,
                        ),
                        bytes: u64::from_be_bytes(
                            buffer[72..80]
                                .try_into()
                                .map_err(|_| OsvSnapshotError::Invalid("package byte width"))?,
                        ),
                    }));
                }
            }
        }
        Ok(None)
    }
}

#[derive(Clone, Copy)]
struct PackageIndexEntry {
    digest: [u8; 32],
    rows: u64,
    bytes: u64,
}

struct PackageWriter {
    writer: BufWriter<File>,
    touched: u64,
}

struct CappedRowWriter {
    bytes: Vec<u8>,
    maximum: usize,
    exceeded: bool,
}

/// One package-scoped OSV posting borrowed from its source advisory.
///
/// Serializing a row directly from borrowed fields avoids cloning the full
/// advisory's affected-package array once per package. The serialized shape
/// and field order intentionally match [`Advisory`].
#[derive(Serialize)]
struct PackageAdvisoryRow<'a> {
    schema: &'a AdvisorySchema,
    key: PackageAdvisoryKey<'a>,
    aliases: &'a [Alias],
    affected: &'a [AffectedRange],
    severity: &'a Severity,
    categories: &'a [AdvisoryCategory],
    published: &'a Option<String>,
    modified: &'a Option<String>,
    withdrawn: &'a Option<String>,
    references: &'a [Reference],
    summary: &'a Option<String>,
    malware: &'a MalwareCoverage,
    evidence: &'a Evidence,
}

#[derive(Serialize)]
struct PackageAdvisoryKey<'a> {
    canonical: &'a str,
    native: &'a crate::NativeAdvisoryId,
}

impl<'a> PackageAdvisoryRow<'a> {
    fn new(advisory: &'a Advisory, affected: &'a AffectedRange) -> Self {
        Self {
            schema: &advisory.schema,
            key: PackageAdvisoryKey {
                canonical: &advisory.key.canonical.0,
                native: &advisory.key.native,
            },
            aliases: &advisory.aliases,
            affected: std::slice::from_ref(affected),
            severity: &advisory.severity,
            categories: &advisory.categories,
            published: &advisory.published,
            modified: &advisory.modified,
            withdrawn: &advisory.withdrawn,
            references: &advisory.references,
            summary: &advisory.summary,
            malware: &advisory.malware,
            evidence: &advisory.evidence,
        }
    }
}

impl CappedRowWriter {
    fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(maximum.min(8192)),
            maximum,
            exceeded: false,
        }
    }
}

impl Write for CappedRowWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(next_len) = self.bytes.len().checked_add(buffer.len()) else {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "OSV row exceeds its encoding limit",
            ));
        };
        if next_len > self.maximum {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "OSV row exceeds its encoding limit",
            ));
        }
        self.bytes
            .try_reserve(buffer.len())
            .map_err(|_| io::Error::new(io::ErrorKind::OutOfMemory, "OSV row allocation failed"))?;
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Bounded writer for one unpublished OSV source generation.
pub struct OsvSnapshotBuilder {
    root: PathBuf,
    root_directory: Option<DirectoryCapability>,
    root_lease: Option<Arc<FileLease>>,
    staging_name: String,
    staging_directory: Option<DirectoryCapability>,
    package_directory: Option<DirectoryCapability>,
    lease_directory: Option<DirectoryCapability>,
    scope: OsvFeedScope,
    maximum_objects: u64,
    maximum_packages: u64,
    maximum_package_rows: u64,
    maximum_stored_bytes: u64,
    maximum_root_bytes: u64,
    seen_ids: BTreeSet<[u8; 32]>,
    packages: BTreeMap<[u8; 32], PackageIndexEntry>,
    writers: BTreeMap<[u8; 32], PackageWriter>,
    tick: u64,
    advisory_objects: u64,
    package_rows: u64,
    stored_bytes: u64,
    failed: bool,
}

impl OsvSnapshotBuilder {
    /// Creates a private stage below the durable snapshot root.
    pub fn create(
        root: impl AsRef<Path>,
        scope: OsvFeedScope,
        maximum_objects: u64,
        maximum_packages: u64,
        maximum_package_rows: u64,
        maximum_stored_bytes: u64,
    ) -> Result<Self, OsvSnapshotError> {
        if maximum_objects == 0
            || maximum_packages == 0
            || maximum_package_rows == 0
            || maximum_stored_bytes == 0
        {
            return Err(OsvSnapshotError::Invalid("zero staging quota"));
        }
        let root = root.as_ref();
        let root_directory = DirectoryCapability::open_or_create_private(root)?;
        let root_lease = Arc::new(
            FileLease::open(
                &root_directory,
                OSV_ROOT_LOCK,
                true,
                LeaseMode::Exclusive,
                false,
            )?
            .ok_or(OsvSnapshotError::Busy)?,
        );
        prune_orphan_stages(&root_directory)?;
        let existing_bytes = snapshot_storage_bytes(&root_directory)?;
        let maximum_root_bytes = maximum_stored_bytes.min(MAX_OSV_SNAPSHOT_STORAGE_BYTES);
        let index_reserve = maximum_packages
            .min(MAX_OSV_SNAPSHOT_PACKAGES)
            .checked_mul(PACKAGE_INDEX_RECORD_BYTES)
            .and_then(|bytes| bytes.checked_add(4096))
            .ok_or(OsvSnapshotError::Limit(OsvSnapshotLimit::StoredBytes))?;
        let maximum_stored_bytes = maximum_root_bytes
            .checked_sub(existing_bytes)
            .and_then(|bytes| bytes.checked_sub(index_reserve))
            .ok_or(OsvSnapshotError::Limit(OsvSnapshotLimit::StoredBytes))?;
        if maximum_stored_bytes == 0 {
            return Err(OsvSnapshotError::Limit(OsvSnapshotLimit::StoredBytes));
        }
        static STAGE_NONCE: AtomicU64 = AtomicU64::new(0);
        let (staging_name, staging_directory) = (0..32)
            .find_map(|_| {
                let name = format!(
                    ".osv-stage-{}-{}",
                    std::process::id(),
                    STAGE_NONCE.fetch_add(1, Ordering::Relaxed)
                );
                match root_directory.create_private_dir(&name) {
                    Ok(directory) => Some(Ok((name, directory))),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .ok_or_else(|| {
                OsvSnapshotError::Io(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "could not allocate unique OSV staging directory",
                ))
            })??;
        let package_directory = staging_directory.create_private_dir("packages")?;
        let lease_directory = staging_directory.create_private_dir(OSV_LEASE_DIRECTORY)?;
        Ok(Self {
            root: root.to_path_buf(),
            root_directory: Some(root_directory),
            root_lease: Some(root_lease),
            staging_name,
            staging_directory: Some(staging_directory),
            package_directory: Some(package_directory),
            lease_directory: Some(lease_directory),
            scope,
            maximum_objects: maximum_objects.min(MAX_OSV_SNAPSHOT_OBJECTS),
            maximum_packages: maximum_packages.min(MAX_OSV_SNAPSHOT_PACKAGES),
            maximum_package_rows: maximum_package_rows.min(MAX_OSV_SNAPSHOT_PACKAGE_ROWS),
            maximum_stored_bytes,
            maximum_root_bytes,
            seen_ids: BTreeSet::new(),
            packages: BTreeMap::new(),
            writers: BTreeMap::new(),
            tick: 0,
            advisory_objects: 0,
            package_rows: 0,
            stored_bytes: 0,
            failed: false,
        })
    }

    /// Admits one fully parsed OSV object into bounded package postings.
    pub fn push(&mut self, advisory: &Advisory) -> Result<(), OsvSnapshotError> {
        if self.failed {
            return Err(OsvSnapshotError::Invalid("staging transaction was aborted"));
        }
        match self.push_one(advisory) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn push_one(&mut self, advisory: &Advisory) -> Result<(), OsvSnapshotError> {
        if advisory.key.native.source != AdvisorySource::Osv {
            return Err(OsvSnapshotError::Invalid("non-OSV source object"));
        }
        let next_objects = self
            .advisory_objects
            .checked_add(1)
            .ok_or(OsvSnapshotError::Limit(OsvSnapshotLimit::Objects))?;
        if next_objects > self.maximum_objects {
            return Err(OsvSnapshotError::Limit(OsvSnapshotLimit::Objects));
        }
        let id_hash = *blake3::hash(advisory.key.native.id.as_bytes()).as_bytes();
        if !self.seen_ids.insert(id_hash) {
            return Err(OsvSnapshotError::Invalid("duplicate OSV advisory identity"));
        }
        self.advisory_objects = next_objects;

        let mut admitted = false;
        for affected in &advisory.affected {
            if !scope_contains(self.scope, &affected.package) {
                continue;
            }
            let key = package_key(&affected.package);
            if !self.packages.contains_key(&key)
                && u64::try_from(self.packages.len()).unwrap_or(u64::MAX) >= self.maximum_packages
            {
                return Err(OsvSnapshotError::Limit(OsvSnapshotLimit::Packages));
            }
            let next_rows = self
                .package_rows
                .checked_add(1)
                .ok_or(OsvSnapshotError::Limit(OsvSnapshotLimit::PackageRows))?;
            if next_rows > self.maximum_package_rows {
                return Err(OsvSnapshotError::Limit(OsvSnapshotLimit::PackageRows));
            }
            let mut encoded = CappedRowWriter::new(MAX_OSV_SNAPSHOT_ROW_BYTES);
            if let Err(error) =
                serde_json::to_writer(&mut encoded, &PackageAdvisoryRow::new(advisory, affected))
            {
                if encoded.exceeded {
                    return Err(OsvSnapshotError::Limit(OsvSnapshotLimit::StoredBytes));
                }
                return Err(OsvSnapshotError::Encoding(error));
            }
            let bytes = encoded.bytes;
            if bytes.len().saturating_add(1) > MAX_OSV_SNAPSHOT_ROW_BYTES {
                return Err(OsvSnapshotError::Limit(OsvSnapshotLimit::StoredBytes));
            }
            let record_bytes = u64::try_from(bytes.len())
                .unwrap_or(u64::MAX)
                .checked_add(1)
                .ok_or(OsvSnapshotError::Limit(OsvSnapshotLimit::StoredBytes))?;
            let next_stored = self
                .stored_bytes
                .checked_add(record_bytes)
                .filter(|bytes| *bytes <= self.maximum_stored_bytes)
                .ok_or(OsvSnapshotError::Limit(OsvSnapshotLimit::StoredBytes))?;
            let writer = self.package_writer(key)?;
            writer.write_all(&bytes)?;
            writer.write_all(b"\n")?;
            let entry = self.packages.entry(key).or_insert(PackageIndexEntry {
                digest: [0; 32],
                rows: 0,
                bytes: 0,
            });
            entry.rows = entry
                .rows
                .checked_add(1)
                .ok_or(OsvSnapshotError::Limit(OsvSnapshotLimit::PackageRows))?;
            entry.bytes = entry
                .bytes
                .checked_add(record_bytes)
                .ok_or(OsvSnapshotError::Limit(OsvSnapshotLimit::StoredBytes))?;
            self.package_rows = next_rows;
            self.stored_bytes = next_stored;
            admitted = true;
        }
        if admitted {
            self.advisory_objects = next_objects;
        }
        Ok(())
    }

    fn package_writer(&mut self, key: [u8; 32]) -> Result<&mut BufWriter<File>, OsvSnapshotError> {
        self.tick = self.tick.saturating_add(1);
        if !self.writers.contains_key(&key) && self.writers.len() >= MAX_OPEN_PACKAGE_FILES {
            let evict = self
                .writers
                .iter()
                .min_by_key(|(_, writer)| writer.touched)
                .map(|(key, _)| *key)
                .ok_or(OsvSnapshotError::Invalid("writer cache invariant"))?;
            let mut writer = self
                .writers
                .remove(&evict)
                .ok_or(OsvSnapshotError::Invalid("writer eviction invariant"))?;
            writer.writer.flush()?;
            writer.writer.get_ref().sync_all()?;
        }
        if !self.writers.contains_key(&key) {
            let name = format!("{}.jsonl", hex(&key));
            let directory = self
                .package_directory
                .as_ref()
                .ok_or(OsvSnapshotError::Invalid("package directory missing"))?;
            let mut file = directory.open_private_file_read_write(&name, true)?;
            file.seek(SeekFrom::End(0))?;
            self.writers.insert(
                key,
                PackageWriter {
                    writer: BufWriter::new(file),
                    touched: self.tick,
                },
            );
        }
        let writer = self
            .writers
            .get_mut(&key)
            .ok_or(OsvSnapshotError::Invalid("writer lookup invariant"))?;
        writer.touched = self.tick;
        Ok(&mut writer.writer)
    }

    /// Seals the staged feed and publishes its immutable generation directory.
    pub fn finish(mut self, source_digest: [u8; 32]) -> Result<OsvSnapshotRef, OsvSnapshotError> {
        if self.failed {
            return Err(OsvSnapshotError::Invalid("staging transaction was aborted"));
        }
        if self.advisory_objects == 0 || self.packages.is_empty() {
            return Err(OsvSnapshotError::Invalid("no supported package advisories"));
        }
        for writer in self.writers.values_mut() {
            writer.writer.flush()?;
            writer.writer.get_ref().sync_all()?;
        }
        self.writers.clear();
        let staging_directory = self
            .staging_directory
            .as_ref()
            .ok_or(OsvSnapshotError::Invalid("staging directory missing"))?;
        let package_directory = self
            .package_directory
            .as_ref()
            .ok_or(OsvSnapshotError::Invalid("package directory missing"))?;
        let lease_directory = self
            .lease_directory
            .as_ref()
            .ok_or(OsvSnapshotError::Invalid("lease directory missing"))?;
        let mut index = staging_directory.create_file_exclusive("package-index.bin")?;
        let mut index_hasher = blake3::Hasher::new();
        for (key, entry) in &mut self.packages {
            let package_file =
                package_directory.open_private_file(&format!("{}.jsonl", hex(key)))?;
            let metadata = package_file.metadata()?;
            if metadata.len() != entry.bytes {
                return Err(OsvSnapshotError::Invalid("package file byte count"));
            }
            entry.digest = digest_file(package_file)?;
            let mut record = [0_u8; PACKAGE_INDEX_RECORD_BYTES as usize];
            record[..32].copy_from_slice(key);
            record[32..64].copy_from_slice(&entry.digest);
            record[64..72].copy_from_slice(&entry.rows.to_be_bytes());
            record[72..80].copy_from_slice(&entry.bytes.to_be_bytes());
            index.write_all(&record)?;
            index_hasher.update(&record);
        }
        index.sync_all()?;
        let package_index_digest = *index_hasher.finalize().as_bytes();
        let generation = generation_id(self.scope, source_digest);
        let manifest = OsvSnapshotManifest {
            schema: SNAPSHOT_SCHEMA,
            generation: generation.clone(),
            scope: self.scope,
            source_digest,
            package_index_digest,
            advisory_objects: self.advisory_objects,
            package_rows: self.package_rows,
            package_count: u64::try_from(self.packages.len()).unwrap_or(u64::MAX),
            stored_bytes: self.stored_bytes,
        };
        let mut manifest_file = staging_directory.create_file_exclusive("manifest.json")?;
        serde_json::to_writer(&mut manifest_file, &manifest)?;
        manifest_file.write_all(b"\n")?;
        manifest_file.sync_all()?;
        let staged_lease_file = lease_directory.create_file_exclusive(OSV_GENERATION_LOCK)?;
        staged_lease_file.sync_all()?;
        FileExt::lock_exclusive(&staged_lease_file)?;
        let mut generation_lease = Some(Arc::new(FileLease(Mutex::new(LeaseState {
            file: staged_lease_file,
            mode: LeaseMode::Exclusive,
        }))));
        package_directory.sync_all()?;
        lease_directory.sync_all()?;
        staging_directory.sync_all()?;
        let root_directory = self
            .root_directory
            .as_ref()
            .ok_or(OsvSnapshotError::Invalid("snapshot root was not attached"))?;
        if snapshot_storage_bytes(root_directory)? > self.maximum_root_bytes {
            return Err(OsvSnapshotError::Limit(OsvSnapshotLimit::StoredBytes));
        }

        // Windows directory renames require all descendant directory handles
        // to be released first. The stage is private and the root writer lease
        // remains held across this short transition.
        self.package_directory.take();
        self.lease_directory.take();
        self.staging_directory.take();
        let staging_name = std::mem::take(&mut self.staging_name);
        let published_with_staged_lease =
            match root_directory.rename(&staging_name, &generation, false) {
                Ok(()) => true,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    generation_lease.take();
                    root_directory.remove_dir_all(&staging_name, MAX_OSV_STORAGE_ENTRIES)?;
                    false
                }
                Err(error) => {
                    let _ = root_directory.remove_dir_all(&staging_name, MAX_OSV_STORAGE_ENTRIES);
                    return Err(OsvSnapshotError::Io(error));
                }
            };
        let mut reference = OsvSnapshotRef {
            generation,
            scope: self.scope,
            source_digest,
            package_index_digest,
            advisory_objects: self.advisory_objects,
            package_rows: self.package_rows,
            package_count: u64::try_from(self.packages.len()).unwrap_or(u64::MAX),
            stored_bytes: self.stored_bytes,
            directory: PathBuf::new(),
            root_directory: None,
            generation_directory: None,
            index_verified: false,
            lease: None,
            pending_root_lease: None,
            index_records: None,
        };
        if published_with_staged_lease {
            reference.attach_root_with_lease(
                &self.root,
                root_directory.clone(),
                root_directory.open_private_dir(&generation)?,
                generation_lease
                    .take()
                    .ok_or(OsvSnapshotError::Invalid("generation lease missing"))?,
            )?;
        } else {
            let generation_directory = root_directory.open_private_dir(&generation)?;
            let lease_directory = generation_directory.open_private_dir(OSV_LEASE_DIRECTORY)?;
            let existing_lease = Arc::new(
                FileLease::open(
                    &lease_directory,
                    OSV_GENERATION_LOCK,
                    false,
                    LeaseMode::Shared,
                    true,
                )?
                .ok_or(OsvSnapshotError::Invalid("generation lease unavailable"))?,
            );
            reference.attach_root_with_lease(
                &self.root,
                root_directory.clone(),
                generation_directory,
                existing_lease,
            )?;
        }
        reference.pending_root_lease = self.root_lease.take();
        Ok(reference)
    }
}

impl Drop for OsvSnapshotBuilder {
    fn drop(&mut self) {
        for writer in self.writers.values_mut() {
            let _ = writer.writer.flush();
        }
        self.writers.clear();
        self.package_directory.take();
        self.lease_directory.take();
        self.staging_directory.take();
        if !self.staging_name.is_empty()
            && let Some(root) = self.root_directory.as_ref()
        {
            let _ = root.remove_dir_all(&self.staging_name, MAX_OSV_STORAGE_ENTRIES);
        }
    }
}

fn scope_contains(scope: OsvFeedScope, package: &PackageIdentity) -> bool {
    let ecosystem = match package.ecosystem.as_str() {
        "cargo" => OsvEcosystem::Cargo,
        "npm" => OsvEcosystem::Npm,
        "pypi" => OsvEcosystem::Pypi,
        "maven" => OsvEcosystem::Maven,
        "nuget" => OsvEcosystem::Nuget,
        "go" => OsvEcosystem::Go,
        _ => return false,
    };
    match scope {
        OsvFeedScope::All => true,
        OsvFeedScope::Ecosystem(selected) => selected == ecosystem,
    }
}

fn package_key(package: &PackageIdentity) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nudox.osv.package.v1\0");
    hasher.update(package.ecosystem.as_bytes());
    hasher.update(&[0]);
    hasher.update(package.name.as_bytes());
    *hasher.finalize().as_bytes()
}

fn generation_id(scope: OsvFeedScope, digest: [u8; 32]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nudox.osv.snapshot.v1\0");
    hasher.update(&[as_u8(scope)]);
    hasher.update(&digest);
    hex(hasher.finalize().as_bytes())
}

const fn as_u8(scope: OsvFeedScope) -> u8 {
    match scope {
        OsvFeedScope::All => 0,
        OsvFeedScope::Ecosystem(OsvEcosystem::Cargo) => 1,
        OsvFeedScope::Ecosystem(OsvEcosystem::Npm) => 2,
        OsvFeedScope::Ecosystem(OsvEcosystem::Pypi) => 3,
        OsvFeedScope::Ecosystem(OsvEcosystem::Maven) => 4,
        OsvFeedScope::Ecosystem(OsvEcosystem::Nuget) => 5,
        OsvFeedScope::Ecosystem(OsvEcosystem::Go) => 6,
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn read_manifest(directory: &DirectoryCapability) -> Result<OsvSnapshotManifest, OsvSnapshotError> {
    let mut file = directory.open_private_file("manifest.json")?;
    let metadata = file.metadata()?;
    if metadata.len() > 4096 {
        return Err(OsvSnapshotError::Invalid("snapshot manifest file"));
    }
    let mut bytes = Vec::with_capacity(4096);
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(OsvSnapshotError::Invalid("snapshot manifest file"));
    }
    serde_json::from_slice(&bytes).map_err(OsvSnapshotError::Encoding)
}

fn digest_file(mut file: File) -> Result<[u8; 32], OsvSnapshotError> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(OsvSnapshotError::Invalid("snapshot data file"));
    }
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn validate_package_index(
    mut file: File,
    expected_digest: [u8; 32],
    package_count: u64,
    expected_rows: u64,
    expected_bytes: u64,
) -> Result<Vec<[u8; PACKAGE_INDEX_RECORD_BYTES as usize]>, OsvSnapshotError> {
    let count = usize::try_from(package_count)
        .map_err(|_| OsvSnapshotError::Invalid("package index count conversion"))?;
    let mut records = Vec::new();
    records
        .try_reserve_exact(count)
        .map_err(|_| OsvSnapshotError::Limit(OsvSnapshotLimit::StoredBytes))?;
    let mut hasher = blake3::Hasher::new();
    let mut record = [0_u8; PACKAGE_INDEX_RECORD_BYTES as usize];
    let mut previous = None;
    let mut rows = 0_u64;
    let mut bytes = 0_u64;
    for _ in 0..package_count {
        file.read_exact(&mut record)?;
        hasher.update(&record);
        let key: [u8; 32] = record[..32]
            .try_into()
            .map_err(|_| OsvSnapshotError::Invalid("package key width"))?;
        if previous.is_some_and(|value| value >= key) {
            return Err(OsvSnapshotError::Invalid("package index ordering"));
        }
        previous = Some(key);
        let package_rows = u64::from_be_bytes(
            record[64..72]
                .try_into()
                .map_err(|_| OsvSnapshotError::Invalid("package row width"))?,
        );
        let package_bytes = u64::from_be_bytes(
            record[72..80]
                .try_into()
                .map_err(|_| OsvSnapshotError::Invalid("package byte width"))?,
        );
        if package_rows == 0 || package_bytes == 0 {
            return Err(OsvSnapshotError::Invalid("empty package index entry"));
        }
        rows = rows
            .checked_add(package_rows)
            .ok_or(OsvSnapshotError::Invalid("package row total overflow"))?;
        bytes = bytes
            .checked_add(package_bytes)
            .ok_or(OsvSnapshotError::Invalid("package byte total overflow"))?;
        records.push(record);
    }
    if *hasher.finalize().as_bytes() != expected_digest
        || rows != expected_rows
        || bytes != expected_bytes
    {
        return Err(OsvSnapshotError::Invalid("package index digest or totals"));
    }
    Ok(records)
}

fn snapshot_storage_bytes(root: &DirectoryCapability) -> Result<u64, OsvSnapshotError> {
    fn visit(
        directory: &DirectoryCapability,
        depth: usize,
        entries: &mut usize,
        total: &mut u64,
    ) -> Result<(), OsvSnapshotError> {
        if depth > 3 {
            return Err(OsvSnapshotError::Invalid("snapshot storage nesting"));
        }
        let remaining = MAX_OSV_STORAGE_ENTRIES.saturating_sub(*entries);
        for item in directory.entries(remaining)? {
            *entries = (*entries)
                .checked_add(1)
                .filter(|count| *count <= MAX_OSV_STORAGE_ENTRIES)
                .ok_or(OsvSnapshotError::Limit(OsvSnapshotLimit::StoredBytes))?;
            let name = item
                .name
                .to_str()
                .ok_or(OsvSnapshotError::Invalid("non-UTF-8 snapshot entry"))?;
            match item.kind {
                backend_platform::directory::EntryKind::Directory => {
                    let child = directory.open_private_dir(name)?;
                    visit(&child, depth + 1, entries, total)?;
                }
                backend_platform::directory::EntryKind::File => {
                    let file = directory.open_private_file(name)?;
                    *total = (*total)
                        .checked_add(file.metadata()?.len())
                        .ok_or(OsvSnapshotError::Limit(OsvSnapshotLimit::StoredBytes))?;
                }
                backend_platform::directory::EntryKind::Link => {
                    return Err(OsvSnapshotError::Invalid("snapshot storage symlink"));
                }
                backend_platform::directory::EntryKind::Special => {
                    return Err(OsvSnapshotError::Invalid("snapshot storage special file"));
                }
            }
        }
        Ok(())
    }

    root.validate_private()?;
    let mut entries = 0;
    let mut total = 0;
    visit(root, 0, &mut entries, &mut total)?;
    Ok(total)
}

fn prune_orphan_stages(root: &DirectoryCapability) -> Result<(), OsvSnapshotError> {
    let entries = root.entries(MAX_OSV_STORAGE_ENTRIES)?;
    for entry in entries {
        let Some(name) = entry.name.to_str() else {
            continue;
        };
        if !name.starts_with(".osv-stage-") {
            continue;
        }
        if entry.kind != backend_platform::directory::EntryKind::Directory {
            return Err(OsvSnapshotError::Invalid("snapshot staging entry"));
        }
        let stage = root.open_private_dir(name)?;
        drop(stage);
        root.remove_dir_all(name, MAX_OSV_STORAGE_ENTRIES)?;
    }
    Ok(())
}

fn prune_unreferenced_generations(
    root: &Path,
    retained: &BTreeSet<String>,
) -> Result<bool, OsvSnapshotError> {
    let directory = match DirectoryCapability::open(root) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(OsvSnapshotError::Io(error)),
    };
    prune_unreferenced_generations_at(&directory, retained)
}

fn prune_unreferenced_generations_at(
    root: &DirectoryCapability,
    retained: &BTreeSet<String>,
) -> Result<bool, OsvSnapshotError> {
    root.validate_private()?;
    let Some(_root_lease) =
        FileLease::open(root, OSV_ROOT_LOCK, true, LeaseMode::Exclusive, false)?
    else {
        return Ok(false);
    };

    static DELETE_NONCE: AtomicU64 = AtomicU64::new(0);
    for item in root.entries(MAX_OSV_STORAGE_ENTRIES)? {
        let Some(name) = item.name.to_str() else {
            continue;
        };
        let recognized = name.starts_with(".osv-stage-")
            || name.starts_with(".osv-delete-")
            || is_generation_name(name);
        if !recognized {
            continue;
        }
        if item.kind != backend_platform::directory::EntryKind::Directory {
            return Err(OsvSnapshotError::Invalid("snapshot generation entry"));
        }
        if name.starts_with(".osv-stage-") || name.starts_with(".osv-delete-") {
            let child = root.open_private_dir(name)?;
            drop(child);
            root.remove_dir_all(name, MAX_OSV_STORAGE_ENTRIES)?;
            continue;
        }
        if retained.contains(name) {
            continue;
        }
        let generation = root.open_private_dir(name)?;
        let leases = generation.open_private_dir(OSV_LEASE_DIRECTORY)?;
        let lease = match FileLease::open(
            &leases,
            OSV_GENERATION_LOCK,
            false,
            LeaseMode::Exclusive,
            false,
        ) {
            Ok(Some(lease)) => lease,
            Ok(None) => continue,
            Err(OsvSnapshotError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                // A generation without its lock cannot prove that no older
                // reader still has it open, so leave it for explicit repair.
                continue;
            }
            Err(error) => return Err(error),
        };
        let tombstone = format!(
            ".osv-delete-{}-{}-{}",
            &name[..16],
            std::process::id(),
            DELETE_NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        drop(leases);
        drop(generation);
        root.rename(name, &tombstone, false)?;
        drop(lease);
        root.remove_dir_all(&tombstone, MAX_OSV_STORAGE_ENTRIES)?;
    }
    root.sync_all()?;
    Ok(true)
}

fn is_generation_name(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn trim_line(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n").unwrap_or(line)
}

fn read_bounded_line(
    reader: &mut BufReader<File>,
    output: &mut Vec<u8>,
    maximum: usize,
) -> Result<bool, OsvSnapshotError> {
    output.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(!output.is_empty());
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if output.len().saturating_add(count) > maximum {
            return Err(OsvSnapshotError::Limit(OsvSnapshotLimit::QueryBytes));
        }
        let consumed_newline = available.get(count.saturating_sub(1)) == Some(&b'\n');
        output.extend_from_slice(&available[..count]);
        reader.consume(count);
        if consumed_newline {
            return Ok(true);
        }
    }
}
