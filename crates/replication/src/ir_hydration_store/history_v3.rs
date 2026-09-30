//! Exact local FileStore admission for complete producer-side typed V3 output.
//!
//! This establishes only the immutable payload closure and typed content
//! proof. It intentionally stops before history commit/ref publication while
//! V3 input declarations and read-closure authority are not yet replayable.

use super::{FileSemanticRangeStore, history_v2};
use crate::ir_generation_store::{HistoryTypedV3RootClaim, TypedV3HistoryLocator};
use crate::{DurableSemanticObjectAdmission, ProducedSemanticTypedPlaneV3};
use backend_semantic::ir::{
    JumboRopeLimits, SemanticInputClaimV2, SemanticTypedPlaneVerificationTierV2,
    VerifiedTypedPlaneContentV2,
};
use backend_store::{
    ClosureCompositionBudget, ClosureMembershipChange, GcPinGuard, ObjectId,
    PinnedStoredClosureReceipt,
};
use backend_version::SchemaIdentity;
use std::collections::BTreeMap;
use std::path::PathBuf;

const MAX_TYPED_V3_ADMISSION_OBJECTS: usize = 200_000;

/// Exact physical-object and payload accounting for one local V3 admission.
/// Reuse counts are local FileStore CAS hits; they do not measure transport
/// skips or worker resend behavior.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct TypedV3HistoryAdmissionMetrics {
    pub(crate) closure_objects: u64,
    pub(crate) closure_payload_bytes: u64,
    pub(crate) admitted_envelope_bytes: u64,
    pub(crate) newly_created_objects: u64,
    pub(crate) newly_created_envelope_bytes: u64,
    pub(crate) reused_objects: u64,
    pub(crate) reused_envelope_bytes: u64,
    pub(crate) reopened_objects: u64,
    pub(crate) reopened_payload_bytes: u64,
}

/// Nonconstructible guard proving this caller holds a GC pin for one exact
/// FileStore root. Admissions borrow it so the proof cannot outlive the pin.
#[must_use = "keep the V3 admission pin alive through the next durable step"]
pub(crate) struct TypedV3HistoryGcPin {
    store_root: PathBuf,
    _guard: GcPinGuard,
}

/// Verified receipt-derived V3 payload closure. This is not a history commit,
/// selected generation, or publication capability. Its borrow ties the
/// receipt set to the caller-held FileStore GC pin for the lifetime of proof.
#[must_use = "retain the proof through the next durable history step"]
pub(crate) struct TypedV3HistoryAdmission<'pin> {
    _pin: &'pin TypedV3HistoryGcPin,
    _producer: &'pin ProducedSemanticTypedPlaneV3,
    _closure_receipt: PinnedStoredClosureReceipt,
    content: VerifiedTypedPlaneContentV2,
    closure: backend_store::ClosureId,
    locator: TypedV3HistoryLocator,
    root_claim: HistoryTypedV3RootClaim,
    metrics: TypedV3HistoryAdmissionMetrics,
}

impl TypedV3HistoryAdmission<'_> {
    pub(crate) const fn content(&self) -> &VerifiedTypedPlaneContentV2 {
        &self.content
    }

    pub(crate) const fn closure(&self) -> backend_store::ClosureId {
        self.closure
    }

    pub(crate) const fn root_claim(&self) -> HistoryTypedV3RootClaim {
        self.root_claim
    }

    pub(crate) const fn locator(&self) -> &TypedV3HistoryLocator {
        &self.locator
    }

    pub(crate) const fn metrics(&self) -> TypedV3HistoryAdmissionMetrics {
        self.metrics
    }
}

#[derive(Clone, Copy)]
struct InventoryEntry {
    payload_bytes: u64,
    envelope_bytes: u64,
    schema: SchemaIdentity,
    created: bool,
}

impl FileSemanticRangeStore {
    /// Acquires the explicit GC pin required by `verify_produced_typed_v3`.
    /// The returned nonconstructible value is bound to this store root.
    pub(crate) fn pin_typed_v3_history_admission(&self) -> Result<TypedV3HistoryGcPin, String> {
        let guard = self
            .store
            .pin_garbage_collection()
            .map_err(|error| format!("pin typed V3 history payload admission: {error:?}"))?;
        Ok(TypedV3HistoryGcPin {
            store_root: self.store.root().to_path_buf(),
            _guard: guard,
        })
    }

