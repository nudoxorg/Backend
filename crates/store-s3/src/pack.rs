//! File-backed immutable packs for small backend-store envelopes.
//!
//! A pack is one bounded S3 object. Its canonical prefix maps physical
//! `ObjectId`s to complete envelope extents and carries a Merkle inclusion
//! proof for each extent. The manifest, layout, and pack identities describe
//! physical storage only; logical object IDs remain backend-store IDs.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fmt,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

use backend_store::{
    ObjectId, RelationAdmissionRegistry, TypedObject, UntrustedObjectId, VerifiedObjectEnvelope,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    AllowedRange, REQUIRED_CONTENT_ENCODING, REQUIRED_IF_NONE_MATCH, RemoteStoreError,
    S3ObjectRoute, TempEnvelope, WorkFence, check_identity_response, drain_ack, ensure_unexpired,
    is_redirect_error, is_retryable_status, map_remote_admission_error, parse_content_length,
    parse_content_range, read_bounded, url_signs_headers,
};

pub use backend_store::artifact_pack::{
    ArtifactLayoutId as S3LayoutId, ArtifactPackExtent as PackExtent, ArtifactPackId as S3PackId,
};
use backend_store::artifact_pack::{ArtifactPackManifest, ArtifactPackPage};

/// Recommended batch target for immutable remote packs.
pub const RECOMMENDED_PACK_BYTES: u64 =
    backend_store::artifact_pack::RECOMMENDED_ARTIFACT_PACK_BYTES;
/// Hard pack ceiling; larger data remains separate logical objects/packs.
pub const MAX_PACK_BYTES: u64 = backend_store::artifact_pack::MAX_ARTIFACT_PACK_BYTES;
/// Maximum number of object extents in one bounded canonical index.
pub const MAX_PACK_OBJECTS: usize = backend_store::artifact_pack::MAX_ARTIFACT_PACK_OBJECTS;
/// Hard bound on the canonical manifest prefix and its in-memory parse.
pub const MAX_PACK_MANIFEST_BYTES: usize =
    backend_store::artifact_pack::MAX_ARTIFACT_PACK_MANIFEST_BYTES;
const MIN_PACK_MANIFEST_BYTES: usize =
    backend_store::artifact_pack::MIN_ARTIFACT_PACK_MANIFEST_BYTES;
/// Hard ceiling for the explicit coalesced root-plus-directory cold-read mode.
pub const MAX_COALESCED_DIRECTORY_PREFIX_BYTES: usize = 100 * 1024;
/// Maximum number of simultaneous exact-range GETs in a bounded multi-object read.
pub const MAX_PARALLEL_PACK_READS: usize = 16;

/// Checked view of the storage-neutral backend-store artifact pack manifest.
///
/// S3-specific checksums and capabilities live outside this layout. Local and
/// remote readers therefore interpret the same canonical extent map and proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct S3PackManifest {
    inner: ArtifactPackManifest,
}

impl S3PackManifest {
    /// Physical pack identity shared with the local artifact-pack layout.
    #[must_use]
    pub const fn pack_id(&self) -> S3PackId {
        self.inner.pack_id()
    }

    /// Physical range-map identity shared with the local artifact-pack layout.
    #[must_use]
    pub const fn layout_id(&self) -> S3LayoutId {
        self.inner.layout_id()
    }

    /// Total S3 object length, including the canonical manifest prefix.
    #[must_use]
    pub const fn pack_bytes(&self) -> u64 {
        self.inner.pack_bytes()
    }

    /// Exact canonical manifest prefix length.
    #[must_use]
    pub const fn manifest_bytes(&self) -> u32 {
        self.inner.manifest_bytes()
    }

    /// Content root over the ordered bounded page roots.
    #[must_use]
    pub const fn page_root(&self) -> &[u8; 32] {
        self.inner.page_root()
    }

    /// Number of distinct logical objects in this pack.
    #[must_use]
    pub fn object_count(&self) -> usize {
        self.inner.object_count()
    }

    /// Full canonical extents when this manifest came from a local builder.
    /// A cold root parse intentionally returns no object rows; use `pages()`
    /// and `S3PackRoute::get_object` to resolve those without loading the pack.
    #[must_use]
    pub fn extents(&self) -> &[PackExtent] {
        self.inner.extents()
    }

    /// Authenticated bounded page directory used for cold object lookup.
    #[must_use]
    pub fn pages(&self) -> &[backend_store::artifact_pack::ArtifactPackPageDescriptor] {
        self.inner.pages()
    }

    /// Finds the one bounded page that may contain a logical object ID.
    #[must_use]
    pub fn page_for_object(
        &self,
        id: ObjectId,
    ) -> Option<&backend_store::artifact_pack::ArtifactPackPageDescriptor> {
        self.inner
            .page_for_object(id)
            .map(|(_, descriptor)| descriptor)
    }

    /// Finds the candidate root page for an untrusted raw object-ID claim.
    /// The page and complete envelope still require cryptographic admission.
    #[must_use]
    pub fn page_for_object_claim(
        &self,
        id: UntrustedObjectId,
    ) -> Option<&backend_store::artifact_pack::ArtifactPackPageDescriptor> {
        self.inner
            .page_for_object_claim(id)
            .map(|(_, descriptor)| descriptor)
    }

    fn parse(bytes: &[u8], actual_pack_bytes: u64) -> Result<Self, RemoteStoreError> {
        let inner = ArtifactPackManifest::parse(bytes, actual_pack_bytes)
            .map_err(map_remote_admission_error)?;
        Ok(Self { inner })
    }
}

/// File-backed bounded pack assembler. Add typed or streamed object payloads,
/// then finish once to create a deterministic shared artifact pack ordered by
/// physical ObjectId. The caller should normally flush around the recommended
/// target, with the hard ceiling as the single-PutObject limit.
pub struct S3PackBuilder {
    max_pack_bytes: u64,
    staged_bytes: u64,
    ids: BTreeSet<[u8; 32]>,
    staged: Vec<StagedObject>,
    registry: RelationAdmissionRegistry,
}

