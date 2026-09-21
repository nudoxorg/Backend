//! Bounded source acquisition and versioned source deltas.
//!
//! The registry modules decode ecosystem wire formats.  This module owns the
//! protocol that follows decoding: canonical source identities, coalesced
//! effects, durable publication fences, negative facts, retry/breaker state,
//! and the immutable delta applied to a source snapshot.
#![allow(clippy::module_name_repetitions)]

use backend_execution::{
    Cancellation, OutputAdmission, OutputValidationError, ResultCoverage, UntrustedOutputClaim,
    WorkInterner, WorkKey, acquisition_work_key,
};
use blake3::Hasher;
use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const CHUNK_BYTES: usize = 64 * 1024;
const ID_BYTES: usize = 32;
static TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn frame(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn digest(domain: &[u8], fields: &[&[u8]]) -> [u8; ID_BYTES] {
    let mut hasher = Hasher::new();
    hasher.update(b"backend.acquisition.identity.v1\0");
    hasher.update(&(domain.len() as u64).to_be_bytes());
    hasher.update(domain);
    for field in fields {
        hasher.update(&((*field).len() as u64).to_be_bytes());
        hasher.update(field);
    }
    *hasher.finalize().as_bytes()
}

macro_rules! identity {
    ($name:ident, $domain:literal) => {
        #[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name([u8; ID_BYTES]);

        impl $name {
            fn derive(fields: &[&[u8]]) -> Self {
                Self(digest($domain, fields))
            }

            /// Returns the fixed-width canonical identity bytes.
            #[must_use]
            pub const fn to_bytes(self) -> [u8; ID_BYTES] {
                self.0
            }

            /// Borrows the fixed-width canonical identity bytes.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; ID_BYTES] {
                &self.0
            }

            /// Encodes the identity for a journal or wire record.
            #[must_use]
            pub const fn encode(self) -> [u8; ID_BYTES] {
                self.0
            }

            pub(crate) const fn from_encoded(bytes: [u8; ID_BYTES]) -> Self {
                Self(bytes)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "("))?;
                for byte in self.0 {
                    write!(f, "{byte:02x}")?;
                }
                f.write_str(")")
            }
        }
    };
}

identity!(ReleaseClaimId, b"release-claim");
identity!(RawArchiveObjectId, b"raw-archive-object");
identity!(TreeManifestId, b"tree-manifest");
identity!(SourceSnapshotId, b"source-snapshot");
identity!(AcquisitionDeltaId, b"acquisition-delta");
identity!(AcquisitionReceiptId, b"acquisition-receipt");
identity!(PublicationRootId, b"publication-root");

/// A canonical release claim, independent of mutable release facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseClaim {
    /// Source authority identity.
    pub source: [u8; ID_BYTES],
    /// Canonical package coordinate.
    pub coordinate: Arc<str>,
    /// Canonical release version.
    pub version: Arc<str>,
    /// Claimed archive identity.
    pub archive: RawArchiveObjectId,
    /// Stable claim identity.
    pub id: ReleaseClaimId,
}

impl ReleaseClaim {
    /// Admits one claim after canonical coordinate/version validation.
    pub fn new(
        source: [u8; ID_BYTES],
        coordinate: impl Into<String>,
        version: impl Into<String>,
        archive: RawArchiveObjectId,
    ) -> Result<Self, IdentityError> {
        let coordinate = canonical_text(coordinate.into())?;
        let version = canonical_text(version.into())?;
        let coordinate: Arc<str> = Arc::from(coordinate);
        let version: Arc<str> = Arc::from(version);
        let id = ReleaseClaimId::derive(&[
            &source,
            coordinate.as_bytes(),
            version.as_bytes(),
            archive.as_bytes(),
        ]);
        Ok(Self {
            source,
            coordinate,
            version,
            archive,
            id,
        })
    }
}

fn canonical_text(value: String) -> Result<String, IdentityError> {
    if value.is_empty()
        || value.trim() != value
        || value
            .bytes()
            .any(|byte| byte == 0 || byte == b'\n' || byte == b'\r')
    {
        return Err(IdentityError::NonCanonicalText);
    }
    Ok(value)
}

/// Identity admission failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityError {
    /// Text contained whitespace/control bytes or was empty.
    NonCanonicalText,
    /// A canonical collection contained a duplicate key.
    Duplicate,
    /// A canonical collection was not sorted by its key.
    Unsorted,
    /// A supplied payload was too large for the configured bound.
    Bounds,
    /// A stream did not match its declared extent.
    LengthMismatch,
    /// The filesystem object was not a regular file.
    NotAFile,
    /// Filesystem traversal failed.
    Io,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "identity admission failed: {self:?}")
    }
}
impl std::error::Error for IdentityError {}

impl RawArchiveObjectId {
    /// Hashes a bounded stream without retaining the archive in memory.
    pub fn from_reader(reader: &mut impl Read, maximum: u64) -> Result<Self, IdentityError> {
        let mut hasher = Hasher::new();
        hasher.update(b"backend.acquisition.archive.v1\0");
        let mut buffer = [0_u8; CHUNK_BYTES];
        let mut length = 0_u64;
        loop {
            let read = reader.read(&mut buffer).map_err(|_| IdentityError::Io)?;
            if read == 0 {
                break;
            }
            length = length
                .checked_add(read as u64)
                .ok_or(IdentityError::Bounds)?;
            if length > maximum {
                return Err(IdentityError::Bounds);
            }
            hasher.update(&buffer[..read]);
        }
        let mut payload = Vec::with_capacity(ID_BYTES + 8);
        payload.extend_from_slice(&length.to_be_bytes());
        payload.extend_from_slice(hasher.finalize().as_bytes());
        Ok(Self::derive(&[&payload]))
    }

    /// Hashes bytes using the same streaming identity grammar.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut cursor = io::Cursor::new(bytes);
        Self::from_reader(&mut cursor, bytes.len() as u64)
            .unwrap_or_else(|_| Self::derive(&[bytes]))
    }
}

/// One canonical path/object row in a tree manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestEntry {
    /// Slash-separated relative path.
    pub path: Arc<str>,
    /// Content identity of the file bytes.
    pub object: RawArchiveObjectId,
    /// Portable mode bits.
    pub mode: u32,
}

/// Immutable sorted tree manifest shared by local directories and archives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeManifest {
    id: TreeManifestId,
    entries: Arc<[ManifestEntry]>,
}

impl TreeManifest {
    /// Builds a canonical manifest. Entries may arrive in arbitrary order;
    /// the admitted representation is always sorted and duplicate-free.
    pub fn new(mut entries: Vec<ManifestEntry>) -> Result<Self, IdentityError> {
        for entry in &entries {
            canonical_text(entry.path.to_string())?;
        }
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        if entries
            .windows(2)
            .any(|window| window[0].path == window[1].path)
        {
            return Err(IdentityError::Duplicate);
        }
        let mut canonical = Vec::new();
        for entry in &entries {
            frame(&mut canonical, entry.path.as_bytes());
            canonical.extend_from_slice(&entry.object.to_bytes());
            canonical.extend_from_slice(&entry.mode.to_be_bytes());
        }
        let id = TreeManifestId::derive(&[&canonical]);
        Ok(Self {
            id,
            entries: Arc::from(entries),
        })
    }

    /// Returns the immutable manifest identity.
    #[must_use]
    pub const fn id(&self) -> TreeManifestId {
        self.id
    }

    /// Returns entries in canonical path order.
    #[must_use]
    pub fn entries(&self) -> &[ManifestEntry] {
        &self.entries
    }

    /// Returns the canonical sorted encoding committed by `id`.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut canonical = Vec::new();
        for entry in self.entries.iter() {
            frame(&mut canonical, entry.path.as_bytes());
            canonical.extend_from_slice(&entry.object.to_bytes());
            canonical.extend_from_slice(&entry.mode.to_be_bytes());
        }
        canonical
    }

    /// Builds the same canonical representation used by an archive source.
    pub fn from_directory(
        path: impl AsRef<Path>,
        maximum_bytes: u64,
    ) -> Result<Self, IdentityError> {
        let mut entries = Vec::new();
        let mut total = 0_u64;
        collect_directory(
            path.as_ref(),
            path.as_ref(),
            maximum_bytes,
            &mut total,
            &mut entries,
        )?;
        Self::new(entries)
    }
}

