//! Immutable pack and typed-object filesystem admission.

use super::{
    FileStore, GcPinGuard, Hash, LayoutId, OBJECT_MAGIC, ObjectId, PACK_MAGIC, Pack, Path, PathBuf,
    RelationAdmissionRegistry, StoreError, TEMP_COUNTER, TypedObject, envelope_limit, fs, io_error,
    put_u32, put_u64, read_byte, read_hash, read_u32, read_u64, sync_directory,
};
use crate::{UntrustedObjectId, WirePack, admit_backend_object_version};
use backend_version::SchemaIdentity;
use std::sync::atomic::Ordering;
use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io::{Read, Write},
};

/// A fully authenticated object whose payload borrows the bounded envelope
/// buffer for the duration of a `FileStore` callback.
///
/// The view has no owning or shared-pointer conversion. Its payload cannot be
/// returned from the callback because the callback is higher-ranked over the
/// view lifetime.
///
/// ```compile_fail
/// use backend_store::{FileStore, StoreError, UntrustedObjectId};
///
/// fn leak_payload<'a>(
///     store: &FileStore,
///     claim: UntrustedObjectId,
/// ) -> Result<&'a [u8], StoreError> {
///     let mut payload = None;
///     store.with_verified_object_claim(claim, |object| {
///         payload = Some(object.bytes());
///         Ok(())
///     })?;
///     Ok(payload.expect("callback ran"))
/// }
/// ```
#[derive(Clone, Copy, Debug)]
pub struct VerifiedObjectView<'a> {
    id: ObjectId,
    schema: SchemaIdentity,
    key: Hash,
    version: Hash,
    bytes: &'a [u8],
}

impl<'a> VerifiedObjectView<'a> {
    /// Returns the content-addressed identity recomputed from this object.
    #[must_use]
    pub const fn id(self) -> ObjectId {
        self.id
    }

    /// Returns the checked runtime schema identity.
    #[must_use]
    pub const fn schema(self) -> SchemaIdentity {
        self.schema
    }

    /// Returns the checked typed key.
    #[must_use]
    pub const fn key(&self) -> &Hash {
        &self.key
    }

    /// Returns the checked complete typed version.
    #[must_use]
    pub const fn version(&self) -> &Hash {
        &self.version
    }

    /// Returns the canonical typed payload borrowed from the verified
    /// envelope buffer.
    #[must_use]
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

/// Removes crash-orphaned files only from the store writer's private temp namespace.
pub(super) fn scavenge_store_temps(root: &Path) -> Result<usize, StoreError> {
    let mut removed = 0usize;
    // Relation-reference writers also hold the process lease. A crashed pre-link publication
    // can leave its final reference with a second temporary link until this recovery pass.
    for name in ["packs", "objects", "closures", "nodes"] {
        let directory = root.join(name);
        let mut removed_here = false;
        for entry in fs::read_dir(&directory).map_err(|error| io_error(&error))? {
            let entry = entry.map_err(|error| io_error(&error))?;
            if !is_store_temp_name(&entry.file_name()) {
                continue;
            }
            let kind = entry.file_type().map_err(|error| io_error(&error))?;
            if !kind.is_file() {
                return Err(StoreError::Corrupt);
            }
            fs::remove_file(entry.path()).map_err(|error| io_error(&error))?;
            removed = removed.checked_add(1).ok_or(StoreError::Bounds)?;
            removed_here = true;
        }
        if removed_here {
            sync_directory(&directory)?;
        }
    }
    Ok(removed)
}

fn is_store_temp_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(body) = name
        .strip_prefix('.')
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    let mut parts = body.rsplitn(3, '.');
    let (Some(counter), Some(process), Some(target)) = (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    !target.is_empty()
        && !process.is_empty()
        && !counter.is_empty()
        && process.bytes().all(|byte| byte.is_ascii_digit())
        && counter.bytes().all(|byte| byte.is_ascii_digit())
}

/// Receipt returned after admitting one immutable object into the durable CAS.
///
/// The receipt distinguishes a newly created file from an existing identical
/// object and reports the exact encoded bytes that were admitted.  A caller
/// can therefore account for structural sharing without inspecting store
/// paths or treating a content-addressed hit as a write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectWriteReceipt {
    id: ObjectId,
    created: bool,
    bytes: u64,
}

impl ObjectWriteReceipt {
    pub(super) const fn new(id: ObjectId, created: bool, bytes: u64) -> Self {
        Self { id, created, bytes }
    }

    /// Returns the admitted immutable object identity.
    #[must_use]
    pub const fn id(self) -> ObjectId {
        self.id
    }

    /// Returns whether this call created a new physical object file.
    #[must_use]
    pub const fn created(self) -> bool {
        self.created
    }

    /// Returns the exact encoded object envelope length.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.bytes
    }
}

pub(super) fn write_immutable(
    path: &Path,
    bytes: &[u8],
    directory: &Path,
) -> Result<(), StoreError> {
    write_immutable_with_status(path, bytes, directory).map(|_| ())
}

/// Links one immutable file into the store and reports whether this call
/// created the physical object.  An existing equal file is a successful
/// structural-share hit; a different file at the same content address is
/// corruption.
pub(super) fn write_immutable_with_status(
    path: &Path,
    bytes: &[u8],
    directory: &Path,
) -> Result<bool, StoreError> {
    write_immutable_with_status_inner(path, bytes, Some(directory))
}

