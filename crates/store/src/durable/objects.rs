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
    for name in ["packs", "objects", "closures"] {
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
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
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

fn create_temp(path: &Path) -> Result<(PathBuf, File), StoreError> {
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

pub(super) fn decode_object(
    bytes: &[u8],
    max_bytes: usize,
    registry: &RelationAdmissionRegistry,
    expected: UntrustedObjectId,
) -> Result<TypedObject, StoreError> {
    let view = verify_object_view(bytes, max_bytes, registry, Some(expected))?;
    let object = TypedObject::from_wire_parts(
        view.schema,
        view.key,
        view.version,
        view.bytes.to_vec().into_boxed_slice(),
    );
    Ok(object)
}

/// Parses and authenticates one complete object envelope while retaining only
/// borrows into its caller-owned, already bounded byte buffer.
fn verify_object_view<'a>(
    bytes: &'a [u8],
    max_bytes: usize,
    registry: &RelationAdmissionRegistry,
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
    if registry.contains_schema(schema) {
        registry.admit_relation_object(schema, &key, &version, payload)?;
    } else {
        admit_backend_object_version(schema, payload, &version)?;
    }

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
        let claimed_id = ObjectId::from_bytes(*claim.as_bytes());
        let mut file =
            super::artifact_fs::open_object(self, claimed_id)?.ok_or(StoreError::Corrupt)?;
        let maximum = self.object_envelope_limit()?;
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
        let object = verify_object_view(&envelope, maximum, &self.relation_registry, Some(claim))?;
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

    fn corrupt_claim(store: &FileStore, id: ObjectId, mutate: impl FnOnce(&mut Vec<u8>)) {
        let path = store.object_path(id);
        let original = fs::read(&path).expect("read fixture envelope");
        let mut changed = original.clone();
        mutate(&mut changed);
        fs::write(&path, changed).expect("write corrupted envelope");
        assert_eq!(store.read_object(id), Err(StoreError::Corrupt));
        assert_eq!(
            store.with_verified_object(id, |_| Ok(())),
            Err(StoreError::Corrupt)
        );
        fs::write(path, original).expect("restore fixture envelope");
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
        fs::write(&path, &original[..original.len() - 1]).expect("truncate object");
        assert_eq!(store.read_object(object.id()), Err(StoreError::Corrupt));
        assert_eq!(
            store.with_verified_object(object.id(), |_| Ok(())),
            Err(StoreError::Corrupt)
        );
        fs::write(&path, original).expect("restore complete envelope");
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