fn collect_directory(
    root: &Path,
    path: &Path,
    maximum: u64,
    total: &mut u64,
    entries: &mut Vec<ManifestEntry>,
) -> Result<(), IdentityError> {
    for item in fs::read_dir(path).map_err(|_| IdentityError::Io)? {
        let item = item.map_err(|_| IdentityError::Io)?;
        let file_type = item.file_type().map_err(|_| IdentityError::Io)?;
        let item_path = item.path();
        if file_type.is_dir() {
            collect_directory(root, &item_path, maximum, total, entries)?;
        } else if file_type.is_file() {
            let relative = item_path
                .strip_prefix(root)
                .map_err(|_| IdentityError::Io)?
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            let metadata = item.metadata().map_err(|_| IdentityError::Io)?;
            *total = total
                .checked_add(metadata.len())
                .ok_or(IdentityError::Bounds)?;
            if *total > maximum {
                return Err(IdentityError::Bounds);
            }
            let mut file = File::open(item_path).map_err(|_| IdentityError::Io)?;
            let object = RawArchiveObjectId::from_reader(&mut file, maximum)?;
            entries.push(ManifestEntry {
                path: Arc::from(relative),
                object,
                mode: 0,
            });
        } else {
            return Err(IdentityError::NotAFile);
        }
    }
    Ok(())
}

/// Immutable source snapshot that carries a manifest and release claims.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSnapshot {
    source: [u8; ID_BYTES],
    cursor: [u8; ID_BYTES],
    policy_epoch: u64,
    manifest: Arc<TreeManifest>,
    claims: Arc<[ReleaseClaimId]>,
    id: SourceSnapshotId,
}

impl SourceSnapshot {
    /// Creates a snapshot with canonical claim ordering.
    pub fn new(
        source: [u8; ID_BYTES],
        cursor: [u8; ID_BYTES],
        policy_epoch: u64,
        manifest: Arc<TreeManifest>,
        mut claims: Vec<ReleaseClaimId>,
    ) -> Result<Self, IdentityError> {
        claims.sort();
        if claims.windows(2).any(|window| window[0] == window[1]) {
            return Err(IdentityError::Duplicate);
        }
        let mut encoded = Vec::new();
        encoded.extend_from_slice(&source);
        encoded.extend_from_slice(&cursor);
        encoded.extend_from_slice(&policy_epoch.to_be_bytes());
        encoded.extend_from_slice(manifest.id().as_bytes());
        for claim in &claims {
            encoded.extend_from_slice(claim.as_bytes());
        }
        let id = SourceSnapshotId::derive(&[&encoded]);
        Ok(Self {
            source,
            cursor,
            policy_epoch,
            manifest,
            claims: Arc::from(claims),
            id,
        })
    }

    /// Returns the exact snapshot root.
    #[must_use]
    pub const fn id(&self) -> SourceSnapshotId {
        self.id
    }
    /// Returns the source authority identity.
    #[must_use]
    pub const fn source(&self) -> [u8; ID_BYTES] {
        self.source
    }
    /// Returns the source cursor/snapshot token.
    #[must_use]
    pub const fn cursor(&self) -> [u8; ID_BYTES] {
        self.cursor
    }
    /// Returns the policy epoch bound into this snapshot.
    #[must_use]
    pub const fn policy_epoch(&self) -> u64 {
        self.policy_epoch
    }
    /// Returns the immutable tree manifest.
    #[must_use]
    pub fn manifest(&self) -> &TreeManifest {
        &self.manifest
    }
    /// Returns release claims in canonical order.
    #[must_use]
    pub fn claims(&self) -> &[ReleaseClaimId] {
        &self.claims
    }

    /// Returns the canonical snapshot preimage used to derive `id`.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut encoded = Vec::new();
        encoded.extend_from_slice(&self.source);
        encoded.extend_from_slice(&self.cursor);
        encoded.extend_from_slice(&self.policy_epoch.to_be_bytes());
        encoded.extend_from_slice(self.manifest.id().as_bytes());
        for claim in self.claims.iter() {
            encoded.extend_from_slice(claim.as_bytes());
        }
        encoded
    }
}

/// One before/after source path transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeltaChange {
    /// Canonical path key.
    pub path: Arc<str>,
    /// Object expected in the base snapshot, if present.
    pub before: Option<RawArchiveObjectId>,
    /// Object published in the target snapshot, if present.
    pub after: Option<RawArchiveObjectId>,
}

/// Error applying a versioned acquisition delta.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeltaError {
    /// Changes were duplicated or did not have a canonical path key.
    InvalidChanges(IdentityError),
    /// The caller supplied a source root other than the bound base root.
    StaleBase {
        expected: SourceSnapshotId,
        actual: SourceSnapshotId,
    },
    /// A before value did not match the base manifest.
    BeforeMismatch,
    /// The computed target root did not match the bound target root.
    TargetMismatch,
}

impl fmt::Display for DeltaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "acquisition delta error: {self:?}")
    }
}
impl std::error::Error for DeltaError {}

/// Immutable root-bound source delta.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcquisitionDelta {
    id: AcquisitionDeltaId,
    base: SourceSnapshotId,
    target: SourceSnapshotId,
    target_snapshot: Arc<SourceSnapshot>,
    changes: Arc<[DeltaChange]>,
}

impl AcquisitionDelta {
    /// Constructs a root-bound delta with canonical sorted changes.
    pub fn new(
        base: &SourceSnapshot,
        target: Arc<SourceSnapshot>,
        mut changes: Vec<DeltaChange>,
    ) -> Result<Self, DeltaError> {
        changes.sort_by(|left, right| left.path.cmp(&right.path));
        if changes
            .windows(2)
            .any(|window| window[0].path == window[1].path)
        {
            return Err(DeltaError::InvalidChanges(IdentityError::Duplicate));
        }
        let mut canonical = Vec::new();
        for change in &changes {
            canonical_text(change.path.to_string()).map_err(DeltaError::InvalidChanges)?;
            frame(&mut canonical, change.path.as_bytes());
            canonical
                .extend_from_slice(&change.before.map_or([0; 32], RawArchiveObjectId::to_bytes));
            canonical.push(u8::from(change.before.is_some()));
            canonical
                .extend_from_slice(&change.after.map_or([0; 32], RawArchiveObjectId::to_bytes));
            canonical.push(u8::from(change.after.is_some()));
        }
        let id =
            AcquisitionDeltaId::derive(&[base.id().as_bytes(), target.id().as_bytes(), &canonical]);
        Ok(Self {
            id,
            base: base.id(),
            target: target.id(),
            target_snapshot: target,
            changes: Arc::from(changes),
        })
    }

    /// Returns this delta's immutable identity.
    #[must_use]
    pub const fn id(&self) -> AcquisitionDeltaId {
        self.id
    }
    /// Returns the exact base root.
    #[must_use]
    pub const fn base(&self) -> SourceSnapshotId {
        self.base
    }
    /// Returns the exact target root.
    #[must_use]
    pub const fn target(&self) -> SourceSnapshotId {
        self.target
    }
    /// Returns canonical sorted changes.
    #[must_use]
    pub fn changes(&self) -> &[DeltaChange] {
        &self.changes
    }

    /// Returns the canonical sorted change encoding committed by `id`.
    #[must_use]
    pub fn canonical_changes(&self) -> Vec<u8> {
        let mut canonical = Vec::new();
        for change in self.changes.iter() {
            frame(&mut canonical, change.path.as_bytes());
            canonical
                .extend_from_slice(&change.before.map_or([0; 32], RawArchiveObjectId::to_bytes));
            canonical.push(u8::from(change.before.is_some()));
            canonical
                .extend_from_slice(&change.after.map_or([0; 32], RawArchiveObjectId::to_bytes));
            canonical.push(u8::from(change.after.is_some()));
        }
        canonical
    }

