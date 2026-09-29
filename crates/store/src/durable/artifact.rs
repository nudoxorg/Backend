//! Bounded streaming admission for immutable artifact closures.
//!
//! The session writes each object directly to a private temporary file,
//! recomputes its typed version while bytes arrive, then links the checked file
//! into FileStore's existing object CAS. Closure membership is updated through
//! the existing persistent ManifestRelation tree. A returned receipt attests
//! to durable storage only; it is not a publication capability.

use super::closure_composer::PinnedStoredClosureReceipt;
use super::{
    ClosureId, FileStore, Hash, ObjectId, StoreError, TypedObject, artifact_fs, io_error, nodes,
};
use crate::UntrustedObjectId;
use crate::closure::ManifestRelation;
use backend_version::{
    IdContext, LazyTree, ObjectVersionHasher, PersistedTreeRoot, SchemaIdentity, TreeChange,
    UntrustedId, admit_canonical_root_claim,
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
};

pub(super) const OBJECT_IDENTITY_DOMAIN: &[u8] = b"store.object.v1\0";
pub(super) const OBJECT_IDENTITY_FIXED_BYTES: u64 = 1 + 2 + 1 + 32 + 32 + 8;
pub(super) const VERIFY_BUFFER_BYTES: usize = 16 * 1024;
const MANIFEST_PAGE_OBJECTS: usize = 128;

/// Resource limits for one artifact closure transfer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactBudget {
    /// Maximum number of put and delete operations in the delta.
    pub max_changes: usize,
    /// Maximum number of members in the complete resulting closure.
    pub max_closure_objects: usize,
    /// Maximum unique canonical payload bytes accepted by the session.
    pub max_payload_bytes: u64,
    /// Maximum bytes in one put frame, including a replayed frame.
    pub max_chunk_bytes: usize,
    /// Maximum put calls, including exact idempotent replays.
    pub max_put_calls: usize,
}

impl ArtifactBudget {
    /// Creates a byte- and count-bounded transfer policy.
    #[must_use]
    pub const fn new(
        max_changes: usize,
        max_closure_objects: usize,
        max_payload_bytes: u64,
        max_chunk_bytes: usize,
        max_put_calls: usize,
    ) -> Self {
        Self {
            max_changes,
            max_closure_objects,
            max_payload_bytes,
            max_chunk_bytes,
            max_put_calls,
        }
    }

    fn validate(self) -> Result<Self, StoreError> {
        if self.max_changes == 0
            || self.max_closure_objects == 0
            || self.max_payload_bytes == 0
            || self.max_chunk_bytes == 0
            || self.max_put_calls == 0
        {
            return Err(StoreError::Bounds);
        }
        Ok(self)
    }
}

/// Untrusted claim for the logical root of an artifact closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactClosureClaim(Hash);

impl ArtifactClosureClaim {
    /// Wraps fixed-width bytes received from a compiler or transport.
    #[must_use]
    pub const fn from_bytes(bytes: Hash) -> Self {
        Self(bytes)
    }

    /// Copies the claimed root bytes for protocol encoding.
    #[must_use]
    pub const fn as_bytes(&self) -> &Hash {
        &self.0
    }

    /// Creates a claim from an already admitted closure identity.
    #[must_use]
    pub const fn from_id(id: ClosureId) -> Self {
        Self(*id.as_bytes())
    }
}

/// Runtime schema and canonical identity claims for one streamed object.
///
/// The typed key is a schema claim because its canonical preimage is not part
/// of an object envelope. `put` recomputes the payload version and physical
/// object ID, which binds that key claim to the exact payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactObjectClaim {
    schema: SchemaIdentity,
    key: Hash,
    version: Hash,
    length: u64,
    object_id: Option<UntrustedObjectId>,
}

impl ArtifactObjectClaim {
    /// Declares one object without asserting a physical store identity.
    #[must_use]
    pub const fn new(schema: SchemaIdentity, key: Hash, version: Hash, length: u64) -> Self {
        Self {
            schema,
            key,
            version,
            length,
            object_id: None,
        }
    }

    /// Adds an optional physical CAS identity for `have` negotiation.
    #[must_use]
    pub const fn with_object_id(mut self, object_id: UntrustedObjectId) -> Self {
        self.object_id = Some(object_id);
        self
    }

    /// Returns the claimed runtime schema.
    #[must_use]
    pub const fn schema(self) -> SchemaIdentity {
        self.schema
    }

    /// Returns the untrusted logical key digest.
    #[must_use]
    pub const fn key(&self) -> &Hash {
        &self.key
    }

    /// Returns the untrusted complete version digest.
    #[must_use]
    pub const fn version(&self) -> &Hash {
        &self.version
    }

    /// Returns the declared canonical payload length.
    #[must_use]
    pub const fn length(self) -> u64 {
        self.length
    }

    /// Returns the optional untrusted physical ID claim used by a streaming
    /// closure builder after it has recomputed the object identity.
    #[must_use]
    pub(super) const fn expected_object_id(self) -> Option<UntrustedObjectId> {
        self.object_id
    }

    /// Returns the optional physical identity claim without admitting it.
    ///
    /// Stream encoders must recompute the identity from the complete payload
    /// and compare it with this claim before exposing their output.
    #[must_use]
    pub const fn object_id_claim(self) -> Option<UntrustedObjectId> {
        self.object_id
    }
}

/// One exact-base closure update.
///
/// Put claims must be strictly ordered by `(schema, key, version)`. Delete
/// claims must be strictly ordered by physical object ID. This gives the
/// session a canonical bounded plan before any payload is accepted.
#[derive(Clone, Debug)]
pub struct ArtifactPlan {
    base: Option<ArtifactClosureClaim>,
    target: ArtifactClosureClaim,
    puts: Vec<ArtifactObjectClaim>,
    deletes: Vec<UntrustedObjectId>,
}

impl ArtifactPlan {
    /// Creates a closure delta against an optional previously stored root.
    #[must_use]
    pub fn new(
        base: Option<ArtifactClosureClaim>,
        target: ArtifactClosureClaim,
        puts: Vec<ArtifactObjectClaim>,
        deletes: Vec<UntrustedObjectId>,
    ) -> Self {
        Self {
            base,
            target,
            puts,
            deletes,
        }
    }
}

/// `have` result aligned with the caller's requested object identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactHaveBitmap {
    present: Vec<bool>,
}

impl ArtifactHaveBitmap {
    /// Number of object claims in this negotiation result.
    #[must_use]
    pub fn len(&self) -> usize {
        self.present.len()
    }

    /// Whether the negotiation result contains no object claims.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.present.is_empty()
    }

    /// Returns whether the object at `index` is already checked in local CAS.
    #[must_use]
    pub fn is_present(&self, index: usize) -> bool {
        self.present.get(index).copied().unwrap_or(false)
    }

    /// Number of verified objects already in local CAS.
    #[must_use]
    pub fn present_count(&self) -> usize {
        self.present.iter().filter(|present| **present).count()
    }
}

/// Per-frame result from a streaming object put.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactChunkReceipt {
    /// True when this exact frame was already written and matched byte-for-byte.
    pub duplicate: bool,
    /// True when the complete object was admitted by this call.
    pub object_complete: bool,
    /// Exact bytes newly accepted from this frame.
    pub bytes_accepted: u64,
    /// Admitted physical object identity when the object is complete.
    pub object_id: Option<ObjectId>,
}

/// Bounded reader over one object whose physical and typed identities have
/// already been recomputed from its CAS payload.
pub struct ArtifactObjectReader {
    id: ObjectId,
    schema: SchemaIdentity,
    key: Hash,
    version: Hash,
    length: u64,
    file: File,
}

/// Checked header facts from a fully consumed object envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedObjectEnvelope {
    id: ObjectId,
    schema: SchemaIdentity,
    key: Hash,
    version: Hash,
    payload_len: u64,
}

/// Object envelope checked against one exact closure index.
///
/// This value cannot be constructed by callers. It proves index membership
/// and full object-envelope admission at the time it was produced; it does
/// not keep an otherwise unreachable closure pinned after the call returns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedClosureMember {
    closure: ClosureId,
    object: VerifiedObjectEnvelope,
}

impl VerifiedClosureMember {
    /// Returns the exact closure index that admitted this member.
    #[must_use]
    pub const fn closure(&self) -> ClosureId {
        self.closure
    }

    /// Returns the complete checked object envelope metadata.
    #[must_use]
    pub const fn object(&self) -> VerifiedObjectEnvelope {
        self.object
    }

    /// Returns the checked physical object ID.
    #[must_use]
    pub const fn object_id(&self) -> ObjectId {
        self.object.id()
    }
}

impl VerifiedObjectEnvelope {
    /// The recomputed physical object identity.
    #[must_use]
    pub const fn id(self) -> ObjectId {
        self.id
    }

    /// The checked runtime schema.
    #[must_use]
    pub const fn schema(self) -> SchemaIdentity {
        self.schema
    }

    /// The checked typed object key.
    #[must_use]
    pub const fn key(&self) -> &Hash {
        &self.key
    }

    /// The checked typed object version.
    #[must_use]
    pub const fn version(&self) -> &Hash {
        &self.version
    }

    /// The checked canonical payload length.
    #[must_use]
    pub const fn payload_len(self) -> u64 {
        self.payload_len
    }
}

