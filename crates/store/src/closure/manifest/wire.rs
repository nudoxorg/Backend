//! Bounded flat closure wire compatibility encoding.

use super::{
    CLOSURE_MAGIC, ClosureManifest, MIN_OBJECT_BYTES, RelationAdmissionRegistry, StoreError,
    TypedObject, put_u32, put_u64, read_hash, read_u32, read_u64,
};
use backend_version::SchemaIdentity;

pub(super) fn encode(manifest: &ClosureManifest, max_bytes: usize) -> Result<Vec<u8>, StoreError> {
    let objects = manifest.objects();
    let mut output = Vec::new();
    output.extend_from_slice(CLOSURE_MAGIC);
    output.extend_from_slice(manifest.id.as_bytes());
    put_u32(&mut output, objects.len())?;
    for object in objects {
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
    }
    Ok(output)
}

pub(super) fn decode(bytes: &[u8], max_bytes: usize) -> Result<ClosureManifest, StoreError> {
    decode_with_registry(bytes, max_bytes, &RelationAdmissionRegistry::default())
}

pub(super) fn decode_with_registry(
    bytes: &[u8],
    max_bytes: usize,
    registry: &RelationAdmissionRegistry,
) -> Result<ClosureManifest, StoreError> {
    decode_inner(bytes, max_bytes, registry, true)
}

pub(super) fn decode_root_with_registry(
    bytes: &[u8],
    max_bytes: usize,
    registry: &RelationAdmissionRegistry,
) -> Result<ClosureManifest, StoreError> {
    decode_inner(bytes, max_bytes, registry, false)
}

fn decode_inner(
    bytes: &[u8],
    max_bytes: usize,
    registry: &RelationAdmissionRegistry,
    verify_edges: bool,
) -> Result<ClosureManifest, StoreError> {
    if bytes.len() > max_bytes || !bytes.starts_with(CLOSURE_MAGIC) {
        return Err(if bytes.len() > max_bytes {
            StoreError::Bounds
        } else {
            StoreError::Corrupt
        });
    }
    let mut at = CLOSURE_MAGIC.len();
    let claimed = read_hash(bytes, &mut at)?;
    let count = usize::try_from(read_u32(bytes, &mut at)?).map_err(|_| StoreError::Bounds)?;
    if count > bytes.len().saturating_sub(at) / MIN_OBJECT_BYTES {
        return Err(StoreError::Bounds);
    }
    let mut objects = Vec::with_capacity(count);
    for _ in 0..count {
        let domain = *bytes.get(at).ok_or(StoreError::Corrupt)?;
        at = at.checked_add(1).ok_or(StoreError::Bounds)?;
        let ty = u16::from_le_bytes(
            bytes
                .get(at..at.checked_add(2).ok_or(StoreError::Bounds)?)
                .ok_or(StoreError::Corrupt)?
                .try_into()
                .map_err(|_| StoreError::Corrupt)?,
        );
        at = at.checked_add(2).ok_or(StoreError::Bounds)?;
        let version_tag = *bytes.get(at).ok_or(StoreError::Corrupt)?;
        at = at.checked_add(1).ok_or(StoreError::Bounds)?;
        let key = read_hash(bytes, &mut at)?;
        let version = read_hash(bytes, &mut at)?;
        let length = usize::try_from(read_u64(bytes, &mut at)?).map_err(|_| StoreError::Bounds)?;
        let end = at.checked_add(length).ok_or(StoreError::Bounds)?;
        let object_bytes = bytes.get(at..end).ok_or(StoreError::Corrupt)?.to_vec();
        at = end;
        objects.push(TypedObject::from_wire_parts(
            SchemaIdentity::new(domain, ty, version_tag),
            key,
            version,
            object_bytes.into_boxed_slice(),
        ));
    }
    if at != bytes.len() {
        return Err(StoreError::Corrupt);
    }
    let manifest = ClosureManifest::new_index(objects)?;
    if verify_edges {
        manifest.admit_with_registry(registry)?;
    } else {
        manifest.admit_objects_with_registry(registry)?;
    }
    if manifest.id.as_bytes() != &claimed {
        return Err(StoreError::Corrupt);
    }
    Ok(manifest)
}