    /// Applies once, or returns the same immutable target for a repeated
    /// application. A stale base is a typed error and never mutates state.
    pub fn apply(&self, base: &SourceSnapshot) -> Result<Arc<SourceSnapshot>, DeltaError> {
        if base.id() == self.target {
            return Ok(Arc::clone(&self.target_snapshot));
        }
        if base.id() != self.base {
            return Err(DeltaError::StaleBase {
                expected: self.base,
                actual: base.id(),
            });
        }
        let mut entries = base.manifest.entries().to_vec();
        for change in self.changes.iter() {
            let index = entries.iter().position(|entry| entry.path == change.path);
            let actual = index.and_then(|index| Some(entries[index].object));
            if actual != change.before {
                return Err(DeltaError::BeforeMismatch);
            }
            match (index, change.after) {
                (Some(index), Some(after)) => entries[index].object = after,
                (Some(index), None) => {
                    entries.remove(index);
                }
                (None, Some(after)) => entries.push(ManifestEntry {
                    path: Arc::clone(&change.path),
                    object: after,
                    mode: 0,
                }),
                (None, None) => {}
            }
        }
        let manifest = Arc::new(TreeManifest::new(entries).map_err(DeltaError::InvalidChanges)?);
        let computed = SourceSnapshot::new(
            self.target_snapshot.source,
            self.target_snapshot.cursor,
            self.target_snapshot.policy_epoch,
            manifest,
            self.target_snapshot.claims.to_vec(),
        )
        .map_err(DeltaError::InvalidChanges)?;
        if computed.id() != self.target {
            return Err(DeltaError::TargetMismatch);
        }
        Ok(Arc::new(computed))
    }
}

/// Closed terminal/result algebra for every acquisition phase.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AcquisitionOutcome<T> {
    /// A verified object or published delta was returned.
    Hit(T),
    /// A signed/authoritative negative fact was returned.
    NegativeFact(NegativeFact),
    /// The caller should retry no earlier than the supplied time.
    RetryAt(RetryAt),
    /// The source circuit is open.
    CircuitOpen(CircuitOpen),
    /// The source could not be reached or completed within bounds.
    Unavailable(Unavailable),
    /// Policy or protocol rejected the request.
    Rejected(RejectReason),
    /// Persisted state or content failed integrity checks.
    Corrupt(CorruptReason),
    /// The caller cancelled its demand.
    Cancelled,
}

/// Why an acquisition was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectReason {
    /// The request is outside configured policy.
    Policy,
    /// The adapter returned malformed protocol data.
    Protocol,
    /// The request exceeded a configured bound.
    Bounds,
}
/// Why durable state or content was corrupt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorruptReason {
    /// Hash did not match the claimed content.
    Integrity,
    /// Journal or root record was malformed.
    Journal,
}
/// Retry timestamp and attempt ordinal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryAt {
    /// Earliest retry instant in monotonic milliseconds from the process epoch.
    pub at_millis: u64,
    /// Retry attempt used for backoff accounting.
    pub attempt: u32,
}
/// Circuit-open observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CircuitOpen {
    /// Time after which a half-open probe may be admitted.
    pub until_millis: u64,
}
/// Unavailability observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Unavailable {
    /// Stable source identity.
    pub source: [u8; ID_BYTES],
}

/// Typed negative fact retained by the negative cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NegativeFact {
    /// Exact negative fact kind.
    pub kind: NegativeFactKind,
    /// Authority that signed/observed the fact.
    pub authority: [u8; ID_BYTES],
    /// Proof digest from the source response.
    pub source_proof: [u8; ID_BYTES],
    /// Feed cursor or snapshot token at observation.
    pub cursor: [u8; ID_BYTES],
    /// Wall-clock observation timestamp.
    pub observed_at_millis: u64,
    /// Expiry timestamp.
    pub expires_at_millis: u64,
    /// Policy epoch under which this fact is valid.
    pub policy_epoch: u64,
}

/// Negative facts distinguish absence from transient failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NegativeFactKind {
    /// Source definitively has no matching coordinate.
    NotFound,
    /// Coordinate existed but was withdrawn.
    Yanked,
    /// Policy authority blocked the release for advisory reasons.
    AdvisoryBlocked,
    /// Source explicitly does not support the requested artifact.
    Unsupported,
}

impl NegativeFact {
    /// Returns whether the fact can be used at an observation time and epoch.
    #[must_use]
    pub const fn valid_at(self, now_millis: u64, policy_epoch: u64) -> bool {
        self.expires_at_millis > now_millis && self.policy_epoch == policy_epoch
    }
}

/// Bounded negative cache keyed by source and coordinate identity.
#[derive(Clone, Debug)]
pub struct NegativeCache {
    entries: Arc<Mutex<BTreeMap<[u8; ID_BYTES], NegativeFact>>>,
    capacity: usize,
}

impl NegativeCache {
    /// Creates a cache with a fixed entry bound.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Arc::new(Mutex::new(BTreeMap::new())),
            capacity,
        }
    }
    /// Records one fact, evicting the lexicographically oldest key only when
    /// the configured bound is reached.
    pub fn record(&self, key: [u8; ID_BYTES], fact: NegativeFact) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.len() >= self.capacity && !entries.contains_key(&key) {
            if let Some(oldest) = entries.keys().next().copied() {
                entries.remove(&oldest);
            }
        }
        if self.capacity != 0 {
            entries.insert(key, fact);
        }
    }
    /// Gets a currently valid fact. Expired facts are removed.
    pub fn get(
        &self,
        key: [u8; ID_BYTES],
        now_millis: u64,
        policy_epoch: u64,
    ) -> Option<NegativeFact> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fact = entries.get(&key).copied();
        if fact.is_some_and(|fact| !fact.valid_at(now_millis, policy_epoch)) {
            entries.remove(&key);
            None
        } else {
            fact
        }
    }
}

/// Error class used by retry and breaker policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryClass {
    /// Network timeout, 5xx, or rate limit.
    Retryable,
    /// Permanent source absence or explicit rejection.
    Permanent,
    /// Malformed protocol response.
    Protocol,
    /// Local policy rejection.
    Policy,
    /// Integrity failure; never converted into a negative fact.
    Integrity,
}

/// Error classification retained with an attempt journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptFailure {
    /// Request timed out.
    Timeout,
    /// Server requested a retry, optionally with Retry-After milliseconds.
    RateLimited { retry_after_millis: Option<u64> },
    /// Source returned an unavailable response.
    Unavailable,
    /// Source returned an explicit negative fact.
    NotFound,
    /// Response was malformed.
    Malformed,
    /// Policy denied the request.
    Policy,
    /// Integrity verification failed.
    Integrity,
}

impl AttemptFailure {
    /// Classifies a failure without collapsing transient errors into absence.
    #[must_use]
    pub const fn class(self) -> RetryClass {
        match self {
            Self::Timeout | Self::RateLimited { .. } | Self::Unavailable => RetryClass::Retryable,
            Self::NotFound => RetryClass::Permanent,
            Self::Malformed => RetryClass::Protocol,
            Self::Policy => RetryClass::Policy,
            Self::Integrity => RetryClass::Integrity,
        }
    }

    /// Converts only a definitive source absence into a negative fact kind.
    /// Every transient, protocol, policy, or integrity failure returns `None`.
    #[must_use]
    pub const fn negative_kind(self) -> Option<NegativeFactKind> {
        match self {
            Self::NotFound => Some(NegativeFactKind::NotFound),
            Self::Timeout
            | Self::RateLimited { .. }
            | Self::Unavailable
            | Self::Malformed
            | Self::Policy
            | Self::Integrity => None,
        }
    }
}