    /// Composes the exact unique FileStore object set named by a complete V3
    /// producer receipt, reopens every member through the existing bounded
    /// history spool, then independently verifies the whole c007 manifest.
    /// No commit or ref is written, and the result cannot authorize selection.
    pub(crate) fn verify_produced_typed_v3<'pin>(
        &self,
        pin: &'pin TypedV3HistoryGcPin,
        produced: &'pin ProducedSemanticTypedPlaneV3,
        tier: SemanticTypedPlaneVerificationTierV2,
        jumbo_limits: JumboRopeLimits,
    ) -> Result<TypedV3HistoryAdmission<'pin>, String> {
        require_pin_for_store(pin, self.store.root())?;
        if !produced.input_witness().coverage().is_authorized_complete()
            || produced.verified_content().input_claim()
                != SemanticInputClaimV2::from_witness(&produced.input_witness())
        {
            return Err("typed V3 producer receipt lacks complete input coverage".to_owned());
        }

        let locator = TypedV3HistoryLocator::from_produced(produced)?;
        let manifest = locator.validate()?;
        if manifest != *produced.manifest()
            || !manifest
                .content_root_claim()
                .matches(produced.verified_content().content_root())
            || !manifest
                .generation_root_claim()
                .matches(produced.verified_content().generation_root())
        {
            return Err("typed V3 manifest differs from its producer proof".to_owned());
        }

        let (inventory, metrics) = receipt_inventory(produced)?;
        let mut changes = Vec::new();
        changes
            .try_reserve_exact(inventory.len())
            .map_err(|_| "typed V3 closure edit allocation failed".to_owned())?;
        let mut closure_payload_bytes = 0_u64;
        for (object, entry) in &inventory {
            changes.push(ClosureMembershipChange::add(*object));
            closure_payload_bytes = closure_payload_bytes
                .checked_add(entry.payload_bytes)
                .ok_or_else(|| "typed V3 closure payload total overflows".to_owned())?;
        }
        let metadata_bytes = ClosureCompositionBudget::metadata_bytes_for(changes.len())
            .map_err(|error| format!("size typed V3 closure metadata: {error:?}"))?;
        let budget = ClosureCompositionBudget::new(
            changes.len().max(1),
            changes.len(),
            closure_payload_bytes.max(1),
            metadata_bytes,
        );
        let closure_receipt = self
            .store
            .compose_closure_index(None, &changes, budget)
            .map_err(|error| format!("compose exact typed V3 payload closure: {error:?}"))?;
        if closure_receipt.receipt().object_count()
            != u64::try_from(inventory.len())
                .map_err(|error| format!("typed V3 closure object count: {error}"))?
            || closure_receipt.receipt().bytes_verified() != closure_payload_bytes
        {
            return Err("typed V3 closure receipt differs from producer inventory".to_owned());
        }
        let closure = closure_receipt.receipt().closure();
        let closure_claim = backend_store::ArtifactClosureClaim::from_id(closure);
        let (content, durable_closure, reopened_objects, reopened_payload_bytes) =
            history_v2::verify_typed_v2_history_payload_closure(
                &self.store,
                closure_claim,
                locator.bridge(),
                &manifest,
                tier,
                jumbo_limits,
            )?;
        if durable_closure != closure
            || content != *produced.verified_content()
            || reopened_objects != inventory.len()
            || reopened_payload_bytes != closure_payload_bytes
        {
            return Err("typed V3 cold closure proof differs from producer receipt".to_owned());
        }
        let locator_id = locator.identity()?;
        let root_claim =
            HistoryTypedV3RootClaim::from_verified(&content, closure_claim, locator_id);
        let metrics = TypedV3HistoryAdmissionMetrics {
            closure_objects: u64::try_from(inventory.len())
                .map_err(|error| format!("typed V3 closure object count: {error}"))?,
            closure_payload_bytes,
            admitted_envelope_bytes: metrics.envelope_bytes,
            newly_created_objects: metrics.created_objects,
            newly_created_envelope_bytes: metrics.created_envelope_bytes,
            reused_objects: metrics.reused_objects,
            reused_envelope_bytes: metrics.reused_envelope_bytes,
            reopened_objects: u64::try_from(reopened_objects)
                .map_err(|error| format!("typed V3 reopened object count: {error}"))?,
            reopened_payload_bytes,
        };
        Ok(TypedV3HistoryAdmission {
            _pin: pin,
            _producer: produced,
            _closure_receipt: closure_receipt,
            content,
            closure,
            locator,
            root_claim,
            metrics,
        })
    }
}

struct ReceiptInventoryMetrics {
    envelope_bytes: u64,
    created_objects: u64,
    created_envelope_bytes: u64,
    reused_objects: u64,
    reused_envelope_bytes: u64,
}