impl From<&VerifiedObject> for VerifiedObjectEnvelope {
    fn from(object: &VerifiedObject) -> Self {
        Self {
            id: object.id,
            schema: object.schema,
            key: object.key,
            version: object.version,
            payload_len: object.payload_bytes,
        }
    }
}

/// Streams and verifies one complete canonical store object envelope.
///
/// The verifier rejects truncation, extra trailing bytes, typed identity
/// mismatches, physical ID mismatches, and malformed registered relations. It
/// uses a fixed payload buffer and retains only the bounded bytes needed by a
/// registered relation decoder.
pub fn admit_object_envelope<R: Read>(
    input: &mut R,
    maximum_bytes: usize,
    registry: &super::RelationAdmissionRegistry,
    expected_id: UntrustedObjectId,
) -> Result<VerifiedObjectEnvelope, StoreError> {
    if expected_id.as_bytes() == &[0; 32] {
        return Err(StoreError::Corrupt);
    }
    let mut magic = [0_u8; super::OBJECT_MAGIC.len()];
    input
        .read_exact(&mut magic)
        .map_err(map_envelope_read_error)?;
    if magic != *super::OBJECT_MAGIC {
        return Err(StoreError::Corrupt);
    }
    let mut id_bytes = [0_u8; 32];
    input
        .read_exact(&mut id_bytes)
        .map_err(map_envelope_read_error)?;
    if id_bytes != *expected_id.as_bytes() {
        return Err(StoreError::Corrupt);
    }
    let mut schema_bytes = [0_u8; 4];
    input
        .read_exact(&mut schema_bytes)
        .map_err(map_envelope_read_error)?;
    let schema = SchemaIdentity::new(
        schema_bytes[0],
        u16::from_le_bytes([schema_bytes[1], schema_bytes[2]]),
        schema_bytes[3],
    );
    let mut key = [0_u8; 32];
    let mut version = [0_u8; 32];
    input
        .read_exact(&mut key)
        .map_err(map_envelope_read_error)?;
    input
        .read_exact(&mut version)
        .map_err(map_envelope_read_error)?;
    let mut length_bytes = [0_u8; 8];
    input
        .read_exact(&mut length_bytes)
        .map_err(map_envelope_read_error)?;
    let payload_len = u64::from_le_bytes(length_bytes);
    let payload_capacity = usize::try_from(payload_len).map_err(|_| StoreError::Bounds)?;
    let envelope_len = object_header_bytes()
        .checked_add(payload_capacity)
        .ok_or(StoreError::Bounds)?;
    if envelope_len > maximum_bytes {
        return Err(StoreError::Bounds);
    }

    let mut version_hasher = if registry.contains_schema(schema) {
        None
    } else {
        Some(ObjectVersionHasher::new(schema, payload_capacity).map_err(|_| StoreError::Corrupt)?)
    };
    let mut object_hasher = blake3::Hasher::new();
    object_hasher.update(OBJECT_IDENTITY_DOMAIN);
    object_hasher.update(
        &OBJECT_IDENTITY_FIXED_BYTES
            .checked_add(payload_len)
            .ok_or(StoreError::Bounds)?
            .to_le_bytes(),
    );
    object_hasher.update(&[schema.domain()]);
    object_hasher.update(&schema.ty().to_le_bytes());
    object_hasher.update(&[schema.version()]);
    object_hasher.update(&key);
    object_hasher.update(&version);
    object_hasher.update(&length_bytes);

    let mut relation_bytes = if registry.contains_schema(schema) {
        if envelope_len > super::relation_object_limit()? {
            return Err(StoreError::Bounds);
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(payload_capacity)
            .map_err(|_| StoreError::Bounds)?;
        Some(bytes)
    } else {
        None
    };
    let mut remaining = payload_len;
    let mut buffer = [0_u8; VERIFY_BUFFER_BYTES];
    while remaining != 0 {
        let take = usize::try_from(remaining.min(VERIFY_BUFFER_BYTES as u64))
            .map_err(|_| StoreError::Bounds)?;
        let chunk = &mut buffer[..take];
        input.read_exact(chunk).map_err(map_envelope_read_error)?;
        if let Some(version_hasher) = &mut version_hasher {
            version_hasher
                .update(chunk)
                .map_err(|_| StoreError::Corrupt)?;
        }
        object_hasher.update(chunk);
        if let Some(bytes) = &mut relation_bytes {
            bytes.extend_from_slice(chunk);
        }
        remaining = remaining
            .checked_sub(u64::try_from(take).map_err(|_| StoreError::Bounds)?)
            .ok_or(StoreError::Bounds)?;
    }
    let mut trailing = [0_u8; 1];
    if input
        .read(&mut trailing)
        .map_err(|error| io_error(&error))?
        != 0
    {
        return Err(StoreError::Corrupt);
    }
    if version_hasher
        .map(|hasher| hasher.finish().map_err(|_| StoreError::Corrupt))
        .transpose()?
        .is_some_and(|actual| actual != version)
        || *object_hasher.finalize().as_bytes() != id_bytes
    {
        return Err(StoreError::Corrupt);
    }
    if let Some(bytes) = relation_bytes.as_deref() {
        registry.admit_relation_object(schema, &key, &version, bytes)?;
    }
    Ok(VerifiedObjectEnvelope {
        id: ObjectId::from_bytes(id_bytes),
        schema,
        key,
        version,
        payload_len,
    })
}

impl ArtifactObjectReader {
    /// The checked physical CAS identity.
    #[must_use]
    pub const fn id(&self) -> ObjectId {
        self.id
    }

    /// The checked runtime schema.
    #[must_use]
    pub const fn schema(&self) -> SchemaIdentity {
        self.schema
    }

    /// The checked typed key.
    #[must_use]
    pub const fn key(&self) -> &Hash {
        &self.key
    }

    /// The checked typed version.
    #[must_use]
    pub const fn version(&self) -> &Hash {
        &self.version
    }

    /// The canonical payload length.
    #[must_use]
    pub const fn payload_len(&self) -> u64 {
        self.length
    }

    /// Reads one bounded payload range from the verified immutable envelope.
    /// The requested buffer is truncated at the end of the payload.
    pub fn read_payload_range(
        &mut self,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, StoreError> {
        if offset > self.length {
            return Err(StoreError::Bounds);
        }
        let available = self.length.checked_sub(offset).ok_or(StoreError::Bounds)?;
        let count = usize::try_from(
            available.min(u64::try_from(output.len()).map_err(|_| StoreError::Bounds)?),
        )
        .map_err(|_| StoreError::Bounds)?;
        if count == 0 {
            return Ok(0);
        }
        let start = u64::try_from(object_header_bytes())
            .map_err(|_| StoreError::Bounds)?
            .checked_add(offset)
            .ok_or(StoreError::Bounds)?;
        self.file
            .seek(SeekFrom::Start(start))
            .map_err(|error| io_error(&error))?;
        self.file
            .read_exact(&mut output[..count])
            .map_err(|error| io_error(&error))?;
        Ok(count)
    }

    /// Writes the exact checked physical envelope without building an
    /// envelope-sized buffer in memory.
    pub fn write_envelope<W: Write + ?Sized>(
        &mut self,
        output: &mut W,
        maximum_bytes: usize,
    ) -> Result<u64, StoreError> {
        let encoded_len = write_object_envelope_header(
            output,
            self.id,
            self.schema,
            &self.key,
            &self.version,
            self.length,
            maximum_bytes,
        )?;
        let mut remaining = self.length;
        let mut offset = 0_u64;
        let mut buffer = [0_u8; VERIFY_BUFFER_BYTES];
        while remaining != 0 {
            let take = usize::try_from(remaining.min(VERIFY_BUFFER_BYTES as u64))
                .map_err(|_| StoreError::Bounds)?;
            self.read_payload_range(offset, &mut buffer[..take])?;
            output
                .write_all(&buffer[..take])
                .map_err(|error| io_error(&error))?;
            offset = offset
                .checked_add(u64::try_from(take).map_err(|_| StoreError::Bounds)?)
                .ok_or(StoreError::Bounds)?;
            remaining = remaining
                .checked_sub(u64::try_from(take).map_err(|_| StoreError::Bounds)?)
                .ok_or(StoreError::Bounds)?;
        }
        u64::try_from(encoded_len).map_err(|_| StoreError::Bounds)
    }
}

/// Streams the checked store object envelope for an admitted typed object.
/// This writes the exact local CAS ABI without constructing a second
/// object-sized buffer.
pub fn write_object_envelope<W: Write + ?Sized>(
    object: &TypedObject,
    output: &mut W,
    maximum_bytes: usize,
) -> Result<u64, StoreError> {
    let length = u64::try_from(object.bytes().len()).map_err(|_| StoreError::Bounds)?;
    let encoded_len = write_object_envelope_header(
        output,
        object.id(),
        object.schema(),
        object.key(),
        object.version(),
        length,
        maximum_bytes,
    )?;
    output
        .write_all(object.bytes())
        .map_err(|error| io_error(&error))?;
    u64::try_from(encoded_len).map_err(|_| StoreError::Bounds)
}

/// Streams a claimed canonical payload into a seekable private envelope file.
///
/// The function reserves the canonical envelope header, copies exactly the
/// declared payload length through a fixed-size buffer, computes the same
/// physical ObjectId as local CAS admission, patches that ID into the header,
/// and runs the store-owned envelope verifier over the completed extent. The
/// output is provisional until this function returns successfully; callers
/// must discard it after any error. The reader is advanced by exactly the
/// declared payload length, so it can be a bounded view into a larger stream.
///
/// # Errors
/// Returns `Bounds` for an envelope above `maximum_bytes`, `Corrupt` for a
/// truncated payload or mismatched typed claim, and `Io` for reader or writer
/// failures.
pub fn write_streamed_object_envelope<R: Read, W: Read + Write + Seek>(
    claim: ArtifactObjectClaim,
    payload: &mut R,
    output: &mut W,
    maximum_bytes: usize,
    registry: &super::RelationAdmissionRegistry,
) -> Result<VerifiedObjectEnvelope, StoreError> {
    let schema = claim.schema();
    let key = *claim.key();
    let version = *claim.version();
    let payload_len = claim.length();
    let start = output.stream_position().map_err(|error| io_error(&error))?;
    let placeholder = ObjectId::from_bytes([0; 32]);
    let encoded_len = write_object_envelope_header(
        output,
        placeholder,
        schema,
        &key,
        &version,
        payload_len,
        maximum_bytes,
    )?;

    let mut object_hasher = blake3::Hasher::new();
    object_hasher.update(OBJECT_IDENTITY_DOMAIN);
    object_hasher.update(
        &OBJECT_IDENTITY_FIXED_BYTES
            .checked_add(payload_len)
            .ok_or(StoreError::Bounds)?
            .to_le_bytes(),
    );
    object_hasher.update(&[schema.domain()]);
    object_hasher.update(&schema.ty().to_le_bytes());
    object_hasher.update(&[schema.version()]);
    object_hasher.update(&key);
    object_hasher.update(&version);
    object_hasher.update(&payload_len.to_le_bytes());

    let mut remaining = payload_len;
    let mut buffer = [0_u8; VERIFY_BUFFER_BYTES];
    while remaining != 0 {
        let count = usize::try_from(remaining.min(VERIFY_BUFFER_BYTES as u64))
            .map_err(|_| StoreError::Bounds)?;
        payload
            .read_exact(&mut buffer[..count])
            .map_err(map_envelope_read_error)?;
        output
            .write_all(&buffer[..count])
            .map_err(|error| io_error(&error))?;
        object_hasher.update(&buffer[..count]);
        remaining = remaining
            .checked_sub(u64::try_from(count).map_err(|_| StoreError::Bounds)?)
            .ok_or(StoreError::Bounds)?;
    }

    let id_bytes = *object_hasher.finalize().as_bytes();
    if claim
        .object_id_claim()
        .is_some_and(|expected| expected.as_bytes() != &id_bytes)
    {
        return Err(StoreError::Corrupt);
    }
    let id = ObjectId::from_bytes(id_bytes);
    let id_offset = start
        .checked_add(u64::try_from(super::OBJECT_MAGIC.len()).map_err(|_| StoreError::Bounds)?)
        .ok_or(StoreError::Bounds)?;
    let end = start
        .checked_add(u64::try_from(encoded_len).map_err(|_| StoreError::Bounds)?)
        .ok_or(StoreError::Bounds)?;
    output
        .seek(SeekFrom::Start(id_offset))
        .map_err(|error| io_error(&error))?;
    output
        .write_all(id.as_bytes())
        .map_err(|error| io_error(&error))?;
    output
        .seek(SeekFrom::Start(start))
        .map_err(|error| io_error(&error))?;
    let verified = {
        let mut extent =
            (&mut *output).take(u64::try_from(encoded_len).map_err(|_| StoreError::Bounds)?);
        admit_object_envelope(
            &mut extent,
            maximum_bytes,
            registry,
            UntrustedObjectId::from_bytes(id_bytes),
        )?
    };
    if verified.id() != id
        || verified.schema() != schema
        || verified.key() != &key
        || verified.version() != &version
        || verified.payload_len() != payload_len
    {
        return Err(StoreError::Corrupt);
    }
    output
        .seek(SeekFrom::Start(end))
        .map_err(|error| io_error(&error))?;
    Ok(verified)
}

fn write_object_envelope_header<W: Write + ?Sized>(
    output: &mut W,
    id: ObjectId,
    schema: SchemaIdentity,
    key: &Hash,
    version: &Hash,
    length: u64,
    maximum_bytes: usize,
) -> Result<usize, StoreError> {
    let encoded_len = object_header_bytes()
        .checked_add(usize::try_from(length).map_err(|_| StoreError::Bounds)?)
        .ok_or(StoreError::Bounds)?;
    if encoded_len > maximum_bytes {
        return Err(StoreError::Bounds);
    }
    output
        .write_all(super::OBJECT_MAGIC)
        .and_then(|()| output.write_all(id.as_bytes()))
        .and_then(|()| output.write_all(&[schema.domain()]))
        .and_then(|()| output.write_all(&schema.ty().to_le_bytes()))
        .and_then(|()| output.write_all(&[schema.version()]))
        .and_then(|()| output.write_all(key))
        .and_then(|()| output.write_all(version))
        .and_then(|()| output.write_all(&length.to_le_bytes()))
        .map_err(|error| io_error(&error))?;
    Ok(encoded_len)
}

/// Storage-only receipt for a complete, durably reachable closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoredClosureReceipt {
    closure: ClosureId,
    object_count: u64,
    payload_bytes: u64,
    bytes_written: u64,
    bytes_verified: u64,
}

impl StoredClosureReceipt {
    pub(super) const fn from_verified(
        closure: ClosureId,
        object_count: u64,
        payload_bytes: u64,
        bytes_written: u64,
        bytes_verified: u64,
    ) -> Self {
        Self {
            closure,
            object_count,
            payload_bytes,
            bytes_written,
            bytes_verified,
        }
    }

    /// The checked logical closure identity.
    #[must_use]
    pub const fn closure(self) -> ClosureId {
        self.closure
    }

    /// Number of object members proven reachable in local CAS.
    #[must_use]
    pub const fn object_count(self) -> u64 {
        self.object_count
    }

    /// Canonical payload bytes streamed by the session, excluding replays.
    #[must_use]
    pub const fn payload_bytes(self) -> u64 {
        self.payload_bytes
    }

    /// Newly created immutable object, relation-index, and closure bytes.
    #[must_use]
    pub const fn bytes_written(self) -> u64 {
        self.bytes_written
    }

    /// Canonical payload bytes rechecked before the receipt was returned.
    #[must_use]
    pub const fn bytes_verified(self) -> u64 {
        self.bytes_verified
    }
}

/// Reusable local sink for content-addressed compiler artifacts.
#[derive(Clone, Debug)]
pub struct ArtifactSink {
    store: FileStore,
    budget: ArtifactBudget,
}

impl FileStore {
    /// Creates a bounded artifact sink over this store's existing object CAS.
    #[must_use]
    pub fn artifact_sink(&self, budget: ArtifactBudget) -> ArtifactSink {
        ArtifactSink {
            store: self.clone(),
            budget,
        }
    }

    /// Verifies one complete object envelope using a reusable bounded buffer
    /// and returns its checked typed metadata without materializing the
    /// payload.
    ///
    /// This authenticates the object identity and typed version, but does not
    /// prove membership in any closure. Callers must establish membership
    /// through a checked closure index when that is required.
    pub fn verify_object_claim(
        &self,
        claim: UntrustedObjectId,
    ) -> Result<VerifiedObjectEnvelope, StoreError> {
        let verified = verify_object_file(self, claim, None)?.ok_or(StoreError::Corrupt)?;
        Ok(VerifiedObjectEnvelope::from(&verified))
    }

    pub(super) fn verify_closure_member_limited(
        &self,
        id: ObjectId,
        maximum_payload_bytes: Option<u64>,
    ) -> Result<(VerifiedObjectEnvelope, Option<VerifiedRelationReferences>), StoreError> {
        let Some((verified, _file)) = verify_object_file_reader_limited(
            self,
            UntrustedObjectId::from_bytes(*id.as_bytes()),
            None,
            maximum_payload_bytes,
        )?
        else {
            return Err(StoreError::Corrupt);
        };
        if verified.id != id {
            return Err(StoreError::Corrupt);
        }
        Ok((
            VerifiedObjectEnvelope::from(&verified),
            verified.relation_references,
        ))
    }

    /// Opens a checked closure-index claim without loading its member payloads.
    ///
    /// The returned index authenticates the descriptor and persistent
    /// manifest tree. Membership proves that an ID belongs to the logical
    /// closure; callers that consume object bytes must still verify each
    /// member with [`Self::verify_object_claim`] or
    /// [`Self::write_verified_object_payload`].
    pub fn open_closure_claim(
        &self,
        claim: ArtifactClosureClaim,
    ) -> Result<super::nodes::DurableManifest, StoreError> {
        self.open_closure(ClosureId::from_bytes(claim.0))
    }

    /// Admits a closure claim by checking its stored descriptor and manifest
    /// index, without loading any member object payloads.
    ///
    /// The returned ID is derived from the authenticated manifest descriptor.
    /// Callers that consume members must still prove membership and verify the
    /// corresponding object envelope.
    pub fn admit_closure_claim(
        &self,
        claim: ArtifactClosureClaim,
    ) -> Result<ClosureId, StoreError> {
        let manifest = self.open_closure_claim(claim)?;
        let id = manifest.id();
        if id.as_bytes() != claim.as_bytes() {
            return Err(StoreError::Corrupt);
        }
        Ok(id)
    }

    /// Opens a checked metadata-only view of a stored closure index.
    ///
    /// This is an alias for [`Self::open_closure_claim`] for callers that
    /// already hold the admitted logical closure ID.
    pub fn read_closure_index(
        &self,
        id: ClosureId,
    ) -> Result<super::nodes::DurableManifest, StoreError> {
        self.open_closure(id)
    }

    /// Verifies an object completely before writing its payload to `output`,
    /// then copies from the same already-open CAS file through a fixed-size
    /// buffer. No bytes reach the output on an identity, schema, or size
    /// mismatch. The output should still be treated as provisional until this
    /// method returns successfully; callers should discard it on any I/O
    /// error.
    ///
    /// CAS files are opened through the immutable-file boundary, which checks
    /// the object path is a regular, single-link file and removes write
    /// permission before verification. Verification and copying use the same
    /// file descriptor, so the method never hashes one inode and reopens a
    /// different object for output.
    ///
    /// # Errors
    /// Returns [`StoreError::Corrupt`] for a missing, malformed, or mismatched
    /// object; [`StoreError::Bounds`] if the object exceeds
    /// `maximum_payload_bytes`; and [`StoreError::Io`] for file or output
    /// failures.
    pub fn write_verified_object_payload<W: Write + ?Sized>(
        &self,
        claim: UntrustedObjectId,
        maximum_payload_bytes: u64,
        output: &mut W,
    ) -> Result<VerifiedObjectEnvelope, StoreError> {
        let Some((verified, file)) =
            verify_object_file_reader_limited(self, claim, None, Some(maximum_payload_bytes))?
        else {
            return Err(StoreError::Corrupt);
        };
        let mut reader = ArtifactObjectReader {
            id: verified.id,
            schema: verified.schema,
            key: verified.key,
            version: verified.version,
            length: verified.payload_bytes,
            file,
        };
        let mut remaining = reader.length;
        let mut offset = 0_u64;
        let mut buffer = [0_u8; VERIFY_BUFFER_BYTES];
        while remaining != 0 {
            let count = usize::try_from(remaining.min(VERIFY_BUFFER_BYTES as u64))
                .map_err(|_| StoreError::Bounds)?;
            let read = reader.read_payload_range(offset, &mut buffer[..count])?;
            if read != count {
                return Err(StoreError::Corrupt);
            }
            output
                .write_all(&buffer[..read])
                .map_err(|error| io_error(&error))?;
            let advance = u64::try_from(read).map_err(|_| StoreError::Bounds)?;
            offset = offset.checked_add(advance).ok_or(StoreError::Bounds)?;
            remaining = remaining.checked_sub(advance).ok_or(StoreError::Bounds)?;
        }
        Ok(VerifiedObjectEnvelope {
            id: reader.id,
            schema: reader.schema,
            key: reader.key,
            version: reader.version,
            payload_len: reader.length,
        })
    }

    /// Streams borrowed payload chunks while the complete object is being
    /// authenticated, and returns checked envelope metadata only after the
    /// payload version, physical ID, registered relation, and exact length
    /// all pass.
    ///
    /// The callback sees *tentative* bytes: a later read, hash, or relation
    /// failure can still make this method return an error after the callback
    /// received earlier chunks. The slice is borrowed from one fixed-size
    /// verifier buffer and cannot outlive the callback invocation. Callers
    /// that stage chunks in an output must keep that output provisional and
    /// discard or roll it back unless this method returns `Ok(Some(_))` and
    /// the returned metadata matches their independently admitted claim.
    ///
    /// This operation reads the immutable CAS payload once. Missing objects
    /// return `Ok(None)`; malformed or oversized objects return an error.
    pub fn stream_tentative_object_payload(
        &self,
        claim: UntrustedObjectId,
        maximum_payload_bytes: u64,
        mut consume_chunk: impl FnMut(&[u8]) -> Result<(), StoreError>,
    ) -> Result<Option<VerifiedObjectEnvelope>, StoreError> {
        let Some((verified, _file)) = verify_object_file_reader_limited_with_payload(
            &self.store,
            claim,
            None,
            Some(maximum_payload_bytes),
            &mut consume_chunk,
        )?
        else {
            return Ok(None);
        };
        Ok(Some(VerifiedObjectEnvelope::from(&verified)))
    }

    /// Reopens a stored closure and proves every member's envelope, typed
    /// identities, relation children, and CAS path under the supplied budget.
    ///
    /// This is the cold-reopen counterpart to [`ArtifactSession::finish`]. It
    /// never loads a complete object payload into memory.
    pub fn reopen_stored_closure(
        &self,
        claim: ArtifactClosureClaim,
        budget: ArtifactBudget,
    ) -> Result<StoredClosureReceipt, StoreError> {
        let budget = budget.validate()?;
        let closure = ClosureId::from_bytes(claim.0);
        let _process_lock = self.acquire_process_lock()?;
        let staging_root = artifact_fs::prepare_staging_root(self)?;
        artifact_fs::reap_stale_sessions(self, &staging_root)?;
        let facts = verify_stored_closure(self, closure, budget.max_closure_objects)?;
        Ok(StoredClosureReceipt {
            closure,
            object_count: facts.object_count,
            payload_bytes: 0,
            bytes_written: 0,
            bytes_verified: facts.payload_bytes,
        })
    }

    /// Reopens a complete stored closure while preventing collection of its
    /// members until the caller has finished using the checked receipt.
    ///
    /// The pin is acquired before any closure read, so a concurrent collector
    /// cannot remove an input object between proof and result admission. A
    /// recovered compiler assignment keeps this lease across its validation
    /// and publication attempt.
    pub fn reopen_pinned_stored_closure(
        &self,
        claim: ArtifactClosureClaim,
        budget: ArtifactBudget,
    ) -> Result<PinnedStoredClosureReceipt, StoreError> {
        let pin = self.acquire_gc_pin()?;
        let receipt = self.reopen_stored_closure(claim, budget)?;
        Ok(PinnedStoredClosureReceipt::from_parts(receipt, pin))
    }
}

impl PinnedStoredClosureReceipt {
    /// Turns a fully streamed cold-reopen proof into a result publication receipt.
    ///
    /// The worker's payload count is only an untrusted comparison value: the
    /// returned receipt takes its payload count from the independently
    /// verified CAS envelopes. This method is available only through the
    /// pinned handle, which the caller must retain until selection completes.
    pub fn admit_recovered_payload(
        &self,
        expected_payload_bytes: u64,
    ) -> Result<StoredClosureReceipt, StoreError> {
        let receipt = self.receipt();
        if expected_payload_bytes == 0
            || receipt.payload_bytes != 0
            || receipt.bytes_verified != expected_payload_bytes
        {
            return Err(StoreError::Corrupt);
        }
        Ok(StoredClosureReceipt {
            payload_bytes: receipt.bytes_verified,
            ..receipt
        })
    }
}

impl ArtifactSink {
    /// Authenticates candidate object IDs against this sink's local CAS.
    /// A missing file is reported as absent; a malformed or conflicting file
    /// fails the whole negotiation.
    pub fn have(&self, object_ids: &[UntrustedObjectId]) -> Result<ArtifactHaveBitmap, StoreError> {
        let budget = self.budget.validate()?;
        if object_ids.len() > budget.max_changes {
            return Err(StoreError::Bounds);
        }
        let mut present = Vec::with_capacity(object_ids.len());
        for claim in object_ids {
            match verify_object_file(&self.store, *claim, None)? {
                None => present.push(false),
                Some(_) => present.push(true),
            }
        }
        Ok(ArtifactHaveBitmap { present })
    }

    /// Verifies that one complete object envelope is a member of an exact
    /// stored closure index.
    ///
    /// The shared GC pin covers the membership lookup and full streamed
    /// envelope verification. `None` means the index does not include the
    /// requested identity; a listed but missing or corrupt CAS object fails
    /// with [`StoreError::Corrupt`].
    pub fn verify_closure_member(
        &self,
        closure: ClosureId,
        claim: UntrustedObjectId,
    ) -> Result<Option<VerifiedClosureMember>, StoreError> {
        let budget = self.budget.validate()?;
        let _gc_pin = self.store.acquire_gc_pin()?;
        let index = self.store.open_closure(closure)?;
        let id = ObjectId::from_bytes(*claim.as_bytes());
        if !index.contains_object_id(id)? {
            return Ok(None);
        }
        let (object, _) = self
            .store
            .verify_closure_member_limited(id, Some(budget.max_payload_bytes))?;
        Ok(Some(VerifiedClosureMember { closure, object }))
    }

    /// Verifies membership from a wire closure claim without requiring callers
    /// to construct an admitted [`ClosureId`] from untrusted bytes.
    pub fn verify_closure_member_claim(
        &self,
        closure: ArtifactClosureClaim,
        claim: UntrustedObjectId,
    ) -> Result<Option<VerifiedClosureMember>, StoreError> {
        self.verify_closure_member(ClosureId::from_bytes(*closure.as_bytes()), claim)
    }

    /// Opens a bounded range reader after checking the stored typed key,
    /// recomputing the typed version, and verifying envelope length and ID.
    pub fn open_object(
        &self,
        object_id: UntrustedObjectId,
    ) -> Result<Option<ArtifactObjectReader>, StoreError> {
        let Some((verified, file)) = verify_object_file_reader(&self.store, object_id, None)?
        else {
            return Ok(None);
        };
        Ok(Some(ArtifactObjectReader {
            id: verified.id,
            schema: verified.schema,
            key: verified.key,
            version: verified.version,
            length: verified.payload_bytes,
            file,
        }))
    }

    /// Opens a checked object reader only when its payload is within the
    /// caller's explicit byte bound. Missing objects return `None`; malformed
    /// or oversized objects remain errors.
    pub fn open_object_limited(
        &self,
        object_id: UntrustedObjectId,
        maximum_payload_bytes: u64,
    ) -> Result<Option<ArtifactObjectReader>, StoreError> {
        let Some((verified, file)) = verify_object_file_reader_limited(
            &self.store,
            object_id,
            None,
            Some(maximum_payload_bytes),
        )?
        else {
            return Ok(None);
        };
        Ok(Some(ArtifactObjectReader {
            id: verified.id,
            schema: verified.schema,
            key: verified.key,
            version: verified.version,
            length: verified.payload_bytes,
            file,
        }))
    }

    /// Starts the affine receive session. The session is the only value that
    /// can accept frames or finish the declared closure.
    pub fn begin(self, plan: ArtifactPlan) -> Result<ArtifactSession, StoreError> {
        ArtifactSession::begin(self, plan)
    }
}

/// One affine, bounded object receive and closure admission session.
pub struct ArtifactSession {
    store: FileStore,
    gc_pin: Option<super::layout::GcPinLease>,
    budget: ArtifactBudget,
    plan: ArtifactPlan,
    object_ids: Vec<Option<ObjectId>>,
    next_put: usize,
    pending: Option<PendingObject>,
    staging_name: String,
    staging_parent: artifact_fs::ArtifactDirectory,
    staging_dir: artifact_fs::ArtifactDirectory,
    lease_file: Option<File>,
    put_calls: usize,
    payload_bytes: u64,
    bytes_written: u64,
    bytes_verified_at_begin: u64,
    protocol_failed: bool,
}

struct PendingObject {
    index: usize,
    name: String,
    file: File,
    claim: ArtifactObjectClaim,
    written: u64,
    version_hasher: Option<ObjectVersionHasher>,
    object_hasher: blake3::Hasher,
    relation_bytes: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct VerifiedClosureFacts {
    pub(super) object_count: u64,
    pub(super) payload_bytes: u64,
}

impl ArtifactSession {
    fn begin(sink: ArtifactSink, plan: ArtifactPlan) -> Result<Self, StoreError> {
        let budget = sink.budget.validate()?;
        validate_plan(&sink.store, &plan, budget)?;

        // Session creation and stale-session cleanup serialize with FileStore
        // recovery, while the per-session OS lock protects a live stream from
        // another process's cleanup pass.
        let gc_pin = sink.store.acquire_gc_pin()?;
        let _process_lock = sink.store.acquire_process_lock()?;
        let staging_root = artifact_fs::prepare_staging_root(&sink.store)?;
        artifact_fs::reap_stale_sessions(&sink.store, &staging_root)?;
        let staging = artifact_fs::create_session(&staging_root)?;
        drop(_process_lock);

        let mut session = Self {
            store: sink.store,
            gc_pin: Some(gc_pin),
            budget,
            object_ids: vec![None; plan.puts.len()],
            plan,
            next_put: 0,
            pending: None,
            staging_name: staging.name,
            staging_parent: staging.parent,
            staging_dir: staging.directory,
            lease_file: Some(staging.lease),
            put_calls: 0,
            payload_bytes: 0,
            bytes_written: 0,
            bytes_verified_at_begin: 0,
            protocol_failed: false,
        };

        // Negotiate only IDs the producer actually knows. The descriptor is
        // checked against the claimed schema, key, version, and byte length.
        for index in 0..session.plan.puts.len() {
            let Some(claimed_id) = session.plan.puts[index].object_id else {
                continue;
            };
            if let Some(found) =
                verify_object_file(&session.store, claimed_id, Some(session.plan.puts[index]))?
            {
                session.bytes_verified_at_begin = session
                    .bytes_verified_at_begin
                    .checked_add(found.payload_bytes)
                    .ok_or(StoreError::Bounds)?;
                session.object_ids[index] = Some(found.id);
                session.index_relation_reference(found)?;
            }
        }
        session.advance_past_present();

        if let Some(base) = session.plan.base {
            let facts = verify_stored_closure(
                &session.store,
                ClosureId::from_bytes(base.0),
                session.budget.max_closure_objects,
            )?;
            session.bytes_verified_at_begin = session
                .bytes_verified_at_begin
                .checked_add(facts.payload_bytes)
                .ok_or(StoreError::Bounds)?;
        } else if !session.plan.deletes.is_empty() {
            return Err(StoreError::WrongBase);
        }

        session.finish_empty_objects()?;
        Ok(session)
    }

    /// Returns `have` results for put claims carrying an expected physical ID.
    #[must_use]
    pub fn have_bitmap(&self) -> ArtifactHaveBitmap {
        ArtifactHaveBitmap {
            present: self.object_ids.iter().map(Option::is_some).collect(),
        }
    }

    /// Writes one contiguous object chunk. Exact replays of a previously
    /// written range are idempotent; gaps, overlaps, reordering, and conflicts
    /// poison the affine session.
    pub fn put(
        &mut self,
        object_index: usize,
        offset: u64,
        bytes: &[u8],
    ) -> Result<ArtifactChunkReceipt, StoreError> {
        if self.protocol_failed || self.put_calls >= self.budget.max_put_calls {
            self.protocol_failed = true;
            return Err(StoreError::Bounds);
        }
        self.put_calls = self.put_calls.checked_add(1).ok_or(StoreError::Bounds)?;
        if bytes.is_empty() || bytes.len() > self.budget.max_chunk_bytes {
            self.protocol_failed = true;
            return Err(StoreError::Bounds);
        }
        if object_index >= self.plan.puts.len() {
            self.protocol_failed = true;
            return Err(StoreError::Corrupt);
        }

        if object_index < self.next_put {
            return self.replay_completed(object_index, offset, bytes);
        }
        if object_index != self.next_put {
            self.protocol_failed = true;
            return Err(StoreError::Corrupt);
        }
        if self.object_ids[object_index].is_some() {
            return self.replay_have(object_index, offset, bytes);
        }
        if self.pending.is_none() {
            self.pending = Some(self.open_pending(object_index)?);
        }
        let pending = self.pending.as_mut().ok_or(StoreError::Corrupt)?;
        if pending.index != object_index {
            self.protocol_failed = true;
            return Err(StoreError::Corrupt);
        }
        let frame_len = u64::try_from(bytes.len()).map_err(|_| StoreError::Bounds)?;
        let frame_end = offset.checked_add(frame_len).ok_or(StoreError::Bounds)?;
        if offset < pending.written {
            if frame_end > pending.written {
                self.protocol_failed = true;
                return Err(StoreError::Corrupt);
            }
            compare_payload_range(&mut pending.file, object_header_bytes(), offset, bytes)?;
            return Ok(ArtifactChunkReceipt {
                duplicate: true,
                object_complete: false,
                bytes_accepted: 0,
                object_id: None,
            });
        }
        if offset != pending.written || frame_end > pending.claim.length {
            self.protocol_failed = true;
            return Err(StoreError::Corrupt);
        }

        pending
            .file
            .seek(SeekFrom::End(0))
            .map_err(|error| io_error(&error))?;
        pending
            .file
            .write_all(bytes)
            .map_err(|error| io_error(&error))?;
        if let Some(version_hasher) = &mut pending.version_hasher {
            version_hasher
                .update(bytes)
                .map_err(|_| StoreError::Corrupt)?;
        }
        pending.object_hasher.update(bytes);
        if let Some(relation_bytes) = &mut pending.relation_bytes {
            relation_bytes.extend_from_slice(bytes);
        }
        pending.written = frame_end;
        self.payload_bytes = self
            .payload_bytes
            .checked_add(frame_len)
            .ok_or(StoreError::Bounds)?;
        if self.payload_bytes > self.budget.max_payload_bytes {
            self.protocol_failed = true;
            return Err(StoreError::Bounds);
        }

        if pending.written != pending.claim.length {
            return Ok(ArtifactChunkReceipt {
                duplicate: false,
                object_complete: false,
                bytes_accepted: frame_len,
                object_id: None,
            });
        }
        let id = self.commit_pending()?;
        self.advance_past_present();
        Ok(ArtifactChunkReceipt {
            duplicate: false,
            object_complete: true,
            bytes_accepted: frame_len,
            object_id: Some(id),
        })
    }

    /// Finalizes the exact target closure and returns storage facts only.
    pub fn finish(mut self) -> Result<StoredClosureReceipt, StoreError> {
        self.finish_inner()
    }

    /// Finalizes the closure without releasing the session's GC pin.
    ///
    /// The returned receipt must remain alive until the caller either selects
    /// the closure into durable authority roots or abandons the candidate. The
    /// existing session pin is transferred rather than reacquired, so a
    /// concurrent collector cannot sweep the unselected result in between.
    pub fn finish_pinned(mut self) -> Result<PinnedStoredClosureReceipt, StoreError> {
        let receipt = self.finish_inner()?;
        let pin = self.gc_pin.take().ok_or(StoreError::Corrupt)?;
        Ok(PinnedStoredClosureReceipt::from_parts(receipt, pin))
    }

    fn finish_inner(&mut self) -> Result<StoredClosureReceipt, StoreError> {
        if self.protocol_failed
            || self.pending.is_some()
            || self.next_put != self.plan.puts.len()
            || self.object_ids.iter().any(Option::is_none)
        {
            return Err(StoreError::Corrupt);
        }

        // Stream receive stays outside the global lock. Final tree admission,
        // member verification, and descriptor commit are bounded by the
        // closure budget, so a concurrent GC cannot sweep an unselected
        // member between those steps.
        let _process_lock = self.store.acquire_process_lock()?;
        let closure = ClosureId::from_bytes(self.plan.target.0);
        let update = self.prepare_closure_update()?;
        if update.target().root().as_bytes() != closure.as_bytes() {
            return Err(StoreError::Corrupt);
        }
        let object_count = update.target().node().row_count();
        if object_count
            > u64::try_from(self.budget.max_closure_objects).map_err(|_| StoreError::Bounds)?
        {
            return Err(StoreError::Bounds);
        }
        let relation_write = self.store.write_lazy_relation_update(&update)?;
        let candidate_facts = verify_closure_tree(
            &self.store,
            update.target_root(),
            object_count,
            self.budget.max_closure_objects,
        )?;
        if candidate_facts.object_count != object_count {
            return Err(StoreError::Corrupt);
        }
        let bytes_verified = self
            .bytes_verified_at_begin
            .checked_add(candidate_facts.payload_bytes)
            .ok_or(StoreError::Bounds)?;
        let descriptor = nodes::encode_manifest_descriptor(
            closure,
            relation_write.root(),
            usize::try_from(object_count).map_err(|_| StoreError::Bounds)?,
        )?;
        let descriptor_len = u64::try_from(descriptor.len()).map_err(|_| StoreError::Bounds)?;
        let relation_bytes =
            u64::try_from(relation_write.bytes_written).map_err(|_| StoreError::Bounds)?;
        let base_bytes_written = self
            .bytes_written
            .checked_add(relation_bytes)
            .ok_or(StoreError::Bounds)?;
        let max_bytes_written = base_bytes_written
            .checked_add(descriptor_len)
            .ok_or(StoreError::Bounds)?;
        let descriptor_created = artifact_fs::write_closure_descriptor(
            &self.staging_dir,
            &self.store,
            closure,
            &descriptor,
        )?;
        let bytes_written = if descriptor_created {
            max_bytes_written
        } else {
            base_bytes_written
        };
        Ok(StoredClosureReceipt {
            closure,
            object_count,
            payload_bytes: self.payload_bytes,
            bytes_written,
            bytes_verified,
        })
    }

    fn prepare_closure_update(
        &self,
    ) -> Result<backend_version::LazyPreparedUpdate<ManifestRelation>, StoreError> {
        let loader = self.store.owned_relation_node_loader();
        let base_root = if let Some(base) = self.plan.base {
            let manifest = self.store.open_closure(ClosureId::from_bytes(base.0))?;
            manifest.root_evidence()
        } else {
            let empty = backend_version::canonical_empty::<ManifestRelation>();
            let claim = UntrustedId::<ManifestRelation>::from_wire(
                &empty.commitment().to_bytes(),
                IdContext::relation::<ManifestRelation>(),
            )
            .map_err(|_| StoreError::Corrupt)?;
            admit_canonical_root_claim(claim, empty.as_bytes()).map_err(|_| StoreError::Corrupt)?
        };
        let tree = LazyTree::from_admitted(&loader, PersistedTreeRoot::from_checked(base_root));

        let mut changes = Vec::with_capacity(
            self.object_ids
                .len()
                .checked_add(self.plan.deletes.len())
                .ok_or(StoreError::Bounds)?,
        );
        for id in self.object_ids.iter().flatten() {
            changes.push((*id.as_bytes(), Some(())));
        }
        for id in &self.plan.deletes {
            changes.push((*id.as_bytes(), None));
        }
        changes.sort_by_key(|(key, _)| *key);
        if changes.windows(2).any(|window| window[0].0 >= window[1].0) {
            return Err(StoreError::MalformedDelta);
        }
        let tree_changes = changes
            .iter()
            .map(|(key, after)| TreeChange {
                key: *key,
                after: *after,
            })
            .collect::<Vec<_>>();

        for id in &self.plan.deletes {
            if tree
                .lookup(id.as_bytes())
                .map_err(|_| StoreError::Corrupt)?
                .is_none()
            {
                return Err(StoreError::WrongBase);
            }
        }
        let update = tree
            .prepare_update(&tree_changes)
            .map_err(|_| StoreError::Corrupt)?;
        Ok(update)
    }

    fn open_pending(&self, index: usize) -> Result<PendingObject, StoreError> {
        let claim = *self.plan.puts.get(index).ok_or(StoreError::Corrupt)?;
        let name = format!("object-{index}.tmp");
        let mut file = artifact_fs::create_stage_file(&self.staging_dir, &name)?;
        let schema = claim.schema;
        file.write_all(super::OBJECT_MAGIC)
            .and_then(|()| file.write_all(&[0; 32]))
            .and_then(|()| file.write_all(&[schema.domain()]))
            .and_then(|()| file.write_all(&schema.ty().to_le_bytes()))
            .and_then(|()| file.write_all(&[schema.version()]))
            .and_then(|()| file.write_all(&claim.key))
            .and_then(|()| file.write_all(&claim.version))
            .and_then(|()| file.write_all(&claim.length.to_le_bytes()))
            .map_err(|error| io_error(&error))?;

        let payload_len = usize::try_from(claim.length).map_err(|_| StoreError::Bounds)?;
        let relation_schema = self.store.relation_registry.contains_schema(schema);
        let version_hasher = if relation_schema {
            None
        } else {
            Some(ObjectVersionHasher::new(schema, payload_len).map_err(|_| StoreError::Bounds)?)
        };
        let preimage_len = OBJECT_IDENTITY_FIXED_BYTES
            .checked_add(claim.length)
            .ok_or(StoreError::Bounds)?;
        let mut object_hasher = blake3::Hasher::new();
        object_hasher.update(OBJECT_IDENTITY_DOMAIN);
        object_hasher.update(&preimage_len.to_le_bytes());
        object_hasher.update(&[schema.domain()]);
        object_hasher.update(&schema.ty().to_le_bytes());
        object_hasher.update(&[schema.version()]);
        object_hasher.update(&claim.key);
        object_hasher.update(&claim.version);
        object_hasher.update(&claim.length.to_le_bytes());

        let relation_bytes = if relation_schema {
            let relation_limit = super::relation_object_limit()?;
            let header = object_header_bytes();
            if header.checked_add(payload_len).ok_or(StoreError::Bounds)? > relation_limit {
                return Err(StoreError::Bounds);
            }
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(payload_len)
                .map_err(|_| StoreError::Bounds)?;
            Some(bytes)
        } else {
            None
        };
        Ok(PendingObject {
            index,
            name,
            file,
            claim,
            written: 0,
            version_hasher,
            object_hasher,
            relation_bytes,
        })
    }

    fn commit_pending(&mut self) -> Result<ObjectId, StoreError> {
        let mut pending = self.pending.take().ok_or(StoreError::Corrupt)?;
        if pending.written != pending.claim.length {
            return Err(StoreError::Corrupt);
        }
        let ordinary_version_verified = if let Some(version_hasher) = pending.version_hasher.take()
        {
            if version_hasher.finish().map_err(|_| StoreError::Corrupt)? != pending.claim.version {
                return Err(StoreError::Corrupt);
            }
            true
        } else {
            false
        };
        if let Some(relation_bytes) = pending.relation_bytes.as_deref() {
            self.store.relation_registry.admit_relation_object(
                pending.claim.schema,
                &pending.claim.key,
                &pending.claim.version,
                relation_bytes,
            )?;
        } else if !ordinary_version_verified {
            return Err(StoreError::Corrupt);
        }
        let id = ObjectId::from_bytes(*pending.object_hasher.finalize().as_bytes());
        if pending
            .claim
            .object_id
            .is_some_and(|expected| expected.as_bytes() != id.as_bytes())
        {
            return Err(StoreError::Corrupt);
        }
        pending
            .file
            .seek(SeekFrom::Start(
                u64::try_from(super::OBJECT_MAGIC.len()).map_err(|_| StoreError::Bounds)?,
            ))
            .and_then(|_| pending.file.write_all(id.as_bytes()))
            .and_then(|_| pending.file.sync_all())
            .map_err(|error| io_error(&error))?;

        let created = artifact_fs::link_stage_object(
            &self.staging_dir,
            &pending.name,
            &pending.file,
            &self.store,
            id,
        )?;
        if created {
            // The staging link was removed only after the CAS directory entry
            // was synchronized. The file descriptor remains open here, but
            // the immutable CAS name now has a single link.
        } else {
            let checked = verify_object_file(
                &self.store,
                UntrustedObjectId::from_bytes(*id.as_bytes()),
                Some(pending.claim),
            )?
            .ok_or(StoreError::Corrupt)?;
            if checked.id != id {
                return Err(StoreError::Corrupt);
            }
            artifact_fs::unlink_stage_file(&self.staging_dir, &pending.name)?;
            self.staging_dir.sync_all()?;
        }
        drop(pending.file);
        let relation_ref_bytes = self.store.index_streamed_relation_reference(
            pending.claim.schema,
            &pending.claim.version,
            id,
        )?;
        if created {
            let encoded_len = object_header_bytes()
                .checked_add(usize::try_from(pending.claim.length).map_err(|_| StoreError::Bounds)?)
                .ok_or(StoreError::Bounds)?;
            self.bytes_written = self
                .bytes_written
                .checked_add(u64::try_from(encoded_len).map_err(|_| StoreError::Bounds)?)
                .ok_or(StoreError::Bounds)?;
        }
        self.bytes_written = self
            .bytes_written
            .checked_add(relation_ref_bytes)
            .ok_or(StoreError::Bounds)?;
        self.object_ids[pending.index] = Some(id);
        Ok(id)
    }

    fn replay_completed(
        &mut self,
        index: usize,
        offset: u64,
        bytes: &[u8],
    ) -> Result<ArtifactChunkReceipt, StoreError> {
        let id = self.object_ids[index].ok_or(StoreError::Corrupt)?;
        let claim = self.plan.puts[index];
        if let Err(error) = compare_committed_payload(&self.store, id, claim, offset, bytes) {
            self.protocol_failed = true;
            return Err(error);
        }
        Ok(ArtifactChunkReceipt {
            duplicate: true,
            object_complete: true,
            bytes_accepted: 0,
            object_id: Some(id),
        })
    }

    fn replay_have(
        &mut self,
        index: usize,
        offset: u64,
        bytes: &[u8],
    ) -> Result<ArtifactChunkReceipt, StoreError> {
        self.replay_completed(index, offset, bytes)
    }

    fn index_relation_reference(&mut self, object: VerifiedObject) -> Result<(), StoreError> {
        let reference_bytes = self.store.index_streamed_relation_reference(
            object.schema,
            &object.version,
            object.id,
        )?;
        self.bytes_written = self
            .bytes_written
            .checked_add(reference_bytes)
            .ok_or(StoreError::Bounds)?;
        Ok(())
    }

    fn advance_past_present(&mut self) {
        while self.next_put < self.object_ids.len() && self.object_ids[self.next_put].is_some() {
            self.next_put += 1;
        }
    }

    fn finish_empty_objects(&mut self) -> Result<(), StoreError> {
        loop {
            if self.next_put >= self.plan.puts.len()
                || self.plan.puts[self.next_put].length != 0
                || self.object_ids[self.next_put].is_some()
            {
                return Ok(());
            }
            self.pending = Some(self.open_pending(self.next_put)?);
            let id = self.commit_pending()?;
            let _ = id;
            self.advance_past_present();
        }
    }
}

impl Drop for ArtifactSession {
    fn drop(&mut self) {
        self.pending.take();
        let _ = artifact_fs::cleanup_session(
            &self.store,
            &self.staging_parent,
            &self.staging_name,
            &self.staging_dir,
        );
        self.lease_file.take();
        self.gc_pin.take();
    }
}

#[derive(Clone, Debug)]
struct VerifiedObject {
    id: ObjectId,
    schema: SchemaIdentity,
    key: Hash,
    version: Hash,
    payload_bytes: u64,
    relation_references: Option<VerifiedRelationReferences>,
}

#[derive(Clone, Debug)]
pub(super) struct VerifiedRelationReferences {
    pub(super) children: Vec<Hash>,
    pub(super) values: Vec<Hash>,
}

fn validate_plan(
    store: &FileStore,
    plan: &ArtifactPlan,
    budget: ArtifactBudget,
) -> Result<(), StoreError> {
    if plan
        .puts
        .len()
        .checked_add(plan.deletes.len())
        .ok_or(StoreError::Bounds)?
        > budget.max_changes
        || plan.puts.windows(2).any(|window| {
            (window[0].schema, window[0].key, window[0].version)
                >= (window[1].schema, window[1].key, window[1].version)
        })
        || plan
            .deletes
            .windows(2)
            .any(|window| window[0].as_bytes() >= window[1].as_bytes())
    {
        return Err(StoreError::MalformedDelta);
    }
    if plan.base.is_none() && !plan.deletes.is_empty() {
        return Err(StoreError::WrongBase);
    }
    let mut total_bytes = 0_u64;
    let max_envelope = store.object_envelope_limit()?;
    for claim in &plan.puts {
        let payload = usize::try_from(claim.length).map_err(|_| StoreError::Bounds)?;
        let encoded = object_header_bytes()
            .checked_add(payload)
            .ok_or(StoreError::Bounds)?;
        if encoded > max_envelope {
            return Err(StoreError::Bounds);
        }
        total_bytes = total_bytes
            .checked_add(claim.length)
            .ok_or(StoreError::Bounds)?;
        if claim.object_id.is_some_and(|id| id.as_bytes() == &[0; 32]) {
            return Err(StoreError::Corrupt);
        }
    }
    if total_bytes > budget.max_payload_bytes {
        return Err(StoreError::Bounds);
    }
    Ok(())
}

pub(super) fn object_header_bytes() -> usize {
    super::OBJECT_MAGIC.len() + 32 + 1 + 2 + 1 + 32 + 32 + 8
}

fn verify_object_file(
    store: &FileStore,
    claim: UntrustedObjectId,
    expected: Option<ArtifactObjectClaim>,
) -> Result<Option<VerifiedObject>, StoreError> {
    Ok(verify_object_file_reader(store, claim, expected)?.map(|(verified, _)| verified))
}

fn verify_object_file_reader(
    store: &FileStore,
    claim: UntrustedObjectId,
    expected: Option<ArtifactObjectClaim>,
) -> Result<Option<(VerifiedObject, File)>, StoreError> {
    verify_object_file_reader_limited(store, claim, expected, None)
}

fn verify_object_file_reader_limited(
    store: &FileStore,
    claim: UntrustedObjectId,
    expected: Option<ArtifactObjectClaim>,
    maximum_payload_bytes: Option<u64>,
) -> Result<Option<(VerifiedObject, File)>, StoreError> {
    verify_object_file_reader_limited_with_payload(
        store,
        claim,
        expected,
        maximum_payload_bytes,
        &mut |_| Ok(()),
    )
}

fn verify_object_file_reader_limited_with_payload(
    store: &FileStore,
    claim: UntrustedObjectId,
    expected: Option<ArtifactObjectClaim>,
    maximum_payload_bytes: Option<u64>,
    consume_chunk: &mut impl FnMut(&[u8]) -> Result<(), StoreError>,
) -> Result<Option<(VerifiedObject, File)>, StoreError> {
    let mut file = match artifact_fs::open_object(store, ObjectId::from_bytes(*claim.as_bytes()))? {
        Some(file) => file,
        None => return Ok(None),
    };
    let metadata = file.metadata().map_err(|error| io_error(&error))?;
    let encoded_len = usize::try_from(metadata.len()).map_err(|_| StoreError::Bounds)?;
    if encoded_len > store.object_envelope_limit()? || encoded_len < object_header_bytes() {
        return Err(StoreError::Corrupt);
    }

    let mut magic = vec![0_u8; super::OBJECT_MAGIC.len()];
    file.read_exact(&mut magic)
        .map_err(|error| io_error(&error))?;
    if magic.as_slice() != super::OBJECT_MAGIC {
        return Err(StoreError::Corrupt);
    }
    let mut id_bytes = [0_u8; 32];
    file.read_exact(&mut id_bytes)
        .map_err(|error| io_error(&error))?;
    if id_bytes != *claim.as_bytes() {
        return Err(StoreError::Corrupt);
    }
    let mut schema_prefix = [0_u8; 4];
    file.read_exact(&mut schema_prefix)
        .map_err(|error| io_error(&error))?;
    let schema = SchemaIdentity::new(
        schema_prefix[0],
        u16::from_le_bytes([schema_prefix[1], schema_prefix[2]]),
        schema_prefix[3],
    );
    let key = read_fixed_hash(&mut file)?;
    let version = read_fixed_hash(&mut file)?;
    let length = read_u64_le(&mut file)?;
    if maximum_payload_bytes.is_some_and(|maximum| length > maximum) {
        return Err(StoreError::Bounds);
    }
    let payload_len = usize::try_from(length).map_err(|_| StoreError::Bounds)?;
    let expected_encoded = object_header_bytes()
        .checked_add(payload_len)
        .ok_or(StoreError::Bounds)?;
    if encoded_len != expected_encoded {
        return Err(StoreError::Corrupt);
    }
    if expected.is_some_and(|expected| {
        expected.schema != schema
            || expected.key != key
            || expected.version != version
            || expected.length != length
            || expected
                .object_id
                .is_some_and(|expected_id| expected_id.as_bytes() != claim.as_bytes())
    }) {
        return Err(StoreError::Corrupt);
    }

    let mut version_hasher = if store.relation_registry.contains_schema(schema) {
        None
    } else {
        Some(ObjectVersionHasher::new(schema, payload_len).map_err(|_| StoreError::Bounds)?)
    };
    let preimage_len = OBJECT_IDENTITY_FIXED_BYTES
        .checked_add(length)
        .ok_or(StoreError::Bounds)?;
    let mut object_hasher = blake3::Hasher::new();
    object_hasher.update(OBJECT_IDENTITY_DOMAIN);
    object_hasher.update(&preimage_len.to_le_bytes());
    object_hasher.update(&[schema.domain()]);
    object_hasher.update(&schema.ty().to_le_bytes());
    object_hasher.update(&[schema.version()]);
    object_hasher.update(&key);
    object_hasher.update(&version);
    object_hasher.update(&length.to_le_bytes());

    let mut relation_bytes = if store.relation_registry.contains_schema(schema) {
        if expected_encoded > super::relation_object_limit()? {
            return Err(StoreError::Bounds);
        }
        let mut relation_bytes = Vec::new();
        relation_bytes
            .try_reserve_exact(payload_len)
            .map_err(|_| StoreError::Bounds)?;
        Some(relation_bytes)
    } else {
        None
    };
    let mut remaining = length;
    let mut buffer = [0_u8; VERIFY_BUFFER_BYTES];
    while remaining > 0 {
        let take = usize::try_from(remaining.min(VERIFY_BUFFER_BYTES as u64))
            .map_err(|_| StoreError::Bounds)?;
        let chunk = &mut buffer[..take];
        file.read_exact(chunk).map_err(|error| io_error(&error))?;
        if let Some(version_hasher) = &mut version_hasher {
            version_hasher
                .update(chunk)
                .map_err(|_| StoreError::Corrupt)?;
        }
        object_hasher.update(chunk);
        if let Some(relation_bytes) = &mut relation_bytes {
            relation_bytes.extend_from_slice(chunk);
        }
        consume_chunk(chunk)?;
        remaining = remaining
            .checked_sub(u64::try_from(take).map_err(|_| StoreError::Bounds)?)
            .ok_or(StoreError::Bounds)?;
    }
    if version_hasher
        .map(|hasher| hasher.finish().map_err(|_| StoreError::Corrupt))
        .transpose()?
        .is_some_and(|actual| actual != version)
        || *object_hasher.finalize().as_bytes() != *claim.as_bytes()
    {
        return Err(StoreError::Corrupt);
    }
    let relation_references = if let Some(relation_bytes) = relation_bytes.as_deref() {
        let references = store.relation_registry.admit_relation_object(
            schema,
            &key,
            &version,
            relation_bytes,
        )?;
        Some(VerifiedRelationReferences {
            children: references.children,
            values: references.value_references,
        })
    } else {
        None
    };
    let verified = VerifiedObject {
        id: ObjectId::from_bytes(*claim.as_bytes()),
        schema,
        key,
        version,
        payload_bytes: length,
        relation_references,
    };
    file.seek(SeekFrom::Start(0))
        .map_err(|error| io_error(&error))?;
    Ok(Some((verified, file)))
}

fn verify_stored_closure(
    store: &FileStore,
    closure: ClosureId,
    max_objects: usize,
) -> Result<VerifiedClosureFacts, StoreError> {
    let manifest = store.open_closure(closure)?;
    let count = manifest
        .entry_count()
        .map(u64::try_from)
        .transpose()
        .map_err(|_| StoreError::Bounds)?
        .unwrap_or_else(|| manifest.root_evidence().node().row_count());
    verify_closure_tree(
        store,
        PersistedTreeRoot::from_checked(manifest.root_evidence()),
        count,
        max_objects,
    )
}

pub(super) fn verify_closure_tree(
    store: &FileStore,
    root: PersistedTreeRoot<ManifestRelation>,
    count: u64,
    max_objects: usize,
) -> Result<VerifiedClosureFacts, StoreError> {
    if count > u64::try_from(max_objects).map_err(|_| StoreError::Bounds)? {
        return Err(StoreError::Bounds);
    }
    let loader = store.owned_relation_node_loader();
    let tree = LazyTree::from_admitted(&loader, root);
    let mut facts = VerifiedClosureFacts::default();
    let mut after: Option<Hash> = None;
    loop {
        let page = tree
            .page(after.as_ref(), MANIFEST_PAGE_OBJECTS)
            .map_err(|_| StoreError::Corrupt)?;
        if page.entries().is_empty() {
            break;
        }
        for (id_bytes, ()) in page.entries() {
            let id = ObjectId::from_bytes(*id_bytes);
            let claim = UntrustedObjectId::from_bytes(*id.as_bytes());
            let object = verify_object_file(store, claim, None)?.ok_or(StoreError::Corrupt)?;
            facts.object_count = facts
                .object_count
                .checked_add(1)
                .ok_or(StoreError::Bounds)?;
            facts.payload_bytes = facts
                .payload_bytes
                .checked_add(object.payload_bytes)
                .ok_or(StoreError::Bounds)?;
            if facts.object_count > u64::try_from(max_objects).map_err(|_| StoreError::Bounds)? {
                return Err(StoreError::Bounds);
            }
            if let Some(references) = object.relation_references {
                if store.read_relation_ref(object.schema, &object.version)? != object.id {
                    return Err(StoreError::Corrupt);
                }
                for child_version in references.children {
                    let child_id = store.read_relation_ref(object.schema, &child_version)?;
                    if tree
                        .lookup(child_id.as_bytes())
                        .map_err(|_| StoreError::Corrupt)?
                        .is_none()
                    {
                        return Err(StoreError::Corrupt);
                    }
                }
                // Relation value references can cross logical closures, but a
                // newly admitted local closure may depend only on a target
                // that is still present in this FileStore. GC follows these
                // edges locally and fails closed on absence, so admitting a
                // closure over an already offloaded target would create a
                // permanently uncollectable root.
                for target in references.values {
                    if tree
                        .lookup(&target)
                        .map_err(|_| StoreError::Corrupt)?
                        .is_none()
                    {
                        store.verify_object_claim(UntrustedObjectId::from_bytes(target))?;
                    }
                }
            }
        }
        after = page.entries().last().map(|(key, ())| *key);
    }
    if facts.object_count != count {
        return Err(StoreError::Corrupt);
    }
    Ok(facts)
}

fn compare_committed_payload(
    store: &FileStore,
    id: ObjectId,
    claim: ArtifactObjectClaim,
    offset: u64,
    expected: &[u8],
) -> Result<(), StoreError> {
    let checked = verify_object_file(
        store,
        UntrustedObjectId::from_bytes(*id.as_bytes()),
        Some(claim),
    )?
    .ok_or(StoreError::Corrupt)?;
    if checked.id != id {
        return Err(StoreError::Corrupt);
    }
    let end = offset
        .checked_add(u64::try_from(expected.len()).map_err(|_| StoreError::Bounds)?)
        .ok_or(StoreError::Bounds)?;
    if end > claim.length {
        return Err(StoreError::Corrupt);
    }
    let mut file = artifact_fs::open_object(store, id)?.ok_or(StoreError::Corrupt)?;
    compare_payload_range(&mut file, object_header_bytes(), offset, expected)
}

fn compare_payload_range(
    file: &mut File,
    header: usize,
    offset: u64,
    expected: &[u8],
) -> Result<(), StoreError> {
    let start = u64::try_from(header)
        .map_err(|_| StoreError::Bounds)?
        .checked_add(offset)
        .ok_or(StoreError::Bounds)?;
    file.seek(SeekFrom::Start(start))
        .map_err(|error| io_error(&error))?;
    let mut compared = 0_usize;
    let mut scratch = [0_u8; VERIFY_BUFFER_BYTES];
    while compared < expected.len() {
        let take = (expected.len() - compared).min(scratch.len());
        file.read_exact(&mut scratch[..take])
            .map_err(|error| io_error(&error))?;
        if scratch[..take] != expected[compared..compared + take] {
            return Err(StoreError::Corrupt);
        }
        compared = compared.checked_add(take).ok_or(StoreError::Bounds)?;
    }
    Ok(())
}

fn read_fixed_hash(file: &mut File) -> Result<Hash, StoreError> {
    let mut bytes = [0_u8; 32];
    file.read_exact(&mut bytes)
        .map_err(|error| io_error(&error))?;
    Ok(bytes)
}

fn map_envelope_read_error(error: std::io::Error) -> StoreError {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        StoreError::Corrupt
    } else {
        io_error(&error)
    }
}

fn read_u64_le(file: &mut File) -> Result<u64, StoreError> {
    let mut bytes = [0_u8; 8];
    file.read_exact(&mut bytes)
        .map_err(|error| io_error(&error))?;
    Ok(u64::from_le_bytes(bytes))
}

impl FileStore {
    pub(in crate::durable) fn index_streamed_relation_reference(
        &self,
        schema: SchemaIdentity,
        version: &Hash,
        object: ObjectId,
    ) -> Result<u64, StoreError> {
        if !self.relation_registry.contains_schema(schema) {
            return Ok(0);
        }
        if self.relation_ref_exists(schema, version)? {
            if self.read_relation_ref(schema, version)? != object {
                return Err(StoreError::Corrupt);
            }
            return Ok(0);
        }
        let created = nodes::write_relation_reference(self, schema, version, object)?;
        if created {
            u64::try_from(nodes::relation_reference_bytes()).map_err(|_| StoreError::Bounds)
        } else {
            Ok(0)
        }
    }
}