/// Writes one immutable file while leaving directory synchronization to a
/// caller that is batching a publication.  The file itself is always synced
/// before it is linked into the CAS; the final directory sync must happen
/// before the caller exposes a closure or root that references this object.
pub(super) fn write_immutable_file_with_status(
    path: &Path,
    bytes: &[u8],
) -> Result<bool, StoreError> {
    write_immutable_with_status_inner(path, bytes, None)
}

fn write_immutable_with_status_inner(
    path: &Path,
    bytes: &[u8],
    directory: Option<&Path>,
) -> Result<bool, StoreError> {
    if let Ok(existing) = fs::read(path) {
        return if existing == bytes {
            Ok(false)
        } else {
            Err(StoreError::Corrupt)
        };
    }
    let (temporary, mut file) = create_temp(path)?;
    let written = file.write_all(bytes).and_then(|()| {
        // Seal the private inode before its existing pre-link durability
        // barrier. Its first admitted read then needs no permission change
        // or second file sync; legacy unsealed members still seal on read.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o400))?;
        }
        file.sync_all()
    });
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(io_error(&error));
    }
    let result = match fs::hard_link(&temporary, path) {
        Ok(()) => directory.map_or(Ok(true), |directory| {
            sync_directory(directory).map(|()| true)
        }),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => match fs::read(path) {
            Ok(existing) if existing == bytes => Ok(false),
            Ok(_) => Err(StoreError::Corrupt),
            Err(read_error) => Err(io_error(&read_error)),
        },
        Err(error) => Err(io_error(&error)),
    };
    let _ = fs::remove_file(&temporary);
    result
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8], directory: &Path) -> Result<(), StoreError> {
    let (temporary, mut file) = create_temp(path)?;
    #[cfg(test)]
    if super::recovery::take_test_fault(5) {
        return Err(StoreError::Io(
            "injected post-temp-create failure".to_owned(),
        ));
    }
    if let Err(error) = file.write_all(bytes) {
        let _ = fs::remove_file(&temporary);
        return Err(io_error(&error));
    }
    #[cfg(test)]
    if super::recovery::take_test_fault(6) {
        return Err(StoreError::Io(
            "injected post-temp-write failure".to_owned(),
        ));
    }
    if let Err(error) = file.sync_all() {
        let _ = fs::remove_file(&temporary);
        return Err(io_error(&error));
    }
    #[cfg(test)]
    if super::recovery::take_test_fault(7) {
        return Err(StoreError::Io("injected post-temp-sync failure".to_owned()));
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return if error.kind() == std::io::ErrorKind::AlreadyExists {
            let existing = fs::read(path).map_err(|read_error| io_error(&read_error))?;
            if existing == bytes {
                Ok(())
            } else {
                Err(StoreError::Corrupt)
            }
        } else {
            Err(io_error(&error))
        };
    }
    #[cfg(test)]
    if super::recovery::take_test_fault(8) {
        return Err(StoreError::Io(
            "injected post-head-rename failure".to_owned(),
        ));
    }
    sync_directory(directory)
}

pub(super) fn create_temp(path: &Path) -> Result<(PathBuf, File), StoreError> {
    loop {
        let temporary = unique_temp(path);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(&error)),
        }
    }
}

fn unique_temp(path: &Path) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("store");
    path.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), counter))
}

pub(super) fn encode_object(object: &TypedObject, max_bytes: usize) -> Result<Vec<u8>, StoreError> {
    let mut output = Vec::new();
    output.extend_from_slice(OBJECT_MAGIC);
    output.extend_from_slice(object.id().as_bytes());
    output.push(object.schema().domain());
    output.extend_from_slice(&object.schema().ty().to_le_bytes());
    output.push(object.schema().version());
    output.extend_from_slice(object.key());
    output.extend_from_slice(object.version());
    put_u64(&mut output, object.bytes().len())?;
    output.extend_from_slice(object.bytes());
    if output.len() > max_bytes {
        return Err(StoreError::Bounds);
    }
    Ok(output)
}

/// Parses and authenticates one complete object envelope while retaining only
/// borrows into its caller-owned, already bounded byte buffer.
// Internal only: authenticates the physical object but does not admit its
// relation grammar. Callers must not expose the borrowed view as authority.
fn verify_object_envelope<'a>(
    bytes: &'a [u8],
    max_bytes: usize,
    expected: Option<UntrustedObjectId>,
) -> Result<VerifiedObjectView<'a>, StoreError> {
    if bytes.len() > max_bytes || !bytes.starts_with(OBJECT_MAGIC) {
        return Err(if bytes.len() > max_bytes {
            StoreError::Bounds
        } else {
            StoreError::Corrupt
        });
    }
    let mut at = OBJECT_MAGIC.len();
    let claimed = read_hash(bytes, &mut at)?;
    let domain = read_byte(bytes, &mut at)?;
    let ty_end = at.checked_add(2).ok_or(StoreError::Bounds)?;
    let ty = u16::from_le_bytes(
        bytes
            .get(at..ty_end)
            .ok_or(StoreError::Corrupt)?
            .try_into()
            .map_err(|_| StoreError::Corrupt)?,
    );
    at = ty_end;
    let version_tag = read_byte(bytes, &mut at)?;
    let key = read_hash(bytes, &mut at)?;
    let version = read_hash(bytes, &mut at)?;
    let length = usize::try_from(read_u64(bytes, &mut at)?).map_err(|_| StoreError::Bounds)?;
    let end = at.checked_add(length).ok_or(StoreError::Bounds)?;
    let payload = bytes.get(at..end).ok_or(StoreError::Corrupt)?;
    if end != bytes.len() {
        return Err(StoreError::Corrupt);
    }

    let schema = SchemaIdentity::new(domain, ty, version_tag);
    let actual = object_commitment_view(schema, &key, &version, payload)?;
    if actual != claimed || expected.is_some_and(|expected| expected.as_bytes() != &claimed) {
        return Err(StoreError::Corrupt);
    }
    Ok(VerifiedObjectView {
        id: ObjectId::from_bytes(actual),
        schema,
        key,
        version,
        bytes: payload,
    })
}