/// Full-jitter exponential retry policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    /// Initial backoff cap.
    pub base: Duration,
    /// Maximum backoff cap.
    pub maximum: Duration,
    /// Maximum retry attempts.
    pub max_attempts: u32,
    seed: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base: Duration::from_millis(50),
            maximum: Duration::from_secs(30),
            max_attempts: 5,
            seed: now_millis() ^ (std::process::id() as u64),
        }
    }
}

impl RetryPolicy {
    /// Creates a deterministic policy useful for replay tests.
    #[must_use]
    pub const fn with_seed(
        base: Duration,
        maximum: Duration,
        max_attempts: u32,
        seed: u64,
    ) -> Self {
        Self {
            base,
            maximum,
            max_attempts,
            seed,
        }
    }

    /// Computes a full-jitter delay and clamps a server Retry-After value.
    #[must_use]
    pub fn delay(self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        let exponent = attempt.min(31);
        let cap = self
            .base
            .checked_mul(1_u32 << exponent)
            .unwrap_or(self.maximum)
            .min(self.maximum);
        let jitter = if cap.is_zero() {
            Duration::ZERO
        } else {
            let mut value = self.seed ^ u64::from(attempt).wrapping_mul(0x9e37_79b9_7f4a_7c15);
            value ^= value << 7;
            value ^= value >> 9;
            let nanos = cap.as_nanos().min(u128::from(u64::MAX)) as u64;
            Duration::from_nanos(value % nanos.saturating_add(1))
        };
        retry_after.map_or(jitter, |requested| requested.min(self.maximum).max(jitter))
    }

    /// Returns whether an attempt may be retried.
    #[must_use]
    pub const fn retryable(self, attempt: u32, failure: AttemptFailure) -> bool {
        attempt < self.max_attempts && matches!(failure.class(), RetryClass::Retryable)
    }
}

/// Persisted circuit state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CircuitState {
    /// Requests flow normally.
    Closed,
    /// Requests fail fast until `until_millis`.
    Open { until_millis: u64 },
    /// Exactly one probe may run.
    HalfOpen,
}

#[derive(Clone, Copy, Debug)]
struct CircuitPersisted {
    state: CircuitState,
    failures: u32,
    probe: bool,
}

/// Persistent breaker with one half-open probe.
#[derive(Clone, Debug)]
pub struct CircuitBreaker {
    state: Arc<Mutex<CircuitPersisted>>,
    threshold: u32,
    cool_down: Duration,
    path: Option<Arc<PathBuf>>,
}

impl CircuitBreaker {
    /// Creates a process-local circuit.
    #[must_use]
    pub fn new(threshold: u32, cool_down: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(CircuitPersisted {
                state: CircuitState::Closed,
                failures: 0,
                probe: false,
            })),
            threshold: threshold.max(1),
            cool_down,
            path: None,
        }
    }

    /// Opens a breaker backed by a small durable state file.
    pub fn open_persisted(
        path: impl Into<PathBuf>,
        threshold: u32,
        cool_down: Duration,
    ) -> io::Result<Self> {
        let path = path.into();
        let breaker = Self {
            state: Arc::new(Mutex::new(CircuitPersisted {
                state: CircuitState::Closed,
                failures: 0,
                probe: false,
            })),
            threshold: threshold.max(1),
            cool_down,
            path: Some(Arc::new(path)),
        };
        breaker.load()?;
        Ok(breaker)
    }

    fn load(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let Ok(text) = fs::read_to_string(path.as_ref()) else {
            return Ok(());
        };
        let mut fields = text.split(':');
        let state = match fields.next() {
            Some("closed") => CircuitState::Closed,
            Some("open") => CircuitState::Open {
                until_millis: fields
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0),
            },
            Some("half") => CircuitState::HalfOpen,
            _ => return Ok(()),
        };
        let failures = fields
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let mut current = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        current.state = state;
        current.failures = failures;
        current.probe = false;
        Ok(())
    }

    fn persist(&self, value: CircuitPersisted) {
        let Some(path) = &self.path else { return };
        let text = match value.state {
            CircuitState::Closed => format!("closed:{}", value.failures),
            CircuitState::Open { until_millis } => {
                format!("open:{until_millis}:{}", value.failures)
            }
            CircuitState::HalfOpen => format!("half:{}", value.failures),
        };
        let _ = fs::write(path.as_ref(), text);
    }

    /// Admits a request or returns a typed open observation. One caller owns
    /// the half-open probe; all others receive `false`.
    pub fn allow(&self, now_millis: u64) -> Result<CircuitPermit, CircuitOpen> {
        let mut value = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match value.state {
            CircuitState::Closed => Ok(CircuitPermit {
                state: Arc::clone(&self.state),
                path: self.path.clone(),
                probe: false,
            }),
            CircuitState::Open { until_millis } if now_millis < until_millis => {
                Err(CircuitOpen { until_millis })
            }
            CircuitState::Open { .. } => {
                if value.probe {
                    return Err(CircuitOpen {
                        until_millis: now_millis.saturating_add(self.cool_down.as_millis() as u64),
                    });
                }
                value.state = CircuitState::HalfOpen;
                value.probe = true;
                self.persist(*value);
                Ok(CircuitPermit {
                    state: Arc::clone(&self.state),
                    path: self.path.clone(),
                    probe: true,
                })
            }
            CircuitState::HalfOpen => Err(CircuitOpen {
                until_millis: now_millis.saturating_add(self.cool_down.as_millis() as u64),
            }),
        }
    }

    /// Records a successful request and closes the breaker.
    pub fn success(&self) {
        let mut value = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        value.state = CircuitState::Closed;
        value.failures = 0;
        value.probe = false;
        self.persist(*value);
    }

    /// Records a retryable failure and opens once the threshold is reached.
    pub fn failure(&self, now_millis: u64) {
        let mut value = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        value.failures = value.failures.saturating_add(1);
        value.probe = false;
        if value.failures >= self.threshold {
            value.state = CircuitState::Open {
                until_millis: now_millis.saturating_add(self.cool_down.as_millis() as u64),
            };
        }
        self.persist(*value);
    }

    /// Returns a compact state snapshot.
    #[must_use]
    pub fn state(&self) -> CircuitState {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .state
    }
}

/// Lease that releases a single half-open probe on drop if still owned.
pub struct CircuitPermit {
    state: Arc<Mutex<CircuitPersisted>>,
    path: Option<Arc<PathBuf>>,
    probe: bool,
}

impl Drop for CircuitPermit {
    fn drop(&mut self) {
        if self.probe {
            let mut value = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            value.probe = false;
            if value.state == CircuitState::HalfOpen {
                value.state = CircuitState::Open {
                    until_millis: now_millis(),
                };
            }
            if let Some(path) = &self.path {
                let text = match value.state {
                    CircuitState::Closed => format!("closed:{}", value.failures),
                    CircuitState::Open { until_millis } => {
                        format!("open:{until_millis}:{}", value.failures)
                    }
                    CircuitState::HalfOpen => format!("half:{}", value.failures),
                };
                let _ = fs::write(path.as_ref(), text);
            }
        }
    }
}

/// Failure acquiring a bounded byte permit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermitError {
    /// Requested bytes exceed the pool's total bound.
    TooLarge,
    /// An arithmetic bound was exceeded.
    Overflow,
}

/// A blocking byte budget used by archive admission.
#[derive(Debug)]
pub struct BytePermitPool {
    state: Mutex<u64>,
    wake: Condvar,
    capacity: u64,
}