struct StagedObject {
    id: ObjectId,
    envelope: TempEnvelope,
}

impl S3PackBuilder {
    /// Creates a builder with a hard byte ceiling no greater than 160 MiB.
    pub fn new(max_pack_bytes: u64) -> Result<Self, RemoteStoreError> {
        Self::new_with_registry(max_pack_bytes, RelationAdmissionRegistry::default())
    }

    /// Creates a builder with the relation grammars admitted by the index
    /// store. This is required when a pack may contain relation objects.
    pub fn new_with_registry(
        max_pack_bytes: u64,
        registry: RelationAdmissionRegistry,
    ) -> Result<Self, RemoteStoreError> {
        if max_pack_bytes == 0 || max_pack_bytes > MAX_PACK_BYTES {
            return Err(RemoteStoreError::Bounds);
        }
        Ok(Self {
            max_pack_bytes,
            staged_bytes: 0,
            ids: BTreeSet::new(),
            staged: Vec::new(),
            registry,
        })
    }

    /// Streams a typed object's payload into a private canonical envelope
    /// spool. The shared backend-store writer recomputes its object ID and
    /// validates its typed version before this method returns.
    pub fn push(&mut self, object: &TypedObject) -> Result<(), RemoteStoreError> {
        let claim = backend_store::ArtifactObjectClaim::new(
            object.schema(),
            *object.key(),
            *object.version(),
            u64::try_from(object.bytes().len()).map_err(|_| RemoteStoreError::Bounds)?,
        )
        .with_object_id(UntrustedObjectId::from_bytes(*object.id().as_bytes()));
        let mut payload = io::Cursor::new(object.bytes());
        self.push_streamed(claim, &mut payload).map(|_| ())
    }

    /// Streams one claimed payload into the exact backend-store envelope ABI.
    ///
    /// The object ID is computed from the payload and typed claim; an optional
    /// ID claim is checked before admission. The payload reader advances by
    /// exactly the declared length. The complete envelope is first written to
    /// a private file, then verified before it can enter the pack.
    pub fn push_streamed<R: Read>(
        &mut self,
        claim: backend_store::ArtifactObjectClaim,
        payload: &mut R,
    ) -> Result<ObjectId, RemoteStoreError> {
        if self.staged.len() >= MAX_PACK_OBJECTS {
            return Err(RemoteStoreError::Bounds);
        }
        let remaining = self
            .max_pack_bytes
            .checked_sub(self.staged_bytes)
            .ok_or(RemoteStoreError::Bounds)?;
        let maximum = usize::try_from(remaining).map_err(|_| RemoteStoreError::Bounds)?;
        let (mut envelope, mut file) = TempEnvelope::create()?;
        let verified = backend_store::write_streamed_object_envelope(
            claim,
            payload,
            &mut file,
            maximum,
            &self.registry,
        )
        .map_err(map_remote_admission_error)?;
        file.flush().map_err(|_| RemoteStoreError::Unavailable)?;
        let length = file
            .metadata()
            .map_err(|_| RemoteStoreError::Unavailable)?
            .len();
        if length == 0 || length > remaining {
            return Err(RemoteStoreError::Bounds);
        }
        envelope.len = length;
        envelope.sha256 = sha256_file(&mut file, length)?;
        drop(file);
        self.stage(verified.id(), envelope)?;
        Ok(verified.id())
    }

    fn stage(&mut self, id: ObjectId, envelope: TempEnvelope) -> Result<(), RemoteStoreError> {
        let raw_id = *id.as_bytes();
        if !self.ids.insert(raw_id) {
            return Err(RemoteStoreError::Identity);
        }
        let next = self
            .staged_bytes
            .checked_add(envelope.len)
            .ok_or(RemoteStoreError::Bounds)?;
        if next > self.max_pack_bytes {
            self.ids.remove(&raw_id);
            return Err(RemoteStoreError::Bounds);
        }
        self.staged_bytes = next;
        self.staged.push(StagedObject { id, envelope });
        Ok(())
    }

    /// Finalizes the shared storage-neutral manifest and proof directory in a
    /// file-backed bounded writer. S3 SHA-256 is computed separately over the
    /// final bytes for the REST checksum header.
    pub fn finish(mut self) -> Result<ImmutableS3Pack, RemoteStoreError> {
        if self.staged.is_empty() {
            return Err(RemoteStoreError::Bounds);
        }
        self.staged
            .sort_by(|left, right| left.id.as_bytes().cmp(right.id.as_bytes()));
        let (mut pack_temp, pack_file) = TempEnvelope::create()?;
        let mut writer = backend_store::artifact_pack::ArtifactPackWriter::new_with_registry(
            pack_file,
            self.staged.len(),
            self.max_pack_bytes,
            self.registry.clone(),
        )
        .map_err(map_remote_admission_error)?;
        for staged in &self.staged {
            let mut input = staged.envelope.open()?;
            writer
                .push_with(staged.id, |output| {
                    io::copy(&mut input, output)
                        .map_err(|error| backend_store::StoreError::Io(error.to_string()))
                })
                .map_err(map_remote_admission_error)?;
        }
        let (mut pack_file, manifest) = writer.finish().map_err(map_remote_admission_error)?;
        let manifest_bytes = manifest.encode();
        let pack_len = manifest.pack_bytes();
        if pack_len > self.max_pack_bytes
            || manifest_bytes.len()
                != usize::try_from(manifest.manifest_bytes())
                    .map_err(|_| RemoteStoreError::Bounds)?
        {
            return Err(RemoteStoreError::Identity);
        }
        pack_temp.len = pack_len;
        pack_temp.sha256 = sha256_file(&mut pack_file, pack_len)?;
        pack_file
            .flush()
            .map_err(|_| RemoteStoreError::Unavailable)?;
        drop(pack_file);
        pack_temp.capture_snapshot()?;
        let retained_metadata_bytes = manifest
            .retained_metadata_bytes()
            .saturating_add(manifest_bytes.capacity());
        let sha256 = pack_temp.sha256;
        Ok(ImmutableS3Pack {
            manifest: S3PackManifest { inner: manifest },
            manifest_bytes,
            file: pack_temp,
            sha256,
            retained_metadata_bytes,
        })
    }
}

