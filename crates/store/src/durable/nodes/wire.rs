//! Checked descriptor and relation-reference wire framing.

use super::super::{
    ClosureId, Hash, LayoutId, ObjectId, PackId, StoreError, io_error, read_byte, read_hash,
    sync_directory,
};
use super::{
    MANIFEST_INDEX_MAGIC, MANIFEST_INDEX_MAGIC_V1, ManifestDescriptor, RELATION_REF_MAGIC,
    TREE_PACK_MAGIC, TreePackDescriptor,
};
use crate::digest;
use backend_version::SchemaIdentity;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static NODE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(in crate::durable) fn is_manifest_descriptor(bytes: &[u8]) -> bool {
    bytes.starts_with(MANIFEST_INDEX_MAGIC) || bytes.starts_with(MANIFEST_INDEX_MAGIC_V1)
}

pub(in crate::durable) fn encode_manifest_descriptor(
    id: ClosureId,
    root: ObjectId,
    count: usize,
) -> Result<Vec<u8>, StoreError> {
    let mut bytes = Vec::with_capacity(MANIFEST_INDEX_MAGIC.len() + (32 * 3) + 8);
    bytes.extend_from_slice(MANIFEST_INDEX_MAGIC);
    bytes.extend_from_slice(id.as_bytes());
    bytes.extend_from_slice(root.as_bytes());
    bytes.extend_from_slice(
        &u64::try_from(count)
            .map_err(|_| StoreError::Bounds)?
            .to_le_bytes(),
    );
    let checksum = digest(b"store.manifest-index.descriptor.v1\0", &bytes);
    bytes.extend_from_slice(&checksum);
    Ok(bytes)
}

pub(in crate::durable) fn decode_manifest_descriptor(
    bytes: &[u8],
    expected_id: ClosureId,
) -> Result<ManifestDescriptor, StoreError> {
    let v2_len = MANIFEST_INDEX_MAGIC.len() + (32 * 3) + 8;
    let v1_len = MANIFEST_INDEX_MAGIC_V1.len() + (32 * 3);
    let (magic, count) = if bytes.len() == v2_len && bytes.starts_with(MANIFEST_INDEX_MAGIC) {
        let offset = MANIFEST_INDEX_MAGIC.len() + 32 + 32;
        let count = usize::try_from(
            bytes
                .get(offset..offset.checked_add(8).ok_or(StoreError::Bounds)?)
                .ok_or(StoreError::Corrupt)?
                .try_into()
                .map(u64::from_le_bytes)
                .map_err(|_| StoreError::Corrupt)?,
        )
        .map_err(|_| StoreError::Bounds)?;
        (MANIFEST_INDEX_MAGIC, Some(count))
    } else if bytes.len() == v1_len && bytes.starts_with(MANIFEST_INDEX_MAGIC_V1) {
        (MANIFEST_INDEX_MAGIC_V1, None)
    } else {
        return Err(StoreError::Corrupt);
    };
    let mut at = magic.len();
    let id = ClosureId::from_bytes(read_hash(bytes, &mut at)?);
    let root = ObjectId::from_bytes(read_hash(bytes, &mut at)?);
    if count.is_some() {
        at = at.checked_add(8).ok_or(StoreError::Bounds)?;
    }
    let checksum = read_hash(bytes, &mut at)?;
    if at != bytes.len()
        || id != expected_id
        || digest(
            b"store.manifest-index.descriptor.v1\0",
            &bytes[..bytes.len().checked_sub(32).ok_or(StoreError::Corrupt)?],
        ) != checksum
    {
        return Err(StoreError::Corrupt);
    }
    Ok(ManifestDescriptor { id, root, count })
}

pub(super) fn encode_tree_descriptor(descriptor: TreePackDescriptor) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(TREE_PACK_MAGIC.len() + (32 * 4) + 32);
    bytes.extend_from_slice(TREE_PACK_MAGIC);
    bytes.extend_from_slice(descriptor.id.as_bytes());
    bytes.extend_from_slice(descriptor.layout.as_bytes());
    bytes.extend_from_slice(&descriptor.target);
    bytes.extend_from_slice(&descriptor.root);
    let checksum = digest(b"store.tree-pack.descriptor.v1\0", &bytes);
    bytes.extend_from_slice(&checksum);
    bytes
}