impl BytePermitPool {
    /// Creates a pool with a fixed maximum of simultaneously admitted bytes.
    #[must_use]
    pub fn new(capacity: u64) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(capacity),
            wake: Condvar::new(),
            capacity,
        })
    }

    /// Acquires `bytes`, blocking until the budget is available.
    pub fn acquire(self: &Arc<Self>, bytes: u64) -> Result<BytePermit, PermitError> {
        if bytes > self.capacity {
            return Err(PermitError::TooLarge);
        }
        let mut available = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *available < bytes {
            available = self
                .wake
                .wait(available)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *available -= bytes;
        Ok(BytePermit {
            pool: Arc::clone(self),
            bytes,
        })
    }

    /// Returns available bytes.
    #[must_use]
    pub fn available(&self) -> u64 {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Affine byte budget reservation.
pub struct BytePermit {
    pool: Arc<BytePermitPool>,
    bytes: u64,
}

impl Drop for BytePermit {
    fn drop(&mut self) {
        let mut available = self
            .pool
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *available = available.saturating_add(self.bytes).min(self.pool.capacity);
        self.pool.wake.notify_all();
    }
}

/// Bounded count semaphore used for metadata/object effects.
#[derive(Debug)]
pub struct PermitPool {
    state: Mutex<usize>,
    wake: Condvar,
    capacity: usize,
}

impl PermitPool {
    /// Creates a count-bounded pool.
    #[must_use]
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(capacity),
            wake: Condvar::new(),
            capacity,
        })
    }
    /// Acquires one slot.
    pub fn acquire(self: &Arc<Self>) -> Permit {
        let mut available = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *available == 0 {
            available = self
                .wake
                .wait(available)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *available -= 1;
        Permit {
            pool: Arc::clone(self),
        }
    }
    /// Returns the number of currently available slots.
    #[must_use]
    pub fn available(&self) -> usize {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Affine count permit.
pub struct Permit {
    pool: Arc<PermitPool>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut available = self
            .pool
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *available = available.saturating_add(1).min(self.pool.capacity);
        self.pool.wake.notify_one();
    }
}

#[derive(Debug, Default)]
struct TelemetryCounters {
    leaders: u64,
    followers: u64,
    reused_bytes: u64,
    downloaded_bytes: u64,
    retries: u64,
    breaker_transitions: u64,
    typed_rejects: u64,
    delta_rows: u64,
}

/// Bounded acquisition telemetry. Counters saturate and the snapshot is a
/// fixed-size value suitable for export without retaining labels/queues.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AcquisitionTelemetry {
    /// Number of leader effects.
    pub leaders: u64,
    /// Number of coalesced followers.
    pub followers: u64,
    /// Bytes served from an admitted object.
    pub reused_bytes: u64,
    /// Bytes read from a source stream.
    pub downloaded_bytes: u64,
    /// Retry attempts.
    pub retries: u64,
    /// Breaker state transitions.
    pub breaker_transitions: u64,
    /// Typed rejects.
    pub typed_rejects: u64,
    /// Published delta rows.
    pub delta_rows: u64,
}

/// Shared telemetry handle.
#[derive(Clone, Default, Debug)]
pub struct Telemetry {
    counters: Arc<Mutex<TelemetryCounters>>,
}

impl Telemetry {
    fn update(&self, update: impl FnOnce(&mut TelemetryCounters)) {
        let mut counters = self
            .counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        update(&mut counters);
    }
    /// Returns the current bounded snapshot.
    #[must_use]
    pub fn snapshot(&self) -> AcquisitionTelemetry {
        let counters = self
            .counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        AcquisitionTelemetry {
            leaders: counters.leaders,
            followers: counters.followers,
            reused_bytes: counters.reused_bytes,
            downloaded_bytes: counters.downloaded_bytes,
            retries: counters.retries,
            breaker_transitions: counters.breaker_transitions,
            typed_rejects: counters.typed_rejects,
            delta_rows: counters.delta_rows,
        }
    }
}

/// A durable lease/fence key. The token is generated privately and is needed
/// for release, so one process can never delete another process's lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcquisitionLease {
    /// Effect key being owned.
    pub key: WorkKey,
    /// Private owner token.
    pub token: [u8; ID_BYTES],
    /// Lease expiry in wall-clock milliseconds.
    pub expires_at_millis: u64,
}

/// Outcome of cross-process first-writer-wins object admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CasAdmission {
    /// This writer created the object.
    Winner,
    /// Another writer won; the caller may use the existing object.
    Existing,
}

/// A private temporary artifact whose drop only removes its own name.
pub struct PrivateTemp {
    path: PathBuf,
    file: Option<File>,
}

impl fmt::Debug for PrivateTemp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrivateTemp")
            .field("path", &self.path)
            .finish()
    }
}

impl PrivateTemp {
    /// Returns the private temporary path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Writes bytes through the private descriptor.
    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("temp closed"))?
            .write_all(bytes)
    }
    /// Flushes and closes the temporary descriptor.
    pub fn sync_close(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
            file.sync_all()?;
        }
        Ok(())
    }
}

impl Drop for PrivateTemp {
    fn drop(&mut self) {
        let _ = self.file.take();
        let _ = fs::remove_file(&self.path);
    }
}

/// Durable effect ownership and publication fences.
#[derive(Clone, Debug)]
pub struct LeaseStore {
    root: Arc<PathBuf>,
    nonce: Arc<Mutex<u64>>,
}

impl LeaseStore {
    /// Opens/creates a private acquisition root.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("leases"))?;
        fs::create_dir_all(root.join("objects"))?;
        fs::create_dir_all(root.join("temps"))?;
        Ok(Self {
            root: Arc::new(root),
            nonce: Arc::new(Mutex::new(now_millis() ^ u64::from(std::process::id()))),
        })
    }

    fn next_token(&self) -> [u8; ID_BYTES] {
        let mut nonce = self
            .nonce
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *nonce = nonce.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut hasher = Hasher::new();
        hasher.update(b"backend.acquisition.fence.v1\0");
        hasher.update(&nonce.to_be_bytes());
        hasher.update(&TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed).to_be_bytes());
        hasher.update(&std::process::id().to_be_bytes());
        *hasher.finalize().as_bytes()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Acquires a durable lease using create-new first-writer admission.
    pub fn acquire(&self, key: WorkKey, ttl: Duration) -> io::Result<Option<LeaseGuard>> {
        let token = self.next_token();
        let path = self.root.join("leases").join(Self::hex(key.as_bytes()));
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let expired = fs::read(&path).ok().and_then(|bytes| {
                    if bytes.len() < ID_BYTES + 8 {
                        return Some(true);
                    }
                    let mut expiry = [0_u8; 8];
                    expiry.copy_from_slice(&bytes[ID_BYTES..ID_BYTES + 8]);
                    Some(u64::from_be_bytes(expiry) <= now_millis())
                }) == Some(true);
                if expired {
                    let _ = fs::remove_file(&path);
                    match OpenOptions::new().write(true).create_new(true).open(&path) {
                        Ok(file) => file,
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                            return Ok(None);
                        }
                        Err(error) => return Err(error),
                    }
                } else {
                    return Ok(None);
                }
            }
            Err(error) => return Err(error),
        };
        let expires = now_millis().saturating_add(ttl.as_millis() as u64);
        file.write_all(&token)?;
        file.write_all(&expires.to_be_bytes())?;
        file.sync_all()?;
        Ok(Some(LeaseGuard {
            store: self.clone(),
            lease: AcquisitionLease {
                key,
                token,
                expires_at_millis: expires,
            },
            path,
        }))
    }

    /// Allocates a private randomised temporary name.
    pub fn temp(&self, suffix: &str) -> io::Result<PrivateTemp> {
        let token = self.next_token();
        let path = self
            .root
            .join("temps")
            .join(format!("{}.{}", Self::hex(&token), suffix));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(PrivateTemp {
            path,
            file: Some(file),
        })
    }

    /// Performs first-writer-wins CAS admission of one complete temp object.
    pub fn cas_admit(
        &self,
        temp: &mut PrivateTemp,
        object: RawArchiveObjectId,
    ) -> io::Result<CasAdmission> {
        temp.sync_close()?;
        let target = self.root.join("objects").join(Self::hex(object.as_bytes()));
        match fs::hard_link(temp.path(), &target) {
            Ok(()) => Ok(CasAdmission::Winner),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Ok(CasAdmission::Existing)
            }
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                let mut source = File::open(temp.path())?;
                let destination = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target);
                match destination {
                    Ok(mut destination) => {
                        io::copy(&mut source, &mut destination)?;
                        destination.sync_all()?;
                        Ok(CasAdmission::Winner)
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        Ok(CasAdmission::Existing)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Returns the durable object path after CAS admission.
    #[must_use]
    pub fn object_path(&self, object: RawArchiveObjectId) -> PathBuf {
        self.root.join("objects").join(Self::hex(object.as_bytes()))
    }

    /// Opens the atomic publication pointer.
    pub fn publisher(&self) -> RootPublisher {
        RootPublisher {
            store: self.clone(),
        }
    }
}

/// Affine durable lease guard.
pub struct LeaseGuard {
    store: LeaseStore,
    lease: AcquisitionLease,
    path: PathBuf,
}

impl LeaseGuard {
    /// Returns this lease's fence.
    #[must_use]
    pub const fn lease(&self) -> AcquisitionLease {
        self.lease
    }
    /// Returns whether this lease is still the file owner.
    #[must_use]
    pub fn owns(&self) -> bool {
        fs::read(&self.path).is_ok_and(|bytes| bytes.starts_with(&self.lease.token))
    }

    /// Extends a lease only while its fence still owns the durable record.
    pub fn renew(&mut self, ttl: Duration) -> io::Result<bool> {
        if !self.owns() {
            return Ok(false);
        }
        let expires = now_millis().saturating_add(ttl.as_millis() as u64);
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.path)?;
        file.write_all(&self.lease.token)?;
        file.write_all(&expires.to_be_bytes())?;
        file.sync_all()?;
        self.lease.expires_at_millis = expires;
        Ok(true)
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if self.owns() {
            let _ = fs::remove_file(&self.path);
        }
        let _ = &self.store;
    }
}