fn require_pin_for_store(
    pin: &TypedV3HistoryGcPin,
    store_root: &std::path::Path,
) -> Result<(), String> {
    if pin.store_root != store_root {
        return Err("typed V3 admission pin belongs to another FileStore".to_owned());
    }
    Ok(())
}

fn receipt_inventory(
    produced: &ProducedSemanticTypedPlaneV3,
) -> Result<(BTreeMap<ObjectId, InventoryEntry>, ReceiptInventoryMetrics), String> {
    let receipt_count = produced
        .segment_admissions()
        .len()
        .checked_add(produced.jumbo_admissions().len())
        .ok_or_else(|| "typed V3 receipt count overflows".to_owned())?;
    if receipt_count > MAX_TYPED_V3_ADMISSION_OBJECTS {
        return Err("typed V3 receipt count exceeds its admission bound".to_owned());
    }
    let mut inventory = BTreeMap::new();
    for receipt in produced
        .segment_admissions()
        .iter()
        .chain(produced.jumbo_admissions())
    {
        insert_receipt(&mut inventory, *receipt)?;
        if inventory.len() > MAX_TYPED_V3_ADMISSION_OBJECTS {
            return Err("typed V3 unique object count exceeds its admission bound".to_owned());
        }
    }
    let metrics = summarize_inventory(&inventory)?;
    Ok((inventory, metrics))
}

fn summarize_inventory(
    inventory: &BTreeMap<ObjectId, InventoryEntry>,
) -> Result<ReceiptInventoryMetrics, String> {
    let mut envelope_bytes = 0_u64;
    let mut created_objects = 0_u64;
    let mut created_envelope_bytes = 0_u64;
    let mut reused_objects = 0_u64;
    let mut reused_envelope_bytes = 0_u64;
    for entry in inventory.values() {
        envelope_bytes = envelope_bytes
            .checked_add(entry.envelope_bytes)
            .ok_or_else(|| "typed V3 envelope-byte total overflows".to_owned())?;
        if entry.created {
            created_objects = created_objects
                .checked_add(1)
                .ok_or_else(|| "typed V3 created-object count overflows".to_owned())?;
            created_envelope_bytes = created_envelope_bytes
                .checked_add(entry.envelope_bytes)
                .ok_or_else(|| "typed V3 created-envelope total overflows".to_owned())?;
        } else {
            reused_objects = reused_objects
                .checked_add(1)
                .ok_or_else(|| "typed V3 reused-object count overflows".to_owned())?;
            reused_envelope_bytes = reused_envelope_bytes
                .checked_add(entry.envelope_bytes)
                .ok_or_else(|| "typed V3 reused-envelope total overflows".to_owned())?;
        }
    }
    Ok(ReceiptInventoryMetrics {
        envelope_bytes,
        created_objects,
        created_envelope_bytes,
        reused_objects,
        reused_envelope_bytes,
    })
}

fn insert_receipt(
    inventory: &mut BTreeMap<ObjectId, InventoryEntry>,
    receipt: DurableSemanticObjectAdmission,
) -> Result<(), String> {
    merge_inventory_entry(
        inventory,
        receipt.object_id(),
        InventoryEntry {
            payload_bytes: receipt.payload_bytes(),
            envelope_bytes: receipt.envelope_bytes(),
            schema: receipt.identity().kind().schema_identity(),
            created: receipt.created(),
        },
    )
}

