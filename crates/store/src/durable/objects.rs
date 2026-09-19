//! Immutable pack and typed-object filesystem admission.

use super::{
    LayoutId, OBJECT_MAGIC, ObjectId, PACK_MAGIC, Pack, Path, PathBuf, RelationAdmissionRegistry,
    StoreError, TEMP_COUNTER, TypedObject, envelope_limit, fs, io_error, put_u32, put_u64,
    read_byte, read_hash, read_u32, read_u64, sync_directory,
};
use crate::{UntrustedObjectId, WirePack};
use backend_version::SchemaIdentity;
use std::sync::atomic::Ordering;
use std::{
    fs::{File, OpenOptions},
    io::Write,
};

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
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(io_error(&error));
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
) -> Result<TypedObject, StoreError> {
    if bytes.len() > max_bytes || !bytes.starts_with(OBJECT_MAGIC) {
        return Err(if bytes.len() > max_bytes {
            StoreError::Bounds
        } else {
            StoreError::Corrupt
        });
    }
    let mut at = OBJECT_MAGIC.len();
    let claimed = UntrustedObjectId::from_bytes(read_hash(bytes, &mut at)?);
    let domain = read_byte(bytes, &mut at)?;
    let ty = u16::from_le_bytes(
        bytes
            .get(at..at.checked_add(2).ok_or(StoreError::Bounds)?)
            .ok_or(StoreError::Corrupt)?
            .try_into()
            .map_err(|_| StoreError::Corrupt)?,
    );
    at = at.checked_add(2).ok_or(StoreError::Bounds)?;
    let version_tag = read_byte(bytes, &mut at)?;
    let key = read_hash(bytes, &mut at)?;
    let version = read_hash(bytes, &mut at)?;
    let length = usize::try_from(read_u64(bytes, &mut at)?).map_err(|_| StoreError::Bounds)?;
    let end = at.checked_add(length).ok_or(StoreError::Bounds)?;
    let object_bytes = bytes.get(at..end).ok_or(StoreError::Corrupt)?.to_vec();
    at = end;
    if at != bytes.len() {
        return Err(StoreError::Corrupt);
    }
    let object = TypedObject::from_wire_parts(
        SchemaIdentity::new(domain, ty, version_tag),
        key,
        version,
        object_bytes.into_boxed_slice(),
    );
    object.verify_wire_version(registry)?;
    let _ = claimed.admit(&object)?;
    Ok(object)
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