/// Atomic old-or-new root pointer publication.
#[derive(Clone, Debug)]
pub struct RootPublisher {
    store: LeaseStore,
}

impl RootPublisher {
    /// Publishes one root pointer by fsync then rename. Recovery can observe
    /// only the previous complete pointer or this complete new pointer.
    pub fn publish(&self, root: PublicationRootId) -> io::Result<()> {
        let mut temp = self.store.temp("root")?;
        temp.write_all(root.as_bytes())?;
        temp.sync_close()?;
        let pointer = self.store.root.join("root.pointer");
        fs::rename(temp.path(), &pointer)?;
        Ok(())
    }
    /// Reads the selected root pointer.
    pub fn read(&self) -> io::Result<Option<PublicationRootId>> {
        match fs::read(self.store.root.join("root.pointer")) {
            Ok(bytes) if bytes.len() == ID_BYTES => {
                let mut root = [0; ID_BYTES];
                root.copy_from_slice(&bytes);
                Ok(Some(PublicationRootId::from_encoded(root)))
            }
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid root pointer",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// Exact key for one acquisition effect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcquisitionRequest {
    /// Source endpoint identity (credentials excluded).
    pub source: [u8; ID_BYTES],
    /// Canonical package coordinate.
    pub coordinate: Arc<str>,
    /// Expected archive identity.
    pub artifact: RawArchiveObjectId,
    /// Adapter protocol schema version.
    pub schema: u16,
    /// Policy epoch bound to this request.
    pub policy_epoch: u64,
}

impl AcquisitionRequest {
    /// Admits one exact request.
    pub fn new(
        source: [u8; ID_BYTES],
        coordinate: impl Into<String>,
        artifact: RawArchiveObjectId,
        schema: u16,
        policy_epoch: u64,
    ) -> Result<Self, IdentityError> {
        Ok(Self {
            source,
            coordinate: Arc::from(canonical_text(coordinate.into())?),
            artifact,
            schema,
            policy_epoch,
        })
    }

    /// Returns the exact process-local interner key.
    #[must_use]
    pub fn work_key(&self) -> WorkKey {
        acquisition_work_key(
            self.source,
            self.coordinate.as_bytes(),
            self.artifact.to_bytes(),
            self.schema,
            self.policy_epoch,
        )
    }
}

/// Resolve phase of the acquisition typestate machine.
#[derive(Clone, Debug)]
pub struct Resolve {
    request: AcquisitionRequest,
}

impl Resolve {
    /// Starts resolution for one request.
    #[must_use]
    pub const fn new(request: AcquisitionRequest) -> Self {
        Self { request }
    }
    /// Returns the request identity.
    #[must_use]
    pub const fn request(&self) -> &AcquisitionRequest {
        &self.request
    }
    /// Admits decoded metadata supplied by a registry adapter.
    pub fn metadata(self, record: MetadataRecord) -> Result<Metadata, AcquisitionOutcome<()>> {
        if record.claim.source != self.request.source
            || record.claim.archive != self.request.artifact
            || record.claim.coordinate.as_ref() != self.request.coordinate.as_ref()
        {
            return Err(AcquisitionOutcome::Rejected(RejectReason::Protocol));
        }
        Ok(Metadata {
            request: self.request,
            record,
        })
    }
}

/// Adapter-decoded metadata handoff. It contains no acquisition ownership.
#[derive(Clone, Debug)]
pub struct MetadataRecord {
    /// Authenticated release claim.
    pub claim: ReleaseClaim,
    /// Declared archive length.
    pub length: u64,
    /// Source cursor/proof digest.
    pub source_proof: [u8; ID_BYTES],
}

/// Metadata phase.
#[derive(Clone, Debug)]
pub struct Metadata {
    request: AcquisitionRequest,
    record: MetadataRecord,
}

impl Metadata {
    /// Admits an object staged by the transport adapter.
    pub fn object(self, object: RawArchiveObjectId) -> Result<Object, AcquisitionOutcome<()>> {
        if object != self.record.claim.archive {
            return Err(AcquisitionOutcome::Corrupt(CorruptReason::Integrity));
        }
        Ok(Object {
            request: self.request,
            record: self.record,
            object,
        })
    }
}

/// Object phase.
#[derive(Clone, Debug)]
pub struct Object {
    request: AcquisitionRequest,
    record: MetadataRecord,
    object: RawArchiveObjectId,
}

impl Object {
    /// Verifies an object using its streamed content identity.
    pub fn verified(
        self,
        actual: RawArchiveObjectId,
    ) -> Result<VerifiedObject, AcquisitionOutcome<()>> {
        if actual != self.object || actual != self.request.artifact {
            return Err(AcquisitionOutcome::Corrupt(CorruptReason::Integrity));
        }
        Ok(VerifiedObject {
            request: self.request,
            record: self.record,
            object: actual,
        })
    }
}

/// VerifiedObject phase.
#[derive(Clone, Debug)]
pub struct VerifiedObject {
    request: AcquisitionRequest,
    record: MetadataRecord,
    object: RawArchiveObjectId,
}

impl VerifiedObject {
    /// Moves to policy evaluation. Policy is explicit and cannot be bypassed.
    pub fn policy(self, allowed: bool) -> Result<Policy, AcquisitionOutcome<()>> {
        if !allowed {
            return Err(AcquisitionOutcome::Rejected(RejectReason::Policy));
        }
        Ok(Policy { verified: self })
    }
    /// Returns the admitted archive identity.
    #[must_use]
    pub const fn object(&self) -> RawArchiveObjectId {
        self.object
    }
}

/// Policy phase.
#[derive(Clone, Debug)]
pub struct Policy {
    verified: VerifiedObject,
}

/// Immutable publication receipt pairing a delta with its root transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcquisitionReceipt {
    /// Receipt identity.
    pub id: AcquisitionReceiptId,
    /// Delta identity included in the receipt.
    pub delta: AcquisitionDeltaId,
    /// Prior source root.
    pub base: SourceSnapshotId,
    /// Newly published source root.
    pub target: SourceSnapshotId,
    /// Publication root identity.
    pub publication: PublicationRootId,
}

impl AcquisitionReceipt {
    /// Returns the fixed canonical receipt preimage.
    #[must_use]
    pub fn canonical_bytes(&self) -> [u8; ID_BYTES * 4] {
        let mut encoded = [0_u8; ID_BYTES * 4];
        encoded[..ID_BYTES].copy_from_slice(self.delta.as_bytes());
        encoded[ID_BYTES..ID_BYTES * 2].copy_from_slice(self.base.as_bytes());
        encoded[ID_BYTES * 2..ID_BYTES * 3].copy_from_slice(self.target.as_bytes());
        encoded[ID_BYTES * 3..].copy_from_slice(self.publication.as_bytes());
        encoded
    }
}

/// PublishedDelta phase payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedDelta {
    /// Exact root-bound delta.
    pub delta: Arc<AcquisitionDelta>,
    /// Immutable publication receipt.
    pub receipt: Arc<AcquisitionReceipt>,
}