fn admit_object_view(view: VerifiedObjectView<'_>, registry: &RelationAdmissionRegistry) -> Result<(), StoreError> {
    if registry.contains_schema(view.schema()) {
        registry.admit_relation_object(view.schema(), view.key(), view.version(), view.bytes())?;
    } else {
        admit_backend_object_version(view.schema(), view.bytes(), view.version())?;
    }
    Ok(())
}

/// Computes the same physical commitment as `TypedObject::id` directly over
/// borrowed fields, without constructing an owned object or payload copy.
fn object_commitment_view(
    schema: SchemaIdentity,
    key: &Hash,
    version: &Hash,
    payload: &[u8],
) -> Result<Hash, StoreError> {
    let payload_len = u64::try_from(payload.len()).map_err(|_| StoreError::Bounds)?;
    let preimage_len = super::artifact::OBJECT_IDENTITY_FIXED_BYTES
        .checked_add(payload_len)
        .ok_or(StoreError::Bounds)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(super::artifact::OBJECT_IDENTITY_DOMAIN);
    hasher.update(&preimage_len.to_le_bytes());
    hasher.update(&[schema.domain()]);
    hasher.update(&schema.ty().to_le_bytes());
    hasher.update(&[schema.version()]);
    hasher.update(key);
    hasher.update(version);
    hasher.update(&payload_len.to_le_bytes());
    hasher.update(payload);
    Ok(*hasher.finalize().as_bytes())
}

/// Computes an untrusted object identity for a value whose logical key and
/// version are both derived from these bytes. This hashes borrowed bytes and
/// does not validate their schema-specific canonical grammar or membership.
pub fn untrusted_object_id_for_encoded_value(schema: SchemaIdentity, bytes: &[u8]) -> Result<UntrustedObjectId, StoreError> {
    let mut key = backend_version::ObjectKeyHasher::new(schema, bytes.len()).map_err(|_| StoreError::Bounds)?;
    key.update(bytes).map_err(|_| StoreError::Bounds)?;
    let key = key.finish().map_err(|_| StoreError::Bounds)?;
    let version = backend_version::object_version_digest(schema, bytes).map_err(|_| StoreError::Bounds)?;
    let id = object_commitment_view(schema, &key, &version, bytes)?;
    Ok(UntrustedObjectId::from_bytes(id))
}

