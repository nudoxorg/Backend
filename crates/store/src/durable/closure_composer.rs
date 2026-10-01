//! ID-only closure composition over objects already admitted to the local CAS.
//!
//! The composer path-copies a checked closure index from canonical member IDs,
//! verifies every resulting member by streaming its existing envelope, and
//! publishes only the durable index descriptor. Payloads never become
//! `TypedObject` values and are never copied back into the CAS.

use super::{
    ArtifactClosureClaim, FileStore, StoreError, StoredClosureReceipt, artifact_fs, nodes,
};
use crate::closure::ManifestRelation;
use crate::{ClosureId, DurableManifest, ObjectId};
use backend_version::{
    DEFAULT_CUT_POLICY, IdContext, LazyTree, LazyTreeError, LazyTreeMetadataShape,
    LazyTreeUpdateBudget, PersistedTreeRoot, TreeChange, UntrustedId, admit_canonical_root_claim,
    canonical_empty,
};
#[cfg(test)]
use std::sync::Mutex;
use std::{fs::File, mem::size_of};

const MEMBER_PAGE_SIZE: usize = 128;
const FAULT_AFTER_NODES: u8 = 15;
const FAULT_AFTER_DESCRIPTOR: u8 = 16;

/// Limits for composing a closure index from already stored object IDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClosureCompositionBudget {
    /// Maximum number of members in the resulting closure.
    pub max_members: usize,
    /// Maximum number of ID additions and removals in one composition.
    pub max_changes: usize,
    /// Maximum aggregate payload bytes streamed while verifying the result.
    pub max_verified_payload_bytes: u64,
    /// Maximum conservative peak metadata charge for sorted edits, the
    /// per-change lazy frontier/overlay, and the genesis bulk-builder path.
    pub max_metadata_bytes: usize,
}

impl ClosureCompositionBudget {
    /// Creates one explicit member-, delta-, payload-, and metadata-bounded policy.
    #[must_use]
    pub const fn new(
        max_members: usize,
        max_changes: usize,
        max_verified_payload_bytes: u64,
        max_metadata_bytes: usize,
    ) -> Self {
        Self {
            max_members,
            max_changes,
            max_verified_payload_bytes,
            max_metadata_bytes,
        }
    }

    /// Returns a conservative metadata charge for `change_count` edits at the
    /// maximum admitted manifest size. Composition performs a tighter check
    /// after opening its base and uses that charge before allocating edit
    /// vectors or preparing a tree.
    ///
    /// This helper is suitable for a budget that must cover either a genesis
    /// build or a path-copy update without knowing the base root in advance.
    pub fn metadata_bytes_for(change_count: usize) -> Result<usize, StoreError> {
        let shape = manifest_metadata_shape(nodes::NODE_MAX_COUNT, 0)?;
        let budget = LazyTreeUpdateBudget::new(change_count, usize::MAX, shape);
        let update = budget
            .update_charge::<ManifestRelation>(change_count)
            .ok_or(StoreError::Bounds)?;
        let bulk = budget
            .bulk_charge::<ManifestRelation>(change_count)
            .ok_or(StoreError::Bounds)?;
        let edits = composer_edit_bytes(change_count)?;
        // `prepare_update_bounded` retains the admitted base root alongside
        // its charged update frontier. Reserve for the largest canonical
        // node plus the typed root wrapper so this public sizing helper also
        // works for a genesis composition and for a non-empty base whose root
        // is not known to the caller.
        let retained_root = size_of::<PersistedTreeRoot<ManifestRelation>>()
            .checked_add(
                DEFAULT_CUT_POLICY
                    .max_encoded_bytes()
                    .checked_mul(2)
                    .ok_or(StoreError::Bounds)?,
            )
            .ok_or(StoreError::Bounds)?;
        edits
            .checked_add(update.max(bulk))
            .and_then(|bytes| bytes.checked_add(retained_root))
            .ok_or(StoreError::Bounds)
    }

    fn validate(self, change_count: usize) -> Result<Self, StoreError> {
        if self.max_members > nodes::NODE_MAX_COUNT
            || self.max_changes > nodes::NODE_MAX_COUNT
            || change_count > self.max_changes
            || change_count > nodes::NODE_MAX_COUNT
            || self.max_metadata_bytes == 0
        {
            return Err(StoreError::Bounds);
        }
        Ok(self)
    }
}

/// One exact-base edit to closure membership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClosureMembershipChange {
    /// Add an object to the result. The CAS envelope is still fully verified.
    Add(ObjectId),
    /// Remove an object that must be present in the base closure.
    Remove(ObjectId),
}

impl ClosureMembershipChange {
    /// Adds one already typed object ID to the resulting closure.
    #[must_use]
    pub const fn add(id: ObjectId) -> Self {
        Self::Add(id)
    }