pub(super) fn decode_tree_descriptor(
    bytes: &[u8],
    expected_id: PackId,
) -> Result<TreePackDescriptor, StoreError> {
    let expected_len = TREE_PACK_MAGIC.len() + (32 * 5);
    if bytes.len() != expected_len || !bytes.starts_with(TREE_PACK_MAGIC) {
        return Err(StoreError::Corrupt);
    }
    let mut at = TREE_PACK_MAGIC.len();
    let id = PackId::from_wire(read_hash(bytes, &mut at)?);
    let layout = LayoutId::from_bytes(read_hash(bytes, &mut at)?);
    let target = read_hash(bytes, &mut at)?;
    let root = read_hash(bytes, &mut at)?;
    let checksum = read_hash(bytes, &mut at)?;
    if at != bytes.len()
        || id != expected_id
        || digest(
            b"store.tree-pack.descriptor.v1\0",
            &bytes[..bytes.len() - 32],
        ) != checksum
    {
        return Err(StoreError::Corrupt);
    }
    let mut identity = Vec::with_capacity(96);
    identity.extend_from_slice(layout.as_bytes());
    identity.extend_from_slice(&target);
    identity.extend_from_slice(&root);
    if digest(b"store.tree-pack.v1\0", &identity) != *id.as_bytes() {
        return Err(StoreError::Corrupt);
    }
    Ok(TreePackDescriptor {
        id,
        layout,
        target,
        root,
    })
}

pub(super) fn relation_ref_encoded_len() -> usize {
    RELATION_REF_MAGIC.len() + 1 + 2 + 1 + 32 + 32 + 32
}

fn encode_relation_ref(schema: SchemaIdentity, version: &Hash, object: ObjectId) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(relation_ref_encoded_len());
    bytes.extend_from_slice(RELATION_REF_MAGIC);
    bytes.push(schema.domain());
    bytes.extend_from_slice(&schema.ty().to_le_bytes());
    bytes.push(schema.version());
    bytes.extend_from_slice(version);
    bytes.extend_from_slice(object.as_bytes());
    let checksum = digest(b"store.relation-ref.v1\0", &bytes);
    bytes.extend_from_slice(&checksum);
    bytes
}

pub(super) fn decode_relation_ref(
    bytes: &[u8],
    expected_schema: SchemaIdentity,
    expected_version: &Hash,
) -> Result<ObjectId, StoreError> {
    if bytes.len() != relation_ref_encoded_len() || !bytes.starts_with(RELATION_REF_MAGIC) {
        return Err(StoreError::Corrupt);
    }
    let mut at = RELATION_REF_MAGIC.len();
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
    let version = read_hash(bytes, &mut at)?;
    let object = ObjectId::from_bytes(read_hash(bytes, &mut at)?);
    let checksum = read_hash(bytes, &mut at)?;
    let schema = SchemaIdentity::new(domain, ty, version_tag);
    if at != bytes.len()
        || schema != expected_schema
        || version != *expected_version
        || digest(
            b"store.relation-ref.v1\0",
            &bytes[..bytes.len().checked_sub(32).ok_or(StoreError::Corrupt)?],
        ) != checksum
    {
        return Err(StoreError::Corrupt);
    }
    Ok(object)
}

pub(super) fn write_relation_ref(
    path: &Path,
    schema: SchemaIdentity,
    version: &Hash,
    object: ObjectId,
    directory: &Path,
) -> Result<bool, StoreError> {
    let bytes = encode_relation_ref(schema, version, object);
    write_immutable_descriptor_with_status(path, &bytes, directory)
}

pub(super) fn write_immutable_descriptor(
    path: &Path,
    bytes: &[u8],
    directory: &Path,
) -> Result<(), StoreError> {
    write_immutable_descriptor_with_status(path, bytes, directory).map(|_| ())
}

fn write_immutable_descriptor_with_status(
    path: &Path,
    bytes: &[u8],
    directory: &Path,
) -> Result<bool, StoreError> {
    if let Ok(existing) = fs::read(path) {
        return if existing == bytes {
            Ok(false)
        } else {
            Err(StoreError::Corrupt)
        };
    }
    let temporary = path.with_file_name(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("tree"),
        std::process::id(),
        NODE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed),
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| io_error(&error))?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(io_error(&error));
    }
    let result = match fs::hard_link(&temporary, path) {
        Ok(()) => sync_directory(directory).map(|()| true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = fs::read(path).map_err(|read_error| io_error(&read_error))?;
            if existing == bytes {
                Ok(false)
            } else {
                Err(StoreError::Corrupt)
            }
        }
        Err(error) => Err(io_error(&error)),
    };
    let _ = fs::remove_file(&temporary);
    result
}

pub(super) fn hex(bytes: &Hash) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    output
}