fn sha256_file(file: &mut File, expected_bytes: u64) -> Result<[u8; 32], RemoteStoreError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|_| RemoteStoreError::Unavailable)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| RemoteStoreError::Unavailable)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read).map_err(|_| RemoteStoreError::Bounds)?)
            .ok_or(RemoteStoreError::Bounds)?;
        if total > expected_bytes {
            return Err(RemoteStoreError::Identity);
        }
        hasher.update(&buffer[..read]);
    }
    if total != expected_bytes {
        return Err(RemoteStoreError::Identity);
    }
    Ok(hasher.finalize().into())
}

/// Immutable file-backed pack ready for one conditional S3 PutObject.
pub struct ImmutableS3Pack {
    manifest: S3PackManifest,
    manifest_bytes: Vec<u8>,
    file: TempEnvelope,
    sha256: [u8; 32],
    retained_metadata_bytes: usize,
}

impl ImmutableS3Pack {
    /// Physical pack identity, separate from every logical object ID.
    #[must_use]
    pub const fn pack_id(&self) -> S3PackId {
        self.manifest.pack_id()
    }

    /// Physical range-map identity.
    #[must_use]
    pub const fn layout_id(&self) -> S3LayoutId {
        self.manifest.layout_id()
    }

    /// Complete pack length including its manifest prefix.
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.file.len
    }

    /// Whether the pack has no bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.file.len == 0
    }

    /// Exact canonical manifest length.
    #[must_use]
    pub const fn manifest_bytes(&self) -> u32 {
        self.manifest.manifest_bytes()
    }

    /// Full-object SHA-256 sent in the S3 REST checksum header.
    #[must_use]
    pub const fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    /// SHA-256 of the canonical manifest prefix, pinned by cold-read grants.
    #[must_use]
    pub fn manifest_sha256(&self) -> [u8; 32] {
        Sha256::digest(&self.manifest_bytes).into()
    }

    /// Number of typed objects packed.
    #[must_use]
    pub fn object_count(&self) -> usize {
        self.manifest.object_count()
    }

    /// Canonical pack manifest for constructing an index-side storage map.
    #[must_use]
    pub const fn manifest(&self) -> &S3PackManifest {
        &self.manifest
    }

    /// A portable accounting bound for retained index metadata. Payloads and
    /// envelope bytes live in private files and are excluded from this count.
    #[must_use]
    pub const fn retained_metadata_bytes(&self) -> usize {
        self.retained_metadata_bytes
    }

    /// Opens the immutable pack spool at byte zero for a streaming PUT.
    #[must_use]
    pub fn open_pack(&self) -> Result<File, RemoteStoreError> {
        self.file.open()
    }
}

impl fmt::Debug for ImmutableS3Pack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImmutableS3Pack")
            .field("pack_id", &self.manifest.pack_id())
            .field("layout_id", &self.manifest.layout_id())
            .field("bytes", &self.file.len)
            .field("object_count", &self.manifest.object_count())
            .field("manifest_bytes", &self.manifest.manifest_bytes())
            .field("sha256", &self.sha256)
            .finish()
    }
}

/// Signed, expiring PutObject capability for exactly one immutable pack.
#[derive(Clone, Deserialize, Serialize)]
pub struct PackUploadCapability {
    url: String,
    pack_id: [u8; 32],
    layout_id: [u8; 32],
    pack_bytes: u64,
    manifest_bytes: u32,
    sha256: [u8; 32],
    manifest_sha256: [u8; 32],
    expires_at_unix_seconds: u64,
    fence: WorkFence,
    if_none_match_signed: bool,
    checksum_signed: bool,
}

impl PackUploadCapability {
    /// Constructs capability claims from the index owner's scoped grant.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        url: impl Into<String>,
        pack_id: S3PackId,
        layout_id: S3LayoutId,
        pack_bytes: u64,
        manifest_bytes: u32,
        sha256: [u8; 32],
        manifest_sha256: [u8; 32],
        expires_at_unix_seconds: u64,
        fence: WorkFence,
        if_none_match_signed: bool,
        checksum_signed: bool,
    ) -> Self {
        Self {
            url: url.into(),
            pack_id: *pack_id.as_bytes(),
            layout_id: *layout_id.as_bytes(),
            pack_bytes,
            manifest_bytes,
            sha256,
            manifest_sha256,
            expires_at_unix_seconds,
            fence,
            if_none_match_signed,
            checksum_signed,
        }
    }
}

impl fmt::Debug for PackUploadCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PackUploadCapability")
            .field("url", &"[REDACTED]")
            .field("pack_id", &self.pack_id)
            .field("layout_id", &self.layout_id)
            .field("pack_bytes", &self.pack_bytes)
            .field("manifest_bytes", &self.manifest_bytes)
            .field("expires_at_unix_seconds", &self.expires_at_unix_seconds)
            .field("fence", &self.fence)
            .finish_non_exhaustive()
    }
}

/// Signed, expiring GET capability for one immutable pack object.
///
/// The bearer URL authorizes access to this pack key; individual ranges are
/// narrowed by `open_pack`/`get_object` and accepted only after the checked
/// manifest and complete-object Merkle proof pass. `full_read_allowed` is
/// required only to resolve a lost PUT acknowledgement after a 412.
#[derive(Clone, Deserialize, Serialize)]
pub struct PackReadCapability {
    url: String,
    pack_id: [u8; 32],
    layout_id: [u8; 32],
    pack_bytes: u64,
    manifest_bytes: u32,
    sha256: [u8; 32],
    manifest_sha256: [u8; 32],
    expires_at_unix_seconds: u64,
    fence: WorkFence,
    full_read_allowed: bool,
}