    /// Removes one already typed object ID from the base closure.
    #[must_use]
    pub const fn remove(id: ObjectId) -> Self {
        Self::Remove(id)
    }

    /// Returns the ID changed by this edit.
    #[must_use]
    pub const fn object_id(self) -> ObjectId {
        match self {
            Self::Add(id) | Self::Remove(id) => id,
        }
    }

    /// Returns whether this edit adds the member.
    #[must_use]
    pub const fn is_addition(self) -> bool {
        matches!(self, Self::Add(_))
    }
}

fn manifest_metadata_shape(
    target_members: usize,
    minimum_tree_levels: usize,
) -> Result<LazyTreeMetadataShape, StoreError> {
    let minimum = usize::from(DEFAULT_CUT_POLICY.min_entries);
    let maximum = usize::from(DEFAULT_CUT_POLICY.max_entries);
    let levels = manifest_tree_levels(target_members)?.max(minimum_tree_levels);
    let spill_nodes = maximum
        .checked_div(minimum)
        .and_then(|count| count.checked_add(2))
        .ok_or(StoreError::Bounds)?;
    Ok(LazyTreeMetadataShape::new(
        size_of::<[u8; 32]>(),
        0,
        minimum,
        maximum,
        spill_nodes,
        levels,
    ))
}

fn manifest_tree_levels(member_count: usize) -> Result<usize, StoreError> {
    let minimum = usize::from(DEFAULT_CUT_POLICY.min_entries);
    let mut nodes = member_count.max(1).div_ceil(minimum);
    let mut levels = 1usize;
    while nodes > 1 {
        nodes = nodes.div_ceil(minimum);
        levels = levels.checked_add(1).ok_or(StoreError::Bounds)?;
    }
    Ok(levels)
}

fn composer_edit_bytes(change_count: usize) -> Result<usize, StoreError> {
    let per_change = size_of::<ClosureMembershipChange>()
        .checked_add(size_of::<TreeChange<ManifestRelation>>())
        .and_then(|bytes| bytes.checked_add(size_of::<[u8; 32]>()))
        .ok_or(StoreError::Bounds)?;
    change_count
        .checked_mul(per_change)
        .ok_or(StoreError::Bounds)
}

/// A completed closure receipt that keeps the shared GC pin alive.
///
/// Hold this value until the closure is committed into a durable selected
/// frontier. Dropping it releases the pin and allows ordinary GC to decide
/// reachability from then-current roots.
pub struct PinnedStoredClosureReceipt {
    receipt: StoredClosureReceipt,
    _gc_pin: super::layout::GcPinLease,
}

struct CompositionStaging {
    store: FileStore,
    name: String,
    parent: artifact_fs::ArtifactDirectory,
    directory: artifact_fs::ArtifactDirectory,
    lease: Option<File>,
}

impl CompositionStaging {
    fn create(store: &FileStore) -> Result<Self, StoreError> {
        let parent = artifact_fs::prepare_staging_root(store)?;
        artifact_fs::reap_stale_sessions(store, &parent)?;
        let session = artifact_fs::create_session(&parent)?;
        Ok(Self {
            store: store.clone(),
            name: session.name,
            parent: session.parent,
            directory: session.directory,
            lease: Some(session.lease),
        })
    }
}

impl Drop for CompositionStaging {
    fn drop(&mut self) {
        let _ =
            artifact_fs::cleanup_session(&self.store, &self.parent, &self.name, &self.directory);
        self.lease.take();
    }
}

impl PinnedStoredClosureReceipt {
    pub(super) fn from_parts(
        receipt: StoredClosureReceipt,
        gc_pin: super::layout::GcPinLease,
    ) -> Self {
        Self {
            receipt,
            _gc_pin: gc_pin,
        }
    }

    /// Copies the storage receipt while retaining this value's GC pin.
    #[must_use]
    pub const fn receipt(&self) -> StoredClosureReceipt {
        self.receipt
    }
}

impl std::fmt::Debug for PinnedStoredClosureReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PinnedStoredClosureReceipt")
            .field("receipt", &self.receipt)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct VerificationFacts {
    object_count: u64,
    payload_bytes: u64,
}

