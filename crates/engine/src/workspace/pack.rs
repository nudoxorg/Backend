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
pub(super) const MAX_AUXILIARY_OBJECTS: usize = 128;
const AUXILIARY_COUNT_BYTES: usize = 2;

pub(crate) fn workspace_pack_layout() -> LayoutId {
    LayoutId::derive(WORKSPACE_PACK_LAYOUT)
}

/// Fixed authenticated pointers needed to recover a typed workspace
/// transition without scanning the closure's compatibility export.
#[derive(Debug)]
pub(super) struct WorkspacePackIndex {
    pub(super) transaction: TransactionId,
    pub(super) payloads: [ObjectId; TRANSITION_PAYLOAD_COUNT],
    pub(super) auxiliary: Vec<ObjectId>,
    pub(super) catalog_descriptor: Option<ObjectId>,
    pub(super) frontier: AuthenticatedRecoveryFrontier,
}

/// Exact claims parsed from the selected owner's authenticated one-record
/// pack. The retained selected capability holds the store process lock and
/// pin; typed transition/receipt admission remains mandatory.
pub(super) struct RetirementPackFrontier<'a> {
    selected: &'a backend_version::retirement_budget::Metered<backend_store::RetirementSelectedHead<'a>>,
    target: [u8; 32],
    closure: backend_store::ClosureId,
    transaction: TransactionId,
    payloads: [backend_store::UntrustedObjectId; TRANSITION_PAYLOAD_COUNT],
    auxiliary: [backend_store::UntrustedObjectId; MAX_AUXILIARY_OBJECTS],
    auxiliary_len: usize,
}

impl RetirementPackFrontier<'_> {
    pub(super) fn store(&self) -> &FileStore { self.selected.get().store() }
    pub(super) fn pin(&self) -> &backend_store::GcPinGuard { self.selected.get().pin() }
    pub(super) fn closure(&self) -> backend_store::ClosureId { self.closure }
    pub(super) fn target(&self) -> [u8; 32] { self.target }
    pub(super) fn transaction(&self) -> TransactionId { self.transaction }
    pub(super) fn ids(&self) -> impl Iterator<Item = backend_store::UntrustedObjectId> + '_ {
        self.payloads.iter().chain(self.auxiliary[..self.auxiliary_len].iter()).copied()
    }
    pub(super) fn belongs_to_budget(&self, budget: &backend_version::retirement_budget::RetirementBudget) -> bool {
        budget.owns(self.selected)
    }
}