impl PackReadCapability {
    /// Constructs a pack-scoped cold-read grant from index-owner claims.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        url: impl Into<String>,
        pack_id: S3PackId,
        layout_id: S3LayoutId,
        pack_bytes: u64,
        manifest_bytes: u32,
        sha256: [u8; 32],
        manifest_sha256: [u8; 32],
        expires_at_unix_seconds: u64,
        fence: WorkFence,
        full_read_allowed: bool,
    ) -> Self {
        Self {
            url: url.into(),
            pack_id: *pack_id.as_bytes(),
            layout_id: *layout_id.as_bytes(),
            pack_bytes,
            manifest_bytes,
            sha256,
            manifest_sha256,
            expires_at_unix_seconds,
            fence,
            full_read_allowed,
        }
    }
}

impl fmt::Debug for PackReadCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PackReadCapability")
            .field("url", &"[REDACTED]")
            .field("pack_id", &self.pack_id)
            .field("layout_id", &self.layout_id)
            .field("pack_bytes", &self.pack_bytes)
            .field("manifest_bytes", &self.manifest_bytes)
            .field("expires_at_unix_seconds", &self.expires_at_unix_seconds)
            .field("fence", &self.fence)
            .field("full_read_allowed", &self.full_read_allowed)
            .finish()
    }
}

/// Storage-only receipt for one immutable S3 pack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoredPackReceipt {
    pack_id: S3PackId,
    layout_id: S3LayoutId,
    pack_bytes: u64,
    object_count: u32,
    sha256: [u8; 32],
    fence: WorkFence,
    reused_after_412: bool,
}

impl StoredPackReceipt {
    /// Stored physical pack ID.
    #[must_use]
    pub const fn pack_id(&self) -> S3PackId {
        self.pack_id
    }

    /// Stored physical layout ID.
    #[must_use]
    pub const fn layout_id(&self) -> S3LayoutId {
        self.layout_id
    }

    /// Complete pack object size in S3.
    #[must_use]
    pub const fn pack_bytes(&self) -> u64 {
        self.pack_bytes
    }

    /// Number of logical object extents inside the pack.
    #[must_use]
    pub const fn object_count(&self) -> u32 {
        self.object_count
    }

    /// Full-pack SHA-256 transport checksum.
    #[must_use]
    pub const fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    /// Exact work/attempt fence carried by the index grant.
    #[must_use]
    pub const fn fence(&self) -> WorkFence {
        self.fence
    }

    /// True when S3 returned 412 and a full byte-for-byte readback proved that
    /// the immutable pack was already present.
    #[must_use]
    pub const fn reused_after_412(&self) -> bool {
        self.reused_after_412
    }
}

/// One complete typed object fetched from a pack and admitted by backend-store.
pub struct CheckedPackedObject {
    id: ObjectId,
    verified_envelope: VerifiedObjectEnvelope,
    file: TempEnvelope,
    pack_id: S3PackId,
    layout_id: S3LayoutId,
    fence: WorkFence,
}

impl CheckedPackedObject {
    /// Store-verified logical object ID.
    #[must_use]
    pub const fn id(&self) -> ObjectId {
        self.id
    }

    /// Typed envelope metadata admitted while verifying this packed object.
    #[must_use]
    pub const fn verified_envelope(&self) -> VerifiedObjectEnvelope {
        self.verified_envelope
    }

    /// Physical pack that supplied the verified object.
    #[must_use]
    pub const fn pack_id(&self) -> S3PackId {
        self.pack_id
    }

    /// Physical layout that located the verified object.
    #[must_use]
    pub const fn layout_id(&self) -> S3LayoutId {
        self.layout_id
    }

    /// Assignment fence carried by the cold-read capability.
    #[must_use]
    pub const fn fence(&self) -> WorkFence {
        self.fence
    }

    /// Opens the checked complete store envelope at byte zero.
    #[must_use]
    pub fn open_envelope(&self) -> Result<File, RemoteStoreError> {
        self.file.open()
    }

    /// Reads exact bytes from the retained, identity-checked envelope without
    /// changing a shared file cursor. The metadata checks are cheap; a changed
    /// spool fails closed so the owner can discard and re-fetch its cache entry.
    pub fn read_envelope_range(
        &self,
        offset: u64,
        output: &mut [u8],
    ) -> Result<(), RemoteStoreError> {
        self.file.read_exact_at(offset, output)
    }

    /// Whether this checked spool still has the file identity admitted from
    /// S3. Cache owners use this to discard changed entries before serving pages.
    #[must_use]
    pub fn integrity_current(&self) -> bool {
        self.file.verify_snapshot().is_ok()
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Returns the backing spool path for mutation tests only.
    #[must_use]
    pub fn temp_path_for_test(&self) -> &std::path::Path {
        &self.file.path
    }

    /// Complete envelope byte count.
    #[must_use]
    pub const fn envelope_bytes(&self) -> u64 {
        self.file.len
    }
}

impl fmt::Debug for CheckedPackedObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CheckedPackedObject")
            .field("id", &self.id)
            .field("pack_id", &self.pack_id)
            .field("layout_id", &self.layout_id)
            .field("envelope_bytes", &self.file.len)
            .field("fence", &self.fence)
            .finish()
    }
}

/// A checked manifest reopened from cold storage.
pub struct S3RemotePack {
    manifest: S3PackManifest,
    capability: PackReadCapability,
    fence: WorkFence,
    verified_pages: Mutex<HashMap<usize, ArtifactPackPage>>,
    prefetched_prefix_bytes: u64,
}

impl S3RemotePack {
    /// Authenticated pack/layout identity parsed from the cold manifest.
    #[must_use]
    pub const fn manifest(&self) -> &S3PackManifest {
        &self.manifest
    }

