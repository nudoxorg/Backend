//! Compact authenticated workspace closure index and store admission.

use super::owner::WorkspaceError;
use super::transition::{PreparedTransition, TRANSITION_PAYLOAD_COUNT, TransactionId};
use backend_store::{
    DurableManifest, FileStore, LayoutId, ObjectId, OrderedMap, StoreError, StoredValue,
    encode_pack,
};

pub(crate) const WORKSPACE_PACK_KEY: &[u8] = b"backend.workspace.closure-index.v3\0";
pub(crate) const WORKSPACE_PACK_MAGIC: &[u8] = b"backend.workspace.index.v3\0";
pub(crate) const WORKSPACE_PACK_LAYOUT: &[u8] = b"backend-engine.workspace-closure-index.v3\0";
const MAX_AUXILIARY_OBJECTS: usize = 128;
const AUXILIARY_COUNT_BYTES: usize = 2;

pub(crate) fn workspace_pack_layout() -> LayoutId {
    LayoutId::derive(WORKSPACE_PACK_LAYOUT)
}

/// Fixed authenticated pointers needed to recover a typed workspace
/// transition without scanning the closure's compatibility export.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WorkspacePackIndex {
    pub(super) transaction: TransactionId,
    pub(super) payloads: [ObjectId; TRANSITION_PAYLOAD_COUNT],
    pub(super) auxiliary: Vec<ObjectId>,
    pub(super) catalog_descriptor: Option<ObjectId>,
}

/// Builds the canonical physical materialization for one checked workspace
/// closure.  The store's generic pack format is used as the immutable byte
/// envelope, while the workspace binding remains the typed root-of-roots in
/// `CheckedWorkspacePublication`.  Keeping the derivation here means a pack
/// can never be selected merely because it has a matching layout or because
/// it happens to be an empty map.
pub(super) fn workspace_pack(
    transition: &PreparedTransition,
    max_bytes: usize,
) -> Result<(LayoutId, backend_store::Pack), StoreError> {
    // The pack is a fixed-size authenticated pointer. Object bytes and the
    // persistent closure index live once in the store's closure/object
    // files; putting every object ID into every generation would create a
    // second, O(closure) authority beside the store HEAD.
    if transition.auxiliary_object_ids().len() > MAX_AUXILIARY_OBJECTS {
        return Err(StoreError::Bounds);
    }
    let index_bytes = 32usize
        .checked_mul(4 + TRANSITION_PAYLOAD_COUNT)
        .and_then(|bytes| bytes.checked_add(AUXILIARY_COUNT_BYTES + 32 * MAX_AUXILIARY_OBJECTS))
        .and_then(|bytes| WORKSPACE_PACK_MAGIC.len().checked_add(bytes))
        .ok_or(StoreError::Bounds)?;
    let mut index = Vec::with_capacity(index_bytes);
    index.extend_from_slice(WORKSPACE_PACK_MAGIC);
    index.extend_from_slice(&transition.target().to_bytes());
    index.extend_from_slice(transition.closure().manifest().id().as_bytes());
    index.extend_from_slice(&transition.transaction().as_bytes());
    for object in transition.payload_object_ids() {
        index.extend_from_slice(object.as_bytes());
    }
    let auxiliary = transition.auxiliary_object_ids();
    index.extend_from_slice(
        &u16::try_from(auxiliary.len())
            .map_err(|_| StoreError::Bounds)?
            .to_be_bytes(),
    );
    for object in auxiliary {
        index.extend_from_slice(object.as_bytes());
    }
    for _ in auxiliary.len()..MAX_AUXILIARY_OBJECTS {
        index.extend_from_slice(&[0; 32]);
    }
    index.extend_from_slice(
        &transition
            .catalog_descriptor()
            .map_or([0; 32], |object| *object.as_bytes()),
    );
    let key = WORKSPACE_PACK_KEY.to_vec();
    let entries = [(key, StoredValue::new(index, 1, Vec::new()))];
    let map = OrderedMap::try_from_iter(entries)?;
    let layout = workspace_pack_layout();
    let pack = encode_pack(&map, layout, max_bytes)?;
    Ok((layout, pack))
}

