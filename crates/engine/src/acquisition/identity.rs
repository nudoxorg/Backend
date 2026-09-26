use blake3::Hasher;
use std::{
    fmt,
    fs::{self, File},
    io::{self, Read},
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) const CHUNK_BYTES: usize = 64 * 1024;
pub(super) const ID_BYTES: usize = 32;
pub(super) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

pub(super) fn frame(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

pub(super) fn digest(domain: &[u8], fields: &[&[u8]]) -> [u8; ID_BYTES] {
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
            pub(super) fn derive(fields: &[&[u8]]) -> Self {
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
    /// Mutable release-fact identity at which this claim was observed.
    pub facts: [u8; ID_BYTES],
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
        Self::new_with_facts(source, coordinate, version, archive, [0; ID_BYTES])
    }

    /// Admits a claim and binds its mutable release-fact frontier.
    pub fn new_with_facts(
        source: [u8; ID_BYTES],
        coordinate: impl Into<String>,
        version: impl Into<String>,
        archive: RawArchiveObjectId,
        facts: [u8; ID_BYTES],
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
            &facts,
        ]);
        Ok(Self {
            source,
            coordinate,
            version,
            archive,
            facts,
            id,
        })
    }
}

pub(super) fn canonical_text(value: String) -> Result<String, IdentityError> {
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

    /// Rehydrates an archive identity from a durable verified claim without
    /// reopening the archive bytes. Registry publication already authenticated
    /// the claim and records its exact extent, so rebuilding a source root can
    /// use this compact identity directly instead of allocating and hashing
    /// every object in the catalog again.
    pub(crate) fn from_verified_claim(length: u64, claim: [u8; ID_BYTES]) -> Self {
        let mut payload = Vec::with_capacity(ID_BYTES + 8);
        payload.extend_from_slice(&length.to_be_bytes());
        payload.extend_from_slice(&claim);
        Self::derive(&[&payload])
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
        Self::from_sorted(entries)
    }

    /// Admits an already canonical path sequence without sorting it again.
    ///
    /// Incremental source deltas maintain sorted order while merging, so this
    /// constructor keeps a sparse update O(base + changes) and avoids a second
    /// allocation/sort pass over a large unchanged manifest.
    pub(crate) fn from_sorted(entries: Vec<ManifestEntry>) -> Result<Self, IdentityError> {
        for entry in &entries {
            canonical_text(entry.path.to_string())?;
        }
        if entries
            .windows(2)
            .any(|window| window[0].path == window[1].path)
        {
            return Err(IdentityError::Duplicate);
        }
        if entries
            .windows(2)
            .any(|window| window[0].path > window[1].path)
        {
            return Err(IdentityError::Unsorted);
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

pub(super) fn collect_directory(
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
    facts_frontier: [u8; ID_BYTES],
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
        claims: Vec<ReleaseClaimId>,
    ) -> Result<Self, IdentityError> {
        Self::new_with_frontier(
            source,
            cursor,
            policy_epoch,
            [0; ID_BYTES],
            manifest,
            claims,
        )
    }

    /// Creates a snapshot bound to the immutable release and mutable-facts
    /// frontier observed by the catalog owner. The frontier is part of the
    /// root identity so a yank/advisory refresh produces a metadata-only root
    /// and never causes archive payload work to be repeated.
    pub fn new_with_frontier(
        source: [u8; ID_BYTES],
        cursor: [u8; ID_BYTES],
        policy_epoch: u64,
        facts_frontier: [u8; ID_BYTES],
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
        encoded.extend_from_slice(&facts_frontier);
        encoded.extend_from_slice(manifest.id().as_bytes());
        for claim in &claims {
            encoded.extend_from_slice(claim.as_bytes());
        }
        let id = SourceSnapshotId::derive(&[&encoded]);
        Ok(Self {
            source,
            cursor,
            policy_epoch,
            facts_frontier,
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
    /// Returns the content identity of mutable release facts and advisory
    /// observations represented by this root.
    #[must_use]
    pub const fn facts_frontier(&self) -> [u8; ID_BYTES] {
        self.facts_frontier
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
        encoded.extend_from_slice(&self.facts_frontier);
        encoded.extend_from_slice(self.manifest.id().as_bytes());
        for claim in self.claims.iter() {
            encoded.extend_from_slice(claim.as_bytes());
        }
        encoded
    }
}