    /// Bytes fetched by an explicit coalesced-directory policy while opening
    /// this pack. This is an I/O metric, not a completeness or trust claim.
    #[must_use]
    pub const fn prefetched_prefix_bytes(&self) -> u64 {
        self.prefetched_prefix_bytes
    }
}

impl fmt::Debug for S3RemotePack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3RemotePack")
            .field("manifest", &self.manifest)
            .field("fence", &self.fence)
            .finish_non_exhaustive()
    }
}

/// Bounded conditional PUT and proof-verified ranged GET for immutable packs.
#[derive(Clone)]
pub struct S3PackRoute {
    object_route: S3ObjectRoute,
    coalesced_directory_limit: Option<usize>,
}

impl fmt::Debug for S3PackRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3PackRoute")
            .field("object_route", &self.object_route)
            .finish_non_exhaustive()
    }
}

impl S3PackRoute {
    /// Wraps the same allowlisted, redirect-free route used by single objects.
    #[must_use]
    pub fn new(object_route: S3ObjectRoute) -> Self {
        Self {
            object_route,
            coalesced_directory_limit: None,
        }
    }

    /// Enables a coalesced cold-read policy when the conservative canonical
    /// root-plus-page prefix fits within `maximum_prefix_bytes`. Opening a
    /// qualifying pack fetches and verifies the bounded directory in one GET,
    /// caching each checked page for subsequent object reads. The cap cannot
    /// exceed [`MAX_COALESCED_DIRECTORY_PREFIX_BYTES`]. The default route is
    /// selective and fetches only the root followed by pages on demand.
    pub fn with_coalesced_directory_prefetch(
        mut self,
        maximum_prefix_bytes: usize,
    ) -> Result<Self, RemoteStoreError> {
        if maximum_prefix_bytes == 0 || maximum_prefix_bytes > MAX_COALESCED_DIRECTORY_PREFIX_BYTES
        {
            return Err(RemoteStoreError::Bounds);
        }
        self.coalesced_directory_limit = Some(maximum_prefix_bytes);
        Ok(self)
    }