pub(crate) fn verify_workspace_pack(
    store: &FileStore,
    transition: &PreparedTransition,
    descriptor: &backend_store::PublicationDescriptor,
    max_bytes: usize,
) -> Result<(), WorkspaceError> {
    let (layout, expected) =
        workspace_pack(transition, max_bytes).map_err(WorkspaceError::store)?;
    if descriptor.layout() != layout || descriptor.pack() != expected.id() {
        return Err(WorkspaceError::Store(
            "physical workspace pack identity disagrees with checked closure".to_owned(),
        ));
    }
    let actual = store
        .read_pack(descriptor.pack())
        .map_err(WorkspaceError::store)?;
    if actual.layout() != layout || actual.id() != expected.id() {
        return Err(WorkspaceError::Store(
            "physical workspace pack failed identity admission".to_owned(),
        ));
    }
    let actual_map = backend_store::decode_pack(&actual).map_err(WorkspaceError::store)?;
    let expected_map = backend_store::decode_pack(&expected).map_err(WorkspaceError::store)?;
    if actual_map != expected_map {
        return Err(WorkspaceError::Store(
            "physical workspace pack does not match checked closure".to_owned(),
        ));
    }
    Ok(())
}

/// Reads the fixed-size authenticated index that accompanies one selected
/// workspace closure. The index carries the owner transaction, target root,
/// and closure identity. Object bytes and typed references remain in the
/// authenticated store closure/object files and are never copied into the
/// pack.
pub(super) fn read_workspace_pack_index(
    store: &FileStore,
    descriptor: &backend_store::PublicationDescriptor,
    manifest: &DurableManifest,
) -> Result<WorkspacePackIndex, WorkspaceError> {
    if manifest.id() != descriptor.closure() {
        return Err(WorkspaceError::Store(
            "workspace index closure descriptor mismatch".to_owned(),
        ));
    }
    let pack = store
        .read_pack(descriptor.pack())
        .map_err(WorkspaceError::store)?;
    let expected_layout = workspace_pack_layout();
    if pack.layout() != expected_layout {
        return Err(WorkspaceError::Store(
            "workspace index layout mismatch".to_owned(),
        ));
    }
    let map = backend_store::decode_pack(&pack).map_err(WorkspaceError::store)?;
    if map.len() != 1 {
        return Err(WorkspaceError::Store(
            "workspace index entry count mismatch".to_owned(),
        ));
    }
    let value = map
        .get(WORKSPACE_PACK_KEY)
        .ok_or_else(|| WorkspaceError::Store("workspace index entry is missing".to_owned()))?;
    let expected_len = WORKSPACE_PACK_MAGIC
        .len()
        .checked_add(
            32usize
                .checked_mul(4 + TRANSITION_PAYLOAD_COUNT)
                .ok_or(WorkspaceError::Bounds)?,
        )
        .and_then(|bytes| bytes.checked_add(AUXILIARY_COUNT_BYTES + 32 * MAX_AUXILIARY_OBJECTS))
        .ok_or(WorkspaceError::Bounds)?;
    if value.availability != 1 || value.value.len() != expected_len {
        return Err(WorkspaceError::Store(
            "workspace index value is malformed".to_owned(),
        ));
    }
    let mut at = WORKSPACE_PACK_MAGIC.len();
    if value.value.get(..WORKSPACE_PACK_MAGIC.len()) != Some(WORKSPACE_PACK_MAGIC) {
        return Err(WorkspaceError::Store(
            "workspace index magic mismatch".to_owned(),
        ));
    }
    let target = take_store_fixed::<32>(&value.value, &mut at)?;
    let encoded_closure_id = take_store_fixed::<32>(&value.value, &mut at)?;
    let transaction = take_store_fixed::<32>(&value.value, &mut at)?;
    let payloads = read_pack_payloads(store, manifest, &value.value, &mut at)?;
    let auxiliary = read_pack_auxiliary(manifest, &value.value, &mut at, &payloads)?;
    let encoded_catalog_descriptor = take_store_fixed::<32>(&value.value, &mut at)?;
    if target != descriptor.target() || encoded_closure_id != *descriptor.closure().as_bytes() {
        return Err(WorkspaceError::Store(
            "workspace index binding mismatch".to_owned(),
        ));
    }
    if !value.references.is_empty() {
        return Err(WorkspaceError::Store(
            "workspace index has unexpected object references".to_owned(),
        ));
    }
    if at != value.value.len() {
        return Err(WorkspaceError::Corrupt("workspace index trailing bytes"));
    }
    let catalog_descriptor = if encoded_catalog_descriptor == [0; 32] {
        None
    } else {
        let claim = backend_store::UntrustedObjectId::from_bytes(encoded_catalog_descriptor);
        let object = store
            .read_object_claim(claim)
            .map_err(WorkspaceError::store)?;
        let object_id = claim.admit(&object).map_err(WorkspaceError::store)?;
        if manifest
            .get(object_id)
            .map_err(WorkspaceError::store)?
            .is_none()
        {
            return Err(WorkspaceError::Store(
                "catalog descriptor is outside selected closure".to_owned(),
            ));
        }
        Some(object_id)
    };
    Ok(WorkspacePackIndex {
        transaction: TransactionId::from_bytes(transaction),
        payloads,
        auxiliary,
        catalog_descriptor,
    })
}