impl FileStore {
    /// Reads and authenticates an immutable object, lending its payload only
    /// for the duration of `visit`.
    ///
    /// The callback runs while a shared GC pin is held. Keep multi-object
    /// traversals efficient by acquiring one [`GcPinGuard`] and using
    /// [`Self::with_verified_object_claim_pinned`] for each member.
    /// Callbacks must not wait for garbage collection or otherwise request an
    /// exclusive GC lease while this shared pin is live.
    pub fn with_verified_object<T, F>(&self, id: ObjectId, visit: F) -> Result<T, StoreError>
    where
        F: for<'a> FnOnce(VerifiedObjectView<'a>) -> Result<T, StoreError>,
    {
        self.with_verified_object_claim(UntrustedObjectId::from_bytes(*id.as_bytes()), visit)
    }

    /// Reads and authenticates an object selected by an untrusted identity,
    /// then lends the verified view to `visit`.
    ///
    /// One envelope-sized bounded buffer is allocated and retained through
    /// the callback. The callback's result cannot contain a borrow from that
    /// buffer.
    pub fn with_verified_object_claim<T, F>(
        &self,
        claim: UntrustedObjectId,
        visit: F,
    ) -> Result<T, StoreError>
    where
        F: for<'a> FnOnce(VerifiedObjectView<'a>) -> Result<T, StoreError>,
    {
        let pin = self.pin_garbage_collection()?;
        self.with_verified_object_claim_pinned(&pin, claim, visit)
    }

    /// Reads and authenticates an object while reusing an existing shared GC
    /// pin for this store.
    ///
    /// This lets callers keep one GC lease across a batch of object reads.
    /// The pin is checked against this store before any file is opened. The
    /// callback-lent view remains bounded by this call even when the supplied
    /// pin outlives it. Returns [`StoreError::Corrupt`] if the pin belongs to
    /// another store.
    pub fn with_verified_object_claim_pinned<T, F>(
        &self,
        pin: &GcPinGuard,
        claim: UntrustedObjectId,
        visit: F,
    ) -> Result<T, StoreError>
    where
        F: for<'a> FnOnce(VerifiedObjectView<'a>) -> Result<T, StoreError>,
    {
        if !pin.covers_identity(self.gc_identity) {
            return Err(StoreError::Corrupt);
        }
        self.with_verified_object_claim_if_present(claim, visit)?
            .ok_or(StoreError::Corrupt)
    }

    /// Authenticates a non-relation object with a caller-controlled reservation.
    ///
    /// `before_open` must reserve filesystem/path scratch and work, and may
    /// reject or cancel before any object is opened. `before_payload` receives
    /// untrusted header fields and must reserve the payload, typed admission,
    /// visitor allocations and work before any payload allocation or read.
    /// Both callbacks must fail closed on exhausted credit. The same immutable
    /// descriptor is used for header and payload. This verifies the object,
    /// not its membership in a selected closure. Only the final callback sees
    /// authenticated data; header fields passed to preflight are not authority.
    /// The caller must retain the reservation until this method returns and
    /// separately account for any data retained by `visit`.
    pub fn with_verified_nonrelation_object_bounded<T, O, P, F>(
        &self, pin: &GcPinGuard, claim: UntrustedObjectId,
        accepted_schemas: &[SchemaIdentity], expected_key: Option<Hash>, max_payload: usize,
        budget: &mut usize, before_open: O, before_payload: P, visit: F,
    ) -> Result<T, StoreError>
    where
        O: FnOnce(&mut usize) -> Result<(), StoreError>,
        P: FnOnce(SchemaIdentity, &Hash, usize, &mut usize) -> Result<(), StoreError>,
        F: for<'a> FnOnce(VerifiedObjectView<'a>, &mut usize) -> Result<T, StoreError>,
    {
        if *budget == 0 { return Err(StoreError::Bounds); }
        before_open(budget)?;
        if !pin.covers_identity(self.gc_identity) { return Err(StoreError::Corrupt); }
        let id = ObjectId::from_bytes(*claim.as_bytes());
        let mut file = super::artifact_fs::open_object(self, id)?.ok_or(StoreError::Corrupt)?;
        const HEADER: usize = OBJECT_MAGIC.len() + 32 + 1 + 2 + 1 + 32 + 32 + 8;
        let file_len = usize::try_from(file.metadata().map_err(|error| io_error(&error))?.len()).map_err(|_| StoreError::Bounds)?;
        if file_len < HEADER || file_len > self.object_envelope_limit()? { return Err(StoreError::Bounds); }
        let mut header = [0u8; HEADER];
        file.read_exact(&mut header).map_err(|error| io_error(&error))?;
        if !header.starts_with(OBJECT_MAGIC) { return Err(StoreError::Corrupt); }
        let mut at = OBJECT_MAGIC.len();
        let claimed = read_hash(&header, &mut at)?;
        if &claimed != claim.as_bytes() { return Err(StoreError::Corrupt); }
        let domain = read_byte(&header, &mut at)?;
        let ty = u16::from_le_bytes(header[at..at + 2].try_into().map_err(|_| StoreError::Corrupt)?); at += 2;
        let tag = read_byte(&header, &mut at)?;
        let schema = SchemaIdentity::new(domain, ty, tag);
        let key = read_hash(&header, &mut at)?;
        let version = read_hash(&header, &mut at)?;
        let length = usize::try_from(read_u64(&header, &mut at)?).map_err(|_| StoreError::Bounds)?;
        if at != HEADER || HEADER.checked_add(length) != Some(file_len) { return Err(StoreError::Corrupt); }
        if length > max_payload { return Err(StoreError::Bounds); }
        if !accepted_schemas.contains(&schema) || expected_key.is_some_and(|expected| expected != key)
            || self.relation_registry.contains_schema(schema) { return Err(StoreError::Corrupt); }
        *budget = budget.checked_sub(length).ok_or(StoreError::Bounds)?;
        before_payload(schema, &key, length, budget)?;
        let mut payload = Vec::new();
        payload.try_reserve_exact(length).map_err(|_| StoreError::Bounds)?;
        payload.resize(length, 0);
        file.read_exact(&mut payload).map_err(|error| io_error(&error))?;
        let mut trailing = [0u8; 1];
        if file.read(&mut trailing).map_err(|error| io_error(&error))? != 0 { return Err(StoreError::Corrupt); }
        let actual = object_commitment_view(schema, &key, &version, &payload)?;
        if actual != claimed { return Err(StoreError::Corrupt); }
        admit_backend_object_version(schema, &payload, &version)?;
        visit(VerifiedObjectView { id, schema, key, version, bytes: &payload }, budget)
    }

    /// Opens and admits the object named by `claim`, returning `None` when
    /// the object path is absent. The safe artifact opener owns path, link,
    /// and immutability checks before the descriptor reaches the decoder.
    pub(in crate::durable) fn with_verified_object_claim_if_present<T, F>(
        &self,
        claim: UntrustedObjectId,
        visit: F,
    ) -> Result<Option<T>, StoreError>
    where
        F: for<'a> FnOnce(VerifiedObjectView<'a>) -> Result<T, StoreError>,
    {
        let claimed_id = ObjectId::from_bytes(*claim.as_bytes());
        let Some(file) = super::artifact_fs::open_object(self, claimed_id)? else {
            return Ok(None);
        };
        self.with_verified_object_file(file, claim, self.object_envelope_limit()?, visit).map(Some)
    }

    /// Reads a relation object with the canonical node envelope ceiling before allocation.
    pub(in crate::durable) fn with_verified_relation_object<T, F>(
        &self, claim: UntrustedObjectId, visit: F,
    ) -> Result<T, StoreError>
    where F: for<'a> FnOnce(VerifiedObjectView<'a>) -> Result<T, StoreError> {
        let id = ObjectId::from_bytes(*claim.as_bytes());
        let file = super::artifact_fs::open_object(self, id)?.ok_or(StoreError::Corrupt)?;
        let maximum = self.object_envelope_limit()?.min(super::relation_object_limit()?);
        self.with_verified_object_file(file, claim, maximum, visit)
    }

    /// Invokes an internal preflight after envelope authentication but before
    /// relation decoding. The returned value is still visited only after the
    /// ordinary registry admission succeeds.
    pub(in crate::durable) fn with_relation_object_preflight<T, P, F>(
        &self, claim: UntrustedObjectId, preflight: P, visit: F,
    ) -> Result<T, StoreError>
    where
        P: for<'a> FnOnce(VerifiedObjectView<'a>) -> Result<(), StoreError>,
        F: for<'a> FnOnce(VerifiedObjectView<'a>) -> Result<T, StoreError>,
    {
        let id = ObjectId::from_bytes(*claim.as_bytes());
        let mut file = super::artifact_fs::open_object(self, id)?.ok_or(StoreError::Corrupt)?;
        let maximum = self.object_envelope_limit()?.min(super::relation_object_limit()?);
        const MAX_RELATION_ENVELOPE: usize = backend_version::DEFAULT_CUT_POLICY.max_encoded_bytes()
            + OBJECT_MAGIC.len() + 32 + 1 + 2 + 1 + 32 + 32 + 8;
        let length = usize::try_from(file.metadata().map_err(|error| io_error(&error))?.len())
            .map_err(|_| StoreError::Bounds)?;
        if length > maximum || length > MAX_RELATION_ENVELOPE { return Err(StoreError::Bounds); }
        // A fixed stack buffer avoids Vec growth before the caller's raw-node
        // preflight. The workspace wrapper reserves it before opening a node.
        let mut envelope = [0u8; MAX_RELATION_ENVELOPE];
        file.read_exact(&mut envelope[..length]).map_err(|error| io_error(&error))?;
        let mut trailing = [0u8; 1];
        if file.read(&mut trailing).map_err(|error| io_error(&error))? != 0 { return Err(StoreError::Corrupt); }
        let object = verify_object_envelope(&envelope[..length], maximum, Some(claim))?;
        preflight(object)?;
        admit_object_view(object, &self.relation_registry)?;
        visit(object)
    }

    /// Admits one object from an already-open immutable object descriptor.
    ///
    /// Keeping the descriptor through bounded reading and envelope admission
    /// ensures callers that have already checked the object path do not reopen
    /// a potentially different file by name.
    fn with_verified_object_file<T, F>(
        &self,
        file: File,
        claim: UntrustedObjectId,
        maximum: usize,
        visit: F,
    ) -> Result<T, StoreError>
    where
        F: for<'a> FnOnce(VerifiedObjectView<'a>) -> Result<T, StoreError>,
    {
        self.with_verified_object_file_before(file, claim, maximum, |_| Ok(()), visit)
    }

    fn with_verified_object_file_before<T, P, F>(
        &self,
        mut file: File,
        claim: UntrustedObjectId,
        maximum: usize,
        preflight: P,
        visit: F,
    ) -> Result<T, StoreError>
    where
        P: for<'a> FnOnce(VerifiedObjectView<'a>) -> Result<(), StoreError>,
        F: for<'a> FnOnce(VerifiedObjectView<'a>) -> Result<T, StoreError>,
    {
        let metadata = file.metadata().map_err(|error| io_error(&error))?;
        let maximum_u64 = u64::try_from(maximum).map_err(|_| StoreError::Bounds)?;
        if metadata.len() > maximum_u64 {
            return Err(StoreError::Bounds);
        }
        let length = usize::try_from(metadata.len()).map_err(|_| StoreError::Bounds)?;
        let mut envelope = Vec::new();
        envelope
            .try_reserve_exact(length)
            .map_err(|_| StoreError::Bounds)?;
        (&mut file)
            .take(metadata.len())
            .read_to_end(&mut envelope)
            .map_err(|error| io_error(&error))?;
        if envelope.len() != length {
            return Err(StoreError::Corrupt);
        }
        let mut trailing = [0_u8; 1];
        if file.read(&mut trailing).map_err(|error| io_error(&error))? != 0 {
            return Err(if length == maximum {
                StoreError::Bounds
            } else {
                StoreError::Corrupt
            });
        }
        let object = verify_object_envelope(&envelope, maximum, Some(claim))?;
        preflight(object)?;
        admit_object_view(object, &self.relation_registry)?;
        visit(object)
    }

    /// Reads and authenticates a known object while reusing an existing
    /// shared GC pin for this store.
    pub fn with_verified_object_pinned<T, F>(
        &self,
        pin: &GcPinGuard,
        id: ObjectId,
        visit: F,
    ) -> Result<T, StoreError>
    where
        F: for<'a> FnOnce(VerifiedObjectView<'a>) -> Result<T, StoreError>,
    {
        self.with_verified_object_claim_pinned(
            pin,
            UntrustedObjectId::from_bytes(*id.as_bytes()),
            visit,
        )
    }
}