    /// Uploads the complete manifest-plus-data pack with one conditional
    /// PutObject. A 412 is accepted only after an authorized full GET proves
    /// exact equality with the local pack spool.
    pub fn put_pack(
        &self,
        pack: &ImmutableS3Pack,
        grant: &PackUploadCapability,
        fence: WorkFence,
        existing_read: Option<&PackReadCapability>,
    ) -> Result<StoredPackReceipt, RemoteStoreError> {
        self.validate_upload(grant, fence)?;
        if grant.pack_id != *pack.pack_id().as_bytes()
            || grant.layout_id != *pack.layout_id().as_bytes()
            || grant.pack_bytes != pack.len()
            || grant.manifest_bytes != pack.manifest_bytes()
            || grant.sha256 != pack.sha256
            || grant.manifest_sha256 != pack.manifest_sha256()
        {
            return Err(RemoteStoreError::Identity);
        }
        if let Some(read) = existing_read {
            self.validate_read(read, fence)?;
            if !read.full_read_allowed
                || read.pack_id != grant.pack_id
                || read.layout_id != grant.layout_id
                || read.pack_bytes != grant.pack_bytes
                || read.sha256 != grant.sha256
            {
                return Err(RemoteStoreError::Capability);
            }
        }
        let checksum = BASE64.encode(grant.sha256);
        let mut attempt = 0;
        loop {
            self.validate_upload(grant, fence)?;
            let input = match pack.file.open() {
                Ok(file) => file,
                Err(error) => return Err(error),
            };
            let result = self
                .object_route
                .agent
                .put(&grant.url)
                .header("if-none-match", REQUIRED_IF_NONE_MATCH)
                .header("x-amz-checksum-sha256", &checksum)
                .header("accept-encoding", REQUIRED_CONTENT_ENCODING)
                .send(input);
            match result {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    if matches!(status, 200 | 201 | 204) {
                        check_identity_response(&response)?;
                        check_optional_full_checksum(&response, &grant.sha256)?;
                        drain_ack(response.body_mut(), self.object_route.config.max_ack_bytes)?;
                        return Ok(self.receipt(pack, fence, false)?);
                    }
                    if status == 412 {
                        let read = existing_read.ok_or(RemoteStoreError::ExistingUnverified)?;
                        let existing = self.read_full_for_retry(read, fence)?;
                        if existing.sha256 != pack.sha256 || !existing.same_bytes(&pack.file)? {
                            return Err(RemoteStoreError::Identity);
                        }
                        return self.receipt(pack, fence, true);
                    }
                    if status == 409 || is_retryable_status(status) {
                        if self.object_route.retry_or_stop(&mut attempt) {
                            continue;
                        }
                        return Err(RemoteStoreError::Unavailable);
                    }
                    return Err(RemoteStoreError::Protocol);
                }
                Err(error) => {
                    if is_redirect_error(&error) {
                        return Err(RemoteStoreError::Protocol);
                    }
                    if self.object_route.retry_or_stop(&mut attempt) {
                        continue;
                    }
                    return Err(RemoteStoreError::Unavailable);
                }
            }
        }
    }

    /// Fetches and checks the bounded manifest prefix, reconstructing the
    /// physical layout without any writer process state.
    pub fn open_pack(
        &self,
        grant: &PackReadCapability,
        fence: WorkFence,
    ) -> Result<S3RemotePack, RemoteStoreError> {
        self.validate_read(grant, fence)?;
        let manifest_len = u64::from(grant.manifest_bytes);
        let coalesced_prefix_len = self
            .coalesced_directory_limit
            .and_then(|limit| {
                backend_store::artifact_pack::artifact_pack_directory_prefix_upper_bound(
                    grant.manifest_bytes,
                )
                .filter(|bound| *bound <= limit)
                .filter(|bound| u64::try_from(*bound).is_ok_and(|value| value < grant.pack_bytes))
                .map(|bound| u64::try_from(bound).ok())
                .flatten()
            })
            .map(|bound| bound.min(grant.pack_bytes))
            .unwrap_or(manifest_len);
        let range = AllowedRange {
            start: 0,
            end_inclusive: coalesced_prefix_len
                .checked_sub(1)
                .ok_or(RemoteStoreError::Capability)?,
        };
        let mut response = self.request_range(grant, range)?;
        let bytes = read_bounded(response.body_mut(), coalesced_prefix_len)?;
        let manifest_len_usize =
            usize::try_from(manifest_len).map_err(|_| RemoteStoreError::Bounds)?;
        let manifest_bytes = bytes
            .get(..manifest_len_usize)
            .ok_or(RemoteStoreError::Identity)?;
        let manifest_sha256: [u8; 32] = Sha256::digest(manifest_bytes).into();
        if bytes.len()
            != usize::try_from(coalesced_prefix_len).map_err(|_| RemoteStoreError::Bounds)?
            || manifest_sha256 != grant.manifest_sha256
        {
            return Err(RemoteStoreError::Identity);
        }
        let manifest = S3PackManifest::parse(manifest_bytes, grant.pack_bytes)?;
        if manifest.pack_id().as_bytes() != &grant.pack_id
            || manifest.layout_id().as_bytes() != &grant.layout_id
            || manifest.pack_bytes() != grant.pack_bytes
            || manifest.manifest_bytes() != grant.manifest_bytes
        {
            return Err(RemoteStoreError::Identity);
        }
        let verified_pages = Mutex::new(HashMap::new());
        if coalesced_prefix_len > manifest_len {
            let mut cached = HashMap::with_capacity(manifest.inner.pages().len());
            for (page_index, descriptor) in manifest.inner.pages().iter().enumerate() {
                let page_start =
                    usize::try_from(descriptor.offset()).map_err(|_| RemoteStoreError::Bounds)?;
                let page_end = page_start
                    .checked_add(
                        usize::try_from(descriptor.bytes())
                            .map_err(|_| RemoteStoreError::Bounds)?,
                    )
                    .ok_or(RemoteStoreError::Bounds)?;
                let page_bytes = bytes
                    .get(page_start..page_end)
                    .ok_or(RemoteStoreError::Identity)?;
                let page = manifest
                    .inner
                    .verify_page(page_index, page_bytes)
                    .map_err(map_remote_admission_error)?;
                cached.insert(page_index, page);
            }
            *verified_pages
                .lock()
                .map_err(|_| RemoteStoreError::Unavailable)? = cached;
        }
        Ok(S3RemotePack {
            manifest,
            capability: grant.clone(),
            fence,
            verified_pages,
            prefetched_prefix_bytes: coalesced_prefix_len,
        })
    }

    /// Fetches one complete object extent, validates HTTP range metadata,
    /// verifies its Merkle page proof, then admits the store envelope.
    pub fn get_object(
        &self,
        pack: &S3RemotePack,
        object_id: ObjectId,
        fence: WorkFence,
        registry: &RelationAdmissionRegistry,
    ) -> Result<CheckedPackedObject, RemoteStoreError> {
        self.get_object_claim(
            pack,
            UntrustedObjectId::from_bytes(*object_id.as_bytes()),
            fence,
            registry,
        )
    }

    /// Fetches one complete object from an untrusted raw ID claim. The returned
    /// receipt contains an admitted `ObjectId` only after directory-page
    /// membership, the full envelope Merkle leaf, and backend-store typed
    /// identity checks all succeed.
    pub fn get_object_claim(
        &self,
        pack: &S3RemotePack,
        object_id: UntrustedObjectId,
        fence: WorkFence,
        registry: &RelationAdmissionRegistry,
    ) -> Result<CheckedPackedObject, RemoteStoreError> {
        self.validate_remote_pack(pack, fence)?;
        let (page_index, descriptor) = pack
            .manifest
            .inner
            .page_for_object_claim(object_id)
            .ok_or(RemoteStoreError::Identity)?;
        let page = self.load_verified_page(pack, page_index, descriptor, fence)?;
        self.get_object_in_page(pack, object_id, &page, fence, registry)
    }

    /// Fetches several complete objects with bounded parallel range reads.
    /// Distinct page proofs are resolved once and cached per opened pack; the
    /// complete object envelopes are then read and admitted in parallel. The
    /// returned objects retain the caller's input order. At most
    /// [`MAX_PARALLEL_PACK_READS`] HTTP requests are active at once.
    ///
    /// Empty input returns an empty vector. Duplicate IDs are allowed and
    /// produce duplicate checked results, while their shared page is fetched
    /// only once.
    pub fn get_objects(
        &self,
        pack: &S3RemotePack,
        object_ids: &[ObjectId],
        fence: WorkFence,
        registry: &RelationAdmissionRegistry,
    ) -> Result<Vec<CheckedPackedObject>, RemoteStoreError> {
        self.validate_remote_pack(pack, fence)?;
        if object_ids.len() > MAX_PACK_OBJECTS {
            return Err(RemoteStoreError::Bounds);
        }
        if object_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut object_pages = Vec::with_capacity(object_ids.len());
        let mut required_pages = Vec::new();
        let mut seen_pages = HashSet::new();
        for id in object_ids {
            let (page_index, descriptor) = pack
                .manifest
                .inner
                .page_for_object(*id)
                .ok_or(RemoteStoreError::Identity)?;
            object_pages.push((page_index, *id));
            if seen_pages.insert(page_index) {
                required_pages.push((page_index, descriptor.clone()));
            }
        }

        let pages = self.map_bounded(&required_pages, |(page_index, descriptor)| {
            self.load_verified_page(pack, *page_index, descriptor, fence)
        })?;
        let mut verified_by_index = HashMap::with_capacity(pages.len());
        for ((page_index, _), page) in required_pages.into_iter().zip(pages) {
            verified_by_index.insert(page_index, page);
        }
        let page_by_object = object_pages
            .into_iter()
            .map(|(page_index, id)| {
                verified_by_index
                    .get(&page_index)
                    .cloned()
                    .map(|page| (id, page))
                    .ok_or(RemoteStoreError::Identity)
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.map_bounded(&page_by_object, |(id, page)| {
            self.get_object_in_page(
                pack,
                UntrustedObjectId::from_bytes(*id.as_bytes()),
                page,
                fence,
                registry,
            )
        })
    }

    fn validate_remote_pack(
        &self,
        pack: &S3RemotePack,
        fence: WorkFence,
    ) -> Result<(), RemoteStoreError> {
        self.validate_read(&pack.capability, fence)?;
        if pack.fence != fence {
            return Err(RemoteStoreError::StaleFence);
        }
        Ok(())
    }

    fn load_verified_page(
        &self,
        pack: &S3RemotePack,
        page_index: usize,
        descriptor: &backend_store::artifact_pack::ArtifactPackPageDescriptor,
        fence: WorkFence,
    ) -> Result<ArtifactPackPage, RemoteStoreError> {
        self.validate_remote_pack(pack, fence)?;
        if let Some(page) = pack
            .verified_pages
            .lock()
            .map_err(|_| RemoteStoreError::Unavailable)?
            .get(&page_index)
            .cloned()
        {
            return Ok(page);
        }
        let page_end = descriptor
            .offset()
            .checked_add(u64::from(descriptor.bytes()))
            .and_then(|end| end.checked_sub(1))
            .ok_or(RemoteStoreError::Bounds)?;
        let mut page_response = self.request_range(
            &pack.capability,
            AllowedRange {
                start: descriptor.offset(),
                end_inclusive: page_end,
            },
        )?;
        let page_bytes = read_bounded(page_response.body_mut(), u64::from(descriptor.bytes()))?;
        let page = pack
            .manifest
            .inner
            .verify_page(page_index, &page_bytes)
            .map_err(map_remote_admission_error)?;
        let mut cache = pack
            .verified_pages
            .lock()
            .map_err(|_| RemoteStoreError::Unavailable)?;
        if let Some(page) = cache.get(&page_index) {
            return Ok(page.clone());
        }
        // The root directory bounds a pack to at most 128 pages, so the cache
        // has a fixed upper bound derived from the canonical pack format.
        if cache.len() >= pack.manifest.inner.pages().len() {
            return Err(RemoteStoreError::Bounds);
        }
        Ok(cache.entry(page_index).or_insert(page).clone())
    }

    fn get_object_in_page(
        &self,
        pack: &S3RemotePack,
        object_id: UntrustedObjectId,
        page: &ArtifactPackPage,
        fence: WorkFence,
        registry: &RelationAdmissionRegistry,
    ) -> Result<CheckedPackedObject, RemoteStoreError> {
        self.validate_remote_pack(pack, fence)?;
        let (_local_index, extent) = page
            .extent_claim(object_id)
            .ok_or(RemoteStoreError::Identity)?;
        let end_inclusive = extent
            .offset()
            .checked_add(extent.length())
            .and_then(|end| end.checked_sub(1))
            .ok_or(RemoteStoreError::Bounds)?;
        let range = AllowedRange {
            start: extent.offset(),
            end_inclusive,
        };
        let mut response = self.request_range(&pack.capability, range)?;
        let file = TempEnvelope::from_body(response.body_mut(), extent.length())?;
        if file.len != extent.length() {
            return Err(RemoteStoreError::Identity);
        }
        let mut envelope = file.open()?;
        let admitted = pack
            .manifest
            .inner
            .verify_object_envelope_in_page_claim(page, object_id, &mut envelope, registry)
            .map_err(map_remote_admission_error)?;
        if admitted.id().as_bytes() != object_id.as_bytes() {
            return Err(RemoteStoreError::Identity);
        }
        file.verify_snapshot()?;
        Ok(CheckedPackedObject {
            id: admitted.id(),
            verified_envelope: admitted.envelope(),
            file,
            pack_id: pack.manifest.pack_id(),
            layout_id: pack.manifest.layout_id(),
            fence,
        })
    }

    fn map_bounded<T: Sync, U: Send>(
        &self,
        items: &[T],
        operation: impl Fn(&T) -> Result<U, RemoteStoreError> + Sync,
    ) -> Result<Vec<U>, RemoteStoreError> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let next = AtomicUsize::new(0);
        let (sender, receiver) = mpsc::channel();
        let workers = items.len().min(MAX_PARALLEL_PACK_READS);
        let mut results = (0..items.len()).map(|_| None).collect::<Vec<_>>();
        thread::scope(|scope| {
            for _ in 0..workers {
                let sender = sender.clone();
                let operation = &operation;
                let next = &next;
                scope.spawn(move || {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(index) else {
                            break;
                        };
                        if sender.send((index, operation(item))).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(sender);
            for (index, result) in receiver {
                results[index] = Some(result);
            }
        });
        results
            .into_iter()
            .map(|result| result.ok_or(RemoteStoreError::Unavailable)?)
            .collect()
    }

    fn request_range(
        &self,
        grant: &PackReadCapability,
        range: AllowedRange,
    ) -> Result<ureq::http::Response<ureq::Body>, RemoteStoreError> {
        let span = range
            .end_inclusive
            .checked_sub(range.start)
            .and_then(|length| length.checked_add(1))
            .ok_or(RemoteStoreError::Bounds)?;
        if span == 0 || span > self.object_route.config.max_object_bytes {
            return Err(RemoteStoreError::Bounds);
        }
        let header = format!("bytes={}-{}", range.start, range.end_inclusive);
        let response = self.object_route.get_response(&grant.url, Some(&header))?;
        if response.status().as_u16() != 206 {
            return Err(RemoteStoreError::Protocol);
        }
        check_identity_response(&response)?;
        let content_range = response
            .headers()
            .get("content-range")
            .and_then(|value| value.to_str().ok())
            .ok_or(RemoteStoreError::Protocol)?;
        if parse_content_range(content_range)? != (range, grant.pack_bytes)
            || parse_content_length(&response)? != span
        {
            return Err(RemoteStoreError::Protocol);
        }
        check_optional_full_checksum(&response, &grant.sha256)?;
        Ok(response)
    }

    fn read_full_for_retry(
        &self,
        grant: &PackReadCapability,
        fence: WorkFence,
    ) -> Result<TempEnvelope, RemoteStoreError> {
        self.validate_read(grant, fence)?;
        if !grant.full_read_allowed {
            return Err(RemoteStoreError::ExistingUnverified);
        }
        let mut response = self.object_route.get_response(&grant.url, None)?;
        if response.status().as_u16() != 200 {
            return Err(RemoteStoreError::Protocol);
        }
        check_identity_response(&response)?;
        if parse_content_length(&response)? != grant.pack_bytes {
            return Err(RemoteStoreError::Identity);
        }
        check_optional_full_checksum(&response, &grant.sha256)?;
        let file = TempEnvelope::from_body(response.body_mut(), grant.pack_bytes)?;
        if file.sha256 != grant.sha256 {
            return Err(RemoteStoreError::Identity);
        }
        Ok(file)
    }

    fn validate_upload(
        &self,
        grant: &PackUploadCapability,
        fence: WorkFence,
    ) -> Result<(), RemoteStoreError> {
        if grant.fence != fence {
            return Err(RemoteStoreError::StaleFence);
        }
        if fence.fence == [0; 32] {
            return Err(RemoteStoreError::Capability);
        }
        if grant.pack_bytes == 0
            || grant.pack_bytes > self.object_route.config.max_object_bytes
            || grant.pack_bytes > MAX_PACK_BYTES
            || usize::try_from(grant.manifest_bytes).map_or(true, |size| {
                size < MIN_PACK_MANIFEST_BYTES || size > MAX_PACK_MANIFEST_BYTES
            })
            || !grant.if_none_match_signed
            || !grant.checksum_signed
        {
            return Err(RemoteStoreError::Capability);
        }
        self.object_route.validate_url(&grant.url)?;
        if !url_signs_headers(
            &grant.url,
            &["host", "if-none-match", "x-amz-checksum-sha256"],
        ) {
            return Err(RemoteStoreError::Capability);
        }
        ensure_unexpired(grant.expires_at_unix_seconds)
    }

    fn validate_read(
        &self,
        grant: &PackReadCapability,
        fence: WorkFence,
    ) -> Result<(), RemoteStoreError> {
        if grant.fence != fence {
            return Err(RemoteStoreError::StaleFence);
        }
        if fence.fence == [0; 32] {
            return Err(RemoteStoreError::Capability);
        }
        if grant.pack_bytes == 0
            || grant.pack_bytes > self.object_route.config.max_object_bytes
            || grant.pack_bytes > MAX_PACK_BYTES
            || usize::try_from(grant.manifest_bytes).map_or(true, |size| {
                size < MIN_PACK_MANIFEST_BYTES
                    || size > MAX_PACK_MANIFEST_BYTES
                    || u64::from(grant.manifest_bytes) >= grant.pack_bytes
            })
        {
            return Err(RemoteStoreError::Capability);
        }
        self.object_route.validate_url(&grant.url)?;
        // A signed `Range` header binds one exact range value, so it cannot
        // authorize both the manifest prefix and each later object extent.
        // The URL remains scoped to this pack key; each returned range is
        // narrowed and authenticated by the checked manifest/page proof.
        if !url_signs_headers(&grant.url, &["host"]) || url_signs_header(&grant.url, "range") {
            return Err(RemoteStoreError::Capability);
        }
        ensure_unexpired(grant.expires_at_unix_seconds)
    }

    fn receipt(
        &self,
        pack: &ImmutableS3Pack,
        fence: WorkFence,
        reused_after_412: bool,
    ) -> Result<StoredPackReceipt, RemoteStoreError> {
        Ok(StoredPackReceipt {
            pack_id: pack.pack_id(),
            layout_id: pack.layout_id(),
            pack_bytes: pack.len(),
            object_count: u32::try_from(pack.object_count())
                .map_err(|_| RemoteStoreError::Bounds)?,
            sha256: pack.sha256,
            fence,
            reused_after_412,
        })
    }
}

fn check_optional_full_checksum(
    response: &ureq::http::Response<ureq::Body>,
    expected_sha256: &[u8; 32],
) -> Result<(), RemoteStoreError> {
    let Some(value) = response.headers().get("x-amz-checksum-sha256") else {
        return Ok(());
    };
    let decoded = value
        .to_str()
        .ok()
        .and_then(|encoded| BASE64.decode(encoded).ok())
        .ok_or(RemoteStoreError::Identity)?;
    if decoded.as_slice() != &expected_sha256[..] {
        return Err(RemoteStoreError::Identity);
    }
    Ok(())
}

fn url_signs_header(url: &str, header: &str) -> bool {
    let Ok(uri) = url.parse::<ureq::http::Uri>() else {
        return false;
    };
    uri.path_and_query()
        .and_then(|path| path.query())
        .and_then(|query| {
            query.split('&').find_map(|field| {
                let (raw_name, raw_value) = field.split_once('=')?;
                let name = super::percent_decode_query_component(raw_name)?;
                if name.eq_ignore_ascii_case("X-Amz-SignedHeaders") {
                    super::percent_decode_query_component(raw_value)
                } else {
                    None
                }
            })
        })
        .is_some_and(|signed| {
            signed
                .split(';')
                .any(|value| value.trim().eq_ignore_ascii_case(header))
        })
}