impl Policy {
    /// Builds and publishes an immutable target delta.
    pub fn publish(
        self,
        base: &SourceSnapshot,
        target: Arc<SourceSnapshot>,
        changes: Vec<DeltaChange>,
    ) -> AcquisitionOutcome<PublishedDelta> {
        let delta = match AcquisitionDelta::new(base, Arc::clone(&target), changes) {
            Ok(delta) => Arc::new(delta),
            Err(_) => return AcquisitionOutcome::Rejected(RejectReason::Protocol),
        };
        let publication = PublicationRootId::derive(&[
            delta.base().as_bytes(),
            delta.target().as_bytes(),
            delta.id().as_bytes(),
        ]);
        let receipt_id = AcquisitionReceiptId::derive(&[
            &self.verified.request.source,
            delta.id().as_bytes(),
            publication.as_bytes(),
        ]);
        let receipt = Arc::new(AcquisitionReceipt {
            id: receipt_id,
            delta: delta.id(),
            base: delta.base(),
            target: delta.target(),
            publication,
        });
        AcquisitionOutcome::Hit(PublishedDelta { delta, receipt })
    }

    /// Returns the verified archive identity used by policy.
    #[must_use]
    pub const fn object(&self) -> RawArchiveObjectId {
        self.verified.object
    }
}

/// One bounded shared effect slot used by the acquisition coordinator.
struct SharedSlot {
    result: Mutex<Option<AcquisitionOutcome<Arc<[u8]>>>>,
    wake: Condvar,
}

impl SharedSlot {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            wake: Condvar::new(),
        }
    }
}

/// Process-local acquisition coordinator using the execution WorkInterner.
#[derive(Clone)]
pub struct AcquisitionCoordinator {
    interner: Arc<WorkInterner>,
    slots: Arc<Mutex<BTreeMap<WorkKey, Arc<SharedSlot>>>>,
    max_slots: usize,
    telemetry: Telemetry,
}

impl AcquisitionCoordinator {
    /// Creates a coordinator with bounded live work and follower demand.
    #[must_use]
    pub fn new(live: usize, followers: usize) -> Self {
        Self {
            interner: WorkInterner::new(live.max(1), followers),
            slots: Arc::new(Mutex::new(BTreeMap::new())),
            max_slots: live.max(1),
            telemetry: Telemetry::default(),
        }
    }

    /// Returns coordinator telemetry.
    #[must_use]
    pub fn telemetry(&self) -> Telemetry {
        self.telemetry.clone()
    }

    /// Coalesces one metadata/object effect. Exactly one leader invokes
    /// `effect`; followers receive its immutable bytes or cancel their own
    /// demand without interrupting the leader.
    pub fn coordinate<F>(
        &self,
        request: &AcquisitionRequest,
        effect: F,
    ) -> AcquisitionOutcome<Arc<[u8]>>
    where
        F: FnOnce() -> AcquisitionOutcome<Arc<[u8]>>,
    {
        let (cancellation, _) = Cancellation::new();
        self.coordinate_with_cancellation(request, &cancellation, effect)
    }

    /// Coalesces one demand while observing caller-owned cancellation. A
    /// cancelled follower drops only its own demand; the leader and remaining
    /// followers continue to share the same effect and receipt.
    pub fn coordinate_with_cancellation<F>(
        &self,
        request: &AcquisitionRequest,
        cancellation: &Cancellation,
        effect: F,
    ) -> AcquisitionOutcome<Arc<[u8]>>
    where
        F: FnOnce() -> AcquisitionOutcome<Arc<[u8]>>,
    {
        let key = request.work_key();
        let slot = {
            let mut slots = self
                .slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if slots.len() >= self.max_slots && !slots.contains_key(&key) {
                return AcquisitionOutcome::Unavailable(Unavailable {
                    source: request.source,
                });
            }
            Arc::clone(
                slots
                    .entry(key)
                    .or_insert_with(|| Arc::new(SharedSlot::new())),
            )
        };
        let interned = match self.interner.intern(key) {
            Ok(interned) => interned,
            Err(_) => {
                return AcquisitionOutcome::Unavailable(Unavailable {
                    source: request.source,
                });
            }
        };
        if !interned.is_leader() {
            self.telemetry
                .update(|counters| counters.followers = counters.followers.saturating_add(1));
            let mut result = slot
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                if cancellation.is_cancelled() || interned.is_cancelled() {
                    interned.cancel();
                    return AcquisitionOutcome::Cancelled;
                }
                if let Some(result) = result.as_ref() {
                    return result.clone();
                }
                let (next, _) = slot
                    .wake
                    .wait_timeout(result, Duration::from_millis(5))
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                result = next;
            }
        }
        self.telemetry
            .update(|counters| counters.leaders = counters.leaders.saturating_add(1));
        let outcome = if cancellation.is_cancelled() {
            AcquisitionOutcome::Cancelled
        } else {
            effect()
        };
        {
            let mut result = slot
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *result = Some(outcome.clone());
            slot.wake.notify_all();
        }
        if let AcquisitionOutcome::Hit(bytes) = &outcome {
            let output = backend_execution::OutputVersion::from_value(bytes.as_ref());
            let claim = UntrustedOutputClaim {
                output: output.to_bytes(),
                canonical_bytes: Arc::new(bytes.to_vec()),
                coverage: ResultCoverage::Complete,
            };
            let validator = |_: backend_execution::OutputVersion,
                             _: &[u8],
                             _: ResultCoverage|
             -> Result<(), OutputValidationError> { Ok(()) };
            if let Ok(admission) = OutputAdmission::admit(claim, &validator) {
                let _ = interned.complete(admission);
            } else {
                let _ = interned.terminate();
            }
        } else {
            let _ = interned.terminate();
        }
        outcome
    }
}

/// Marker for the six phases in the typed acquisition state machine.
pub trait AcquisitionPhase: sealed::Sealed {}
mod sealed {
    pub trait Sealed {}
}
impl sealed::Sealed for Resolve {}
impl sealed::Sealed for Metadata {}
impl sealed::Sealed for Object {}
impl sealed::Sealed for VerifiedObject {}
impl sealed::Sealed for Policy {}
impl sealed::Sealed for PublishedDelta {}
impl AcquisitionPhase for Resolve {}
impl AcquisitionPhase for Metadata {}
impl AcquisitionPhase for Object {}
impl AcquisitionPhase for VerifiedObject {}
impl AcquisitionPhase for Policy {}
impl AcquisitionPhase for PublishedDelta {}

/// Type-level owner of one acquisition phase. The concrete phase structs
/// above expose the useful transition APIs; this alias makes generic bounds
/// available to orchestration code without a runtime enum.
pub type AcquisitionState<S> = S;