impl FileStore {
    /// Composes a new exact closure index from a checked base and ID-only edits.
    ///
    /// Every member in the final index is streamed through the full envelope,
    /// typed-version, and physical-ID verifier. Relation members must resolve
    /// their checked node references and every relation child must remain in
    /// the resulting membership set. The object payloads are never
    /// materialized or rewritten. The returned non-clone receipt keeps a
    /// shared GC pin alive so the caller can retain the new closure through
    /// selection.
    ///
    /// The caller should keep the returned value alive through its durable
    /// compare-and-select step, then drop it. A crash before the descriptor is
    /// installed leaves only unreferenced content-addressed relation nodes;
    /// a crash after installation leaves a cold-reopenable closure index.
    pub fn compose_closure_index(
        &self,
        base: Option<ArtifactClosureClaim>,
        changes: &[ClosureMembershipChange],
        budget: ClosureCompositionBudget,
    ) -> Result<PinnedStoredClosureReceipt, StoreError> {
        let budget = budget.validate(changes.len())?;
        let gc_pin = self.acquire_gc_pin()?;

        let base_index = base
            .map(|claim| self.open_closure_claim(claim))
            .transpose()?;
        let base_count = base_index.as_ref().map_or(0, DurableManifest::object_count);
        if base_count > u64::try_from(budget.max_members).map_err(|_| StoreError::Bounds)? {
            return Err(StoreError::Bounds);
        }

        let base_root = if let Some(manifest) = &base_index {
            manifest.root_evidence()
        } else {
            let empty = canonical_empty::<ManifestRelation>();
            let claim = UntrustedId::<ManifestRelation>::from_wire(
                &empty.commitment().to_bytes(),
                IdContext::relation::<ManifestRelation>(),
            )
            .map_err(|_| StoreError::Corrupt)?;
            admit_canonical_root_claim(claim, empty.as_bytes()).map_err(|_| StoreError::Corrupt)?
        };
        let addition_count = changes.iter().filter(|change| change.is_addition()).count();
        let target_upper = usize::try_from(base_count)
            .map_err(|_| StoreError::Bounds)?
            .checked_add(addition_count)
            .ok_or(StoreError::Bounds)?;
        let shape = manifest_metadata_shape(
            target_upper,
            usize::from(base_root.node().level()).saturating_add(1),
        )?;
        let genesis = base_root.node().row_count() == 0
            && changes
                .iter()
                .copied()
                .all(ClosureMembershipChange::is_addition);
        let lazy_budget =
            LazyTreeUpdateBudget::new(budget.max_changes, budget.max_metadata_bytes, shape);
        let internal_charge = if genesis {
            lazy_budget.bulk_charge::<ManifestRelation>(changes.len())
        } else {
            lazy_budget.update_charge::<ManifestRelation>(changes.len())
        }
        .ok_or(StoreError::Bounds)?;
        let edit_charge = composer_edit_bytes(changes.len())?;
        if edit_charge
            .checked_add(internal_charge)
            .ok_or(StoreError::Bounds)?
            > budget.max_metadata_bytes
        {
            return Err(StoreError::Bounds);
        }
        let lazy_budget = LazyTreeUpdateBudget::new(
            budget.max_changes,
            budget
                .max_metadata_bytes
                .checked_sub(edit_charge)
                .ok_or(StoreError::Bounds)?,
            shape,
        );

        let mut ordered = changes.to_vec();
        ordered.sort_unstable_by_key(|change| change.object_id());
        if ordered
            .windows(2)
            .any(|pair| pair[0].object_id() >= pair[1].object_id())
        {
            return Err(StoreError::MalformedDelta);
        }

        let loader = self.owned_relation_node_loader();
        let tree = LazyTree::from_admitted(&loader, PersistedTreeRoot::from_checked(base_root));

        let mut additions = 0_u64;
        let mut removals = 0_u64;
        let mut tree_changes = Vec::new();
        tree_changes
            .try_reserve_exact(ordered.len())
            .map_err(|_| StoreError::Bounds)?;
        for change in &ordered {
            let id = change.object_id();
            let is_present = tree
                .lookup(id.as_bytes())
                .map_err(|_| StoreError::Corrupt)?
                .is_some();
            match (*change, is_present) {
                (ClosureMembershipChange::Add(_), true)
                | (ClosureMembershipChange::Remove(_), false) => {
                    return Err(StoreError::WrongBase);
                }
                (ClosureMembershipChange::Add(_), false) => {
                    additions = additions.checked_add(1).ok_or(StoreError::Bounds)?;
                }
                (ClosureMembershipChange::Remove(_), true) => {
                    removals = removals.checked_add(1).ok_or(StoreError::Bounds)?;
                }
            }
            tree_changes.push(TreeChange {
                key: *id.as_bytes(),
                after: change.is_addition().then_some(()),
            });
        }
        let expected_count = base_count
            .checked_add(additions)
            .and_then(|count| count.checked_sub(removals))
            .ok_or(StoreError::Bounds)?;
        if expected_count > u64::try_from(budget.max_members).map_err(|_| StoreError::Bounds)? {
            return Err(StoreError::Bounds);
        }

        let update = tree
            .prepare_update_bounded(&tree_changes, lazy_budget)
            .map_err(|error| match error {
                LazyTreeError::MetadataBudgetExceeded { .. } => StoreError::Bounds,
                _ => StoreError::Corrupt,
            })?;
        let charged_peak = edit_charge
            .checked_add(update.work().peak_metadata_bytes)
            .ok_or(StoreError::Bounds)?;
        if charged_peak > budget.max_metadata_bytes {
            return Err(StoreError::Bounds);
        }
        if update.target().node().row_count() != expected_count {
            return Err(StoreError::Corrupt);
        }
        let facts = verify_target_members(self, base_index.as_ref(), &ordered, budget)?;
        if facts.object_count != expected_count {
            return Err(StoreError::Corrupt);
        }

        let closure = ClosureId::from_bytes(*update.target().root().as_bytes());
        let _process_lock = self.acquire_process_lock()?;
        let relation_write = self.write_lazy_relation_update(&update)?;
        if take_test_fault(FAULT_AFTER_NODES) {
            return Err(StoreError::Io(
                "injected interruption after closure relation nodes".to_owned(),
            ));
        }
        let descriptor = nodes::encode_manifest_descriptor(
            closure,
            relation_write.root(),
            usize::try_from(expected_count).map_err(|_| StoreError::Bounds)?,
        )?;
        let descriptor_len = u64::try_from(descriptor.len()).map_err(|_| StoreError::Bounds)?;
        let relation_bytes =
            u64::try_from(relation_write.bytes_written).map_err(|_| StoreError::Bounds)?;
        let staging = CompositionStaging::create(self)?;
        let descriptor_created =
            artifact_fs::write_closure_descriptor(&staging.directory, self, closure, &descriptor)?;
        if take_test_fault(FAULT_AFTER_DESCRIPTOR) {
            return Err(StoreError::Io(
                "injected interruption after closure descriptor durability".to_owned(),
            ));
        }
        let cold = self.open_closure(closure)?;
        if cold.object_count() != expected_count {
            return Err(StoreError::Corrupt);
        }
        let bytes_written = relation_bytes
            .checked_add(if descriptor_created {
                descriptor_len
            } else {
                0
            })
            .ok_or(StoreError::Bounds)?;
        let receipt = StoredClosureReceipt::from_verified(
            closure,
            expected_count,
            0,
            bytes_written,
            facts.payload_bytes,
        );
        drop(staging);
        drop(_process_lock);
        Ok(PinnedStoredClosureReceipt::from_parts(receipt, gc_pin))
    }
}