fn read_pack_payloads(
    store: &FileStore,
    manifest: &DurableManifest,
    bytes: &[u8],
    at: &mut usize,
) -> Result<[ObjectId; TRANSITION_PAYLOAD_COUNT], WorkspaceError> {
    let mut payloads = Vec::with_capacity(TRANSITION_PAYLOAD_COUNT);
    for _ in 0..TRANSITION_PAYLOAD_COUNT {
        let raw = take_store_fixed(bytes, at)?;
        let untrusted = backend_store::UntrustedObjectId::from_bytes(raw);
        let object = store
            .read_object_claim(untrusted)
            .map_err(WorkspaceError::store)?;
        let object_id = untrusted.admit(&object).map_err(WorkspaceError::store)?;
        let selected = manifest
            .get(object_id)
            .map_err(WorkspaceError::store)?
            .ok_or_else(|| {
                WorkspaceError::Store("workspace payload is outside selected closure".to_owned())
            })?;
        if selected.id() != object_id {
            return Err(WorkspaceError::Store(
                "workspace payload identity mismatch".to_owned(),
            ));
        }
        payloads.push(object_id);
    }
    payloads
        .try_into()
        .map_err(|_| WorkspaceError::Corrupt("workspace index payload count"))
}

fn read_pack_auxiliary(
    manifest: &DurableManifest,
    bytes: &[u8],
    at: &mut usize,
    payloads: &[ObjectId; TRANSITION_PAYLOAD_COUNT],
) -> Result<Vec<ObjectId>, WorkspaceError> {
    let auxiliary_count = usize::from(u16::from_be_bytes(take_store_fixed(bytes, at)?));
    if auxiliary_count > MAX_AUXILIARY_OBJECTS {
        return Err(WorkspaceError::Bounds);
    }
    let mut auxiliary = Vec::with_capacity(auxiliary_count);
    for index in 0..MAX_AUXILIARY_OBJECTS {
        let raw = take_store_fixed(bytes, at)?;
        if index < auxiliary_count {
            let object_id = manifest
                .admit_claim(backend_store::UntrustedObjectId::from_bytes(raw))
                .map_err(WorkspaceError::store)?
                .ok_or_else(|| {
                    WorkspaceError::Store(
                        "workspace auxiliary object is outside selected closure".to_owned(),
                    )
                })?;
            if payloads.contains(&object_id) || auxiliary.contains(&object_id) {
                return Err(WorkspaceError::Corrupt(
                    "workspace auxiliary object identity is duplicated",
                ));
            }
            auxiliary.push(object_id);
        } else if raw != [0; 32] {
            return Err(WorkspaceError::Corrupt(
                "workspace auxiliary padding is nonzero",
            ));
        }
    }
    Ok(auxiliary)
}

fn take_store_fixed<const N: usize>(
    bytes: &[u8],
    at: &mut usize,
) -> Result<[u8; N], WorkspaceError> {
    let end = at.checked_add(N).ok_or(WorkspaceError::Bounds)?;
    let value = bytes
        .get(*at..end)
        .ok_or(WorkspaceError::Corrupt("workspace index field"))?;
    *at = end;
    value
        .try_into()
        .map_err(|_| WorkspaceError::Corrupt("workspace index field"))
}