/// Streams an archive into a private temp object and admits its identity.
pub fn admit_archive(
    source: &mut impl Read,
    store: &LeaseStore,
    maximum: u64,
) -> Result<(RawArchiveObjectId, CasAdmission), AcquisitionOutcome<()>> {
    let mut temp = store.temp("archive").map_err(|_| {
        AcquisitionOutcome::Unavailable(Unavailable {
            source: [0; ID_BYTES],
        })
    })?;
    let mut hasher = Hasher::new();
    hasher.update(b"backend.acquisition.archive.v1\0");
    let mut buffer = [0_u8; CHUNK_BYTES];
    let mut length = 0_u64;
    loop {
        let read = source.read(&mut buffer).map_err(|_| {
            AcquisitionOutcome::Unavailable(Unavailable {
                source: [0; ID_BYTES],
            })
        })?;
        if read == 0 {
            break;
        }
        length = length
            .checked_add(read as u64)
            .ok_or(AcquisitionOutcome::Corrupt(CorruptReason::Integrity))?;
        if length > maximum {
            return Err(AcquisitionOutcome::Rejected(RejectReason::Bounds));
        }
        hasher.update(&buffer[..read]);
        temp.write_all(&buffer[..read]).map_err(|_| {
            AcquisitionOutcome::Unavailable(Unavailable {
                source: [0; ID_BYTES],
            })
        })?;
    }
    let digest = *hasher.finalize().as_bytes();
    let mut payload = Vec::with_capacity(ID_BYTES + 8);
    payload.extend_from_slice(&length.to_be_bytes());
    payload.extend_from_slice(&digest);
    let object = RawArchiveObjectId::derive(&[&payload]);
    let admission = store.cas_admit(&mut temp, object).map_err(|_| {
        AcquisitionOutcome::Unavailable(Unavailable {
            source: [0; ID_BYTES],
        })
    })?;
    Ok((object, admission))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Barrier,
        atomic::{AtomicUsize, Ordering},
    };

    fn snapshot(value: u8) -> Arc<SourceSnapshot> {
        let object = RawArchiveObjectId::from_bytes(&[value]);
        let manifest = Arc::new(
            TreeManifest::new(vec![ManifestEntry {
                path: Arc::from("src/lib.rs"),
                object,
                mode: 0,
            }])
            .expect("manifest"),
        );
        Arc::new(SourceSnapshot::new([1; 32], [2; 32], 7, manifest, Vec::new()).expect("snapshot"))
    }

    #[test]
    fn delta_binds_roots_and_repeated_apply_is_idempotent() {
        let base = snapshot(1);
        let target = snapshot(2);
        let before = base.manifest().entries()[0].object;
        let after = target.manifest().entries()[0].object;
        let delta = AcquisitionDelta::new(
            &base,
            Arc::clone(&target),
            vec![DeltaChange {
                path: Arc::from("src/lib.rs"),
                before: Some(before),
                after: Some(after),
            }],
        )
        .expect("delta");
        assert_eq!(delta.apply(&base).expect("apply").id(), target.id());
        assert_eq!(delta.apply(&target).expect("repeat").id(), target.id());
        assert!(matches!(
            delta.apply(&snapshot(3)),
            Err(DeltaError::StaleBase { .. })
        ));
    }

    #[test]
    fn thirty_two_callers_share_one_leader_effect() {
        let artifact = RawArchiveObjectId::from_bytes(b"archive");
        let request =
            Arc::new(AcquisitionRequest::new([9; 32], "pkg@1", artifact, 1, 4).expect("request"));
        let coordinator = Arc::new(AcquisitionCoordinator::new(4, 64));
        let barrier = Arc::new(Barrier::new(32));
        let effects = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        for _ in 0..32 {
            let coordinator = Arc::clone(&coordinator);
            let request = Arc::clone(&request);
            let barrier = Arc::clone(&barrier);
            let effects = Arc::clone(&effects);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                coordinator.coordinate(&request, || {
                    effects.fetch_add(1, Ordering::AcqRel);
                    std::thread::sleep(Duration::from_millis(20));
                    AcquisitionOutcome::Hit(Arc::<[u8]>::from(&b"receipt"[..]))
                })
            }));
        }
        let outcomes: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().expect("join"))
            .collect();
        assert_eq!(effects.load(Ordering::Acquire), 1);
        assert!(outcomes.iter().all(|outcome| {
            matches!(outcome, AcquisitionOutcome::Hit(bytes) if bytes.as_ref() == b"receipt")
        }));
        assert_eq!(coordinator.telemetry().snapshot().leaders, 1);
        assert_eq!(coordinator.telemetry().snapshot().followers, 31);
    }

    #[test]
    fn follower_cancellation_does_not_cancel_leader() {
        let request = Arc::new(
            AcquisitionRequest::new(
                [8; 32],
                "pkg@2",
                RawArchiveObjectId::from_bytes(b"archive-2"),
                1,
                4,
            )
            .expect("request"),
        );
        let coordinator = Arc::new(AcquisitionCoordinator::new(2, 4));
        let leader_coordinator = Arc::clone(&coordinator);
        let leader_request = Arc::clone(&request);
        let leader = std::thread::spawn(move || {
            leader_coordinator.coordinate(&leader_request, || {
                std::thread::sleep(Duration::from_millis(40));
                AcquisitionOutcome::Hit(Arc::<[u8]>::from(&b"shared"[..]))
            })
        });
        std::thread::sleep(Duration::from_millis(5));
        let (cancellation, handle) = Cancellation::new();
        let follower_coordinator = Arc::clone(&coordinator);
        let follower_request = Arc::clone(&request);
        let follower = std::thread::spawn(move || {
            follower_coordinator.coordinate_with_cancellation(
                &follower_request,
                &cancellation,
                || AcquisitionOutcome::Hit(Arc::<[u8]>::from(&b"wrong"[..])),
            )
        });
        std::thread::sleep(Duration::from_millis(10));
        handle.cancel();
        assert!(matches!(
            follower.join().expect("follower"),
            AcquisitionOutcome::Cancelled
        ));
        assert!(
            matches!(leader.join().expect("leader"), AcquisitionOutcome::Hit(bytes) if bytes.as_ref() == b"shared")
        );
    }

    #[test]
    fn lease_and_cas_are_first_writer_wins() {
        let root = std::env::temp_dir().join(format!("acquisition-{}", now_millis()));
        let _ = fs::remove_dir_all(&root);
        let store = LeaseStore::open(&root).expect("store");
        let store_other = LeaseStore::open(&root).expect("store");
        let request = AcquisitionRequest::new(
            [7; 32],
            "pkg@1",
            RawArchiveObjectId::from_bytes(b"archive"),
            1,
            1,
        )
        .expect("request");
        let guard = store
            .acquire(request.work_key(), Duration::from_secs(60))
            .expect("lease");
        assert!(guard.is_some());
        assert!(
            store_other
                .acquire(request.work_key(), Duration::from_secs(60))
                .expect("lease")
                .is_none()
        );
        drop(guard);
        let mut first = store.temp("archive").expect("temp");
        first.write_all(b"bytes").expect("write");
        let object = RawArchiveObjectId::from_bytes(b"bytes");
        assert_eq!(
            store.cas_admit(&mut first, object).expect("cas"),
            CasAdmission::Winner
        );
        let mut second = store_other.temp("archive").expect("temp");
        second.write_all(b"bytes").expect("write");
        assert_eq!(
            store_other.cas_admit(&mut second, object).expect("cas"),
            CasAdmission::Existing
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn transient_failures_never_make_negative_facts_and_breaker_has_one_probe() {
        assert_eq!(AttemptFailure::Timeout.negative_kind(), None);
        assert_eq!(AttemptFailure::Integrity.negative_kind(), None);
        assert_eq!(
            AttemptFailure::NotFound.negative_kind(),
            Some(NegativeFactKind::NotFound)
        );
        let breaker = CircuitBreaker::new(2, Duration::from_millis(10));
        breaker.failure(10);
        breaker.failure(10);
        assert!(breaker.allow(10).is_err());
        let probe = breaker.allow(20).expect("probe");
        assert!(breaker.allow(20).is_err());
        breaker.success();
        drop(probe);
        assert_eq!(breaker.state(), CircuitState::Closed);
        let policy =
            RetryPolicy::with_seed(Duration::from_millis(10), Duration::from_millis(100), 4, 1);
        assert!(policy.delay(8, Some(Duration::from_secs(2))) <= Duration::from_millis(100));
    }
}