#[cfg(test)]
static TEST_FAULTS: Mutex<Vec<(std::thread::ThreadId, u8)>> = Mutex::new(Vec::new());

#[cfg(test)]
pub(super) fn set_test_fault(point: u8) {
    if let Ok(mut faults) = TEST_FAULTS.lock() {
        let owner = std::thread::current().id();
        faults.retain(|(thread, _)| *thread != owner);
        faults.push((owner, point));
    }
}

#[cfg(test)]
fn take_test_fault(point: u8) -> bool {
    let Ok(mut faults) = TEST_FAULTS.lock() else {
        return false;
    };
    let owner = std::thread::current().id();
    let Some(index) = faults
        .iter()
        .position(|(thread, pending)| *thread == owner && *pending == point)
    else {
        return false;
    };
    faults.remove(index);
    true
}

#[cfg(not(test))]
fn take_test_fault(_point: u8) -> bool {
    false
}

fn verify_target_members(
    store: &FileStore,
    base: Option<&DurableManifest>,
    changes: &[ClosureMembershipChange],
    budget: ClosureCompositionBudget,
) -> Result<VerificationFacts, StoreError> {
    let mut facts = VerificationFacts::default();
    // A persisted base closure was admitted when it was written, and the caller
    // holds a GC pin for the entire composition. Adding members cannot break
    // any relation edge already closed within that immutable base. Reopen and
    // verify only the new envelopes; the checked base root supplies the exact
    // unchanged member count. Removals still require the full walk below,
    // because a retained relation may refer to a removed child.
    if changes.iter().all(|change| change.is_addition()) {
        facts.object_count = base.map_or(0, DurableManifest::object_count);
        for change in changes {
            if let ClosureMembershipChange::Add(id) = change {
                verify_target_member(store, base, changes, *id, budget, &mut facts)?;
            }
        }
        return Ok(facts);
    }
    if let Some(base) = base {
        let mut after = None;
        let mut previous = None;
        let mut base_seen = 0_u64;
        loop {
            let page = base.page_ids(after, MEMBER_PAGE_SIZE)?;
            if page.object_ids().is_empty() {
                break;
            }
            for id in page.object_ids() {
                if previous.is_some_and(|prior| prior >= *id) {
                    return Err(StoreError::Corrupt);
                }
                previous = Some(*id);
                base_seen = base_seen.checked_add(1).ok_or(StoreError::Bounds)?;
                match changes.binary_search_by_key(id, |change| change.object_id()) {
                    Ok(index) => match changes[index] {
                        ClosureMembershipChange::Remove(_) => continue,
                        ClosureMembershipChange::Add(_) => return Err(StoreError::Corrupt),
                    },
                    Err(_) => {}
                }
                verify_target_member(store, Some(base), changes, *id, budget, &mut facts)?;
            }
            match page.next() {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        if base_seen != base.object_count() {
            return Err(StoreError::Corrupt);
        }
    }
    for change in changes {
        if let ClosureMembershipChange::Add(id) = change {
            verify_target_member(store, base, changes, *id, budget, &mut facts)?;
        }
    }
    Ok(facts)
}

fn verify_target_member(
    store: &FileStore,
    base: Option<&DurableManifest>,
    changes: &[ClosureMembershipChange],
    id: ObjectId,
    budget: ClosureCompositionBudget,
    facts: &mut VerificationFacts,
) -> Result<(), StoreError> {
    let remaining_bytes = budget
        .max_verified_payload_bytes
        .checked_sub(facts.payload_bytes)
        .ok_or(StoreError::Bounds)?;
    let (envelope, relation_references) =
        store.verify_closure_member_limited(id, Some(remaining_bytes))?;
    if envelope.id() != id {
        return Err(StoreError::Corrupt);
    }
    facts.object_count = facts
        .object_count
        .checked_add(1)
        .ok_or(StoreError::Bounds)?;
    facts.payload_bytes = facts
        .payload_bytes
        .checked_add(envelope.payload_len())
        .ok_or(StoreError::Bounds)?;
    if facts.object_count > u64::try_from(budget.max_members).map_err(|_| StoreError::Bounds)?
        || facts.payload_bytes > budget.max_verified_payload_bytes
    {
        return Err(StoreError::Bounds);
    }

    let Some(references) = relation_references else {
        return Ok(());
    };
    let schema = envelope.schema();
    if store.read_relation_ref(schema, envelope.version())? != id {
        return Err(StoreError::Corrupt);
    }
    for child_version in references.children {
        let child = store.read_relation_ref(schema, &child_version)?;
        if !target_contains(base, changes, child)? {
            return Err(StoreError::Corrupt);
        }
    }
    for target in references.values {
        let target = ObjectId::from_bytes(target);
        if !target_contains(base, changes, target)? {
            store.verify_object_claim(crate::UntrustedObjectId::from_bytes(*target.as_bytes()))?;
        }
    }
    Ok(())
}

fn target_contains(
    base: Option<&DurableManifest>,
    changes: &[ClosureMembershipChange],
    id: ObjectId,
) -> Result<bool, StoreError> {
    match changes.binary_search_by_key(&id, |change| change.object_id()) {
        Ok(index) => Ok(changes[index].is_addition()),
        Err(_) => match base {
            Some(base) => base.contains_object_id(id),
            None => Ok(false),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ArtifactBudget, ClosureManifest, GcLimits, GcRoot, GcRoots, TypedObject};
    use backend_version::{
        LazyTree, LazyTreeUpdateBudget, ObjectKey, PersistedTreeRoot, Schema, TreeChange,
    };
    use std::{
        fs,
        path::PathBuf,
        sync::mpsc,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };

    static NEXT_STORE: AtomicUsize = AtomicUsize::new(0);

    struct BytesSchema;

    impl Schema for BytesSchema {
        const DOMAIN: u8 = 0xf5;
        const TYPE: u16 = 25;
        const VERSION: u8 = 1;
        type Value = [u8];

        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }

    struct TestStore {
        path: PathBuf,
        store: FileStore,
    }

    impl TestStore {
        fn new() -> Self {
            let ordinal = NEXT_STORE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "backend-store-closure-composer-{}-{ordinal}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            let store = FileStore::open(&path, 1024 * 1024).expect("open composer test store");
            Self { path, store }
        }
    }

    impl Drop for TestStore {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn object(payload: &[u8]) -> TypedObject {
        let key = ObjectKey::<BytesSchema>::from_value(payload);
        TypedObject::from_value(&key, payload)
    }

    fn manifest(mut objects: Vec<TypedObject>) -> ClosureManifest {
        objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
        ClosureManifest::new(objects).expect("build expected closure")
    }

    fn budget() -> ClosureCompositionBudget {
        ClosureCompositionBudget::new(
            1024,
            128,
            64 * 1024 * 1024,
            ClosureCompositionBudget::metadata_bytes_for(128).expect("metadata charge"),
        )
    }

    fn compose(
        store: &FileStore,
        base: Option<ClosureId>,
        changes: &[ClosureMembershipChange],
    ) -> PinnedStoredClosureReceipt {
        store
            .compose_closure_index(base.map(ArtifactClosureClaim::from_id), changes, budget())
            .expect("compose checked closure")
    }

    #[test]
    fn single_member_genesis_fits_the_public_metadata_budget_helper() {
        let test = TestStore::new();
        let item = object(b"single member genesis");
        test.store.write_object(&item).expect("write member");
        let metadata =
            ClosureCompositionBudget::metadata_bytes_for(1).expect("single-member metadata charge");
        let composed = test
            .store
            .compose_closure_index(
                None,
                &[ClosureMembershipChange::add(item.id())],
                ClosureCompositionBudget::new(1, 1, 1024, metadata),
            )
            .expect("compose one member within advertised metadata budget");
        assert_eq!(composed.receipt().object_count(), 1);
        assert!(
            test.store
                .read_closure_index(composed.receipt().closure())
                .is_ok()
        );
    }

    #[test]
    fn id_only_composition_matches_canonical_closure_and_cold_reopens() {
        let test = TestStore::new();
        let first = object(b"first semantic image");
        let second = object(b"metadata");
        let first_id = test.store.write_object(&first).expect("write first");
        let second_id = test.store.write_object(&second).expect("write second");
        let changes = [
            ClosureMembershipChange::add(second_id),
            ClosureMembershipChange::add(first_id),
        ];
        let composed = compose(&test.store, None, &changes);
        let canonical = manifest(vec![first, second]);
        assert_eq!(composed.receipt().closure(), canonical.id());
        assert_eq!(composed.receipt().object_count(), 2);
        drop(composed);

        let cold = FileStore::open(&test.path, 1024 * 1024).expect("cold reopen store");
        let index = cold
            .read_closure_index(canonical.id())
            .expect("cold reopen composed index");
        assert!(
            index
                .contains_object_id(first_id)
                .expect("first membership")
        );
        assert!(
            index
                .contains_object_id(second_id)
                .expect("second membership")
        );
    }

    #[test]
    fn base_plus_add_remove_composes_exact_sorted_target() {
        let test = TestStore::new();
        let first = object(b"remove me");
        let retained = object(b"keep me");
        let added = object(b"new image");
        let first_id = test.store.write_object(&first).expect("write first");
        let retained_id = test.store.write_object(&retained).expect("write retained");
        let added_id = test.store.write_object(&added).expect("write added");
        let base = manifest(vec![first.clone(), retained.clone()]);
        test.store.write_closure(&base).expect("write base index");

        let composed = compose(
            &test.store,
            Some(base.id()),
            &[
                ClosureMembershipChange::add(added_id),
                ClosureMembershipChange::remove(first_id),
            ],
        );
        let target = manifest(vec![added, retained]);
        assert_eq!(composed.receipt().closure(), target.id());
        drop(composed);
        assert!(
            test.store
                .read_closure_index(target.id())
                .expect("reopen target index")
                .contains_object_id(retained_id)
                .expect("retained member")
        );
    }

    #[test]
    fn asymmetric_structured_delta_matches_independent_set_oracle() {
        let test = TestStore::new();
        let mut objects = Vec::new();
        let mut ids = Vec::new();
        for index in 0_u32..260 {
            let length =
                usize::try_from((index.wrapping_mul(173) % 4093) + 4).expect("payload length");
            let mut payload = vec![index as u8; length];
            payload[..4].copy_from_slice(&index.to_be_bytes());
            let item = object(&payload);
            ids.push(test.store.write_object(&item).expect("write member"));
            objects.push(item);
        }

        let base_objects = objects[..173].to_vec();
        let base = manifest(base_objects.clone());
        test.store.write_closure(&base).expect("admit base");
        let removed = [1_usize, 17, 55, 101, 169];
        let added = [181_usize, 187, 193, 199, 211, 223, 239, 257];
        let mut edits = removed
            .iter()
            .map(|index| ClosureMembershipChange::remove(ids[*index]))
            .chain(
                added
                    .iter()
                    .map(|index| ClosureMembershipChange::add(ids[*index])),
            )
            .collect::<Vec<_>>();
        edits.sort_unstable_by_key(|change| change.object_id());

        let mut oracle = base_objects
            .iter()
            .map(TypedObject::id)
            .collect::<std::collections::BTreeSet<_>>();
        for index in removed {
            assert!(oracle.remove(&ids[index]));
        }
        for index in added {
            assert!(oracle.insert(ids[index]));
        }
        let mut target_objects = base_objects;
        target_objects.retain(|item| !removed.iter().any(|index| item.id() == ids[*index]));
        target_objects.extend(added.iter().map(|index| objects[*index].clone()));
        let target = manifest(target_objects);

        let composed = compose(&test.store, Some(base.id()), &edits);
        assert_eq!(composed.receipt().closure(), target.id());
        drop(composed);
        let cold = FileStore::open(&test.path, 1024 * 1024).expect("cold reopen result");
        let index = cold
            .read_closure_index(target.id())
            .expect("reopen asymmetric target");
        let mut observed = Vec::new();
        let mut after = None;
        loop {
            let page = index.page_ids(after, 37).expect("read target page");
            observed.extend_from_slice(page.object_ids());
            match page.next() {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        assert_eq!(observed, oracle.into_iter().collect::<Vec<_>>());
    }

    #[test]
    fn low_metadata_budget_rejects_before_closure_publication() {
        let test = TestStore::new();
        let item = object(b"must not publish under a tiny budget");
        let id = test.store.write_object(&item).expect("write member");
        let target = manifest(vec![item]);
        let budget = ClosureCompositionBudget::new(8, 1, 1024 * 1024, 1);
        assert!(matches!(
            test.store
                .compose_closure_index(None, &[ClosureMembershipChange::add(id)], budget,),
            Err(StoreError::Bounds)
        ));
        assert!(test.store.read_closure_index(target.id()).is_err());
    }

    #[test]
    fn bounded_path_copy_reports_peak_and_keeps_authenticated_work_bounded() {
        let test = TestStore::new();
        let mut base_objects = Vec::new();
        for index in 0_u32..640 {
            let payload = index.to_be_bytes();
            let item = object(&payload);
            test.store.write_object(&item).expect("write base row");
            base_objects.push(item);
        }
        let base = manifest(base_objects);
        test.store.write_closure(&base).expect("admit base");
        let reopened = test.store.read_closure_index(base.id()).expect("open base");
        let mut changes = Vec::new();
        for index in 700_u32..716 {
            let item = object(&index.to_be_bytes());
            let id = test.store.write_object(&item).expect("write addition");
            changes.push(TreeChange {
                key: *id.as_bytes(),
                after: Some(()),
            });
        }
        changes.sort_unstable_by_key(|change| change.key);
        let root = reopened.root_evidence();
        let shape = manifest_metadata_shape(
            usize::try_from(root.row_count()).expect("base count") + changes.len(),
            usize::from(root.node().level()).saturating_add(1),
        )
        .expect("manifest metadata shape");
        let maximum = 64 * 1024 * 1024;
        let loader = test.store.owned_relation_node_loader();
        let tree = LazyTree::from_admitted(&loader, PersistedTreeRoot::from_checked(root));
        let prepared = tree
            .prepare_update_bounded(
                &changes,
                LazyTreeUpdateBudget::new(changes.len(), maximum, shape),
            )
            .expect("bounded path copy");
        let work = prepared.work();
        assert!(work.peak_metadata_bytes > 0);
        assert!(work.peak_metadata_bytes <= maximum);
        assert_eq!(work.rebuilt_nodes, prepared.changed_nodes().len());
        assert!(work.loaded_nodes < 640);
        assert!(work.rebuilt_nodes < changes.len() * shape.max_tree_levels * 8);
    }

    #[test]
    fn add_only_delta_reuses_admitted_base_without_reverifying_its_payloads() {
        let test = TestStore::new();
        let mut base_objects = Vec::new();
        for index in 0_u32..256 {
            let mut payload = vec![0x41; 8 * 1024];
            payload[..4].copy_from_slice(&index.to_be_bytes());
            let item = object(&payload);
            test.store.write_object(&item).expect("write base member");
            base_objects.push(item);
        }
        let base = manifest(base_objects.clone());
        test.store.write_closure(&base).expect("admit base closure");

        let addition = object(b"one changed semantic segment");
        let addition_id = test.store.write_object(&addition).expect("write addition");
        let expected_changed_bytes = addition.bytes().len() as u64;
        let composed = compose(
            &test.store,
            Some(base.id()),
            &[ClosureMembershipChange::add(addition_id)],
        );
        base_objects.push(addition);
        let mut expected_ids = base_objects.iter().map(TypedObject::id).collect::<Vec<_>>();
        expected_ids.sort_unstable();
        let independent_target = manifest(base_objects);
        assert_eq!(composed.receipt().closure(), independent_target.id());
        assert_eq!(composed.receipt().object_count(), 257);
        assert_eq!(composed.receipt().bytes_verified(), expected_changed_bytes);
        drop(composed);

        let cold = FileStore::open(&test.path, 1024 * 1024).expect("cold reopen delta");
        let selected = cold
            .read_closure_index(independent_target.id())
            .expect("cold reopen exact target");
        assert_eq!(selected.object_count(), 257);
        let mut observed_ids = Vec::new();
        let mut after = None;
        loop {
            let page = selected.page_ids(after, 128).expect("read selected page");
            observed_ids.extend_from_slice(page.object_ids());
            match page.next() {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        assert_eq!(observed_ids, expected_ids);
    }

    #[test]
    fn composer_gc_pin_survives_until_selection_owner_drops_receipt() {
        let test = TestStore::new();
        let item = object(b"pinned until selected");
        test.store.write_object(&item).expect("write member");
        let held = compose(
            &test.store,
            None,
            &[ClosureMembershipChange::add(item.id())],
        );
        let store = test.store.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _ = started_tx.send(());
            let result = store.collect_garbage(&GcRoots::new(), GcLimits::default());
            let _ = done_tx.send(result);
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("GC thread started");
        assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());
        assert!(
            test.store
                .contains_object(item.id())
                .expect("member remains while pinned")
        );
        drop(held);
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("GC continues after receipt drop")
            .expect("GC completes");
        worker.join().expect("join GC worker");
    }

    #[test]
    fn cold_reopened_closure_pin_holds_members_through_recovered_admission() {
        let test = TestStore::new();
        let item = object(b"recover exact compiler input");
        let id = test.store.write_object(&item).expect("write input object");
        let closure = manifest(vec![item]);
        test.store
            .write_closure(&closure)
            .expect("admit input closure");

        let reopened = FileStore::open(&test.path, 1024 * 1024).expect("cold reopen store");
        let held = reopened
            .reopen_pinned_stored_closure(
                ArtifactClosureClaim::from_id(closure.id()),
                ArtifactBudget::new(1, 1, 1024 * 1024, 16 * 1024, 16),
            )
            .expect("reopen and pin exact closure");
        assert_eq!(held.receipt().closure(), closure.id());
        assert_eq!(held.receipt().object_count(), 1);

        let collector = reopened.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _ = done_tx.send(collector.collect_garbage(&GcRoots::new(), GcLimits::default()));
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());
        assert!(reopened.contains_object(id).expect("pinned input survives"));
        drop(held);
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("GC resumes after pin release")
            .expect("GC completes");
        worker.join().expect("join collector");
    }

    #[test]
    fn collector_resolves_selected_roots_after_waiting_for_publication_pin() {
        let test = TestStore::new();
        let item = object(b"new selected closure");
        test.store.write_object(&item).expect("write member");
        let held = compose(
            &test.store,
            None,
            &[ClosureMembershipChange::add(item.id())],
        );
        let closure = held.receipt().closure();
        let selected = Arc::new(Mutex::new(None::<ClosureId>));
        let selected_for_gc = Arc::clone(&selected);
        let store = test.store.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _ = started_tx.send(());
            let result = store.collect_garbage_resolving_roots(
                |roots| {
                    if let Some(root) = *selected_for_gc.lock().expect("selected root lock") {
                        roots.add(GcRoot::Closure(root));
                    }
                    Ok(())
                },
                GcLimits::default(),
            );
            let _ = done_tx.send(result);
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("GC started");
        assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());
        *selected.lock().expect("selected root lock") = Some(closure);
        drop(held);
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("GC continues")
            .expect("GC completes");
        worker.join().expect("join GC worker");
        assert!(
            test.store
                .contains_object(item.id())
                .expect("selected object survives")
        );
        assert!(test.store.read_closure_index(closure).is_ok());
    }

    #[test]
    fn descriptor_fault_has_cold_reopenable_orphan_boundary() {
        let test = TestStore::new();
        let item = object(b"fault target");
        let id = test.store.write_object(&item).expect("write fault member");
        let target = ClosureManifest::new(vec![item]).expect("target closure");
        let edit = [ClosureMembershipChange::add(id)];

        set_test_fault(FAULT_AFTER_NODES);
        assert!(
            test.store
                .compose_closure_index(None, &edit, budget())
                .is_err()
        );
        assert!(test.store.read_closure_index(target.id()).is_err());

        set_test_fault(FAULT_AFTER_DESCRIPTOR);
        assert!(
            test.store
                .compose_closure_index(None, &edit, budget())
                .is_err()
        );
        let cold = FileStore::open(&test.path, 1024 * 1024).expect("cold open after fault");
        let index = cold
            .read_closure_index(target.id())
            .expect("descriptor persisted before receipt fault");
        assert!(index.contains_object_id(id).expect("fault target member"));
        let recovered = compose(&cold, None, &edit);
        assert_eq!(recovered.receipt().closure(), target.id());
        drop(recovered);
    }
}