pub(super) fn encode_pack_file(pack: &Pack, max_pack_bytes: usize) -> Result<Vec<u8>, StoreError> {
    let mut output = Vec::new();
    output.extend_from_slice(PACK_MAGIC);
    output.extend_from_slice(pack.id().as_bytes());
    output.extend_from_slice(pack.layout().as_bytes());
    put_u64(&mut output, pack.bytes().len())?;
    output.extend_from_slice(pack.bytes());
    put_u64(&mut output, pack.locations().len())?;
    for (key, (offset, length)) in pack.locations() {
        put_u32(&mut output, key.len())?;
        output.extend_from_slice(key);
        output.extend_from_slice(&offset.to_le_bytes());
        output.extend_from_slice(&length.to_le_bytes());
    }
    if output.len() > envelope_limit(max_pack_bytes)? {
        return Err(StoreError::Bounds);
    }
    Ok(output)
}

pub(super) fn decode_pack_file(
    bytes: &[u8],
    max_pack_bytes: usize,
) -> Result<WirePack, StoreError> {
    if bytes.len() < PACK_MAGIC.len() || !bytes.starts_with(PACK_MAGIC) {
        return Err(StoreError::Corrupt);
    }
    let mut at = PACK_MAGIC.len();
    let id = read_hash(bytes, &mut at)?;
    let layout = LayoutId::from_bytes(read_hash(bytes, &mut at)?);
    let payload_len = usize::try_from(read_u64(bytes, &mut at)?).map_err(|_| StoreError::Bounds)?;
    if payload_len > max_pack_bytes {
        return Err(StoreError::Bounds);
    }
    let payload_end = at.checked_add(payload_len).ok_or(StoreError::Bounds)?;
    let payload = bytes
        .get(at..payload_end)
        .ok_or(StoreError::Corrupt)?
        .to_vec();
    at = payload_end;
    let location_count =
        usize::try_from(read_u64(bytes, &mut at)?).map_err(|_| StoreError::Bounds)?;
    let mut locations = std::collections::BTreeMap::new();
    for _ in 0..location_count {
        let key_len = usize::try_from(read_u32(bytes, &mut at)?).map_err(|_| StoreError::Bounds)?;
        let key_end = at.checked_add(key_len).ok_or(StoreError::Bounds)?;
        let key = bytes.get(at..key_end).ok_or(StoreError::Corrupt)?.to_vec();
        at = key_end;
        let offset = read_u32(bytes, &mut at)?;
        let length = read_u32(bytes, &mut at)?;
        if locations.insert(key, (offset, length)).is_some() {
            return Err(StoreError::Corrupt);
        }
    }
    if at != bytes.len() {
        return Err(StoreError::Corrupt);
    }
    Ok(WirePack {
        id,
        layout,
        bytes: payload,
        locations,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use backend_version::{ObjectKey, Schema};
    use std::{panic::AssertUnwindSafe, sync::mpsc, time::Duration};

    struct BytesSchema;

    impl Schema for BytesSchema {
        const DOMAIN: u8 = 0xf2;
        const TYPE: u16 = 0x5101;
        type Value = Vec<u8>;

        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }

    fn test_store(label: &str) -> (FileStore, PathBuf) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "backend-store-borrowed-object-{label}-{}-{nonce}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let store = FileStore::open(&root, 1024 * 1024).expect("open test store");
        (store, root)
    }

    fn bytes_object(bytes: Vec<u8>) -> TypedObject {
        let key = ObjectKey::<BytesSchema>::from_value(&bytes);
        TypedObject::from_value(&key, &bytes)
    }

    fn replace_fixture(path: &Path, bytes: &[u8]) {
        if path.exists() {
            fs::remove_file(path).expect("remove immutable fixture");
        }
        fs::write(path, bytes).expect("write replacement fixture");
    }

    fn corrupt_claim(store: &FileStore, id: ObjectId, mutate: impl FnOnce(&mut Vec<u8>)) {
        let path = store.object_path(id);
        let original = fs::read(&path).expect("read fixture envelope");
        let mut changed = original.clone();
        mutate(&mut changed);
        replace_fixture(&path, &changed);
        assert_eq!(store.read_object(id), Err(StoreError::Corrupt));
        assert_eq!(
            store.with_verified_object(id, |_| Ok(())),
            Err(StoreError::Corrupt)
        );
        replace_fixture(&path, &original);
    }

    #[test]
    fn bounded_nonrelation_read_preflights_before_payload_and_authenticates() {
        use std::cell::Cell;
        let (store, root) = test_store("bounded-nonrelation");
        let object = bytes_object(vec![9; 512 * 1024]);
        assert_eq!(untrusted_object_id_for_encoded_value(object.schema(), object.bytes()).expect("hash").as_bytes(), object.id().as_bytes());
        store.write_object(&object).expect("write");
        let pin = store.pin_garbage_collection().expect("pin");
        let claim = UntrustedObjectId::from_bytes(*object.id().as_bytes());
        let schema = object.schema();
        let opened = Cell::new(false);
        let payload_gate = Cell::new(false);
        let visited = Cell::new(false);
        let mut budget = 0;
        assert!(matches!(store.with_verified_nonrelation_object_bounded(&pin, claim, &[schema], Some(*object.key()), object.bytes().len(), &mut budget,
            |_| { opened.set(true); Ok(()) }, |_,_,_,_| { payload_gate.set(true); Ok(()) }, |_,_| { visited.set(true); Ok(()) }), Err(StoreError::Bounds)));
        assert!(!opened.get() && !payload_gate.get() && !visited.get());
        budget = object.bytes().len() - 1;
        assert!(matches!(store.with_verified_nonrelation_object_bounded(&pin, claim, &[schema], Some(*object.key()), object.bytes().len(), &mut budget,
            |_| Ok(()), |_,_,_,_| { payload_gate.set(true); Ok(()) }, |_,_| { visited.set(true); Ok(()) }), Err(StoreError::Bounds)));
        assert!(!payload_gate.get() && !visited.get());
        budget = object.bytes().len();
        let measured = allocation_counter::measure(|| {
            store.with_verified_nonrelation_object_bounded(&pin, claim, &[schema], Some(*object.key()), object.bytes().len(), &mut budget,
                |_| Ok(()), |_,_,len,_| { assert_eq!(len, object.bytes().len()); Ok(()) },
                |view,_| { assert_eq!(view.id(), object.id()); assert_eq!(view.bytes(), object.bytes()); Ok(()) }).expect("read");
        });
        assert!(usize::try_from(measured.bytes_max).unwrap() <= object.bytes().len() + 4096);
        let path = store.object_path(object.id());
        let mut bytes = fs::read(&path).expect("read envelope");
        *bytes.last_mut().expect("payload") ^= 1;
        replace_fixture(&path, &bytes);
        budget = object.bytes().len();
        assert!(store.with_verified_nonrelation_object_bounded(&pin, claim, &[schema], None, object.bytes().len(), &mut budget,
            |_| Ok(()), |_,_,_,_| Ok(()), |_,_| { visited.set(true); Ok(()) }).is_err());
        assert!(!visited.get());
        drop(pin);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn borrowed_view_matches_the_owned_read_oracle() {
        let (store, root) = test_store("equality");
        let object = bytes_object(b"borrowed bytes stay in the envelope buffer".to_vec());
        store.write_object(&object).expect("write object");
        let owned = store.read_object(object.id()).expect("owned read oracle");
        assert_eq!(owned, object);
        store
            .with_verified_object(object.id(), |view| {
                assert_eq!(view.id(), owned.id());
                assert_eq!(view.schema(), owned.schema());
                assert_eq!(view.key(), owned.key());
                assert_eq!(view.version(), owned.version());
                assert_eq!(view.bytes(), owned.bytes());
                Ok(())
            })
            .expect("borrowed read");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn borrowed_admission_rejects_corrupt_identity_envelope_schema_and_partial_file() {
        let (store, root) = test_store("corruption");
        let object = bytes_object(b"canonical value".to_vec());
        store.write_object(&object).expect("write object");

        corrupt_claim(&store, object.id(), |envelope| envelope[0] ^= 0xff);
        corrupt_claim(&store, object.id(), |envelope| {
            envelope[OBJECT_MAGIC.len()] ^= 0x80;
        });
        corrupt_claim(&store, object.id(), |envelope| {
            envelope[OBJECT_MAGIC.len() + 32] ^= 0x01;
        });

        let path = store.object_path(object.id());
        let original = fs::read(&path).expect("read complete envelope");
        replace_fixture(&path, &original[..original.len() - 1]);
        assert_eq!(store.read_object(object.id()), Err(StoreError::Corrupt));
        assert_eq!(
            store.with_verified_object(object.id(), |_| Ok(())),
            Err(StoreError::Corrupt)
        );
        replace_fixture(&path, &original);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn opened_object_admission_uses_the_supplied_descriptor() {
        let (store, root) = test_store("opened-descriptor");
        let expected = bytes_object(b"expected object".to_vec());
        let opened = bytes_object(b"different object".to_vec());
        store
            .write_object(&expected)
            .expect("write expected object");
        store.write_object(&opened).expect("write other object");

        let file = super::super::artifact_fs::open_object(&store, opened.id())
            .expect("open other object")
            .expect("other object exists");
        assert_eq!(
            store.with_verified_object_file(
                file,
                UntrustedObjectId::from_bytes(*expected.id().as_bytes()),
                store.object_envelope_limit().expect("object limit"),
                |_| Ok(()),
            ),
            Err(StoreError::Corrupt)
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn owned_object_read_rejects_a_symlinked_object_path() {
        let (store, root) = test_store("symlink-object");
        let object = bytes_object(b"symlink target".to_vec());
        let target = root.join("outside-object");
        fs::write(&target, b"not an object envelope").expect("write symlink target");
        let path = store.object_path(object.id());
        std::os::unix::fs::symlink(&target, &path).expect("replace path with symlink");

        assert_eq!(store.read_object(object.id()), Err(StoreError::UnsafePath));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn owned_object_read_rejects_a_hard_linked_object_path() {
        let (store, root) = test_store("hardlink-object");
        let object = bytes_object(b"hard-linked object".to_vec());
        store.write_object(&object).expect("write object");
        let path = store.object_path(object.id());
        let alias = root.join("object-alias");
        fs::hard_link(&path, &alias).expect("create hostile hard link");

        assert_eq!(store.read_object(object.id()), Err(StoreError::UnsafePath));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bounded_reader_checks_sparse_oversize_before_allocating() {
        let (store, root) = test_store("oversize");
        let claim = UntrustedObjectId::from_bytes([0x9a; 32]);
        let path = store
            .root()
            .join("objects")
            .join(format!("{}.object", super::super::hex(claim.as_bytes())));
        let file = File::create(&path).expect("create sparse object");
        let oversized_len = u64::try_from(store.object_envelope_limit().expect("envelope limit"))
            .expect("limit fits")
            + 1;
        file.set_len(oversized_len)
            .expect("grow sparse object past the bound");
        assert_eq!(store.read_object_claim(claim), Err(StoreError::Bounds));
        assert!(matches!(
            store.with_verified_object_claim(claim, |_| Ok(())),
            Err(StoreError::Bounds)
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn empty_and_maximum_bounded_payloads_are_admitted() {
        let (store, root) = test_store("payload-bounds");
        let envelope_limit = envelope_limit(store.max_pack_bytes()).expect("write envelope limit");
        let header_bytes = OBJECT_MAGIC.len() + 32 + 1 + 2 + 1 + 32 + 32 + 8;
        let maximum_payload = envelope_limit
            .checked_sub(header_bytes)
            .expect("object header fits envelope");
        let objects = [
            bytes_object(Vec::new()),
            bytes_object(vec![0x5a; maximum_payload]),
        ];
        for object in objects {
            store.write_object(&object).expect("write bounded object");
            store
                .with_verified_object(object.id(), |view| {
                    assert_eq!(view.bytes(), object.bytes());
                    Ok(())
                })
                .expect("read bounded object");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn callback_error_and_panic_release_the_convenience_pin() {
        let (store, root) = test_store("callback-exit");
        let object = bytes_object(b"callback result".to_vec());
        store.write_object(&object).expect("write object");
        assert!(matches!(
            store.with_verified_object(object.id(), |_| -> Result<(), StoreError> {
                Err(StoreError::TargetMismatch)
            }),
            Err(StoreError::TargetMismatch)
        ));
        assert_eq!(
            store
                .try_with_gc_exclusive_lease(|| Ok(()))
                .expect("exclusive lease after early error"),
            Some(())
        );

        let panic = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _ = store.with_verified_object(object.id(), |_| -> Result<(), StoreError> {
                panic!("callback panic fixture")
            });
        }));
        assert!(panic.is_err());
        assert_eq!(
            store
                .try_with_gc_exclusive_lease(|| Ok(()))
                .expect("exclusive lease after callback panic"),
            Some(())
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn existing_pin_is_reused_and_rejected_for_another_store() {
        let (store, root) = test_store("pinned-read");
        let (other, other_root) = test_store("foreign-pin");
        let object = bytes_object(b"pinned".to_vec());
        store.write_object(&object).expect("write object");
        let pin = store.pin_garbage_collection().expect("pin object store");
        assert_eq!(
            store
                .with_verified_object_pinned(&pin, object.id(), |view| Ok(view.bytes().len()))
                .expect("read with existing pin"),
            object.bytes().len()
        );
        assert!(matches!(
            other.with_verified_object_pinned(&pin, object.id(), |_| Ok(())),
            Err(StoreError::Corrupt)
        ));
        drop(pin);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(other_root);
    }

    #[test]
    fn callback_view_survives_path_replacement_and_gc_waits_for_the_callback() {
        let (store, root) = test_store("gc-callback");
        let object = bytes_object(b"view remains valid through callback".to_vec());
        store.write_object(&object).expect("write object");
        let path = store.object_path(object.id());
        let displaced = path.with_extension("held");
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let collector = store.clone();

        let worker = store
            .with_verified_object(object.id(), |view| {
                assert_eq!(view.bytes(), object.bytes());
                fs::rename(&path, &displaced).expect("replace the opened path");
                fs::write(&path, b"replacement").expect("write replacement path");
                assert_eq!(view.bytes(), object.bytes());
                let worker = std::thread::spawn(move || {
                    started_tx.send(()).expect("signal collector start");
                    let result = collector.collect_garbage(
                        &super::super::GcRoots::new(),
                        super::super::GcLimits::default(),
                    );
                    finished_tx
                        .send(result.is_ok())
                        .expect("signal collector completion");
                });
                started_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("collector started");
                assert!(
                    finished_rx.recv_timeout(Duration::from_millis(75)).is_err(),
                    "GC must wait for the callback's shared pin"
                );
                fs::remove_file(&path).expect("remove replacement path");
                fs::rename(&displaced, &path).expect("restore original object path");
                Ok(worker)
            })
            .expect("borrowed callback");

        worker.join().expect("join collector");
        assert!(
            finished_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("collector resumes after callback")
        );
        assert!(!store.contains_object(object.id()).expect("object metadata"));
        let _ = fs::remove_dir_all(root);
    }
}