/// Read the selected fixed pack using the owner/store-bound capability. This
/// is not a typed transition admission and rejects catalog-bearing packs in
/// this first slice; no caller-selected head or store is accepted.
pub(super) fn read_retirement_pack_frontier<'a>(
    selected: &'a backend_version::retirement_budget::Metered<backend_store::RetirementSelectedHead<'a>>,
    budget: &backend_version::retirement_budget::RetirementBudget,
    bookkeeping: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<backend_version::retirement_budget::Metered<RetirementPackFrontier<'a>>, StoreError> {
    if !budget.owns(selected) || cancelled() { return Err(StoreError::Bounds); }
    let descriptor = selected.get().selected().descriptor();
    let layout = workspace_pack_layout();
    if descriptor.layout() != layout { return Err(StoreError::Corrupt); }
    let value_len = WORKSPACE_PACK_MAGIC.len().checked_add(32usize.checked_mul(4 + TRANSITION_PAYLOAD_COUNT).ok_or(StoreError::Bounds)?)
        .and_then(|n| n.checked_add(AUXILIARY_COUNT_BYTES + 32 * MAX_AUXILIARY_OBJECTS)).ok_or(StoreError::Bounds)?;
    let output = budget.reserve(std::mem::size_of::<RetirementPackFrontier<'a>>().checked_add(bookkeeping).ok_or(StoreError::Bounds)?)
        .map_err(|_| StoreError::Bounds)?;
    let value = selected.get().store().read_single_entry_pack_retirement(
        selected.get().pin(), descriptor.pack(), layout, WORKSPACE_PACK_KEY, value_len, budget, bookkeeping, cancelled,
    )?;
    let bytes = value.get().as_slice();
    if bytes.get(..WORKSPACE_PACK_MAGIC.len()) != Some(WORKSPACE_PACK_MAGIC) { return Err(StoreError::Corrupt); }
    fn take<const N: usize>(bytes: &[u8], at: &mut usize) -> Result<[u8; N], StoreError> {
        let end = at.checked_add(N).ok_or(StoreError::Bounds)?;
        let value = bytes.get(*at..end).ok_or(StoreError::Corrupt)?.try_into().map_err(|_| StoreError::Corrupt)?;
        *at = end;
        Ok(value)
    }
    let mut at = WORKSPACE_PACK_MAGIC.len();
    let target = take::<32>(bytes, &mut at)?;
    let closure = take::<32>(bytes, &mut at)?;
    let transaction = TransactionId::from_bytes(take::<32>(bytes, &mut at)?);
    if target != descriptor.target() || closure != *descriptor.closure().as_bytes() { return Err(StoreError::Corrupt); }
    let zero = backend_store::UntrustedObjectId::from_bytes([0; 32]);
    let mut payloads = [zero; TRANSITION_PAYLOAD_COUNT];
    for index in 0..TRANSITION_PAYLOAD_COUNT {
        let id = backend_store::UntrustedObjectId::from_bytes(take::<32>(bytes, &mut at)?);
        if id == zero || payloads[..index].contains(&id) { return Err(StoreError::Corrupt); }
        payloads[index] = id;
    }
    let count = usize::from(u16::from_be_bytes(take::<2>(bytes, &mut at)?));
    if count > MAX_AUXILIARY_OBJECTS { return Err(StoreError::Bounds); }
    let mut auxiliary = [zero; MAX_AUXILIARY_OBJECTS];
    for index in 0..MAX_AUXILIARY_OBJECTS {
        let id = backend_store::UntrustedObjectId::from_bytes(take::<32>(bytes, &mut at)?);
        if index < count {
            if id == zero || payloads.contains(&id) || (index > 0 && auxiliary[index - 1] >= id) { return Err(StoreError::Corrupt); }
            auxiliary[index] = id;
        } else if id != zero { return Err(StoreError::Corrupt); }
    }
    if take::<32>(bytes, &mut at)? != [0; 32] || at != bytes.len() || cancelled() { return Err(StoreError::Bounds); }
    Ok(output.meter(RetirementPackFrontier { selected, target, closure: descriptor.closure(), transaction, payloads, auxiliary, auxiliary_len: count }))
}

/// A closed witness minted only by canonical selected-pack admission below.
#[derive(Debug)]
pub(super) struct AuthenticatedRecoveryFrontier {
    target: [u8; 32],
    closure: backend_store::ClosureId,
    transaction: TransactionId,
    payloads: [ObjectId; TRANSITION_PAYLOAD_COUNT],
    auxiliary: Vec<ObjectId>,
}

impl AuthenticatedRecoveryFrontier {
    pub(super) fn admit_transition(
        self,
        transition: &PreparedTransition,
    ) -> Result<Vec<ObjectId>, WorkspaceError> {
        if transition.target().to_bytes() != self.target
            || transition.closure().membership_id() != self.closure
            || transition.transaction() != self.transaction
            || transition.payload_object_ids() != self.payloads
            || self.auxiliary.iter().any(|id| {
                !transition.auxiliary_object_ids().contains(id)
                    || !transition.closure().control_manifest().contains_object_id(*id)
            })
        {
            return Err(WorkspaceError::Corrupt(
                "authenticated recovery frontier differs from typed selected controls",
            ));
        }
        Ok(self.auxiliary)
    }
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
    index.extend_from_slice(transition.closure().membership_id().as_bytes());
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
    let canonical = backend_store::encode_pack(&map, expected_layout, pack.bytes().len())
        .map_err(WorkspaceError::store)?;
    if descriptor.layout() != expected_layout
        || pack.id() != descriptor.pack()
        || canonical.id() != pack.id()
    {
        return Err(WorkspaceError::Corrupt("workspace index is not canonical selected pack"));
    }
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
        frontier: AuthenticatedRecoveryFrontier {
            target,
            closure: manifest.id(),
            transaction: TransactionId::from_bytes(transaction),
            payloads,
            auxiliary: auxiliary.clone(),
        },
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
            if auxiliary.last().is_some_and(|previous| previous >= &object_id) {
                return Err(WorkspaceError::Corrupt(
                    "workspace auxiliary frontier is not in canonical order",
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
