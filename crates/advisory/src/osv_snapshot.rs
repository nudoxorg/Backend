//! Immutable, package-keyed storage for large OSV snapshots.
//!
//! The archive parser streams one advisory at a time into a private staging
//! directory. A complete generation is published only after its package files
//! and binary lookup index have been flushed and checksummed. Runtime lookup
//! reads one package's bounded posting file instead of reconstructing the
//! global advisory map in memory.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    AcquisitionGate, Advisory, AdvisorySource, CanonicalAdvisoryId, OsvEcosystem, OsvFeedScope,
    PackageIdentity,
};

const SNAPSHOT_SCHEMA: u16 = 1;
const PACKAGE_INDEX_RECORD_BYTES: u64 = 80;
const MAX_OPEN_PACKAGE_FILES: usize = 32;
const MAX_OSV_SNAPSHOT_ROW_BYTES: usize = 16 * 1024 * 1024;
const MAX_OSV_QUERY_RECORDS: u64 = 100_000;
const MAX_OSV_QUERY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_OSV_STORAGE_ENTRIES: usize = 4_000_000;

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
}

impl std::fmt::Display for OsvSnapshotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(_) => formatter.write_str("OSV snapshot local storage failed"),
            Self::Encoding(_) => formatter.write_str("OSV snapshot record encoding failed"),
            Self::Invalid(reason) => write!(formatter, "OSV snapshot is invalid: {reason}"),
            Self::Limit(limit) => write!(formatter, "OSV snapshot limit reached: {limit:?}"),
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
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
    index_verified: bool,
}

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
        if self.generation.len() != 64
            || !self
                .generation
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(OsvSnapshotError::Invalid("generation identity"));
        }
        let directory = root.as_ref().join(&self.generation);
        let directory_metadata = fs::symlink_metadata(&directory)?;
        if directory_metadata.file_type().is_symlink()
            || !directory_metadata.is_dir()
            || !private_permissions(&directory_metadata)
        {
            return Err(OsvSnapshotError::Invalid("generation directory"));
        }
        let package_directory = directory.join("packages");
        let package_metadata = fs::symlink_metadata(&package_directory)?;
        if package_metadata.file_type().is_symlink()
            || !package_metadata.is_dir()
            || !private_permissions(&package_metadata)
        {
            return Err(OsvSnapshotError::Invalid("package posting directory"));
        }
        let manifest = read_manifest(&directory)?;
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
            return Err(OsvSnapshotError::Invalid("manifest does not match authority"));
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
            return Err(OsvSnapshotError::Invalid("snapshot manifest exceeds limits"));
        }
        let index_path = directory.join("package-index.bin");
        let index_metadata = fs::symlink_metadata(&index_path)?;
        if index_metadata.file_type().is_symlink()
            || !index_metadata.is_file()
            || !private_permissions(&index_metadata)
        {
            return Err(OsvSnapshotError::Invalid("package index file"));
        }
        let expected_len = self
            .package_count
            .checked_mul(PACKAGE_INDEX_RECORD_BYTES)
            .ok_or(OsvSnapshotError::Invalid("package index length overflow"))?;
        if index_metadata.len() != expected_len {
            return Err(OsvSnapshotError::Invalid("package index length"));
        }
        validate_package_index(
            &index_path,
            self.package_index_digest,
            self.package_count,
            self.package_rows,
            self.stored_bytes,
        )?;
        self.directory = directory;
        self.index_verified = true;
        Ok(())
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
        let path = self
            .directory
            .join("packages")
            .join(format!("{}.jsonl", hex(&key)));
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || !private_permissions(&metadata)
            || metadata.len() != entry.bytes
        {
            return Err(OsvSnapshotError::Invalid("package posting file"));
        }
        let file = File::open(path)?;
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
                    return Err(OsvSnapshotError::Limit(if matched_bytes > MAX_OSV_QUERY_BYTES {
                        OsvSnapshotLimit::QueryBytes
                    } else {
                        OsvSnapshotLimit::QueryRows
                    }));
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
        let index_path = self.directory.join("package-index.bin");
        let mut index = File::open(index_path)?;
        let mut low = 0_u64;
        let mut high = self.package_count;
        let mut buffer = [0_u8; PACKAGE_INDEX_RECORD_BYTES as usize];
        while low < high {
            let middle = low + (high - low) / 2;
            let offset = middle
                .checked_mul(PACKAGE_INDEX_RECORD_BYTES)
                .ok_or(OsvSnapshotError::Invalid("package index offset overflow"))?;
            index.seek(SeekFrom::Start(offset))?;
            index.read_exact(&mut buffer)?;
            match buffer[..32].cmp(key) {
                std::cmp::Ordering::Less => low = middle + 1,
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

/// Bounded writer for one unpublished OSV source generation.
pub struct OsvSnapshotBuilder {
    root: PathBuf,
    staging: Option<tempfile::TempDir>,
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
        fs::create_dir_all(root)?;
        let root_metadata = fs::symlink_metadata(root)?;
        if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
            return Err(OsvSnapshotError::Invalid("snapshot root directory"));
        }
        let existing_bytes = snapshot_storage_bytes(root)?;
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
        let staging = tempfile::Builder::new()
            .prefix(".osv-stage-")
            .tempdir_in(root)
            .map_err(OsvSnapshotError::Io)?;
        let package_directory = staging.path().join("packages");
        fs::create_dir(&package_directory)?;
        set_private_directory_permissions(&package_directory)?;
        Ok(Self {
            root: root.to_path_buf(),
            staging: Some(staging),
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
                && u64::try_from(self.packages.len()).unwrap_or(u64::MAX)
                    >= self.maximum_packages
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
            let mut row = advisory.clone();
            row.key.canonical = CanonicalAdvisoryId(row.key.native.id.clone());
            row.affected = Box::new([affected.clone()]);
            let bytes = serde_json::to_vec(&row)?;
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
            let path = self
                .staging
                .as_ref()
                .ok_or(OsvSnapshotError::Invalid("staging directory missing"))?
                .path()
                .join("packages")
                .join(format!("{}.jsonl", hex(&key)));
            let mut options = OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            self.writers.insert(
                key,
                PackageWriter {
                    writer: BufWriter::new(options.open(path)?),
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
        let staging = self
            .staging
            .as_ref()
            .ok_or(OsvSnapshotError::Invalid("staging directory missing"))?;
        let index_path = staging.path().join("package-index.bin");
        let mut index = create_private_file(&index_path)?;
        let mut index_hasher = blake3::Hasher::new();
        for (key, entry) in &mut self.packages {
            let package_path = staging
                .path()
                .join("packages")
                .join(format!("{}.jsonl", hex(key)));
            entry.digest = digest_file(&package_path)?;
            let metadata = fs::metadata(&package_path)?;
            if metadata.len() != entry.bytes {
                return Err(OsvSnapshotError::Invalid("package file byte count"));
            }
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
        let manifest_path = staging.path().join("manifest.json");
        let mut manifest_file = create_private_file(&manifest_path)?;
        serde_json::to_writer(&mut manifest_file, &manifest)?;
        manifest_file.write_all(b"\n")?;
        manifest_file.sync_all()?;
        sync_directory(&staging.path().join("packages"))?;
        sync_directory(staging.path())?;
        if snapshot_storage_bytes(&self.root)? > self.maximum_root_bytes {
            return Err(OsvSnapshotError::Limit(OsvSnapshotLimit::StoredBytes));
        }

        let staging_path = self
            .staging
            .take()
            .ok_or(OsvSnapshotError::Invalid("staging directory missing"))?
            .keep();
        let destination = self.root.join(&generation);
        match fs::rename(&staging_path, &destination) {
            Ok(()) => sync_directory(&self.root)?,
            Err(_error) if destination.exists() => {
                let _ = fs::remove_dir_all(&staging_path);
            }
            Err(error) => {
                let _ = fs::remove_dir_all(&staging_path);
                return Err(OsvSnapshotError::Io(error));
            }
        }
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
            index_verified: false,
        };
        reference.attach_root(&self.root)?;
        Ok(reference)
    }
}

impl Drop for OsvSnapshotBuilder {
    fn drop(&mut self) {
        for writer in self.writers.values_mut() {
            let _ = writer.writer.flush();
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
    hasher.update(&[scope as_u8(scope)]);
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

fn read_manifest(directory: &Path) -> Result<OsvSnapshotManifest, OsvSnapshotError> {
    let path = directory.join("manifest.json");
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || !private_permissions(&metadata)
        || metadata.len() > 4096
    {
        return Err(OsvSnapshotError::Invalid("snapshot manifest file"));
    }
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(OsvSnapshotError::Encoding)
}

fn digest_file(path: &Path) -> Result<[u8; 32], OsvSnapshotError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || !private_permissions(&metadata) {
        return Err(OsvSnapshotError::Invalid("snapshot data file"));
    }
    let mut file = File::open(path)?;
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
    path: &Path,
    expected_digest: [u8; 32],
    package_count: u64,
    expected_rows: u64,
    expected_bytes: u64,
) -> Result<(), OsvSnapshotError> {
    let mut file = File::open(path)?;
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
    }
    if *hasher.finalize().as_bytes() != expected_digest
        || rows != expected_rows
        || bytes != expected_bytes
    {
        return Err(OsvSnapshotError::Invalid("package index digest or totals"));
    }
    Ok(())
}

fn create_private_file(path: &Path) -> Result<File, OsvSnapshotError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(OsvSnapshotError::Io)
}

fn private_permissions(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o077 == 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

fn set_private_directory_permissions(path: &Path) -> Result<(), OsvSnapshotError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), OsvSnapshotError> {
    match OpenOptions::new().read(true).open(path) {
        Ok(directory) => directory.sync_all().map_err(OsvSnapshotError::Io),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::Unsupported | io::ErrorKind::PermissionDenied
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(OsvSnapshotError::Io(error)),
    }
}

fn snapshot_storage_bytes(root: &Path) -> Result<u64, OsvSnapshotError> {
    fn visit(
        directory: &Path,
        depth: usize,
        entries: &mut usize,
        total: &mut u64,
    ) -> Result<(), OsvSnapshotError> {
        if depth > 3 {
            return Err(OsvSnapshotError::Invalid("snapshot storage nesting"));
        }
        for item in fs::read_dir(directory)? {
            let item = item?;
            *entries = (*entries)
                .checked_add(1)
                .filter(|count| *count <= MAX_OSV_STORAGE_ENTRIES)
                .ok_or(OsvSnapshotError::Limit(OsvSnapshotLimit::StoredBytes))?;
            let file_type = item.file_type()?;
            if file_type.is_symlink() {
                return Err(OsvSnapshotError::Invalid("snapshot storage symlink"));
            }
            let path = item.path();
            if file_type.is_dir() {
                visit(&path, depth + 1, entries, total)?;
            } else if file_type.is_file() {
                let metadata = fs::symlink_metadata(&path)?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(OsvSnapshotError::Invalid("snapshot storage file"));
                }
                *total = (*total)
                    .checked_add(metadata.len())
                    .ok_or(OsvSnapshotError::Limit(OsvSnapshotLimit::StoredBytes))?;
            } else {
                return Err(OsvSnapshotError::Invalid("snapshot storage special file"));
            }
        }
        Ok(())
    }

    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(OsvSnapshotError::Invalid("snapshot root directory"));
    }
    let mut entries = 0;
    let mut total = 0;
    visit(root, 0, &mut entries, &mut total)?;
    Ok(total)
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