fn merge_inventory_entry(
    inventory: &mut BTreeMap<ObjectId, InventoryEntry>,
    id: ObjectId,
    entry: InventoryEntry,
) -> Result<(), String> {
    if let Some(previous) = inventory.get_mut(&id) {
        if previous.payload_bytes != entry.payload_bytes
            || previous.envelope_bytes != entry.envelope_bytes
            || previous.schema != entry.schema
        {
            return Err("typed V3 receipts alias one object with conflicting metadata".to_owned());
        }
        previous.created |= entry.created;
    } else {
        inventory.insert(id, entry);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir_generation_store::TypedV3HistoryLocator;
    use crate::ir_hydration_store::history_v2::verify_typed_v2_history_payload_closure;
    use backend_semantic::ir::JumboRopeLimits;
    use backend_store::{FileStore, TypedObject};
    use backend_version::ObjectKey;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn create() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "backend-typed-v3-history-admission-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create V3 admission fixture directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn compose_fixture_closure(
        store: &FileStore,
        objects: &[TypedObject],
        omit: Option<usize>,
        add_extra: bool,
    ) -> PinnedStoredClosureReceipt {
        let mut members = BTreeMap::<ObjectId, u64>::new();
        for (index, object) in objects.iter().enumerate() {
            let id = store
                .write_object(object)
                .expect("durably write fixture object");
            if Some(index) != omit {
                members.insert(
                    id,
                    u64::try_from(object.bytes().len()).expect("fixture envelope length fits"),
                );
            }
        }
        if add_extra {
            let payload = b"unclaimed V3 closure member";
            let extra = TypedObject::from_value(
                &ObjectKey::<super::super::SemanticSegmentPayload>::from_value(payload),
                payload,
            );
            let id = store.write_object(&extra).expect("write unclaimed object");
            members.insert(
                id,
                u64::try_from(extra.bytes().len()).expect("extra envelope length fits"),
            );
        }
        let changes = members
            .keys()
            .copied()
            .map(ClosureMembershipChange::add)
            .collect::<Vec<_>>();
        let bytes = members.values().copied().sum::<u64>();
        let metadata_bytes = ClosureCompositionBudget::metadata_bytes_for(changes.len())
            .expect("fixture closure metadata budget");
        store
            .compose_closure_index(
                None,
                &changes,
                ClosureCompositionBudget::new(
                    changes.len().max(1),
                    changes.len(),
                    bytes.max(1),
                    metadata_bytes,
                ),
            )
            .expect("compose real FileStore fixture closure")
    }

    #[test]
    fn exact_v3_bridge_closure_cold_reopens_and_rejects_missing_or_extra_members() {
        let fixture = crate::ir_hydration_store::positive_v2_history_fixture_for_test();
        let locator = TypedV3HistoryLocator::from_v2_bridge(fixture.locator.clone())
            .expect("positive c007 manifest has a portable V3 locator");
        let directory = TestDirectory::create();
        let store = FileStore::open(directory.0.join("cas"), 64 * 1024 * 1024)
            .expect("open real V3 admission FileStore");
        let exact = compose_fixture_closure(&store, &fixture.objects, None, false);
        let exact_claim = backend_store::ArtifactClosureClaim::from_id(exact.receipt().closure());
        let (verified, closure_id, objects, bytes) = verify_typed_v2_history_payload_closure(
            &store,
            exact_claim,
            locator.bridge(),
            &fixture.manifest,
            SemanticTypedPlaneVerificationTierV2::Standard,
            JumboRopeLimits::default(),
        )
        .expect("exact receipt-shaped FileStore closure cold-verifies");
        assert_eq!(closure_id, exact.receipt().closure());
        assert_eq!(objects as u64, exact.receipt().object_count());
        assert!(bytes > 0);
        assert_eq!(
            verified.content_root().as_bytes(),
            &fixture.expected_content_root
        );
        assert_eq!(
            verified.generation_root().as_bytes(),
            &fixture.expected_generation_root
        );
        let locator_id = locator.identity().expect("derive V3 locator ID");
        let root_claim = HistoryTypedV3RootClaim::from_verified(&verified, exact_claim, locator_id);
        let portable_claim = root_claim
            .encode_portable()
            .expect("encode exact closure root claim");
        assert_eq!(
            HistoryTypedV3RootClaim::decode_portable(&portable_claim)
                .expect("decode portable root claim"),
            root_claim
        );
        drop(exact);
        drop(store);

        let cold_store = FileStore::open(directory.0.join("cas"), 64 * 1024 * 1024)
            .expect("cold reopen real V3 admission FileStore");
        let cold = cold_store
            .open_closure_claim(exact_claim)
            .expect("cold reopen exact closure index");
        assert_eq!(cold.object_count(), objects as u64);
        let (cold_verified, cold_id, cold_objects, cold_bytes) =
            verify_typed_v2_history_payload_closure(
                &cold_store,
                exact_claim,
                locator.bridge(),
                &fixture.manifest,
                SemanticTypedPlaneVerificationTierV2::Standard,
                JumboRopeLimits::default(),
            )
            .expect("cold exact closure independently verifies");
        assert_eq!(cold_id, closure_id);
        assert_eq!(cold_objects, objects);
        assert_eq!(cold_bytes, bytes);
        assert_eq!(cold_verified, verified);

        let missing_segment =
            compose_fixture_closure(&cold_store, &fixture.objects, Some(0), false);
        let missing_claim =
            backend_store::ArtifactClosureClaim::from_id(missing_segment.receipt().closure());
        assert!(
            verify_typed_v2_history_payload_closure(
                &cold_store,
                missing_claim,
                locator.bridge(),
                &fixture.manifest,
                SemanticTypedPlaneVerificationTierV2::Standard,
                JumboRopeLimits::default(),
            )
            .is_err()
        );

        let jumbo_object = locator
            .bridge()
            .jumbo
            .first()
            .expect("positive fixture includes jumbo payloads")
            .object();
        let missing_jumbo_index = fixture
            .objects
            .iter()
            .position(|object| object.id().as_bytes() == jumbo_object.as_bytes())
            .expect("jumbo mapping resolves to a durable fixture object");
        let missing_jumbo = compose_fixture_closure(
            &cold_store,
            &fixture.objects,
            Some(missing_jumbo_index),
            false,
        );
        assert!(
            verify_typed_v2_history_payload_closure(
                &cold_store,
                backend_store::ArtifactClosureClaim::from_id(missing_jumbo.receipt().closure()),
                locator.bridge(),
                &fixture.manifest,
                SemanticTypedPlaneVerificationTierV2::Standard,
                JumboRopeLimits::default(),
            )
            .is_err()
        );

        let extra = compose_fixture_closure(&cold_store, &fixture.objects, None, true);
        let extra_claim = backend_store::ArtifactClosureClaim::from_id(extra.receipt().closure());
        assert!(
            verify_typed_v2_history_payload_closure(
                &cold_store,
                extra_claim,
                locator.bridge(),
                &fixture.manifest,
                SemanticTypedPlaneVerificationTierV2::Standard,
                JumboRopeLimits::default(),
            )
            .is_err()
        );
    }

    #[test]
    fn repeated_physical_ids_count_once_and_conflicting_metadata_fails_closed() {
        let fixture = crate::ir_hydration_store::positive_v2_history_fixture_for_test();
        let directory = TestDirectory::create();
        let store = FileStore::open(directory.0.join("cas"), 64 * 1024 * 1024)
            .expect("open receipt inventory FileStore");
        let object = fixture.objects.first().expect("positive fixture segment");
        let id = store
            .write_object(object)
            .expect("admit real segment object");
        let segment = fixture
            .locator
            .segments
            .iter()
            .find(|mapping| mapping.object().as_bytes() == id.as_bytes())
            .expect("segment mapping matches physical ID");
        let entry = InventoryEntry {
            payload_bytes: segment.byte_length(),
            envelope_bytes: u64::try_from(object.bytes().len()).expect("envelope length fits"),
            schema: crate::ProducedSemanticObjectKind::Segment.schema_identity(),
            created: true,
        };
        let mut inventory = BTreeMap::new();
        merge_inventory_entry(&mut inventory, id, entry).expect("insert first receipt");
        merge_inventory_entry(
            &mut inventory,
            id,
            InventoryEntry {
                created: false,
                ..entry
            },
        )
        .expect("same ID and metadata deduplicate");
        let metrics = summarize_inventory(&inventory).expect("measure unique physical objects");
        assert_eq!(inventory.len(), 1);
        assert_eq!(metrics.created_objects, 1);
        assert_eq!(metrics.reused_objects, 0);
        assert_eq!(metrics.envelope_bytes, entry.envelope_bytes);

        assert!(
            merge_inventory_entry(
                &mut inventory,
                id,
                InventoryEntry {
                    payload_bytes: entry.payload_bytes + 1,
                    ..entry
                },
            )
            .is_err()
        );
        assert!(
            merge_inventory_entry(
                &mut inventory,
                id,
                InventoryEntry {
                    schema: crate::ProducedSemanticObjectKind::JumboLeaf.schema_identity(),
                    ..entry
                },
            )
            .is_err()
        );
    }

    #[test]
    fn pin_from_another_filestore_is_rejected() {
        let directory = TestDirectory::create();
        let first =
            FileStore::open(directory.0.join("first"), 1024 * 1024).expect("open first pin store");
        let second = FileStore::open(directory.0.join("second"), 1024 * 1024)
            .expect("open second pin store");
        let range_store = FileSemanticRangeStore::open(
            first,
            crate::TransportLimits {
                max_chunk: super::super::MAX_RANGE_BYTES,
                max_frame: super::super::MAX_RANGE_BYTES + 192,
                ..crate::TransportLimits::default()
            },
        )
        .expect("open first FileSemanticRangeStore");
        let pin = range_store
            .pin_typed_v3_history_admission()
            .expect("pin first FileStore");
        assert!(require_pin_for_store(&pin, second.root()).is_err());
    }
}
